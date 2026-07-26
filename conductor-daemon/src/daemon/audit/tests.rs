// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Integration tests for audit logging

use super::*;
use std::time::Duration;

#[test]
fn test_audit_logger_persistence() {
    // Create a temp file for the database
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test_audit.db");

    // Create logger and add entries
    {
        let config = AuditLoggerConfig {
            db_path: db_path.clone(),
            ..Default::default()
        };
        let logger = AuditLogger::new(config).unwrap();

        logger.log_tool_complete(
            "test_tool",
            AuditRiskTier::ReadOnly,
            Some(r#"{"test": true}"#),
            Some(r#"{"result": "ok"}"#),
            Duration::from_millis(50),
            Some(UserContext::local_user()),
        );

        assert_eq!(logger.count().unwrap(), 1);
    }

    // Reopen and verify entries persist
    {
        let config = AuditLoggerConfig {
            db_path: db_path.clone(),
            ..Default::default()
        };
        let logger = AuditLogger::new(config).unwrap();

        assert_eq!(logger.count().unwrap(), 1);

        let entries = logger.query(&AuditQuery::default()).unwrap();
        assert_eq!(entries[0].tool_name, Some("test_tool".to_string()));
    }
}

#[test]
fn test_audit_cleanup() {
    let logger = AuditLogger::in_memory().unwrap();

    // Log an entry
    logger.log_tool_complete(
        "old_tool",
        AuditRiskTier::ReadOnly,
        None,
        None,
        Duration::from_millis(10),
        None,
    );

    // Cleanup with 0 day retention should remove everything
    // Note: We can't easily test time-based cleanup in unit tests
    // without manipulating the created_at timestamp directly

    assert!(logger.count().unwrap() >= 1);
}

#[test]
fn test_concurrent_logging() {
    use std::sync::Arc;
    use std::thread;

    let logger = Arc::new(AuditLogger::in_memory().unwrap());
    let mut handles = vec![];

    // Spawn multiple threads logging concurrently
    for i in 0..10 {
        let logger = Arc::clone(&logger);
        handles.push(thread::spawn(move || {
            for j in 0..10 {
                logger.log_tool_complete(
                    &format!("tool_{}_{}", i, j),
                    AuditRiskTier::ReadOnly,
                    None,
                    None,
                    Duration::from_millis(10),
                    None,
                );
            }
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }

    // Should have 100 entries
    assert_eq!(logger.count().unwrap(), 100);
}

#[test]
fn test_time_range_query() {
    let logger = AuditLogger::in_memory().unwrap();

    // Log entry with known timestamp
    logger.log_tool_complete(
        "tool1",
        AuditRiskTier::ReadOnly,
        None,
        None,
        Duration::from_millis(10),
        None,
    );

    let now = chrono::Utc::now().timestamp_millis();

    // Query with time range including now
    let entries = logger
        .query(&AuditQuery {
            start_time: Some(now - 60000), // 1 minute ago
            end_time: Some(now + 60000),   // 1 minute from now
            ..Default::default()
        })
        .unwrap();

    assert_eq!(entries.len(), 1);

    // Query with time range in the past (should find nothing)
    let entries = logger
        .query(&AuditQuery {
            start_time: Some(now - 120000), // 2 minutes ago
            end_time: Some(now - 60000),    // 1 minute ago
            ..Default::default()
        })
        .unwrap();

    assert_eq!(entries.len(), 0);
}

#[test]
fn test_pagination() {
    let logger = AuditLogger::in_memory().unwrap();

    // Log 25 entries
    for i in 0..25 {
        logger.log_tool_complete(
            &format!("tool_{}", i),
            AuditRiskTier::ReadOnly,
            None,
            None,
            Duration::from_millis(10),
            None,
        );
    }

    // First page (10 entries)
    let page1 = logger
        .query(&AuditQuery {
            limit: Some(10),
            offset: Some(0),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(page1.len(), 10);

    // Second page
    let page2 = logger
        .query(&AuditQuery {
            limit: Some(10),
            offset: Some(10),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(page2.len(), 10);

    // Third page (only 5 remaining)
    let page3 = logger
        .query(&AuditQuery {
            limit: Some(10),
            offset: Some(20),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(page3.len(), 5);

    // Verify no duplicates
    let all_ids: std::collections::HashSet<_> = page1
        .iter()
        .chain(page2.iter())
        .chain(page3.iter())
        .map(|e| e.id.clone())
        .collect();
    assert_eq!(all_ids.len(), 25);
}

#[test]
fn test_denied_logging() {
    let logger = AuditLogger::in_memory().unwrap();

    logger.log_tool_denied(
        "conductor_send_sysex",
        AuditRiskTier::HardwareIO,
        "User denied hardware access",
        Some(UserContext::local_user()),
    );

    let entries = logger
        .query(&AuditQuery {
            event_type: Some(AuditEventType::ToolDenied),
            ..Default::default()
        })
        .unwrap();

    assert_eq!(entries.len(), 1);
    assert!(entries[0].is_error);
    assert_eq!(
        entries[0].error_message,
        Some("User denied hardware access".to_string())
    );
}

// ─── ADR-027 D13b — append-only hash chain ───────────────────

#[test]
fn d13b_chain_verifies_for_legitimate_writes() {
    let logger = AuditLogger::in_memory().unwrap();

    // Genesis entry.
    logger.log_tool_complete(
        "conductor_status",
        AuditRiskTier::ReadOnly,
        None,
        Some(r#"{"running":true}"#),
        Duration::from_millis(2),
        Some(UserContext::local_user()),
    );
    // Two more entries — the chain should bind them all.
    logger.log_tool_complete(
        "conductor_get_config",
        AuditRiskTier::ReadOnly,
        None,
        Some(r#"{"version":1}"#),
        Duration::from_millis(5),
        Some(UserContext::local_user()),
    );
    logger.log_tool_denied(
        "conductor_send_midi",
        AuditRiskTier::HardwareIO,
        "Confirmation expired",
        Some(UserContext::local_user()),
    );

    let n = logger.verify_chain().unwrap_or_else(|e| {
        panic!(
            "ADR-027 D13b: chain over 3 fresh entries must verify; \
             got break: {e}",
        )
    });
    assert_eq!(n, 3, "verify_chain must report all 3 entries");
}

#[test]
fn d13b_empty_chain_verifies_trivially() {
    // Edge: a fresh DB has no entries; the chain is vacuously
    // valid. verify_chain returns Ok(0) so callers can
    // distinguish "checked, found nothing" from "didn't run".
    let logger = AuditLogger::in_memory().unwrap();
    assert_eq!(logger.verify_chain().unwrap(), 0);
}

#[test]
fn d13b_chain_detects_content_tamper() {
    // Direct-DB tamper: a privileged attacker UPDATEs an audit
    // row's `tool_name` (or `arguments`, etc.) without
    // recomputing the hash. verify_chain must report this as
    // HashMismatch.
    let logger = AuditLogger::in_memory().unwrap();

    logger.log_tool_complete(
        "conductor_status",
        AuditRiskTier::ReadOnly,
        None,
        Some(r#"{"ok":true}"#),
        Duration::from_millis(2),
        Some(UserContext::local_user()),
    );
    logger.log_tool_complete(
        "conductor_get_config",
        AuditRiskTier::ReadOnly,
        None,
        Some(r#"{"benign":true}"#),
        Duration::from_millis(3),
        Some(UserContext::local_user()),
    );

    // Tamper scenario: simulate an attacker modifying the
    // second entry's `tool_name` directly via SQL but NOT
    // updating the row's stored entry_hash to match. The
    // recomputation from canonical bytes mismatches the
    // stored value → HashMismatch.
    //
    // PR #1031 review (2026-05-02): the previous comment said
    // an attacker "can't recompute a valid entry_hash" — that
    // overstates the threat model. A pure unkeyed hash chain
    // does NOT prevent a privileged DB writer from updating a
    // row AND recomputing prev_hash/entry_hash for that row
    // and every subsequent row, making `verify_chain()` pass
    // again. D13b's guarantee is "tamper-evident UNLESS the
    // attacker bothers to fix up the chain": detection-on-
    // demand for the casual UPDATE-without-fixup attack.
    // Stronger guarantees (signed entries, append-only FS,
    // periodic external anchoring) are out of scope for D13b
    // and tracked under D15 (remote anchoring) and follow-up
    // platform-specific append-only enforcement.
    {
        let conn = logger.conn_for_tests();
        conn.execute(
            "UPDATE audit_log SET tool_name = 'malicious_tool_substituted' \
             WHERE tool_name = 'conductor_get_config'",
            [],
        )
        .unwrap();
    }

    let result = logger.verify_chain();
    match result {
        Err(super::ChainBreak::HashMismatch { .. }) => {
            // expected
        }
        Err(other) => panic!(
            "ADR-027 D13b: tamper of `tool_name` must be detected as \
             HashMismatch (the row's stored entry_hash no longer \
             matches the recomputation). Got: {other}",
        ),
        Ok(n) => panic!(
            "ADR-027 D13b: tamper went UNDETECTED — verify_chain \
             returned Ok({n}). The hash chain isn't actually \
             gating integrity; an attacker can rewrite arbitrary \
             entries without leaving a trace.",
        ),
    }
}

#[test]
fn d13b_chain_detects_provenance_tamper() {
    // #2120 (clawpatch #2103): the v3 `provenance` column records who
    // initiated a mutation. Pre-fix it was EXCLUDED from the hash chain, so a
    // privileged attacker could rewrite an entry's provenance — forging "this
    // config change came from the CLI" onto an LLM-initiated change, say —
    // without `verify_chain` noticing. The provenance is now part of the
    // canonical bytes, so a direct-DB tamper must surface as HashMismatch.
    use conductor_core::config::{Initiator, Provenance, Source};

    let logger = AuditLogger::in_memory().unwrap();

    // An entry that carries provenance (initiator = CLI).
    logger.log_plan_applied(
        "plan-abc",
        2,
        Duration::from_millis(5),
        Some(UserContext::local_user()),
        Some(Provenance {
            initiator: Initiator::Cli,
            source: Source::InMemoryEdit,
            peer: None,
        }),
    );

    // Sanity: the freshly-written chain verifies.
    assert_eq!(
        logger.verify_chain().unwrap(),
        1,
        "the provenance-bearing entry must hash into the chain"
    );

    // Tamper: rewrite the stored provenance to forge a different initiator,
    // WITHOUT recomputing the row's entry_hash.
    {
        let conn = logger.conn_for_tests();
        let forged = serde_json::to_string(&Provenance {
            initiator: Initiator::Daemon,
            source: Source::InMemoryEdit,
            peer: None,
        })
        .unwrap();
        let n = conn
            .execute(
                "UPDATE audit_log SET provenance = ?1 WHERE provenance IS NOT NULL",
                [&forged],
            )
            .unwrap();
        assert_eq!(
            n, 1,
            "the tamper must hit exactly the provenance-bearing row"
        );
    }

    match logger.verify_chain() {
        Err(super::ChainBreak::HashMismatch { .. }) => { /* expected */ }
        Err(other) => {
            panic!("#2120: provenance tamper must be detected as HashMismatch, got: {other}")
        }
        Ok(n) => panic!(
            "#2120: provenance tamper went UNDETECTED — verify_chain returned \
             Ok({n}). The provenance column is outside the hash chain; an \
             attacker can forge mutation provenance without a trace.",
        ),
    }
}

#[test]
fn d13b_chain_unaffected_for_entries_without_provenance() {
    // Backward-compat guard for the #2120 fix: entries with NO provenance
    // (the common case, and every v2-era row) must hash byte-identically to
    // before — `skip_serializing_if` omits the field, so the canonical bytes
    // are unchanged and the chain still verifies.
    let logger = AuditLogger::in_memory().unwrap();
    logger.log_tool_complete(
        "conductor_status",
        AuditRiskTier::ReadOnly,
        None,
        Some(r#"{"ok":true}"#),
        Duration::from_millis(1),
        Some(UserContext::local_user()),
    );
    assert_eq!(
        logger.verify_chain().unwrap(),
        1,
        "a provenance-free entry must verify unchanged after the #2120 fix"
    );
}

#[test]
fn d13b_chain_detects_deletion_breaking_link() {
    // Direct-DB tamper: an attacker deletes a middle row to
    // hide an event. The next row's `prev_hash` still points
    // at the deleted row's `entry_hash`, but that row is gone
    // — verify_chain should now see a chain link broken.
    let logger = AuditLogger::in_memory().unwrap();
    for i in 0..3 {
        logger.log_tool_complete(
            &format!("tool_{i}"),
            AuditRiskTier::ReadOnly,
            None,
            Some(&format!(r#"{{"i":{i}}}"#)),
            Duration::from_millis(1),
            Some(UserContext::local_user()),
        );
    }
    assert_eq!(logger.verify_chain().unwrap(), 3);

    // Delete the middle entry.
    {
        let conn = logger.conn_for_tests();
        conn.execute("DELETE FROM audit_log WHERE tool_name = 'tool_1'", [])
            .unwrap();
    }

    let result = logger.verify_chain();
    match result {
        Err(super::ChainBreak::BrokenLink { .. }) => { /* expected */ }
        Ok(n) => panic!(
            "ADR-027 D13b: deletion of a middle audit row went \
             UNDETECTED — verify_chain returned Ok({n}). The chain \
             isn't actually catching deletes.",
        ),
        Err(other) => panic!("ADR-027 D13b: deletion should surface as BrokenLink; got {other}",),
    }
}

#[test]
fn d13b_cleanup_re_roots_chain_so_verify_still_passes() {
    // PR #1031 round-2 review (2026-05-02): retention
    // `cleanup()` deletes old rows; pre-fix this left the
    // first surviving row's `prev_hash` pointing at a
    // now-missing previous-row's `entry_hash`, so
    // `verify_chain` returned BrokenLink — indistinguishable
    // from attacker deletion. Now `cleanup()` re-roots the
    // chain at the first surviving row so verify still
    // passes.
    let logger = AuditLogger::in_memory().unwrap();

    // Insert 3 rows. We can't easily backdate the first one's
    // created_at without a direct SQL UPDATE, so simulate
    // retention deletion by directly DELETE-ing the first
    // row, then exercise the same re-root logic by calling
    // a helper that mirrors cleanup()'s code path. Easier:
    // backdate row 1 via SQL, set max_age_days low, and let
    // cleanup() do its real thing.
    for i in 0..3 {
        logger.log_tool_complete(
            &format!("tool_{i}"),
            AuditRiskTier::ReadOnly,
            None,
            Some(&format!(r#"{{"i":{i}}}"#)),
            Duration::from_millis(1),
            Some(UserContext::local_user()),
        );
    }
    assert_eq!(logger.verify_chain().unwrap(), 3);

    // Backdate the first row to before the retention cutoff so
    // cleanup() will delete it.
    {
        let conn = logger.conn_for_tests();
        // 100 days ago — well past the default 90-day cutoff.
        let old = chrono::Utc::now().timestamp_millis() - (100 * 24 * 60 * 60 * 1000);
        conn.execute(
            "UPDATE audit_log SET created_at = ?1 WHERE tool_name = 'tool_0'",
            [old],
        )
        .unwrap();
    }

    let removed = logger.cleanup().unwrap();
    assert_eq!(removed, 1, "should have deleted 1 expired row");

    // Chain must verify after cleanup — the first surviving
    // row was re-rooted at GENESIS_PREV_HASH and its
    // entry_hash recomputed.
    let n = logger.verify_chain().unwrap_or_else(|e| {
        panic!(
            "ADR-027 D13b: chain MUST still verify after a \
             retention cleanup; pre-fix this returned BrokenLink \
             because the first surviving row's prev_hash pointed \
             at the deleted row. cleanup() should re-root the \
             chain. Got: {e}",
        )
    });
    assert_eq!(n, 2, "verify_chain reports the 2 surviving rows");
}

/// Insert a raw row that mimics a v1 legacy audit_log entry —
/// no prev_hash, no entry_hash. The schema migration path
/// produces rows of this shape on a pre-D13b database; once
/// the v2 columns are added by ALTER TABLE, the legacy rows
/// have NULL in both hash columns. Used by the legacy-row
/// tests below.
fn insert_legacy_v1_row(logger: &AuditLogger, id: &str, tool: &str, created_at_ms: i64) {
    let conn = logger.conn_for_tests();
    conn.execute(
        "INSERT INTO audit_log ( \
             id, event_type, tool_name, user_context, arguments, \
             result, risk_tier, is_error, error_message, \
             execution_time_ms, created_at, prev_hash, entry_hash \
         ) VALUES (?1, 'tool_complete', ?2, NULL, NULL, NULL, \
                   'read_only', 0, NULL, 1, ?3, NULL, NULL)",
        rusqlite::params![id, tool, created_at_ms],
    )
    .unwrap();
}

#[test]
fn d13b_chain_verify_skips_leading_legacy_v1_rows() {
    // PR #1031 round-5 review (2026-05-02): the migration
    // doc says legacy v1 rows "stay NULL" and v2 entries
    // "form a chain rooted at the v1→v2 boundary". Pre-fix
    // verify_chain() errored on the very first NULL-hashed
    // row (MissingHash) and never reached the v2 segment, so
    // the documented design was unreachable. Now verify_chain
    // skips leading rows where BOTH hash columns are NULL
    // (legacy untouched), then verifies the v2 segment from
    // GENESIS forward.
    let logger = AuditLogger::in_memory().unwrap();

    let now = chrono::Utc::now().timestamp_millis();
    insert_legacy_v1_row(&logger, "legacy-1", "old_tool_a", now - 1000);
    insert_legacy_v1_row(&logger, "legacy-2", "old_tool_b", now - 500);

    // v2 entries written through the normal path — these
    // carry prev_hash + entry_hash.
    logger.log_tool_complete(
        "conductor_status",
        AuditRiskTier::ReadOnly,
        None,
        Some(r#"{"running":true}"#),
        Duration::from_millis(1),
        Some(UserContext::local_user()),
    );
    logger.log_tool_complete(
        "conductor_get_config",
        AuditRiskTier::ReadOnly,
        None,
        Some(r#"{"version":1}"#),
        Duration::from_millis(2),
        Some(UserContext::local_user()),
    );

    let n = logger.verify_chain().unwrap_or_else(|e| {
        panic!(
            "ADR-027 D13b: chain MUST verify across legacy v1 \
             rows + v2 segment — legacy NULL-hashed rows should \
             be skipped, v2 segment should verify rooted at \
             GENESIS. Got: {e}",
        )
    });
    assert_eq!(
        n, 2,
        "verify_chain reports the 2 v2-segment rows (legacy rows skipped, not counted)"
    );
}

#[test]
fn d13b_cleanup_preserves_legacy_null_rows() {
    // PR #1031 round-5 review (2026-05-02): pre-fix cleanup()
    // rebuilt the chain over ALL surviving rows including
    // legacy v1 rows, which silently backfilled their NULL
    // hash columns and contradicted the migration design
    // ("legacy rows stay NULL"). Now cleanup() rebuilds only
    // the v2 segment (rows where entry_hash IS NOT NULL).
    let logger = AuditLogger::in_memory().unwrap();

    let now = chrono::Utc::now().timestamp_millis();
    // Legacy rows have RECENT timestamps so retention won't
    // delete them — we want to confirm cleanup() leaves them
    // untouched, not that it deletes them.
    insert_legacy_v1_row(&logger, "legacy-1", "old_tool_a", now - 1000);
    insert_legacy_v1_row(&logger, "legacy-2", "old_tool_b", now - 500);

    // Three v2 rows.
    for i in 0..3 {
        logger.log_tool_complete(
            &format!("tool_{i}"),
            AuditRiskTier::ReadOnly,
            None,
            Some(&format!(r#"{{"i":{i}}}"#)),
            Duration::from_millis(1),
            Some(UserContext::local_user()),
        );
    }

    // Backdate the FIRST v2 row past the cutoff so cleanup
    // exercises the rebuild path (deletion + chain re-root).
    {
        let conn = logger.conn_for_tests();
        let old = chrono::Utc::now().timestamp_millis() - (100 * 24 * 60 * 60 * 1000);
        conn.execute(
            "UPDATE audit_log SET created_at = ?1 WHERE tool_name = 'tool_0'",
            [old],
        )
        .unwrap();
    }

    let removed = logger.cleanup().unwrap();
    assert_eq!(removed, 1, "should have deleted 1 expired v2 row");

    // Legacy rows MUST still have NULL hashes — cleanup is
    // forbidden from rewriting them.
    {
        let conn = logger.conn_for_tests();
        let mut stmt = conn
            .prepare(
                "SELECT prev_hash, entry_hash FROM audit_log \
                 WHERE id LIKE 'legacy-%' ORDER BY id ASC",
            )
            .unwrap();
        let rows: Vec<(Option<String>, Option<String>)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(rows.len(), 2, "both legacy rows must still be present");
        for (i, (prev, hash)) in rows.iter().enumerate() {
            assert!(
                prev.is_none(),
                "legacy row {i} prev_hash MUST stay NULL (was {prev:?})",
            );
            assert!(
                hash.is_none(),
                "legacy row {i} entry_hash MUST stay NULL (was {hash:?})",
            );
        }
    }

    // verify_chain should still pass: skip the 2 legacy rows,
    // verify the 2 surviving v2 rows (rebuilt + re-rooted).
    let n = logger.verify_chain().unwrap_or_else(|e| {
        panic!(
            "ADR-027 D13b: post-cleanup verify_chain MUST pass \
             with legacy NULL rows preserved + v2 segment \
             re-rooted. Got: {e}",
        )
    });
    assert_eq!(
        n, 2,
        "verify_chain reports the 2 surviving v2 rows (legacy skipped)"
    );
}

#[test]
fn d13b_migration_idempotent_via_pragma() {
    // PR #1031 round-6 review (2026-05-02): `run_idempotent_alter`
    // now uses `PRAGMA table_info` to check column existence
    // before running the ALTER, instead of matching on the
    // SQLite error message "duplicate column name". This test
    // confirms that opening the same on-disk DB twice (which
    // would previously trip the duplicate-column error path)
    // succeeds cleanly with the PRAGMA approach.
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test_idempotent_migration.db");

    // First open: creates v2 schema from scratch.
    {
        let config = AuditLoggerConfig {
            db_path: db_path.clone(),
            ..Default::default()
        };
        let logger = AuditLogger::new(config).unwrap();
        logger.log_tool_complete(
            "conductor_status",
            AuditRiskTier::ReadOnly,
            None,
            Some(r#"{"ok":true}"#),
            Duration::from_millis(1),
            None,
        );
        assert_eq!(logger.count().unwrap(), 1);
    }

    // Second open: migration path runs again — PRAGMA check
    // detects columns already present, skips ALTERs. Must not
    // error out or corrupt the existing entry.
    {
        let config = AuditLoggerConfig {
            db_path: db_path.clone(),
            ..Default::default()
        };
        let logger = AuditLogger::new(config).unwrap();
        assert_eq!(
            logger.count().unwrap(),
            1,
            "re-opening must not lose existing entries"
        );
        let n = logger
            .verify_chain()
            .expect("chain must still verify after idempotent re-open");
        assert_eq!(n, 1, "chain must still have 1 hashed row");
    }
}

#[test]
fn d13b_v4_migration_rehashes_legacy_provenance_rows() {
    use super::hash_chain::{
        CanonicalRow, GENESIS_PREV_HASH, canonical_persisted_bytes, compute_entry_hash,
    };
    use super::schema::{CREATE_AUDIT_TABLE, CREATE_SCHEMA_VERSION_TABLE, SCHEMA_VERSION};

    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test_v4_rehash.db");

    let created_at_1 = chrono::Utc::now().timestamp_millis() - 1000;
    let created_at_2 = created_at_1 + 1;
    let provenance = conductor_core::config::Provenance {
        initiator: conductor_core::config::Initiator::Cli,
        source: conductor_core::config::Source::InMemoryEdit,
        peer: None,
    };
    let provenance_json = serde_json::to_string(&provenance).unwrap();

    let row_1_old = CanonicalRow {
        id: "legacy-v3-1",
        event_type: "plan_applied",
        tool_name: None,
        user_context: None,
        arguments: None,
        result: None,
        risk_tier: "high",
        is_error: 0,
        error_message: None,
        execution_time_ms: Some(1),
        created_at: created_at_1,
        // Legacy v3 hash omitted provenance even when the column was non-NULL.
        provenance: None,
    };
    let row_1_hash_old =
        compute_entry_hash(GENESIS_PREV_HASH, &canonical_persisted_bytes(&row_1_old));
    let row_2_old = CanonicalRow {
        id: "legacy-v3-2",
        event_type: "plan_applied",
        tool_name: None,
        user_context: None,
        arguments: None,
        result: None,
        risk_tier: "high",
        is_error: 0,
        error_message: None,
        execution_time_ms: Some(1),
        created_at: created_at_2,
        provenance: None,
    };
    let row_2_hash_old =
        compute_entry_hash(&row_1_hash_old, &canonical_persisted_bytes(&row_2_old));

    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(CREATE_SCHEMA_VERSION_TABLE).unwrap();
        conn.execute_batch(CREATE_AUDIT_TABLE).unwrap();
        conn.execute("DELETE FROM schema_version", []).unwrap();
        conn.execute("INSERT INTO schema_version (version) VALUES (3)", [])
            .unwrap();
        conn.execute(
            "INSERT INTO audit_log (id, event_type, tool_name, user_context, arguments, \
             result, risk_tier, is_error, error_message, execution_time_ms, created_at, \
             prev_hash, entry_hash, provenance) \
             VALUES (?1, ?2, NULL, NULL, NULL, NULL, ?3, 0, NULL, 1, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                "legacy-v3-1",
                "plan_applied",
                "high",
                created_at_1,
                GENESIS_PREV_HASH,
                row_1_hash_old,
                provenance_json,
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO audit_log (id, event_type, tool_name, user_context, arguments, \
             result, risk_tier, is_error, error_message, execution_time_ms, created_at, \
             prev_hash, entry_hash, provenance) \
             VALUES (?1, ?2, NULL, NULL, NULL, NULL, ?3, 0, NULL, 1, ?4, ?5, ?6, NULL)",
            rusqlite::params![
                "legacy-v3-2",
                "plan_applied",
                "high",
                created_at_2,
                row_1_hash_old,
                row_2_hash_old,
            ],
        )
        .unwrap();
    }

    let logger = AuditLogger::new(AuditLoggerConfig {
        db_path: db_path.clone(),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        logger.verify_chain().unwrap(),
        2,
        "v4 migration should re-hash pre-existing provenance-bearing rows"
    );

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let version: i32 = conn
        .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        version, SCHEMA_VERSION,
        "schema_version must be bumped to v4"
    );
}
// ─── ADR-027 D13c — audit log PII redaction ───────────────────

#[test]
fn d13c_secret_arguments_are_redacted_before_persisting() {
    // Default config has redact_arguments=true. A tool call
    // with a secret-shaped argument key should be persisted in
    // redacted form — otherwise the audit DB carries a
    // long-tail credential disclosure.
    let logger = AuditLogger::in_memory().unwrap();
    let secret_args = serde_json::json!({
        "url": "https://api.example.com/notify",
        "auth_token": "ghp_actual_credential_value"
    })
    .to_string();
    logger.log_tool_complete(
        "conductor_send_http",
        AuditRiskTier::HardwareIO,
        Some(&secret_args),
        None,
        Duration::from_millis(5),
        Some(UserContext::local_user()),
    );

    let entries = logger.query(&AuditQuery::default()).unwrap();
    assert_eq!(entries.len(), 1);
    let stored_args: serde_json::Value = serde_json::from_str(
        entries[0]
            .arguments
            .as_deref()
            .expect("arguments should still be present after redaction"),
    )
    .expect("audit-stored arguments must be valid JSON");
    assert_eq!(
        stored_args
            .pointer("/url")
            .and_then(serde_json::Value::as_str),
        Some("https://api.example.com/notify"),
        "non-secret URL must be preserved",
    );
    let stored_tok = stored_args
        .pointer("/auth_token")
        .and_then(serde_json::Value::as_str)
        .expect("auth_token still present");
    assert!(
        stored_tok.starts_with("<redacted:"),
        "ADR-027 D13c: secret-shaped `auth_token` MUST be redacted \
         before persisting to the audit DB. Got: {stored_tok}",
    );
    assert!(
        !stored_tok.contains("ghp_actual_credential_value"),
        "real credential leaked into audit DB despite redaction",
    );
}

#[test]
fn d13c_redaction_can_be_disabled_for_debug_audit() {
    // The flags exist so an operator running a debug instance
    // can opt-out (with full risk-acceptance). Verify the
    // opt-out actually opts out — flipping the bit must let
    // the secret persist verbatim, so the operator knows
    // what they're agreeing to.
    let config = AuditLoggerConfig {
        db_path: std::path::PathBuf::from(":memory:"),
        redact_arguments: false,
        redact_results: false,
        ..Default::default()
    };
    let logger = AuditLogger::new(config).unwrap();

    let secret_args = r#"{"api_key":"sk-leaked-on-purpose"}"#;
    logger.log_tool_complete(
        "conductor_set_secret",
        AuditRiskTier::ConfigChange,
        Some(secret_args),
        None,
        Duration::from_millis(1),
        None,
    );

    let entries = logger.query(&AuditQuery::default()).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].arguments.as_deref(),
        Some(secret_args),
        "with redact_arguments=false, the secret must persist \
         verbatim — confirming the opt-out flag actually takes \
         effect.",
    );
}

#[test]
fn d13c_secret_results_are_redacted_before_persisting() {
    // PR #1032 review (2026-05-02): the original integration
    // tests covered redact_arguments but not redact_results.
    // Companion: log a tool call whose RESULT carries a
    // secret-shaped key, verify it's redacted before
    // persisting. Also exercises the new `Authorization`
    // pattern from the same review.
    let logger = AuditLogger::in_memory().unwrap();
    let secret_result = serde_json::json!({
        "status": "ok",
        "Authorization": "Bearer real-token-do-not-leak"
    })
    .to_string();
    logger.log_tool_complete(
        "conductor_send_http",
        AuditRiskTier::HardwareIO,
        Some(r#"{"url":"https://example.com"}"#),
        Some(&secret_result),
        Duration::from_millis(5),
        Some(UserContext::local_user()),
    );

    let entries = logger.query(&AuditQuery::default()).unwrap();
    assert_eq!(entries.len(), 1);
    let stored: serde_json::Value = serde_json::from_str(
        entries[0]
            .result
            .as_deref()
            .expect("result still present after redaction"),
    )
    .expect("audit-stored result must be valid JSON");
    assert_eq!(
        stored
            .pointer("/status")
            .and_then(serde_json::Value::as_str),
        Some("ok"),
        "non-secret status must be preserved",
    );
    let stored_auth = stored
        .pointer("/Authorization")
        .and_then(serde_json::Value::as_str)
        .expect("Authorization still present");
    assert!(
        stored_auth.starts_with("<redacted:"),
        "ADR-027 D13c: secret-shaped `Authorization` (HTTP \
         header convention) MUST be redacted before persisting. \
         Got: {stored_auth}",
    );
    assert!(
        !stored_auth.contains("real-token-do-not-leak"),
        "real Bearer token leaked into audit DB despite redaction",
    );
}

#[test]
fn d13c_redact_results_can_be_disabled_independently() {
    // Mirror of the arguments-flag test for `redact_results`.
    let config = AuditLoggerConfig {
        db_path: std::path::PathBuf::from(":memory:"),
        redact_results: false,
        ..Default::default()
    };
    let logger = AuditLogger::new(config).unwrap();
    let secret_result = r#"{"api_key":"sk-leaked-deliberately"}"#;
    logger.log_tool_complete(
        "tool",
        AuditRiskTier::ConfigChange,
        None,
        Some(secret_result),
        Duration::from_millis(1),
        None,
    );

    let entries = logger.query(&AuditQuery::default()).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].result.as_deref(),
        Some(secret_result),
        "redact_results=false must let the result persist verbatim",
    );
}

#[test]
fn d13c_oversize_result_redacts_before_truncating() {
    // PR #1032 review (2026-05-02): pre-fix `with_result`
    // truncated by raw byte slice, which broke JSON validity
    // and caused the redactor's parse step to fail; the
    // redactor then returned the truncated string unchanged,
    // leaking any post-truncation-point secrets. Now the
    // pipeline is redact-then-truncate, so secrets are
    // filtered before the truncation step.
    let secret_value = "sk-this-secret-must-not-leak-through-truncation";
    let large_blob = "x".repeat(15 * 1024); // > 10KB cutoff
    let raw_result = serde_json::json!({
        "api_key": secret_value,
        "data": large_blob,
    })
    .to_string();
    assert!(
        raw_result.len() > 12 * 1024,
        "test fixture must exceed truncation cutoff",
    );

    let logger = AuditLogger::in_memory().unwrap();
    logger.log_tool_complete(
        "tool_with_huge_response",
        AuditRiskTier::ReadOnly,
        None,
        Some(&raw_result),
        Duration::from_millis(1),
        None,
    );

    let entries = logger.query(&AuditQuery::default()).unwrap();
    assert_eq!(entries.len(), 1);
    let stored = entries[0]
        .result
        .as_deref()
        .expect("result column populated");
    assert!(
        !stored.contains(secret_value),
        "ADR-027 D13c: oversize result MUST have its secret \
         redacted before truncation; pre-fix the truncator \
         broke JSON validity, made redaction fail, and let the \
         secret persist. Got stored value (first 200 bytes): \
         {:?}",
        &stored[..stored.len().min(200)],
    );
}

// ────────────────────────────────────────────────────────────────────
// ADR-034 §D4.A.3.3.B.2 — provenance column (schema v3)
// ────────────────────────────────────────────────────────────────────

/// Insert a raw v2-shape row that pre-dates the v3 `provenance`
/// column. Existing v2 production DBs have rows of this shape;
/// after the v2→v3 ALTER runs, the column exists but these rows
/// have NULL in it. Mirrors `insert_legacy_v1_row` above.
fn insert_v2_shape_row(logger: &AuditLogger, id: &str, tool: &str, created_at_ms: i64) {
    let conn = logger.conn_for_tests();
    // Bind explicit NULL for provenance — the v3 column exists on
    // the fresh in-memory DB (CREATE_AUDIT_TABLE creates it), so
    // we simulate a row written before the v2→v3 ALTER by setting
    // it to NULL.
    conn.execute(
        "INSERT INTO audit_log ( \
             id, event_type, tool_name, user_context, arguments, \
             result, risk_tier, is_error, error_message, \
             execution_time_ms, created_at, prev_hash, entry_hash, \
             provenance \
         ) VALUES (?1, 'plan_applied', ?2, NULL, NULL, NULL, \
                   'config_change', 0, NULL, 1, ?3, \
                   'genesis-stub', 'hash-stub', NULL)",
        rusqlite::params![id, tool, created_at_ms],
    )
    .unwrap();
}

#[test]
fn d4a3_3b2_v2_shape_rows_load_with_none_provenance() {
    // Spec acceptance (#1282): existing v2 DB rows (with NULL
    // provenance) MUST load via `query()` without error and
    // surface as `provenance = None`. Confirms the v2→v3 read path
    // is backwards-compatible — the explicit non-spec contract for
    // the ALTER-adds-NULL column.
    let logger = AuditLogger::in_memory().unwrap();

    let now = chrono::Utc::now().timestamp_millis();
    insert_v2_shape_row(&logger, "v2-legacy-1", "apply_plan", now - 1000);
    insert_v2_shape_row(&logger, "v2-legacy-2", "apply_plan", now - 500);

    let entries = logger.query(&AuditQuery::default()).unwrap();
    assert_eq!(entries.len(), 2, "both v2-shape rows must load");
    for entry in entries {
        assert!(
            entry.provenance.is_none(),
            "v2-shape row '{}' must surface as provenance=None, \
             got: {:?}",
            entry.id,
            entry.provenance,
        );
    }
}

#[test]
fn d4a3_3b2_round_trip_with_llm_provenance() {
    // Acceptance: a `Provenance::Llm` written via
    // `log_plan_applied(..., Some(prov))` survives the
    // INSERT→SELECT round-trip with identity preserved.
    let logger = AuditLogger::in_memory().unwrap();

    let prov = conductor_core::config::Provenance {
        initiator: conductor_core::config::Initiator::Llm {
            provider: "anthropic".to_string(),
            model: "claude-opus-4-7".to_string(),
            plan_id: "test-plan-uuid-1234".to_string(),
        },
        source: conductor_core::config::Source::InMemoryEdit,
        peer: None,
    };

    logger.log_plan_applied(
        "test-plan-uuid-1234",
        2,
        Duration::from_millis(15),
        Some(UserContext::local_user()),
        Some(prov.clone()),
    );

    let entries = logger.query(&AuditQuery::default()).unwrap();
    assert_eq!(entries.len(), 1, "single plan_applied entry expected");
    let entry = &entries[0];

    let recovered = entry
        .provenance
        .as_ref()
        .expect("provenance must round-trip from log_plan_applied");
    assert_eq!(
        recovered, &prov,
        "provenance must round-trip byte-identically through \
         INSERT → SELECT → serde",
    );
}

#[test]
fn d4a3_3b2_serde_backward_compat_old_json_loads_with_none_provenance() {
    // The broadcast channel + any external consumers may have
    // cached `AuditEntry` JSON serialised before the v3 schema.
    // `#[serde(default)]` on the provenance field MUST mean an
    // entry JSON missing the field deserialises with
    // `provenance: None`, not a parse error.
    let old_json = r#"{
        "id": "pre-v3-id",
        "event_type": "tool_complete",
        "tool_name": "conductor_get_config",
        "user_context": null,
        "arguments": null,
        "result": null,
        "risk_tier": "read_only",
        "is_error": false,
        "error_message": null,
        "execution_time": null,
        "created_at": 1234567890
    }"#;
    let entry: AuditEntry = serde_json::from_str(old_json)
        .expect("pre-v3 AuditEntry JSON must deserialise (provenance is #[serde(default)])");
    assert!(
        entry.provenance.is_none(),
        "missing-field deserialisation must yield provenance=None"
    );
}

#[test]
fn d4a3_3b2_log_plan_applied_without_provenance_stays_none() {
    // Regression test: passing `None` for the new provenance
    // parameter must not invent a default Provenance — the
    // persisted row should have provenance=NULL.
    let logger = AuditLogger::in_memory().unwrap();

    logger.log_plan_applied(
        "plan_no_prov",
        1,
        Duration::from_millis(5),
        Some(UserContext::local_user()),
        None,
    );

    let entries = logger.query(&AuditQuery::default()).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(
        entries[0].provenance.is_none(),
        "log_plan_applied(..., None) must persist as provenance=None"
    );
}

#[test]
fn d4a3_3b2_mixed_provenance_rows_verify_chain() {
    // Rows with and without provenance must coexist in one valid
    // chain as long as each row's stored hash matches the current
    // canonical form.
    let logger = AuditLogger::in_memory().unwrap();

    let prov = conductor_core::config::Provenance {
        initiator: conductor_core::config::Initiator::Llm {
            provider: "tbd".to_string(),
            model: "tbd".to_string(),
            plan_id: "chain-test".to_string(),
        },
        source: conductor_core::config::Source::InMemoryEdit,
        peer: None,
    };

    // Mix of with-provenance and without-provenance rows.
    logger.log_plan_applied("p1", 1, Duration::from_millis(1), None, Some(prov.clone()));
    logger.log_plan_applied("p2", 1, Duration::from_millis(1), None, None);
    logger.log_plan_applied("p3", 1, Duration::from_millis(1), None, Some(prov));

    let n = logger
        .verify_chain()
        .expect("hash chain must remain intact regardless of provenance presence");
    assert_eq!(n, 3, "all three rows participate in the chain");
}

// ============================================================
// ADR-045 D5 (#2493) — dual-sink redaction parity.
//
// The ADR-042 D6 / ADR-027 D13c redaction corpus must hold for BOTH
// sinks. The per-sink suites cover each in depth (this file for SQLite;
// `audit/jsonl.rs` tests for JSONL); this test pins PARITY — the same
// secret-bearing fixture produces byte-identical redacted arguments in
// both, because both route through the shared `redact_audit_field`.
// ============================================================

#[test]
fn d5_dual_sink_redaction_parity() {
    use super::jsonl::{JsonlAuditSink, JsonlSinkConfig};
    use super::sink::AuditSink as _;

    let fixture =
        r#"{"auth_token": "super-secret-value", "url": "http://example.com/cb?api_key=leak#frag"}"#;

    // SQLite sink.
    let logger = AuditLogger::in_memory().unwrap();
    logger.log_tool_start(
        "conductor_send_osc",
        AuditRiskTier::Stateful,
        Some(fixture),
        None,
    );
    let rows = logger
        .query(&AuditQuery {
            limit: Some(10),
            ..Default::default()
        })
        .unwrap();
    let sqlite_args = rows[0].arguments.clone().expect("sqlite arguments stored");

    // JSONL sink.
    let dir = tempfile::tempdir().unwrap();
    let cfg = JsonlSinkConfig::at(dir.path().join("audit.jsonl"));
    let path = cfg.path.clone();
    let sink = JsonlAuditSink::new(cfg).unwrap();
    sink.log_tool_start(
        "conductor_send_osc",
        AuditRiskTier::Stateful,
        Some(fixture),
        None,
    );
    drop(sink);
    let line = std::fs::read_to_string(&path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(line.lines().next().unwrap()).unwrap();
    let jsonl_args = parsed["record"]["arguments"]
        .as_str()
        .expect("jsonl arguments stored")
        .to_string();

    assert_eq!(
        sqlite_args, jsonl_args,
        "both sinks must apply identical redaction to identical input"
    );
    assert!(!sqlite_args.contains("super-secret-value"));
    assert!(!sqlite_args.contains("api_key=leak"));
}
