// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! HID → OSC structured transform (ADR-039-B, #1762 step 3).
//!
//! Maps a gamepad trigger (button/axis) to an OSC message: the trigger's
//! canonical name (see [`crate::transforms::hid_trigger`]) selects the OSC
//! address, and the event's 7-bit value (button velocity / axis 0-127) becomes
//! the single OSC argument — a normalized `Float` 0.0-1.0 when `value_to_float`
//! (the usual OSC convention for continuous controls), else a raw `Int`.
//!
//! Structured analogue of [`crate::transforms::hid_to_midi`] /
//! [`crate::transforms::hid_to_artnet`]: the route engine threads the original
//! `&InputEvent` here via `RouteEvalContext` (spec §6.2.1) because the byte
//! serialization is lossy/ambiguous for gamepad. Unlike `MidiToOsc` (one
//! address template covering many CCs, needing `{cc}` placeholders), each HID
//! trigger maps to its own fully-specified address, so no placeholder
//! substitution is needed. OSC addresses are validated (`must start with '/'`)
//! at config-load.

use crate::transforms::hid_trigger::trigger_name_and_value;
use conductor_core::config::types::SignalTransform;
use conductor_core::events::InputEvent;
use rosc::encoder;
use rosc::{OscMessage, OscPacket, OscType};

/// Apply a `SignalTransform::HidToOsc` to a gamepad `InputEvent`.
///
/// Returns the encoded OSC packet bytes, or `None` if:
/// - `transform` is not `SignalTransform::HidToOsc` (defensive guard so the
///   route dispatch can pass any transform without pre-matching the enum),
/// - the event is not a gamepad input (MIDI-sourced `channel: Some(_)`, or a
///   variant HID never produces),
/// - the resolved trigger name is not in `trigger_to_address`,
/// - rosc encoding fails (e.g. an oversized address) — surfaced as a route skip
///   rather than a panic, matching `midi_to_osc`.
pub fn apply(transform: &SignalTransform, event: &InputEvent) -> Option<Vec<u8>> {
    let SignalTransform::HidToOsc {
        trigger_to_address,
        value_to_float,
    } = transform
    else {
        return None;
    };

    let (trigger_name, value) = trigger_name_and_value(event)?;
    let addr = trigger_to_address.get(trigger_name)?;

    let arg = if *value_to_float {
        // Normalize the 7-bit value to 0.0-1.0 (OSC convention for faders/axes).
        // `clamp` is defense-in-depth: `trigger_name_and_value` yields 7-bit
        // values today, but pinning the OSC contract to [0.0, 1.0] keeps it
        // honest if that upstream bound ever widens (mirrors the clamp in the
        // sibling `hid_to_midi` transform).
        OscType::Float((f32::from(value) / 127.0).clamp(0.0, 1.0))
    } else {
        OscType::Int(i32::from(value))
    };

    let packet = OscPacket::Message(OscMessage {
        addr: addr.clone(),
        args: vec![arg],
    });
    encoder::encode(&packet).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use conductor_core::gamepad_events::{button_ids, encoder_ids};
    use rosc::OscPacket;
    use std::collections::HashMap;
    use std::time::Instant;

    fn hid_to_osc(pairs: &[(&str, &str)], value_to_float: bool) -> SignalTransform {
        SignalTransform::HidToOsc {
            trigger_to_address: pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
            value_to_float,
        }
    }

    fn button(pad: u8, velocity: u8) -> InputEvent {
        InputEvent::PadPressed {
            pad,
            velocity,
            channel: None,
            time: Instant::now(),
        }
    }

    /// Decode the encoded packet back to (address, args) for assertions.
    fn decode(bytes: &[u8]) -> (String, Vec<OscType>) {
        match rosc::decoder::decode_udp(bytes)
            .expect("valid OSC packet")
            .1
        {
            OscPacket::Message(m) => (m.addr, m.args),
            other => panic!("expected OscMessage, got {other:?}"),
        }
    }

    #[test]
    fn button_maps_to_osc_address_with_float_arg() {
        let t = hid_to_osc(&[("south", "/pad/a")], true);
        let bytes = apply(&t, &button(button_ids::SOUTH, 127)).expect("south is mapped");
        let (addr, args) = decode(&bytes);
        assert_eq!(addr, "/pad/a");
        assert_eq!(args, vec![OscType::Float(1.0)], "127 → 1.0");
    }

    #[test]
    fn axis_maps_to_int_arg_when_float_disabled() {
        let t = hid_to_osc(&[("left_stick_x", "/stick/x")], false);
        let ev = InputEvent::EncoderTurned {
            encoder: encoder_ids::LEFT_STICK_X,
            value: 64,
            channel: None,
            analog: None,
            time: Instant::now(),
        };
        let bytes = apply(&t, &ev).expect("axis is mapped");
        let (addr, args) = decode(&bytes);
        assert_eq!(addr, "/stick/x");
        assert_eq!(
            args,
            vec![OscType::Int(64)],
            "raw int when value_to_float=false"
        );
    }

    #[test]
    fn unmapped_trigger_yields_none() {
        let t = hid_to_osc(&[("south", "/pad/a")], true);
        assert!(apply(&t, &button(button_ids::NORTH, 100)).is_none());
    }

    #[test]
    fn midi_sourced_event_yields_none() {
        // channel: Some(_) ⇒ MIDI source, not a gamepad trigger.
        let t = hid_to_osc(&[("south", "/pad/a")], true);
        let midi_ev = InputEvent::PadPressed {
            pad: button_ids::SOUTH,
            velocity: 100,
            channel: Some(0),
            time: Instant::now(),
        };
        assert!(apply(&t, &midi_ev).is_none());
    }

    #[test]
    fn wrong_transform_variant_yields_none() {
        let t = SignalTransform::HidToArtNet {
            trigger_to_channel: HashMap::new(),
        };
        assert!(apply(&t, &button(button_ids::SOUTH, 100)).is_none());
    }
}
