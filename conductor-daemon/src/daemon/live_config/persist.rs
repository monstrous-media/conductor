// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-034 §D3.1 — atomic write recipe for the daemon-managed
//! canonical config files (`live.toml`, `live.toml.known_good`).
//!
//! # Recipe
//!
//! 1. `tempfile::NamedTempFile::new_in(target.parent())` — same FS
//!    as target so the rename is atomic (cross-FS rename would fall
//!    back to copy+delete and lose atomicity).
//! 2. `write_all(bytes)` — buffered write.
//! 3. `tmp.as_file().sync_all()` — file data + metadata durable.
//! 4. `tmp.persist(target)` — atomic rename to the canonical path.
//! 5. `File::open(dir)?.sync_all()` — directory entry durable so a
//!    crash doesn't leave the rename invisible after fsck.
//!
//! # Concurrency
//!
//! Wrapped in `tokio::task::spawn_blocking` per R4-M6: file I/O on
//! the tokio runtime would block the executor; offloading to a
//! blocking worker preserves async throughput on the commit path.
//! Only one writer at a time enters this function (gated by
//! `LiveConfig::mutate_lock`), so there's no cross-write contention
//! at the filesystem level.

use std::fs::File;
use std::io::{self, Write};
use std::path::Path;

/// Atomically persist `canonical_bytes` to `target`.
///
/// `target.parent()` must exist; the caller is responsible for
/// `mkdir_p` during startup (see [`super::paths::LivePaths::ensure_dir`]).
///
/// Errors:
/// - `io::Error` if the tempfile can't be created in `target.parent()`
///   (most commonly: parent doesn't exist, or disk full).
/// - `io::Error` if `sync_all` fails on the tempfile or parent dir.
/// - `io::Error` if `persist` (rename) fails — usually a permissions
///   issue or cross-filesystem path.
/// - `io::Error` if the `spawn_blocking` task panics or is cancelled.
pub async fn persist_atomically(target: &Path, canonical_bytes: &[u8]) -> io::Result<()> {
    let target = target.to_path_buf();
    let bytes = canonical_bytes.to_vec();
    tokio::task::spawn_blocking(move || -> io::Result<()> {
        let dir = target.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("target has no parent directory: {}", target.display()),
            )
        })?;
        let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
        tmp.write_all(&bytes)?;
        // Explicit 0600 per ADR-034 §"Persistence file layout".
        // `tempfile::NamedTempFile::new_in` already defaults to 0600 on
        // Unix (it uses `O_CREAT|O_EXCL|O_RDWR` with mode 0o600), but
        // setting it explicitly here is defensive against future
        // tempfile-crate default changes and documents the contract
        // at the persist boundary (Council #1293 round 2 spec
        // compliance fix). No-op on Windows where permissions are ACL
        // based.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tmp.as_file()
                .set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        tmp.as_file().sync_all()?;
        // `persist` consumes the NamedTempFile and renames to `target`.
        // On Unix this is `rename(2)` — atomic on the same filesystem.
        // The renamed file inherits the tempfile's 0600 mode.
        tmp.persist(&target)
            .map_err(|persist_err| persist_err.error)?;
        // Directory fsync so the rename is durable across crash.
        // On macOS HFS+ this is a no-op for directories; on Linux
        // ext4/xfs it materialises the rename's metadata. Failure
        // here is rare but worth surfacing.
        File::open(dir)?.sync_all()?;
        Ok(())
    })
    .await
    .map_err(|join_err| {
        // `JoinError` distinguishes panic vs cancellation —
        // surface that so an operator triaging from logs knows
        // whether to look for a panic backtrace (panic) or a
        // task-shutdown event (cancellation). `is_panic()` /
        // `is_cancelled()` cover all variants per the tokio
        // docs (Council #1293 round 1 fix — prior code collapsed
        // both into a flat string and lost the diagnostic).
        let kind = if join_err.is_panic() {
            "panicked"
        } else if join_err.is_cancelled() {
            "cancelled"
        } else {
            "ended unexpectedly"
        };
        io::Error::other(format!(
            "persist_atomically spawn_blocking {kind}: {join_err}"
        ))
    })?
}
