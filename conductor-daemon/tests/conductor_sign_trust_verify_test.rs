// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! #1894: `conductor-sign trust verify <manifest.json>` — end-to-end smoke test.
//!
//! Exercises the full operator workflow:
//!
//! 1. Generate a fresh Ed25519 keypair via `conductor-sign generate-key`.
//! 2. Build a minimal root-only rotation manifest using the generated public key.
//! 3. `trust verify <manifest.json>` with an empty trust store → **exit 1**
//!    (rejected: untrusted root).
//! 4. `trust add <pubkey> <name>` to add the key to the trust store.
//! 5. `trust verify <manifest.json>` again → **exit 0** (chain validated).
//!
//! HOME and XDG_CONFIG_HOME are both redirected to a temporary directory so the
//! developer's real trusted_keys.toml cannot influence the result.

#![cfg(feature = "plugin-signing")]

use std::path::PathBuf;
use std::process::Command;

fn conductor_sign_path() -> PathBuf {
    if let Some(p) = option_env!("CARGO_BIN_EXE_conductor-sign") {
        return PathBuf::from(p);
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .expect("workspace root")
        .join("target")
        .join("debug")
        .join("conductor-sign")
}

#[test]
fn trust_verify_untrusted_root_exits_nonzero() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let bin = conductor_sign_path();

    // Generate a keypair so we have a real Ed25519 public key to put in the
    // manifest.  We never add it to the trust store in this test.
    let key_path = tempdir.path().join("test-key");
    let out = Command::new(&bin)
        .args(["generate-key", key_path.to_str().unwrap()])
        .env("HOME", tempdir.path())
        .env("XDG_CONFIG_HOME", tempdir.path().join("config"))
        .output()
        .expect("run generate-key");
    assert!(
        out.status.success(),
        "generate-key failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Read the hex public key.
    let pk_hex = std::fs::read_to_string(format!("{}.public", key_path.display()))
        .expect("read public key file")
        .trim()
        .to_string();

    // Write a minimal root-only rotation manifest.
    let manifest_path = tempdir.path().join("plugin.keys.json");
    let manifest_json = format!(
        r#"{{"signing_keys":[{{"seq":0,"public_key":"{pk_hex}","valid_from":"2026-01-01T00:00:00Z"}}]}}"#
    );
    std::fs::write(&manifest_path, &manifest_json).expect("write manifest");

    // Trust store is empty (fresh HOME) — must be rejected.
    let out = Command::new(&bin)
        .args(["trust", "verify", manifest_path.to_str().unwrap()])
        .env("HOME", tempdir.path())
        .env("XDG_CONFIG_HOME", tempdir.path().join("config"))
        .output()
        .expect("run trust verify (untrusted)");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(1),
        "trust verify with untrusted root MUST exit 1; \
         got {:?}\nstdout=\n{stdout}\nstderr=\n{stderr}",
        out.status.code()
    );
    assert!(
        stderr.contains("Rejected"),
        "expected 'Rejected' in stderr; got:\nstderr=\n{stderr}"
    );
}

#[test]
fn trust_verify_trusted_root_exits_zero() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let bin = conductor_sign_path();

    // Generate a keypair.
    let key_path = tempdir.path().join("test-key");
    let out = Command::new(&bin)
        .args(["generate-key", key_path.to_str().unwrap()])
        .env("HOME", tempdir.path())
        .env("XDG_CONFIG_HOME", tempdir.path().join("config"))
        .output()
        .expect("run generate-key");
    assert!(out.status.success(), "generate-key failed");

    let pk_hex = std::fs::read_to_string(format!("{}.public", key_path.display()))
        .expect("read public key file")
        .trim()
        .to_string();

    // Write the rotation manifest.
    let manifest_path = tempdir.path().join("plugin.keys.json");
    let manifest_json = format!(
        r#"{{"signing_keys":[{{"seq":0,"public_key":"{pk_hex}","valid_from":"2026-01-01T00:00:00Z"}}]}}"#
    );
    std::fs::write(&manifest_path, &manifest_json).expect("write manifest");

    // Add the key to the trust store.
    let out = Command::new(&bin)
        .args(["trust", "add", &pk_hex, "Test Author"])
        .env("HOME", tempdir.path())
        .env("XDG_CONFIG_HOME", tempdir.path().join("config"))
        .output()
        .expect("run trust add");
    assert!(
        out.status.success(),
        "trust add failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Now verify — the key IS trusted, chain must validate.
    let out = Command::new(&bin)
        .args(["trust", "verify", manifest_path.to_str().unwrap()])
        .env("HOME", tempdir.path())
        .env("XDG_CONFIG_HOME", tempdir.path().join("config"))
        .output()
        .expect("run trust verify (trusted)");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(0),
        "trust verify with trusted root MUST exit 0; \
         got {:?}\nstdout=\n{stdout}\nstderr=\n{stderr}",
        out.status.code()
    );
    assert!(
        stdout.contains("Valid rotation chain"),
        "expected 'Valid rotation chain' in stdout; got:\nstdout=\n{stdout}"
    );
}
