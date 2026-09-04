// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Gamepad event mapping to InputEvent abstraction
//!
//! This module maps gamepad/HID input events (via gilrs) to the protocol-agnostic
//! InputEvent abstraction. It uses button IDs in the 128-255 range to avoid conflicts
//! with MIDI note numbers (0-127).
//!
//! # Button ID Mapping
//!
//! - **Face buttons** (A/B/X/Y): 128-131
//! - **D-Pad**: 132-135 (Up/Down/Left/Right)
//! - **Shoulder buttons** (L1/R1): 136-137
//! - **Stick clicks** (L3/R3): 138-139
//! - **Menu buttons** (Start/Select/Guide): 140-142
//! - **Left stick axes**: Encoder 128 (X), 129 (Y)
//! - **Right stick axes**: Encoder 130 (X), 131 (Y)
//! - **Triggers** (L2/R2): Encoder 132-133 (analog), or Pad 143-144 (digital)
//!
//! # Design Principles
//!
//! - **Non-overlapping IDs**: Gamepad buttons use 128-255 to avoid MIDI 0-127 range
//! - **Analog stick normalization**: -1.0 to 1.0 → 0-127 (MIDI-compatible range)
//! - **Pressure sensitivity**: Analog triggers mapped to velocity/pressure values
//! - **Standard mapping**: Follows SDL2 GameController mappings (gilrs default)

use crate::events::InputEvent;
use std::time::Instant;

// Stream-quality filters moved to their own module; re-exported here so
// existing import paths keep working.
pub use crate::gamepad_filters::{AXIS_ZERO_SUPPRESS_WINDOW, AxisZeroFilter, TriggerNoiseGate};

/// Gamepad button ID ranges (128-255, non-overlapping with MIDI 0-127)
pub mod button_ids {
    // Face buttons (128-131)
    pub const SOUTH: u8 = 128; // A (Xbox), Cross (PS), B (Nintendo)
    pub const EAST: u8 = 129; // B (Xbox), Circle (PS), A (Nintendo)
    pub const WEST: u8 = 130; // X (Xbox), Square (PS), Y (Nintendo)
    pub const NORTH: u8 = 131; // Y (Xbox), Triangle (PS), X (Nintendo)

    // D-Pad (132-135)
    pub const DPAD_UP: u8 = 132;
    pub const DPAD_DOWN: u8 = 133;
    pub const DPAD_LEFT: u8 = 134;
    pub const DPAD_RIGHT: u8 = 135;

    // Shoulder buttons (136-137)
    pub const LEFT_SHOULDER: u8 = 136; // L1, LB
    pub const RIGHT_SHOULDER: u8 = 137; // R1, RB

    // Stick clicks (138-139)
    pub const LEFT_THUMB: u8 = 138; // L3
    pub const RIGHT_THUMB: u8 = 139; // R3

    // Menu buttons (140-142)
    pub const START: u8 = 140; // Start, Options, +
    pub const SELECT: u8 = 141; // Back, Share, -
    pub const GUIDE: u8 = 142; // Xbox, PS, Home

    // Trigger digital fallback (143-144)
    pub const LEFT_TRIGGER: u8 = 143; // L2, LT (digital threshold)
    pub const RIGHT_TRIGGER: u8 = 144; // R2, RT (digital threshold)

    // ADR-047 §D3b: named-but-unmapped extra buttons (reserved 145+). Fixed ids
    // (not a per-device table) → restart-stable, cross-machine portable,
    // collision-safe. Matchable today via `GamepadButton` (accepts id ≥ 128).
    pub const EXTRA_C: u8 = 145; // Button::C (e.g. Sega-style 6-button pads)
    pub const EXTRA_Z: u8 = 146; // Button::Z
}

/// Encoder/axis ID ranges for analog inputs
pub mod encoder_ids {
    // Analog stick axes (128-131)
    pub const LEFT_STICK_X: u8 = 128;
    pub const LEFT_STICK_Y: u8 = 129;
    pub const RIGHT_STICK_X: u8 = 130;
    pub const RIGHT_STICK_Y: u8 = 131;

    // Trigger axes (132-133)
    pub const LEFT_TRIGGER: u8 = 132; // L2, LT analog value
    pub const RIGHT_TRIGGER: u8 = 133; // R2, RT analog value

    // ADR-047 §D3b: d-pad reported as an axis on some backends. Fixed reserved
    // encoder ids (disjoint from the button namespace). Matchable via
    // `GamepadAnalogStick` (widened to accept these in mapping.rs).
    pub const EXTRA_DPAD_X: u8 = 147;
    pub const EXTRA_DPAD_Y: u8 = 148;
}

/// Convert gilrs button to Conductor button ID
///
/// Maps gilrs::Button to the 128-255 range for non-overlapping MIDI compatibility.
/// Returns `None` for buttons gilrs reports that have no Conductor id (ADR-047 §D3a).
/// Unmapped buttons are **not** collapsed onto a single id — previously every
/// unknown button aliased onto id 255, so e.g. both Xbox Elite paddles became
/// "button 255". Callers drop a `None` and may log it once per raw control.
///
/// # Examples
///
/// ```rust,ignore
/// use gilrs::Button;
/// use conductor_core::gamepad_events::button_to_id;
///
/// let id = button_to_id(Button::South);
/// assert_eq!(id, Some(128)); // SOUTH (A/Cross/B)
/// assert_eq!(button_to_id(Button::Unknown), None);
/// ```
pub fn button_to_id(button: gilrs::Button) -> Option<u8> {
    use gilrs::Button::*;
    match button {
        South => Some(button_ids::SOUTH),
        East => Some(button_ids::EAST),
        West => Some(button_ids::WEST),
        North => Some(button_ids::NORTH),
        DPadUp => Some(button_ids::DPAD_UP),
        DPadDown => Some(button_ids::DPAD_DOWN),
        DPadLeft => Some(button_ids::DPAD_LEFT),
        DPadRight => Some(button_ids::DPAD_RIGHT),
        LeftTrigger => Some(button_ids::LEFT_SHOULDER), // L1/LB
        RightTrigger => Some(button_ids::RIGHT_SHOULDER), // R1/RB
        LeftTrigger2 => Some(button_ids::LEFT_TRIGGER), // L2/LT digital
        RightTrigger2 => Some(button_ids::RIGHT_TRIGGER), // R2/RT digital
        LeftThumb => Some(button_ids::LEFT_THUMB),      // L3
        RightThumb => Some(button_ids::RIGHT_THUMB),    // R3
        Start => Some(button_ids::START),
        Select => Some(button_ids::SELECT),
        Mode => Some(button_ids::GUIDE),
        // ADR-047 §D3b: named-but-unmapped extras get fixed reserved ids.
        C => Some(button_ids::EXTRA_C),
        Z => Some(button_ids::EXTRA_Z),
        // ADR-047 §D3a/§D3b: truly-unknown controls stay unmapped → None (dropped
        // + logged once); never 255. gilrs can't enumerate them at connect, so
        // they are not addressable (see ADR-047 §D3b Rev 4).
        _ => None,
    }
}

/// Convert gilrs axis to Conductor encoder ID
///
/// Maps gilrs::Axis to encoder IDs for analog stick and trigger inputs.
///
/// # Examples
///
/// ```rust,ignore
/// use gilrs::Axis;
/// use conductor_core::gamepad_events::axis_to_encoder_id;
///
/// let id = axis_to_encoder_id(Axis::LeftStickX);
/// assert_eq!(id, Some(128)); // LEFT_STICK_X
/// assert_eq!(axis_to_encoder_id(Axis::DPadX), Some(147)); // d-pad-as-axis (ADR-047 §D3b)
/// assert_eq!(axis_to_encoder_id(Axis::Unknown), None); // truly-unknown → dropped
/// ```
///
/// Returns `None` only for truly-`Unknown` axes (ADR-047 §D3a — dropped + logged,
/// never the old `255` sentinel). `DPadX`/`DPadY` (d-pad reported as an axis) get
/// fixed reserved encoder ids 147/148 (ADR-047 §D3b).
pub fn axis_to_encoder_id(axis: gilrs::Axis) -> Option<u8> {
    use gilrs::Axis::*;
    match axis {
        LeftStickX => Some(encoder_ids::LEFT_STICK_X),
        LeftStickY => Some(encoder_ids::LEFT_STICK_Y),
        RightStickX => Some(encoder_ids::RIGHT_STICK_X),
        RightStickY => Some(encoder_ids::RIGHT_STICK_Y),
        LeftZ => Some(encoder_ids::LEFT_TRIGGER), // L2/LT analog
        RightZ => Some(encoder_ids::RIGHT_TRIGGER), // R2/RT analog
        // ADR-047 §D3b: d-pad-as-axis gets fixed reserved encoder ids.
        DPadX => Some(encoder_ids::EXTRA_DPAD_X),
        DPadY => Some(encoder_ids::EXTRA_DPAD_Y),
        // ADR-047 §D3a/§D3b: truly-unknown axes stay unmapped → None (dropped +
        // logged once), never 255; not enumerable at connect, so not addressable.
        _ => None,
    }
}

/// Default dead zone for analog sticks (10% of full range)
pub const DEFAULT_STICK_DEADZONE: f32 = 0.1;

/// Default dead zone for analog triggers (10% of full range)
pub const DEFAULT_TRIGGER_DEADZONE: f32 = 0.1;

/// Normalize analog axis value to MIDI-compatible range
///
/// Converts gilrs axis values (-1.0 to 1.0) to MIDI-compatible 0-127 range.
/// Uses the default dead zone (10%) to reduce drift noise.
///
/// # Arguments
///
/// * `value` - Raw axis value from gilrs (-1.0 to 1.0)
///
/// # Returns
///
/// MIDI-compatible value (0-127), with 64 as center
///
/// # Examples
///
/// ```rust
/// use conductor_core::gamepad_events::normalize_axis;
///
/// assert_eq!(normalize_axis(0.0), 64); // Center
/// assert_eq!(normalize_axis(1.0), 127); // Max right/up
/// assert_eq!(normalize_axis(-1.0), 0); // Max left/down
/// assert_eq!(normalize_axis(0.05), 64); // Deadzone (< 0.1)
/// let half = normalize_axis(0.5);
/// assert!(half > 80 && half < 100); // Rescaled after dead zone
/// ```
pub fn normalize_axis(value: f32) -> u8 {
    normalize_axis_with_deadzone(value, DEFAULT_STICK_DEADZONE)
}

/// Normalize analog axis value with configurable dead zone
///
/// Converts gilrs axis values (-1.0 to 1.0) to MIDI-compatible 0-127 range.
/// Values within the dead zone snap to center (64). Values outside are
/// rescaled so the usable range maps smoothly to 0-127.
///
/// # Arguments
///
/// * `value` - Raw axis value from gilrs (-1.0 to 1.0)
/// * `deadzone` - Dead zone threshold (0.0 to 1.0), e.g. 0.1 for 10%
///
/// # Returns
///
/// MIDI-compatible value (0-127), with 64 as center
pub fn normalize_axis_with_deadzone(value: f32, deadzone: f32) -> u8 {
    // Clamp deadzone to prevent division by zero or inverted behavior
    let deadzone = deadzone.clamp(0.0, 0.999);

    // Apply dead zone - return center (64) if within dead zone
    if value.abs() < deadzone {
        return 64;
    }

    // Rescale: remap the range [deadzone..1.0] to [0.0..1.0]
    // This avoids a jump from center to a non-center value at the dead zone edge
    let sign = value.signum();
    let rescaled = (value.abs() - deadzone) / (1.0 - deadzone);
    let rescaled_signed = sign * rescaled;

    // Map -1.0..1.0 → 0..127, with 64 as center
    let normalized = ((rescaled_signed + 1.0) * 63.5).round() as i32;
    normalized.clamp(0, 127) as u8
}

/// Normalize analog trigger value with configurable threshold
///
/// Converts gilrs trigger values (0.0 to 1.0) to MIDI-compatible 0-127 range.
/// Values below the threshold return 0. Values above are rescaled to 0-127.
///
/// # Arguments
///
/// * `value` - Raw trigger value from gilrs (0.0 to 1.0)
/// * `threshold` - Dead zone threshold (0.0 to 1.0), e.g. 0.1 for 10%
///
/// # Returns
///
/// MIDI-compatible value (0-127)
pub fn normalize_trigger(value: f32, threshold: f32) -> u8 {
    // Clamp threshold to prevent division by zero
    let threshold = threshold.clamp(0.0, 0.999);

    if value < threshold {
        return 0;
    }

    // Rescale: remap [threshold..1.0] to [0.0..1.0]
    let rescaled = (value - threshold) / (1.0 - threshold);
    let normalized = (rescaled * 127.0).round() as i32;
    normalized.clamp(0, 127) as u8
}

/// Convert gilrs ButtonPressed event to InputEvent
///
/// Maps gamepad button press to PadPressed with velocity 100 (default press strength).
/// Future enhancement: pressure-sensitive buttons could vary velocity.
///
/// # Arguments
///
/// * `button` - gilrs Button that was pressed
///
/// # Returns
///
/// `Some(InputEvent::PadPressed)` with button ID in 128-255 range, or `None`
/// when the gilrs button has no Conductor id (ADR-047 §D3a — dropped, not 255).
pub fn button_pressed_to_input(button: gilrs::Button) -> Option<InputEvent> {
    Some(InputEvent::PadPressed {
        pad: button_to_id(button)?,
        velocity: 100, // Default velocity for digital buttons
        channel: None,
        time: Instant::now(),
    })
}

/// Convert gilrs ButtonReleased event to InputEvent
///
/// Maps gamepad button release to PadReleased.
///
/// # Arguments
///
/// * `button` - gilrs Button that was released
///
/// # Returns
///
/// `Some(InputEvent::PadReleased)` with button ID in 128-255 range, or `None`
/// when the gilrs button has no Conductor id (ADR-047 §D3a — dropped, not 255).
pub fn button_released_to_input(button: gilrs::Button) -> Option<InputEvent> {
    Some(InputEvent::PadReleased {
        pad: button_to_id(button)?,
        channel: None,
        time: Instant::now(),
    })
}

/// Convert a gilrs ButtonChanged event for an analog trigger to InputEvent
///
/// On several gilrs backends (notably macOS IOKit), analog trigger travel is
/// delivered as `EventType::ButtonChanged(LeftTrigger2 | RightTrigger2, value)`
/// rather than `AxisChanged(LeftZ | RightZ, …)`. Without this conversion,
/// triggers only ever register as digital button presses and the analog
/// `gamepad_trigger` stream (encoders 132-133) never exists on those backends.
///
/// Returns `None` for every other button — their analog value (if any) has no
/// encoder mapping; the digital ButtonPressed/ButtonReleased path covers them.
///
/// # Arguments
///
/// * `button` - gilrs Button whose analog value changed
/// * `value` - Raw trigger value from gilrs (0.0 to 1.0)
/// * `trigger_deadzone` - Dead zone threshold for the quantised MIDI value
///
/// # Returns
///
/// `Some(InputEvent::EncoderTurned)` with encoder 132/133, the deadzone-quantised
/// MIDI value, and the raw value preserved in `analog`; `None` for non-trigger buttons.
pub fn trigger_button_changed_to_input(
    button: gilrs::Button,
    value: f32,
    trigger_deadzone: f32,
) -> Option<InputEvent> {
    let encoder = match button {
        gilrs::Button::LeftTrigger2 => encoder_ids::LEFT_TRIGGER,
        gilrs::Button::RightTrigger2 => encoder_ids::RIGHT_TRIGGER,
        _ => return None,
    };
    Some(InputEvent::EncoderTurned {
        encoder,
        value: normalize_trigger(value, trigger_deadzone),
        channel: None,
        // Preserve the raw value for high-precision value bars.
        analog: Some(value),
        time: Instant::now(),
    })
}

/// Convert gilrs AxisChanged event to InputEvent
///
/// Maps analog stick and trigger movements to EncoderTurned events.
/// Normalizes values to MIDI-compatible 0-127 range using default dead zones.
///
/// # Arguments
///
/// * `axis` - gilrs Axis that changed
/// * `value` - Raw axis value (-1.0 to 1.0 for sticks, 0.0 to 1.0 for triggers)
///
/// # Returns
///
/// `Some(InputEvent::EncoderTurned)` with normalized value (0-127), or `None`
/// when the gilrs axis has no Conductor encoder id (ADR-047 §D3a — dropped, not 255).
pub fn axis_changed_to_input(axis: gilrs::Axis, value: f32) -> Option<InputEvent> {
    axis_changed_to_input_with_deadzone(
        axis,
        value,
        DEFAULT_STICK_DEADZONE,
        DEFAULT_TRIGGER_DEADZONE,
    )
}

/// Convert gilrs AxisChanged event to InputEvent with configurable dead zones
///
/// Uses stick dead zone for analog sticks and trigger dead zone for L2/R2 triggers.
/// Returns `None` for axes with no Conductor encoder id (ADR-047 §D3a).
pub fn axis_changed_to_input_with_deadzone(
    axis: gilrs::Axis,
    value: f32,
    stick_deadzone: f32,
    trigger_deadzone: f32,
) -> Option<InputEvent> {
    let encoder = axis_to_encoder_id(axis)?;
    let normalized = match axis {
        gilrs::Axis::LeftZ | gilrs::Axis::RightZ => {
            // Triggers: 0.0 to 1.0 range
            normalize_trigger(value, trigger_deadzone)
        }
        _ => {
            // Sticks: -1.0 to 1.0 range
            normalize_axis_with_deadzone(value, stick_deadzone)
        }
    };
    Some(InputEvent::EncoderTurned {
        encoder,
        value: normalized,
        channel: None,
        // Preserve the raw gilrs value (pre-deadzone, pre-quantise) so
        // the GUI can render high-precision value bars.
        analog: Some(value),
        time: Instant::now(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_button_id_ranges() {
        // Face buttons
        assert_eq!(button_ids::SOUTH, 128);
        assert_eq!(button_ids::NORTH, 131);

        // D-Pad
        assert_eq!(button_ids::DPAD_UP, 132);
        assert_eq!(button_ids::DPAD_RIGHT, 135);

        // Shoulders
        assert_eq!(button_ids::LEFT_SHOULDER, 136);
        assert_eq!(button_ids::RIGHT_SHOULDER, 137);

        // Stick clicks
        assert_eq!(button_ids::LEFT_THUMB, 138);
        assert_eq!(button_ids::RIGHT_THUMB, 139);

        // Menu
        assert_eq!(button_ids::START, 140);
        assert_eq!(button_ids::GUIDE, 142);

        // Triggers
        assert_eq!(button_ids::LEFT_TRIGGER, 143);
        assert_eq!(button_ids::RIGHT_TRIGGER, 144);
    }

    #[test]
    fn test_encoder_id_ranges() {
        // Stick axes
        assert_eq!(encoder_ids::LEFT_STICK_X, 128);
        assert_eq!(encoder_ids::RIGHT_STICK_Y, 131);

        // Trigger axes
        assert_eq!(encoder_ids::LEFT_TRIGGER, 132);
        assert_eq!(encoder_ids::RIGHT_TRIGGER, 133);
    }

    #[test]
    fn test_button_to_id() {
        use gilrs::Button::*;

        assert_eq!(button_to_id(South), Some(128));
        assert_eq!(button_to_id(East), Some(129));
        assert_eq!(button_to_id(DPadUp), Some(132));
        assert_eq!(button_to_id(LeftTrigger), Some(136)); // L1/LB
        assert_eq!(button_to_id(LeftThumb), Some(138)); // L3
        assert_eq!(button_to_id(Start), Some(140));
    }

    /// ADR-047 §D3a: unmapped buttons return `None` (dropped), never the old
    /// `255` collision sentinel — and distinct unmapped buttons do not collide.
    #[test]
    fn test_button_to_id_unmapped_is_none() {
        use gilrs::Button::*;

        assert_eq!(button_to_id(Unknown), None);
        // No mapped button ever yields 255 (the frozen legacy sentinel).
        for b in [
            South,
            East,
            West,
            North,
            DPadUp,
            DPadDown,
            DPadLeft,
            DPadRight,
            LeftTrigger,
            RightTrigger,
            LeftTrigger2,
            RightTrigger2,
            LeftThumb,
            RightThumb,
            Start,
            Select,
            Mode,
        ] {
            assert_ne!(button_to_id(b), Some(255), "{b:?} must not map to 255");
        }
    }

    #[test]
    fn test_axis_to_encoder_id() {
        use gilrs::Axis::*;

        assert_eq!(axis_to_encoder_id(LeftStickX), Some(128));
        assert_eq!(axis_to_encoder_id(LeftStickY), Some(129));
        assert_eq!(axis_to_encoder_id(RightStickX), Some(130));
        assert_eq!(axis_to_encoder_id(RightStickY), Some(131));
        assert_eq!(axis_to_encoder_id(LeftZ), Some(132)); // L2 analog
        assert_eq!(axis_to_encoder_id(RightZ), Some(133)); // R2 analog
    }

    /// ADR-047 §D3b: only truly-`Unknown` axes return `None` (dropped, never 255).
    /// `DPadX`/`DPadY` now map to fixed reserved encoder ids 147/148 — covered by
    /// `test_named_extras_have_fixed_reserved_ids`.
    #[test]
    fn test_axis_to_encoder_id_unmapped_is_none() {
        use gilrs::Axis::*;

        // ADR-047 §D3b: only truly-unknown axes stay None (DPadX/DPadY now mapped).
        assert_eq!(axis_to_encoder_id(Unknown), None);
    }

    /// ADR-047 §D3b: named-but-unmapped extras get fixed reserved ids.
    #[test]
    fn test_named_extras_have_fixed_reserved_ids() {
        use gilrs::{Axis, Button};

        // Fixed values — restart-stable AND cross-machine portable.
        assert_eq!(button_to_id(Button::C), Some(145));
        assert_eq!(button_to_id(Button::Z), Some(146));
        assert_eq!(axis_to_encoder_id(Axis::DPadX), Some(147));
        assert_eq!(axis_to_encoder_id(Axis::DPadY), Some(148));
        // Reserved ids are distinct and in the free 145+ range.
        let ids = [145u8, 146, 147, 148];
        for id in ids {
            assert!(id >= 145, "extras live in the reserved 145+ range");
        }
        // Truly-unknown stays unaddressable.
        assert_eq!(button_to_id(Button::Unknown), None);
        assert_eq!(axis_to_encoder_id(Axis::Unknown), None);
    }

    /// ADR-047 §D3a: converters propagate `None` so an unmapped control is
    /// dropped at the source rather than emitted as a phantom id-255 event.
    #[test]
    fn test_converters_drop_unmapped_controls() {
        use gilrs::{Axis, Button};

        // Only truly-unknown controls drop (ADR-047 §D3b: C/Z/DPadX/DPadY now mapped).
        assert!(button_pressed_to_input(Button::Unknown).is_none());
        assert!(button_released_to_input(Button::Unknown).is_none());
        assert!(axis_changed_to_input(Axis::Unknown, 1.0).is_none());
        assert!(
            axis_changed_to_input_with_deadzone(Axis::Unknown, 1.0, 0.1, 0.1).is_none(),
            "unmapped axis must not produce an encoder event"
        );
        // The named extras DO produce events now.
        assert!(button_pressed_to_input(Button::C).is_some());
        assert!(axis_changed_to_input(Axis::DPadX, 1.0).is_some());
    }

    #[test]
    fn test_normalize_axis() {
        // Center position
        assert_eq!(normalize_axis(0.0), 64);

        // Max positions
        assert_eq!(normalize_axis(1.0), 127);
        assert_eq!(normalize_axis(-1.0), 0);

        // Half positions (rescaled after dead zone)
        let half_right = normalize_axis(0.5);
        assert!(
            half_right > 80 && half_right < 100,
            "Half right got {}",
            half_right
        );
        let half_left = normalize_axis(-0.5);
        assert!(
            half_left > 20 && half_left < 45,
            "Half left got {}",
            half_left
        );

        // Deadzone (< 0.1)
        assert_eq!(normalize_axis(0.05), 64);
        assert_eq!(normalize_axis(-0.08), 64);

        // Outside deadzone — rescaled value should be above center
        let outside = normalize_axis(0.3);
        assert!(
            outside > 64 && outside < 90,
            "Outside deadzone got {}",
            outside
        );
    }

    #[test]
    fn test_button_pressed_event() {
        use gilrs::Button;

        let event = button_pressed_to_input(Button::South).expect("South is mapped");

        match event {
            InputEvent::PadPressed { pad, velocity, .. } => {
                assert_eq!(pad, 128); // SOUTH
                assert_eq!(velocity, 100); // Default velocity
            }
            _ => panic!("Expected PadPressed"),
        }
    }

    #[test]
    fn test_button_released_event() {
        use gilrs::Button;

        let event = button_released_to_input(Button::East).expect("East is mapped");

        match event {
            InputEvent::PadReleased { pad, .. } => {
                assert_eq!(pad, 129); // EAST
            }
            _ => panic!("Expected PadReleased"),
        }
    }

    #[test]
    fn test_axis_changed_event() {
        use gilrs::Axis;

        let event = axis_changed_to_input(Axis::LeftStickX, 0.5).expect("LeftStickX is mapped");

        match event {
            InputEvent::EncoderTurned { encoder, value, .. } => {
                assert_eq!(encoder, 128); // LEFT_STICK_X
                assert!(value > 80 && value < 100, "Normalized 0.5 got {}", value);
            }
            _ => panic!("Expected EncoderTurned"),
        }
    }

    #[test]
    fn test_normalize_axis_configurable_deadzone() {
        // Default dead zone is 10% (0.1)
        assert_eq!(normalize_axis_with_deadzone(0.05, 0.1), 64); // Inside dead zone
        assert_eq!(normalize_axis_with_deadzone(-0.08, 0.1), 64); // Inside dead zone

        // Custom 15% dead zone
        assert_eq!(normalize_axis_with_deadzone(0.12, 0.15), 64); // Inside 15% dead zone
        assert_ne!(normalize_axis_with_deadzone(0.35, 0.15), 64); // Clearly outside 15% dead zone

        // Zero dead zone passes everything through
        assert_ne!(normalize_axis_with_deadzone(0.1, 0.0), 64);

        // Extremes still work
        assert_eq!(normalize_axis_with_deadzone(1.0, 0.1), 127);
        assert_eq!(normalize_axis_with_deadzone(-1.0, 0.1), 0);
    }

    #[test]
    fn test_normalize_axis_rescales_after_deadzone() {
        // After dead zone, values should be rescaled so that just outside
        // dead zone maps near center, not to the raw converted value.
        // With 0.1 dead zone: value 0.1 should map close to center (64),
        // not to ((0.1+1.0)*63.5) = 70
        let just_outside = normalize_axis_with_deadzone(0.11, 0.1);
        assert!(
            (64..=66).contains(&just_outside),
            "Just outside dead zone should map near center, got {}",
            just_outside
        );

        let halfway = normalize_axis_with_deadzone(0.55, 0.1);
        assert!(
            halfway > 80 && halfway < 100,
            "Halfway should be in upper range, got {}",
            halfway
        );
    }

    #[test]
    fn test_normalize_trigger_with_threshold() {
        // Triggers go 0.0 to 1.0 (not -1.0 to 1.0)
        // Below threshold = 0, above = scaled 0-127
        assert_eq!(normalize_trigger(0.0, 0.1), 0); // No pull
        assert_eq!(normalize_trigger(0.05, 0.1), 0); // Below threshold
        assert_eq!(normalize_trigger(1.0, 0.1), 127); // Full pull

        let half_pull = normalize_trigger(0.5, 0.1);
        assert!(
            half_pull > 40 && half_pull < 80,
            "Half pull should be mid-range, got {}",
            half_pull
        );
    }

    #[test]
    fn test_deadzone_division_by_zero_safety() {
        // deadzone = 1.0 should not panic (division by zero)
        let result = normalize_axis_with_deadzone(0.5, 1.0);
        assert!(result <= 127); // Just shouldn't panic

        // deadzone > 1.0 should be clamped
        let result = normalize_axis_with_deadzone(0.5, 2.0);
        assert!(result <= 127);

        // Negative deadzone should be clamped to 0
        let result = normalize_axis_with_deadzone(0.5, -0.5);
        assert!(result > 64);

        // Same for triggers
        let result = normalize_trigger(0.5, 1.0);
        assert!(result <= 127);
    }

    #[test]
    fn test_axis_changed_trigger_uses_trigger_deadzone() {
        use gilrs::Axis;

        // Trigger axis should use trigger dead zone, not stick dead zone
        let event = axis_changed_to_input_with_deadzone(
            Axis::LeftZ,
            0.05,
            0.0,
            0.1, // stick_dz=0, trigger_dz=0.1
        )
        .expect("LeftZ is mapped");
        match event {
            InputEvent::EncoderTurned { value, .. } => {
                assert_eq!(
                    value, 0,
                    "Trigger below threshold should be 0, got {}",
                    value
                );
            }
            _ => panic!("Expected EncoderTurned"),
        }

        // Stick axis should use stick dead zone
        let event = axis_changed_to_input_with_deadzone(
            Axis::LeftStickX,
            0.05,
            0.1,
            0.0, // stick_dz=0.1, trigger_dz=0
        )
        .expect("LeftStickX is mapped");
        match event {
            InputEvent::EncoderTurned { value, .. } => {
                assert_eq!(
                    value, 64,
                    "Stick inside dead zone should be center, got {}",
                    value
                );
            }
            _ => panic!("Expected EncoderTurned"),
        }
    }

    #[test]
    fn test_axis_changed_preserves_raw_analog_value() {
        use gilrs::Axis;

        // Stick: raw f32 preserved exactly, even though MIDI value is quantised
        let event = axis_changed_to_input(Axis::LeftStickX, 0.5).expect("LeftStickX is mapped");
        match event {
            InputEvent::EncoderTurned { analog, .. } => {
                assert_eq!(analog, Some(0.5));
            }
            _ => panic!("Expected EncoderTurned"),
        }

        // Negative stick deflection preserved
        let event = axis_changed_to_input(Axis::LeftStickY, -0.73).expect("LeftStickY is mapped");
        match event {
            InputEvent::EncoderTurned { analog, .. } => {
                assert_eq!(analog, Some(-0.73));
            }
            _ => panic!("Expected EncoderTurned"),
        }

        // Trigger: raw value preserved even inside the deadzone (where the
        // quantised MIDI value collapses to 0)
        let event = axis_changed_to_input(Axis::LeftZ, 0.05).expect("LeftZ is mapped");
        match event {
            InputEvent::EncoderTurned { value, analog, .. } => {
                assert_eq!(value, 0, "deadzone should zero the MIDI value");
                assert_eq!(analog, Some(0.05), "raw analog must survive deadzone");
            }
            _ => panic!("Expected EncoderTurned"),
        }
    }

    #[test]
    fn test_trigger_button_changed_maps_trigger2_buttons() {
        use gilrs::Button;

        // LeftTrigger2 → encoder 132 with quantised value + raw analog
        let event = trigger_button_changed_to_input(Button::LeftTrigger2, 0.5, 0.1)
            .expect("LeftTrigger2 is an analog trigger");
        match event {
            InputEvent::EncoderTurned {
                encoder,
                value,
                analog,
                channel,
                ..
            } => {
                assert_eq!(encoder, 132);
                assert_eq!(value, normalize_trigger(0.5, 0.1));
                assert_eq!(analog, Some(0.5));
                assert_eq!(channel, None);
            }
            _ => panic!("Expected EncoderTurned"),
        }

        // RightTrigger2 → encoder 133
        let event = trigger_button_changed_to_input(Button::RightTrigger2, 1.0, 0.1)
            .expect("RightTrigger2 is an analog trigger");
        match event {
            InputEvent::EncoderTurned { encoder, value, .. } => {
                assert_eq!(encoder, 133);
                assert_eq!(value, 127);
            }
            _ => panic!("Expected EncoderTurned"),
        }
    }

    #[test]
    fn test_trigger_button_changed_preserves_raw_inside_deadzone() {
        use gilrs::Button;

        let event = trigger_button_changed_to_input(Button::LeftTrigger2, 0.05, 0.1)
            .expect("LeftTrigger2 is an analog trigger");
        match event {
            InputEvent::EncoderTurned { value, analog, .. } => {
                assert_eq!(value, 0, "deadzone should zero the MIDI value");
                assert_eq!(analog, Some(0.05), "raw analog must survive deadzone");
            }
            _ => panic!("Expected EncoderTurned"),
        }
    }

    #[test]
    fn test_trigger_button_changed_ignores_non_trigger_buttons() {
        use gilrs::Button;

        for b in [
            Button::South,
            Button::LeftTrigger,  // L1 shoulder — digital
            Button::RightTrigger, // R1 shoulder — digital
            Button::DPadUp,
            Button::Start,
        ] {
            assert!(
                trigger_button_changed_to_input(b, 0.7, 0.1).is_none(),
                "{:?} must not produce an analog trigger event",
                b
            );
        }
    }

    #[test]
    fn test_default_deadzone_constant() {
        assert_eq!(DEFAULT_STICK_DEADZONE, 0.1);
        assert_eq!(DEFAULT_TRIGGER_DEADZONE, 0.1);
    }

    #[test]
    fn test_non_overlapping_with_midi() {
        // Ensure all gamepad button IDs are >= 128 (outside MIDI 0-127 range)
        use gilrs::Button::*;

        let buttons = vec![
            South,
            East,
            West,
            North,
            DPadUp,
            DPadDown,
            DPadLeft,
            DPadRight,
            LeftTrigger,
            RightTrigger,
            LeftTrigger2,
            RightTrigger2,
            LeftThumb,
            RightThumb,
            Start,
            Select,
            Mode,
        ];

        for button in buttons {
            let id = button_to_id(button).expect("all listed buttons are mapped");
            assert!(
                id >= 128,
                "Button {:?} has ID {} which overlaps with MIDI range (0-127)",
                button,
                id
            );
        }
    }
}
