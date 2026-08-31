// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! #1312: `conductor-sign verify` returns exit 0 for untrusted
//! signatures.
//!
//! Pre-fix: the verify command catches the "untrusted key" error
//! from `verify_plugin_signature`, prints a warning + trust-add
//! guidance, and returns normally. Process exit code 0 — automation
//! that runs `conductor-sign verify plugin.wasm` in CI treats a
//! plugin signed by an unknown key as accepted.
//!
//! Post-fix: the same warning text is printed but the process exits
//! with status 1 so callers can distinguish trusted from untrusted.
//!
//! End-to-end test:
//! 1. tempdir for HOME so the developer's real trusted_keys.toml
//!    doesn't leak in.
//! 2. `conductor-sign generate-key` produces .private + .public.
//! 3. Write any bytes to plugin.wasm.
//! 4. `conductor-sign sign plugin.wasm key --name "T" --email "t@e.com"`
//!    writes plugin.wasm.sig signed with our just-generated key.
//! 5. Do NOT add the public key to trusted_keys (we never run
//!    `trust add`, and HOME points at an empty tempdir).
//! 6. `conductor-sign verify plugin.wasm` MUST exit non-zero.

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

/// THE regression test for #1312. End-to-end: generate a key, sign
/// a plugin with it, then verify the plugin without ever trusting
/// the key. The CLI MUST exit with non-zero status.
#[test]
fn verify_with_untrusted_key_exits_non_zero() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let key_path = tempdir.path().join("test-key");
    let plugin_path = tempdir.path().join("plugin.wasm");

    let bin = conductor_sign_path();

    // 1. Generate keypair. The binary writes <path>.private and
    //    <path>.public next to the supplied path.
    let out = Command::new(&bin)
        .args(["generate-key", key_path.to_str().unwrap()])
        .env("HOME", tempdir.path())
        .env("XDG_CONFIG_HOME", tempdir.path().join("config"))
        .output()
        .expect("run generate-key");
    assert!(
        out.status.success(),
        "generate-key must succeed; stdout={}, stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // 2. Write a fake plugin (any bytes — only the SHA256 matters
    //    for signing, and we don't care about content).
    std::fs::write(&plugin_path, b"fake wasm plugin contents for #1312 test")
        .expect("write fake plugin");

    // 3. Sign it with our test key.
    let out = Command::new(&bin)
        .args([
            "sign",
            plugin_path.to_str().unwrap(),
            key_path.to_str().unwrap(),
            "--name",
            "Test Author",
            "--email",
            "test@example.com",
        ])
        .env("HOME", tempdir.path())
        .env("XDG_CONFIG_HOME", tempdir.path().join("config"))
        .output()
        .expect("run sign");
    assert!(
        out.status.success(),
        "sign must succeed; stdout={}, stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let sig_path = format!("{}.sig", plugin_path.display());
    assert!(
        std::path::Path::new(&sig_path).exists(),
        "sign should have produced {sig_path}"
    );

    // 4. Verify WITHOUT trusting the key. Trusted keys live under
    //    `dirs::config_dir()/conductor/trusted_keys.toml`, which on
    //    macOS reads from HOME and on Linux honours XDG_CONFIG_HOME
    //    — both pointing at our empty tempdir. So the verify hits
    //    the "valid signature but untrusted key" branch.
    let out = Command::new(&bin)
        .args(["verify", plugin_path.to_str().unwrap()])
        .env("HOME", tempdir.path())
        .env("XDG_CONFIG_HOME", tempdir.path().join("config"))
        .output()
        .expect("run verify");

    // THE FIX (#1312): exit status MUST be exactly 1. Council R1
    // tightened this from `!status.success()` to `code() == Some(1)`
    // so a panic (which gives 101) or a different early-exit (e.g.
    // 2 for missing .sig) wouldn't masquerade as the #1312 fix.
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(1),
        "verify with untrusted key MUST exit with code 1 (#1312); \
         got code={:?}\nstdout=\n{stdout}\nstderr=\n{stderr}",
        out.status.code()
    );

    // Defensive: confirm the failure was the "untrusted key" branch
    // (not e.g. a missing .sig file), so a regression that broke
    // signing wouldn't masquerade as a #1312 fix.
    assert!(
        stdout.contains("not trusted") || stdout.contains("untrusted"),
        "expected 'untrusted/not trusted' messaging on stdout (the warning text \
         should still print); got:\nstdout=\n{stdout}\nstderr=\n{stderr}"
    );
}

/// Council R3 companion: the happy-path positive test. After
/// trusting the signing key, `verify` must exit 0. Catches a
/// fail-open regression where the fix in `verify_plugin` started
/// over-rejecting (e.g. exiting non-zero even for trusted keys).
#[test]
fn verify_with_trusted_key_exits_zero() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let key_path = tempdir.path().join("test-key");
    let plugin_path = tempdir.path().join("plugin.wasm");
    let bin = conductor_sign_path();

    // Sign chain (same as the untrusted test).
    let out = Command::new(&bin)
        .args(["generate-key", key_path.to_str().unwrap()])
        .env("HOME", tempdir.path())
        .env("XDG_CONFIG_HOME", tempdir.path().join("config"))
        .output()
        .expect("run generate-key");
    assert!(out.status.success(), "generate-key");

    std::fs::write(&plugin_path, b"happy path plugin bytes").expect("write plugin");

    let out = Command::new(&bin)
        .args([
            "sign",
            plugin_path.to_str().unwrap(),
            key_path.to_str().unwrap(),
            "--name",
            "Test Author",
            "--email",
            "test@example.com",
        ])
        .env("HOME", tempdir.path())
        .env("XDG_CONFIG_HOME", tempdir.path().join("config"))
        .output()
        .expect("run sign");
    assert!(out.status.success(), "sign");

    // Read the public key hex from the generated .public file and
    // trust it via `trust add`.
    let public_key_hex = std::fs::read_to_string(format!("{}.public", key_path.display()))
        .expect("read public key")
        .trim()
        .to_string();

    let out = Command::new(&bin)
        .args(["trust", "add", &public_key_hex, "Test Author"])
        .env("HOME", tempdir.path())
        .env("XDG_CONFIG_HOME", tempdir.path().join("config"))
        .output()
        .expect("run trust add");
    assert!(
        out.status.success(),
        "trust add must succeed; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Now verify — the key IS trusted, so we expect exit 0.
    let out = Command::new(&bin)
        .args(["verify", plugin_path.to_str().unwrap()])
        .env("HOME", tempdir.path())
        .env("XDG_CONFIG_HOME", tempdir.path().join("config"))
        .output()
        .expect("run verify");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(0),
        "verify with TRUSTED key MUST exit 0 (don't over-reject); \
         got code={:?}\nstdout=\n{stdout}\nstderr=\n{stderr}",
        out.status.code()
    );
    assert!(
        stdout.contains("verified successfully") || stdout.contains("trusted key"),
        "expected positive verification messaging on stdout; got:\nstdout=\n{stdout}\nstderr=\n{stderr}"
    );
}
