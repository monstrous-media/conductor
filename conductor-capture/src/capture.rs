// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Capture session management
//!
//! Handles recording input events from MIDI and gamepad devices.
//!
//! Note: This module is in early development - types defined but not fully used yet.

// Allow unused code during development phase
#![allow(dead_code)]

use chrono::{DateTime, Utc};
use conductor_core::events::InputEvent;
use serde::{Deserialize, Serialize};
use std::time::Instant;
use uuid::Uuid;

use crate::privacy::PrivacyLevel;

/// Serializable version of InputEvent (without Instant which can't be serialized)
///
/// This type mirrors `InputEvent` but removes the `time: Instant` field since
/// `CapturedEvent` already stores the relative timestamp in `timestamp_ms`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SerializableInputEvent {
    /// Pad pressed (physical button/pad on controller)
    PadPressed { pad: u8, velocity: u8 },
    /// Pad released (button/pad released)
    PadReleased { pad: u8 },
    /// Encoder turned (rotary knob or encoder)
    EncoderTurned { encoder: u8, value: u8 },
    /// Polyphonic aftertouch/pressure applied to specific pad
    PolyPressure { pad: u8, pressure: u8 },
    /// Aftertouch/pressure applied (channel-wide)
    Aftertouch { pressure: u8 },
    /// Pitch bend/touch strip moved
    PitchBend { value: u16 },
    /// Program change
    ProgramChange { program: u8 },
    /// Generic control change (for unmapped controls)
    ControlChange { control: u8, value: u8 },
}

impl From<InputEvent> for SerializableInputEvent {
    fn from(event: InputEvent) -> Self {
        match event {
            InputEvent::PadPressed { pad, velocity, .. } => {
                SerializableInputEvent::PadPressed { pad, velocity }
            }
            InputEvent::PadReleased { pad, .. } => SerializableInputEvent::PadReleased { pad },
            InputEvent::EncoderTurned { encoder, value, .. } => {
                SerializableInputEvent::EncoderTurned { encoder, value }
            }
            InputEvent::PolyPressure { pad, pressure, .. } => {
                SerializableInputEvent::PolyPressure { pad, pressure }
            }
            InputEvent::Aftertouch { pressure, .. } => {
                SerializableInputEvent::Aftertouch { pressure }
            }
            InputEvent::PitchBend { value, .. } => SerializableInputEvent::PitchBend { value },
            InputEvent::ProgramChange { program, .. } => {
                SerializableInputEvent::ProgramChange { program }
            }
            InputEvent::ControlChange { control, value, .. } => {
                SerializableInputEvent::ControlChange { control, value }
            }
        }
    }
}

/// A capture session that records input events
#[derive(Debug)]
pub struct CaptureSession {
    /// Unique session ID
    pub id: Uuid,

    /// Session name (set when stopped)
    pub name: Option<String>,

    /// Privacy level
    pub privacy: PrivacyLevel,

    /// When the session started
    pub started_at: DateTime<Utc>,

    /// Session state
    pub state: SessionState,

    /// Captured events
    events: Vec<CapturedEvent>,

    /// Start time for relative timestamps
    start_instant: Instant,

    /// Metadata
    pub metadata: CaptureMetadata,
}

/// State of a capture session
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// Session is actively recording
    Recording,

    /// Session is paused
    Paused,

    /// Session has been stopped
    Stopped,
}

/// A captured input event with relative timestamp
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapturedEvent {
    /// Relative timestamp in milliseconds from session start
    pub timestamp_ms: u64,

    /// The input event (serializable form)
    pub event: SerializableInputEvent,

    /// Optional comment/annotation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

/// Metadata about a capture session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureMetadata {
    /// Tags for categorization
    pub tags: Vec<String>,

    /// Description
    pub description: Option<String>,

    /// Protocol(s) captured
    pub protocol: String,

    /// Total duration in milliseconds
    pub duration_ms: Option<u64>,

    /// Number of events
    pub event_count: usize,

    /// Conductor version
    pub conductor_version: String,
}

impl CaptureSession {
    /// Create a new capture session
    pub fn new(privacy: PrivacyLevel, _protocol: String, metadata: CaptureMetadata) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: None,
            privacy,
            started_at: Utc::now(),
            state: SessionState::Recording,
            events: Vec::new(),
            start_instant: Instant::now(),
            metadata,
        }
    }

    /// Record an input event
    pub fn record_event(&mut self, event: InputEvent) {
        if self.state != SessionState::Recording {
            return;
        }

        let timestamp_ms = self.start_instant.elapsed().as_millis() as u64;

        self.events.push(CapturedEvent {
            timestamp_ms,
            event: event.into(), // Convert to SerializableInputEvent
            comment: None,
        });
    }

    /// Pause the session
    pub fn pause(&mut self) {
        if self.state == SessionState::Recording {
            self.state = SessionState::Paused;
        }
    }

    /// Resume the session
    pub fn resume(&mut self) {
        if self.state == SessionState::Paused {
            self.state = SessionState::Recording;
        }
    }

    /// Stop the session and finalize
    pub fn stop(&mut self, name: String) {
        self.state = SessionState::Stopped;
        self.name = Some(name);

        // Update metadata
        self.metadata.duration_ms = Some(self.start_instant.elapsed().as_millis() as u64);
        self.metadata.event_count = self.events.len();
    }

    /// Get the captured events
    pub fn events(&self) -> &[CapturedEvent] {
        &self.events
    }

    /// Get duration in milliseconds
    pub fn duration_ms(&self) -> u64 {
        self.start_instant.elapsed().as_millis() as u64
    }
}

impl Default for CaptureMetadata {
    fn default() -> Self {
        Self {
            tags: Vec::new(),
            description: None,
            protocol: "both".to_string(),
            duration_ms: None,
            event_count: 0,
            conductor_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conductor_core::events::InputEvent;
    use std::time::Instant;

    #[test]
    fn test_new_session() {
        let session = CaptureSession::new(
            PrivacyLevel::Private,
            "midi".to_string(),
            CaptureMetadata::default(),
        );

        assert_eq!(session.state, SessionState::Recording);
        assert_eq!(session.events().len(), 0);
    }

    #[test]
    fn test_record_event() {
        let mut session = CaptureSession::new(
            PrivacyLevel::Private,
            "midi".to_string(),
            CaptureMetadata::default(),
        );

        // Record a pad press event
        session.record_event(InputEvent::PadPressed {
            pad: 60,
            velocity: 100,
            channel: None,
            time: Instant::now(),
        });

        assert_eq!(session.events().len(), 1);
    }

    #[test]
    fn test_pause_resume() {
        let mut session = CaptureSession::new(
            PrivacyLevel::Private,
            "midi".to_string(),
            CaptureMetadata::default(),
        );

        session.pause();
        assert_eq!(session.state, SessionState::Paused);

        // Events should not be recorded when paused
        session.record_event(InputEvent::PadPressed {
            pad: 60,
            velocity: 100,
            channel: None,
            time: Instant::now(),
        });
        assert_eq!(session.events().len(), 0);

        session.resume();
        assert_eq!(session.state, SessionState::Recording);

        // Events should be recorded again
        session.record_event(InputEvent::PadPressed {
            pad: 60,
            velocity: 100,
            channel: None,
            time: Instant::now(),
        });
        assert_eq!(session.events().len(), 1);
    }

    #[test]
    fn test_stop() {
        let mut session = CaptureSession::new(
            PrivacyLevel::Private,
            "midi".to_string(),
            CaptureMetadata::default(),
        );

        session.stop("test-capture".to_string());

        assert_eq!(session.state, SessionState::Stopped);
        assert_eq!(session.name, Some("test-capture".to_string()));
        assert!(session.metadata.duration_ms.is_some());
    }
}
