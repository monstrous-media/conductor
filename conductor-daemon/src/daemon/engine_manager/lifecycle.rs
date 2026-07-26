// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! `EngineManager` methods extracted from `engine_manager::mod` (refactor #2073).

use super::*;

impl EngineManager {
    /// ADR-025 Phase 3.F (#886): abort the deferred PC-observation check
    /// if one is pending. Called both when a new check is about to be
    /// scheduled (so the old one doesn't fire against a stale expected
    /// set) and at every shutdown entry point (so a late-wake can't
    /// emit a phantom warning while the daemon is tearing down).
    ///
    /// Flips the cancel flag first, then aborts the `JoinHandle`. The
    /// flag closes the race where the sleeper's `.await` has already
    /// resolved and the task is running its synchronous tail — in that
    /// window `abort()` can't interrupt execution, but the task's
    /// cooperative cancellation check sees the flag and bails before
    /// emitting any log.
    ///
    /// Idempotent — safe to call repeatedly.
    pub(crate) fn abort_pending_pc_observation_check(&mut self) {
        use std::sync::atomic::Ordering;
        if let Some(cancel) = self.pending_pc_observation_cancel.take() {
            cancel.store(true, Ordering::Release);
        }
        if let Some(handle) = self.pending_pc_observation_check.take() {
            handle.abort();
        }
    }
    /// ADR-025 Phase 3.F runtime check (#886): after each config-swap,
    /// wait a grace period and then warn about any `(device, channel)`
    /// tuples the config expects but that are still absent from the
    /// control-state store snapshot when the deferred check fires.
    /// Aborts any previously-scheduled check so rapid config changes
    /// don't stack warnings.
    ///
    /// Grace period is hardcoded at 60s — enough time for the user to
    /// plug in hardware and exercise a preset after daemon start/reload.
    /// Making this configurable via `advanced_settings` is a follow-up.
    pub(crate) fn schedule_pc_observation_check(&mut self, context: &str) {
        const GRACE_SECS: u64 = 60;

        // Abort any prior check that hasn't fired yet. A new config-swap
        // has landed; the old task's expected-set may not match the new
        // config, and firing both is pure noise.
        self.abort_pending_pc_observation_check();

        let live_config = Arc::clone(&self.live_config);
        let store = Arc::clone(&self.control_state);
        let ctx = context.to_string();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel_for_task = std::sync::Arc::clone(&cancel);
        let handle = tokio::spawn(async move {
            use std::sync::atomic::Ordering;
            tokio::time::sleep(std::time::Duration::from_secs(GRACE_SECS)).await;
            // Cooperative cancellation check. `JoinHandle::abort()` fires
            // at `.await` points only — once the sleep resolves, the
            // synchronous tail below (config clone + analyzer +
            // `tracing::warn!`) can't be interrupted by abort alone.
            // `abort_pending_pc_observation_check` flips this flag before
            // calling abort, so a shutdown / swap that lands in the
            // post-sleep window still suppresses the log.
            if cancel_for_task.load(Ordering::Acquire) {
                return;
            }
            // D4.A.3.3.A: snapshot the config via `live_config.load()`
            // (lock-free ArcSwap read) and clone out the inner `Config`
            // so the walker/logger doesn't keep the snapshot Arc alive
            // across the analysis — a concurrent config-swap publishes
            // a new snapshot without waiting on us. Read the current
            // config at fire time — if the user reloaded with unrelated
            // mappings in the meantime without triggering our abort
            // path, we still warn against the truly-current expected
            // set.
            let cfg = (*live_config.load().config).clone();
            // Re-check after the second `.await` — the read could have
            // queued briefly behind a concurrent swap that just flipped
            // the cancel flag.
            if cancel_for_task.load(Ordering::Acquire) {
                return;
            }
            control_state_analyzer::log_unobserved_pc_tuples(&cfg, &store, &ctx, GRACE_SECS);
        });
        self.pending_pc_observation_check = Some(handle);
        self.pending_pc_observation_cancel = Some(cancel);
    }
    /// Transition to a new lifecycle state
    pub(crate) async fn transition_state(&self, new_state: LifecycleState) -> Result<()> {
        let mut state = self.state.write().await;
        let old_state = *state;

        if !old_state.can_transition_to(new_state) {
            return Err(DaemonError::InvalidStateTransition {
                from: format!("{}", old_state),
                to: format!("{}", new_state),
            });
        }

        *state = new_state;
        info!("State transition: {} → {}", old_state, new_state);

        Ok(())
    }
    /// Update device status
    pub(crate) async fn update_device_status(
        &self,
        connected: bool,
        name: Option<String>,
        port: Option<usize>,
    ) {
        let mut status = self.device_status.write().await;
        status.connected = connected;
        status.name = name;
        status.port = port;

        if connected {
            status.last_event_at = Some(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            );
        }
    }
    /// Log an error
    pub(crate) async fn log_error(&self, kind: impl Into<String>, message: impl Into<String>) {
        let entry = ErrorEntry::new(kind, message);
        let mut log = self.error_log.write().await;

        log.push(entry);

        // Keep only last 10 errors
        if log.len() > 10 {
            log.remove(0);
        }

        // Update statistics
        let mut stats = self.statistics.write().await;
        stats.errors_since_start += 1;
    }
}
