// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! HID → MIDI structured transform (ADR-039-B, #1762 step 2).
//!
//! Maps a gamepad trigger (button/axis) to a MIDI **Control Change** on a
//! configured channel: the trigger's canonical name (see
//! [`crate::transforms::hid_trigger`]) selects the CC number, and the event's
//! 7-bit value (button press velocity / axis 0-127) becomes the CC value
//! verbatim — both are 7-bit, so no scaling is needed (unlike `HidToArtNet`,
//! whose DMX level is 8-bit).
//!
//! This is the structured analogue of `HidToArtNet`: the route engine threads
//! the original `&InputEvent` to this transform via `RouteEvalContext`
//! (spec §6.2.1) because the byte serialization is lossy/ambiguous for gamepad
//! (button 128 → MIDI note 0). CC was chosen as the MIDI representation because
//! it is the natural target for continuous gamepad axes and is unambiguous for
//! buttons (press → CC value, e.g. 127); note/velocity mapping can be added as
//! a follow-up if demanded.

use crate::transforms::hid_trigger::trigger_name_and_value;
use conductor_core::config::types::SignalTransform;
use conductor_core::events::InputEvent;

/// Apply a `SignalTransform::HidToMidi` to a gamepad `InputEvent`.
///
/// Returns `Some(midi_bytes)` — a 3-byte Control Change `[0xB0|ch, cc, value]` —
/// or `None` if:
/// - `transform` is not `SignalTransform::HidToMidi` (defensive guard so the
///   route dispatch can pass any transform without pre-matching the enum),
/// - the event is not a gamepad input (MIDI-sourced `channel: Some(_)`, or a
///   variant HID never produces),
/// - the resolved trigger name is not in `trigger_to_cc`.
pub fn apply(transform: &SignalTransform, event: &InputEvent) -> Option<Vec<u8>> {
    let SignalTransform::HidToMidi {
        trigger_to_cc,
        channel,
    } = transform
    else {
        return None;
    };

    let (trigger_name, value) = trigger_name_and_value(event)?;
    let cc = *trigger_to_cc.get(trigger_name)?;
    // CC status 0xB0 | channel; controller + value are 7-bit. The gamepad value
    // is already 0-127, so it maps to the CC value verbatim (no scaling).
    //
    // `channel` (0-15) and `cc` (0-127) ranges are REJECTED at config-load by
    // `validation::validate` (mirroring the existing CC/channel checks), so for
    // any config that loaded these clamps are no-ops. They remain as cheap
    // defense-in-depth against a validation bypass producing a malformed status
    // byte. We CLAMP (`.min`) rather than mask (`& 0x7F`): saturating an
    // out-of-range value is less surprising than bit-wrapping it (200 → 127, not
    // 72), matching `hid_trigger::scale_7bit_to_8bit`'s clamp stance.
    Some(vec![0xB0 | (*channel).min(15), cc.min(127), value.min(127)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use conductor_core::gamepad_events::{button_ids, encoder_ids};
    use std::collections::HashMap;
    use std::time::Instant;

    fn hid_to_midi(pairs: &[(&str, u8)], channel: u8) -> SignalTransform {
        SignalTransform::HidToMidi {
            trigger_to_cc: pairs.iter().map(|(k, v)| ((*k).to_string(), *v)).collect(),
            channel,
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

    #[test]
    fn button_maps_to_cc_on_configured_channel() {
        let t = hid_to_midi(&[("south", 20)], 2);
        let out = apply(&t, &button(button_ids::SOUTH, 100)).expect("south is mapped");
        assert_eq!(out, vec![0xB0 | 2, 20, 100], "CC#20 value 100 on channel 2");
    }

    #[test]
    fn axis_value_passes_through_7bit() {
        let t = hid_to_midi(&[("left_stick_x", 7)], 0);
        let ev = InputEvent::EncoderTurned {
            encoder: encoder_ids::LEFT_STICK_X,
            value: 64,
            channel: None,
            analog: None,
            time: Instant::now(),
        };
        let out = apply(&t, &ev).expect("axis is mapped");
        assert_eq!(
            out,
            vec![0xB0, 7, 64],
            "axis 64 → CC#7 value 64 (no scaling)"
        );
    }

    #[test]
    fn unmapped_trigger_yields_none() {
        let t = hid_to_midi(&[("south", 20)], 0);
        assert!(apply(&t, &button(button_ids::NORTH, 100)).is_none());
    }

    #[test]
    fn midi_sourced_event_yields_none() {
        // channel: Some(_) ⇒ MIDI source, not a gamepad trigger.
        let t = hid_to_midi(&[("south", 20)], 0);
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

    #[test]
    fn out_of_range_cc_and_value_are_clamped_defensively() {
        // CC > 127 is rejected at config-load and gamepad values are 0-127 by
        // ingest; if either slipped through, apply() saturates both to 127 so the
        // emitted CC stays valid 7-bit (defense-in-depth, not a bit-wrap).
        let t = hid_to_midi(&[("south", 200)], 0);
        let out = apply(&t, &button(button_ids::SOUTH, 200)).unwrap();
        assert_eq!(
            out,
            vec![0xB0, 127, 127],
            "cc 200 and value 200 saturate to 127"
        );
    }

    #[test]
    fn out_of_range_channel_is_clamped_defensively() {
        // Defense-in-depth: validation rejects channel > 15 at config-load, but
        // if one slipped through it is CLAMPED (saturated to 15), not bit-masked
        // — so the status byte is always a valid CC status. (200 → 127 for value
        // likewise, vs a masking wrap to 72.)
        let t = hid_to_midi(&[("south", 20)], 0xFF);
        let out = apply(&t, &button(button_ids::SOUTH, 100)).unwrap();
        assert_eq!(out[0], 0xB0 | 15, "channel saturates to 15 (0x0F)");
    }
}
