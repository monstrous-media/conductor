use super::*;

impl EngineManager {
    pub(crate) fn handle_ping(&mut self, id: String) -> IpcResponse {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        create_success_response(
            &id,
            Some(json!({
                "message": "pong",
                "timestamp": timestamp
            })),
        )
    }

    pub(crate) fn handle_get_listener_status(&mut self, id: String) -> IpcResponse {
        // ADR-042 Phase A: report each bound network listener.
        let listeners: Vec<_> = self
            .network_listener_status()
            .into_iter()
            .map(|(alias, addr)| {
                json!({
                    "alias": alias,
                    "host": addr.ip().to_string(),
                    "port": addr.port(),
                    "state": "bound",
                })
            })
            .collect();
        create_success_response(&id, Some(json!({ "listeners": listeners })))
    }

    pub(crate) async fn handle_status(&mut self, id: String) -> IpcResponse {
        let state = *self.state.read().await;
        let device_status = self.device_status.read().await.clone();
        let stats = self.statistics.read().await.clone();
        let uptime_secs = self.start_time.elapsed().as_secs();
        // ADR-032 P4 (#1089) — read GUI's last-reported UI mode so we
        // can include it in the response when set. Read under the
        // RwLock and clone to avoid holding the guard across the
        // json! macro expansion.
        let ui_mode_snapshot: Option<String> = self.ui_mode.read().await.clone();

        // Lock-free mode read (v4.26.2 — fix #167: StatusBar shows lifecycle_state instead of mode name)
        let mode_snapshot = self.current_mode.load();
        let current_mode_name = if mode_snapshot.name.is_empty() {
            "None".to_string()
        } else {
            mode_snapshot.name.clone()
        };

        // Get input manager info (v3.0)
        // Return None when input_manager is not initialized
        // (LLM Council feedback v4.13.3: don't falsely report "MidiOnly")
        // v4.14.0: Changed to Option<String> for consistency with get_daemon_state()
        let (input_mode, hid_devices, device_count): (Option<String>, Vec<_>, usize) =
            if let Some(ref mgr) = *self.input_manager.lock().await {
                let mode = match mgr.mode() {
                    InputMode::MidiOnly => "MidiOnly".to_string(),
                    InputMode::GamepadOnly => "GamepadOnly".to_string(),
                    InputMode::Both => "Both".to_string(),
                };
                let gamepads = mgr
                    .get_connected_gamepads()
                    .into_iter()
                    .map(|(id, name)| json!({"id": id, "name": name, "connected": true}))
                    .collect::<Vec<_>>();
                let count = mgr.get_device_bindings().len();
                (Some(mode), gamepads, count)
            } else {
                (None, vec![], 0)
            };

        // ADR-034 §D8.3 — surface mutations the startup reconciliation found
        // in flight at the previous crash (never published), so an operator can
        // see them via `conductorctl status`. Empty unless the audit outbox is
        // enabled and held unresolved pending rows.
        let audit_pending_at_crash: Vec<_> = self
            .live_config
            .pending_at_crash()
            .iter()
            .map(|m| {
                json!({
                    "id": m.id,
                    "intended_revision": m.intended_revision,
                })
            })
            .collect();

        let mut status_payload = json!({
                // Nested structure for frontend compatibility
                "daemon": {
                    "lifecycle_state": format!("{}", state),
                    "uptime_seconds": uptime_secs,
                },
                "audit": {
                    // ADR-034 §D8.3 startup-reconciliation result.
                    "pending_at_crash": audit_pending_at_crash,
                },
                "statistics": {
                    "events_processed": stats.events_processed,
                    "errors_since_start": stats.errors_since_start,
                    "config_reloads": stats.config_reloads,
                },
                "input": {
                    "mode": input_mode,
                    "hid_devices": hid_devices,
                },
                "device": device_status,
                // Legacy fields for backward compatibility
                "state": format!("{}", state),
                "current_mode": current_mode_name,
                "device_count": device_count,
                "config_path": self.config_path.display().to_string(),
                "config_loaded_at": stats.uptime_secs,
                "device_status": device_status,
                "uptime_secs": uptime_secs,
                "events_processed": stats.events_processed,
                "errors_since_start": stats.errors_since_start,
                "config_reloads": stats.config_reloads,
                "reload_stats": {
                    "last_reload_ms": stats.last_reload_duration_ms,
                    "fastest_reload_ms": stats.fastest_reload_ms,
                    "slowest_reload_ms": stats.slowest_reload_ms,
                    "avg_reload_ms": stats.avg_reload_ms,
                },
                "active_profile": (**self.active_profile.load()).as_ref().map(|p| json!({
                    "name": p.name,
                    "config_path": p.config_path
                })),
                "profile_cache": serde_json::to_value(self.profile_cache.metrics()).unwrap_or_default(),
        });
        // ADR-032 P4 (#1089) — only include `ui_mode` when set, so
        // consumers without a connected GUI see no shape change.
        if let Some(mode) = ui_mode_snapshot {
            status_payload["ui_mode"] = json!(mode);
        }
        create_success_response(&id, Some(status_payload))
    }

    pub(crate) async fn handle_set_ui_mode(
        &mut self,
        request: &crate::daemon::types::IpcRequest,
        id: String,
    ) -> IpcResponse {
        let mode = request
            .args
            .get("mode")
            .and_then(|m| m.as_str())
            .map(|s| s.to_string());
        match mode {
            Some(ref m) if m == "llm" || m == "studio" => {
                *self.ui_mode.write().await = Some(m.clone());
                create_success_response(&id, Some(json!({ "ui_mode": m })))
            }
            Some(other) => IpcResponse {
                id,
                status: ResponseStatus::Error,
                data: None,
                error: Some(ErrorDetails {
                    code: IpcErrorCode::InvalidRequest.as_u16(),
                    message: format!("Invalid ui_mode: '{}' — expected 'llm' or 'studio'", other),
                    details: None,
                }),
            },
            None => IpcResponse {
                id,
                status: ResponseStatus::Error,
                data: None,
                error: Some(ErrorDetails {
                    code: IpcErrorCode::InvalidRequest.as_u16(),
                    message: "Missing required argument: mode".to_string(),
                    details: None,
                }),
            },
        }
    }

    #[cfg(feature = "audit-db")]
    pub(crate) fn handle_query_audit(
        &mut self,
        request: &crate::daemon::types::IpcRequest,
        id: String,
    ) -> IpcResponse {
        let Some(logger) = self.audit_logger.as_ref() else {
            return IpcResponse {
                id,
                status: ResponseStatus::Error,
                data: None,
                error: Some(ErrorDetails {
                    code: IpcErrorCode::InternalError.as_u16(),
                    message: "Audit logging is disabled on this daemon".to_string(),
                    details: None,
                }),
            };
        };
        let denied_only = request
            .args
            .get("denied_only")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        // Clamp the limit to a sane window — the CLI default
        // is 50; cap at 10k so a malformed request can't ask
        // the daemon to materialise an unbounded result set.
        let limit = request
            .args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(50)
            .clamp(1, 10_000) as u32;
        let query = crate::daemon::audit::AuditQuery {
            event_type: denied_only.then_some(crate::daemon::audit::AuditEventType::ToolDenied),
            limit: Some(limit),
            ..Default::default()
        };
        match logger.query(&query) {
            Ok(entries) => create_success_response(
                &id,
                Some(json!({
                    "denied_only": denied_only,
                    "count": entries.len(),
                    "entries": entries,
                })),
            ),
            Err(e) => IpcResponse {
                id,
                status: ResponseStatus::Error,
                data: None,
                error: Some(ErrorDetails {
                    code: IpcErrorCode::InternalError.as_u16(),
                    message: format!("Audit query failed: {e}"),
                    details: None,
                }),
            },
        }
    }

    #[cfg(feature = "audit-db")]
    pub(crate) fn handle_subscribe_audit(&mut self, id: String) -> IpcResponse {
        IpcResponse {
            id,
            status: ResponseStatus::Error,
            data: None,
            error: Some(ErrorDetails {
                code: IpcErrorCode::InternalError.as_u16(),
                message: "SubscribeAudit must be handled at the IPC connection layer".to_string(),
                details: None,
            }),
        }
    }

    pub(crate) fn handle_handshake(&mut self, id: String) -> IpcResponse {
        IpcResponse {
            id,
            status: ResponseStatus::Error,
            data: None,
            error: Some(ErrorDetails {
                code: IpcErrorCode::InternalError.as_u16(),
                message: "Handshake routing not yet wired — see ADR-027 D19 follow-up PR"
                    .to_string(),
                details: None,
            }),
        }
    }

    pub(crate) async fn handle_stop(&mut self, id: String) -> IpcResponse {
        // Broadcast shutdown to IPC server, config watcher, MCP server
        let _ = self.shutdown_tx.send(());
        // Also send Shutdown command through the command channel so the
        // engine manager's own main loop breaks and disconnects devices.
        let _ = self.command_tx.send(DaemonCommand::Shutdown).await;

        create_success_response(
            &id,
            Some(json!({
                "message": "Daemon stopping",
                "state_saved": true
            })),
        )
    }

    pub(crate) fn handle_set_log_level(
        &mut self,
        request: &crate::daemon::types::IpcRequest,
        id: String,
    ) -> IpcResponse {
        let level_str = request
            .args
            .get("level")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let valid_levels = ["error", "warn", "info", "debug", "trace"];
        if !valid_levels.contains(&level_str.as_str()) {
            return IpcResponse {
                id,
                status: ResponseStatus::Error,
                data: None,
                error: Some(ErrorDetails {
                    code: IpcErrorCode::InvalidRequest.as_u16(),
                    message: format!(
                        "Invalid log level '{}'. Valid: {}",
                        level_str,
                        valid_levels.join(", ")
                    ),
                    details: None,
                }),
            };
        }

        // The GUI persists daemon.toml before sending this IPC notification.
        // The daemon does NOT write daemon.toml here to avoid dual-writer races.
        // Future: wire the reload::Handle to apply the level at runtime.
        info!(
            "Received log level change notification: '{}' (takes effect on restart)",
            level_str
        );

        create_success_response(
            &id,
            Some(
                json!({ "level": level_str, "message": "Log level acknowledged (takes effect on restart)" }),
            ),
        )
    }

    pub(crate) fn handle_check_permissions(
        &mut self,
        request: &crate::daemon::types::IpcRequest,
        id: String,
    ) -> IpcResponse {
        use crate::permissions::{
            PermissionStatus, check_input_monitoring, invalidate_input_monitoring_cache,
        };
        // PR #997 round-11 review: callers can pass
        // `args: { "force": true }` to bypass the daemon's
        // 30s probe cache for this call. The GUI uses this
        // after "Open System Settings" so the next probe
        // reflects a freshly granted permission immediately
        // — without it, the GUI's local cache invalidation
        // is meaningless because the daemon would just
        // return the cached pre-grant value.
        let force = request
            .args
            .get("force")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if force {
            invalidate_input_monitoring_cache();
        }
        let status = check_input_monitoring();
        let (input_monitoring_label, input_monitoring_granted) = match &status {
            PermissionStatus::Granted => ("granted", Some(true)),
            PermissionStatus::NotGranted => ("not_granted", Some(false)),
            PermissionStatus::Unknown(_) => ("unknown", None),
            PermissionStatus::NotApplicable => ("not_applicable", None),
        };
        let detail = match &status {
            PermissionStatus::Unknown(reason) => Some(reason.clone()),
            _ => None,
        };
        create_success_response(
            &id,
            Some(json!({
                "platform": std::env::consts::OS,
                "input_monitoring": input_monitoring_label,
                // Tri-state: true / false / null. The GUI can
                // treat null as "ask the user to verify in
                // System Settings" rather than auto-prompting.
                "input_monitoring_granted": input_monitoring_granted,
                "detail": detail,
            })),
        )
    }

    pub(crate) fn handle_get_probe_history(
        &mut self,
        request: &crate::daemon::types::IpcRequest,
        id: String,
    ) -> IpcResponse {
        let Some(port_name) = request.args.get("port_name").and_then(|v| v.as_str()) else {
            return IpcResponse {
                id,
                status: ResponseStatus::Error,
                data: None,
                error: Some(ErrorDetails {
                    code: IpcErrorCode::InvalidRequest.as_u16(),
                    message: "Missing 'port_name' argument".to_string(),
                    details: Some(json!({"example": {"port_name": "Mikro IN"}})),
                }),
            };
        };
        let history = self.probe_coordinator.history_for_port(port_name);
        create_success_response(
            &id,
            Some(json!({
                "port_name": port_name,
                "history": history,
            })),
        )
    }
}
