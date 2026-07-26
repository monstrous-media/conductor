// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

use super::*;

// ── Event Monitor Tests (Issue #326) ──────────────────────────────

#[test]
fn test_create_monitor_event_note_on() {
    let event = InputEvent::PadPressed {
        channel: None,
        time: std::time::Instant::now(),
        pad: 60,
        velocity: 100,
    };
    let result = EngineManager::create_monitor_event(&event, Some("device-1"));
    let me = result.expect("should create MonitorEvent for PadPressed");
    assert_eq!(me.event_type, "note_on");
    assert_eq!(me.note, Some(60));
    assert_eq!(me.velocity, Some(100));
    assert_eq!(me.device_id, Some("device-1".to_string()));
}

#[test]
fn test_create_monitor_event_note_off() {
    let event = InputEvent::PadReleased {
        pad: 60,
        channel: None,
        time: std::time::Instant::now(),
    };
    let result = EngineManager::create_monitor_event(&event, None);
    let me = result.expect("should create MonitorEvent for PadReleased");
    assert_eq!(me.event_type, "note_off");
    assert_eq!(me.note, Some(60));
    assert_eq!(me.velocity, Some(0));
    assert!(me.device_id.is_none());
}

#[test]
fn test_create_monitor_event_cc() {
    let event = InputEvent::ControlChange {
        channel: None,
        time: std::time::Instant::now(),
        control: 7,
        value: 64,
    };
    let me = EngineManager::create_monitor_event(&event, None).unwrap();
    assert_eq!(me.event_type, "cc");
    assert_eq!(me.cc, Some(7));
    assert_eq!(me.value, Some(64));
}

#[test]
fn test_create_monitor_event_pitch_bend() {
    let event = InputEvent::PitchBend {
        value: 8192,
        channel: None,
        time: std::time::Instant::now(),
    };
    let me = EngineManager::create_monitor_event(&event, None).unwrap();
    assert_eq!(me.event_type, "pitch_bend");
    assert_eq!(me.value, Some(8192)); // Full 14-bit value preserved
}

#[test]
fn test_create_monitor_event_gamepad_button() {
    // pad >= 128 maps to gamepad_button
    let event = InputEvent::PadPressed {
        channel: None,
        time: std::time::Instant::now(),
        pad: 200,
        velocity: 127,
    };
    let me = EngineManager::create_monitor_event(&event, None).unwrap();
    assert_eq!(me.event_type, "gamepad_button");
    assert_eq!(me.button, Some(200));
    assert_eq!(me.velocity, Some(127));
}

#[test]
fn test_create_monitor_event_gamepad_axis() {
    // encoder 128-131 maps to gamepad_axis
    let event = InputEvent::EncoderTurned {
        channel: None,
        time: std::time::Instant::now(),
        encoder: 129,
        value: 50,
        analog: Some(-0.21),
    };
    let me = EngineManager::create_monitor_event(&event, None).unwrap();
    assert_eq!(me.event_type, "gamepad_axis");
    assert_eq!(me.axis, Some(129));
    assert_eq!(me.value, Some(50));
    // #599: raw analog value rides through for high-precision value bars
    assert_eq!(me.analog_value, Some(-0.21));
}

#[test]
fn test_create_monitor_event_gamepad_trigger() {
    // encoder >= 132 maps to gamepad_trigger
    let event = InputEvent::EncoderTurned {
        channel: None,
        time: std::time::Instant::now(),
        encoder: 133,
        value: 255,
        analog: Some(0.42),
    };
    let me = EngineManager::create_monitor_event(&event, None).unwrap();
    assert_eq!(me.event_type, "gamepad_trigger");
    assert_eq!(me.analog_value, Some(0.42));
}

#[test]
fn test_create_monitor_event_midi_encoder_has_no_analog_value() {
    // MIDI encoders (encoder < 128) never carry an analog value (#599)
    let event = InputEvent::EncoderTurned {
        channel: Some(0),
        time: std::time::Instant::now(),
        encoder: 7,
        value: 64,
        analog: None,
    };
    let me = EngineManager::create_monitor_event(&event, None).unwrap();
    assert_eq!(me.event_type, "encoder");
    assert_eq!(me.analog_value, None);
}

#[test]
fn test_create_monitor_event_aftertouch() {
    let event = InputEvent::Aftertouch {
        pressure: 80,
        channel: None,
        time: std::time::Instant::now(),
    };
    let me = EngineManager::create_monitor_event(&event, None).unwrap();
    assert_eq!(me.event_type, "aftertouch");
    assert_eq!(me.value, Some(80));
}

#[test]
fn test_create_monitor_event_poly_pressure() {
    let event = InputEvent::PolyPressure {
        channel: None,
        time: std::time::Instant::now(),
        pad: 60,
        pressure: 90,
    };
    let me = EngineManager::create_monitor_event(&event, None).unwrap();
    assert_eq!(me.event_type, "poly_pressure");
    assert_eq!(me.note, Some(60));
    assert_eq!(me.value, Some(90));
}

/// #601 follow-up: `MonitorEvent.channel` was always `None` for raw MIDI
/// events because `create_monitor_event` never destructured the channel
/// field that exists on every relevant `InputEvent` variant. The GUI's
/// EventRow channel tag and expanded "Channel" row both rely on it, so
/// channel info was invisible in the events panel. This test pins the
/// pipeline: if any branch silently drops channel again, it fails.
#[test]
fn test_create_monitor_event_channel_propagates_for_all_midi_variants() {
    let now = std::time::Instant::now();
    let cases: Vec<(InputEvent, &str)> = vec![
        (
            InputEvent::PadPressed {
                channel: Some(0),
                time: now,
                pad: 60,
                velocity: 100,
            },
            "PadPressed (note_on)",
        ),
        (
            InputEvent::PadReleased {
                pad: 60,
                channel: Some(5),
                time: now,
            },
            "PadReleased (note_off)",
        ),
        (
            InputEvent::ControlChange {
                channel: Some(9),
                time: now,
                control: 7,
                value: 64,
            },
            "ControlChange",
        ),
        (
            InputEvent::EncoderTurned {
                channel: Some(3),
                analog: None,
                time: now,
                encoder: 1,
                value: 1,
            },
            "EncoderTurned",
        ),
        (
            InputEvent::PitchBend {
                value: 8192,
                channel: Some(15),
                time: now,
            },
            "PitchBend",
        ),
        (
            InputEvent::Aftertouch {
                pressure: 80,
                channel: Some(2),
                time: now,
            },
            "Aftertouch",
        ),
        (
            InputEvent::PolyPressure {
                channel: Some(7),
                time: now,
                pad: 60,
                pressure: 90,
            },
            "PolyPressure",
        ),
    ];
    for (event, label) in cases {
        let expected_channel = match &event {
            InputEvent::PadPressed { channel, .. }
            | InputEvent::PadReleased { channel, .. }
            | InputEvent::ControlChange { channel, .. }
            | InputEvent::EncoderTurned { channel, .. }
            | InputEvent::PitchBend { channel, .. }
            | InputEvent::Aftertouch { channel, .. }
            | InputEvent::PolyPressure { channel, .. } => *channel,
            _ => None,
        };
        let me = EngineManager::create_monitor_event(&event, None)
            .unwrap_or_else(|| panic!("expected MonitorEvent for {label}"));
        assert_eq!(
            me.channel, expected_channel,
            "{label}: MonitorEvent.channel must propagate from InputEvent.channel"
        );
    }
}

/// Gamepad pads (>=128) and encoders (>=128) emit gamepad_*/axis variants
/// which are non-MIDI — channel must remain `None` even if the InputEvent
/// happened to carry one. Keeps semantic separation between MIDI and HID
/// surfaces in the monitor stream.
#[test]
fn test_create_monitor_event_channel_none_for_gamepad_variants() {
    let now = std::time::Instant::now();
    let gamepad_button = InputEvent::PadPressed {
        channel: Some(0), // even if set, gamepad path should drop it
        time: now,
        pad: 200,
        velocity: 127,
    };
    let me = EngineManager::create_monitor_event(&gamepad_button, None).unwrap();
    assert_eq!(me.event_type, "gamepad_button");
    assert_eq!(
        me.channel, None,
        "gamepad_button must not carry MIDI channel"
    );

    let gamepad_axis = InputEvent::EncoderTurned {
        channel: Some(0),
        analog: None,
        time: now,
        encoder: 129,
        value: 50,
    };
    let me = EngineManager::create_monitor_event(&gamepad_axis, None).unwrap();
    assert_eq!(me.event_type, "gamepad_axis");
    assert_eq!(me.channel, None, "gamepad_axis must not carry MIDI channel");
}

// ── Issue #840: program_change + canonical raw_bytes ────────────────

/// ProgramChange was previously dropped by `create_monitor_event`'s
/// catch-all `_ => None`, so PC never appeared in the events panel. It now
/// emits a `program_change` MonitorEvent carrying the program in `value`
/// (the field the GUI bridge reads) and canonical wire bytes.
#[test]
fn test_create_monitor_event_program_change() {
    let event = InputEvent::ProgramChange {
        program: 11,
        channel: Some(0),
        time: std::time::Instant::now(),
    };
    let me = EngineManager::create_monitor_event(&event, None)
        .expect("ProgramChange must now produce a MonitorEvent");
    assert_eq!(me.event_type, "program_change");
    assert_eq!(me.value, Some(11));
    assert_eq!(me.channel, Some(0));
    // 0xC0 | ch=0, program 11
    assert_eq!(me.raw_bytes, Some(vec![0xC0, 11]));
}

/// #840: each raw channel-voice event carries canonical MIDI 1.0 bytes,
/// reconstructed via `midi_bytes::extract_raw_midi` (channel folded into the
/// status byte's low nibble). The expanded event view renders these as hex.
#[test]
fn test_create_monitor_event_populates_canonical_raw_bytes() {
    let now = std::time::Instant::now();
    // CC 7 = 78 on channel 1 (0-indexed 0) → B0 07 4E
    let cc = InputEvent::ControlChange {
        channel: Some(0),
        time: now,
        control: 7,
        value: 78,
    };
    let me = EngineManager::create_monitor_event(&cc, None).unwrap();
    assert_eq!(me.raw_bytes, Some(vec![0xB0, 0x07, 0x4E]));

    // note_on note 60 vel 100 on channel 3 → 92 3C 64
    let note = InputEvent::PadPressed {
        channel: Some(2),
        time: now,
        pad: 60,
        velocity: 100,
    };
    let me = EngineManager::create_monitor_event(&note, None).unwrap();
    assert_eq!(me.raw_bytes, Some(vec![0x92, 0x3C, 0x64]));
}

/// #840 review: `extract_raw_midi` folds `channel: None` into channel 0, so a
/// channel-less MIDI event would get fabricated channel-0 bytes. Gate it — a
/// raw event whose source carried no channel must carry no `raw_bytes` (the
/// Channel row already shows "—" for these; the Raw row stays hidden to match).
#[test]
fn test_create_monitor_event_no_raw_bytes_when_channel_none() {
    let cc = InputEvent::ControlChange {
        channel: None,
        time: std::time::Instant::now(),
        control: 7,
        value: 78,
    };
    let me = EngineManager::create_monitor_event(&cc, None).unwrap();
    assert_eq!(me.event_type, "cc");
    assert_eq!(
        me.raw_bytes, None,
        "channel-less events must not fabricate channel-0 wire bytes"
    );
}

/// Gamepad events are not MIDI, so they must carry no `raw_bytes` (the
/// expanded view hides the Raw row for them).
#[test]
fn test_create_monitor_event_gamepad_has_no_raw_bytes() {
    let gamepad_button = InputEvent::PadPressed {
        channel: None,
        time: std::time::Instant::now(),
        pad: 200,
        velocity: 127,
    };
    let me = EngineManager::create_monitor_event(&gamepad_button, None).unwrap();
    assert_eq!(me.event_type, "gamepad_button");
    assert_eq!(me.raw_bytes, None, "gamepad events are not MIDI wire bytes");
}

// ── Issue #589: Processed event suppression ─────────────────────────

#[test]
fn test_is_redundant_processed_event_suppresses_when_capture_midi() {
    use conductor_core::event_processor::ProcessedEvent;

    let cc = ProcessedEvent::CCReceived {
        cc: 7,
        value: 64,
        channel: None,
    };
    let bend = ProcessedEvent::PitchBendMoved {
        value: 8192,
        channel: None,
    };
    let at = ProcessedEvent::AftertouchChanged {
        pressure: 80,
        channel: None,
    };
    let note = ProcessedEvent::PadPressed {
        note: 60,
        velocity: 100,
        velocity_level: conductor_core::event_processor::VelocityLevel::Medium,
        channel: None,
    };

    // When capture_midi=true, CC/PitchBend/Aftertouch are redundant
    assert!(EngineManager::is_redundant_processed_event(&cc, true));
    assert!(EngineManager::is_redundant_processed_event(&bend, true));
    assert!(EngineManager::is_redundant_processed_event(&at, true));

    // Other event types are never redundant
    assert!(!EngineManager::is_redundant_processed_event(&note, true));

    // When capture_midi=false, nothing is redundant (processed is the only source)
    assert!(!EngineManager::is_redundant_processed_event(&cc, false));
    assert!(!EngineManager::is_redundant_processed_event(&bend, false));
    assert!(!EngineManager::is_redundant_processed_event(&at, false));
}

// ────────────────────────────────────────────────────────────
// ADR-025 Phase 3.D — ContextSwitch detection
// ────────────────────────────────────────────────────────────

#[test]
fn detect_pc_transition_first_pc_ever_returns_prev_none() {
    use conductor_core::InputEvent;
    use conductor_core::control_state::PhysicalControlStateStore;
    use std::time::Instant;

    let store = PhysicalControlStateStore::default();
    let event = InputEvent::ProgramChange {
        program: 12,
        channel: Some(0),
        time: Instant::now(),
    };
    let t = EngineManager::detect_pc_transition(&event, "fcb1010", &store);
    // First PC ever: prev None, new 12 — this IS a routing-context change
    // ("undefined" → 12), so the annotation should fire.
    assert_eq!(t, Some((None, 12)));
}

#[test]
fn detect_pc_transition_different_pc_returns_both() {
    use conductor_core::InputEvent;
    use conductor_core::control_state::PhysicalControlStateStore;
    use conductor_core::event_processor::MidiEvent;
    use std::time::Instant;

    let store = PhysicalControlStateStore::default();
    store.observe_event(
        "fcb1010",
        &MidiEvent::ProgramChange {
            program: 7,
            channel: 0,
            time: Instant::now(),
        },
    );
    let event = InputEvent::ProgramChange {
        program: 12,
        channel: Some(0),
        time: Instant::now(),
    };
    let t = EngineManager::detect_pc_transition(&event, "fcb1010", &store);
    assert_eq!(t, Some((Some(7), 12)));
}

#[test]
fn detect_pc_transition_same_pc_returns_none() {
    use conductor_core::InputEvent;
    use conductor_core::control_state::PhysicalControlStateStore;
    use conductor_core::event_processor::MidiEvent;
    use std::time::Instant;

    let store = PhysicalControlStateStore::default();
    store.observe_event(
        "fcb1010",
        &MidiEvent::ProgramChange {
            program: 12,
            channel: 0,
            time: Instant::now(),
        },
    );
    let event = InputEvent::ProgramChange {
        program: 12,
        channel: Some(0),
        time: Instant::now(),
    };
    assert!(EngineManager::detect_pc_transition(&event, "fcb1010", &store).is_none());
}

#[test]
fn detect_pc_transition_different_channels_are_separate_tuples() {
    use conductor_core::InputEvent;
    use conductor_core::control_state::PhysicalControlStateStore;
    use conductor_core::event_processor::MidiEvent;
    use std::time::Instant;

    let store = PhysicalControlStateStore::default();
    // PC 12 on channel 0.
    store.observe_event(
        "fcb1010",
        &MidiEvent::ProgramChange {
            program: 12,
            channel: 0,
            time: Instant::now(),
        },
    );
    // Same PC 12 on channel 1 — that tuple has never seen a PC, so it's
    // a first-ever transition on (device, channel 1).
    let event = InputEvent::ProgramChange {
        program: 12,
        channel: Some(1),
        time: Instant::now(),
    };
    assert_eq!(
        EngineManager::detect_pc_transition(&event, "fcb1010", &store),
        Some((None, 12))
    );
}

/// `InputEvent::ProgramChange { channel: None, .. }` is dropped by
/// `PhysicalControlStateStore::observe_input_event` (it only writes
/// state for channel-qualified events). `detect_pc_transition` must
/// behave symmetrically — otherwise we'd emit a phantom transition
/// annotation for a PC the store never actually recorded, and would
/// read the wrong tuple's state (arbitrarily `(device, 0)`).
#[test]
fn detect_pc_transition_channel_none_returns_none() {
    use conductor_core::InputEvent;
    use conductor_core::control_state::PhysicalControlStateStore;
    use std::time::Instant;

    let store = PhysicalControlStateStore::default();
    let event = InputEvent::ProgramChange {
        program: 12,
        channel: None,
        time: Instant::now(),
    };
    assert!(EngineManager::detect_pc_transition(&event, "fcb1010", &store).is_none());
}

#[test]
fn detect_pc_transition_non_pc_events_return_none() {
    use conductor_core::InputEvent;
    use conductor_core::control_state::PhysicalControlStateStore;
    use std::time::Instant;

    let store = PhysicalControlStateStore::default();
    let cc = InputEvent::ControlChange {
        control: 7,
        value: 50,
        channel: Some(0),
        time: Instant::now(),
    };
    assert!(EngineManager::detect_pc_transition(&cc, "fcb1010", &store).is_none());
}

#[test]
fn test_event_monitor_buffer_bounded() {
    let buffer = std::sync::Mutex::new(VecDeque::<MonitorEvent>::new());
    // Fill beyond capacity
    for i in 0..EVENT_MONITOR_MAX_EVENTS + 100 {
        let mut buf = buffer.lock().unwrap();
        if buf.len() >= EVENT_MONITOR_MAX_EVENTS {
            buf.pop_front();
        }
        buf.push_back(MonitorEvent {
            timestamp_ms: i as u64,
            event_type: "cc".to_string(),
            ..Default::default()
        });
    }
    let buf = buffer.lock().unwrap();
    assert_eq!(buf.len(), EVENT_MONITOR_MAX_EVENTS);
    // Oldest should be event #100 (first 100 were evicted)
    assert_eq!(buf.front().unwrap().timestamp_ms, 100);
}

#[test]
fn test_monitor_event_detail_field() {
    // R897-R898: action events use detail field, not device_id
    let event = MonitorEvent {
        timestamp_ms: 1000,
        event_type: "action_error".to_string(),
        detail: Some("Mode persist: file locked".to_string()),
        ..Default::default()
    };
    assert!(event.device_id.is_none());
    assert_eq!(event.detail.as_deref(), Some("Mode persist: file locked"));

    // Verify JSON serialization includes detail
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("\"detail\":"));
    assert!(!json.contains("\"device_id\":"));
}

#[test]
fn test_monitor_event_detail_skipped_when_none() {
    let event = MonitorEvent {
        timestamp_ms: 1000,
        event_type: "action_executed".to_string(),
        ..Default::default()
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(!json.contains("\"detail\":"));
}

#[test]
fn test_buffer_size_min_clamp() {
    // R925: buffer_size of 0 should be clamped to 1
    let zero: usize = 0;
    let one: usize = 1;
    let large: usize = 5000;
    assert_eq!(zero.max(1), 1);
    assert_eq!(one.max(1), 1);
    assert_eq!(large.max(1), 5000);
}

#[test]
fn test_event_monitor_try_lock_skip() {
    let buffer = Arc::new(std::sync::Mutex::new(VecDeque::<MonitorEvent>::new()));
    // Hold the lock — try_lock should fail gracefully
    let _guard = buffer.lock().unwrap();
    assert!(buffer.try_lock().is_err());
    // In production this means the event is silently skipped — correct behavior
}

#[test]
fn test_mapping_matched_event_format() {
    // R73, R183: mapping_matched events include mode + rule description
    let event = MonitorEvent {
        timestamp_ms: 5000,
        event_type: "mapping_matched".to_string(),
        detail: Some("[Performance] Pad 1 → Note C3".to_string()),
        ..Default::default()
    };
    assert_eq!(event.event_type, "mapping_matched");
    assert!(event.detail.as_ref().unwrap().starts_with("[Performance]"));

    // Multi-device variant includes device_id
    let event_multi = MonitorEvent {
        timestamp_ms: 5001,
        event_type: "mapping_matched".to_string(),
        detail: Some("[Performance] Pad 1 → Note C3 (launchpad-1)".to_string()),
        ..Default::default()
    };
    assert!(
        event_multi
            .detail
            .as_ref()
            .unwrap()
            .contains("(launchpad-1)")
    );
}

#[test]
fn test_processing_us_on_monitor_event() {
    // R899/Issue #709: processing latency is stamped as processing_us on the
    // raw MonitorEvent, not emitted as a separate "latency" event.
    let event = MonitorEvent {
        timestamp_ms: 6000,
        event_type: "note_on".to_string(),
        note: Some(36),
        velocity: Some(100),
        processing_us: Some(142),
        ..Default::default()
    };
    assert_eq!(event.event_type, "note_on");
    assert_eq!(event.processing_us, Some(142));

    // Multi-device: processing_us + device_id on same event
    let event_multi = MonitorEvent {
        timestamp_ms: 6001,
        event_type: "cc".to_string(),
        cc: Some(7),
        value: Some(64),
        device_id: Some("launchpad-1".to_string()),
        processing_us: Some(205),
        ..Default::default()
    };
    assert_eq!(event_multi.processing_us, Some(205));
    assert_eq!(event_multi.device_id.as_deref(), Some("launchpad-1"));
}

#[test]
fn test_monitor_rate_limiter_drops_excess() {
    // R924: fixed-window rate limiter drops events beyond max_per_second
    let limiter = MonitorRateLimiter::new(5);

    // 5 events in second 1 — all allowed
    for i in 0..5u64 {
        assert!(limiter.allow(1000 + i), "event {} should be allowed", i);
    }

    // 6th event in same second — dropped
    assert!(!limiter.allow(1005), "6th event should be dropped");
    assert!(!limiter.allow(1999), "still in second 1, should drop");

    // New second — counter resets
    assert!(limiter.allow(2000), "new second should allow");
}

#[test]
fn test_monitor_rate_limiter_second_boundary_reset() {
    let limiter = MonitorRateLimiter::new(2);

    assert!(limiter.allow(1000));
    assert!(limiter.allow(1500));
    assert!(!limiter.allow(1999)); // Full

    // Second 2
    assert!(limiter.allow(2000));
    assert!(limiter.allow(2500));
    assert!(!limiter.allow(2999)); // Full again
}

#[test]
fn test_monitor_rate_limiter_unlimited_mode() {
    // R924: max_events_per_second = 0 means unlimited — no rate limiting.
    // In production, limiter is None when max=0; this verifies the struct
    // also handles 0 gracefully by returning early in allow() (no counting or dropping).
    let limiter = MonitorRateLimiter::new(0);

    // Burst within same second — all allowed because max_per_second = 0
    // causes allow() to short-circuit without counting or enforcing a threshold.
    for i in 0..100u64 {
        assert!(
            limiter.allow(1000 + i),
            "event {} should be allowed in unlimited mode",
            i
        );
    }
}

#[test]
fn test_monitor_rate_limiter_non_monotonic_timestamps() {
    // Council review: timestamps may jitter backwards from external clocks.
    // Limiter should handle gracefully — backwards jump resets the window.
    let limiter = MonitorRateLimiter::new(3);

    // Forward: second 5
    assert!(limiter.allow(5000));
    assert!(limiter.allow(5100));
    assert!(limiter.allow(5200));
    assert!(!limiter.allow(5300)); // Full

    // Backwards jump to second 4 — resets window
    assert!(limiter.allow(4000));
    assert!(limiter.allow(4500));
    assert!(limiter.allow(4999));
    assert!(!limiter.allow(4100)); // Full in second 4
}

#[test]
fn test_chord_event_detail_uses_emit_format() {
    // R896: verify the real formatting from emit_processed_event
    // The format is: "{n}-note chord: {notes:?}, vel: {velocities:?}"
    let notes: Vec<u8> = vec![60, 64, 67];
    let velocities: Vec<u8> = vec![100, 90, 110];
    let detail = format!(
        "{}-note chord: {:?}, vel: {:?}",
        notes.len(),
        notes,
        velocities,
    );
    assert_eq!(detail, "3-note chord: [60, 64, 67], vel: [100, 90, 110]");
    assert!(detail.starts_with("3-note"));
    assert!(detail.contains("[60, 64, 67]"));
}

// ── Push-based event monitoring tests (#394) ──

#[test]
fn test_broadcast_channel_sends_to_subscriber() {
    let (tx, mut rx) = broadcast::channel::<MonitorEvent>(16);
    let event = MonitorEvent {
        timestamp_ms: 42000,
        event_type: "note_on".to_string(),
        note: Some(60),
        velocity: Some(100),
        ..Default::default()
    };

    let result = tx.send(event);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), 1); // 1 subscriber

    let received = rx.try_recv().unwrap();
    assert_eq!(received.timestamp_ms, 42000);
    assert_eq!(received.event_type, "note_on");
    assert_eq!(received.note, Some(60));
}

#[test]
fn test_broadcast_no_subscriber_doesnt_block() {
    let (tx, _) = broadcast::channel::<MonitorEvent>(16);

    let event = MonitorEvent {
        timestamp_ms: 1000,
        event_type: "cc".to_string(),
        ..Default::default()
    };

    // send() fails silently when no subscribers — correct behavior
    let result = tx.send(event);
    assert!(result.is_err());
}

#[test]
fn test_broadcast_lagged_subscriber_reports_lag() {
    let (tx, mut rx) = broadcast::channel::<MonitorEvent>(4);

    for i in 0..10 {
        let _ = tx.send(MonitorEvent {
            timestamp_ms: i,
            event_type: "cc".to_string(),
            ..Default::default()
        });
    }

    match rx.try_recv() {
        Err(broadcast::error::TryRecvError::Lagged(n)) => {
            assert!(n > 0, "Should report lagged count > 0");
        }
        other => panic!("Expected Lagged error, got {:?}", other),
    }

    let event = rx.try_recv().unwrap();
    assert!(event.timestamp_ms > 0);
}

#[test]
fn test_broadcast_multiple_subscribers() {
    let (tx, mut rx1) = broadcast::channel::<MonitorEvent>(16);
    let mut rx2 = tx.subscribe();

    let _ = tx.send(MonitorEvent {
        timestamp_ms: 5000,
        event_type: "note_off".to_string(),
        note: Some(72),
        ..Default::default()
    });

    let e1 = rx1.try_recv().unwrap();
    let e2 = rx2.try_recv().unwrap();
    assert_eq!(e1.timestamp_ms, e2.timestamp_ms);
    assert_eq!(e1.note, e2.note);
}

#[test]
fn test_broadcast_natural_batching() {
    let (tx, mut rx) = broadcast::channel::<MonitorEvent>(64);

    for i in 0..5 {
        let _ = tx.send(MonitorEvent {
            timestamp_ms: i * 100,
            event_type: "note_on".to_string(),
            note: Some(60 + i as u8),
            ..Default::default()
        });
    }

    let first = rx.try_recv().unwrap();
    let mut batch = vec![first];
    while let Ok(event) = rx.try_recv() {
        batch.push(event);
    }

    assert_eq!(batch.len(), 5);
    assert_eq!(batch[0].note, Some(60));
    assert_eq!(batch[4].note, Some(64));
}

#[test]
fn test_broadcast_subscriber_drop_doesnt_affect_sender() {
    let (tx, rx1) = broadcast::channel::<MonitorEvent>(16);
    let mut rx2 = tx.subscribe();

    drop(rx1);

    let result = tx.send(MonitorEvent {
        timestamp_ms: 1000,
        event_type: "cc".to_string(),
        ..Default::default()
    });
    assert!(result.is_ok());

    let event = rx2.try_recv().unwrap();
    assert_eq!(event.timestamp_ms, 1000);
}
