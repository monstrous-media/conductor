// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-025 Phase 2.A+B: state condition variants.
//!
//! Verifies that `Condition::ActivePcIs`, `Condition::CcValueInRange`,
//! and `Condition::NoteHeld` read from the physical control state store
//! through `ConditionContext.state` and evaluate against the live store.
//!
//! User-validatable end-to-end: hand-author a Conditional TOML mapping
//! with `ActivePcIs`, stomp a preset, verify the triggered action only
//! fires when the active PC matches.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use conductor_core::Condition;
use conductor_core::control_state::PhysicalControlStateStore;
use conductor_core::event_processor::MidiEvent;
use conductor_daemon::conditions::{ConditionContext, evaluate_condition};

fn mk_store() -> Arc<PhysicalControlStateStore> {
    Arc::new(PhysicalControlStateStore::default())
}

fn ctx(store: &Arc<PhysicalControlStateStore>) -> ConditionContext {
    ConditionContext::with_state(Arc::clone(store))
}

fn pc(program: u8, channel: u8) -> MidiEvent {
    MidiEvent::ProgramChange {
        program,
        channel,
        time: Instant::now(),
    }
}

fn cc(cc: u8, value: u8, channel: u8) -> MidiEvent {
    MidiEvent::ControlChange {
        cc,
        value,
        channel,
        time: Instant::now(),
    }
}

fn note_on(note: u8, velocity: u8, channel: u8) -> MidiEvent {
    MidiEvent::NoteOn {
        note,
        velocity,
        channel,
        time: Instant::now(),
    }
}

fn note_off(note: u8, channel: u8) -> MidiEvent {
    MidiEvent::NoteOff {
        note,
        channel,
        time: Instant::now(),
    }
}

// ────────────────────────────────────────────────────────────────
// ActivePcIs
// ────────────────────────────────────────────────────────────────

#[test]
fn active_pc_is_matches_when_pc_equals() {
    let store = mk_store();
    store.observe_event("fcb1010", &pc(12, 0));

    let cond = Condition::ActivePcIs {
        pc: 12,
        channel: 0,
        device: "fcb1010".into(),
    };
    assert!(evaluate_condition(&cond, Some(&ctx(&store))));
}

#[test]
fn active_pc_is_fails_when_pc_differs() {
    let store = mk_store();
    store.observe_event("fcb1010", &pc(12, 0));

    let cond = Condition::ActivePcIs {
        pc: 7,
        channel: 0,
        device: "fcb1010".into(),
    };
    assert!(!evaluate_condition(&cond, Some(&ctx(&store))));
}

#[test]
fn active_pc_is_fails_when_no_pc_received() {
    // Fresh store, no PC ever observed. Condition must be false
    // (not true, not panic) — ADR-025 D7: unknown state = false.
    let store = mk_store();

    let cond = Condition::ActivePcIs {
        pc: 0,
        channel: 0,
        device: "fcb1010".into(),
    };
    assert!(!evaluate_condition(&cond, Some(&ctx(&store))));
}

#[test]
fn active_pc_is_fails_when_wrong_channel() {
    let store = mk_store();
    store.observe_event("fcb1010", &pc(12, 0));

    let cond = Condition::ActivePcIs {
        pc: 12,
        channel: 5,
        device: "fcb1010".into(),
    };
    assert!(!evaluate_condition(&cond, Some(&ctx(&store))));
}

#[test]
fn active_pc_is_fails_when_wrong_device() {
    let store = mk_store();
    store.observe_event("fcb1010", &pc(12, 0));

    let cond = Condition::ActivePcIs {
        pc: 12,
        channel: 0,
        device: "mc6".into(),
    };
    assert!(!evaluate_condition(&cond, Some(&ctx(&store))));
}

#[test]
fn active_pc_is_fails_when_no_state_in_context() {
    // No state attached to context — condition can't resolve, returns false.
    let cond = Condition::ActivePcIs {
        pc: 12,
        channel: 0,
        device: "fcb1010".into(),
    };
    let empty = ConditionContext::default();
    assert!(!evaluate_condition(&cond, Some(&empty)));
    assert!(!evaluate_condition(&cond, None));
}

// ────────────────────────────────────────────────────────────────
// CcValueInRange
// ────────────────────────────────────────────────────────────────

#[test]
fn cc_value_in_range_inclusive_bounds() {
    let store = mk_store();

    // Value exactly at min → true
    store.observe_event("keyboard", &cc(1, 64, 0));
    let cond = Condition::CcValueInRange {
        cc: 1,
        channel: 0,
        min: 64,
        max: 127,
        device: "keyboard".into(),
    };
    assert!(evaluate_condition(&cond, Some(&ctx(&store))));

    // Value exactly at max
    store.observe_event("keyboard", &cc(1, 127, 0));
    assert!(evaluate_condition(&cond, Some(&ctx(&store))));

    // Value just below min
    store.observe_event("keyboard", &cc(1, 63, 0));
    assert!(!evaluate_condition(&cond, Some(&ctx(&store))));

    // Value just above max (still within range because max=127)
    store.observe_event("keyboard", &cc(1, 126, 0));
    assert!(evaluate_condition(&cond, Some(&ctx(&store))));
}

#[test]
fn cc_value_in_range_fails_when_cc_unseen() {
    let store = mk_store();
    // CC 1 received but we query CC 2
    store.observe_event("keyboard", &cc(1, 100, 0));
    let cond = Condition::CcValueInRange {
        cc: 2,
        channel: 0,
        min: 0,
        max: 127,
        device: "keyboard".into(),
    };
    assert!(!evaluate_condition(&cond, Some(&ctx(&store))));
}

// ────────────────────────────────────────────────────────────────
// NoteHeld
// ────────────────────────────────────────────────────────────────

#[test]
fn note_held_true_while_held() {
    let store = mk_store();
    store.observe_event("keyboard", &note_on(60, 100, 0));

    let cond = Condition::NoteHeld {
        note: 60,
        channel: 0,
        device: "keyboard".into(),
        ttl_override_ms: None,
    };
    assert!(evaluate_condition(&cond, Some(&ctx(&store))));
}

#[test]
fn note_held_false_after_note_off() {
    let store = mk_store();
    store.observe_event("keyboard", &note_on(60, 100, 0));
    store.observe_event("keyboard", &note_off(60, 0));

    let cond = Condition::NoteHeld {
        note: 60,
        channel: 0,
        device: "keyboard".into(),
        ttl_override_ms: None,
    };
    assert!(!evaluate_condition(&cond, Some(&ctx(&store))));
}

#[test]
fn note_held_false_after_ttl_expiry() {
    // Store with a short TTL; the note's lazy eviction kicks in on read.
    let store = Arc::new(PhysicalControlStateStore::new(10)); // 10ms
    store.observe_event("keyboard", &note_on(60, 100, 0));

    thread::sleep(Duration::from_millis(25));

    let cond = Condition::NoteHeld {
        note: 60,
        channel: 0,
        device: "keyboard".into(),
        ttl_override_ms: None,
    };
    assert!(!evaluate_condition(
        &cond,
        Some(&ConditionContext::with_state(Arc::clone(&store)))
    ));
}

#[test]
fn note_held_false_when_no_state() {
    let cond = Condition::NoteHeld {
        note: 60,
        channel: 0,
        device: "keyboard".into(),
        ttl_override_ms: None,
    };
    assert!(!evaluate_condition(&cond, None));
}

// ────────────────────────────────────────────────────────────────
// Logical composition — state conditions work with And/Or/Not
// ────────────────────────────────────────────────────────────────

#[test]
fn state_conditions_compose_with_and() {
    let store = mk_store();
    store.observe_event("fcb1010", &pc(12, 0));
    store.observe_event("keyboard", &note_on(60, 100, 0));

    let cond = Condition::And {
        conditions: vec![
            Condition::ActivePcIs {
                pc: 12,
                channel: 0,
                device: "fcb1010".into(),
            },
            Condition::NoteHeld {
                note: 60,
                channel: 0,
                device: "keyboard".into(),
                ttl_override_ms: None,
            },
        ],
    };
    assert!(evaluate_condition(&cond, Some(&ctx(&store))));
}

#[test]
fn state_conditions_compose_with_not() {
    let store = mk_store();
    // Never any PC received → ActivePcIs is false → Not() is true
    let cond = Condition::Not {
        condition: Box::new(Condition::ActivePcIs {
            pc: 0,
            channel: 0,
            device: "fcb1010".into(),
        }),
    };
    assert!(evaluate_condition(&cond, Some(&ctx(&store))));
}

// ────────────────────────────────────────────────────────────────
// ConditionContext builder ergonomics
// ────────────────────────────────────────────────────────────────

// ────────────────────────────────────────────────────────────────
// TOML round-trip — proves the user-facing config syntax works
// ────────────────────────────────────────────────────────────────

/// Regression: parse the exact Conditional+ActivePcIs TOML pattern
/// from PR #844's manual-validation example. The Condition enum was
/// missing `#[serde(tag = "type")]` so external authors hit
/// "invalid value: map, expected map with a single key" the moment
/// they wrote `type = "ActivePcIs"` in TOML.
#[test]
fn conditional_with_active_pc_is_parses_from_toml() {
    use conductor_core::config::types::Config;

    let toml_input = r#"
[[modes]]
name = "Default"

[[modes.mappings]]
description = "Test PC condition"

[modes.mappings.trigger]
type = "Note"
note = 60

[modes.mappings.action]
type = "Conditional"

[modes.mappings.action.condition]
type = "ActivePcIs"
pc = 12
channel = 0
device = "fcb1010"

[modes.mappings.action.then_action]
type = "Shell"
command = "say -v Fred 'Lead'"

[modes.mappings.action.else_action]
type = "Shell"
command = "say -v Fred 'Rhythm'"
"#;

    let cfg: Config = toml::from_str(toml_input).expect("Conditional+ActivePcIs TOML should parse");
    let mode = &cfg.modes[0];
    let mapping = &mode.mappings[0];
    use conductor_core::config::types::ActionConfig;
    match &mapping.action {
        ActionConfig::Conditional {
            condition,
            then_action: _,
            else_action: _,
        } => match condition {
            Condition::ActivePcIs {
                pc,
                channel,
                device,
            } => {
                assert_eq!(*pc, 12);
                assert_eq!(*channel, 0);
                assert_eq!(device, "fcb1010");
            }
            other => panic!("expected ActivePcIs, got {:?}", other),
        },
        other => panic!("expected Conditional action, got {:?}", other),
    }
}

#[test]
fn conditional_with_cc_value_in_range_parses_from_toml() {
    use conductor_core::config::types::Config;

    let toml_input = r#"
[[modes]]
name = "Default"

[[modes.mappings]]

[modes.mappings.trigger]
type = "Note"
note = 60

[modes.mappings.action]
type = "Conditional"

[modes.mappings.action.condition]
type = "CcValueInRange"
cc = 1
channel = 0
min = 64
max = 127
device = "keyboard"

[modes.mappings.action.then_action]
type = "Shell"
command = "echo loud"
"#;

    let cfg: Config = toml::from_str(toml_input).expect("CcValueInRange TOML should parse");
    use conductor_core::config::types::ActionConfig;
    if let ActionConfig::Conditional { condition, .. } = &cfg.modes[0].mappings[0].action
        && let Condition::CcValueInRange { min, max, .. } = condition
    {
        assert_eq!(*min, 64);
        assert_eq!(*max, 127);
    } else {
        panic!("expected Conditional/CcValueInRange");
    }
}

#[test]
fn conditional_with_note_held_parses_from_toml() {
    use conductor_core::config::types::Config;

    let toml_input = r#"
[[modes]]
name = "Default"

[[modes.mappings]]

[modes.mappings.trigger]
type = "Note"
note = 36

[modes.mappings.action]
type = "Conditional"

[modes.mappings.action.condition]
type = "NoteHeld"
note = 64
channel = 0
device = "keyboard"

[modes.mappings.action.then_action]
type = "Shell"
command = "echo sustained"
"#;

    let cfg: Config = toml::from_str(toml_input).expect("NoteHeld TOML should parse");
    use conductor_core::config::types::ActionConfig;
    if let ActionConfig::Conditional { condition, .. } = &cfg.modes[0].mappings[0].action
        && let Condition::NoteHeld { note, .. } = condition
    {
        assert_eq!(*note, 64);
    } else {
        panic!("expected Conditional/NoteHeld");
    }
}

/// Regression: existing pre-state Condition variants must still parse
/// after the `#[serde(tag = "type")]` addition. ModeIs is the canonical
/// one in production configs.
#[test]
fn pre_existing_mode_is_condition_still_parses_from_toml() {
    use conductor_core::config::types::Config;

    let toml_input = r#"
[[modes]]
name = "Default"

[[modes.mappings]]

[modes.mappings.trigger]
type = "Note"
note = 36

[modes.mappings.action]
type = "Conditional"

[modes.mappings.action.condition]
type = "ModeIs"
mode = "Performance"

[modes.mappings.action.then_action]
type = "Shell"
command = "echo perf"
"#;

    let cfg: Config = toml::from_str(toml_input).expect("ModeIs TOML should parse");
    use conductor_core::config::types::ActionConfig;
    if let ActionConfig::Conditional { condition, .. } = &cfg.modes[0].mappings[0].action
        && let Condition::ModeIs { mode } = condition
    {
        assert_eq!(mode, "Performance");
    } else {
        panic!("expected Conditional/ModeIs");
    }
}

#[test]
fn condition_context_with_state_and_mode_composition() {
    let store = mk_store();
    let ctx = ConditionContext::with_state(Arc::clone(&store)).and_mode("Default".into());
    assert_eq!(ctx.current_mode.as_deref(), Some("Default"));
    assert!(ctx.state.is_some());
}

// ─── ADR-025 Phase 2.C: CcIsOn / CcIsOff sugar ───────────────────
//
// Canonical MIDI switch-CC convention: values 64..=127 are "on" and
// 0..=63 are "off" (MIDI 1.0 spec for CC 64 Sustain, CC 66 Sostenuto,
// etc. — applies by convention to any CC used as a toggle). The sugar
// variants let config authors write `CcIsOn { cc: 64 }` instead of
// spelling out the full `CcValueInRange { min: 64, max: 127 }` every
// time.
//
// The test bar for sugar: (a) must parse from TOML, (b) must behave
// identically to the canonical CcValueInRange with the conventional
// bounds, (c) must work inside And/Or/Not, (d) must pass validation.

#[test]
fn cc_is_on_matches_value_64_and_above() {
    let store = mk_store();
    let cond = Condition::CcIsOn {
        cc: 64,
        channel: 0,
        device: "keyboard".into(),
    };

    // Boundary: exactly 64 → on
    store.observe_event("keyboard", &cc(64, 64, 0));
    assert!(evaluate_condition(&cond, Some(&ctx(&store))));

    // Max: 127 → on
    store.observe_event("keyboard", &cc(64, 127, 0));
    assert!(evaluate_condition(&cond, Some(&ctx(&store))));

    // Below threshold: 63 → off
    store.observe_event("keyboard", &cc(64, 63, 0));
    assert!(!evaluate_condition(&cond, Some(&ctx(&store))));

    // Zero: 0 → off
    store.observe_event("keyboard", &cc(64, 0, 0));
    assert!(!evaluate_condition(&cond, Some(&ctx(&store))));
}

#[test]
fn cc_is_off_matches_value_63_and_below() {
    let store = mk_store();
    let cond = Condition::CcIsOff {
        cc: 64,
        channel: 0,
        device: "keyboard".into(),
    };

    // Zero: 0 → off-condition matches
    store.observe_event("keyboard", &cc(64, 0, 0));
    assert!(evaluate_condition(&cond, Some(&ctx(&store))));

    // Boundary: exactly 63 → off-condition matches
    store.observe_event("keyboard", &cc(64, 63, 0));
    assert!(evaluate_condition(&cond, Some(&ctx(&store))));

    // Just above: 64 → off-condition does NOT match
    store.observe_event("keyboard", &cc(64, 64, 0));
    assert!(!evaluate_condition(&cond, Some(&ctx(&store))));

    // Max: 127 → off-condition does NOT match
    store.observe_event("keyboard", &cc(64, 127, 0));
    assert!(!evaluate_condition(&cond, Some(&ctx(&store))));
}

#[test]
fn cc_is_on_false_when_cc_never_observed() {
    // No CC on this (device, channel, cc) has been observed —
    // sugar must inherit the "absence = false" rule from
    // CcValueInRange, not secretly match on default 0.
    let store = mk_store();
    let cond = Condition::CcIsOn {
        cc: 64,
        channel: 0,
        device: "keyboard".into(),
    };
    assert!(!evaluate_condition(&cond, Some(&ctx(&store))));
}

#[test]
fn cc_is_off_false_when_cc_never_observed() {
    // Slightly counter-intuitive but correct: if we've never seen
    // the CC, we don't know it's "off" either. Both sugar variants
    // report false when state is absent. If the user wants "sustain
    // default-off at startup", they can send a CC 64 value=0 as an
    // initial sync.
    let store = mk_store();
    let cond = Condition::CcIsOff {
        cc: 64,
        channel: 0,
        device: "keyboard".into(),
    };
    assert!(!evaluate_condition(&cond, Some(&ctx(&store))));
}

#[test]
fn cc_is_on_behaves_identically_to_cc_value_in_range_64_127() {
    // The sugar is DEFINED as CcValueInRange { min:64, max:127 } —
    // this test pins the semantic equivalence so any future change
    // to CcValueInRange semantics doesn't silently drift.
    let store = mk_store();
    let sugar = Condition::CcIsOn {
        cc: 7,
        channel: 0,
        device: "keyboard".into(),
    };
    let canonical = Condition::CcValueInRange {
        cc: 7,
        channel: 0,
        min: 64,
        max: 127,
        device: "keyboard".into(),
    };

    for v in [0u8, 1, 32, 63, 64, 100, 127] {
        store.observe_event("keyboard", &cc(7, v, 0));
        let c = ctx(&store);
        assert_eq!(
            evaluate_condition(&sugar, Some(&c)),
            evaluate_condition(&canonical, Some(&c)),
            "CcIsOn must match CcValueInRange{{64..=127}} at value={}",
            v
        );
    }
}

#[test]
fn cc_is_off_behaves_identically_to_cc_value_in_range_0_63() {
    let store = mk_store();
    let sugar = Condition::CcIsOff {
        cc: 7,
        channel: 0,
        device: "keyboard".into(),
    };
    let canonical = Condition::CcValueInRange {
        cc: 7,
        channel: 0,
        min: 0,
        max: 63,
        device: "keyboard".into(),
    };

    for v in [0u8, 1, 32, 63, 64, 100, 127] {
        store.observe_event("keyboard", &cc(7, v, 0));
        let c = ctx(&store);
        assert_eq!(
            evaluate_condition(&sugar, Some(&c)),
            evaluate_condition(&canonical, Some(&c)),
            "CcIsOff must match CcValueInRange{{0..=63}} at value={}",
            v
        );
    }
}

#[test]
fn cc_is_on_composes_with_and() {
    let store = mk_store();
    store.observe_event("fcb1010", &pc(12, 0));
    store.observe_event("keyboard", &cc(64, 100, 0));

    let cond = Condition::And {
        conditions: vec![
            Condition::ActivePcIs {
                pc: 12,
                channel: 0,
                device: "fcb1010".into(),
            },
            Condition::CcIsOn {
                cc: 64,
                channel: 0,
                device: "keyboard".into(),
            },
        ],
    };
    assert!(evaluate_condition(&cond, Some(&ctx(&store))));
}

// ─── TOML round-trip ─────────────────────────────────────────────

#[test]
fn cc_is_on_parses_from_toml() {
    use conductor_core::config::types::{ActionConfig, Config};

    let toml_input = r#"
[[modes]]
name = "Default"

[[modes.mappings]]

[modes.mappings.trigger]
type = "Note"
note = 60

[modes.mappings.action]
type = "Conditional"

[modes.mappings.action.condition]
type = "CcIsOn"
cc = 64
channel = 0
device = "keyboard"

[modes.mappings.action.then_action]
type = "Shell"
command = "echo sustain_on"
"#;

    let cfg: Config = toml::from_str(toml_input).expect("CcIsOn TOML should parse");
    match &cfg.modes[0].mappings[0].action {
        ActionConfig::Conditional { condition, .. } => match condition {
            Condition::CcIsOn {
                cc,
                channel,
                device,
            } => {
                assert_eq!(*cc, 64);
                assert_eq!(*channel, 0);
                assert_eq!(device, "keyboard");
            }
            other => panic!("expected CcIsOn, got {:?}", other),
        },
        other => panic!("expected Conditional action, got {:?}", other),
    }
}

#[test]
fn cc_is_off_parses_from_toml() {
    use conductor_core::config::types::{ActionConfig, Config};

    let toml_input = r#"
[[modes]]
name = "Default"

[[modes.mappings]]

[modes.mappings.trigger]
type = "Note"
note = 60

[modes.mappings.action]
type = "Conditional"

[modes.mappings.action.condition]
type = "CcIsOff"
cc = 64
channel = 0
device = "keyboard"

[modes.mappings.action.then_action]
type = "Shell"
command = "echo sustain_off"
"#;

    let cfg: Config = toml::from_str(toml_input).expect("CcIsOff TOML should parse");
    if let ActionConfig::Conditional { condition, .. } = &cfg.modes[0].mappings[0].action
        && let Condition::CcIsOff {
            cc,
            channel,
            device,
        } = condition
    {
        assert_eq!(*cc, 64);
        assert_eq!(*channel, 0);
        assert_eq!(device, "keyboard");
    } else {
        panic!("expected Conditional/CcIsOff");
    }
}
