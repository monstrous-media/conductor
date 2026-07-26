// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! `EngineManager` MIDI-synthesis / processed-event / PC-transition helpers
//! (`trigger_info_from_trigger`, `default_value_for`, `synthesize_midi_bytes`,
//! `is_redundant_processed_event`, `detect_pc_transition`,
//! `emit_processed_event_with_transition`), extracted from
//! `engine_manager::events` (refactor #2073).

use super::*;

impl EngineManager {
    pub(crate) fn trigger_info_from_trigger(
        trigger: &conductor_core::config::types::Trigger,
    ) -> FiredTriggerInfo {
        use conductor_core::config::types::Trigger;
        let ch = trigger.channel();
        match trigger {
            Trigger::Note { note, device, .. } => FiredTriggerInfo {
                trigger_type: "note".to_string(),
                channel: ch,
                device: device.clone(),
                number: Some(*note),
                value: None,
            },
            Trigger::CC { cc, device, .. } => FiredTriggerInfo {
                trigger_type: "cc".to_string(),
                channel: ch,
                device: device.clone(),
                number: Some(*cc),
                value: None,
            },
            Trigger::EncoderTurn { cc, device, .. } => FiredTriggerInfo {
                trigger_type: "encoder".to_string(),
                channel: ch,
                device: device.clone(),
                number: Some(*cc),
                value: None,
            },
            Trigger::Aftertouch { device, .. } => FiredTriggerInfo {
                trigger_type: "aftertouch".to_string(),
                channel: ch,
                device: device.clone(),
                number: None,
                value: None,
            },
            Trigger::PolyAftertouch { note, device, .. } => FiredTriggerInfo {
                trigger_type: "poly_aftertouch".to_string(),
                channel: ch,
                device: device.clone(),
                number: Some(*note),
                value: None,
            },
            Trigger::PitchBend { device, .. } => FiredTriggerInfo {
                trigger_type: "pitch_bend".to_string(),
                channel: ch,
                device: device.clone(),
                number: None,
                value: None,
            },
            Trigger::LongPress { note, device, .. } => FiredTriggerInfo {
                trigger_type: "long_press".to_string(),
                channel: ch,
                device: device.clone(),
                number: Some(*note),
                value: None,
            },
            Trigger::DoubleTap { note, device, .. } => FiredTriggerInfo {
                trigger_type: "double_tap".to_string(),
                channel: ch,
                device: device.clone(),
                number: Some(*note),
                value: None,
            },
            Trigger::NoteChord { notes, device, .. } => FiredTriggerInfo {
                trigger_type: "chord".to_string(),
                channel: ch,
                device: device.clone(),
                number: notes.first().copied(),
                value: Some(notes.len() as u16),
            },
            Trigger::VelocityRange { note, device, .. } => FiredTriggerInfo {
                // Real path: VelocityRange matches PadPressed → "note"
                trigger_type: "note".to_string(),
                channel: ch,
                device: device.clone(),
                number: Some(*note),
                value: None,
            },
            Trigger::GamepadButton { button, device, .. } => FiredTriggerInfo {
                trigger_type: "gamepad_button".to_string(),
                device: device.clone(),
                channel: None,
                number: Some(*button),
                value: None,
            },
            Trigger::GamepadButtonChord {
                buttons, device, ..
            } => FiredTriggerInfo {
                trigger_type: "gamepad_chord".to_string(),
                device: device.clone(),
                channel: None,
                number: buttons.first().copied(),
                value: Some(buttons.len() as u16),
            },
            Trigger::GamepadAnalogStick { axis, device, .. } => FiredTriggerInfo {
                // Real path: GamepadAnalogStick matches EncoderTurned → "encoder"
                trigger_type: "encoder".to_string(),
                device: device.clone(),
                channel: None,
                number: Some(*axis),
                value: None,
            },
            Trigger::GamepadTrigger {
                trigger, device, ..
            } => FiredTriggerInfo {
                // Real path: GamepadTrigger matches EncoderTurned → "encoder"
                trigger_type: "encoder".to_string(),
                device: device.clone(),
                channel: None,
                number: Some(*trigger),
                value: None,
            },
            Trigger::ProgramChange { pc, device, .. } => FiredTriggerInfo {
                trigger_type: "program_change".to_string(),
                channel: ch,
                device: device.clone(),
                number: *pc,
                value: None,
            },
            // OSC triggers (ADR-039-A Slice 2, #2325): no MIDI number/value —
            // the address lives in the trigger config, not in FiredTriggerInfo's
            // numeric fields.
            Trigger::OscMessage { device, .. } => FiredTriggerInfo {
                trigger_type: "osc_message".to_string(),
                channel: None,
                device: device.clone(),
                number: None,
                value: None,
            },
            Trigger::OscAddressPattern { device, .. } => FiredTriggerInfo {
                trigger_type: "osc_pattern".to_string(),
                channel: None,
                device: device.clone(),
                number: None,
                value: None,
            },
            Trigger::OscArgRange { device, .. } => FiredTriggerInfo {
                trigger_type: "osc_arg_range".to_string(),
                channel: None,
                device: device.clone(),
                number: None,
                value: None,
            },
        }
    }
    /// Default velocity/value for a trigger type (for simulate_mapping)
    ///
    /// Returns a sensible default when no real input event is available.
    /// Uses 100 for note-like triggers (medium-hard velocity), 64 for CC/encoder/aftertouch/pitch_bend.
    pub(crate) fn default_value_for(trigger_info: &FiredTriggerInfo) -> Option<u8> {
        match trigger_info.trigger_type.as_str() {
            "note" | "gamepad_button" => Some(100),
            "cc" | "encoder" | "aftertouch" | "poly_aftertouch" => Some(64),
            // pitch_bend returns None — simulate_mapping uses 14-bit center (8192) directly
            _ => None,
        }
    }
    /// Synthesize raw MIDI bytes from a config Trigger for MidiForward support
    ///
    /// Produces a minimal valid MIDI message so that simulated mappings with
    /// MidiForward actions have raw bytes to forward.
    pub(crate) fn synthesize_midi_bytes(
        trigger: &conductor_core::config::types::Trigger,
        value: Option<u8>,
    ) -> Option<Vec<u8>> {
        use conductor_core::config::types::Trigger;
        const PITCH_BEND_MAX_14BIT: u32 = 16383;
        let vel = value.unwrap_or(100);
        match trigger {
            Trigger::Note { note, .. }
            | Trigger::LongPress { note, .. }
            | Trigger::DoubleTap { note, .. }
            | Trigger::VelocityRange { note, .. } => {
                // NoteOn, channel 0: 0x90, note, velocity
                Some(vec![0x90, *note, vel])
            }
            Trigger::CC { cc, .. } => {
                // CC, channel 0: 0xB0, cc, value
                Some(vec![0xB0, *cc, vel])
            }
            Trigger::EncoderTurn { cc, .. } => {
                // CC, channel 0: 0xB0, cc, value
                Some(vec![0xB0, *cc, vel])
            }
            Trigger::Aftertouch { .. } => {
                // Channel Aftertouch, channel 0: 0xD0, pressure
                Some(vec![0xD0, vel])
            }
            Trigger::PitchBend { .. } => {
                // Pitch Bend, channel 0: 0xE0, LSB, MSB
                // Use 14-bit center (8192) when no value override, else map 0-127 → 0-16383
                let pb14 = match value {
                    Some(v) => (v as u32) * PITCH_BEND_MAX_14BIT / 127,
                    None => 8192,
                };
                let lsb = (pb14 & 0x7F) as u8;
                let msb = ((pb14 >> 7) & 0x7F) as u8;
                Some(vec![0xE0, lsb, msb])
            }
            Trigger::NoteChord { notes, .. } => {
                // First note of the chord as NoteOn
                notes.first().map(|note| vec![0x90, *note, vel])
            }
            Trigger::PolyAftertouch { note, channel, .. } => {
                // Polyphonic aftertouch: status 0xA0, note, pressure (#575).
                let ch = channel.unwrap_or(0) & 0x0F;
                Some(vec![0xA0 | ch, *note, vel])
            }
            // Gamepad triggers have no MIDI representation
            _ => None,
        }
    }
    /// Returns true if the processed event is redundant with a raw monitor
    /// event when `capture_midi` is enabled (Issue #589). These processed
    /// variants carry identical data to the raw InputEvent, causing
    /// duplicate rows in the GUI event stream.
    pub(crate) fn is_redundant_processed_event(
        pe: &conductor_core::event_processor::ProcessedEvent,
        capture_midi: bool,
    ) -> bool {
        use conductor_core::event_processor::ProcessedEvent;
        capture_midi
            && matches!(
                pe,
                ProcessedEvent::CCReceived { .. }
                    | ProcessedEvent::PitchBendMoved { .. }
                    | ProcessedEvent::AftertouchChanged { .. }
                    | ProcessedEvent::PolyAftertouchChanged { .. }
            )
    }
    /// Emit a processed event to the monitor buffer (R893 — LongPress/ShortPress visibility)
    /// ADR-025 Phase 3.D — snapshot prior PC state before the store write so
    /// the subsequent emit path can tell whether this PC is a real transition
    /// or a repeat. Returns `Some((prev_pc, new_pc))` when the event is a
    /// `ProgramChange` with a concrete channel and either (a) no PC has ever
    /// been seen on this `(device, channel)` tuple — prev_pc is `None`,
    /// representing "undefined → new_pc" — or (b) the previous PC differs
    /// from the new one.
    ///
    /// Returns `None` for:
    /// - non-PC events
    /// - PC repeats (same value as stored)
    /// - PC events with `channel: None` — `PhysicalControlStateStore::
    ///   observe_input_event` also drops these, so emitting a transition
    ///   annotation for a PC the store didn't actually record would surface
    ///   phantom state changes for channel-less sources.
    pub(crate) fn detect_pc_transition(
        input_event: &InputEvent,
        device_id: &str,
        store: &PhysicalControlStateStore,
    ) -> Option<(Option<u8>, u8)> {
        use conductor_core::control_state::StateKey;
        if let InputEvent::ProgramChange {
            program,
            channel: Some(channel),
            ..
        } = input_event
        {
            let prev = store
                .get(&StateKey::ProgramChange {
                    device: device_id.to_string(),
                    channel: *channel,
                })
                .map(|v| v.value as u8);
            match prev {
                None => Some((None, *program)),
                Some(p) if p != *program => Some((Some(p), *program)),
                _ => None,
            }
        } else {
            None
        }
    }
    pub(crate) fn emit_processed_event_with_transition(
        &self,
        pe: &conductor_core::event_processor::ProcessedEvent,
        device_id: Option<&str>,
        pc_transition: Option<(Option<u8>, u8)>,
    ) {
        use conductor_core::event_processor::ProcessedEvent;

        if Self::is_redundant_processed_event(pe, self.capture_midi) {
            return;
        }

        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let mut event = MonitorEvent {
            timestamp_ms,
            device_id: device_id.map(|d| d.to_string()),
            ..Default::default()
        };
        match pe {
            ProcessedEvent::PadPressed { note, velocity, .. } => {
                event.event_type = "pad_pressed".to_string();
                event.note = Some(*note);
                event.velocity = Some(*velocity);
            }
            ProcessedEvent::PadReleased {
                note,
                hold_duration_ms,
                ..
            } => {
                event.event_type = "pad_released".to_string();
                event.note = Some(*note);
                event.detail = Some(format!("held {}ms", hold_duration_ms));
            }
            ProcessedEvent::ShortPress { note, .. } => {
                event.event_type = "short_press".to_string();
                event.note = Some(*note);
                event.detail = Some(format!(
                    "<{}ms threshold",
                    conductor_core::event_processor::SHORT_PRESS_MS
                ));
            }
            ProcessedEvent::MediumPress {
                note, duration_ms, ..
            } => {
                event.event_type = "medium_press".to_string();
                event.note = Some(*note);
                event.detail = Some(format!(
                    "{}ms (>={}ms & <{}ms)",
                    duration_ms,
                    conductor_core::event_processor::SHORT_PRESS_MS,
                    conductor_core::event_processor::LONG_PRESS_MS
                ));
            }
            ProcessedEvent::LongPress {
                note, duration_ms, ..
            } => {
                event.event_type = "long_press".to_string();
                event.note = Some(*note);
                event.detail = Some(format!(
                    "{}ms (>={}ms threshold)",
                    duration_ms,
                    conductor_core::event_processor::LONG_PRESS_MS
                ));
            }
            ProcessedEvent::HoldDetected {
                note, duration_ms, ..
            } => {
                event.event_type = "hold_detected".to_string();
                event.note = Some(*note);
                event.detail = Some(format!("{}ms", duration_ms));
            }
            ProcessedEvent::DoubleTap {
                note, interval_ms, ..
            } => {
                event.event_type = "double_tap".to_string();
                event.note = Some(*note);
                event.detail = Some(format!("interval {}ms", interval_ms));
            }
            ProcessedEvent::ChordDetected {
                notes, velocities, ..
            } => {
                event.event_type = "chord_detected".to_string();
                // R896: chord timing debugging — show note count, values, velocities
                event.detail = Some(format!(
                    "{}-note chord: {:?}, vel: {:?}",
                    notes.len(),
                    notes,
                    velocities,
                ));
            }
            ProcessedEvent::EncoderTurned {
                cc,
                value,
                direction,
                delta,
                ..
            } => {
                event.event_type = "encoder_turn".to_string();
                event.cc = Some(*cc);
                event.value = Some(*value as u16);
                event.detail = Some(format!("{:?} Δ{}", direction, delta));
            }
            ProcessedEvent::CCReceived { cc, value, .. } => {
                event.event_type = "cc_received".to_string();
                event.cc = Some(*cc);
                event.value = Some(*value as u16);
            }
            ProcessedEvent::AftertouchChanged { pressure, .. } => {
                event.event_type = "aftertouch".to_string();
                event.value = Some(*pressure as u16);
            }
            ProcessedEvent::PolyAftertouchChanged { note, pressure, .. } => {
                event.event_type = "poly_aftertouch".to_string();
                event.note = Some(*note);
                event.value = Some(*pressure as u16);
            }
            ProcessedEvent::PitchBendMoved { value, .. } => {
                event.event_type = "pitch_bend".to_string();
                event.value = Some(*value);
            }
            ProcessedEvent::ProgramChange { program, channel } => {
                event.event_type = "program_change".to_string();
                event.value = Some(u16::from(*program));
                event.channel = *channel;
                // ADR-025 Phase 3.D: annotate genuine PC transitions so the
                // Events panel can render a "via PC X → PC Y" badge. Absent
                // payload == repeat PC (no routing context change).
                if let Some((prev_pc, new_pc)) = pc_transition {
                    event.payload = Some(json!({
                        "context_switch": {
                            "pattern_type": PatternType::ContextSwitch,
                            "prev_pc": prev_pc,
                            "new_pc": new_pc,
                        }
                    }));
                }
            }
            ProcessedEvent::Raw(_) => return, // Skip raw passthrough
            // ADR-039-A Slice 2 (#2325): inbound OSC reaching the mapping
            // engine. The raw gamepad/MIDI monitor stream already shows the
            // datagram via the route path; this is the mapping-engine-visible
            // form.
            ProcessedEvent::OscReceived { address, args } => {
                event.event_type = "osc_received".to_string();
                event.detail = Some(format!("{} ({} args)", address, args.len()));
            }
        }
        // #835: stamp the structured pattern annotation onto the always-on
        // processed-monitor stream so gesture badges (DoubleTap / Chord /
        // LongPress / GamepadChord) render in EVERY mode — normal, manual
        // Learn, and LLM Learn — sourced from the event buffer the panel
        // already reads, NOT the single-consumer Learn buffer that LLM Learn
        // must leave intact. Mirrors exactly the PatternType set + timeout
        // values that `capture_pattern_events` writes to the Learn buffer, so
        // monitor-sourced and Learn-sourced badges are identical. Skipped when
        // a payload is already set (ProgramChange context_switch).
        if event.payload.is_none()
            && let Some(pattern) = processed_event_pattern(pe)
        {
            event.payload = Some(json!({ "pattern": pattern }));
        }
        self.push_monitor_event(event);
    }
}

/// Structured pattern annotation for the always-on processed-monitor stream
/// (#835). Returns `None` for non-gesture events. Mirrors the PatternType set,
/// `pattern_*` field names, and detection-window constants that
/// `capture_pattern_events` writes to the MIDI Learn buffer, so a badge looks
/// the same whether it came from the monitor stream or a polled Learn buffer.
fn processed_event_pattern(
    pe: &conductor_core::event_processor::ProcessedEvent,
) -> Option<serde_json::Value> {
    use conductor_core::event_processor::ProcessedEvent as PE;
    match pe {
        PE::LongPress { duration_ms, .. } => Some(serde_json::json!({
            "pattern_type": PatternType::LongPress,
            "pattern_duration_ms": *duration_ms as u64,
        })),
        PE::DoubleTap { .. } => Some(serde_json::json!({
            "pattern_type": PatternType::DoubleTap,
            "pattern_timeout_ms": 300,
        })),
        PE::ChordDetected { notes, .. } => {
            if notes.iter().any(|n| *n >= 128) {
                Some(serde_json::json!({
                    "pattern_type": PatternType::GamepadChord,
                    "pattern_buttons": notes,
                    "pattern_timeout_ms": 50,
                }))
            } else {
                Some(serde_json::json!({
                    "pattern_type": PatternType::Chord,
                    "pattern_notes": notes,
                    "pattern_timeout_ms": 100,
                }))
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod pattern_annotation_tests {
    use super::processed_event_pattern;
    use conductor_core::event_processor::ProcessedEvent;

    #[test]
    fn double_tap_annotation() {
        let pe = ProcessedEvent::DoubleTap {
            note: 65,
            first_velocity: 100,
            second_velocity: 100,
            interval_ms: 280,
            channel: Some(0),
        };
        let p = processed_event_pattern(&pe).expect("double tap → pattern");
        assert_eq!(p["pattern_type"].as_str(), Some("double_tap"));
        assert_eq!(p["pattern_timeout_ms"].as_u64(), Some(300));
    }

    #[test]
    fn chord_annotation_carries_notes() {
        let pe = ProcessedEvent::ChordDetected {
            notes: vec![60, 64, 67],
            velocities: vec![100, 100, 100],
            channel: Some(0),
        };
        let p = processed_event_pattern(&pe).expect("chord → pattern");
        assert_eq!(p["pattern_type"].as_str(), Some("chord"));
        assert_eq!(p["pattern_notes"], serde_json::json!([60, 64, 67]));
    }

    #[test]
    fn gamepad_chord_uses_buttons_not_notes() {
        let pe = ProcessedEvent::ChordDetected {
            notes: vec![128, 130],
            velocities: vec![1, 1],
            channel: None,
        };
        let p = processed_event_pattern(&pe).expect("gamepad chord → pattern");
        assert_eq!(p["pattern_type"].as_str(), Some("gamepad_chord"));
        assert_eq!(p["pattern_buttons"], serde_json::json!([128, 130]));
    }

    #[test]
    fn long_press_carries_duration() {
        let pe = ProcessedEvent::LongPress {
            note: 40,
            duration_ms: 2200,
            channel: Some(0),
        };
        let p = processed_event_pattern(&pe).expect("long press → pattern");
        assert_eq!(p["pattern_type"].as_str(), Some("long_press"));
        assert_eq!(p["pattern_duration_ms"].as_u64(), Some(2200));
    }

    #[test]
    fn non_gesture_events_have_no_pattern() {
        let pe = ProcessedEvent::ShortPress {
            note: 40,
            channel: Some(0),
        };
        assert!(processed_event_pattern(&pe).is_none());
    }
}
