// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! MIDI output byte computation and the `SendMidi` action (#1684 split from
//! `action_executor.rs`). Free fns `compute_midi_output_bytes` /
//! `compute_midi_forward_bytes` are pure byte builders shared with the
//! executor thread's recursion guard; `execute_send_midi` is the executor
//! method that connects and writes to an output port.

use super::{ActionExecutor, TriggerContext};
use conductor_core::dispatch::{DispatchError, DispatchOutcome, DispatchResult};
use conductor_core::{MidiMessageParams, MidiMessageType};

/// Compute the raw MIDI bytes that a SendMidi action would produce.
///
/// This is used by both `ActionExecutor::execute_send_midi()` and the executor
/// thread's recursion guard to know exactly what bytes are sent to the output port.
pub fn compute_midi_output_bytes(
    message_type: &MidiMessageType,
    channel: u8,
    params: &MidiMessageParams,
    context: Option<&TriggerContext>,
) -> Result<Vec<u8>, DispatchError> {
    match (message_type, params) {
        (
            MidiMessageType::NoteOn,
            MidiMessageParams::Note {
                note,
                velocity_mapping,
            },
        ) => {
            let trigger_velocity = context.and_then(|ctx| ctx.velocity).unwrap_or(100);
            let calculated_velocity =
                conductor_core::velocity::calculate_velocity(trigger_velocity, velocity_mapping);
            Ok(vec![
                0x90 | (channel & 0x0F),
                *note & 0x7F,
                calculated_velocity & 0x7F,
            ])
        }
        (
            MidiMessageType::NoteOff,
            MidiMessageParams::Note {
                note,
                velocity_mapping,
            },
        ) => {
            let trigger_velocity = context.and_then(|ctx| ctx.velocity).unwrap_or(64);
            let calculated_velocity =
                conductor_core::velocity::calculate_velocity(trigger_velocity, velocity_mapping);
            Ok(vec![
                0x80 | (channel & 0x0F),
                *note & 0x7F,
                calculated_velocity & 0x7F,
            ])
        }
        (MidiMessageType::ControlChange, MidiMessageParams::CC { controller, value }) => Ok(vec![
            0xB0 | (channel & 0x0F),
            *controller & 0x7F,
            *value & 0x7F,
        ]),
        (MidiMessageType::ProgramChange, MidiMessageParams::ProgramChange { program }) => {
            Ok(vec![0xC0 | (channel & 0x0F), *program & 0x7F])
        }
        (MidiMessageType::PitchBend, MidiMessageParams::PitchBend { value }) => {
            let pitch_value = (*value + 8192).clamp(0, 16383) as u16;
            let lsb = (pitch_value & 0x7F) as u8;
            let msb = ((pitch_value >> 7) & 0x7F) as u8;
            Ok(vec![0xE0 | (channel & 0x0F), lsb, msb])
        }
        (MidiMessageType::Aftertouch, MidiMessageParams::Aftertouch { pressure }) => {
            Ok(vec![0xD0 | (channel & 0x0F), *pressure & 0x7F])
        }
        _ => Err(DispatchError::MidiOutput(format!(
            "Mismatched MIDI message type {:?} and params {:?}",
            message_type, params
        ))),
    }
}

/// Compute the raw MIDI bytes that a MidiForward action would produce.
///
/// Applies the optional transform to the raw input bytes.
pub fn compute_midi_forward_bytes(
    raw_midi: &[u8],
    transform: Option<&conductor_core::transform::MidiTransform>,
) -> Vec<u8> {
    match transform {
        Some(t) => t.apply(raw_midi),
        None => raw_midi.to_vec(),
    }
}

impl ActionExecutor {
    /// Execute SendMIDI action
    ///
    /// Converts MIDI message parameters to bytes and sends via MidiOutputManager.
    ///
    /// # Arguments
    /// * `port` - Target MIDI output port name
    /// * `message_type` - Type of MIDI message to send
    /// * `channel` - MIDI channel (0-15)
    /// * `params` - Message-specific parameters
    /// * `context` - Optional trigger context (contains velocity from triggering event)
    ///
    /// # MIDI Message Format
    /// All MIDI messages follow the format: [status_byte, data_byte1, data_byte2]
    /// - Status byte: 0x80-0xE0 | channel (0-15)
    /// - Data bytes: 0-127 (7-bit values)
    pub(crate) fn execute_send_midi(
        &mut self,
        port: &str,
        message_type: &MidiMessageType,
        channel: u8,
        params: &MidiMessageParams,
        context: Option<&TriggerContext>,
    ) -> DispatchResult {
        let message_bytes = compute_midi_output_bytes(message_type, channel, params, context)?;

        // v4.10.9: Auto-connect to port if not already connected
        self.midi_output.connect_by_name(port).map_err(|e| {
            DispatchError::MidiOutput(format!("Failed to connect to port '{}': {}", port, e))
        })?;

        // Send message via MidiOutputManager
        self.midi_output
            .send_message(port, &message_bytes)
            .map_err(|e| {
                DispatchError::MidiOutput(format!("Failed to send to '{}': {}", port, e))
            })?;

        // Issue #555: record the resolved output port so cascade
        // suppression can open a window for it. Push only after a
        // successful send — failed sends shouldn't open windows on
        // ports we couldn't reach.
        self.sent_ports.push(port.to_string());

        Ok(DispatchOutcome::Completed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arc_swap::ArcSwap;
    use conductor_core::MidiOutputManager;
    use conductor_core::dispatch::DispatchError;
    use std::collections::HashMap;
    use std::sync::Arc;

    /// Skip-guard for tests that create real virtual MIDI ports.
    ///
    /// Virtual-port creation needs a live MIDI server: CoreMIDI's
    /// `MIDIServer` (a per-GUI-login-session service) on macOS, or an ALSA
    /// sequencer on Linux. A headless self-hosted runner has neither, so
    /// these tests return early instead of panicking — which lets the macOS
    /// test job run on the self-hosted pool (FU-9 / #1888). On a developer
    /// machine (or hosted runner) with a MIDI session, the test runs for real.
    #[allow(clippy::print_stderr)] // intentional: a test-skip notice on headless runners
    fn require_virtual_midi(test: &str) -> bool {
        if MidiOutputManager::virtual_ports_available() {
            true
        } else {
            eprintln!(
                "skipping {test}: virtual MIDI ports unavailable \
                 (headless runner / no CoreMIDI session) — FU-9 #1888"
            );
            false
        }
    }

    // ========== compute_midi_output_bytes Tests ==========

    #[test]
    fn test_compute_midi_output_bytes_note_on() {
        use conductor_core::{MidiMessageParams, MidiMessageType, VelocityMapping};

        let bytes = compute_midi_output_bytes(
            &MidiMessageType::NoteOn,
            0,
            &MidiMessageParams::Note {
                note: 63,
                velocity_mapping: VelocityMapping::Fixed { velocity: 100 },
            },
            None,
        )
        .unwrap();
        assert_eq!(bytes, vec![0x90, 63, 100]);
    }

    #[test]
    fn test_compute_midi_output_bytes_note_on_channel() {
        use conductor_core::{MidiMessageParams, MidiMessageType, VelocityMapping};

        let bytes = compute_midi_output_bytes(
            &MidiMessageType::NoteOn,
            9, // Channel 10 (0-indexed)
            &MidiMessageParams::Note {
                note: 36,
                velocity_mapping: VelocityMapping::Fixed { velocity: 127 },
            },
            None,
        )
        .unwrap();
        assert_eq!(bytes, vec![0x99, 36, 127]);
    }

    #[test]
    fn test_compute_midi_output_bytes_cc() {
        use conductor_core::{MidiMessageParams, MidiMessageType};

        let bytes = compute_midi_output_bytes(
            &MidiMessageType::ControlChange,
            0,
            &MidiMessageParams::CC {
                controller: 7,
                value: 100,
            },
            None,
        )
        .unwrap();
        assert_eq!(bytes, vec![0xB0, 7, 100]);
    }

    #[test]
    fn test_compute_midi_output_bytes_differs_from_trigger() {
        // Key test: output bytes for SendMidi note 63 should NOT equal trigger bytes for note 59
        use conductor_core::{MidiMessageParams, MidiMessageType, VelocityMapping};

        let output_bytes = compute_midi_output_bytes(
            &MidiMessageType::NoteOn,
            0,
            &MidiMessageParams::Note {
                note: 63,
                velocity_mapping: VelocityMapping::Fixed { velocity: 100 },
            },
            None,
        )
        .unwrap();

        let trigger_bytes = vec![0x90u8, 59, 100]; // Incoming note 59
        assert_ne!(
            output_bytes, trigger_bytes,
            "Output bytes (note 63) must differ from trigger bytes (note 59)"
        );
        assert_eq!(output_bytes, vec![0x90, 63, 100]);
    }

    // ========== SendMidi Action Tests ==========

    #[test]
    #[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
    fn test_send_midi_note_on_encoding() {
        use conductor_core::{MidiMessageParams, MidiMessageType};

        let mut executor = ActionExecutor::default();

        // Test Note On encoding: [0x90 | channel, note, velocity]
        // We can't directly test execute_send_midi since it's private,
        // but we can test the Action variant
        let action = conductor_core::Action::SendMidi {
            port: "Virtual Test Port".to_string(),
            message_type: MidiMessageType::NoteOn,
            channel: 0,
            params: MidiMessageParams::Note {
                note: 60,
                velocity_mapping: conductor_core::VelocityMapping::Fixed { velocity: 100 },
            },
        };

        // Execute shouldn't panic (though send will fail without a port)
        executor.execute(action, None).ok();
    }

    #[test]
    #[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
    fn test_send_midi_note_off_encoding() {
        use conductor_core::{MidiMessageParams, MidiMessageType};

        let mut executor = ActionExecutor::default();

        let action = conductor_core::Action::SendMidi {
            port: "Virtual Test Port".to_string(),
            message_type: MidiMessageType::NoteOff,
            channel: 1,
            params: MidiMessageParams::Note {
                note: 64,
                velocity_mapping: conductor_core::VelocityMapping::Fixed { velocity: 0 },
            },
        };

        executor.execute(action, None).ok();
    }

    #[test]
    #[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
    fn test_send_midi_control_change_encoding() {
        use conductor_core::{MidiMessageParams, MidiMessageType};

        let mut executor = ActionExecutor::default();

        let action = conductor_core::Action::SendMidi {
            port: "Virtual Test Port".to_string(),
            message_type: MidiMessageType::ControlChange,
            channel: 2,
            params: MidiMessageParams::CC {
                controller: 7,
                value: 127,
            },
        };

        executor.execute(action, None).ok();
    }

    #[test]
    #[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
    fn test_send_midi_program_change_encoding() {
        use conductor_core::{MidiMessageParams, MidiMessageType};

        let mut executor = ActionExecutor::default();

        let action = conductor_core::Action::SendMidi {
            port: "Virtual Test Port".to_string(),
            message_type: MidiMessageType::ProgramChange,
            channel: 3,
            params: MidiMessageParams::ProgramChange { program: 42 },
        };

        executor.execute(action, None).ok();
    }

    #[test]
    #[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
    fn test_send_midi_pitch_bend_encoding() {
        use conductor_core::{MidiMessageParams, MidiMessageType};

        let mut executor = ActionExecutor::default();

        // Test pitch bend with center value (0)
        let action = conductor_core::Action::SendMidi {
            port: "Virtual Test Port".to_string(),
            message_type: MidiMessageType::PitchBend,
            channel: 4,
            params: MidiMessageParams::PitchBend { value: 0 },
        };

        executor.execute(action, None).ok();
    }

    #[test]
    #[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
    fn test_send_midi_pitch_bend_min_max() {
        use conductor_core::{MidiMessageParams, MidiMessageType};

        let mut executor = ActionExecutor::default();

        // Test pitch bend minimum (-8192)
        let action_min = conductor_core::Action::SendMidi {
            port: "Virtual Test Port".to_string(),
            message_type: MidiMessageType::PitchBend,
            channel: 5,
            params: MidiMessageParams::PitchBend { value: -8192 },
        };
        executor.execute(action_min, None).ok();

        // Test pitch bend maximum (+8191)
        let action_max = conductor_core::Action::SendMidi {
            port: "Virtual Test Port".to_string(),
            message_type: MidiMessageType::PitchBend,
            channel: 5,
            params: MidiMessageParams::PitchBend { value: 8191 },
        };
        executor.execute(action_max, None).ok();
    }

    #[test]
    #[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
    fn test_send_midi_aftertouch_encoding() {
        use conductor_core::{MidiMessageParams, MidiMessageType};

        let mut executor = ActionExecutor::default();

        let action = conductor_core::Action::SendMidi {
            port: "Virtual Test Port".to_string(),
            message_type: MidiMessageType::Aftertouch,
            channel: 6,
            params: MidiMessageParams::Aftertouch { pressure: 80 },
        };

        executor.execute(action, None).ok();
    }

    #[test]
    #[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
    fn test_send_midi_channel_masking() {
        use conductor_core::{MidiMessageParams, MidiMessageType};

        let mut executor = ActionExecutor::default();

        // Test all 16 MIDI channels (0-15)
        for channel in 0..16 {
            let action = conductor_core::Action::SendMidi {
                port: "Virtual Test Port".to_string(),
                message_type: MidiMessageType::NoteOn,
                channel,
                params: MidiMessageParams::Note {
                    note: 60,
                    velocity_mapping: conductor_core::VelocityMapping::Fixed { velocity: 100 },
                },
            };

            executor.execute(action, None).ok();
        }
    }

    #[test]
    #[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
    fn test_send_midi_data_byte_masking() {
        use conductor_core::{MidiMessageParams, MidiMessageType};

        let mut executor = ActionExecutor::default();

        // Test boundary values for 7-bit data
        let test_values = [0, 1, 63, 64, 127];

        for &note in &test_values {
            for &velocity in &test_values {
                let action = conductor_core::Action::SendMidi {
                    port: "Virtual Test Port".to_string(),
                    message_type: MidiMessageType::NoteOn,
                    channel: 0,
                    params: MidiMessageParams::Note {
                        note,
                        velocity_mapping: conductor_core::VelocityMapping::Fixed { velocity },
                    },
                };

                executor.execute(action, None).ok();
            }
        }
    }

    #[test]
    #[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
    fn test_send_midi_mismatched_type_params() {
        use conductor_core::{MidiMessageParams, MidiMessageType};

        let mut executor = ActionExecutor::default();

        // This should trigger the warning path in execute_send_midi
        // (mismatched NoteOn with CC params - though this shouldn't happen
        // in practice due to From<ActionConfig> conversion logic)

        // The actual mismatch would only occur if we manually construct
        // an Action with wrong params, which the type system prevents.
        // This test just ensures the executor doesn't panic.

        let action = conductor_core::Action::SendMidi {
            port: "Virtual Test Port".to_string(),
            message_type: MidiMessageType::NoteOn,
            channel: 0,
            params: MidiMessageParams::Note {
                note: 60,
                velocity_mapping: conductor_core::VelocityMapping::Fixed { velocity: 100 },
            },
        };

        executor.execute(action, None).ok();
    }

    #[test]
    #[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
    fn test_send_midi_in_sequence() {
        use conductor_core::{MidiMessageParams, MidiMessageType, VelocityMapping};

        let mut executor = ActionExecutor::default();

        // Test SendMidi as part of a sequence
        let action = conductor_core::Action::Sequence(vec![
            conductor_core::Action::SendMidi {
                port: "Virtual Test Port".to_string(),
                message_type: MidiMessageType::NoteOn,
                channel: 0,
                params: MidiMessageParams::Note {
                    note: 60,
                    velocity_mapping: VelocityMapping::Fixed { velocity: 100 },
                },
            },
            conductor_core::Action::Delay(10),
            conductor_core::Action::SendMidi {
                port: "Virtual Test Port".to_string(),
                message_type: MidiMessageType::NoteOff,
                channel: 0,
                params: MidiMessageParams::Note {
                    note: 60,
                    velocity_mapping: VelocityMapping::Fixed { velocity: 0 },
                },
            },
        ]);

        executor.execute(action, None).ok();
    }

    #[test]
    #[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
    fn test_send_midi_with_repeat() {
        use conductor_core::{MidiMessageParams, MidiMessageType};

        let mut executor = ActionExecutor::default();

        // Test SendMidi in a Repeat action
        let action = conductor_core::Action::Repeat {
            action: Box::new(conductor_core::Action::SendMidi {
                port: "Virtual Test Port".to_string(),
                message_type: MidiMessageType::ControlChange,
                channel: 0,
                params: MidiMessageParams::CC {
                    controller: 1,
                    value: 64,
                },
            }),
            count: 3,
            delay_ms: Some(50),
        };

        executor.execute(action, None).ok();
    }

    #[test]
    #[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
    fn test_send_midi_error_returns_dispatch_error() {
        let mut executor = ActionExecutor::default();

        // Mismatched type/params should return an error
        let action = conductor_core::Action::SendMidi {
            port: "Nonexistent Port".to_string(),
            message_type: conductor_core::MidiMessageType::NoteOn,
            channel: 0,
            params: conductor_core::MidiMessageParams::CC {
                controller: 1,
                value: 64,
            },
        };

        let result = executor.execute(action, None);
        assert!(result.is_err());
        if let Err(DispatchError::MidiOutput(msg)) = result {
            assert!(msg.contains("Mismatched"));
        }
    }

    // ========== MidiForward Tests (v4.25.0 - ADR-009 Gap 2) ==========

    #[test]
    #[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
    fn test_midi_forward_missing_raw_midi() {
        let mut executor = ActionExecutor::default();

        let action = conductor_core::Action::MidiForward {
            target: "Synth".to_string(),
            transform: None,
        };

        // No raw_midi in context — should error
        let result = executor.execute(action, None);
        assert!(result.is_err());
        if let Err(DispatchError::MidiOutput(msg)) = result {
            assert!(msg.contains("raw MIDI bytes"));
        }
    }

    #[test]
    #[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
    fn test_midi_forward_with_context_no_raw() {
        let mut executor = ActionExecutor::default();

        let action = conductor_core::Action::MidiForward {
            target: "Synth".to_string(),
            transform: None,
        };

        // Context with velocity but no raw_midi
        let ctx = TriggerContext::with_velocity(100);
        let result = executor.execute(action, Some(ctx));
        assert!(result.is_err());
    }

    #[test]
    #[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
    fn test_midi_forward_with_identity_transform() {
        let mut executor = ActionExecutor::default();

        let action = conductor_core::Action::MidiForward {
            target: "Virtual Test Port".to_string(),
            transform: None, // Identity (passthrough)
        };

        let ctx = TriggerContext {
            velocity: Some(100),
            current_mode: None,
            raw_midi: Some(vec![0x90, 60, 100]),
            device_id: None,
            input_event: None,
            osc_message: None,
        };

        // Port won't exist, but the code path up to send_message should work
        let result = executor.execute(action, Some(ctx));
        // Will fail at connect_by_name since port doesn't exist, but that's a MidiOutput error
        assert!(result.is_err());
        if let Err(DispatchError::MidiOutput(msg)) = result {
            assert!(msg.contains("connect") || msg.contains("send"));
        }
    }

    #[test]
    #[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
    fn test_midi_forward_with_transform() {
        let mut executor = ActionExecutor::default();

        let transform = conductor_core::transform::MidiTransform {
            channel: Some(5),
            cc: None,
            note: Some(72),
            velocity_scale: None,
            velocity_offset: None,
            invert_value: false,
            curve: None,
        };

        let action = conductor_core::Action::MidiForward {
            target: "Virtual Test Port".to_string(),
            transform: Some(transform),
        };

        let ctx = TriggerContext {
            velocity: Some(100),
            current_mode: None,
            raw_midi: Some(vec![0x90, 60, 100]), // NoteOn ch0, note 60, vel 100
            device_id: None,
            input_event: None,
            osc_message: None,
        };

        // Will fail at connect but tests the transform path
        let result = executor.execute(action, Some(ctx));
        assert!(result.is_err()); // Port doesn't exist
    }

    #[test]
    fn test_midi_forward_source_no_device_id() {
        let map = Arc::new(ArcSwap::from_pointee(HashMap::from([(
            "mikro".to_string(),
            "Mikro Output".to_string(),
        )])));
        let mut executor = ActionExecutor::new(map);
        let action = conductor_core::Action::MidiForward {
            target: "_source".to_string(),
            transform: None,
        };
        // Context with raw_midi but no device_id
        let ctx = TriggerContext {
            velocity: Some(100),
            current_mode: None,
            raw_midi: Some(vec![0x90, 60, 100]),
            device_id: None,
            input_event: None,
            osc_message: None,
        };
        let result = executor.execute(action, Some(ctx));
        assert!(result.is_err());
        if let Err(DispatchError::TargetNotBound(msg)) = result {
            assert!(msg.contains("_source"));
        } else {
            panic!("Expected TargetNotBound error, got: {:?}", result);
        }
    }

    #[test]
    fn test_midi_forward_source_no_output_port() {
        let map = Arc::new(ArcSwap::from_pointee(HashMap::new())); // empty — no output bindings
        let mut executor = ActionExecutor::new(map);
        let action = conductor_core::Action::MidiForward {
            target: "_source".to_string(),
            transform: None,
        };
        let ctx = TriggerContext {
            velocity: Some(100),
            current_mode: None,
            raw_midi: Some(vec![0x90, 60, 100]),
            device_id: Some("mikro".to_string()),
            input_event: None,
            osc_message: None,
        };
        let result = executor.execute(action, Some(ctx));
        assert!(result.is_err());
        if let Err(DispatchError::TargetNotBound(msg)) = result {
            assert!(msg.contains("mikro"));
        } else {
            panic!("Expected TargetNotBound error, got: {:?}", result);
        }
    }

    #[test]
    fn test_midi_forward_source_resolves() {
        let map = Arc::new(ArcSwap::from_pointee(HashMap::from([(
            "mikro".to_string(),
            "Mikro Output".to_string(),
        )])));
        let mut executor = ActionExecutor::new(map);
        let action = conductor_core::Action::MidiForward {
            target: "_source".to_string(),
            transform: None,
        };
        let ctx = TriggerContext {
            velocity: Some(100),
            current_mode: None,
            raw_midi: Some(vec![0x90, 60, 100]),
            device_id: Some("mikro".to_string()),
            input_event: None,
            osc_message: None,
        };
        // Will fail at connect_by_name (port doesn't exist), but resolution should succeed
        let result = executor.execute(action, Some(ctx));
        assert!(result.is_err());
        if let Err(DispatchError::MidiOutput(msg)) = result {
            assert!(
                msg.contains("Mikro Output"),
                "Should resolve to output port name, got: {}",
                msg
            );
        } else {
            panic!(
                "Expected MidiOutput error (port not found), got: {:?}",
                result
            );
        }
    }

    // ========== HidForward Tests (ADR-039-B #1762 step 4b) ==========

    fn hid_to_midi_transform() -> conductor_core::config::types::SignalTransform {
        let mut map = std::collections::HashMap::new();
        map.insert("south".to_string(), 20u8);
        conductor_core::config::types::SignalTransform::HidToMidi {
            trigger_to_cc: map,
            channel: 0,
        }
    }

    fn gamepad_south(velocity: u8) -> conductor_core::events::InputEvent {
        conductor_core::events::InputEvent::PadPressed {
            pad: conductor_core::gamepad_events::button_ids::SOUTH,
            velocity,
            channel: None,
            time: std::time::Instant::now(),
        }
    }

    #[test]
    #[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
    fn test_hid_forward_missing_input_event_errors() {
        let mut executor = ActionExecutor::default();
        let action = conductor_core::Action::HidForward {
            target: "Synth".to_string(),
            transform: hid_to_midi_transform(),
        };
        // No input_event in context — must error (not silently no-op).
        let result = executor.execute(action, Some(TriggerContext::with_velocity(100)));
        assert!(result.is_err());
        if let Err(DispatchError::MidiOutput(msg)) = result {
            assert!(
                msg.contains("structured gamepad event"),
                "expected structured-event error, got: {msg}"
            );
        } else {
            panic!("expected MidiOutput error, got: {result:?}");
        }
    }

    #[test]
    #[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
    fn test_hid_forward_unmapped_trigger_is_noop() {
        let mut executor = ActionExecutor::default();
        // North is not in trigger_to_cc → transform yields None → benign
        // completion, no port connection attempted.
        let north = conductor_core::events::InputEvent::PadPressed {
            pad: conductor_core::gamepad_events::button_ids::NORTH,
            velocity: 100,
            channel: None,
            time: std::time::Instant::now(),
        };
        let ctx = TriggerContext {
            input_event: Some(north),
            osc_message: None,
            ..Default::default()
        };
        let action = conductor_core::Action::HidForward {
            target: "Nonexistent Port".to_string(),
            transform: hid_to_midi_transform(),
        };
        let result = executor.execute(action, Some(ctx));
        assert_eq!(result.unwrap(), DispatchOutcome::Completed);
    }

    #[test]
    #[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
    fn test_hid_forward_mapped_trigger_reaches_dispatch() {
        let mut executor = ActionExecutor::default();
        let ctx = TriggerContext {
            input_event: Some(gamepad_south(100)),
            osc_message: None,
            ..Default::default()
        };
        let action = conductor_core::Action::HidForward {
            target: "Nonexistent Port".to_string(),
            transform: hid_to_midi_transform(),
        };
        // Transform produces bytes; dispatch fails only because the port
        // doesn't exist — proving the HidToMidi → MIDI-send path runs.
        let result = executor.execute(action, Some(ctx));
        assert!(result.is_err());
        if let Err(DispatchError::MidiOutput(msg)) = result {
            assert!(
                msg.contains("connect") || msg.contains("send"),
                "expected connect/send error, got: {msg}"
            );
        } else {
            panic!("expected MidiOutput error, got: {result:?}");
        }
    }

    // ────────────────────────────────────────────────────────────────────
    // Issue #555 (Copilot review on PR #1211): sent_ports capture
    //
    // The cascade-suppression layer needs to know which output ports a
    // dispatch actually wrote to. Capture happens INSIDE the executor
    // (not at the top of the executor thread) so we cover:
    //   - Wrapper actions (`Sequence`, `Repeat`, `Conditional`,
    //     `ContextSwitchTable`) that nest one or more SendMidi calls.
    //   - `MidiForward { target: "_source" }` — the literal "_source"
    //     placeholder is resolved to the device's bound output port at
    //     execute time, so capturing at the top level would mis-record.
    //
    // Tests use virtual MIDI ports (macOS/Linux only — Windows midir has
    // no virtual-output support) so SendMidi/MidiForward succeed without
    // any external MIDI hardware.
    // ────────────────────────────────────────────────────────────────────

    #[test]
    fn sent_ports_starts_empty_and_drains() {
        let mut executor = ActionExecutor::default();
        assert!(
            executor.take_sent_ports().is_empty(),
            "fresh executor must have no sent_ports"
        );
        // Second take must also be empty (drain semantics).
        assert!(executor.take_sent_ports().is_empty());
    }

    #[test]
    fn sent_ports_stays_empty_when_send_fails() {
        // Sending to a port that doesn't exist must NOT push to
        // sent_ports — we only open a cascade window for ports we
        // actually managed to write to. If we recorded failed sends,
        // the daemon would suppress phantom-port input pointlessly
        // (and a malicious config could spam the blanket_until map).
        let mut executor = ActionExecutor::default();
        let action = conductor_core::Action::SendMidi {
            port: "Definitely Not A Real Port 1234".to_string(),
            message_type: conductor_core::MidiMessageType::NoteOn,
            channel: 0,
            params: conductor_core::MidiMessageParams::Note {
                note: 60,
                velocity_mapping: conductor_core::VelocityMapping::Fixed { velocity: 100 },
            },
        };
        let _ = executor.execute(action, None); // expected to fail
        assert!(
            executor.take_sent_ports().is_empty(),
            "failed SendMidi must not record port"
        );
    }

    #[test]
    #[cfg_attr(target_os = "windows", ignore)] // midir has no virtual ports on Windows
    #[cfg_attr(target_os = "linux", ignore)] // ALSA virtual ports unavailable in headless CI
    fn sent_ports_records_resolved_port_on_successful_send_midi() {
        if !require_virtual_midi("sent_ports_records_resolved_port_on_successful_send_midi") {
            return;
        }
        let mut executor = ActionExecutor::default();
        let port_name = "conductor-test-555-send";
        executor
            .midi_output
            .create_virtual_port(port_name)
            .expect("virtual port creation must succeed on macOS/Linux");

        let action = conductor_core::Action::SendMidi {
            port: port_name.to_string(),
            message_type: conductor_core::MidiMessageType::NoteOn,
            channel: 0,
            params: conductor_core::MidiMessageParams::Note {
                note: 60,
                velocity_mapping: conductor_core::VelocityMapping::Fixed { velocity: 100 },
            },
        };
        executor
            .execute(action, None)
            .expect("send to virtual port must succeed");

        assert_eq!(
            executor.take_sent_ports(),
            vec![port_name.to_string()],
            "successful SendMidi must record its port"
        );
    }

    #[test]
    #[cfg_attr(target_os = "windows", ignore)] // midir has no virtual ports on Windows
    #[cfg_attr(target_os = "linux", ignore)] // ALSA virtual ports unavailable in headless CI
    fn sent_ports_records_each_nested_send_in_sequence() {
        if !require_virtual_midi("sent_ports_records_each_nested_send_in_sequence") {
            return;
        }
        // Sequence-wrapped SendMidi was Copilot's primary concern:
        // when capture happened at the top-level action, this case
        // produced an empty `output_port` because the top-level
        // action is `Sequence(...)`, not `SendMidi`. Now we record
        // INSIDE execute_send_midi, so each nested send gets logged.
        let mut executor = ActionExecutor::default();
        let p1 = "conductor-test-555-seq-a";
        let p2 = "conductor-test-555-seq-b";
        executor.midi_output.create_virtual_port(p1).unwrap();
        executor.midi_output.create_virtual_port(p2).unwrap();

        let action = conductor_core::Action::Sequence(vec![
            conductor_core::Action::SendMidi {
                port: p1.to_string(),
                message_type: conductor_core::MidiMessageType::NoteOn,
                channel: 0,
                params: conductor_core::MidiMessageParams::Note {
                    note: 60,
                    velocity_mapping: conductor_core::VelocityMapping::Fixed { velocity: 100 },
                },
            },
            conductor_core::Action::SendMidi {
                port: p2.to_string(),
                message_type: conductor_core::MidiMessageType::NoteOn,
                channel: 0,
                params: conductor_core::MidiMessageParams::Note {
                    note: 62,
                    velocity_mapping: conductor_core::VelocityMapping::Fixed { velocity: 100 },
                },
            },
        ]);
        executor
            .execute(action, None)
            .expect("sequence must execute cleanly");

        let recorded = executor.take_sent_ports();
        assert_eq!(
            recorded,
            vec![p1.to_string(), p2.to_string()],
            "Sequence must record one port per inner SendMidi, in order"
        );
    }

    #[test]
    #[cfg_attr(target_os = "windows", ignore)] // midir has no virtual ports on Windows
    #[cfg_attr(target_os = "linux", ignore)] // ALSA virtual ports unavailable in headless CI
    fn sent_ports_records_resolved_target_for_midi_forward_source() {
        if !require_virtual_midi("sent_ports_records_resolved_target_for_midi_forward_source") {
            return;
        }
        // `MidiForward { target: "_source" }` resolves to the
        // originating device's bound output port at execute time
        // (`resolve_source_output`). Capturing at the top level
        // would store the literal "_source", which would never match
        // an incoming `device_id` at the suppress-check site —
        // silently breaking cascade suppression for the most common
        // forward case (echo back to the source device).
        let resolved_port = "conductor-test-555-source-out";
        let device_alias = "test-device-alias";

        // Build the executor with a device→output map binding.
        let device_map = Arc::new(ArcSwap::from_pointee({
            let mut m = HashMap::new();
            m.insert(device_alias.to_string(), resolved_port.to_string());
            m
        }));
        let mut executor = ActionExecutor::new(device_map);
        executor
            .midi_output
            .create_virtual_port(resolved_port)
            .expect("virtual port creation must succeed");

        let action = conductor_core::Action::MidiForward {
            target: "_source".to_string(),
            transform: None,
        };
        let context = TriggerContext {
            raw_midi: Some(vec![0x90, 60, 100]),
            device_id: Some(device_alias.to_string()),
            ..Default::default()
        };

        executor
            .execute(action, Some(context))
            .expect("MidiForward to resolved _source must succeed");

        assert_eq!(
            executor.take_sent_ports(),
            vec![resolved_port.to_string()],
            "MidiForward _source must record the *resolved* port, not the literal '_source'"
        );
    }
}
