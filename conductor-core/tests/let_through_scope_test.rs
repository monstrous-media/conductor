// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! ADR-038 Slice 2 — `let_through` is winner-metadata threaded onto the
//! `ActionEnvelope`; it does NOT alter the match algorithm.
//!
//! A `let_through = true` mode mapping still wins via first-match-wins and does
//! NOT fall through to other mode mappings or to global mappings — the matcher
//! returns exactly that one winner, with `let_through`/`mapping_id` copied from
//! the winning `CompiledRule`.
//!
//! Spec: docs/let-through/ADR-038-implementation-spec.md §4.2, §5 Slice 2.

use conductor_core::actions::Action;
use conductor_core::events::{ProcessedEvent, VelocityLevel};
use conductor_core::{ActionConfig, Config, Mapping, Mode, Trigger};

fn note_on(note: u8) -> ProcessedEvent {
    ProcessedEvent::PadPressed {
        note,
        velocity: 64,
        velocity_level: VelocityLevel::Medium,
        channel: Some(0),
    }
}

fn mapping(note: u8, text: &str, let_through: bool) -> Mapping {
    Mapping {
        trigger: Trigger::Note {
            note,
            velocity_min: None,
            channel: None,
            device: None,
        },
        action: ActionConfig::Text {
            text: text.to_string(),
        },
        description: Some(text.to_string()),
        let_through,
    }
}

fn config_with(modes: Vec<Mode>, global: Vec<Mapping>) -> Config {
    Config {
        mcp: Default::default(),
        config_meta: Default::default(),
        security: Default::default(),
        endpoints: vec![],
        modes,
        global_mappings: global,
        logging: None,
        advanced_settings: Default::default(),
        last_selected_mode: None,
        per_app_modes: None,
        default_mode: None,
        led: None,
        event_console: None,
        routes: vec![],
    }
}

/// A `let_through = true` mode mapping wins and the envelope carries the flag;
/// it does NOT fall through to a global mapping on the same note.
#[test]
fn let_through_winner_does_not_fall_through_to_global() {
    let config = config_with(
        vec![Mode {
            name: "Default".to_string(),
            color: None,
            mappings: vec![mapping(60, "mode-winner", true)],
        }],
        // A global mapping on the same note that MUST NOT be reached.
        vec![mapping(60, "global-fallthrough", false)],
    );
    let rule_set = conductor_core::rule_compiler::compile(&config, 1);

    let envelope = rule_set
        .match_event_with_provenance(&note_on(60), 0, None)
        .expect("note 60 should match the mode mapping");

    match &envelope.action {
        Action::Text(t) => assert_eq!(t, "mode-winner", "must return the mode winner, not global"),
        other => panic!("expected Text action, got {other:?}"),
    }
    assert!(
        envelope.let_through,
        "the winning rule's let_through flag must be copied onto the envelope"
    );
    // The single mode mapping sits at authored index 0, so mapping_id must be
    // exactly Some(0) — asserting the value (not just is_some) catches off-by-one
    // or wrong-source-list regressions (Copilot review on PR #1861).
    assert_eq!(
        envelope.mapping_id,
        Some(0),
        "mapping_id must be the winning rule's authored positional index"
    );
}

/// A normal (default) mapping yields `let_through = false` on the envelope.
#[test]
fn normal_mapping_envelope_is_not_let_through() {
    let config = config_with(
        vec![Mode {
            name: "Default".to_string(),
            color: None,
            mappings: vec![mapping(62, "normal", false)],
        }],
        vec![],
    );
    let rule_set = conductor_core::rule_compiler::compile(&config, 1);

    let envelope = rule_set
        .match_event_with_provenance(&note_on(62), 0, None)
        .expect("note 62 should match");

    assert!(
        !envelope.let_through,
        "a default mapping must not be marked let_through"
    );
}

/// `let_through` is metadata only: it does not cause the event to additionally
/// match a *second* mode mapping. The matcher still returns exactly one winner
/// (first-match-wins preserved).
#[test]
fn let_through_does_not_match_a_second_mode_mapping() {
    let config = config_with(
        vec![Mode {
            name: "Default".to_string(),
            color: None,
            mappings: vec![mapping(64, "first", true), mapping(64, "second", false)],
        }],
        vec![],
    );
    let rule_set = conductor_core::rule_compiler::compile(&config, 1);

    let envelope = rule_set
        .match_event_with_provenance(&note_on(64), 0, None)
        .expect("note 64 should match the first mapping");

    match &envelope.action {
        Action::Text(t) => assert_eq!(
            t, "first",
            "first-match-wins: the let_through winner is returned, not the second mapping"
        ),
        other => panic!("expected Text action, got {other:?}"),
    }
    assert!(envelope.let_through);
}
