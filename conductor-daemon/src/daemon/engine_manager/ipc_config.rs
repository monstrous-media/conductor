use super::*;

impl EngineManager {
    pub(crate) fn handle_get_config_snapshot(&mut self, id: String) -> IpcResponse {
        // Returns the current LiveConfig snapshot's metadata.
        // In AwaitingConfig (post-integration commit) the
        // initial snapshot has `state_generation = 0` — that
        // sentinel doubles as the "not yet bootstrapped"
        // indicator clients use to distinguish daemon-idle
        // from daemon-running. Until the DaemonService::run
        // integration lands, this always returns the live
        // snapshot.
        let snap = self.live_config.load();
        let payload = json!({
            "state_generation": snap.state_generation,
            "revision": format!("{}", snap.revision),
            "known_good_revision": snap.known_good_revision.map(|r| format!("{r}")),
            "applied_at": snap.applied_at
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs()),
            // Config body intentionally omitted — large + the
            // operator can use the conductorctl `config show`
            // path to read it. Sentinel payload mirrors what
            // a real AwaitingConfig response would return
            // (state_generation = 0 with everything else None).
        });
        create_success_response(&id, Some(payload))
    }

    /// Return the canonical `LiveConfig` snapshot **including the config body**
    /// (#1779 B2). Unlike [`Self::handle_get_config_snapshot`] (metadata only),
    /// this carries the full `config` tree so the GUI's `get_config` reflects
    /// the daemon's in-memory authority — not a stale on-disk `config.toml` that
    /// a later save would clobber (ADR-043 anti-clobber defeat). The returned
    /// `state_generation` is the CAS base the client threads into its next
    /// `SaveConfig`, so display and save come from one coherent snapshot.
    ///
    /// In the reserved `AwaitingConfig` sentinel (`state_generation == 0`) there
    /// is no bootstrapped tree to serve, so `config` is `null` and the client
    /// falls back to its on-disk read.
    pub(crate) fn handle_get_config_body(&mut self, id: String) -> IpcResponse {
        let snap = self.live_config.load();
        // Sentinel: not yet bootstrapped → no canonical body to serve.
        let config = if snap.state_generation == 0 {
            serde_json::Value::Null
        } else {
            match serde_json::to_value(snap.config.as_ref()) {
                Ok(v) => v,
                Err(e) => {
                    return ipc_err(
                        id,
                        IpcErrorCode::InternalError,
                        format!("failed to serialize canonical config: {e}"),
                    );
                }
            }
        };
        let payload = json!({
            "state_generation": snap.state_generation,
            "config": config,
            "revision": format!("{}", snap.revision),
            "applied_at": snap.applied_at
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs()),
        });
        create_success_response(&id, Some(payload))
    }

    /// Structured diff of the in-memory live config vs the on-disk config — the
    /// drift source (#2414, ADR-034 §D4.D). ReadOnly: reads `self.config_path`
    /// and compares it to `live_config`'s in-memory tree. Returns
    /// `{ differs, changed_sections, live, target }` so #2284's drift banner can
    /// summarise which sections changed and the GUI can render the detail. A
    /// missing/unparseable on-disk file is surfaced as the normal not-found /
    /// parse error (no diff to compute).
    pub(crate) async fn handle_get_config_diff(&mut self, id: String) -> IpcResponse {
        let live = self.live_config.load();
        let live_json = match serde_json::to_value(live.config.as_ref()) {
            Ok(v) => v,
            Err(e) => {
                return ipc_err(
                    id,
                    IpcErrorCode::InternalError,
                    format!("failed to serialize live config: {e}"),
                );
            }
        };

        // Read the on-disk config through the §D2.2 safe-walk rather than a plain
        // `read_to_string`: this handler RETURNS the file's parsed content to the
        // GUI, so it must not follow a symlink at `config_path` out of the config
        // directory and leak arbitrary file content (Copilot #2435). The safe-walk
        // opens the file beneath the config-directory allowlist root without
        // traversing any symlink and reads from the validated fd (no TOCTOU).
        // The root is `config_path`'s own directory; root derivation + open run
        // inside the blocking task, so there's no check-then-use window.
        let path = self.config_path.clone();
        // SAFETY: geteuid is infallible and has no preconditions.
        let expected_uid = unsafe { libc::geteuid() };
        let root_src = path.clone();
        let target_path = path.clone();
        let read = tokio::task::spawn_blocking(move || {
            crate::daemon::path_validation::read_config_beneath_config_dir(
                &root_src,
                &target_path,
                expected_uid,
            )
        })
        .await;
        let text = match read {
            Ok(Ok(t)) => t,
            Ok(Err(crate::daemon::path_validation::SafeReadError::Validation(e))) => {
                use crate::daemon::path_validation::PathRejectReason;
                // A genuinely missing file is an operator/setup issue, not a
                // security rejection — surface ConfigNotFound (mirrors the
                // ReloadFromDisk trusted-read path).
                if e.reason_code() == PathRejectReason::TargetNotFound {
                    return ipc_err(
                        id,
                        IpcErrorCode::ConfigNotFound,
                        format!("read {}: not found", path.display()),
                    );
                }
                #[cfg(feature = "audit-db")]
                if let Some(logger) = &self.audit_logger {
                    logger.log_path_validation_failed(
                        &path.display().to_string(),
                        e.reason_code().as_str(),
                    );
                }
                return ipc_err(id, IpcErrorCode::PathValidationFailed, e.reason());
            }
            Ok(Err(crate::daemon::path_validation::SafeReadError::Read(e))) => {
                let code = if e.kind() == std::io::ErrorKind::NotFound {
                    IpcErrorCode::ConfigNotFound
                } else {
                    IpcErrorCode::InternalError
                };
                return ipc_err(id, code, format!("read {}: {e}", path.display()));
            }
            Err(join) => {
                return ipc_err(
                    id,
                    IpcErrorCode::InternalError,
                    format!("path-validation task failed: {join}"),
                );
            }
        };
        let target: conductor_core::Config = match toml::from_str(&text) {
            Ok(c) => c,
            Err(e) => {
                return ipc_err(
                    id,
                    IpcErrorCode::ConfigParseFailed,
                    format!("parse {}: {e}", path.display()),
                );
            }
        };
        let target_json = match serde_json::to_value(&target) {
            Ok(v) => v,
            Err(e) => {
                return ipc_err(
                    id,
                    IpcErrorCode::InternalError,
                    format!("failed to serialize on-disk config: {e}"),
                );
            }
        };

        let changed = config_changed_sections(&live_json, &target_json);
        let payload = json!({
            "differs": !changed.is_empty(),
            "changed_sections": changed,
            "live": live_json,
            "target": target_json,
        });
        create_success_response(&id, Some(payload))
    }

    pub(crate) async fn handle_init(
        &mut self,
        request: &crate::daemon::types::IpcRequest,
        id: String,
    ) -> IpcResponse {
        // Bootstrap a live config from the supplied
        // `ConfigSource`. Per spec §D4.2 this is CLI-only —
        // the integration commit on this branch will reject
        // from non-CLI initiators (Gui / Llm) at the
        // accept-list level. For now we accept all callers
        // and just delegate to `live_config.mutate`.
        use conductor_core::Config;

        let source_json = match request.args.get("source") {
            Some(s) => s.clone(),
            None => {
                return IpcResponse {
                    id,
                    status: ResponseStatus::Error,
                    data: None,
                    error: Some(ErrorDetails {
                        code: IpcErrorCode::MissingField.as_u16(),
                        message: "Init requires a 'source' field (ConfigSource)".to_string(),
                        details: None,
                    }),
                };
            }
        };
        let source: crate::daemon::types::ConfigSource = match serde_json::from_value(source_json) {
            Ok(s) => s,
            Err(e) => {
                return IpcResponse {
                    id,
                    status: ResponseStatus::Error,
                    data: None,
                    error: Some(ErrorDetails {
                        code: IpcErrorCode::InvalidRequest.as_u16(),
                        message: format!("Invalid ConfigSource: {e}"),
                        details: None,
                    }),
                };
            }
        };

        // Load Config bytes per source variant.
        let config = match source {
            crate::daemon::types::ConfigSource::Defaults => Config::default_config(),
            crate::daemon::types::ConfigSource::FromPath { path } => {
                if !path.is_absolute() {
                    return IpcResponse {
                        id,
                        status: ResponseStatus::Error,
                        data: None,
                        error: Some(ErrorDetails {
                            code: IpcErrorCode::InvalidRequest.as_u16(),
                            message: format!("Init path must be absolute, got: {}", path.display()),
                            details: None,
                        }),
                    };
                }
                match tokio::fs::read_to_string(&path).await {
                    Ok(text) => match toml::from_str::<Config>(&text) {
                        Ok(c) => c,
                        Err(e) => {
                            return IpcResponse {
                                id,
                                status: ResponseStatus::Error,
                                data: None,
                                error: Some(ErrorDetails {
                                    code: IpcErrorCode::ConfigParseFailed.as_u16(),
                                    message: format!("Init parse {}: {e}", path.display()),
                                    details: None,
                                }),
                            };
                        }
                    },
                    Err(e) => {
                        return IpcResponse {
                            id,
                            status: ResponseStatus::Error,
                            data: None,
                            error: Some(ErrorDetails {
                                code: IpcErrorCode::ConfigNotFound.as_u16(),
                                message: format!("Init read {}: {e}", path.display()),
                                details: None,
                            }),
                        };
                    }
                }
            }
        };

        // ADR-044 Phase 2: PREPARE before committing — a config that can't build
        // (bad listener ACL, mapping compile failure) is rejected here without
        // ever publishing it (atomic; no committed-but-stale window).
        let prepared = match self.prepare_runtime(&config).await {
            Ok(p) => p,
            Err(e) => {
                return ipc_err(
                    id,
                    IpcErrorCode::ConfigValidationFailed,
                    format!("config rejected before commit: {e}"),
                );
            }
        };

        // Publish via the LiveConfig seam — same path
        // any other `ReplaceWhole` mutation takes. Initiator
        // is `Cli` (the integration commit will reject
        // non-CLI peers at the accept-list level before
        // reaching here).
        let snap = self.live_config.load();
        let base_generation = snap.state_generation;
        let mutate_result = self
            .live_config
            .mutate(
                self.default_cli_provenance(),
                base_generation,
                crate::daemon::live_config::ConfigOp::ReplaceWhole {
                    config: Box::new(config),
                },
            )
            .await;

        match mutate_result {
            Ok(outcome) => {
                // #2051 (ADR-043 D2): rebuild the running runtime from the
                // committed config. #2100 Phase 2: install the PREPAREd bundle
                // via the revision-equivalence guard (re-prepares from the
                // committed snapshot only on a revision mismatch). The IPC
                // dispatch backstop would also catch this, but applying inline
                // makes the rebuild land before the Init response. APPLY is
                // infallible (ADR-044) — it runs post-commit, so it never fails
                // an already-committed config (Council #2168 R3).
                self.apply_committed_guarded(prepared, "init").await;
                create_success_response(
                    &id,
                    Some(json!({
                        "state_generation": outcome.applied_generation,
                        "revision": format!("{}", outcome.applied_revision),
                    })),
                )
            }
            Err(e) => {
                // Map via mutate_error_to_ipc so CAS / stale-generation /
                // restart-required errors keep their specific codes (was
                // hardcoded ConfigValidationFailed — Council review),
                // consistent with SaveConfig / RollbackConfig / ReloadFromDisk.
                let (code, message) = mutate_error_to_ipc(&e);
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

    /// ADR-034 §D8 / #2380 — `conductorctl audit resume`: recover from the
    /// fail-closed audit-unavailable state. Delegates to
    /// `LiveConfig::resume_audit` (rotate-corrupt-aside + fresh self-audited
    /// chain); on success, if the daemon is in `AuditDegraded`, transitions it
    /// back to `Running` (config mutations are no longer refused). On failure
    /// the gate stays closed and the error (with the rotated filename) is
    /// surfaced. Privileged: same pinned-peer IPC surface as the ConfigChange
    /// commands; the operator identity is recorded in the ChainReset attestation.
    pub(crate) async fn handle_resume_audit(
        &mut self,
        id: String,
        caller_ctx: &Option<crate::security::CallerContext>,
    ) -> IpcResponse {
        // Operator identity for the ChainReset attestation (best-effort from the
        // pinned peer; "unknown" if the connection wasn't pinned).
        let operator = match caller_ctx.as_ref() {
            Some(c) => match c.peer.as_ref() {
                Some(p) => format!(
                    "{:?} uid={} pid={} exe={}",
                    c.trust_level,
                    p.uid,
                    p.pid,
                    p.initial_exe.display()
                ),
                None => format!("{:?}", c.trust_level),
            },
            None => "unknown".to_string(),
        };

        match self.live_config.resume_audit(operator).await {
            Ok(outcome) => {
                use crate::daemon::live_config::ResumeOutcome;
                let (recovered, rotated_path) = match outcome {
                    ResumeOutcome::AlreadyHealthy => (false, None),
                    ResumeOutcome::Recovered { rotated_path } => {
                        (true, rotated_path.map(|p| p.display().to_string()))
                    }
                };
                // Leave AuditDegraded ONLY when we actually recovered the gate.
                // An `AlreadyHealthy` outcome means the audit gate was never down
                // for an audit reason (Available/Disabled), so it must not blindly
                // punch the daemon back to `Running` if it happens to be in
                // `AuditDegraded` for some other/racing reason — that would clear
                // the degraded state without any recovery having occurred
                // (Council #2384). Gate on `recovered`.
                if recovered
                    && *self.state.read().await == LifecycleState::AuditDegraded
                    && let Err(e) = self.transition_state(LifecycleState::Running).await
                {
                    tracing::warn!(
                        "audit resume: recovered the outbox but failed to leave \
                         AuditDegraded: {e}"
                    );
                }
                create_success_response(
                    &id,
                    Some(json!({
                        "recovered": recovered,
                        "rotated_path": rotated_path,
                    })),
                )
            }
            Err(e) => {
                let (code, message) = mutate_error_to_ipc(&e);
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

    pub(crate) async fn handle_save_config(
        &mut self,
        request: &crate::daemon::types::IpcRequest,
        id: String,
        caller_ctx: &Option<crate::security::CallerContext>,
    ) -> IpcResponse {
        // Full-config replace; supersedes the GUI's legacy direct
        // `Config::save(user.toml)` write so the daemon owns all
        // config persistence post-D4.C. Source = InMemoryEdit.
        let Some(base_generation) = request
            .args
            .get("base_generation")
            .and_then(serde_json::Value::as_u64)
        else {
            return ipc_err(
                id,
                IpcErrorCode::MissingField,
                "SaveConfig requires a u64 `base_generation` in args",
            );
        };
        let config: conductor_core::Config = match request.args.get("config") {
            Some(v) => match serde_json::from_value(v.clone()) {
                Ok(c) => c,
                Err(e) => {
                    return ipc_err(
                        id,
                        IpcErrorCode::ConfigParseFailed,
                        format!("SaveConfig invalid `config`: {e}"),
                    );
                }
            },
            None => {
                return ipc_err(
                    id,
                    IpcErrorCode::MissingField,
                    "SaveConfig requires a `config` object in args",
                );
            }
        };
        // #2417 (B2 completion, ADR-034 §D4 / ADR-043): optional content-hash
        // guard. The `base_generation` CAS below only protects storage atomicity
        // (the GUI re-fetches a fresh generation), so it can't see a content
        // change that landed between the client's `GetConfigBody` read and this
        // save. When the client supplies the `base_revision` it displayed, reject
        // the save if the live revision has since diverged — content-hash, NOT
        // generation, so a daemon self-write that bumps the generation without
        // changing content does not false-positive. Absent ⇒ no guard (back-compat).
        // Distinguish ABSENT (no guard, back-compat) from PRESENT-but-not-a-string
        // (malformed). Collapsing them via `.and_then(as_str)` would let a buggy
        // client silently disable the guard with `base_revision: null` (or a
        // number/object) and reintroduce the clobber — so a present non-string is
        // a hard error, not a skip (Copilot #2418).
        if let Some(base_revision_val) = request.args.get("base_revision") {
            let Some(base_revision) = base_revision_val.as_str() else {
                return ipc_err(
                    id,
                    IpcErrorCode::MissingField,
                    "SaveConfig `base_revision` must be a string when present \
                     (omit it entirely to skip the content-hash guard)",
                );
            };
            let current_revision = format!("{}", self.live_config.load().revision);
            if base_revision != current_revision {
                return ipc_err(
                    id,
                    IpcErrorCode::StaleBaseContent,
                    format!(
                        "base_revision is stale: supplied={base_revision}, current={current_revision}"
                    ),
                );
            }
        }

        // ADR-025 Phase 3.F (#886): cancel any pending old-config
        // observation sleeper before the awaited compile + swap below.
        self.abort_pending_pc_observation_check();

        // ADR-044 Phase 2: PREPARE before committing — a GUI-supplied config
        // that can't build (bad listener ACL, mapping compile failure) is
        // rejected here without ever publishing it (atomic; closes the
        // committed-but-stale window the pre-#2100 path had).
        let prepared = match self.prepare_runtime(&config).await {
            Ok(p) => p,
            Err(e) => {
                return ipc_err(
                    id,
                    IpcErrorCode::ConfigValidationFailed,
                    format!("config rejected before commit: {e}"),
                );
            }
        };

        let prov = provenance_for(caller_ctx, conductor_core::config::Source::InMemoryEdit);
        // #2554: no self-write suppression is armed here. The mutate writes only
        // `live.toml`, which the §D9 watcher does NOT watch (post-#2551 it watches
        // the user file `config.toml`) — so there is nothing to suppress, and a
        // stale arm would wrongly drop a genuine external `config.toml` edit within
        // its window. (The §D11 write-through that armed this was removed, Option C.)
        match self
            .live_config
            .mutate(
                prov,
                base_generation,
                crate::daemon::live_config::ConfigOp::ReplaceWhole {
                    config: Box::new(config),
                },
            )
            .await
        {
            Ok(outcome) => {
                // #2051 (ADR-043 D2): the mutate committed the new config to
                // LiveConfig, but the running daemon's mapping engine,
                // connector registry, device_output_map and route engine are
                // only rebuilt by the post-commit path — without this a
                // GUI-created endpoint never reaches the routing graph (empty
                // graph, idle status, LLM-blind) until a restart.
                //
                // #2100 Phase 2 (ADR-044): install the PREPAREd bundle via the
                // revision-equivalence guard. The mapping engine, connector
                // registry, device_output_map, route engine, listeners, etc. are
                // all rebuilt to match the committed config; the build already
                // succeeded pre-commit (above), so APPLY is infallible save for
                // the rare re-prepare path on a revision mismatch. This closes
                // the pre-#2100 committed-but-stale window. The IPC dispatch
                // backstop would also catch a forgotten apply, but applying
                // inline makes the rebuild land before the SaveConfig response.
                // APPLY is infallible (ADR-044) — it runs post-commit, so it
                // never fails an already-committed config (Council #2168 R3).
                self.apply_committed_guarded(prepared, "save").await;
                // ADR-043 Option C (#2554): NO write-back to the profile/user file.
                // `live.toml` is the sole durable authority — the CAS commit above
                // persists it and the GUI reads it via GetConfigBody (#2540/#2552).
                // The #2260 virtual-port-delete guarantee holds via `live.toml`,
                // which the daemon resumes on boot. (Removed the §D11 write-through.)
                create_success_response(
                    &id,
                    Some(json!({
                        "state_generation": outcome.applied_generation,
                        "previous_generation": outcome.previous_generation,
                        "revision": format!("{}", outcome.applied_revision),
                    })),
                )
            }
            Err(e) => {
                let (code, message) = mutate_error_to_ipc(&e);
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

    /// Write `config` to the daemon's `user_file_path` (`config.toml`) via
    /// `Config::save`, with the §D9 self-write suppression armed so the
    /// ConfigWatcher — which watches that same user file (#2551) — doesn't
    /// surface our own write as external drift. Returns the failure message on error (and
    /// clears the suppression marker, since no `ConfigFileChanged` will arrive to
    /// consume it — Copilot #2264). Used by the error-surfacing "Overwrite
    /// user.toml" handler ([`handle_overwrite_config_file`], #2284). (The §D11
    /// SaveConfig/plan-apply write-through that also used this was removed under
    /// ADR-043 Option C, #2554.)
    async fn armed_profile_write(
        &self,
        config: &conductor_core::Config,
    ) -> std::result::Result<(), String> {
        // #2553: write the operator-editable USER file (`config.toml`), NOT the
        // `live.toml` authority (`config_path`). The Overwrite action means "my
        // live config wins — make the user file match it"; targeting the
        // authority left `config.toml` stale (the reported symptom).
        let path_str = self
            .user_file_path
            .to_str()
            .map(str::to_owned)
            .ok_or_else(|| {
                format!(
                    "user config path is not valid UTF-8: {:?}",
                    self.user_file_path
                )
            })?;
        // Suppress the watcher reload for our own write. Window matches
        // persist_mode_change (> the 500ms ConfigWatcher debounce).
        {
            let mut guard = self.config_write_suppress.lock().await;
            *guard = Some(Instant::now());
        }
        let config = config.clone();
        // Map the (non-Send) `Box<dyn Error>` to a String inside the closure so
        // the JoinHandle's payload is Send (mirrors persist_mode_change).
        let result = match tokio::task::spawn_blocking(move || {
            config.save(&path_str).map_err(|e| e.to_string())
        })
        .await
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(format!("save failed: {e}")),
            Err(e) => Err(format!("join error: {e}")),
        };
        if result.is_err() {
            // The write never reached the atomic rename, so no `ConfigFileChanged`
            // event will fire to consume the suppression we just armed. Clear it
            // now, else a genuine external edit arriving within the 1s window would
            // be wrongly treated as our own write and dropped (Copilot #2264).
            *self.config_write_suppress.lock().await = None;
        }
        result
    }

    /// Overwrite the on-disk config file with the daemon's live config — the
    /// "Overwrite user.toml" drift-banner action (#2284, ADR-034 §D4.D), i.e. "my
    /// live config wins". Writes the live snapshot to `user_file_path`
    /// (`config.toml`, #2553) through the §D9-suppressed [`armed_profile_write`]
    /// so the watcher doesn't re-surface it as drift. A dedicated op rather than `SaveConfig` with the live body, which
    /// is a semantic-identical no-op (same revision ⇒ no CAS bump ⇒ no
    /// write-through) and would leave the drifted file untouched.
    ///
    /// The `state_generation == 0` guard below is the *sole* runtime protection
    /// against persisting an unbootstrapped config: the AwaitingConfig sentinel
    /// has no committed live config to write. The `allowed_during_awaiting_config`
    /// accept-list is spec-only scaffolding — its dispatch-level filter was removed
    /// as dead code (#1323, AwaitingConfig is unreachable at runtime today), so it
    /// does NOT gate this command; the reject-list test pins the spec for an
    /// eventual reinstatement, nothing more. Production boots to `state_generation
    /// >= 1`, so this guard only ever trips defensively.
    pub(crate) async fn handle_overwrite_config_file(&self, id: String) -> IpcResponse {
        let snap = self.live_config.load();
        if snap.state_generation == 0 {
            return ipc_err(
                id,
                IpcErrorCode::ConfigNotFound,
                "no live config to write to disk (daemon is awaiting its initial config)",
            );
        }
        match self.armed_profile_write(snap.config.as_ref()).await {
            Ok(()) => create_success_response(
                &id,
                Some(json!({ "revision": format!("{}", snap.revision) })),
            ),
            Err(msg) => ipc_err(
                id,
                IpcErrorCode::InternalError,
                format!("failed to overwrite config file: {msg}"),
            ),
        }
    }

    pub(crate) async fn handle_reload_from_disk_or_import(
        &mut self,
        request: &crate::daemon::types::IpcRequest,
        id: String,
        caller_ctx: &Option<crate::security::CallerContext>,
    ) -> IpcResponse {
        // ReloadFromDisk: re-read the daemon's config file (or an
        // explicit `--path` for diagnostic loads) and republish via
        // the mutate seam. ImportConfig: same semantics but `path`
        // is REQUIRED — a distinct command so the audit trail can
        // tell an operator reload from a targeted import. Both record
        // Source::DiskImport { path, revision }.
        let is_import = matches!(request.command, IpcCommand::ImportConfig);

        let Some(base_generation) = request
            .args
            .get("base_generation")
            .and_then(serde_json::Value::as_u64)
        else {
            return ipc_err(
                id,
                IpcErrorCode::MissingField,
                "ReloadFromDisk/ImportConfig requires a u64 `base_generation`",
            );
        };

        let arg_path = request
            .args
            .get("path")
            .and_then(|v| v.as_str())
            .map(std::path::PathBuf::from);
        // Whether the path is caller-supplied (untrusted → §D2.2 safe-walk) vs
        // the daemon's own config_path (trusted → direct read).
        let caller_supplied_path = arg_path.is_some();

        let path = match (arg_path, is_import) {
            (Some(p), _) => {
                // §D2 lexical pre-checks (absolute, no `..`, ends `.toml`).
                if let Err(msg) = lexical_config_path_ok(&p) {
                    return ipc_err(id, IpcErrorCode::InvalidRequest, msg);
                }
                p
            }
            // ImportConfig requires an explicit path.
            (None, true) => {
                return ipc_err(
                    id,
                    IpcErrorCode::MissingField,
                    "ImportConfig requires a `path` (.toml) in args",
                );
            }
            // ReloadFromDisk with no path defaults to the daemon's
            // own config file — the "I hand-edited it, pick it up" flow.
            (None, false) => self.config_path.clone(),
        };

        // Read the config text. A caller-supplied path is untrusted: it goes
        // through the ADR-034 §D2.2 safe-walk, which opens the file beneath the
        // config-directory allowlist root without following any symlink, then
        // reads from the *validated fd* (no re-open → no TOCTOU). The daemon's
        // own config_path is trusted and read directly.
        let text = if caller_supplied_path {
            // Allowlist root = the daemon's config directory. Root derivation
            // (config_path → parent) is co-located with the safe-walk *inside*
            // the blocking task, so there is no check-then-use window on the
            // root. The blocking open/openat/read syscalls run on a worker.
            // SAFETY: geteuid is infallible and has no preconditions.
            let expected_uid = unsafe { libc::geteuid() };
            let config_path = self.config_path.clone();
            let target = path.clone();
            let read = tokio::task::spawn_blocking(move || {
                crate::daemon::path_validation::read_config_beneath_config_dir(
                    &config_path,
                    &target,
                    expected_uid,
                )
            })
            .await;
            match read {
                Ok(Ok(t)) => t,
                Ok(Err(crate::daemon::path_validation::SafeReadError::Validation(e))) => {
                    use crate::daemon::path_validation::PathRejectReason;
                    // A genuinely missing component is an operator typo, not a
                    // security rejection: surface ConfigNotFound (as the trusted
                    // read path does) and do NOT audit it as a validation failure.
                    if e.reason_code() == PathRejectReason::TargetNotFound {
                        return ipc_err(
                            id,
                            IpcErrorCode::ConfigNotFound,
                            format!("read {}: not found", path.display()),
                        );
                    }
                    // Audit the STABLE coarse discriminator (e.g. `symlink_in_path`)
                    // so records aggregate cleanly; the IPC message carries the
                    // fuller human reason for the operator.
                    #[cfg(feature = "audit-db")]
                    if let Some(logger) = &self.audit_logger {
                        logger.log_path_validation_failed(
                            &path.display().to_string(),
                            e.reason_code().as_str(),
                        );
                    }
                    return ipc_err(id, IpcErrorCode::PathValidationFailed, e.reason());
                }
                Ok(Err(crate::daemon::path_validation::SafeReadError::Read(e))) => {
                    let code = if e.kind() == std::io::ErrorKind::NotFound {
                        IpcErrorCode::ConfigNotFound
                    } else {
                        IpcErrorCode::InternalError
                    };
                    return ipc_err(id, code, format!("read {}: {e}", path.display()));
                }
                Err(join) => {
                    return ipc_err(
                        id,
                        IpcErrorCode::InternalError,
                        format!("path-validation task failed: {join}"),
                    );
                }
            }
        } else {
            // Async fs read so the Tokio worker isn't blocked. Distinguish
            // NotFound (operator gave a bad path) from other IO errors
            // (permission denied, is-a-directory, …) so the error code isn't
            // a misleading ConfigNotFound for every failure.
            match tokio::fs::read_to_string(&path).await {
                Ok(t) => t,
                Err(e) => {
                    let code = if e.kind() == std::io::ErrorKind::NotFound {
                        IpcErrorCode::ConfigNotFound
                    } else {
                        IpcErrorCode::InternalError
                    };
                    return ipc_err(id, code, format!("read {}: {e}", path.display()));
                }
            }
        };
        let config: conductor_core::Config = match toml::from_str(&text) {
            Ok(c) => c,
            Err(e) => {
                return ipc_err(
                    id,
                    IpcErrorCode::ConfigParseFailed,
                    format!("parse {}: {e}", path.display()),
                );
            }
        };

        // Revision over the SAME normalizer the snapshot uses, so the
        // DiskImport provenance carries a comparable content hash.
        let revision = match crate::daemon::live_config::revision_for(&config) {
            Ok(r) => r,
            Err(e) => {
                return ipc_err(
                    id,
                    IpcErrorCode::ConfigValidationFailed,
                    format!("revision {}: {e}", path.display()),
                );
            }
        };
        // ADR-044 Phase 2: PREPARE before committing — a disk/import config that
        // can't build is rejected here without ever publishing it (atomic).
        let prepared = match self.prepare_runtime(&config).await {
            Ok(p) => p,
            Err(e) => {
                return ipc_err(
                    id,
                    IpcErrorCode::ConfigValidationFailed,
                    format!("config rejected before commit: {e}"),
                );
            }
        };

        let prov = provenance_for(
            caller_ctx,
            conductor_core::config::Source::DiskImport {
                path: path.clone(),
                revision,
            },
        );
        match self
            .live_config
            .mutate(
                prov,
                base_generation,
                crate::daemon::live_config::ConfigOp::ReplaceWhole {
                    config: Box::new(config),
                },
            )
            .await
        {
            Ok(outcome) => {
                // #2100 Phase 2 (ADR-044): install the PREPAREd bundle via the
                // revision-equivalence guard — rebuilds the mapping engine,
                // registry, route engine, device map, listeners, etc. to match
                // the committed config. Build already succeeded pre-commit, so
                // this is atomic (no committed-but-stale window).
                let reason = if is_import {
                    "import"
                } else {
                    "reload-from-disk"
                };
                // APPLY is infallible (ADR-044) — it runs post-commit, so it
                // never fails an already-committed config (Council #2168 R3).
                self.apply_committed_guarded(prepared, reason).await;
                create_success_response(
                    &id,
                    Some(json!({
                        "state_generation": outcome.applied_generation,
                        "previous_generation": outcome.previous_generation,
                        "revision": format!("{}", outcome.applied_revision),
                        "source_path": path.display().to_string(),
                    })),
                )
            }
            Err(e) => {
                let (code, message) = mutate_error_to_ipc(&e);
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
}
