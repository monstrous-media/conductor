// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! ADR-045 D5 (#2493) — the always-compiled append-only JSONL audit sink.
//!
//! The free (no-`audit-db`) composition still needs a durable, redacted,
//! tamper-evident audit trail — ADR-042 made listener audit a security
//! control, and ADR-045 D5 makes that control composition-independent.
//!
//! Design, per the four in-ADR invariants (ADR-045 D5, Council R1 #4):
//!
//! 1. **Single-writer serialization** — every write flows through one
//!    dedicated writer THREAD fed by a bounded `sync_channel` (the pump
//!    idiom, cf. ADR-039). Producers `try_send`; a full channel drops the
//!    entry and bumps a counter (ADR-042 rate-limit-audit aggregation:
//!    shed load is *observable*, never blocking the event path). Chain
//!    hashing happens in the writer, so lines can never interleave. A
//!    plain OS thread (not a tokio task) keeps the per-line `fdatasync`
//!    off the async runtime and works in every composition.
//! 2. **Tail truncation accepted** — a line-hash chain detects mid-chain
//!    tampering, not deletion of the last N lines; identical threat-model
//!    posture to the SQLite chain (ADR-027 D13b).
//! 3. **Fail-closed for network listeners** — enforced at listener
//!    startup in `engine_manager` (no sink ⇒ listeners refuse to start),
//!    not here; this sink's constructor returns `Err` on unusable paths
//!    so the caller can make that call.
//! 4. **Bounded disk** — size-capped rotation: when the active segment
//!    exceeds `max_segment_bytes` it is renamed to `<path>.1` (replacing
//!    any previous rotated segment; at most two segments exist). The
//!    writer's in-memory chain tail carries across, so the new segment's
//!    first record chains to the rotated segment's head hash
//!    ([`verify_jsonl_chain`] checks exactly this).
//!
//! Line format mirrors the audit outbox (ADR-034 §D8): one JSON object per
//! line — `{ "record": <AuditEntry>, "prev_hash": ..., "entry_hash": ... }`
//! — chained with the shared [`compute_entry_hash`] over the serialized
//! `record` bytes. Redaction (ADR-027 D13c / ADR-042 D6) and redact-THEN-
//! truncate ordering are applied before enqueue with the same primitives
//! the SQLite sink uses, so the chain protects the post-redaction bytes.

use std::io::Write;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tracing::{error, info, warn};

use super::hash_chain::{GENESIS_PREV_HASH, compute_entry_hash};
use super::reconcile::ReconciledMutation;
use super::redaction::{redact_audit_field, truncate_for_storage};
use super::schema::{AuditEntry, AuditEventType, AuditRiskTier, UserContext};
use super::sink::AuditSink;
use conductor_core::config::Provenance;

/// Bounded pump depth. Producers never block: beyond this, entries are
/// dropped and counted (invariant 1 backpressure).
const PUMP_CAPACITY: usize = 1024;

/// Live-tail ring capacity (mirrors the SQLite sink's 1024).
const BROADCAST_CAPACITY: usize = 1024;

/// Default rotation cap. Two segments ⇒ worst-case ~16 MiB on disk.
pub const DEFAULT_MAX_SEGMENT_BYTES: u64 = 8 * 1024 * 1024;

/// One persisted JSONL line. `deny_unknown_fields` keeps every byte of the
/// wrapper hash-covered or structural — nothing can ride along unverified
/// (same posture as the outbox wrapper, ADR-034 §D8.4).
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonlLine {
    record: AuditEntry,
    prev_hash: String,
    entry_hash: String,
}

/// Configuration for [`JsonlAuditSink`].
#[derive(Debug, Clone)]
pub struct JsonlSinkConfig {
    /// Active segment path (rotated segment lives at `<path>.1`).
    pub path: PathBuf,
    /// Rotation threshold for the active segment (invariant 4).
    pub max_segment_bytes: u64,
    /// ADR-027 D13c: redact secret-shaped values in `arguments`.
    pub redact_arguments: bool,
    /// ADR-027 D13c: redact secret-shaped values in `result`.
    pub redact_results: bool,
    /// Log ReadOnly-tier tool start/complete events (parity with
    /// `AuditLoggerConfig::log_readonly`).
    pub log_readonly: bool,
}

impl JsonlSinkConfig {
    /// Config rooted at the daemon state dir (`audit.jsonl`, beside the
    /// audit outbox — both are daemon-owned append-only state).
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            max_segment_bytes: DEFAULT_MAX_SEGMENT_BYTES,
            redact_arguments: true,
            redact_results: true,
            log_readonly: true,
        }
    }
}

/// Why a JSONL chain failed verification.
#[derive(Debug, PartialEq, Eq)]
pub enum JsonlChainBreak {
    /// I/O error reading a segment.
    Io(String),
    /// A non-final line failed to parse (torn/corrupt mid-file).
    Parse { segment: String, line: usize },
    /// A line's `prev_hash` does not match the running chain tail.
    BrokenLink { segment: String, line: usize },
    /// A line's `entry_hash` does not match its recomputed hash.
    HashMismatch { segment: String, line: usize },
}

/// Append-only JSONL audit sink (always compiled).
pub struct JsonlAuditSink {
    tx: SyncSender<AuditEntry>,
    event_tx: broadcast::Sender<AuditEntry>,
    dropped: Arc<AtomicU64>,
    config: JsonlSinkConfig,
    writer: Option<std::thread::JoinHandle<()>>,
}

impl JsonlAuditSink {
    /// Open (or create) the sink at `config.path` and start the writer
    /// pump. Recovers the chain tail from existing segments so appends
    /// continue the chain across restarts. Returns `Err` when the path is
    /// unusable — the caller decides fail-open vs fail-closed (D5 rule 3).
    pub fn new(config: JsonlSinkConfig) -> std::io::Result<Self> {
        if let Some(parent) = config.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Recover the tail: prefer the active segment's tail; if the active
        // segment has no valid lines, fall back to the rotated segment's.
        // A torn final line (crash mid-append) is TRUNCATED away — same
        // recovery as `AuditOutbox::open` — so the next append lands on a
        // clean line boundary instead of leaving a mid-file parse error.
        let rotated = rotated_path(&config.path);
        let tail = match segment_recover_truncating(&config.path)? {
            Some(t) => Some(t),
            None => segment_tail_hash(&rotated)?,
        }
        .unwrap_or_else(|| GENESIS_PREV_HASH.to_string());

        // Probe writability up front so construction fails loud (D5 rule 3)
        // instead of the pump discovering it on the first entry.
        probe_appendable(&config.path)?;

        let (tx, rx) = std::sync::mpsc::sync_channel::<AuditEntry>(PUMP_CAPACITY);
        let (event_tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        let dropped = Arc::new(AtomicU64::new(0));

        let writer = {
            let path = config.path.clone();
            let cap = config.max_segment_bytes;
            let event_tx = event_tx.clone();
            let dropped = Arc::clone(&dropped);
            std::thread::Builder::new()
                .name("audit-jsonl-writer".into())
                .spawn(move || writer_pump(rx, path, cap, tail, event_tx, dropped))?
        };

        info!(path = %config.path.display(), "JSONL audit sink started");
        Ok(Self {
            tx,
            event_tx,
            dropped,
            config,
            writer: Some(writer),
        })
    }

    /// Entries dropped because the pump channel was full (invariant 1
    /// backpressure is shed-and-count, never block).
    pub fn dropped_entries(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Redact-then-truncate (ADR-027 D13c ordering) and enqueue.
    fn submit(&self, mut entry: AuditEntry) -> String {
        let id = entry.id.clone();
        if self.config.redact_arguments {
            entry.arguments = redact_audit_field(entry.arguments.as_deref());
        }
        // Council review on PR #2605: cap arguments like results — an
        // append-only file must bound every field (redact-then-truncate
        // ordering preserved; D13c).
        entry.arguments = entry.arguments.take().map(truncate_for_storage);
        let result = if self.config.redact_results {
            redact_audit_field(entry.result.as_deref())
        } else {
            entry.result.take()
        };
        entry.result = result.map(truncate_for_storage);

        match self.tx.try_send(entry) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                let n = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
                // Aggregated shed-load visibility: log the first drop and
                // every 256th thereafter (cf. ADR-042 rate-limit-audit).
                if n == 1 || n.is_multiple_of(256) {
                    warn!(dropped = n, "JSONL audit pump full — shedding entries");
                }
            }
            Err(TrySendError::Disconnected(_)) => {
                warn!("JSONL audit writer gone — entry dropped");
            }
        }
        id
    }
}

impl Drop for JsonlAuditSink {
    fn drop(&mut self) {
        // Close the channel so the pump drains outstanding entries and
        // exits; join so buffered lines are durable before drop returns.
        let (closed_tx, _closed_rx) = std::sync::mpsc::sync_channel(1);
        drop(std::mem::replace(&mut self.tx, closed_tx));
        if let Some(handle) = self.writer.take() {
            let _ = handle.join();
        }
    }
}

/// The single-writer pump (invariant 1): owns the chain tail, serializes,
/// hashes, appends durably, rotates, and only THEN broadcasts (subscribers
/// only ever see durable entries).
fn writer_pump(
    rx: Receiver<AuditEntry>,
    path: PathBuf,
    cap: u64,
    mut tail: String,
    event_tx: broadcast::Sender<AuditEntry>,
    dropped: Arc<AtomicU64>,
) {
    while let Ok(entry) = rx.recv() {
        let canonical = match serde_json::to_vec(&entry) {
            Ok(b) => b,
            Err(e) => {
                dropped.fetch_add(1, Ordering::Relaxed);
                error!("JSONL audit: entry serialization failed: {e}");
                continue;
            }
        };
        let entry_hash = compute_entry_hash(&tail, &canonical);
        let line = JsonlLine {
            record: entry,
            prev_hash: tail.clone(),
            entry_hash: entry_hash.clone(),
        };
        let serialized = match serde_json::to_string(&line) {
            Ok(s) => s,
            Err(e) => {
                error!("JSONL audit: line serialization failed: {e}");
                continue;
            }
        };
        if let Err(e) = append_line(&path, &serialized) {
            // Best-effort per D5 rule 3 (fail-open outside listener
            // startup): keep the daemon alive, keep the chain tail
            // UNCHANGED so the next successful append still chains.
            // Council review on PR #2605: a disk-failed entry is as
            // dropped as a shed one — count it so `dropped_entries()`
            // reflects EVERY entry that did not reach disk.
            dropped.fetch_add(1, Ordering::Relaxed);
            error!(path = %path.display(), "JSONL audit append failed: {e}");
            continue;
        }
        tail = entry_hash;
        let _ = event_tx.send(line.record);

        // Invariant 4 — bounded disk: rotate when the active segment
        // exceeds the cap. The in-memory tail carries across, so the new
        // segment's first record chains to this segment's head hash.
        if let Ok(meta) = std::fs::metadata(&path)
            && meta.len() > cap
        {
            // Windows `rename` fails when the destination exists (Copilot
            // review on PR #2605); drop the previous rotated segment first.
            // Unix `rename` replaces atomically either way; NotFound is the
            // normal first-rotation case.
            let dest = rotated_path(&path);
            if let Err(e) = std::fs::remove_file(&dest)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                error!("JSONL audit rotation: removing old segment failed: {e}");
            }
            if let Err(e) = std::fs::rename(&path, &dest) {
                error!("JSONL audit rotation failed: {e}");
            }
        }
    }
}

/// Durable append (recipe shared with the audit outbox, ADR-034 §D8.1):
/// `O_APPEND` + `O_NOFOLLOW`, 0600 enforced, single write, `fdatasync`.
fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
    let mut buf = String::with_capacity(line.len() + 1);
    buf.push_str(line);
    buf.push('\n');

    let mut opts = std::fs::OpenOptions::new();
    opts.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = opts.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(buf.as_bytes())?;
    file.sync_data()?;
    Ok(())
}

/// Open-for-append probe used at construction so an unusable path fails
/// the constructor (D5 rule 3) rather than the first audited event.
fn probe_appendable(path: &Path) -> std::io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    opts.open(path).map(|_| ())
}

/// `<path>.1` — the single rotated segment.
pub fn rotated_path(path: &Path) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(".1");
    PathBuf::from(os)
}

/// Last valid `entry_hash` in a segment, or `None` for missing/empty/fully
/// torn segments. A torn FINAL line is tolerated (crash mid-append).
fn segment_tail_hash(path: &Path) -> std::io::Result<Option<String>> {
    Ok(segment_scan(path)?.map(|(tail, _)| tail))
}

/// Like [`segment_tail_hash`], but additionally TRUNCATES a torn tail off
/// the ACTIVE segment so subsequent appends land on a line boundary
/// (mirrors `AuditOutbox::open` recovery).
fn segment_recover_truncating(path: &Path) -> std::io::Result<Option<String>> {
    let Some((tail, valid_len)) = segment_scan(path)? else {
        // No valid lines at all: a fully-torn file is truncated to empty.
        if let Ok(meta) = std::fs::metadata(path)
            && meta.len() > 0
        {
            warn!(path = %path.display(), "JSONL audit: truncating fully-torn segment");
            let f = std::fs::OpenOptions::new().write(true).open(path)?;
            f.set_len(0)?;
        }
        return Ok(None);
    };
    let meta = std::fs::metadata(path)?;
    if meta.len() > valid_len {
        warn!(
            path = %path.display(),
            torn_bytes = meta.len() - valid_len,
            "JSONL audit: truncating torn final line"
        );
        let f = std::fs::OpenOptions::new().write(true).open(path)?;
        f.set_len(valid_len)?;
    }
    Ok(Some(tail))
}

/// Scan a segment: returns `(last_valid_entry_hash, byte_len_of_valid_prefix)`
/// or `None` when the file is missing or holds no valid line.
fn segment_scan(path: &Path) -> std::io::Result<Option<(String, u64)>> {
    let data = match std::fs::read_to_string(path) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut tail: Option<(String, u64)> = None;
    let mut offset = 0u64;
    for line in data.split_inclusive('\n') {
        let trimmed = line.strip_suffix('\n').unwrap_or(line);
        // Only a newline-terminated, parseable line counts as valid.
        if line.ends_with('\n')
            && let Ok(parsed) = serde_json::from_str::<JsonlLine>(trimmed)
        {
            offset += line.len() as u64;
            tail = Some((parsed.entry_hash, offset));
        } else {
            break;
        }
    }
    Ok(tail)
}

/// Verify the RETAINED chain window: the rotated segment (if any), then the
/// active segment continuing from the rotated segment's tail (invariant 4
/// cross-segment chaining). Returns the number of verified entries.
///
/// Window semantics: bounded retention (two segments) means segments older
/// than `<path>.1` are gone, so after two or more rotations the oldest
/// retained line legitimately chains to a discarded predecessor. The oldest
/// retained line's `prev_hash` is therefore ADOPTED as the window start —
/// ancestry beyond the window is unverifiable by design (the same accepted
/// trade-off as tail truncation, ADR-045 D5 invariants 2/4). Within the
/// window every link and every entry hash must verify; a torn FINAL line in
/// the ACTIVE segment is tolerated (crash mid-append); anything else breaks.
pub fn verify_jsonl_chain(active: &Path) -> Result<usize, JsonlChainBreak> {
    let mut expected_prev: Option<String> = None;
    let mut count = 0usize;

    let rotated = rotated_path(active);
    for (segment, tolerate_torn_tail) in [(rotated.as_path(), false), (active, true)] {
        let data = match std::fs::read_to_string(segment) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(JsonlChainBreak::Io(e.to_string())),
        };
        let seg_name = segment.display().to_string();
        let lines: Vec<&str> = data.lines().collect();
        for (i, raw) in lines.iter().enumerate() {
            let parsed: JsonlLine = match serde_json::from_str(raw) {
                Ok(p) => p,
                Err(_) if tolerate_torn_tail && i == lines.len() - 1 => break,
                Err(_) => {
                    return Err(JsonlChainBreak::Parse {
                        segment: seg_name,
                        line: i + 1,
                    });
                }
            };
            match &expected_prev {
                Some(prev) if parsed.prev_hash != *prev => {
                    return Err(JsonlChainBreak::BrokenLink {
                        segment: seg_name,
                        line: i + 1,
                    });
                }
                _ => {} // first retained line: adopt its prev_hash as window start
            }
            let canonical = serde_json::to_vec(&parsed.record)
                .map_err(|e| JsonlChainBreak::Io(e.to_string()))?;
            if compute_entry_hash(&parsed.prev_hash, &canonical) != parsed.entry_hash {
                return Err(JsonlChainBreak::HashMismatch {
                    segment: seg_name,
                    line: i + 1,
                });
            }
            expected_prev = Some(parsed.entry_hash);
            count += 1;
        }
    }
    Ok(count)
}

/// Production wire-up: the JSONL sink at its default location,
/// `<state_dir>/audit.jsonl` (beside the audit outbox — both daemon-owned
/// append-only state). Returns `None` (with an `error!`) when the sink
/// cannot be constructed; the caller applies the D5 fail-open/fail-closed
/// policy (listeners refuse to start; everything else warns and continues).
pub fn default_jsonl_sink() -> Option<std::sync::Arc<JsonlAuditSink>> {
    let state_dir = match crate::daemon::state::get_state_dir() {
        Ok(d) => d,
        Err(e) => {
            error!("JSONL audit sink: cannot resolve state dir: {e}");
            return None;
        }
    };
    match JsonlAuditSink::new(JsonlSinkConfig::at(state_dir.join("audit.jsonl"))) {
        Ok(sink) => Some(std::sync::Arc::new(sink)),
        Err(e) => {
            error!("JSONL audit sink init failed: {e}");
            None
        }
    }
}

impl AuditSink for JsonlAuditSink {
    fn log_tool_start(
        &self,
        tool_name: &str,
        risk_tier: AuditRiskTier,
        arguments: Option<&str>,
        user_context: Option<UserContext>,
    ) -> String {
        if risk_tier == AuditRiskTier::ReadOnly && !self.config.log_readonly {
            return String::new();
        }
        let mut entry = AuditEntry::new(AuditEventType::ToolStart, risk_tier).with_tool(tool_name);
        if let Some(args) = arguments {
            entry = entry.with_arguments(args);
        }
        if let Some(ctx) = user_context {
            entry = entry.with_user(ctx);
        }
        self.submit(entry)
    }

    fn log_tool_complete(
        &self,
        tool_name: &str,
        risk_tier: AuditRiskTier,
        arguments: Option<&str>,
        result: Option<&str>,
        execution_time: Duration,
        user_context: Option<UserContext>,
    ) {
        if risk_tier == AuditRiskTier::ReadOnly && !self.config.log_readonly {
            return;
        }
        let mut entry = AuditEntry::new(AuditEventType::ToolComplete, risk_tier)
            .with_tool(tool_name)
            .with_execution_time(execution_time);
        if let Some(args) = arguments {
            entry = entry.with_arguments(args);
        }
        if let Some(res) = result {
            entry = entry.with_result(res);
        }
        if let Some(ctx) = user_context {
            entry = entry.with_user(ctx);
        }
        self.submit(entry);
    }

    fn log_tool_error(
        &self,
        tool_name: &str,
        risk_tier: AuditRiskTier,
        arguments: Option<&str>,
        error_message: &str,
        execution_time: Duration,
        user_context: Option<UserContext>,
    ) {
        let mut entry = AuditEntry::new(AuditEventType::ToolError, risk_tier)
            .with_tool(tool_name)
            .with_execution_time(execution_time)
            .with_error(error_message);
        if let Some(args) = arguments {
            entry = entry.with_arguments(args);
        }
        if let Some(ctx) = user_context {
            entry = entry.with_user(ctx);
        }
        self.submit(entry);
    }

    fn log_tool_denied(
        &self,
        tool_name: &str,
        risk_tier: AuditRiskTier,
        reason: &str,
        user_context: Option<UserContext>,
    ) {
        let mut entry = AuditEntry::new(AuditEventType::ToolDenied, risk_tier)
            .with_tool(tool_name)
            .with_error(reason);
        if let Some(ctx) = user_context {
            entry = entry.with_user(ctx);
        }
        self.submit(entry);
    }

    fn log_llm_budget_exceeded(
        &self,
        tool_name: &str,
        risk_tier: AuditRiskTier,
        dimension: &str,
        limit: u64,
        observed: u64,
        user_context: Option<UserContext>,
    ) {
        let mut entry = AuditEntry::new(AuditEventType::LlmBudgetExceeded, risk_tier)
            .with_tool(tool_name)
            .with_arguments(
                serde_json::json!({
                    "dimension": dimension,
                    "limit": limit,
                    "observed": observed,
                })
                .to_string(),
            );
        if let Some(ctx) = user_context {
            entry = entry.with_user(ctx);
        }
        self.submit(entry);
    }

    fn log_network_event(
        &self,
        event_type: AuditEventType,
        listener: &str,
        ip: IpAddr,
        summary: Option<&str>,
    ) -> String {
        let entry = AuditEntry::new(event_type, AuditRiskTier::Internal)
            .with_tool(listener)
            .with_arguments(
                serde_json::json!({ "ip": ip.to_string(), "summary": summary }).to_string(),
            );
        self.submit(entry)
    }

    fn log_path_validation_failed(&self, attempted_path: &str, reason: &str) -> String {
        let entry = AuditEntry::new(
            AuditEventType::PathValidationFailed,
            AuditRiskTier::ConfigChange,
        )
        .with_arguments(
            serde_json::json!({ "path": attempted_path, "reason": reason }).to_string(),
        );
        self.submit(entry)
    }

    fn log_pending_at_crash_batch(&self, pending: &[ReconciledMutation]) -> usize {
        let mut n = 0;
        for m in pending {
            let entry = AuditEntry::new(
                AuditEventType::ConfigMutationPendingAtCrash,
                AuditRiskTier::Internal,
            )
            .with_tool("config_mutation")
            .with_arguments(
                serde_json::json!({
                    "mutation_id": m.id,
                    "intended_revision": m.intended_revision,
                })
                .to_string(),
            );
            self.submit(entry);
            n += 1;
        }
        n
    }

    fn log_plan_created(
        &self,
        plan_id: &str,
        changes_count: usize,
        user_context: Option<UserContext>,
    ) {
        let mut entry = AuditEntry::new(AuditEventType::PlanCreated, AuditRiskTier::ConfigChange)
            .with_arguments(
                serde_json::json!({ "plan_id": plan_id, "changes_count": changes_count })
                    .to_string(),
            );
        if let Some(ctx) = user_context {
            entry = entry.with_user(ctx);
        }
        self.submit(entry);
    }

    fn log_plan_applied(
        &self,
        plan_id: &str,
        changes_applied: usize,
        execution_time: Duration,
        user_context: Option<UserContext>,
        provenance: Option<Provenance>,
    ) {
        let mut entry = AuditEntry::new(AuditEventType::PlanApplied, AuditRiskTier::ConfigChange)
            .with_execution_time(execution_time)
            .with_arguments(
                serde_json::json!({ "plan_id": plan_id, "changes_applied": changes_applied })
                    .to_string(),
            );
        if let Some(p) = provenance {
            entry = entry.with_provenance(p);
        }
        if let Some(ctx) = user_context {
            entry = entry.with_user(ctx);
        }
        self.submit(entry);
    }

    fn log_plan_rejected(&self, plan_id: &str, user_context: Option<UserContext>) {
        let mut entry = AuditEntry::new(AuditEventType::PlanRejected, AuditRiskTier::ConfigChange)
            .with_arguments(serde_json::json!({ "plan_id": plan_id }).to_string());
        if let Some(ctx) = user_context {
            entry = entry.with_user(ctx);
        }
        self.submit(entry);
    }

    fn subscribe(&self) -> broadcast::Receiver<AuditEntry> {
        self.event_tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_at(dir: &tempfile::TempDir) -> JsonlSinkConfig {
        JsonlSinkConfig::at(dir.path().join("audit.jsonl"))
    }

    fn read_lines(path: &Path) -> Vec<JsonlLine> {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).expect("valid line"))
            .collect()
    }

    /// Invariant: writes chain, verify counts them all.
    #[test]
    fn chain_verifies_after_writes() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_at(&dir);
        let path = cfg.path.clone();
        let sink = JsonlAuditSink::new(cfg).unwrap();
        for i in 0..10 {
            sink.log_tool_denied(&format!("tool_{i}"), AuditRiskTier::Stateful, "nope", None);
        }
        drop(sink); // joins the pump: everything durable
        assert_eq!(verify_jsonl_chain(&path), Ok(10));
        let lines = read_lines(&path);
        assert_eq!(lines[0].prev_hash, GENESIS_PREV_HASH);
    }

    /// ADR-027 D13b parity: mid-chain tampering is detected.
    #[test]
    fn tampering_breaks_verification() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_at(&dir);
        let path = cfg.path.clone();
        let sink = JsonlAuditSink::new(cfg).unwrap();
        for _ in 0..3 {
            sink.log_tool_denied("t", AuditRiskTier::Stateful, "no", None);
        }
        drop(sink);
        // Tamper: flip the denial reason in line 2's record.
        let content = std::fs::read_to_string(&path).unwrap();
        let tampered = content.replacen("\"no\"", "\"ok\"", 1);
        assert_ne!(content, tampered, "fixture must actually change a byte");
        std::fs::write(&path, tampered).unwrap();
        match verify_jsonl_chain(&path) {
            Err(JsonlChainBreak::HashMismatch { .. }) => {}
            other => panic!("expected HashMismatch, got {other:?}"),
        }
    }

    /// Crash recovery: a torn final line is truncated on reopen and the
    /// chain continues unbroken across the restart.
    #[test]
    fn torn_tail_truncated_on_reopen_chain_continues() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_at(&dir);
        let path = cfg.path.clone();
        let sink = JsonlAuditSink::new(cfg.clone()).unwrap();
        sink.log_tool_denied("t1", AuditRiskTier::Stateful, "no", None);
        sink.log_tool_denied("t2", AuditRiskTier::Stateful, "no", None);
        drop(sink);
        // Simulate a crash mid-append: garbage partial line, no newline.
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(b"{\"record\":{\"id\":\"torn").unwrap();
        drop(f);

        let sink = JsonlAuditSink::new(cfg).unwrap();
        sink.log_tool_denied("t3", AuditRiskTier::Stateful, "no", None);
        drop(sink);
        assert_eq!(
            verify_jsonl_chain(&path),
            Ok(3),
            "torn line gone, chain whole"
        );
    }

    /// Invariant 4: rotation preserves the chain across segments — the new
    /// segment's first record chains to the rotated segment's head hash.
    #[test]
    fn rotation_chains_across_segments() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = cfg_at(&dir);
        // One line is ~450-550 bytes; a 1200-byte cap rotates once after the
        // third entry, leaving the first entries in `.1` and the rest active.
        cfg.max_segment_bytes = 1200;
        let path = cfg.path.clone();
        let sink = JsonlAuditSink::new(cfg).unwrap();
        for i in 0..4 {
            sink.log_tool_denied(&format!("tool_{i}"), AuditRiskTier::Stateful, "no", None);
        }
        drop(sink);
        let rotated = rotated_path(&path);
        assert!(rotated.exists(), "cap must have rotated exactly once");
        assert_eq!(
            verify_jsonl_chain(&path),
            Ok(4),
            "all entries retained + cross-segment chain intact"
        );
        // Pin the cross-segment link precisely: the active segment's first
        // record chains to the rotated segment's LAST entry hash, and the
        // rotated segment starts at genesis (single rotation).
        let old = read_lines(&rotated);
        let active = read_lines(&path);
        assert_eq!(old.first().unwrap().prev_hash, GENESIS_PREV_HASH);
        assert_eq!(
            active.first().unwrap().prev_hash,
            old.last().unwrap().entry_hash,
            "new segment's first record must chain to predecessor's head hash"
        );
    }

    /// Bounded retention: after MULTIPLE rotations older segments are
    /// discarded; verification covers the retained window and stays
    /// continuous across the surviving pair.
    #[test]
    fn multiple_rotations_keep_retained_window_verifiable() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = cfg_at(&dir);
        cfg.max_segment_bytes = 600; // rotate roughly every entry or two
        let path = cfg.path.clone();
        let sink = JsonlAuditSink::new(cfg).unwrap();
        for i in 0..10 {
            sink.log_tool_denied(&format!("tool_{i}"), AuditRiskTier::Stateful, "no", None);
        }
        drop(sink);
        let retained = verify_jsonl_chain(&path).expect("retained window verifies");
        assert!(retained >= 1, "window must retain the newest entries");
        assert!(retained < 10, "older segments must have been discarded");
    }

    /// Invariant 1: concurrent producers never interleave bytes or break
    /// the chain — the single-writer pump serializes everything.
    #[test]
    fn concurrent_producers_never_interleave() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_at(&dir);
        let path = cfg.path.clone();
        let sink = std::sync::Arc::new(JsonlAuditSink::new(cfg).unwrap());
        let mut handles = Vec::new();
        for t in 0..8 {
            let s = std::sync::Arc::clone(&sink);
            handles.push(std::thread::spawn(move || {
                for i in 0..50 {
                    s.log_tool_denied(
                        &format!("thread{t}_call{i}"),
                        AuditRiskTier::Stateful,
                        "no",
                        None,
                    );
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let dropped = sink.dropped_entries();
        drop(std::sync::Arc::try_unwrap(sink).ok().expect("sole owner"));
        let written = verify_jsonl_chain(&path).expect("chain intact under concurrency");
        assert_eq!(
            written as u64 + dropped,
            400,
            "every entry written or counted"
        );
        assert_eq!(dropped, 0, "channel cap 1024 must not shed 400 entries");
    }

    /// ADR-042 D6 / ADR-027 D13c corpus — secret arguments are redacted
    /// before the line is persisted (and therefore before it is hashed).
    #[test]
    fn d6_secret_arguments_redacted_before_persisting() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_at(&dir);
        let path = cfg.path.clone();
        let sink = JsonlAuditSink::new(cfg).unwrap();
        sink.log_tool_start(
            "conductor_send_osc",
            AuditRiskTier::Stateful,
            Some(r#"{"auth_token": "super-secret-value", "url": "http://example.com/path"}"#),
            None,
        );
        drop(sink);
        let lines = read_lines(&path);
        let args = lines[0].record.arguments.as_deref().unwrap();
        assert!(
            !args.contains("super-secret-value"),
            "secret leaked: {args}"
        );
        assert!(
            args.contains("<redacted:"),
            "redaction marker missing: {args}"
        );
        assert!(
            args.contains("example.com"),
            "non-secret URL must survive: {args}"
        );
        assert_eq!(
            verify_jsonl_chain(&path),
            Ok(1),
            "chain covers redacted bytes"
        );
    }

    /// D13c: secret results are redacted, and redact-THEN-truncate ordering
    /// holds for oversized results (no secret can hide past the cut).
    #[test]
    fn d6_oversize_result_redacts_before_truncating() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_at(&dir);
        let path = cfg.path.clone();
        let sink = JsonlAuditSink::new(cfg).unwrap();
        let padding = "x".repeat(20 * 1024);
        let oversize = format!(r#"{{"padding": "{padding}", "api_key": "tail-secret-value"}}"#);
        sink.log_tool_complete(
            "conductor_get_config",
            AuditRiskTier::ReadOnly,
            None,
            Some(&oversize),
            Duration::from_millis(5),
            None,
        );
        drop(sink);
        let lines = read_lines(&path);
        let result = lines[0].record.result.as_deref().unwrap();
        assert!(
            !result.contains("tail-secret-value"),
            "secret leaked: {result}"
        );
        assert!(
            result.ends_with("...[truncated]"),
            "truncation marker missing"
        );
        assert!(
            result.len() <= 10 * 1024,
            "hard cap exceeded: {}",
            result.len()
        );
    }

    /// D13c: redaction flags can be disabled independently (debug parity
    /// with `AuditLoggerConfig`).
    #[test]
    fn d6_redaction_can_be_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = cfg_at(&dir);
        cfg.redact_arguments = false;
        let path = cfg.path.clone();
        let sink = JsonlAuditSink::new(cfg).unwrap();
        sink.log_tool_start(
            "t",
            AuditRiskTier::Stateful,
            Some(r#"{"auth_token": "visible-on-purpose"}"#),
            None,
        );
        drop(sink);
        let lines = read_lines(&path);
        let args = lines[0].record.arguments.as_deref().unwrap();
        assert!(
            args.contains("visible-on-purpose"),
            "flag must disable redaction"
        );
    }

    /// Parity with `AuditLoggerConfig::log_readonly`: ReadOnly start/complete
    /// events are skipped when disabled; errors are always logged.
    #[test]
    fn readonly_skip_respected() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = cfg_at(&dir);
        cfg.log_readonly = false;
        let path = cfg.path.clone();
        let sink = JsonlAuditSink::new(cfg).unwrap();
        sink.log_tool_start("ro", AuditRiskTier::ReadOnly, None, None);
        sink.log_tool_complete(
            "ro",
            AuditRiskTier::ReadOnly,
            None,
            None,
            Duration::ZERO,
            None,
        );
        sink.log_tool_error(
            "ro",
            AuditRiskTier::ReadOnly,
            None,
            "boom",
            Duration::ZERO,
            None,
        );
        drop(sink);
        assert_eq!(verify_jsonl_chain(&path), Ok(1), "only the error persists");
    }

    /// Live tail: subscribers see entries only after they are durable.
    #[tokio::test]
    async fn subscribe_receives_durable_entries() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg_at(&dir);
        let sink = JsonlAuditSink::new(cfg).unwrap();
        let mut rx = AuditSink::subscribe(&sink);
        sink.log_tool_denied("t", AuditRiskTier::Stateful, "no", None);
        let entry = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("broadcast within 5s")
            .expect("entry");
        assert_eq!(entry.tool_name.as_deref(), Some("t"));
    }
}

#[cfg(all(test, unix))]
mod drop_accounting_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Council on PR #2605: entries lost to disk-append failures must be
    /// counted in `dropped_entries()` — silent loss is an observability gap.
    #[test]
    fn append_failures_are_counted_as_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = JsonlSinkConfig::at(dir.path().join("audit.jsonl"));
        let path = cfg.path.clone();
        let sink = JsonlAuditSink::new(cfg).unwrap();
        // Make the FILE unwritable so per-line append opens fail with
        // EACCES (the constructor's probe already created it 0600).
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
        for _ in 0..3 {
            sink.log_tool_denied("t", AuditRiskTier::Stateful, "no", None);
        }
        // The pump processes asynchronously; poll the counter briefly.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while sink.dropped_entries() < 3 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            sink.dropped_entries(),
            3,
            "every disk-failed entry must be counted"
        );
        drop(sink);
        let contents = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(contents.is_empty(), "nothing may have reached the file");
    }

    /// Arguments are capped like results (redact-then-truncate).
    #[test]
    fn oversize_arguments_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = JsonlSinkConfig::at(dir.path().join("audit.jsonl"));
        let path = cfg.path.clone();
        let sink = JsonlAuditSink::new(cfg).unwrap();
        let huge = format!(r#"{{"blob": "{}"}}"#, "y".repeat(30 * 1024));
        sink.log_tool_start("t", AuditRiskTier::Stateful, Some(&huge), None);
        drop(sink);
        let line = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(line.lines().next().unwrap()).unwrap();
        let args = parsed["record"]["arguments"].as_str().unwrap();
        assert!(
            args.len() <= 10 * 1024,
            "arguments not capped: {}",
            args.len()
        );
        assert!(args.ends_with("...[truncated]"));
    }
}
