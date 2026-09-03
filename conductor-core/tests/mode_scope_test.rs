// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-040 — `ModeScope` unification + `Named > All` precedence.
//!
//! `[[global_mappings]]` is the only all-modes sugar. Internally every
//! `CompiledRule` now carries a `ModeScope`: global mappings lower to
//! `ModeScope::All`, mode-block mappings to `ModeScope::Named([mode])`.
//! This is the uniform IR that later slices and ADR-033 target.
//!
//! Per the ADR-040 spec §4.1 the change is **behaviour-preserving**:
//! the post-ADR-037 matcher (`rule_set::match_event`) already keeps global
//! and mode rules in separate buckets walked *mode-first*
//! (1. mode-device → 2. mode-any → 3. global-device → 4. global-any,
//! first-match-wins), so `Named` outranks `All` **structurally** — the
//! `scope` tag is metadata the matcher never consults. The R3 BLOCKER's
//! "global can't tie/shadow a mode-specific rule" is therefore satisfied by
//! the bucket-walk order, not by a weight folded into
//! `strictly_more_specific_than` (which only matters in a merged-bucket
//! model this code does not use).
//!
//! Spec: `docs/context-consolidation/ADR-040-implementation-spec.md`
//! §4.1, §5.

use conductor_core::config::types::Config;
use conductor_core::event_processor::{ProcessedEvent, VelocityLevel};
use conductor_core::rule_compiler;
use conductor_core::rule_set::{CompiledRuleSet, ModeScope};
use std::sync::Arc;

fn compile(toml: &str) -> CompiledRuleSet {
    let config: Config = toml::from_str(toml).expect("config parses");
    rule_compiler::compile(&config, 1)
}

fn note(n: u8) -> ProcessedEvent {
    ProcessedEvent::PadPressed {
        note: n,
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    }
}

const TWO_MODES_ONE_GLOBAL: &str = r#"
[[modes]]
name = "A"

[[modes]]
name = "B"

[[global_mappings]]
description = "global"
trigger = { type = "Note", note = 36 }
action = { type = "Text", text = "global" }
"#;

/// Acceptance: `[[global_mappings]]` still evaluate in *every* mode. With no
/// mode-specific rule for note 36, the global rule fires in both mode A and B.
#[test]
fn global_mapping_fires_in_every_mode() {
    let rs = compile(TWO_MODES_ONE_GLOBAL);
    for idx in [0usize, 1usize] {
        let env = rs
            .match_event_with_provenance(&note(36), idx, None)
            .expect("global rule matches in this mode");
        assert_eq!(
            env.matched_rule.as_deref(),
            Some("global"),
            "global mapping must fire in mode index {idx}"
        );
    }
}

/// Acceptance (R3 BLOCKER): a `Named` rule beats an `All` rule on an
/// otherwise-identical trigger. Mode A and the global both bind note 36; in
/// mode A the mode rule must win.
#[test]
fn mode_named_rule_beats_global_all_on_identical_trigger() {
    let toml = r#"
[[modes]]
name = "A"

[[modes.mappings]]
description = "mode-A"
trigger = { type = "Note", note = 36 }
action = { type = "Text", text = "mode" }

[[global_mappings]]
description = "global"
trigger = { type = "Note", note = 36 }
action = { type = "Text", text = "global" }
"#;
    let rs = compile(toml);
    let env = rs
        .match_event_with_provenance(&note(36), 0, None)
        .expect("a rule matches note 36 in mode A");
    assert_eq!(
        env.matched_rule.as_deref(),
        Some("mode-A"),
        "Named(mode) must outrank All(global) on an identical trigger"
    );
}

/// Regression: an additive `scope` tag must not perturb routing. A mode rule
/// and a global rule on *different* triggers each still fire correctly.
#[test]
fn mode_and_global_on_distinct_triggers_both_fire() {
    let toml = r#"
[[modes]]
name = "A"

[[modes.mappings]]
description = "mode-note-40"
trigger = { type = "Note", note = 40 }
action = { type = "Text", text = "mode" }

[[global_mappings]]
description = "global-note-36"
trigger = { type = "Note", note = 36 }
action = { type = "Text", text = "global" }
"#;
    let rs = compile(toml);
    assert_eq!(
        rs.match_event_with_provenance(&note(40), 0, None)
            .expect("mode rule matches note 40")
            .matched_rule
            .as_deref(),
        Some("mode-note-40"),
    );
    assert_eq!(
        rs.match_event_with_provenance(&note(36), 0, None)
            .expect("global rule matches note 36")
            .matched_rule
            .as_deref(),
        Some("global-note-36"),
    );
}

/// IR tag: mode-block mappings compile to `ModeScope::Named([mode name])`.
#[test]
fn mode_rules_are_tagged_named() {
    let toml = r#"
[[modes]]
name = "A"

[[modes.mappings]]
trigger = { type = "Note", note = 36 }
action = { type = "Text", text = "x" }
"#;
    let rs = compile(toml);
    let mode = rs.mode_rules(0).expect("mode 0 exists");
    let rule = &mode.specific_any_device_rules[0];
    assert_eq!(
        rule.scope,
        ModeScope::Named(Arc::from(vec!["A".to_string()]))
    );
}

/// All rules in one mode share a single `Arc<[ModeId]>` — the per-rule
/// `scope.clone()` is a refcount bump, not a deep clone of the mode list —
/// this avoids N heap allocations for large configs.
#[test]
fn mode_rules_share_one_arc_scope() {
    let toml = r#"
[[modes]]
name = "A"

[[modes.mappings]]
trigger = { type = "Note", note = 36 }
action = { type = "Text", text = "x" }

[[modes.mappings]]
trigger = { type = "Note", note = 37 }
action = { type = "Text", text = "y" }
"#;
    let rs = compile(toml);
    let mode = rs.mode_rules(0).expect("mode 0 exists");
    let scopes: Vec<_> = mode
        .specific_any_device_rules
        .iter()
        .map(|r| &r.scope)
        .collect();
    match (scopes[0], scopes[1]) {
        (ModeScope::Named(a), ModeScope::Named(b)) => assert!(
            Arc::ptr_eq(a, b),
            "rules in one mode must share one Arc, not deep-clone the mode list"
        ),
        other => panic!("expected two Named scopes, got {other:?}"),
    }
}

/// IR tag: `[[global_mappings]]` compile to `ModeScope::All`.
#[test]
fn global_rules_are_tagged_all() {
    let rs = compile(TWO_MODES_ONE_GLOBAL);
    let globals = rs.global_any_device_rules();
    assert_eq!(globals.len(), 1, "one global any-device rule");
    assert_eq!(globals[0].scope, ModeScope::All);
}

/// The precedence intent is encoded in `ModeScope::weight()` (`All → 0`,
/// `Named → 1`) so any future merged-bucket consumer can score scope
/// directly. Today the separate-bucket matcher enforces it structurally.
#[test]
fn named_weight_outranks_all_weight() {
    assert!(ModeScope::Named(Arc::from(vec!["A".to_string()])).weight() > ModeScope::All.weight());
}
