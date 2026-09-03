// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! `conductor-sign generate-key` writes Ed25519 private
//! keys with default umask (0644 typical on Unix). Other local
//! users can read the signing credential.
//!
//! Fix: use `OpenOptions::new().mode(0o600).write(true).create_new(true)`
//! so the file is created with restrictive permissions atomically
//! and refuses to overwrite an existing key. The public key keeps
//! the default permissions (it's, well, public).
//!
//! This test runs the built binary as a subprocess and inspects
//! the .private file's mode bits. Unix-only — Windows doesn't have
//! the same mode-bit model and is out of scope (the binary is
//! gated on `plugin-signing` which is Unix-only in practice).

#![cfg(all(unix, feature = "plugin-signing"))]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Mutex;

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

/// `umask(2)` is process-global (shared across threads), and the
/// permission-mode tests below pin it before spawning the `conductor-sign`
/// subprocess so the asserted modes are deterministic. cargo runs tests
/// concurrently, so those tests MUST be serialized against each other — this
/// lock provides that without pulling in the `serial_test` crate.
static UMASK_LOCK: Mutex<()> = Mutex::new(());

/// Run `conductor-sign generate-key <key_path>` with the process umask pinned
/// to `umask` for the duration of the subprocess spawn, restoring the previous
/// umask before returning.
///
/// Pinning the umask is what makes the mode assertions a real regression guard.
/// The fixed implementation sets the private key's mode to `0o600` *explicitly*
/// (`OpenOptions::mode(0o600)`), so its result is independent of the umask. But
/// a regressed `std::fs::write` implementation would create the file with
/// `0o666 & !umask`. Under an *uncontrolled*, restrictive runner umask (e.g.
/// `0o077`) that regression still yields `0o600`, so the test would pass and
/// miss the bug. With a known permissive umask of `0o022`, the regression
/// instead yields `0o644` and the assertion fails — which is the point.
///
/// Holding `UMASK_LOCK` for the whole set→spawn→restore window guarantees no
/// other test thread observes or fights over the temporarily-changed umask.
fn run_generate_key_with_umask(
    bin: &Path,
    key_path: &Path,
    home: &Path,
    umask: libc::mode_t,
) -> Output {
    let _guard = UMASK_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // SAFETY: `umask(2)` cannot fail; it sets the mask and returns the prior
    // value. We hold `UMASK_LOCK`, so no other test thread mutates it during
    // this window.
    let previous = unsafe { libc::umask(umask) };
    let out = Command::new(bin)
        .args(["generate-key", key_path.to_str().unwrap()])
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .output()
        .expect("run generate-key");
    // Restore the umask for subsequent tests / the runner process.
    unsafe { libc::umask(previous) };

    out
}

/// THE regression test. After `conductor-sign generate-key
/// /path/to/key` runs, the `.private` file MUST have mode 0o600
/// (owner-read/write only). Pre-fix, `std::fs::write` honoured the
/// process umask which typically leaves the file 0644 (world-read).
///
/// The umask is pinned to `0o022` (permissive) around the subprocess so
/// this is a real regression guard. A pre-fix `std::fs::write` implementation
/// would create the file `0o666 & !0o022 = 0o644` and fail this assertion;
/// without the pin, a restrictive runner umask (e.g. `0o077`) would mask that
/// regression down to `0o600` and the test would pass despite the bug.
#[test]
fn generate_key_private_file_has_mode_0600() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let key_path = tempdir.path().join("regression-key");
    let bin = conductor_sign_path();

    let out = run_generate_key_with_umask(&bin, &key_path, tempdir.path(), 0o022);
    assert!(
        out.status.success(),
        "generate-key must succeed; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let private_path = tempdir.path().join("regression-key.private");
    assert!(
        private_path.exists(),
        "generate-key should write .private file"
    );

    let metadata = std::fs::metadata(&private_path).expect("metadata");
    let mode_bits = metadata.permissions().mode() & 0o777;

    assert_eq!(
        mode_bits, 0o600,
        "private signing key MUST be 0o600 (owner-only) even under a permissive \
         0o022 umask; got {mode_bits:#o} (a std::fs::write regression would be \
         0o644 here — #1316/#1542)"
    );
}

/// Defensive companion: the PUBLIC key keeps default permissions —
/// it's public by definition. Pin this so a future "harden everything"
/// PR doesn't accidentally restrict it (which would break legitimate
/// downstream tooling that reads .public files).
///
/// With the umask pinned to `0o022`, the public key — written with the
/// default create mode (`0o666`) and no explicit `.mode()` — must land at
/// exactly `0o644`. Asserting the exact mode (rather than only owner-read)
/// catches BOTH an accidental hardening to `0o600` (the original intent) AND an
/// accidental loosening (e.g. world-writable `0o666`). The previous
/// `mode & 0o400 != 0` check, under an uncontrolled umask, would have passed
/// even if the file were silently hardened to `0o600`.
#[test]
fn generate_key_public_file_is_readable() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let key_path = tempdir.path().join("public-perms-key");
    let bin = conductor_sign_path();

    let out = run_generate_key_with_umask(&bin, &key_path, tempdir.path(), 0o022);
    assert!(out.status.success(), "generate-key must succeed");

    let public_path = tempdir.path().join("public-perms-key.public");
    let metadata = std::fs::metadata(&public_path).expect("public metadata");
    let mode_bits = metadata.permissions().mode() & 0o777;

    assert_eq!(
        mode_bits, 0o644,
        "public key must keep the default broadly-readable mode (0o644 under a \
         0o022 umask); got {mode_bits:#o}. An accidental harden to 0o600 or \
         loosen to 0o666 must fail here (#1542)."
    );
}

/// Sub-requirement: `create_new` semantics. A second
/// `generate-key` on the same output path MUST refuse to overwrite
/// — silently clobbering an existing private key is the worst
/// possible UX for a credential-management tool. The first call
/// established the key; the second should fail with a clear error.
#[test]
fn generate_key_refuses_to_overwrite_existing_private_key() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let key_path = tempdir.path().join("no-overwrite-key");
    let bin = conductor_sign_path();

    // First invocation: clean dir, succeeds.
    let first = Command::new(&bin)
        .args(["generate-key", key_path.to_str().unwrap()])
        .env("HOME", tempdir.path())
        .env("XDG_CONFIG_HOME", tempdir.path().join("config"))
        .output()
        .expect("first generate-key");
    assert!(first.status.success(), "first generate-key must succeed");

    // Capture the private-key bytes so we can verify they survive
    // unchanged after the second call's refusal.
    let private_path = tempdir.path().join("no-overwrite-key.private");
    let first_bytes = std::fs::read(&private_path).expect("read first private");

    // Second invocation: same path. MUST refuse (non-zero exit) AND
    // MUST NOT mutate the existing file.
    let second = Command::new(&bin)
        .args(["generate-key", key_path.to_str().unwrap()])
        .env("HOME", tempdir.path())
        .env("XDG_CONFIG_HOME", tempdir.path().join("config"))
        .output()
        .expect("second generate-key");
    assert!(
        !second.status.success(),
        "second generate-key MUST exit non-zero rather than overwrite (#1316); \
         stdout={}, stderr={}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );

    let second_bytes = std::fs::read(&private_path).expect("read after second attempt");
    assert_eq!(
        first_bytes, second_bytes,
        "existing private key MUST survive the rejected overwrite attempt"
    );
}

/// The no-overwrite guarantee must cover the WHOLE keypair prefix,
/// not just the private half. If `<prefix>.public` exists but
/// `<prefix>.private` does not, `generate-key` previously created a new
/// private key and silently truncated the existing public key — a
/// trust-distribution artifact. It MUST now refuse, leaving the public
/// file untouched and no private key behind.
#[test]
fn generate_key_refuses_to_overwrite_existing_public_key() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let key_path = tempdir.path().join("preexisting-public");
    let public_path = tempdir.path().join("preexisting-public.public");
    let private_path = tempdir.path().join("preexisting-public.private");
    let bin = conductor_sign_path();

    // Seed an existing public file (no private), with sentinel contents.
    let sentinel = b"PREEXISTING-PUBLIC-KEY-DO-NOT-CLOBBER".to_vec();
    std::fs::write(&public_path, &sentinel).expect("seed public file");

    let out = Command::new(&bin)
        .args(["generate-key", key_path.to_str().unwrap()])
        .env("HOME", tempdir.path())
        .env("XDG_CONFIG_HOME", tempdir.path().join("config"))
        .output()
        .expect("run generate-key");

    assert!(
        !out.status.success(),
        "generate-key MUST refuse (non-zero) when <prefix>.public exists (#1419); \
         stdout={}, stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("already exists"),
        "refusal must be the overwrite guard, not some other error: {stderr}"
    );

    // The existing public file MUST be byte-for-byte intact...
    let after = std::fs::read(&public_path).expect("read public after refusal");
    assert_eq!(
        after, sentinel,
        "existing public key MUST survive the rejected generate-key (#1419)"
    );
    // ...and no private key should have been created (the preflight refuses
    // before writing anything).
    assert!(
        !private_path.exists(),
        "no private key must be created when the public path is already occupied"
    );
}
