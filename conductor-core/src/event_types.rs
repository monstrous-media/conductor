// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Type-safe event and pattern type definitions (v4.9.0 - ADR-004)
//!
//! This module provides strongly-typed enums for event types and pattern types,
//! replacing stringly-typed matching across daemon and GUI layers.
//!
//! These types serialize to/from snake_case strings via serde for JSON IPC compatibility.

use crate::actions::{
    Action, KeyCode, MidiMessageParams, MidiMessageType, ModifierKey, MouseButton, VolumeOperation,
};
use serde::{Deserialize, Serialize};

/// Event types emitted during MIDI Learn capture.
///
/// These represent the raw input event classification before pattern detection.
/// Serializes to snake_case strings (e.g., `EventType::NoteOn` -> `"note_on"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    // MIDI Events
    /// MIDI Note On message (velocity > 0)
    #[default]
    NoteOn,
    /// MIDI Note Off message (or Note On with velocity 0)
    NoteOff,
    /// MIDI Control Change message
    Cc,
    /// MIDI encoder/knob rotation (CC with direction)
    Encoder,
    /// MIDI Pitch Bend message
    PitchBend,
    /// MIDI Channel Aftertouch (pressure)
    Aftertouch,
    /// MIDI Polyphonic Key Pressure
    PolyPressure,
    /// MIDI Program Change (ADR-025 Phase 1 — captured by MIDI Learn so
    /// foot-controller bank stomps and similar can be detected directly).
    ProgramChange,

    // Gamepad Events (v3.0+)
    /// Gamepad button press (IDs 128-255)
    GamepadButton,
    /// Gamepad button release
    GamepadButtonRelease,
    /// Gamepad analog stick movement (left/right X/Y)
    GamepadAxis,
    /// Gamepad analog stick direction (mapped from axis)
    GamepadStick,
    /// Gamepad analog trigger pull (L2/R2)
    GamepadTrigger,
}

/// Pattern types detected by EventProcessor during MIDI Learn, plus
/// state-transition annotations emitted outside Learn mode.
///
/// These represent higher-level patterns detected from sequences of raw events.
/// Serializes to snake_case strings (e.g., `PatternType::LongPress` -> `"long_press"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PatternType {
    /// Note held for extended duration (default 2000ms)
    #[default]
    LongPress,
    /// Note held for medium duration (200-1000ms)
    MediumPress,
    /// Rapid consecutive presses of the same note (default 300ms window)
    DoubleTap,
    /// Multiple MIDI notes pressed simultaneously (IDs 0-127)
    Chord,
    /// Multiple gamepad buttons pressed simultaneously (IDs 128-255)
    GamepadChord,
    /// Active Program Change transitioned on a `(device, channel)` tuple
    /// (ADR-025 Phase 3.D). Attached to the PC MonitorEvent that caused the
    /// transition; carries the previous PC (if any) alongside the new one so
    /// the Events panel can render a `via PC X → PC Y` annotation.
    ///
    /// Unlike the Learn-gated variants above, this one is always emitted —
    /// it annotates the live routing state, which always applies.
    ContextSwitch,
}

impl EventType {
    /// Returns true if this event type is from a gamepad/HID device.
    pub fn is_gamepad(&self) -> bool {
        matches!(
            self,
            EventType::GamepadButton
                | EventType::GamepadButtonRelease
                | EventType::GamepadAxis
                | EventType::GamepadStick
                | EventType::GamepadTrigger
        )
    }

    /// Returns true if this event type is from a MIDI device.
    pub fn is_midi(&self) -> bool {
        !self.is_gamepad()
    }
}

impl PatternType {
    /// Returns true if this pattern type is from a gamepad/HID device.
    pub fn is_gamepad(&self) -> bool {
        matches!(self, PatternType::GamepadChord)
    }

    /// Returns true if this pattern type is from a MIDI device.
    pub fn is_midi(&self) -> bool {
        !self.is_gamepad()
    }
}

/// Trigger information within a MappingFiredPayload (ADR-014)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FiredTriggerInfo {
    /// Trigger type: "note", "cc", "encoder", "gamepad_button", etc.
    #[serde(rename = "type")]
    pub trigger_type: String,
    /// Device alias or port name
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// MIDI channel (0-15)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<u8>,
    /// Note/CC/button number
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<u8>,
    /// Velocity/CC value
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<u16>,
}

/// Action information within a MappingFiredPayload (ADR-014)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FiredActionInfo {
    /// Action type: "keystroke", "launch", "send_midi", etc.
    #[serde(rename = "type")]
    pub action_type: String,
    /// Human-readable summary (e.g., "Cmd+C", "Launch Spotify")
    pub summary: String,
}

/// Result of a mapping firing (ADR-014)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FiredResult {
    /// Action completed successfully
    Ok,
    /// Action failed with an error
    Error,
}

/// Structured payload emitted when a mapping fires (ADR-014 Phase 1A)
///
/// Sent on the monitor SSE channel as event_type "mapping_fired".
/// Contains full trigger/action/result context for GUI feedback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MappingFiredPayload {
    /// Trigger information
    pub trigger: FiredTriggerInfo,
    /// Action information
    pub action: FiredActionInfo,
    /// Result of the action
    pub result: FiredResult,
    /// Error message (when result is Error)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Processing latency in microseconds (end-to-end: dispatch → completion)
    pub latency_us: u64,
    /// Active mode name
    pub mode: String,
    /// Optional mapping label/description from the rule
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mapping_label: Option<String>,
    /// Timestamp in milliseconds since epoch
    pub timestamp: u64,
    /// Invocation ID for correlating mapping_matched → mapping_fired (ADR-015)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation_id: Option<u64>,
    /// Action execution time in microseconds (wall clock on executor thread) (ADR-015 D12)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_time_us: Option<u64>,
    /// ADR-025 Phase 3.A: routing breadcrumbs for context-switch actions.
    ///
    /// Empty for non-context-switch actions. When a state-based route
    /// resolves — either a `ContextSwitchTable` branch match or a
    /// lowered `Conditional` chain whose inner state-bearing condition
    /// matched — the executor appends a human-readable breadcrumb so
    /// the GUI can annotate the Events row with the route the event
    /// actually took. Exact string shape is intentionally not part of
    /// the wire contract — consumers should treat entries as opaque
    /// human text. For reference, the current executor emits forms like:
    ///
    /// * `"PC 12 on fcb1010 ch1"` (matched `Condition::ActivePcIs` /
    ///   PC `ContextSwitchTable` branch)
    /// * `"CC 1=50 ∈ [0, 63] on keyboard ch1"` (CC range match via
    ///   `ContextSwitchTable`)
    /// * `"CC 64 ≥ 64 on keyboard ch1"` (matched `Condition::CcIsOn`
    ///   sugar)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routing_trace: Vec<String>,
    /// ADR-038: whether the matched mapping set `let_through = true` (fires the
    /// action AND lets the event continue to the route stage). `#[serde(default)]`
    /// keeps older payloads (no field) deserializing to the swallow default.
    #[serde(default)]
    pub let_through: bool,
}

/// Structured payload emitted when a mapping has been accepted for dispatch (ADR-015)
///
/// Sent after a successful `try_dispatch` to the executor thread (i.e., when the mapping
/// is matched and accepted). Failed dispatches emit `mapping_dropped` instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MappingMatchedPayload {
    /// Invocation ID for correlating with mapping_fired
    pub invocation_id: u64,
    /// Trigger info
    pub trigger: FiredTriggerInfo,
    /// Action type string
    pub action_type: String,
    /// Human-readable action summary
    pub action_summary: String,
    /// Active mode name
    pub mode: String,
    /// Optional mapping label
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mapping_label: Option<String>,
    /// ADR-038: whether the matched mapping set `let_through = true` (fires the
    /// action AND lets the event continue to the route stage). Lets the Events
    /// panel badge the row as observe-and-pass-through. `#[serde(default)]` keeps
    /// older payloads (no field) deserializing to the swallow default.
    #[serde(default)]
    pub let_through: bool,
    /// Timestamp in milliseconds since epoch
    pub timestamp: u64,
}

/// Structured payload emitted when a mapping dispatch is dropped (ADR-015 D3)
///
/// Sent when the executor channel is full and the action cannot be dispatched.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MappingDroppedPayload {
    /// Invocation ID
    pub invocation_id: u64,
    /// Reason for dropping
    pub reason: String,
    /// Timestamp in milliseconds since epoch
    pub timestamp: u64,
}

/// Structured payload emitted when a mapping execution is cancelled (ADR-015 D13)
///
/// Sent when shutdown interrupts an in-flight or queued action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MappingCancelledPayload {
    /// Invocation ID
    pub invocation_id: u64,
    /// Reason for cancellation
    pub reason: String,
    /// Timestamp in milliseconds since epoch
    pub timestamp: u64,
}

/// Produce a human-readable summary of an Action (ADR-014)
///
/// Used by the GUI event stream "Fired" entries to show what action ran.
pub fn summarize_action(action: &Action) -> String {
    match action {
        Action::Keystroke { keys, modifiers } => {
            let mut parts: Vec<String> = modifiers.iter().map(format_modifier).collect();
            for key in keys {
                parts.push(format_key(key));
            }
            parts.join("+")
        }
        Action::Text(text) => {
            let truncated = truncate_str(text, 27);
            if truncated.len() < text.len() {
                format!("Type: \"{}...\"", truncated)
            } else {
                format!("Type: \"{}\"", text)
            }
        }
        Action::Launch(app) => format!("Launch {}", app),
        Action::Shell { command, args, .. } => {
            // Render the rough invocation for the events stream. The
            // argv form joins `command` + args with spaces; this is a
            // human-readable summary, not a shell-safe round-trip.
            let display = match args {
                Some(args) if !args.is_empty() => {
                    format!("{} {}", command, args.join(" "))
                }
                _ => command.clone(),
            };
            let truncated = truncate_str(&display, 27);
            if truncated.len() < display.len() {
                format!("Shell: {}...", truncated)
            } else {
                format!("Shell: {}", display)
            }
        }
        Action::ModeChange { mode } => format!("Switch to {}", mode),
        Action::VolumeControl { operation, value } => match (operation, value) {
            (VolumeOperation::Up, Some(v)) => format!("Volume +{}%", v),
            (VolumeOperation::Up, None) => "Volume Up".to_string(),
            (VolumeOperation::Down, Some(v)) => format!("Volume -{}%", v),
            (VolumeOperation::Down, None) => "Volume Down".to_string(),
            (VolumeOperation::Mute, _) => "Mute".to_string(),
            (VolumeOperation::Unmute, _) => "Unmute".to_string(),
            (VolumeOperation::Set, Some(v)) => format!("Volume {}%", v),
            (VolumeOperation::Set, None) => "Volume Set".to_string(),
        },
        Action::SendMidi {
            port,
            message_type,
            channel,
            params,
        } => {
            let msg = match (message_type, params) {
                (MidiMessageType::ControlChange, MidiMessageParams::CC { controller, .. }) => {
                    format!("CC {} ch{}", controller, channel + 1)
                }
                (MidiMessageType::NoteOn, MidiMessageParams::Note { note, .. }) => {
                    format!("Note {} ch{}", note, channel + 1)
                }
                (MidiMessageType::NoteOff, MidiMessageParams::Note { note, .. }) => {
                    format!("NoteOff {} ch{}", note, channel + 1)
                }
                (MidiMessageType::ProgramChange, MidiMessageParams::ProgramChange { program }) => {
                    format!("PC {} ch{}", program, channel + 1)
                }
                (MidiMessageType::PitchBend, MidiMessageParams::PitchBend { value }) => {
                    format!("PitchBend {} ch{}", value, channel + 1)
                }
                _ => format!("MIDI ch{}", channel + 1),
            };
            format!("Send {} → {}", msg, port)
        }
        Action::MouseClick { button, .. } => {
            let btn = match button {
                MouseButton::Left => "Left",
                MouseButton::Right => "Right",
                MouseButton::Middle => "Middle",
            };
            format!("{} Click", btn)
        }
        Action::Sequence(actions) => {
            format!("Sequence ({})", actions.len())
        }
        Action::Delay(ms) => format!("Delay {}ms", ms),
        Action::Repeat { count, .. } => format!("Repeat ×{}", count),
        Action::Conditional { .. } => "Conditional".to_string(),
        Action::Plugin { plugin, .. } => format!("Plugin: {}", plugin),
        Action::MidiForward { target, .. } => format!("Forward → {}", target),
        Action::HidForward { target, .. } => format!("HID Forward → {}", target),
        Action::OscForward { target, .. } => format!("OSC Forward → {}", target),
        Action::OscSend {
            host,
            port,
            address,
            ..
        } => {
            format!("OSC {} → {}:{}", address, host, port)
        }
        Action::ContextSwitchTable {
            kind,
            channel,
            device,
            branches,
            ..
        } => {
            use crate::actions::{ContextBranchTable, ContextKind};
            let (axis, count) = match (kind, branches) {
                (ContextKind::Pc, ContextBranchTable::Pc(m)) => ("PC".to_string(), m.len()),
                (ContextKind::Cc, ContextBranchTable::Cc { cc, ranges }) => {
                    (format!("CC{}", cc), ranges.len())
                }
                // kind / branches mismatch would be a lowering bug;
                // fall back to just printing the kind label.
                (ContextKind::Pc, _) => ("PC".to_string(), 0),
                (ContextKind::Cc, _) => ("CC?".to_string(), 0),
            };
            format!(
                "Context switch ({}): {} branches on {}/ch{}",
                axis,
                count,
                device,
                channel + 1
            )
        }
        Action::Tap { message } => {
            format!("Tap: {}", truncate_str(message, 27))
        }
    }
}

/// Truncate a string to at most `max_bytes` bytes on a valid UTF-8 char boundary.
fn truncate_str(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    // Find the last char boundary at or before max_bytes
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn format_modifier(m: &ModifierKey) -> String {
    match m {
        ModifierKey::Command => "Cmd".to_string(),
        ModifierKey::Control => "Ctrl".to_string(),
        ModifierKey::Option => "Opt".to_string(),
        ModifierKey::Shift => "Shift".to_string(),
    }
}

fn format_key(k: &KeyCode) -> String {
    match k {
        KeyCode::Unicode(c) => c.to_uppercase().to_string(),
        KeyCode::Space => "Space".to_string(),
        KeyCode::Return => "Return".to_string(),
        KeyCode::Tab => "Tab".to_string(),
        KeyCode::Escape => "Esc".to_string(),
        KeyCode::Backspace => "Backspace".to_string(),
        KeyCode::Delete => "Delete".to_string(),
        KeyCode::UpArrow => "↑".to_string(),
        KeyCode::DownArrow => "↓".to_string(),
        KeyCode::LeftArrow => "←".to_string(),
        KeyCode::RightArrow => "→".to_string(),
        KeyCode::Home => "Home".to_string(),
        KeyCode::End => "End".to_string(),
        KeyCode::PageUp => "PgUp".to_string(),
        KeyCode::PageDown => "PgDn".to_string(),
        KeyCode::F1 => "F1".to_string(),
        KeyCode::F2 => "F2".to_string(),
        KeyCode::F3 => "F3".to_string(),
        KeyCode::F4 => "F4".to_string(),
        KeyCode::F5 => "F5".to_string(),
        KeyCode::F6 => "F6".to_string(),
        KeyCode::F7 => "F7".to_string(),
        KeyCode::F8 => "F8".to_string(),
        KeyCode::F9 => "F9".to_string(),
        KeyCode::F10 => "F10".to_string(),
        KeyCode::F11 => "F11".to_string(),
        KeyCode::F12 => "F12".to_string(),
        KeyCode::F13 => "F13".to_string(),
        KeyCode::F14 => "F14".to_string(),
        KeyCode::F15 => "F15".to_string(),
        KeyCode::F16 => "F16".to_string(),
        KeyCode::F17 => "F17".to_string(),
        KeyCode::F18 => "F18".to_string(),
        KeyCode::F19 => "F19".to_string(),
        KeyCode::F20 => "F20".to_string(),
        KeyCode::VolumeUp => "VolUp".to_string(),
        KeyCode::VolumeDown => "VolDn".to_string(),
        KeyCode::Mute => "Mute".to_string(),
        KeyCode::PlayPause => "Play".to_string(),
        KeyCode::Stop => "Stop".to_string(),
        KeyCode::NextTrack => "Next".to_string(),
        KeyCode::PreviousTrack => "Prev".to_string(),
        KeyCode::Insert => "Insert".to_string(),
        KeyCode::PrintScreen => "PrtSc".to_string(),
        KeyCode::ScrollLock => "ScrLk".to_string(),
        KeyCode::Pause => "Pause".to_string(),
        KeyCode::CapsLock => "CapsLk".to_string(),
        KeyCode::NumLock => "NumLk".to_string(),
    }
}

/// Extract action type string from an Action (for FiredActionInfo)
pub fn action_type_string(action: &Action) -> &'static str {
    match action {
        Action::Keystroke { .. } => "keystroke",
        Action::Text(_) => "text",
        Action::Launch(_) => "launch",
        Action::Shell { .. } => "shell",
        Action::Sequence(_) => "sequence",
        Action::Delay(_) => "delay",
        Action::MouseClick { .. } => "mouse_click",
        Action::Repeat { .. } => "repeat",
        Action::Conditional { .. } => "conditional",
        Action::VolumeControl { .. } => "volume_control",
        Action::ModeChange { .. } => "mode_change",
        Action::SendMidi { .. } => "send_midi",
        Action::Plugin { .. } => "plugin",
        Action::MidiForward { .. } => "midi_forward",
        Action::HidForward { .. } => "hid_forward",
        Action::OscSend { .. } => "osc_send",
        Action::OscForward { .. } => "osc_forward",
        Action::ContextSwitchTable { .. } => "context_switch_table",
        Action::Tap { .. } => "tap",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // =============================================================================
    // EventType Serialization Tests (TDD - ADR-004)
    // =============================================================================

    #[test]
    fn test_event_type_serializes_to_snake_case() {
        assert_eq!(
            serde_json::to_string(&EventType::NoteOn).unwrap(),
            "\"note_on\""
        );
        assert_eq!(
            serde_json::to_string(&EventType::NoteOff).unwrap(),
            "\"note_off\""
        );
        assert_eq!(serde_json::to_string(&EventType::Cc).unwrap(), "\"cc\"");
        assert_eq!(
            serde_json::to_string(&EventType::Encoder).unwrap(),
            "\"encoder\""
        );
        assert_eq!(
            serde_json::to_string(&EventType::PitchBend).unwrap(),
            "\"pitch_bend\""
        );
        assert_eq!(
            serde_json::to_string(&EventType::Aftertouch).unwrap(),
            "\"aftertouch\""
        );
        assert_eq!(
            serde_json::to_string(&EventType::PolyPressure).unwrap(),
            "\"poly_pressure\""
        );
    }

    #[test]
    fn test_event_type_gamepad_serializes_to_snake_case() {
        assert_eq!(
            serde_json::to_string(&EventType::GamepadButton).unwrap(),
            "\"gamepad_button\""
        );
        assert_eq!(
            serde_json::to_string(&EventType::GamepadButtonRelease).unwrap(),
            "\"gamepad_button_release\""
        );
        assert_eq!(
            serde_json::to_string(&EventType::GamepadAxis).unwrap(),
            "\"gamepad_axis\""
        );
        assert_eq!(
            serde_json::to_string(&EventType::GamepadStick).unwrap(),
            "\"gamepad_stick\""
        );
        assert_eq!(
            serde_json::to_string(&EventType::GamepadTrigger).unwrap(),
            "\"gamepad_trigger\""
        );
    }

    #[test]
    fn test_event_type_deserializes_from_snake_case() {
        let note_on: EventType = serde_json::from_str("\"note_on\"").unwrap();
        assert_eq!(note_on, EventType::NoteOn);

        let gamepad_button: EventType = serde_json::from_str("\"gamepad_button\"").unwrap();
        assert_eq!(gamepad_button, EventType::GamepadButton);

        let pitch_bend: EventType = serde_json::from_str("\"pitch_bend\"").unwrap();
        assert_eq!(pitch_bend, EventType::PitchBend);
    }

    #[test]
    fn test_event_type_invalid_string_returns_error() {
        let result: Result<EventType, _> = serde_json::from_str("\"invalid_type\"");
        assert!(result.is_err());

        let result: Result<EventType, _> = serde_json::from_str("\"NoteOn\""); // Wrong case
        assert!(result.is_err());

        let result: Result<EventType, _> = serde_json::from_str("\"note-on\""); // Wrong separator
        assert!(result.is_err());
    }

    // =============================================================================
    // PatternType Serialization Tests (TDD - ADR-004)
    // =============================================================================

    #[test]
    fn test_pattern_type_serializes_to_snake_case() {
        assert_eq!(
            serde_json::to_string(&PatternType::LongPress).unwrap(),
            "\"long_press\""
        );
        assert_eq!(
            serde_json::to_string(&PatternType::DoubleTap).unwrap(),
            "\"double_tap\""
        );
        assert_eq!(
            serde_json::to_string(&PatternType::Chord).unwrap(),
            "\"chord\""
        );
        assert_eq!(
            serde_json::to_string(&PatternType::GamepadChord).unwrap(),
            "\"gamepad_chord\""
        );
        // ADR-025 Phase 3.D
        assert_eq!(
            serde_json::to_string(&PatternType::ContextSwitch).unwrap(),
            "\"context_switch\""
        );
    }

    #[test]
    fn test_pattern_type_deserializes_from_snake_case() {
        let long_press: PatternType = serde_json::from_str("\"long_press\"").unwrap();
        assert_eq!(long_press, PatternType::LongPress);

        let chord: PatternType = serde_json::from_str("\"chord\"").unwrap();
        assert_eq!(chord, PatternType::Chord);

        let gamepad_chord: PatternType = serde_json::from_str("\"gamepad_chord\"").unwrap();
        assert_eq!(gamepad_chord, PatternType::GamepadChord);

        // ADR-025 Phase 3.D
        let context_switch: PatternType = serde_json::from_str("\"context_switch\"").unwrap();
        assert_eq!(context_switch, PatternType::ContextSwitch);
    }

    #[test]
    fn test_pattern_type_invalid_string_returns_error() {
        let result: Result<PatternType, _> = serde_json::from_str("\"invalid_pattern\"");
        assert!(result.is_err());

        let result: Result<PatternType, _> = serde_json::from_str("\"LongPress\""); // Wrong case
        assert!(result.is_err());
    }

    // =============================================================================
    // Helper Method Tests
    // =============================================================================

    #[test]
    fn test_event_type_is_gamepad() {
        assert!(EventType::GamepadButton.is_gamepad());
        assert!(EventType::GamepadButtonRelease.is_gamepad());
        assert!(EventType::GamepadAxis.is_gamepad());
        assert!(EventType::GamepadStick.is_gamepad());
        assert!(EventType::GamepadTrigger.is_gamepad());

        assert!(!EventType::NoteOn.is_gamepad());
        assert!(!EventType::NoteOff.is_gamepad());
        assert!(!EventType::Cc.is_gamepad());
    }

    #[test]
    fn test_event_type_is_midi() {
        assert!(EventType::NoteOn.is_midi());
        assert!(EventType::NoteOff.is_midi());
        assert!(EventType::Cc.is_midi());
        assert!(EventType::Encoder.is_midi());
        assert!(EventType::PitchBend.is_midi());
        assert!(EventType::Aftertouch.is_midi());
        assert!(EventType::PolyPressure.is_midi());

        assert!(!EventType::GamepadButton.is_midi());
    }

    #[test]
    fn test_pattern_type_is_gamepad() {
        assert!(PatternType::GamepadChord.is_gamepad());

        assert!(!PatternType::LongPress.is_gamepad());
        assert!(!PatternType::DoubleTap.is_gamepad());
        assert!(!PatternType::Chord.is_gamepad());
        // ADR-025 Phase 3.D — context switch is a MIDI-PC transition,
        // not a gamepad pattern.
        assert!(!PatternType::ContextSwitch.is_gamepad());
    }

    #[test]
    fn test_pattern_type_is_midi() {
        assert!(PatternType::LongPress.is_midi());
        assert!(PatternType::DoubleTap.is_midi());
        assert!(PatternType::Chord.is_midi());

        assert!(!PatternType::GamepadChord.is_midi());
    }

    // =============================================================================
    // summarize_action Tests (TDD - ADR-014 Phase 1A)
    // =============================================================================

    #[test]
    fn test_summarize_keystroke_with_modifiers() {
        let action = Action::Keystroke {
            keys: vec![KeyCode::Unicode('c')],
            modifiers: vec![ModifierKey::Command],
        };
        assert_eq!(summarize_action(&action), "Cmd+C");
    }

    #[test]
    fn test_summarize_keystroke_multiple_modifiers() {
        let action = Action::Keystroke {
            keys: vec![KeyCode::Unicode('z')],
            modifiers: vec![ModifierKey::Command, ModifierKey::Shift],
        };
        assert_eq!(summarize_action(&action), "Cmd+Shift+Z");
    }

    #[test]
    fn test_summarize_keystroke_no_modifiers() {
        let action = Action::Keystroke {
            keys: vec![KeyCode::F5],
            modifiers: vec![],
        };
        assert_eq!(summarize_action(&action), "F5");
    }

    #[test]
    fn test_summarize_launch() {
        let action = Action::Launch("Spotify".to_string());
        assert_eq!(summarize_action(&action), "Launch Spotify");
    }

    #[test]
    fn test_summarize_shell_short() {
        let action = Action::Shell {
            sandbox: None,
            command: "echo hello".to_string(),
            args: None,
            timeout_ms: None,
        };
        assert_eq!(summarize_action(&action), "Shell: echo hello");
    }

    #[test]
    fn test_summarize_shell_long_truncates() {
        let action = Action::Shell {
            sandbox: None,
            command: "some-very-long-command --with-many-flags --and-arguments".to_string(),
            args: None,
            timeout_ms: None,
        };
        let summary = summarize_action(&action);
        assert!(summary.len() <= 40);
        assert!(summary.ends_with("..."));
    }

    #[test]
    fn test_summarize_mode_change() {
        let action = Action::ModeChange {
            mode: "Live".to_string(),
        };
        assert_eq!(summarize_action(&action), "Switch to Live");
    }

    #[test]
    fn test_summarize_volume_up_with_value() {
        let action = Action::VolumeControl {
            operation: VolumeOperation::Up,
            value: Some(5),
        };
        assert_eq!(summarize_action(&action), "Volume +5%");
    }

    #[test]
    fn test_summarize_volume_down_no_value() {
        let action = Action::VolumeControl {
            operation: VolumeOperation::Down,
            value: None,
        };
        assert_eq!(summarize_action(&action), "Volume Down");
    }

    #[test]
    fn test_summarize_volume_mute() {
        let action = Action::VolumeControl {
            operation: VolumeOperation::Mute,
            value: None,
        };
        assert_eq!(summarize_action(&action), "Mute");
    }

    #[test]
    fn test_summarize_send_midi_cc() {
        let action = Action::SendMidi {
            port: "Synth".to_string(),
            message_type: MidiMessageType::ControlChange,
            channel: 0,
            params: MidiMessageParams::CC {
                controller: 7,
                value: 100,
            },
        };
        assert_eq!(summarize_action(&action), "Send CC 7 ch1 → Synth");
    }

    #[test]
    fn test_summarize_send_midi_note() {
        use crate::actions::VelocityMapping;
        let action = Action::SendMidi {
            port: "Piano".to_string(),
            message_type: MidiMessageType::NoteOn,
            channel: 2,
            params: MidiMessageParams::Note {
                note: 60,
                velocity_mapping: VelocityMapping::PassThrough,
            },
        };
        assert_eq!(summarize_action(&action), "Send Note 60 ch3 → Piano");
    }

    #[test]
    fn test_summarize_mouse_click() {
        let action = Action::MouseClick {
            button: MouseButton::Left,
            x: None,
            y: None,
        };
        assert_eq!(summarize_action(&action), "Left Click");
    }

    #[test]
    fn test_summarize_sequence() {
        let action = Action::Sequence(vec![Action::Delay(100), Action::Text("hi".to_string())]);
        assert_eq!(summarize_action(&action), "Sequence (2)");
    }

    #[test]
    fn test_summarize_delay() {
        let action = Action::Delay(500);
        assert_eq!(summarize_action(&action), "Delay 500ms");
    }

    #[test]
    fn test_summarize_repeat() {
        let action = Action::Repeat {
            action: Box::new(Action::Delay(100)),
            count: 3,
            delay_ms: None,
        };
        assert_eq!(summarize_action(&action), "Repeat ×3");
    }

    #[test]
    fn test_summarize_plugin() {
        let action = Action::Plugin {
            plugin: "spotify".to_string(),
            params: serde_json::json!({}),
        };
        assert_eq!(summarize_action(&action), "Plugin: spotify");
    }

    #[test]
    fn test_summarize_midi_forward() {
        let action = Action::MidiForward {
            target: "DAW".to_string(),
            transform: None,
        };
        assert_eq!(summarize_action(&action), "Forward → DAW");
    }

    #[test]
    fn test_summarize_osc_send() {
        let action = Action::OscSend {
            host: "127.0.0.1".to_string(),
            port: 9000,
            address: "/track/1/volume".to_string(),
            args: vec![],
        };
        assert_eq!(
            summarize_action(&action),
            "OSC /track/1/volume → 127.0.0.1:9000"
        );
    }

    #[test]
    fn test_summarize_text_short() {
        let action = Action::Text("hello world".to_string());
        assert_eq!(summarize_action(&action), "Type: \"hello world\"");
    }

    #[test]
    fn test_summarize_text_long_truncates() {
        let action = Action::Text("a".repeat(50));
        let summary = summarize_action(&action);
        assert!(summary.ends_with("...\""));
        assert!(summary.len() < 50);
    }

    #[test]
    fn test_summarize_text_multibyte_no_panic() {
        // Emoji are 4 bytes each — slicing at byte 27 would land mid-char
        let action = Action::Text("🎵🎹🎶🎵🎹🎶🎵🎹🎶".to_string()); // 9 emoji = 36 bytes
        let summary = summarize_action(&action);
        assert!(summary.contains("...\""));
        // Should not panic
    }

    #[test]
    fn test_summarize_shell_multibyte_no_panic() {
        let action = Action::Shell {
            sandbox: None,
            command: "echo ñoño ñoño ñoño ñoño ñoño".to_string(),
            args: None,
            timeout_ms: None,
        };
        let summary = summarize_action(&action);
        assert!(summary.ends_with("..."));
    }

    #[test]
    fn test_summarize_conditional() {
        use crate::actions::Condition;
        let action = Action::Conditional {
            condition: Condition::Always,
            then_action: Box::new(Action::Delay(100)),
            else_action: None,
        };
        assert_eq!(summarize_action(&action), "Conditional");
    }

    // =============================================================================
    // action_type_string Tests (ADR-014)
    // =============================================================================

    #[test]
    fn test_action_type_string_coverage() {
        assert_eq!(
            action_type_string(&Action::Keystroke {
                keys: vec![],
                modifiers: vec![],
            }),
            "keystroke"
        );
        assert_eq!(action_type_string(&Action::Launch("x".into())), "launch");
        assert_eq!(
            action_type_string(&Action::Shell {
                sandbox: None,
                command: "x".into(),
                args: None,
                timeout_ms: None,
            }),
            "shell"
        );
        assert_eq!(
            action_type_string(&Action::ModeChange { mode: "x".into() }),
            "mode_change"
        );
        assert_eq!(
            action_type_string(&Action::MidiForward {
                target: "x".into(),
                transform: None,
            }),
            "midi_forward"
        );
    }

    // =============================================================================
    // MappingFiredPayload Serialization Tests (ADR-014)
    // =============================================================================

    #[test]
    fn test_mapping_fired_payload_serializes() {
        let payload = MappingFiredPayload {
            trigger: FiredTriggerInfo {
                trigger_type: "note".to_string(),
                device: Some("Mikro".to_string()),
                channel: Some(0),
                number: Some(36),
                value: Some(100),
            },
            action: FiredActionInfo {
                action_type: "keystroke".to_string(),
                summary: "Cmd+C".to_string(),
            },
            result: FiredResult::Ok,
            error: None,
            latency_us: 250,
            mode: "Default".to_string(),
            mapping_label: Some("Copy shortcut".to_string()),
            timestamp: 1700000000000,
            invocation_id: Some(42),
            execution_time_us: Some(200),
            routing_trace: Vec::new(),
            let_through: false,
        };
        let v: serde_json::Value = serde_json::to_value(&payload).unwrap();
        assert_eq!(v["trigger"]["type"], "note");
        assert_eq!(v["trigger"]["device"], "Mikro");
        assert_eq!(v["trigger"]["number"], 36);
        assert_eq!(v["trigger"]["value"], 100);
        assert_eq!(v["action"]["type"], "keystroke");
        assert_eq!(v["action"]["summary"], "Cmd+C");
        assert_eq!(v["result"], "ok");
        assert_eq!(v["latency_us"], 250);
        assert_eq!(v["mode"], "Default");
        assert_eq!(v["mapping_label"], "Copy shortcut");
        assert!(v["error"].is_null()); // None fields skipped
        assert_eq!(v["invocation_id"], 42);
        assert_eq!(v["execution_time_us"], 200);
        // Phase 3.A: empty routing_trace is omitted from the wire
        // format by `skip_serializing_if = "Vec::is_empty"`.
        assert!(v.get("routing_trace").is_none());
        // ADR-038: let_through=false must not be omitted from the wire (no skip_serializing_if).
        assert_eq!(v["let_through"], false);
    }

    #[test]
    fn test_mapping_fired_payload_error_case() {
        let payload = MappingFiredPayload {
            trigger: FiredTriggerInfo {
                trigger_type: "cc".to_string(),
                device: None,
                channel: None,
                number: Some(7),
                value: Some(127),
            },
            action: FiredActionInfo {
                action_type: "send_midi".to_string(),
                summary: "Send CC 7 ch1 → Synth".to_string(),
            },
            result: FiredResult::Error,
            error: Some("port not found".to_string()),
            latency_us: 500,
            mode: "Mix".to_string(),
            mapping_label: None,
            timestamp: 1700000000001,
            invocation_id: None,
            execution_time_us: None,
            routing_trace: Vec::new(),
            let_through: false,
        };
        let v: serde_json::Value = serde_json::to_value(&payload).unwrap();
        assert_eq!(v["result"], "error");
        assert_eq!(v["error"], "port not found");
        assert_eq!(v["trigger"]["type"], "cc");
        assert_eq!(v["trigger"]["number"], 7);
        // Optional fields with None should be skipped
        assert!(v.get("mapping_label").is_none() || v["mapping_label"].is_null());
        assert!(
            v.get("trigger").unwrap().get("device").is_none() || v["trigger"]["device"].is_null()
        );
        // invocation_id/execution_time_us with None should be skipped
        assert!(v.get("invocation_id").is_none() || v["invocation_id"].is_null());
    }

    #[test]
    fn test_mapping_fired_payload_routing_trace_serializes_and_round_trips() {
        // Companion to the empty-trace assertion above — confirms the
        // populated case actually lands on the wire with the expected
        // shape (array of strings) and survives a full round-trip
        // through serde.
        let payload = MappingFiredPayload {
            trigger: FiredTriggerInfo {
                trigger_type: "cc".to_string(),
                device: Some("fcb1010".to_string()),
                channel: Some(0),
                number: Some(7),
                value: Some(64),
            },
            action: FiredActionInfo {
                action_type: "keystroke".to_string(),
                summary: "Cmd+V".to_string(),
            },
            result: FiredResult::Ok,
            error: None,
            latency_us: 320,
            mode: "Live".to_string(),
            mapping_label: Some("Expression pedal via preset router".to_string()),
            timestamp: 1700000000002,
            invocation_id: Some(7),
            execution_time_us: Some(180),
            routing_trace: vec![
                "PC 12 on fcb1010 ch1".to_string(),
                "CC 7=50 ∈ [0, 63] on fcb1010 ch1".to_string(),
            ],
            let_through: false,
        };

        let v: serde_json::Value = serde_json::to_value(&payload).unwrap();
        let trace = v
            .get("routing_trace")
            .expect("routing_trace must be present when non-empty");
        let arr = trace
            .as_array()
            .expect("routing_trace must serialize as array");
        assert_eq!(arr.len(), 2);
        // Each entry is a JSON string; the exact human-readable shape is
        // intentionally not part of the wire contract (see field docstring),
        // so we only verify the structural type here.
        assert!(arr.iter().all(|v| v.is_string()));

        // Round-trip through serde preserves the vector values structurally,
        // which is the actual wire-contract guarantee.
        let round: MappingFiredPayload = serde_json::from_value(v).expect("round-trip");
        assert_eq!(round.routing_trace, payload.routing_trace);
    }
}
