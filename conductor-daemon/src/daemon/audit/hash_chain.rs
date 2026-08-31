// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-027 D13b — append-only hash chain over the audit log.
//!
//! Each audit entry stores `prev_hash` (the previous entry's
//! `entry_hash`) and `entry_hash` (sha256 of `prev_hash` ||
//! canonical-bytes-of-this-entry). A reader can walk the table
//! and recompute every hash, detecting any tamper:
//!
//! - Mutation of any entry's content WITHOUT also recomputing
//!   `entry_hash`: stored value no longer matches the
//!   re-derivation from canonical bytes.
//! - Deletion or reorder WITHOUT also fixing up subsequent
//!   rows' `prev_hash` links: the next entry's `prev_hash`
//!   doesn't match the actual previous row's `entry_hash`.
//! - Insertion of a forged row WITHOUT also rebuilding the
//!   chain forward: the forged row's `prev_hash` doesn't match
//!   the actual prev, or the next legitimate row's `prev_hash`
//!   no longer matches the forged row's `entry_hash`.
//!
//! PR #1031 round-4 review (2026-05-02): the "WITHOUT also
//! fixing up" qualifier is load-bearing. An attacker with
//! write access to the SQLite file can also READ it, compute
//! the legitimate hash for any modified row, and rebuild the
//! chain forward to mask the tamper. D13b's actual guarantee
//! is "tamper-evidence for casual UPDATE-without-fixup", not
//! "tamper-prevention against any privileged adversary".
//! Stronger guarantees (signed entries, periodic external
//! anchoring) are tracked under D15.
//!
//! Closes Finding F-10 (audit log tamperability). Caveats:
//!
//! - This protects against tamper *detection*, not *prevention*.
//!   An attacker with write access to the SQLite file can still
//!   modify entries; the chain just makes the modification
//!   visible the next time `verify_chain` runs.
//! - On Linux the spec also calls for `chattr +a` (append-only
//!   inode flag) where supported. Deferred to a follow-up — it
//!   conflicts with SQLite WAL mode without careful handling and
//!   isn't on the critical path for the cryptographic guarantee.
//! - Genesis entry uses the all-zeros prev_hash. An attacker who
//!   wholesale replaces the table from row 1 onward can present
//!   a valid alternate chain — that's the standard limit of any
//!   single-machine append-only log; mitigations (anchoring to a
//!   remote hash periodically, or signing) are out of scope here.

use sha2::{Digest, Sha256};

/// What went wrong while walking the audit hash chain. Returned
/// by [`AuditLogger::verify_chain`] when integrity is broken;
/// see those docs for usage. Carries the failing row's id so an
/// operator triaging a chain break can locate the suspect entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainBreak {
    /// First row's `prev_hash` doesn't match
    /// [`GENESIS_PREV_HASH`]. Suggests the genesis row was
    /// rewritten or the table's row order was reversed.
    BrokenGenesis {
        entry_id: String,
        found_prev: String,
    },
    /// A row's stored `prev_hash` doesn't match the previous
    /// row's stored `entry_hash`. Suggests deletion, reorder,
    /// or insertion of a forged row.
    BrokenLink {
        entry_id: String,
        expected_prev: String,
        found_prev: String,
    },
    /// A row's stored `entry_hash` doesn't match a re-derivation
    /// from `(prev_hash, canonical_entry_bytes)`. Suggests the
    /// row's content was modified after write.
    HashMismatch {
        entry_id: String,
        recomputed: String,
        stored: String,
    },
    /// A row in a v2 schema is missing one or both hash columns.
    /// Acceptable for v1-era rows during the migration window;
    /// fatal once v2 is the established baseline.
    MissingHash { entry_id: String },
    /// A row's `execution_time_ms` column is negative — likely
    /// corruption or tamper (PR #1031 review, 2026-05-02). Pre-
    /// fix this would silently wrap to a huge `Duration`; now
    /// it's surfaced as an integrity failure so the operator
    /// triaging the audit log knows the row is suspect.
    NegativeExecutionTime { entry_id: String, ms: i64 },
    /// `verify_chain` failed for a reason unrelated to chain
    /// integrity — DB lock poisoning, prepared-statement error,
    /// row-decode failure (PR #1031 review, 2026-05-02). Pre-
    /// fix these were lossy-mapped to `MissingHash`, which
    /// confused triage. `details` is a human-readable
    /// description; the verifier doesn't mint a structured
    /// SqliteError because that crate's error type isn't
    /// `Clone + PartialEq` and we want this enum to derive both.
    DbError { details: String },
}

impl std::fmt::Display for ChainBreak {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BrokenGenesis {
                entry_id,
                found_prev,
            } => write!(
                f,
                "audit chain: genesis row {entry_id:?} has prev_hash \
                 {found_prev:?}; expected {GENESIS_PREV_HASH:?}",
            ),
            Self::BrokenLink {
                entry_id,
                expected_prev,
                found_prev,
            } => write!(
                f,
                "audit chain: row {entry_id:?} has prev_hash \
                 {found_prev:?}; previous row's entry_hash was \
                 {expected_prev:?} (deletion or reorder)",
            ),
            Self::HashMismatch {
                entry_id,
                recomputed,
                stored,
            } => write!(
                f,
                "audit chain: row {entry_id:?} stored entry_hash \
                 {stored:?}; recomputed from canonical bytes is \
                 {recomputed:?} (content tamper)",
            ),
            Self::MissingHash { entry_id } => write!(
                f,
                "audit chain: row {entry_id:?} has NULL \
                 prev_hash or entry_hash (corrupt or \
                 partially-written row encountered after \
                 v2 chain started)",
            ),
            Self::NegativeExecutionTime { entry_id, ms } => write!(
                f,
                "audit chain: row {entry_id:?} has negative \
                 execution_time_ms={ms} — likely corruption or \
                 tamper",
            ),
            Self::DbError { details } => write!(
                f,
                "audit chain: verifier could not read the chain \
                 due to a non-integrity error: {details}",
            ),
        }
    }
}

impl std::error::Error for ChainBreak {}

/// All-zeros 64-char hex sentinel used as the "previous hash"
/// for the very first audit entry in a fresh database — there's
/// no prior entry to chain from. (PR #1031 review fix: the
/// previous comment described this as "lowercase-hex sha256 of
/// an empty input", which is wrong — sha256("") is `e3b0...`,
/// not all-zeros. This constant is a deliberately-chosen
/// sentinel, not a hash of anything.)
///
/// Concrete constant (rather than `String::new()` or similar)
/// so a table-scrub attack that resets the first row's
/// `prev_hash` gets caught: only this exact 64-char string is
/// acceptable as genesis.
pub const GENESIS_PREV_HASH: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

/// Persisted-form columns of an audit entry, as they live in
/// SQLite. Both `insert_entry` and `verify_chain` hash THIS
/// view of the row, never the in-memory [`AuditEntry`] struct.
///
/// PR #1031 review (2026-05-02): the previous version hashed
/// `&AuditEntry` directly. That created undetectable tamper
/// surface because Rust→DB→Rust round-trips are LOSSY:
///
/// - `Duration` → `INTEGER ms` rounds sub-millisecond precision
///   (so `Duration::from_micros(1500)` writes `1` and reads
///   back as `Duration::from_millis(1)` — different bytes).
/// - `AuditEventType` parses unknown strings to `ToolStart`
///   and `AuditRiskTier` to `ReadOnly`, so an attacker mutating
///   the stored string to garbage gets the same canonical
///   bytes as the original.
/// - `UserContext` rehydrates invalid JSON to `None`, so
///   mutating a `NULL` to garbage hashes identically.
///
/// By hashing the persisted form (raw strings as written, ms
/// as the actual stored integer, JSON as the actual stored
/// blob), every byte-level mutation in the DB shows up as a
/// hash mismatch. Both insert (using the values bound to the
/// SQL params) and verify (using the values read from the SQL
/// columns) build the same `CanonicalRow` and hash it.
#[derive(serde::Serialize)]
pub(crate) struct CanonicalRow<'a> {
    pub id: &'a str,
    /// Persisted as the raw string — `event_type.as_str()` on
    /// insert, `row.get(1)` on verify. Lossy parsing is gone.
    pub event_type: &'a str,
    pub tool_name: Option<&'a str>,
    /// Persisted as the raw JSON string from `to_json()` /
    /// `row.get(...)`, NOT a parsed `UserContext`. A mutation
    /// of the JSON blob (or NULL → garbage) is now detectable.
    pub user_context: Option<&'a str>,
    pub arguments: Option<&'a str>,
    pub result: Option<&'a str>,
    /// Persisted as the raw string. Same rationale as event_type.
    pub risk_tier: &'a str,
    pub is_error: i32,
    pub error_message: Option<&'a str>,
    /// Persisted as `Option<i64>` ms — same shape as the SQL
    /// column. Sub-millisecond precision in `Duration::elapsed()`
    /// no longer matters here: insert and verify both see the
    /// same ms value.
    pub execution_time_ms: Option<i64>,
    pub created_at: i64,
    /// Persisted as the raw `provenance` TEXT (the serialized
    /// `Provenance { initiator, source, peer }` JSON), exactly as
    /// written to / read from the v3 column (#2120). Including it
    /// makes provenance tamper-evident — modifying the column now
    /// breaks `verify_chain`.
    ///
    /// `skip_serializing_if = "Option::is_none"` is load-bearing:
    /// a `None` provenance omits the field entirely, so the
    /// canonical JSON is byte-identical to a pre-v3 / no-provenance
    /// row. Every existing row (v2, and v3 rows that carry no
    /// provenance) therefore still verifies unchanged; only rows
    /// that actually carry a `Provenance` gain the extra segment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<&'a str>,
}

/// Build the canonical byte representation for hashing. Excludes
/// `prev_hash` and `entry_hash` themselves — those are
/// bookkeeping fields that we're about to compute; including
/// them would be either a recursive reference or trivially
/// defeatable.
///
/// JSON over raw concatenation: (1) field-rename or field-add
/// to [`CanonicalRow`] won't silently invalidate every prior
/// chain — the JSON output explicitly names every field, so a
/// schema bump shows up as a verifier-detectable change rather
/// than a silent corruption; (2) the canonical byte stream is
/// a debuggable artifact during chain-integrity investigations.
///
/// PR #1031 review: `panic!` on serde failure rather than
/// silently producing empty bytes. CanonicalRow is a fixed,
/// finite-size shape with no failable serializations
/// (primitives + Option<&str>); the only way `to_vec` can fail
/// is OOM or programmer error (e.g. someone adds a field with
/// a custom Serialize that panics). Both are bugs that
/// shouldn't be smoothed over.
pub(crate) fn canonical_persisted_bytes(row: &CanonicalRow<'_>) -> Vec<u8> {
    serde_json::to_vec(row).expect(
        "ADR-027 D13b: canonical hash bytes failed to serialize. \
         CanonicalRow is a primitive-only shape; serde_json failure \
         here indicates a programmer/build error (likely a new \
         non-trivially-serializable field added). Refusing to hash \
         empty bytes — that would silently weaken the chain.",
    )
}

/// Compute the entry hash given the previous entry's hash and
/// this entry's canonical bytes. Hash format: lowercase hex of
/// sha256(prev_hash_ascii_bytes || 0x0a || canonical_bytes).
///
/// The 0x0a separator (newline) prevents a length-extension or
/// concatenation ambiguity: without it, `prev_hash="00...A"` +
/// content `"BC"` and `prev_hash="00...AB"` + content `"C"`
/// would hash identically. With the separator they don't.
pub fn compute_entry_hash(prev_hash: &str, content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(prev_hash.as_bytes());
    hasher.update(b"\n"); // unambiguous separator
    hasher.update(content);
    hex::encode(hasher.finalize())
}

/// Verify a single persisted-form row against the prior
/// entry's hash. Returns `true` if the row's recomputed hash
/// matches its stored `entry_hash` AND its `prev_hash` matches
/// the supplied `expected_prev`.
///
/// PR #1031 review (2026-05-02): the previous version took
/// `&AuditEntry` (an in-memory parsed struct) which created
/// undetectable tamper surface from lossy decoding (see
/// [`CanonicalRow`] docs). This now takes a [`CanonicalRow`]
/// built directly from the stored column values — every byte
/// of the persisted row contributes to the hash.
///
/// The `stored_prev_hash` / `stored_entry_hash` parameters are
/// `&str` (not `Option<&str>`); pre-D13b legacy rows without
/// hash columns are detected by the caller via
/// [`super::ChainBreak::MissingHash`] before reaching this
/// function — they never produce an `Option` here.
// Currently used only by the in-module unit tests; kept as a
// reusable building block in case a future caller wants
// per-row verification (e.g. a streaming audit replicator that
// validates rows as they arrive). Marking dead-code-allowed
// rather than deleting keeps the test suite documenting the
// per-row invariant directly.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn verify_entry(
    row: &CanonicalRow<'_>,
    expected_prev: &str,
    stored_prev_hash: &str,
    stored_entry_hash: &str,
) -> bool {
    if stored_prev_hash != expected_prev {
        return false;
    }
    let recomputed = compute_entry_hash(stored_prev_hash, &canonical_persisted_bytes(row));
    recomputed == stored_entry_hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_row<'a>(id: &'a str, tool: &'a str) -> CanonicalRow<'a> {
        CanonicalRow {
            id,
            event_type: "tool_start",
            tool_name: Some(tool),
            user_context: None,
            arguments: Some(r#"{"input":"hello"}"#),
            result: None,
            risk_tier: "read_only",
            is_error: 0,
            error_message: None,
            execution_time_ms: None,
            created_at: 1_700_000_000_000,
            provenance: None,
        }
    }

    // ─── canonical bytes ────────────────────────────────────

    #[test]
    fn canonical_bytes_are_deterministic_for_same_row() {
        // Hashing requires byte-stable serialization. Two
        // independent calls on the same row MUST produce the
        // same bytes — otherwise `compute_entry_hash` is
        // non-deterministic and chain verification fails for
        // rows that were valid when written.
        let r = fixture_row("a", "conductor_status");
        let bytes_a = canonical_persisted_bytes(&r);
        let bytes_b = canonical_persisted_bytes(&r);
        assert_eq!(bytes_a, bytes_b);
    }

    #[test]
    fn canonical_bytes_differ_when_any_field_differs() {
        // Anti-tamper: the verifier must be able to distinguish
        // rows that have DIFFERENT content. If two rows hash
        // identically the chain doesn't actually detect mutation.
        let original = fixture_row("a", "conductor_status");
        let original_bytes = canonical_persisted_bytes(&original);
        let mutated = CanonicalRow {
            tool_name: Some("conductor_get_config"),
            ..fixture_row("a", "conductor_status")
        };
        let mutated_bytes = canonical_persisted_bytes(&mutated);
        assert_ne!(
            original_bytes, mutated_bytes,
            "ADR-027 D13b: canonical_persisted_bytes MUST \
             distinguish rows with different content; if \
             identical, the chain is silently tamper-blind",
        );
    }

    #[test]
    fn canonical_bytes_distinguish_event_type_string_mutation() {
        // PR #1031 review (2026-05-02): the previous
        // `canonical_entry_bytes` took a parsed `AuditEventType`,
        // and unknown strings parsed to `ToolStart`. An attacker
        // could rewrite `event_type` from `"tool_start"` to
        // `"garbage"` and the verifier (rebuilding the entry from
        // the DB) would parse it as `ToolStart` — same canonical
        // bytes. Now we hash the raw string, so this mutation
        // shows up.
        let original = fixture_row("a", "tool");
        let mutated = CanonicalRow {
            event_type: "garbage_unparseable",
            ..fixture_row("a", "tool")
        };
        assert_ne!(
            canonical_persisted_bytes(&original),
            canonical_persisted_bytes(&mutated),
            "string-level event_type mutation MUST be detected",
        );
    }

    #[test]
    fn canonical_bytes_distinguish_user_context_blob_mutation() {
        // Same lossy-decode hole as event_type: previous version
        // parsed the JSON to UserContext, so invalid JSON
        // round-tripped to `None` and a NULL→garbage mutation
        // was undetectable. Now we hash the raw blob.
        let with_null = fixture_row("a", "tool");
        let with_garbage = CanonicalRow {
            user_context: Some("not valid json {"),
            ..fixture_row("a", "tool")
        };
        assert_ne!(
            canonical_persisted_bytes(&with_null),
            canonical_persisted_bytes(&with_garbage),
        );
    }

    #[test]
    fn canonical_bytes_distinguish_sub_ms_execution_time() {
        // Sub-millisecond precision wasn't preserved by the old
        // `Duration → ms → Duration` round-trip, so legit entries
        // with non-zero nanos failed verification. Now we hash
        // the persisted ms value directly.
        let row_1ms = CanonicalRow {
            execution_time_ms: Some(1),
            ..fixture_row("a", "tool")
        };
        let row_2ms = CanonicalRow {
            execution_time_ms: Some(2),
            ..fixture_row("a", "tool")
        };
        assert_ne!(
            canonical_persisted_bytes(&row_1ms),
            canonical_persisted_bytes(&row_2ms),
        );
    }

    // ─── compute_entry_hash ────────────────────────────────

    #[test]
    fn entry_hash_is_64_hex_chars() {
        let h = compute_entry_hash(GENESIS_PREV_HASH, b"some content");
        assert_eq!(h.len(), 64);
        assert!(
            h.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }

    #[test]
    fn entry_hash_changes_when_prev_changes() {
        // Each entry's hash depends on the previous entry's
        // hash — that's how mutating any entry cascades to all
        // following entries' verification failures.
        let h1 = compute_entry_hash(GENESIS_PREV_HASH, b"same content");
        let h2 = compute_entry_hash(
            "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            b"same content",
        );
        assert_ne!(h1, h2);
    }

    #[test]
    fn entry_hash_changes_when_content_changes() {
        let h1 = compute_entry_hash(GENESIS_PREV_HASH, b"content A");
        let h2 = compute_entry_hash(GENESIS_PREV_HASH, b"content B");
        assert_ne!(h1, h2);
    }

    #[test]
    fn entry_hash_separator_prevents_concatenation_ambiguity() {
        // Without a separator between prev_hash and content,
        // `(prev="00...01", content="23")` and
        // `(prev="00...0123", content="")` (or any similar
        // boundary shift) would hash identically. The 0x0a
        // separator we insert prevents this. Verify by
        // constructing the boundary-confusion case.
        //
        // prev_hash A is 64 hex chars; content is "extra".
        // prev_hash B is 64 hex + "extra"; content is empty.
        // With a separator, these MUST hash differently.
        let prev_a = "0000000000000000000000000000000000000000000000000000000000000001";
        let content_a = b"extra";
        let prev_b_full = format!(
            "{prev_a}{content}",
            content = std::str::from_utf8(content_a).unwrap()
        );
        let h_a = compute_entry_hash(prev_a, content_a);
        let h_b = compute_entry_hash(&prev_b_full, b"");
        assert_ne!(
            h_a, h_b,
            "ADR-027 D13b: hash MUST distinguish prev/content \
             boundary; if identical, an attacker can shift bytes \
             between prev_hash and content and forge entries",
        );
    }

    // ─── verify_entry ──────────────────────────────────────

    #[test]
    fn verify_entry_accepts_legitimate_row() {
        let r = fixture_row("a", "conductor_status");
        let prev = GENESIS_PREV_HASH;
        let entry_hash = compute_entry_hash(prev, &canonical_persisted_bytes(&r));
        assert!(
            verify_entry(&r, prev, prev, &entry_hash),
            "legitimate row must verify",
        );
    }

    #[test]
    fn verify_entry_rejects_mutated_content() {
        // The classic D13b detection: an attacker modified an
        // existing row's content but left prev_hash and
        // entry_hash unchanged.
        let original = fixture_row("a", "conductor_status");
        let prev = GENESIS_PREV_HASH;
        let entry_hash_of_original =
            compute_entry_hash(prev, &canonical_persisted_bytes(&original));

        // Attacker mutates the row's content but leaves
        // entry_hash unchanged.
        let tampered = CanonicalRow {
            tool_name: Some("conductor_send_midi"),
            ..fixture_row("a", "conductor_status")
        };

        assert!(
            !verify_entry(&tampered, prev, prev, &entry_hash_of_original),
            "ADR-027 D13b: verify_entry MUST detect content \
             mutation when entry_hash is unchanged. If this \
             passes, the integrity check is broken.",
        );
    }

    #[test]
    fn verify_entry_rejects_broken_prev_hash_link() {
        // Detection of deletion / reorder: an attacker removed
        // an earlier row, so the supplied `expected_prev`
        // (the actual previous row's entry_hash) doesn't match
        // what's stored in this row's prev_hash field.
        let r = fixture_row("a", "conductor_status");
        let stored_prev = "1111111111111111111111111111111111111111111111111111111111111111";
        let expected_prev = GENESIS_PREV_HASH;
        let entry_hash = compute_entry_hash(stored_prev, &canonical_persisted_bytes(&r));

        assert!(
            !verify_entry(&r, expected_prev, stored_prev, &entry_hash),
            "ADR-027 D13b: verify_entry MUST detect a broken \
             prev_hash link (stored prev_hash differs from \
             actual previous row's entry_hash)",
        );
    }

    #[test]
    fn verify_entry_rejects_recomputed_entry_hash_mismatch() {
        // An attacker mutates the row AND updates entry_hash —
        // but using the wrong canonical input. The recomputed
        // hash from canonical bytes won't match the (forged)
        // stored value.
        let r = fixture_row("a", "conductor_status");
        let prev = GENESIS_PREV_HASH;
        let forged_hash = "deadbeef".repeat(8); // 64 hex chars but bogus
        assert!(
            !verify_entry(&r, prev, prev, &forged_hash),
            "ADR-027 D13b: verify_entry MUST recompute the hash \
             from canonical bytes and compare; a forged hash \
             that doesn't match the recomputation must fail.",
        );
    }
}
