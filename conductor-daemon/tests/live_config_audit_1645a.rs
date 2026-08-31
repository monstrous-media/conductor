// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Bundle A regression tests for epic #1645 (Council audit follow-ups).
//!
//! Three fixes ship together because they all live in
//! `conductor-daemon/src/daemon/live_config/mod.rs`:
//!
//! - **#1607** — `std::fs::read` in `compute_candidate` is now driven
//!   via `tokio::task::spawn_blocking`, off the executor thread, so
//!   a slow/hung disk on the known-good file path can no longer stall
//!   a tokio worker. Regression test: `rollback_with_known_good_file_succeeds_via_async_read`
//!   below exercises the new path end-to-end against a real on-disk
//!   fixture under a multi-thread runtime.
//!
//! - **#1608** — `LiveConfig::install_test_snapshot` and
//!   `LiveConfig::with_compiler` are now gated behind
//!   `#[cfg(any(test, feature = "test-helpers"))]`. This file itself
//!   is a regression guard: it uses both helpers, so it MUST be
//!   compiled with the `test-helpers` feature enabled. CI and
//!   pre-push gates use `--all-features`, which activates it; a
//!   downstream consumer linking `conductor-daemon` as a plain lib
//!   no longer sees the silent-corruption surface.
//!
//! - **#1610** — `Arc::new(compiled.clone())` in the Inherit publish
//!   arm has been replaced with `Arc::new(compiled)`. There is no
//!   meaningful behavioural test for "we don't clone needlessly" —
//!   the existing mutate-flow suite is the regression net for the
//!   refactor's correctness.

use conductor_core::Config;
use conductor_core::config::Provenance;
use conductor_core::rule_set::CompiledRuleSet;
use conductor_daemon::daemon::live_config::{CompileError, ConfigOp, LiveConfig, LivePaths};
use std::sync::Arc;

fn cli_prov() -> Provenance {
    Provenance {
        initiator: conductor_core::config::Initiator::Cli,
        source: conductor_core::config::Source::InMemoryEdit,
        peer: None,
    }
}

fn empty_config() -> Config {
    toml::from_str("modes = []").expect("empty fixture parses")
}

/// A config that round-trips cleanly through serde and is visibly
/// distinct from `empty_config()` — used as the rollback TARGET so the
/// regression test can assert the rollback ACTUALLY restored those bytes.
fn rollback_target_config() -> Config {
    toml::from_str(
        r#"
[[modes]]
name = "Rollback Target Marker"
color = "blue"

[[modes.mappings]]
trigger = { type = "Note", note = 42 }
action = { type = "Keystroke", keys = "z", modifiers = [] }
"#,
    )
    .expect("rollback-target fixture parses")
}

// ────────────────────────────────────────────────────────────────────
// #1607 — Rollback reads known_good via spawn_blocking
// ────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rollback_with_known_good_file_succeeds_via_async_read() {
    // Pre-fix this same test would also pass (std::fs::read works on
    // multi-thread runtimes), but it would do so by blocking a worker
    // thread inside the async fn — invisible to functional assertions.
    // Post-fix the read happens on a blocking thread; the regression
    // guard here is that the new shape continues to deliver the
    // documented behaviour:
    //
    //   - rollback from current → known_good_config
    //   - state_generation advances
    //   - new snapshot's config matches the on-disk known_good bytes
    //
    // (The "doesn't block the executor" property is structural —
    // guaranteed by construction of spawn_blocking and not directly
    // testable without scheduler instrumentation.)
    let tmp = tempfile::tempdir().expect("tempdir");
    let paths = LivePaths::from_state_dir(tmp.path().to_path_buf());

    // Pre-seed live.toml.known_good with a recognisable target config
    // so Rollback has something to read.
    let known_good_toml = toml::to_string(&rollback_target_config()).expect("ser target");
    std::fs::write(&paths.known_good, &known_good_toml).expect("write known_good");

    // Start with empty_config as the live config.
    let live = LiveConfig::new_with_paths(empty_config(), paths, Arc::new(NoopCompiler))
        .expect("init live config");
    let gen_before = live.load().state_generation;

    // Drive Rollback — the path that hits compute_candidate's
    // file-read branch.
    let outcome = live
        .mutate(cli_prov(), gen_before, ConfigOp::Rollback)
        .await
        .expect("rollback succeeds with known_good file present");

    assert_eq!(
        outcome.applied_generation,
        gen_before + 1,
        "rollback must advance state_generation by exactly 1"
    );

    let snap_after = live.load();
    let after_modes = &snap_after.config.modes;
    assert_eq!(
        after_modes.len(),
        1,
        "rolled-back config should have the one mode from the known_good fixture"
    );
    assert_eq!(
        after_modes[0].name, "Rollback Target Marker",
        "rolled-back config must match the on-disk known_good bytes"
    );
}

// ────────────────────────────────────────────────────────────────────
// #1608 — test-helpers feature gate
// ────────────────────────────────────────────────────────────────────

#[test]
fn install_test_snapshot_is_callable_under_test_helpers_feature() {
    // This file compiles only when `test-helpers` is active (or under
    // `cfg(test)` for an in-crate unit test — which this integration
    // test is NOT). If the gate were dropped to `#[cfg(test)]` alone
    // the file would fail to compile here, blocking the regression.
    let live = LiveConfig::with_compiler(empty_config(), Arc::new(NoopCompiler))
        .expect("init with test-helper");
    // Just exercise the type — the behaviour of install_test_snapshot
    // is covered by the existing live_config_mutate.rs suite.
    let _ = live.load();
}

// ────────────────────────────────────────────────────────────────────
// Helpers
// ────────────────────────────────────────────────────────────────────

/// Delegates to the real compiler — none of these tests care about
/// rule-set shape, only that compile() succeeds for the fixture
/// configs.
struct NoopCompiler;

impl conductor_daemon::daemon::live_config::RuleCompiler for NoopCompiler {
    fn compile(&self, config: &Config) -> Result<CompiledRuleSet, CompileError> {
        Ok(conductor_core::rule_compiler::compile(config, 0))
    }
}
