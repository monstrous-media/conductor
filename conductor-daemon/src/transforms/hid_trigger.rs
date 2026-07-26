// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Shared gamepad-trigger resolution for the structured HID transforms
//! (ADR-039-B). `HidToArtNet`, `HidToMidi`, and `HidToOsc` all map a gamepad
//! `InputEvent` to a canonical trigger *name* + a 7-bit value, then look that
//! name up in a per-transform table. The name table and the 7-bit/8-bit scaler
//! live here so the transforms agree on the gamepad vocabulary (extracted from
//! `hid_to_artnet` in #1762 step 2 when the second HID transform landed).

use conductor_core::events::InputEvent;

/// A HID control, typed by kind (ADR-039-B; the typed-control namespace handed
/// off from ADR-047 §D7).
///
/// Gamepad buttons and axes share the 128+ numeric id space but are disjoint by
/// `InputEvent` *variant* (`PadPressed` vs `EncoderTurned`). `Control` promotes
/// that distinction to an explicit value so the HID transforms switch on the
/// control *kind* rather than the implicit "which id→name map do I consult"
/// choice, and so the L2/R2 digital↔analog alias has a single, documented home
/// (see [`Control::name`]). Inner `u8` is the positional id. [`Control::name`]
/// currently resolves button ids 128-144 and axis/encoder ids 128-133; the
/// #2428 reserved extras (buttons 145/146, d-pad-as-axis 147/148) are valid
/// trigger ids but have no canonical *transform* name yet, so `name()` returns
/// `None` for them (forwarding those via HID transforms is a possible follow-up).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Control {
    Button(u8),
    Axis(u8),
}

impl Control {
    /// Derive the typed control + its 7-bit value from a HID `InputEvent`.
    ///
    /// Returns `None` for non-gamepad events (`channel: Some(_)` is a MIDI
    /// source, or a variant HID never produces). The button-vs-axis kind comes
    /// from the event *variant*, never from sniffing the id's numeric range.
    pub(crate) fn from_event(event: &InputEvent) -> Option<(Control, u8)> {
        match event {
            InputEvent::PadPressed {
                pad,
                velocity,
                channel: None,
                ..
            } => Some((Control::Button(*pad), *velocity)),
            InputEvent::EncoderTurned {
                encoder,
                value,
                channel: None,
                ..
            } => Some((Control::Axis(*encoder), *value)),
            _ => None,
        }
    }

    /// Canonical trigger name for this control, or `None` if the id is unmapped.
    ///
    /// The analog trigger axes (`encoder_ids::LEFT_TRIGGER`/`RIGHT_TRIGGER`,
    /// 132/133) and their digital button counterparts
    /// (`button_ids::LEFT_TRIGGER`/`RIGHT_TRIGGER`, 143/144) **intentionally**
    /// resolve to the same `"left_trigger"`/`"right_trigger"` name: a backend
    /// that reports a trigger as a digital button OR an analog axis maps to one
    /// logical control, so a config (and `HidToMidi`/`HidToOsc`/`HidToArtNet`)
    /// targets it once regardless of how it was delivered.
    pub(crate) fn name(self) -> Option<&'static str> {
        use conductor_core::gamepad_events::{button_ids, encoder_ids};
        match self {
            // The L2/R2 alias lives ONLY here (single source): the digital
            // trigger button and the analog trigger axis are the same logical
            // control. `button_name`/`encoder_name` deliberately do NOT carry
            // the trigger names, so the two id→name tables can't drift apart.
            Control::Button(button_ids::LEFT_TRIGGER)
            | Control::Axis(encoder_ids::LEFT_TRIGGER) => Some("left_trigger"),
            Control::Button(button_ids::RIGHT_TRIGGER)
            | Control::Axis(encoder_ids::RIGHT_TRIGGER) => Some("right_trigger"),
            Control::Button(id) => button_name(id),
            Control::Axis(id) => encoder_name(id),
        }
    }
}

/// Resolve a gamepad `InputEvent` to its canonical trigger name + 7-bit value.
///
/// Returns `None` for non-gamepad events (anything with `channel: Some(_)` is a
/// MIDI source, or an event variant HID never produces). Buttons yield their
/// press velocity; encoders/axes yield their 0-127 value. Dispatches via the
/// typed [`Control`] (variant-based), never numeric-range sniffing.
pub(crate) fn trigger_name_and_value(event: &InputEvent) -> Option<(&'static str, u8)> {
    let (control, value) = Control::from_event(event)?;
    Some((control.name()?, value))
}

/// Map a gamepad button ID to its canonical trigger name (non-trigger buttons
/// only — the L2/R2 trigger ids 143/144 are resolved in [`Control::name`], the
/// single home for the digital↔analog trigger alias).
pub(crate) fn button_name(pad: u8) -> Option<&'static str> {
    use conductor_core::gamepad_events::button_ids::*;
    Some(match pad {
        SOUTH => "south",
        EAST => "east",
        WEST => "west",
        NORTH => "north",
        DPAD_UP => "dpad_up",
        DPAD_DOWN => "dpad_down",
        DPAD_LEFT => "dpad_left",
        DPAD_RIGHT => "dpad_right",
        LEFT_SHOULDER => "left_shoulder",
        RIGHT_SHOULDER => "right_shoulder",
        LEFT_THUMB => "left_thumb",
        RIGHT_THUMB => "right_thumb",
        START => "start",
        SELECT => "select",
        GUIDE => "guide",
        // 143/144 (digital trigger) → resolved in Control::name (alias home).
        _ => return None,
    })
}

/// Map a gamepad encoder ID to its canonical trigger name (sticks only — the
/// L2/R2 trigger axes 132/133 are resolved in [`Control::name`], the single
/// home for the digital↔analog trigger alias).
pub(crate) fn encoder_name(encoder: u8) -> Option<&'static str> {
    use conductor_core::gamepad_events::encoder_ids::*;
    Some(match encoder {
        LEFT_STICK_X => "left_stick_x",
        LEFT_STICK_Y => "left_stick_y",
        RIGHT_STICK_X => "right_stick_x",
        RIGHT_STICK_Y => "right_stick_y",
        // 132/133 (analog trigger) → resolved in Control::name (alias home).
        _ => return None,
    })
}

/// Scale a 7-bit gamepad value (0-127) to an 8-bit level (0-255).
/// 0 → 0, 127 → 255, 64 → 128. The `debug_assert + clamp` defends against an
/// upstream ingest bug producing out-of-range values (Council R1 / P5 slice 5).
pub(crate) fn scale_7bit_to_8bit(v: u8) -> u8 {
    debug_assert!(
        v <= 127,
        "HID value out of 7-bit range: {v}; gamepad ingest normalizes \
         to 0-127 — this is a regression in the ingest path"
    );
    let clamped = v.min(127);
    ((u16::from(clamped) * 255) / 127) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use conductor_core::gamepad_events::{button_ids, encoder_ids};
    use std::time::Instant;

    #[test]
    fn button_event_resolves_to_name_and_velocity() {
        let ev = InputEvent::PadPressed {
            pad: button_ids::SOUTH,
            velocity: 100,
            channel: None,
            time: Instant::now(),
        };
        assert_eq!(trigger_name_and_value(&ev), Some(("south", 100)));
    }

    #[test]
    fn encoder_event_resolves_to_name_and_value() {
        let ev = InputEvent::EncoderTurned {
            encoder: encoder_ids::LEFT_STICK_X,
            value: 64,
            channel: None,
            analog: None,
            time: Instant::now(),
        };
        assert_eq!(trigger_name_and_value(&ev), Some(("left_stick_x", 64)));
    }

    #[test]
    fn midi_sourced_event_is_not_a_gamepad_trigger() {
        // channel: Some(_) ⇒ MIDI source, never a HID trigger.
        let ev = InputEvent::PadPressed {
            pad: 60,
            velocity: 100,
            channel: Some(0),
            time: Instant::now(),
        };
        assert_eq!(trigger_name_and_value(&ev), None);
    }

    #[test]
    fn scaler_endpoints() {
        assert_eq!(scale_7bit_to_8bit(0), 0);
        assert_eq!(scale_7bit_to_8bit(127), 255);
        assert_eq!(scale_7bit_to_8bit(64), 128);
    }

    // ---- ADR-039-B typed control namespace (#2437, ex-ADR-047 D7) ----

    #[test]
    fn control_from_button_event_is_typed_button() {
        let ev = InputEvent::PadPressed {
            pad: button_ids::SOUTH,
            velocity: 100,
            channel: None,
            time: Instant::now(),
        };
        assert_eq!(
            Control::from_event(&ev),
            Some((Control::Button(button_ids::SOUTH), 100))
        );
    }

    #[test]
    fn control_from_axis_event_is_typed_axis() {
        let ev = InputEvent::EncoderTurned {
            encoder: encoder_ids::RIGHT_STICK_X,
            value: 64,
            channel: None,
            analog: None,
            time: Instant::now(),
        };
        assert_eq!(
            Control::from_event(&ev),
            Some((Control::Axis(encoder_ids::RIGHT_STICK_X), 64))
        );
    }

    #[test]
    fn control_from_midi_event_is_none() {
        // channel: Some(_) ⇒ MIDI source, never a typed HID control.
        let ev = InputEvent::PadPressed {
            pad: 60,
            velocity: 100,
            channel: Some(0),
            time: Instant::now(),
        };
        assert_eq!(Control::from_event(&ev), None);
    }

    #[test]
    fn l2_r2_digital_and_analog_alias_to_one_name_intentionally() {
        // The digital trigger button (143/144) and the analog trigger axis
        // (132/133) are the SAME physical control delivered two ways; both must
        // resolve to one logical name so a config/transform targets it once.
        assert_eq!(
            Control::Button(button_ids::LEFT_TRIGGER).name(),
            Control::Axis(encoder_ids::LEFT_TRIGGER).name(),
        );
        assert_eq!(
            Control::Button(button_ids::LEFT_TRIGGER).name(),
            Some("left_trigger")
        );
        assert_eq!(
            Control::Button(button_ids::RIGHT_TRIGGER).name(),
            Control::Axis(encoder_ids::RIGHT_TRIGGER).name(),
        );
        assert_eq!(
            Control::Button(button_ids::RIGHT_TRIGGER).name(),
            Some("right_trigger")
        );
        // sanity: the two id spaces really are distinct numbers (143≠132).
        assert_ne!(button_ids::LEFT_TRIGGER, encoder_ids::LEFT_TRIGGER);
        // single-source invariant: the underlying id→name maps do NOT carry the
        // trigger names — the alias is resolved only in Control::name.
        assert_eq!(button_name(button_ids::LEFT_TRIGGER), None);
        assert_eq!(button_name(button_ids::RIGHT_TRIGGER), None);
        assert_eq!(encoder_name(encoder_ids::LEFT_TRIGGER), None);
        assert_eq!(encoder_name(encoder_ids::RIGHT_TRIGGER), None);
    }

    #[test]
    fn control_name_none_for_unmapped_id() {
        assert_eq!(Control::Button(200).name(), None);
        assert_eq!(Control::Axis(200).name(), None);
    }
}
