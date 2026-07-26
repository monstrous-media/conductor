//! Integration tests for ADR-027 D7 (full) — shell timeout enforcement (#1166).
//!
//! Asserts that `spawn_with_timeout`:
//!   - Kills a long-running child within `timeout + grace + slack`.
//!   - Leaves a quickly-finishing child alone (no false-positive kills).
//!   - Uses the process group on Unix so children forked by the action
//!     can also be reaped.
//!
//! These tests spawn the real `sleep` binary; they are skipped on
//! Windows (no Unix process groups, different signal semantics).

#![cfg(unix)]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use conductor_daemon::shell_timeout::{WatcherOutcome, spawn_with_timeout};

/// Whether `pid` is still a live, signalable process. `kill -0` exits 0 when the
/// PID exists and we may signal it (we own it here), non-zero (ESRCH) once it's
/// gone. Dependency-free and works on macOS + Linux. stdout/stderr are silenced
/// so the `kill` binary's "No such process" message doesn't clutter test output.
///
/// Panics if the probe itself can't be spawned (e.g. `kill` not on PATH): a
/// liveness check that *fails to run* must not be silently read as "process
/// gone", which would let a regression slip through. A non-zero EXIT (the
/// process is gone) is the normal path and returns `false`, not a panic.
fn process_alive(pid: i32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("`kill -0` liveness probe must execute")
        .success()
}

/// Poll until `pid` is gone or `deadline` elapses; returns `true` if it became
/// gone (reaped). The grandchild's parent `sh` is killed too, so the kernel
/// reparents the dead `sleep` to init/launchd which reaps it promptly.
fn wait_until_gone(pid: i32, deadline: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < deadline {
        if !process_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    !process_alive(pid)
}

/// Read a PID the wrapper shell wrote to `path`, polling until it appears and
/// parses (the shell writes it asynchronously after we spawn).
fn read_pid(path: &std::path::Path, deadline: Duration) -> Option<i32> {
    let start = Instant::now();
    while start.elapsed() < deadline {
        if let Ok(s) = std::fs::read_to_string(path)
            && let Ok(pid) = s.trim().parse::<i32>()
        {
            return Some(pid);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

/// `/bin/sleep 30` with a 500ms timeout MUST be killed in < 10s.
#[test]
fn long_running_command_is_killed_after_timeout() {
    let mut cmd = Command::new("sleep");
    cmd.arg("30");

    let start = Instant::now();
    let result = spawn_with_timeout(cmd, Duration::from_millis(500), Duration::from_secs(2))
        .expect("spawn should succeed");

    // #1178 — watcher now returns `WatcherReport { outcome, captured }`.
    // Existing tests inspect `.outcome` exactly as before.
    let report = result.watcher.join().expect("watcher panicked");
    let elapsed = start.elapsed();

    assert!(
        matches!(
            report.outcome,
            WatcherOutcome::KilledOnTimeout | WatcherOutcome::KilledAfterGrace
        ),
        "expected killed outcome, got {:?}",
        report.outcome
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "watcher should finish within timeout+grace+slack, got {:?}",
        elapsed
    );
}

/// `/usr/bin/true` returns immediately. Watcher MUST report completion
/// (NOT a false-positive timeout kill).
#[test]
fn quick_command_completes_naturally() {
    let cmd = Command::new("true");

    let result = spawn_with_timeout(cmd, Duration::from_secs(5), Duration::from_secs(2))
        .expect("spawn should succeed");

    let report = result.watcher.join().expect("watcher panicked");

    match report.outcome {
        WatcherOutcome::Completed(status) => {
            assert!(status.success(), "true should exit 0, got {:?}", status);
        }
        other => panic!("expected Completed, got {:?}", other),
    }
}

/// `sh -c "sleep 60 & wait"` forks a grandchild — process-group kill
/// MUST reap it. Without process-group setup, only `sh` dies and
/// `sleep` lingers.
///
/// #1560: asserting only the watcher outcome + timing is insufficient — an
/// implementation that killed just the `sh` child would still report a killed
/// outcome quickly while leaving `sleep 60` running. The shell records the
/// backgrounded sleep's PID, and after the watcher joins we verify that PID is
/// actually gone (reaped), with cleanup if a regression left it alive.
#[test]
fn process_group_kill_reaps_forked_grandchild() {
    let pid_file = tempfile::NamedTempFile::new().expect("create temp pid file");
    let pid_path = pid_file.path().to_path_buf();

    // Background a long sleep, record its PID, then wait. `$!` is the sleep's
    // PID; the path is a tempfile (no spaces) but quoted defensively.
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(format!(
        "sleep 60 & echo $! > \"{}\"; wait",
        pid_path.display()
    ));

    // A 2s timeout (not a tight 500ms) so the shell deterministically records
    // the PID before the kill lands — under load a 500ms timeout could kill `sh`
    // before it runs `echo $!`. The "kills quickly" property is already covered
    // by `long_running_command_is_killed_after_timeout`; here we only need the
    // kill to eventually fire so we can assert the grandchild was reaped.
    let start = Instant::now();
    let result = spawn_with_timeout(cmd, Duration::from_secs(2), Duration::from_secs(2))
        .expect("spawn should succeed");

    // Read the grandchild PID before the kill lands (the shell writes it almost
    // immediately, well within the 2s timeout).
    let pid = read_pid(&pid_path, Duration::from_secs(5))
        .expect("wrapper shell must record the backgrounded sleep's PID");

    let report = result.watcher.join().expect("watcher panicked");
    let elapsed = start.elapsed();

    assert!(
        matches!(
            report.outcome,
            WatcherOutcome::KilledOnTimeout | WatcherOutcome::KilledAfterGrace
        ),
        "expected killed outcome, got {:?}",
        report.outcome
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "process-group kill should complete within timeout+grace+slack, got {:?}",
        elapsed
    );

    // THE actual contract: the forked grandchild was reaped, not left running.
    let reaped = wait_until_gone(pid, Duration::from_secs(5));

    // Cleanup so a regression doesn't leak a 60s sleep into the test host.
    if !reaped {
        let _ = Command::new("kill")
            .arg("-9")
            .arg(pid.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    assert!(
        reaped,
        "grandchild `sleep 60` (pid {pid}) must be reaped by the process-group \
         kill, but it was still alive after the watcher reported a killed outcome \
         — only the `sh` child was killed (ADR-027 D7 regression)"
    );
}
