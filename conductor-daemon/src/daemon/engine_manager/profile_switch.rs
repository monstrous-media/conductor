// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! `EngineManager::execute_profile_switch` + `sync_config_after_apply`,
//! extracted from `engine_manager::reload` (refactor #2073).

use super::*;

impl EngineManager {
    /// Execute a profile switch directly (avoids command_tx deadlock).
    /// Called from handle_ipc_request for conductor_switch_profile, which
    /// runs inside the command_rx select arm and would deadlock if it sent
    /// a DaemonCommand::ProfileSwitch back through command_tx.
    pub(crate) async fn execute_profile_switch(
        &mut self,
        name: &str,
        path: &str,
        profile_id: Option<String>,
    ) -> std::result::Result<String, String> {
        let validated_path = crate::daemon::types::validate_profile_path(path)
            .map_err(|e| format!("Profile path validation failed: {}", e))?;
        let old_config_path = self.config_path.clone();
        let old_state = *self.state.read().await;
        self.config_path = validated_path.clone();

        use crate::daemon::profile_cache::CacheLookup;
        let result = match self.profile_cache.get(&validated_path) {
            CacheLookup::Hit(cached_config) => self
                .reload_from_cached_config(*cached_config)
                .await
                .map_err(|e| e.to_string()),
            CacheLookup::Miss => {
                let r = self.reload_config().await.map_err(|e| e.to_string());
                if r.is_ok() && self.config_path == validated_path {
                    let config = (*self.live_config.load().config).clone();
                    self.profile_cache.insert(&validated_path, config);
                }
                r
            }
        };

        match result {
            Ok(metrics) => {
                // #2564 (Council D4): commit identity via the shared choke
                // point, LAST after the reload succeeded — in-memory ArcSwap +
                // durable active_profile.json together.
                self.commit_active_profile(ActiveProfileInfo {
                    id: profile_id,
                    name: name.to_string(),
                    config_path: self.config_path.display().to_string(),
                });
                info!(
                    "Profile '{}' activated (reload: {}ms)",
                    name, metrics.duration_ms
                );

                // #2553: retarget the watcher AND move the Overwrite/§D9 write
                // target together, so `user_file_path` never diverges from the
                // watched profile file after a switch.
                self.retarget_watched_user_file(self.config_path.clone())
                    .await;

                Ok(format!("Profile '{}' activated successfully", name))
            }
            Err(e) => {
                error!("Profile switch failed: {}", e);
                self.config_path = old_config_path;
                // Restore previous lifecycle state — reload may have left us in Reloading
                if let Err(te) = self.transition_state(old_state).await {
                    warn!(
                        "Failed to restore {:?} state after profile switch failure: {}",
                        old_state, te
                    );
                }
                Err(format!("Profile switch failed: {}", e))
            }
        }
    }
    /// Sync config back after plan apply: save to disk, update state, recompile rule set (#265)
    ///
    /// Reuses compilation logic from `reload_config()` but writes the provided config
    /// to disk instead of reading from disk.
    #[cfg(feature = "llm-executor")]
    pub(crate) async fn sync_config_after_apply(&mut self, new_config: Config) -> Result<()> {
        // ADR-025 Phase 3.F (#886): abort any pending observation check
        // BEFORE any awaited swap work. See reload_config for the race.
        self.abort_pending_pc_observation_check();

        // #2316: PREPARE before ANY write. `prepare_runtime` can fail (bad
        // listener ACL, mapping/route compile error) and returns `Err` via `?`.
        // Doing it first means a failure aborts the apply with NO disk write —
        // avoiding the split-brain where the profile file already holds the new
        // config but the daemon's live config / runtime do not (mirrors
        // `handle_save_config`'s prepare-then-commit; ADR-044 Phase 2). The
        // plan-apply path rebuilds the full runtime (listeners, rate limiter,
        // probe toggle, capture flags, port rescan, device_output_map, device
        // status) — the same rebuild `reload_config` does — via APPLY below.
        let prepared = self.prepare_runtime(&new_config).await?;

        // COMMIT → APPLY → profile write-through — the same ordering as
        // `handle_save_config` (ADR-044 Phase 2 + ADR-034 §D11). PREPARE already
        // succeeded above, so a build failure aborted with no commit and no write.
        //
        // 1. COMMIT the config to `live.toml` (the sole authority). #2554: no
        //    self-write suppression is armed — the mutate writes only `live.toml`,
        //    which the §D9 watcher does NOT watch (post-#2551 it watches the user
        //    file), so there is nothing to suppress and a stale arm would wrongly
        //    drop a genuine external `config.toml` edit. Committing BEFORE any
        //    disk-facing work also means a commit failure can't leave state
        //    diverged (the split-brain edge #2316 is about).
        let live = Arc::clone(&self.live_config);
        let snap = live.load();
        if let Err(e) = live
            .mutate(
                self.default_cli_provenance(),
                snap.state_generation,
                crate::daemon::live_config::ConfigOp::ReplaceWhole {
                    config: Box::new(new_config.clone()),
                },
            )
            .await
        {
            return Err(DaemonError::Ipc(format!("live_config mutate: {e}")));
        }

        // 2. APPLY is infallible (ADR-044) — runs post-commit (Council #2168 R3),
        //    rebuilding the full runtime (listeners, rate limiter, probe toggle,
        //    capture flags, port rescan, device_output_map, device status).
        self.apply_committed_guarded(prepared, "plan-apply").await;

        // ADR-043 Option C (#2554): NO write-back to the profile/user file — the
        // plan-apply commit above persists `live.toml` (the sole durable
        // authority); the GUI reads it via GetConfigBody. (Removed the §D11
        // write-through, which wrote `self.config_path` = live.toml anyway.)

        info!(
            "Config synced after plan apply: {} modes, {} mappings",
            new_config.modes.len(),
            new_config
                .modes
                .iter()
                .map(|m| m.mappings.len())
                .sum::<usize>()
                + new_config.global_mappings.len()
        );

        // (ADR-025 Phase 3.F PC-state re-log + observation check now happens
        // inside `apply_committed_config` via the reconcile above, labelled
        // "plan-apply".)

        Ok(())
    }
}
