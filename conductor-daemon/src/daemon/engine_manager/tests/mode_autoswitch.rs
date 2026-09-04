// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-040 — app/window → mode auto-switch + lock reconciliation.
//!
//! Exercises the two `EngineManager` halves of the mode auto-switch path:
//! [`resolve_context_mode`] (compile `[per_app_modes]` → resolve → lock-aware
//! apply) and [`reconcile_lock_after_reload`] (§4.2 lock-stomp on profile
//! switch). The app-detector command-emission half (`emit_context_switch`,
//! including the §4.5 stale-title invalidation) is covered by inline tests in
//! `app_detector.rs`, which need no FFI.
//!
//! Each test constructs an `EngineManager`, whose `new()` builds an
//! `ActionExecutor` (→ `Enigo::new()`), so all are `#[cfg_attr(linux, ignore)]`.

use super::*;
use std::io::Write;
use std::path::Path;
use tempfile::NamedTempFile;

/// Write `body` to a real temp file (`persist_mode_change`/`reload_config` both
/// touch the path on disk) and return the guard + its path.
fn config_file(body: &str) -> (NamedTempFile, PathBuf) {
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(body.as_bytes()).unwrap();
    let path = f.path().to_path_buf();
    (f, path)
}

async fn manager_at(path: &Path) -> EngineManager {
    let config = Config::load(&path.to_string_lossy()).unwrap();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);
    EngineManager::new(config, path.to_path_buf(), cmd_rx, cmd_tx, shutdown_tx)
        .expect("EngineManager::new")
}

fn active_mode(mgr: &EngineManager) -> String {
    mgr.current_mode.load().name.clone()
}

/// Walk the startup lifecycle transitions `run()` performs, so `reload_config`
/// (which needs a Running lifecycle — `Init → Reloading` is invalid) works in a
/// unit test that never spins the loop. Mirrors `mutation.rs`.
async fn start_lifecycle(mgr: &EngineManager) {
    mgr.transition_state(crate::daemon::types::LifecycleState::Starting)
        .await
        .unwrap();
    mgr.transition_state(crate::daemon::types::LifecycleState::Running)
        .await
        .unwrap();
}

/// Two modes + a `[per_app_modes]` mapping "Logic Pro" → Edit, default Mix.
const PER_APP_MODES_CONFIG: &str = r#"
[[modes]]
name = "Mix"

[[modes]]
name = "Edit"

[per_app_modes]
default = "Mix"

[per_app_modes.rules]
"Logic Pro" = "Edit"
"#;

// ── resolve_context_mode (§4.5/§4.7 producer) ──────────────────────────────

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new → Enigo::new()
async fn app_rule_switches_mode() {
    // Acceptance: app→mode switch fires.
    let (_f, p) = config_file(PER_APP_MODES_CONFIG);
    let mgr = manager_at(&p).await;
    mgr.resolve_context_mode("Logic Pro".into(), None).await;
    assert_eq!(active_mode(&mgr), "Edit", "the app rule resolved + applied");
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn unmatched_app_falls_to_default() {
    // An app with no rule resolves to `[per_app_modes].default`.
    let (_f, p) = config_file(PER_APP_MODES_CONFIG);
    let mgr = manager_at(&p).await;
    // Move off the default first so the switch-back is observable.
    mgr.resolve_context_mode("Logic Pro".into(), None).await;
    assert_eq!(active_mode(&mgr), "Edit");
    mgr.resolve_context_mode("Safari".into(), None).await;
    assert_eq!(active_mode(&mgr), "Mix", "no rule → default mode");
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn auto_switch_suppressed_while_locked() {
    // Acceptance: locked → no switch. apply_auto_switch is the single gate.
    let (_f, p) = config_file(PER_APP_MODES_CONFIG);
    let mgr = manager_at(&p).await;
    mgr.set_mode_manual("Mix".into(), true, LockSource::Cli)
        .await
        .unwrap();
    // "Logic Pro" would resolve to Edit, but the lock pins Mix.
    mgr.resolve_context_mode("Logic Pro".into(), None).await;
    assert_eq!(active_mode(&mgr), "Mix", "lock suppresses the auto-switch");
    assert!(mgr.mode_lock().is_some(), "lock still held");
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn no_per_app_modes_is_a_noop() {
    // A config without `[per_app_modes]` resolves nothing — mode unchanged.
    let (_f, p) = config_file("[[modes]]\nname = \"Mix\"\n\n[[modes]]\nname = \"Edit\"\n");
    let mgr = manager_at(&p).await;
    let before = active_mode(&mgr);
    mgr.resolve_context_mode("Logic Pro".into(), None).await;
    assert_eq!(active_mode(&mgr), before, "no [per_app_modes] → no switch");
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn auto_switch_to_already_active_mode_is_a_noop() {
    // Resolving to the CURRENT mode (e.g. an unmapped app
    // falling to the same default on every app change) must not persist — that
    // would write the config file + mutate LiveConfig on every focus change.
    // `apply_auto_switch` reports `false` (no switch) for the already-active mode.
    let (_f, p) = config_file(PER_APP_MODES_CONFIG);
    let mgr = manager_at(&p).await;
    // Switch to Edit first (a real switch → true).
    assert!(mgr.apply_auto_switch("Edit".into()).await.unwrap());
    assert_eq!(active_mode(&mgr), "Edit");
    // Re-applying the active mode is a no-op (no persist).
    assert!(
        !mgr.apply_auto_switch("Edit".into()).await.unwrap(),
        "already-active mode reports not-applied (skips persist)"
    );
    assert_eq!(active_mode(&mgr), "Edit");
}

// ── reconcile_lock_after_reload (§4.2 lock-stomp on profile switch) ─────────

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn lock_survives_reload_when_mode_present() {
    // §4.2: a lock whose mode still exists in the reloaded profile is kept.
    let (_f, p) = config_file(PER_APP_MODES_CONFIG);
    let mut mgr = manager_at(&p).await;
    start_lifecycle(&mgr).await;
    mgr.set_mode_manual("Edit".into(), true, LockSource::Cli)
        .await
        .unwrap();
    // Reload a config that STILL declares "Edit".
    std::fs::write(
        &p,
        "[[modes]]\nname = \"Mix\"\n\n[[modes]]\nname = \"Edit\"\n",
    )
    .unwrap();
    mgr.reload_config().await.unwrap();

    assert!(
        !mgr.reconcile_lock_after_reload(),
        "lock mode still present"
    );
    let lock = mgr.mode_lock().expect("lock retained");
    assert_eq!(lock.mode, "Edit");
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn lock_dropped_when_mode_absent_in_new_profile() {
    // Acceptance: lock dropped when the locked mode is absent in the new profile.
    let (_f, p) = config_file(PER_APP_MODES_CONFIG);
    let mut mgr = manager_at(&p).await;
    start_lifecycle(&mgr).await;
    mgr.set_mode_manual("Edit".into(), true, LockSource::Cli)
        .await
        .unwrap();
    // Reload a profile WITHOUT "Edit" — the lock now points at a vanished mode.
    // It carries its OWN `[per_app_modes]` (default "Solo") so the re-resolve
    // below actually exercises the auto-switch path rather than no-opping.
    std::fs::write(
        &p,
        "[[modes]]\nname = \"Mix\"\n\n[[modes]]\nname = \"Solo\"\n\n\
         [per_app_modes]\ndefault = \"Solo\"\n",
    )
    .unwrap();
    mgr.reload_config().await.unwrap();

    assert!(
        mgr.reconcile_lock_after_reload(),
        "absent mode → lock cleared"
    );
    assert!(mgr.mode_lock().is_none(), "lock released");

    // …and with the lock gone, auto-switch resumes within the new profile: an
    // unmatched app resolves to the new default "Solo" and is applied (the old
    // locked "Edit" is gone, so this is a real switch, not a coincidental hold).
    mgr.resolve_context_mode("Safari".into(), None).await;
    assert_eq!(
        active_mode(&mgr),
        "Solo",
        "re-resolves to the new profile's default after the lock dropped"
    );
}
