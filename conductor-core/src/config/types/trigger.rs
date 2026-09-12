// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Modes, mappings, and the Trigger enum.

use super::*;

/// Input mode selection for device management
///
/// Determines which input protocols are active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
pub enum InputMode {
    /// Use MIDI device only
    MidiOnly,
    /// Use gamepad/HID device only
    GamepadOnly,
    /// Use both MIDI and gamepad simultaneously (default for best compatibility)
    #[default]
    Both,
}

/// A mode defines a set of mappings that can be switched between at runtime
///
/// Each mode has its own mapping set and optional visual identifier (color).
/// Users can switch between modes using special triggers (e.g., encoder rotation).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Mode {
    /// Mode name (used for mode switching triggers)
    pub name: String,
    /// Optional color for visual identification (e.g., "blue", "green", "#FF0000")
    pub color: Option<String>,
    /// Mappings active only in this mode
    #[serde(default)]
    pub mappings: Vec<Mapping>,
}

/// A mapping connects a MIDI trigger to an action
///
/// When a trigger is detected, the associated action is executed.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Mapping {
    /// The MIDI trigger that activates this mapping
    pub trigger: Trigger,
    /// The action to execute when the trigger is detected
    pub action: ActionConfig,
    /// Optional human-readable description of this mapping
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// ADR-038: fire the action AND let the event continue to the route stage.
    ///
    /// Does NOT cause the event to match other mappings — first-match-wins is
    /// preserved. `let_through` is metadata on the winning mapping, consumed only
    /// at the event pump's route-disposition gate. Default `false` (swallow, the
    /// pre-ADR-038 behaviour).
    ///
    /// Skipped from serialization when `false` so a default mapping serializes
    /// byte-identically to a pre-ADR-038 config — keeping the feature purely
    /// additive and the canonical-serialise golden hash stable. (Deviation from
    /// the spec's literal `#[serde(default)]`-only attribute; see
    /// `docs/let-through/ADR-038-implementation-spec.md` §4.1.)
    #[serde(default, skip_serializing_if = "is_false")]
    pub let_through: bool,
}

/// MIDI message type filter for `Trigger::Raw` (ADR-030 D3)
///
/// Restricts which MIDI message types a Raw trigger matches. When the
/// filter list is empty, all MIDI message types match.
///
/// Distinct from `crate::actions::MidiMessageType`, which describes the
/// payload of a `SendMidi` action (different variant set: `ControlChange`
/// vs. `CC`, no `ChannelPressure`/`SysEx`). Kept separate to avoid
/// breaking action serialization while giving Raw filters their own
/// vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub enum MidiMessageType {
    NoteOn,
    NoteOff,
    CC,
    ProgramChange,
    Aftertouch,
    PitchBend,
    ChannelPressure,
    /// Polyphonic aftertouch. Distinct from `Aftertouch`
    /// (channel-wide) because Raw filters / overlap detection
    /// must be able to discriminate per-note pressure from
    /// channel-pressure events.
    PolyAftertouch,
    SysEx,
}

/// MIDI trigger types
///
/// Defines different ways a MIDI message can activate a mapping.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum Trigger {
    /// Basic note trigger with optional velocity threshold
    ///
    /// # Examples
    /// ```toml
    /// [trigger]
    /// type = "Note"
    /// note = 60
    /// velocity_min = 1
    /// ```
    Note {
        /// MIDI note number (0-127)
        note: u8,
        /// Minimum velocity to trigger (0-127), None = any velocity
        velocity_min: Option<u8>,
        /// MIDI channel filter (0-indexed: 0-15), None = match any channel
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel: Option<u8>,
        /// Device filter: only match events from this device alias (ADR-009)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },

    /// Velocity-sensitive trigger with different actions per velocity level
    ///
    /// Classifies note presses into soft, medium, and hard based on velocity thresholds.
    /// Used with `VelocityRange` action type for velocity-dependent behavior.
    VelocityRange {
        /// MIDI note number (0-127)
        note: u8,
        /// Maximum velocity for soft (default 40), velocities below this are soft
        soft_max: Option<u8>,
        /// Maximum velocity for medium (default 80), velocities below this are medium (after soft_max)
        medium_max: Option<u8>,
        /// MIDI channel filter (0-indexed: 0-15), None = match any channel
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel: Option<u8>,
        /// Device filter (ADR-009)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },

    /// Long press detection (hold threshold in ms)
    ///
    /// Triggers when a note is held for longer than the specified duration.
    LongPress {
        /// MIDI note number (0-127)
        note: u8,
        /// Duration in milliseconds to trigger long press (default 2000ms)
        duration_ms: Option<u64>,
        /// MIDI channel filter (0-indexed: 0-15), None = match any channel
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel: Option<u8>,
        /// Device filter (ADR-009)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },

    /// Double-tap detection
    ///
    /// Triggers when a note is pressed and released quickly twice within a time window.
    DoubleTap {
        /// MIDI note number (0-127)
        note: u8,
        /// Time window in milliseconds for detecting double-tap (default 300ms)
        timeout_ms: Option<u64>,
        /// MIDI channel filter (0-indexed: 0-15), None = match any channel
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel: Option<u8>,
        /// Device filter (ADR-009)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },

    /// Chord detection (multiple notes pressed simultaneously)
    ///
    /// Triggers when all specified notes are pressed within a narrow time window.
    NoteChord {
        /// List of MIDI note numbers that form this chord
        notes: Vec<u8>,
        /// Time window in milliseconds for detecting simultaneous presses (default 50ms)
        timeout_ms: Option<u64>,
        /// MIDI channel filter (0-indexed: 0-15), None = match any channel
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel: Option<u8>,
        /// Device filter (ADR-009)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },

    /// Encoder turn with direction
    ///
    /// Triggers on continuous controller (CC) messages from encoder/knob rotation.
    /// Can filter by direction (clockwise/counter-clockwise) or respond to both.
    EncoderTurn {
        /// Control Change number (0-127)
        cc: u8,
        /// Direction filter: "Clockwise", "CounterClockwise", or None for either
        direction: Option<String>,
        /// MIDI channel filter (0-indexed: 0-15), None = match any channel
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel: Option<u8>,
        /// Device filter (ADR-009)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },

    /// Aftertouch/pressure sensitivity
    ///
    /// Triggers based on channel pressure (aftertouch) values.
    Aftertouch {
        /// Minimum pressure value to trigger (0-127)
        pressure_min: Option<u8>,
        /// MIDI channel filter (0-indexed: 0-15), None = match any channel
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel: Option<u8>,
        /// Device filter (ADR-009)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },

    /// Polyphonic aftertouch (per-note pressure).
    ///
    /// Distinct from `Aftertouch` (channel-wide). Matches MIDI
    /// status `0xA0` events on a SPECIFIC note. Native to MPE
    /// controllers (Roli Seaboard, Linnstrument, MPK Mini Plus).
    PolyAftertouch {
        /// Note number (0-127) the trigger fires on. Required —
        /// channel-wide poly aftertouch is meaningless; the
        /// whole point of poly is per-note discrimination.
        note: u8,
        /// Minimum pressure value to trigger (0-127). `None`
        /// fires on any pressure (including 0 / finger-release).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pressure_min: Option<u8>,
        /// MIDI channel filter (0-indexed: 0-15), `None` = any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel: Option<u8>,
        /// Device filter (ADR-009).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },

    /// Pitch bend
    ///
    /// Triggers based on pitch bend messages from touch strips or pitch bend wheels.
    PitchBend {
        /// Minimum value range (0-16383)
        value_min: Option<u16>,
        /// Maximum value range (0-16383)
        value_max: Option<u16>,
        /// MIDI channel filter (0-indexed: 0-15), None = match any channel
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel: Option<u8>,
        /// Device filter (ADR-009)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },

    /// Control Change (generic CC)
    ///
    /// Triggers on any control change message matching the specified CC number.
    CC {
        /// Control Change number (0-127)
        cc: u8,
        /// Minimum value to trigger (0-127)
        value_min: Option<u8>,
        /// MIDI channel filter (0-indexed: 0-15), None = match any channel
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel: Option<u8>,
        /// Device filter (ADR-009)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },

    /// Program Change (ADR-025 Phase 1)
    ///
    /// Triggers on PC messages. Primary use case is multi-function
    /// expression pedals (FCB1010-style) that send a PC on stomp and
    /// then send CCs that should route differently per preset.
    ProgramChange {
        /// Specific program number (0-127), or `None` to match any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pc: Option<u8>,
        /// MIDI channel filter (0-indexed: 0-15), `None` = any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel: Option<u8>,
        /// Device filter.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },

    // ===== Gamepad Triggers =====
    /// Gamepad button press
    ///
    /// Triggers when a gamepad button is pressed. Button IDs use the range 128-255
    /// to avoid conflicts with MIDI note numbers (0-127).
    ///
    /// # Examples
    /// ```toml
    /// [trigger]
    /// type = "GamepadButton"
    /// button = 128  # South button (A/Cross/B)
    /// velocity_min = 1
    /// ```
    GamepadButton {
        /// Gamepad button ID (128-255)
        /// Face buttons: 128-131 (South/East/West/North)
        /// D-Pad: 132-135 (Up/Down/Left/Right)
        /// Shoulders: 136-137 (L1/R1)
        /// Stick clicks: 138-139 (L3/R3)
        /// Menu buttons: 140-142 (Start/Select/Guide)
        /// Trigger buttons: 143-144 (L2/R2 digital)
        button: u8,
        /// Minimum velocity to trigger (0-127), None = any velocity
        velocity_min: Option<u8>,
        /// Device filter (ADR-009)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },

    /// Gamepad button chord (multiple buttons pressed simultaneously)
    ///
    /// Triggers when all specified gamepad buttons are pressed within a narrow time window.
    /// Similar to NoteChord but for gamepad buttons.
    ///
    /// # Examples
    /// ```toml
    /// [trigger]
    /// type = "GamepadButtonChord"
    /// buttons = [128, 129]  # South + East (A+B / Cross+Circle)
    /// timeout_ms = 50
    /// ```
    GamepadButtonChord {
        /// List of gamepad button IDs that form this chord (128-255)
        buttons: Vec<u8>,
        /// Time window in milliseconds for detecting simultaneous presses (default 50ms)
        timeout_ms: Option<u64>,
        /// Device filter (ADR-009)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },

    /// Gamepad analog stick movement
    ///
    /// Triggers on analog stick axis movement. Axis IDs use the range 128-131:
    /// - 128: Left stick X-axis
    /// - 129: Left stick Y-axis
    /// - 130: Right stick X-axis
    /// - 131: Right stick Y-axis
    ///
    /// # Examples
    /// ```toml
    /// [trigger]
    /// type = "GamepadAnalogStick"
    /// axis = 128  # Left stick X-axis
    /// direction = "Clockwise"  # Moving right
    /// ```
    GamepadAnalogStick {
        /// Analog stick axis ID (128-131)
        axis: u8,
        /// Direction filter: "Clockwise" (right/up), "CounterClockwise" (left/down), or None for either
        direction: Option<String>,
        /// Device filter (ADR-009)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },

    /// Gamepad analog trigger pull
    ///
    /// Triggers on analog trigger (L2/R2) pull. Trigger IDs:
    /// - 132: Left trigger (L2/LT)
    /// - 133: Right trigger (R2/RT)
    ///
    /// # Examples
    /// ```toml
    /// [trigger]
    /// type = "GamepadTrigger"
    /// trigger = 132  # Left trigger (L2/LT)
    /// threshold = 64  # Minimum pull value (0-127)
    /// ```
    GamepadTrigger {
        /// Analog trigger ID (132-133)
        trigger: u8,
        /// Minimum pull value to trigger (0-127), None = any value
        threshold: Option<u8>,
        /// Device filter (ADR-009)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },

    /// OSC message with an exact address match (ADR-039-A).
    ///
    /// Matches an inbound OSC message whose address equals `address` exactly.
    /// OSC-origin events carry a network-listener taint: sensitive actions
    /// (`Shell`/`Launch`/`Keystroke`, incl. statically nested) are refused +
    /// audited unless the originating endpoint sets
    /// `allow_sensitive_actions = true` (ADR-042 D17).
    ///
    /// # Examples
    /// ```toml
    /// [trigger]
    /// type = "OscMessage"
    /// address = "/eos/go"
    /// ```
    OscMessage {
        /// Exact OSC address (must start with '/').
        address: String,
        /// Device filter: the OSC listener endpoint alias (recommended —
        /// without it the trigger matches messages from any OSC listener).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },

    /// OSC address pattern trigger (ADR-039-A).
    ///
    /// Native OSC 1.0 wildcards — `?` (one char), `*` (within a `/` part),
    /// `[a-z]`/`[!…]` (char class), `{a,b}` (alternation) — NOT regex.
    /// Validated and compiled at config-load (`osc_pattern::OscPattern`).
    ///
    /// # Examples
    /// ```toml
    /// [trigger]
    /// type = "OscAddressPattern"
    /// pattern = "/eos/fader/*"
    /// ```
    OscAddressPattern {
        /// OSC 1.0 address pattern (must start with '/').
        pattern: String,
        /// Device filter: the OSC listener endpoint alias.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },

    /// OSC argument range trigger (ADR-039-A).
    ///
    /// Matches any inbound OSC message whose argument at `arg_index` is
    /// numeric (Int or Float) and within `min..=max`. Combine with a
    /// `device` filter to scope to one listener; for address + value
    /// conditions prefer an `OscAddressPattern` mapping whose action is
    /// `Conditional`.
    OscArgRange {
        /// Zero-based argument index.
        arg_index: usize,
        /// Inclusive lower bound.
        min: f32,
        /// Inclusive upper bound.
        max: f32,
        /// Device filter: the OSC listener endpoint alias.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },
}

impl Trigger {
    /// Get the device filter from any trigger variant (ADR-009)
    pub fn device(&self) -> Option<&String> {
        match self {
            Trigger::Note { device, .. }
            | Trigger::VelocityRange { device, .. }
            | Trigger::LongPress { device, .. }
            | Trigger::DoubleTap { device, .. }
            | Trigger::NoteChord { device, .. }
            | Trigger::EncoderTurn { device, .. }
            | Trigger::Aftertouch { device, .. }
            | Trigger::PolyAftertouch { device, .. }
            | Trigger::PitchBend { device, .. }
            | Trigger::CC { device, .. }
            | Trigger::ProgramChange { device, .. }
            | Trigger::GamepadButton { device, .. }
            | Trigger::GamepadButtonChord { device, .. }
            | Trigger::GamepadAnalogStick { device, .. }
            | Trigger::GamepadTrigger { device, .. }
            | Trigger::OscMessage { device, .. }
            | Trigger::OscAddressPattern { device, .. }
            | Trigger::OscArgRange { device, .. } => device.as_ref(),
        }
    }

    /// Set the device filter on any trigger variant (used for alias cascading).
    pub fn set_device(&mut self, new_device: Option<String>) {
        match self {
            Trigger::Note { device, .. }
            | Trigger::VelocityRange { device, .. }
            | Trigger::LongPress { device, .. }
            | Trigger::DoubleTap { device, .. }
            | Trigger::NoteChord { device, .. }
            | Trigger::EncoderTurn { device, .. }
            | Trigger::Aftertouch { device, .. }
            | Trigger::PolyAftertouch { device, .. }
            | Trigger::PitchBend { device, .. }
            | Trigger::CC { device, .. }
            | Trigger::ProgramChange { device, .. }
            | Trigger::GamepadButton { device, .. }
            | Trigger::GamepadButtonChord { device, .. }
            | Trigger::GamepadAnalogStick { device, .. }
            | Trigger::GamepadTrigger { device, .. }
            | Trigger::OscMessage { device, .. }
            | Trigger::OscAddressPattern { device, .. }
            | Trigger::OscArgRange { device, .. } => *device = new_device,
        }
    }

    /// Get the MIDI channel filter from any trigger variant.
    /// Returns None for gamepad triggers (they have no MIDI channel).
    pub fn channel(&self) -> Option<u8> {
        match self {
            Trigger::Note { channel, .. }
            | Trigger::VelocityRange { channel, .. }
            | Trigger::LongPress { channel, .. }
            | Trigger::DoubleTap { channel, .. }
            | Trigger::NoteChord { channel, .. }
            | Trigger::EncoderTurn { channel, .. }
            | Trigger::Aftertouch { channel, .. }
            | Trigger::PolyAftertouch { channel, .. }
            | Trigger::PitchBend { channel, .. }
            | Trigger::CC { channel, .. }
            | Trigger::ProgramChange { channel, .. } => *channel,
            // Gamepad and OSC triggers have no MIDI channel
            Trigger::GamepadButton { .. }
            | Trigger::GamepadButtonChord { .. }
            | Trigger::GamepadAnalogStick { .. }
            | Trigger::GamepadTrigger { .. }
            | Trigger::OscMessage { .. }
            | Trigger::OscAddressPattern { .. }
            | Trigger::OscArgRange { .. } => None,
        }
    }

    /// Returns `true` iff `self`'s match set is a (non-strict) superset
    /// of `other`'s — i.e., every event that fires `other` also fires `self`.
    /// Used by the validator to detect shadowed mappings: if mapping A appears
    /// before mapping B in the same mode and `A.trigger.shadows(&B.trigger)`,
    /// B will never fire because the rule engine matches first-match-wins.
    ///
    /// **Conservative scope.** Only the four trigger types most often
    /// involved in shadow bugs are analysed (Note, CC, Aftertouch,
    /// PolyAftertouch). Cross-type pairs and unanalyzed variants
    /// (LongPress, DoubleTap, NoteChord, EncoderTurn, PitchBend,
    /// ProgramChange, Gamepad*, Raw) return `false` rather than risk a
    /// false-positive warning. Follow-up subset analysis for the remaining
    /// variants can extend this method without changing the validator
    /// wiring.
    pub fn shadows(&self, other: &Trigger) -> bool {
        // device_match: self's filter is broader iff it accepts everything
        // (None) or matches the same alias other requires.
        fn device_covers(broad: Option<&String>, strict: Option<&String>) -> bool {
            match (broad, strict) {
                (None, _) => true,
                (Some(_), None) => false,
                (Some(a), Some(b)) => a == b,
            }
        }
        // channel_match: same shape as device.
        fn channel_covers(broad: Option<u8>, strict: Option<u8>) -> bool {
            match (broad, strict) {
                (None, _) => true,
                (Some(_), None) => false,
                (Some(a), Some(b)) => a == b,
            }
        }
        // numeric-min cover: A covers B iff A's threshold is ≤ B's, treating
        // None as 0 (matches anything ≥ 0). E.g. Note{velocity_min=None}
        // covers Note{velocity_min=80}; Note{velocity_min=10} covers
        // Note{velocity_min=10} but NOT Note{velocity_min=5}.
        fn min_covers(broad: Option<u8>, strict: Option<u8>) -> bool {
            broad.unwrap_or(0) <= strict.unwrap_or(0)
        }
        match (self, other) {
            (
                Trigger::Note {
                    note: a_note,
                    velocity_min: a_vmin,
                    channel: a_ch,
                    device: a_dev,
                },
                Trigger::Note {
                    note: b_note,
                    velocity_min: b_vmin,
                    channel: b_ch,
                    device: b_dev,
                },
            ) => {
                a_note == b_note
                    && min_covers(*a_vmin, *b_vmin)
                    && channel_covers(*a_ch, *b_ch)
                    && device_covers(a_dev.as_ref(), b_dev.as_ref())
            }
            (
                Trigger::CC {
                    cc: a_cc,
                    value_min: a_vmin,
                    channel: a_ch,
                    device: a_dev,
                },
                Trigger::CC {
                    cc: b_cc,
                    value_min: b_vmin,
                    channel: b_ch,
                    device: b_dev,
                },
            ) => {
                a_cc == b_cc
                    && min_covers(*a_vmin, *b_vmin)
                    && channel_covers(*a_ch, *b_ch)
                    && device_covers(a_dev.as_ref(), b_dev.as_ref())
            }
            (
                Trigger::Aftertouch {
                    pressure_min: a_pmin,
                    channel: a_ch,
                    device: a_dev,
                },
                Trigger::Aftertouch {
                    pressure_min: b_pmin,
                    channel: b_ch,
                    device: b_dev,
                },
            ) => {
                min_covers(*a_pmin, *b_pmin)
                    && channel_covers(*a_ch, *b_ch)
                    && device_covers(a_dev.as_ref(), b_dev.as_ref())
            }
            (
                Trigger::PolyAftertouch {
                    note: a_note,
                    pressure_min: a_pmin,
                    channel: a_ch,
                    device: a_dev,
                },
                Trigger::PolyAftertouch {
                    note: b_note,
                    pressure_min: b_pmin,
                    channel: b_ch,
                    device: b_dev,
                },
            ) => {
                a_note == b_note
                    && min_covers(*a_pmin, *b_pmin)
                    && channel_covers(*a_ch, *b_ch)
                    && device_covers(a_dev.as_ref(), b_dev.as_ref())
            }
            // Cross-type pairs and unanalyzed variants are conservatively NOT
            // flagged as shadowing in v1.
            _ => false,
        }
    }
}
