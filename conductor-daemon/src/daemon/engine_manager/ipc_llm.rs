// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use super::*;

impl EngineManager {
    pub(crate) async fn handle_apply_plan(
        &mut self,
        request: &crate::daemon::types::IpcRequest,
        id: String,
    ) -> IpcResponse {
        // Extract plan_id from args
        let plan_id_str = request.args.get("plan_id").and_then(|v| v.as_str());

        match plan_id_str {
            Some(id_str) => {
                match uuid::Uuid::parse_str(id_str) {
                    Ok(plan_id) => {
                        // D4.A.3.3.B.1: tool_executor + engine_manager share the same
                        // Arc<LiveConfig> — no set_config push needed. apply_plan mutates
                        // through live_config.mutate_replace_whole, so reading the
                        // post-apply snapshot here gives the freshly-published Config.
                        match self.tool_executor.apply_plan(&plan_id).await {
                            Ok(changes_applied) => {
                                // Persist post-apply snapshot to disk + recompile rule
                                // set. LiveConfig already published the new
                                // revision; this just mirrors it to disk.
                                let new_config = (*self.live_config.load().config).clone();
                                if let Err(e) = self.sync_config_after_apply(new_config).await {
                                    error!("Failed to sync config after plan apply: {}", e);
                                }
                                info!(
                                    "Plan {} applied successfully ({} changes)",
                                    plan_id, changes_applied
                                );
                                create_success_response(
                                    &id,
                                    Some(json!({
                                        "applied": true,
                                        "plan_id": id_str,
                                        "changes_applied": changes_applied
                                    })),
                                )
                            }
                            Err(e) => IpcResponse {
                                id,
                                status: ResponseStatus::Error,
                                data: None,
                                error: Some(ErrorDetails {
                                    code: IpcErrorCode::InvalidRequest.as_u16(),
                                    message: e.to_string(),
                                    details: None,
                                }),
                            },
                        }
                    }
                    Err(e) => IpcResponse {
                        id,
                        status: ResponseStatus::Error,
                        data: None,
                        error: Some(ErrorDetails {
                            code: IpcErrorCode::InvalidRequest.as_u16(),
                            message: format!("Invalid plan_id format: {}", e),
                            details: None,
                        }),
                    },
                }
            }
            None => IpcResponse {
                id,
                status: ResponseStatus::Error,
                data: None,
                error: Some(ErrorDetails {
                    code: IpcErrorCode::InvalidRequest.as_u16(),
                    message: "Missing 'plan_id' argument".to_string(),
                    details: Some(json!({"example": {"plan_id": "uuid-here"}})),
                }),
            },
        }
    }

    pub(crate) async fn handle_reject_plan(
        &mut self,
        request: &crate::daemon::types::IpcRequest,
        id: String,
    ) -> IpcResponse {
        // Extract plan_id from args
        let plan_id_str = request.args.get("plan_id").and_then(|v| v.as_str());

        match plan_id_str {
            Some(id_str) => match uuid::Uuid::parse_str(id_str) {
                Ok(plan_id) => match self.tool_executor.reject_plan(&plan_id).await {
                    Ok(()) => {
                        info!("Plan {} rejected", plan_id);
                        create_success_response(
                            &id,
                            Some(json!({
                                "rejected": true,
                                "plan_id": id_str
                            })),
                        )
                    }
                    Err(e) => IpcResponse {
                        id,
                        status: ResponseStatus::Error,
                        data: None,
                        error: Some(ErrorDetails {
                            code: IpcErrorCode::InvalidRequest.as_u16(),
                            message: e.to_string(),
                            details: None,
                        }),
                    },
                },
                Err(e) => IpcResponse {
                    id,
                    status: ResponseStatus::Error,
                    data: None,
                    error: Some(ErrorDetails {
                        code: IpcErrorCode::InvalidRequest.as_u16(),
                        message: format!("Invalid plan_id format: {}", e),
                        details: None,
                    }),
                },
            },
            None => IpcResponse {
                id,
                status: ResponseStatus::Error,
                data: None,
                error: Some(ErrorDetails {
                    code: IpcErrorCode::InvalidRequest.as_u16(),
                    message: "Missing 'plan_id' argument".to_string(),
                    details: Some(json!({"example": {"plan_id": "uuid-here"}})),
                }),
            },
        }
    }

    pub(crate) async fn handle_list_pending_plans(&mut self, id: String) -> IpcResponse {
        let plans = self.tool_executor.list_pending_plans().await;
        let plan_summaries: Vec<serde_json::Value> = plans
            .iter()
            .map(|p| {
                json!({
                    "id": p.id.to_string(),
                    "description": p.description,
                    "expires_at": p.expires_at.to_rfc3339(),
                    "created_at": p.created_at.to_rfc3339(),
                    "change_count": p.changes.len()
                })
            })
            .collect();
        create_success_response(
            &id,
            Some(json!({
                "plans": plan_summaries
            })),
        )
    }

    /// ADR-027 §D6 — session-boundary trust check shared by the two
    /// lifecycle handlers. GuiTrusted ONLY, as a direct trust-band match
    /// (the RollbackConfigForce / Chain A precedent, not `enforce()` —
    /// these are not risk-tiered tools, and shadow mode must not soften
    /// them). `internal_trusted()` also qualifies (it pins GuiTrusted).
    ///
    /// Why not `CliTrusted`: whoever can signal a session/turn boundary
    /// can RESET the corresponding budget counters, and a budgeted LLM
    /// holding a shell-exec capability could invoke the signed
    /// `conductorctl` binary — which classifies CliTrusted — to launder
    /// its own caps. The GUI's Rust backend is the one trusted boundary
    /// signaller in the D6 threat model (the compromised party is the
    /// provider/webview, which cannot reach this surface directly).
    fn llm_lifecycle_denial(
        &self,
        id: &str,
        command: &str,
        caller_ctx: &Option<crate::security::CallerContext>,
    ) -> Option<IpcResponse> {
        let trusted = matches!(
            caller_ctx.as_ref().map(|c| c.trust_level),
            Some(crate::security::TrustLevel::GuiTrusted)
        );
        if trusted {
            return None;
        }
        warn!(
            "{command} denied: caller trust {:?} (verified Conductor GUI peer required)",
            caller_ctx.as_ref().map(|c| c.trust_level)
        );
        Some(IpcResponse {
            id: id.to_string(),
            status: ResponseStatus::Error,
            data: None,
            error: Some(ErrorDetails {
                code: IpcErrorCode::PermissionDenied.as_u16(),
                message: format!(
                    "{command} requires a verified Conductor GUI peer — session/turn \
                     boundaries reset budget counters, so only the trusted GUI backend \
                     may signal them"
                ),
                details: None,
            }),
        })
    }

    /// ADR-027 §D6 — `LlmSessionBegin`: compile the file-only
    /// `[security.llm]` budget from the operator-editable USER config
    /// file and attach a fresh per-session state to the tool executor.
    /// The user file — not `config_path` — is the correct source: the
    /// daemon's `live.toml` authority is serialised from the
    /// round-trippable `Config`, which deliberately does not carry the
    /// file-only `[security]` table (ADR-027 D3), so reading it would
    /// silently discard the operator's limits. A missing user file means
    /// the ADR §D6 defaults (same semantics as `conductorctl llm budgets
    /// show`); a malformed `[security.llm]` is a hard error and leaves
    /// the budget unattached — the operator tried to configure limits,
    /// so silently running defaults would betray that intent.
    pub(crate) async fn handle_llm_session_begin(
        &mut self,
        id: String,
        caller_ctx: &Option<crate::security::CallerContext>,
    ) -> IpcResponse {
        if let Some(denial) = self.llm_lifecycle_denial(&id, "LlmSessionBegin", caller_ctx) {
            return denial;
        }

        let budget_config = match std::fs::read_to_string(&self.user_file_path) {
            Ok(text) => match conductor_core::security::SecurityConfig::from_toml_str(&text) {
                Ok(sec) => sec.llm,
                Err(e) => {
                    warn!(
                        "LlmSessionBegin: [security.llm] in {} failed to parse: {e}",
                        self.user_file_path.display()
                    );
                    return IpcResponse {
                        id,
                        status: ResponseStatus::Error,
                        data: None,
                        error: Some(ErrorDetails {
                            code: IpcErrorCode::ConfigParseFailed.as_u16(),
                            message: format!(
                                "[security.llm] in the user config file failed to parse \
                                     (budget NOT activated): {e}"
                            ),
                            details: None,
                        }),
                    };
                }
            },
            // No user file: the ADR-027 §D6 defaults apply (every
            // dimension enforced at its default limit).
            Err(_) => conductor_core::security::LlmBudgetConfig::default(),
        };

        let now_ms = crate::daemon::llm::executor::llm_budget_now_ms();
        self.tool_executor
            .set_budget_state(std::sync::Arc::new(tokio::sync::Mutex::new(
                conductor_core::security::LlmBudgetState::new(budget_config, now_ms),
            )));
        info!(
            "LLM session budget attached (source: {})",
            self.user_file_path.display()
        );
        create_success_response(&id, Some(json!({ "active": true })))
    }

    /// ADR-027 §D6 — `LlmTurnBegin`: reset the per-turn budget
    /// dimensions on the attached session state. `active: false` when no
    /// session has begun — a no-op, not an error, so the GUI can fire it
    /// unconditionally at the top of every agentic-loop entry.
    pub(crate) async fn handle_llm_turn_begin(
        &mut self,
        id: String,
        caller_ctx: &Option<crate::security::CallerContext>,
    ) -> IpcResponse {
        if let Some(denial) = self.llm_lifecycle_denial(&id, "LlmTurnBegin", caller_ctx) {
            return denial;
        }
        let now_ms = crate::daemon::llm::executor::llm_budget_now_ms();
        let active = self.tool_executor.begin_llm_turn(now_ms).await;
        create_success_response(&id, Some(json!({ "active": active })))
    }

    pub(crate) async fn handle_execute_mcp_tool(
        &mut self,
        request: &crate::daemon::types::IpcRequest,
        id: String,
        caller_ctx: &Option<crate::security::CallerContext>,
    ) -> IpcResponse {
        // Extract tool_name and arguments from args
        let tool_name = request.args.get("tool_name").and_then(|v| v.as_str());
        let arguments = request.args.get("arguments").cloned();

        match tool_name {
            // SysEx identity probe must be handled directly here for the
            // same reason as `conductor_switch_profile` below: the
            // tool_executor's HardwareIO branch routes through
            // `state_refs.command_tx.send(DaemonCommand::ProbeDeviceIdentity)`
            // and awaits a oneshot. handle_ipc_request runs inside the
            // command_rx select arm, so the inner DaemonCommand sits in
            // the mpsc buffer unprocessed and the oneshot never resolves
            // until the executor's 30 s tokio::time::timeout fires —
            // long after the IPC client has given up at 5 s and closed
            // the pipe.
            //
            // Bypass the executor and call run_probe_device_identity
            // directly. The wire format is reconstructed to match what
            // the executor would have produced
            // (`ExecutionResult::HardwareIoConfirmation { status:
            // Confirmed { result: <stringified ProbeOutcomeWire JSON> },
            // tool_name }`) so the GUI's
            // `extract_probe_outcome_from_execution_result` helper
            // (Phase 3.D.1) parses the response without modification.
            //
            // Skipping `request_sysex_confirmation` is safe — the bytes
            // sent are the hardcoded `IDENTITY_REQUEST` constant, not
            // user-supplied SysEx; there's no validation surface that
            // applies.
            Some(tool_name @ "conductor_probe_device_identity") => {
                // ADR-027 admission: bypassing tool_executor's dispatch
                // for deadlock reasons must not also bypass its
                // admission sequence. `admit` runs the security gate,
                // the executor rate limiter and the LLM budget (when
                // attached) with the same ordering, audit-on-deny and
                // wire shapes `ToolExecutor::execute` uses. An
                // `Ok(ToHardwareIo)` route (the gate's
                // RequireConfirmation row for a trusted peer) is
                // accepted as admitted here: the confirmation step is
                // satisfied by construction because the bytes sent are
                // the hardcoded `IDENTITY_REQUEST` constant, not
                // user-supplied SysEx (see below). A gate `Deny`
                // (untrusted / unpinned peer under the production
                // policy) is honoured.
                //
                // Remaining intentional differences from the executor
                // path:
                // - **SysEx validation**: a no-op for the hardcoded
                //   `IDENTITY_REQUEST` constant (auto-confirms today).
                //   Skipping is safe because the bytes aren't
                //   user-supplied. Drift risk if validation rules add
                //   new compliance checks later.
                // - The hardware-side per-port 60 s window still lives
                //   at the `ProbeCoordinator` (ADR-026 Phase 1.A D1)
                //   inside `run_probe_device_identity`, layered under
                //   the executor rate limit applied here.
                use crate::daemon::hardware_io::ConfirmationStatus;
                use crate::daemon::llm::executor::ExecutionResult;
                use conductor_core::device_intelligence::probe::ProbeOutcomeWire;

                if let Err(denial) = self
                    .tool_executor
                    .admit(tool_name, caller_ctx.as_ref())
                    .await
                {
                    return create_success_response(&id, Some(execution_result_to_value(&denial)));
                }
                let started = std::time::Instant::now();

                let port_name = arguments
                    .as_ref()
                    .and_then(|a| a.get("port_name"))
                    .and_then(|v| v.as_str());

                match port_name {
                    Some(port) => {
                        let probe_outcome = self.run_probe_device_identity(port).await;
                        let wire = ProbeOutcomeWire::from(probe_outcome);
                        // If `ProbeOutcomeWire` ever fails to
                        // serialise (effectively impossible with
                        // current types — all derived
                        // `Serialize` over owned strings, ints,
                        // and enums — but guarded defensively
                        // for future-proofing), surface that as
                        // `ExecutionResult::Error` rather than
                        // smuggling an incompatible
                        // `{"error":"..."}` blob into the
                        // `Confirmed.result` field. The GUI's
                        // `extract_probe_outcome_from_execution_result`
                        // helper expects flat `ProbeOutcomeWire`
                        // shape inside `result`; an error blob
                        // would silently misparse.
                        let result = match serde_json::to_string(&wire) {
                            Ok(outcome_json) => ExecutionResult::HardwareIoConfirmation {
                                status: ConfirmationStatus::Confirmed {
                                    result: outcome_json,
                                },
                                tool_name: tool_name.to_string(),
                            },
                            Err(e) => ExecutionResult::Error {
                                message: format!("Failed to serialise probe outcome: {}", e),
                            },
                        };
                        // ADR-027 D13: the direct path leaves the same
                        // audit trail the executor's HardwareIO handler
                        // would.
                        match &result {
                            ExecutionResult::HardwareIoConfirmation {
                                status: ConfirmationStatus::Confirmed { result: outcome },
                                ..
                            } => self.tool_executor.audit_direct_tool_outcome(
                                tool_name,
                                Ok(Some(outcome)),
                                started.elapsed(),
                            ),
                            ExecutionResult::Error { message } => {
                                self.tool_executor.audit_direct_tool_outcome(
                                    tool_name,
                                    Err(message),
                                    started.elapsed(),
                                )
                            }
                            _ => self.tool_executor.audit_direct_tool_outcome(
                                tool_name,
                                Ok(None),
                                started.elapsed(),
                            ),
                        }
                        create_success_response(&id, Some(execution_result_to_value(&result)))
                    }
                    None => {
                        let result = ExecutionResult::Error {
                            message: "Missing required argument: port_name".to_string(),
                        };
                        create_success_response(&id, Some(execution_result_to_value(&result)))
                    }
                }
            }
            // Profile switch must be handled directly here — NOT through
            // ToolExecutor → command_tx → command_rx, because handle_ipc_request
            // runs inside the command_rx select arm, creating a deadlock.
            Some(tool_name @ "conductor_switch_profile") => {
                // ADR-027 admission: same rationale as the probe arm
                // above — the deadlock-avoiding direct dispatch runs the
                // executor's gate → rate-limit → budget sequence first.
                // (Stateful tier: the gate's decision table admits every
                // local caller; the rate limiter and any attached LLM
                // budget still bound it.)
                if let Err(denial) = self
                    .tool_executor
                    .admit(tool_name, caller_ctx.as_ref())
                    .await
                {
                    return create_success_response(&id, Some(execution_result_to_value(&denial)));
                }
                let started = std::time::Instant::now();

                let profile_name = arguments
                    .as_ref()
                    .and_then(|a| a.get("profile_name"))
                    .and_then(|v| v.as_str());
                let config_path = arguments
                    .as_ref()
                    .and_then(|a| a.get("config_path"))
                    .and_then(|v| v.as_str());
                // Optional GUI profile id.
                let profile_id = arguments
                    .as_ref()
                    .and_then(|a| a.get("profile_id"))
                    .and_then(|v| v.as_str())
                    .map(str::to_owned);

                match (profile_name, config_path) {
                    (Some(name), Some(path)) => {
                        match self.execute_profile_switch(name, path, profile_id).await {
                            Ok(msg) => {
                                // ADR-027 D13: same audit trail the
                                // executor's Stateful handler would leave.
                                self.tool_executor.audit_direct_tool_outcome(
                                    tool_name,
                                    Ok(Some(&msg)),
                                    started.elapsed(),
                                );
                                create_success_response(
                                    &id,
                                    Some(json!({
                                        "content": [{"type": "text", "text": serde_json::to_string(&json!({
                                            "success": true,
                                            "profile_name": name,
                                            "config_path": self.config_path.display().to_string(),
                                            "message": msg,
                                        })).unwrap_or_default()}],
                                    })),
                                )
                            }
                            Err(e) => {
                                self.tool_executor.audit_direct_tool_outcome(
                                    tool_name,
                                    Err(&e),
                                    started.elapsed(),
                                );
                                create_success_response(
                                    &id,
                                    Some(json!({
                                        "content": [{"type": "text", "text": e}],
                                        "isError": true,
                                    })),
                                )
                            }
                        }
                    }
                    _ => create_success_response(
                        &id,
                        Some(json!({
                            "content": [{"type": "text", "text": "Missing required arguments: profile_name and config_path"}],
                            "isError": true,
                        })),
                    ),
                }
            }
            Some(name) => {
                // D4.A.3.3.B.1: tool_executor reads config directly from
                // shared Arc<LiveConfig> — no set_config push needed.
                let result = self
                    .tool_executor
                    .execute(name, arguments, caller_ctx.as_ref())
                    .await;
                // Same wire-shape-safe serialisation as the
                // direct-handler arms above — see
                // `execution_result_to_value` rationale.
                create_success_response(&id, Some(execution_result_to_value(&result)))
            }
            None => IpcResponse {
                id,
                status: ResponseStatus::Error,
                data: None,
                error: Some(ErrorDetails {
                    code: IpcErrorCode::InvalidRequest.as_u16(),
                    message: "Missing 'tool_name' argument".to_string(),
                    details: Some(
                        json!({"example": {"tool_name": "conductor_get_config", "arguments": {}}}),
                    ),
                }),
            },
        }
    }
}
