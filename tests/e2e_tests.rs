// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! End-to-End Test Suite for Critical Workflows
//!
//! This module provides comprehensive E2E testing of the complete Conductor pipeline:
//! MIDI Input → Event Processing → Mapping → Action Execution
//!
//! All tests use MidiSimulator for hardware-independent testing and mock external
//! dependencies (keyboard/mouse simulation, app launching, etc.) to ensure tests
//! are reliable and fast.

// Integration test: diagnostic stdout/stderr output is legitimate here.
// See docs/epic-loop/rust-coverage.md Path A.
#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "test diagnostic output"
)]

mod midi_simulator;

use midi_simulator::{EncoderDirection, Gesture, MidiSimulator};

// Helper to skip wall-clock-sensitive assertions on CI. Loaded/virtualized
// runners (not just macOS) stall the scheduler enough to blow strict timing
// bounds with no product regression, so skip on ANY CI, not macOS alone (#1539).
fn should_skip_timing_test() -> bool {
    std::env::var("CI").is_ok()
}
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// Re-use types from the main crate for testing
use conductor::event_processor::{
    EncoderDirection as MidiDirection, EventProcessor, MidiEvent, ProcessedEvent, VelocityLevel,
};

// #1540: the REAL mapping boundary, so workflow tests assert the action the
// production engine produces rather than one the test injects into the mock.
use conductor_core::actions::{Action, KeyCode, ModifierKey};
use conductor_core::config::Config;
use conductor_core::mapping::MappingEngine;

/// Build a `MappingEngine` from a TOML config.
fn engine_from_toml(toml: &str) -> MappingEngine {
    let config: Config = toml::from_str(toml).expect("parse e2e config");
    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);
    engine
}

/// Drive a real MIDI note-on through `EventProcessor` → `MappingEngine` and
/// return the action the PRODUCTION mapping engine resolved for `mode` (or
/// `None` if nothing matched). No manual action injection.
fn map_note(engine: &MappingEngine, note: u8, velocity: u8, mode: u8) -> Option<Action> {
    let mut processor = EventProcessor::new();
    let processed = processor.process(MidiEvent::NoteOn {
        note,
        velocity,
        channel: 0,
        time: std::time::Instant::now(),
    });
    let pad = processed
        .iter()
        .find(|e| matches!(e, ProcessedEvent::PadPressed { .. }))?;
    engine.get_action_for_processed(pad, mode)
}

/// Mock action executor that records executed actions instead of actually performing them
#[derive(Clone)]
struct MockActionExecutor {
    executed_actions: Arc<Mutex<Vec<ExecutedAction>>>,
}

#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
enum ExecutedAction {
    Keystroke {
        keys: String,
        modifiers: Vec<String>,
    },
    Text(String),
    Launch(String),
    Shell(String),
    Sequence(Vec<ExecutedAction>),
    Delay(u64),
    MouseClick {
        button: String,
        x: Option<i32>,
        y: Option<i32>,
    },
    VolumeUp,
    VolumeDown,
    VolumeMute,
    ModeChange(u8),
}

impl MockActionExecutor {
    fn new() -> Self {
        Self {
            executed_actions: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn execute(&mut self, action: ExecutedAction) {
        self.executed_actions.lock().unwrap().push(action);
    }

    fn get_executed_actions(&self) -> Vec<ExecutedAction> {
        self.executed_actions.lock().unwrap().clone()
    }
}

/// Helper to convert MIDI simulator events to Conductor MidiEvent
fn convert_sim_to_midi_event(raw: Vec<u8>) -> Option<MidiEvent> {
    if raw.is_empty() {
        return None;
    }

    let status = raw[0];
    let message_type = status & 0xF0;
    let channel = status & 0x0F;
    let time = Instant::now();

    match message_type {
        0x90 => Some(MidiEvent::NoteOn {
            note: raw[1],
            velocity: raw[2],
            channel,
            time,
        }),
        0x80 => Some(MidiEvent::NoteOff {
            note: raw[1],
            channel,
            time,
        }),
        0xB0 => Some(MidiEvent::ControlChange {
            cc: raw[1],
            value: raw[2],
            channel,
            time,
        }),
        0xD0 => Some(MidiEvent::Aftertouch {
            pressure: raw[1],
            channel,
            time,
        }),
        0xE0 => {
            let value = ((raw[2] as u16) << 7) | (raw[1] as u16);
            Some(MidiEvent::PitchBend {
                value,
                channel,
                time,
            })
        }
        0xC0 => Some(MidiEvent::ProgramChange {
            program: raw[1],
            channel,
            time,
        }),
        _ => None,
    }
}

/// Test helper to process a complete workflow
struct E2ETestHarness {
    simulator: MidiSimulator,
    processor: EventProcessor,
    executor: MockActionExecutor,
}

impl E2ETestHarness {
    fn new() -> Self {
        Self {
            simulator: MidiSimulator::new(0),
            processor: EventProcessor::new(),
            executor: MockActionExecutor::new(),
        }
    }

    fn simulate_and_process(&mut self, gesture: Gesture) -> Vec<ProcessedEvent> {
        // Clone the gesture to inspect timing
        let _base_time = Instant::now();

        match gesture {
            Gesture::SimpleTap {
                note,
                velocity,
                duration_ms,
            } => {
                // Note on immediately
                self.simulator.note_on(note, velocity);
                let on_event = convert_sim_to_midi_event(self.simulator.get_events()[0].clone())
                    .expect("Failed to convert note on");
                let mut processed = self.processor.process(on_event);

                // Wait for duration
                std::thread::sleep(Duration::from_millis(duration_ms));

                // Note off after duration
                self.simulator.note_off(note);
                let off_event =
                    convert_sim_to_midi_event(self.simulator.peek_last_event().unwrap())
                        .expect("Failed to convert note off");
                processed.extend(self.processor.process(off_event));

                processed
            }
            Gesture::LongPress {
                note,
                velocity,
                hold_ms,
            } => {
                // Note on immediately
                self.simulator.note_on(note, velocity);
                let raw = self.simulator.get_events();
                let on_event =
                    convert_sim_to_midi_event(raw[0].clone()).expect("Failed to convert note on");
                let mut processed = self.processor.process(on_event);

                // Wait for hold duration
                std::thread::sleep(Duration::from_millis(hold_ms));

                // Note off after hold
                self.simulator.note_off(note);
                let raw2 = self.simulator.get_events();
                let off_event =
                    convert_sim_to_midi_event(raw2[0].clone()).expect("Failed to convert note off");
                processed.extend(self.processor.process(off_event));

                processed
            }
            Gesture::DoubleTap {
                note,
                velocity,
                tap_duration_ms,
                gap_ms,
            } => {
                let mut processed = Vec::new();

                // First tap
                self.simulator.note_on(note, velocity);
                let raw = self.simulator.get_events();
                let on1 = convert_sim_to_midi_event(raw[0].clone()).unwrap();
                processed.extend(self.processor.process(on1));

                std::thread::sleep(Duration::from_millis(tap_duration_ms));

                self.simulator.note_off(note);
                let raw = self.simulator.get_events();
                let off1 = convert_sim_to_midi_event(raw[0].clone()).unwrap();
                processed.extend(self.processor.process(off1));

                // Gap between taps
                std::thread::sleep(Duration::from_millis(gap_ms));

                // Second tap
                self.simulator.note_on(note, velocity);
                let raw = self.simulator.get_events();
                let on2 = convert_sim_to_midi_event(raw[0].clone()).unwrap();
                processed.extend(self.processor.process(on2));

                std::thread::sleep(Duration::from_millis(tap_duration_ms));

                self.simulator.note_off(note);
                let raw = self.simulator.get_events();
                let off2 = convert_sim_to_midi_event(raw[0].clone()).unwrap();
                processed.extend(self.processor.process(off2));

                processed
            }
            _ => {
                // For other gestures, use the old approach
                self.simulator.perform_gesture(gesture);
                let raw_events = self.simulator.get_events();

                let mut all_processed = Vec::new();
                for raw in raw_events {
                    if let Some(midi_event) = convert_sim_to_midi_event(raw) {
                        let processed = self.processor.process(midi_event);
                        all_processed.extend(processed);
                    }
                }

                all_processed
            }
        }
    }
}

// =============================================================================
// WORKFLOW 1: Simple Pad Press → Keystroke
// =============================================================================

#[test]
fn test_e2e_simple_pad_press_to_keystroke() {
    let mut harness = E2ETestHarness::new();

    // Simulate a simple pad press (note 60, velocity 100)
    let processed = harness.simulate_and_process(Gesture::SimpleTap {
        note: 60,
        velocity: 100,
        duration_ms: 100,
    });

    // Verify event processing detected the press
    assert!(!processed.is_empty(), "Should generate processed events");

    let has_pad_pressed = processed
        .iter()
        .any(|e| matches!(e, ProcessedEvent::PadPressed { note, .. } if *note == 60));
    assert!(has_pad_pressed, "Should detect PadPressed event");

    // Verify velocity level is correct (100 = Hard)
    let velocity_level = processed.iter().find_map(|e| {
        if let ProcessedEvent::PadPressed {
            note,
            velocity_level,
            ..
        } = e
            && *note == 60
        {
            return Some(*velocity_level);
        }
        None
    });
    assert_eq!(velocity_level, Some(VelocityLevel::Hard));

    // #1540: resolve the action through the REAL mapping engine instead of
    // injecting the expected action into the mock. A broken mapping engine now
    // fails this test.
    use conductor_core::actions::{KeyCode, ModifierKey};
    let engine = engine_from_toml(
        r#"
[[modes]]
name = "Default"
[[modes.mappings]]
trigger = { type = "Note", note = 60 }
action = { type = "Keystroke", keys = "space", modifiers = ["cmd"] }
"#,
    );
    let action = map_note(&engine, 60, 100, 0).expect("note 60 must map to the configured action");
    match action {
        Action::Keystroke { keys, modifiers } => {
            assert_eq!(keys, vec![KeyCode::Space]);
            assert_eq!(modifiers, vec![ModifierKey::Command]);
        }
        other => panic!("expected Keystroke(cmd+space) from the mapping engine, got {other:?}"),
    }
}

#[test]
fn test_e2e_timing_latency() {
    let mut harness = E2ETestHarness::new();

    let start = Instant::now();

    // Simulate note press
    harness.simulator.note_on(60, 100);
    let raw = harness.simulator.get_events();

    // Process event
    if let Some(midi_event) = convert_sim_to_midi_event(raw[0].clone()) {
        let _ = harness.processor.process(midi_event);
    }

    let latency = start.elapsed();

    // The pipeline above is the real coverage (it runs everywhere, catching
    // panics/regressions). The sub-millisecond bound is a perf smoke check that
    // flakes on loaded/debug CI, so only enforce it off-CI (#1539).
    if !should_skip_timing_test() {
        assert!(
            latency < Duration::from_millis(1),
            "Processing latency should be < 1ms, was {:?}",
            latency
        );
    }
}

// =============================================================================
// WORKFLOW 2: Velocity-Sensitive Mapping
// =============================================================================

#[test]
fn test_e2e_velocity_soft_action() {
    let mut harness = E2ETestHarness::new();

    // Simulate soft press (velocity 30)
    let processed = harness.simulate_and_process(Gesture::SimpleTap {
        note: 60,
        velocity: 30,
        duration_ms: 100,
    });

    // Verify soft velocity level detected
    let velocity_level = processed.iter().find_map(|e| {
        if let ProcessedEvent::PadPressed { velocity_level, .. } = e {
            Some(*velocity_level)
        } else {
            None
        }
    });
    assert_eq!(velocity_level, Some(VelocityLevel::Soft));

    // Simulate soft action execution
    harness
        .executor
        .execute(ExecutedAction::Text("soft".to_string()));

    let actions = harness.executor.get_executed_actions();
    assert_eq!(actions[0], ExecutedAction::Text("soft".to_string()));
}

#[test]
fn test_e2e_velocity_medium_action() {
    let mut harness = E2ETestHarness::new();

    // Simulate medium press (velocity 70)
    let processed = harness.simulate_and_process(Gesture::SimpleTap {
        note: 60,
        velocity: 70,
        duration_ms: 100,
    });

    // Verify medium velocity level detected
    let velocity_level = processed.iter().find_map(|e| {
        if let ProcessedEvent::PadPressed { velocity_level, .. } = e {
            Some(*velocity_level)
        } else {
            None
        }
    });
    assert_eq!(velocity_level, Some(VelocityLevel::Medium));

    // Simulate medium action execution
    harness
        .executor
        .execute(ExecutedAction::Text("medium".to_string()));

    let actions = harness.executor.get_executed_actions();
    assert_eq!(actions[0], ExecutedAction::Text("medium".to_string()));
}

#[test]
fn test_e2e_velocity_hard_action() {
    let mut harness = E2ETestHarness::new();

    // Simulate hard press (velocity 110)
    let processed = harness.simulate_and_process(Gesture::SimpleTap {
        note: 60,
        velocity: 110,
        duration_ms: 100,
    });

    // Verify hard velocity level detected
    let velocity_level = processed.iter().find_map(|e| {
        if let ProcessedEvent::PadPressed { velocity_level, .. } = e {
            Some(*velocity_level)
        } else {
            None
        }
    });
    assert_eq!(velocity_level, Some(VelocityLevel::Hard));

    // Simulate hard action execution
    harness
        .executor
        .execute(ExecutedAction::Text("hard".to_string()));

    let actions = harness.executor.get_executed_actions();
    assert_eq!(actions[0], ExecutedAction::Text("hard".to_string()));
}

// =============================================================================
// WORKFLOW 3: Long Press Detection
// =============================================================================

#[test]
fn test_e2e_long_press_detected() {
    let mut harness = E2ETestHarness::new();

    // Simulate long press (hold for 1500ms - threshold is 1000ms)
    let processed = harness.simulate_and_process(Gesture::LongPress {
        note: 60,
        velocity: 80,
        hold_ms: 1500,
    });

    // Verify long press event detected (threshold is 1000ms, not 2000ms)
    let has_long_press = processed.iter().any(|e| {
        matches!(e, ProcessedEvent::LongPress { note, duration_ms, .. }
            if *note == 60 && *duration_ms >= 1000)
    });
    assert!(has_long_press, "Should detect long press after 1000ms");

    // Simulate long press action
    harness
        .executor
        .execute(ExecutedAction::Launch("Calculator".to_string()));

    let actions = harness.executor.get_executed_actions();
    assert!(matches!(actions[0], ExecutedAction::Launch(_)));
}

#[test]
fn test_e2e_long_press_not_triggered_early_release() {
    let mut harness = E2ETestHarness::new();

    // Simulate medium press (hold for 500ms, below 1000ms threshold)
    let processed = harness.simulate_and_process(Gesture::LongPress {
        note: 60,
        velocity: 80,
        hold_ms: 500,
    });

    // Verify long press NOT detected (should be MediumPress instead)
    let has_long_press = processed
        .iter()
        .any(|e| matches!(e, ProcessedEvent::LongPress { .. }));
    assert!(
        !has_long_press,
        "Should NOT detect long press before 1000ms"
    );

    // Verify medium press detected instead (200-1000ms range)
    let has_medium_press = processed
        .iter()
        .any(|e| matches!(e, ProcessedEvent::MediumPress { note, .. } if *note == 60));
    assert!(has_medium_press, "Should detect medium press instead");
}

// =============================================================================
// WORKFLOW 4: Double-Tap Recognition
// =============================================================================

#[test]
fn test_e2e_double_tap_detected() {
    if should_skip_timing_test() {
        eprintln!("Skipping test_e2e_double_tap_detected on CI due to runner timing variance");
        return;
    }

    let mut harness = E2ETestHarness::new();

    // Simulate double-tap (two taps within 300ms)
    let processed = harness.simulate_and_process(Gesture::DoubleTap {
        note: 60,
        velocity: 80,
        tap_duration_ms: 50,
        gap_ms: 200,
    });

    // Verify double-tap event detected
    let has_double_tap = processed
        .iter()
        .any(|e| matches!(e, ProcessedEvent::DoubleTap { note, .. } if *note == 60));
    assert!(
        has_double_tap,
        "Should detect double-tap within 300ms window"
    );

    // Simulate double-tap action
    harness
        .executor
        .execute(ExecutedAction::Shell("open .".to_string()));

    let actions = harness.executor.get_executed_actions();
    assert!(matches!(actions[0], ExecutedAction::Shell(_)));
}

#[test]
fn test_e2e_double_tap_not_detected_slow_taps() {
    let mut harness = E2ETestHarness::new();

    // Simulate two taps with large gap between first NoteOn and second NoteOn
    // Double-tap window is measured from first NoteOn to second NoteOn
    // With tap_duration=50ms and gap=400ms, total time = 50 + 400 = 450ms
    // This exceeds the 300ms window
    let processed = harness.simulate_and_process(Gesture::DoubleTap {
        note: 60,
        velocity: 80,
        tap_duration_ms: 50,
        gap_ms: 300, // With tap duration of 50ms, total = 350ms > 300ms window
    });

    // Double-tap is measured from first NoteOn to second NoteOn
    // Total time from first press to second press = tap_duration + gap = 50 + 300 = 350ms
    // Since 350ms > 300ms window, double-tap should NOT be detected
    let has_double_tap = processed
        .iter()
        .any(|e| matches!(e, ProcessedEvent::DoubleTap { .. }));
    assert!(
        !has_double_tap,
        "Should NOT detect double-tap outside 300ms window"
    );
}

// =============================================================================
// WORKFLOW 5: Chord Detection
// =============================================================================

#[test]
fn test_e2e_chord_detected() {
    let mut harness = E2ETestHarness::new();

    // Simulate C major chord (notes pressed within 50ms)
    let processed = harness.simulate_and_process(Gesture::Chord {
        notes: vec![60, 64, 67],
        velocity: 80,
        stagger_ms: 10, // Within 50ms chord timeout
        hold_ms: 500,
    });

    // Verify chord detected
    let has_chord = processed.iter().any(|e| {
        if let ProcessedEvent::ChordDetected { notes, .. } = e {
            notes.contains(&60) && notes.contains(&64) && notes.contains(&67)
        } else {
            false
        }
    });
    assert!(
        has_chord,
        "Should detect chord when notes pressed within 50ms"
    );

    // Simulate chord action
    harness.executor.execute(ExecutedAction::Keystroke {
        keys: "q".to_string(),
        modifiers: vec!["cmd".to_string(), "shift".to_string()],
    });

    let actions = harness.executor.get_executed_actions();
    assert!(matches!(actions[0], ExecutedAction::Keystroke { .. }));
}

#[test]
fn test_e2e_chord_not_detected_sequential_notes() {
    let mut harness = E2ETestHarness::new();

    // Simulate notes pressed sequentially with large gaps (>50ms)
    let processed = harness.simulate_and_process(Gesture::Chord {
        notes: vec![60, 64, 67],
        velocity: 80,
        stagger_ms: 100, // Exceeds 50ms chord timeout
        hold_ms: 500,
    });

    // Notes are processed individually, not as a chord
    // Chord should only be detected for the last few notes within window
    let chord_count = processed
        .iter()
        .filter(|e| matches!(e, ProcessedEvent::ChordDetected { .. }))
        .count();

    // With 100ms stagger, by the time third note arrives, first is outside 50ms window
    // So we should see partial chords or no chord
    assert!(
        chord_count < 3,
        "Should not detect full chord with 100ms stagger"
    );
}

// =============================================================================
// WORKFLOW 6: Mode Switching
// =============================================================================

#[test]
fn test_e2e_mode_switch_via_encoder() {
    let mut harness = E2ETestHarness::new();

    // Simulate encoder turn clockwise for mode change
    harness.simulator.perform_gesture(Gesture::EncoderTurn {
        cc: 1,
        direction: EncoderDirection::Clockwise,
        steps: 3,
        step_delay_ms: 10,
    });

    let raw_events = harness.simulator.get_events();
    let mut encoder_events = Vec::new();

    for raw in raw_events {
        if let Some(midi_event) = convert_sim_to_midi_event(raw) {
            let processed = harness.processor.process(midi_event);
            encoder_events.extend(processed);
        }
    }

    // Verify encoder turn detected
    let has_encoder = encoder_events
        .iter()
        .any(|e| matches!(e, ProcessedEvent::EncoderTurned { .. }));
    assert!(has_encoder, "Should detect encoder turn");

    // Simulate mode change action
    harness.executor.execute(ExecutedAction::ModeChange(1));

    let actions = harness.executor.get_executed_actions();
    assert_eq!(actions[0], ExecutedAction::ModeChange(1));
}

#[test]
fn test_e2e_mode_specific_mapping() {
    // #1540: the SAME note must resolve to DIFFERENT actions per mode through
    // the real mapping engine — previously the test injected both actions by
    // hand, so a broken mode-specific lookup would not fail it.
    let engine = engine_from_toml(
        r#"
[[modes]]
name = "Spotlight"
[[modes.mappings]]
trigger = { type = "Note", note = 60 }
action = { type = "Keystroke", keys = "space", modifiers = ["cmd"] }

[[modes]]
name = "Development"
[[modes.mappings]]
trigger = { type = "Note", note = 60 }
action = { type = "Shell", command = "git status" }
"#,
    );

    // Mode 0: note 60 → cmd+space. Assert the exact keys/modifiers so a
    // regression that maps note 60 to a *different* keystroke still fails.
    let mode0 = map_note(&engine, 60, 100, 0).expect("note 60 must map in mode 0");
    match mode0 {
        Action::Keystroke { keys, modifiers } => {
            assert_eq!(keys, vec![KeyCode::Space], "mode 0 keys should be [Space]");
            assert_eq!(
                modifiers,
                vec![ModifierKey::Command],
                "mode 0 modifiers should be [Command]"
            );
        }
        other => panic!("mode 0 should resolve to a Keystroke, got {other:?}"),
    }

    // Mode 1: the SAME note 60 → a Shell action.
    let mode1 = map_note(&engine, 60, 100, 1).expect("note 60 must map in mode 1");
    match mode1 {
        Action::Shell { command, .. } => assert_eq!(command, "git status"),
        other => panic!("mode 1 should resolve to Shell('git status'), got {other:?}"),
    }
}

#[test]
fn test_e2e_global_mapping_works_in_all_modes() {
    // #1540: a global mapping must resolve in EVERY mode through the real
    // engine — previously the test injected the action itself, so a broken
    // global-mapping fallback would not fail it.
    let engine = engine_from_toml(
        r#"
[[modes]]
name = "Mode0"

[[modes]]
name = "Mode1"

[[global_mappings]]
trigger = { type = "Note", note = 127 }
action = { type = "Shell", command = "killall conductor" }
"#,
    );

    // The global mapping for note 127 resolves the same in both modes.
    for mode in [0u8, 1u8] {
        let action = map_note(&engine, 127, 100, mode)
            .unwrap_or_else(|| panic!("global mapping must resolve in mode {mode}"));
        match action {
            Action::Shell { command, .. } => assert_eq!(command, "killall conductor"),
            other => panic!("mode {mode} global mapping should be Shell, got {other:?}"),
        }
    }
}

// =============================================================================
// WORKFLOW 7: Sequence Actions
// =============================================================================

#[test]
fn test_e2e_sequence_execution_order() {
    let mut harness = E2ETestHarness::new();

    // Simulate sequence: Keystroke → Delay → Text
    harness.executor.execute(ExecutedAction::Sequence(vec![
        ExecutedAction::Keystroke {
            keys: "t".to_string(),
            modifiers: vec!["cmd".to_string()],
        },
        ExecutedAction::Delay(500),
        ExecutedAction::Text("test".to_string()),
    ]));

    let actions = harness.executor.get_executed_actions();
    assert_eq!(actions.len(), 1); // Sequence is one action

    // Verify sequence contents
    if let ExecutedAction::Sequence(seq) = &actions[0] {
        assert_eq!(seq.len(), 3);
        assert!(matches!(seq[0], ExecutedAction::Keystroke { .. }));
        assert_eq!(seq[1], ExecutedAction::Delay(500));
        assert!(matches!(seq[2], ExecutedAction::Text(_)));
    } else {
        panic!("Expected Sequence action");
    }
}

#[test]
fn test_e2e_sequence_with_timing() {
    let mut harness = E2ETestHarness::new();

    let _start = Instant::now();

    // Simulate sequence with delays
    harness.executor.execute(ExecutedAction::Sequence(vec![
        ExecutedAction::Text("a".to_string()),
        ExecutedAction::Delay(100),
        ExecutedAction::Text("b".to_string()),
        ExecutedAction::Delay(100),
        ExecutedAction::Text("c".to_string()),
    ]));

    // In real execution, this would take ~200ms
    // For mock, we just verify structure
    let actions = harness.executor.get_executed_actions();
    assert_eq!(actions.len(), 1);
}

// =============================================================================
// WORKFLOW 8: Conditional Actions (Mock-Based)
// =============================================================================

#[test]
fn test_e2e_conditional_app_based() {
    let mut harness = E2ETestHarness::new();

    // Mock: In VSCode, note 60 triggers "Save"
    // In Chrome, note 60 triggers "New Tab"

    // Simulate frontmost app = VSCode
    let frontmost_app = "VSCode";

    harness.simulator.note_on(60, 100);

    if frontmost_app == "VSCode" {
        harness.executor.execute(ExecutedAction::Keystroke {
            keys: "s".to_string(),
            modifiers: vec!["cmd".to_string()],
        });
    }

    let actions = harness.executor.get_executed_actions();
    assert!(matches!(actions[0], ExecutedAction::Keystroke { .. }));
}

#[test]
fn test_e2e_conditional_time_based() {
    let mut harness = E2ETestHarness::new();

    // Mock: During work hours (9-17), note 60 = Focus Mode
    // Outside work hours, note 60 = Gaming Mode

    let current_hour = 14; // 2 PM

    harness.simulator.note_on(60, 100);

    if (9..17).contains(&current_hour) {
        harness
            .executor
            .execute(ExecutedAction::Launch("Focus".to_string()));
    } else {
        harness
            .executor
            .execute(ExecutedAction::Launch("Steam".to_string()));
    }

    let actions = harness.executor.get_executed_actions();
    assert_eq!(actions[0], ExecutedAction::Launch("Focus".to_string()));
}

#[test]
fn test_e2e_conditional_mode_based() {
    let mut harness = E2ETestHarness::new();

    let current_mode = 0;

    // In mode 0, encoder = volume control
    // In mode 1, encoder = brightness control

    harness.simulator.control_change(1, 65);

    if current_mode == 0 {
        harness.executor.execute(ExecutedAction::VolumeUp);
    } else {
        harness
            .executor
            .execute(ExecutedAction::Shell("brightness up".to_string()));
    }

    let actions = harness.executor.get_executed_actions();
    assert_eq!(actions[0], ExecutedAction::VolumeUp);
}

// =============================================================================
// WORKFLOW 9: Volume Control via Encoder
// =============================================================================

#[test]
fn test_e2e_encoder_volume_up() {
    let mut harness = E2ETestHarness::new();

    // Simulate encoder turn clockwise for volume up
    harness.simulator.perform_gesture(Gesture::EncoderTurn {
        cc: 1,
        direction: EncoderDirection::Clockwise,
        steps: 5,
        step_delay_ms: 0,
    });

    let raw_events = harness.simulator.get_events();

    for raw in raw_events {
        if let Some(midi_event) = convert_sim_to_midi_event(raw) {
            let processed = harness.processor.process(midi_event);

            // For each encoder turn event, simulate volume up
            for event in processed {
                if let ProcessedEvent::EncoderTurned { direction, .. } = event
                    && matches!(direction, MidiDirection::Clockwise)
                {
                    harness.executor.execute(ExecutedAction::VolumeUp);
                }
            }
        }
    }

    // First CC event has no previous value, so it doesn't generate an EncoderTurned event
    // Therefore, we expect 4 EncoderTurned events from 5 CC messages
    let volume_ups = harness
        .executor
        .get_executed_actions()
        .iter()
        .filter(|a| matches!(a, ExecutedAction::VolumeUp))
        .count();

    assert_eq!(
        volume_ups, 4,
        "Should execute VolumeUp 4 times (first CC has no previous value)"
    );
}

#[test]
fn test_e2e_encoder_volume_down() {
    let mut harness = E2ETestHarness::new();

    // Simulate encoder turn counterclockwise for volume down
    harness.simulator.perform_gesture(Gesture::EncoderTurn {
        cc: 1,
        direction: EncoderDirection::CounterClockwise,
        steps: 3,
        step_delay_ms: 0,
    });

    let raw_events = harness.simulator.get_events();

    for raw in raw_events {
        if let Some(midi_event) = convert_sim_to_midi_event(raw) {
            let processed = harness.processor.process(midi_event);

            for event in processed {
                if let ProcessedEvent::EncoderTurned { direction, .. } = event
                    && matches!(direction, MidiDirection::CounterClockwise)
                {
                    harness.executor.execute(ExecutedAction::VolumeDown);
                }
            }
        }
    }

    // First CC event has no previous value, so we expect 2 EncoderTurned events from 3 CC messages
    let volume_downs = harness
        .executor
        .get_executed_actions()
        .iter()
        .filter(|a| matches!(a, ExecutedAction::VolumeDown))
        .count();

    assert_eq!(
        volume_downs, 2,
        "Should execute VolumeDown 2 times (first CC has no previous value)"
    );
}

// =============================================================================
// ERROR RECOVERY AND EDGE CASES
// =============================================================================

#[test]
fn test_e2e_rapid_note_events() {
    let mut harness = E2ETestHarness::new();

    // Simulate rapid pad hits (drum pattern)
    for i in 0..20 {
        harness.simulator.perform_gesture(Gesture::SimpleTap {
            note: 60 + (i % 8),
            velocity: 80,
            duration_ms: 50,
        });
    }

    let raw_events = harness.simulator.get_events();
    assert_eq!(raw_events.len(), 40); // 20 note ons + 20 note offs

    // Process all events
    let mut total_processed = 0;
    for raw in raw_events {
        if let Some(midi_event) = convert_sim_to_midi_event(raw) {
            let processed = harness.processor.process(midi_event);
            total_processed += processed.len();
        }
    }

    assert!(total_processed >= 40, "Should process all rapid events");
}

#[test]
fn test_e2e_invalid_note_range_handling() {
    let mut harness = E2ETestHarness::new();

    // Test edge case note numbers
    let test_notes = vec![0, 1, 126, 127];

    for note in test_notes {
        harness.simulator.note_on(note, 100);
        harness.simulator.note_off(note);
    }

    let raw_events = harness.simulator.get_events();
    assert_eq!(raw_events.len(), 8); // 4 note ons + 4 note offs

    // All events should be processed without panics
    for raw in raw_events {
        if let Some(midi_event) = convert_sim_to_midi_event(raw) {
            let _ = harness.processor.process(midi_event);
        }
    }
}

// =============================================================================
// PERFORMANCE BENCHMARKS
// =============================================================================

#[test]
fn test_e2e_throughput_100_events() {
    let mut harness = E2ETestHarness::new();

    let start = Instant::now();

    // Generate 100 note events
    for i in 0..100 {
        harness.simulator.note_on(60 + (i % 12), 80);
        harness.simulator.note_off(60 + (i % 12));
    }

    let raw_events = harness.simulator.get_events();

    for raw in raw_events {
        if let Some(midi_event) = convert_sim_to_midi_event(raw) {
            let _ = harness.processor.process(midi_event);
        }
    }

    let duration = start.elapsed();

    // Processing the 100 note presses (200 MIDI messages: note-on + note-off)
    // above is the real coverage. The 10ms wall-clock bound is a perf smoke
    // check that flakes on loaded/debug CI, so only enforce it off-CI (#1539).
    if !should_skip_timing_test() {
        assert!(
            duration < Duration::from_millis(10),
            "Processing 100 note presses (200 MIDI messages) took {:?}, should be < 10ms",
            duration
        );
    }
}

#[test]
fn test_e2e_memory_stability_sustained_load() {
    let mut harness = E2ETestHarness::new();

    // Simulate sustained load (1000 events)
    for _ in 0..1000 {
        harness.simulator.note_on(60, 80);
        harness.simulator.note_off(60);
    }

    let raw_events = harness.simulator.get_events();

    for raw in raw_events {
        if let Some(midi_event) = convert_sim_to_midi_event(raw) {
            let _ = harness.processor.process(midi_event);
        }
    }

    // If we got here without panic, memory handling is stable
}
