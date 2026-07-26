// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! ADR-037 Slice 4 — rule-compiler simplification (10-stage → 4-stage).
//!
//! After Slice 3 lowers `Trigger::Raw` to routes at config load, the
//! compiled rule set never contains Raw rules, so the matcher's two Raw
//! sub-buckets per scope are dead. This slice removes them, collapsing
//! the per-event walk from 8 sub-bucket checks to 4:
//!
//!   Stage 1 (Mode Context):  mode.specific_device[dev] → mode.specific_any
//!   Stage 2 (Global Rules):  global.specific_device[dev] → global.specific_any
//!
//! These are characterization tests: they pin the surviving 4-bucket
//! walk order and shadowing semantics so the structural change is proven
//! behaviour-preserving for non-Raw configs. The proptest is the
//! regression proof the spec calls for ("for any event and any config
//! without Raw, the simplified matcher returns the same result as the
//! documented bucket order").
//!
//! Spec: `docs/routing-unification/ADR-036-037-implementation-spec.md`
//! § 5 Slice 4. Closes #1662.

use conductor_core::config::types::Config;
use conductor_core::event_processor::{ProcessedEvent, VelocityLevel};
use conductor_core::rule_compiler;
use proptest::prelude::*;

fn compile(toml: &str) -> conductor_core::rule_set::CompiledRuleSet {
    let config: Config = toml::from_str(toml).expect("config parses");
    rule_compiler::compile(&config, 1)
}

fn note(note: u8) -> ProcessedEvent {
    // The device the event "came from" is supplied to `match_event`
    // separately (the `device_id` arg), not carried on the event here.
    ProcessedEvent::PadPressed {
        note,
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    }
}

fn cc(cc: u8, value: u8) -> ProcessedEvent {
    ProcessedEvent::CCReceived {
        cc,
        value,
        channel: Some(0),
    }
}

/// A config with the same Note-36 trigger present in all four surviving
/// buckets, each tagged with a distinct description so provenance reveals
/// which bucket the matcher selected.
const ALL_FOUR_BUCKETS: &str = r#"
[[bindings]]
alias = "pads"
[bindings.input]
matchers = [{ type = "NameContains", value = "Mikro" }]

[[modes]]
name = "Default"

[[modes.mappings]]
description = "mode-specific-device"
trigger = { type = "Note", note = 36, device = "pads" }
action = { type = "Keystroke", keys = "a" }

[[modes.mappings]]
description = "mode-specific-any"
trigger = { type = "Note", note = 36 }
action = { type = "Keystroke", keys = "b" }

[[global_mappings]]
description = "global-specific-device"
trigger = { type = "Note", note = 36, device = "pads" }
action = { type = "Keystroke", keys = "c" }

[[global_mappings]]
description = "global-specific-any"
trigger = { type = "Note", note = 36 }
action = { type = "Keystroke", keys = "d" }
"#;

fn matched(rules: &conductor_core::rule_set::CompiledRuleSet, device: Option<&str>) -> String {
    rules
        .match_event_with_provenance(&note(36), 0, device)
        .and_then(|e| e.matched_rule)
        .expect("a rule matches Note 36")
}

#[test]
fn stage1_mode_specific_device_wins_when_present() {
    let rules = compile(ALL_FOUR_BUCKETS);
    assert_eq!(matched(&rules, Some("pads")), "mode-specific-device");
}

#[test]
fn stage1b_mode_specific_any_wins_when_no_device_bucket_matches() {
    // Same config, but the event comes from a device with no device-bucket
    // rule → falls through to mode-specific-any (still ahead of global).
    let rules = compile(ALL_FOUR_BUCKETS);
    assert_eq!(matched(&rules, Some("other-device")), "mode-specific-any");
}

#[test]
fn stage2_global_specific_device_wins_when_mode_buckets_absent() {
    let toml = r#"
[[bindings]]
alias = "pads"
[bindings.input]
matchers = [{ type = "NameContains", value = "Mikro" }]

[[modes]]
name = "Default"

[[global_mappings]]
description = "global-specific-device"
trigger = { type = "Note", note = 36, device = "pads" }
action = { type = "Keystroke", keys = "c" }

[[global_mappings]]
description = "global-specific-any"
trigger = { type = "Note", note = 36 }
action = { type = "Keystroke", keys = "d" }
"#;
    let rules = compile(toml);
    assert_eq!(matched(&rules, Some("pads")), "global-specific-device");
}

#[test]
fn stage2b_global_specific_any_is_last_resort() {
    let toml = r#"
[[modes]]
name = "Default"

[[global_mappings]]
description = "global-specific-any"
trigger = { type = "Note", note = 36 }
action = { type = "Keystroke", keys = "d" }
"#;
    let rules = compile(toml);
    assert_eq!(matched(&rules, Some("pads")), "global-specific-any");
}

#[test]
fn mode_scope_shadows_global_scope() {
    // The whole point of the 2-stage model: any matching mode rule beats
    // any global rule for the same event.
    let rules = compile(ALL_FOUR_BUCKETS);
    let m = matched(&rules, Some("pads"));
    assert!(
        m.starts_with("mode-"),
        "a mode rule must shadow global rules; matched {m}"
    );
}

#[test]
fn no_match_when_no_bucket_has_the_note() {
    let rules = compile(ALL_FOUR_BUCKETS);
    assert!(
        rules
            .match_event_with_provenance(&note(40), 0, Some("pads"))
            .is_none(),
        "Note 40 matches nothing in a Note-36-only config"
    );
}

// ── Regression proof (spec §5 Slice 4) ─────────────────────────────
//
// Oracle: independently compute the expected bucket for a Note event over
// a config built from random per-bucket membership flags, then assert the
// matcher agrees. Restricted to Note triggers so the oracle stays a
// faithful, simple re-statement of the documented 4-bucket priority.

#[derive(Debug, Clone)]
struct BucketFlags {
    mode_specific_device: bool,
    mode_specific_any: bool,
    global_specific_device: bool,
    global_specific_any: bool,
}

fn build_config(flags: &BucketFlags) -> String {
    let mut s = String::from(
        r#"
[[bindings]]
alias = "pads"
[bindings.input]
matchers = [{ type = "NameContains", value = "Mikro" }]

[[modes]]
name = "Default"
"#,
    );
    if flags.mode_specific_device {
        s.push_str(
            "\n[[modes.mappings]]\ndescription = \"mode-specific-device\"\n\
             trigger = { type = \"Note\", note = 36, device = \"pads\" }\n\
             action = { type = \"Keystroke\", keys = \"a\" }\n",
        );
    }
    if flags.mode_specific_any {
        s.push_str(
            "\n[[modes.mappings]]\ndescription = \"mode-specific-any\"\n\
             trigger = { type = \"Note\", note = 36 }\n\
             action = { type = \"Keystroke\", keys = \"b\" }\n",
        );
    }
    if flags.global_specific_device {
        s.push_str(
            "\n[[global_mappings]]\ndescription = \"global-specific-device\"\n\
             trigger = { type = \"Note\", note = 36, device = \"pads\" }\n\
             action = { type = \"Keystroke\", keys = \"c\" }\n",
        );
    }
    if flags.global_specific_any {
        s.push_str(
            "\n[[global_mappings]]\ndescription = \"global-specific-any\"\n\
             trigger = { type = \"Note\", note = 36 }\n\
             action = { type = \"Keystroke\", keys = \"d\" }\n",
        );
    }
    s
}

/// The documented 4-bucket priority, restated independently of the matcher.
/// `event_device_is_pads` controls whether the device-filtered buckets can
/// fire (a device rule only matches events from its own device).
fn oracle(flags: &BucketFlags, event_device_is_pads: bool) -> Option<&'static str> {
    if event_device_is_pads && flags.mode_specific_device {
        return Some("mode-specific-device");
    }
    if flags.mode_specific_any {
        return Some("mode-specific-any");
    }
    if event_device_is_pads && flags.global_specific_device {
        return Some("global-specific-device");
    }
    if flags.global_specific_any {
        return Some("global-specific-any");
    }
    None
}

proptest! {
    #[test]
    fn matcher_agrees_with_four_bucket_oracle(
        msd in any::<bool>(),
        msa in any::<bool>(),
        gsd in any::<bool>(),
        gsa in any::<bool>(),
        event_on_pads in any::<bool>(),
    ) {
        let flags = BucketFlags {
            mode_specific_device: msd,
            mode_specific_any: msa,
            global_specific_device: gsd,
            global_specific_any: gsa,
        };
        let rules = compile(&build_config(&flags));
        let device = if event_on_pads { Some("pads") } else { Some("keys") };
        let got = rules
            .match_event_with_provenance(&note(36), 0, device)
            .and_then(|e| e.matched_rule);
        let expected = oracle(&flags, event_on_pads).map(String::from);
        prop_assert_eq!(got, expected);
    }
}

// ── Slice 5 (ADR-037 D2): set-theory specificity ordering ──────────

use conductor_core::rule_set::TriggerConstraints;

fn tc(
    has_device: bool,
    has_channel: bool,
    has_note: bool,
    has_cc: bool,
    has_velocity_range: bool,
) -> TriggerConstraints {
    TriggerConstraints {
        has_device,
        has_channel,
        has_note,
        has_cc,
        has_velocity_range,
        has_message_types: false,
        has_direction: false,
        has_pressure_range: false,
        has_program: false,
    }
}

#[test]
fn strict_superset_is_more_specific() {
    // {device, channel, note} ⊋ {device, channel}
    let more = tc(true, true, true, false, false);
    let less = tc(true, true, false, false, false);
    assert!(more.strictly_more_specific_than(&less));
    assert!(!less.strictly_more_specific_than(&more));
}

#[test]
fn adding_one_constraint_is_more_specific() {
    // {device, channel} ⊋ {device}
    let more = tc(true, true, false, false, false);
    let less = tc(true, false, false, false, false);
    assert!(more.strictly_more_specific_than(&less));
    assert!(!less.strictly_more_specific_than(&more));
}

#[test]
fn empty_constraints_beaten_by_any() {
    let empty = tc(false, false, false, false, false);
    let one = tc(false, false, true, false, false);
    assert!(one.strictly_more_specific_than(&empty));
    assert!(!empty.strictly_more_specific_than(&one));
    // empty vs empty: not strictly more specific than itself.
    assert!(!empty.strictly_more_specific_than(&empty));
}

#[test]
fn disjoint_constraint_sets_are_neither_superset() {
    // {device, velocity_range} vs {device, channel} — same size, differ in
    // one dimension each → neither is a superset of the other.
    let a = tc(true, false, false, false, true);
    let b = tc(true, true, false, false, false);
    assert!(!a.strictly_more_specific_than(&b));
    assert!(!b.strictly_more_specific_than(&a));
    assert!(a.is_disjoint_with(&b));
}

#[test]
fn more_specific_rule_wins_even_when_declared_later() {
    // Two rules on the same device for the same note: one adds a channel
    // constraint (a strict superset), declared SECOND. For an event on that
    // channel the superset must win despite source order — that's the whole
    // point of specificity ordering replacing first-match-wins here.
    let toml = r#"
[[bindings]]
alias = "pads"
[bindings.input]
matchers = [{ type = "NameContains", value = "Mikro" }]

[[modes]]
name = "Default"

[[modes.mappings]]
description = "note-only"
trigger = { type = "Note", note = 36, device = "pads" }
action = { type = "Keystroke", keys = "a" }

[[modes.mappings]]
description = "note-plus-channel"
trigger = { type = "Note", note = 36, channel = 0, device = "pads" }
action = { type = "Keystroke", keys = "b" }
"#;
    let rules = compile(toml);
    // Event on channel 0 matches BOTH rules; the channel-constrained
    // (more specific) rule must be selected.
    assert_eq!(
        matched(&rules, Some("pads")),
        "note-plus-channel",
        "the strict-superset rule must win regardless of declaration order"
    );
}

#[test]
fn cc_value_min_rule_wins_even_when_declared_later() {
    // Two CC rules in the same bucket: one adds a value_min constraint
    // (strict superset), declared SECOND. For value=100 both match, so the
    // constrained rule must win.
    let toml = r#"
[[bindings]]
alias = "pads"
[bindings.input]
matchers = [{ type = "NameContains", value = "Mikro" }]

[[modes]]
name = "Default"

[[modes.mappings]]
description = "cc-any-value"
trigger = { type = "CC", cc = 7, device = "pads" }
action = { type = "Keystroke", keys = "a" }

[[modes.mappings]]
description = "cc-min-64"
trigger = { type = "CC", cc = 7, value_min = 64, device = "pads" }
action = { type = "Keystroke", keys = "b" }
"#;
    let rules = compile(toml);
    let matched = rules
        .match_event_with_provenance(&cc(7, 100), 0, Some("pads"))
        .and_then(|e| e.matched_rule)
        .expect("a rule matches CC 7 value 100");
    assert_eq!(
        matched, "cc-min-64",
        "the strict-superset CC rule must win regardless of declaration order"
    );
}

#[test]
fn program_change_specific_pc_wins_over_wildcard_declared_first() {
    // #2132: a wildcard ProgramChange rule (`pc` unset = any program) declared
    // FIRST must not shadow a program-specific rule (`pc = 5`). The specific
    // rule fixes the `program` dimension (a strict superset), so it must sort
    // ahead of the wildcard and win for an event on that program — exactly as
    // CC `value_min` and Note `channel` already do above.
    let toml = r#"
[[modes]]
name = "Default"

[[modes.mappings]]
description = "pc-any"
trigger = { type = "ProgramChange" }
action = { type = "Keystroke", keys = "a" }

[[modes.mappings]]
description = "pc-5"
trigger = { type = "ProgramChange", pc = 5 }
action = { type = "Keystroke", keys = "b" }
"#;
    let rules = compile(toml);

    let pc5 = ProcessedEvent::ProgramChange {
        program: 5,
        channel: Some(0),
    };
    let matched = rules
        .match_event_with_provenance(&pc5, 0, None)
        .and_then(|e| e.matched_rule)
        .expect("a rule matches ProgramChange program 5");
    assert_eq!(
        matched, "pc-5",
        "the program-specific rule must win over the wildcard declared first (#2132)"
    );

    // The wildcard still serves a program with no specific rule.
    let pc9 = ProcessedEvent::ProgramChange {
        program: 9,
        channel: Some(0),
    };
    let matched_any = rules
        .match_event_with_provenance(&pc9, 0, None)
        .and_then(|e| e.matched_rule)
        .expect("the wildcard matches ProgramChange program 9");
    assert_eq!(
        matched_any, "pc-any",
        "the wildcard must still match programs without a specific rule"
    );
}

#[test]
fn equal_specificity_preserves_config_order() {
    // Note 36 and Note 37 share the same constraint dimensions
    // ({device, note}); neither is a superset. They must keep config order
    // in the compiled bucket (stable sort) — note36 before note37.
    let toml = r#"
[[bindings]]
alias = "pads"
[bindings.input]
matchers = [{ type = "NameContains", value = "Mikro" }]

[[modes]]
name = "Default"

[[modes.mappings]]
description = "note36"
trigger = { type = "Note", note = 36, device = "pads" }
action = { type = "Keystroke", keys = "a" }

[[modes.mappings]]
description = "note37"
trigger = { type = "Note", note = 37, device = "pads" }
action = { type = "Keystroke", keys = "b" }
"#;
    let rules = compile(toml);
    let mode = rules.mode_rules(0).expect("Default mode");
    let order: Vec<_> = mode
        .specific_device_rules
        .get("pads")
        .expect("pads bucket")
        .iter()
        .map(|r| r.description.as_deref().unwrap_or(""))
        .collect();
    assert_eq!(
        order,
        vec!["note36", "note37"],
        "equal-specificity rules keep config order (stable sort)"
    );
}

#[test]
fn structurally_identical_rules_in_same_bucket_are_a_validation_error() {
    // Two structurally identical triggers in the same sub-bucket: the
    // second can never fire (first-match-wins on identical match sets).
    // ADR-037 D2 (promoted to error per Council R2).
    let cfg: Config = toml::from_str(
        r#"
[[bindings]]
alias = "pads"
[bindings.input]
matchers = [{ type = "NameContains", value = "Mikro" }]

[[modes]]
name = "Default"

[[modes.mappings]]
trigger = { type = "Note", note = 36, device = "pads" }
action = { type = "Keystroke", keys = "a" }

[[modes.mappings]]
trigger = { type = "Note", note = 36, device = "pads" }
action = { type = "Keystroke", keys = "b" }
"#,
    )
    .expect("config parses");
    let report = conductor_core::config::validation::validate_config(&cfg);
    assert!(
        report.errors.iter().any(|e| {
            e.message.to_lowercase().contains("identical")
                || e.message.to_lowercase().contains("duplicate")
                || e.message.to_lowercase().contains("never fire")
        }),
        "identical triggers in one bucket must error; got: {:#?}",
        report.errors
    );
}

#[test]
fn distinct_notes_in_same_bucket_are_not_a_duplicate_error() {
    // Note 36 and Note 37 share constraint *dimensions* but are different
    // triggers — must NOT be flagged as duplicates (a common config).
    let cfg: Config = toml::from_str(
        r#"
[[bindings]]
alias = "pads"
[bindings.input]
matchers = [{ type = "NameContains", value = "Mikro" }]

[[modes]]
name = "Default"

[[modes.mappings]]
trigger = { type = "Note", note = 36, device = "pads" }
action = { type = "Keystroke", keys = "a" }

[[modes.mappings]]
trigger = { type = "Note", note = 37, device = "pads" }
action = { type = "Keystroke", keys = "b" }
"#,
    )
    .expect("config parses");
    let report = conductor_core::config::validation::validate_config(&cfg);
    let dup_errors: Vec<_> = report
        .errors
        .iter()
        .filter(|e| {
            let m = e.message.to_lowercase();
            m.contains("identical") || m.contains("duplicate") || m.contains("never fire")
        })
        .collect();
    assert!(
        dup_errors.is_empty(),
        "distinct notes must not be a duplicate error; got: {:#?}",
        dup_errors
    );
}
