// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! IPC server for daemon control
//!
//! # Security Considerations
//!
//! This module implements several security measures to prevent abuse:
//!
//! ## Request Size Limiting
//!
//! All incoming IPC requests are limited to [`MAX_REQUEST_SIZE`] (1MB) to prevent
//! memory exhaustion attacks. Attackers could otherwise send arbitrarily large
//! JSON payloads to consume daemon memory and cause denial of service.
//!
//! When a request exceeds the size limit:
//! - The request is immediately rejected without processing
//! - An error response with code 1004 (InvalidRequest) is returned
//! - The oversized data is not accumulated in memory
//! - The attempt is logged as a warning for security monitoring
//!
//! ## Timeout Protection
//!
//! All IPC operations have a 10-second timeout to prevent resource exhaustion
//! from slow or stalled clients.
//!
//! ## Unix Socket Permissions
//!
//! The Unix domain socket and its directory are created with secure permissions:
//! - Socket directory: 0700 (rwx------) - owner-only access
//! - Socket file: 0600 (rw-------) - owner-only read/write
//! - Directory ownership is validated to match the current user
//! - Existing directories with insecure permissions are automatically fixed
//!
//! These measures prevent unauthorized local users from intercepting or sending
//! commands to the daemon.
//!
//! # Protocol
//!
//! Messages are JSON-encoded lines over a Unix domain socket:
//! - Request: `{"id": "...", "command": "...", "args": {...}}\n`
//! - Response: `{"id": "...", "status": "...", "data": {...}}\n`
//!
//! See module documentation for available commands and error codes.

use crate::daemon::audit::{AuditEntry, AuditEventType, AuditSink};
use crate::daemon::connection_limiter::{ConnectionLimiter, OwnedConnectionPermit, RefusalLogger};
use crate::daemon::error::{DaemonError, IpcErrorCode, Result};
use crate::daemon::ipc_rate_limit::{IpcMessageRateLimiter, RateLimitDecision};
use crate::daemon::state::get_socket_path;
use crate::daemon::types::{
    DaemonCommand, ErrorDetails, IpcCommand, IpcRequest, IpcResponse, MonitorEvent, ResponseStatus,
};
use crate::security::{CallerContext, PinnedPeer};
use serde_json::json;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc, oneshot};
use tracing::{debug, error, info, warn};

/// Maximum allowed size for a single IPC request (1MB).
///
/// This prevents memory exhaustion attacks from oversized requests. It is the
/// `max_bytes` passed to
/// [`read_bounded_line`](crate::daemon::ipc_framing::read_bounded_line) for
/// every incoming request, so integration tests can import it and drive the
/// real reader at the boundary instead of duplicating the literal.
pub const MAX_REQUEST_SIZE: usize = 1_048_576; // 1MB

/// ADR-027 §D16 (full) — drop an IPC connection that has
/// been silent for this long. Prevents a same-user attacker from
/// camping connections to pin the concurrent-connection budget.
/// 300s mirrors the spec's `connection_idle_timeout_sec = 300`.
const IPC_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// IPC server for handling daemon control requests
pub struct IpcServer {
    socket_path: String,
    command_tx: mpsc::Sender<DaemonCommand>,
    shutdown_rx: broadcast::Receiver<()>,
    /// Broadcast sender for push-based event monitoring
    event_broadcast_tx: broadcast::Sender<MonitorEvent>,
    /// Audit logger handle for `SubscribeAudit` streaming (ADR-027
    /// D13a). `None` when audit init failed at startup — a
    /// `SubscribeAudit` request then gets a clean error instead of
    /// a dropped connection. ADR-045 D5: the AuditSink seam —
    /// present in every composition (SQLite or JSONL).
    audit_sink: Option<Arc<dyn AuditSink>>,
    /// Per-peer IPC message rate limiter (ADR-027 D16).
    /// Shared across all client-handler tasks via `Arc`. Each
    /// handler checks the limiter on every incoming request,
    /// keyed by the pinned peer's PID.
    rate_limiter: Arc<IpcMessageRateLimiter>,
    /// ADR-027 D16: caps the number of concurrent connections so a
    /// same-user attacker can't OOM the daemon by opening sockets
    /// in a loop. Default 32 (see `DEFAULT_MAX_CONCURRENT_CONNECTIONS`).
    connection_limiter: ConnectionLimiter,
    /// Rate-limits the at-cap refusal warning so the same-user
    /// attacker who can spam connection attempts can't flip the
    /// D16 mitigation into a log-flood DoS. At most one warn per
    /// 5s window; suppressed refusals are batched into the next
    /// emission. `Arc` so it can be cheaply cloned and shared with
    /// future diagnostics endpoints (e.g. a `conductorctl ipc stats`
    /// reporter) without each holder owning a separate logger
    /// state. `Arc` doesn't reduce lock contention on `record()`
    /// itself — calls still serialise on the inner `Mutex` —
    /// but the lock is only taken when at-cap, which is the
    /// rare flood case.
    refusal_logger: Arc<RefusalLogger>,
}

impl IpcServer {
    /// Create a new IPC server
    pub fn new(
        command_tx: mpsc::Sender<DaemonCommand>,
        shutdown_rx: broadcast::Receiver<()>,
        event_broadcast_tx: broadcast::Sender<MonitorEvent>,
        audit_sink: Option<Arc<dyn AuditSink>>,
    ) -> Result<Self> {
        let socket_path = get_socket_path()?;
        let socket_str = socket_path.to_string_lossy().to_string();

        Ok(Self {
            socket_path: socket_str,
            command_tx,
            shutdown_rx,
            event_broadcast_tx,
            audit_sink,
            rate_limiter: Arc::new(IpcMessageRateLimiter::new()),
            connection_limiter: ConnectionLimiter::new(),
            refusal_logger: Arc::new(RefusalLogger::new()),
        })
    }

    /// Run the IPC server loop
    pub async fn run(&mut self) -> Result<()> {
        // Remove existing socket file if it exists (Unix only)
        #[cfg(unix)]
        {
            let _ = tokio::fs::remove_file(&self.socket_path).await;
        }

        // Create listener
        let listener = self.create_listener().await?;

        // Set secure permissions on socket file (Unix only)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(metadata) = tokio::fs::metadata(&self.socket_path).await {
                let mut perms = metadata.permissions();
                perms.set_mode(0o600); // rw------- (owner-only access)
                if let Err(e) = tokio::fs::set_permissions(&self.socket_path, perms).await {
                    warn!("Failed to set socket permissions: {}", e);
                }
            }
        }

        info!("IPC server listening on {}", self.socket_path);

        loop {
            tokio::select! {
                // Handle incoming connections
                stream_result = listener.accept() => {
                    match stream_result {
                        Ok((stream, _addr)) => {
                            // ADR-027 D16: try to grab a slot from
                            // the global concurrent-connection cap.
                            // If at-cap, drop the new socket on the
                            // floor (closes the connection
                            // immediately) and log the denial.
                            // Non-blocking: queueing would just
                            // defer the same memory pressure.
                            let permit = match self.connection_limiter.try_acquire() {
                                Some(p) => p,
                                None => {
                                    // Rate-limit the warn so a flood of refused
                                    // connections doesn't itself become a DoS.
                                    if let Some(suppressed) = self.refusal_logger.record() {
                                        warn!(
                                            "IPC connection refused: at concurrent-connection \
                                             cap ({}); dropping new client. Possible local DoS \
                                             attempt or connection-leak in a client. \
                                             {} additional refusals suppressed since last warning.",
                                            self.connection_limiter.max(),
                                            suppressed,
                                        );
                                    }
                                    drop(stream);
                                    continue;
                                }
                            };
                            let cmd_tx = self.command_tx.clone();
                            let event_tx = self.event_broadcast_tx.clone();
                            let audit_sink = self.audit_sink.clone();
                            let rate_limiter = self.rate_limiter.clone();
                            tokio::spawn(async move {
                                // Move the permit into the handler
                                // task; it's dropped (releasing the
                                // slot) when this future completes.
                                let _permit: OwnedConnectionPermit = permit;

                                // ADR-027 D1 wiring: pin the
                                // peer before reading any request.
                                // We do this *inside* the spawned
                                // task — not in the accept loop —
                                // because both `pin_linux`
                                // (`/proc/<pid>/exe` readlink) and
                                // `pin_macos` (Security.framework
                                // signature inspection) make
                                // synchronous syscalls that would
                                // briefly block the tokio worker
                                // running the accept loop, causing
                                // head-of-line blocking for the
                                // next incoming connection. Each
                                // per-connection task runs on the
                                // multi-threaded runtime so it can
                                // pin in parallel with new accepts.
                                //
                                // Pin failures don't reject the
                                // connection here —
                                // `SecurityPolicy::shadow_mode =
                                // true` (Phase 1A invariant) means
                                // the gate is not yet enforcing,
                                // and dropping a connection on pin
                                // failure during the wiring rollout
                                // would regress today's (working)
                                // behaviour. The error is logged so
                                // the daemon operator can spot a
                                // misconfigured kernel (Linux < 5.3
                                // → pidfd_open fails) or a same-uid
                                // TCC anomaly. A later flag flip is
                                // what turns "tolerated" into
                                // "denied".
                                let caller_ctx = match PinnedPeer::from_stream(&stream) {
                                    Ok(peer) => {
                                        Some(CallerContext::from_peer(Arc::new(peer)))
                                    }
                                    Err(e) => {
                                        warn!(
                                            "Peer pinning failed during IPC accept (shadow-mode \
                                             tolerated; flag flip will reject): {}",
                                            e
                                        );
                                        None
                                    }
                                };

                                if let Err(e) = handle_client(
                                    stream,
                                    cmd_tx,
                                    event_tx,
                                    audit_sink,
                                    rate_limiter,
                                    caller_ctx,
                                )
                                .await
                                {
                                    error!("Client handler error: {}", e);
                                }
                            });
                        }
                        Err(e) => {
                            error!("Failed to accept connection: {}", e);
                        }
                    }
                }

                // Handle shutdown signal
                _ = self.shutdown_rx.recv() => {
                    info!("IPC server shutting down");
                    break;
                }
            }
        }

        // Cleanup socket file (Unix only)
        #[cfg(unix)]
        {
            let _ = tokio::fs::remove_file(&self.socket_path).await;
        }

        Ok(())
    }

    /// Create platform-specific listener
    async fn create_listener(&self) -> Result<UnixListener> {
        #[cfg(unix)]
        {
            UnixListener::bind(&self.socket_path)
                .map_err(|e| DaemonError::Ipc(format!("Failed to create Unix socket: {}", e)))
        }

        #[cfg(not(unix))]
        {
            // Windows: Use named pipes (requires different approach)
            // For now, return error on Windows
            Err(DaemonError::Ipc(
                "Windows named pipes not yet implemented".to_string(),
            ))
        }
    }
}

/// Handle a single client connection
///
/// `caller_ctx` is the ADR-027 D1-pinned + classified peer
/// identity, carried from the accept loop. It is wired here
/// for plumbing only; later wiring sub-pieces hand it to
/// `security::gate::enforce` before dispatching tool / action
/// requests. `None` means peer pinning failed at accept (logged
/// there); the request handler currently treats this as
/// "anonymous" and proceeds — once the Phase 1A flag flips,
/// `None` will deny by default. Until then we intentionally
/// accept the connection so behaviour stays unchanged through
/// the wiring rollout.
async fn handle_client(
    stream: UnixStream,
    command_tx: mpsc::Sender<DaemonCommand>,
    event_broadcast_tx: broadcast::Sender<MonitorEvent>,
    audit_sink: Option<Arc<dyn AuditSink>>,
    rate_limiter: Arc<IpcMessageRateLimiter>,
    caller_ctx: Option<CallerContext>,
) -> Result<()> {
    // Log the pin + classification result for operator-side
    // visibility, then keep the context bound to its
    // function-scope lifetime so it can later be consumed for
    // `gate::enforce` calls without an early-drop hazard. The
    // `if let` arms below are the only use today; the earlier
    // `let _ = caller_ctx;` was removed since the binding is read
    // by the debug-log block.
    if let Some(ref ctx) = caller_ctx
        && let Some(ref peer) = ctx.peer
    {
        debug!(
            "IPC connection pinned: uid={} pid={} exe={} trust={:?}",
            peer.uid,
            peer.pid,
            peer.initial_exe.display(),
            ctx.trust_level
        );
    }

    let (reader, writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut writer = writer;
    // `line` is now `Vec<u8>` so the bounded-read helper can
    // append bytes directly without an intermediate UTF-8 check on
    // each loop iteration. UTF-8 validation runs once per frame at
    // parse time below.
    let mut line: Vec<u8> = Vec::new();

    // Extract the peer identity once for rate-limit keying
    // (ADR-027 §D12): `(pid, exe)`. `None` means peer
    // pinning failed at accept (logged there) — fall through with
    // a sentinel so the limiter still tracks "anonymous" peers as
    // one bucket rather than disabling the rate limit entirely
    // when pinning is unavailable (e.g. on Linux kernels without
    // pidfd_open). The sentinel exe path is `/anonymous` so the
    // bucket is observably distinct from any real peer.
    let (peer_pid, peer_exe): (u32, std::path::PathBuf) = caller_ctx
        .as_ref()
        .and_then(|ctx| ctx.peer.as_ref())
        .map(|p| (p.pid, p.initial_exe.clone()))
        .unwrap_or_else(|| (0, std::path::PathBuf::from("/anonymous")));

    // ADR-027 §D16 — wrap the request-receive read in an
    // idle-timeout window. A connection that has been silent for
    // `IPC_IDLE_TIMEOUT` (300s) is dropped cleanly, releasing its
    // slot in the concurrent-connection cap. Streaming subscriptions
    // (`SubscribeEvents`, `SubscribeAudit`) take over the connection
    // BEFORE this loop iterates again, so a live subscription is
    // unaffected by the idle timeout.
    //
    // Per-frame size cap is enforced INSIDE the read via
    // `read_bounded_line`. The previous code called
    // `BufReader::read_line` (unbounded) and only checked
    // `line.len() > MAX_REQUEST_SIZE` AFTER the read returned —
    // letting a malicious client OOM the daemon with a newline-free
    // frame larger than 1 MB.
    loop {
        let read_result = tokio::time::timeout(
            IPC_IDLE_TIMEOUT,
            crate::daemon::ipc_framing::read_bounded_line(&mut reader, &mut line, MAX_REQUEST_SIZE),
        )
        .await;
        let outcome = match read_result {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => return Err(e.into()),
            Err(_elapsed) => {
                info!(
                    "Closing IPC connection idle for >{}s",
                    IPC_IDLE_TIMEOUT.as_secs()
                );
                rate_limiter.forget(peer_pid, &peer_exe);
                break;
            }
        };
        match outcome {
            crate::daemon::ipc_framing::ReadResult::Eof => {
                rate_limiter.forget(peer_pid, &peer_exe);
                break;
            }
            crate::daemon::ipc_framing::ReadResult::Overflow => {
                // peer sent more than MAX_REQUEST_SIZE bytes
                // without a newline. The bounded reader stopped at
                // exactly `MAX_REQUEST_SIZE + 1` bytes — memory is
                // safe. Send an error response then CLOSE the
                // connection: an overflowed frame can't be parsed
                // and subsequent bytes on the wire are unsynchronised
                // relative to the IPC framing.
                warn!(
                    "Rejected oversized IPC request: ≥{} bytes (max: {} bytes), closing connection",
                    line.len(),
                    MAX_REQUEST_SIZE
                );
                let error_response = create_error_response(
                    "unknown",
                    IpcErrorCode::InvalidRequest,
                    format!(
                        "Request too large: exceeded maximum of {} bytes (1MB); connection closed",
                        MAX_REQUEST_SIZE
                    ),
                    Some(json!({
                        "max_size": MAX_REQUEST_SIZE,
                        "security": "Request rejected and connection closed to prevent memory exhaustion"
                    })),
                );
                send_response(&mut writer, &error_response).await?;
                rate_limiter.forget(peer_pid, &peer_exe);
                break;
            }
            crate::daemon::ipc_framing::ReadResult::Line(_n) => {
                // Fall through to parse.
            }
        }

        // Parse request. Convert bytes → &str at the boundary; an
        // invalid-UTF-8 frame is treated as malformed JSON (same as
        // legacy `parse_request(&String)` would have surfaced via
        // serde's UTF-8 expectations on the input slice).
        let line_str = match std::str::from_utf8(&line) {
            Ok(s) => s,
            Err(e) => {
                let error_response = create_error_response(
                    "unknown",
                    IpcErrorCode::InvalidJson,
                    format!("request bytes are not valid UTF-8: {e}"),
                    None,
                );
                send_response(&mut writer, &error_response).await?;
                line.clear();
                continue;
            }
        };
        // ADR-034 §D2.3: pre-deserialisation payload cap. Peek the
        // command WITHOUT building the args/config tree (serde skips `args`
        // as `IgnoredAny`), then reject an over-cap config-carrying request
        // HERE — before `parse_request` allocates the full args Value or the
        // handler deserialises the Config. This is the allocator-pressure
        // defence the global 1 MB line cap is too loose to provide.
        //
        // The cap is measured on `line.len()` — the raw on-wire BYTE count,
        // which is exactly what bounds the would-be allocation (and equals
        // `line_str.len()` for the valid UTF-8 we just decoded). Byte length,
        // not char count, is the correct metric for an allocation-pressure
        // guard.
        if let Some(peek) = peek_request(line_str)
            && payload_cap_exceeded(&peek.command, line.len())
        {
            warn!(
                "Rejected oversized {:?} request: {} bytes (request cap {} KiB)",
                peek.command,
                line.len(),
                MAX_CONFIG_REQUEST_BYTES / 1024
            );
            // Own the id (move, no clone) so it doesn't borrow `peek` — and
            // because `peek` is unused past this block anyway (we `continue`).
            let id = if peek.id.is_empty() {
                "unknown".to_string()
            } else {
                peek.id
            };
            let error_response = create_error_response(
                &id,
                IpcErrorCode::PayloadTooLarge,
                format!(
                    "request exceeds the {} KiB size cap for config-carrying \
                     commands ({} bytes received)",
                    MAX_CONFIG_REQUEST_BYTES / 1024,
                    line.len()
                ),
                Some(json!({
                    "max_payload_bytes": MAX_CONFIG_PAYLOAD_BYTES,
                    "max_request_bytes": MAX_CONFIG_REQUEST_BYTES,
                    "received_bytes": line.len(),
                })),
            );
            send_response(&mut writer, &error_response).await?;
            line.clear();
            continue;
        }

        let request = match parse_request(line_str) {
            Ok(req) => req,
            Err(e) => {
                // Send error response
                let error_response = create_error_response(
                    "unknown",
                    IpcErrorCode::InvalidJson,
                    e.to_string(),
                    None,
                );
                send_response(&mut writer, &error_response).await?;
                line.clear();
                continue;
            }
        };

        debug!("Received IPC request: {:?}", request.command);

        // ADR-027 §D16 (full) — per-peer message rate limit.
        // The check runs AFTER parse so a malformed-JSON flood
        // doesn't burn the limiter's budget on bytes that never
        // reach a handler; the parse-error response is itself
        // bounded by `line.len() > MAX_REQUEST_SIZE` upstream. A
        // peer over its 100/sec budget gets a structured denial
        // and the request is dropped — the connection itself is
        // kept open so a brief burst doesn't blow up a
        // long-running client.
        match rate_limiter.check(peer_pid, &peer_exe) {
            RateLimitDecision::Allowed => {}
            RateLimitDecision::Denied {
                recent_count,
                max_per_window,
            } => {
                warn!(
                    "IPC rate-limit exceeded for peer pid {}: {} msgs in last 1s (cap {})",
                    peer_pid, recent_count, max_per_window,
                );
                let error_response = create_error_response(
                    &request.id,
                    IpcErrorCode::RateLimitExceeded,
                    format!(
                        "Rate limit exceeded: peer {} sent {} messages in the last second (cap {})",
                        peer_pid, recent_count, max_per_window,
                    ),
                    Some(json!({
                        "peer_pid": peer_pid,
                        "recent_count": recent_count,
                        "max_per_window": max_per_window,
                        "window_secs": 1,
                    })),
                );
                send_response(&mut writer, &error_response).await?;
                line.clear();
                continue;
            }
            RateLimitDecision::InternalStateCorrupt => {
                error!(
                    "IPC rate-limiter state corrupt — failing closed for peer pid {}",
                    peer_pid,
                );
                let error_response = create_error_response(
                    &request.id,
                    IpcErrorCode::InternalError,
                    "Rate-limiter state corrupt (failing closed)".to_string(),
                    None,
                );
                send_response(&mut writer, &error_response).await?;
                line.clear();
                continue;
            }
        }

        // SubscribeEvents: switch to push/streaming mode.
        // This takes over the connection — no more request-response after this.
        if matches!(request.command, IpcCommand::SubscribeEvents) {
            // Subscribe BEFORE sending StartEventMonitor to avoid race condition
            // (events between Start and subscribe() would be lost)
            let rx = event_broadcast_tx.subscribe();

            let response = create_success_response(&request.id, Some(json!({"subscribed": true})));
            send_response(&mut writer, &response).await?;
            info!("Client subscribed to event stream");

            // Send StartEventMonitor to daemon to enable capture
            let (start_tx, _) = oneshot::channel();
            let _ = command_tx
                .send(DaemonCommand::IpcRequest {
                    request: IpcRequest {
                        id: uuid::Uuid::new_v4().to_string(),
                        command: IpcCommand::StartEventMonitor,
                        args: json!({}),
                    },
                    // Internal-origin (subscription bootstrap) — clone
                    // the caller's context so the engine-manager handler
                    // can still see who triggered the subscribe.
                    caller_ctx: caller_ctx.clone(),
                    response_tx: start_tx,
                })
                .await;

            // Enter streaming loop with pre-created receiver
            handle_event_subscription(&mut writer, rx).await;
            info!("Client event subscription ended");
            return Ok(());
        }

        // SubscribeAudit: switch to push/streaming mode for the
        // live audit tail (ADR-027 D13a). Like
        // SubscribeEvents, this takes over the connection.
        if matches!(request.command, IpcCommand::SubscribeAudit) {
            let denied_only = request
                .args
                .get("denied_only")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let Some(sink) = audit_sink.as_ref() else {
                let response = create_error_response(
                    &request.id,
                    IpcErrorCode::InternalError,
                    "Audit logging is disabled on this daemon".to_string(),
                    None,
                );
                send_response(&mut writer, &response).await?;
                return Ok(());
            };

            // Subscribe BEFORE acking so no denial logged between
            // the ack and the subscribe is lost (mirrors the
            // SubscribeEvents race fix).
            let rx = sink.subscribe();
            let response = create_success_response(
                &request.id,
                Some(json!({ "subscribed": true, "denied_only": denied_only })),
            );
            send_response(&mut writer, &response).await?;
            info!("Client subscribed to audit stream (denied_only={denied_only})");

            handle_audit_subscription(&mut writer, rx, denied_only).await;
            info!("Client audit subscription ended");
            return Ok(());
        }

        // Create response channel
        let (response_tx, response_rx) = oneshot::channel();

        // Send command to daemon. Clone the per-connection
        // CallerContext so each request carries the same pinned peer
        // identity through to the gate; cloning is cheap (Arc bumps,
        // small struct copy) and the alternative — moving — would
        // make the binding unusable for follow-up requests on this
        // same connection.
        command_tx
            .send(DaemonCommand::IpcRequest {
                request,
                caller_ctx: caller_ctx.clone(),
                response_tx,
            })
            .await
            .map_err(|_| DaemonError::ChannelSend)?;

        // Wait for response with timeout
        let response =
            // 120s ceiling: most requests complete in <1s. Long-running work
            // (e.g. SimulateMapping sequences) is now dispatched asynchronously,
            // so the IPC handler responds quickly. This is a conservative safety net.
            match tokio::time::timeout(std::time::Duration::from_secs(120), response_rx).await {
                Ok(Ok(resp)) => resp,
                Ok(Err(_)) => create_error_response(
                    "unknown",
                    IpcErrorCode::InternalError,
                    "Response channel closed".to_string(),
                    None,
                ),
                Err(_) => create_error_response(
                    "unknown",
                    IpcErrorCode::Timeout,
                    "Request timed out".to_string(),
                    None,
                ),
            };

        // Send response
        send_response(&mut writer, &response).await?;

        line.clear();
    }

    Ok(())
}

/// Maximum events per batch write (prevent OOM from unbounded drain)
const MAX_BATCH_SIZE: usize = 100;

/// Handle push-based event streaming over a persistent connection
///
/// Writes batched events as newline-delimited JSON arrays. Uses natural batching:
/// waits for at least one event, then drains up to MAX_BATCH_SIZE immediately
/// available events into a single write.
///
/// Returns when the connection drops (write error) or the broadcast channel closes.
async fn handle_event_subscription(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    mut rx: broadcast::Receiver<MonitorEvent>,
) {
    loop {
        // Wait for at least one event
        let first = match rx.recv().await {
            Ok(event) => event,
            Err(broadcast::error::RecvError::Lagged(n)) => {
                warn!("Event subscriber lagged by {n} events, catching up");
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => {
                debug!("Event broadcast channel closed");
                break;
            }
        };

        // Natural batching: drain immediately available events, capped to prevent OOM
        let mut batch = vec![first];
        while batch.len() < MAX_BATCH_SIZE {
            match rx.try_recv() {
                Ok(event) => batch.push(event),
                Err(broadcast::error::TryRecvError::Lagged(n)) => {
                    warn!("Event subscriber lagged by {n} events during batch drain");
                    break;
                }
                Err(_) => break,
            }
        }

        // Write batch as JSON array line
        let json = match serde_json::to_string(&batch) {
            Ok(j) => j,
            Err(e) => {
                error!("Failed to serialize event batch: {e}");
                continue;
            }
        };

        if writer.write_all(json.as_bytes()).await.is_err()
            || writer.write_all(b"\n").await.is_err()
            || writer.flush().await.is_err()
        {
            // Connection dropped — subscriber disconnected
            debug!("Event subscriber disconnected");
            break;
        }
    }
}

/// Handle push-based audit-event streaming (ADR-027 D13a).
///
/// Backs `conductorctl audit tail -f` / `audit denied`. Writes each
/// `AuditEntry` as a single newline-delimited JSON object.
///
/// **Lag is surfaced explicitly** (impl-spec case 1J): when the
/// broadcast receiver falls behind `AUDIT_BROADCAST_CAPACITY`
/// events, an explicit `{"lagged": n}` marker line is written to
/// the client rather than silently skipping the gap — the operator
/// is told to re-query the persistent log (`audit tail --last N`)
/// for the missed window. The persistent sink (SQLite under
/// `audit-db`, the JSONL trail otherwise — ADR-045 D5) is unaffected
/// by broadcast-ring pressure; it remains the complete record.
async fn handle_audit_subscription(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    mut rx: broadcast::Receiver<AuditEntry>,
    denied_only: bool,
) {
    loop {
        let entry = match rx.recv().await {
            Ok(entry) => entry,
            Err(broadcast::error::RecvError::Lagged(n)) => {
                // Explicit lag marker — case 1J. The client prints
                // this so the operator knows to backfill from the
                // persistent log.
                warn!("Audit subscriber lagged by {n} events");
                let marker = json!({ "lagged": n }).to_string();
                if writer.write_all(marker.as_bytes()).await.is_err()
                    || writer.write_all(b"\n").await.is_err()
                    || writer.flush().await.is_err()
                {
                    debug!("Audit subscriber disconnected");
                    break;
                }
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => {
                debug!("Audit broadcast channel closed");
                break;
            }
        };

        // `denied_only` filter: skip non-denial events entirely so
        // `audit denied` is a clean denial-only stream.
        if denied_only && entry.event_type != AuditEventType::ToolDenied {
            continue;
        }

        let json = match serde_json::to_string(&entry) {
            Ok(j) => j,
            Err(e) => {
                error!("Failed to serialize audit entry: {e}");
                continue;
            }
        };

        if writer.write_all(json.as_bytes()).await.is_err()
            || writer.write_all(b"\n").await.is_err()
            || writer.flush().await.is_err()
        {
            debug!("Audit subscriber disconnected");
            break;
        }
    }
}

/// Parse IPC request from JSON line
fn parse_request(line: &str) -> Result<IpcRequest> {
    serde_json::from_str(line.trim())
        .map_err(|e| DaemonError::Ipc(format!("Failed to parse request JSON: {}", e)))
}

/// ADR-034 §D2.3: documented config-payload cap (256 KiB).
pub const MAX_CONFIG_PAYLOAD_BYTES: usize = 256 * 1024;

/// Enforced LINE-length cap for config-carrying commands = the 256 KiB
/// payload cap plus a 1 KiB allowance for the JSON envelope
/// (`id`/`command`/`base_generation` framing around the config body).
const MAX_CONFIG_REQUEST_BYTES: usize = MAX_CONFIG_PAYLOAD_BYTES + 1024;

// Compile-time guard: the documented request cap is exactly
// 257 KiB (256 KiB payload + 1 KiB envelope). Fail the build if either
// constant drifts from that relationship rather than silently shipping a
// different security cap than the docs/CHANGELOG state.
const _: () = assert!(MAX_CONFIG_REQUEST_BYTES == 257 * 1024);

/// ADR-034 §D2.3: the tighter pre-deserialisation line cap for commands
/// that carry a config in their payload, or `None` when only the global
/// [`MAX_REQUEST_SIZE`] guard applies.
///
/// `SaveConfig` carries an inline config body; `ImportConfig` carries a
/// path (so its payload is small in practice) but is capped too for
/// defence in depth and spec parity (§D2.3). `ReloadFromDisk` reads from
/// disk and carries no body, so it is not capped here.
fn config_payload_cap(command: &IpcCommand) -> Option<usize> {
    match command {
        IpcCommand::SaveConfig | IpcCommand::ImportConfig => Some(MAX_CONFIG_REQUEST_BYTES),
        _ => None,
    }
}

/// `true` when `line_len` exceeds the §D2.3 cap for `command`. Pure so the
/// allocator-pressure decision is unit-testable without the IPC server.
fn payload_cap_exceeded(command: &IpcCommand, line_len: usize) -> bool {
    config_payload_cap(command).is_some_and(|cap| line_len > cap)
}

/// Minimal projection of an IPC request used to enforce the §D2.3 cap
/// BEFORE the full request (and its potentially large `args`/config tree)
/// is deserialised. Serde scans past `args` as `IgnoredAny` — it is never
/// allocated — so peeking is O(bytes) but carries no allocator pressure.
#[derive(serde::Deserialize)]
struct RequestPeek {
    #[serde(default)]
    id: String,
    command: IpcCommand,
}

/// Cheaply extract `(id, command)` from a request line without typing or
/// allocating `args`. Returns `None` for malformed JSON or an unknown
/// command — the caller then falls through to [`parse_request`], which
/// produces the proper structured parse error.
fn peek_request(line: &str) -> Option<RequestPeek> {
    serde_json::from_str::<RequestPeek>(line.trim()).ok()
}

/// Send IPC response as JSON line
async fn send_response(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    response: &IpcResponse,
) -> Result<()> {
    let json = serde_json::to_string(response)?;
    writer.write_all(json.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

/// Create an error response
fn create_error_response(
    id: &str,
    code: IpcErrorCode,
    message: String,
    details: Option<serde_json::Value>,
) -> IpcResponse {
    IpcResponse {
        id: id.to_string(),
        status: ResponseStatus::Error,
        data: None,
        error: Some(ErrorDetails {
            code: code.as_u16(),
            message,
            details,
        }),
    }
}

/// Create a success response
pub fn create_success_response(id: &str, data: Option<serde_json::Value>) -> IpcResponse {
    IpcResponse {
        id: id.to_string(),
        status: ResponseStatus::Success,
        data,
        error: None,
    }
}

/// IPC client for sending commands to daemon
pub struct IpcClient {
    /// Persistent buffered reader over the connection. A previous design
    /// created a temporary `BufReader<&mut stream>` per `send_request` and a
    /// fresh `BufReader<stream>` in `into_reader`; if the per-request reader
    /// buffered bytes PAST the response newline (the daemon writing the response
    /// and a following streamed line in one syscall), those bytes were silently
    /// dropped when the temporary was dropped and `into_reader` started blind to
    /// them. Holding one reader for the connection's whole life preserves any
    /// read-ahead across `send_request` → `into_reader`.
    reader: BufReader<UnixStream>,
}

impl IpcClient {
    /// Create new IPC client with custom socket path
    pub async fn new(socket_path: String) -> Result<Self> {
        #[cfg(unix)]
        let stream = UnixStream::connect(&socket_path)
            .await
            .map_err(|e| DaemonError::Ipc(format!("Failed to connect to daemon: {}", e)))?;

        #[cfg(not(unix))]
        return Err(DaemonError::Ipc(
            "Windows named pipes not yet implemented".to_string(),
        ));

        Ok(Self {
            reader: BufReader::new(stream),
        })
    }

    /// Connect to daemon IPC server using default socket path
    pub async fn connect() -> Result<Self> {
        let socket_path = get_socket_path()?;
        let socket_str = socket_path.to_string_lossy();

        Self::new(socket_str.to_string()).await
    }

    /// Send a request and wait for response
    pub async fn send_request(&mut self, request: IpcRequest) -> Result<IpcResponse> {
        // Serialize request
        let json = serde_json::to_string(&request)?;

        // Send request — write through the reader's underlying stream
        // (UnixStream is full-duplex; writing via `get_mut()` does not disturb
        // the reader's buffered read-ahead).
        let stream = self.reader.get_mut();
        stream.write_all(json.as_bytes()).await?;
        stream.write_all(b"\n").await?;
        stream.flush().await?;

        // Read response from the PERSISTENT reader so any bytes it buffers past
        // the response newline survive for `into_reader`.
        let mut line = String::new();
        self.reader.read_line(&mut line).await?;

        // Parse response
        let response: IpcResponse = serde_json::from_str(&line)?;

        Ok(response)
    }

    /// Ping the daemon
    pub async fn ping(&mut self) -> Result<IpcResponse> {
        let request = IpcRequest {
            id: uuid::Uuid::new_v4().to_string(),
            command: crate::daemon::types::IpcCommand::Ping,
            args: json!({}),
        };

        self.send_request(request).await
    }

    /// Get daemon status
    pub async fn status(&mut self) -> Result<IpcResponse> {
        let request = IpcRequest {
            id: uuid::Uuid::new_v4().to_string(),
            command: crate::daemon::types::IpcCommand::Status,
            args: json!({}),
        };

        self.send_request(request).await
    }

    /// Request config reload
    pub async fn reload(&mut self) -> Result<IpcResponse> {
        let request = IpcRequest {
            id: uuid::Uuid::new_v4().to_string(),
            command: crate::daemon::types::IpcCommand::Reload,
            args: json!({}),
        };

        self.send_request(request).await
    }

    /// Stop daemon
    pub async fn stop(&mut self) -> Result<IpcResponse> {
        let request = IpcRequest {
            id: uuid::Uuid::new_v4().to_string(),
            command: crate::daemon::types::IpcCommand::Stop,
            args: json!({}),
        };

        self.send_request(request).await
    }

    /// Send a generic command with arguments
    pub async fn send_command(
        &mut self,
        command: crate::daemon::types::IpcCommand,
        args: serde_json::Value,
    ) -> Result<IpcResponse> {
        let request = IpcRequest {
            id: uuid::Uuid::new_v4().to_string(),
            command,
            args,
        };

        self.send_request(request).await
    }

    /// Consume the client into a streaming reader for push-based subscriptions
    ///
    /// After sending a `SubscribeEvents` command and receiving the initial response,
    /// call this to get a `BufReader` for reading streamed event lines.
    pub fn into_reader(self) -> BufReader<UnixStream> {
        // Return the SAME reader used for request/response — not a fresh
        // one — so any bytes buffered past the last response newline (e.g. the
        // first streamed event the daemon coalesced with the subscribe ack) are
        // not lost.
        self.reader
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::types::IpcCommand;

    #[test]
    fn test_parse_request_valid() {
        let json = r#"{"id":"test-123","command":"PING","args":{}}"#;
        let request = parse_request(json).unwrap();
        assert_eq!(request.id, "test-123");
        assert!(matches!(request.command, IpcCommand::Ping));
    }

    #[test]
    fn test_parse_request_invalid_json() {
        let json = r#"{"id":"test-123","command":"PING"#; // missing closing brace
        let result = parse_request(json);
        assert!(result.is_err());
    }

    // ── ADR-034 §D2.3 payload cap ────────────────────────────

    #[test]
    fn config_payload_cap_only_for_config_carrying_commands() {
        // SaveConfig (inline config body) + ImportConfig get the tighter cap.
        assert_eq!(
            config_payload_cap(&IpcCommand::SaveConfig),
            Some(MAX_CONFIG_REQUEST_BYTES)
        );
        assert_eq!(
            config_payload_cap(&IpcCommand::ImportConfig),
            Some(MAX_CONFIG_REQUEST_BYTES)
        );
        // Commands without a large inline payload fall back to the global
        // MAX_REQUEST_SIZE cap (None = no tighter cap here).
        assert_eq!(config_payload_cap(&IpcCommand::Ping), None);
        assert_eq!(config_payload_cap(&IpcCommand::ConfigDriftStatus), None);
        // ReloadFromDisk reads from disk — it carries a path, not a body.
        assert_eq!(config_payload_cap(&IpcCommand::ReloadFromDisk), None);
    }

    #[test]
    fn payload_cap_exceeded_trips_only_above_the_config_cap() {
        // One byte over → rejected (allocator-pressure defense).
        assert!(payload_cap_exceeded(
            &IpcCommand::SaveConfig,
            MAX_CONFIG_REQUEST_BYTES + 1
        ));
        // Exactly at the cap → allowed.
        assert!(!payload_cap_exceeded(
            &IpcCommand::SaveConfig,
            MAX_CONFIG_REQUEST_BYTES
        ));
        // A non-config command is NOT capped here even at a large size —
        // only the global MAX_REQUEST_SIZE guard applies to it.
        assert!(!payload_cap_exceeded(&IpcCommand::Ping, MAX_REQUEST_SIZE));
    }

    #[test]
    fn peek_request_extracts_command_and_id_without_typing_args() {
        // `args.config` here is a bare string, NOT a real Config — the cheap
        // peek must still surface command + id without deserialising (or
        // allocating) the args tree. This is what lets the cap run BEFORE the
        // expensive Config build.
        // Wire name is SCREAMING_SNAKE_CASE (serde rename_all on IpcCommand).
        let line = r#"{"id":"x1","command":"SAVE_CONFIG","args":{"config":"not-a-real-config","base_generation":3}}"#;
        let peek = peek_request(line).expect("peek succeeds");
        assert_eq!(peek.id, "x1");
        assert!(matches!(peek.command, IpcCommand::SaveConfig));
    }

    #[test]
    fn peek_request_is_none_on_malformed_or_unknown_command() {
        assert!(peek_request(r#"{"id":"x","command":"#).is_none());
        assert!(peek_request(r#"{"id":"x","command":"NotACommand","args":{}}"#).is_none());
    }

    #[test]
    fn server_loop_decision_rejects_oversized_save_config_accepts_in_spec() {
        // Mirror the per-connection socket loop's exact decision: peek the
        // command, then apply the §D2.3 cap to the raw line length — BEFORE
        // any full deserialisation. A config body padded past the cap is
        // rejected; an in-spec one is not.
        let padded = "x".repeat(MAX_CONFIG_REQUEST_BYTES);
        let oversized =
            format!(r#"{{"id":"s","command":"SAVE_CONFIG","args":{{"config":"{padded}"}}}}"#);
        let peek = peek_request(&oversized).expect("peek");
        assert!(
            payload_cap_exceeded(&peek.command, oversized.len()),
            "oversized SAVE_CONFIG must be rejected pre-deserialisation"
        );

        let in_spec = r#"{"id":"s","command":"SAVE_CONFIG","args":{"config":{}}}"#;
        let peek = peek_request(in_spec).expect("peek");
        assert!(
            !payload_cap_exceeded(&peek.command, in_spec.len()),
            "a small in-spec SAVE_CONFIG must pass the cap"
        );
    }

    #[test]
    fn test_create_success_response() {
        let response = create_success_response("test-456", Some(json!({"message": "pong"})));
        assert_eq!(response.id, "test-456");
        assert!(matches!(response.status, ResponseStatus::Success));
        assert!(response.error.is_none());
        assert!(response.data.is_some());
    }

    #[test]
    fn test_create_error_response() {
        let response = create_error_response(
            "test-789",
            IpcErrorCode::InvalidJson,
            "Invalid JSON".to_string(),
            None,
        );
        assert_eq!(response.id, "test-789");
        assert!(matches!(response.status, ResponseStatus::Error));
        assert!(response.error.is_some());
        assert_eq!(response.error.unwrap().code, 1001);
    }

    #[tokio::test]
    async fn test_socket_path() {
        let path = get_socket_path().unwrap();

        #[cfg(unix)]
        {
            // Unix platforms - should end with conductor.sock
            assert!(path.ends_with("conductor.sock"));

            // Verify the path is NOT in /tmp (security requirement)
            assert!(
                !path.starts_with("/tmp"),
                "Socket path should not be in /tmp for security reasons"
            );
        }

        #[cfg(windows)]
        // Use expect() with message
        assert_eq!(
            path.to_str()
                .expect("Windows named pipe path should be valid UTF-8"),
            r"\\.\pipe\conductor"
        );
    }

    #[test]
    fn test_max_request_size_constant() {
        // Verify the constant is set to 1MB
        assert_eq!(MAX_REQUEST_SIZE, 1_048_576);
        assert_eq!(MAX_REQUEST_SIZE, 1024 * 1024);
    }

    #[test]
    fn test_request_size_enforcement() {
        // Create a request that exceeds MAX_REQUEST_SIZE
        let oversized_request = "x".repeat(MAX_REQUEST_SIZE + 1);
        assert!(oversized_request.len() > MAX_REQUEST_SIZE);

        // Create a request within limits
        let valid_request = r#"{"id":"test-123","command":"PING","args":{}}"#;
        assert!(valid_request.len() < MAX_REQUEST_SIZE);
    }

    // ── Push-based event monitoring tests ──

    #[test]
    fn test_subscribe_events_command_parses() {
        let json = r#"{"id":"sub-1","command":"SUBSCRIBE_EVENTS","args":{}}"#;
        let request = parse_request(json).unwrap();
        assert_eq!(request.id, "sub-1");
        assert!(matches!(request.command, IpcCommand::SubscribeEvents));
    }

    #[tokio::test]
    async fn test_handle_event_subscription_streams_events() {
        use tokio::io::AsyncBufReadExt;
        let (client_stream, server_stream) = UnixStream::pair().unwrap();

        let (event_tx, _) = broadcast::channel::<MonitorEvent>(64);

        let (_, server_writer) = server_stream.into_split();
        let mut writer = server_writer;

        // Subscribe BEFORE spawning handler (matches production code flow)
        let rx = event_tx.subscribe();
        let handle = tokio::spawn(async move {
            handle_event_subscription(&mut writer, rx).await;
        });

        // Small yield to let handler start its recv() loop
        tokio::task::yield_now().await;

        let _ = event_tx.send(MonitorEvent {
            timestamp_ms: 1000,
            event_type: "note_on".to_string(),
            note: Some(60),
            velocity: Some(100),
            ..Default::default()
        });

        // Read from client side
        let mut reader = BufReader::new(client_stream);
        let mut line = String::new();

        let read_result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            reader.read_line(&mut line),
        )
        .await;
        assert!(read_result.is_ok(), "Should receive event within timeout");

        let events: Vec<MonitorEvent> = serde_json::from_str(line.trim()).unwrap();
        assert!(!events.is_empty());
        assert_eq!(events[0].event_type, "note_on");

        // Clean up
        drop(reader);
        let _ = tokio::time::timeout(std::time::Duration::from_millis(200), handle).await;
    }

    #[tokio::test]
    async fn test_handle_event_subscription_ends_on_disconnect() {
        let (client_stream, server_stream) = UnixStream::pair().unwrap();
        let (event_tx, _) = broadcast::channel::<MonitorEvent>(16);

        let (_, server_writer) = server_stream.into_split();
        let mut writer = server_writer;

        // Subscribe before spawning (matches production flow)
        let rx = event_tx.subscribe();
        let handle = tokio::spawn(async move {
            handle_event_subscription(&mut writer, rx).await;
        });

        // Let handler start its recv() loop
        tokio::task::yield_now().await;

        // Drop the client side BEFORE sending — the write in the handler will fail
        drop(client_stream);

        // Send an event to unblock recv().await — the subsequent write will fail
        let _ = event_tx.send(MonitorEvent {
            timestamp_ms: 1000,
            event_type: "note_on".to_string(),
            ..Default::default()
        });

        let result = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
        assert!(
            result.is_ok(),
            "Subscription handler should exit on disconnect"
        );
    }

    #[tokio::test]
    async fn test_broadcast_recv_returns_closed_when_all_senders_dropped() {
        // Verify the broadcast channel behavior that handle_event_subscription relies on:
        // when all senders are dropped, recv() returns RecvError::Closed
        let (event_tx, _) = broadcast::channel::<MonitorEvent>(16);
        let mut rx = event_tx.subscribe();

        drop(event_tx); // Drop the only sender

        match rx.recv().await {
            Err(broadcast::error::RecvError::Closed) => {} // expected
            other => panic!("Expected Closed, got {:?}", other),
        }
    }

    /// The daemon may write a response AND a following streamed line in one
    /// syscall; the bytes past the response newline must survive
    /// `send_request` → `into_reader`. With the old
    /// per-request `BufReader`, the coalesced event line was buffered into the
    /// temporary and dropped; the persistent reader preserves it.
    #[tokio::test]
    async fn ipc_client_into_reader_preserves_bytes_buffered_during_send_request() {
        let (mut server, client_stream) = UnixStream::pair().unwrap();
        let mut client = IpcClient {
            reader: BufReader::new(client_stream),
        };

        let server_task = tokio::spawn(async move {
            // Read the request line, then write the response + a coalesced event
            // line in a SINGLE write so the client reads past the response.
            let mut req = String::new();
            BufReader::new(&mut server)
                .read_line(&mut req)
                .await
                .unwrap();
            server
                .write_all(b"{\"id\":\"x\",\"status\":\"success\"}\n{\"event\":1}\n")
                .await
                .unwrap();
            server.flush().await.unwrap();
            // Hold the connection open so the client doesn't hit EOF early.
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        });

        let resp = client
            .send_request(IpcRequest {
                id: "x".to_string(),
                command: IpcCommand::Ping,
                args: json!({}),
            })
            .await
            .unwrap();
        assert!(matches!(resp.status, ResponseStatus::Success));

        // The coalesced event line must still be readable from the reader.
        let mut reader = client.into_reader();
        let mut event = String::new();
        reader.read_line(&mut event).await.unwrap();
        assert!(
            event.contains("event"),
            "buffered-ahead event line was lost: {event:?}"
        );
        server_task.abort();
    }
}
