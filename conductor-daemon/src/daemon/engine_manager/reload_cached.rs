// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! `EngineManager::reload_from_cached_config`, extracted from
//! `engine_manager::reload`.

use super::*;

impl EngineManager {
    /// Fast-path profile switch using a pre-validated cached config.
    ///
    /// Skips file I/O and schema validation (already done at cache time).
    /// Only does: state transition → compile → atomic swap → state transition.
    /// Target: <50ms total (R770).
    pub(crate) async fn reload_from_cached_config(
        &mut self,
        cached_config: Config,
    ) -> Result<ReloadMetrics> {
        let start = Instant::now();

        // ADR-025 Phase 3.F: abort any pending observation check
        // BEFORE any awaited swap work. See the equivalent comment in
        // reload_config for the race this closes.
        self.abort_pending_pc_observation_check();

        let current_state = *self.state.read().await;

        // Don't silently succeed when already reloading — caller needs to know
        // the switch didn't actually happen.
        if current_state == LifecycleState::Reloading {
            return Err(DaemonError::Ipc(
                "Cannot switch profile: reload already in progress".to_string(),
            ));
        }

        if !current_state.can_transition_to(LifecycleState::Reloading) {
            return Err(DaemonError::InvalidStateTransition {
                from: format!("{}", current_state),
                to: "Reloading".to_string(),
            });
        }
        self.transition_state(LifecycleState::Reloading).await?;

        // Skip file I/O and validation — config is pre-validated from cache.
        let config_load_ms = 0;

        // ADR-044 Phase 2 — atomic PREPARE → COMMIT → APPLY. This cache-hit fast
        // path used to skip the network listeners / SysEx probe toggle / input
        // port rescan / device_output_map / device status vs the full
        // `reload_config`; routing through prepare/apply converges them AND makes
        // it atomic: PREPARE before the commit so a build failure rejects the
        // switch without committing. Restore the pre-reload state on any failure
        // (the sole caller `execute_profile_switch` also restores on Err; this
        // is the defensive fallback so we never leave the daemon stuck in
        // Reloading; force-set as a last resort).
        let swap_start = Instant::now();

        let prepared = match self.prepare_runtime(&cached_config).await {
            Ok(p) => p,
            Err(e) => {
                // Nothing committed yet — restore the pre-reload state and bail.
                if let Err(te) = self.transition_state(current_state).await {
                    warn!(
                        "Failed to restore {:?} after cached-reload PREPARE failure, forcing: {}",
                        current_state, te
                    );
                    *self.state.write().await = current_state;
                }
                return Err(e);
            }
        };

        // Extract the metric counts BEFORE moving `cached_config` into the
        // op, so the cache-hit fast path doesn't clone the whole Config just to
        // read `.modes`/`.global_mappings` for metrics afterward.
        let modes_loaded = cached_config.modes.len();
        let total_mappings: usize = cached_config
            .modes
            .iter()
            .map(|m| m.mappings.len())
            .sum::<usize>()
            + cached_config.global_mappings.len();

        {
            let live = Arc::clone(&self.live_config);
            let snap = live.load();
            if let Err(e) = live
                .mutate(
                    self.default_cli_provenance(),
                    snap.state_generation,
                    crate::daemon::live_config::ConfigOp::ReplaceWhole {
                        config: Box::new(cached_config),
                    },
                )
                .await
            {
                // Commit failed (e.g. a CAS / stale-generation conflict) —
                // nothing was published, so restore the pre-reload state and
                // bail, mirroring the PREPARE-failure path above. Unlike
                // `reload_config`, this fast path has no error-restoring wrapper,
                // so it must restore locally rather than lean on the caller
                // (don't rely on a fragile caller contract).
                if let Err(te) = self.transition_state(current_state).await {
                    warn!(
                        "Failed to restore {:?} after cached-reload COMMIT failure, forcing: {}",
                        current_state, te
                    );
                    *self.state.write().await = current_state;
                }
                return Err(DaemonError::Ipc(format!("live_config mutate: {e}")));
            }
        }

        // APPLY is infallible (ADR-044) — it runs post-commit, so it never
        // reverts the lifecycle state away from the now-committed config
        // The pre-commit PREPARE failure above is the only
        // path that restores `current_state`.
        let mapping_compile_ms = self
            .apply_committed_guarded(prepared, "profile-switch")
            .await
            .mapping_compile_ms;

        let swap_ms = swap_start.elapsed().as_millis() as u64;

        let duration_ms = start.elapsed().as_millis() as u64;

        let metrics = ReloadMetrics {
            duration_ms,
            modes_loaded,
            mappings_loaded: total_mappings,
            config_load_ms,
            mapping_compile_ms,
            swap_ms,
        };

        {
            let mut stats = self.statistics.write().await;
            stats.update_reload_metrics(&metrics);
        }

        info!(
            "Profile switch (cached) in {}ms: {} modes, {} mappings [compile: {}ms, swap: {}ms]",
            duration_ms, metrics.modes_loaded, metrics.mappings_loaded, mapping_compile_ms, swap_ms
        );

        // (ADR-025 Phase 3.F PC-state re-log + observation check now happens
        // inside `apply_committed_config` via the reconcile above, labelled
        // "profile-switch".)

        // Transition back to Running. Config is already swapped so we can't rollback;
        // if this fails, force-set to avoid leaving daemon stuck in Reloading.
        if let Err(e) = self.transition_state(LifecycleState::Running).await {
            warn!(
                "Failed to transition to Running after cached reload, forcing: {}",
                e
            );
            *self.state.write().await = LifecycleState::Running;
        }
        Ok(metrics)
    }
}
