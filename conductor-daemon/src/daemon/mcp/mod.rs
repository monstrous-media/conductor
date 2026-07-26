// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! MCP Server for LLM integration (ADR-007 Phase 1B)
//!
//! Implements the Model Context Protocol (MCP) server that exposes Conductor
//! tools to external LLM agents like Claude Code, Cursor, etc.
//!
//! # Protocol
//!
//! MCP uses JSON-RPC 2.0 over Unix domain sockets (or stdio in future).
//!
//! ## Supported Methods
//!
//! - `initialize`: Initialize the MCP session
//! - `tools/list`: List available tools
//! - `tools/call`: Execute a tool
//!
//! # Security
//!
//! The MCP server uses Unix domain socket permissions (0600) to restrict
//! access to the current user only. All tools in Phase 1B are ReadOnly,
//! meaning they cannot modify configuration.

use super::connection_limiter::{ConnectionLimiter, OwnedConnectionPermit, RefusalLogger};
use super::engine_manager::SharedDaemonStateRefs;
use super::error::{DaemonError, Result};
use super::mcp_tools::{McpToolExecutor, get_tool_definitions};
use super::mcp_types::{
    InitializeResult, McpError, McpRequest, McpResponse, ServerCapabilities, ServerInfo,
    ToolsCapability, ToolsListResult,
};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

/// Default socket path for MCP server
const MCP_SOCKET_NAME: &str = "conductor-mcp.sock";

/// Maximum request size (1MB)
const MAX_REQUEST_SIZE: usize = 1_048_576;

/// Maximum concurrent MCP client connections
const MAX_CONCURRENT_CLIENTS: usize = 16;

/// MCP Server state
pub struct McpServer {
    socket_path: PathBuf,
    shutdown_rx: broadcast::Receiver<()>,
    config: Arc<super::live_config::LiveConfig>,
    /// Shared state references from engine manager (ADR-007 Phase 2)
    /// Wrapped in Arc so it can be cloned to each client handler
    shared_state: Option<Arc<SharedDaemonStateRefs>>,
}

impl McpServer {
    /// Create a new MCP server
    pub fn new(
        shutdown_rx: broadcast::Receiver<()>,
        config: Arc<super::live_config::LiveConfig>,
    ) -> Result<Self> {
        let socket_path = get_mcp_socket_path()?;

        Ok(Self {
            socket_path,
            shutdown_rx,
            config,
            shared_state: None,
        })
    }

    /// Create a new MCP server with shared state from engine manager (ADR-007 Phase 2)
    pub fn new_with_shared_state(
        shutdown_rx: broadcast::Receiver<()>,
        config: Arc<super::live_config::LiveConfig>,
        shared_state: SharedDaemonStateRefs,
    ) -> Result<Self> {
        let socket_path = get_mcp_socket_path()?;

        Ok(Self {
            socket_path,
            shutdown_rx,
            config,
            shared_state: Some(Arc::new(shared_state)),
        })
    }

    /// Create a new MCP server with a custom socket path (for testing)
    #[cfg(test)]
    pub fn new_with_path(
        socket_path: PathBuf,
        shutdown_rx: broadcast::Receiver<()>,
        config: Arc<super::live_config::LiveConfig>,
    ) -> Self {
        Self {
            socket_path,
            shutdown_rx,
            config,
            shared_state: None,
        }
    }

    /// Run the MCP server loop
    pub async fn run(&mut self) -> Result<()> {
        // Remove existing socket file if it exists
        #[cfg(unix)]
        {
            let _ = tokio::fs::remove_file(&self.socket_path).await;
        }

        // Create parent directory if needed
        if let Some(parent) = self.socket_path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                DaemonError::Mcp(format!("Failed to create MCP socket directory: {}", e))
            })?;
        }

        // Create listener
        let listener = UnixListener::bind(&self.socket_path)
            .map_err(|e| DaemonError::Mcp(format!("Failed to bind MCP socket: {}", e)))?;

        // Set secure permissions on socket file (Unix only)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(metadata) = tokio::fs::metadata(&self.socket_path).await {
                let mut perms = metadata.permissions();
                perms.set_mode(0o600);
                if let Err(e) = tokio::fs::set_permissions(&self.socket_path, perms).await {
                    warn!("Failed to set MCP socket permissions: {}", e);
                }
            }
        }

        info!("MCP server listening on {:?}", self.socket_path);

        // Limit concurrent connections to prevent resource exhaustion
        // (#1480). The permit is acquired BEFORE spawning, and an at-cap
        // connection is dropped immediately — mirroring the IPC server
        // (ADR-027 D16). The previous code spawned a task that then blocked
        // on `sem.acquire().await` while still holding the accepted stream
        // and cloned state, so a local client could open many idle
        // connections and grow FD / task / memory counts without bound,
        // despite the cap. `try_acquire` is non-blocking; queueing would
        // just defer the same pressure.
        let connection_limiter = ConnectionLimiter::with_max(MAX_CONCURRENT_CLIENTS);
        let refusal_logger = RefusalLogger::new();

        loop {
            tokio::select! {
                stream_result = listener.accept() => {
                    match stream_result {
                        Ok((stream, _addr)) => {
                            let permit = match connection_limiter.try_acquire() {
                                Some(p) => p,
                                None => {
                                    // Rate-limit the warn so a flood of refused
                                    // connections doesn't itself become a DoS.
                                    if let Some(suppressed) = refusal_logger.record() {
                                        warn!(
                                            "MCP connection refused: at concurrent-connection \
                                             cap ({}); dropping new client. Possible local DoS \
                                             attempt or connection-leak in a client. {} \
                                             additional refusals suppressed since last warning.",
                                            connection_limiter.max(),
                                            suppressed,
                                        );
                                    }
                                    drop(stream);
                                    continue;
                                }
                            };
                            let config = Arc::clone(&self.config);
                            let shared_state = self.shared_state.clone();

                            // #1311: pin peer credentials at accept and
                            // look up their tier ceiling in `McpRegistry`.
                            // `None` ceiling = unregistered → ReadOnly only.
                            // Pinning errors (kernel without pidfd_open, etc.)
                            // are logged and treated as "unregistered" so the
                            // peer is still allowed to read but not mutate.
                            let peer_ceiling = resolve_peer_tier_ceiling(&stream);

                            tokio::spawn(async move {
                                // Hold the permit for the connection's life;
                                // it releases the slot when the handler ends.
                                let _permit: OwnedConnectionPermit = permit;
                                if let Err(e) =
                                    handle_mcp_client(stream, config, shared_state, peer_ceiling).await
                                {
                                    error!("MCP client handler error: {}", e);
                                }
                            });
                        }
                        Err(e) => {
                            error!("Failed to accept MCP connection: {}", e);
                        }
                    }
                }

                _ = self.shutdown_rx.recv() => {
                    info!("MCP server shutting down");
                    break;
                }
            }
        }

        // Cleanup socket file
        #[cfg(unix)]
        {
            let _ = tokio::fs::remove_file(&self.socket_path).await;
        }

        Ok(())
    }
}

/// Get the MCP socket path
pub fn get_mcp_socket_path() -> Result<PathBuf> {
    let runtime_dir = dirs::runtime_dir()
        .or_else(dirs::cache_dir)
        .or_else(|| dirs::home_dir().map(|h| h.join(".cache")))
        .ok_or_else(|| DaemonError::Mcp("Could not determine runtime directory".to_string()))?;

    Ok(runtime_dir.join("conductor").join(MCP_SOCKET_NAME))
}

/// #1311: pin the peer credentials of a freshly-accepted MCP
/// `UnixStream`, canonicalize its exe path, and look the path up in
/// `McpRegistry`. Returns the registered tier ceiling, or `None` for
/// unregistered peers / pin failures.
///
/// Pin failures (older Linux kernels without `pidfd_open`, sandboxed
/// macOS environments, etc.) are logged and treated as "unregistered"
/// — same posture as the per-peer rate limiter (ADR-027 §D16). An
/// unregistered peer can still invoke ReadOnly tools; mutations
/// (Stateful, ConfigChange, HardwareIO) are rejected at dispatch.
fn resolve_peer_tier_ceiling(
    stream: &tokio::net::UnixStream,
) -> Option<super::audit::AuditRiskTier> {
    use super::mcp_registry::{McpRegistry, default_registry_path};
    use crate::security::PinnedPeer;
    use std::os::fd::AsFd;

    let peer = match PinnedPeer::from_stream(&stream.as_fd()) {
        Ok(p) => p,
        Err(e) => {
            debug!("MCP peer pin failed (treating as unregistered): {e}");
            return None;
        }
    };
    let registry_path = match default_registry_path() {
        Some(p) => p,
        None => {
            debug!(
                "MCP registry path unresolvable (no data_local_dir) — treating peer as unregistered"
            );
            return None;
        }
    };
    let registry = match McpRegistry::load(&registry_path) {
        Ok(r) => r,
        Err(e) => {
            debug!(
                "MCP registry load failed at {} (treating peer as unregistered): {e}",
                registry_path.display()
            );
            return None;
        }
    };
    // Peer's exe is already canonicalised by `PinnedPeer::from_stream`
    // (see security::peer_pin). `mcp register` canonicalises on save
    // (#1317 fixes the symmetric gap on revoke). So a direct path
    // comparison matches both sides without further normalisation.
    registry.lookup_tier(&peer.initial_exe)
}

/// Handle a single MCP client connection.
///
/// `peer_ceiling` is the per-client tier ceiling from
/// `McpRegistry::lookup_tier(canonical(peer.exe))`. `None` means the
/// peer is unregistered → clamped to `ReadOnly` per ADR-027 §D18.
/// Enforcement happens at the dispatch site in `handle_tools_call`
/// via [`check_peer_tier_ceiling`] — pre-#1311 the dispatch
/// short-circuited `CallerContext::internal_trusted()` which let
/// any same-UID process invoke ConfigChange tools regardless of
/// registry state.
async fn handle_mcp_client(
    stream: UnixStream,
    config: Arc<super::live_config::LiveConfig>,
    shared_state: Option<Arc<SharedDaemonStateRefs>>,
    peer_ceiling: Option<super::audit::AuditRiskTier>,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    let tool_executor = McpToolExecutor::new();
    let mut initialized = false;

    debug!("New MCP client connected");

    loop {
        line.clear();

        // Read a line with bounded allocation to prevent DoS via unbounded memory growth.
        // Uses take() to cap the read at MAX_REQUEST_SIZE + 1 bytes before the newline check.
        let bytes_read = {
            let mut limited = (&mut reader).take(MAX_REQUEST_SIZE as u64 + 1);
            tokio::time::timeout(
                std::time::Duration::from_secs(300), // 5 minute timeout
                limited.read_line(&mut line),
            )
            .await
            .map_err(|_| DaemonError::Mcp("Read timeout".to_string()))?
            .map_err(|e| DaemonError::Mcp(format!("Read error: {}", e)))?
        };

        if bytes_read == 0 {
            debug!("MCP client disconnected");
            break;
        }

        if line.len() > MAX_REQUEST_SIZE {
            // If take() capped before finding newline, drain remaining bytes until
            // newline to prevent stream desynchronization. Use bounded drain to
            // prevent unbounded allocation from a malicious client.
            if !line.ends_with('\n') {
                let mut discard = String::new();
                let mut drain = (&mut reader).take(MAX_REQUEST_SIZE as u64);
                let _ = drain.read_line(&mut discard).await;
            }
            let response = McpResponse::error(None, McpError::invalid_request("Request too large"));
            send_response(&mut writer, &response).await?;
            continue;
        }

        // Parse request
        let request: McpRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let response = McpResponse::error(None, McpError::parse_error(&e.to_string()));
                send_response(&mut writer, &response).await?;
                continue;
            }
        };

        debug!("MCP request: {} (id: {:?})", request.method, request.id);

        // Get fresh daemon state per request (ADR-007 Phase 2)
        let daemon_state = match &shared_state {
            Some(refs) => Some(refs.get_daemon_state().await),
            None => None,
        };

        // Handle request — returns None for JSON-RPC notifications (no response needed)
        if let Some(response) = handle_mcp_request(
            &request,
            &config,
            &daemon_state,
            &tool_executor,
            &mut initialized,
            &shared_state,
            peer_ceiling,
        )
        .await
        {
            send_response(&mut writer, &response).await?;
        }
    }

    Ok(())
}

/// Handle a single MCP request. Returns None for notifications (no response needed per JSON-RPC 2.0).
async fn handle_mcp_request(
    request: &McpRequest,
    config: &Arc<super::live_config::LiveConfig>,
    daemon_state: &Option<super::types::DaemonState>,
    tool_executor: &McpToolExecutor,
    initialized: &mut bool,
    shared_state: &Option<Arc<SharedDaemonStateRefs>>,
    peer_ceiling: Option<super::audit::AuditRiskTier>,
) -> Option<McpResponse> {
    Some(match request.method.as_str() {
        "initialize" => handle_initialize(request, initialized),
        "initialized" => {
            // JSON-RPC 2.0 notification — no response per spec
            *initialized = true;
            return None;
        }
        "tools/list" => {
            if !*initialized {
                return Some(McpResponse::error(
                    request.id.clone(),
                    McpError::invalid_request("Not initialized"),
                ));
            }
            handle_tools_list(request)
        }
        "tools/call" => {
            if !*initialized {
                return Some(McpResponse::error(
                    request.id.clone(),
                    McpError::invalid_request("Not initialized"),
                ));
            }
            handle_tools_call(
                request,
                config,
                daemon_state,
                tool_executor,
                shared_state,
                peer_ceiling,
            )
            .await
        }
        "ping" => McpResponse::success(request.id.clone(), json!({})),
        _ => McpResponse::error(
            request.id.clone(),
            McpError::method_not_found(&request.method),
        ),
    })
}

/// Handle initialize request
fn handle_initialize(request: &McpRequest, initialized: &mut bool) -> McpResponse {
    *initialized = true;

    let result = InitializeResult {
        protocol_version: "2024-11-05".to_string(),
        capabilities: ServerCapabilities {
            tools: Some(ToolsCapability {
                list_changed: false,
            }),
            resources: None,
            prompts: None,
        },
        server_info: ServerInfo {
            name: "conductor".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
    };

    McpResponse::success(request.id.clone(), serde_json::to_value(result).unwrap())
}

/// Handle tools/list request
fn handle_tools_list(request: &McpRequest) -> McpResponse {
    let tools = get_tool_definitions();
    let result = ToolsListResult { tools };
    McpResponse::success(request.id.clone(), serde_json::to_value(result).unwrap())
}

#[cfg(test)]
mod tests;
mod tools_call;

pub(crate) use tools_call::handle_tools_call;

/// #1311: per-client tier-ceiling check for the MCP socket.
///
/// The MCP registry (`mcp_registry::McpRegistry`, ADR-027 §D18)
/// stores per-exe tier grants. A peer that has been registered with
/// `AuditRiskTier::ConfigChange` may invoke tools at the
/// `ReadOnly`, `Stateful`, or `ConfigChange` tier — anything strictly
/// higher (`HardwareIO`, `Privileged`) is denied. An unregistered
/// peer (`None` ceiling) is clamped to `ReadOnly` only.
///
/// `AuditRiskTier::Internal` is reserved for daemon-internal callers
/// and never reachable from an MCP socket grant — it implies "trust
/// fully" and would defeat the per-client model. The function still
/// honours it for completeness (allows everything below
/// `Privileged`).
///
/// `ToolRiskTier::Privileged` is NEVER reachable from a registered
/// MCP peer — that tier is reserved for daemon-internal operations
/// (e.g. shutdown). Even a `HardwareIO`-registered peer is denied.
///
/// Returns `Ok(())` on allow, `Err(&'static str)` with a short
/// human-readable denial reason on deny. Caller maps the `Err` to
/// an MCP `permission_denied` response.
pub fn check_peer_tier_ceiling(
    registered_ceiling: Option<super::audit::AuditRiskTier>,
    requested_tool_tier: super::mcp_types::ToolRiskTier,
) -> std::result::Result<(), &'static str> {
    use super::audit::AuditRiskTier;
    use super::mcp_types::ToolRiskTier;

    // Privileged is never grantable via the registry — reject up
    // front regardless of ceiling.
    if matches!(requested_tool_tier, ToolRiskTier::Privileged) {
        return Err("Privileged tier is daemon-internal only; not reachable from MCP socket peers");
    }

    // ArtifactRender is currently not in the AuditRiskTier vocabulary
    // (registry can't grant it). Treat it as requiring at least
    // ConfigChange-tier ceiling — pragmatic mapping; revisit when
    // ArtifactRender tools land that MCP clients should reach.
    let required_min_ceiling = match requested_tool_tier {
        ToolRiskTier::ReadOnly => return Ok(()), // always allowed
        ToolRiskTier::Stateful => AuditRiskTier::Stateful,
        ToolRiskTier::ArtifactRender => AuditRiskTier::ConfigChange,
        ToolRiskTier::ConfigChange => AuditRiskTier::ConfigChange,
        ToolRiskTier::HardwareIO => AuditRiskTier::HardwareIO,
        ToolRiskTier::Privileged => unreachable!("handled above"),
    };

    let ceiling = match registered_ceiling {
        None => {
            return Err(
                "Unregistered MCP peer (ReadOnly only); run `conductorctl mcp register` to grant higher tier",
            );
        }
        Some(super::audit::AuditRiskTier::Internal) => {
            // Council R3 defensive posture: `Internal` is reserved
            // for daemon-internal callers (which never traverse
            // this MCP socket path). An external registry entry at
            // tier=Internal is either operator misconfiguration or
            // tampering — treat exactly like unregistered. ReadOnly
            // was already returned above before this branch, so
            // everything that lands here is non-ReadOnly → deny.
            return Err("Registry-observed tier=Internal is daemon-only; \
                 external MCP peers cannot be granted Internal — \
                 re-register with --tier ReadOnly/Stateful/ConfigChange/HardwareIO");
        }
        Some(c) => c,
    };

    // Ordinal: ReadOnly=0, Stateful=1, ConfigChange=2, HardwareIO=3.
    // Internal is rejected at the match above (Council R3 defensive).
    let ceiling_ord = audit_tier_ordinal(ceiling);
    let required_ord = audit_tier_ordinal(required_min_ceiling);

    if ceiling_ord >= required_ord {
        Ok(())
    } else {
        Err("MCP peer's registered tier does not cover the requested tool tier")
    }
}

/// Internal ordinal for `AuditRiskTier` — the registry uses
/// `AuditRiskTier` (no `Ord` derive on the public type to avoid
/// implying a numeric relationship the audit log shouldn't depend
/// on). #1311's tier ceiling check needs ordering; encode it here.
fn audit_tier_ordinal(tier: super::audit::AuditRiskTier) -> u8 {
    use super::audit::AuditRiskTier;
    match tier {
        AuditRiskTier::ReadOnly => 0,
        AuditRiskTier::Stateful => 1,
        AuditRiskTier::ConfigChange => 2,
        AuditRiskTier::HardwareIO => 3,
        AuditRiskTier::Internal => 4,
    }
}

/// Send a response to the client
async fn send_response(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    response: &McpResponse,
) -> Result<()> {
    let json = serde_json::to_string(response)
        .map_err(|e| DaemonError::Mcp(format!("Failed to serialize response: {}", e)))?;

    writer
        .write_all(json.as_bytes())
        .await
        .map_err(|e| DaemonError::Mcp(format!("Failed to write response: {}", e)))?;

    writer
        .write_all(b"\n")
        .await
        .map_err(|e| DaemonError::Mcp(format!("Failed to write newline: {}", e)))?;

    writer
        .flush()
        .await
        .map_err(|e| DaemonError::Mcp(format!("Failed to flush: {}", e)))?;

    Ok(())
}
