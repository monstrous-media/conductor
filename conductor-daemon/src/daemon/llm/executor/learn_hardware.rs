// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! MIDI-learn analysis, ConfigChange, and HardwareIO execution.

use super::*;

impl ToolExecutor {
    /// Analyze captured MIDI Learn events to suggest a trigger pattern
    pub(super) fn analyze_midi_learn_events(&self, events: &[MidiLearnEvent]) -> Option<Value> {
        if events.is_empty() {
            return None;
        }

        // Find the most common event type and suggest a trigger
        // This is a simple analysis - more sophisticated pattern detection
        // is handled by the GUI's MidiLearnSession

        // Look for pattern events first (detected by EventProcessor)
        for event in events.iter().rev() {
            if let Some(pattern_type) = &event.pattern_type {
                match pattern_type {
                    PatternType::LongPress => {
                        if let Some(note) = event.note {
                            return Some(json!({
                                "type": "LongPress",
                                "note": note,
                                "duration_ms": event.pattern_duration_ms.unwrap_or(2000)
                            }));
                        }
                    }
                    PatternType::DoubleTap => {
                        if let Some(note) = event.note {
                            return Some(json!({
                                "type": "DoubleTap",
                                "note": note,
                                "timeout_ms": event.pattern_timeout_ms.unwrap_or(300)
                            }));
                        }
                    }
                    PatternType::Chord => {
                        if let Some(notes) = &event.pattern_notes {
                            return Some(json!({
                                "type": "Chord",
                                "notes": notes,
                                "window_ms": event.pattern_timeout_ms.unwrap_or(100)
                            }));
                        }
                    }
                    PatternType::GamepadChord => {
                        if let Some(buttons) = &event.pattern_buttons {
                            return Some(json!({
                                "type": "GamepadButtonChord",
                                "buttons": buttons,
                                "window_ms": event.pattern_timeout_ms.unwrap_or(100)
                            }));
                        }
                    }
                    PatternType::MediumPress => {
                        if let Some(note) = event.note {
                            return Some(json!({
                                "type": "Note",
                                "note": note,
                                "velocity_min": 1
                            }));
                        }
                    }
                    // ContextSwitch is not an input gesture — it annotates a
                    // state transition, never a Learn-suggested trigger.
                    PatternType::ContextSwitch => {}
                }
            }
        }

        // Fall back to simple event analysis
        if let Some(first_event) = events.first() {
            match first_event.event_type {
                EventType::NoteOn => {
                    if let Some(note) = first_event.note {
                        // Check for VelocityRange: 3+ presses of the same note with velocity range > 30
                        let same_note_velocities: Vec<u8> = events
                            .iter()
                            .filter(|e| e.event_type == EventType::NoteOn && e.note == Some(note))
                            .filter_map(|e| e.velocity)
                            .collect();

                        if same_note_velocities.len() >= 3 {
                            let min_vel = *same_note_velocities.iter().min().unwrap_or(&1);
                            let max_vel = *same_note_velocities.iter().max().unwrap_or(&127);
                            if max_vel - min_vel > 30 {
                                // Suggest VelocityRange with soft/medium/hard zones.
                                // Emit the CANONICAL `Trigger::VelocityRange`
                                // field names (`soft_max` / `medium_max`), not a
                                // `ranges` object. The previous `ranges` shape did
                                // not match the enum variant, so applying the
                                // suggestion made serde silently drop it and the
                                // trigger defaulted to soft_max=40/medium_max=80,
                                // discarding the learned thresholds.
                                let range = max_vel - min_vel;
                                let soft_max = min_vel + range / 3;
                                let medium_max = min_vel + 2 * range / 3;
                                return Some(json!({
                                    "type": "VelocityRange",
                                    "note": note,
                                    "soft_max": soft_max,
                                    "medium_max": medium_max
                                }));
                            }
                        }

                        return Some(json!({
                            "type": "Note",
                            "note": note,
                            "velocity_min": first_event.velocity.unwrap_or(1)
                        }));
                    }
                }
                EventType::Cc => {
                    if let Some(cc) = first_event.cc {
                        return Some(json!({
                            "type": "CC",
                            "cc": cc
                        }));
                    }
                }
                EventType::Encoder => {
                    if let Some(cc) = first_event.cc {
                        return Some(json!({
                            "type": "EncoderTurn",
                            "cc": cc,
                            "direction": "Any"
                        }));
                    }
                }
                EventType::PitchBend => {
                    return Some(json!({
                        "type": "PitchBend",
                        "bend_range": [-8192, 8191]
                    }));
                }
                EventType::Aftertouch => {
                    return Some(json!({
                        "type": "Aftertouch",
                        "pressure_min": 1
                    }));
                }
                EventType::PolyPressure => {
                    // PolyPressure (polyphonic aftertouch) maps to Aftertouch trigger
                    // (the Trigger enum has no PolyPressure variant — Aftertouch is the closest match)
                    return Some(json!({
                        "type": "Aftertouch",
                        "pressure_min": 1
                    }));
                }
                EventType::GamepadButton => {
                    if let Some(button) = first_event.button {
                        return Some(json!({
                            "type": "GamepadButton",
                            "button": button
                        }));
                    }
                }
                EventType::GamepadAxis => {
                    if let Some(axis) = first_event.axis {
                        return Some(json!({
                            "type": "GamepadAnalogStick",
                            "axis": axis
                        }));
                    }
                }
                EventType::GamepadTrigger => {
                    if let Some(trigger) = first_event.trigger {
                        return Some(json!({
                            "type": "GamepadTrigger",
                            "trigger": trigger
                        }));
                    }
                }
                // ADR-025 Phase 1: a single PC capture suggests the
                // ProgramChange trigger. Channel comes from the captured
                // event (defaults to 0 in MidiLearnEvent::default).
                EventType::ProgramChange => {
                    if let Some(pc) = first_event.pc {
                        return Some(json!({
                            "type": "ProgramChange",
                            "pc": pc,
                            "channel": first_event.channel,
                        }));
                    }
                }
                _ => {}
            }
        }

        None
    }

    /// Execute a ConfigChange tool by creating a plan
    pub(super) async fn execute_config_change(
        &self,
        tool_name: &str,
        arguments: Option<Value>,
    ) -> ExecutionResult {
        let args_json = arguments.as_ref().map(|a| a.to_string());

        // D4.A.3.3.B.1: snapshot config via LiveConfig — always loaded
        // post-EngineManager::new, so the legacy "Configuration not loaded"
        // branch became unreachable. Keep the `&Config` binding alive via the
        // snap Arc; create_plan_for_tool needs `&Config` for hashing.
        let snap = self.live_config.load();
        let config: &Config = snap.config.as_ref();

        // Parse arguments and create plan
        let plan_result = self.create_plan_for_tool(tool_name, arguments, config);

        match plan_result {
            Ok(plan) => {
                let plan_id = plan.id;
                let changes_count = plan.changes.len();

                // Store the plan
                {
                    let mut plans = self.pending_plans.write().await;
                    plans.insert(plan_id, plan.clone());
                    info!("Created plan {} for tool '{}'", plan_id, tool_name);
                }

                // Audit log plan creation (P4-04)
                if let Some(ref logger) = self.audit_logger {
                    logger.log_plan_created(
                        &plan_id.to_string(),
                        changes_count,
                        Some(UserContext::local_user()),
                    );
                }

                ExecutionResult::PlanCreated { plan }
            }
            Err(e) => {
                // Audit log the error
                if let Some(ref logger) = self.audit_logger {
                    logger.log_tool_error(
                        tool_name,
                        AuditRiskTier::ConfigChange,
                        args_json.as_deref(),
                        &e.to_string(),
                        std::time::Duration::ZERO,
                        Some(UserContext::local_user()),
                    );
                }
                ExecutionResult::Error {
                    message: e.to_string(),
                }
            }
        }
    }

    /// Execute a HardwareIO tool with multi-step confirmation (P4-01)
    pub(super) async fn execute_hardware_io(
        &self,
        tool_name: &str,
        arguments: Option<Value>,
    ) -> ExecutionResult {
        let start_time = Instant::now();
        let args = arguments.unwrap_or(json!({}));
        let args_json = serde_json::to_string(&args).ok();

        let confirmation_token = args.get("confirmation_token").and_then(|t| t.as_str());

        let status = match tool_name {
            "conductor_send_sysex" => {
                let device = match args.get("device").and_then(|d| d.as_str()) {
                    Some(d) => d,
                    None => {
                        return ExecutionResult::Error {
                            message: "Missing required argument: device".to_string(),
                        };
                    }
                };

                let data: Vec<u8> = match args.get("data").and_then(|d| d.as_array()) {
                    Some(arr) => arr
                        .iter()
                        .filter_map(|v| v.as_u64().map(|n| n as u8))
                        .collect(),
                    None => {
                        return ExecutionResult::Error {
                            message: "Missing required argument: data (array of bytes)".to_string(),
                        };
                    }
                };

                match self.confirmation_manager.request_sysex_confirmation(
                    device,
                    &data,
                    confirmation_token,
                ) {
                    Ok(status) => status,
                    Err(e) => {
                        return ExecutionResult::Error {
                            message: e.to_string(),
                        };
                    }
                }
            }

            "conductor_device_reset" => {
                let device = match args.get("device").and_then(|d| d.as_str()) {
                    Some(d) => d,
                    None => {
                        return ExecutionResult::Error {
                            message: "Missing required argument: device".to_string(),
                        };
                    }
                };

                let reset_type = match args.get("reset_type").and_then(|r| r.as_str()) {
                    Some(r) => r,
                    None => {
                        return ExecutionResult::Error {
                            message: "Missing required argument: reset_type".to_string(),
                        };
                    }
                };

                match self.confirmation_manager.request_reset_confirmation(
                    device,
                    reset_type,
                    confirmation_token,
                ) {
                    Ok(status) => status,
                    Err(e) => {
                        return ExecutionResult::Error {
                            message: e.to_string(),
                        };
                    }
                }
            }

            "conductor_send_midi" => {
                let port = match args.get("port").and_then(|p| p.as_str()) {
                    Some(p) => p,
                    None => {
                        return ExecutionResult::Error {
                            message: "Missing required argument: port".to_string(),
                        };
                    }
                };

                let messages: Vec<MidiSendMessage> =
                    match args.get("messages").and_then(|m| m.as_array()) {
                        Some(arr) => {
                            let mut msgs = Vec::new();
                            for (i, item) in arr.iter().enumerate() {
                                match serde_json::from_value::<MidiSendMessage>(item.clone()) {
                                    Ok(msg) => {
                                        if let Err(e) = msg.validate() {
                                            return ExecutionResult::Error {
                                                message: format!(
                                                    "Invalid message at index {}: {}",
                                                    i, e
                                                ),
                                            };
                                        }
                                        msgs.push(msg);
                                    }
                                    Err(e) => {
                                        return ExecutionResult::Error {
                                            message: format!(
                                                "Failed to parse message at index {}: {}",
                                                i, e
                                            ),
                                        };
                                    }
                                }
                            }
                            msgs
                        }
                        None => {
                            return ExecutionResult::Error {
                                message: "Missing required argument: messages (array)".to_string(),
                            };
                        }
                    };

                if messages.is_empty() {
                    return ExecutionResult::Error {
                        message: "Messages array must not be empty".to_string(),
                    };
                }

                // Build byte representations for audit
                let byte_descriptions: Vec<String> = messages
                    .iter()
                    .filter_map(|m| m.to_bytes().ok().map(|b| format!("{:02X?}", b)))
                    .collect();

                match self.confirmation_manager.request_midi_send_confirmation(
                    port,
                    &messages,
                    confirmation_token,
                ) {
                    Ok(status) => match &status {
                        ConfirmationStatus::Confirmed { .. } => {
                            // Auto-confirmed — return success with byte details
                            ConfirmationStatus::Confirmed {
                                result: format!(
                                    "Approved: {} MIDI message(s) to '{}': [{}]",
                                    messages.len(),
                                    port,
                                    byte_descriptions.join(", ")
                                ),
                            }
                        }
                        _ => status,
                    },
                    Err(e) => {
                        return ExecutionResult::Error {
                            message: e.to_string(),
                        };
                    }
                }
            }

            "conductor_probe_device_identity" => {
                let port_name = match args.get("port_name").and_then(|p| p.as_str()) {
                    Some(p) => p.to_string(),
                    None => {
                        return ExecutionResult::Error {
                            message: "Missing required argument: port_name".to_string(),
                        };
                    }
                };

                // The Identity Request is `F0 7E 7F 06 01 F7` — universal
                // and benign. `SysExValidator::validate()` expects the
                // *inner* SysEx data only (without F0/F7 framing) per its
                // contract; passing the full frame causes the validator
                // to read 0xF0 as a manufacturer ID, classify the message
                // as `UnknownManufacturer` (which DOES require user
                // confirmation), and break the auto-confirm path. Slice
                // to bytes 1..5 = `[7E, 7F, 06, 01]` so the validator
                // hits the Universal Non-Realtime → IdentityRequest path
                // which is `requires_confirmation() == false`.
                use conductor_core::device_intelligence::sysex_identity::IDENTITY_REQUEST;
                let validator_data = &IDENTITY_REQUEST[1..IDENTITY_REQUEST.len() - 1];
                match self.confirmation_manager.request_sysex_confirmation(
                    &port_name,
                    validator_data,
                    confirmation_token,
                ) {
                    Ok(ConfirmationStatus::Confirmed { .. }) => {
                        // Dispatch the actual probe through the daemon
                        // command channel. The engine_manager handler
                        // resolves the paired output and runs the sync
                        // probe via spawn_blocking, then sends the
                        // outcome back through the response channel.
                        let Some(state_refs) = self.daemon_state_refs.as_ref() else {
                            return ExecutionResult::Error {
                                message:
                                    "Daemon state refs not available — probe requires running daemon"
                                        .to_string(),
                            };
                        };
                        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
                        if let Err(e) = state_refs
                            .command_tx
                            .send(crate::daemon::DaemonCommand::ProbeDeviceIdentity {
                                port_name: port_name.clone(),
                                response_tx: resp_tx,
                            })
                            .await
                        {
                            return ExecutionResult::Error {
                                message: format!("Failed to dispatch probe command: {}", e),
                            };
                        }
                        // Bound the wait so a stalled engine loop
                        // (shutdown, hung task) surfaces a clear error
                        // instead of hanging the MCP client. Sizing:
                        // Phase 1.A serialises probes globally with a
                        // 1 s reply timeout, so queue_wait grows
                        // roughly linearly with concurrent probes —
                        // probe-on-connect (Phase 3) can dispatch one
                        // per device discovered in a hot-plug burst.
                        // 30 s comfortably covers ~25-device bursts
                        // (25 × 1 s reply + scheduling slack) while
                        // still failing fast for genuine deadlocks. A
                        // single healthy probe never approaches this.
                        // `probe_outcome` is `Result<ProbeResult,
                        // ProbeStartError>` — the *overall* probe
                        // outcome, NOT a `ProbeResult`. Naming
                        // mirrors `ProbeOutcomeWire` to make the
                        // collapse below read clearly.
                        let probe_outcome =
                            match tokio::time::timeout(std::time::Duration::from_secs(30), resp_rx)
                                .await
                            {
                                Ok(Ok(o)) => o,
                                Ok(Err(_)) => {
                                    return ExecutionResult::Error {
                                        message:
                                            "Probe response channel closed before result arrived"
                                                .to_string(),
                                    };
                                }
                                Err(_) => {
                                    return ExecutionResult::Error {
                                        message:
                                            "Probe timed out waiting for daemon response (>30s)"
                                                .to_string(),
                                    };
                                }
                            };
                        // Phase 3.B.1: collapse the
                        // `Result<ProbeResult, ProbeStartError>` through
                        // `ProbeOutcomeWire` for the JSON wire format —
                        // produces the flat `{"status": "..."}` shape
                        // Phase 2 MCP callers parse. Build the fallback
                        // via `json!()` so any quotes/newlines in the
                        // serde error message are properly escaped — raw
                        // `format!()` would produce invalid JSON.
                        let wire = ProbeOutcomeWire::from(probe_outcome);
                        let outcome_json = serde_json::to_string(&wire).unwrap_or_else(|e| {
                            serde_json::json!({
                                "error": format!("serialize: {}", e),
                            })
                            .to_string()
                        });
                        ConfirmationStatus::Confirmed {
                            result: outcome_json,
                        }
                    }
                    Ok(other_status) => other_status,
                    Err(e) => {
                        return ExecutionResult::Error {
                            message: e.to_string(),
                        };
                    }
                }
            }

            _ => {
                return ExecutionResult::Error {
                    message: format!("Unknown HardwareIO tool: {}", tool_name),
                };
            }
        };

        // Audit log based on status
        if let Some(ref logger) = self.audit_logger {
            let execution_time = start_time.elapsed();
            match &status {
                ConfirmationStatus::Confirmed { .. } => {
                    logger.log_tool_complete(
                        tool_name,
                        AuditRiskTier::HardwareIO,
                        args_json.as_deref(),
                        Some(&format!("{:?}", status)),
                        execution_time,
                        Some(UserContext::local_user()),
                    );
                }
                ConfirmationStatus::Blocked { reason } => {
                    logger.log_tool_denied(
                        tool_name,
                        AuditRiskTier::HardwareIO,
                        reason,
                        Some(UserContext::local_user()),
                    );
                }
                ConfirmationStatus::RequiresConfirmation { .. } => {
                    // Log that confirmation was requested
                    logger.log_tool_start(
                        tool_name,
                        AuditRiskTier::HardwareIO,
                        args_json.as_deref(),
                        Some(UserContext::local_user()),
                    );
                }
                ConfirmationStatus::InvalidToken { reason } => {
                    logger.log_tool_error(
                        tool_name,
                        AuditRiskTier::HardwareIO,
                        args_json.as_deref(),
                        reason,
                        execution_time,
                        Some(UserContext::local_user()),
                    );
                }
            }
        }

        ExecutionResult::HardwareIoConfirmation {
            status,
            tool_name: tool_name.to_string(),
        }
    }
}
