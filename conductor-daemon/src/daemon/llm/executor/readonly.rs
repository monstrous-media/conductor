// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ReadOnly-tier tool execution.

use super::*;

impl ToolExecutor {
    /// Execute a ReadOnly tool immediately
    pub(super) async fn execute_readonly(
        &self,
        tool_name: &str,
        arguments: Option<Value>,
    ) -> ExecutionResult {
        let start_time = Instant::now();
        let args_json = arguments.as_ref().map(|a| a.to_string());
        let risk_tier = get_tool_risk_tier(tool_name);
        let audit_tier = tool_risk_to_audit_risk(&risk_tier);

        // ADR-025 Phase 1: control-state tools route to a dedicated handler
        // that needs the live Arc<PhysicalControlStateStore> rather than a
        // serialized status snapshot. Fall through to the standard path
        // otherwise.
        let control_state_ref = self
            .daemon_state_refs
            .as_ref()
            .map(|r| r.control_state.as_ref());
        if let Some(result) = crate::daemon::llm::control_state_tools::handle_readonly(
            tool_name,
            arguments.as_ref(),
            control_state_ref,
        ) {
            // Audit log and return.
            if let Some(ref logger) = self.audit_logger {
                let execution_time = start_time.elapsed();
                let result_json = serde_json::to_string(&result).ok();
                if result.is_error == Some(true) {
                    let error_msg = result
                        .content
                        .first()
                        .map(|c| match c {
                            crate::daemon::mcp_types::ToolContent::Text { text } => text.clone(),
                            crate::daemon::mcp_types::ToolContent::Image { .. } => {
                                "Image error".to_string()
                            }
                            crate::daemon::mcp_types::ToolContent::Resource { text, .. } => {
                                text.clone().unwrap_or_else(|| "Resource error".to_string())
                            }
                        })
                        .unwrap_or_else(|| "Unknown error".to_string());
                    logger.log_tool_error(
                        tool_name,
                        audit_tier,
                        args_json.as_deref(),
                        &error_msg,
                        execution_time,
                        Some(UserContext::local_user()),
                    );
                } else {
                    logger.log_tool_complete(
                        tool_name,
                        audit_tier,
                        args_json.as_deref(),
                        result_json.as_deref(),
                        execution_time,
                        Some(UserContext::local_user()),
                    );
                }
            }
            return ExecutionResult::Success { result };
        }

        // ADR-026 Phase 2: SysEx identity ReadOnly tools. Both pull
        // from the shared `ProbeCoordinator` cache populated by
        // `conductor_probe_device_identity` / probe-on-connect (Phase 3).
        if matches!(
            tool_name,
            "conductor_get_device_identity" | "conductor_list_device_identities"
        ) {
            let Some(refs) = self.daemon_state_refs.as_ref() else {
                return ExecutionResult::Error {
                    message:
                        "Daemon state refs not available — identity lookup requires running daemon"
                            .to_string(),
                };
            };
            let coord = &refs.probe_coordinator;
            let result = match tool_name {
                "conductor_get_device_identity" => {
                    let port_name = arguments
                        .as_ref()
                        .and_then(|a| a.get("port_name"))
                        .and_then(|v| v.as_str());
                    let Some(port) = port_name else {
                        return ExecutionResult::Error {
                            message: "Missing required argument: port_name".to_string(),
                        };
                    };
                    // Phase 3.A: cache returns (identity, confidence).
                    // Surface confidence in the response shape so GUI
                    // and LLM callers can render the badge without a
                    // second round-trip. `null` for both when unprobed.
                    let cached = coord.cached(port);
                    let payload = json!({
                        "port_name": port,
                        "identity": cached.as_ref().map(|(id, _)| id),
                        "confidence": cached.as_ref().map(|(_, c)| c),
                    });
                    crate::daemon::mcp_types::ToolCallResult::json(&payload)
                }
                "conductor_list_device_identities" => {
                    let snapshot = coord.snapshot();
                    let entries: Vec<serde_json::Value> = snapshot
                        .into_iter()
                        .map(|(port, identity, confidence)| {
                            json!({
                                "port_name": port,
                                "identity": identity,
                                "confidence": confidence,
                            })
                        })
                        .collect();
                    let payload = json!({ "identities": entries });
                    crate::daemon::mcp_types::ToolCallResult::json(&payload)
                }
                _ => unreachable!(),
            };
            if let Some(ref logger) = self.audit_logger {
                let execution_time = start_time.elapsed();
                let result_json = serde_json::to_string(&result).ok();
                logger.log_tool_complete(
                    tool_name,
                    audit_tier,
                    args_json.as_deref(),
                    result_json.as_deref(),
                    execution_time,
                    Some(UserContext::local_user()),
                );
            }
            return ExecutionResult::Success { result };
        }

        // (ADR-035 follow-up: `conductor_list_connectors` was removed —
        // its runtime connectors+status view is a subset of
        // `conductor_get_resolved_routing_graph` (below), which reports the same
        // per-connector `connected`/`bound_port` plus route resolution.)

        // ADR-042 Phase B-early — B.7 visibility: report the
        // network-approval HMAC key's rotation status. Report-only and
        // infallible at the tool level — a missing key / unavailable backend
        // degrades to a structured "unavailable" payload (mirroring
        // `conductorctl security status`), never an ExecutionResult::Error: a
        // status probe must not hard-fail. Reads the OS keychain directly, so
        // it needs no `daemon_state_refs`.
        if tool_name == "conductor_security_status" {
            let payload = crate::daemon::llm::security_status::payload().await;
            let result = crate::daemon::mcp_types::ToolCallResult::json(&payload);
            if let Some(ref logger) = self.audit_logger {
                let execution_time = start_time.elapsed();
                let result_json = serde_json::to_string(&result).ok();
                logger.log_tool_complete(
                    tool_name,
                    audit_tier,
                    args_json.as_deref(),
                    result_json.as_deref(),
                    execution_time,
                    Some(UserContext::local_user()),
                );
            }
            return ExecutionResult::Success { result };
        }

        // ADR-031 §3.4 / Phase 1 — runtime-resolved routing graph.
        // The canonical view the GUI should render: connectors from the
        // live registry (bindings lowered, explicit `[[connectors]]`
        // folded in) and routes resolved against that registry so
        // `from_missing`/`to_missing` surface validator-bypassed paths.
        // Distinct from `conductor_get_routing_graph` which returns the
        // declared/config view. Extends line 855's resolver-of-record
        // principle from action execution to graph rendering. Requires
        // `daemon_state_refs` (reads the live registry / input manager).
        if tool_name == "conductor_get_resolved_routing_graph" {
            // Phase 2 Step C — read from AUTHORITATIVE sources
            // (input_manager + device_output_map), not from
            // `LiveConnector.bound_port` / `.connected`. The registry's
            // runtime fields are initialised to `None`/`false` by
            // `from_config` and nothing populates them — Bindings panel
            // showed devices connected while Routing Graph showed
            // everything unbound until this read site was rewritten.
            // See `resolved_routing_graph.rs` module doc + memory
            // `[[tdd-must-exercise-production-data-path]]` for the lesson.
            let Some(refs) = self.daemon_state_refs.as_ref() else {
                return ExecutionResult::Error {
                    message:
                        "Daemon state refs not available — routing graph requires running daemon"
                            .to_string(),
                };
            };
            // Lock-ordering note: acquire the locks-with-await BEFORE
            // the connector_registry
            // RwLockReadGuard so we never hold the registry guard
            // across an `.await`. Holding a guard across await opens
            // a deadlock window with any path that takes input_manager
            // first and then tries to acquire registry.
            //
            // Order:
            // 1. `device_output_map` — lock-free ArcSwap load.
            // 2. `input_manager.lock()` — short-lived Mutex; snapshot
            //    bindings and immediately drop.
            // 3. `live_config.load()` — lock-free ArcSwap.
            // 4. `connector_registry.read()` — held only across the
            //    SYNCHRONOUS response build below; no `.await` until
            //    the function returns.

            let output_map_arc = refs.device_output_map.load();

            // Loaded here (lock-free ArcSwap) so its endpoint set feeds
            // `reachable_output_ports` below; reused for `routes` at build time.
            let snap = self.live_config.load();

            // The live set of MIDI output ports, so an output endpoint
            // whose resolved port isn't actually present (e.g. an input-only
            // target) reports connected=false instead of a misleading green.
            // `enumerate_output_ports` is a synchronous midir scan, so offload it
            // to a blocking thread rather than stalling the Tokio runtime (same
            // pattern `enumerate_output_ports_async` uses). This `.await` is
            // BEFORE the connector_registry read guard below, so the
            // lock-ordering invariant (no `.await` while holding the guard) is
            // preserved. A join failure degrades safely to "no outputs available".
            //
            // A midir scan from THIS process does not list the virtual
            // output ports the daemon itself created, so `reachable_output_ports`
            // folds in the enabled MidiVirtualPort endpoints the daemon
            // materializes. Without it, a working daemon virtual output rendered
            // red in Endpoints + Routing Graph while Discovered Ports showed it
            // green (those views derive status from a separate enumeration). The
            // fold-in is gated on `virtual_ports_available()` so platforms that
            // cannot create virtual ports (Windows) don't report them connected.
            let enumerated =
                tokio::task::spawn_blocking(crate::daemon::output_resolver::enumerate_output_ports)
                    .await
                    .unwrap_or_default();
            let available_outputs: std::collections::HashSet<String> =
                crate::daemon::output_resolver::reachable_output_ports(
                    enumerated,
                    &snap.config.endpoints,
                    conductor_core::midi_output::MidiOutputManager::virtual_ports_available(),
                );

            let input_bindings: Vec<_> = {
                let manager_guard = refs.input_manager.lock().await;
                manager_guard
                    .as_ref()
                    .map(|mgr| {
                        // Build entries inside this closure so `is_device_enabled`
                        // can be queried while `mgr` is in scope.
                        mgr.get_device_bindings()
                            .into_iter()
                            .filter(|(_, _, _, is_configured)| *is_configured)
                            .map(|(device_id, port_name, connected, _)| {
                                // Runtime mute = NOT enabled (ADR-009 4b).
                                let muted = !mgr.is_device_enabled(&device_id);
                                crate::daemon::llm::resolved_routing_graph::InputBindingEntry {
                                    alias: device_id.as_str().to_string(),
                                    port_name,
                                    connected,
                                    muted,
                                }
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            };

            // Acquired last + dropped at end of scope; no `.await`
            // after this point.
            let registry = refs.connector_registry.read().await;

            let mut sorted: Vec<_> = registry
                .iter()
                .map(
                    |(_alias, live)| crate::daemon::llm::resolved_routing_graph::ConnectorView {
                        config: &live.config,
                    },
                )
                .collect();
            sorted.sort_by_key(|c| c.alias());

            let payload =
                crate::daemon::llm::resolved_routing_graph::build_resolved_routing_graph_response(
                    &sorted,
                    &input_bindings,
                    &output_map_arc,
                    &available_outputs,
                    &snap.config.routes,
                );
            let result = crate::daemon::mcp_types::ToolCallResult::json(&payload);
            if let Some(ref logger) = self.audit_logger {
                let execution_time = start_time.elapsed();
                let result_json = serde_json::to_string(&result).ok();
                logger.log_tool_complete(
                    tool_name,
                    audit_tier,
                    args_json.as_deref(),
                    result_json.as_deref(),
                    execution_time,
                    Some(UserContext::local_user()),
                );
            }
            return ExecutionResult::Success { result };
        }

        // ADR-036 D5: explain why each route fires or is
        // skipped for a hypothetical event, against the LIVE RouteEngine.
        if tool_name == "conductor_explain_route_match" {
            let Some(refs) = self.daemon_state_refs.as_ref() else {
                return ExecutionResult::Error {
                    message:
                        "Daemon state refs not available — explain_route_match requires running daemon"
                            .to_string(),
                };
            };
            let args = arguments.as_ref();
            let Some(active_mode) = args
                .and_then(|a| a.get("active_mode"))
                .and_then(|v| v.as_str())
            else {
                return ExecutionResult::Error {
                    message: "conductor_explain_route_match requires 'active_mode' (string)"
                        .to_string(),
                };
            };
            let Some(event) = args.and_then(|a| a.get("event")) else {
                return ExecutionResult::Error {
                    message: "conductor_explain_route_match requires an 'event' object".to_string(),
                };
            };
            let (source_alias, raw) = match parse_explain_event(event) {
                Ok(v) => v,
                Err(message) => return ExecutionResult::Error { message },
            };

            let route_engine = refs.route_engine.load();
            let explanations = route_engine.explain_route_match(&source_alias, &raw, active_mode);
            let payload = serde_json::json!({
                "device": source_alias,
                "active_mode": active_mode,
                "event": crate::daemon::dispatch_trace::summarize_midi(&raw),
                "routes": explanations,
                "note": if explanations.is_empty() {
                    serde_json::Value::String(format!(
                        "no routes have from = '{source_alias}' — nothing to evaluate"
                    ))
                } else {
                    serde_json::Value::Null
                },
            });
            let result = crate::daemon::mcp_types::ToolCallResult::json(&payload);
            if let Some(ref logger) = self.audit_logger {
                let execution_time = start_time.elapsed();
                let result_json = serde_json::to_string(&result).ok();
                logger.log_tool_complete(
                    tool_name,
                    audit_tier,
                    args_json.as_deref(),
                    result_json.as_deref(),
                    execution_time,
                    Some(UserContext::local_user()),
                );
            }
            return ExecutionResult::Success { result };
        }

        // ADR-036 §8: return the last N dispatch
        // decisions from the bounded trace ring buffer.
        if tool_name == "conductor_get_dispatch_trace" {
            let Some(refs) = self.daemon_state_refs.as_ref() else {
                return ExecutionResult::Error {
                    message:
                        "Daemon state refs not available — get_dispatch_trace requires running daemon"
                            .to_string(),
                };
            };
            // `last`: default 32, capped at 256.
            let last = arguments
                .as_ref()
                .and_then(|a| a.get("last"))
                .and_then(|v| v.as_u64())
                .map(|n| n.min(256) as usize)
                .unwrap_or(32);
            let entries = refs.dispatch_trace.last(last);
            let payload = serde_json::json!({
                "count": entries.len(),
                "requested": last,
                "entries": entries,
            });
            let result = crate::daemon::mcp_types::ToolCallResult::json(&payload);
            if let Some(ref logger) = self.audit_logger {
                let execution_time = start_time.elapsed();
                let result_json = serde_json::to_string(&result).ok();
                logger.log_tool_complete(
                    tool_name,
                    audit_tier,
                    args_json.as_deref(),
                    result_json.as_deref(),
                    execution_time,
                    Some(UserContext::local_user()),
                );
            }
            return ExecutionResult::Success { result };
        }

        // D4.A.3.3.B.1: snapshot config via LiveConfig (lock-free ArcSwap read).
        // Snap binding outlives `config_ref` so the &Config reference into the
        // snapshot Arc stays valid across the mcp_executor.execute() await.
        let snap = self.live_config.load();
        let config_ref = Some(snap.config.as_ref());

        // Fetch device data if this is the list_devices tool
        let devices_data = if tool_name == "conductor_list_devices" {
            Self::fetch_devices_data().await
        } else {
            None
        };

        // Get live status data from daemon state
        let status_data = if let Some(refs) = &self.daemon_state_refs {
            let state = refs.get_daemon_state().await;
            Some(state.to_status_json())
        } else {
            None
        };

        // ADR-022 D7: Pass event_stats from EngineManager via SharedDaemonStateRefs
        let event_stats_ref = self.daemon_state_refs.as_ref().map(|r| &*r.event_stats);
        let result = self
            .mcp_executor
            .execute(
                tool_name,
                arguments,
                status_data,
                devices_data,
                config_ref,
                event_stats_ref,
            )
            .await;

        // Audit log the execution
        if let Some(ref logger) = self.audit_logger {
            let execution_time = start_time.elapsed();
            let result_json = serde_json::to_string(&result).ok();

            if result.is_error == Some(true) {
                let error_msg = result
                    .content
                    .first()
                    .map(|c| match c {
                        crate::daemon::mcp_types::ToolContent::Text { text } => text.clone(),
                        crate::daemon::mcp_types::ToolContent::Image { .. } => {
                            "Image error".to_string()
                        }
                        crate::daemon::mcp_types::ToolContent::Resource { text, .. } => {
                            text.clone().unwrap_or_else(|| "Resource error".to_string())
                        }
                    })
                    .unwrap_or_else(|| "Unknown error".to_string());
                logger.log_tool_error(
                    tool_name,
                    audit_tier,
                    args_json.as_deref(),
                    &error_msg,
                    execution_time,
                    Some(UserContext::local_user()),
                );
            } else {
                logger.log_tool_complete(
                    tool_name,
                    audit_tier,
                    args_json.as_deref(),
                    result_json.as_deref(),
                    execution_time,
                    Some(UserContext::local_user()),
                );
            }
        }

        ExecutionResult::Success { result }
    }
}
