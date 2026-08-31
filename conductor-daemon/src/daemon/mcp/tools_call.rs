// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! `tools/call` dispatch for the MCP server (#2601 split from `mcp.rs`).

use crate::daemon::engine_manager::SharedDaemonStateRefs;
use crate::daemon::mcp_tools::McpToolExecutor;
use crate::daemon::mcp_types::{McpError, McpRequest, McpResponse, ToolCallParams};
use conductor_core::config::Config;
#[cfg(feature = "mcp-write")]
use conductor_core::device_intelligence::probe::ProbeOutcomeWire;
use serde_json::json;
use std::sync::Arc;
use tracing::{debug, warn};

use super::check_peer_tier_ceiling;

/// Wrap a `ToolCallResult` as a successful JSON-RPC response.
///
/// Defensive serialization (Council r2 on PR #2600): `ToolCallResult` is
/// `Serialize` over owned types so `to_value` is practically infallible,
/// but a panic here would crash the daemon from the socket path — degrade
/// to an internal error response instead (mirrors the
/// `execution_result_to_value` fallback idiom in engine_manager/helpers.rs).
fn tool_result_response(
    id: Option<crate::daemon::mcp_types::McpRequestId>,
    result: &crate::daemon::mcp_types::ToolCallResult,
) -> McpResponse {
    match serde_json::to_value(result) {
        Ok(value) => McpResponse::success(id, value),
        Err(e) => {
            warn!("failed to serialize ToolCallResult: {e}");
            McpResponse::error(
                id,
                McpError::internal_error(&format!("failed to serialize tool result: {e}")),
            )
        }
    }
}

/// Handle tools/call request
pub(crate) async fn handle_tools_call(
    request: &McpRequest,
    config: &Arc<crate::daemon::live_config::LiveConfig>,
    daemon_state: &Option<crate::daemon::types::DaemonState>,
    tool_executor: &McpToolExecutor,
    shared_state: &Option<Arc<SharedDaemonStateRefs>>,
    peer_ceiling: Option<crate::daemon::audit::AuditRiskTier>,
) -> McpResponse {
    use crate::daemon::mcp_tools::get_tool_risk_tier;
    use crate::daemon::mcp_types::{ToolCallResult, ToolRiskTier};
    #[cfg(feature = "mcp-write")]
    use conductor_core::identity::DeviceId;

    // Parse tool call params
    let params: ToolCallParams = match request.params.as_ref() {
        Some(p) => match serde_json::from_value(p.clone()) {
            Ok(params) => params,
            Err(e) => {
                return McpResponse::error(
                    request.id.clone(),
                    McpError::invalid_params(&e.to_string()),
                );
            }
        },
        None => {
            return McpResponse::error(
                request.id.clone(),
                McpError::invalid_params("Missing params"),
            );
        }
    };

    debug!(
        "Tool call: {} with args: {:?}",
        params.name, params.arguments
    );

    // v4.23.0: Special-case tools that need direct shared_state access (ADR-009 Phase 5)
    // Enforce risk tier: these tools are Stateful, not ReadOnly — verify before executing
    // ADR-045 D2 (#2492): in compositions without `mcp-write`, the MCP
    // socket carries ONLY the compiled (ReadOnly inspection) catalog.
    // Anything else — including write tools that exist in richer builds —
    // gets the standard "not available in this build" error naming the
    // Studio tier. The tool name is echoed from the request, never
    // embedded here (ADR-045 D3 negative artifact assertions).
    #[cfg(not(feature = "mcp-write"))]
    if !crate::daemon::mcp_tools::is_compiled_tool(&params.name) {
        let result = crate::daemon::mcp_tools::tool_unavailable_error(&params.name);
        return tool_result_response(request.id.clone(), &result);
    }

    let risk_tier = get_tool_risk_tier(&params.name);

    // #1311: per-client tier ceiling enforcement. Unregistered MCP
    // peers are clamped to ReadOnly per ADR-027 §D18; registered
    // peers are capped at their registered tier. Pre-fix the dispatch
    // synthesised `CallerContext::internal_trusted()` and let any
    // same-UID process invoke ConfigChange / HardwareIO tools.
    if let Err(reason) = check_peer_tier_ceiling(peer_ceiling, risk_tier) {
        warn!(
            "MCP peer denied {:?} tool '{}': {}",
            risk_tier, params.name, reason
        );
        return McpResponse::error(
            request.id.clone(),
            McpError::invalid_request(&format!(
                "Permission denied (#1311): {reason} — tool '{}' requires {:?} tier",
                params.name, risk_tier
            )),
        );
    }

    match params.name.as_str() {
        #[cfg(feature = "mcp-write")]
        "conductor_set_device_enabled" if risk_tier == ToolRiskTier::Stateful => {
            let result = match shared_state {
                Some(refs) => {
                    let device_id = params
                        .arguments
                        .as_ref()
                        .and_then(|a| a.get("device_id"))
                        .and_then(|v| v.as_str());
                    let enabled = params
                        .arguments
                        .as_ref()
                        .and_then(|a| a.get("enabled"))
                        .and_then(|v| v.as_bool());

                    match (device_id, enabled) {
                        (Some(id), Some(en)) => {
                            let dev_id = DeviceId::from_alias(id);
                            let mut guard = refs.input_manager.lock().await;
                            if let Some(ref mut mgr) = *guard {
                                mgr.set_device_enabled(&dev_id, en);
                                let action = if en { "enabled" } else { "muted" };
                                ToolCallResult::json(&json!({
                                    "device_id": id,
                                    "enabled": en,
                                    "message": format!("Device '{}' {}", id, action)
                                }))
                            } else {
                                ToolCallResult::error("Input manager not available")
                            }
                        }
                        _ => ToolCallResult::error(
                            "Missing required arguments: device_id (string), enabled (boolean)",
                        ),
                    }
                }
                None => ToolCallResult::error("Shared state not available"),
            };
            return tool_result_response(request.id.clone(), &result);
        }

        #[cfg(feature = "mcp-write")]
        "conductor_scan_ports" if risk_tier == ToolRiskTier::Stateful => {
            let result = match shared_state {
                Some(refs) => {
                    if let Err(e) = refs
                        .command_tx
                        .send(crate::daemon::types::DaemonCommand::HotPlugCheck)
                        .await
                    {
                        ToolCallResult::error(&format!("Failed to trigger rescan: {}", e))
                    } else {
                        ToolCallResult::json(&json!({
                            "message": "Port rescan triggered"
                        }))
                    }
                }
                None => ToolCallResult::error("Shared state not available"),
            };
            return tool_result_response(request.id.clone(), &result);
        }

        // Council review fix: wire conductor_switch_mode to actually send DaemonCommand::ModeChange.
        // DEPRECATED (ADR-040): switches the mode without touching any manual
        // lock — prefer conductor_set_mode (which can also lock). Kept as-is
        // (not delegated) so its existing behaviour + no-refs simulation path are
        // unchanged; the description is what was inaccurate (Copilot #2290).
        #[cfg(feature = "mcp-write")]
        "conductor_switch_mode" if risk_tier == ToolRiskTier::Stateful => {
            let mode_name = params
                .arguments
                .as_ref()
                .and_then(|a| a.get("mode"))
                .and_then(|v| v.as_str());

            let result = match (mode_name, shared_state) {
                (Some(name), Some(refs)) => {
                    let snap = config.load();
                    let mode_index = snap.config.modes.iter().position(|m| m.name == name);

                    match mode_index {
                        Some(idx) => {
                            if let Err(e) = refs
                                .command_tx
                                .send(crate::daemon::types::DaemonCommand::ModeChange {
                                    mode: name.to_string(),
                                })
                                .await
                            {
                                ToolCallResult::error(&format!(
                                    "Failed to trigger mode change: {}",
                                    e
                                ))
                            } else {
                                ToolCallResult::json(&json!({
                                    "mode_name": name,
                                    "mode_index": idx,
                                    "status": "switched"
                                }))
                            }
                        }
                        None => {
                            let available = snap
                                .config
                                .modes
                                .iter()
                                .map(|m| m.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", ");
                            ToolCallResult::error(&format!(
                                "Mode not found: {}. Available modes: {}",
                                name, available
                            ))
                        }
                    }
                }
                (None, _) => ToolCallResult::error("Missing required argument: mode"),
                (_, None) => ToolCallResult::error("Shared state not available"),
            };
            return tool_result_response(request.id.clone(), &result);
        }

        // ADR-040 D4 §4.2 (Slice 4c) — mode-lock tools. Like switch_mode they
        // need the live engine, reached via the daemon command channel; the
        // shared helpers in `mode_mcp` do the send/await.
        #[cfg(feature = "mcp-write")]
        "conductor_set_mode" => {
            let result = match shared_state {
                Some(refs) => {
                    crate::daemon::mode_mcp::set_mode(&refs.command_tx, params.arguments.as_ref())
                        .await
                }
                None => ToolCallResult::error("Shared state not available"),
            };
            return tool_result_response(request.id.clone(), &result);
        }
        #[cfg(feature = "mcp-write")]
        "conductor_unlock_mode" => {
            let result = match shared_state {
                Some(refs) => crate::daemon::mode_mcp::unlock_mode(&refs.command_tx).await,
                None => ToolCallResult::error("Shared state not available"),
            };
            return tool_result_response(request.id.clone(), &result);
        }
        "conductor_mode_status" if risk_tier == ToolRiskTier::ReadOnly => {
            let result = match shared_state {
                Some(refs) => crate::daemon::mode_mcp::mode_status(&refs.command_tx).await,
                None => ToolCallResult::error("Shared state not available"),
            };
            return tool_result_response(request.id.clone(), &result);
        }

        // ADR-026 Phase 2: SysEx identity tools route through shared_state
        // for both daemon-internal MCP clients (this path, Unix socket
        // JSON-RPC) and the LLM executor path (Tauri / chat). Without
        // this branch the Unix-socket path would always return the
        // "must be executed via the LLM executor path" sentinel error.
        #[cfg(feature = "mcp-write")]
        "conductor_probe_device_identity" if risk_tier == ToolRiskTier::HardwareIO => {
            let result = match shared_state {
                Some(refs) => {
                    let port_name = params
                        .arguments
                        .as_ref()
                        .and_then(|a| a.get("port_name"))
                        .and_then(|v| v.as_str());
                    match port_name {
                        Some(port) => {
                            let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
                            if let Err(e) = refs
                                .command_tx
                                .send(crate::daemon::types::DaemonCommand::ProbeDeviceIdentity {
                                    port_name: port.to_string(),
                                    response_tx: resp_tx,
                                })
                                .await
                            {
                                ToolCallResult::error(&format!(
                                    "Failed to dispatch probe command: {}",
                                    e
                                ))
                            } else {
                                // Bound the wait so a stalled engine
                                // loop surfaces a clear error instead
                                // of hanging the MCP client. Phase 1.A
                                // serialises probes globally with a
                                // 1 s reply timeout, so queue_wait
                                // grows roughly linearly with
                                // concurrent probes — probe-on-connect
                                // (Phase 3) can dispatch one per
                                // device in a hot-plug burst. 30 s
                                // covers ~25-device bursts comfortably
                                // while still failing fast for
                                // genuine deadlocks.
                                match tokio::time::timeout(
                                    std::time::Duration::from_secs(30),
                                    resp_rx,
                                )
                                .await
                                {
                                    Ok(Ok(probe_outcome)) => {
                                        // Phase 3.B.1: `probe_outcome` is the
                                        // overall `Result<ProbeResult,
                                        // ProbeStartError>` — NOT a
                                        // `ProbeResult` — so the binding
                                        // name reflects the wider type.
                                        // Collapse via `ProbeOutcomeWire`
                                        // so the wire format matches Phase
                                        // 2 callers (flat `{"status":
                                        // "..."}` shape).
                                        let wire = ProbeOutcomeWire::from(probe_outcome);
                                        match serde_json::to_value(&wire) {
                                            Ok(value) => ToolCallResult::json(&value),
                                            Err(e) => ToolCallResult::error(&format!(
                                                "Failed to serialise probe outcome: {}",
                                                e
                                            )),
                                        }
                                    }
                                    Ok(Err(_)) => ToolCallResult::error(
                                        "Probe response channel closed before result arrived",
                                    ),
                                    Err(_) => ToolCallResult::error(
                                        "Probe timed out waiting for daemon response (>30s)",
                                    ),
                                }
                            }
                        }
                        None => ToolCallResult::error("Missing required argument: port_name"),
                    }
                }
                None => ToolCallResult::error("Shared state not available"),
            };
            return tool_result_response(request.id.clone(), &result);
        }

        "conductor_get_device_identity" if risk_tier == ToolRiskTier::ReadOnly => {
            let result = match shared_state {
                Some(refs) => {
                    let port_name = params
                        .arguments
                        .as_ref()
                        .and_then(|a| a.get("port_name"))
                        .and_then(|v| v.as_str());
                    match port_name {
                        Some(port) => {
                            // Phase 3.A: surface confidence alongside
                            // identity so GUI/LLM callers can render
                            // the badge state. `null` for both when
                            // unprobed.
                            let cached = refs.probe_coordinator.cached(port);
                            ToolCallResult::json(&json!({
                                "port_name": port,
                                "identity": cached.as_ref().map(|(id, _)| id),
                                "confidence": cached.as_ref().map(|(_, c)| c),
                            }))
                        }
                        None => ToolCallResult::error("Missing required argument: port_name"),
                    }
                }
                None => ToolCallResult::error("Shared state not available"),
            };
            return tool_result_response(request.id.clone(), &result);
        }

        "conductor_list_device_identities" if risk_tier == ToolRiskTier::ReadOnly => {
            let result = match shared_state {
                Some(refs) => {
                    let entries: Vec<serde_json::Value> = refs
                        .probe_coordinator
                        .snapshot()
                        .into_iter()
                        .map(|(port, identity, confidence)| {
                            json!({
                                "port_name": port,
                                "identity": identity,
                                "confidence": confidence,
                            })
                        })
                        .collect();
                    ToolCallResult::json(&json!({ "identities": entries }))
                }
                None => ToolCallResult::error("Shared state not available"),
            };
            return tool_result_response(request.id.clone(), &result);
        }

        // ADR-031 P4 § 6.2 / #1144 slice 4 — live per-connector
        // metrics. Reads from the runtime `connector_registry` on
        // `shared_state`, not from config; required because
        // `McpToolExecutor` is stateless and can't reach the live
        // registry. Returns 0 metrics across the board if shared_state
        // is unavailable (test harness, daemon startup race) rather
        // than erroring — readers can distinguish "no activity" from
        // "no registry" via the `state_available` flag.
        "conductor_get_connector_metrics" if risk_tier == ToolRiskTier::ReadOnly => {
            let result = match shared_state {
                Some(refs) => {
                    let registry = refs.connector_registry.read().await;
                    let aliases: Vec<String> =
                        registry.iter().map(|(alias, _)| alias.clone()).collect();
                    let now = std::time::Instant::now();
                    let connectors: Vec<serde_json::Value> = aliases
                        .into_iter()
                        .filter_map(|alias| {
                            registry.get(&alias).map(|connector| {
                                let m = &connector.metrics;
                                // `last_activity_ago_ms`: millis since the
                                // last forward, or null if never active.
                                // Reported as elapsed rather than absolute
                                // so callers don't have to do clock math
                                // against an `Instant` that has no public
                                // serializer.
                                let last_activity_ago_ms = m
                                    .last_activity
                                    .map(|t| now.saturating_duration_since(t).as_millis() as u64);
                                json!({
                                    "alias": alias,
                                    "total_messages": m.total_messages,
                                    "throughput_msgs_per_sec": registry.current_throughput(&alias),
                                    "error_count": m.error_count,
                                    "last_activity_ago_ms": last_activity_ago_ms,
                                })
                            })
                        })
                        .collect();
                    ToolCallResult::json(&json!({
                        "state_available": true,
                        "connectors": connectors,
                    }))
                }
                None => ToolCallResult::json(&json!({
                    "state_available": false,
                    "connectors": [],
                })),
            };
            return tool_result_response(request.id.clone(), &result);
        }

        _ => {} // Fall through to ConfigChange dispatch / generic executor
    }

    // ADR-031 P3 / #1274 slice 20: ConfigChange dispatch over the
    // daemon's Unix MCP socket. The generic `McpToolExecutor` below
    // only handles ReadOnly tools, plus the few Stateful/HardwareIO
    // tools special-cased above. ConfigChange tools (every
    // `conductor_create_*` / `conductor_update_*` / `conductor_delete_*`
    // / `conductor_batch_changes`) are advertised in tools/list (since
    // slice 12 for `conductor_batch_changes`, longer for the singletons)
    // but would otherwise fall through to "Unknown tool" at the generic
    // executor's match arm — breaking external MCP clients (Cursor,
    // Claude Code over MCP, etc.) that can see the schema but cannot
    // invoke mutations.
    //
    // This generic arm delegates ALL ConfigChange tools to the LLM
    // `ToolExecutor` (which already routes them through `execute_config_change`
    // → `PlanCreated`), then auto-applies the plan. See
    // `handle_config_change_over_mcp` for the contract.
    // ADR-045 D1/D3 (#2492): ConfigChange-over-MCP is the core `mcp-write`
    // surface — official artifacts never compile it, so the MCP socket
    // cannot mutate config even in the Studio bundle (Council R1 #2).
    #[cfg(feature = "mcp-write")]
    if risk_tier == ToolRiskTier::ConfigChange {
        return handle_config_change_over_mcp(request, &params, config).await;
    }

    // D4.A.3.3.B.1: snapshot config via LiveConfig (lock-free ArcSwap read).
    // Snapshot wrapped in Option for downstream API compat — LiveConfig is
    // always loaded post-EngineManager::new, so this is effectively always Some.
    let config_snapshot: Option<Config> = Some((*config.load().config).clone());

    // Get daemon state lazily — only compute what the requested tool needs
    let status_data = daemon_state.as_ref().map(|state| state.to_status_json());

    // Enumerate MIDI devices for tools that need port data (avoids 100ms warmup on every call)
    let devices_data = if params.name == "conductor_list_devices"
        || params.name == "conductor_list_discovered_ports"
    {
        match daemon_state.as_ref() {
            Some(state) => {
                let midi_devices =
                    crate::daemon::device_utils::enumerate_midi_devices_fresh_async().await;
                Some(state.to_devices_json(midi_devices))
            }
            None => None,
        }
    } else {
        None
    };

    // Execute tool (no lock held across this await)
    let event_stats_ref = shared_state.as_ref().map(|s| &*s.event_stats);
    let result = tool_executor
        .execute(
            &params.name,
            params.arguments,
            status_data,
            devices_data,
            config_snapshot.as_ref(),
            event_stats_ref,
        )
        .await;

    tool_result_response(request.id.clone(), &result)
}

/// ADR-031 P3 / #1274 slice 20 — handle ConfigChange-tier tools over
/// the daemon's Unix MCP socket.
///
/// ## Contract
///
/// **Auto-apply** semantics. MCP clients have no UI to approve a plan,
/// and chaining a second `tools/call` for explicit apply would just be
/// busywork that fragments the audit trail. So this dispatch:
///
/// 1. Constructs a per-call `ToolExecutor` from the shared `LiveConfig`
///    (the same hot-reload primitive the in-process GUI executor uses).
/// 2. Calls `executor.execute(name, args, ctx)` — the executor routes
///    ConfigChange tools through its internal `execute_config_change`,
///    which always returns `ExecutionResult::PlanCreated { plan }`.
/// 3. Immediately calls `executor.apply_plan(plan.id)`, returning both
///    the plan and the apply outcome as a single `ToolCallResult::json`.
///
/// The response payload shape is intentional — clients see the full
/// audit trail (plan id, change descriptions, change count) in one
/// response, so they can replay or echo without a second round-trip.
///
/// ## Trust model
///
/// `CallerContext::internal_trusted()` is passed because the Unix MCP
/// socket has 0600 permissions (current user only). The OS-level trust
/// bar is cleared by the time we reach this dispatch; gate-level
/// `Untrusted` (the `synthetic_unpinned` path used by the IPC accept
/// loop) would `Deny` ConfigChange tools and re-break the gap #1274
/// closes. This matches the precedent set by the in-process
/// `conductor_*_plugin` daemon-internal arms in
/// `executor.rs::execute_internal_arms`.
///
/// ## Per-call executor — known limitation
///
/// We construct a fresh `ToolExecutor` per request rather than sharing
/// one via `SharedDaemonStateRefs`. Trade-offs:
///
/// - **Pro**: zero invasive changes to `engine_manager` construction.
/// - **Con**: audit logger, rate limiter, undo stack are NOT shared
///   with the GUI executor instance. Each MCP socket call starts with
///   a fresh undo stack scoped to its `ToolExecutor` lifetime.
///
/// Sharing the executor instance is a follow-up if continuity across
/// transports becomes a hard requirement.
#[cfg(feature = "mcp-write")]
async fn handle_config_change_over_mcp(
    request: &McpRequest,
    params: &ToolCallParams,
    config: &Arc<crate::daemon::live_config::LiveConfig>,
) -> McpResponse {
    use crate::daemon::llm::executor::{ExecutionResult, ToolExecutor};
    use crate::daemon::mcp_types::ToolCallResult;
    use crate::security::CallerContext;

    let executor = ToolExecutor::new(config.clone());
    let ctx = CallerContext::internal_trusted();

    let result = executor
        .execute(&params.name, params.arguments.clone(), Some(&ctx))
        .await;

    let tool_result = match result {
        ExecutionResult::PlanCreated { plan } => {
            let plan_id = plan.id;
            let plan_description = plan.description.clone();
            let plan_changes = plan.changes.clone();
            let change_descriptions = plan.change_descriptions.clone();
            match executor.apply_plan(&plan_id).await {
                Ok(changes_count) => ToolCallResult::json(&json!({
                    "status": "applied",
                    "plan_id": plan_id.to_string(),
                    "changes_count": changes_count,
                    "plan_description": plan_description,
                    "plan_changes": plan_changes,
                    "change_descriptions": change_descriptions,
                })),
                Err(e) => {
                    ToolCallResult::error(&format!("Plan {plan_id} created but apply failed: {e}"))
                }
            }
        }
        ExecutionResult::Success { result } => result,
        ExecutionResult::Logged { result, .. } => result,
        ExecutionResult::Error { message } => ToolCallResult::error(&message),
        ExecutionResult::RateLimited {
            tier,
            current,
            limit,
            retry_after_secs,
        } => ToolCallResult::error(&format!(
            "Rate-limited at tier {tier:?}: {current}/{limit}, retry in {retry_after_secs}s"
        )),
        ExecutionResult::HardwareIoConfirmation { status, tool_name } => {
            // HardwareIO tools shouldn't reach this arm (risk_tier
            // gate is ConfigChange-only), but degrade gracefully if
            // a future risk-tier reclassification accidentally
            // routes one here.
            ToolCallResult::error(&format!(
                "HardwareIO confirmation flow not supported over the MCP socket \
                 (tool={tool_name}, status={status:?})"
            ))
        }
    };

    tool_result_response(request.id.clone(), &tool_result)
}

// Device enumeration moved to device_utils::enumerate_midi_devices_fresh() (v4.17.0, #104)
