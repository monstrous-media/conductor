// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! HID → Art-Net transform (ADR-031 § 7.3 / #1145 P5 slice 7).
//!
//! Translates a gamepad/HID `InputEvent` into a single DMX channel-update
//! record based on a `SignalTransform::HidToArtNet` config. Pure function —
//! no I/O, no state. Mirrors [`midi_to_artnet`](super::midi_to_artnet) for
//! the HID side of P5: the runtime sender shipped in slice 6
//! (`ConnectorRegistry::send_artnet`) is reused as-is.
//!
//! ## Supported HID inputs
//!
//! Gamepad inputs flow through `InputEvent` per
//! `conductor_core::gamepad_events` with non-overlapping IDs (128-255):
//!
//! - `InputEvent::PadPressed { pad, velocity, channel: None, .. }` for
//!   button presses (pad IDs 128-144). `channel: Some(_)` indicates a
//!   MIDI source — those events return `None` here (they belong to a
//!   `MidiToArtNet` route, not `HidToArtNet`).
//! - `InputEvent::EncoderTurned { encoder, value, channel: None, .. }`
//!   for analog stick axes and analog triggers (encoder IDs 128-133).
//!
//! `InputEvent::PadReleased` returns `None` — a release does not carry
//! velocity, so there's no useful intensity to send. Other event types
//! (Aftertouch, ProgramChange, etc.) are MIDI-specific and return `None`.
//!
//! ## Canonical trigger names
//!
//! The `trigger_to_channel: HashMap<String, u16>` config keys use the
//! same mnemonic strings as `conductor_core::gamepad_events::button_ids`
//! / `encoder_ids`, lowercased:
//!
//! | Source                                  | Trigger name      |
//! |-----------------------------------------|-------------------|
//! | `PadPressed { pad: 128 }` (SOUTH)       | `"south"`         |
//! | `PadPressed { pad: 129 }` (EAST)        | `"east"`          |
//! | `PadPressed { pad: 130 }` (WEST)        | `"west"`          |
//! | `PadPressed { pad: 131 }` (NORTH)       | `"north"`         |
//! | `PadPressed { pad: 132 }` (DPad up)     | `"dpad_up"`       |
//! | `PadPressed { pad: 133 }` (DPad down)   | `"dpad_down"`     |
//! | `PadPressed { pad: 134 }` (DPad left)   | `"dpad_left"`     |
//! | `PadPressed { pad: 135 }` (DPad right)  | `"dpad_right"`    |
//! | `PadPressed { pad: 136 }` (L1/LB)       | `"left_shoulder"` |
//! | `PadPressed { pad: 137 }` (R1/RB)       | `"right_shoulder"`|
//! | `PadPressed { pad: 138 }` (L3)          | `"left_thumb"`    |
//! | `PadPressed { pad: 139 }` (R3)          | `"right_thumb"`   |
//! | `PadPressed { pad: 140 }` (Start)       | `"start"`         |
//! | `PadPressed { pad: 141 }` (Select)      | `"select"`        |
//! | `PadPressed { pad: 142 }` (Guide)       | `"guide"`         |
//! | `EncoderTurned { encoder: 128 }`        | `"left_stick_x"`  |
//! | `EncoderTurned { encoder: 129 }`        | `"left_stick_y"`  |
//! | `EncoderTurned { encoder: 130 }`        | `"right_stick_x"` |
//! | `EncoderTurned { encoder: 131 }`        | `"right_stick_y"` |
//! | `EncoderTurned { encoder: 132 }`        | `"left_trigger"`  |
//! | `EncoderTurned { encoder: 133 }`        | `"right_trigger"` |
//!
//! `PadPressed { pad: 143/144 }` (digital trigger fallback) reuse the
//! `"left_trigger"`/`"right_trigger"` names, because configs that map
//! `left_trigger` to a DMX channel want the same DMX behavior whether
//! the gamepad reports the trigger as analog or as digital threshold.
//! Out-of-range button/encoder IDs return `None`.
//!
//! ## Value scaling
//!
//! HID values arrive as 7-bit (0-127, matching MIDI convention per
//! `gamepad_events` design); DMX channel levels are 8-bit (0-255).
//! The scale formula is identical to `midi_to_artnet`'s — see that
//! module's `scale_7bit_to_8bit` for the exact mapping.
//!
//! ## What this transform does NOT do
//!
//! - **No Art-Net frame construction** —
//!   [`ConnectorRegistry::send_artnet`](crate::connector_registry::ConnectorRegistry::send_artnet)
//!   accumulates `DmxUpdate`s and emits OpDmx packets.
//! - **No HID input ingest** — gamepad → `InputEvent` conversion lives in
//!   `gamepad_device.rs` and flows through the unified pump.
//!
//! As of ADR-039-B (#1762) this transform IS wired through route dispatch:
//! `RouteEngine::compile()` admits `HidToArtNet`, and `evaluate_route` applies
//! it on the structured `InputEvent` threaded via `RouteEvalContext` (spec
//! §6.2.1) — so a gamepad event on a `HidToArtNet` route produces a DMX update.

use crate::transforms::hid_trigger::{scale_7bit_to_8bit, trigger_name_and_value};
use crate::transforms::midi_to_artnet::DmxUpdate;
use conductor_core::config::types::SignalTransform;
use conductor_core::events::InputEvent;

/// Apply a `SignalTransform::HidToArtNet` to a gamepad `InputEvent`.
///
/// Returns `Some(DmxUpdate)` for a successful translation, or `None`
/// if:
/// - `transform` is not `SignalTransform::HidToArtNet` (defensive guard
///   so callers can dispatch uniformly without pre-matching the enum)
/// - The event is not a gamepad input (`channel.is_some()` → MIDI source,
///   or event type that has no HID meaning)
/// - The gamepad ID is out of the documented 128-255 range
/// - The trigger name is not in `trigger_to_channel`
pub fn apply(transform: &SignalTransform, event: &InputEvent) -> Option<DmxUpdate> {
    let SignalTransform::HidToArtNet { trigger_to_channel } = transform else {
        return None;
    };

    let (trigger_name, value) = trigger_name_and_value(event)?;
    let channel = *trigger_to_channel.get(trigger_name)?;
    let scaled = scale_7bit_to_8bit(value);
    Some(DmxUpdate {
        channel,
        value: scaled,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use conductor_core::config::types::SignalTransform;
    use conductor_core::gamepad_events::{button_ids, encoder_ids};
    use std::collections::HashMap;
    use std::time::Instant;

    fn hid_transform(pairs: &[(&str, u16)]) -> SignalTransform {
        SignalTransform::HidToArtNet {
            trigger_to_channel: pairs.iter().map(|(k, v)| ((*k).to_string(), *v)).collect(),
        }
    }

    fn pad(id: u8, velocity: u8) -> InputEvent {
        InputEvent::PadPressed {
            pad: id,
            velocity,
            channel: None,
            time: Instant::now(),
        }
    }

    fn encoder(id: u8, value: u8) -> InputEvent {
        InputEvent::EncoderTurned {
            encoder: id,
            value,
            channel: None,
            analog: None,
            time: Instant::now(),
        }
    }

    #[test]
    fn apply_returns_dmx_update_for_mapped_button() {
        let t = hid_transform(&[("south", 5)]);
        let evt = pad(button_ids::SOUTH, 127);
        assert_eq!(
            apply(&t, &evt),
            Some(DmxUpdate {
                channel: 5,
                value: 255
            })
        );
    }

    #[test]
    fn apply_returns_dmx_update_for_mapped_encoder() {
        let t = hid_transform(&[("left_trigger", 12)]);
        let evt = encoder(encoder_ids::LEFT_TRIGGER, 64);
        // 64 * 255 / 127 = 128
        assert_eq!(
            apply(&t, &evt),
            Some(DmxUpdate {
                channel: 12,
                value: 128
            })
        );
    }

    #[test]
    fn apply_aliases_digital_trigger_pads_to_analog_trigger_names() {
        // The config maps "left_trigger" -> channel 7. Whether the
        // gamepad reports it via the analog encoder (132) or the digital
        // pad fallback (143), the DMX channel must be the same — this
        // is the documented alias behaviour.
        let t = hid_transform(&[("left_trigger", 7)]);
        let analog = encoder(encoder_ids::LEFT_TRIGGER, 127); // encoder 132
        let digital = pad(button_ids::LEFT_TRIGGER, 127); // pad 143

        assert_eq!(
            apply(&t, &analog),
            Some(DmxUpdate {
                channel: 7,
                value: 255
            })
        );
        assert_eq!(
            apply(&t, &digital),
            Some(DmxUpdate {
                channel: 7,
                value: 255
            })
        );
    }

    #[test]
    fn apply_returns_none_for_wrong_transform_variant() {
        let t = SignalTransform::MidiToArtNet {
            cc_to_dmx: HashMap::new(),
            note_to_dmx: HashMap::new(),
        };
        let evt = pad(button_ids::SOUTH, 127);
        assert_eq!(apply(&t, &evt), None);
    }

    #[test]
    fn apply_returns_none_for_unmapped_trigger_name() {
        // Map "south" only — pressing "east" returns None.
        let t = hid_transform(&[("south", 5)]);
        let evt = pad(button_ids::EAST, 127);
        assert_eq!(apply(&t, &evt), None);
    }

    #[test]
    fn apply_returns_none_for_out_of_range_button_id() {
        // pad 145 is outside the 128-144 button range; no name → None.
        let t = hid_transform(&[("south", 5), ("anything", 6)]);
        let evt = pad(145, 100);
        assert_eq!(apply(&t, &evt), None);
    }

    #[test]
    fn apply_returns_none_for_out_of_range_encoder_id() {
        // encoder 134 is outside the 128-133 encoder range.
        let t = hid_transform(&[("anything", 6)]);
        let evt = encoder(134, 100);
        assert_eq!(apply(&t, &evt), None);
    }

    #[test]
    fn apply_returns_none_for_midi_sourced_pad_event() {
        // PadPressed with channel: Some(_) is a MIDI note, NOT a gamepad
        // button — it belongs to a MidiToArtNet route, not HidToArtNet.
        let t = hid_transform(&[("south", 5)]);
        let evt = InputEvent::PadPressed {
            pad: button_ids::SOUTH,
            velocity: 100,
            channel: Some(0),
            time: Instant::now(),
        };
        assert_eq!(apply(&t, &evt), None);
    }

    #[test]
    fn apply_returns_none_for_midi_sourced_encoder_event() {
        let t = hid_transform(&[("left_trigger", 7)]);
        let evt = InputEvent::EncoderTurned {
            encoder: encoder_ids::LEFT_TRIGGER,
            value: 100,
            channel: Some(0),
            analog: None,
            time: Instant::now(),
        };
        assert_eq!(apply(&t, &evt), None);
    }

    #[test]
    fn apply_returns_none_for_pad_released() {
        // Releases carry no velocity / intensity — DMX update would be
        // meaningless. Caller should send the "off" frame explicitly.
        let t = hid_transform(&[("south", 5)]);
        let evt = InputEvent::PadReleased {
            pad: button_ids::SOUTH,
            channel: None,
            time: Instant::now(),
        };
        assert_eq!(apply(&t, &evt), None);
    }

    #[test]
    fn apply_returns_none_for_unrelated_input_event_types() {
        // Aftertouch / ProgramChange / etc. are MIDI-only — never produced
        // by gamepad ingest — return None.
        let t = hid_transform(&[("south", 5)]);
        let evt = InputEvent::Aftertouch {
            pressure: 100,
            channel: Some(0),
            time: Instant::now(),
        };
        assert_eq!(apply(&t, &evt), None);
    }

    #[test]
    fn scale_endpoints_match_midi_to_artnet() {
        // Sanity: the scaler is identical to slice 5's so configs
        // see consistent DMX levels for equivalent 7-bit inputs.
        assert_eq!(scale_7bit_to_8bit(0), 0);
        assert_eq!(scale_7bit_to_8bit(127), 255);
        assert_eq!(scale_7bit_to_8bit(64), 128);
    }

    #[test]
    fn scale_release_build_clamps_out_of_range_input() {
        // In release builds, the debug_assert is compiled out and the
        // `.min(127)` clamp keeps the scaler bounded. This pins that
        // contract for release CI (debug builds panic via the assert
        // — that path is covered by upstream regression tests).
        if !cfg!(debug_assertions) {
            assert_eq!(scale_7bit_to_8bit(200), 255);
            assert_eq!(scale_7bit_to_8bit(255), 255);
        }
    }

    #[test]
    fn apply_with_empty_map_returns_none_for_any_input() {
        let t = hid_transform(&[]);
        assert_eq!(apply(&t, &pad(button_ids::SOUTH, 127)), None);
        assert_eq!(apply(&t, &encoder(encoder_ids::LEFT_STICK_X, 64)), None);
    }

    #[test]
    fn apply_handles_multiple_distinct_mappings_independently() {
        // Two buttons mapped to different channels should produce
        // independent DmxUpdates — no cross-table contamination.
        let t = hid_transform(&[("south", 5), ("east", 6)]);
        assert_eq!(
            apply(&t, &pad(button_ids::SOUTH, 127)),
            Some(DmxUpdate {
                channel: 5,
                value: 255
            })
        );
        assert_eq!(
            apply(&t, &pad(button_ids::EAST, 127)),
            Some(DmxUpdate {
                channel: 6,
                value: 255
            })
        );
    }
}
