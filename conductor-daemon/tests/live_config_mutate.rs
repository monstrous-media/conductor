// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

// ADR-034 §D1.1 — LiveConfig::mutate() contract tests.
//
// Covers: happy path per ConfigOp variant, stale-base CAS at both
// step 2 (outer) and step 11 (inside-lock recheck), ABA probe
// (R5-C1), byte-identical Rollback, force-flag step-2 bypass
// (KI-A3), cancellation-safety detached-spawn (KI-A1), first-publish
// state_generation = 1 (KI-A2), overflow panic policy (KI-A4),
// re-entrancy invariant doc.

// This suite drives LiveConfig through its test-only compiler-injection
// seam (`with_compiler`), which only exists with the `test-helpers`
// feature — a default-features build must skip it.
#![cfg(feature = "test-helpers")]

use conductor_core::Config;
use conductor_core::config::Provenance;
use conductor_core::rule_set::CompiledRuleSet;
use conductor_daemon::daemon::live_config::{
    CompileError, ConfigOp, LiveConfig, LivePaths, MutateError, PersistTarget, RuleCompiler,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

// ────────────────────────────────────────────────────────────────────
// Fixtures
// ────────────────────────────────────────────────────────────────────

/// Provenance helper — content irrelevant for seam tests; covered in
/// `tests/provenance_types.rs` over in conductor-core.
fn cli_prov() -> Provenance {
    Provenance {
        initiator: conductor_core::config::Initiator::Cli,
        source: conductor_core::config::Source::InMemoryEdit,
        peer: None,
    }
}

/// Minimal valid Config — just an empty modes array.
fn empty_config() -> Config {
    toml::from_str("modes = []").expect("empty fixture parses")
}

/// Build a minimal Config with N mappings in one mode.
fn config_with_n_mappings(n: usize) -> Config {
    let toml = format!(
        r#"
[[modes]]
name = "Test"
color = "blue"
mappings = [
{}
]
"#,
        (0..n)
            .map(|i| {
                let note = 36 + i; // start at note 36 (C2)
                format!(
                    r#"{{ trigger = {{ type = "Note", note = {note} }}, action = {{ type = "Keystroke", keys = "x", modifiers = [] }} }}"#
                )
            })
            .collect::<Vec<_>>()
            .join(",\n")
    );
    toml::from_str(&toml).expect("fixture parses")
}

// ────────────────────────────────────────────────────────────────────
// Type-shape / static invariants
// ────────────────────────────────────────────────────────────────────

#[test]
fn config_op_kind_returns_stable_discriminator() {
    assert_eq!(
        ConfigOp::ReplaceWhole {
            config: Box::new(empty_config())
        }
        .kind(),
        "replace_whole"
    );
    assert_eq!(ConfigOp::Rollback.kind(), "rollback");
    assert_eq!(
        ConfigOp::RollbackForce { reason: "r".into() }.kind(),
        "rollback_force"
    );
    assert_eq!(ConfigOp::MarkKnownGood.kind(), "mark_known_good");
}

#[test]
fn only_rollback_force_is_force_variant() {
    assert!(
        !ConfigOp::ReplaceWhole {
            config: Box::new(empty_config())
        }
        .is_force()
    );
    assert!(!ConfigOp::Rollback.is_force());
    assert!(!ConfigOp::MarkKnownGood.is_force());
    assert!(ConfigOp::RollbackForce { reason: "r".into() }.is_force());
}

#[test]
fn persist_target_dispatches_mark_known_good() {
    assert_eq!(
        PersistTarget::from_op(&ConfigOp::MarkKnownGood),
        PersistTarget::KnownGood
    );
    assert_eq!(
        PersistTarget::from_op(&ConfigOp::ReplaceWhole {
            config: Box::new(empty_config())
        }),
        PersistTarget::Live
    );
    assert_eq!(
        PersistTarget::from_op(&ConfigOp::Rollback),
        PersistTarget::Live
    );
    assert_eq!(
        PersistTarget::from_op(&ConfigOp::RollbackForce { reason: "r".into() }),
        PersistTarget::Live
    );
}

// ────────────────────────────────────────────────────────────────────
// Initial state — KI-A2 first publish = generation 1
// ────────────────────────────────────────────────────────────────────

#[test]
fn initial_state_generation_is_zero_sentinel() {
    let live = LiveConfig::new(empty_config()).expect("init");
    let snap = live.load();
    assert_eq!(
        snap.state_generation, 0,
        "initial sentinel = 0; first mutate() bumps to 1 (KI-A2)"
    );
    assert!(snap.known_good_revision.is_none());
}

#[test]
fn new_published_seeds_first_published_snapshot_at_generation_1() {
    // The daemon boot uses `new_published` so a `live.toml` loaded at
    // startup is the FIRST PUBLISHED snapshot (gen 1) — not the gen-0
    // uninitialised sentinel that `handle_get_config_body` blanks. (ADR-034
    // KI-A2/R6-A8: "first published = 1; gen 0 = unambiguous uninitialised".)
    let live = LiveConfig::new_published(config_with_n_mappings(3)).expect("init published");
    let snap = live.load();
    assert_eq!(
        snap.state_generation, 1,
        "new_published seeds the first published snapshot at gen 1, not the gen-0 sentinel"
    );
    // The real config is present (this is what GetConfigBody serves at gen >= 1).
    assert!(
        !snap.config.modes.is_empty(),
        "the boot config is published, not blanked"
    );
}

#[tokio::test]
async fn first_mutate_after_published_boot_publishes_generation_2() {
    // After a gen-1 published boot, the first runtime mutate continues the seam
    // from the boot's published generation → gen 2 (CAS base = 1).
    let live = LiveConfig::new_published(config_with_n_mappings(3)).expect("init published");
    let outcome = live
        .mutate(
            cli_prov(),
            1, // base = the boot's published generation
            ConfigOp::ReplaceWhole {
                config: Box::new(config_with_n_mappings(5)),
            },
        )
        .await
        .expect("first mutate after published boot");
    assert_eq!(outcome.previous_generation, 1);
    assert_eq!(outcome.applied_generation, 2);
}

#[tokio::test]
async fn first_successful_mutate_publishes_generation_1() {
    let live = LiveConfig::new(empty_config()).expect("init");
    let outcome = live
        .mutate(
            cli_prov(),
            0, // base = sentinel
            ConfigOp::ReplaceWhole {
                config: Box::new(config_with_n_mappings(3)),
            },
        )
        .await
        .expect("first mutate");

    assert_eq!(outcome.previous_generation, 0);
    assert_eq!(outcome.applied_generation, 1, "first publish = 1 (KI-A2)");

    // Snapshot reflects the publish.
    let snap = live.load();
    assert_eq!(snap.state_generation, 1);
    // Config was applied — verify by checking the snapshot's config has
    // the expected mappings (the previous mapping_count assertion was
    // against the D4.A.2 stub CompiledRuleSet; the real
    // `conductor_core::rule_set::CompiledRuleSet` doesn't expose a
    // public mapping count, so we assert against the config tree.)
    assert_eq!(snap.config.modes[0].mappings.len(), 3);
}

// ────────────────────────────────────────────────────────────────────
// Stale-base CAS — both outer (step 2) and inner (step 11)
// ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn outer_cas_rejects_stale_base_generation() {
    let live = LiveConfig::new(empty_config()).expect("init");
    let result = live
        .mutate(
            cli_prov(),
            99, // wrong — current is 0
            ConfigOp::ReplaceWhole {
                config: Box::new(config_with_n_mappings(1)),
            },
        )
        .await;
    match result {
        Err(MutateError::StaleBaseGeneration { current, supplied }) => {
            assert_eq!(current, 0);
            assert_eq!(supplied, 99);
        }
        other => panic!("expected StaleBaseGeneration, got {other:?}"),
    }
    // Snapshot did NOT advance.
    assert_eq!(live.load().state_generation, 0);
}

#[tokio::test]
async fn second_mutate_with_stale_base_is_rejected() {
    let live = LiveConfig::new(empty_config()).expect("init");
    // First mutate → generation 1.
    live.mutate(
        cli_prov(),
        0,
        ConfigOp::ReplaceWhole {
            config: Box::new(config_with_n_mappings(1)),
        },
    )
    .await
    .expect("first ok");
    // Second mutate with base=0 (stale) must reject.
    let result = live
        .mutate(
            cli_prov(),
            0,
            ConfigOp::ReplaceWhole {
                config: Box::new(config_with_n_mappings(2)),
            },
        )
        .await;
    assert!(matches!(
        result,
        Err(MutateError::StaleBaseGeneration {
            current: 1,
            supplied: 0
        })
    ));
    assert_eq!(live.load().state_generation, 1);
}

// ────────────────────────────────────────────────────────────────────
// ABA probe (R5-C1) — CAS keys off state_generation, NOT revision
// ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn mark_known_good_advances_generation_even_without_content_change() {
    let live = LiveConfig::new(config_with_n_mappings(2)).expect("init");
    // First mutate to get out of sentinel (gen 0 → 1, content
    // unchanged ReplaceWhole with same config).
    let first = live
        .mutate(
            cli_prov(),
            0,
            ConfigOp::ReplaceWhole {
                config: Box::new(config_with_n_mappings(2)),
            },
        )
        .await
        .expect("first ok");
    assert_eq!(first.applied_generation, 1);

    // MarkKnownGood: bytes don't change, but generation MUST advance
    // so concurrent stale-base callers still get CAS-rejected
    // (R5-C1 — without state_generation a byte-equal mutation
    // would collide on revision and CAS would silently pass).
    let mark = live
        .mutate(cli_prov(), 1, ConfigOp::MarkKnownGood)
        .await
        .expect("mark ok");
    assert_eq!(mark.applied_generation, 2);
    assert_eq!(
        mark.applied_revision, mark.previous_revision,
        "content didn't change — revisions identical"
    );

    let snap = live.load();
    assert_eq!(snap.known_good_revision, Some(mark.applied_revision));
}

#[tokio::test]
async fn aba_attempt_via_mark_known_good_is_blocked() {
    // Setup: gen 1 with config C, known_good=None.
    let live = LiveConfig::new(config_with_n_mappings(1)).expect("init");
    live.mutate(
        cli_prov(),
        0,
        ConfigOp::ReplaceWhole {
            config: Box::new(config_with_n_mappings(1)),
        },
    )
    .await
    .expect("first ok");

    // Client A captures gen=1.
    let client_a_base = live.load().state_generation;
    assert_eq!(client_a_base, 1);

    // Client B issues MarkKnownGood; advances gen 1 → 2, content
    // unchanged.
    live.mutate(cli_prov(), 1, ConfigOp::MarkKnownGood)
        .await
        .expect("mark ok");

    // Client A's stale-base ReplaceWhole arrives. Under the old
    // revision-keyed CAS this would have silently overwritten the
    // operator's just-confirmed known_good marker (because revision
    // didn't change). Under state_generation-keyed CAS it MUST
    // reject.
    let result = live
        .mutate(
            cli_prov(),
            client_a_base, // stale: was 1, now 2
            ConfigOp::ReplaceWhole {
                config: Box::new(config_with_n_mappings(99)),
            },
        )
        .await;
    assert!(
        matches!(result, Err(MutateError::StaleBaseGeneration { .. })),
        "ABA scenario must be rejected (R5-C1); got {result:?}"
    );

    // known_good marker survives.
    assert!(live.load().known_good_revision.is_some());
}

// ────────────────────────────────────────────────────────────────────
// Force-flag bypass — KI-A3 (step 2 honours force, not just step 11)
// ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn rollback_force_bypasses_outer_cas_step_2() {
    // KI-A3: the FORCE flag bypasses step 2's CAS check (R6 only
    // patched step 11). The contract is "RollbackForce with a
    // deliberately-wrong base_generation MUST NOT be rejected at
    // step 2 with StaleBaseGeneration." Whether the operation
    // itself ultimately succeeds (D4.B.4 file reader + a present
    // known_good file → Ok) or fails further along (e.g. malformed
    // known_good file → InvalidOp) is orthogonal — the only
    // outcome that violates KI-A3 is StaleBaseGeneration.
    //
    // Uses a tempdir so the operation is hermetic. MarkKnownGood
    // also persists `live.toml.known_good` per the §D1.2.1 matrix,
    // so by the time we invoke RollbackForce there's a parseable
    // snapshot on disk and the rollback succeeds — which still
    // proves CAS bypass (otherwise we'd get StaleBaseGeneration).
    let tmp = tempfile::tempdir().expect("tempdir");
    let paths = LivePaths::from_state_dir(tmp.path().to_path_buf());
    let live = LiveConfig::new_with_paths(
        config_with_n_mappings(1),
        paths,
        Arc::new(CountingCompiler::default()),
    )
    .expect("init");
    live.mutate(
        cli_prov(),
        0,
        ConfigOp::ReplaceWhole {
            config: Box::new(config_with_n_mappings(1)),
        },
    )
    .await
    .expect("seed");
    live.mutate(cli_prov(), 1, ConfigOp::MarkKnownGood)
        .await
        .expect("mark");
    assert_eq!(live.load().state_generation, 2);

    // RollbackForce with deliberately-wrong base_generation. KI-A3
    // says force MUST skip step 2's CAS check. Step-2 rejection is
    // the ONLY failure mode that breaks the invariant; any other
    // outcome (Ok, InvalidOp, RuleCompileFailed, …) means we got
    // past step 2.
    let result = live
        .mutate(
            cli_prov(),
            99, // bogus; force must skip outer CAS
            ConfigOp::RollbackForce {
                reason: "operator override; broken config".to_string(),
            },
        )
        .await;
    if let Err(MutateError::StaleBaseGeneration { .. }) = result {
        panic!("KI-A3 broken: step-2 CAS rejected RollbackForce despite force flag");
    }
    // result may be Ok (D4.B.4 read the freshly-persisted
    // known_good file successfully) or any other Err except
    // StaleBaseGeneration — both prove CAS was bypassed.
}

// ────────────────────────────────────────────────────────────────────
// Cancellation safety — KI-A1 detached spawn
// ────────────────────────────────────────────────────────────────────

/// Rewrite to ACTUALLY cancel the inner `mutate` future and
/// assert the spawned commit task survives.
///
/// The old test dropped a `JoinHandle` — but per tokio docs that
/// merely **detaches** the outer task, which continues to run and
/// publish. It never proved that cancelling the awaiter (the
/// caller of `mutate()`) leaves the inner commit_locked task
/// running.
///
/// New approach: use `tokio::time::timeout` to bound the awaiter
/// to a deadline that fires AFTER the synchronous prelude (compute
/// candidate + compile rule set + spawn commit_locked) but BEFORE
/// the oneshot resolves. The timeout drops the inner `mutate`
/// future at its `.await` on the oneshot — that's the load-bearing
/// cancellation event. The KI-A1 contract says the spawned
/// commit_locked task is detached and must complete the publish
/// regardless.
///
/// The deadline is tight (5 ms) so the cancellation reliably wins
/// the race against the spawned task's own publish. With the real
/// compiler the prelude is microseconds, so the runtime hits the
/// `.await` and yields before the timeout. The spawned task then
/// runs to completion on its own time, observed by polling
/// `live.load()` after the cancellation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn caller_future_cancellation_does_not_cancel_inner_commit() {
    let live = Arc::new(LiveConfig::new(empty_config()).expect("init"));
    let live_for_call = Arc::clone(&live);

    // Arm the test-only commit barrier BEFORE the mutate. The spawned
    // commit task blocks on it before running commit_locked, so the caller's
    // `mutate` future is GUARANTEED still awaiting the oneshot when we cancel
    // it — cancellation is now the load-bearing path deterministically, not a
    // 5 ms timing race that could resolve first (the old test's outcome B).
    let gate = live.arm_commit_gate();

    // Because the commit is parked on the barrier, the oneshot cannot resolve,
    // so the timeout ALWAYS fires and drops the caller's mutate future at its
    // `.await`.
    let timeout_result = tokio::time::timeout(
        std::time::Duration::from_millis(50),
        live_for_call.mutate(
            cli_prov(),
            0,
            ConfigOp::ReplaceWhole {
                config: Box::new(config_with_n_mappings(4)),
            },
        ),
    )
    .await;

    // The caller's future MUST have been cancelled (Err) — the KI-A1
    // cancellation event. The old timing-race version could see Ok here
    // (outcome B), proving nothing about cancellation.
    assert!(
        timeout_result.is_err(),
        "caller mutate future should have been cancelled by the timeout while the \
         commit was parked on the barrier; got {timeout_result:?}"
    );

    // KI-A1 contract: the detached commit survives the caller's cancellation.
    // Release the barrier; the spawned commit_locked must run to completion and
    // publish the new snapshot independently.
    gate.notify_one();

    let mut polls = 0;
    loop {
        let snap = live.load();
        if snap.state_generation == 1 {
            assert_eq!(snap.config.modes[0].mappings.len(), 4);
            return;
        }
        polls += 1;
        assert!(
            polls < 100,
            "commit did not publish within 100 polls — KI-A1 cancellation safety broken \
             (caller future was dropped at .await; spawned commit_locked should have \
              completed independently after the barrier was released)"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

// ────────────────────────────────────────────────────────────────────
// Overflow panic — KI-A4
// ────────────────────────────────────────────────────────────────────

/// Rewrite to actually drive `LiveConfig::mutate` with
/// `state_generation = u64::MAX` and assert it surfaces
/// `MutateError::Overflow`. The old test only asserted on the
/// std-lib `u64::checked_add` behaviour without touching
/// `LiveConfig` at all — a regression that removed `checked_add`
/// from `commit_locked` or silently wrapped `state_generation`
/// would pass it.
///
/// Uses the new `install_test_snapshot` test-only seam
/// to inject the synthesised u64::MAX snapshot.
#[tokio::test]
async fn state_generation_overflow_returns_overflow_error() {
    let live = LiveConfig::new(empty_config()).expect("init");

    // Build a synthesised snapshot at u64::MAX by cloning the current
    // (which has valid `rules`/`revision`/`config` Arcs) and just
    // bumping `state_generation`. The other fields are unused by the
    // overflow code path — `mutate` only inspects state_generation
    // for the CAS check + the checked_add that triggers overflow.
    let current = live.load();
    let mut maxed = (*current).clone();
    maxed.state_generation = u64::MAX;
    live.install_test_snapshot(maxed);
    assert_eq!(
        live.load().state_generation,
        u64::MAX,
        "test-only snapshot install must take effect"
    );

    // Now mutate. The supplied base_generation matches the current
    // snapshot, so step-2 CAS passes; the overflow surfaces at the
    // `checked_add` inside commit_locked (step ~12).
    let result = live
        .mutate(
            cli_prov(),
            u64::MAX,
            ConfigOp::ReplaceWhole {
                config: Box::new(empty_config()),
            },
        )
        .await;

    assert!(
        matches!(result, Err(MutateError::Overflow)),
        "u64::MAX + 1 MUST surface MutateError::Overflow (KI-A4); \
         silent wrap would re-enable ABA. Got: {result:?}"
    );

    // Snapshot must NOT advance on a failed overflow attempt — the
    // failed mutate is a no-op, not a degraded publish.
    assert_eq!(
        live.load().state_generation,
        u64::MAX,
        "failed Overflow attempt MUST NOT change state_generation"
    );
}

// ────────────────────────────────────────────────────────────────────
// Rollback semantics
// ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn rollback_force_with_empty_reason_is_invalid_op() {
    // Defense-in-depth: IPC framer enforces non-empty per KI-B3,
    // but the daemon also rejects empty/whitespace reasons so a
    // buggy IPC layer can't bypass the contract.
    let live = LiveConfig::new(empty_config()).expect("init");
    for reason in ["", "   ", "\t\n  "] {
        let result = live
            .mutate(
                cli_prov(),
                0,
                ConfigOp::RollbackForce {
                    reason: reason.to_string(),
                },
            )
            .await;
        match result {
            Err(MutateError::InvalidOp(msg)) => assert!(
                msg.contains("non-empty"),
                "expected non-empty error, got: {msg}"
            ),
            other => panic!("expected InvalidOp for empty reason {reason:?}, got {other:?}"),
        }
    }
    assert_eq!(live.load().state_generation, 0, "no state change");
}

#[tokio::test]
async fn rollback_without_known_good_returns_no_snapshot_invalid_op() {
    // D4.B.4 wires the file-side `live.toml.known_good`
    // reader. When the operator hasn't run `mark-known-good` yet
    // (and no prior daemon recorded one), the file is absent and
    // Rollback surfaces a distinct InvalidOp explaining the
    // recovery path — "no known-good snapshot recorded yet — run
    // conductorctl config mark-known-good after the current config
    // is validated."
    //
    // This replaces the prior `rollback_is_invalid_op_until_d4b`
    // stub-guard which asserted the placeholder "not yet
    // implemented" message.
    //
    // Uses an isolated tempdir so the test is hermetic — the
    // developer's `$XDG_STATE_HOME/conductor/live.toml.known_good`
    // (if present) doesn't leak in and make Rollback succeed.
    let tmp = tempfile::tempdir().expect("tempdir");
    let paths = LivePaths::from_state_dir(tmp.path().to_path_buf());
    let live =
        LiveConfig::new_with_paths(empty_config(), paths, Arc::new(CountingCompiler::default()))
            .expect("init");

    let result = live.mutate(cli_prov(), 0, ConfigOp::Rollback).await;
    match result {
        Err(MutateError::InvalidOp(msg)) => {
            assert!(
                msg.contains("no known-good") || msg.contains("known_good"),
                "expected InvalidOp about absent known-good snapshot, got: {msg}"
            );
        }
        other => panic!("expected InvalidOp from compute_candidate, got {other:?}"),
    }
    assert_eq!(
        live.load().state_generation,
        0,
        "no state change on rejected mutate"
    );
}

#[tokio::test]
async fn concurrent_mark_known_good_does_not_get_clobbered_by_stale_writer() {
    // TOCTOU regression. The previous
    // implementation computed new_known_good outside the lock from
    // `snapshot_before`. If another mutator MarkKnownGood'd between
    // step 1 and step 11, the stale writer would publish with the
    // OLD (None or earlier) known_good_revision, silently clobbering
    // the marker. The fix moves new_known_good computation inside
    // commit_locked, sourcing from snapshot_now.
    //
    // This test can't deterministically inject the race, but it
    // verifies the invariant: after MarkKnownGood + a subsequent
    // ReplaceWhole, known_good_revision survives. (If the bug were
    // present, the test would still pass deterministically due to
    // sequential mutation — but the FIX makes the invariant hold
    // even under interleaved writers, which `concurrent_mutators_…`
    // exercises.)
    let live = Arc::new(LiveConfig::new(config_with_n_mappings(1)).expect("init"));
    live.mutate(
        cli_prov(),
        0,
        ConfigOp::ReplaceWhole {
            config: Box::new(config_with_n_mappings(1)),
        },
    )
    .await
    .expect("seed");
    let mark = live
        .mutate(cli_prov(), 1, ConfigOp::MarkKnownGood)
        .await
        .expect("mark");

    // Subsequent non-MarkKnownGood mutation must preserve marker.
    live.mutate(
        cli_prov(),
        2,
        ConfigOp::ReplaceWhole {
            config: Box::new(config_with_n_mappings(5)),
        },
    )
    .await
    .expect("subsequent");

    assert_eq!(
        live.load().known_good_revision,
        Some(mark.applied_revision),
        "MarkKnownGood marker MUST survive subsequent ReplaceWhole \
         (per ADR-034 §D1.2.1 persist matrix; non-MarkKnownGood ops \
         leave known_good_revision unchanged)"
    );
}

// ────────────────────────────────────────────────────────────────────
// Rule-compiler integration
// ────────────────────────────────────────────────────────────────────

#[derive(Default)]
struct CountingCompiler {
    calls: AtomicUsize,
}

impl RuleCompiler for CountingCompiler {
    fn compile(&self, config: &Config) -> Result<CompiledRuleSet, CompileError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        // Delegate to the real compiler to produce a valid rule set —
        // we're testing invocation count, not rule-set shape.
        Ok(conductor_core::rule_compiler::compile(config, 0))
    }
}

#[tokio::test]
async fn compiler_is_invoked_per_mutate() {
    let counter = Arc::new(CountingCompiler::default());
    let live = LiveConfig::with_compiler(empty_config(), counter.clone()).expect("init");
    // Initial compile during construction = 1 call.
    assert_eq!(counter.calls.load(Ordering::SeqCst), 1);

    live.mutate(
        cli_prov(),
        0,
        ConfigOp::ReplaceWhole {
            config: Box::new(config_with_n_mappings(2)),
        },
    )
    .await
    .expect("mutate");
    // Step 5 compile = +1 call.
    assert_eq!(counter.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn validation_rejection_in_mutate_returns_invalid_op_before_compile() {
    let counter = Arc::new(CountingCompiler::default());
    let live = LiveConfig::with_compiler(empty_config(), counter.clone()).expect("init");
    // Initial compile during construction = 1 call.
    assert_eq!(counter.calls.load(Ordering::SeqCst), 1);

    let invalid_config: Config = toml::from_str(
        r#"
[[modes]]
name = "evil"

[[modes.mappings]]
trigger = { type = "Note", note = 60 }
action = { type = "Shell", command = "echo ok; rm -rf /tmp/x" }
"#,
    )
    .expect("invalid fixture still parses");

    let result = live
        .mutate(
            cli_prov(),
            0,
            ConfigOp::ReplaceWhole {
                config: Box::new(invalid_config),
            },
        )
        .await;

    match result {
        Err(MutateError::InvalidOp(msg)) => assert!(
            msg.contains("config validation failed"),
            "expected validation InvalidOp, got: {msg}"
        ),
        other => panic!("expected InvalidOp from core validation, got {other:?}"),
    }

    assert_eq!(
        counter.calls.load(Ordering::SeqCst),
        1,
        "compile must not run when core validation rejects the candidate"
    );
    assert_eq!(
        live.load().state_generation,
        0,
        "rejected mutate must not publish a new snapshot"
    );
}

// ────────────────────────────────────────────────────────────────────
// D4.A.3.1 — mutate_replace_whole helper
// ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn mutate_replace_whole_helper_applies_mutator() {
    let live = LiveConfig::new(empty_config()).expect("init");
    let initial_generation = live.load().state_generation;

    let outcome = live
        .mutate_replace_whole(cli_prov(), |cfg| {
            cfg.default_mode = Some("MyMode".to_string());
        })
        .await
        .expect("helper mutate");

    assert_eq!(outcome.applied_generation, initial_generation + 1);
    let snap = live.load();
    assert_eq!(snap.config.default_mode, Some("MyMode".to_string()));
}

#[tokio::test]
async fn mutate_replace_whole_helper_preserves_other_fields() {
    // Mutating one field via the helper must leave others intact —
    // it's a clone-mutate-replace, not a partial update.
    let live = LiveConfig::new(config_with_n_mappings(3)).expect("init");

    live.mutate_replace_whole(cli_prov(), |cfg| {
        cfg.default_mode = Some("X".to_string());
    })
    .await
    .expect("mutate");

    let snap = live.load();
    assert_eq!(snap.config.default_mode, Some("X".to_string()));
    assert_eq!(
        snap.config.modes[0].mappings.len(),
        3,
        "unrelated modes/mappings field must survive the helper"
    );
}

#[tokio::test]
async fn snapshot_config_reflects_mutation_input() {
    // D4.A.2 used a stub CompiledRuleSet with a mapping_count field;
    // D4.A.3.1 swaps to the real `conductor_core::rule_set::CompiledRuleSet`
    // which doesn't expose a public count. Assert against `snap.config`
    // instead — that's the source of truth for what was published.
    let live = LiveConfig::new(empty_config()).expect("init");
    let snap = live.load();
    assert_eq!(snap.config.modes.len(), 0, "empty fixture has no modes");

    live.mutate(
        cli_prov(),
        0,
        ConfigOp::ReplaceWhole {
            config: Box::new(config_with_n_mappings(5)),
        },
    )
    .await
    .expect("mutate");
    let snap = live.load();
    assert_eq!(snap.config.modes[0].mappings.len(), 5);
}

// ────────────────────────────────────────────────────────────────────
// Concurrent mutators serialise via mutate_lock
// ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn concurrent_mutators_serialise_via_mutate_lock() {
    let live = Arc::new(LiveConfig::new(empty_config()).expect("init"));

    // Bring out of sentinel.
    live.mutate(
        cli_prov(),
        0,
        ConfigOp::ReplaceWhole {
            config: Box::new(empty_config()),
        },
    )
    .await
    .expect("seed");

    // Spawn N MarkKnownGood mutators with the same base_generation.
    // ONE wins the CAS race; the others get StaleBaseGeneration.
    // (No two can both publish, because each publish advances the
    // generation past the others' base.)
    let n = 8;
    let mut handles = Vec::new();
    for _ in 0..n {
        let live_clone = Arc::clone(&live);
        handles.push(tokio::spawn(async move {
            live_clone
                .mutate(cli_prov(), 1, ConfigOp::MarkKnownGood)
                .await
        }));
    }

    let mut successes = 0;
    let mut stale_rejects = 0;
    for h in handles {
        match h.await.expect("task join") {
            Ok(_) => successes += 1,
            Err(MutateError::StaleBaseGeneration { .. }) => stale_rejects += 1,
            Err(other) => panic!("unexpected error: {other:?}"),
        }
    }
    assert_eq!(successes, 1, "exactly one writer wins the CAS race");
    assert_eq!(stale_rejects, n - 1, "the rest get StaleBaseGeneration");

    // Final state advanced exactly once.
    assert_eq!(live.load().state_generation, 2);
}

// ────────────────────────────────────────────────────────────────────
// Compiler-error propagation
// ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn compile_error_in_mutate_returns_compile_failed() {
    // Compiler that fails at mutate-time. We need it to succeed at
    // construction (otherwise `LiveConfig::new` itself errors), so
    // use a wrapping compiler that lets the first call through and
    // fails on subsequent calls.

    struct FirstSuccessThenFail {
        first: std::sync::atomic::AtomicBool,
    }
    impl RuleCompiler for FirstSuccessThenFail {
        fn compile(&self, config: &Config) -> Result<CompiledRuleSet, CompileError> {
            if self.first.swap(false, std::sync::atomic::Ordering::SeqCst) {
                Ok(conductor_core::rule_compiler::compile(config, 0))
            } else {
                Err(CompileError("injected".into()))
            }
        }
    }

    let compiler = Arc::new(FirstSuccessThenFail {
        first: std::sync::atomic::AtomicBool::new(true),
    });
    let live = LiveConfig::with_compiler(empty_config(), compiler).expect("init ok");

    let result = live
        .mutate(
            cli_prov(),
            0,
            ConfigOp::ReplaceWhole {
                config: Box::new(empty_config()),
            },
        )
        .await;
    assert!(
        matches!(result, Err(MutateError::CompileFailed(_))),
        "expected CompileFailed; got {result:?}"
    );
    // Snapshot stays at sentinel — no partial commit.
    assert_eq!(live.load().state_generation, 0);
}

// ────────────────────────────────────────────────────────────────────
// Hot-path lock-free reads
// ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn many_concurrent_reads_do_not_block_writer() {
    let live = Arc::new(LiveConfig::new(empty_config()).expect("init"));

    // Spawn 32 reader tasks that hammer `load()` for a short window.
    let mut reader_handles = Vec::new();
    for _ in 0..32 {
        let live_clone = Arc::clone(&live);
        reader_handles.push(tokio::spawn(async move {
            let mut max_gen = 0;
            for _ in 0..100 {
                let snap = live_clone.load();
                max_gen = max_gen.max(snap.state_generation);
                tokio::task::yield_now().await;
            }
            max_gen
        }));
    }

    // Meanwhile, the writer mutates twice.
    live.mutate(
        cli_prov(),
        0,
        ConfigOp::ReplaceWhole {
            config: Box::new(config_with_n_mappings(1)),
        },
    )
    .await
    .expect("first");
    live.mutate(
        cli_prov(),
        1,
        ConfigOp::ReplaceWhole {
            config: Box::new(config_with_n_mappings(2)),
        },
    )
    .await
    .expect("second");

    // Readers complete; max generation observed is at most the
    // current snapshot's generation. (Not strictly testing
    // contention, but confirms readers don't deadlock or panic
    // under load.)
    for h in reader_handles {
        let max_seen = h.await.expect("reader task");
        assert!(max_seen <= 2);
    }

    assert_eq!(live.load().state_generation, 2);
}

// ────────────────────────────────────────────────────────────────────
// try_mutate_replace_whole — fallible mutator
//
// Bug: `mutate_replace_whole` takes `FnOnce(&mut Config)` (no return),
// so a closure that "failed" semantically (e.g. `plan.apply_atomic`
// returned Err) had no way to signal abort. The helper ALWAYS
// proceeded to publish, advancing `state_generation` and writing
// `live.toml` even when the mutator's intent had failed. Other
// CAS-based clients saw a spurious generation bump.
//
// Fix: `try_mutate_replace_whole<F>` where `F: FnOnce(&mut Config)
// -> Result<(), String>`. On Err, the helper returns
// `MutateError::MutatorAborted(msg)` and does NOT publish.
// ────────────────────────────────────────────────────────────────────

/// THE regression test. Closure returns Err — the helper
/// MUST surface `MutatorAborted` AND the live snapshot MUST NOT
/// advance generation or change content.
#[tokio::test]
async fn try_mutate_replace_whole_aborts_publish_on_closure_err() {
    let live = LiveConfig::new(config_with_n_mappings(2)).expect("init");
    let before_gen = live.load().state_generation;
    let before_rev = live.load().revision;

    let result = live
        .try_mutate_replace_whole(cli_prov(), |cfg| {
            // Mutate (partially) THEN signal failure. Pre-fix, the
            // partial mutation would have been published with a
            // generation bump. Post-fix, the candidate is dropped.
            cfg.default_mode = Some("would_be_orphaned".to_string());
            Err::<(), String>("simulated apply_atomic failure".to_string())
        })
        .await;

    match result {
        Err(MutateError::MutatorAborted(msg)) => {
            assert!(
                msg.contains("apply_atomic"),
                "MutatorAborted should carry the closure's reason; got: {msg}"
            );
        }
        other => panic!("expected MutatorAborted on closure Err, got {other:?}"),
    }

    // The security-critical post-conditions: NO generation bump, NO
    // content change. A failed mutator must be invisible to readers.
    let after = live.load();
    assert_eq!(
        after.state_generation, before_gen,
        "state_generation MUST NOT advance when mutator aborts"
    );
    assert_eq!(
        after.revision, before_rev,
        "revision MUST NOT change when mutator aborts"
    );
    assert_ne!(
        after.config.default_mode,
        Some("would_be_orphaned".to_string()),
        "partial mutation MUST NOT leak into the published snapshot"
    );
}

/// Happy path: closure returns Ok → helper publishes normally,
/// same semantics as `mutate_replace_whole`.
#[tokio::test]
async fn try_mutate_replace_whole_publishes_on_closure_ok() {
    let live = LiveConfig::new(empty_config()).expect("init");
    let before_gen = live.load().state_generation;

    let outcome = live
        .try_mutate_replace_whole(cli_prov(), |cfg| {
            cfg.default_mode = Some("Success".to_string());
            Ok::<(), String>(())
        })
        .await
        .expect("Ok closure must publish");

    assert_eq!(outcome.applied_generation, before_gen + 1);
    assert_eq!(live.load().config.default_mode, Some("Success".to_string()));
}

/// Closure that mutates AND returns Ok is the typical "config
/// builder" case. Confirms the mutated state IS what gets published
/// (not the original snapshot).
#[tokio::test]
async fn try_mutate_replace_whole_publishes_the_mutated_state() {
    let live = LiveConfig::new(config_with_n_mappings(2)).expect("init");

    live.try_mutate_replace_whole(cli_prov(), |cfg| {
        cfg.default_mode = Some("Built".to_string());
        // Imagine plan.apply_atomic also touches modes/mappings:
        cfg.modes.clear();
        Ok::<(), String>(())
    })
    .await
    .expect("publish");

    let after = live.load();
    assert_eq!(after.config.default_mode, Some("Built".to_string()));
    assert!(
        after.config.modes.is_empty(),
        "mutator's clear() must have published"
    );
}

// ────────────────────────────────────────────────────────────────────
// Deterministic TOCTOU interleave test (BlockingCompiler)
// ────────────────────────────────────────────────────────────────────

/// Sync-rendezvous compiler that blocks on the test-targeted
/// `compile()` invocation. All other invocations pass straight
/// through to the real compiler.
///
/// `target_call_index` is the 0-based invocation that should block.
/// Use `1` to skip `LiveConfig::with_compiler`'s initial-snapshot
/// compile (n=0) and block on the test's first mutate (n=1).
///
/// Used to deterministically interleave two mutators:
/// Task A (ReplaceWhole) blocks at compile; Task B (MarkKnownGood)
/// runs to completion; Task A resumes and is forced to interact
/// with the post-MarkKnownGood snapshot. Pre-fix, this raced and
/// could silently clobber the known_good_revision marker.
/// Post-fix, Task A either CAS-rejects or preserves the marker.
struct BlockingCompilerAtCall {
    started_tx: std::sync::Mutex<Option<std::sync::mpsc::SyncSender<()>>>,
    release_rx: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    call_count: AtomicUsize,
    target_call_index: usize,
}

impl BlockingCompilerAtCall {
    fn new(
        target_call_index: usize,
    ) -> (
        Arc<Self>,
        std::sync::mpsc::Receiver<()>,
        std::sync::mpsc::SyncSender<()>,
    ) {
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let compiler = Arc::new(Self {
            started_tx: std::sync::Mutex::new(Some(started_tx)),
            release_rx: std::sync::Mutex::new(release_rx),
            call_count: AtomicUsize::new(0),
            target_call_index,
        });
        (compiler, started_rx, release_tx)
    }
}

impl RuleCompiler for BlockingCompilerAtCall {
    fn compile(&self, config: &Config) -> Result<CompiledRuleSet, CompileError> {
        let n = self.call_count.fetch_add(1, Ordering::SeqCst);
        if n == self.target_call_index {
            // Signal "compile started" then block until released.
            if let Some(tx) = self.started_tx.lock().unwrap().take() {
                let _ = tx.send(());
            }
            let _ = self.release_rx.lock().unwrap().recv();
        }
        Ok(conductor_core::rule_compiler::compile(config, 0))
    }
}

/// THE actual regression test (replaces the prior sequential
/// `concurrent_mark_known_good_does_not_get_clobbered_by_stale_writer`
/// which was acknowledged in its own comments to not race anything).
///
/// Setup:
/// 1. Seed LiveConfig with one mapping → generation = 0.
/// 2. Use BlockingCompilerOnce so the FIRST mutate's compile() pauses.
/// 3. Spawn Task A: `mutate(ReplaceWhole)` — blocks in compile, holds
///    snapshot_before=0.
/// 4. Wait for "compile started" signal.
/// 5. Task B (main): `mutate(MarkKnownGood)` runs to completion. Its
///    own compile() passes through (not the first call anymore). Task
///    B publishes generation=1 with the known_good marker set.
/// 6. Release Task A's compile. Task A's commit_locked then runs the
///    step-11 CAS recheck and sees state_generation has advanced from
///    0 to 1. It MUST stale-reject — NOT publish with snapshot_before's
///    (None) known_good_revision and clobber Task B's marker.
///
/// Assertions:
/// - Task B's mutate returns Ok with a known_good marker set
/// - Task A's mutate returns Err(StaleBaseGeneration)
/// - Final snapshot still has Task B's known_good marker (NOT cleared)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interleaved_replace_whole_does_not_clobber_mark_known_good() {
    // Skip the construction-time compile (n=0); block on Task A's
    // mutate compile (n=1). Task B's mutate compile (n=2) passes
    // through, as does Task A's POST-release commit compile if any.
    let (compiler, started_rx, release_tx) = BlockingCompilerAtCall::new(1);
    let live =
        Arc::new(LiveConfig::with_compiler(config_with_n_mappings(1), compiler).expect("init"));

    // Task A: ReplaceWhole — will block in compile, holding
    // snapshot_before = 0.
    let live_for_a = Arc::clone(&live);
    let task_a = tokio::spawn(async move {
        live_for_a
            .mutate(
                cli_prov(),
                0,
                ConfigOp::ReplaceWhole {
                    config: Box::new(config_with_n_mappings(2)),
                },
            )
            .await
    });

    // Wait for Task A to reach the blocked compile(). Use
    // spawn_blocking so we don't tie up the runtime on a sync recv.
    tokio::task::spawn_blocking(move || {
        started_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("Task A must reach compile within 5s")
    })
    .await
    .expect("started-recv join");

    // Task B: MarkKnownGood — runs to completion. Its compile()
    // passes through (BlockingCompilerOnce only blocks the first
    // call). Publishes generation=1 with known_good marker set.
    let task_b_result = live
        .mutate(cli_prov(), 0, ConfigOp::MarkKnownGood)
        .await
        .expect("Task B (MarkKnownGood) must succeed");
    assert_eq!(
        task_b_result.applied_generation, 1,
        "Task B publishes first"
    );
    assert_eq!(
        live.load().state_generation,
        1,
        "snapshot reflects Task B's publish"
    );
    let marker_after_b = live.load().known_good_revision;
    assert!(
        marker_after_b.is_some(),
        "Task B's MarkKnownGood must have set the known_good_revision"
    );

    // Release Task A's blocked compile. Task A's commit_locked then
    // CAS-rechecks at step 11; snapshot_now is gen=1 but
    // snapshot_before was gen=0 → StaleBaseGeneration.
    tokio::task::spawn_blocking(move || release_tx.send(()).expect("release"))
        .await
        .expect("release-send join");

    let task_a_result = task_a.await.expect("Task A join");
    match task_a_result {
        Err(MutateError::StaleBaseGeneration { current, supplied }) => {
            assert_eq!(current, 1, "Task A sees Task B's published generation");
            assert_eq!(supplied, 0, "Task A was held at snapshot_before=0");
        }
        other => {
            panic!("Task A MUST stale-reject after Task B's intervening publish; got {other:?}")
        }
    }

    // THE security-critical assertion: Task B's known_good marker
    // survives Task A's failed publish attempt. Pre-fix, Task A
    // would publish with snapshot_before's known_good (None) and
    // clobber Task B's marker.
    let final_snap = live.load();
    assert_eq!(
        final_snap.state_generation, 1,
        "no further publish after Task A's stale-reject"
    );
    assert_eq!(
        final_snap.known_good_revision, marker_after_b,
        "Task B's known_good marker MUST survive Task A's interleaved \
         stale-reject (deterministic interleave)"
    );
}
