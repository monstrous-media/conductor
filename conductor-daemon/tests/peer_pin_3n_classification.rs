// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-027 D1 (3/N) integration test for [`classify_peer`].
//!
//! The 2/N work pinned the kernel-handle for liveness; (3/N) maps a
//! [`PinnedPeer`] to the gate's [`TrustLevel`] enum — the input to
//! D5's per-tier decision table. This test validates the negative
//! case: a peer that is *not* the conductor GUI, conductorctl, or
//! the conductor daemon itself classifies as `Untrusted`. The
//! positive cases (`GuiTrusted` / `CliTrusted`) are exercised by
//! the unit tests inside `peer_pin.rs`, which use synthetic
//! [`PinnedPeer`] values to drive the classifier without needing a
//! real signed `conductor-gui` binary at a known path.
//!
//! ## Why this test lives in `tests/` rather than `peer_pin.rs::tests`
//!
//! The helper subprocess (`d1-peer-test-helper`, registered as a
//! `[[bin]]` for the same purpose in 2/N) gives us a real peer pid
//! and exe path — `cargo test` couldn't synthesize those for a
//! same-process self-connect. Reusing the helper keeps the
//! infrastructure paid-once across both 2/N and 3/N test files.

#![cfg(any(target_os = "linux", target_os = "macos"))]

use conductor_daemon::security::TrustLevel;
use conductor_daemon::security::peer_pin::{PinnedPeer, classify_peer};
use std::io::Read;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;

/// Locate the helper binary (same logic as
/// `tests/peer_pin_2n_lifecycle.rs`) — `option_env!` with a
/// manifest-relative fallback so older Cargo versions don't hard
/// fail.
fn helper_binary() -> PathBuf {
    if let Some(p) = option_env!("CARGO_BIN_EXE_d1-peer-test-helper") {
        return PathBuf::from(p);
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .expect("workspace root")
        .join("target")
        .join("debug")
        .join("d1-peer-test-helper")
}

/// RAII guard that kills and reaps the spawned helper subprocess on
/// drop. Several fallible steps run between spawning the helper and the
/// explicit cleanup — accepting the connection, reading the greeting,
/// validating the greeting byte, and pinning the peer. If any
/// `expect`/`assert` panics, this guard's `Drop` still tears the helper
/// down during unwinding. Without it, `Child` is dropped without
/// `kill`/`wait` (std does not reap on drop) and the helper — which
/// blocks in `pause()` forever — leaks and stalls cargo's test runner
/// (#1506). On the happy path the test cleans up explicitly and calls
/// [`ChildGuard::disarm`] so `Drop` becomes a no-op.
struct ChildGuard(Option<std::process::Child>);

impl ChildGuard {
    fn new(child: std::process::Child) -> Self {
        Self(Some(child))
    }

    fn child_mut(&mut self) -> &mut std::process::Child {
        self.0.as_mut().expect("child guard already disarmed")
    }

    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[test]
fn classify_peer_returns_untrusted_for_foreign_binary() {
    // Spawn the helper as a real subprocess. Its exe basename is
    // `d1-peer-test-helper`, which doesn't match any of conductor's
    // trusted binary names (`conductor`, `conductorctl`,
    // `conductor-gui`). On macOS its code-signature is either
    // ad-hoc or absent (no Apple Team ID), so the macOS path
    // also returns `Untrusted`. On Linux the basename mismatch is
    // sufficient.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let sock_path = tmp.path().join("d1-3n-classify.sock");

    let listener = UnixListener::bind(&sock_path).expect("bind listener");

    let child = std::process::Command::new(helper_binary())
        .arg(&sock_path)
        .spawn()
        .expect("spawn d1-peer-test-helper");
    // Guard the helper from the moment it is spawned: every fallible step
    // below would otherwise leak it on panic (#1506). Disarmed after the
    // explicit happy-path cleanup.
    let mut guard = ChildGuard::new(child);

    let (mut server_side, _addr) = listener.accept().expect("accept connection");
    let mut greet = [0u8; 1];
    server_side
        .read_exact(&mut greet)
        .expect("read helper greeting");
    assert_eq!(greet[0], b'x', "expected helper greeting byte");

    let pinned = PinnedPeer::from_stream(&server_side).expect("pin peer");

    // Capture the classifier result while the helper is still alive
    // — the macOS path (3/N) inspects the peer via Security.framework
    // and benefits from a live target. Linux's exe-basename + uid
    // path is independent of liveness but the same ordering keeps
    // the test platform-agnostic.
    let trust = classify_peer(&pinned);

    // Clean up the helper *before* asserting. If the assertion
    // panics, the test thread unwinds with `child` already
    // reaped — without this ordering, a failing assert leaks
    // the helper (which sits in `pause()` forever) and stalls
    // cargo's test runner.
    let exe = pinned.initial_exe.clone();
    drop(pinned);
    guard.child_mut().kill().expect("kill helper");
    let _ = guard.child_mut().wait().expect("wait for helper");
    // Helper is reaped; disarm so the guard's Drop is a no-op and a panic
    // in the headline assertion below cannot double-kill.
    guard.disarm();

    // The headline assertion: classify_peer must return Untrusted
    // for a peer whose binary is not part of conductor's
    // trust-anchored set. Returning Trusted here would let
    // arbitrary same-uid processes inherit the gate decisions
    // intended for the GUI / CLI.
    assert_eq!(
        trust,
        TrustLevel::Untrusted,
        "non-conductor binary {} must classify as Untrusted (got {:?})",
        exe.display(),
        trust,
    );
}
