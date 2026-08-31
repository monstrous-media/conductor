// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-040 Slice 4a (#1767) — transient mode-lock state machine.
//!
//! Drives the `EngineManager` lock methods directly: manual set (with/without
//! lock), unlock, re-lock on a different mode, the auto-switch suppression gate,
//! and "no lock restored at startup". The CLI (4b) and MCP (4c) affordances are
//! thin IPC wrappers over these and land in their own sub-PRs.
//!
//! Each test constructs an `EngineManager`, whose `new()` builds an
//! `ActionExecutor` (→ `Enigo::new()`), so all are `#[cfg_attr(linux, ignore)]`.

use super::*;
use std::io::Write;
use std::path::Path;
use tempfile::NamedTempFile;

/// A two-mode config written to a real temp file (persist_mode_change writes
/// back to the path, so it must exist on disk). Returns the guard (keep it
/// alive for the test) and its path.
fn two_mode_config() -> (NamedTempFile, PathBuf) {
    let mut f = NamedTempFile::new().unwrap();
    writeln!(
        f,
        r#"
[[modes]]
name = "Mix"

[[modes]]
name = "Edit"
"#
    )
    .unwrap();
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

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new → Enigo::new()
async fn no_lock_at_startup() {
    let (_f, p) = two_mode_config();
    let mgr = manager_at(&p).await;
    // §4.2: locks are transient — a fresh daemon holds no lock, regardless of
    // any persisted mode.
    assert!(mgr.mode_lock().is_none());
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn set_mode_with_lock_pins_and_records_origin() {
    let (_f, p) = two_mode_config();
    let mgr = manager_at(&p).await;
    mgr.set_mode_manual("Edit".into(), true, LockSource::Cli)
        .await
        .unwrap();
    let lock = mgr.mode_lock().expect("locked");
    assert_eq!(lock.mode, "Edit");
    assert_eq!(lock.origin, LockSource::Cli);
    assert_eq!(active_mode(&mgr), "Edit");
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn set_mode_without_lock_clears_existing_lock() {
    let (_f, p) = two_mode_config();
    let mgr = manager_at(&p).await;
    mgr.set_mode_manual("Edit".into(), true, LockSource::Cli)
        .await
        .unwrap();
    assert!(mgr.mode_lock().is_some());
    // `--no-lock` switches mode and clears the lock (resume auto-switch).
    mgr.set_mode_manual("Mix".into(), false, LockSource::Cli)
        .await
        .unwrap();
    assert!(mgr.mode_lock().is_none());
    assert_eq!(active_mode(&mgr), "Mix");
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn selecting_a_different_mode_relocks() {
    let (_f, p) = two_mode_config();
    let mgr = manager_at(&p).await;
    mgr.set_mode_manual("Edit".into(), true, LockSource::Cli)
        .await
        .unwrap();
    mgr.set_mode_manual("Mix".into(), true, LockSource::Mcp)
        .await
        .unwrap();
    let lock = mgr.mode_lock().expect("re-locked");
    assert_eq!(lock.mode, "Mix");
    assert_eq!(lock.origin, LockSource::Mcp);
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn unlock_releases_and_is_idempotent() {
    let (_f, p) = two_mode_config();
    let mgr = manager_at(&p).await;
    mgr.set_mode_manual("Edit".into(), true, LockSource::Gui)
        .await
        .unwrap();
    assert!(
        mgr.unlock_mode().await,
        "first unlock reports a lock was held"
    );
    assert!(mgr.mode_lock().is_none());
    assert!(
        !mgr.unlock_mode().await,
        "second unlock reports nothing was held"
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn auto_switch_suppressed_while_locked() {
    let (_f, p) = two_mode_config();
    let mgr = manager_at(&p).await;
    mgr.set_mode_manual("Edit".into(), true, LockSource::Cli)
        .await
        .unwrap();
    let applied = mgr.apply_auto_switch("Mix".into()).await.unwrap();
    assert!(!applied, "auto-switch is suppressed while locked");
    assert_eq!(
        active_mode(&mgr),
        "Edit",
        "mode unchanged by suppressed switch"
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn auto_switch_applies_when_unlocked() {
    let (_f, p) = two_mode_config();
    let mgr = manager_at(&p).await;
    let applied = mgr.apply_auto_switch("Edit".into()).await.unwrap();
    assert!(applied, "auto-switch applies when no lock is held");
    assert_eq!(active_mode(&mgr), "Edit");
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn auto_switch_to_unknown_mode_is_not_applied() {
    // Copilot review on #2278: persist_mode_change returns Ok(()) for unknown
    // modes (warn-only), so apply_auto_switch must validate to avoid falsely
    // reporting "applied".
    let (_f, p) = two_mode_config();
    let mgr = manager_at(&p).await;
    let before = active_mode(&mgr);
    let applied = mgr.apply_auto_switch("Ghost".into()).await.unwrap();
    assert!(
        !applied,
        "auto-switch to an unknown mode reports not-applied"
    );
    assert_eq!(active_mode(&mgr), before, "mode unchanged");
}

// ── ADR-040 D4/§4.2 (#2350): ModeChange ACTION locks the mode ──────────

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn modechange_action_locks_the_mode() {
    // A pad-fired `ModeChange` is a manual selection → it pins the mode with
    // origin `Action` (was: bypassed the lock, so auto-switch could override it).
    let (_f, p) = two_mode_config();
    let mgr = manager_at(&p).await;
    mgr.apply_modechange_action("Edit".into()).await.unwrap();
    assert_eq!(active_mode(&mgr), "Edit");
    let lock = mgr.mode_lock().expect("ModeChange action must lock");
    assert_eq!(lock.mode, "Edit");
    assert_eq!(lock.origin, LockSource::Action);
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn modechange_action_lock_suppresses_auto_switch() {
    // The whole point: after a pad selects a mode, an app/window auto-switch
    // must NOT silently override it.
    let (_f, p) = two_mode_config();
    let mgr = manager_at(&p).await;
    mgr.apply_modechange_action("Edit".into()).await.unwrap();
    let applied = mgr.apply_auto_switch("Mix".into()).await.unwrap();
    assert!(
        !applied,
        "auto-switch suppressed after a ModeChange-action lock"
    );
    assert_eq!(active_mode(&mgr), "Edit", "pad-selected mode holds");
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn modechange_action_relocks_on_a_different_mode() {
    // Selecting a different mode replaces the lock (§4.2).
    let (_f, p) = two_mode_config();
    let mgr = manager_at(&p).await;
    mgr.apply_modechange_action("Edit".into()).await.unwrap();
    mgr.apply_modechange_action("Mix".into()).await.unwrap();
    let lock = mgr.mode_lock().expect("re-locked");
    assert_eq!(lock.mode, "Mix");
    assert_eq!(lock.origin, LockSource::Action);
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn modechange_action_to_unknown_mode_is_lenient_and_does_not_lock() {
    // Unknown target: persisted warn-only (legacy leniency for the action path),
    // and must NOT acquire a lock pointing at a nonexistent mode.
    let (_f, p) = two_mode_config();
    let mgr = manager_at(&p).await;
    let before = active_mode(&mgr);
    mgr.apply_modechange_action("Ghost".into()).await.unwrap();
    assert!(mgr.mode_lock().is_none(), "unknown mode must not lock");
    assert_eq!(
        active_mode(&mgr),
        before,
        "unknown mode leaves mode unchanged"
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn set_unknown_mode_errors_and_sets_no_lock() {
    let (_f, p) = two_mode_config();
    let mgr = manager_at(&p).await;
    let r = mgr
        .set_mode_manual("Ghost".into(), true, LockSource::Cli)
        .await;
    assert!(r.is_err(), "unknown mode is rejected");
    assert!(mgr.mode_lock().is_none(), "no lock set for an unknown mode");
}

// ── Slice 4b: IPC handlers (conductorctl mode set/unlock/status) ────

use crate::daemon::types::{IpcCommand, IpcRequest, ResponseStatus};

fn ipc_req(command: IpcCommand, args: serde_json::Value) -> IpcRequest {
    IpcRequest {
        id: "test".into(),
        command,
        args,
    }
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn ipc_set_mode_locks_and_status_reports_origin() {
    let (_f, p) = two_mode_config();
    let mut mgr = manager_at(&p).await;
    let resp = mgr
        .handle_set_mode(
            &ipc_req(IpcCommand::SetMode, json!({ "mode": "Edit", "lock": true })),
            "test".into(),
        )
        .await;
    assert!(matches!(resp.status, ResponseStatus::Success));

    let status = mgr
        .handle_mode_status(
            &ipc_req(IpcCommand::ModeStatus, serde_json::Value::Null),
            "test".into(),
        )
        .await;
    let data = status.data.expect("status data");
    assert_eq!(data.get("mode").and_then(|v| v.as_str()), Some("Edit"));
    assert_eq!(data.get("locked").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(
        data.get("lock_origin").and_then(|v| v.as_str()),
        Some("Cli")
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn ipc_set_mode_no_lock_leaves_unlocked() {
    let (_f, p) = two_mode_config();
    let mut mgr = manager_at(&p).await;
    let resp = mgr
        .handle_set_mode(
            &ipc_req(
                IpcCommand::SetMode,
                json!({ "mode": "Edit", "lock": false }),
            ),
            "test".into(),
        )
        .await;
    assert!(matches!(resp.status, ResponseStatus::Success));
    assert!(
        mgr.mode_lock().is_none(),
        "lock:false leaves auto-switch active"
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn ipc_unlock_releases_lock() {
    let (_f, p) = two_mode_config();
    let mut mgr = manager_at(&p).await;
    mgr.handle_set_mode(
        &ipc_req(IpcCommand::SetMode, json!({ "mode": "Edit", "lock": true })),
        "test".into(),
    )
    .await;
    let resp = mgr
        .handle_unlock_mode(
            &ipc_req(IpcCommand::UnlockMode, serde_json::Value::Null),
            "test".into(),
        )
        .await;
    assert!(matches!(resp.status, ResponseStatus::Success));
    assert_eq!(
        resp.data
            .and_then(|d| d.get("unlocked").and_then(|v| v.as_bool())),
        Some(true)
    );
    assert!(mgr.mode_lock().is_none());
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn ipc_set_unknown_mode_returns_error() {
    let (_f, p) = two_mode_config();
    let mut mgr = manager_at(&p).await;
    let resp = mgr
        .handle_set_mode(
            &ipc_req(
                IpcCommand::SetMode,
                json!({ "mode": "Ghost", "lock": true }),
            ),
            "test".into(),
        )
        .await;
    assert!(matches!(resp.status, ResponseStatus::Error));
    // An unknown mode is a bad request, not an internal fault (Copilot review #2283).
    assert_eq!(
        resp.error.as_ref().map(|e| e.code),
        Some(crate::daemon::error::IpcErrorCode::InvalidRequest.as_u16())
    );
    assert!(mgr.mode_lock().is_none());
}
