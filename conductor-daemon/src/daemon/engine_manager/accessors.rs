// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! `EngineManager` methods extracted from `engine_manager::mod`.

use super::*;

impl EngineManager {
    /// Get current config info for state persistence
    pub async fn get_config_info(&self) -> Result<ConfigInfo> {
        let checksum = calculate_checksum(&self.config_path).await?;
        let loaded_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Ok(ConfigInfo {
            path: self.config_path.clone(),
            loaded_at,
            checksum,
        })
    }
    /// Get current engine info for state persistence
    pub async fn get_engine_info(&self) -> EngineInfo {
        let device_status = self.device_status.read().await.clone();
        // Lock-free mode read (ADR-009 Phase 3)
        let mode = self.current_mode.load();

        EngineInfo {
            current_mode: if mode.name.is_empty() {
                "None".to_string()
            } else {
                mode.name.clone()
            },
            current_mode_index: mode.index,
            device_status,
        }
    }
    /// Get current statistics
    pub async fn get_statistics(&self) -> DaemonStatistics {
        let mut stats = self.statistics.read().await.clone();
        stats.uptime_secs = self.start_time.elapsed().as_secs();
        stats
    }
    /// Get recent errors
    pub async fn get_recent_errors(&self) -> Vec<ErrorEntry> {
        self.error_log.read().await.clone()
    }
    /// Get current lifecycle state
    pub async fn get_state(&self) -> LifecycleState {
        *self.state.read().await
    }
    /// Get the `LiveConfig` Arc (ADR-034 §D1).
    /// D4.A.3.1 exposes this for tests + downstream wiring; D4.A.3.2
    /// onwards routes all writers through `live_config.mutate()`.
    /// D4.A.3.3.A retired the legacy `Arc<RwLock<Config>>` accessor —
    /// this is now the only config-Arc handout from engine_manager.
    pub fn get_live_config(&self) -> Arc<crate::daemon::live_config::LiveConfig> {
        Arc::clone(&self.live_config)
    }
    /// Default provenance for daemon-internal mutations triggered
    /// by IPC handlers. D4.A.3.2 transition; D4.A.3.3 will route
    /// individual handlers through `live_config.mutate()` directly
    /// with caller-specific Provenance (CLI vs GUI vs LLM).
    pub(crate) fn default_cli_provenance(&self) -> conductor_core::config::Provenance {
        conductor_core::config::Provenance {
            initiator: conductor_core::config::Initiator::Cli,
            source: conductor_core::config::Source::InMemoryEdit,
            peer: None,
        }
    }
    /// Get config path
    pub fn get_config_path(&self) -> &PathBuf {
        &self.config_path
    }
    /// Get the event broadcast sender for push-based event monitoring
    ///
    /// The IPC server uses this to subscribe clients that send `SubscribeEvents`.
    /// Call `tx.subscribe()` to get a new receiver for each subscriber.
    pub fn event_broadcast_tx(&self) -> broadcast::Sender<MonitorEvent> {
        self.event_broadcast_tx.clone()
    }
    /// Audit logger handle (ADR-027 D13a). The IPC server
    /// uses this to serve `SubscribeAudit` streams off the audit
    /// broadcast channel. `None` when audit init failed at startup.
    #[cfg(feature = "audit-db")]
    pub fn audit_logger(&self) -> Option<Arc<crate::daemon::audit::AuditLogger>> {
        self.audit_logger.clone()
    }
    /// ADR-045 D5: the write-side audit seam (SQLite in `audit-db`
    /// builds, JSONL otherwise). The IPC server serves `SubscribeAudit`
    /// off this sink's broadcast channel in every composition.
    pub fn audit_sink(&self) -> Option<Arc<dyn crate::daemon::audit::AuditSink>> {
        self.audit_sink.clone()
    }
    /// Per-device event statistics for fingerprinting (ADR-022 D7)
    pub fn event_stats(&self) -> &Arc<DashMap<String, EventStats>> {
        &self.event_stats
    }

    /// ADR-034 §D8.3 — emit a `ConfigMutationPendingAtCrash` audit event for
    /// each config mutation that was `Pending` in the audit outbox when the
    /// previous daemon died and did not publish (surfaced by startup
    /// reconciliation in `LiveConfig::with_audit_outbox`). Called once at daemon
    /// startup, after construction wires the audit logger. Returns the number of
    /// events emitted (0 when there was a clean shutdown or audit is disabled).
    pub fn emit_pending_at_crash_audit(&self) -> usize {
        let Some(sink) = self.audit_sink.as_ref() else {
            return 0;
        };
        sink.log_pending_at_crash_batch(self.live_config.pending_at_crash())
    }

    /// Set the ConfigWatcher retarget channel
    pub fn set_watcher_retarget_tx(&mut self, tx: mpsc::Sender<PathBuf>) {
        self.watcher_retarget_tx = Some(tx);
    }

    /// Point the "Overwrite user.toml" drift action at the operator-editable
    /// user file (`config.toml`), distinct from the `live.toml` authority
    /// (`config_path`). Called from `service.rs` when the two paths differ so
    /// the Overwrite action writes the user file rather than the authority
    /// Defaults to `config_path` if never called.
    pub fn set_user_file_path(&mut self, path: PathBuf) {
        self.user_file_path = path;
    }

    /// Point identity persistence at the daemon state dir (where
    /// `active_profile.json` lives). Wired from `service.rs`; `None` (unwired,
    /// e.g. unit tests) means switches update the in-memory identity only.
    pub fn set_active_profile_persist_dir(&mut self, dir: PathBuf) {
        self.active_profile_persist_dir = Some(dir);
    }

    /// Seed the in-memory active-profile identity restored at BOOT from
    /// `active_profile.json` (or the one-time manifest migration). ArcSwap
    /// store ONLY — deliberately no re-persist: the identity just came FROM
    /// disk, and an explicit `--config` boot (which passes no identity) must
    /// stay ephemeral rather than clobber the durable selection.
    pub fn seed_active_profile_identity(&self, info: ActiveProfileInfo) {
        self.active_profile.store(Arc::new(Some(info)));
    }

    /// The single choke point BOTH profile-switch sites
    /// (`execute_profile_switch`, the run_loop `ProfileSwitch` handler) commit
    /// through, so in-memory identity and the durable `active_profile.json`
    /// can never take different code paths. Called ONLY after the switch
    /// succeeded (config reloaded + runtime swapped): identity is written
    /// LAST, so a crash mid-switch leaves the old identity naming the old
    /// (still-loaded-on-boot) content — never a new name over old mappings.
    /// Persistence failure is a WARN, never a switch failure: the file then
    /// keeps the previous, still-true identity (benign boot-time UI lag).
    pub(crate) fn commit_active_profile(&self, info: ActiveProfileInfo) {
        self.active_profile.store(Arc::new(Some(info.clone())));
        if let Some(ref dir) = self.active_profile_persist_dir {
            let record = crate::daemon::active_profile_persist::PersistedActiveProfile {
                version: 1,
                profile_id: info.id,
                name: info.name,
                config_path: PathBuf::from(info.config_path),
            };
            if let Err(e) = crate::daemon::active_profile_persist::persist(dir, &record) {
                warn!(
                    "Failed to persist active-profile identity to {}: {} (switch succeeded; identity will lag on next boot)",
                    dir.display(),
                    e
                );
            }
        }
    }

    /// Re-target the §D9 config watcher to `path` (the new profile file after a
    /// profile switch) AND move the "Overwrite user.toml" write target to the
    /// same path. Single source of truth so `user_file_path` — what
    /// [`armed_profile_write`](EngineManager::handle_overwrite_config_file)
    /// writes and where §D9 self-write suppression applies — never diverges from
    /// the file the watcher watches. Both profile-switch sites
    /// (`execute_profile_switch`, `run_loop`) route their retarget through here.
    ///
    /// Ordering matters: `user_file_path` is moved ONLY once the
    /// watcher retarget is successfully queued. If the retarget send fails (the
    /// watcher task's receiver is gone), the watcher is still watching the
    /// PREVIOUS file, so we leave `user_file_path` there too — keeping Overwrite
    /// and §D9 suppression aligned with the actual watch target rather than
    /// pointing at a file nothing watches. With no watcher wired at all
    /// (`watcher_retarget_tx == None`, e.g. `user_file_policy = ignore`) there is
    /// nothing to diverge from, so `user_file_path` still advances to `path`.
    pub(crate) async fn retarget_watched_user_file(&mut self, path: PathBuf) {
        if let Some(ref retarget_tx) = self.watcher_retarget_tx
            && let Err(e) = retarget_tx.send(path.clone()).await
        {
            warn!("Failed to re-target config watcher: {}", e);
            return;
        }
        self.user_file_path = path;
    }
    /// Initialize and start the app detector for automatic profile switching
    ///
    /// Loads profile-to-app mappings from the profiles manifest and begins
    /// monitoring the frontmost macOS application.
    #[cfg(target_os = "macos")]
    pub async fn start_app_detector(&mut self) {
        use crate::daemon::app_detector::{DaemonAppDetector, load_mappings_from_manifest};

        // Determine profiles directory (same location as GUI uses)
        let profiles_dir = if let Some(config_dir) = dirs::config_dir() {
            config_dir.join("conductor").join("profiles")
        } else {
            warn!("No config directory found, skipping app detector");
            return;
        };

        let mut detector = DaemonAppDetector::new(self.command_tx.clone(), 500);

        // ADR-040 §4.3: enable focused-window-title polling ONLY when
        // `[per_app_modes].window_rules` are present — lazy, so a config without
        // window rules never invokes the macOS Accessibility APIs / requires
        // Accessibility permission (no system dialog is shown; an ungranted
        // permission surfaces via `window_permission_degraded`).
        {
            use crate::daemon::platform::window_title;
            let snap = self.live_config.load();
            if window_title::title_polling_enabled(snap.config.per_app_modes.as_ref()) {
                let poll_ms = snap.config.advanced_settings.window_title_poll_ms;
                detector.enable_title_polling(
                    window_title::default_window_title_source().into(),
                    poll_ms,
                );
            }
        }
        let detector = Arc::new(detector);

        let mappings = {
            let dir = profiles_dir.clone();
            tokio::task::spawn_blocking(move || load_mappings_from_manifest(&dir))
                .await
                .unwrap_or_default()
        };
        if mappings.is_empty() {
            info!("No app-profile mappings found yet; app detector started in idle state");
        } else {
            detector.update_mappings(mappings).await;
        }

        detector.start().await;
        info!("App detector started for automatic profile switching");
        self.app_detector = Some(detector);
    }
    #[cfg(not(target_os = "macos"))]
    pub async fn start_app_detector(&mut self) {
        info!("App detector not supported on this platform, skipping");
    }
    /// Get shared state references for MCP server (ADR-007 Phase 2)
    ///
    /// Returns clones of the internal Arc references that can be shared
    /// with the MCP server for real-time state access.
    pub fn get_shared_state_refs(&self) -> SharedDaemonStateRefs {
        SharedDaemonStateRefs {
            lifecycle_state: Arc::clone(&self.state),
            device_status: Arc::clone(&self.device_status),
            statistics: Arc::clone(&self.statistics),
            input_manager: Arc::clone(&self.input_manager),
            config_path: self.config_path.clone(),
            start_time: self.start_time,
            command_tx: self.command_tx.clone(),
            active_profile: Arc::clone(&self.active_profile),
            event_stats: Arc::clone(&self.event_stats),
            control_state: Arc::clone(&self.control_state),
            probe_coordinator: Arc::clone(&self.probe_coordinator),
            connector_registry: Arc::clone(&self.connector_registry),
            device_output_map: Arc::clone(&self.device_output_map),
            route_engine: Arc::clone(&self.route_engine),
            dispatch_trace: Arc::clone(&self.dispatch_trace),
        }
    }
    /// Get current daemon state snapshot for MCP tools (ADR-007 Phase 2)
    pub async fn get_daemon_state(&self) -> DaemonState {
        let lifecycle_state = *self.state.read().await;
        let device_status = self.device_status.read().await.clone();
        let statistics = self.statistics.read().await.clone();

        // Get input mode from input manager
        // Return None when input_manager is not initialized
        // Don't falsely report "MidiOnly" here.
        // Extract raw data under lock, do JSON conversion after release
        let (input_mode, hid_devices) = {
            let raw_data = {
                let guard = self.input_manager.lock().await;
                guard.as_ref().map(|mgr| {
                    let mode = mgr.mode();
                    let gamepads = mgr.get_connected_gamepads();
                    (mode, gamepads)
                })
            };
            // Lock released — now do allocations/serialization
            match raw_data {
                Some((mode, gamepads)) => {
                    let mode_str = match mode {
                        InputMode::MidiOnly => "MidiOnly".to_string(),
                        InputMode::GamepadOnly => "GamepadOnly".to_string(),
                        InputMode::Both => "Both".to_string(),
                    };
                    let devices = gamepads
                        .into_iter()
                        .map(|(id, name)| json!({"id": id, "name": name, "connected": true}))
                        .collect::<Vec<_>>();
                    (Some(mode_str), devices)
                }
                None => (None, vec![]),
            }
        };

        DaemonState {
            lifecycle_state: Some(lifecycle_state),
            device_status: Some(device_status),
            statistics: Some(statistics),
            input_mode,
            hid_devices,
            uptime_secs: self.start_time.elapsed().as_secs(),
            config_path: self.config_path.to_str().map(|s| s.to_string()),
            active_profile: (**self.active_profile.load()).clone(),
        }
    }
}
