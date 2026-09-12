// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Stateful-tier tool execution.

use super::*;

impl ToolExecutor {
    /// Execute a Stateful tool with logging
    pub(super) async fn execute_stateful(
        &self,
        tool_name: &str,
        arguments: Option<Value>,
    ) -> ExecutionResult {
        let start_time = Instant::now();
        let args_json = arguments.as_ref().map(|a| a.to_string());
        let risk_tier = get_tool_risk_tier(tool_name);
        let audit_tier = tool_risk_to_audit_risk(&risk_tier);

        // For now, stateful tools are handled same as readonly but with logging
        // In Phase 2, MIDI Learn will be implemented here
        // D4.A.3.3.B.1: snapshot config via LiveConfig (lock-free).
        let snap = self.live_config.load();
        let config_ref = Some(snap.config.as_ref());

        // Execute the tool
        let result = self
            .execute_stateful_tool(tool_name, arguments.clone(), config_ref)
            .await;
        let execution_time = start_time.elapsed();

        // Create log entry
        let log_entry = LogEntry {
            id: Uuid::new_v4(),
            tool_name: tool_name.to_string(),
            arguments: arguments.clone(),
            timestamp: chrono::Utc::now(),
            result_summary: self.summarize_result(&result),
        };

        // Store log entry
        {
            let mut log = self.execution_log.write().await;
            log.push(log_entry.clone());
            info!("Logged stateful tool execution: {}", tool_name);
        }

        // Audit log the execution (P4-04)
        if let Some(ref logger) = self.audit_logger {
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

        ExecutionResult::Logged { result, log_entry }
    }

    /// Execute a stateful tool
    pub(super) async fn execute_stateful_tool(
        &self,
        tool_name: &str,
        arguments: Option<Value>,
        _config: Option<&Config>,
    ) -> ToolCallResult {
        // ADR-025 Phase 1: control-state reset routes to its dedicated
        // handler, which needs the live store Arc (not a snapshot).
        let control_state_ref = self
            .daemon_state_refs
            .as_ref()
            .map(|r| r.control_state.as_ref());
        if let Some(result) = crate::daemon::llm::control_state_tools::handle_stateful(
            tool_name,
            arguments.as_ref(),
            control_state_ref,
        ) {
            return result;
        }

        match tool_name {
            "conductor_start_learn" | "conductor_start_midi_learn" => {
                let timeout_seconds = arguments
                    .as_ref()
                    .and_then(|a| a.get("timeout_seconds"))
                    .and_then(|t| t.as_u64())
                    .unwrap_or(30);

                // Check if MIDI Learn state is available
                match &self.midi_learn_active {
                    Some(active) => {
                        // Bump the session generation BEFORE flipping
                        // active=true so any prior timer that wakes
                        // during this start window sees a stale gen
                        // and skips its swap. The
                        // returned value is the unique ID for this
                        // session; the spawned timer captures it and
                        // checks for match before stopping.
                        let my_gen = self.midi_learn_session_gen.fetch_add(1, Ordering::SeqCst) + 1;

                        // Set MIDI Learn active. `swap` returns the
                        // prior value so the tool result can tell the
                        // LLM whether it just preempted an active
                        // session (without this signal the LLM
                        // rapid-restarts learn without acknowledging
                        // the restart to the user).
                        let was_already_active = active.swap(true, Ordering::SeqCst);

                        // Spawn the daemon-side auto-stop timer. The
                        // LLM agent loop can't reliably "remember to call
                        // stop later" — it has no async timer and is
                        // stateless across turns — so the deadline lives
                        // here. A subsequent start aborts this timer and
                        // installs a fresh one (extending the deadline);
                        // an explicit conductor_stop_learn aborts it
                        // (preventing the timer from firing late and
                        // stopping a fresh session that came after).
                        {
                            let mut timer_guard = self.midi_learn_timer.lock().await;
                            if let Some(prev) = timer_guard.take() {
                                prev.abort();
                            }
                            let active_for_timer = active.clone();
                            let session_gen_for_timer = self.midi_learn_session_gen.clone();
                            *timer_guard = Some(tokio::spawn(async move {
                                tokio::time::sleep(Duration::from_secs(timeout_seconds)).await;
                                // Generation check: if a
                                // newer session has replaced us, the counter
                                // has advanced. Skip the swap entirely so
                                // we don't stop the freshly-started session.
                                if session_gen_for_timer.load(Ordering::SeqCst) != my_gen {
                                    return;
                                }
                                // `swap` is atomic — only the first writer
                                // (this timer or an explicit stop) sees the
                                // previous true. If `false` was already set
                                // by an explicit stop, this no-ops.
                                if active_for_timer.swap(false, Ordering::SeqCst) {
                                    info!(
                                        "MIDI Learn mode auto-stopped by daemon timeout ({}s)",
                                        timeout_seconds
                                    );
                                }
                            }));
                        }

                        info!(
                            "MIDI Learn mode started via LLM tool (timeout: {}s, daemon-enforced{})",
                            timeout_seconds,
                            if was_already_active {
                                ", restarted"
                            } else {
                                ""
                            }
                        );

                        let (message, instructions) = if was_already_active {
                            (
                                "MIDI Learn mode RESTARTED — your previous start_learn call was preempted by this one. The timer is now fresh. NOTE: events captured during the prior session remain in the buffer; the next conductor_stop_midi_learn will return events accumulated across both sessions. Press a button/pad on your controller.".to_string(),
                                "You MUST acknowledge to the user in chat that you restarted Learn (e.g. 'Restarting Learn — try again') before doing anything else. Otherwise call conductor_stop_midi_learn when input is received. The daemon will auto-stop after timeout_seconds even with no explicit stop call.".to_string(),
                            )
                        } else {
                            (
                                "MIDI Learn mode started. Press a button/pad on your controller.".to_string(),
                                "Use conductor_stop_midi_learn to stop and retrieve captured events. The daemon will auto-stop the session after timeout_seconds even if no explicit stop call arrives.".to_string(),
                            )
                        };

                        ToolCallResult::json(&json!({
                            "success": true,
                            "was_already_active": was_already_active,
                            "message": message,
                            "timeout_seconds": timeout_seconds,
                            "instructions": instructions,
                        }))
                    }
                    None => {
                        // Graceful fallback when running in test mode or standalone
                        warn!(
                            "MIDI Learn start requested but state not connected to engine manager"
                        );
                        ToolCallResult::json(&json!({
                            "success": true,
                            "message": "MIDI Learn mode started (simulation mode - no device connected).",
                            "timeout_seconds": timeout_seconds,
                            "simulation": true,
                            "instructions": "Connect to a device via the daemon for full MIDI Learn functionality."
                        }))
                    }
                }
            }
            "conductor_stop_learn" | "conductor_stop_midi_learn" => {
                // Check if MIDI Learn state is available
                match (&self.midi_learn_active, &self.midi_learn_events) {
                    (Some(active), Some(events)) => {
                        // Bump session generation so any pending timer's
                        // wake check fails. Belt-and-
                        // braces with the abort() below — even if abort
                        // loses the race, the gen check makes the timer's
                        // body a no-op.
                        self.midi_learn_session_gen.fetch_add(1, Ordering::SeqCst);

                        // Stop MIDI Learn
                        active.store(false, Ordering::SeqCst);

                        // Cancel the auto-stop timer so it doesn't
                        // fire late and stop a fresh session that came
                        // after this explicit stop. No-op if the timer
                        // already fired or no session was active.
                        {
                            let mut timer_guard = self.midi_learn_timer.lock().await;
                            if let Some(prev) = timer_guard.take() {
                                prev.abort();
                            }
                        }

                        // Drain captured events
                        let captured_events: Vec<MidiLearnEvent> = {
                            let mut events_guard = events.lock().await;
                            events_guard.drain(..).collect()
                        };

                        let event_count = captured_events.len();
                        info!(
                            "MIDI Learn mode stopped via LLM tool ({} events captured)",
                            event_count
                        );

                        // Analyze events to suggest a trigger config
                        let suggested_trigger = self.analyze_midi_learn_events(&captured_events);

                        ToolCallResult::json(&json!({
                            "success": true,
                            "message": format!("MIDI Learn stopped. {} events captured.", event_count),
                            "events": captured_events,
                            "suggested_trigger": suggested_trigger,
                            "event_count": event_count
                        }))
                    }
                    _ => {
                        // Graceful fallback when running in test mode or standalone
                        warn!(
                            "MIDI Learn stop requested but state not connected to engine manager"
                        );
                        ToolCallResult::json(&json!({
                            "success": true,
                            "message": "MIDI Learn stopped (simulation mode).",
                            "events": [],
                            "pattern": null,
                            "event_count": 0,
                            "simulation": true
                        }))
                    }
                }
            }
            // v4.23.0: Multi-device stateful tools (ADR-009 Phase 5)
            "conductor_set_device_enabled" => match &self.daemon_state_refs {
                Some(refs) => {
                    let device_id = arguments
                        .as_ref()
                        .and_then(|a| a.get("device_id"))
                        .and_then(|v| v.as_str());
                    let enabled = arguments
                        .as_ref()
                        .and_then(|a| a.get("enabled"))
                        .and_then(|v| v.as_bool());

                    match (device_id, enabled) {
                        (Some(id), Some(en)) => {
                            let dev_id = conductor_core::identity::DeviceId::from_alias(id);
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
                None => ToolCallResult::error("Daemon state not available"),
            },
            "conductor_scan_ports" => match &self.daemon_state_refs {
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
                None => ToolCallResult::error("Daemon state not available"),
            },
            // Switch active mode by name
            // DEPRECATED (ADR-040): switches the mode without touching any manual
            // lock — prefer conductor_set_mode. Behaviour unchanged (the
            // description was the inaccurate part).
            "conductor_switch_mode" => {
                let mode_name = arguments
                    .as_ref()
                    .and_then(|a| a.get("mode"))
                    .and_then(|v| v.as_str());

                match mode_name {
                    Some(name) => {
                        // Phase 2: Validate mode exists in config
                        let mode_index =
                            _config.and_then(|cfg| cfg.modes.iter().position(|m| m.name == name));

                        match mode_index {
                            Some(idx) => {
                                // Phase 2: Send mode change command to daemon
                                match &self.daemon_state_refs {
                                    Some(refs) => {
                                        // Use send().await instead of try_send to avoid silent drops
                                        if let Err(e) = refs
                                            .command_tx
                                            .send(crate::daemon::types::DaemonCommand::ModeChange {
                                                mode: name.to_string(),
                                            })
                                            .await
                                        {
                                            warn!(
                                                "Failed to send mode change command (channel closed): {}",
                                                e
                                            );
                                            return ToolCallResult::error(&format!(
                                                "Failed to trigger mode change (daemon shutting down): {}",
                                                e
                                            ));
                                        }

                                        info!(
                                            "Mode change to '{}' (index {}) requested via MCP tool",
                                            name, idx
                                        );
                                        ToolCallResult::json(&json!({
                                            "success": true,
                                            "mode_name": name,
                                            "mode_index": idx,
                                            "message": format!("Mode change to '{}' triggered", name)
                                        }))
                                    }
                                    None => {
                                        // Fallback when daemon state refs not available (test mode)
                                        warn!(
                                            "Mode switch requested but daemon state refs not available"
                                        );
                                        ToolCallResult::json(&json!({
                                            "success": true,
                                            "mode_name": name,
                                            "mode_index": idx,
                                            "message": format!("Mode change to '{}' validated (simulation mode)", name),
                                            "simulation": true
                                        }))
                                    }
                                }
                            }
                            None => {
                                let available: Vec<&str> = _config
                                    .map(|cfg| cfg.modes.iter().map(|m| m.name.as_str()).collect())
                                    .unwrap_or_default();
                                ToolCallResult::error(&format!(
                                    "Mode '{}' not found. Available modes: {:?}",
                                    name, available
                                ))
                            }
                        }
                    }
                    None => ToolCallResult::error("Missing required argument: mode (string)"),
                }
            }
            // ADR-040 D4 §4.2 — mode-lock tools (shared helpers; same
            // command-channel path as conductor_switch_mode above).
            "conductor_set_mode" => match &self.daemon_state_refs {
                Some(refs) => {
                    crate::daemon::mode_mcp::set_mode(&refs.command_tx, arguments.as_ref()).await
                }
                None => ToolCallResult::error("Daemon state not available"),
            },
            "conductor_unlock_mode" => match &self.daemon_state_refs {
                Some(refs) => crate::daemon::mode_mcp::unlock_mode(&refs.command_tx).await,
                None => ToolCallResult::error("Daemon state not available"),
            },
            "conductor_mode_status" => match &self.daemon_state_refs {
                Some(refs) => crate::daemon::mode_mcp::mode_status(&refs.command_tx).await,
                None => ToolCallResult::error("Daemon state not available"),
            },
            // Phase 1: Switch profile
            "conductor_switch_profile" => {
                let profile_name = arguments
                    .as_ref()
                    .and_then(|a| a.get("profile_name"))
                    .and_then(|v| v.as_str());
                let config_path = arguments
                    .as_ref()
                    .and_then(|a| a.get("config_path"))
                    .and_then(|v| v.as_str());
                // Optional GUI profile id (additive) so the daemon can
                // persist/report the identity the GUI keys by.
                let profile_id = arguments
                    .as_ref()
                    .and_then(|a| a.get("profile_id"))
                    .and_then(|v| v.as_str())
                    .map(str::to_owned);

                match (profile_name, config_path) {
                    (Some(name), Some(path)) => {
                        // Validate profile path using shared helper
                        let validated_path = match crate::daemon::types::validate_profile_path(path)
                        {
                            Ok(path) => path,
                            Err(e) => {
                                return ToolCallResult::error(&e);
                            }
                        };
                        let path = validated_path.display().to_string();

                        match &self.daemon_state_refs {
                            Some(refs) => {
                                // Phase 2 S7: Synchronous profile switch — await result
                                let (result_tx, result_rx) = tokio::sync::oneshot::channel();
                                if let Err(e) = refs
                                    .command_tx
                                    .send(crate::daemon::types::DaemonCommand::ProfileSwitch {
                                        profile_name: name.to_string(),
                                        config_path: path.to_string(),
                                        profile_id,
                                        result_tx: Some(result_tx),
                                    })
                                    .await
                                {
                                    return ToolCallResult::error(&format!(
                                        "Failed to trigger profile switch: {}",
                                        e
                                    ));
                                }

                                // Wait for result with timeout
                                match tokio::time::timeout(
                                    std::time::Duration::from_secs(10),
                                    result_rx,
                                )
                                .await
                                {
                                    Ok(Ok(Ok(activated_name))) => {
                                        info!(
                                            "Profile '{}' activated via MCP tool",
                                            activated_name
                                        );
                                        ToolCallResult::json(&json!({
                                            "success": true,
                                            "profile_name": activated_name,
                                            "config_path": path,
                                            "message": format!("Profile '{}' activated successfully", activated_name)
                                        }))
                                    }
                                    Ok(Ok(Err(err))) => {
                                        warn!("Profile switch failed via MCP tool: {}", err);
                                        ToolCallResult::error(&format!(
                                            "Profile switch failed: {}",
                                            err
                                        ))
                                    }
                                    Ok(Err(_)) => ToolCallResult::error(
                                        "Profile switch result channel closed",
                                    ),
                                    Err(_) => {
                                        ToolCallResult::error("Profile switch timed out after 10s")
                                    }
                                }
                            }
                            None => {
                                warn!(
                                    "Profile switch requested but daemon state refs not available"
                                );
                                ToolCallResult::json(&json!({
                                    "success": true,
                                    "profile_name": name,
                                    "config_path": path,
                                    "message": format!("Profile switch to '{}' validated (simulation mode)", name),
                                    "simulation": true
                                }))
                            }
                        }
                    }
                    _ => ToolCallResult::error(
                        "Missing required arguments: profile_name and config_path",
                    ),
                }
            }

            // Phase 1: Get active profile
            "conductor_get_active_profile" => match &self.daemon_state_refs {
                Some(refs) => {
                    let profile = (**refs.active_profile.load()).clone();
                    ToolCallResult::json(&json!({
                        "active_profile": profile
                    }))
                }
                None => ToolCallResult::json(&json!({
                    "active_profile": null
                })),
            },

            // GUI-only profile tools (ADR-023: profile state lives in GUI, not daemon)
            // These should be intercepted frontend-side; if they reach the daemon,
            // return a clear error rather than falling through to "unknown tool".
            "conductor_list_profiles" | "conductor_create_profile" | "conductor_delete_profile" => {
                ToolCallResult::error(crate::daemon::mcp_tools::GUI_ONLY_TOOL_ERROR)
            }

            "conductor_list_plugins"
            | "conductor_plugin_info"
            | "conductor_enable_plugin"
            | "conductor_disable_plugin" => {
                // Plugin tools require daemon state — route via IPC command channel
                match &self.daemon_state_refs {
                    Some(refs) => {
                        let ipc_cmd = match tool_name {
                            "conductor_list_plugins" => {
                                crate::daemon::types::IpcCommand::ListPlugins
                            }
                            "conductor_plugin_info" => {
                                crate::daemon::types::IpcCommand::GetPluginInfo
                            }
                            "conductor_enable_plugin" => {
                                crate::daemon::types::IpcCommand::EnablePlugin
                            }
                            "conductor_disable_plugin" => {
                                crate::daemon::types::IpcCommand::DisablePlugin
                            }
                            _ => unreachable!(),
                        };
                        let args = arguments.clone().unwrap_or(json!({}));
                        let request = crate::daemon::types::IpcRequest {
                            id: uuid::Uuid::new_v4().to_string(),
                            command: ipc_cmd,
                            args,
                        };
                        // Send command and await response via command channel
                        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
                        if let Err(e) =
                            refs.command_tx
                                .send(crate::daemon::types::DaemonCommand::IpcRequest {
                                    request,
                                    // Internal-origin daemon command
                                    // (no external peer). Today the
                                    // receiving plugin-management arms in
                                    // `engine_manager::handle_ipc_request`
                                    // (`IpcCommand::ListPlugins` / `GetPluginInfo`
                                    // / `EnablePlugin` / `DisablePlugin`)
                                    // don't consult `caller_ctx` — they
                                    // dispatch directly without going
                                    // through `ToolExecutor::execute` —
                                    // so this field is currently inert
                                    // for those handlers. We pass
                                    // `internal_trusted` rather than
                                    // `None` deliberately as
                                    // future-proofing: when those
                                    // handlers are eventually wired
                                    // through the gate (or other
                                    // gate-aware handlers grow that
                                    // also dispatch via this channel),
                                    // `GuiTrusted` is the correct trust
                                    // band for a daemon-internal call
                                    // whose outer LLM tool boundary has
                                    // already been gate-checked. Closes
                                    // the earlier `TODO(gate-bypass on
                                    // None)` for this site at the
                                    // type level even though the
                                    // runtime effect is currently nil.
                                    caller_ctx: Some(
                                        crate::security::CallerContext::internal_trusted(),
                                    ),
                                    response_tx: resp_tx,
                                })
                                .await
                        {
                            return ToolCallResult::error(&format!(
                                "Failed to send plugin command: {}",
                                e
                            ));
                        }
                        match resp_rx.await {
                            Ok(response) => ToolCallResult::json(&json!(response)),
                            Err(e) => ToolCallResult::error(&format!(
                                "Failed to receive plugin response: {}",
                                e
                            )),
                        }
                    }
                    None => ToolCallResult::error(
                        "Plugin management not available (no daemon connection)",
                    ),
                }
            }

            // LLM Editor tools (ADR-017 Phase 2C) — return structured data for GUI.
            //
            // These tools are intentionally "advisory" not "imperative": they return
            // success JSON describing the desired editor state but do NOT directly
            // mutate the frontend. The LLM communicates the result to the user, who
            // then acts in the GUI. The frontend's agentic tool loop intercepts these
            // tool names and applies the changes to the MappingEditor store client-side.
            "conductor_set_mapping_editor" => {
                let trigger = arguments.as_ref().and_then(|a| a.get("trigger").cloned());
                let action = arguments.as_ref().and_then(|a| a.get("action").cloned());
                let description = arguments
                    .as_ref()
                    .and_then(|a| a.get("description"))
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string();
                let mode = arguments
                    .as_ref()
                    .and_then(|a| a.get("mode"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("Default")
                    .to_string();

                ToolCallResult::json(&json!({
                    "status": "editor_opened",
                    "trigger": trigger,
                    "action": action,
                    "description": description,
                    "mode": mode,
                    "instructions": "The GUI MappingEditor has been populated with this data. The user can review and save."
                }))
            }
            "conductor_update_mapping_editor" => {
                // LLMs sometimes double-encode nested objects as JSON strings.
                // Try as_object() first, then fall back to parsing the string.
                let fields_value = arguments.as_ref().and_then(|a| a.get("fields"));
                let fields = fields_value.and_then(|f| {
                    f.as_object().cloned().or_else(|| {
                        f.as_str().and_then(|s| {
                            serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(s)
                                .ok()
                        })
                    })
                });

                if fields.as_ref().is_none_or(|f| f.is_empty()) {
                    return ToolCallResult::error(
                        "Missing or empty required parameter: fields (must be a JSON object or JSON-encoded string)",
                    );
                }

                let fields_map = fields.unwrap();
                let updated_keys: Vec<&String> = fields_map.keys().collect();

                ToolCallResult::json(&json!({
                    "status": "fields_updated",
                    "fields": fields_map,
                    "updated_keys": updated_keys,
                    "instructions": "The MappingEditor fields have been updated. The user can review the changes."
                }))
            }

            _ => ToolCallResult::error(&format!("Unknown stateful tool: {}", tool_name)),
        }
    }
}
