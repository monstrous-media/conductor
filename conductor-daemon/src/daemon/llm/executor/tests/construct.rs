// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use super::*;

// =========================================================================
// Constructor Tests
// =========================================================================

#[tokio::test]
async fn test_new_with_live_config_uses_passed_config() {
    // D4.A.3.3.B.1: renamed from `test_new_with_config_uses_passed_config`
    // — the legacy `new_with_config(Arc<RwLock<Config>>)` constructor
    // retired alongside the `Arc<RwLock<Option<Config>>>` migration. The
    // test now verifies the standard `ToolExecutor::new(Arc<LiveConfig>)`
    // path preserves config identity, which is the only remaining shape.
    use crate::daemon::mcp_types::ToolContent;

    let mut config = create_test_config();
    // ADR-035 removed `[device]`; mutate a serializing field (the mode
    // name) instead to prove the passed config's identity is preserved.
    config.modes[0].name = "LLM Council Test Mode".to_string();

    let executor = ToolExecutor::new(live_config_arc(config));

    let result = executor.execute("conductor_get_config", None, None).await;

    match result {
        ExecutionResult::Success { result } => {
            let content = result.content.first().expect("Should have content");
            let text = match content {
                ToolContent::Text { text } => text,
                _ => panic!("Expected text content"),
            };
            assert!(
                text.contains("LLM Council Test Mode"),
                "Config should contain our test mode name, got: {}",
                text
            );
        }
        ExecutionResult::Error { message } => {
            panic!("Expected Success result, got error: {}", message);
        }
        _ => panic!("Expected Success result for ReadOnly tool"),
    }
}

#[tokio::test]
async fn test_get_config_returns_modified_after_apply() {
    let config = create_test_config();
    let config_arc = live_config_arc(config);
    let executor = ToolExecutor::new(config_arc.clone());

    // Verify initial state — D4.A.3.3.B.1: get_config() now returns
    // Config directly (no Option), since LiveConfig is always loaded.
    let initial = executor.get_config();
    assert_eq!(initial.modes[0].mappings.len(), 1);

    // Create and apply a plan
    let args = json!({
        "mode": "Default",
        "trigger": { "type": "Note", "note": 37, "velocity_min": 1 },
        "action": { "type": "Keystroke", "keys": "v", "modifiers": ["cmd"] },
        "description": "Paste"
    });

    let result = executor
        .execute("conductor_create_mapping", Some(args), None)
        .await;
    let plan_id = match result {
        ExecutionResult::PlanCreated { plan } => plan.id,
        _ => panic!("Expected PlanCreated"),
    };

    executor.apply_plan(&plan_id).await.unwrap();

    // get_config should return the modified config with 2 mappings
    let modified = executor.get_config();
    assert_eq!(modified.modes[0].mappings.len(), 2);
    assert_eq!(
        modified.modes[0].mappings[1].description,
        Some("Paste".to_string())
    );
}

#[tokio::test]
async fn test_analyze_midi_learn_note_on_returns_note_trigger() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let events = vec![MidiLearnEvent {
        event_type: EventType::NoteOn,
        note: Some(36),
        velocity: Some(100),
        ..Default::default()
    }];

    let result = executor.analyze_midi_learn_events(&events);
    assert!(result.is_some());
    let trigger = result.unwrap();
    assert_eq!(trigger["type"], "Note");
    assert_eq!(trigger["note"], 36);
}

#[tokio::test]
async fn test_analyze_midi_learn_chord_returns_chord_trigger() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let events = vec![MidiLearnEvent {
        event_type: EventType::NoteOn,
        pattern_type: Some(PatternType::Chord),
        pattern_notes: Some(vec![36, 40, 44]),
        pattern_timeout_ms: Some(100),
        ..Default::default()
    }];

    let result = executor.analyze_midi_learn_events(&events);
    assert!(result.is_some());
    let trigger = result.unwrap();
    assert_eq!(trigger["type"], "Chord");
    assert_eq!(trigger["notes"], json!([36, 40, 44]));
}

#[tokio::test]
async fn test_analyze_midi_learn_program_change_returns_pc_trigger() {
    // ADR-025 Phase 1: a single PC capture must suggest a
    // ProgramChange trigger so foot-controller bank stomps can be
    // mapped via Learn.
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let events = vec![MidiLearnEvent {
        event_type: EventType::ProgramChange,
        pc: Some(12),
        channel: 0,
        ..Default::default()
    }];

    let result = executor
        .analyze_midi_learn_events(&events)
        .expect("PC event should yield a trigger suggestion");
    assert_eq!(result["type"], "ProgramChange");
    assert_eq!(result["pc"], 12);
    assert_eq!(result["channel"], 0);
}

#[tokio::test]
async fn test_analyze_midi_learn_program_change_carries_channel() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let events = vec![MidiLearnEvent {
        event_type: EventType::ProgramChange,
        pc: Some(42),
        channel: 5,
        ..Default::default()
    }];

    let result = executor.analyze_midi_learn_events(&events).unwrap();
    assert_eq!(result["channel"], 5);
}

#[tokio::test]
async fn test_analyze_midi_learn_cc_returns_cc_trigger() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let events = vec![MidiLearnEvent {
        event_type: EventType::Cc,
        cc: Some(1),
        value: Some(64),
        ..Default::default()
    }];

    let result = executor.analyze_midi_learn_events(&events);
    assert!(result.is_some());
    let trigger = result.unwrap();
    assert_eq!(trigger["type"], "CC");
    assert_eq!(trigger["cc"], 1);
}

#[tokio::test]
async fn test_analyze_midi_learn_encoder_returns_encoder_trigger() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let events = vec![MidiLearnEvent {
        event_type: EventType::Encoder,
        cc: Some(16),
        ..Default::default()
    }];

    let result = executor.analyze_midi_learn_events(&events);
    assert!(result.is_some());
    let trigger = result.unwrap();
    assert_eq!(trigger["type"], "EncoderTurn");
    assert_eq!(trigger["cc"], 16);
}

#[tokio::test]
async fn test_analyze_midi_learn_gamepad_chord_uses_pattern_buttons() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let events = vec![MidiLearnEvent {
        event_type: EventType::NoteOn,
        pattern_type: Some(PatternType::GamepadChord),
        pattern_buttons: Some(vec![128, 129, 130]),
        pattern_timeout_ms: Some(100),
        ..Default::default()
    }];

    let result = executor.analyze_midi_learn_events(&events);
    assert!(result.is_some());
    let trigger = result.unwrap();
    assert_eq!(trigger["type"], "GamepadButtonChord");
    assert_eq!(trigger["buttons"], json!([128, 129, 130]));
}

#[tokio::test]
async fn test_analyze_midi_learn_velocity_range_suggestion() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    // 3+ note presses with velocity range > 30 → should suggest VelocityRange
    let events = vec![
        MidiLearnEvent {
            event_type: EventType::NoteOn,
            note: Some(36),
            velocity: Some(30),
            ..Default::default()
        },
        MidiLearnEvent {
            event_type: EventType::NoteOn,
            note: Some(36),
            velocity: Some(80),
            ..Default::default()
        },
        MidiLearnEvent {
            event_type: EventType::NoteOn,
            note: Some(36),
            velocity: Some(127),
            ..Default::default()
        },
    ];

    let result = executor.analyze_midi_learn_events(&events);
    assert!(result.is_some());
    let trigger = result.unwrap();
    assert_eq!(trigger["type"], "VelocityRange");
    assert_eq!(trigger["note"], 36);

    // The suggestion must use the CANONICAL `soft_max` / `medium_max`
    // fields, not a `ranges` object — otherwise applying it silently drops
    // the thresholds. velocities 30/80/127 → min 30, max 127, range 97 →
    // soft_max = 30 + 97/3 = 62, medium_max = 30 + 2*97/3 = 94.
    assert!(
        trigger.get("ranges").is_none(),
        "must not emit the incompatible `ranges` shape; got {trigger}"
    );
    assert_eq!(trigger["soft_max"], 62);
    assert_eq!(trigger["medium_max"], 94);

    // Round-trip: the suggestion must deserialize into Trigger::VelocityRange
    // and preserve the learned thresholds — the real cross-helper contract.
    let parsed: conductor_core::config::types::Trigger = serde_json::from_value(trigger.clone())
        .expect("suggestion must deserialize into Trigger::VelocityRange");
    match parsed {
        conductor_core::config::types::Trigger::VelocityRange {
            note,
            soft_max,
            medium_max,
            ..
        } => {
            assert_eq!(note, 36);
            assert_eq!(soft_max, Some(62));
            assert_eq!(medium_max, Some(94));
        }
        other => panic!("expected Trigger::VelocityRange, got {other:?}"),
    }
}

#[tokio::test]
async fn test_analyze_midi_learn_uniform_velocity_returns_note() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    // 3 presses with similar velocity (range < 30) → plain Note
    let events = vec![
        MidiLearnEvent {
            event_type: EventType::NoteOn,
            note: Some(36),
            velocity: Some(90),
            ..Default::default()
        },
        MidiLearnEvent {
            event_type: EventType::NoteOn,
            note: Some(36),
            velocity: Some(100),
            ..Default::default()
        },
        MidiLearnEvent {
            event_type: EventType::NoteOn,
            note: Some(36),
            velocity: Some(95),
            ..Default::default()
        },
    ];

    let result = executor.analyze_midi_learn_events(&events);
    assert!(result.is_some());
    let trigger = result.unwrap();
    assert_eq!(trigger["type"], "Note");
}
