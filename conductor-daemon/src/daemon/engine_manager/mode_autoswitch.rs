// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! ADR-040 Slice 5 (#1768) — app/window → mode auto-switch producer.
//!
//! The app detector ([`crate::daemon::app_detector`]) emits a
//! [`DaemonCommand::ResolveContextMode`](crate::daemon::types::DaemonCommand)
//! on every frontmost-app change. The run loop dispatches it here: compile the
//! live config's `[per_app_modes]`, resolve the active mode from the D4
//! precedence stack ([`mode_resolver`](crate::daemon::mode_resolver)), and apply
//! it through the lock-aware [`apply_auto_switch`](EngineManager::apply_auto_switch).
//!
//! Ordering (§4.7): when the same app change also maps to a profile, the detector
//! sends `ProfileSwitch` *before* `ResolveContextMode` on the same channel, so by
//! the time we resolve, the new profile's config is already live and its
//! `[per_app_modes]` are what we compile.

use super::*;
use crate::daemon::mode_resolver::{ContextSnapshot, ResolverRules, resolve_mode};

impl EngineManager {
    /// Resolve and apply the active mode for a frontmost-context snapshot.
    ///
    /// `app` is the frontmost application *name* (the key `[per_app_modes].rules`
    /// and `window_rules` match on — distinct from the bundle id that profiles
    /// key on). `window_title` is the reconciled title (`None` after an app
    /// change invalidated the stale cache, §4.5; populated by Slice 6).
    ///
    /// Best-effort: a missing `[per_app_modes]`, a no-match-no-default resolve, a
    /// lock-suppressed switch, or an unknown target mode all leave the mode
    /// unchanged without surfacing an error — auto-switching must never disrupt
    /// the running engine.
    pub(crate) async fn resolve_context_mode(&self, app: String, window_title: Option<String>) {
        // Snapshot the rules under one `load()` so we compile a coherent config.
        let pam = match self.live_config.load().config.per_app_modes.clone() {
            Some(pam) => pam,
            // No `[per_app_modes]` configured → nothing to auto-switch.
            None => return,
        };
        let rules = match ResolverRules::compile(&pam) {
            Ok(rules) => rules,
            Err(e) => {
                // The config validator already rejects bad regexes at load
                // (Slice 2); this is the defensive boundary.
                warn!("per_app_modes resolve skipped — rule compile failed: {e}");
                return;
            }
        };

        let snapshot = ContextSnapshot {
            app: Some(app),
            window_title,
        };

        // Resolve with `locked = None`: the lock is NOT a resolver input here —
        // `apply_auto_switch` is the single place that honours it (§4.2). Passing
        // the lock to `resolve_mode` would just re-derive the locked mode and
        // then `apply_auto_switch` would suppress it anyway; resolving the
        // *would-be* auto target keeps the two concerns separate and lets the
        // suppression decision live in one place.
        if let Some((mode, layer)) = resolve_mode(None, &snapshot, &rules) {
            match self.apply_auto_switch(mode.clone()).await {
                Ok(true) => debug!("Auto-switched mode to '{mode}' (layer {layer:?})"),
                // Suppressed by a held lock, or the resolved mode isn't declared.
                Ok(false) => {}
                Err(e) => warn!("Auto-switch to '{mode}' failed: {e}"),
            }
        }
    }
}
