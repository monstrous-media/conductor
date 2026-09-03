// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

// ADR-034 §D4.B.1 — persistence foundation tests.
//
// `persist_atomically` recipe (§D3.1): NamedTempFile → write → fsync(file)
// → rename → fsync(dir). Wrapped in `spawn_blocking` per R4-M6.
//
// `LivePaths`: resolves `$XDG_STATE_HOME/conductor/{live.toml,
// live.toml.known_good, conductor.lock}`. Per-UID via env.
//
// Per-op persist matrix: each `ConfigOp` routes to a `PersistTarget`
// (Live, KnownGood) via `PersistTarget::from_op`. `mutate()` step 13
// dispatches via that target — `ReplaceWhole` writes `live.toml` only;
// `MarkKnownGood` writes `live.toml.known_good` only and leaves
// `live.toml` untouched. Verified by the `*_persists_to_*` tests
// below.

use conductor_core::Config;
use conductor_core::config::Provenance;
use conductor_core::rule_set::CompiledRuleSet;
use conductor_daemon::daemon::audit::{OutboxPhase, read_outbox_entries};
use conductor_daemon::daemon::live_config::{
    CompileError, ConfigOp, LiveConfig, LivePaths, MutateError, PersistTarget, ResumeOutcome,
    RuleCompiler, persist_atomically,
};
use std::sync::Arc;
use tempfile::TempDir;

// ────────────────────────────────────────────────────────────────────
// Fixtures
// ────────────────────────────────────────────────────────────────────

fn cli_prov() -> Provenance {
    Provenance {
        initiator: conductor_core::config::Initiator::Cli,
        source: conductor_core::config::Source::InMemoryEdit,
        peer: None,
    }
}

/// Compiler that returns an empty rule set — same trick the
/// `live_config_mutate.rs` suite uses. Real compilation isn't
/// the subject under test here; the persist plumbing is.
struct StubCompiler;
impl RuleCompiler for StubCompiler {
    fn compile(&self, config: &Config) -> Result<CompiledRuleSet, CompileError> {
        Ok(conductor_core::rule_compiler::compile(config, 1))
    }
}

fn fresh_live_config(paths: LivePaths) -> Arc<LiveConfig> {
    let config = Config::default_config();
    Arc::new(
        LiveConfig::new_with_paths(config, paths, Arc::new(StubCompiler))
            .expect("LiveConfig::new_with_paths"),
    )
}

// ────────────────────────────────────────────────────────────────────
// `persist_atomically` direct tests
// ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn persist_atomically_round_trip() {
    // Write bytes via the atomic recipe, then read them back —
    // round-trip must be byte-identical.
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("live.toml");
    let bytes = b"hello = \"world\"\n";

    persist_atomically(&target, bytes)
        .await
        .expect("persist_atomically failed");

    let read_back = std::fs::read(&target).expect("read target back");
    assert_eq!(read_back, bytes, "round-trip must be byte-identical");
}

#[tokio::test]
async fn persist_atomically_overwrites_existing() {
    // The recipe uses rename, which atomically replaces an existing
    // file. Verify second write fully replaces the first.
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("live.toml");

    persist_atomically(&target, b"first contents")
        .await
        .unwrap();
    persist_atomically(&target, b"second contents")
        .await
        .unwrap();

    let read_back = std::fs::read(&target).unwrap();
    assert_eq!(
        read_back, b"second contents",
        "rename must replace atomically"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn persist_atomically_sets_0600_permissions() {
    // ADR-034 §"Persistence file layout" requires `live.toml` and
    // `live.toml.known_good` at mode 0600. NamedTempFile defaults to
    // 0600 on Unix, but `persist.rs` sets it explicitly to harden
    // against future default changes (Council #1293 round 2 fix).
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("live.toml");

    persist_atomically(&target, b"contents").await.unwrap();

    let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o600,
        "persisted file must be 0600 per ADR-034 spec, got {mode:o}"
    );
}

#[tokio::test]
async fn persist_atomically_leaves_no_tempfile_on_success() {
    // The recipe creates a NamedTempFile in the target's parent,
    // then renames it. On success there should be exactly one file
    // in the dir (the target) — no leftover `tmpXXXXXX` files.
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("live.toml");

    persist_atomically(&target, b"contents").await.unwrap();

    let entries: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(
        entries,
        vec!["live.toml".to_string()],
        "no leftover tempfile expected, got: {entries:?}"
    );
}

// ────────────────────────────────────────────────────────────────────
// `LivePaths` resolution
// ────────────────────────────────────────────────────────────────────

#[test]
fn live_paths_from_state_dir_constructs_expected_paths() {
    // `LivePaths::from_state_dir(dir)` is the test-friendly
    // constructor — `from_env()` reads `$XDG_STATE_HOME` which would
    // pollute test isolation.
    let dir = TempDir::new().unwrap();
    let paths = LivePaths::from_state_dir(dir.path().to_path_buf());

    assert_eq!(paths.state_dir, dir.path());
    assert_eq!(paths.live, dir.path().join("live.toml"));
    assert_eq!(paths.known_good, dir.path().join("live.toml.known_good"));
    assert_eq!(paths.lockfile, dir.path().join("conductor.lock"));
}

// ────────────────────────────────────────────────────────────────────
// Per-op persist matrix — ReplaceWhole, MarkKnownGood, Rollback
// ────────────────────────────────────────────────────────────────────

#[test]
fn persist_target_from_op_routes_each_variant() {
    // Spec table (phase-b-durable-persistence.md §"Per-op persist matrix"):
    //   ReplaceWhole / ApplyPlan / ImportFromFile → Live
    //   MarkKnownGood → KnownGood
    //   Rollback / RollbackForce → Live (from known_good)
    assert_eq!(
        PersistTarget::from_op(&ConfigOp::ReplaceWhole {
            config: Box::new(Config::default_config())
        }),
        PersistTarget::Live
    );
    assert_eq!(
        PersistTarget::from_op(&ConfigOp::MarkKnownGood),
        PersistTarget::KnownGood
    );
    assert_eq!(
        PersistTarget::from_op(&ConfigOp::Rollback),
        PersistTarget::Live
    );
    assert_eq!(
        PersistTarget::from_op(&ConfigOp::RollbackForce {
            reason: "test".to_string()
        }),
        PersistTarget::Live
    );
}

#[tokio::test]
async fn replace_whole_persists_to_live_toml_only() {
    // After a `ReplaceWhole` mutation, `live.toml` is written but
    // `live.toml.known_good` stays absent.
    let dir = TempDir::new().unwrap();
    let paths = LivePaths::from_state_dir(dir.path().to_path_buf());
    let live = fresh_live_config(paths.clone());

    let mut new_config = Config::default_config();
    new_config.last_selected_mode = Some("mutated".to_string());
    let snap = live.load();
    live.mutate(
        cli_prov(),
        snap.state_generation,
        ConfigOp::ReplaceWhole {
            config: Box::new(new_config),
        },
    )
    .await
    .expect("mutate failed");

    assert!(
        paths.live.exists(),
        "live.toml must exist after ReplaceWhole"
    );
    assert!(
        !paths.known_good.exists(),
        "live.toml.known_good must NOT exist after ReplaceWhole"
    );

    // Bytes round-trip: the persisted file is the canonical form
    // of the published config.
    let persisted = std::fs::read(&paths.live).unwrap();
    let snap_after = live.load();
    let expected = conductor_core::config::canonical::serialise(&snap_after.config)
        .expect("canonical serialise");
    assert_eq!(
        persisted, expected,
        "persisted bytes must equal the canonical form of the published snapshot"
    );
}

#[tokio::test]
async fn mark_known_good_persists_to_known_good_only() {
    // After `MarkKnownGood`, `live.toml.known_good` is written but
    // `live.toml` stays absent (it wasn't ever published in this
    // test). This is the inverse of the ReplaceWhole assertion.
    let dir = TempDir::new().unwrap();
    let paths = LivePaths::from_state_dir(dir.path().to_path_buf());
    let live = fresh_live_config(paths.clone());

    let snap = live.load();
    live.mutate(cli_prov(), snap.state_generation, ConfigOp::MarkKnownGood)
        .await
        .expect("MarkKnownGood failed");

    assert!(
        !paths.live.exists(),
        "live.toml must NOT exist after solo MarkKnownGood"
    );
    assert!(
        paths.known_good.exists(),
        "live.toml.known_good must exist after MarkKnownGood"
    );

    // Content, not just existence: known_good must be the canonical
    // serialization of the current snapshot (mirrors the ReplaceWhole
    // byte round-trip above). An empty / stale / wrong known_good would
    // otherwise satisfy the existence check while breaking rollback
    // durability (#1497).
    let persisted_known_good = std::fs::read(&paths.known_good).unwrap();
    let expected = conductor_core::config::canonical::serialise(&live.load().config)
        .expect("canonical serialise");
    assert_eq!(
        persisted_known_good, expected,
        "known_good bytes must equal the canonical form of the current snapshot"
    );
}

#[tokio::test]
async fn mark_known_good_does_not_overwrite_existing_live_toml() {
    // Sequence: ReplaceWhole (writes live.toml) → MarkKnownGood
    // (writes known_good, MUST leave live.toml byte-identical).
    let dir = TempDir::new().unwrap();
    let paths = LivePaths::from_state_dir(dir.path().to_path_buf());
    let live = fresh_live_config(paths.clone());

    let mut new_config = Config::default_config();
    new_config.last_selected_mode = Some("mutated".to_string());
    let snap = live.load();
    live.mutate(
        cli_prov(),
        snap.state_generation,
        ConfigOp::ReplaceWhole {
            config: Box::new(new_config),
        },
    )
    .await
    .unwrap();

    let live_bytes_before = std::fs::read(&paths.live).unwrap();

    let snap = live.load();
    live.mutate(cli_prov(), snap.state_generation, ConfigOp::MarkKnownGood)
        .await
        .unwrap();

    let live_bytes_after = std::fs::read(&paths.live).unwrap();
    assert_eq!(
        live_bytes_before, live_bytes_after,
        "MarkKnownGood must NOT touch live.toml"
    );
    assert!(
        paths.known_good.exists(),
        "MarkKnownGood must write known_good"
    );

    // Content, not just existence: known_good must hold the canonical
    // serialization of the current snapshot. Since the marked-good
    // config is exactly the one just published, those bytes are also
    // identical to the (untouched) live.toml — so a wrong/stale
    // known_good can't hide behind the existence + live-untouched
    // checks (#1497).
    let persisted_known_good = std::fs::read(&paths.known_good).unwrap();
    let expected = conductor_core::config::canonical::serialise(&live.load().config)
        .expect("canonical serialise");
    assert_eq!(
        persisted_known_good, expected,
        "known_good bytes must equal the canonical form of the current snapshot"
    );
    assert_eq!(
        persisted_known_good, live_bytes_after,
        "known_good and the untouched live.toml describe the same marked-good snapshot"
    );
}

// ────────────────────────────────────────────────────────────────────
// ADR-034 §D8 — two-phase audit-outbox recording on mutate()
// ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn mutate_records_pending_then_applied_in_outbox() {
    use conductor_daemon::daemon::audit::{OutboxPhase, read_outbox_entries};

    let dir = TempDir::new().unwrap();
    let paths = LivePaths::from_state_dir(dir.path().to_path_buf());
    let outbox_path = paths.audit_outbox.clone();

    let lc = LiveConfig::new_with_paths(Config::default_config(), paths, Arc::new(StubCompiler))
        .expect("LiveConfig::new_with_paths")
        .with_audit_outbox();

    // No mutation yet → outbox empty.
    assert!(read_outbox_entries(&outbox_path).unwrap().is_empty());

    let base = lc.load().state_generation;
    let mut next = Config::default_config();
    next.last_selected_mode = Some("outbox-test".to_string());
    lc.mutate(
        cli_prov(),
        base,
        ConfigOp::ReplaceWhole {
            config: Box::new(next),
        },
    )
    .await
    .expect("mutate should succeed");

    let entries = read_outbox_entries(&outbox_path).expect("read outbox");
    assert_eq!(entries.len(), 2, "expected Pending + Applied rows");
    assert_eq!(entries[0].record.phase, OutboxPhase::Pending);
    assert_eq!(entries[1].record.phase, OutboxPhase::Applied);
    // Both markers carry the SAME mutation id.
    assert_eq!(entries[0].record.id, entries[1].record.id);
    // The Pending row records the intended revision for §D8.3 reconciliation.
    assert!(entries[0].record.intended_revision.is_some());
}

#[tokio::test]
async fn mutate_without_outbox_is_unaffected() {
    // No `with_audit_outbox()` → no outbox file is created, mutate still works.
    let dir = TempDir::new().unwrap();
    let paths = LivePaths::from_state_dir(dir.path().to_path_buf());
    let outbox_path = paths.audit_outbox.clone();
    let lc = fresh_live_config(paths);

    let base = lc.load().state_generation;
    lc.mutate(
        cli_prov(),
        base,
        ConfigOp::ReplaceWhole {
            config: Box::new(Config::default_config()),
        },
    )
    .await
    .expect("mutate should succeed without an outbox");
    assert!(
        !outbox_path.exists(),
        "no outbox file when recording is disabled"
    );
}

#[tokio::test]
async fn mutate_persist_failure_appends_failed_marker() {
    // cloud-review coverage: when persist fails AFTER the Pending row is
    // enqueued, a `Failed` marker is appended and the mutation returns Err
    // without advancing the generation. Force persist failure by making
    // `live.toml` a directory so the atomic rename can't land.
    use conductor_daemon::daemon::audit::{OutboxPhase, read_outbox_entries};

    let dir = TempDir::new().unwrap();
    let paths = LivePaths::from_state_dir(dir.path().to_path_buf());
    let outbox_path = paths.audit_outbox.clone();
    let live = paths.live.clone();

    let lc = LiveConfig::new_with_paths(Config::default_config(), paths, Arc::new(StubCompiler))
        .expect("LiveConfig::new_with_paths")
        .with_audit_outbox();

    // live.toml as a directory → persist_atomically's rename fails.
    std::fs::create_dir(&live).unwrap();

    let base = lc.load().state_generation;
    let res = lc
        .mutate(
            cli_prov(),
            base,
            ConfigOp::ReplaceWhole {
                config: Box::new(Config::default_config()),
            },
        )
        .await;
    assert!(
        res.is_err(),
        "persist onto a directory must fail the mutation"
    );

    let entries = read_outbox_entries(&outbox_path).expect("read outbox");
    assert_eq!(entries.len(), 2, "expected Pending + Failed rows");
    assert_eq!(entries[0].record.phase, OutboxPhase::Pending);
    assert_eq!(entries[1].record.phase, OutboxPhase::Failed);
    assert_eq!(entries[0].record.id, entries[1].record.id);
    // No new generation was published.
    assert_eq!(lc.load().state_generation, base);
}

#[tokio::test]
async fn mutate_refused_when_audit_outbox_fails_to_open() {
    // ADR-034 §D8 sub-slice C (#2296): if audit recording was requested but the
    // outbox can't be opened (corrupt chain), the daemon must run audit-unavailable
    // and refuse config mutations fail-closed — NOT silently commit un-recorded
    // changes (the pre-#2296 bug, where an open-failure left `audit_outbox = None`
    // and the mutation proceeded). Mirrors the at-cap refusal.
    let dir = TempDir::new().unwrap();
    let paths = LivePaths::from_state_dir(dir.path().to_path_buf());
    let outbox_path = paths.audit_outbox.clone();

    // `new_with_paths` ensures the state dir; then plant a corrupt outbox so the
    // subsequent `with_audit_outbox` open fails. A non-final unparseable line is a
    // fatal `Corruption` (only the LAST line is tolerated as torn).
    let lc = LiveConfig::new_with_paths(Config::default_config(), paths, Arc::new(StubCompiler))
        .expect("LiveConfig::new_with_paths");
    std::fs::write(&outbox_path, b"{not-json\n{\"x\":1}\n").unwrap();
    let lc = lc.with_audit_outbox();

    assert!(
        lc.is_audit_unavailable(),
        "#2296: a failed outbox open must mark the daemon audit-unavailable"
    );

    let base = lc.load().state_generation;
    let mut next = Config::default_config();
    next.last_selected_mode = Some("should-be-refused".to_string());
    let res = lc
        .mutate(
            cli_prov(),
            base,
            ConfigOp::ReplaceWhole {
                config: Box::new(next),
            },
        )
        .await;
    match res {
        Err(MutateError::AuditUnavailable(_)) => {}
        other => panic!("#2296: expected AuditUnavailable when outbox open failed, got {other:?}"),
    }
    assert_eq!(
        lc.load().state_generation,
        base,
        "#2296: a refused mutation must NOT publish a new generation"
    );
}

// ────────────────────────────────────────────────────────────────────
// ADR-034 §D8 / #2380 — `conductorctl audit resume` recovery
// ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn resume_audit_recovers_corrupt_outbox_and_clears_gate() {
    // #2380: a corrupt outbox bricks config writes (audit-unavailable). The
    // operator-gated `resume_audit` must rotate the corrupt file aside, open a
    // fresh chain with a ChainReset attestation, clear the gate, and let
    // mutations proceed again.
    let dir = TempDir::new().unwrap();
    let paths = LivePaths::from_state_dir(dir.path().to_path_buf());
    let outbox_path = paths.audit_outbox.clone();

    let lc = LiveConfig::new_with_paths(Config::default_config(), paths, Arc::new(StubCompiler))
        .expect("LiveConfig::new_with_paths");
    // A non-final unparseable line → fatal Corruption on open.
    std::fs::write(&outbox_path, b"{not-json\n{\"x\":1}\n").unwrap();
    let lc = lc.with_audit_outbox();
    assert!(
        lc.is_audit_unavailable(),
        "corrupt outbox → audit-unavailable"
    );

    let outcome = lc
        .resume_audit("test-operator".to_string())
        .await
        .expect("resume_audit should recover a corrupt outbox");
    let rotated = match outcome {
        ResumeOutcome::Recovered {
            rotated_path: Some(p),
        } => p,
        other => panic!("expected Recovered{{Some}}, got {other:?}"),
    };

    assert!(
        !lc.is_audit_unavailable(),
        "#2380: resume must clear the gate"
    );
    assert!(rotated.exists(), "corrupt outbox preserved at {rotated:?}");

    // The fresh chain's FIRST record is the ChainReset attestation.
    let entries = read_outbox_entries(&outbox_path).expect("fresh outbox reads clean");
    assert_eq!(entries.len(), 1, "fresh chain has exactly the reset record");
    assert_eq!(entries[0].record.phase, OutboxPhase::ChainReset);
    let prov = entries[0].record.provenance.as_deref().unwrap_or("");
    assert!(
        prov.contains("chain_reset_by_operator"),
        "provenance: {prov}"
    );
    assert!(prov.contains("test-operator"), "operator recorded: {prov}");

    // And config mutations work again (gate cleared end-to-end).
    let base = lc.load().state_generation;
    let mut next = Config::default_config();
    next.last_selected_mode = Some("after-resume".to_string());
    lc.mutate(
        cli_prov(),
        base,
        ConfigOp::ReplaceWhole {
            config: Box::new(next),
        },
    )
    .await
    .expect("#2380: mutations succeed after resume");
    assert!(lc.load().state_generation > base);
}

#[tokio::test]
async fn resume_audit_on_healthy_outbox_is_noop() {
    // #2380: resume on an already-healthy (or never-corrupt) outbox is a no-op.
    let dir = TempDir::new().unwrap();
    let paths = LivePaths::from_state_dir(dir.path().to_path_buf());
    let lc = LiveConfig::new_with_paths(Config::default_config(), paths, Arc::new(StubCompiler))
        .expect("LiveConfig::new_with_paths")
        .with_audit_outbox();
    assert!(!lc.is_audit_unavailable());

    match lc.resume_audit("op".to_string()).await {
        Ok(ResumeOutcome::AlreadyHealthy) => {}
        other => panic!("expected AlreadyHealthy, got {other:?}"),
    }
    assert!(!lc.is_audit_unavailable());
}

// ────────────────────────────────────────────────────────────────────
// ADR-034 §D8.3 — startup reconciliation wired into with_audit_outbox
// ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn with_audit_outbox_reconciles_at_startup() {
    use conductor_daemon::daemon::audit::{AuditOutbox, OutboxPhase, read_outbox_entries};

    let dir = TempDir::new().unwrap();
    let paths = LivePaths::from_state_dir(dir.path().to_path_buf());
    let outbox_path = paths.audit_outbox.clone();
    let config = Config::default_config();

    // Probe the revision the loaded config will carry (deterministic for the
    // same config), so we can seed a Pending row whose intended revision matches.
    let probe = LiveConfig::new_with_paths(config.clone(), paths.clone(), Arc::new(StubCompiler))
        .expect("probe");
    let live_rev = probe.load().revision.to_string();
    drop(probe);

    // Seed the outbox: one Pending-only row that DID publish (intended == live →
    // promote) and one that did NOT (intended != live → pending-at-crash).
    {
        let (mut ob, _) = AuditOutbox::open(outbox_path.clone()).unwrap();
        ob.enqueue_pending("promote-me", None, Some(live_rev.clone()), 1)
            .unwrap();
        ob.enqueue_pending("crashed", None, Some("a-different-revision".into()), 2)
            .unwrap();
    }

    // Construct with the outbox → §D8.3 reconciliation runs.
    let lc = LiveConfig::new_with_paths(config, paths, Arc::new(StubCompiler))
        .expect("engine")
        .with_audit_outbox();

    // Only the genuinely-unpublished mutation is pending-at-crash.
    assert_eq!(lc.pending_at_crash().len(), 1);
    assert_eq!(lc.pending_at_crash()[0].id, "crashed");

    // The promote-able row got an Applied marker appended.
    let entries = read_outbox_entries(&outbox_path).unwrap();
    assert!(
        entries
            .iter()
            .any(|e| e.record.id == "promote-me" && e.record.phase == OutboxPhase::Applied),
        "promote-me should have an Applied marker re-attached"
    );
    // The crashed row got no marker.
    assert!(
        !entries
            .iter()
            .any(|e| e.record.id == "crashed" && e.record.phase != OutboxPhase::Pending),
        "crashed should remain Pending-only"
    );
}

// ── ADR-034 §D8.2 — fail-closed when the audit outbox is at cap ───────

#[tokio::test]
async fn mutate_refused_when_audit_outbox_at_cap() {
    use conductor_daemon::daemon::audit::{AuditOutbox, OUTBOX_CAP};

    let dir = TempDir::new().unwrap();
    let paths = LivePaths::from_state_dir(dir.path().to_path_buf());
    let mut lc = LiveConfig::new_with_paths(
        Config::default_config(),
        paths.clone(),
        Arc::new(StubCompiler),
    )
    .expect("engine");

    // Install an outbox forced to its §D8.1 cap (no 4096 real appends).
    let (mut ob, _) = AuditOutbox::open(paths.audit_outbox.clone()).unwrap();
    ob.set_count_for_test(OUTBOX_CAP);
    lc.set_audit_outbox_for_test(ob);

    let base = lc.load().state_generation;
    let res = lc
        .mutate(
            cli_prov(),
            base,
            ConfigOp::ReplaceWhole {
                config: Box::new(Config::default_config()),
            },
        )
        .await;
    match res {
        Err(MutateError::AuditUnavailable(_)) => {}
        other => panic!("expected AuditUnavailable at cap, got {other:?}"),
    }
    // Fail-closed: nothing persisted or published.
    assert_eq!(lc.load().state_generation, base);
    assert!(
        !paths.live.exists(),
        "no config should be persisted when refused"
    );
}
