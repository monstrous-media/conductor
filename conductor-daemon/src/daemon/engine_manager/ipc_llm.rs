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
                                // set (#265). LiveConfig already published the new
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
            // the pipe (issue #945).
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
                // KNOWN GAPS (all tracked in #949): bypassing
                // tool_executor also bypasses the executor-owned
                // RateLimiter, AuditLogger, and the
                // SysExValidator/ConfirmationManager pre-step.
                // Same shape of gap exists in the
                // `conductor_switch_profile` direct handler below
                // and predates this fix. Mitigations:
                //
                // - **Rate limit**: the hardware-side per-port
                //   60 s window lives at the `ProbeCoordinator`
                //   (ADR-026 Phase 1.A D1) inside
                //   `run_probe_device_identity`, regardless of
                //   how it's invoked. Uncapped IPC calls can't
                //   flood the wire.
                // - **SysEx validation**: a no-op for the
                //   hardcoded `IDENTITY_REQUEST` constant
                //   (auto-confirms today). Skipping is safe
                //   because the bytes aren't user-supplied.
                //   Drift risk if validation rules add new
                //   compliance checks later — that's what #949
                //   covers.
                // - **Audit logging**: real observability gap.
                //   The probe attempt leaves no trail today.
                //
                // Proper fix: extract executor pre/post hooks
                // both direct handlers can wrap (#949).
                use crate::daemon::hardware_io::ConfirmationStatus;
                use crate::daemon::llm::executor::ExecutionResult;
                use conductor_core::device_intelligence::probe::ProbeOutcomeWire;

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
            Some("conductor_switch_profile") => {
                let profile_name = arguments
                    .as_ref()
                    .and_then(|a| a.get("profile_name"))
                    .and_then(|v| v.as_str());
                let config_path = arguments
                    .as_ref()
                    .and_then(|a| a.get("config_path"))
                    .and_then(|v| v.as_str());
                // #2564 D5 (additive): optional GUI profile id.
                let profile_id = arguments
                    .as_ref()
                    .and_then(|a| a.get("profile_id"))
                    .and_then(|v| v.as_str())
                    .map(str::to_owned);

                match (profile_name, config_path) {
                    (Some(name), Some(path)) => {
                        match self.execute_profile_switch(name, path, profile_id).await {
                            Ok(msg) => create_success_response(
                                &id,
                                Some(json!({
                                    "content": [{"type": "text", "text": serde_json::to_string(&json!({
                                        "success": true,
                                        "profile_name": name,
                                        "config_path": self.config_path.display().to_string(),
                                        "message": msg,
                                    })).unwrap_or_default()}],
                                })),
                            ),
                            Err(e) => create_success_response(
                                &id,
                                Some(json!({
                                    "content": [{"type": "text", "text": e}],
                                    "isError": true,
                                })),
                            ),
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
