// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! ADR-040 §4.4 / §D6 Phase 1 — `Conditional` + top-level `ModeIs` deprecation.
//!
//! A `Conditional` whose *outermost* condition is `ModeIs` (with the else branch
//! absent or itself a `ModeIs` dispatch chain) is mode-scoping expressed the hard
//! way and earns a `Severity::Warning`. Composite conditions (`And`/`Or`/`Not`
//! wrapping `ModeIs`) express mode∩app etc. and stay silent. The config still
//! validates — this is a warning, never an error (Phase 1).

use conductor_core::actions::Condition;
use conductor_core::config::validation::validate_config;
use conductor_core::{ActionConfig, Config, Mapping, Mode, Trigger};

/// Distinctive phrase from `MODEIS_DEPRECATION_HINT`. `ValidationReport` findings
/// carry only a free-text `message` (no stable machine code), so keying on this
/// phrase is the available way to isolate THIS validator's warnings from any
/// other warning the full `validate_config` pass may emit.
const HINT_MARK: &str = "mode-scoping expressed the hard way";

/// Chain depths chosen relative to the validator's internal action-depth cap
/// (`MAX_ACTION_DEPTH = 64`, private to `validation.rs`). `WITHIN` sits
/// comfortably under it (the deprecation still fires normally); `OVER` is far
/// past it (exercises the depth guards — recursion stops, no stack overflow).
const WITHIN_DEPTH_CAP: usize = 40;
const OVER_DEPTH_CAP: usize = 300;

fn note_trigger(note: u8) -> Trigger {
    Trigger::Note {
        note,
        velocity_min: None,
        channel: None,
        device: None,
    }
}

fn keystroke() -> ActionConfig {
    ActionConfig::Keystroke {
        keys: "a".to_string(),
        modifiers: vec![],
    }
}

fn modeis(mode: &str) -> Condition {
    Condition::ModeIs {
        mode: mode.to_string(),
    }
}

fn conditional(condition: Condition, else_action: Option<ActionConfig>) -> ActionConfig {
    ActionConfig::Conditional {
        condition,
        then_action: Box::new(keystroke()),
        else_action: else_action.map(Box::new),
    }
}

/// A `Conditional` with an explicit then-branch (for nesting tests).
fn conditional_then(
    condition: Condition,
    then_action: ActionConfig,
    else_action: Option<ActionConfig>,
) -> ActionConfig {
    ActionConfig::Conditional {
        condition,
        then_action: Box::new(then_action),
        else_action: else_action.map(Box::new),
    }
}

/// One mode ("Mix") with a single mapping carrying `action`.
fn config_with_action(action: ActionConfig) -> Config {
    Config {
        modes: vec![Mode {
            name: "Mix".to_string(),
            color: None,
            mappings: vec![Mapping {
                trigger: note_trigger(36),
                action,
                description: None,
                let_through: false,
            }],
        }],
        ..Config::default_config()
    }
}

/// Count only the Conditional+ModeIs deprecation warnings (by their hint marker),
/// so unrelated validator warnings don't pollute the assertions.
fn modeis_warnings(config: &Config) -> usize {
    validate_config(config)
        .warnings
        .iter()
        .filter(|w| w.message.contains(HINT_MARK))
        .count()
}

// ── Deprecated: top-level ModeIs ───────────────────────────────────────────

#[test]
fn top_level_modeis_no_else_warns() {
    // `if ModeIs("Mix") then X` — pure mode-scoping, deprecated.
    let cfg = config_with_action(conditional(modeis("Mix"), None));
    assert_eq!(modeis_warnings(&cfg), 1);
}

#[test]
fn chained_modeis_warns_exactly_once() {
    // `if ModeIs(A) then X else if ModeIs(B) then Y` — one ModeIs dispatch chain,
    // a single deprecation (not one per link).
    let inner = conditional(modeis("Edit"), None);
    let cfg = config_with_action(conditional(modeis("Mix"), Some(inner)));
    assert_eq!(modeis_warnings(&cfg), 1);
}

#[test]
fn global_mapping_modeis_warns() {
    // The walk covers global_mappings too, not just per-mode mappings.
    let mut cfg = config_with_action(keystroke()); // benign per-mode mapping
    cfg.global_mappings = vec![Mapping {
        trigger: note_trigger(60),
        action: conditional(modeis("Mix"), None),
        description: None,
        let_through: false,
    }];
    assert_eq!(modeis_warnings(&cfg), 1);
}

// ── NOT deprecated: composites and real branching ──────────────────────────

#[test]
fn composite_and_modeis_is_silent() {
    // `And(ModeIs, AppFrontmost)` expresses mode∩app — mode-scoping can't, so it
    // is NOT deprecated.
    let cond = Condition::And {
        conditions: vec![
            modeis("Mix"),
            Condition::AppFrontmost {
                app_name: "OBS".to_string(),
            },
        ],
    };
    let cfg = config_with_action(conditional(cond, None));
    assert_eq!(modeis_warnings(&cfg), 0);
}

#[test]
fn not_modeis_is_silent() {
    // `Not(ModeIs)` = "when NOT in this mode" — the inverse of mode-scoping, not
    // deprecated.
    let cond = Condition::Not {
        condition: Box::new(modeis("Mix")),
    };
    let cfg = config_with_action(conditional(cond, None));
    assert_eq!(modeis_warnings(&cfg), 0);
}

#[test]
fn modeis_with_plain_else_is_silent() {
    // `if ModeIs(Mix) then X else Y(plain)` — the else is real branching, not a
    // ModeIs chain, so per §4.4 it is NOT the deprecated pure-dispatch shape.
    let cfg = config_with_action(conditional(modeis("Mix"), Some(keystroke())));
    assert_eq!(modeis_warnings(&cfg), 0);
}

#[test]
fn non_modeis_conditional_is_silent() {
    // A Conditional on a non-ModeIs condition is untouched.
    let cond = Condition::AppFrontmost {
        app_name: "Safari".to_string(),
    };
    let cfg = config_with_action(conditional(cond, None));
    assert_eq!(modeis_warnings(&cfg), 0);
}

#[test]
fn composite_or_modeis_is_silent() {
    // `Or(ModeIs, ModeIs)` — outermost is `Or`, not `ModeIs`, so not deprecated
    // (composite, even though both arms are ModeIs).
    let cond = Condition::Or {
        conditions: vec![modeis("Mix"), modeis("Edit")],
    };
    let cfg = config_with_action(conditional(cond, None));
    assert_eq!(modeis_warnings(&cfg), 0);
}

// ── Walker recurses into Sequence / Repeat ─────────────────────────────────

#[test]
fn dispatch_nested_in_sequence_warns() {
    // A ModeIs dispatch buried in a Sequence is still mode-the-hard-way → warns.
    let action = ActionConfig::Sequence {
        actions: vec![keystroke(), conditional(modeis("Mix"), None)],
    };
    let cfg = config_with_action(action);
    assert_eq!(modeis_warnings(&cfg), 1);
}

#[test]
fn dispatch_nested_in_repeat_warns() {
    let action = ActionConfig::Repeat {
        action: Box::new(conditional(modeis("Mix"), None)),
        count: 2,
        delay_ms: None,
    };
    let cfg = config_with_action(action);
    assert_eq!(modeis_warnings(&cfg), 1);
}

// ── Severity contract ──────────────────────────────────────────────────────

// ── Copilot #2334: nested dispatch inside a chain link + deep-chain safety ──

#[test]
fn nested_dispatch_inside_a_chain_link_then_branch_is_caught() {
    // `if ModeIs(A) then (if ModeIs(X) then K) else if ModeIs(B) then K`
    // The outer A→B chain is one deprecation; the `if ModeIs(X)` buried in A's
    // then-branch is a SEPARATE deprecated dispatch and must also warn.
    let nested = conditional(modeis("X"), None);
    let chain = conditional_then(
        modeis("A"),
        nested,                               // then = nested dispatch
        Some(conditional(modeis("B"), None)), // else = next chain link
    );
    let cfg = config_with_action(chain);
    assert_eq!(
        modeis_warnings(&cfg),
        2,
        "outer chain (1) + nested dispatch in the then-branch (1)"
    );
}

/// Build a `ModeIs` dispatch chain of `links` nested `else if` levels.
fn modeis_chain(links: usize) -> ActionConfig {
    let mut action = conditional(modeis("m0"), None);
    for i in 1..links {
        action = conditional_then(modeis(&format!("m{i}")), keystroke(), Some(action));
    }
    action
}

#[test]
fn modeis_chain_within_depth_bound_warns_once_and_is_valid() {
    // A reasonably deep (well under the cap) pure-ModeIs chain is the deprecated
    // shape: it warns exactly once and the config still validates. This pins the
    // deprecation BOUNDARY (vs. the overflow test below, which only proves the
    // bound stops recursion) — Council #2334.
    let cfg = config_with_action(modeis_chain(WITHIN_DEPTH_CAP));
    assert_eq!(modeis_warnings(&cfg), 1, "one warning for the whole chain");
    assert!(validate_config(&cfg).is_valid(), "within depth — no error");
}

#[test]
fn deeply_nested_else_chain_does_not_overflow() {
    // A pathologically deep `else if ModeIs(…)` chain (user-controlled) must not
    // blow the stack — every walker here is depth-bounded. The assertion is that
    // validation RETURNS (no stack overflow); past the action-depth cap the
    // generic guard also reports an error rather than crashing.
    let cfg = config_with_action(modeis_chain(OVER_DEPTH_CAP));
    let report = validate_config(&cfg);
    assert!(
        !report.is_valid(),
        "a config past the action-depth cap should error (not panic)"
    );
}

#[test]
fn deprecation_is_warning_not_error() {
    // Phase 1: the deprecated shape warns but the config still validates.
    let cfg = config_with_action(conditional(modeis("Mix"), None));
    let report = validate_config(&cfg);
    assert!(
        report.is_valid(),
        "deprecation must not produce a validation error: {:?}",
        report.errors
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.message.contains(HINT_MARK)),
        "expected a deprecation warning"
    );
}
