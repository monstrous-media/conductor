// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-027 D7 (issue #1178) — capture truncated stdout/stderr from
//! shell actions.
//!
//! Spec text: "Truncated stdout/stderr (4 KB cap) is logged to audit."
//!
//! PR #1174 (D7 timeout half) wires the SIGTERM→grace→SIGKILL
//! watchdog but spawns the child with default stdio (inherit from
//! parent). This issue adds bounded capture: the watcher thread
//! reads up to `CAPTURE_CAP_BYTES = 4096` from each stream into a
//! `CapturedOutput` payload, draining past the cap so the child
//! doesn't block on a full pipe, then surfaces the captured bytes
//! through `WatcherReport.captured`.
//!
//! Unix-only — Windows skipped (no fork(2), different signal
//! semantics — D7's Windows arm is itself a stub via `Child::kill`).

#![cfg(unix)]

use std::process::Command;
use std::time::Duration;

use conductor_daemon::shell_timeout::{CAPTURE_CAP_BYTES, WatcherOutcome, spawn_with_timeout};

/// A child that prints a small known stdout + stderr MUST be captured
/// in full and surfaced via `WatcherReport.captured`. Truncation flags
/// stay false because output fits well under `CAPTURE_CAP_BYTES`.
#[test]
fn small_output_captured_in_full() {
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg("printf 'hello stdout'; printf 'hello stderr' >&2");

    let result = spawn_with_timeout(cmd, Duration::from_secs(5), Duration::from_secs(2))
        .expect("spawn should succeed");
    let report = result.watcher.join().expect("watcher panicked");

    match &report.outcome {
        WatcherOutcome::Completed(status) => assert!(status.success(), "child should exit 0"),
        other => panic!("expected Completed, got {:?}", other),
    }

    assert_eq!(
        report.captured.stdout, b"hello stdout",
        "stdout MUST be captured exactly"
    );
    assert_eq!(
        report.captured.stderr, b"hello stderr",
        "stderr MUST be captured exactly"
    );
    assert!(
        !report.captured.stdout_truncated,
        "stdout fit under cap — truncated flag must be false"
    );
    assert!(
        !report.captured.stderr_truncated,
        "stderr fit under cap — truncated flag must be false"
    );
}

/// Issue's central truncation invariant: a child that emits MORE
/// than `CAPTURE_CAP_BYTES` to stdout MUST surface exactly
/// `CAPTURE_CAP_BYTES` of captured bytes AND `stdout_truncated = true`.
/// The child MUST still complete naturally (draining past the cap
/// keeps the pipe from filling and stalling the child).
#[test]
fn oversized_stdout_is_truncated_at_cap_and_flagged() {
    // Emit 8 KB to stdout — twice the cap — with a deterministic
    // pattern so the captured prefix is verifiable.
    //
    // `printf '%.0sA' {1..8192}` would shell-expand the range and
    // produce 8192 'A's in bash but not POSIX sh. Use `yes` piped
    // through `head -c` instead, which is portable.
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(format!(
        "yes A | tr -d '\\n' | head -c {}",
        CAPTURE_CAP_BYTES * 2
    ));

    let result = spawn_with_timeout(cmd, Duration::from_secs(10), Duration::from_secs(2))
        .expect("spawn should succeed");
    let report = result.watcher.join().expect("watcher panicked");

    assert!(
        matches!(report.outcome, WatcherOutcome::Completed(_)),
        "child MUST complete naturally despite oversized output \
         (draining past the cap prevents pipe-full stall); got {:?}",
        report.outcome
    );
    assert_eq!(
        report.captured.stdout.len(),
        CAPTURE_CAP_BYTES,
        "captured stdout MUST be exactly CAPTURE_CAP_BYTES bytes \
         when child output exceeds the cap"
    );
    assert!(
        report.captured.stdout.iter().all(|&b| b == b'A'),
        "captured prefix MUST be the leading bytes of the child's \
         output (the truncation point is the cap, not arbitrary \
         reads from an interleaved stream)"
    );
    assert!(
        report.captured.stdout_truncated,
        "stdout exceeded cap — truncated flag MUST be true"
    );
}

/// Mirror of the above for stderr. Same invariants — separate test
/// so a regression on one stream is identifiable without a confused
/// failure on the other.
#[test]
fn oversized_stderr_is_truncated_at_cap_and_flagged() {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(format!(
        "yes E | tr -d '\\n' | head -c {} 1>&2",
        CAPTURE_CAP_BYTES * 2
    ));

    let result = spawn_with_timeout(cmd, Duration::from_secs(10), Duration::from_secs(2))
        .expect("spawn should succeed");
    let report = result.watcher.join().expect("watcher panicked");

    assert!(
        matches!(report.outcome, WatcherOutcome::Completed(_)),
        "child MUST complete naturally despite oversized output"
    );
    assert_eq!(report.captured.stderr.len(), CAPTURE_CAP_BYTES);
    assert!(report.captured.stderr.iter().all(|&b| b == b'E'));
    assert!(report.captured.stderr_truncated);
    // Cross-stream sanity: stdout untouched on a stderr-only child.
    assert!(report.captured.stdout.is_empty());
    assert!(!report.captured.stdout_truncated);
}

/// Child that emits nothing — capture buffers are empty, truncation
/// flags false. Regression test for the "no output" baseline so a
/// future "always capture at least one byte" bug surfaces here.
#[test]
fn empty_output_produces_empty_capture() {
    let cmd = Command::new("true");

    let result = spawn_with_timeout(cmd, Duration::from_secs(5), Duration::from_secs(2))
        .expect("spawn should succeed");
    let report = result.watcher.join().expect("watcher panicked");

    assert!(matches!(report.outcome, WatcherOutcome::Completed(_)));
    assert!(report.captured.stdout.is_empty());
    assert!(report.captured.stderr.is_empty());
    assert!(!report.captured.stdout_truncated);
    assert!(!report.captured.stderr_truncated);
}
