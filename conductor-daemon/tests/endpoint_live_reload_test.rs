// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-035 Slice 9 (§D5) — live-config / ADR-034 CAS interaction.
//!
//! Two guarantees:
//!  1. The CAS `revision` is derived over the NORMALIZED endpoint set, so a
//!     real endpoint change — or any other config change — bumps it, while a
//!     re-author that doesn't change the normalized set stays reload-neutral
//!     (same revision ⇒ no spurious CAS bump). (ADR-035 removed the legacy
//!     `[[bindings]]`/`[[connectors]]` forms, so the former legacy↔endpoint
//!     equivalence test no longer applies.)
//!  2. Changing the `direction` of a LIVE endpoint is **restart-required**: the
//!     mutate is rejected with `MutateError::RestartRequired` and the running
//!     config (including the routes referencing the endpoint) is left intact.
//!
//! Built with the `test-helpers` feature (LiveConfig test seams), like the
//! sibling `live_config_*` suites.

use conductor_core::Config;
use conductor_core::config::Provenance;
use conductor_core::rule_set::CompiledRuleSet;
use conductor_daemon::daemon::live_config::{
    CompileError, ConfigOp, LiveConfig, LivePaths, MutateError,
};
use std::sync::Arc;

struct NoopCompiler;
impl conductor_daemon::daemon::live_config::RuleCompiler for NoopCompiler {
    fn compile(&self, config: &Config) -> Result<CompiledRuleSet, CompileError> {
        Ok(conductor_core::rule_compiler::compile(config, 0))
    }
}

fn cli_prov() -> Provenance {
    Provenance {
        initiator: conductor_core::config::Initiator::Cli,
        source: conductor_core::config::Source::InMemoryEdit,
        peer: None,
    }
}

fn parse(toml: &str) -> Config {
    toml::from_str(toml).expect("fixture parses")
}

/// Build a `LiveConfig` over a fresh temp state dir. Returns the `TempDir`
/// too — it must outlive `LiveConfig` (paths point into it).
fn build_live(cfg: Config) -> (LiveConfig, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let paths = LivePaths::from_state_dir(tmp.path().to_path_buf());
    let live = LiveConfig::new_with_paths(cfg, paths, Arc::new(NoopCompiler)).expect("init live");
    (live, tmp)
}

/// The initial-snapshot revision a config hashes to.
fn revision_of(cfg: Config) -> conductor_core::config::ConfigRevision {
    let (live, _tmp) = build_live(cfg);
    live.load().revision
}

// ── (1) revision invariance / sensitivity ──────────────────────────

#[test]
fn cosmetic_endpoint_reauthor_is_revision_neutral() {
    // Two byte-different authorings that normalize to the SAME endpoint set
    // (here: an explicit `enabled = true` vs the serde default) must hash to
    // the same CAS revision — a no-op re-author must not trigger a reload.
    let implicit = parse(
        r#"
modes = []

[[endpoints]]
alias = "pads"
direction = "Input"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Mikro" }]
"#,
    );
    let explicit = parse(
        r#"
modes = []

[[endpoints]]
alias = "pads"
direction = "Input"
type = "Matcher"
enabled = true
matchers = [{ type = "NameContains", value = "Mikro" }]
"#,
    );
    assert_eq!(
        revision_of(implicit),
        revision_of(explicit),
        "a normalization-neutral re-author of the same endpoint must be revision-neutral"
    );
}

#[test]
fn real_endpoint_change_bumps_revision() {
    // A genuine matcher difference DOES change the normalized digest, so the
    // revision must differ (proving the digest participates in the revision —
    // a NORMALIZER_VERSION bump bumps it the same way, via canonical_endpoint_digest).
    let a = parse(
        r#"
modes = []
[[endpoints]]
alias = "pads"
direction = "Input"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Mikro" }]
"#,
    );
    let b = parse(
        r#"
modes = []
[[endpoints]]
alias = "pads"
direction = "Input"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Launchpad" }]
"#,
    );
    assert_ne!(
        revision_of(a),
        revision_of(b),
        "a real endpoint matcher change must bump the revision"
    );
}

#[test]
fn non_endpoint_change_still_bumps_revision() {
    // The stripped (non-I/O) config is still hashed, so a mode/mapping change
    // bumps the revision even with identical endpoints.
    let a = parse("modes = []");
    let b = parse(
        r#"
[[modes]]
name = "Mix"
"#,
    );
    assert_ne!(
        revision_of(a),
        revision_of(b),
        "a mode change must still bump the revision"
    );
}

// ── (2) live direction change = restart-required ────────────────────

const INPUT_DEV_WITH_ROUTE: &str = r#"
modes = []

[[endpoints]]
alias = "dev"
direction = "Input"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Mikro" }]

[[endpoints]]
alias = "sink"
direction = "Output"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Synth" }]

[[routes]]
from = "dev"
to = "sink"
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_direction_flip_is_restart_required_and_leaves_config_intact() {
    let (live, _tmp) = build_live(parse(INPUT_DEV_WITH_ROUTE));
    let gen_before = live.load().state_generation;
    let rev_before = live.load().revision;

    // Flip 'dev' from Input → Output (everything else identical).
    let flipped = parse(
        r#"
modes = []

[[endpoints]]
alias = "dev"
direction = "Output"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Mikro" }]

[[endpoints]]
alias = "sink"
direction = "Output"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Synth" }]

[[routes]]
from = "dev"
to = "sink"
"#,
    );

    let result = live
        .mutate(
            cli_prov(),
            gen_before,
            ConfigOp::ReplaceWhole {
                config: Box::new(flipped),
            },
        )
        .await;

    match result {
        Err(MutateError::RestartRequired(msg)) => {
            assert!(
                msg.contains("dev") && msg.to_lowercase().contains("restart"),
                "error must name the endpoint and say restart: {msg}"
            );
        }
        other => panic!("expected RestartRequired, got {other:?}"),
    }

    // Running config untouched: generation, revision, and the routes all intact.
    let snap = live.load();
    assert_eq!(
        snap.state_generation, gen_before,
        "generation must not advance"
    );
    assert_eq!(snap.revision, rev_before, "revision must not change");
    assert_eq!(
        snap.config.routes.len(),
        1,
        "the route referencing 'dev' survives"
    );
    assert_eq!(snap.config.routes[0].from, "dev");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn adding_a_new_endpoint_is_not_restart_required() {
    // The restart-required guard fires ONLY on a direction flip of an existing
    // alias — adding a new endpoint (different alias) is an ordinary reload.
    let (live, _tmp) = build_live(parse(INPUT_DEV_WITH_ROUTE));
    let gen_before = live.load().state_generation;

    let with_new = parse(
        r#"
modes = []

[[endpoints]]
alias = "dev"
direction = "Input"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Mikro" }]

[[endpoints]]
alias = "sink"
direction = "Output"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Synth" }]

[[endpoints]]
alias = "extra"
direction = "Output"
type = "OscEndpoint"
host = "127.0.0.1"
port = 9000

[[routes]]
from = "dev"
to = "sink"
"#,
    );

    let outcome = live
        .mutate(
            cli_prov(),
            gen_before,
            ConfigOp::ReplaceWhole {
                config: Box::new(with_new),
            },
        )
        .await
        .expect("adding a new endpoint must succeed (not restart-required)");
    assert_eq!(outcome.applied_generation, gen_before + 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rollback_force_bypasses_restart_required() {
    // Break-glass means break-glass (KI-A3): RollbackForce bypasses CAS AND the
    // restart-required direction guard. An operator forcing a known-good restore
    // must not be blocked because that snapshot happens to flip a direction.
    let tmp = tempfile::tempdir().expect("tempdir");
    let paths = LivePaths::from_state_dir(tmp.path().to_path_buf());

    // Pre-seed known_good with 'dev' as Output — a direction flip vs the live
    // config below (where 'dev' is Input).
    let known_good = r#"
modes = []

[[endpoints]]
alias = "dev"
direction = "Output"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Mikro" }]
"#;
    std::fs::write(&paths.known_good, known_good).expect("write known_good");

    let live = LiveConfig::new_with_paths(
        parse(
            r#"
modes = []

[[endpoints]]
alias = "dev"
direction = "Input"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Mikro" }]
"#,
        ),
        paths,
        Arc::new(NoopCompiler),
    )
    .expect("init live");
    let gen_before = live.load().state_generation;

    let outcome = live
        .mutate(
            cli_prov(),
            gen_before,
            ConfigOp::RollbackForce {
                reason: "operator break-glass restore".to_string(),
            },
        )
        .await
        .expect("RollbackForce must bypass the restart-required direction guard");
    assert_eq!(
        outcome.applied_generation,
        gen_before + 1,
        "force rollback applies despite the Input→Output direction flip"
    );
}
