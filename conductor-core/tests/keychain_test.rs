// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-042 Phase B-early — `FilePermsKeychain` contract tests.
//!
//! These run on any Unix (macOS + Linux CI). The cross-platform OS-keyring
//! roundtrip lives in `keychain_keyring_test.rs` so it also compiles on Windows
//! (this file is Unix-only because the hardening checks use `libc`/`O_NOFOLLOW`).

#![cfg(unix)]

use conductor_core::security::keychain::{FilePermsKeychain, KeychainError, KeychainStore};
use std::os::unix::fs::{MetadataExt, PermissionsExt};

fn temp_security_dir() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("security");
    (tmp, dir)
}

#[test]
fn file_perms_roundtrip_and_rotation() {
    let (_guard, dir) = temp_security_dir();
    let kc = FilePermsKeychain::new_at(dir).expect("construct");

    let a = kc.get_or_create_hmac_key().expect("create");
    let again = kc.get_or_create_hmac_key().expect("reload");
    assert_eq!(
        a.fingerprint(),
        again.fingerprint(),
        "second get_or_create must return the persisted key"
    );

    let rotated = kc.rotate_hmac_key().expect("rotate");
    assert_ne!(
        a.fingerprint(),
        rotated.fingerprint(),
        "rotation must produce a different key"
    );

    let after_rotate = kc.get_or_create_hmac_key().expect("reload after rotate");
    assert_eq!(rotated.fingerprint(), after_rotate.fingerprint());
}

#[test]
fn file_perms_uses_0600_file_and_0700_dir() {
    let (_guard, dir) = temp_security_dir();
    let kc = FilePermsKeychain::new_at(dir.clone()).expect("construct");
    kc.get_or_create_hmac_key().expect("create");

    let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
    assert_eq!(dir_mode, 0o700, "security dir must be owner-only");

    let key_file = dir.join("network_hmac_key.json");
    let file_mode = std::fs::metadata(&key_file).unwrap().permissions().mode() & 0o777;
    assert_eq!(file_mode, 0o600, "key file must be mode 0600");

    let euid = unsafe { libc::geteuid() };
    assert_eq!(std::fs::metadata(&key_file).unwrap().uid(), euid);
}

#[test]
fn file_perms_rejects_world_readable_key_file() {
    let (_guard, dir) = temp_security_dir();
    let kc = FilePermsKeychain::new_at(dir.clone()).expect("construct");
    kc.get_or_create_hmac_key().expect("create");

    // Loosen the mode out-of-band; the hardened read must fail closed.
    let key_file = dir.join("network_hmac_key.json");
    std::fs::set_permissions(&key_file, std::fs::Permissions::from_mode(0o644)).unwrap();

    let err = kc.get_or_create_hmac_key().unwrap_err();
    assert!(
        matches!(err, KeychainError::InsecurePermissions { .. }),
        "expected InsecurePermissions, got {err:?}"
    );
}

#[test]
fn file_perms_rejects_symlinked_key_file() {
    let (_guard, dir) = temp_security_dir();
    let kc = FilePermsKeychain::new_at(dir.clone()).expect("construct");

    // Plant a symlink where the key file would live: O_NOFOLLOW must refuse it.
    let outside = _guard.path().join("attacker_target.json");
    std::fs::write(&outside, br#"{"key_hex":"00","created_at_secs":0}"#).unwrap();
    let key_file = dir.join("network_hmac_key.json");
    std::os::unix::fs::symlink(&outside, &key_file).unwrap();

    let err = kc.get_or_create_hmac_key().unwrap_err();
    assert!(
        matches!(err, KeychainError::InsecurePermissions { .. }),
        "expected InsecurePermissions (symlink), got {err:?}"
    );
}

#[test]
fn file_perms_metadata_reports_fresh_key() {
    let (_guard, dir) = temp_security_dir();
    let kc = FilePermsKeychain::new_at(dir).expect("construct");
    let key = kc.get_or_create_hmac_key().expect("create");

    let md = kc.key_metadata().expect("metadata");
    assert_eq!(md.age_days, 0, "a freshly created key is 0 days old");
    assert_eq!(md.fingerprint, key.fingerprint());
}

#[test]
fn file_perms_concurrent_get_or_create_converge_on_one_key() {
    // TOCTOU regression: many creators racing on the same dir must converge on
    // a single key — none may clobber another's freshly-created key.
    let (_guard, dir) = temp_security_dir();
    std::fs::create_dir_all(&dir).unwrap();

    let mut handles = Vec::new();
    for _ in 0..8 {
        let d = dir.clone();
        handles.push(std::thread::spawn(move || {
            let kc = FilePermsKeychain::new_at(d).expect("construct");
            kc.get_or_create_hmac_key().expect("create").fingerprint()
        }));
    }
    let fps: Vec<String> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let first = &fps[0];
    assert!(
        fps.iter().all(|f| f == first),
        "all concurrent creators must agree on one key: {fps:?}"
    );
}

#[test]
fn file_perms_rotation_never_loses_the_key_file() {
    // Non-atomic-rotation regression: rotate must replace atomically and leave
    // a readable file (never a remove-then-write gap).
    let (_guard, dir) = temp_security_dir();
    let kc = FilePermsKeychain::new_at(dir.clone()).expect("construct");
    let k1 = kc.get_or_create_hmac_key().expect("create");
    let k2 = kc.rotate_hmac_key().expect("rotate");
    assert_ne!(k1.fingerprint(), k2.fingerprint());

    assert!(
        dir.join("network_hmac_key.json").exists(),
        "key file must still exist after rotation"
    );
    let reread = kc.get_or_create_hmac_key().expect("reload after rotate");
    assert_eq!(k2.fingerprint(), reread.fingerprint());
}

#[test]
fn file_perms_corrupt_on_disk_key_fails_closed() {
    // A garbled (but well-permissioned) key file must error, never silently
    // produce an all-zero key.
    let (_guard, dir) = temp_security_dir();
    let kc = FilePermsKeychain::new_at(dir.clone()).expect("construct");
    kc.get_or_create_hmac_key().expect("create");

    let key_file = dir.join("network_hmac_key.json");
    std::fs::write(&key_file, br#"{"key_hex":"zz","created_at_secs":0}"#).unwrap();
    std::fs::set_permissions(&key_file, std::fs::Permissions::from_mode(0o600)).unwrap();

    let err = kc.get_or_create_hmac_key().unwrap_err();
    assert!(
        matches!(err, KeychainError::Corrupt(_)),
        "corrupt key file must fail closed, got {err:?}"
    );
}
