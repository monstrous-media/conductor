// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Integration tests for InputEvent protocol abstraction
//!
//! These tests verify that the InputEvent enum correctly abstracts
//! MIDI events into protocol-agnostic representations.

use conductor_core::events::{InputEvent, MidiEvent};
use std::time::Instant;

#[test]
fn test_pad_pressed_conversion() {
    let time = Instant::now();
    let midi = MidiEvent::NoteOn {
        note: 36,
        velocity: 100,
        channel: 0,
        time,
    };

    let input: InputEvent = midi.into();

    match input {
        InputEvent::PadPressed {
            pad,
            velocity,
            channel: Some(0),
            time: event_time,
        } => {
            assert_eq!(pad, 36, "Pad number should match MIDI note");
            assert_eq!(velocity, 100, "Velocity should be preserved");
            assert_eq!(event_time, time, "Timestamp should be preserved");
        }
        _ => panic!("Expected PadPressed variant"),
    }
}

#[test]
fn test_pad_released_conversion() {
    let time = Instant::now();
    let midi = MidiEvent::NoteOff {
        note: 48,
        channel: 0,
        time,
    };

    let input: InputEvent = midi.into();

    match input {
        InputEvent::PadReleased {
            pad,
            channel: Some(0),
            time: event_time,
        } => {
            assert_eq!(pad, 48, "Pad number should match MIDI note");
            assert_eq!(event_time, time, "Timestamp should be preserved");
        }
        _ => panic!("Expected PadReleased variant"),
    }
}

#[test]
fn test_control_change_conversion() {
    // v4.10.9: CC events now convert to ControlChange (not EncoderTurned)
    // The EventProcessor detects encoder rotation from value changes
    let time = Instant::now();
    let midi = MidiEvent::ControlChange {
        cc: 1,
        value: 64,
        channel: 0,
        time,
    };

    let input: InputEvent = midi.into();

    match input {
        InputEvent::ControlChange {
            control,
            value,
            channel: Some(0),
            time: event_time,
        } => {
            assert_eq!(control, 1, "Control ID should match CC number");
            assert_eq!(value, 64, "Control value should be preserved");
            assert_eq!(event_time, time, "Timestamp should be preserved");
        }
        _ => panic!("Expected ControlChange variant"),
    }
}

#[test]
fn test_poly_pressure_conversion() {
    let time = Instant::now();
    let midi = MidiEvent::PolyPressure {
        note: 60,
        pressure: 90,
        channel: 0,
        time,
    };

    let input: InputEvent = midi.into();

    match input {
        InputEvent::PolyPressure {
            pad,
            pressure,
            channel: Some(0),
            time: event_time,
        } => {
            assert_eq!(pad, 60, "Pad number should match MIDI note");
            assert_eq!(pressure, 90, "Pressure should be preserved");
            assert_eq!(event_time, time, "Timestamp should be preserved");
        }
        _ => panic!("Expected PolyPressure variant"),
    }
}

#[test]
fn test_aftertouch_conversion() {
    let time = Instant::now();
    let midi = MidiEvent::Aftertouch {
        pressure: 80,
        channel: 0,
        time,
    };

    let input: InputEvent = midi.into();

    match input {
        InputEvent::Aftertouch {
            pressure,
            channel: Some(0),
            time: event_time,
        } => {
            assert_eq!(pressure, 80, "Pressure should be preserved");
            assert_eq!(event_time, time, "Timestamp should be preserved");
        }
        _ => panic!("Expected Aftertouch variant"),
    }
}

#[test]
fn test_pitch_bend_conversion() {
    let time = Instant::now();
    let midi = MidiEvent::PitchBend {
        value: 12000,
        channel: 0,
        time,
    };

    let input: InputEvent = midi.into();

    match input {
        InputEvent::PitchBend {
            value,
            channel: Some(0),
            time: event_time,
        } => {
            assert_eq!(value, 12000, "Pitch bend value should be preserved");
            assert_eq!(event_time, time, "Timestamp should be preserved");
        }
        _ => panic!("Expected PitchBend variant"),
    }
}

#[test]
fn test_program_change_conversion() {
    let time = Instant::now();
    let midi = MidiEvent::ProgramChange {
        program: 5,
        channel: 0,
        time,
    };

    let input: InputEvent = midi.into();

    match input {
        InputEvent::ProgramChange {
            program,
            channel: Some(0),
            time: event_time,
        } => {
            assert_eq!(program, 5, "Program number should be preserved");
            assert_eq!(event_time, time, "Timestamp should be preserved");
        }
        _ => panic!("Expected ProgramChange variant"),
    }
}

#[test]
fn test_velocity_range_preservation() {
    let time = Instant::now();

    // Test minimum velocity (0)
    let midi_min = MidiEvent::NoteOn {
        note: 1,
        velocity: 0,
        channel: 0,
        time,
    };
    let input_min: InputEvent = midi_min.into();
    if let InputEvent::PadPressed { velocity, .. } = input_min {
        assert_eq!(velocity, 0, "Minimum velocity should be preserved");
    } else {
        panic!("Expected PadPressed variant");
    }

    // Test medium velocity (64)
    let midi_med = MidiEvent::NoteOn {
        note: 1,
        velocity: 64,
        channel: 0,
        time,
    };
    let input_med: InputEvent = midi_med.into();
    if let InputEvent::PadPressed { velocity, .. } = input_med {
        assert_eq!(velocity, 64, "Medium velocity should be preserved");
    } else {
        panic!("Expected PadPressed variant");
    }

    // Test maximum velocity (127)
    let midi_max = MidiEvent::NoteOn {
        note: 1,
        velocity: 127,
        channel: 0,
        time,
    };
    let input_max: InputEvent = midi_max.into();
    if let InputEvent::PadPressed { velocity, .. } = input_max {
        assert_eq!(velocity, 127, "Maximum velocity should be preserved");
    } else {
        panic!("Expected PadPressed variant");
    }
}

#[test]
fn test_pitch_bend_range_preservation() {
    let time = Instant::now();

    // Test minimum pitch bend (0 = full down)
    let midi_min = MidiEvent::PitchBend {
        value: 0,
        channel: 0,
        time,
    };
    let input_min: InputEvent = midi_min.into();
    if let InputEvent::PitchBend { value, .. } = input_min {
        assert_eq!(value, 0, "Minimum pitch bend should be preserved");
    } else {
        panic!("Expected PitchBend variant");
    }

    // Test center pitch bend (8192 = neutral)
    let midi_center = MidiEvent::PitchBend {
        value: 8192,
        channel: 0,
        time,
    };
    let input_center: InputEvent = midi_center.into();
    if let InputEvent::PitchBend { value, .. } = input_center {
        assert_eq!(value, 8192, "Center pitch bend should be preserved");
    } else {
        panic!("Expected PitchBend variant");
    }

    // Test maximum pitch bend (16383 = full up)
    let midi_max = MidiEvent::PitchBend {
        value: 16383,
        channel: 0,
        time,
    };
    let input_max: InputEvent = midi_max.into();
    if let InputEvent::PitchBend { value, .. } = input_max {
        assert_eq!(value, 16383, "Maximum pitch bend should be preserved");
    } else {
        panic!("Expected PitchBend variant");
    }
}

#[test]
fn test_timestamp_accessor() {
    let time = Instant::now();

    let events = vec![
        InputEvent::PadPressed {
            pad: 1,
            velocity: 100,
            channel: None,
            time,
        },
        InputEvent::PadReleased {
            pad: 1,
            channel: None,
            time,
        },
        InputEvent::EncoderTurned {
            encoder: 1,
            value: 64,
            channel: None,
            analog: None,
            time,
        },
        InputEvent::PolyPressure {
            pad: 60,
            pressure: 90,
            channel: None,
            time,
        },
        InputEvent::Aftertouch {
            pressure: 50,
            channel: None,
            time,
        },
        InputEvent::PitchBend {
            value: 8192,
            channel: None,
            time,
        },
        InputEvent::ProgramChange {
            program: 1,
            channel: None,
            time,
        },
        InputEvent::ControlChange {
            control: 7,
            value: 100,
            channel: None,
            time,
        },
    ];

    for event in events {
        assert_eq!(
            event.timestamp(),
            time,
            "Timestamp accessor should return correct time for {:?}",
            event
        );
    }
}

#[test]
fn test_event_type_strings() {
    let time = Instant::now();

    let test_cases = vec![
        (
            InputEvent::PadPressed {
                pad: 1,
                velocity: 100,
                channel: None,
                time,
            },
            "PadPressed",
        ),
        (
            InputEvent::PadReleased {
                pad: 1,
                channel: None,
                time,
            },
            "PadReleased",
        ),
        (
            InputEvent::EncoderTurned {
                encoder: 1,
                value: 64,
                channel: None,
                analog: None,
                time,
            },
            "EncoderTurned",
        ),
        (
            InputEvent::PolyPressure {
                pad: 60,
                pressure: 90,
                channel: None,
                time,
            },
            "PolyPressure",
        ),
        (
            InputEvent::Aftertouch {
                pressure: 50,
                channel: None,
                time,
            },
            "Aftertouch",
        ),
        (
            InputEvent::PitchBend {
                value: 8192,
                channel: None,
                time,
            },
            "PitchBend",
        ),
        (
            InputEvent::ProgramChange {
                program: 1,
                channel: None,
                time,
            },
            "ProgramChange",
        ),
        (
            InputEvent::ControlChange {
                control: 7,
                value: 100,
                channel: None,
                time,
            },
            "ControlChange",
        ),
    ];

    for (event, expected_type) in test_cases {
        assert_eq!(
            event.event_type(),
            expected_type,
            "Event type string should match for {:?}",
            event
        );
    }
}

#[test]
fn test_batch_conversion() {
    // Simulate a sequence of MIDI events being converted
    let base_time = Instant::now();

    let midi_events = vec![
        MidiEvent::NoteOn {
            note: 36,
            velocity: 100,
            channel: 0,
            time: base_time,
        },
        MidiEvent::NoteOn {
            note: 38,
            velocity: 80,
            channel: 0,
            time: base_time,
        },
        MidiEvent::ControlChange {
            cc: 1,
            value: 64,
            channel: 0,
            time: base_time,
        },
        MidiEvent::NoteOff {
            note: 36,
            channel: 0,
            time: base_time,
        },
        MidiEvent::NoteOff {
            note: 38,
            channel: 0,
            time: base_time,
        },
    ];

    let input_events: Vec<InputEvent> = midi_events.into_iter().map(Into::into).collect();

    assert_eq!(input_events.len(), 5, "All events should be converted");

    // Verify first event
    match &input_events[0] {
        InputEvent::PadPressed { pad, velocity, .. } => {
            assert_eq!(*pad, 36);
            assert_eq!(*velocity, 100);
        }
        _ => panic!("Expected PadPressed"),
    }

    // Verify second event
    match &input_events[1] {
        InputEvent::PadPressed { pad, velocity, .. } => {
            assert_eq!(*pad, 38);
            assert_eq!(*velocity, 80);
        }
        _ => panic!("Expected PadPressed"),
    }

    // Verify CC event (v4.10.9: CC → ControlChange, not EncoderTurned)
    match &input_events[2] {
        InputEvent::ControlChange { control, value, .. } => {
            assert_eq!(*control, 1);
            assert_eq!(*value, 64);
        }
        _ => panic!("Expected ControlChange"),
    }

    // Verify releases
    match &input_events[3] {
        InputEvent::PadReleased { pad, .. } => {
            assert_eq!(*pad, 36);
        }
        _ => panic!("Expected PadReleased"),
    }

    match &input_events[4] {
        InputEvent::PadReleased { pad, .. } => {
            assert_eq!(*pad, 38);
        }
        _ => panic!("Expected PadReleased"),
    }
}

#[test]
fn test_protocol_agnostic_field_names() {
    // This test verifies that InputEvent uses protocol-agnostic terminology
    // (pad, encoder, pressure) instead of MIDI-specific terms (note, cc)

    let time = Instant::now();

    // Create a pad press (not "note on")
    let _pad_event = InputEvent::PadPressed {
        pad: 36, // Not "note"
        velocity: 100,
        channel: None,
        time,
    };

    // Create an encoder turn (not "control change")
    let _encoder_event = InputEvent::EncoderTurned {
        encoder: 1, // Not "cc"
        value: 64,
        channel: None,
        analog: None,
        time,
    };

    // Create pressure input (not "channel pressure")
    let _pressure_event = InputEvent::Aftertouch {
        pressure: 80, // Domain terminology
        channel: None,
        time,
    };

    // If this compiles, the field names are protocol-agnostic
}

#[test]
fn test_control_change_variant_exists() {
    // Verify that we have a generic ControlChange variant for unmapped controls
    let time = Instant::now();
    let event = InputEvent::ControlChange {
        control: 7,
        value: 100,
        channel: None,
        time,
    };

    assert_eq!(event.event_type(), "ControlChange");
}
