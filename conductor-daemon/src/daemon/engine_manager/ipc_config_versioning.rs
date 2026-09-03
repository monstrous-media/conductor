// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Config versioning IPC handlers (`handle_reload`, `handle_validate_config`,
//! `handle_mark_known_good`, `handle_rollback_config`,
//! `handle_rollback_config_force`, `handle_config_drift_status`), extracted
//! from `engine_manager::ipc_config`.

use super::*;

impl EngineManager {
    pub(crate) async fn handle_reload(&mut self, id: String) -> IpcResponse {
        match self.reload_config().await {
            Ok(metrics) => create_success_response(
                &id,
                Some(json!({
                    "message": "Config reloaded successfully",
                    "config_path": self.config_path,
                    "reload_duration_ms": metrics.duration_ms,
                    "modes_loaded": metrics.modes_loaded,
                    "mappings_loaded": metrics.mappings_loaded,
                    "config_load_ms": metrics.config_load_ms,
                    "mapping_compile_ms": metrics.mapping_compile_ms,
                    "swap_ms": metrics.swap_ms,
                    "performance_grade": metrics.performance_grade(),
                    "met_target": metrics.met_target(),
                })),
            ),
            Err(e) => IpcResponse {
                id,
                status: ResponseStatus::Error,
                data: None,
                error: Some(ErrorDetails {
                    code: IpcErrorCode::ConfigValidationFailed.as_u16(),
                    message: e.to_string(),
                    details: None,
                }),
            },
        }
    }

    pub(crate) fn handle_validate_config(
        &mut self,
        request: &crate::daemon::types::IpcRequest,
        id: String,
    ) -> IpcResponse {
        // Extract path from args
        let path = request
            .args
            .get("path")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| self.config_path.clone());

        // Properly handle non-UTF8 paths
        let path_str = match pathbuf_to_str_or_err(&path, "ValidateConfig path") {
            Ok(s) => s,
            Err(e) => {
                return IpcResponse {
                    id,
                    status: ResponseStatus::Error,
                    data: None,
                    error: Some(ErrorDetails {
                        code: IpcErrorCode::ConfigValidationFailed.as_u16(),
                        message: e.to_string(),
                        details: Some(json!({"path": format!("{:?}", path)})),
                    }),
                };
            }
        };

        match Config::load(path_str) {
            Ok(config) => {
                let total_mappings: usize =
                    config.modes.iter().map(|m| m.mappings.len()).sum::<usize>()
                        + config.global_mappings.len();

                create_success_response(
                    &id,
                    Some(json!({
                        "valid": true,
                        "modes": config.modes.len(),
                        "mappings": total_mappings,
                        "warnings": []
                    })),
                )
            }
            Err(e) => IpcResponse {
                id,
                status: ResponseStatus::Error,
                data: None,
                error: Some(ErrorDetails {
                    code: IpcErrorCode::ConfigValidationFailed.as_u16(),
                    message: e.to_string(),
                    details: None,
                }),
            },
        }
    }

    pub(crate) async fn handle_mark_known_good(&mut self, id: String) -> IpcResponse {
        // ADR-034 §D1.2.1 / D4.B.4 — promote the current
        // LiveConfig snapshot's `revision` to `known_good_revision`.
        // The underlying ConfigOp::MarkKnownGood routes through the
        // mutate seam: CAS-checked, per-op persist matrix writes
        // `live.toml.known_good` (NOT `live.toml`) inside the lock.
        //
        // CLI-only per spec — the AwaitingConfig-style accept-list
        // for non-CLI-tier mutating commands lands as a follow-up
        // commit on this branch alongside RollbackConfigForce
        // (which has identical CLI-only semantics).
        let snap = self.live_config.load();
        let base_generation = snap.state_generation;
        let mutate_result = self
            .live_config
            .mutate(
                self.default_cli_provenance(),
                base_generation,
                crate::daemon::live_config::ConfigOp::MarkKnownGood,
            )
            .await;
        match mutate_result {
            Ok(outcome) => create_success_response(
                &id,
                Some(json!({
                    "state_generation": outcome.applied_generation,
                    "revision": format!("{}", outcome.applied_revision),
                    "message": "Current snapshot marked as known-good",
                })),
            ),
            Err(e) => IpcResponse {
                id,
                status: ResponseStatus::Error,
                data: None,
                error: Some(ErrorDetails {
                    code: IpcErrorCode::InternalError.as_u16(),
                    message: format!("MarkKnownGood failed: {e}"),
                    details: None,
                }),
            },
        }
    }

    pub(crate) async fn handle_rollback_config(&mut self, id: String) -> IpcResponse {
        // ADR-034 §D1.2.1 / D4.B.4 — CAS-checked rollback
        // routed through `live_config.mutate(ConfigOp::Rollback)`.
        // Replaces the legacy `fs::copy + reload_config` flow.
        //
        // The mutate seam reads `live.toml.known_good` inside the
        // lock, recompiles the rule set, persists the new
        // `live.toml`, and atomically swaps the snapshot. CAS
        // protects against racing writers between the IPC
        // accept and the lock acquire.
        let snap = self.live_config.load();
        let base_generation = snap.state_generation;
        let mutate_result = self
            .live_config
            .mutate(
                self.default_cli_provenance(),
                base_generation,
                crate::daemon::live_config::ConfigOp::Rollback,
            )
            .await;
        match mutate_result {
            Ok(outcome) => {
                info!(
                    "Config rolled back via live_config.mutate: gen {} → {}",
                    outcome.previous_generation, outcome.applied_generation
                );
                create_success_response(
                    &id,
                    Some(json!({
                        "message": "Config rolled back to known-good snapshot",
                        "state_generation": outcome.applied_generation,
                        "previous_generation": outcome.previous_generation,
                        "revision": format!("{}", outcome.applied_revision),
                    })),
                )
            }
            Err(e) => {
                use crate::daemon::live_config::MutateError;
                let (code, message) = match &e {
                    MutateError::InvalidOp(msg) => {
                        (IpcErrorCode::InvalidRequest.as_u16(), msg.clone())
                    }
                    MutateError::StaleBaseGeneration { current, supplied } => (
                        IpcErrorCode::InvalidRequest.as_u16(),
                        format!("stale base_generation: current={current}, supplied={supplied}"),
                    ),
                    // ADR-035 §D5: operator-actionable, not an internal
                    // fault — surface the restart-required message as a
                    // 4xx-class InvalidRequest, not a misleading InternalError.
                    MutateError::RestartRequired(msg) => {
                        (IpcErrorCode::InvalidRequest.as_u16(), msg.clone())
                    }
                    other => (
                        IpcErrorCode::InternalError.as_u16(),
                        format!("Rollback failed: {other}"),
                    ),
                };
                IpcResponse {
                    id,
                    status: ResponseStatus::Error,
                    data: None,
                    error: Some(ErrorDetails {
                        code,
                        message,
                        details: None,
                    }),
                }
            }
        }
    }

    pub(crate) async fn handle_rollback_config_force(
        &mut self,
        request: &crate::daemon::types::IpcRequest,
        id: String,
        caller_ctx: &Option<crate::security::CallerContext>,
    ) -> IpcResponse {
        // ADR-034 §D1.2.1 + §D6 / D4.B.4 — break-glass
        // non-CAS rollback. CLI-only per spec; Gui/Untrusted
        // peers get rejected before reaching the mutate seam.
        //
        // `reason` is a required non-empty string in `args`;
        // re-validated here (in addition to the IPC-framer
        // check per KI-B3 and `compute_candidate`'s defence
        // in depth).
        if let Some(ctx) = caller_ctx
            && !matches!(ctx.trust_level, crate::security::TrustLevel::CliTrusted)
        {
            return IpcResponse {
                id,
                status: ResponseStatus::Error,
                data: None,
                error: Some(ErrorDetails {
                    code: IpcErrorCode::InvalidRequest.as_u16(),
                    message: "RollbackConfigForce is CLI-only — \
                                      use RollbackConfig (CAS-checked) \
                                      from Gui / Llm peers"
                        .to_string(),
                    details: None,
                }),
            };
        }

        let reason = request
            .args
            .get("reason")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let Some(reason) = reason else {
            return IpcResponse {
                id,
                status: ResponseStatus::Error,
                data: None,
                error: Some(ErrorDetails {
                    code: IpcErrorCode::MissingField.as_u16(),
                    message: "RollbackConfigForce requires non-empty \
                                      `reason` field in args"
                        .to_string(),
                    details: None,
                }),
            };
        };

        // Force bypasses CAS at both step 2 and step 11 (KI-A3).
        // base_generation is still passed for audit / observability
        // — the mutate seam ignores its value for force ops but
        // records what the caller supplied.
        let snap = self.live_config.load();
        let base_generation = snap.state_generation;
        let mutate_result = self
            .live_config
            .mutate(
                self.default_cli_provenance(),
                base_generation,
                crate::daemon::live_config::ConfigOp::RollbackForce {
                    reason: reason.clone(),
                },
            )
            .await;
        match mutate_result {
            Ok(outcome) => {
                warn!(
                    "BREAK-GLASS rollback (force): gen {} → {}; reason: {}",
                    outcome.previous_generation, outcome.applied_generation, reason
                );
                create_success_response(
                    &id,
                    Some(json!({
                        "message": "Config force-rolled-back to known-good snapshot",
                        "state_generation": outcome.applied_generation,
                        "previous_generation": outcome.previous_generation,
                        "revision": format!("{}", outcome.applied_revision),
                        "reason": reason,
                    })),
                )
            }
            Err(e) => {
                use crate::daemon::live_config::MutateError;
                let (code, message) = match &e {
                    MutateError::InvalidOp(msg) => {
                        (IpcErrorCode::InvalidRequest.as_u16(), msg.clone())
                    }
                    other => (
                        IpcErrorCode::InternalError.as_u16(),
                        format!("RollbackForce failed: {other}"),
                    ),
                };
                IpcResponse {
                    id,
                    status: ResponseStatus::Error,
                    data: None,
                    error: Some(ErrorDetails {
                        code,
                        message,
                        details: None,
                    }),
                }
            }
        }
    }

    pub(crate) async fn handle_config_drift_status(&mut self, id: String) -> IpcResponse {
        // Pure read (ReadOnly tier; on the AwaitingConfig accept-list):
        // does the daemon's config file on disk differ from the live
        // snapshot? Compares revisions computed with the SAME
        // normalizer so an endpoint-rewrite alone never reads as drift.
        let snap = self.live_config.load();
        let live_revision = snap.revision;
        let path = self.config_path.clone();

        // Async read so the Tokio worker isn't blocked.
        let on_disk = tokio::fs::read_to_string(&path)
            .await
            .ok()
            .and_then(|t| toml::from_str::<conductor_core::Config>(&t).ok())
            .and_then(|c| crate::daemon::live_config::revision_for(&c).ok());

        // `user_toml_hash` matches the documented response contract on
        // `IpcCommand::ConfigDriftStatus`; `config_path` is an extra.
        let payload = match on_disk {
            Some(disk_rev) => json!({
                "drift": disk_rev != live_revision,
                "live_revision": format!("{live_revision}"),
                "user_toml_hash": format!("{disk_rev}"),
                "config_path": path.display().to_string(),
            }),
            // Unreadable / unparseable on disk: cannot match the live
            // snapshot, so report drift with a flag the caller can act on.
            None => json!({
                "drift": true,
                "live_revision": format!("{live_revision}"),
                "user_toml_hash": serde_json::Value::Null,
                "config_path": path.display().to_string(),
                "disk_unavailable": true,
            }),
        };
        create_success_response(&id, Some(payload))
    }
}
