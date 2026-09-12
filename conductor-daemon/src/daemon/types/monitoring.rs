// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Event monitoring: filters, stats, and the monitor event stream shape.

use super::*;

/// Event filter for real-time monitoring
///
/// Reusable filter that can be applied to `MonitorEvent` streams.
/// Used by both CLI (`conductorctl events`) and GUI (LiveEventConsole).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EventFilter {
    /// Filter by event type(s) — comma-separated (e.g., "note_on,note_off")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_type: Option<String>,
    /// Filter by MIDI channel (0-15)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<u8>,
    /// Filter by minimum note number (inclusive)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note_min: Option<u8>,
    /// Filter by maximum note number (inclusive)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note_max: Option<u8>,
    /// Filter by device ID
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    /// Filter events newer than this timestamp (milliseconds since epoch)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_ms: Option<u64>,
}

impl EventFilter {
    /// Check if a `MonitorEvent` matches this filter.
    /// Returns `true` if the event passes all filter criteria.
    pub fn matches(&self, event: &MonitorEvent) -> bool {
        // Event type filter
        if let Some(ref filter) = self.event_type
            && !filter.split(',').any(|f| f.trim() == event.event_type)
        {
            return false;
        }

        // Channel filter
        if let Some(ch) = self.channel {
            match event.channel {
                Some(event_ch) if event_ch == ch => {}
                Some(_) => return false,
                // If event has no channel info, don't filter it out
                None => {}
            }
        }

        // Note range filter
        if let Some(min) = self.note_min {
            match event.note {
                Some(n) if n >= min => {}
                Some(_) => return false,
                None => return false, // No note = doesn't match note range filter
            }
        }
        if let Some(max) = self.note_max {
            match event.note {
                Some(n) if n <= max => {}
                Some(_) => return false,
                None => return false,
            }
        }

        // Device ID filter
        if let Some(ref dev) = self.device_id {
            match &event.device_id {
                Some(event_dev) if event_dev == dev => {}
                Some(_) => return false,
                None => return false,
            }
        }

        // Time filter (R891)
        if let Some(since) = self.since_ms
            && event.timestamp_ms < since
        {
            return false;
        }

        true
    }
}

/// Event statistics for monitoring dashboard
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EventStats {
    /// Total events received
    pub total_events: u64,
    /// Events in the last second
    pub events_per_second: f64,
    /// Average velocity across note events
    pub avg_velocity: f64,
    /// Most frequently triggered note
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub most_active_note: Option<u8>,
    /// Count of the most active note
    pub most_active_note_count: u64,
    /// Error count
    pub error_count: u64,
}

/// Event for real-time event monitoring
///
/// A simplified, serializable event type for CLI monitoring.
/// Captures MIDI and gamepad events with device attribution.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MonitorEvent {
    /// Event timestamp in milliseconds since epoch
    pub timestamp_ms: u64,
    /// Event type string. Raw MIDI: "note_on", "note_off", "cc", "encoder",
    /// "pitch_bend", "aftertouch", "poly_pressure",
    /// "gamepad_button", "gamepad_button_release", "gamepad_axis", "gamepad_trigger".
    /// Processed gestures: "pad_pressed", "pad_released", "short_press",
    /// "medium_press", "long_press", "hold_detected", "double_tap",
    /// "chord_detected", "encoder_turn", "cc_received".
    /// Actions: "action_executed", "action_error", "mode_change".
    pub event_type: String,
    /// Source device identity (multi-device mode)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    /// MIDI channel (0-15) for raw MIDI events. `None` for non-channel events
    /// (gamepad, gestures, action results) and for variants where the source
    /// `InputEvent` happened to carry no channel.
    ///
    /// Populated by `EngineManager::create_monitor_event` for the seven MIDI
    /// `InputEvent` variants (PadPressed, PadReleased, ControlChange,
    /// EncoderTurned, PitchBend, Aftertouch, PolyPressure) when they are not
    /// re-routed to gamepad/HID surfaces by their pad/encoder ID range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<u8>,
    /// MIDI note number (0-127)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<u8>,
    /// MIDI velocity (0-127)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub velocity: Option<u8>,
    /// MIDI CC number (0-127)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cc: Option<u8>,
    /// MIDI CC or other value (0-127 for most events, 0-16383 for pitch bend)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<u16>,
    /// Gamepad button ID
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub button: Option<u8>,
    /// Gamepad axis ID
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub axis: Option<u8>,
    /// Raw analog value from HID input, before MIDI quantisation.
    /// "gamepad_axis": -1.0 to +1.0 (center 0.0).
    /// "gamepad_trigger": 0.0 to 1.0 (released 0.0).
    /// `None` for all other event types.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analog_value: Option<f32>,
    /// Human-readable detail/message (action results, errors, etc.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Structured payload for typed events (e.g., mapping_fired) (ADR-014)
    ///
    /// When present, contains the full structured data for the event type.
    /// Consumers should prefer this over parsing `detail` as JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    /// Processing time in microseconds (R919: track_latency)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub processing_us: Option<u64>,
    /// Resident memory in bytes at event time (R920: track_memory)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_bytes: Option<u64>,
    /// Canonical MIDI 1.0 wire bytes for raw channel-voice events, e.g.
    /// `[0xB0, 0x07, 0x4E]` for CC 7 = 78 on channel 1. Reconstructed from the
    /// parsed fields (the original `InputEvent` no longer carries the source
    /// bytes by the time it reaches monitoring), so it is byte-identical to the
    /// canonical form of the message rather than a literal capture (running
    /// status is expanded; note-on velocity 0 stays note-on). `None` for
    /// gamepad/gesture/action events and for channel-voice events whose source
    /// carried no channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_bytes: Option<Vec<u8>>,
    /// Monotonic emission sequence number stamped at `push_monitor_event` time.
    /// Gives a total order across every event type and both Tauri
    /// channels (`midi-events` + `mapping-fired`) so the GUI can render true
    /// daemon-emission order regardless of cross-channel delivery timing.
    /// `#[serde(default)]` keeps old payloads and the many
    /// `..Default::default()` construction sites working unchanged.
    #[serde(default)]
    pub seq: u64,
}
