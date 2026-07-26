// ADR-034 §D10 / §D4.B.2 — singleton enforcement via `flock(2)`.
//
// The daemon acquires `flock(LOCK_EX | LOCK_NB)` on
// `$XDG_STATE_HOME/conductor/conductor.lock` BEFORE IPC socket bind
// or config load. On contention: log + exit `EX_TEMPFAIL = 75`.
// Held for daemon lifetime; POSIX `flock` releases automatically on
// process exit (including SIGKILL).
//
// These tests exercise the lock primitive directly. Service-level
// integration (acquire-before-bind, exit-75-on-contention) is covered
// by the service-startup tests landing alongside B.2.

#![cfg(unix)]

use conductor_daemon::daemon::singleton_lock::{SingletonLock, SingletonLockError};
use tempfile::TempDir;

#[test]
fn acquire_succeeds_on_unowned_lockfile() {
    let dir = TempDir::new().unwrap();
    let lockfile = dir.path().join("conductor.lock");

    let lock = SingletonLock::acquire(&lockfile).expect("first acquire");
    drop(lock);
    // Lock file is created by the acquire, but we don't assert
    // anything about whether it remains — `flock` only locks, not
    // the file's lifecycle.
    assert!(lockfile.exists(), "lockfile should be created");
}

#[test]
fn second_acquire_returns_contention_error() {
    // The whole point: a second concurrent daemon attempting to
    // acquire the same lock must see `Contention` rather than block
    // (LOCK_NB) or succeed (would-be-doubly-started daemon).
    let dir = TempDir::new().unwrap();
    let lockfile = dir.path().join("conductor.lock");

    let _first = SingletonLock::acquire(&lockfile).expect("first acquire");
    let second = SingletonLock::acquire(&lockfile);
    match second {
        Err(SingletonLockError::Contention { .. }) => {} // ok
        other => panic!("expected Contention, got: {other:?}"),
    }
}

#[test]
fn dropping_lock_releases_it_for_a_subsequent_acquire() {
    // Closing the fd via Drop releases the BSD flock — a fresh
    // process (or in-process second `acquire`) must then succeed.
    let dir = TempDir::new().unwrap();
    let lockfile = dir.path().join("conductor.lock");

    let first = SingletonLock::acquire(&lockfile).expect("first acquire");
    drop(first);

    let second = SingletonLock::acquire(&lockfile).expect("second acquire after drop");
    drop(second);
}

// Env vars that switch this test binary into "child lock-holder" mode when the
// cross-process test re-execs itself (see `lock_released_when_subprocess_exits_normally`).
// Set by the parent; absent in a normal test run.
const LOCK_CHILD_ENV: &str = "CONDUCTOR_SINGLETON_LOCK_CHILD";
const LOCK_PATH_ENV: &str = "CONDUCTOR_SINGLETON_LOCK_PATH";
const LOCK_READY_ENV: &str = "CONDUCTOR_SINGLETON_LOCK_READY";

#[test]
fn lock_released_when_subprocess_exits_normally() {
    // Real-world "second daemon can start after the first exits" check across
    // two distinct OS processes. Council #1297 round 1 flagged an earlier
    // version as vacuous (it spawned `true` without touching the lock).
    //
    // #1552: the next version used a `python3` one-liner as the lock holder and
    // SILENTLY `return`ed — passing with zero assertions — when python3 was
    // absent. So a CI configuration without python3 reported this cross-process
    // coverage as passing while running none of it. This version re-execs the
    // test binary itself as the child holder, using the REAL `SingletonLock`
    // type (stronger than python's raw `fcntl.flock`) and no external
    // dependency, so the cross-process path ALWAYS runs on every Unix CI.
    //
    // Handshake is a filesystem sentinel (created only after the child's
    // `acquire` succeeds), not stdout — libtest's own output on the child's
    // stdout therefore cannot corrupt it, and there are no `sleep`-based races.
    use std::io::Read;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    // ---- Child mode: hold the lock until stdin closes, then exit. ----
    if std::env::var_os(LOCK_CHILD_ENV).is_some() {
        // Read paths via `var_os` (not `var`) so a non-UTF-8 OS path — valid on
        // Unix — round-trips faithfully instead of spuriously failing.
        let lock_path =
            PathBuf::from(std::env::var_os(LOCK_PATH_ENV).expect("child: lock path env"));
        let ready_path =
            PathBuf::from(std::env::var_os(LOCK_READY_ENV).expect("child: ready path env"));
        // Hold the REAL production lock for our lifetime. `_held` (named, not
        // bare `_`) keeps the guard alive; its `Drop` runs `flock(LOCK_UN)` when
        // this function returns below.
        let _held = SingletonLock::acquire(&lock_path).expect("child must acquire the lock");
        // Signal the parent that the lock is now held — created only AFTER a
        // successful acquire, so the parent never races ahead of the child.
        std::fs::write(&ready_path, b"ok").expect("child: write ready marker");
        // Block until the parent closes our stdin, then RETURN (not
        // `process::exit`) so the `_held` guard's `Drop` releases the flock
        // deterministically via `flock(LOCK_UN)`, rather than relying on the
        // kernel releasing it when process exit closes the fd (which would skip
        // RAII cleanup).
        let mut sink = Vec::new();
        let _ = std::io::stdin().read_to_end(&mut sink);
        return;
    }

    // ---- Parent mode. ----
    let dir = TempDir::new().unwrap();
    let lockfile = dir.path().join("conductor.lock");
    let ready_marker = dir.path().join("child-holds-lock");

    let exe = std::env::current_exe().expect("path to this test binary");
    let mut child = Command::new(exe)
        // Run ONLY this test in the child; the env guard above makes it act as
        // the lock holder instead of recursing into the parent logic.
        .args([
            "--exact",
            "lock_released_when_subprocess_exits_normally",
            "--test-threads=1",
        ])
        .env(LOCK_CHILD_ENV, "1")
        .env(LOCK_PATH_ENV, &lockfile)
        .env(LOCK_READY_ENV, &ready_marker)
        .stdin(Stdio::piped())
        // Null the child's stdout (libtest banner noise) but INHERIT stderr so a
        // child panic (e.g. a failed acquire) surfaces in CI output rather than
        // leaving the parent with only an opaque exit status.
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn child lock-holder (this test binary)");

    // Wait for the deterministic handshake: the child creates `ready_marker`
    // only after it has acquired the lock. Bounded so a stuck child fails the
    // test loudly instead of hanging forever.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready_marker.exists() {
        if let Some(status) = child
            .try_wait()
            .expect("poll child while waiting for ready marker")
        {
            panic!("child exited before signaling lock-held readiness: {status:?}");
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("child did not acquire the lock within 10s (no ready marker)");
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    // While the child holds the lock, a parent-side acquire must contend. This
    // is the production-equivalent check: two distinct OS processes opening the
    // same file, the second `flock` fails.
    let contended = SingletonLock::acquire(&lockfile);
    assert!(
        matches!(contended, Err(SingletonLockError::Contention { .. })),
        "expected Contention while the subprocess holds the lock, got: {contended:?}"
    );

    // Closing the child's stdin sends EOF → child returns from read → exits →
    // the kernel releases the flock.
    drop(child.stdin.take().expect("child stdin"));
    let status = child.wait().expect("wait for child");
    assert!(status.success(), "child exited with failure: {status:?}");

    // Now the parent acquire must succeed. Retry briefly to avoid a flaky race
    // where `wait()` has returned but the lock release is not yet observable.
    let reacquire_deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match SingletonLock::acquire(&lockfile) {
            Ok(lock_after) => {
                drop(lock_after);
                break;
            }
            Err(SingletonLockError::Contention { .. }) if Instant::now() < reacquire_deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(other) => panic!("acquire after subprocess exit must succeed, got: {other:?}"),
        }
    }
}
