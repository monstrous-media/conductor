// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use super::*;

impl EngineManager {
    pub(crate) async fn handle_switch_profile(
        &mut self,
        request: &crate::daemon::types::IpcRequest,
        id: String,
    ) -> IpcResponse {
        // Use execute_profile_switch directly — NOT command_tx.
        // handle_ipc_request runs inside the command_rx select arm,
        // so sending DaemonCommand::ProfileSwitch through command_tx
        // would deadlock (same fix as conductor_switch_profile in ExecuteMcpTool).
        let profile_name = request.args.get("profile_name").and_then(|v| v.as_str());
        let config_path = request.args.get("config_path").and_then(|v| v.as_str());
        // Optional GUI profile id, persisted/reported so
        // the GUI can key its dropdown without path-mapping.
        let profile_id = request
            .args
            .get("profile_id")
            .and_then(|v| v.as_str())
            .map(str::to_owned);

        match (profile_name, config_path) {
            (Some(name), Some(path)) => {
                match self.execute_profile_switch(name, path, profile_id).await {
                    Ok(msg) => create_success_response(
                        &id,
                        Some(json!({
                            "message": msg,
                            "profile_name": name,
                            "config_path": self.config_path.display().to_string(),
                            "success": true,
                        })),
                    ),
                    Err(e) => IpcResponse {
                        id,
                        status: ResponseStatus::Error,
                        data: None,
                        error: Some(ErrorDetails {
                            code: IpcErrorCode::InternalError.as_u16(),
                            message: e,
                            details: None,
                        }),
                    },
                }
            }
            _ => IpcResponse {
                id,
                status: ResponseStatus::Error,
                data: None,
                error: Some(ErrorDetails {
                    code: IpcErrorCode::InvalidRequest.as_u16(),
                    message: "Missing 'profile_name' or 'config_path' argument".to_string(),
                    details: Some(
                        json!({"example": {"profile_name": "Logic Pro", "config_path": "/path/to/profile.toml"}}),
                    ),
                }),
            },
        }
    }

    pub(crate) fn handle_get_active_profile(&mut self, id: String) -> IpcResponse {
        let profile = (**self.active_profile.load()).clone();
        create_success_response(
            &id,
            Some(json!({
                "active_profile": profile
            })),
        )
    }

    pub(crate) async fn handle_refresh_app_mappings(&mut self, id: String) -> IpcResponse {
        if let Err(e) = self
            .command_tx
            .send(DaemonCommand::RefreshAppMappings)
            .await
        {
            return IpcResponse {
                id,
                status: ResponseStatus::Error,
                data: None,
                error: Some(ErrorDetails {
                    code: IpcErrorCode::InternalError.as_u16(),
                    message: format!("Failed to send refresh command: {}", e),
                    details: None,
                }),
            };
        }
        create_success_response(
            &id,
            Some(json!({"message": "App mappings refresh initiated"})),
        )
    }

    pub(crate) async fn handle_switch_mode(
        &mut self,
        request: &crate::daemon::types::IpcRequest,
        id: String,
    ) -> IpcResponse {
        let mode_name = request
            .args
            .get("mode")
            .and_then(|v| v.as_str())
            .map(String::from);

        match mode_name {
            Some(name) => {
                // Validate mode exists
                let exists = self
                    .live_config
                    .load()
                    .config
                    .modes
                    .iter()
                    .any(|m| m.name == name);
                if !exists {
                    return IpcResponse {
                        id,
                        status: ResponseStatus::Error,
                        data: None,
                        error: Some(ErrorDetails {
                            code: IpcErrorCode::InvalidRequest.as_u16(),
                            message: format!("Unknown mode '{}'", name),
                            details: None,
                        }),
                    };
                }

                // Switch mode directly (avoid command_tx to prevent deadlock)
                self.emit_action_event("mode_change", Some(&format!("Mode → {}", name)));
                if let Err(e) = self.persist_mode_change(name.clone()).await {
                    warn!("SwitchMode persist failed: {}", e);
                }
                create_success_response(&id, Some(json!({ "mode": name, "switched": true })))
            }
            None => IpcResponse {
                id,
                status: ResponseStatus::Error,
                data: None,
                error: Some(ErrorDetails {
                    code: IpcErrorCode::InvalidRequest.as_u16(),
                    message: "Missing 'mode' argument".to_string(),
                    details: Some(json!({"example": {"mode": "Edit"}})),
                }),
            },
        }
    }

    /// ADR-040 D4 §4.2 — set the active mode and optionally lock it
    /// against auto-switching (`conductorctl mode set [--no-lock]`). `lock`
    /// defaults to `true`. Runs inside the command-rx select arm, so it calls
    /// `set_mode_manual` directly (no `command_tx`, like `handle_switch_mode`).
    pub(crate) async fn handle_set_mode(
        &mut self,
        request: &crate::daemon::types::IpcRequest,
        id: String,
    ) -> IpcResponse {
        let Some(mode) = request.args.get("mode").and_then(|v| v.as_str()) else {
            return IpcResponse {
                id,
                status: ResponseStatus::Error,
                data: None,
                error: Some(ErrorDetails {
                    code: IpcErrorCode::InvalidRequest.as_u16(),
                    message: "Missing 'mode' argument".to_string(),
                    details: Some(json!({"example": {"mode": "Edit", "lock": true}})),
                }),
            };
        };
        // `mode set` locks by default; `--no-lock` sends `lock: false`.
        let lock = request
            .args
            .get("lock")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        match self
            .set_mode_manual(mode.to_string(), lock, LockSource::Cli)
            .await
        {
            Ok(()) => {
                self.emit_action_event("mode_change", Some(&format!("Mode → {mode}")));
                create_success_response(&id, Some(json!({ "mode": mode, "locked": lock })))
            }
            // `SetModeError` is classified at the source under the mode lock, so
            // there is no TOCTOU re-check here: an
            // unknown mode is a bad request; anything else is an internal fault.
            Err(e) => {
                let (code, message) = match e {
                    SetModeError::UnknownMode => (
                        IpcErrorCode::InvalidRequest,
                        format!("Cannot set mode: unknown mode '{mode}'"),
                    ),
                    SetModeError::Internal(err) => (IpcErrorCode::InternalError, err.to_string()),
                };
                IpcResponse {
                    id,
                    status: ResponseStatus::Error,
                    data: None,
                    error: Some(ErrorDetails {
                        code: code.as_u16(),
                        message,
                        details: None,
                    }),
                }
            }
        }
    }

    /// ADR-040 D4 §4.2 — release the manual mode lock
    /// (`conductorctl mode unlock`). Reports whether a lock was held.
    pub(crate) async fn handle_unlock_mode(
        &mut self,
        _request: &crate::daemon::types::IpcRequest,
        id: String,
    ) -> IpcResponse {
        let was_locked = self.unlock_mode().await;
        create_success_response(&id, Some(json!({ "unlocked": was_locked })))
    }

    /// ADR-040 D4 §4.2 — report the active mode + lock state
    /// (`conductorctl mode status`): mode name/index, whether locked, and the
    /// lock's origin/mode. The rich resolution-layer + window-permission flags
    /// land with the MCP `conductor_mode_status` tool.
    pub(crate) async fn handle_mode_status(
        &mut self,
        _request: &crate::daemon::types::IpcRequest,
        id: String,
    ) -> IpcResponse {
        create_success_response(&id, Some(self.mode_status_json()))
    }
}
