// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-036 Slice 6 — RouteEngine mode awareness + re-entrancy guard.
//!
//! Covers the engine-level capabilities added in this slice:
//!   - `route_destinations` filters by the active mode (empty `modes` =
//!     all modes). All routes are post-mapping (ADR-036 Phase 3 removed
//!     the `pre_mapping` phase).
//!   - `DispatchGuard` rejects cycles and over-deep fan-out chains.
//!
//! Spec: `docs/routing-unification/ADR-036-037-implementation-spec.md`
//! § 5 Slice 6, § 4.5. Closes #1664.

use conductor_core::Config;
use conductor_core::config::types::RouteConfig;
use conductor_daemon::route_engine::{DispatchGuard, ReentrancyError, RouteEngine};

fn route_with(from: &str, to: &str, modes: Vec<&str>) -> RouteConfig {
    RouteConfig {
        from: from.into(),
        to: to.into(),
        transform: None,
        filter: None,
        enabled: true,
        description: None,
        modes: modes.into_iter().map(String::from).collect(),
    }
}

fn note_on() -> Vec<u8> {
    vec![0x90, 60, 64]
}

fn aliases(engine: &RouteEngine, source: &str, active_mode: &str) -> Vec<String> {
    engine
        .route_destinations_midi(source, &note_on(), active_mode)
        .into_iter()
        .map(|o| o.to_alias)
        .collect()
}

// ── Mode scoping ───────────────────────────────────────────────────

#[test]
fn mode_scoped_route_fires_in_its_active_mode() {
    let engine = RouteEngine::compile(&[route_with("pads", "absynth", vec!["Play"])]);
    assert_eq!(
        aliases(&engine, "pads", "Play"),
        vec!["absynth".to_string()],
        "a route scoped to 'Play' must fire when 'Play' is active"
    );
}

#[test]
fn mode_scoped_route_is_skipped_in_other_mode() {
    let engine = RouteEngine::compile(&[route_with("pads", "absynth", vec!["Play"])]);
    assert!(
        aliases(&engine, "pads", "Edit").is_empty(),
        "a route scoped to 'Play' must NOT fire when 'Edit' is active"
    );
}

#[test]
fn empty_modes_route_fires_in_any_mode() {
    let engine = RouteEngine::compile(&[route_with("pads", "absynth", vec![])]);
    assert_eq!(
        aliases(&engine, "pads", "Play"),
        vec!["absynth".to_string()]
    );
    assert_eq!(
        aliases(&engine, "pads", "Edit"),
        vec!["absynth".to_string()]
    );
}

// ── Re-entrancy guard ──────────────────────────────────────────────

#[test]
fn dispatch_guard_allows_distinct_hops_up_to_depth() {
    let mut guard = DispatchGuard::new(8);
    for alias in ["a", "b", "c", "d", "e", "f", "g", "h"] {
        assert!(guard.enter(alias).is_ok(), "hop {alias} within depth 8");
    }
}

#[test]
fn dispatch_guard_rejects_cycle() {
    let mut guard = DispatchGuard::new(8);
    guard.enter("a").unwrap();
    guard.enter("b").unwrap();
    assert_eq!(
        guard.enter("a"),
        Err(ReentrancyError::CycleDetected("a".to_string())),
        "re-entering a visited alias is a cycle"
    );
}

#[test]
fn dispatch_guard_rejects_over_depth() {
    let mut guard = DispatchGuard::new(3);
    guard.enter("a").unwrap();
    guard.enter("b").unwrap();
    guard.enter("c").unwrap();
    assert_eq!(
        guard.enter("d"),
        Err(ReentrancyError::DepthExceeded(3)),
        "the 4th distinct hop exceeds max_depth=3"
    );
}

#[test]
fn dispatch_guard_reports_cycle_over_depth_when_both_apply() {
    // When the chain is already at max_depth AND the next alias is a
    // repeat, the cycle is the more informative diagnosis — `enter` must
    // report CycleDetected, not DepthExceeded (Copilot review on #1685).
    let mut guard = DispatchGuard::new(2);
    guard.enter("a").unwrap();
    guard.enter("b").unwrap();
    // visited.len() == max_depth (2) AND "a" is a repeat.
    assert_eq!(
        guard.enter("a"),
        Err(ReentrancyError::CycleDetected("a".to_string())),
        "a repeat at max depth must report the cycle, not the depth bound"
    );
}

#[test]
fn max_route_depth_default_is_8() {
    // The guard's bound comes from advanced_settings.max_route_depth,
    // which defaults to 8 (ADR-036 D4.3).
    let cfg = Config::default_config();
    assert_eq!(cfg.advanced_settings.max_route_depth, 8);
}

// ── Direct-cycle rejection at load is unchanged (Slice 6 verification) ──

#[test]
fn direct_two_cycle_is_still_rejected_by_the_validator() {
    // A→B + B→A is caught statically at config load (ADR-031 § 4.3); the
    // runtime DispatchGuard is the second line of defence for longer
    // cycles. Verify the static check still fires after the Slice 6 changes.
    let cfg: Config = toml::from_str(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "a"
direction = "Bidirectional"
type = "Matcher"
matchers = [{ type = "NameContains", value = "DevA" }]

[[endpoints]]
alias = "b"
direction = "Bidirectional"
type = "Matcher"
matchers = [{ type = "NameContains", value = "DevB" }]

[[routes]]
from = "a"
to = "b"

[[routes]]
from = "b"
to = "a"
"#,
    )
    .expect("config parses");
    let report = conductor_core::config::validation::validate_config(&cfg);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.to_lowercase().contains("cycle")),
        "direct A→B + B→A cycle must still be rejected; got: {:#?}",
        report.errors
    );
}
