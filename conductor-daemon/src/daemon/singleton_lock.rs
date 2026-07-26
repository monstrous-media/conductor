// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! ADR-034 §D10 / §D4.B.2 — singleton daemon enforcement via
//! `flock(2)`.
//!
//! Acquired BEFORE IPC socket bind or config load in
//! `service::run_daemon_with_config`. Held for the daemon's lifetime
//! in an `Arc<SingletonLock>` that's dropped on shutdown — POSIX
//! `flock(2)` also releases on process exit (including SIGKILL) so
//! a crashed or kill-9'd daemon doesn't strand the lock.
//!
//! # Why `flock(2)` not `fcntl(F_SETLK)`?
//!
//! Per-process semantics matter here. `fcntl` POSIX locks are
//! per-process per-inode: if a single process opens the lockfile
//! twice and locks both fds, the second lock silently succeeds (no
//! conflict between the same process's own locks). `flock(2)` BSD
//! locks are per-OFD (open file description): two opens of the same
//! file get distinct OFDs, the second `flock` blocks/fails as
//! expected. For daemon-singleton enforcement we want the
//! per-OFD semantics — every distinct opener (different daemon
//! processes) must contend.
//!
//! # Cross-platform note
//!
//! `flock(2)` is Unix only. This module is `#[cfg(unix)]`. Windows
//! daemon hosting isn't supported today; if it becomes a goal,
//! `LockFileEx` is the equivalent.

#![cfg(unix)]

use nix::fcntl::{Flock, FlockArg};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Errors from [`SingletonLock::acquire`].
#[derive(Debug, Error)]
pub enum SingletonLockError {
    /// Another daemon process already holds the lock.
    ///
    /// Service layer maps this to `std::process::exit(EX_TEMPFAIL = 75)`
    /// so systemd / launchd treats it as "transient, try again" rather
    /// than "this unit is broken, stop restarting".
    #[error("singleton lock contention on {path}: another daemon already running")]
    Contention { path: PathBuf },

    /// I/O failure opening or fsyncing the lockfile.
    #[error("singleton lock I/O on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

// `Debug` so tests can `panic!("{other:?}")` on the Result variant
// without bespoke matching. `Flock<File>` doesn't implement Debug, so
// derive(Debug) would fail — hand-roll a minimal `Debug` that prints
// the path only.

/// RAII guard for the daemon-singleton flock.
///
/// Held by `service::run_daemon_with_config` for the daemon's
/// lifetime. Drop releases the lock (POSIX `flock` releases on
/// close of the last OFD reference). The lockfile itself is NOT
/// removed on drop — leaving it in place is fine: subsequent
/// daemons re-open it and re-acquire the flock; the file's presence
/// is just an idle marker between runs.
pub struct SingletonLock {
    /// Owns the locked fd. Drop releases the flock via `close`.
    /// `nix::fcntl::Flock` is the RAII wrapper around the file +
    /// the active flock; dropping it issues `flock(fd, LOCK_UN)`
    /// explicitly (cleaner than relying on close-releases-flock).
    _flock: Flock<File>,
    /// Path retained for diagnostics — useful in error messages
    /// when contention happens far from the acquire site.
    #[allow(dead_code)]
    path: PathBuf,
}

impl std::fmt::Debug for SingletonLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SingletonLock")
            .field("path", &self.path)
            .field("_flock", &"<held>")
            .finish()
    }
}

impl SingletonLock {
    /// Acquire the singleton lock on `lockfile_path`.
    ///
    /// Creates the file if absent (mode 0600 per ADR-034
    /// §"Persistence file layout"). Calls `flock(LOCK_EX | LOCK_NB)`
    /// — exclusive, non-blocking — so contention surfaces as
    /// [`SingletonLockError::Contention`] rather than a hang.
    ///
    /// The caller (service layer) is expected to:
    /// 1. Resolve the lockfile path via
    ///    `LivePaths::from_env().unwrap().lockfile`.
    /// 2. Ensure the parent directory exists (also handled by
    ///    `LivePaths::ensure_dir()`).
    /// 3. Map `Contention` to `process::exit(75)` (EX_TEMPFAIL)
    ///    with a log line; map `Io` to a fatal startup error.
    pub fn acquire(lockfile_path: &Path) -> Result<Self, SingletonLockError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(lockfile_path)
            .map_err(|e| SingletonLockError::Io {
                path: lockfile_path.to_path_buf(),
                source: e,
            })?;

        // `Flock::lock(file, NonblockingExclusive)` issues
        // `flock(LOCK_EX | LOCK_NB)` and returns the locked file
        // wrapped for RAII unlock. On contention nix surfaces
        // `EWOULDBLOCK` as a `(File, Errno)` tuple in the Err
        // branch — we discard the file (close releases nothing
        // since we never acquired), wrapping the error as
        // `Contention` for callers.
        Flock::lock(file, FlockArg::LockExclusiveNonblock)
            .map(|flock| Self {
                _flock: flock,
                path: lockfile_path.to_path_buf(),
            })
            .map_err(|(_unlocked_file, errno)| {
                if errno == nix::errno::Errno::EWOULDBLOCK {
                    SingletonLockError::Contention {
                        path: lockfile_path.to_path_buf(),
                    }
                } else {
                    // Council #1297 round 1: prefer the idiomatic
                    // `Errno -> io::Error` conversion over manual
                    // `from_raw_os_error(errno as i32)`. nix's
                    // `From<Errno> for io::Error` does the right
                    // thing including preserving the errno kind
                    // mapping (e.g. ENOENT → ErrorKind::NotFound).
                    SingletonLockError::Io {
                        path: lockfile_path.to_path_buf(),
                        source: io::Error::from(errno),
                    }
                }
            })
    }
}
