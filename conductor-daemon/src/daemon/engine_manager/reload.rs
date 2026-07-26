// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! `EngineManager` methods extracted from `engine_manager::mod` (refactor #2073).

use super::*;

/// Runtime artifacts built from a config by the **fallible** PREPARE phase
/// (#2100 / ADR-044), ready to be installed by the **infallible** APPLY phase
/// (`apply_prepared`). Phase 1 (this change) builds these immediately before
/// applying them — behaviour-preserving; Phase 2 moves PREPARE *before* the
/// `LiveConfig` commit so a build failure rejects the change without leaving
/// the config committed but the runtime stale.
pub(crate) struct PreparedRuntime {
    /// Compiled mapping engine (the expensive `spawn_blocking` step).
    mapping_engine: MappingEngine,
    /// Wall-time of the mapping compile, surfaced in `ReloadMetrics`.
    mapping_compile_ms: u64,
    /// Unified endpoint set (`normalize_to_endpoints`) — shared by the
    /// connector-registry rebuild and the output-map / device-status rebuild
    /// so it is normalized exactly once.
    endpoints: Vec<conductor_core::config::types::EndpointConfig>,
    /// Parsed (not yet bound) network listener set
    /// (`ListenerManager::from_config`) — the fallible config/ACL parse.
    listeners: crate::listeners::ListenerManager,
    /// Content revision of the config these artifacts were built from
    /// (`revision_for`). Phase 2's revision-equivalence guard compares this to
    /// the committed snapshot's revision before APPLY: if they differ (a future
    /// content-transforming `ConfigOp`, or — defensively — a competing commit),
    /// the prepared bundle is discarded and re-prepared from the committed
    /// snapshot. (ADR-044.)
    target_revision: conductor_core::config::ConfigRevision,
}

/// Outcome of the **infallible** APPLY phase (#2100 / ADR-044). Returned by
/// `apply_prepared` so the post-commit rebuild reports what it did (and what
/// it skipped) as a value rather than only via logs — APPLY never returns a
/// `Result`, so a rebuild can never `?`-bail and leave the runtime half-built.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct ApplyReport {
    /// Mapping-engine compile duration carried from PREPARE (ms).
    pub mapping_compile_ms: u64,
    /// Network listeners successfully bound.
    pub listeners_bound: usize,
    /// Network listeners skipped (bind failed — non-fatal, logged + audited).
    pub listeners_skipped: usize,
    /// Input ports opened / removed / re-keyed by the rescan.
    pub ports_opened: usize,
    pub ports_removed: usize,
    pub ports_rekeyed: usize,
}

impl EngineManager {
    /// Reload configuration with atomic swap
    pub(crate) async fn reload_config(&mut self) -> Result<ReloadMetrics> {
        let start = Instant::now();

        // ADR-025 Phase 3.F (#886): abort any pending observation check
        // BEFORE any awaited swap work. If the old sleeper's grace period
        // ended during the file-I/O + compile + lock-swap work below, it
        // would otherwise wake and emit a warning with stale-context
        // expected-set before the new check could be scheduled. The
        // end-of-swap `schedule_pc_observation_check` call also aborts,
        // but only at that point — too late if the sleeper already woke.
        self.abort_pending_pc_observation_check();

        // Check current state
        let current_state = *self.state.read().await;

        // If already reloading, skip silently to avoid race conditions
        // (happens when config file watcher fires multiple times quickly)
        if current_state == LifecycleState::Reloading {
            debug!("Already reloading config, skipping duplicate reload request");
            return Ok(ReloadMetrics {
                duration_ms: 0,
                modes_loaded: 0,
                mappings_loaded: 0,
                config_load_ms: 0,
                mapping_compile_ms: 0,
                swap_ms: 0,
            });
        }

        // Transition to Reloading state
        if !current_state.can_transition_to(LifecycleState::Reloading) {
            return Err(DaemonError::InvalidStateTransition {
                from: format!("{}", current_state),
                to: "Reloading".to_string(),
            });
        }
        self.transition_state(LifecycleState::Reloading).await?;

        // Run the fallible reload body; on ANY error restore the pre-Reloading
        // state. Otherwise a failed reload (bad file, validation error, PREPARE
        // failure) leaves the daemon stuck in `Reloading`, and the
        // top-of-function "skip while Reloading" guard then silently no-ops
        // every subsequent reload — bricking config reload until restart.
        // `run_loop` only logs a reload error and does NOT reset state, so
        // `reload_config` must restore it itself (Council #2168 R1).
        let outcome = self.reload_config_after_lock(start).await;
        if outcome.is_err()
            && let Err(te) = self.transition_state(current_state).await
        {
            warn!(
                "Failed to restore {:?} after reload failure, forcing: {}",
                current_state, te
            );
            *self.state.write().await = current_state;
        }
        outcome
    }

    /// The fallible body of [`Self::reload_config`], split out so the wrapper can
    /// restore the pre-`Reloading` lifecycle state on any error (Council #2168
    /// R1). On success it transitions back to `Running` itself.
    async fn reload_config_after_lock(&mut self, start: Instant) -> Result<ReloadMetrics> {
        // Phase 1: Load and validate new config
        // v4.14.0: Properly handle non-UTF8 paths (LLM Council feedback v4.13.3)
        let config_load_start = Instant::now();
        let config_path_str =
            pathbuf_to_str_or_err(&self.config_path, "config_path in reload_config")?;
        let new_config = Config::load(config_path_str)
            .map_err(|e| DaemonError::Ipc(format!("Config load failed: {}", e)))?;
        let config_load_ms = config_load_start.elapsed().as_millis() as u64;

        // Phase 1b: Schema validation (v4.26.77)
        let validation_report = conductor_core::config::validator::validate_config(&new_config);
        for finding in &validation_report.warnings {
            warn!(
                "Config validation warning: {}: {}",
                finding.path, finding.message
            );
        }
        if !validation_report.is_valid() {
            for finding in &validation_report.errors {
                error!(
                    "Config validation error: {}: {}",
                    finding.path, finding.message
                );
            }
            return Err(DaemonError::Ipc(format!(
                "Config validation failed with {} error(s)",
                validation_report.errors.len()
            )));
        }

        // ADR-044 Phase 2 — atomic PREPARE → COMMIT → APPLY:
        //  1. PREPARE the new config (compile mapping engine, normalize
        //     endpoints, parse listeners) BEFORE committing — a build failure
        //     returns here without ever publishing the config (no split-brain).
        //  2. COMMIT through LiveConfig (RealRuleCompiler recompiles the
        //     lock-free rule set inside `mutate()`).
        //  3. APPLY the prepared artifacts (revision-guarded, infallible).
        let swap_start = Instant::now();

        let prepared = self.prepare_runtime(&new_config).await?;

        // D4.A.3.2: route the full-config swap through LiveConfig.
        {
            let live = Arc::clone(&self.live_config);
            let snap = live.load();
            live.mutate(
                self.default_cli_provenance(),
                snap.state_generation,
                crate::daemon::live_config::ConfigOp::ReplaceWhole {
                    config: Box::new(new_config.clone()),
                },
            )
            .await
            .map_err(|e| DaemonError::Ipc(format!("live_config mutate: {e}")))?;
        }

        // APPLY is infallible (ADR-044): everything below is post-commit, so a
        // failure here must NOT propagate as `Err` — the wrapper would then
        // revert the lifecycle state away from the now-committed config (Council
        // #2168 R3).
        let mapping_compile_ms = self
            .apply_committed_guarded(prepared, "reload")
            .await
            .mapping_compile_ms;

        let swap_ms = swap_start.elapsed().as_millis() as u64;

        // Calculate metrics
        let duration_ms = start.elapsed().as_millis() as u64;
        let total_mappings: usize = new_config
            .modes
            .iter()
            .map(|m| m.mappings.len())
            .sum::<usize>()
            + new_config.global_mappings.len();

        let metrics = ReloadMetrics {
            duration_ms,
            modes_loaded: new_config.modes.len(),
            mappings_loaded: total_mappings,
            config_load_ms,
            mapping_compile_ms,
            swap_ms,
        };

        // Update statistics
        {
            let mut stats = self.statistics.write().await;
            stats.update_reload_metrics(&metrics);
        }

        info!(
            "Config reloaded in {}ms (grade: {}): {} modes, {} mappings [load: {}ms, compile: {}ms, swap: {}ms]",
            duration_ms,
            metrics.performance_grade(),
            metrics.modes_loaded,
            metrics.mappings_loaded,
            config_load_ms,
            mapping_compile_ms,
            swap_ms
        );

        // Phase 4: Create known_good backup on successful validation (v4.26.77).
        // #2173: serialize the VALIDATED IN-MEMORY config rather than
        // `std::fs::copy`-ing the file from disk. The copy re-read the on-disk
        // file AFTER load+validate, so a concurrent edit landing in that window
        // could be captured into `*.toml.known_good` — a backup that never passed
        // validation (TOCTOU). `new_config` is exactly what we loaded, validated,
        // PREPAREd, and committed. Best-effort (warn on failure), as before.
        let known_good_path = self.config_path.with_extension("toml.known_good");
        match pathbuf_to_str_or_err(&known_good_path, "known_good backup path") {
            Ok(known_good_str) => {
                if let Err(e) = new_config.save(known_good_str) {
                    warn!("Failed to create known_good config backup: {}", e);
                } else {
                    debug!("Config backup saved to {:?}", known_good_path);
                }
            }
            Err(e) => warn!("known_good backup path is not valid UTF-8: {}", e),
        }

        // Transition back to Running. The config is committed and the runtime
        // applied by this point, so a failed transition must NOT bubble up as an
        // `Err` (the wrapper would revert the lifecycle state away from the live
        // config). Force-set as a last resort, mirroring `reload_from_cached_config`
        // (Council #2168 R3).
        if let Err(e) = self.transition_state(LifecycleState::Running).await {
            warn!(
                "Failed to transition to Running after reload, forcing: {}",
                e
            );
            *self.state.write().await = LifecycleState::Running;
        }

        // #1070: broadcast a config_reloaded signal so subscribed GUIs
        // refresh immediately instead of waiting up to one 3s poll. This
        // covers every reload_config() trigger uniformly — file-watcher
        // edits, tray-menu Reload, and `conductorctl reload`. Emitting on
        // the existing MonitorEvent broadcast (rather than a bespoke
        // channel) follows the pattern #943 established for
        // `ambiguous_port_detected`. Fire-and-forget: a send error just
        // means no subscribers, which is the normal headless-daemon case.
        let reloaded_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let _ = self.event_broadcast_tx.send(MonitorEvent {
            timestamp_ms: reloaded_ms,
            event_type: "config_reloaded".to_string(),
            payload: Some(json!({
                "success": true,
                "duration_ms": metrics.duration_ms,
                "modes_loaded": metrics.modes_loaded,
                "mappings_loaded": metrics.mappings_loaded,
            })),
            ..Default::default()
        });

        Ok(metrics)
    }

    /// Post-commit runtime rebuild (#2051 / ADR-043 D2), now expressed as the
    /// PREPARE→APPLY pair (#2100 / ADR-044 Phase 1): build the fallible
    /// artifacts (`prepare_runtime`) then install them (`apply_prepared`).
    ///
    /// Behaviour-preserving — still invoked post-commit by
    /// `reconcile_runtime_to_live`; Phase 2 moves PREPARE *before* the
    /// `LiveConfig` commit so a build failure rejects the change without
    /// leaving the config committed but the runtime stale.
    ///
    /// `reason` labels the PC-state log + observation check ("reload",
    /// "save", …). `new_config` is the already-committed config. Returns the
    /// mapping-compile duration so `reload_config` can report it.
    pub(crate) async fn apply_committed_config(
        &mut self,
        new_config: &Config,
        reason: &str,
    ) -> Result<u64> {
        let prepared = self.prepare_runtime(new_config).await?;
        let report = self.apply_prepared(prepared, new_config, reason).await;
        Ok(report.mapping_compile_ms)
    }

    /// PREPARE phase (#2100 / ADR-044) — **fallible, no side effects.**
    ///
    /// Builds the artifacts whose construction can fail — the MappingEngine
    /// compile, endpoint normalization, and the network-listener config/ACL
    /// parse — so the infallible APPLY phase only installs ready values. In
    /// Phase 2 the committing call sites run this *before* the `LiveConfig`
    /// commit, so a build failure rejects the change atomically. Takes `&self`
    /// (no installation here); `&mut self` install is `apply_prepared`.
    pub(crate) async fn prepare_runtime(&self, new_config: &Config) -> Result<PreparedRuntime> {
        // MappingEngine compile — the expensive step, off the runtime thread.
        // (LiveConfig's RealRuleCompiler recompiles the lock-free rule set
        // inside `mutate()`; this is the separate MappingEngine.)
        let mapping_compile_start = Instant::now();
        let compile_config = new_config.clone();
        let mapping_engine = tokio::task::spawn_blocking(move || {
            let mut engine = MappingEngine::new();
            engine.load_from_config(&compile_config);
            engine
        })
        .await
        .map_err(|e| DaemonError::Ipc(format!("Compilation task failed: {}", e)))?;
        let mapping_compile_ms = mapping_compile_start.elapsed().as_millis() as u64;

        // Unified endpoint set (ADR-035 Slice 6) — normalized once and reused
        // by the APPLY registry / output-map / device-status steps.
        let (endpoints, _findings) =
            conductor_core::config::loader::normalize_to_endpoints(new_config)?;

        // Parse — but do NOT bind — the network listener set. This is the
        // fallible config/ACL parse (`ListenerManager::from_config`); the
        // actual socket bind is non-fatal and happens in APPLY.
        let listeners = crate::listeners::ListenerManager::from_config(new_config)
            .map_err(|e| DaemonError::Ipc(format!("network listener config error: {e}")))?;

        // Content revision of the candidate, for the Phase 2 revision-equivalence
        // guard. Matches the digest the `LiveConfig` snapshot publishes, so an
        // exact compare tells us whether the committed config is the one we
        // prepared from. (ADR-044.)
        let target_revision = crate::daemon::live_config::revision_for(new_config)
            .map_err(|e| DaemonError::Ipc(format!("revision compute: {e}")))?;

        Ok(PreparedRuntime {
            mapping_engine,
            mapping_compile_ms,
            endpoints,
            listeners,
            target_revision,
        })
    }

    /// Apply an already-PREPAREd runtime to the just-committed config with the
    /// **revision-equivalence guard** (#2100 Phase 2 / ADR-044). Committing
    /// call sites run `prepare_runtime(candidate)` *before* the `LiveConfig`
    /// commit (so a build failure rejects the change without committing), then
    /// commit, then call this. If the committed snapshot's revision matches
    /// what `prepared` was built from, install it directly; otherwise — a
    /// future content-transforming `ConfigOp`, or defensively a competing
    /// commit that landed in the PREPARE→COMMIT window (the CAS on the commit
    /// already prevents this in practice) — discard the prepared bundle and
    /// re-prepare from the committed snapshot. Advances `last_reconciled_revision`
    /// so the dispatch backstop no-ops.
    ///
    /// **Infallible** (ADR-044: APPLY runs after the commit, so it must never
    /// fail the operation — the config is already the source of truth). The lone
    /// fallible step is the re-prepare on a revision mismatch; if that fails
    /// (only reachable for a future content-transforming `ConfigOp` — never for
    /// the `ReplaceWhole` commits used today, where committed == prepared) we
    /// keep the current runtime, leave `last_reconciled_revision` un-advanced so
    /// the dispatch backstop (`reconcile_runtime_to_live`) retries the rebuild,
    /// and return a default report. We never `?`-bail: bailing post-commit would
    /// report a live config as failed and (via callers' restore logic) revert
    /// the lifecycle state away from the committed config (Council #2168 R3).
    pub(crate) async fn apply_committed_guarded(
        &mut self,
        prepared: PreparedRuntime,
        reason: &str,
    ) -> ApplyReport {
        let (committed_revision, committed_cfg) = {
            let snap = self.live_config.load();
            (snap.revision, (*snap.config).clone())
        };
        let report = if committed_revision == prepared.target_revision {
            self.apply_prepared(prepared, &committed_cfg, reason).await
        } else {
            warn!(
                "post-commit revision mismatch (prepared {} != committed {}); \
                 re-preparing from the committed snapshot before APPLY (ADR-044)",
                prepared.target_revision, committed_revision
            );
            match self.prepare_runtime(&committed_cfg).await {
                Ok(re_prepared) => {
                    self.apply_prepared(re_prepared, &committed_cfg, reason)
                        .await
                }
                Err(e) => {
                    // Post-commit re-prepare failed. Keep the current runtime and
                    // leave `last_reconciled_revision` un-advanced so the dispatch
                    // backstop retries against the committed snapshot. Never bail.
                    error!(
                        "post-commit re-prepare failed ({e}); keeping current runtime, \
                         dispatch backstop will retry against committed revision {committed_revision}"
                    );
                    return ApplyReport::default();
                }
            }
        };
        self.last_reconciled_revision = Some(committed_revision);
        report
    }

    /// APPLY phase (#2100 / ADR-044) — **infallible** install of a
    /// `PreparedRuntime`. Returns an [`ApplyReport`] (never a `Result`) so a
    /// rebuild can't `?`-bail mid-way and leave the runtime split-brained.
    /// `new_config` is the committed config (== the one `prepared` was built
    /// from in Phase 1); `reason` labels the PC-state re-log.
    pub(crate) async fn apply_prepared(
        &mut self,
        prepared: PreparedRuntime,
        new_config: &Config,
        reason: &str,
    ) -> ApplyReport {
        let PreparedRuntime {
            mapping_engine,
            mapping_compile_ms,
            endpoints,
            listeners,
            // The revision guard already consumed this in `apply_committed_guarded`.
            target_revision: _,
        } = prepared;
        let mut report = ApplyReport {
            mapping_compile_ms,
            ..Default::default()
        };

        // Mode reconciliation reads the pre-swap current_mode. The store
        // isn't updated until the reconcile step at the end of this method.
        let old_mode = self.current_mode.load();
        let old_mode_index = old_mode.index;
        let old_mode_name = if old_mode.name.is_empty() {
            None
        } else {
            Some(old_mode.name.clone())
        };

        // Install the prepared MappingEngine (compiled in PREPARE) — the single
        // place every committed-config apply path swaps the runtime engine.
        let _new_version = self.rule_set_version.fetch_add(1, Ordering::Relaxed) + 1;
        *self.mapping_engine.write().await = mapping_engine;

        // ADR-031 § 4.4: rebuild route engine alongside rule_set so
        // stage-9 reflects the new `[[routes]]` config. Same atomic
        // swap pattern.
        let new_route_engine = crate::route_engine::RouteEngine::compile(&new_config.routes);
        new_route_engine.log_exclusions();
        self.route_engine.store(Arc::new(new_route_engine));

        // (Connector registry is rebuilt once, in the device-output-map block
        // below, from this same prepared endpoint set — the earlier pre-#2100
        // code rebuilt it twice from identical endpoints with nothing reading
        // the registry in between; collapsed to a single rebuild. Council
        // review on #2163.)

        // Network listeners: bind the prepared (already-parsed) set. The config
        // parse happened in PREPARE; here we only bind, and a bind failure is
        // non-fatal — logged + audited and counted in the report, never failing
        // the apply (ADR-044: APPLY is infallible). `bind_network_listeners`
        // stops the old set first (idempotent).
        let (listeners_bound, listeners_skipped) =
            self.bind_network_listeners(listeners, new_config).await;
        report.listeners_bound = listeners_bound;
        report.listeners_skipped = listeners_skipped;

        // Reset rate limiter with new config limit (v4.26.0 - ADR-009 D9)
        self.device_rate_limiter =
            DeviceRateLimiter::new(new_config.advanced_settings.max_events_per_sec);

        // #2486 + #2490: re-apply ALL configurable timing knobs (chord +
        // double-tap + hold) to EXISTING per-device EventProcessors so a runtime
        // config change reaches already-seen devices — not just newly-created
        // ones (processors only read these at creation; before #2490 only chord
        // was re-applied, and hold/double-tap were never applied at all). Chord
        // is Learn-aware per `midi_learn_active`, matching the creation path in
        // `events_device`.
        let timings = super::helpers::event_timings_from_config(
            self.midi_learn_active.load(Ordering::SeqCst),
            &new_config.advanced_settings,
        );
        for mut entry in self.event_processors.iter_mut() {
            entry.value_mut().apply_timings(timings);
        }

        // ADR-027 §D10b: re-apply the shell-sandbox policy so a changed
        // `security.shell.allow_unsandboxed` takes effect on reload (Copilot
        // review — startup-only wiring meant reloads were ignored). Moved here
        // from the pre-commit body for ADR-044 Phase 2 atomicity (#2100): this
        // is an infallible side effect, so it must run in APPLY (post-commit)
        // — never before the commit, where a failed PREPARE/COMMIT would leave
        // the executor's policy mutated while the published config still
        // reflects the old one. Living in `apply_prepared` also unifies it
        // across every committing path (reload, cached profile switch,
        // SaveConfig/Init/Import IPC, plan-apply) instead of reload-only.
        self.action_executor
            .lock()
            .await
            .set_shell_security(&new_config.security.shell);

        // Phase 4.1: re-apply the master SysEx probe toggle. Reloading
        // a config that flips `sysex_identity_probing` to false must
        // prevent any subsequent probes from leaving the daemon —
        // including ones queued via the MCP tool moments after the
        // reload completes.
        self.probe_coordinator
            .set_enabled(new_config.advanced_settings.sysex_identity_probing);

        // Phase 4.2: re-emit the no_probe + SysExIdentity override
        // warning for the new config. A reload that introduces or
        // removes the conflict should produce / suppress the message
        // accordingly so the operator can spot it without diff'ing
        // configs. Wording describes the override only — actual
        // probing is gated by `sysex_identity_probing`,
        // `probe_on_connect`, paired-output presence, and rate limit
        // (#976 review).
        for alias in new_config.endpoints_with_no_probe_sysex_override() {
            warn!(
                device = %alias,
                "`no_probe = true` is overridden for this device because its bindings \
                 include a SysExIdentity matcher (which cannot resolve without a probe); \
                 actual probing is still subject to the global / per-connect / \
                 rate-limit gates"
            );
        }

        // Refresh event monitor capture flags from new config (profile switch may change these)
        let ec_ref = new_config.event_console.as_ref();
        self.capture_midi = ec_ref.is_none_or(|ec| ec.capture_midi);
        self.capture_processed = ec_ref.is_none_or(|ec| ec.capture_processed);
        self.capture_actions = ec_ref.is_none_or(|ec| ec.capture_actions);

        // #955: re-run port resolution against the new config so that
        // adding/removing/renaming `[[bindings]]` actually affects
        // runtime DeviceIds. Without this rescan, the InputManager
        // keeps the port→DeviceId mapping it built at startup, so
        // events keep flowing under stale aliases until the daemon
        // restarts. We do this BEFORE the device_output_map rebuild
        // below so it picks up the freshly re-keyed bindings.
        // #885: every config now flows through the multi-device manager,
        // so the rescan is unconditional whenever an InputManager exists.
        // #2390: enumerate ports OFF the run-loop (blocking CoreMIDI) BEFORE
        // taking the input_manager lock, then pass them into the cheap diff.
        // #2393: skip the scan in GamepadOnly — rescan_ports discards the result
        // there, so it's pure waste (and emits spurious MIDI warnings on hosts
        // with no MIDI stack).
        let reload_ports = if crate::input_manager::midi_enumeration_enabled(
            new_config.advanced_settings.input_mode,
        ) {
            crate::input_manager::InputManager::enumerate_input_ports_async().await
        } else {
            Ok(Vec::new())
        };
        if let Some(ref mut mgr) = *self.input_manager.lock().await {
            match reload_ports {
                Ok(port_infos) => match mgr.rescan_ports(
                    port_infos,
                    new_config,
                    &self.device_event_tx,
                    &self.command_tx,
                ) {
                    Ok((opened, removed, rekeyed)) => {
                        report.ports_opened = opened;
                        report.ports_removed = removed;
                        report.ports_rekeyed = rekeyed;
                        if opened + removed + rekeyed > 0 {
                            info!(
                                opened,
                                removed,
                                rekeyed,
                                "Reload rescan: {} new, {} removed, {} re-keyed",
                                opened,
                                removed,
                                rekeyed
                            );
                        }
                    }
                    Err(e) => warn!(error = %e, "Reload rescan failed"),
                },
                Err(e) => warn!(error = %e, "Reload rescan: port enumeration failed, skipping"),
            }
        }

        // ADR-021 Phase 2A: Rebuild device_output_map with new config's device/output definitions.
        // #955: Also refresh device_status.devices so the wire-format
        // `device_id`/`is_configured`/`direction` reflect the new
        // resolution after the rescan above. Without this, the
        // status JSON keeps stale DeviceIds even though the InputManager
        // has been re-keyed.
        // #885: status refresh is unconditional now (every config flows
        // through the multi-device manager).
        {
            let (full_bindings, enabled_states) =
                if let Some(ref mgr) = *self.input_manager.lock().await {
                    let b = mgr.get_device_bindings();
                    let e: Vec<bool> = b
                        .iter()
                        .map(|(device_id, _, _, _)| mgr.is_device_enabled(device_id))
                        .collect();
                    (b, e)
                } else {
                    (Vec::new(), Vec::new())
                };

            let input_bindings: Vec<(String, String)> = full_bindings
                .iter()
                .filter(|(_, _, _, is_configured)| *is_configured)
                .map(|(device_id, port_name, _, _)| {
                    (device_id.as_str().to_string(), port_name.clone())
                })
                .collect();
            let output_ports = crate::daemon::output_resolver::enumerate_output_ports_async().await;
            // ADR-035 Slice 6/9.5: build the MIDI output map AND rebuild the
            // signal-routing registry from the prepared (already-normalized)
            // endpoint set. `build_output_map` folds in what the legacy
            // `build_device_output_map` + `build_connector_output_map` pair
            // used to do — including the #1611 output/bidirectional alias
            // resolution for `MidiForward { target = "<alias>" }`.
            let output_map = crate::daemon::output_resolver::build_output_map(
                &endpoints,
                &input_bindings,
                &output_ports,
            );
            let flat_map: HashMap<String, String> = output_map
                .iter()
                .map(|(alias, res)| (alias.clone(), res.port_name.clone()))
                .collect();
            self.device_output_map.store(Arc::new(flat_map));

            // #2063: reconcile OS virtual MIDI ports to the reloaded endpoint
            // set (create added, tear down removed/disabled). Post-commit and
            // infallible, consistent with the ADR-044 APPLY phase.
            self.sync_virtual_ports(&endpoints);

            rebuild_connector_registry(&self.connector_registry, &endpoints).await;

            // Refresh device_status.devices to mirror the post-rescan view.
            let device_statuses: Vec<DevicePortStatus> = full_bindings
                .iter()
                .zip(enabled_states.iter())
                .map(
                    |((device_id, port_name, connected, is_configured), enabled)| {
                        let alias = device_id.as_str();
                        let io = resolve_device_io(alias, *is_configured, &endpoints, &output_map);
                        DevicePortStatus {
                            device_id: alias.to_string(),
                            port_name: port_name.clone(),
                            port_index: 0,
                            connected: *connected,
                            enabled: *enabled,
                            last_event_at: None,
                            is_configured: *is_configured,
                            direction: io.direction,
                            output_connected: io.output_connected,
                            output_port_name: io.output_port_name,
                            output_auto_paired: io.output_auto_paired,
                            // TODO(#742): Currently MIDI-only. Derive from binding source
                            // when HID/OSC devices are added to the binding path.
                            protocol: "midi".to_string(),
                        }
                    },
                )
                .collect();
            self.rebuild_port_name_cache(&device_statuses);
            let any_connected = full_bindings.iter().any(|(_, _, c, _)| *c);
            let first_name = full_bindings.first().map(|(_, name, _, _)| name.clone());
            let mut status = self.device_status.write().await;
            status.connected = any_connected;
            status.name = first_name;
            status.devices = device_statuses;
        }

        // Reconcile current_mode with new config (v4.10.10, v4.21.0)
        // Guard against empty modes array and clamp to valid range
        //
        // SAFETY INVARIANT (v4.13.3 - LLM Council Review):
        // When modes is empty, we set index to 0. This is safe because:
        // 1. All access to config.modes uses .get() which returns Option
        // 2. .get(0) on empty Vec returns None, not a panic
        // 3. Callers must handle None (see get_engine_info, process_device_event)
        let new_mode_index = if new_config.modes.is_empty() {
            0 // Safe: all consumers use .get() which returns None for empty arrays
        } else {
            // Priority: 1) Find old mode by name, 2) Use last_selected_mode, 3) Default to 0
            old_mode_name
                .as_ref()
                .and_then(|name| new_config.modes.iter().position(|m| &m.name == name))
                .or_else(|| {
                    new_config
                        .last_selected_mode
                        .as_ref()
                        .and_then(|name| new_config.modes.iter().position(|m| &m.name == name))
                })
                .unwrap_or(0)
                .min(new_config.modes.len() - 1) // Clamp to valid range
        };

        if new_mode_index != old_mode_index {
            debug!(
                "Mode index reconciled on config reload: {} -> {} (mode: {:?})",
                old_mode_index, new_mode_index, old_mode_name
            );
        }
        let new_mode_name = new_config
            .modes
            .get(new_mode_index)
            .map(|m| m.name.clone())
            .unwrap_or_default();
        self.current_mode.store(Arc::new(ModeState {
            index: new_mode_index,
            name: new_mode_name,
        }));

        // ADR-025 Phase 3.F — re-log PC-state dependencies after the
        // commit so the inventory follows config edits. Also re-schedule
        // the runtime observed-vs-expected warning (#886) — the new
        // config may have a different expected set than the prior one.
        // `reason` labels both so the log line says which path triggered
        // it (reload / save / …).
        control_state_analyzer::log_expected_pc_tuples(new_config, reason);
        self.schedule_pc_observation_check(reason);

        report
    }

    /// Structural post-commit reconcile (#2071, ADR-043 D2/Q2).
    ///
    /// The single, content-guarded entry point that makes the running
    /// daemon's runtime state match the latest committed `LiveConfig`
    /// snapshot. Rebuilds *from the live snapshot itself* (not from a
    /// caller-supplied `Config`), so the runtime is guaranteed to track
    /// what was actually committed — then delegates the rebuild to the
    /// canonical [`Self::apply_committed_config`].
    ///
    /// Idempotent: rebuilds only when the live snapshot's `revision`
    /// differs from the last one we reconciled, so it is a cheap no-op
    /// for read-only IPC commands and for byte-identical commits (e.g.
    /// `MarkKnownGood`, which bumps `state_generation` but not content).
    ///
    /// Per ADR-043 Q2 the rebuild guarantee must be **structural**, not a
    /// flat caller-remembered `apply_committed_config` call that each
    /// mutation site has to remember (the exact pattern that caused
    /// Defect 2). The IPC dispatch backstop in `handle_ipc_request` calls
    /// this after *every* command, so no committed mutation can leave the
    /// runtime stale even if a future handler forgets to reconcile. The
    /// in-routine calls (`reload_config`, `reload_from_cached_config`,
    /// `sync_config_after_apply`, the `SaveConfig`/`Init`/import handlers)
    /// are idempotent fast-paths that also surface the mapping-compile
    /// duration synchronously.
    ///
    /// Returns the mapping-compile duration (ms) from the rebuild, or `0`
    /// when the live content was already reconciled.
    pub(crate) async fn reconcile_runtime_to_live(&mut self, reason: &str) -> Result<u64> {
        // Check the revision guard BEFORE cloning the config: the IPC dispatch
        // backstop calls this after *every* successful command, so the no-op
        // path (read-only commands, byte-identical commits) must stay
        // genuinely cheap — an ArcSwap load + a `Copy` revision compare, with
        // NO deep `Config` clone (Copilot review on #2099). Only clone the
        // committed config when a rebuild is actually due.
        let (revision, committed) = {
            let snap = self.live_config.load();
            if self.last_reconciled_revision == Some(snap.revision) {
                return Ok(0);
            }
            (snap.revision, (*snap.config).clone())
        };
        let mapping_compile_ms = self.apply_committed_config(&committed, reason).await?;
        self.last_reconciled_revision = Some(revision);
        Ok(mapping_compile_ms)
    }
}
