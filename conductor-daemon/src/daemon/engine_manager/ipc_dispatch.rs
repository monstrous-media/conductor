// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! `EngineManager::handle_ipc_request` IPC command dispatcher (plus the
//! test-only audit-logger injector), extracted from `engine_manager::mod`.

use super::*;

impl EngineManager {
    /// Handle IPC request
    ///
    /// `caller_ctx` carries the ADR-027 D1-pinned peer identity from
    /// the IPC accept loop, threaded down to
    /// `tool_executor.execute` so the gate can consult the trust
    /// band before dispatching MCP tools. Other request types ignore
    /// it for now — that consumption will widen over time.
    pub(crate) async fn handle_ipc_request(
        &mut self,
        request: crate::daemon::types::IpcRequest,
        caller_ctx: Option<crate::security::CallerContext>,
    ) -> IpcResponse {
        let id = request.id.clone();
        // Own a copy of the request id up front for the post-commit
        // backstop's error path below. `id` is moved into the per-command
        // handlers, and the `match request.command` borrows (unit variants)
        // rather than moves `request`, but binding `req_id` here makes the
        // backstop's ownership obviously sound without relying on that
        // subtlety.
        let req_id = request.id.clone();

        // ADR-034 §D4.2 / D4.B.3.B — AwaitingConfig IPC accept-list:
        // REMOVED as dead code.
        //
        // The `AwaitingConfig` idle mode (start the IPC server with no
        // live config and accept only a bootstrap accept-list) was never
        // wired: nothing transitions the daemon INTO
        // `LifecycleState::AwaitingConfig` at startup, so this early-return
        // accept-list filter was unreachable at runtime. Rather than ship a
        // public contract the daemon doesn't honour, the unreachable mode is
        // downgraded to "reserved" — a fresh install with no resolvable
        // config now exits with a descriptive error (see `main.rs`) instead
        // of pretending to enter a recoverable idle state. The reserved
        // scaffolding (`LifecycleState::AwaitingConfig`,
        // `IpcCommand::allowed_during_awaiting_config`,
        // `IpcErrorCode::DaemonAwaitingConfig`) is retained so the mode can
        // be reinstated without a wire-format break if headless/zero-touch
        // provisioning becomes a real goal.

        let response = match request.command {
            IpcCommand::Ping => self.handle_ping(id),
            IpcCommand::GetListenerStatus => self.handle_get_listener_status(id),
            IpcCommand::Status => self.handle_status(id).await,
            IpcCommand::SetUiMode => self.handle_set_ui_mode(&request, id).await,
            #[cfg(feature = "audit-db")]
            IpcCommand::QueryAudit => self.handle_query_audit(&request, id),
            #[cfg(feature = "audit-db")]
            IpcCommand::SubscribeAudit => self.handle_subscribe_audit(id),
            // ADR-045 D1: compositions without the SQLite audit DB
            // answer audit queries with a clean error, not a hang-up.
            #[cfg(not(feature = "audit-db"))]
            IpcCommand::QueryAudit | IpcCommand::SubscribeAudit => {
                crate::daemon::engine_manager::helpers::ipc_err(
                    id,
                    IpcErrorCode::InternalError,
                    "Audit logging is not available in this build",
                )
            }
            IpcCommand::ResumeAudit => self.handle_resume_audit(id, &caller_ctx).await,
            IpcCommand::Handshake => self.handle_handshake(id),
            IpcCommand::Reload => self.handle_reload(id).await,
            IpcCommand::Stop => self.handle_stop(id).await,
            IpcCommand::ValidateConfig => self.handle_validate_config(&request, id),
            IpcCommand::ListDevices => self.handle_list_devices(id),
            IpcCommand::SetDevice => self.handle_set_device(&request, id).await,
            IpcCommand::GetDevice => self.handle_get_device(id).await,
            IpcCommand::StartMidiLearn => self.handle_start_midi_learn(id),
            IpcCommand::StopMidiLearn => self.handle_stop_midi_learn(id).await,
            IpcCommand::GetMidiLearnEvents => self.handle_get_midi_learn_events(id).await,
            IpcCommand::StartEventMonitor => self.handle_start_event_monitor(id),
            IpcCommand::StopEventMonitor => self.handle_stop_event_monitor(id),
            IpcCommand::SubscribeEvents => self.handle_subscribe_events(id),
            IpcCommand::GetMonitorEvents => self.handle_get_monitor_events(id),
            IpcCommand::SetDeviceEnabled => self.handle_set_device_enabled(&request, id).await,
            IpcCommand::GetProbeHistory => self.handle_get_probe_history(&request, id),
            IpcCommand::CheckPermissions => self.handle_check_permissions(&request, id),
            #[cfg(feature = "llm-executor")]
            IpcCommand::ApplyPlan => self.handle_apply_plan(&request, id).await,
            #[cfg(feature = "llm-executor")]
            IpcCommand::RejectPlan => self.handle_reject_plan(&request, id).await,
            #[cfg(feature = "llm-executor")]
            IpcCommand::ListPendingPlans => self.handle_list_pending_plans(id).await,
            // ADR-045 D1: the plan/apply write machinery ships in
            // `llm-executor` compositions (the Studio bundle); the OSS
            // daemon answers with a clean error naming the tier.
            #[cfg(not(feature = "llm-executor"))]
            IpcCommand::ApplyPlan
            | IpcCommand::RejectPlan
            | IpcCommand::ListPendingPlans
            | IpcCommand::ExecuteMcpTool => crate::daemon::engine_manager::helpers::ipc_err(
                id,
                IpcErrorCode::InternalError,
                "LLM tool execution is not available in this build (ships with \
                 Conductor Studio; source builds can enable the `llm-executor` \
                 cargo feature)",
            ),
            #[cfg(feature = "llm-executor")]
            IpcCommand::ExecuteMcpTool => {
                self.handle_execute_mcp_tool(&request, id, &caller_ctx)
                    .await
            }
            IpcCommand::SwitchProfile => self.handle_switch_profile(&request, id).await,
            IpcCommand::GetActiveProfile => self.handle_get_active_profile(id),
            IpcCommand::RefreshAppMappings => self.handle_refresh_app_mappings(id).await,
            IpcCommand::GetLedStatus => self.handle_get_led_status(id),
            IpcCommand::SetLedScheme => self.handle_set_led_scheme(&request, id).await,
            IpcCommand::SetLedBrightness => self.handle_set_led_brightness(&request, id).await,
            IpcCommand::MarkKnownGood => self.handle_mark_known_good(id).await,
            IpcCommand::RollbackConfig => self.handle_rollback_config(id).await,
            IpcCommand::RollbackConfigForce => {
                self.handle_rollback_config_force(&request, id, &caller_ctx)
                    .await
            }
            IpcCommand::SetLogLevel => self.handle_set_log_level(&request, id),
            IpcCommand::ListPlugins => self.handle_list_plugins(id).await,
            IpcCommand::GetPluginInfo => self.handle_get_plugin_info(&request, id).await,
            IpcCommand::EnablePlugin => self.handle_enable_plugin(&request, id).await,
            IpcCommand::DisablePlugin => self.handle_disable_plugin(&request, id).await,
            IpcCommand::SwitchMode => self.handle_switch_mode(&request, id).await,
            IpcCommand::SetMode => self.handle_set_mode(&request, id).await,
            IpcCommand::UnlockMode => self.handle_unlock_mode(&request, id).await,
            IpcCommand::ModeStatus => self.handle_mode_status(&request, id).await,
            IpcCommand::SimulateMapping => self.handle_simulate_mapping(&request, id).await,
            IpcCommand::GetConfigSnapshot => self.handle_get_config_snapshot(id),
            IpcCommand::GetConfigBody => self.handle_get_config_body(id),
            IpcCommand::Init => self.handle_init(&request, id).await,
            IpcCommand::SaveConfig => self.handle_save_config(&request, id, &caller_ctx).await,
            IpcCommand::ReloadFromDisk | IpcCommand::ImportConfig => {
                self.handle_reload_from_disk_or_import(&request, id, &caller_ctx)
                    .await
            }
            IpcCommand::ConfigDriftStatus => self.handle_config_drift_status(id).await,
            IpcCommand::GetConfigDiff => self.handle_get_config_diff(id).await,
            IpcCommand::OverwriteConfigFile => self.handle_overwrite_config_file(id).await,
        };

        // ADR-043 Q2 — structural post-commit rebuild backstop.
        //
        // Every committing handler above commits through the `LiveConfig`
        // mutate seam; the runtime rebuild (registry, bindings, output map,
        // listeners, rate limiter, probe toggle, capture flags, device
        // status, mode) must follow. Rather than rely on each handler
        // remembering to rebuild — the caller-remembered pattern that caused
        // ADR-043 Defect 2 — `reconcile_runtime_to_live` runs here after
        // EVERY command. It is content-guarded (compares the live snapshot's
        // `revision`), so it is a cheap no-op for read-only commands and for
        // handlers that already reconciled inline, but it makes it
        // structurally impossible for a committed mutation to leave the
        // runtime stale even if a future handler forgets. We reconcile only
        // on a successful response (a failed command did not commit) and
        // override the response only if the rebuild itself fails.
        if matches!(response.status, ResponseStatus::Success)
            && let Err(e) = self.reconcile_runtime_to_live("ipc").await
        {
            error!("post-commit runtime rebuild failed after IPC command: {e}");
            return IpcResponse {
                id: req_id,
                status: ResponseStatus::Error,
                data: None,
                error: Some(ErrorDetails {
                    code: IpcErrorCode::InternalError.as_u16(),
                    message: format!("config committed but runtime rebuild failed: {e}"),
                    details: None,
                }),
            };
        }

        response
    }

    /// Inject an audit logger for tests (the production path uses
    /// `default_audit_logger`, which writes to the real platform data dir).
    /// Must be called before `start_network_listeners` so the drain task
    /// captures it.
    #[cfg(all(test, feature = "audit-db"))]
    pub(crate) fn set_audit_logger_for_test(
        &mut self,
        logger: Arc<crate::daemon::audit::AuditLogger>,
    ) {
        self.audit_sink = Some(logger.clone());
        self.audit_logger = Some(logger);
    }

    /// ADR-045 D5 invariant 3 test seam: simulate "no audit sink could be
    /// initialized" so the listener fail-closed path is exercisable.
    #[cfg(test)]
    pub(crate) fn clear_audit_sink_for_test(&mut self) {
        self.audit_sink = None;
        #[cfg(feature = "audit-db")]
        {
            self.audit_logger = None;
        }
    }
}
