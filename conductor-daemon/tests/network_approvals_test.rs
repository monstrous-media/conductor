// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-042 Phase B-early Slice B.3 — approval-registry file I/O tests.
//!
//! The crypto/envelope battery (tamper, alg-confusion, duplicate-key) lives in
//! the module's inline unit tests; this file exercises the hardened on-disk
//! load/save path. Unix-only (hardening uses `libc`/`O_NOFOLLOW`).

#![cfg(unix)]

use conductor_core::security::keychain::HmacKey;
use conductor_daemon::security::network_approvals::{
    ApprovalKey, ApprovalRegistry, ApprovingSurface, RegistryError,
};
use std::os::unix::fs::PermissionsExt;

fn key() -> HmacKey {
    HmacKey::from_bytes([0x7au8; 32])
}

fn listener() -> ApprovalKey {
    ApprovalKey {
        alias: "artnet-in".into(),
        host: "10.0.0.5".into(),
        port: 6454,
        acl_hash: "deadbeef".into(),
    }
}

#[test]
fn save_then_load_roundtrip_and_mode_0600() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("net").join("network_approvals.json");

    let mut reg = ApprovalRegistry::new();
    reg.add_listener_approval(&listener(), ApprovingSurface::Cli);
    reg.save(&path, &key()).expect("save");

    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "registry file must be 0600");

    let loaded = ApprovalRegistry::load(&path, &key()).expect("load");
    assert!(loaded.listener_is_approved(&listener()));
    assert_eq!(reg, loaded);
}

#[test]
fn load_missing_file_returns_empty() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("network_approvals.json");
    let reg = ApprovalRegistry::load(&path, &key()).expect("load missing");
    assert!(reg.listeners.is_empty());
    assert!(!reg.listener_is_approved(&listener()));
}

#[test]
fn on_disk_tamper_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("network_approvals.json");
    let mut reg = ApprovalRegistry::new();
    reg.add_listener_approval(&listener(), ApprovingSurface::Cli);
    reg.save(&path, &key()).unwrap();

    // Corrupt the signed `data` on disk without re-MACing.
    let raw = std::fs::read_to_string(&path).unwrap();
    let mut env: serde_json::Value = serde_json::from_str(&raw).unwrap();
    env["data"] = serde_json::Value::String(format!("{} ", env["data"].as_str().unwrap()));
    std::fs::write(&path, serde_json::to_vec(&env).unwrap()).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

    assert!(
        matches!(
            ApprovalRegistry::load(&path, &key()),
            Err(RegistryError::MacMismatch)
        ),
        "tampered on-disk registry must fail closed"
    );
}

#[test]
fn load_rejects_world_readable_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("network_approvals.json");
    ApprovalRegistry::new().save(&path, &key()).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

    assert!(matches!(
        ApprovalRegistry::load(&path, &key()),
        Err(RegistryError::InsecurePermissions { .. })
    ));
}

#[test]
fn load_rejects_symlinked_file() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real.json");
    ApprovalRegistry::new().save(&real, &key()).unwrap();
    let link = dir.path().join("network_approvals.json");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    assert!(matches!(
        ApprovalRegistry::load(&link, &key()),
        Err(RegistryError::InsecurePermissions { .. })
    ));
}

#[test]
fn load_rejects_oversized_file() {
    // A pathologically large registry file must be refused, not allocated.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("network_approvals.json");
    let big = vec![b'x'; (1 << 20) + 16]; // > 1 MiB cap
    std::fs::write(&path, big).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

    assert!(
        matches!(
            ApprovalRegistry::load(&path, &key()),
            Err(RegistryError::Parse(_))
        ),
        "oversized registry must be refused"
    );
}

#[test]
fn save_rejects_symlinked_parent_dir() {
    // A symlinked parent dir must be refused (a local user could redirect the
    // registry path despite O_NOFOLLOW on the file itself).
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let path = link.join("network_approvals.json");
    assert!(matches!(
        ApprovalRegistry::new().save(&path, &key()),
        Err(RegistryError::InsecurePermissions { .. })
    ));
}

#[test]
fn save_tightens_loose_existing_dir_to_0700() {
    let dir = tempfile::tempdir().unwrap();
    let sec = dir.path().join("loose");
    std::fs::create_dir(&sec).unwrap();
    std::fs::set_permissions(&sec, std::fs::Permissions::from_mode(0o755)).unwrap();

    let path = sec.join("network_approvals.json");
    ApprovalRegistry::new().save(&path, &key()).unwrap();
    let mode = std::fs::metadata(&sec).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700, "a loose pre-existing dir must be tightened");
}

#[test]
fn save_creates_security_dir_0700() {
    let dir = tempfile::tempdir().unwrap();
    let sec = dir.path().join("conductor-sec");
    let path = sec.join("network_approvals.json");
    ApprovalRegistry::new().save(&path, &key()).unwrap();
    let mode = std::fs::metadata(&sec).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700, "created registry dir must be owner-only");
}

#[test]
fn save_overwrites_existing_registry_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("network_approvals.json");

    ApprovalRegistry::new().save(&path, &key()).unwrap();
    let mut reg = ApprovalRegistry::load(&path, &key()).unwrap();
    reg.add_listener_approval(&listener(), ApprovingSurface::Gui);
    reg.save(&path, &key()).unwrap();

    let reloaded = ApprovalRegistry::load(&path, &key()).unwrap();
    assert!(reloaded.listener_is_approved(&listener()));
    // No stray temp files left behind.
    let strays: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
        .collect();
    assert!(strays.is_empty(), "atomic write must not leave temp files");
}
