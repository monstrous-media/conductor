// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-040 D4 §4.2 — transient manual-override mode lock (core state machine).
//!
//! When a user manually selects a mode (GUI click, `conductorctl mode set`,
//! `conductor_set_mode{lock:true}`, or a `ModeChange` **action** fired by a
//! mapping — D4/§4.2), the mode is **locked** against auto-switching until they
//! unlock, select a different mode, or the daemon restarts. The lock is **transient** — never persisted: a forgotten lock must
//! not survive a restart and silently disable auto-switching (§4.2 / §7 OQ-b).
//!
//! This is sub-PR 4a of #1767 (the state machine + auto-switch gate). The CLI
//! affordances (`conductorctl mode set/unlock/status`, 4b) and the MCP tools
//! (`conductor_set_mode`/`conductor_unlock_mode`/`conductor_mode_status`, 4c)
//! drive these methods over IPC; the Slice-5 app/window auto-switch producer
//! calls [`EngineManager::apply_auto_switch`] instead of
//! [`persist_mode_change`](EngineManager::persist_mode_change) directly.

use super::*;

/// Where a manual mode lock originated (surfaced in status for supportability).
///
/// `Cli` is wired by 4b; `Mcp` by the MCP tools (4c); `Action` by a pad-fired
/// `ModeChange` action (D4/§4.2 — #2350); `Gui` belongs to the separate GUI ADR
/// (D9, anticipatory until then).
#[allow(dead_code)] // `Gui` is wired by the GUI ADR (D9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LockSource {
    /// GUI mode-tab click.
    Gui,
    /// `conductorctl mode set`.
    Cli,
    /// `conductor_set_mode` MCP tool.
    Mcp,
    /// A `ModeChange` action fired by a mapping (controller pad / chord).
    Action,
}

/// Why a manual [`set_mode_manual`](EngineManager::set_mode_manual) failed.
///
/// Classifying at the source (inside the locked critical section) lets callers
/// map the failure to a precise IPC/MCP error code **without** re-reading mode
/// state afterwards — which would be a TOCTOU race against a concurrent config
/// reload (Council review on #2283).
#[derive(Debug)]
pub(crate) enum SetModeError {
    /// The requested mode is not a declared `[[modes]]` block (a bad request).
    UnknownMode,
    /// An internal fault applying the change (e.g. a persist join error).
    Internal(DaemonError),
}

/// A held manual-override lock (D4 §4.2). Transient — NOT persisted to
/// `state.rs`/config; reconstructed only by an explicit manual selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModeLock {
    /// The pinned mode name.
    pub mode: String,
    /// Who set the lock.
    pub origin: LockSource,
}

impl EngineManager {
    /// The current mode lock, if held. `None` ⇒ auto-switching is active.
    pub(crate) fn mode_lock(&self) -> Option<ModeLock> {
        self.mode_lock.load().as_ref().map(|arc| (**arc).clone())
    }

    /// The active mode + lock state as a JSON object — the single source of
    /// truth for `conductorctl mode status` (4b), the `conductor_mode_status`
    /// MCP tool (4c), and the `QueryModeStatus` command.
    pub(crate) fn mode_status_json(&self) -> serde_json::Value {
        let current = self.current_mode.load();
        let lock = self.mode_lock();
        // Resolution layer (§4.2 D4): while a manual lock is held the active
        // mode is decided by `ManualLock`. The window/app/default layers are
        // only known once the Slice-5 app/window snapshot resolver is wired, so
        // they report `null` until then.
        let resolution_layer = lock.as_ref().map(|_| "ManualLock");
        // §4.3 (Slice 6): `window_permission_degraded` is meaningful only when
        // title polling is active (window_rules present). When inactive — or no
        // detector on this platform — report `null` (titles aren't read), not
        // `false` (which would imply "healthy").
        let window_permission_degraded = self
            .app_detector
            .as_ref()
            .filter(|d| d.title_polling_active())
            .map(|d| serde_json::Value::Bool(d.window_permission_degraded()))
            .unwrap_or(serde_json::Value::Null);
        serde_json::json!({
            "mode": current.name,
            "index": current.index,
            "locked": lock.is_some(),
            "lock_origin": lock.as_ref().map(|l| format!("{:?}", l.origin)),
            "lock_mode": lock.as_ref().map(|l| l.mode.clone()),
            "resolution_layer": resolution_layer,
            "window_permission_degraded": window_permission_degraded,
        })
    }

    /// Whether `mode` names a declared `[[modes]]` block in the live config.
    /// Both lock entry points validate through this because
    /// [`persist_mode_change`](Self::persist_mode_change) accepts unknown modes
    /// (warn-only) for gamepad-chord backwards compatibility.
    fn mode_exists(&self, mode: &str) -> bool {
        self.live_config
            .load()
            .config
            .modes
            .iter()
            .any(|m| m.name == mode)
    }

    /// Manually set the active mode (D4 §4.2).
    ///
    /// Applies the mode change, then: if `lock`, pins the mode against
    /// auto-switching with `origin`; otherwise **clears** any existing lock
    /// (resuming auto-switch from the chosen mode). Selecting a *different* mode
    /// with `lock` replaces the previous lock. Returns `Err` for an unknown mode
    /// — the lock can never point at a mode that doesn't exist.
    pub(crate) async fn set_mode_manual(
        &self,
        mode: String,
        lock: bool,
        origin: LockSource,
    ) -> std::result::Result<(), SetModeError> {
        // Serialise with the other mode-mutators so the validate→persist→store
        // sequence is atomic w.r.t. concurrent callers (no TOCTOU across the
        // `persist_mode_change` await). Classifying the failure here, under the
        // lock, also means callers never re-check mode existence afterwards.
        let _guard = self.mode_mutation_lock.lock().await;
        if !self.mode_exists(&mode) {
            return Err(SetModeError::UnknownMode);
        }
        self.persist_mode_change(mode.clone())
            .await
            .map_err(SetModeError::Internal)?;
        if lock {
            info!("Mode locked to '{mode}' (origin {origin:?})");
            self.mode_lock
                .store(Some(Arc::new(ModeLock { mode, origin })));
        } else {
            self.mode_lock.store(None);
        }
        Ok(())
    }

    /// ADR-040 §4.2 / §4.7 — after a profile switch reloads the config, drop a
    /// manual lock whose mode no longer exists in the new profile. Returns
    /// `true` if a lock was cleared.
    ///
    /// A lock survives a profile switch when the locked mode is still declared;
    /// otherwise it would suppress auto-switching forever against a mode the new
    /// profile doesn't have (§4.2 "preserve if the locked mode survives the new
    /// profile, else clear + re-resolve"). Clearing here lets the Slice-5
    /// `ResolveContextMode` that follows the switch re-resolve from the stack.
    ///
    /// Sync (no `mode_mutation_lock`): the sole caller is the `ProfileSwitch`
    /// arm of the single-consumer command loop, which is the same task that runs
    /// `set_mode_manual`/`apply_auto_switch` — there is no concurrent mutator to
    /// serialise against.
    pub(crate) fn reconcile_lock_after_reload(&self) -> bool {
        if let Some(lock) = self.mode_lock()
            && !self.mode_exists(&lock.mode)
        {
            self.mode_lock.store(None);
            info!(
                "Mode lock '{}' dropped — mode absent in reloaded profile (§4.2)",
                lock.mode
            );
            return true;
        }
        false
    }

    /// Release the manual lock, resuming auto-switching. Returns whether a lock
    /// was held (so callers can report "was already unlocked"). `async` only to
    /// share the mode-mutation guard with the other mutators (linearizability).
    pub(crate) async fn unlock_mode(&self) -> bool {
        let _guard = self.mode_mutation_lock.lock().await;
        let was_locked = self.mode_lock.load().is_some();
        self.mode_lock.store(None);
        if was_locked {
            info!("Mode lock released — auto-switching resumed");
        }
        was_locked
    }

    /// Apply an automatic (app/window) mode switch — **suppressed while a lock is
    /// held** (D4 §4.2). Returns `true` if a switch was applied; `false` if it
    /// was a no-op — suppressed by a lock, an unknown target mode, or the mode is
    /// already active (no persist). The Slice-5 auto-switch producer calls this
    /// rather than
    /// [`persist_mode_change`](Self::persist_mode_change) so the lock is honoured.
    ///
    /// Wired by the Slice-5 app/window auto-switch producer
    /// ([`resolve_context_mode`](Self::resolve_context_mode)).
    pub(crate) async fn apply_auto_switch(&self, mode: String) -> Result<bool> {
        // Serialise with manual set/unlock so the lock check and the switch can't
        // straddle a concurrent lock change (TOCTOU): the guarantee "suppressed
        // while locked" holds only if the check→persist is atomic w.r.t. set/unlock.
        let _guard = self.mode_mutation_lock.lock().await;
        if self.mode_lock.load().is_some() {
            debug!("Auto-switch to '{mode}' suppressed — manual mode lock held");
            return Ok(false);
        }
        // `persist_mode_change` accepts unknown modes (warn-only), so validate
        // here — otherwise an unknown mode would falsely report "applied".
        // Best-effort: an unknown auto-switch target is dropped, not an error.
        if !self.mode_exists(&mode) {
            warn!("Auto-switch to unknown mode '{mode}' ignored");
            return Ok(false);
        }
        // Already on the resolved mode → nothing to switch. Skip the persist
        // (config-file write + LiveConfig mutation in `persist_mode_change`) that
        // would otherwise churn on EVERY app change resolving to the current mode
        // — e.g. an unmapped app falling to the same default — and could trip the
        // §D9 self-write drift watcher (Copilot review #2317). Manual selection
        // (`set_mode_manual`) intentionally still persists; this guard is only on
        // the auto-switch path.
        if self.current_mode.load().name == mode {
            debug!("Auto-switch to '{mode}' skipped — already the active mode");
            return Ok(false);
        }
        self.persist_mode_change(mode).await?;
        Ok(true)
    }

    /// Apply a `ModeChange` **action** (fired by a mapping — a controller pad or
    /// chord). Per ADR-040 D4 / §4.2 a `ModeChange` action is a *manual
    /// selection*, so it pins the mode against auto-switching (origin
    /// [`LockSource::Action`]) exactly like a GUI/CLI/MCP selection — otherwise a
    /// pad-selected mode would be silently overridden by the next app/window
    /// auto-switch (#2350). Selecting a *different* mode replaces the lock
    /// (handled by [`set_mode_manual`](Self::set_mode_manual)).
    ///
    /// Preserves [`persist_mode_change`](Self::persist_mode_change)'s lenient
    /// unknown-mode behaviour: an unknown target is persisted warn-only and does
    /// NOT lock. Config validation already rejects `ModeChange` to an undeclared
    /// mode, so this only guards a stray runtime value (and avoids regressing the
    /// pre-#2350 warn-only path).
    pub(crate) async fn apply_modechange_action(&self, mode: String) -> Result<()> {
        // Hold `mode_mutation_lock` across the whole exists-check → persist →
        // lock-store sequence, exactly like `set_mode_manual`, so it is atomic
        // w.r.t. concurrent mode mutators (no TOCTOU across the persist await) —
        // Council #2351. Inlined rather than calling `set_mode_manual` because the
        // tokio `Mutex` is non-reentrant (calling it under the guard would
        // deadlock) and the unknown-mode fallback must persist warn-only *under
        // the same guard*, not after `set_mode_manual` releases it.
        let _guard = self.mode_mutation_lock.lock().await;
        let known = self.mode_exists(&mode);
        // `persist_mode_change` is warn-only (no state change) for an unknown
        // mode — the pre-#2350 lenient behaviour on that path.
        self.persist_mode_change(mode.clone()).await?;
        if known {
            info!("Mode locked to '{mode}' (origin Action)");
            self.mode_lock.store(Some(Arc::new(ModeLock {
                mode,
                origin: LockSource::Action,
            })));
        }
        Ok(())
    }
}
