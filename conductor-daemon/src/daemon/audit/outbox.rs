// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-034 §D8 — two-phase durable audit outbox (append-only file).
//!
//! The outbox is a separate file from the SQLite audit DB
//! (`$XDG_STATE_HOME/conductor/audit-outbox.log`) so audit durability survives
//! a SQLite outage. Each entry is one line of JSON forming a hash chain
//! (§D8.4); a flusher later moves applied entries into SQLite (sub-slice B).
//!
//! This module is the file primitive — append/read/verify, the §D8.2 at-cap
//! refusal in [`AuditOutbox::enqueue_pending`], and the §D8 sub-slice B2
//! rewrite-from-genesis compaction in [`AuditOutbox::compact_retaining`]. The
//! `mutate()` flow consumes it; the SQLite flusher *task* that drives
//! compaction and the full `AuditDegraded` lifecycle are follow-ups. It performs
//! **no wall-clock reads** (`created_at` is caller-supplied) so it stays
//! deterministic and unit-testable.
//!
//! **Write semantics (§D8.1, R5-M4):** each entry is appended with
//! `open(O_WRONLY | O_APPEND | O_NOFOLLOW | O_CLOEXEC, mode 0600) → single
//! write(line + "\n") → fdatasync → close`. `OpenOptions::append(true)` sets
//! `O_APPEND` (the kernel positions every write at EOF atomically); the standard
//! library sets `O_CLOEXEC` on Unix; `O_NOFOLLOW` refuses a symlinked outbox
//! path and `mode(0o600)` keeps it owner-only (§D3.3 — it can carry
//! provenance); [`std::fs::File::sync_data`] is `fdatasync` on Linux (data-only
//! durability, no metadata) — exactly the spec's requirement.
//!
//! **Reader tolerance (§D8.1):** a SIGKILL between `write` and `fdatasync` can
//! leave the final line truncated. The reader tolerates a parse error on the
//! **last** line only (warn + skip); a parse error or chain break **anywhere
//! else** is fatal — it indicates corruption or tampering (§D8.4).

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::hash_chain::{GENESIS_PREV_HASH, compute_entry_hash};

/// Outbox size cap (§D8.1): 4096 entries (~4 MB at typical row sizes). At cap,
/// [`AuditOutbox::enqueue_pending`] refuses a new `Pending` row (`AtCap`) so the
/// daemon fail-closes config mutations (§D8.2); resolution markers stay allowed.
pub const OUTBOX_CAP: usize = 4096;

/// Two-phase (plus failure) lifecycle of an outbox row (§D8.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboxPhase {
    /// Enqueued before persist (`mutate()` step 12), fsync'd before persist
    /// returns.
    Pending,
    /// Marked after the new snapshot was published (`mutate()` step 15).
    Applied,
    /// Marked when a mutation failed after the pending enqueue (rollback).
    Failed,
    /// ADR-034 §D8 — a meta-event, NOT a mutation: the operator ran
    /// `conductorctl audit resume`, which rotated a corrupt outbox aside and
    /// started this fresh chain. Written as the FIRST record of the new file so
    /// the integrity break is attested (provenance carries the operator, the
    /// rotated filename, the last valid hash of the old chain, and the reason).
    /// Reconciliation skips it — it never pairs with a Pending row.
    ChainReset,
}

/// The content of an outbox row — every field the hash chain covers.
/// `prev_hash` / `entry_hash` are bookkeeping and are deliberately **excluded**
/// from the canonical bytes (mirroring [`super::hash_chain`]'s `CanonicalRow`):
/// including them would be self-referential.
///
/// `deny_unknown_fields`: an extra JSON key injected into a row would otherwise
/// be dropped on deserialize and never enter `canonical_bytes`, so it would be
/// invisible to the hash chain — undetectable tampering. Rejecting unknown
/// fields makes any injected key a fatal parse error instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutboxRecord {
    /// Id of the audit entry / mutation this row refers to. A `mark_*` row
    /// carries the same `id` as the `Pending` row it resolves.
    pub id: String,
    /// Two-phase lifecycle marker.
    pub phase: OutboxPhase,
    /// Serialized `Provenance` JSON (Source / Initiator), when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<String>,
    /// For a `Pending` row: the config revision the mutation intends to
    /// produce. Startup reconciliation (§D8.3, sub-slice C) compares it against
    /// `live.toml`'s hash to decide whether the publish completed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intended_revision: Option<String>,
    /// Unix-epoch milliseconds the row was created. Caller-supplied — this
    /// module performs no wall-clock reads.
    pub created_at: i64,
}

/// A persisted outbox entry: the [`OutboxRecord`] plus its chain hashes.
/// Serialized as one JSON line: `{ "record": { … }, "prev_hash": …,
/// "entry_hash": … }`.
///
/// The record is a **nested** object (not `#[serde(flatten)]`): flatten is
/// incompatible with `deny_unknown_fields`, and without that guard an attacker
/// could inject extra top-level keys that the hash chain would not cover.
/// Nesting + `deny_unknown_fields` on both levels makes every
/// byte of the line either hash-covered or a fatal parse error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutboxEntry {
    /// The hashed content.
    pub record: OutboxRecord,
    /// sha256 chain link to the previous entry (`GENESIS_PREV_HASH` for the
    /// first entry).
    pub prev_hash: String,
    /// `compute_entry_hash(prev_hash, canonical_bytes(record))`.
    pub entry_hash: String,
}

/// Failure reading or appending to the outbox.
#[derive(Debug)]
pub enum OutboxError {
    /// Underlying filesystem I/O error.
    Io(std::io::Error),
    /// A non-final line failed to parse, or the hash chain is discontinuous —
    /// corruption or tampering (§D8.1 / §D8.4). Fatal.
    Corruption(String),
    /// The outbox is at its §D8.1 cap, so a new `Pending` row is refused
    /// atomically (§D8.2). Resolution markers (`mark_applied`/`mark_failed`) are
    /// NOT cap-blocked — they drain the backlog. Caller maps this to a
    /// fail-closed `AuditUnavailable`.
    AtCap,
}

impl std::fmt::Display for OutboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "audit outbox I/O error: {e}"),
            Self::Corruption(m) => write!(f, "audit outbox corruption: {m}"),
            Self::AtCap => write!(f, "audit outbox is at capacity"),
        }
    }
}

impl std::error::Error for OutboxError {}

/// Canonical bytes hashed for an entry: the record serialized as JSON, with
/// `prev_hash`/`entry_hash` excluded by construction. serde_json emits struct
/// fields in declaration order, so the bytes are stable across calls.
fn canonical_bytes(record: &OutboxRecord) -> Vec<u8> {
    serde_json::to_vec(record).expect(
        "OutboxRecord is a fixed primitive-only shape; serde_json serialization \
         cannot fail here except on OOM/programmer error — refusing to hash \
         empty bytes (that would silently weaken the chain).",
    )
}

/// Append one line with the §D8.1 durable-write recipe:
/// `O_WRONLY | O_APPEND | O_NOFOLLOW | O_CLOEXEC` open → single
/// `write(line + "\n")` → `fdatasync` → close.
///
/// - **`mode(0o600)`** (§D3.3): the outbox can hold provenance and is created
///   owner-only, never world-readable under a permissive umask.
/// - **`O_NOFOLLOW`**: refuse a symlinked outbox path (defense in depth — the
///   file lives in the daemon's trusted state dir, but it must not be
///   redirectable via a final-component symlink).
/// - **single `write`**: `line + "\n"` is written from one buffer. `write_all`
///   may in principle loop on a short write, but entries are well under
///   `PIPE_BUF` (4 KiB) and appends are serialised by the caller's mutate lock,
///   so the buffer is written in one `write(2)` and `O_APPEND`'s atomic-at-EOF
///   guarantee holds — the entry and its terminator are never torn apart.
fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mut buf = String::with_capacity(line.len() + 1);
    buf.push_str(line);
    buf.push('\n');

    let mut file = OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    // `mode(0o600)` only applies when the file is CREATED. Enforce 0600 on the
    // open fd unconditionally so a pre-existing file with looser perms (e.g.
    // 0644 pre-created by another process) is tightened to owner-only (§D3.3).
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(buf.as_bytes())?;
    // `sync_data` == fdatasync on Linux: file data is durable, metadata is not
    // forced (§D8.1 — only data needs durability). File closes on drop.
    file.sync_data()?;
    Ok(())
}

/// Hard ceiling on the bytes [`read_entries`] will load (§D8.1 DoS guard):
/// `OUTBOX_CAP` entries × a generous 4 KiB/entry = 16 MiB. A legitimate outbox
/// is ~4 MiB; anything larger is corruption or an attempt to exhaust memory.
pub const MAX_OUTBOX_BYTES: u64 = OUTBOX_CAP as u64 * 4096;

/// Read all entries from the outbox at `path`, verifying the hash chain.
///
/// - A missing file is an empty outbox (`Ok(vec![])`).
/// - The file is opened `O_NOFOLLOW` (a symlinked outbox path is rejected,
///   matching the append path) and the read is capped at [`MAX_OUTBOX_BYTES`]
///   so a giant/attacker-grown file cannot exhaust memory.
/// - A parse error on the **last** line is tolerated (truncated final write):
///   warn + skip, return the entries before it.
/// - A parse error on any **earlier** line, or any hash-chain discontinuity, is
///   [`OutboxError::Corruption`] (fatal).
pub fn read_entries(path: &Path) -> Result<Vec<OutboxEntry>, OutboxError> {
    read_entries_with_valid_len(path).map(|(entries, _valid_bytes)| entries)
}

/// Like [`read_entries`], but also returns the cumulative byte length of the
/// **newline-terminated, chain-validated** prefix — the offset past the last
/// fully-committed entry. [`AuditOutbox::open`] uses this to truncate any
/// tolerated torn trailing bytes from disk (§D8.1) before an `O_APPEND`
/// write can glue a new entry onto a partial line.
///
/// A record is "committed" only once its terminating `\n` is on disk: the append
/// path writes `line + "\n"` in a single `write(2)`, so a torn write leaves an
/// unparseable partial line (skipped). A complete line that is missing only its
/// trailing newline (the final segment of a file that does not end in `\n`) is
/// likewise treated as un-committed and skipped, keeping the returned entries
/// and `valid_bytes` exactly consistent with the on-disk durable prefix.
fn read_entries_with_valid_len(path: &Path) -> Result<(Vec<OutboxEntry>, u64), OutboxError> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;

    // `O_NOFOLLOW`: refuse a symlinked outbox path (consistent with the append
    // path) rather than reading through it.
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), 0)),
        Err(e) => return Err(OutboxError::Io(e)),
    };
    // DoS guard: bound the load by the file's declared size, rejecting anything
    // over the cap before allocating.
    let len = file.metadata().map_err(OutboxError::Io)?.len();
    if len > MAX_OUTBOX_BYTES {
        return Err(OutboxError::Corruption(format!(
            "outbox is {len} bytes, over the {MAX_OUTBOX_BYTES}-byte cap (corruption/DoS)"
        )));
    }
    let mut bytes = Vec::with_capacity(len as usize);
    // `take` is a belt-and-braces bound in case the file grows between stat and
    // read (e.g. a concurrent appender).
    file.take(MAX_OUTBOX_BYTES)
        .read_to_end(&mut bytes)
        .map_err(OutboxError::Io)?;
    // Split on raw bytes and parse each line with `serde_json::from_slice`,
    // which validates UTF-8 itself — invalid bytes in a line are a clean parse
    // error, with no lossy `U+FFFD` substitution that could obscure what was on
    // disk. A non-final parse error is fatal; the final line is
    // tolerated (torn write).
    let mut lines: Vec<&[u8]> = bytes.split(|&b| b == b'\n').collect();
    // A clean file ends with '\n', producing a trailing empty segment. Whether it
    // was present tells us if the genuine final line was newline-terminated (i.e.
    // fully committed) — drop the empty segment so the final entry is the last.
    let file_ended_with_newline = lines.last().is_some_and(|l| l.is_empty());
    if file_ended_with_newline {
        lines.pop();
    }

    let n = lines.len();
    let mut entries = Vec::with_capacity(n);
    let mut prev = GENESIS_PREV_HASH.to_string();
    // Bytes of the validated, newline-terminated prefix (each committed line plus
    // its '\n'). Trailing torn/un-terminated bytes are excluded so `open()` can
    // truncate them.
    let mut valid_bytes: u64 = 0;
    for (i, line) in lines.iter().enumerate() {
        let is_last = i + 1 == n;
        // The final segment is newline-terminated (committed) only if the file
        // ended with '\n'; every earlier segment necessarily was.
        let newline_terminated = !is_last || file_ended_with_newline;
        let entry: OutboxEntry = match serde_json::from_slice(line) {
            Ok(e) => e,
            Err(e) => {
                if is_last {
                    // §D8.1: a SIGKILL between write and fdatasync can truncate
                    // the final line. Tolerate exactly the last line.
                    tracing::warn!("audit outbox: skipping truncated trailing line: {e}");
                    break;
                }
                return Err(OutboxError::Corruption(format!(
                    "line {} failed to parse (corruption/tamper): {e}",
                    i + 1
                )));
            }
        };
        if !newline_terminated {
            // Parsed, but its terminating '\n' never reached disk — the record is
            // not yet committed (torn write of the newline). Tolerate like a torn
            // final line so it is truncated rather than kept (which would let the
            // next O_APPEND glue onto it). This can only be the last line.
            tracing::warn!(
                "audit outbox: skipping un-terminated trailing line (no newline on disk)"
            );
            break;
        }

        let expected = compute_entry_hash(&prev, &canonical_bytes(&entry.record));
        if entry.prev_hash != prev {
            return Err(OutboxError::Corruption(format!(
                "line {}: prev_hash discontinuity (entry deleted/reordered?)",
                i + 1
            )));
        }
        if entry.entry_hash != expected {
            return Err(OutboxError::Corruption(format!(
                "line {}: entry_hash mismatch (content tampered?)",
                i + 1
            )));
        }
        prev = entry.entry_hash.clone();
        entries.push(entry);
        valid_bytes += line.len() as u64 + 1; // line + its '\n'
    }
    Ok((entries, valid_bytes))
}

/// ADR-034 §D8 — best-effort `entry_hash` of the last hash-valid entry
/// of a (possibly corrupt) outbox file, for the `ChainReset` attestation written
/// by `conductorctl audit resume`. Walks the chain from genesis and stops at the
/// FIRST break (mid-file `prev_hash` discontinuity, `entry_hash` tamper, parse
/// error, or torn tail), returning the last good `entry_hash` — or
/// [`GENESIS_PREV_HASH`] if nothing validated (missing / empty / corrupt from the
/// first line). Never errors: corruption is the expected input here.
pub fn last_valid_entry_hash(path: &Path) -> String {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;

    let mut prev = GENESIS_PREV_HASH.to_string();
    // `O_NOFOLLOW` rejects a symlinked path at open; `O_NONBLOCK` ensures a
    // read-only open of a FIFO returns immediately instead of blocking for a
    // writer (POSIX). Together with the `is_file()` fstat below this keeps the
    // best-effort helper hang-free even if the outbox path was replaced by a
    // FIFO / device / socket — an attacker (or accidental admin action) must
    // not be able to wedge the operator recovery flow on EOF.
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(f) => f,
        Err(_) => return prev,
    };
    // Only read regular files. A FIFO/char-device/socket opened above would
    // otherwise hang `read_to_end` waiting for an EOF that never comes.
    match file.metadata() {
        Ok(md) if md.file_type().is_file() => {}
        _ => return prev,
    }
    let mut bytes = Vec::new();
    // Bound the read by the same cap as `read_entries_with_valid_len`.
    if file.take(MAX_OUTBOX_BYTES).read_to_end(&mut bytes).is_err() {
        return prev;
    }
    let mut lines: Vec<&[u8]> = bytes.split(|&b| b == b'\n').collect();
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    for line in &lines {
        let entry: OutboxEntry = match serde_json::from_slice(line) {
            Ok(e) => e,
            Err(_) => break,
        };
        let expected = compute_entry_hash(&entry.prev_hash, &canonical_bytes(&entry.record));
        if entry.prev_hash != prev || entry.entry_hash != expected {
            break;
        }
        prev = entry.entry_hash.clone();
    }
    prev
}

/// Truncate any tolerated torn trailing bytes from the outbox file (§D8.1).
/// `valid_bytes` is the offset past the last fully-committed entry (from
/// [`read_entries_with_valid_len`]). If the file on disk is longer, the excess is
/// a torn/un-terminated final line that [`read_entries`] skipped — `set_len` +
/// `sync_data` it away so a subsequent `O_APPEND` cannot glue a new entry onto
/// the partial line (which would brick recovery as a non-final unparseable line
/// on the next restart).
fn truncate_trailing_garbage(path: &Path, valid_bytes: u64) -> Result<(), OutboxError> {
    use std::os::unix::fs::OpenOptionsExt;

    // Open the existing file for writing (no create), `O_NOFOLLOW` for symlink
    // safety (consistent with `append_line`/`read_entries`). A missing file means
    // an empty outbox — nothing to truncate.
    let file = match OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(OutboxError::Io(e)),
    };
    let len = file.metadata().map_err(OutboxError::Io)?.len();
    if len <= valid_bytes {
        // Clean file (ends exactly on a committed entry) — no trailing garbage.
        return Ok(());
    }
    file.set_len(valid_bytes).map_err(OutboxError::Io)?;
    file.sync_data().map_err(OutboxError::Io)?;
    tracing::warn!(
        "audit outbox: truncated {} bytes of torn trailing data \
         (file was {len}, valid prefix {valid_bytes})",
        len - valid_bytes
    );
    Ok(())
}

/// An append-only, hash-chained audit outbox file (§D8).
///
/// Construct with [`AuditOutbox::open`] (which recovers the chain tail and entry
/// count from any existing file), then [`AuditOutbox::append`] /
/// [`AuditOutbox::enqueue_pending`] / [`AuditOutbox::mark_applied`] /
/// [`AuditOutbox::mark_failed`]. The append methods perform blocking,
/// `fdatasync`'d writes and must run on a blocking context.
#[derive(Debug)]
pub struct AuditOutbox {
    path: PathBuf,
    /// `prev_hash` for the next appended entry — the last entry's `entry_hash`,
    /// or `GENESIS_PREV_HASH` for an empty outbox.
    tail_hash: String,
    count: usize,
}

impl AuditOutbox {
    /// Open the outbox at `path`, reading and chain-verifying any existing
    /// entries to recover the tail hash and count. A missing file is an empty
    /// outbox. The recovered entries are returned for startup reconciliation
    /// (§D8.3, consumed in sub-slice C).
    pub fn open(path: PathBuf) -> Result<(Self, Vec<OutboxEntry>), OutboxError> {
        let (entries, valid_bytes) = read_entries_with_valid_len(&path)?;
        // §D8.1: drop any tolerated torn trailing bytes from disk BEFORE
        // an O_APPEND write can glue a new entry onto the partial line (which
        // would brick recovery on the next restart). No-op for a clean file.
        truncate_trailing_garbage(&path, valid_bytes)?;
        let tail_hash = entries
            .last()
            .map(|e| e.entry_hash.clone())
            .unwrap_or_else(|| GENESIS_PREV_HASH.to_string());
        let count = entries.len();
        Ok((
            Self {
                path,
                tail_hash,
                count,
            },
            entries,
        ))
    }

    /// Number of entries appended/recovered.
    pub fn len(&self) -> usize {
        self.count
    }

    /// Whether the outbox holds no entries.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Whether the outbox has reached its §D8.1 cap. The caller drives the
    /// daemon into degraded mode on `true` (sub-slice C); this primitive does
    /// not itself reject appends.
    pub fn is_at_cap(&self) -> bool {
        self.count >= OUTBOX_CAP
    }

    /// **Test-only.** Force the in-memory entry count, so the §D8.2 cap-refusal
    /// path can be exercised without writing [`OUTBOX_CAP`] real `fdatasync`
    /// entries. Does not touch the file.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn set_count_for_test(&mut self, count: usize) {
        self.count = count;
    }

    /// Append `record`, chaining it onto the current tail and durably writing it
    /// (§D8.1 recipe). Returns the persisted entry.
    pub fn append(&mut self, record: OutboxRecord) -> Result<OutboxEntry, OutboxError> {
        let prev_hash = self.tail_hash.clone();
        let entry_hash = compute_entry_hash(&prev_hash, &canonical_bytes(&record));
        let entry = OutboxEntry {
            record,
            prev_hash,
            entry_hash,
        };
        let line = serde_json::to_string(&entry)
            .map_err(|e| OutboxError::Corruption(format!("serialize outbox entry: {e}")))?;
        append_line(&self.path, &line).map_err(OutboxError::Io)?;
        self.tail_hash = entry.entry_hash.clone();
        self.count += 1;
        Ok(entry)
    }

    /// Append a `Pending` row (§D8.1 step 12). `intended_revision` is the config
    /// revision the in-flight mutation will produce.
    pub fn enqueue_pending(
        &mut self,
        id: impl Into<String>,
        provenance: Option<String>,
        intended_revision: Option<String>,
        created_at: i64,
    ) -> Result<OutboxEntry, OutboxError> {
        // §D8.2: refuse a NEW pending row at cap — atomic with the append (same
        // `&mut self`, so no check-then-act race). Resolution markers
        // (`mark_applied`/`mark_failed`) are intentionally exempt so an at-cap
        // outbox can still drain its backlog.
        if self.is_at_cap() {
            return Err(OutboxError::AtCap);
        }
        self.append(OutboxRecord {
            id: id.into(),
            phase: OutboxPhase::Pending,
            provenance,
            intended_revision,
            created_at,
        })
    }

    /// Append an `Applied` marker for a previously-enqueued `id` (step 15).
    pub fn mark_applied(
        &mut self,
        id: impl Into<String>,
        created_at: i64,
    ) -> Result<OutboxEntry, OutboxError> {
        self.append(OutboxRecord {
            id: id.into(),
            phase: OutboxPhase::Applied,
            provenance: None,
            intended_revision: None,
            created_at,
        })
    }

    /// Append a `Failed` marker for a previously-enqueued `id` (mutation rolled
    /// back after enqueue).
    pub fn mark_failed(
        &mut self,
        id: impl Into<String>,
        created_at: i64,
    ) -> Result<OutboxEntry, OutboxError> {
        self.append(OutboxRecord {
            id: id.into(),
            phase: OutboxPhase::Failed,
            provenance: None,
            intended_revision: None,
            created_at,
        })
    }

    /// ADR-034 §D8 — append the `ChainReset` attestation as the first
    /// record of a freshly-opened (post-rotation) outbox. `provenance` is the
    /// JSON describing the reset (operator, rotated filename, last valid hash of
    /// the old chain, reason). Like every append it is `fdatasync`'d before it
    /// returns, so a successful call proves the new chain is durably writable —
    /// the caller (`resume_audit`) only clears the audit-unavailable gate after
    /// this returns `Ok`.
    pub fn record_chain_reset(
        &mut self,
        id: impl Into<String>,
        provenance: String,
        created_at: i64,
    ) -> Result<OutboxEntry, OutboxError> {
        self.append(OutboxRecord {
            id: id.into(),
            phase: OutboxPhase::ChainReset,
            provenance: Some(provenance),
            intended_revision: None,
            created_at,
        })
    }

    /// **Rewrite-from-genesis compaction (§D8 sub-slice B2).** Rebuild the outbox
    /// file containing only the entries for which `keep` returns `true`,
    /// re-chained from [`GENESIS_PREV_HASH`]. The flusher calls this after the
    /// rows it is dropping have been durably written to SQLite, to bound the file
    /// against the §D8.1 cap *without* breaking the append-only hash chain
    /// (deleting individual lines would orphan every following `prev_hash`).
    ///
    /// Returns the number of entries dropped, and updates the in-memory
    /// tail/count to the compacted state. A `keep`-everything call is a no-op
    /// (returns 0, no rewrite/`fsync`).
    ///
    /// **Concurrency invariant (load-bearing).** This reads the file, then
    /// rewrites it — a concurrent *append* slipping in between the read and the
    /// rename would be dropped. That cannot happen: every method that mutates the
    /// outbox (this one, [`Self::append`] and the `enqueue_pending`/`mark_*`
    /// wrappers) takes `&mut self`, and the single live `AuditOutbox` is owned
    /// behind the `LiveConfig` outbox mutex, so appends and compactions are
    /// strictly serialized — never interleaved — within the (single) daemon
    /// process. The caller MUST hold that exclusive access for the whole call
    /// (the `&mut self` receiver enforces it at the type level).
    ///
    /// **Crash safety.** The survivors are written to a sibling temp file
    /// (`…​.compact.tmp`, `O_NOFOLLOW`, 0600), `fdatasync`'d, then **atomically
    /// renamed** over the outbox; the containing directory is then `fsync`'d so
    /// the rename is durable. A crash *before* the rename leaves the original
    /// file intact — the flusher re-runs and re-compacts (idempotent); a crash
    /// *after* leaves the compacted file. There is no window where the outbox is
    /// missing, partially written, or chain-broken.
    ///
    /// The directory `fsync` failure is **propagated** (not swallowed): an `Err`
    /// returned *after* the rename means the file was compacted in memory and on
    /// disk but the rename's durability across a power loss is unconfirmed. That
    /// is recoverable — a crash simply re-presents the pre-compaction file, which
    /// the flusher re-compacts idempotently — but the caller is told rather than
    /// being misled into believing the write was durable. The in-memory tail/count
    /// are advanced to the compacted state regardless, so they always match the
    /// on-disk file (and a retry of `compact_retaining` is a safe no-op).
    pub fn compact_retaining<F>(&mut self, keep: F) -> Result<usize, OutboxError>
    where
        F: Fn(&OutboxEntry) -> bool,
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

        // Read + chain-verify the current file (a corrupt file fails here rather
        // than being silently rewritten into a "clean" one).
        let entries = read_entries(&self.path)?;
        let before = entries.len();
        let retained: Vec<&OutboxEntry> = entries.iter().filter(|e| keep(e)).collect();
        let dropped = before - retained.len();
        if dropped == 0 {
            // Nothing to remove — skip the rewrite + fsync entirely.
            return Ok(0);
        }

        // Re-chain the survivors from genesis into a buffer.
        let mut prev = GENESIS_PREV_HASH.to_string();
        let mut new_tail = GENESIS_PREV_HASH.to_string();
        let mut buf = String::new();
        for e in &retained {
            let entry_hash = compute_entry_hash(&prev, &canonical_bytes(&e.record));
            let entry = OutboxEntry {
                record: e.record.clone(),
                prev_hash: prev.clone(),
                entry_hash: entry_hash.clone(),
            };
            let line = serde_json::to_string(&entry).map_err(|err| {
                OutboxError::Corruption(format!("serialize outbox entry during compaction: {err}"))
            })?;
            buf.push_str(&line);
            buf.push('\n');
            prev = entry_hash.clone();
            new_tail = entry_hash;
        }

        // Write the temp file durably. Same directory as the outbox so the rename
        // is atomic (same filesystem). `O_NOFOLLOW` refuses a symlinked temp;
        // remove any stale temp left by a prior crash first.
        let tmp_path = self.path.with_extension("compact.tmp");
        let _ = std::fs::remove_file(&tmp_path);
        {
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&tmp_path)
                .map_err(OutboxError::Io)?;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(OutboxError::Io)?;
            file.write_all(buf.as_bytes()).map_err(OutboxError::Io)?;
            file.sync_data().map_err(OutboxError::Io)?;
        }

        // Atomic replace. After this returns, the on-disk file IS the compacted
        // one, so the in-memory tail/count MUST be advanced to match (a later
        // append chains onto the new tail). This happens before the directory
        // fsync below so the in-memory view stays consistent with what is on disk
        // even if that fsync errors.
        std::fs::rename(&tmp_path, &self.path).map_err(OutboxError::Io)?;
        self.tail_hash = new_tail;
        self.count = retained.len();

        // Durably record the rename by fsync'ing the containing directory.
        // Failures are **propagated**, not swallowed: silently ignoring them
        // would let us report a durable compaction that a power loss could
        // revert, contradicting the crash-safety contract above. (On such a
        // revert the pre-compaction file reappears and the flusher re-compacts
        // idempotently, so an `Err` here means "compacted in memory and on disk,
        // but the rename's durability is unconfirmed" — recoverable, and the
        // caller is told rather than misled.)
        let dir = self
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        std::fs::File::open(&dir)
            .and_then(|dirfd| dirfd.sync_all())
            .map_err(OutboxError::Io)?;

        Ok(dropped)
    }

    /// **Test-only.** The on-disk outbox path, for re-open assertions.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn path_for_test(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outbox() -> (tempfile::TempDir, AuditOutbox) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit-outbox.log");
        let (ob, recovered) = AuditOutbox::open(path).unwrap();
        assert!(recovered.is_empty());
        (dir, ob)
    }

    #[test]
    fn append_then_read_round_trips() {
        let (dir, mut ob) = outbox();
        let path = dir.path().join("audit-outbox.log");
        let e1 = ob
            .enqueue_pending("m1", Some("{\"x\":1}".into()), Some("rev1".into()), 1000)
            .unwrap();
        let e2 = ob.mark_applied("m1", 2000).unwrap();
        assert_eq!(ob.len(), 2);

        let read = read_entries(&path).unwrap();
        assert_eq!(read.len(), 2);
        assert_eq!(read[0], e1);
        assert_eq!(read[1], e2);
        assert_eq!(read[0].record.phase, OutboxPhase::Pending);
        assert_eq!(read[1].record.phase, OutboxPhase::Applied);
        assert_eq!(read[0].record.intended_revision.as_deref(), Some("rev1"));
    }

    #[test]
    fn first_entry_chains_from_genesis() {
        let (_dir, mut ob) = outbox();
        let e = ob.enqueue_pending("m1", None, None, 1).unwrap();
        assert_eq!(e.prev_hash, GENESIS_PREV_HASH);
        // The second entry chains from the first.
        let e2 = ob.mark_applied("m1", 2).unwrap();
        assert_eq!(e2.prev_hash, e.entry_hash);
    }

    #[test]
    fn open_recovers_tail_and_count() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit-outbox.log");

        // First session: write two entries, remember the tail hash.
        let (mut ob, _) = AuditOutbox::open(path.clone()).unwrap();
        ob.enqueue_pending("a", None, None, 1).unwrap();
        let tail = ob.mark_applied("a", 2).unwrap().entry_hash;
        drop(ob);

        // Re-open: tail + count recovered, and the next append chains onto it.
        let (mut reopened, recovered) = AuditOutbox::open(path.clone()).unwrap();
        assert_eq!(recovered.len(), 2);
        assert_eq!(reopened.len(), 2);
        let e3 = reopened.enqueue_pending("b", None, None, 3).unwrap();
        assert_eq!(e3.prev_hash, tail);
        assert_eq!(read_entries(&path).unwrap().len(), 3);
    }

    #[test]
    fn missing_file_is_empty_outbox() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.log");
        assert!(read_entries(&path).unwrap().is_empty());
        let (ob, recovered) = AuditOutbox::open(path).unwrap();
        assert!(ob.is_empty());
        assert!(recovered.is_empty());
    }

    /// §D8.1 reader tolerance: a truncated final line (SIGKILL mid-write) is
    /// skipped, and the entries before it are returned intact.
    #[test]
    fn tolerates_truncated_trailing_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit-outbox.log");
        let (mut ob, _) = AuditOutbox::open(path.clone()).unwrap();
        ob.enqueue_pending("m1", None, None, 1).unwrap();
        ob.mark_applied("m1", 2).unwrap();
        ob.enqueue_pending("m2", None, None, 3).unwrap();

        // Simulate a torn final write: lop off the trailing newline + half the
        // last line's bytes.
        let mut raw = std::fs::read(&path).unwrap();
        let cut = raw.len() - 12; // mid last line (no trailing '\n' remains)
        raw.truncate(cut);
        std::fs::write(&path, &raw).unwrap();

        let read = read_entries(&path).unwrap();
        assert_eq!(read.len(), 2, "the torn third entry must be skipped");
        assert_eq!(read[1].record.id, "m1");
    }

    /// §D8.1: a tolerated torn trailing line must be TRUNCATED from disk
    /// at `open()`, so a subsequent `O_APPEND` write cannot glue a new entry onto
    /// the partial line and brick recovery on the next restart.
    #[test]
    fn open_truncates_torn_trailing_line_so_next_append_does_not_brick() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit-outbox.log");
        let (mut ob, _) = AuditOutbox::open(path.clone()).unwrap();
        ob.enqueue_pending("m1", None, None, 1).unwrap();
        ob.mark_applied("m1", 2).unwrap();
        ob.enqueue_pending("m2", None, None, 3).unwrap();
        drop(ob);

        // Simulate a torn final write: drop the trailing newline + half the last
        // line (same cut as `tolerates_truncated_trailing_line`).
        let mut raw = std::fs::read(&path).unwrap();
        let cut = raw.len() - 12;
        raw.truncate(cut);
        std::fs::write(&path, &raw).unwrap();
        let torn_len = std::fs::metadata(&path).unwrap().len();

        // Re-open: the torn tail is tolerated (2 entries recovered) AND the
        // garbage bytes are truncated from disk.
        let (mut reopened, recovered) = AuditOutbox::open(path.clone()).unwrap();
        assert_eq!(recovered.len(), 2, "torn third entry skipped on recovery");
        let after_open_len = std::fs::metadata(&path).unwrap().len();
        assert!(
            after_open_len < torn_len,
            "#2228: open() must truncate the torn trailing bytes \
             (was {torn_len}, now {after_open_len})"
        );

        // Append a fresh entry — it must chain cleanly onto entry 2, not glue
        // onto the torn partial.
        reopened.enqueue_pending("m4", None, None, 4).unwrap();

        // THE REGRESSION: a second restart must NOT brick. Pre-fix, the new entry
        // was appended after the un-truncated torn partial, producing a
        // non-final unparseable line → fatal `Corruption`.
        let (_again, recovered2) = AuditOutbox::open(path.clone())
            .expect("#2228: second restart must not brick on a glued torn line");
        assert_eq!(
            recovered2.len(),
            3,
            "two recovered entries + the freshly appended one"
        );
        assert_eq!(recovered2[2].record.id, "m4");
    }

    /// §D8.4 partial-tampering detection: flipping a content byte in a
    /// non-final entry breaks the chain → fatal corruption.
    #[test]
    fn detects_tampered_middle_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit-outbox.log");
        let (mut ob, _) = AuditOutbox::open(path.clone()).unwrap();
        ob.enqueue_pending("victim", Some("orig".into()), None, 1)
            .unwrap();
        ob.mark_applied("victim", 2).unwrap();
        ob.enqueue_pending("after", None, None, 3).unwrap();

        // Tamper: rewrite the first line's provenance content in place.
        let text = std::fs::read_to_string(&path).unwrap();
        let tampered = text.replacen("orig", "evil", 1);
        assert_ne!(text, tampered);
        std::fs::write(&path, tampered).unwrap();

        match read_entries(&path) {
            Err(OutboxError::Corruption(_)) => {}
            other => panic!("expected Corruption on tampered entry, got {other:?}"),
        }
    }

    /// A deleted middle entry breaks `prev_hash` continuity → fatal.
    #[test]
    fn detects_deleted_middle_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit-outbox.log");
        let (mut ob, _) = AuditOutbox::open(path.clone()).unwrap();
        ob.enqueue_pending("a", None, None, 1).unwrap();
        ob.mark_applied("a", 2).unwrap();
        ob.enqueue_pending("c", None, None, 3).unwrap();

        // Drop the MIDDLE line.
        let text = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<&str> = text.lines().collect();
        lines.remove(1);
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();

        match read_entries(&path) {
            Err(OutboxError::Corruption(_)) => {}
            other => panic!("expected Corruption on deleted entry, got {other:?}"),
        }
    }

    /// A parse error on a NON-final line is fatal (only the last line is
    /// tolerated).
    #[test]
    fn non_final_garbage_line_is_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit-outbox.log");
        let (mut ob, _) = AuditOutbox::open(path.clone()).unwrap();
        ob.enqueue_pending("a", None, None, 1).unwrap();
        ob.mark_applied("a", 2).unwrap();

        // Corrupt the FIRST line into non-JSON; keep a valid trailing line.
        let text = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<String> = text.lines().map(String::from).collect();
        lines[0] = "{not json".into();
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();

        match read_entries(&path) {
            Err(OutboxError::Corruption(_)) => {}
            other => panic!("expected Corruption on non-final garbage, got {other:?}"),
        }
    }

    /// §D3.3: the outbox file is created owner-only (0600), never world-readable.
    #[test]
    fn created_outbox_is_owner_only_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit-outbox.log");
        let (mut ob, _) = AuditOutbox::open(path.clone()).unwrap();
        ob.enqueue_pending("m1", None, None, 1).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "outbox must be 0600, got {:o}",
            mode & 0o777
        );
    }

    /// Append-time `O_NOFOLLOW` guards against a symlink swapped in AFTER open
    /// (a TOCTOU the read-time check can't see): the append refuses to write
    /// through it.
    #[test]
    fn append_rejects_symlink_swapped_after_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit-outbox.log");
        let other = dir.path().join("elsewhere.log");
        std::fs::write(&other, "").unwrap();

        // Open on a clean (non-existent) path → empty outbox.
        let (mut ob, _) = AuditOutbox::open(path.clone()).unwrap();
        // Attacker swaps the path for a symlink before the first append.
        std::os::unix::fs::symlink(&other, &path).unwrap();

        match ob.enqueue_pending("m1", None, None, 1) {
            Err(OutboxError::Io(e)) => {
                // O_NOFOLLOW on a symlink → ELOOP.
                assert_eq!(
                    e.raw_os_error(),
                    Some(libc::ELOOP),
                    "expected ELOOP from O_NOFOLLOW, got {e}"
                );
            }
            other => panic!("expected Io(ELOOP) on symlinked outbox, got {other:?}"),
        }
    }

    /// `deny_unknown_fields`: an extra JSON key injected into a non-final entry
    /// is a fatal parse error, not silently dropped (would otherwise bypass the
    /// hash chain).
    #[test]
    fn detects_injected_extra_field() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit-outbox.log");
        let (mut ob, _) = AuditOutbox::open(path.clone()).unwrap();
        ob.enqueue_pending("a", None, None, 1).unwrap();
        ob.mark_applied("a", 2).unwrap();

        // Inject an unmodelled top-level key into the FIRST line.
        let text = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<String> = text.lines().map(String::from).collect();
        lines[0] = lines[0].replacen("\"prev_hash\"", "\"evil\":true,\"prev_hash\"", 1);
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();

        match read_entries(&path) {
            Err(OutboxError::Corruption(_)) => {}
            other => panic!("expected Corruption on injected field, got {other:?}"),
        }
    }

    /// Invalid UTF-8 in a non-final line is a clean fatal parse error (byte-based
    /// `from_slice`, no lossy substitution).
    #[test]
    fn invalid_utf8_in_middle_line_is_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit-outbox.log");
        let (mut ob, _) = AuditOutbox::open(path.clone()).unwrap();
        ob.enqueue_pending("a", None, None, 1).unwrap();
        ob.mark_applied("a", 2).unwrap();

        // Corrupt the first line with a raw invalid-UTF-8 byte (0xFF).
        let mut raw = std::fs::read(&path).unwrap();
        let nl = raw.iter().position(|&b| b == b'\n').unwrap();
        raw[nl / 2] = 0xFF;
        std::fs::write(&path, &raw).unwrap();

        match read_entries(&path) {
            Err(OutboxError::Corruption(_)) => {}
            other => panic!("expected Corruption on invalid UTF-8, got {other:?}"),
        }
    }

    /// `read_entries` refuses a symlinked outbox path (consistent with append).
    #[test]
    fn read_rejects_symlinked_path() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real-outbox.log");
        let (mut ob, _) = AuditOutbox::open(real.clone()).unwrap();
        ob.enqueue_pending("a", None, None, 1).unwrap();

        let link = dir.path().join("audit-outbox.log");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        match read_entries(&link) {
            Err(OutboxError::Io(e)) => {
                assert_eq!(
                    e.raw_os_error(),
                    Some(libc::ELOOP),
                    "expected ELOOP, got {e}"
                );
            }
            other => panic!("expected Io(ELOOP) on symlinked read, got {other:?}"),
        }
    }

    /// A file over the [`MAX_OUTBOX_BYTES`] cap is rejected before being read
    /// into memory (DoS guard). Uses a sparse `set_len` so the test doesn't
    /// write 16 MiB.
    #[test]
    fn rejects_oversized_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit-outbox.log");
        let f = std::fs::File::create(&path).unwrap();
        f.set_len(MAX_OUTBOX_BYTES + 1).unwrap();
        drop(f);
        match read_entries(&path) {
            Err(OutboxError::Corruption(m)) => assert!(m.contains("cap"), "got {m}"),
            other => panic!("expected Corruption on oversized file, got {other:?}"),
        }
    }

    #[test]
    fn cap_constant_and_predicate() {
        assert_eq!(OUTBOX_CAP, 4096);
        assert_eq!(MAX_OUTBOX_BYTES, 4096 * 4096);
        let (_dir, ob) = outbox();
        assert!(!ob.is_at_cap());
    }

    /// §D8.2: at cap, `enqueue_pending` refuses atomically with `AtCap`, but
    /// resolution markers (`mark_applied`/`mark_failed`) are exempt so the
    /// backlog can still drain.
    #[test]
    fn at_cap_refuses_pending_but_allows_markers() {
        let (_dir, mut ob) = outbox();
        ob.set_count_for_test(OUTBOX_CAP);
        assert!(ob.is_at_cap());

        match ob.enqueue_pending("blocked", None, Some("r".into()), 1) {
            Err(OutboxError::AtCap) => {}
            other => panic!("expected AtCap, got {other:?}"),
        }
        // Markers are NOT cap-blocked.
        ob.mark_applied("drain-me", 2)
            .expect("markers exempt from cap");
        ob.mark_failed("drain-me", 3)
            .expect("markers exempt from cap");
    }

    // ── §D8 sub-slice B2: rewrite-from-genesis compaction ────────────────

    /// Compaction drops the un-kept rows, re-chains the survivors from genesis,
    /// and the rewritten file re-opens cleanly (chain re-verified).
    #[test]
    fn compact_retaining_drops_unkept_and_rechains() {
        let (_dir, mut ob) = outbox();
        ob.enqueue_pending("a", None, Some("ra".into()), 1).unwrap();
        ob.enqueue_pending("b", None, Some("rb".into()), 2).unwrap();
        ob.enqueue_pending("c", None, Some("rc".into()), 3).unwrap();
        assert_eq!(ob.len(), 3);

        // Drop the row for id "b" (e.g. its mutation was flushed to SQLite).
        let dropped = ob.compact_retaining(|e| e.record.id != "b").unwrap();
        assert_eq!(dropped, 1);
        assert_eq!(ob.len(), 2);

        // Re-open: the rewritten file is a valid chain from genesis with only a/c.
        let path = ob.path_for_test().to_path_buf();
        let (reopened, recovered) = AuditOutbox::open(path).unwrap();
        assert_eq!(reopened.len(), 2);
        let ids: Vec<&str> = recovered.iter().map(|e| e.record.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "c"]);
        // First survivor is re-rooted at genesis.
        assert_eq!(recovered[0].prev_hash, GENESIS_PREV_HASH);

        // Appending after compaction chains onto the compacted tail.
        let mut ob2 = reopened;
        ob2.enqueue_pending("d", None, Some("rd".into()), 4)
            .unwrap();
        assert_eq!(ob2.len(), 3);
    }

    /// A post-compaction append (§D8.4) must survive a RESTART with no
    /// `prev_hash` discontinuity at the compaction boundary. This is the exact
    /// "drive mutations → compaction → restart → check chain" scenario that
    /// could break. Compaction rewrites survivors from genesis and
    /// advances the in-memory tail, so an entry appended afterwards chains onto
    /// the rewritten tail; re-opening from disk must read the whole chain cleanly.
    #[test]
    fn compaction_boundary_chain_survives_reopen() {
        let (_dir, mut ob) = outbox();
        ob.enqueue_pending("a", None, Some("ra".into()), 1).unwrap();
        ob.mark_applied("a", 2).unwrap();
        ob.enqueue_pending("b", None, Some("rb".into()), 3).unwrap();
        ob.enqueue_pending("c", None, Some("rc".into()), 4).unwrap();
        let path = ob.path_for_test().to_path_buf();

        // Compact away the resolved "a" rows (Applied marker + its Pending),
        // re-rooting the chain at genesis on "b".
        let dropped = ob.compact_retaining(|e| e.record.id != "a").unwrap();
        assert_eq!(dropped, 2, "both 'a' rows (Pending + Applied) dropped");

        // Append ACROSS the compaction boundary, then drop and RESTART.
        ob.enqueue_pending("d", None, Some("rd".into()), 5).unwrap();
        drop(ob);

        // The whole chain — compacted survivors + the post-compaction append —
        // must re-read cleanly: no prev_hash discontinuity, no fatal corruption.
        let (reopened, recovered) = AuditOutbox::open(path.clone())
            .expect("#2296: compaction boundary must not break the chain on restart");
        let ids: Vec<&str> = recovered.iter().map(|e| e.record.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["b", "c", "d"],
            "survivors + boundary append, in order"
        );
        assert_eq!(
            recovered[0].prev_hash, GENESIS_PREV_HASH,
            "first survivor re-rooted at genesis"
        );

        // And one more append+restart cycle stays continuous.
        let mut ob3 = reopened;
        ob3.enqueue_pending("e", None, Some("re".into()), 6)
            .unwrap();
        drop(ob3);
        let (_again, recovered2) = AuditOutbox::open(path)
            .expect("#2296: a second restart after another append stays continuous");
        let ids2: Vec<&str> = recovered2.iter().map(|e| e.record.id.as_str()).collect();
        assert_eq!(ids2, vec!["b", "c", "d", "e"]);
    }

    /// Keeping every row is a no-op: returns 0 and does not rewrite.
    #[test]
    fn compact_retaining_noop_when_all_kept() {
        let (_dir, mut ob) = outbox();
        ob.enqueue_pending("a", None, None, 1).unwrap();
        ob.enqueue_pending("b", None, None, 2).unwrap();
        let dropped = ob.compact_retaining(|_| true).unwrap();
        assert_eq!(dropped, 0);
        assert_eq!(ob.len(), 2);
    }

    /// Dropping everything yields an empty, re-openable outbox.
    #[test]
    fn compact_retaining_to_empty() {
        let (_dir, mut ob) = outbox();
        ob.enqueue_pending("a", None, None, 1).unwrap();
        ob.enqueue_pending("b", None, None, 2).unwrap();
        let dropped = ob.compact_retaining(|_| false).unwrap();
        assert_eq!(dropped, 2);
        assert_eq!(ob.len(), 0);
        assert!(ob.is_empty());

        let path = ob.path_for_test().to_path_buf();
        let (reopened, recovered) = AuditOutbox::open(path).unwrap();
        assert!(reopened.is_empty());
        assert!(recovered.is_empty());
    }

    // ── ChainReset record + last-valid-hash extraction ──────────────

    #[test]
    fn record_chain_reset_round_trips_and_chains_from_genesis() {
        let (dir, mut ob) = outbox();
        let path = dir.path().join("audit-outbox.log");
        let e = ob
            .record_chain_reset("reset-1", "{\"kind\":\"chain_reset\"}".into(), 1234)
            .unwrap();
        assert_eq!(e.prev_hash, GENESIS_PREV_HASH);
        let read = read_entries(&path).unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].record.phase, OutboxPhase::ChainReset);
        assert_eq!(
            read[0].record.provenance.as_deref(),
            Some("{\"kind\":\"chain_reset\"}")
        );
    }

    #[test]
    fn last_valid_entry_hash_missing_or_empty_is_genesis() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit-outbox.log");
        // Missing file.
        assert_eq!(last_valid_entry_hash(&path), GENESIS_PREV_HASH);
        // Empty (opened, nothing appended).
        let (_ob, _) = AuditOutbox::open(path.clone()).unwrap();
        assert_eq!(last_valid_entry_hash(&path), GENESIS_PREV_HASH);
    }

    #[test]
    fn last_valid_entry_hash_clean_file_returns_tail() {
        let (dir, mut ob) = outbox();
        let path = dir.path().join("audit-outbox.log");
        ob.enqueue_pending("m1", None, Some("rev1".into()), 1)
            .unwrap();
        let e2 = ob.mark_applied("m1", 2).unwrap();
        assert_eq!(last_valid_entry_hash(&path), e2.entry_hash);
    }

    #[test]
    fn last_valid_entry_hash_stops_at_mid_file_break() {
        use std::io::Write;
        let (dir, mut ob) = outbox();
        let path = dir.path().join("audit-outbox.log");
        ob.enqueue_pending("m1", None, None, 1).unwrap();
        let e2 = ob.mark_applied("m1", 2).unwrap();
        // Append a hash-discontinuous third line directly (simulating a mid-file
        // corruption / deleted-or-reordered entry). It parses as JSON but its
        // prev_hash won't match e2.entry_hash.
        let bogus = OutboxEntry {
            record: OutboxRecord {
                id: "bogus".into(),
                phase: OutboxPhase::Pending,
                provenance: None,
                intended_revision: None,
                created_at: 3,
            },
            prev_hash: "deadbeef".into(),
            entry_hash: "cafef00d".into(),
        };
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "{}", serde_json::to_string(&bogus).unwrap()).unwrap();
        f.sync_data().unwrap();
        // read_entries (strict) must reject the mid-file break…
        assert!(matches!(
            read_entries(&path),
            Err(OutboxError::Corruption(_))
        ));
        // …but last_valid_entry_hash returns the last good entry's hash (e2).
        assert_eq!(last_valid_entry_hash(&path), e2.entry_hash);
    }

    #[test]
    fn last_valid_entry_hash_on_fifo_returns_genesis_without_hanging() {
        // If the outbox path is replaced by a FIFO (or device),
        // the best-effort helper must NOT block forever on EOF — it returns
        // GENESIS. The test would hang (and fail by timeout) if O_NONBLOCK +
        // the is_file() fstat were missing. No writer is ever opened.
        use std::ffi::CString;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit-outbox.log");
        let c_path = CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        // 0o600, same perms the real outbox uses.
        let rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
        assert_eq!(rc, 0, "mkfifo failed: {}", std::io::Error::last_os_error());
        assert_eq!(last_valid_entry_hash(&path), GENESIS_PREV_HASH);
    }
}
