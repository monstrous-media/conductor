// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! MIDI Channel Pipeline Tests (Issues #434, #437 — Phases 1 & 4)
//!
//! Verifies that MIDI channel information is preserved and propagated through
//! the entire event pipeline: MidiEvent → InputEvent → ProcessedEvent → Trigger matching.
//!
//! Key invariants:
//! - Channel is 0-indexed internally (0-15). **Config TOML `channel` values are
//!   also 0-indexed** and pass through unchanged: `channel = 5` is the internal
//!   `Some(5)` (MIDI channel 6 to a human). Config is NOT a display surface —
//!   the 1-indexed convention (1-16) is applied only at the GUI display layer,
//!   never in config serde. So the round-trip tests below intentionally assert
//!   `channel = 5` ↔ `Some(5)` with no off-by-one shift, and config validation
//!   rejects `channel > 15` (see `config_trigger_channel_above_15_is_rejected`).
//! - `channel: None` on triggers means "match any channel" (backward compatible)
//! - Gamepad events have no channel (always None)

use conductor_core::event_processor::{EventProcessor, MidiEvent, ProcessedEvent};
use conductor_core::events::InputEvent;
use std::time::{Duration, Instant};

// ────────────────────────────────────────────────────────────────
// 1. MidiEvent — channel field preserved from raw MIDI bytes
// ────────────────────────────────────────────────────────────────

#[test]
fn midi_event_from_midi_msg_preserves_channel() {
    // Note On on channel 0 (status 0x90): note 60, velocity 100
    let event = MidiEvent::from_midi_msg(&[0x90, 60, 100]).unwrap();
    match event {
        MidiEvent::NoteOn {
            note,
            velocity,
            channel,
            ..
        } => {
            assert_eq!(note, 60);
            assert_eq!(velocity, 100);
            assert_eq!(channel, 0);
        }
        _ => panic!("Expected NoteOn, got {:?}", event),
    }
}

#[test]
fn midi_event_channel_ranges_0_to_15() {
    for ch in 0u8..16 {
        let status = 0x90 | ch; // Note On on channel ch
        let event = MidiEvent::from_midi_msg(&[status, 60, 100]).unwrap();
        match event {
            MidiEvent::NoteOn { channel, .. } => {
                assert_eq!(
                    channel, ch,
                    "Channel should be {} for status byte 0x{:02X}",
                    ch, status
                );
            }
            _ => panic!("Expected NoteOn for channel {}", ch),
        }
    }
}

#[test]
fn midi_event_note_off_preserves_channel() {
    // Note Off on channel 5 (status 0x85): note 36
    let event = MidiEvent::from_midi_msg(&[0x85, 36, 0]).unwrap();
    match event {
        MidiEvent::NoteOff { note, channel, .. } => {
            assert_eq!(note, 36);
            assert_eq!(channel, 5);
        }
        _ => panic!("Expected NoteOff, got {:?}", event),
    }
}

#[test]
fn midi_event_note_on_velocity_zero_is_note_off_with_channel() {
    // Note On with velocity 0 on channel 3 → treated as Note Off
    let event = MidiEvent::from_midi_msg(&[0x93, 60, 0]).unwrap();
    match event {
        MidiEvent::NoteOff { note, channel, .. } => {
            assert_eq!(note, 60);
            assert_eq!(channel, 3);
        }
        _ => panic!("Expected NoteOff (vel=0), got {:?}", event),
    }
}

#[test]
fn midi_event_cc_preserves_channel() {
    // CC on channel 10 (status 0xBA): CC 7, value 127
    let event = MidiEvent::from_midi_msg(&[0xBA, 7, 127]).unwrap();
    match event {
        MidiEvent::ControlChange {
            cc, value, channel, ..
        } => {
            assert_eq!(cc, 7);
            assert_eq!(value, 127);
            assert_eq!(channel, 10);
        }
        _ => panic!("Expected ControlChange, got {:?}", event),
    }
}

#[test]
fn midi_event_aftertouch_preserves_channel() {
    // Channel Pressure on channel 2 (status 0xD2): pressure 80
    let event = MidiEvent::from_midi_msg(&[0xD2, 80]).unwrap();
    match event {
        MidiEvent::Aftertouch {
            pressure, channel, ..
        } => {
            assert_eq!(pressure, 80);
            assert_eq!(channel, 2);
        }
        _ => panic!("Expected Aftertouch, got {:?}", event),
    }
}

#[test]
fn midi_event_pitch_bend_preserves_channel() {
    // Pitch Bend on channel 7 (status 0xE7): LSB 0, MSB 64 = center (8192)
    let event = MidiEvent::from_midi_msg(&[0xE7, 0, 64]).unwrap();
    match event {
        MidiEvent::PitchBend { channel, .. } => {
            assert_eq!(channel, 7);
        }
        _ => panic!("Expected PitchBend, got {:?}", event),
    }
}

#[test]
fn midi_event_program_change_preserves_channel() {
    // Program Change on channel 15 (status 0xCF): program 42
    let event = MidiEvent::from_midi_msg(&[0xCF, 42]).unwrap();
    match event {
        MidiEvent::ProgramChange {
            program, channel, ..
        } => {
            assert_eq!(program, 42);
            assert_eq!(channel, 15);
        }
        _ => panic!("Expected ProgramChange, got {:?}", event),
    }
}

#[test]
fn midi_event_poly_pressure_preserves_channel() {
    // Poly Pressure on channel 4 (status 0xA4): note 60, pressure 90
    let event = MidiEvent::from_midi_msg(&[0xA4, 60, 90]).unwrap();
    match event {
        MidiEvent::PolyPressure {
            note,
            pressure,
            channel,
            ..
        } => {
            assert_eq!(note, 60);
            assert_eq!(pressure, 90);
            assert_eq!(channel, 4);
        }
        _ => panic!("Expected PolyPressure, got {:?}", event),
    }
}

// ────────────────────────────────────────────────────────────────
// 2. MidiEvent → InputEvent conversion preserves channel
// ────────────────────────────────────────────────────────────────

#[test]
fn input_event_from_midi_preserves_channel() {
    let time = Instant::now();
    let midi = MidiEvent::NoteOn {
        note: 36,
        velocity: 100,
        channel: 5,
        time,
    };
    let input: InputEvent = midi.into();
    match input {
        InputEvent::PadPressed {
            pad,
            velocity,
            channel,
            ..
        } => {
            assert_eq!(pad, 36);
            assert_eq!(velocity, 100);
            assert_eq!(channel, Some(5));
        }
        _ => panic!("Expected PadPressed"),
    }
}

#[test]
fn input_event_pad_released_preserves_channel() {
    let time = Instant::now();
    let midi = MidiEvent::NoteOff {
        note: 60,
        channel: 12,
        time,
    };
    let input: InputEvent = midi.into();
    match input {
        InputEvent::PadReleased { pad, channel, .. } => {
            assert_eq!(pad, 60);
            assert_eq!(channel, Some(12));
        }
        _ => panic!("Expected PadReleased"),
    }
}

#[test]
fn input_event_cc_preserves_channel() {
    let time = Instant::now();
    let midi = MidiEvent::ControlChange {
        cc: 7,
        value: 64,
        channel: 3,
        time,
    };
    let input: InputEvent = midi.into();
    match input {
        InputEvent::ControlChange {
            control,
            value,
            channel,
            ..
        } => {
            assert_eq!(control, 7);
            assert_eq!(value, 64);
            assert_eq!(channel, Some(3));
        }
        _ => panic!("Expected ControlChange"),
    }
}

#[test]
fn input_event_aftertouch_preserves_channel() {
    let time = Instant::now();
    let midi = MidiEvent::Aftertouch {
        pressure: 80,
        channel: 9,
        time,
    };
    let input: InputEvent = midi.into();
    match input {
        InputEvent::Aftertouch {
            pressure, channel, ..
        } => {
            assert_eq!(pressure, 80);
            assert_eq!(channel, Some(9));
        }
        _ => panic!("Expected Aftertouch"),
    }
}

#[test]
fn input_event_pitch_bend_preserves_channel() {
    let time = Instant::now();
    let midi = MidiEvent::PitchBend {
        value: 8192,
        channel: 0,
        time,
    };
    let input: InputEvent = midi.into();
    match input {
        InputEvent::PitchBend { value, channel, .. } => {
            assert_eq!(value, 8192);
            assert_eq!(channel, Some(0));
        }
        _ => panic!("Expected PitchBend"),
    }
}

// ────────────────────────────────────────────────────────────────
// 3. ProcessedEvent — channel propagated through EventProcessor
// ────────────────────────────────────────────────────────────────

#[test]
fn processed_event_pad_pressed_has_channel() {
    let mut processor = EventProcessor::new();
    let event = MidiEvent::NoteOn {
        note: 36,
        velocity: 100,
        channel: 5,
        time: Instant::now(),
    };
    let results = processor.process(event);
    // Should have PadPressed in results
    let pad_pressed = results
        .iter()
        .find(|e| matches!(e, ProcessedEvent::PadPressed { .. }));
    assert!(pad_pressed.is_some(), "Expected PadPressed in results");
    match pad_pressed.unwrap() {
        ProcessedEvent::PadPressed { note, channel, .. } => {
            assert_eq!(*note, 36);
            assert_eq!(*channel, Some(5));
        }
        _ => unreachable!(),
    }
}

#[test]
fn processed_event_cc_received_has_channel() {
    let mut processor = EventProcessor::new();
    let event = MidiEvent::ControlChange {
        cc: 7,
        value: 64,
        channel: 10,
        time: Instant::now(),
    };
    let results = processor.process(event);
    let cc = results
        .iter()
        .find(|e| matches!(e, ProcessedEvent::CCReceived { .. }));
    assert!(cc.is_some(), "Expected CCReceived in results");
    match cc.unwrap() {
        ProcessedEvent::CCReceived { cc, value, channel } => {
            assert_eq!(*cc, 7);
            assert_eq!(*value, 64);
            assert_eq!(*channel, Some(10));
        }
        _ => unreachable!(),
    }
}

#[test]
fn processed_event_chord_has_channel() {
    let mut processor = EventProcessor::new();
    let now = Instant::now();
    // Two notes on channel 5 within chord timeout
    processor.process(MidiEvent::NoteOn {
        note: 36,
        velocity: 100,
        channel: 5,
        time: now,
    });
    let results = processor.process(MidiEvent::NoteOn {
        note: 40,
        velocity: 80,
        channel: 5,
        time: now,
    });
    let chord = results
        .iter()
        .find(|e| matches!(e, ProcessedEvent::ChordDetected { .. }));
    assert!(chord.is_some(), "Expected ChordDetected in results");
    match chord.unwrap() {
        ProcessedEvent::ChordDetected { channel, .. } => {
            // Channel from the most recent note in the chord
            assert_eq!(*channel, Some(5));
        }
        _ => unreachable!(),
    }
}

#[test]
fn processed_event_double_tap_has_channel() {
    let mut processor = EventProcessor::new();
    let now = Instant::now();
    let later = now + std::time::Duration::from_millis(100);
    // First tap
    processor.process(MidiEvent::NoteOn {
        note: 36,
        velocity: 100,
        channel: 3,
        time: now,
    });
    processor.process(MidiEvent::NoteOff {
        note: 36,
        channel: 3,
        time: now + std::time::Duration::from_millis(50),
    });
    // Second tap
    let results = processor.process(MidiEvent::NoteOn {
        note: 36,
        velocity: 80,
        channel: 3,
        time: later,
    });
    let dt = results
        .iter()
        .find(|e| matches!(e, ProcessedEvent::DoubleTap { .. }));
    assert!(dt.is_some(), "Expected DoubleTap in results");
    match dt.unwrap() {
        ProcessedEvent::DoubleTap { channel, .. } => {
            assert_eq!(*channel, Some(3));
        }
        _ => unreachable!(),
    }
}

#[test]
fn processed_event_aftertouch_has_channel() {
    let mut processor = EventProcessor::new();
    let event = MidiEvent::Aftertouch {
        pressure: 80,
        channel: 9,
        time: Instant::now(),
    };
    let results = processor.process(event);
    let at = results
        .iter()
        .find(|e| matches!(e, ProcessedEvent::AftertouchChanged { .. }));
    assert!(at.is_some());
    match at.unwrap() {
        ProcessedEvent::AftertouchChanged { pressure, channel } => {
            assert_eq!(*pressure, 80);
            assert_eq!(*channel, Some(9));
        }
        _ => unreachable!(),
    }
}

#[test]
fn processed_event_pitch_bend_has_channel() {
    let mut processor = EventProcessor::new();
    let event = MidiEvent::PitchBend {
        value: 8192,
        channel: 7,
        time: Instant::now(),
    };
    let results = processor.process(event);
    let pb = results
        .iter()
        .find(|e| matches!(e, ProcessedEvent::PitchBendMoved { .. }));
    assert!(pb.is_some());
    match pb.unwrap() {
        ProcessedEvent::PitchBendMoved { value, channel } => {
            assert_eq!(*value, 8192);
            assert_eq!(*channel, Some(7));
        }
        _ => unreachable!(),
    }
}

// ────────────────────────────────────────────────────────────────
// 4. Trigger matching — channel filter on triggers
// ────────────────────────────────────────────────────────────────

#[test]
fn trigger_channel_none_matches_any_channel() {
    // A trigger with channel=None should match events on any channel
    use conductor_core::config::Config;
    use conductor_core::config::{ActionConfig, Mapping, Trigger};
    use conductor_core::mapping::MappingEngine;

    let config = Config {
        mcp: Default::default(),
        config_meta: Default::default(),
        security: Default::default(),
        endpoints: vec![],
        modes: vec![conductor_core::config::Mode {
            name: "Test".to_string(),
            color: None,
            mappings: vec![Mapping {
                trigger: Trigger::Note {
                    note: 60,
                    velocity_min: None,
                    channel: None,
                    device: None,
                },
                action: ActionConfig::Keystroke {
                    keys: "a".into(),
                    modifiers: vec![],
                },
                description: None,
                let_through: false,
            }],
        }],
        global_mappings: vec![],
        logging: None,
        advanced_settings: Default::default(),
        last_selected_mode: None,
        per_app_modes: None,
        default_mode: None,
        led: None,
        event_console: None,
        routes: vec![],
    };

    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    // Should match events on channel 0, 5, 15
    for ch in [0u8, 5, 15] {
        let mut processor = EventProcessor::new();
        let results = processor.process(MidiEvent::NoteOn {
            note: 60,
            velocity: 100,
            channel: ch,
            time: Instant::now(),
        });
        let pad = results
            .iter()
            .find(|e| matches!(e, ProcessedEvent::PadPressed { .. }))
            .unwrap();
        let action = engine.get_action_for_processed(pad, 0);
        assert!(
            action.is_some(),
            "Trigger with channel=None should match channel {}",
            ch
        );
    }
}

#[test]
fn trigger_channel_some_matches_only_that_channel() {
    use conductor_core::config::Config;
    use conductor_core::config::{ActionConfig, Mapping, Trigger};
    use conductor_core::mapping::MappingEngine;

    let config = Config {
        mcp: Default::default(),
        config_meta: Default::default(),
        security: Default::default(),
        endpoints: vec![],
        modes: vec![conductor_core::config::Mode {
            name: "Test".to_string(),
            color: None,
            mappings: vec![Mapping {
                trigger: Trigger::Note {
                    note: 60,
                    velocity_min: None,
                    channel: Some(5),
                    device: None,
                },
                action: ActionConfig::Keystroke {
                    keys: "a".into(),
                    modifiers: vec![],
                },
                description: None,
                let_through: false,
            }],
        }],
        global_mappings: vec![],
        logging: None,
        advanced_settings: Default::default(),
        last_selected_mode: None,
        per_app_modes: None,
        default_mode: None,
        led: None,
        event_console: None,
        routes: vec![],
    };

    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    // Channel 5 should match
    let mut processor = EventProcessor::new();
    let results = processor.process(MidiEvent::NoteOn {
        note: 60,
        velocity: 100,
        channel: 5,
        time: Instant::now(),
    });
    let pad = results
        .iter()
        .find(|e| matches!(e, ProcessedEvent::PadPressed { .. }))
        .unwrap();
    assert!(
        engine.get_action_for_processed(pad, 0).is_some(),
        "Should match channel 5"
    );

    // Channel 0 should NOT match
    let mut processor2 = EventProcessor::new();
    let results2 = processor2.process(MidiEvent::NoteOn {
        note: 60,
        velocity: 100,
        channel: 0,
        time: Instant::now(),
    });
    let pad2 = results2
        .iter()
        .find(|e| matches!(e, ProcessedEvent::PadPressed { .. }))
        .unwrap();
    assert!(
        engine.get_action_for_processed(pad2, 0).is_none(),
        "Should NOT match channel 0"
    );
}

// ────────────────────────────────────────────────────────────────
// 5. Backward compatibility — configs without channel field
// ────────────────────────────────────────────────────────────────

#[test]
fn config_without_channel_deserializes_as_none() {
    let toml_str = r#"
[[modes]]
name = "Test"
[[modes.mappings]]
trigger = { type = "Note", note = 60 }
action = { type = "Keystroke", keys = "a" }
"#;
    let config: conductor_core::config::Config = toml::from_str(toml_str).unwrap();
    match &config.modes[0].mappings[0].trigger {
        conductor_core::config::Trigger::Note { channel, .. } => {
            assert_eq!(
                *channel, None,
                "Missing channel field should deserialize as None"
            );
        }
        _ => panic!("Expected Note trigger"),
    }
}

#[test]
fn config_with_channel_deserializes_correctly() {
    // Config TOML channels are 0-indexed (see module docs): `channel = 5`
    // deserializes to the internal `Some(5)` with NO off-by-one shift. The
    // 1-indexed (1-16) convention is a GUI-display concern only, so this must
    // NOT be "fixed" to map `channel = 6` -> Some(5).
    let toml_str = r#"
[[modes]]
name = "Test"
[[modes.mappings]]
trigger = { type = "Note", note = 60, channel = 5 }
action = { type = "Keystroke", keys = "a" }
"#;
    let config: conductor_core::config::Config = toml::from_str(toml_str).unwrap();
    match &config.modes[0].mappings[0].trigger {
        conductor_core::config::Trigger::Note { channel, .. } => {
            assert_eq!(*channel, Some(5));
        }
        _ => panic!("Expected Note trigger"),
    }
}

#[test]
fn config_channel_serialization_skips_none() {
    let trigger = conductor_core::config::Trigger::Note {
        note: 60,
        velocity_min: None,
        channel: None,
        device: None,
    };
    let serialized = toml::to_string(&trigger).unwrap();
    assert!(
        !serialized.contains("channel"),
        "None channel should be skipped in serialization"
    );
}

#[test]
fn config_channel_serialization_includes_some() {
    let trigger = conductor_core::config::Trigger::Note {
        note: 60,
        velocity_min: None,
        channel: Some(5),
        device: None,
    };
    let serialized = toml::to_string(&trigger).unwrap();
    // 0-indexed config contract: internal Some(5) serializes back as
    // `channel = 5` (no +1 display shift). See module docs.
    assert!(
        serialized.contains("channel = 5"),
        "Some(5) channel should be serialized"
    );
}

/// #1514: config TOML channels are 0-indexed (valid 0-15), so config
/// validation must reject `channel > 15`. This pins the 0-indexed contract
/// from the side of the range bound — together with the round-trip tests above
/// (which pin pass-through with no off-by-one) it makes the contract explicit,
/// resolving the apparent contradiction with the 1-indexed *display* convention.
#[test]
fn config_trigger_channel_above_15_is_rejected() {
    use conductor_core::config::Trigger;

    // 16 is out of the 0-indexed 0-15 range → validation error.
    let bad = config_with_trigger(Trigger::Note {
        note: 60,
        velocity_min: None,
        channel: Some(16),
        device: None,
    });
    assert!(
        bad.validate().is_err(),
        "channel 16 must be rejected (valid range is 0-15)"
    );

    // Both ends of the valid 0-indexed range are accepted.
    for ch in [0u8, 15u8] {
        let ok = config_with_trigger(Trigger::Note {
            note: 60,
            velocity_min: None,
            channel: Some(ch),
            device: None,
        });
        assert!(
            ok.validate().is_ok(),
            "channel {ch} must be valid (0-15 inclusive)"
        );
    }
}

// ────────────────────────────────────────────────────────────────
// 6. CC trigger with channel filter
// ────────────────────────────────────────────────────────────────

#[test]
fn cc_trigger_with_channel_matches_correct_channel() {
    use conductor_core::config::Config;
    use conductor_core::config::{ActionConfig, Mapping, Trigger};
    use conductor_core::mapping::MappingEngine;

    let config = Config {
        mcp: Default::default(),
        config_meta: Default::default(),
        security: Default::default(),
        endpoints: vec![],
        modes: vec![conductor_core::config::Mode {
            name: "Test".to_string(),
            color: None,
            mappings: vec![Mapping {
                trigger: Trigger::CC {
                    cc: 7,
                    value_min: None,
                    channel: Some(10),
                    device: None,
                },
                action: ActionConfig::Keystroke {
                    keys: "a".into(),
                    modifiers: vec![],
                },
                description: None,
                let_through: false,
            }],
        }],
        global_mappings: vec![],
        logging: None,
        advanced_settings: Default::default(),
        last_selected_mode: None,
        per_app_modes: None,
        default_mode: None,
        led: None,
        event_console: None,
        routes: vec![],
    };

    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    // Channel 10 should match
    let mut processor = EventProcessor::new();
    let results = processor.process(MidiEvent::ControlChange {
        cc: 7,
        value: 127,
        channel: 10,
        time: Instant::now(),
    });
    let cc_event = results
        .iter()
        .find(|e| matches!(e, ProcessedEvent::CCReceived { .. }))
        .unwrap();
    assert!(
        engine.get_action_for_processed(cc_event, 0).is_some(),
        "Should match channel 10"
    );

    // Channel 0 should NOT match
    let mut processor2 = EventProcessor::new();
    let results2 = processor2.process(MidiEvent::ControlChange {
        cc: 7,
        value: 127,
        channel: 0,
        time: Instant::now(),
    });
    let cc_event2 = results2
        .iter()
        .find(|e| matches!(e, ProcessedEvent::CCReceived { .. }))
        .unwrap();
    assert!(
        engine.get_action_for_processed(cc_event2, 0).is_none(),
        "Should NOT match channel 0"
    );
}

// ────────────────────────────────────────────────────────────────
// 7. Gamepad events — channel is always None
// ────────────────────────────────────────────────────────────────

#[test]
fn gamepad_input_events_have_no_channel() {
    // Gamepad events should always have channel: None since they are not MIDI
    let time = Instant::now();

    let pad_pressed = InputEvent::PadPressed {
        pad: 128, // HID range
        velocity: 100,
        channel: None,
        time,
    };
    let pad_released = InputEvent::PadReleased {
        pad: 128,
        channel: None,
        time,
    };
    let encoder = InputEvent::EncoderTurned {
        encoder: 130,
        value: 64,
        channel: None,
        analog: None,
        time,
    };

    // Process through EventProcessor — channel should remain None in ProcessedEvent
    let mut processor = EventProcessor::new();

    let results = processor.process_input(pad_pressed);
    for event in &results {
        match event {
            ProcessedEvent::PadPressed { channel, .. } => assert_eq!(
                *channel, None,
                "Gamepad PadPressed should have channel None"
            ),
            ProcessedEvent::ShortPress { channel, .. }
            | ProcessedEvent::MediumPress { channel, .. }
            | ProcessedEvent::LongPress { channel, .. } => {
                assert_eq!(
                    *channel, None,
                    "Gamepad velocity event should have channel None"
                );
            }
            _ => {}
        }
    }

    let results = processor.process_input(pad_released);
    for event in &results {
        if let ProcessedEvent::PadReleased { channel, .. } = event {
            assert_eq!(
                *channel, None,
                "Gamepad PadReleased should have channel None"
            );
        }
    }

    // EncoderTurned needs two events to detect direction (first establishes baseline)
    processor.process_input(encoder);
    let encoder2 = InputEvent::EncoderTurned {
        encoder: 130,
        value: 80,
        channel: None,
        analog: None,
        time,
    };
    let results = processor.process_input(encoder2);
    for event in &results {
        if let ProcessedEvent::EncoderTurned { channel, .. } = event {
            assert_eq!(
                *channel, None,
                "Gamepad EncoderTurned should have channel None"
            );
        }
    }
}

// ────────────────────────────────────────────────────────────────
// 8. End-to-end: raw MIDI bytes → trigger matching (#437 Phase 4)
//    Tests use MappingEngine for convenience; Section 9 below
//    verifies the daemon's actual hot path via CompiledRuleSet.
// ────────────────────────────────────────────────────────────────

/// Mirrors the daemon's MIDI callback path and verifies channel propagation
/// through core components up to trigger matching via MappingEngine.
/// Exercises: raw MIDI bytes → MidiEvent → InputEvent → EventProcessor → MappingEngine.
#[test]
fn e2e_raw_bytes_to_trigger_match_with_channel() {
    use conductor_core::config::Config;
    use conductor_core::config::{ActionConfig, Mapping, Trigger};
    use conductor_core::mapping::MappingEngine;

    // Config: Note 60 on channel 5 only
    let config = Config {
        mcp: Default::default(),
        config_meta: Default::default(),
        security: Default::default(),
        endpoints: vec![],
        modes: vec![conductor_core::config::Mode {
            name: "Test".to_string(),
            color: None,
            mappings: vec![Mapping {
                trigger: Trigger::Note {
                    note: 60,
                    velocity_min: None,
                    channel: Some(5),
                    device: None,
                },
                action: ActionConfig::Keystroke {
                    keys: "a".into(),
                    modifiers: vec![],
                },
                description: None,
                let_through: false,
            }],
        }],
        global_mappings: vec![],
        logging: None,
        advanced_settings: Default::default(),
        last_selected_mode: None,
        per_app_modes: None,
        default_mode: None,
        led: None,
        event_console: None,
        routes: vec![],
    };

    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    // Raw MIDI bytes: Note On, channel 5 (status 0x95), note 60, velocity 100
    let midi_event = MidiEvent::from_midi_msg(&[0x95, 60, 100]).unwrap();
    // Daemon-style conversion: MidiEvent → InputEvent via Into, mirroring the daemon's input manager
    let input_event: InputEvent = midi_event.into();
    // Per-device EventProcessor (same as daemon's per-device DashMap)
    let mut processor = EventProcessor::new();
    let processed = processor.process_input(input_event);

    let pad = processed
        .iter()
        .find(|e| matches!(e, ProcessedEvent::PadPressed { .. }))
        .expect("Should produce PadPressed");
    assert!(
        engine.get_action_for_processed(pad, 0).is_some(),
        "Raw bytes on channel 5 should match trigger with channel=Some(5)"
    );
}

/// Same pipeline but with wrong channel — trigger must NOT fire.
#[test]
fn e2e_raw_bytes_wrong_channel_does_not_fire() {
    use conductor_core::config::Config;
    use conductor_core::config::{ActionConfig, Mapping, Trigger};
    use conductor_core::mapping::MappingEngine;

    let config = Config {
        mcp: Default::default(),
        config_meta: Default::default(),
        security: Default::default(),
        endpoints: vec![],
        modes: vec![conductor_core::config::Mode {
            name: "Test".to_string(),
            color: None,
            mappings: vec![Mapping {
                trigger: Trigger::Note {
                    note: 60,
                    velocity_min: None,
                    channel: Some(5),
                    device: None,
                },
                action: ActionConfig::Keystroke {
                    keys: "a".into(),
                    modifiers: vec![],
                },
                description: None,
                let_through: false,
            }],
        }],
        global_mappings: vec![],
        logging: None,
        advanced_settings: Default::default(),
        last_selected_mode: None,
        per_app_modes: None,
        default_mode: None,
        led: None,
        event_console: None,
        routes: vec![],
    };

    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    // Raw MIDI bytes: Note On, channel 0 (status 0x90), note 60, velocity 100
    let midi_event = MidiEvent::from_midi_msg(&[0x90, 60, 100]).unwrap();
    let input_event: InputEvent = midi_event.into();
    let mut processor = EventProcessor::new();
    let processed = processor.process_input(input_event);

    let pad = processed
        .iter()
        .find(|e| matches!(e, ProcessedEvent::PadPressed { .. }))
        .expect("Should produce PadPressed");
    assert!(
        engine.get_action_for_processed(pad, 0).is_none(),
        "Raw bytes on channel 0 should NOT match trigger with channel=Some(5)"
    );
}

/// End-to-end CC: raw bytes → channel-filtered CC trigger matching.
#[test]
fn e2e_raw_bytes_cc_channel_filter() {
    use conductor_core::config::Config;
    use conductor_core::config::{ActionConfig, Mapping, Trigger};
    use conductor_core::mapping::MappingEngine;

    let config = Config {
        mcp: Default::default(),
        config_meta: Default::default(),
        security: Default::default(),
        endpoints: vec![],
        modes: vec![conductor_core::config::Mode {
            name: "Test".to_string(),
            color: None,
            mappings: vec![Mapping {
                trigger: Trigger::CC {
                    cc: 1,
                    value_min: None,
                    channel: Some(9),
                    device: None,
                },
                action: ActionConfig::Keystroke {
                    keys: "b".into(),
                    modifiers: vec![],
                },
                description: None,
                let_through: false,
            }],
        }],
        global_mappings: vec![],
        logging: None,
        advanced_settings: Default::default(),
        last_selected_mode: None,
        per_app_modes: None,
        default_mode: None,
        led: None,
        event_console: None,
        routes: vec![],
    };

    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    // CC 1 on channel 9 (status 0xB9), value 64
    let midi_right = MidiEvent::from_midi_msg(&[0xB9, 1, 64]).unwrap();
    let input_right: InputEvent = midi_right.into();
    let mut proc1 = EventProcessor::new();
    let results_right = proc1.process_input(input_right);
    let cc_right = results_right
        .iter()
        .find(|e| matches!(e, ProcessedEvent::CCReceived { .. }))
        .expect("Should produce CCReceived");
    assert!(
        engine.get_action_for_processed(cc_right, 0).is_some(),
        "CC on channel 9 should match"
    );

    // CC 1 on channel 0 (status 0xB0), value 64 — should NOT match
    let midi_wrong = MidiEvent::from_midi_msg(&[0xB0, 1, 64]).unwrap();
    let input_wrong: InputEvent = midi_wrong.into();
    let mut proc2 = EventProcessor::new();
    let results_wrong = proc2.process_input(input_wrong);
    let cc_wrong = results_wrong
        .iter()
        .find(|e| matches!(e, ProcessedEvent::CCReceived { .. }))
        .expect("Should produce CCReceived");
    assert!(
        engine.get_action_for_processed(cc_wrong, 0).is_none(),
        "CC on channel 0 should NOT match trigger with channel=Some(9)"
    );
}

/// End-to-end: channel=None trigger matches raw bytes on any channel.
#[test]
fn e2e_raw_bytes_channel_none_matches_all() {
    use conductor_core::config::Config;
    use conductor_core::config::{ActionConfig, Mapping, Trigger};
    use conductor_core::mapping::MappingEngine;

    let config = Config {
        mcp: Default::default(),
        config_meta: Default::default(),
        security: Default::default(),
        endpoints: vec![],
        modes: vec![conductor_core::config::Mode {
            name: "Test".to_string(),
            color: None,
            mappings: vec![Mapping {
                trigger: Trigger::Note {
                    note: 36,
                    velocity_min: None,
                    channel: None, // Any channel
                    device: None,
                },
                action: ActionConfig::Keystroke {
                    keys: "c".into(),
                    modifiers: vec![],
                },
                description: None,
                let_through: false,
            }],
        }],
        global_mappings: vec![],
        logging: None,
        advanced_settings: Default::default(),
        last_selected_mode: None,
        per_app_modes: None,
        default_mode: None,
        led: None,
        event_console: None,
        routes: vec![],
    };

    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    // Test all 16 channels
    for ch in 0u8..16 {
        let status = 0x90 | ch;
        let midi = MidiEvent::from_midi_msg(&[status, 36, 80]).unwrap();
        let input: InputEvent = midi.into();
        let mut proc = EventProcessor::new();
        let results = proc.process_input(input);
        let pad = results
            .iter()
            .find(|e| matches!(e, ProcessedEvent::PadPressed { .. }))
            .expect("Should produce PadPressed");
        assert!(
            engine.get_action_for_processed(pad, 0).is_some(),
            "channel=None trigger should match raw bytes on channel {}",
            ch
        );
    }
}

// ────────────────────────────────────────────────────────────────
// 9. CompiledRuleSet — daemon hot path channel matching (#437)
//    The daemon uses CompiledRuleSet (via ArcSwap) for lock-free
//    event matching, not MappingEngine. This section verifies
//    channel filtering works through that code path.
// ────────────────────────────────────────────────────────────────

/// Verifies channel matching through the daemon's actual hot path:
/// rule_compiler::compile() → CompiledRuleSet::match_event().
#[test]
fn compiled_rule_set_channel_match_and_reject() {
    use conductor_core::config::Config;
    use conductor_core::config::{ActionConfig, Mapping, Trigger};
    use conductor_core::rule_compiler;

    let config = Config {
        mcp: Default::default(),
        config_meta: Default::default(),
        security: Default::default(),
        endpoints: vec![],
        modes: vec![conductor_core::config::Mode {
            name: "Test".to_string(),
            color: None,
            mappings: vec![Mapping {
                trigger: Trigger::Note {
                    note: 60,
                    velocity_min: None,
                    channel: Some(5),
                    device: None,
                },
                action: ActionConfig::Keystroke {
                    keys: "a".into(),
                    modifiers: vec![],
                },
                description: None,
                let_through: false,
            }],
        }],
        global_mappings: vec![],
        logging: None,
        advanced_settings: Default::default(),
        last_selected_mode: None,
        per_app_modes: None,
        default_mode: None,
        led: None,
        event_console: None,
        routes: vec![],
    };

    let rules = rule_compiler::compile(&config, 1);

    // Channel 5 should match
    let midi = MidiEvent::from_midi_msg(&[0x95, 60, 100]).unwrap();
    let input: InputEvent = midi.into();
    let mut proc = EventProcessor::new();
    let results = proc.process_input(input);
    let pad = results
        .iter()
        .find(|e| matches!(e, ProcessedEvent::PadPressed { .. }))
        .expect("Should produce PadPressed");
    assert!(
        rules.match_event(pad, 0, None).is_some(),
        "CompiledRuleSet should match channel 5"
    );

    // Channel 0 should NOT match
    let midi2 = MidiEvent::from_midi_msg(&[0x90, 60, 100]).unwrap();
    let input2: InputEvent = midi2.into();
    let mut proc2 = EventProcessor::new();
    let results2 = proc2.process_input(input2);
    let pad2 = results2
        .iter()
        .find(|e| matches!(e, ProcessedEvent::PadPressed { .. }))
        .expect("Should produce PadPressed");
    assert!(
        rules.match_event(pad2, 0, None).is_none(),
        "CompiledRuleSet should NOT match channel 0 when trigger has channel=Some(5)"
    );
}

// ============================================================
// 10. Channel-filter coverage for EVERY channel-bearing trigger (#1513)
//
// Previously only Note (+ CC) had channel-filter match/reject tests, and
// the CompiledRuleSet hot path covered Note only — a regression in any
// other channel-bearing trigger's match arm could ship while this suite
// stayed green. These table-driven cases construct each *matching*
// ProcessedEvent directly at a chosen channel (this is a trigger-FILTER
// test, not an event-production test) and assert accept-on-5 / reject-on-0
// through BOTH the MappingEngine and the daemon hot path
// (rule_compiler::compile + CompiledRuleSet::match_event).
// ============================================================

/// Build a minimal single-mapping config wrapping `trigger`.
fn config_with_trigger(trigger: conductor_core::config::Trigger) -> conductor_core::config::Config {
    use conductor_core::config::{ActionConfig, Config, Mapping, Mode};
    Config {
        mcp: Default::default(),
        config_meta: Default::default(),
        security: Default::default(),
        endpoints: vec![],
        modes: vec![Mode {
            name: "Test".to_string(),
            color: None,
            mappings: vec![Mapping {
                trigger,
                action: ActionConfig::Keystroke {
                    keys: "a".into(),
                    modifiers: vec![],
                },
                description: None,
                let_through: false,
            }],
        }],
        global_mappings: vec![],
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

/// Assert `trigger` (which carries `channel: Some(5)`) ACCEPTS an event on
/// channel 5 and REJECTS one on channel 0, through both the MappingEngine and
/// the CompiledRuleSet hot path. `event_at` builds the matching ProcessedEvent
/// at a given channel.
fn assert_channel_filter(
    label: &str,
    trigger: conductor_core::config::Trigger,
    event_at: impl Fn(Option<u8>) -> ProcessedEvent,
) {
    use conductor_core::mapping::MappingEngine;
    use conductor_core::rule_compiler;

    let config = config_with_trigger(trigger);
    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);
    let rules = rule_compiler::compile(&config, 1);

    let accept = event_at(Some(5));
    assert!(
        engine.get_action_for_processed(&accept, 0).is_some(),
        "{label}: MappingEngine must ACCEPT channel 5"
    );
    assert!(
        rules.match_event(&accept, 0, None).is_some(),
        "{label}: CompiledRuleSet must ACCEPT channel 5"
    );

    let reject = event_at(Some(0));
    assert!(
        engine.get_action_for_processed(&reject, 0).is_none(),
        "{label}: MappingEngine must REJECT channel 0"
    );
    assert!(
        rules.match_event(&reject, 0, None).is_none(),
        "{label}: CompiledRuleSet must REJECT channel 0"
    );
}

#[test]
fn channel_filter_accept_reject_for_all_channel_bearing_triggers() {
    use conductor_core::config::Trigger;
    use conductor_core::event_processor::{EncoderDirection, VelocityLevel};

    assert_channel_filter(
        "Note",
        Trigger::Note {
            note: 60,
            velocity_min: None,
            channel: Some(5),
            device: None,
        },
        |ch| ProcessedEvent::PadPressed {
            note: 60,
            velocity: 100,
            velocity_level: VelocityLevel::Hard,
            channel: ch,
        },
    );

    assert_channel_filter(
        "VelocityRange",
        Trigger::VelocityRange {
            note: 60,
            soft_max: None,
            medium_max: None,
            channel: Some(5),
            device: None,
        },
        |ch| ProcessedEvent::PadPressed {
            note: 60,
            velocity: 100,
            velocity_level: VelocityLevel::Hard,
            channel: ch,
        },
    );

    assert_channel_filter(
        "CC",
        Trigger::CC {
            cc: 7,
            value_min: None,
            channel: Some(5),
            device: None,
        },
        |ch| ProcessedEvent::CCReceived {
            cc: 7,
            value: 100,
            channel: ch,
        },
    );

    assert_channel_filter(
        "Aftertouch",
        Trigger::Aftertouch {
            pressure_min: None,
            channel: Some(5),
            device: None,
        },
        |ch| ProcessedEvent::AftertouchChanged {
            pressure: 80,
            channel: ch,
        },
    );

    assert_channel_filter(
        "PolyAftertouch",
        Trigger::PolyAftertouch {
            note: 60,
            pressure_min: None,
            channel: Some(5),
            device: None,
        },
        |ch| ProcessedEvent::PolyAftertouchChanged {
            note: 60,
            pressure: 80,
            channel: ch,
        },
    );

    assert_channel_filter(
        "PitchBend",
        Trigger::PitchBend {
            value_min: None,
            value_max: None,
            channel: Some(5),
            device: None,
        },
        |ch| ProcessedEvent::PitchBendMoved {
            value: 8192,
            channel: ch,
        },
    );

    assert_channel_filter(
        "ProgramChange",
        Trigger::ProgramChange {
            pc: None,
            channel: Some(5),
            device: None,
        },
        |ch| ProcessedEvent::ProgramChange {
            program: 5,
            channel: ch,
        },
    );

    // Explicit duration_ms so the case isn't coupled to the compiled default;
    // the event's 200ms clears the trigger's 100ms threshold.
    assert_channel_filter(
        "LongPress",
        Trigger::LongPress {
            note: 60,
            duration_ms: Some(100),
            channel: Some(5),
            device: None,
        },
        |ch| ProcessedEvent::LongPress {
            note: 60,
            duration_ms: 200,
            channel: ch,
        },
    );

    assert_channel_filter(
        "DoubleTap",
        Trigger::DoubleTap {
            note: 60,
            timeout_ms: None,
            channel: Some(5),
            device: None,
        },
        |ch| ProcessedEvent::DoubleTap {
            note: 60,
            first_velocity: 100,
            second_velocity: 100,
            interval_ms: 100,
            channel: ch,
        },
    );

    assert_channel_filter(
        "NoteChord",
        Trigger::NoteChord {
            notes: vec![60, 64],
            timeout_ms: None,
            channel: Some(5),
            device: None,
        },
        |ch| ProcessedEvent::ChordDetected {
            notes: vec![60, 64],
            velocities: vec![100, 100],
            channel: ch,
        },
    );

    // direction None matches any encoder direction.
    assert_channel_filter(
        "EncoderTurn",
        Trigger::EncoderTurn {
            cc: 20,
            direction: None,
            channel: Some(5),
            device: None,
        },
        |ch| ProcessedEvent::EncoderTurned {
            cc: 20,
            value: 65,
            direction: EncoderDirection::Clockwise,
            delta: 1,
            channel: ch,
        },
    );
}

// ────────────────────────────────────────────────────────────────
// #2125 — held-note (long-press) tracking is channel-isolated
// ────────────────────────────────────────────────────────────────

/// #2125: the held-note map is keyed by `(channel, note)`, so the SAME note
/// number held on two different MIDI channels is tracked independently. This
/// pins that invariant — pre-#434 (channel preservation) the held-note tracking
/// keyed by note alone, so the second NoteOn overwrote the first and one hold
/// was lost (a cross-channel collision). With note-only keying this asserts
/// 1 hold instead of 2 and fails.
#[test]
fn held_notes_are_isolated_by_channel() {
    let mut processor = EventProcessor::new();
    // Zero threshold: any held note qualifies immediately, so the test is
    // deterministic without sleeping.
    processor.set_hold_threshold(Duration::from_millis(0));

    // Same note number, two different MIDI channels.
    processor.process(MidiEvent::NoteOn {
        note: 60,
        velocity: 100,
        channel: 0,
        time: Instant::now(),
    });
    processor.process(MidiEvent::NoteOn {
        note: 60,
        velocity: 110,
        channel: 9,
        time: Instant::now(),
    });

    let channels: Vec<Option<u8>> = processor
        .check_holds()
        .iter()
        .filter_map(|e| match e {
            ProcessedEvent::HoldDetected {
                note: 60, channel, ..
            } => Some(*channel),
            _ => None,
        })
        .collect();

    assert_eq!(
        channels.len(),
        2,
        "note 60 held on two channels must yield two independent holds (#2125); got {channels:?}"
    );
    assert!(
        channels.contains(&Some(0)) && channels.contains(&Some(9)),
        "both channel-0 and channel-9 holds must be present; got {channels:?}"
    );
}
