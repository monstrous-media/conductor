// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-027 D1 lifecycle tests for [`PinnedPeer::still_pinned`].
//!
//! These tests exercise the TOCTOU defence added here. They spawn
//! a real subprocess (the `d1-peer-test-helper` bin) so that the
//! peer's pid + exe can be captured and then forcibly terminated —
//! the unit tests inside `peer_pin.rs` use same-process self-connect
//! and can't kill themselves without taking the test runner with
//! them.
//!
//! ## Failure mode under the prior stub scaffold
//!
//! `still_pinned()` always returns `true` in the prior stub scaffold. The test
//! `still_pinned_returns_false_after_peer_exits` MUST fail against
//! that scaffold. Once the kernel-handle
//! check (`pidfd` / `audit_token`) replaces the stub, the test goes green.

#![cfg(any(target_os = "linux", target_os = "macos"))]

use conductor_daemon::security::peer_pin::PinnedPeer;
use std::io::Read;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::Child;
use std::time::{Duration, Instant};

/// How long to wait for the helper to connect before giving up. Generous
/// (the helper connects in milliseconds) but finite, so a helper that exits
/// early, hangs, or is the wrong binary fails the test promptly instead of
/// blocking `cargo test` forever.
const ACCEPT_TIMEOUT: Duration = Duration::from_secs(10);

/// Accept the helper's connection with a bound instead of a naked
/// `listener.accept()` that blocks indefinitely. Polls the
/// non-blocking listener while also checking the child: if the helper exits
/// before connecting we fail immediately with its status; on timeout we kill
/// it. The child is reaped on every error path — an already-exited child is
/// reaped by `try_wait`; every other failure goes through `reap_and_err`
/// (kill-if-running + wait). The listener is restored to blocking before
/// return, and on success so is the accepted stream.
fn accept_helper(
    listener: &UnixListener,
    child: &mut Child,
    timeout: Duration,
) -> Result<UnixStream, String> {
    // Kill (if still running) and reap the child, then return the error, so no
    // failure path leaves a stray child (Copilot review).
    fn reap_and_err(child: &mut Child, msg: String) -> Result<UnixStream, String> {
        let _ = child.kill();
        let _ = child.wait();
        Err(msg)
    }

    if let Err(e) = listener.set_nonblocking(true) {
        return reap_and_err(child, format!("set_nonblocking on listener: {e}"));
    }

    let deadline = Instant::now() + timeout;
    let outcome: Result<UnixStream, String> = loop {
        match listener.accept() {
            Ok((stream, _addr)) => break Ok(stream),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                match child.try_wait() {
                    // `try_wait` already reaped the exited child.
                    Ok(Some(status)) => {
                        break Err(format!(
                            "d1-peer-test-helper exited before connecting (status {status:?})"
                        ));
                    }
                    Ok(None) => {}
                    Err(e) => break reap_and_err(child, format!("try_wait: {e}")),
                }
                if Instant::now() >= deadline {
                    break reap_and_err(
                        child,
                        format!(
                            "timed out after {timeout:?} waiting for \
                             d1-peer-test-helper to connect"
                        ),
                    );
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => break reap_and_err(child, format!("accept failed: {e}")),
        }
    };

    // Restore the listener to blocking on every path so a caller that reuses
    // it isn't surprised by a non-blocking accept (Copilot review).
    let _ = listener.set_nonblocking(false);

    let stream = outcome?;
    stream
        .set_nonblocking(false)
        .map_err(|e| format!("restore blocking on accepted stream: {e}"))?;
    Ok(stream)
}

/// Locate the helper binary that cargo-test built alongside the
/// integration test. `CARGO_BIN_EXE_<name>` is set by cargo when an
/// integration test in the same package depends on a `[[bin]]`;
/// we use `option_env!` (not `env!`) with a manifest-relative
/// fallback so older cargo versions don't fail at compile time —
/// matching the pattern in `conductorctl_permissions_test.rs`.
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

#[test]
fn still_pinned_returns_false_after_peer_exits() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let sock_path = tmp.path().join("d1-2n-lifecycle.sock");

    let listener = UnixListener::bind(&sock_path).expect("bind listener");

    // Spawn the helper as a child process. It will connect to our
    // socket, write a greeting byte, and then sleep forever.
    let mut child = std::process::Command::new(helper_binary())
        .arg(&sock_path)
        .spawn()
        .expect("spawn d1-peer-test-helper");

    // Accept the helper's connection (bounded — never block forever)
    // and read its greeting byte to confirm it's the connecting peer (not a
    // bystander). A read timeout guards against a connected-but-mute helper.
    let mut server_side =
        accept_helper(&listener, &mut child, ACCEPT_TIMEOUT).expect("accept helper connection");
    server_side
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set greeting read timeout");
    let mut greet = [0u8; 1];
    server_side
        .read_exact(&mut greet)
        .expect("read helper greeting");
    assert_eq!(greet[0], b'x', "expected helper greeting byte");

    // Pin the peer. The prior stub captured uid/pid/initial_exe; the
    // current implementation also captures a kernel handle (pidfd on
    // Linux, audit_token on macOS) for race-free liveness.
    let pinned = PinnedPeer::from_stream(&server_side).expect("pin peer");
    assert_eq!(
        pinned.pid,
        child.id(),
        "pinned pid must match the helper child's pid"
    );

    // Kill the helper and wait for it to actually exit. After this
    // returns, the helper's pid is no longer valid (subject to
    // recycling) and any cached `(pid, exe)` snapshot is stale.
    child.kill().expect("kill helper");
    let status = child.wait().expect("wait for helper");
    assert!(
        !status.success(),
        "helper must exit non-zero after SIGKILL, got {:?}",
        status
    );

    // The TOCTOU defence: still_pinned() must observe that the
    // pinned process no longer exists. Under the prior stub this is
    // hard-coded `true` and this assertion fails — that failure is
    // the TDD red bar this kernel-handle check must turn green.
    assert!(
        !pinned.still_pinned(),
        "still_pinned() must return false after peer process has exited; \
         this is the TOCTOU defence the 2/N kernel handle provides"
    );
}

/// Regression: when the spawned child exits before ever connecting,
/// `accept_helper` must fail FAST (detecting the child's exit) rather than
/// blocking until the timeout — and it must reap the child. A naked
/// `listener.accept()` would block forever here.
#[test]
fn accept_helper_fails_fast_when_child_exits_before_connecting() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let sock_path = tmp.path().join("early-exit.sock");
    let listener = UnixListener::bind(&sock_path).expect("bind listener");

    // A child that exits immediately without ever connecting to the socket.
    let mut child = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg("exit 0")
        .spawn()
        .expect("spawn dummy child");

    let start = Instant::now();
    let result = accept_helper(&listener, &mut child, Duration::from_secs(10));

    assert!(
        result.is_err(),
        "accept_helper must fail when the child never connects, got Ok"
    );
    // It must notice the child exited rather than waiting the full timeout.
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "accept_helper should fail fast on early child exit, took {:?}",
        start.elapsed()
    );
    // The child has terminated (it ran `exit 0`); confirm its status is
    // observable without blocking — i.e. accept_helper didn't leave it
    // running. (try_wait is idempotent once a child has been reaped.)
    assert!(
        child.try_wait().expect("try_wait").is_some(),
        "child must have terminated, not be left running by accept_helper"
    );
}
