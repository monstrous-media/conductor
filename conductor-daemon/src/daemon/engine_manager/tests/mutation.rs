// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use super::*;

// ── ADR-034 §D2 / D4.C IPC mutation surface ──────────────────
//
// Free-function tests (no `EngineManager`, so they run on Linux too):
// provenance mapping, mutate-error → IPC-code mapping, path lexical checks.

#[test]
fn provenance_for_maps_trust_band_to_initiator() {
    use conductor_core::config::{Initiator, Source};
    let gui = provenance_for(
        &Some(crate::security::CallerContext::new(
            crate::security::TrustLevel::GuiTrusted,
        )),
        Source::InMemoryEdit,
    );
    assert!(matches!(gui.initiator, Initiator::Gui));

    let cli = provenance_for(
        &Some(crate::security::CallerContext::new(
            crate::security::TrustLevel::CliTrusted,
        )),
        Source::InMemoryEdit,
    );
    assert!(matches!(cli.initiator, Initiator::Cli));

    // Untrusted / unpinned default to Cli (raw-IPC fallback).
    let untrusted = provenance_for(
        &Some(crate::security::CallerContext::new(
            crate::security::TrustLevel::Untrusted,
        )),
        Source::InMemoryEdit,
    );
    assert!(matches!(untrusted.initiator, Initiator::Cli));
    assert!(matches!(
        provenance_for(&None, Source::InMemoryEdit).initiator,
        Initiator::Cli
    ));
}

#[test]
fn provenance_for_threads_source_unchanged() {
    use conductor_core::config::{ConfigRevision, Source};
    let rev = ConfigRevision::from_bytes([7u8; 32]);
    let prov = provenance_for(
        &None,
        Source::DiskImport {
            path: PathBuf::from("/x/user.toml"),
            revision: rev,
        },
    );
    match prov.source {
        // ReloadFromDisk / ImportConfig MUST record DiskImport (§D6) —
        // this is the provenance the audit outbox persists.
        Source::DiskImport { path, revision } => {
            assert_eq!(path, PathBuf::from("/x/user.toml"));
            assert_eq!(revision, rev);
        }
        other => panic!("expected DiskImport, got {other:?}"),
    }
}

#[test]
fn mutate_error_maps_stale_base_to_5002() {
    use crate::daemon::live_config::MutateError;
    let (code, _msg) = mutate_error_to_ipc(&MutateError::StaleBaseGeneration {
        current: 5,
        supplied: 3,
    });
    // §D2.1: the dedicated StaleBaseGeneration code, not a generic 1004.
    assert_eq!(code, IpcErrorCode::StaleBaseGeneration.as_u16());
    assert_eq!(code, 5002);

    let (vcode, _) = mutate_error_to_ipc(&MutateError::InvalidOp("bad".into()));
    assert_eq!(vcode, IpcErrorCode::ConfigValidationFailed.as_u16());

    // §D8.2: outbox-full refusal surfaces as AuditUnavailable (5004), not a
    // generic internal error, so the caller can distinguish + retry after drain.
    let (acode, _) = mutate_error_to_ipc(&MutateError::AuditUnavailable("full".into()));
    assert_eq!(acode, IpcErrorCode::AuditUnavailable.as_u16());
    assert_eq!(acode, 5004);
}

#[test]
fn lexical_config_path_rejects_traversal_relative_and_non_toml() {
    // Accepts a plain absolute .toml.
    assert!(lexical_config_path_ok(std::path::Path::new("/etc/conductor/user.toml")).is_ok());
    // Rejects `..`.
    assert!(lexical_config_path_ok(std::path::Path::new("/etc/../user.toml")).is_err());
    // Rejects relative.
    assert!(lexical_config_path_ok(std::path::Path::new("user.toml")).is_err());
    // Rejects non-.toml.
    assert!(lexical_config_path_ok(std::path::Path::new("/etc/conductor/user.json")).is_err());
}

// ── Handler tests (need `EngineManager::new` → Enigo, ignored on Linux) ──

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new (Enigo) needs a display server
async fn save_config_applies_and_advances_generation() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _r) = tokio::sync::broadcast::channel(1);
    let mut mgr = EngineManager::new(
        create_test_config(),
        tmp.path().to_path_buf(),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("engine builds");

    let base = mgr.live_config.load().state_generation;
    let mut new_cfg = Config::default_config();
    new_cfg.last_selected_mode = Some("saved".to_string());

    let req = IpcRequest {
        id: "s1".to_string(),
        command: IpcCommand::SaveConfig,
        args: json!({ "base_generation": base, "config": new_cfg }),
    };
    let resp = mgr.handle_ipc_request(req, None).await;
    assert!(
        matches!(resp.status, ResponseStatus::Success),
        "resp: {resp:?}"
    );

    let snap = mgr.live_config.load();
    assert_eq!(snap.state_generation, base + 1);
    assert_eq!(snap.config.last_selected_mode.as_deref(), Some("saved"));
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new (Enigo) needs a display server
async fn save_config_does_not_write_the_user_file() {
    // ADR-043 Option C: SaveConfig writes ONLY `live.toml` (the sole
    // durable authority) — it must NOT write back to the user/profile file
    // (`self.config_path`). The guarantee (a GUI ENDPOINTS Delete stays
    // deleted) now holds via `live.toml`, which the daemon resumes on the next
    // boot and the GUI reads via GetConfigBody — not via a
    // profile write-back that used to diverge the two files.
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _r) = tokio::sync::broadcast::channel(1);
    let mut mgr = EngineManager::new(
        create_test_config(),
        path.clone(),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("engine builds");

    let base = mgr.live_config.load().state_generation;
    // Seed the user file with a sentinel so "unchanged" is a
    // strong assertion — if SaveConfig wrote the config back, the sentinel would be
    // gone (rather than relying on the file merely being empty).
    let user_file_sentinel = "# untouched-by-saveconfig-option-c\n";
    std::fs::write(&path, user_file_sentinel).unwrap();
    let mut new_cfg = Config::default_config();
    new_cfg.last_selected_mode = Some("written_through".to_string());

    let req = IpcRequest {
        id: "wt1".to_string(),
        command: IpcCommand::SaveConfig,
        args: json!({ "base_generation": base, "config": new_cfg }),
    };
    let resp = mgr.handle_ipc_request(req, None).await;
    assert!(
        matches!(resp.status, ResponseStatus::Success),
        "resp: {resp:?}"
    );

    // The live snapshot (the authority) reflects the committed change...
    assert_eq!(
        mgr.live_config.load().config.last_selected_mode.as_deref(),
        Some("written_through"),
        "the committed config must be the live authority after SaveConfig"
    );
    // ...but the user/profile file on disk is UNTOUCHED — no write-back (Option C).
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        user_file_sentinel,
        "SaveConfig must NOT write the user/profile file (Option C removes the §D11 write-through)"
    );
    // No stale self-write-suppress arm left behind — the mutate
    // writes only live.toml (unwatched), so arming would wrongly drop a
    // genuine external config.toml edit within its window.
    assert!(
        mgr.config_write_suppress.lock().await.is_none(),
        "SaveConfig must not arm config_write_suppress (the live.toml write is unwatched)"
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new (Enigo) needs a display server
async fn save_config_rejects_stale_base_generation() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _r) = tokio::sync::broadcast::channel(1);
    let mut mgr = EngineManager::new(
        create_test_config(),
        tmp.path().to_path_buf(),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("engine builds");
    // The boot publishes the first snapshot at gen 1 (new_published), so
    // capture it rather than hard-coding 0 — a rejected save must leave it here.
    let boot_gen = mgr.live_config.load().state_generation;

    let req = IpcRequest {
        id: "s2".to_string(),
        command: IpcCommand::SaveConfig,
        args: json!({ "base_generation": 999u64, "config": Config::default_config() }),
    };
    let resp = mgr.handle_ipc_request(req, None).await;
    assert!(matches!(resp.status, ResponseStatus::Error));
    assert_eq!(
        resp.error.unwrap().code,
        IpcErrorCode::StaleBaseGeneration.as_u16()
    );
    // Snapshot did NOT advance.
    assert_eq!(mgr.live_config.load().state_generation, boot_gen);
    // ADR-043 Option C: the user/profile file is never written by
    // SaveConfig at all now, so a rejected CAS trivially leaves it untouched.
    assert_eq!(
        std::fs::read_to_string(tmp.path()).unwrap(),
        "",
        "SaveConfig (rejected or not) must not write the user/profile file"
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new (Enigo) needs a display server
async fn save_config_requires_base_generation() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _r) = tokio::sync::broadcast::channel(1);
    let mut mgr = EngineManager::new(
        create_test_config(),
        tmp.path().to_path_buf(),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("engine builds");

    let req = IpcRequest {
        id: "s3".to_string(),
        command: IpcCommand::SaveConfig,
        args: json!({ "config": Config::default_config() }),
    };
    let resp = mgr.handle_ipc_request(req, None).await;
    assert_eq!(
        resp.error.unwrap().code,
        IpcErrorCode::MissingField.as_u16()
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new (Enigo) needs a display server
async fn reload_from_disk_picks_up_edited_file() {
    // Write a DIFFERENT config to the daemon's config_path, then
    // ReloadFromDisk (no path) must republish it through the mutate seam.
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    let mut on_disk = Config::default_config();
    on_disk.last_selected_mode = Some("from_disk".to_string());
    let bytes = conductor_core::config::canonical::serialise(&on_disk).unwrap();
    std::fs::write(&path, &bytes).unwrap();

    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _r) = tokio::sync::broadcast::channel(1);
    let mut mgr = EngineManager::new(
        create_test_config(),
        path.clone(),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("engine builds");

    let base = mgr.live_config.load().state_generation;
    let req = IpcRequest {
        id: "r1".to_string(),
        command: IpcCommand::ReloadFromDisk,
        args: json!({ "base_generation": base }),
    };
    // From a "GUI" peer → Initiator::Gui in the recorded provenance.
    let resp = mgr
        .handle_ipc_request(
            req,
            Some(crate::security::CallerContext::internal_trusted()),
        )
        .await;
    assert!(
        matches!(resp.status, ResponseStatus::Success),
        "resp: {resp:?}"
    );
    assert_eq!(
        mgr.live_config.load().config.last_selected_mode.as_deref(),
        Some("from_disk")
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new (Enigo) needs a display server
async fn import_config_requires_path() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _r) = tokio::sync::broadcast::channel(1);
    let mut mgr = EngineManager::new(
        create_test_config(),
        tmp.path().to_path_buf(),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("engine builds");

    let req = IpcRequest {
        id: "i1".to_string(),
        command: IpcCommand::ImportConfig,
        args: json!({ "base_generation": 0u64 }),
    };
    let resp = mgr.handle_ipc_request(req, None).await;
    assert_eq!(
        resp.error.unwrap().code,
        IpcErrorCode::MissingField.as_u16()
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new (Enigo) needs a display server
async fn config_drift_status_detects_disk_edit() {
    // config_path starts byte-identical to the live snapshot → no drift.
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    let cfg = Config::default_config();
    std::fs::write(
        &path,
        conductor_core::config::canonical::serialise(&cfg).unwrap(),
    )
    .unwrap();

    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _r) = tokio::sync::broadcast::channel(1);
    let mut mgr =
        EngineManager::new(cfg, path.clone(), cmd_rx, cmd_tx, shutdown_tx).expect("builds");

    let req = || IpcRequest {
        id: "d1".to_string(),
        command: IpcCommand::ConfigDriftStatus,
        args: json!({}),
    };
    let resp = mgr.handle_ipc_request(req(), None).await;
    assert!(matches!(resp.status, ResponseStatus::Success));
    let data = resp.data.unwrap();
    assert_eq!(data["drift"], json!(false));
    // Documented contract uses `user_toml_hash`, not `disk_revision`.
    assert!(data.get("user_toml_hash").is_some());
    assert!(data.get("disk_revision").is_none());

    // Hand-edit the file on disk → drift.
    let mut edited = Config::default_config();
    edited.last_selected_mode = Some("drifted".to_string());
    std::fs::write(
        &path,
        conductor_core::config::canonical::serialise(&edited).unwrap(),
    )
    .unwrap();
    let resp = mgr.handle_ipc_request(req(), None).await;
    assert_eq!(resp.data.unwrap()["drift"], json!(true));
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new (Enigo) needs a display server
async fn import_config_directory_named_toml_is_rejected_by_path_validation() {
    // A directory named `*.toml` passes the lexical check but is not a regular
    // file. ADR-034 §D2.2 safe-walk catches it via the post-open fstat →
    // PathValidationFailed (5006), NOT ConfigNotFound (reserved for genuine
    // NotFound) and NOT a generic InternalError.
    let dir = tempfile::tempdir().unwrap();
    let bogus = dir.path().join("config.toml");
    std::fs::create_dir(&bogus).unwrap(); // a directory, not a file
    // config_path lives in the same directory so `dir` is the allowlist root.
    let cfg_path = dir.path().join("live.toml");

    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _r) = tokio::sync::broadcast::channel(1);
    let mut mgr = EngineManager::new(create_test_config(), cfg_path, cmd_rx, cmd_tx, shutdown_tx)
        .expect("engine builds");

    let req = IpcRequest {
        id: "i2".to_string(),
        command: IpcCommand::ImportConfig,
        args: json!({ "base_generation": 0u64, "path": bogus.to_string_lossy() }),
    };
    let resp = mgr.handle_ipc_request(req, None).await;
    let code = resp.error.unwrap().code;
    assert_ne!(
        code,
        IpcErrorCode::ConfigNotFound.as_u16(),
        "is-a-directory must not masquerade as ConfigNotFound"
    );
    assert_eq!(code, IpcErrorCode::PathValidationFailed.as_u16());
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new (Enigo) needs a display server
async fn import_config_missing_path_beneath_root_is_config_not_found() {
    // ADR-034 §D2.2: a genuinely missing import path beneath the config dir is
    // an operator typo → ConfigNotFound (not PathValidationFailed), preserving
    // the pre-safe-walk semantics.
    let dir = tempfile::tempdir().unwrap();
    let cfg_path = dir.path().join("live.toml");
    let missing = dir.path().join("nope.toml");

    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _r) = tokio::sync::broadcast::channel(1);
    let mut mgr = EngineManager::new(create_test_config(), cfg_path, cmd_rx, cmd_tx, shutdown_tx)
        .expect("engine builds");

    let req = IpcRequest {
        id: "i5".to_string(),
        command: IpcCommand::ImportConfig,
        args: json!({ "base_generation": 0u64, "path": missing.to_string_lossy() }),
    };
    let resp = mgr.handle_ipc_request(req, None).await;
    assert_eq!(
        resp.error.unwrap().code,
        IpcErrorCode::ConfigNotFound.as_u16(),
        "a missing path beneath the root should be ConfigNotFound, not 5006"
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new (Enigo) needs a display server
async fn import_config_rejects_symlink_traversal() {
    // ADR-034 §D2.2: an import path that reaches its target through a symlink
    // must be refused even when the symlink target is itself a valid config.
    // This is the `~/.config/conductor -> /tmp/attacker` class of attack.
    let dir = tempfile::tempdir().unwrap();
    let cfg_path = dir.path().join("live.toml");

    // A real, valid config OUTSIDE the allowlist root…
    let outside = tempfile::tempdir().unwrap();
    let real = outside.path().join("evil.toml");
    let mut evil = Config::default_config();
    evil.last_selected_mode = Some("attacker".to_string());
    std::fs::write(
        &real,
        conductor_core::config::canonical::serialise(&evil).unwrap(),
    )
    .unwrap();
    // …reached via a symlink BENEATH the root.
    let link = dir.path().join("import.toml");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _r) = tokio::sync::broadcast::channel(1);
    let mut mgr = EngineManager::new(create_test_config(), cfg_path, cmd_rx, cmd_tx, shutdown_tx)
        .expect("engine builds");

    let base = mgr.live_config.load().state_generation;
    let req = IpcRequest {
        id: "i3".to_string(),
        command: IpcCommand::ImportConfig,
        args: json!({ "base_generation": base, "path": link.to_string_lossy() }),
    };
    let resp = mgr.handle_ipc_request(req, None).await;
    assert_eq!(
        resp.error.unwrap().code,
        IpcErrorCode::PathValidationFailed.as_u16(),
        "symlinked import path must be refused by the safe-walk"
    );
    // The attacker config must NOT have been applied.
    assert_eq!(
        mgr.live_config.load().state_generation,
        base,
        "rejected import must not advance the live snapshot"
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new (Enigo) needs a display server
async fn import_config_accepts_valid_toml_beneath_root() {
    // ADR-034 §D2.2: a real, regular .toml sibling beneath the config
    // directory imports successfully through the safe-walk.
    let dir = tempfile::tempdir().unwrap();
    let cfg_path = dir.path().join("live.toml");
    let sibling = dir.path().join("import.toml");
    let mut wanted = Config::default_config();
    wanted.last_selected_mode = Some("imported".to_string());
    std::fs::write(
        &sibling,
        conductor_core::config::canonical::serialise(&wanted).unwrap(),
    )
    .unwrap();

    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _r) = tokio::sync::broadcast::channel(1);
    let mut mgr = EngineManager::new(create_test_config(), cfg_path, cmd_rx, cmd_tx, shutdown_tx)
        .expect("engine builds");

    let base = mgr.live_config.load().state_generation;
    let req = IpcRequest {
        id: "i4".to_string(),
        command: IpcCommand::ImportConfig,
        args: json!({ "base_generation": base, "path": sibling.to_string_lossy() }),
    };
    let resp = mgr
        .handle_ipc_request(
            req,
            Some(crate::security::CallerContext::internal_trusted()),
        )
        .await;
    assert!(
        matches!(resp.status, ResponseStatus::Success),
        "valid import beneath root should succeed: {resp:?}"
    );
    assert_eq!(
        mgr.live_config.load().config.last_selected_mode.as_deref(),
        Some("imported")
    );
}

// ── ADR-034 §D9 ConfigWatcher demotion ───────────────────────
//
// Behavioural tests for `handle_external_config_change`: in the managed
// default an external write to the watched file must NOT reload the live
// tree; legacy `source = "file"` retains auto-reload.

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new (Enigo) needs a display server
async fn external_change_in_managed_notify_mode_does_not_reload() {
    // ADR-034 §D9: write a DIFFERENT config to the watched path, then signal
    // an external change. The managed/notify default must leave the live
    // in-memory tree authoritative — no reload, generation unchanged.
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    let mut on_disk = Config::default_config();
    on_disk.last_selected_mode = Some("edited_externally".to_string());
    std::fs::write(
        &path,
        conductor_core::config::canonical::serialise(&on_disk).unwrap(),
    )
    .unwrap();

    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _r) = tokio::sync::broadcast::channel(1);
    // create_test_config() defaults to managed + notify.
    let mut mgr = EngineManager::new(
        create_test_config(),
        path.clone(),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("engine builds");

    let base = mgr.live_config.load().state_generation;
    mgr.handle_external_config_change(path.clone()).await;

    let snap = mgr.live_config.load();
    assert_eq!(
        snap.state_generation, base,
        "notify mode must not reload on external edit"
    );
    assert_ne!(
        snap.config.last_selected_mode.as_deref(),
        Some("edited_externally"),
        "the external edit must not have been loaded into the live tree"
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new (Enigo) needs a display server
async fn external_change_in_legacy_file_mode_reloads() {
    // ADR-034 §D9: legacy `source = "file"` retains the pre-ADR auto-reload.
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    let mut on_disk = Config::default_config();
    on_disk.last_selected_mode = Some("reloaded_from_disk".to_string());
    std::fs::write(
        &path,
        conductor_core::config::canonical::serialise(&on_disk).unwrap(),
    )
    .unwrap();

    // Daemon's live config opts into legacy file source.
    let mut live = create_test_config();
    live.config_meta.source = conductor_core::ConfigSource::File;

    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _r) = tokio::sync::broadcast::channel(1);
    let mut mgr =
        EngineManager::new(live, path.clone(), cmd_rx, cmd_tx, shutdown_tx).expect("engine builds");
    // `reload_config` requires a Running lifecycle (Init → Reloading is invalid);
    // walk the normal startup transitions the daemon's `run()` would perform.
    mgr.transition_state(crate::daemon::types::LifecycleState::Starting)
        .await
        .unwrap();
    mgr.transition_state(crate::daemon::types::LifecycleState::Running)
        .await
        .unwrap();

    // Sanity: the live tree starts WITHOUT the on-disk marker, in legacy mode.
    assert_eq!(
        mgr.live_config.load().config.config_meta.source,
        conductor_core::ConfigSource::File,
        "new() must preserve the legacy file source into live_config"
    );
    assert_ne!(
        mgr.live_config.load().config.last_selected_mode.as_deref(),
        Some("reloaded_from_disk")
    );

    mgr.handle_external_config_change(path.clone()).await;

    // Legacy auto-reload loaded the edited file into the live tree.
    assert_eq!(
        mgr.live_config.load().config.last_selected_mode.as_deref(),
        Some("reloaded_from_disk"),
        "legacy file mode must load the edited file"
    );
}

// ── ADR-034 §D8.3 — GetStatus surfaces pending-at-crash ───────

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new (Enigo) needs a display server
async fn status_includes_audit_pending_at_crash_array() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _r) = tokio::sync::broadcast::channel(1);
    let mut mgr = EngineManager::new(
        create_test_config(),
        tmp.path().to_path_buf(),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("engine builds");

    let req = IpcRequest {
        id: "st1".to_string(),
        command: IpcCommand::Status,
        args: json!({}),
    };
    let resp = mgr.handle_ipc_request(req, None).await;
    assert!(
        matches!(resp.status, ResponseStatus::Success),
        "resp: {resp:?}"
    );

    // §D8.3: the status payload always carries `audit.pending_at_crash` as an
    // array (empty here — no unresolved outbox rows), so an operator/GUI can
    // surface in-flight-at-crash mutations.
    let data = resp.data.expect("status data");
    let pac = &data["audit"]["pending_at_crash"];
    assert!(
        pac.is_array(),
        "audit.pending_at_crash must be an array, got {pac:?}"
    );
}

// ── GetConfigBody read IPC ──────────────────────────────
//
// The GUI's `get_config` previously read `config.toml` from disk, so it could
// display a config the running daemon was NOT serving (after an LLM/IPC mutate
// of the live tree) and a later save would CAS-pass against a fresh generation
// while clobbering the canonical content (ADR-043 anti-clobber defeated). B2
// adds `GetConfigBody`: a ReadOnly IPC returning the daemon's canonical config
// body + `state_generation` atomically, so the GUI reads what the daemon runs.

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new (Enigo) needs a display server
async fn get_config_body_returns_canonical_config_and_generation() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _r) = tokio::sync::broadcast::channel(1);
    let mut mgr = EngineManager::new(
        create_test_config(),
        tmp.path().to_path_buf(),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("engine builds");

    // Mutate the live config so the on-disk file (untouched here) would NOT
    // match — the read must reflect the daemon's in-memory tree, not disk.
    let base = mgr.live_config.load().state_generation;
    let mut new_cfg = Config::default_config();
    new_cfg.last_selected_mode = Some("canonical_marker".to_string());
    let save = IpcRequest {
        id: "gcb-save".to_string(),
        command: IpcCommand::SaveConfig,
        args: json!({ "base_generation": base, "config": new_cfg }),
    };
    assert!(matches!(
        mgr.handle_ipc_request(save, None).await.status,
        ResponseStatus::Success
    ));

    let req = IpcRequest {
        id: "gcb1".to_string(),
        command: IpcCommand::GetConfigBody,
        args: json!({}),
    };
    let resp = mgr.handle_ipc_request(req, None).await;
    assert!(
        matches!(resp.status, ResponseStatus::Success),
        "resp: {resp:?}"
    );
    let data = resp.data.expect("GetConfigBody returns data");

    // state_generation matches the post-save snapshot (so the GUI can thread it
    // back as the next SaveConfig base_generation — one coherent snapshot).
    assert_eq!(
        data["state_generation"].as_u64(),
        Some(base + 1),
        "GetConfigBody must carry the canonical state_generation"
    );
    // The config BODY is present (unlike metadata-only GetConfigSnapshot) and
    // reflects the daemon's in-memory tree, not the on-disk fallback.
    assert_eq!(
        data["config"]["last_selected_mode"].as_str(),
        Some("canonical_marker"),
        "GetConfigBody must return the daemon's canonical config body, got {data:?}"
    );
    // The body deserializes cleanly back into a Config (frontend round-trips it).
    let parsed: Config = serde_json::from_value(data["config"].clone())
        .expect("GetConfigBody body deserializes into Config");
    assert_eq!(
        parsed.last_selected_mode.as_deref(),
        Some("canonical_marker")
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new (Enigo) needs a display server
async fn get_config_body_serves_freshly_booted_config_without_a_mutate() {
    // Regression: a cold-booted daemon (NO mutate yet) must serve its live
    // config via GetConfigBody — not blank it as the gen-0 sentinel. Before the boot
    // published at gen 1 (`LiveConfig::new_published`, ADR-034 KI-A2/R6-A8), the boot
    // seeded gen 0, `handle_get_config_body` blanked the body, and the GUI fell back
    // to stale `config.toml` (the slider-reverts-on-restart symptom). This test
    // FAILS on the old gen-0 boot and passes now.
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _r) = tokio::sync::broadcast::channel(1);
    let mut cfg = create_test_config();
    cfg.last_selected_mode = Some("boot_marker".to_string());
    let mut mgr = EngineManager::new(cfg, tmp.path().to_path_buf(), cmd_rx, cmd_tx, shutdown_tx)
        .expect("engine builds");

    // Read straight after boot — no mutate.
    let req = IpcRequest {
        id: "gcb-boot".to_string(),
        command: IpcCommand::GetConfigBody,
        args: json!({}),
    };
    let resp = mgr.handle_ipc_request(req, None).await;
    assert!(
        matches!(resp.status, ResponseStatus::Success),
        "resp: {resp:?}"
    );
    let data = resp.data.expect("GetConfigBody returns data");

    // Boot is the FIRST PUBLISHED snapshot → gen 1, NOT the gen-0 sentinel.
    assert_eq!(
        data["state_generation"].as_u64(),
        Some(1),
        "boot publishes gen 1"
    );
    // The body is SERVED, not blanked to null.
    assert!(
        !data["config"].is_null(),
        "freshly-booted GetConfigBody must serve the config, got {data:?}"
    );
    assert_eq!(
        data["config"]["last_selected_mode"].as_str(),
        Some("boot_marker"),
        "GetConfigBody serves the real boot config on a cold start"
    );
}

// ── SaveConfig content-hash guard ────────────
//
// An earlier fix closed the stale-DISPLAY clobber (GUI now reads the canonical tree).
// The residual race: save_config re-fetches a FRESH base_generation, so the
// mutate CAS always passes — an LLM/conductorctl mutation landing between the
// GUI's display and the user's save is silently overwritten. The guard: an
// optional `base_revision` (content hash) the client captured at display time;
// SaveConfig rejects (StaleBaseContent) if it no longer matches the daemon's
// current revision. Content-hash, NOT generation — the daemon's own self-writes
// bump the generation without changing content, so a generation check would
// false-positive (ADR-034 §D4 / ADR-043).

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new (Enigo) needs a display server
async fn save_config_rejects_stale_base_revision_without_committing() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _r) = tokio::sync::broadcast::channel(1);
    let mut mgr = EngineManager::new(
        create_test_config(),
        tmp.path().to_path_buf(),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("engine builds");

    // Capture the base the "GUI" would have displayed.
    let stale_revision = format!("{}", mgr.live_config.load().revision);

    // An intervening mutation advances the content (and the revision).
    let base = mgr.live_config.load().state_generation;
    let mut intervening = Config::default_config();
    intervening.last_selected_mode = Some("llm_changed_this".to_string());
    let r = mgr
        .handle_ipc_request(
            IpcRequest {
                id: "interv".to_string(),
                command: IpcCommand::SaveConfig,
                args: json!({ "base_generation": base, "config": intervening }),
            },
            None,
        )
        .await;
    assert!(matches!(r.status, ResponseStatus::Success), "resp: {r:?}");
    let current_revision = format!("{}", mgr.live_config.load().revision);
    assert_ne!(
        stale_revision, current_revision,
        "precondition: the intervening mutation changed the revision"
    );
    let gen_after_interv = mgr.live_config.load().state_generation;

    // The user now saves their edit against the STALE displayed revision, but a
    // FRESH base_generation (mirroring the GUI re-fetch). The generation CAS
    // would pass; the content-hash guard must catch the conflict.
    let mut user_edit = Config::default_config();
    user_edit.last_selected_mode = Some("user_would_clobber".to_string());
    let resp = mgr
        .handle_ipc_request(
            IpcRequest {
                id: "guard".to_string(),
                command: IpcCommand::SaveConfig,
                args: json!({
                    "base_generation": gen_after_interv,
                    "base_revision": stale_revision,
                    "config": user_edit,
                }),
            },
            None,
        )
        .await;

    assert!(
        matches!(resp.status, ResponseStatus::Error),
        "resp: {resp:?}"
    );
    assert_eq!(
        resp.error.unwrap().code,
        IpcErrorCode::StaleBaseContent.as_u16()
    );
    // The intervening change is intact — the user's save did NOT clobber it.
    assert_eq!(
        mgr.live_config.load().config.last_selected_mode.as_deref(),
        Some("llm_changed_this"),
        "a stale-revision save must not commit (no clobber)"
    );
    assert_eq!(
        mgr.live_config.load().state_generation,
        gen_after_interv,
        "rejected save must not advance the generation"
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new (Enigo) needs a display server
async fn save_config_accepts_matching_base_revision() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _r) = tokio::sync::broadcast::channel(1);
    let mut mgr = EngineManager::new(
        create_test_config(),
        tmp.path().to_path_buf(),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("engine builds");

    let base = mgr.live_config.load().state_generation;
    let base_revision = format!("{}", mgr.live_config.load().revision);
    let mut new_cfg = Config::default_config();
    new_cfg.last_selected_mode = Some("matched".to_string());

    let resp = mgr
        .handle_ipc_request(
            IpcRequest {
                id: "match".to_string(),
                command: IpcCommand::SaveConfig,
                args: json!({
                    "base_generation": base,
                    "base_revision": base_revision,
                    "config": new_cfg,
                }),
            },
            None,
        )
        .await;

    assert!(
        matches!(resp.status, ResponseStatus::Success),
        "resp: {resp:?}"
    );
    assert_eq!(
        mgr.live_config.load().config.last_selected_mode.as_deref(),
        Some("matched")
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new (Enigo) needs a display server
async fn save_config_without_base_revision_skips_content_guard() {
    // Backward compatibility: a SaveConfig with no `base_revision` behaves
    // exactly as before the guard — committed on a matching base_generation.
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _r) = tokio::sync::broadcast::channel(1);
    let mut mgr = EngineManager::new(
        create_test_config(),
        tmp.path().to_path_buf(),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("engine builds");

    let base = mgr.live_config.load().state_generation;
    let mut new_cfg = Config::default_config();
    new_cfg.last_selected_mode = Some("no_guard".to_string());

    let resp = mgr
        .handle_ipc_request(
            IpcRequest {
                id: "noguard".to_string(),
                command: IpcCommand::SaveConfig,
                args: json!({ "base_generation": base, "config": new_cfg }),
            },
            None,
        )
        .await;

    assert!(
        matches!(resp.status, ResponseStatus::Success),
        "resp: {resp:?}"
    );
    assert_eq!(
        mgr.live_config.load().config.last_selected_mode.as_deref(),
        Some("no_guard")
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new (Enigo) needs a display server
async fn save_config_rejects_present_non_string_base_revision() {
    // `base_revision` is OPTIONAL, but a PRESENT value must be a
    // string. A non-string (null / number / object) must NOT silently disable
    // the guard — that would let a malformed client reintroduce the clobber. Only
    // an ABSENT key skips the guard; a present non-string is a malformed request.
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _r) = tokio::sync::broadcast::channel(1);
    let mut mgr = EngineManager::new(
        create_test_config(),
        tmp.path().to_path_buf(),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("engine builds");

    let base = mgr.live_config.load().state_generation;
    for bad in [json!(null), json!(123), json!({"x": 1})] {
        let resp = mgr
            .handle_ipc_request(
                IpcRequest {
                    id: "badrev".to_string(),
                    command: IpcCommand::SaveConfig,
                    args: json!({
                        "base_generation": base,
                        "base_revision": bad,
                        "config": Config::default_config(),
                    }),
                },
                None,
            )
            .await;
        assert!(
            matches!(resp.status, ResponseStatus::Error),
            "present non-string base_revision must be rejected, not silently skipped: {resp:?}"
        );
        // Did NOT commit.
        assert_eq!(mgr.live_config.load().state_generation, base);
    }
}

// ── GetConfigDiff IPC (ADR-034 §D4.D) ─────────────────────────
//
// Structured diff of the in-memory live config vs the on-disk drift source.
// Precursor for the drift-banner Review-diff / Overwrite.

#[test]
fn config_changed_sections_lists_only_differing_top_level_keys() {
    let live = json!({ "modes": [1], "endpoints": ["a"], "advanced_settings": {"x": 1} });
    let target = json!({ "modes": [1], "endpoints": ["a", "b"], "advanced_settings": {"x": 2} });
    // Sorted (BTreeSet), only the keys whose values differ.
    assert_eq!(
        config_changed_sections(&live, &target),
        vec!["advanced_settings".to_string(), "endpoints".to_string()]
    );
    // Identical → no changed sections.
    assert!(config_changed_sections(&live, &live).is_empty());
    // A key present on only one side still registers (union of keys).
    assert_eq!(
        config_changed_sections(&json!({ "routes": [1] }), &json!({})),
        vec!["routes".to_string()]
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new (Enigo) needs a display server
async fn get_config_diff_compares_in_memory_live_vs_on_disk() {
    let tmp = tempfile::tempdir().unwrap();
    // Canonical dir (macOS $TMPDIR is /var → /private/var) + a `.toml` name: the
    // §D2.2 safe-walk in GetConfigDiff rejects both symlinked components and
    // non-`.toml` paths.
    let path = std::fs::canonicalize(tmp.path())
        .unwrap()
        .join("config.toml");
    let cfg = create_test_config();
    // Seed disk with the live config so the initial diff is empty.
    std::fs::write(&path, toml::to_string(&cfg).unwrap()).unwrap();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _r) = tokio::sync::broadcast::channel(1);
    let mut mgr =
        EngineManager::new(cfg, path.clone(), cmd_rx, cmd_tx, shutdown_tx).expect("engine builds");

    // live == disk → differs=false, well-formed payload.
    let resp = mgr
        .handle_ipc_request(
            IpcRequest {
                id: "d1".to_string(),
                command: IpcCommand::GetConfigDiff,
                args: json!({}),
            },
            None,
        )
        .await;
    assert!(
        matches!(resp.status, ResponseStatus::Success),
        "resp: {resp:?}"
    );
    let data = resp.data.expect("diff data");
    assert_eq!(
        data["differs"],
        json!(false),
        "live==disk must not differ: {data}"
    );
    assert_eq!(data["changed_sections"], json!([]));
    assert!(data["live"].is_object() && data["target"].is_object());

    // Drift the on-disk file (the §D9 "external edit" scenario).
    let mut drifted = create_test_config();
    drifted.last_selected_mode = Some("drifted_on_disk".to_string());
    std::fs::write(&path, toml::to_string(&drifted).unwrap()).unwrap();

    let resp2 = mgr
        .handle_ipc_request(
            IpcRequest {
                id: "d2".to_string(),
                command: IpcCommand::GetConfigDiff,
                args: json!({}),
            },
            None,
        )
        .await;
    let data2 = resp2.data.expect("diff data");
    assert_eq!(
        data2["differs"],
        json!(true),
        "drifted disk must differ: {data2}"
    );
    assert!(
        data2["changed_sections"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s == "last_selected_mode"),
        "changed_sections must name the drifted key: {data2}"
    );
    // The full target tree is returned for the GUI to render the detail.
    assert_eq!(
        data2["target"]["last_selected_mode"],
        json!("drifted_on_disk")
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new (Enigo) needs a display server
async fn get_config_diff_missing_on_disk_file_is_config_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    // Canonical dir (macOS $TMPDIR symlink) + a file that's never created → the
    // §D2.2 safe-walk reports TargetNotFound, mapped to ConfigNotFound.
    let path = std::fs::canonicalize(tmp.path())
        .unwrap()
        .join("absent.toml");
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _r) = tokio::sync::broadcast::channel(1);
    let mut mgr = EngineManager::new(create_test_config(), path, cmd_rx, cmd_tx, shutdown_tx)
        .expect("engine builds");

    let resp = mgr
        .handle_ipc_request(
            IpcRequest {
                id: "d3".to_string(),
                command: IpcCommand::GetConfigDiff,
                args: json!({}),
            },
            None,
        )
        .await;
    assert!(
        matches!(resp.status, ResponseStatus::Error),
        "resp: {resp:?}"
    );
    assert_eq!(
        resp.error.unwrap().code,
        IpcErrorCode::ConfigNotFound.as_u16()
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new (Enigo) needs a display server
async fn overwrite_config_file_writes_live_over_drifted_disk() {
    // "Overwrite user.toml": the on-disk file drifted from the daemon's
    // live config; Overwrite makes disk match live ("my live config wins").
    let tmp = tempfile::tempdir().unwrap();
    // Canonical dir (macOS $TMPDIR is /var → /private/var) + a `.toml` name so
    // Config::save's allowed-dir check (temp_dir) passes.
    let path = std::fs::canonicalize(tmp.path())
        .unwrap()
        .join("config.toml");
    std::fs::write(&path, toml::to_string(&create_test_config()).unwrap()).unwrap();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _r) = tokio::sync::broadcast::channel(1);
    let mut mgr = EngineManager::new(
        create_test_config(),
        path.clone(),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("engine builds");
    // The boot publishes at gen 1 (new_published), so OverwriteConfigFile's
    // gen-0 sentinel guard already passes. Commit a marked live config (threading
    // the boot generation as the CAS base) — this is the "live wins" config.
    let boot_gen = mgr.live_config.load().state_generation;
    let mut live = Config::default_config();
    live.last_selected_mode = Some("live_wins".to_string());
    let save = mgr
        .handle_ipc_request(
            IpcRequest {
                id: "s1".to_string(),
                command: IpcCommand::SaveConfig,
                args: json!({ "base_generation": boot_gen, "config": live }),
            },
            None,
        )
        .await;
    assert!(
        matches!(save.status, ResponseStatus::Success),
        "save: {save:?}"
    );

    // Drift the on-disk file away from live.
    let mut drifted = Config::default_config();
    drifted.last_selected_mode = Some("drifted_on_disk".to_string());
    std::fs::write(&path, toml::to_string(&drifted).unwrap()).unwrap();

    // Overwrite: disk must match live again.
    let resp = mgr
        .handle_ipc_request(
            IpcRequest {
                id: "ow1".to_string(),
                command: IpcCommand::OverwriteConfigFile,
                args: json!({}),
            },
            None,
        )
        .await;
    assert!(
        matches!(resp.status, ResponseStatus::Success),
        "overwrite should succeed: {resp:?}"
    );
    assert!(
        resp.data.expect("revision data").get("revision").is_some(),
        "overwrite response carries the live revision"
    );

    // Disk now parses back to the LIVE config — the drift marker is replaced.
    let on_disk: conductor_core::Config =
        toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        on_disk.last_selected_mode,
        Some("live_wins".to_string()),
        "overwrite must replace the drifted disk file with the daemon's live config"
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new (Enigo) needs a display server
async fn overwrite_config_file_writes_the_user_file_not_the_authority() {
    // The "Overwrite user.toml" action must write the operator-editable
    // USER file (`config.toml`), NOT the daemon's `live.toml` authority. The
    // reported symptom was `config.toml`'s mtime staying stale while `live.toml`
    // was fresh — `armed_profile_write` was targeting `config_path` (the
    // authority) instead of the user file. Here the two paths are DISTINCT, so
    // the test pins which file gets written.
    let tmp = tempfile::tempdir().unwrap();
    // Canonical dir (macOS $TMPDIR is /var → /private/var) + `.toml` names so
    // Config::save's allowed-dir check (temp_dir) passes.
    let dir = std::fs::canonicalize(tmp.path()).unwrap();
    let authority = dir.join("live.toml"); // config_path (the authority)
    let user_file = dir.join("config.toml"); // user_file_path (operator-editable)
    // The authority file on disk is the in-memory LiveConfig's backing store; the
    // Overwrite action must NOT touch it. Seed a sentinel to prove it's untouched.
    let authority_sentinel = "# authority-live-toml-untouched-by-overwrite\n";
    std::fs::write(&authority, authority_sentinel).unwrap();
    std::fs::write(&user_file, toml::to_string(&create_test_config()).unwrap()).unwrap();

    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _r) = tokio::sync::broadcast::channel(1);
    let mut mgr = EngineManager::new(
        create_test_config(),
        authority.clone(), // config_path = the live.toml authority
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("engine builds");
    // Point the user file at config.toml (mirrors service.rs run()).
    mgr.set_user_file_path(user_file.clone());

    // Commit a marked live config ("live wins") threading the boot generation.
    let boot_gen = mgr.live_config.load().state_generation;
    let mut live = Config::default_config();
    live.last_selected_mode = Some("live_wins".to_string());
    let save = mgr
        .handle_ipc_request(
            IpcRequest {
                id: "s1".to_string(),
                command: IpcCommand::SaveConfig,
                args: json!({ "base_generation": boot_gen, "config": live }),
            },
            None,
        )
        .await;
    assert!(
        matches!(save.status, ResponseStatus::Success),
        "save: {save:?}"
    );

    // Overwrite: writes the live config to the USER file.
    let resp = mgr
        .handle_ipc_request(
            IpcRequest {
                id: "ow1".to_string(),
                command: IpcCommand::OverwriteConfigFile,
                args: json!({}),
            },
            None,
        )
        .await;
    assert!(
        matches!(resp.status, ResponseStatus::Success),
        "overwrite should succeed: {resp:?}"
    );

    // The USER file (config.toml) now parses back to the LIVE config.
    let on_user: conductor_core::Config =
        toml::from_str(&std::fs::read_to_string(&user_file).unwrap()).unwrap();
    assert_eq!(
        on_user.last_selected_mode,
        Some("live_wins".to_string()),
        "Overwrite must write the daemon's live config to the USER file (config.toml)"
    );

    // The authority file (live.toml) on disk is UNTOUCHED — it's the LiveConfig's
    // in-memory backing store; armed_profile_write must not clobber it.
    assert_eq!(
        std::fs::read_to_string(&authority).unwrap(),
        authority_sentinel,
        "Overwrite must NOT write the live.toml authority path"
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new (Enigo) needs a display server
async fn profile_switch_persists_identity_with_id_and_reports_it() {
    // A successful SwitchProfile (with the additive profile_id arg)
    // commits identity through the choke point — in-memory ArcSwap (reported by
    // GetActiveProfile, including the id the GUI keys by) AND the durable
    // `active_profile.json` in the persist dir.
    use crate::daemon::active_profile_persist::{self, LoadOutcome};

    let tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap();
    let profile_file = dir.join("studio-profile.toml");
    std::fs::write(
        &profile_file,
        toml::to_string(&create_test_config()).unwrap(),
    )
    .unwrap();

    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _r) = tokio::sync::broadcast::channel(1);
    let mut mgr = EngineManager::new(
        create_test_config(),
        dir.join("live.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("engine builds");
    let persist_dir = dir.join("state");
    std::fs::create_dir_all(&persist_dir).unwrap();
    mgr.set_active_profile_persist_dir(persist_dir.clone());
    // Profile switch requires Running (Init → Reloading is not a valid
    // lifecycle transition) — mirror the lifecycle.rs switch tests.
    mgr.transition_state(crate::daemon::types::LifecycleState::Starting)
        .await
        .unwrap();
    mgr.transition_state(crate::daemon::types::LifecycleState::Running)
        .await
        .unwrap();

    let resp = mgr
        .handle_ipc_request(
            IpcRequest {
                id: "sw1".to_string(),
                command: IpcCommand::SwitchProfile,
                args: json!({
                    "profile_name": "Studio",
                    "config_path": profile_file.display().to_string(),
                    "profile_id": "profile-42",
                }),
            },
            None,
        )
        .await;
    assert!(
        matches!(resp.status, ResponseStatus::Success),
        "switch should succeed: {resp:?}"
    );

    // Durable identity written with the id (choke point → persist).
    match active_profile_persist::load(&persist_dir) {
        LoadOutcome::Valid(p) => {
            assert_eq!(p.profile_id.as_deref(), Some("profile-42"));
            assert_eq!(p.name, "Studio");
        }
        other => panic!("expected persisted identity, got {other:?}"),
    }

    // Reported identity carries the id too (GetActiveProfile).
    let get = mgr.handle_get_active_profile("gp1".to_string());
    let data = get.data.expect("data");
    assert_eq!(data["active_profile"]["id"], "profile-42");
    assert_eq!(data["active_profile"]["name"], "Studio");
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new (Enigo) needs a display server
async fn failed_profile_switch_does_not_touch_persisted_identity() {
    // Identity is written LAST and only on SUCCESS — a
    // failed switch leaves active_profile.json holding the previous,
    // still-true identity.
    use crate::daemon::active_profile_persist::{self, LoadOutcome, PersistedActiveProfile};

    let tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _r) = tokio::sync::broadcast::channel(1);
    let mut mgr = EngineManager::new(
        create_test_config(),
        dir.join("live.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("engine builds");
    let persist_dir = dir.join("state");
    std::fs::create_dir_all(&persist_dir).unwrap();
    mgr.set_active_profile_persist_dir(persist_dir.clone());

    // Seed a previous, true identity.
    let previous = PersistedActiveProfile {
        version: 1,
        profile_id: Some("profile-old".to_string()),
        name: "Old".to_string(),
        config_path: dir.join("old.toml"),
    };
    active_profile_persist::persist(&persist_dir, &previous).unwrap();

    // Switch to a nonexistent profile config → must fail.
    let resp = mgr
        .handle_ipc_request(
            IpcRequest {
                id: "sw-bad".to_string(),
                command: IpcCommand::SwitchProfile,
                args: json!({
                    "profile_name": "Ghost",
                    "config_path": dir.join("missing.toml").display().to_string(),
                    "profile_id": "profile-ghost",
                }),
            },
            None,
        )
        .await;
    assert!(
        matches!(resp.status, ResponseStatus::Error),
        "switch to missing config must fail: {resp:?}"
    );

    // The durable identity is untouched.
    match active_profile_persist::load(&persist_dir) {
        LoadOutcome::Valid(p) => assert_eq!(p.profile_id.as_deref(), Some("profile-old")),
        other => panic!("previous identity must survive a failed switch, got {other:?}"),
    }
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new (Enigo) needs a display server
async fn retarget_watched_user_file_keeps_overwrite_target_aligned_with_watcher() {
    // A profile switch retargets the §D9 watcher to the new
    // profile file. The "Overwrite user.toml" write target (`user_file_path`)
    // must move to the SAME file, or Overwrite + §D9 self-write suppression
    // would target the stale boot user file while the watcher watches the new
    // one. `retarget_watched_user_file` is the single source of truth both
    // profile-switch sites route through.
    let tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap();
    let boot_user = dir.join("config.toml");
    std::fs::write(&boot_user, toml::to_string(&create_test_config()).unwrap()).unwrap();

    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _r) = tokio::sync::broadcast::channel(1);
    let mut mgr = EngineManager::new(
        create_test_config(),
        dir.join("live.toml"), // config_path = authority
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("engine builds");
    mgr.set_user_file_path(boot_user.clone());

    // Observe the retarget channel (mirrors service.rs wiring).
    let (retarget_tx, mut retarget_rx) = mpsc::channel(4);
    mgr.set_watcher_retarget_tx(retarget_tx);

    // Simulate a profile switch retargeting to a new profile file.
    let profile = dir.join("studio-profile.toml");
    mgr.retarget_watched_user_file(profile.clone()).await;

    // The watcher is retargeted to the new profile file...
    assert_eq!(
        retarget_rx.recv().await,
        Some(profile.clone()),
        "watcher must be retargeted to the new profile file"
    );
    // ...and the Overwrite/§D9 write target moved to the SAME file, not the
    // stale boot `config.toml`.
    assert_eq!(
        mgr.user_file_path, profile,
        "user_file_path must track the retargeted watch file after a profile switch"
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new (Enigo) needs a display server
async fn retarget_watched_user_file_does_not_move_target_when_retarget_fails() {
    // `user_file_path` moves ONLY when the watcher retarget is
    // successfully queued. If the retarget send fails (watcher receiver gone),
    // the watcher is still on the PREVIOUS file — so the Overwrite/§D9 target
    // must stay there too, not point at a file nothing watches.
    let tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap();
    let boot_user = dir.join("config.toml");
    std::fs::write(&boot_user, toml::to_string(&create_test_config()).unwrap()).unwrap();

    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _r) = tokio::sync::broadcast::channel(1);
    let mut mgr = EngineManager::new(
        create_test_config(),
        dir.join("live.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("engine builds");
    mgr.set_user_file_path(boot_user.clone());

    // Wire a retarget channel, then DROP the receiver so the next send fails.
    let (retarget_tx, retarget_rx) = mpsc::channel::<std::path::PathBuf>(4);
    mgr.set_watcher_retarget_tx(retarget_tx);
    drop(retarget_rx);

    let profile = dir.join("studio-profile.toml");
    mgr.retarget_watched_user_file(profile).await;

    // Retarget send failed → the write target stays on the previous watch file,
    // preserving the invariant (user_file_path == the file actually watched).
    assert_eq!(
        mgr.user_file_path, boot_user,
        "a failed watcher retarget must NOT move user_file_path off the still-watched file"
    );
}
