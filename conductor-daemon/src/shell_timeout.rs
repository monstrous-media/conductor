// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Shell action timeout enforcement — ADR-027 D7.
//!
//! Long-running shell actions historically ran as fire-and-forget
//! children. A misbehaved action could pin a process slot indefinitely
//! and (transitively) prevent the daemon from cleanly shutting down.
//! D7 closes that gap by spawning every shell action under a watchdog
//! that escalates SIGTERM → grace window → SIGKILL.
//!
//! On Unix, the child is placed in its own process group
//! (`setpgid(0, 0)`) so the kill targets the entire tree — shell
//! actions like `sh -c "sleep 30 & wait"` would otherwise leak the
//! grandchild.
//!
//! On Windows we fall back to `Child::kill()` (no SIGTERM analogue);
//! the grace window is effectively wasted but the watchdog still runs.

use std::io::{self, Read};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Default timeout when an action carries no explicit `timeout_ms`.
/// Hardcoded at 30s for the initial cut; a per-action override
/// rides on the `timeout_ms` schema field shipped alongside.
pub const DEFAULT_SHELL_TIMEOUT_MS: u64 = 30_000;

/// SIGTERM → SIGKILL grace window. Long enough for a child that
/// installs a SIGTERM handler to flush state, short enough that a
/// runaway child doesn't pin the daemon for a perceptible delay.
pub const SHELL_TIMEOUT_GRACE_MS: u64 = 5_000;

/// ADR-027 D7 — bounded stdout/stderr capture cap.
/// Each stream is captured up to this many bytes; reader threads
/// continue draining past the cap so the child doesn't block on a
/// full pipe (the OS pipe buffer is typically 64 KB; a deny here
/// would silently stall any shell action that prints more than that).
pub const CAPTURE_CAP_BYTES: usize = 4 * 1024;

/// How often the watcher polls `Child::try_wait`. 100ms keeps the
/// overhead invisible (watchdog wakes ~10×/s) without slop on the
/// timeout boundary.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Outcome reported by the watcher thread.
#[derive(Debug)]
pub enum WatcherOutcome {
    /// Child finished on its own before the timeout fired.
    Completed(ExitStatus),
    /// Timeout hit — SIGTERM (or `Child::kill()` on Windows) cleaned
    /// the child up within the grace window.
    KilledOnTimeout,
    /// Timeout hit — SIGTERM was ignored, SIGKILL escalation reaped
    /// the child after the grace window.
    KilledAfterGrace,
    /// `Child::wait` / `try_wait` returned an OS-level error.
    WaitError(io::Error),
}

/// Captured stdout/stderr from the child, bounded at
/// `CAPTURE_CAP_BYTES` per stream. If the child emitted more than
/// the cap, the corresponding `*_truncated` flag is `true` and the
/// `Vec<u8>` holds only the leading `CAPTURE_CAP_BYTES` bytes; the
/// reader thread continued draining past that point to keep the
/// child from stalling on a full pipe.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CapturedOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

/// Report produced by the watcher thread — pairs the lifecycle
/// [`WatcherOutcome`] with the bounded [`CapturedOutput`].
/// Tests `.join()` the watcher to inspect this; the production
/// fire-and-forget path drops the join handle (the watcher logs
/// captured output via `tracing` before returning).
#[derive(Debug)]
pub struct WatcherReport {
    pub outcome: WatcherOutcome,
    pub captured: CapturedOutput,
}

/// Result of [`spawn_with_timeout`]. The watcher thread is detached
/// for the production fire-and-forget path; tests `.join()` it to
/// observe the outcome and any captured output.
pub struct SpawnResult {
    pub pid: u32,
    pub watcher: JoinHandle<WatcherReport>,
}

/// Spawn `command` and return immediately with a watcher thread that
/// enforces `timeout` + `grace`. The caller may drop the
/// `JoinHandle` to detach (fire-and-forget) or `.join()` it to await
/// the outcome.
///
/// Unix: the child is placed in a fresh process group so signals
/// target the whole tree.
pub fn spawn_with_timeout(
    mut command: Command,
    timeout: Duration,
    grace: Duration,
) -> io::Result<SpawnResult> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Fresh process group. `0` means "use the child's pid as the
        // pgid", so a later `kill(-pid, SIGTERM)` reaps the tree.
        command.process_group(0);
    }

    // Wire stdout/stderr as pipes so the watcher thread can
    // capture (bounded at CAPTURE_CAP_BYTES). Without this, the
    // child inherits the daemon's stdio and capture would have
    // nowhere to read from.
    command.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = command.spawn()?;
    let pid = child.id();
    // SAFETY (of unwrap): we just configured both streams as
    // `Stdio::piped()`, so `Child::stdout` / `Child::stderr` are
    // guaranteed `Some` here.
    let stdout = child.stdout.take().expect("stdout was Stdio::piped");
    let stderr = child.stderr.take().expect("stderr was Stdio::piped");

    let watcher =
        std::thread::spawn(move || run_watcher(child, pid, timeout, grace, stdout, stderr));

    Ok(SpawnResult { pid, watcher })
}

/// Drain a child's pipe end into a capped buffer. Returns
/// the captured prefix plus a `truncated` flag. After the cap is
/// reached, subsequent reads are discarded so the pipe keeps
/// flowing and the child doesn't block on a write-side stall.
fn drain_capped<R: Read>(mut reader: R) -> (Vec<u8>, bool) {
    let mut captured = Vec::with_capacity(CAPTURE_CAP_BYTES);
    let mut truncated = false;
    let mut chunk = [0u8; 1024];

    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break, // EOF — child closed its end
            Ok(n) => {
                if captured.len() < CAPTURE_CAP_BYTES {
                    let remaining = CAPTURE_CAP_BYTES - captured.len();
                    let take = n.min(remaining);
                    captured.extend_from_slice(&chunk[..take]);
                    if n > remaining {
                        truncated = true;
                    }
                } else {
                    // Already at cap — keep draining (discard) so
                    // the child's write side never blocks.
                    truncated = true;
                }
            }
            Err(e) => {
                // EAGAIN / EWOULDBLOCK shouldn't appear on a
                // blocking pipe read; anything else means the pipe
                // is broken (child died, fd closed). Either way,
                // we're done capturing — log and stop.
                tracing::debug!("shell_timeout: pipe read ended: {e}");
                break;
            }
        }
    }

    (captured, truncated)
}

fn run_watcher<O: Read + Send + 'static, E: Read + Send + 'static>(
    mut child: Child,
    pid: u32,
    timeout: Duration,
    grace: Duration,
    stdout: O,
    stderr: E,
) -> WatcherReport {
    // Spawn reader threads BEFORE the kill/timeout loop. The
    // readers run concurrently with the lifecycle watcher and
    // finish on their own (EOF when the child closes its
    // end — either naturally or after a kill).
    let stdout_thread = std::thread::spawn(move || drain_capped(stdout));
    let stderr_thread = std::thread::spawn(move || drain_capped(stderr));

    let outcome = run_watcher_lifecycle(&mut child, pid, timeout, grace);

    // Joining the reader threads waits until they hit EOF — which
    // happens once `child.wait` (or kill) has closed the pipe ends.
    // The pre-kill outcome arms that DON'T explicitly wait can
    // still rely on EOF arriving because the kernel closes pipes
    // when the child process exits.
    let (stdout_buf, stdout_truncated) = stdout_thread.join().unwrap_or((Vec::new(), false));
    let (stderr_buf, stderr_truncated) = stderr_thread.join().unwrap_or((Vec::new(), false));

    let captured = CapturedOutput {
        stdout: stdout_buf,
        stderr: stderr_buf,
        stdout_truncated,
        stderr_truncated,
    };

    // Surface captured output via tracing so operators have
    // a record alongside the lifecycle outcome. The audit-log
    // integration item is deferred: the underlying
    // Shell-action audit entry doesn't exist yet (none of the
    // current audit-log API surface covers configured-mapping
    // shell actions — only LLM-tool invocations). Filed as
    // follow-up; tracing covers the immediate "operator can see
    // what the child printed" requirement.
    if !captured.stdout.is_empty() || captured.stdout_truncated {
        tracing::info!(
            pid,
            bytes = captured.stdout.len(),
            truncated = captured.stdout_truncated,
            "shell action stdout captured"
        );
    }
    if !captured.stderr.is_empty() || captured.stderr_truncated {
        tracing::info!(
            pid,
            bytes = captured.stderr.len(),
            truncated = captured.stderr_truncated,
            "shell action stderr captured"
        );
    }

    WatcherReport { outcome, captured }
}

/// Lifecycle-only arm of the watcher (split out for clarity now
/// that the parent thread also coordinates capture readers).
/// Returns the [`WatcherOutcome`] after either natural completion
/// or escalation through SIGTERM → grace → SIGKILL.
fn run_watcher_lifecycle(
    child: &mut Child,
    pid: u32,
    timeout: Duration,
    grace: Duration,
) -> WatcherOutcome {
    let start = Instant::now();

    while start.elapsed() < timeout {
        match child.try_wait() {
            Ok(Some(status)) => return WatcherOutcome::Completed(status),
            Ok(None) => std::thread::sleep(POLL_INTERVAL),
            Err(e) => return WatcherOutcome::WaitError(e),
        }
    }

    // Timeout exceeded — send the polite signal first.
    send_term(child, pid);

    let grace_start = Instant::now();
    while grace_start.elapsed() < grace {
        match child.try_wait() {
            Ok(Some(_)) => return WatcherOutcome::KilledOnTimeout,
            Ok(None) => std::thread::sleep(POLL_INTERVAL),
            Err(e) => {
                // Don't short-circuit out of
                // the grace loop on a transient `try_wait` error.
                // ECHILD means the child is already reaped (safe),
                // anything else likely means SIGKILL escalation is
                // still appropriate. Log and proceed to the
                // unconditional kill below rather than leaving a
                // possibly-live runaway child orphaned.
                tracing::warn!(
                    "shell_timeout: grace-loop try_wait error (pid {}): {}; \
                     escalating to SIGKILL",
                    pid,
                    e
                );
                break;
            }
        }
    }

    // Grace exhausted — escalate to the unblockable signal.
    send_kill(child, pid);

    match child.wait() {
        Ok(_) => WatcherOutcome::KilledAfterGrace,
        Err(e) => WatcherOutcome::WaitError(e),
    }
}

/// Saturating cast from `u32` pid to a negative-`i32` process-group
/// target for `libc::kill`. Linux's `pid_max` is bounded to 2²² so
/// the saturating arm is defensive — but it makes the intent
/// (target the whole pgid, never wrap) self-documenting.
#[cfg(unix)]
fn pgid_target(pid: u32) -> i32 {
    -(pid.min(i32::MAX as u32) as i32)
}

#[cfg(unix)]
fn send_term(_child: &mut Child, pid: u32) {
    // `libc::kill` returns -1 on ESRCH if the
    // process group is already gone. The subsequent `try_wait` /
    // `wait` correctly handles that case, so the return is
    // intentionally discarded — make it explicit so future
    // `-D unused-must-use` passes don't trip.
    let pgid = pgid_target(pid);
    unsafe {
        let _ = libc::kill(pgid, libc::SIGTERM);
    }
}

#[cfg(unix)]
fn send_kill(_child: &mut Child, pid: u32) {
    let pgid = pgid_target(pid);
    unsafe {
        let _ = libc::kill(pgid, libc::SIGKILL);
    }
}

#[cfg(windows)]
fn send_term(child: &mut Child, _pid: u32) {
    // No SIGTERM analogue — terminate immediately. Child::kill maps
    // to TerminateProcess which is unblockable.
    let _ = child.kill();
}

#[cfg(windows)]
fn send_kill(child: &mut Child, _pid: u32) {
    let _ = child.kill();
}
