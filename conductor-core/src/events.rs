// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Event type definitions
//!
//! This module defines the event types used throughout the Conductor engine:
//!
//! - [`MidiEvent`]: Raw MIDI protocol events (defined in event_processor.rs)
//! - [`InputEvent`]: Protocol-agnostic input events for abstraction layer
//! - [`ProcessedEvent`]: High-level processed events with timing/state detection
//!
//! The [`InputEvent`] enum provides a protocol-agnostic abstraction over MIDI events,
//! using domain terminology (pad, encoder, pressure) instead of MIDI-specific terms
//! (note, cc). This enables future support for HID and other input protocols.

use std::time::Instant;

pub use crate::event_processor::{EncoderDirection, MidiEvent, ProcessedEvent, VelocityLevel};

/// Protocol-agnostic input event abstraction
///
/// InputEvent provides a protocol-independent representation of controller inputs,
/// abstracting away MIDI-specific terminology. This enables the system to support
/// multiple input protocols (MIDI, HID, etc.) with a unified interface.
///
/// # Design Principles
///
/// - **Protocol-agnostic naming**: Uses domain terms (pad, encoder) not protocol terms (note, cc)
/// - **Timestamp preservation**: All events carry their original timestamp
/// - **Direct mapping**: One-to-one correspondence with MIDI events for now
/// - **Future extensibility**: Designed to support HID and other protocols later
///
/// # Examples
///
/// ```rust
/// use conductor_core::events::{InputEvent, MidiEvent};
/// use std::time::Instant;
///
/// let time = Instant::now();
/// let midi = MidiEvent::NoteOn { note: 36, velocity: 100, channel: 0, time };
/// let input: InputEvent = midi.into();
///
/// match input {
///     InputEvent::PadPressed { pad, velocity, .. } => {
///         println!("Pad {} pressed with velocity {}", pad, velocity);
///     }
///     _ => {}
/// }
/// ```
#[derive(Debug, Clone, PartialEq)]
pub enum InputEvent {
    /// Pad pressed (physical button/pad on controller)
    PadPressed {
        pad: u8,
        velocity: u8,
        /// MIDI channel (0-indexed, None for non-MIDI sources like gamepads)
        channel: Option<u8>,
        time: Instant,
    },
    /// Pad released (button/pad released)
    PadReleased {
        pad: u8,
        /// MIDI channel (0-indexed, None for non-MIDI sources)
        channel: Option<u8>,
        time: Instant,
    },
    /// Encoder turned (rotary knob or encoder)
    EncoderTurned {
        encoder: u8,
        value: u8,
        /// MIDI channel (0-indexed, None for non-MIDI sources)
        channel: Option<u8>,
        /// Raw analog value from HID input, before MIDI quantisation (#599).
        /// `GamepadAnalogStick` (encoders 128-131): -1.0 to +1.0 (center 0.0).
        /// `GamepadTrigger` (encoders 132-133): 0.0 to 1.0 (released 0.0).
        /// `None` for MIDI encoders.
        analog: Option<f32>,
        time: Instant,
    },
    /// Polyphonic aftertouch/pressure applied to specific pad
    PolyPressure {
        pad: u8,
        pressure: u8,
        /// MIDI channel (0-indexed, None for non-MIDI sources)
        channel: Option<u8>,
        time: Instant,
    },
    /// Aftertouch/pressure applied (channel-wide)
    Aftertouch {
        pressure: u8,
        /// MIDI channel (0-indexed, None for non-MIDI sources)
        channel: Option<u8>,
        time: Instant,
    },
    /// Pitch bend/touch strip moved
    PitchBend {
        value: u16,
        /// MIDI channel (0-indexed, None for non-MIDI sources)
        channel: Option<u8>,
        time: Instant,
    },
    /// Program change
    ProgramChange {
        program: u8,
        /// MIDI channel (0-indexed, None for non-MIDI sources)
        channel: Option<u8>,
        time: Instant,
    },
    /// Generic control change (for unmapped controls)
    ControlChange {
        control: u8,
        value: u8,
        /// MIDI channel (0-indexed, None for non-MIDI sources)
        channel: Option<u8>,
        time: Instant,
    },
}

impl InputEvent {
    /// Returns the timestamp of this event
    ///
    /// # Examples
    ///
    /// ```rust
    /// use conductor_core::events::InputEvent;
    /// use std::time::Instant;
    ///
    /// let time = Instant::now();
    /// let event = InputEvent::PadPressed { pad: 1, velocity: 100, channel: None, time };
    /// assert_eq!(event.timestamp(), time);
    /// ```
    pub fn timestamp(&self) -> Instant {
        match self {
            InputEvent::PadPressed { time, .. }
            | InputEvent::PadReleased { time, .. }
            | InputEvent::EncoderTurned { time, .. }
            | InputEvent::PolyPressure { time, .. }
            | InputEvent::Aftertouch { time, .. }
            | InputEvent::PitchBend { time, .. }
            | InputEvent::ProgramChange { time, .. }
            | InputEvent::ControlChange { time, .. } => *time,
        }
    }

    /// Returns the event type as a string
    ///
    /// Useful for debugging, logging, and display purposes.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use conductor_core::events::InputEvent;
    /// use std::time::Instant;
    ///
    /// let event = InputEvent::PadPressed {
    ///     pad: 1,
    ///     velocity: 100,
    ///     channel: None,
    ///     time: Instant::now()
    /// };
    /// assert_eq!(event.event_type(), "PadPressed");
    /// ```
    pub fn event_type(&self) -> &'static str {
        match self {
            InputEvent::PadPressed { .. } => "PadPressed",
            InputEvent::PadReleased { .. } => "PadReleased",
            InputEvent::EncoderTurned { .. } => "EncoderTurned",
            InputEvent::PolyPressure { .. } => "PolyPressure",
            InputEvent::Aftertouch { .. } => "Aftertouch",
            InputEvent::PitchBend { .. } => "PitchBend",
            InputEvent::ProgramChange { .. } => "ProgramChange",
            InputEvent::ControlChange { .. } => "ControlChange",
        }
    }
}

/// Convert MIDI events to protocol-agnostic input events
///
/// This conversion preserves all data while abstracting protocol-specific terminology.
/// MIDI concepts are mapped as follows:
///
/// - NoteOn → PadPressed (note → pad)
/// - NoteOff → PadReleased (note → pad)
/// - ControlChange → ControlChange (cc → control) (v4.10.9: no longer auto-converts to EncoderTurned)
/// - PolyPressure → PolyPressure (note → pad, pressure preserved)
/// - Aftertouch → Aftertouch (pressure preserved)
/// - PitchBend → PitchBend (value preserved)
/// - ProgramChange → ProgramChange (program preserved)
impl From<MidiEvent> for InputEvent {
    fn from(midi: MidiEvent) -> Self {
        match midi {
            MidiEvent::NoteOn {
                note,
                velocity,
                channel,
                time,
            } => InputEvent::PadPressed {
                pad: note,
                velocity,
                channel: Some(channel),
                time,
            },
            MidiEvent::NoteOff {
                note,
                channel,
                time,
            } => InputEvent::PadReleased {
                pad: note,
                channel: Some(channel),
                time,
            },
            // v4.10.9: CC events stay as ControlChange, not EncoderTurned
            // The EventProcessor will generate EncoderTurned for actual encoder rotation
            MidiEvent::ControlChange {
                cc,
                value,
                channel,
                time,
            } => InputEvent::ControlChange {
                control: cc,
                value,
                channel: Some(channel),
                time,
            },
            MidiEvent::PolyPressure {
                note,
                pressure,
                channel,
                time,
            } => InputEvent::PolyPressure {
                pad: note,
                pressure,
                channel: Some(channel),
                time,
            },
            MidiEvent::Aftertouch {
                pressure,
                channel,
                time,
            } => InputEvent::Aftertouch {
                pressure,
                channel: Some(channel),
                time,
            },
            MidiEvent::PitchBend {
                value,
                channel,
                time,
            } => InputEvent::PitchBend {
                value,
                channel: Some(channel),
                time,
            },
            MidiEvent::ProgramChange {
                program,
                channel,
                time,
            } => InputEvent::ProgramChange {
                program,
                channel: Some(channel),
                time,
            },
        }
    }
}

// ============================================================================
// ADR-039 Cross-Protocol Parity — shared substrate (charter, ticket #1758)
// ============================================================================
//
// `ProtocolEvent` is the protocol-tagged wrapper the unified event pump carries.
// `InputEvent` is MIDI/HID-shaped; OSC (address + typed args) and Art-Net (a
// 512-channel universe frame) do not fit it, so the charter wraps them in a
// higher enum (spec §2, "Option B"). The 512-byte DMX frame is BOXED via the
// `DmxFrame` newtype so it never inflates the common `Input` hot path (R2 P0),
// and the `Box` is hidden behind the newtype so ADR-039-C can later swap to a
// slab/arena pool without an API break (R3).
//
// These are pure data types (no runtime/channel dependency) and therefore live
// in `conductor-core`. The pump-facing `InputSource` trait lives in
// `conductor-daemon` instead (spec §4.1, R4) — it is typed on a tokio channel,
// and `core` keeps `tokio` optional to stay a runtime-free engine.
//
// The `OscInbound`/`DmxInbound` payloads here are the MINIMAL inbound shapes the
// substrate needs so the `ProtocolEvent` variants exist and have live consumers.
// The full OSC trigger/arg-coercion and Art-Net fixture design belong to the
// sub-ADRs (039-A / 039-C) and intentionally are NOT modelled here.

/// Minimal inbound OSC payload — an address pattern plus its typed argument list
/// (spec §2: "small — address + arg vec"). Stays inline in [`ProtocolEvent`];
/// only the DMX frame is boxed.
///
/// Reuses the existing [`crate::actions::OscArg`] vocabulary (shared with the
/// `OscSend` output action) rather than introducing a second one — ADR-039-A
/// extends that enum (e.g. blob/bool) when it implements the listener.
#[derive(Debug, Clone, PartialEq)]
pub struct OscInbound {
    /// OSC address pattern, e.g. `/1/fader1`.
    pub address: String,
    /// Typed arguments in wire order.
    pub args: Vec<crate::actions::OscArg>,
    /// Receipt timestamp (set by the listener).
    pub time: Instant,
}

/// Number of channels in a single DMX-512 / Art-Net universe.
pub const DMX_UNIVERSE_SIZE: usize = 512;

/// Minimal inbound Art-Net/DMX payload — one full 512-channel universe frame.
///
/// This is the large payload the charter boxes: a naïve inline
/// `Dmx(DmxInbound)` variant would inflate *every* [`ProtocolEvent`] (including
/// 3-byte MIDI events) to >512 bytes, wrecking L1 locality on the hot path
/// (R2 P0). Always wrap it in [`DmxFrame`].
#[derive(Debug, Clone, PartialEq)]
pub struct DmxInbound {
    /// Art-Net universe number.
    pub universe: u16,
    /// The 512 channel values (0..=255) for this universe.
    pub channels: [u8; DMX_UNIVERSE_SIZE],
    /// Receipt timestamp (set by the listener).
    pub time: Instant,
}

/// Boxed DMX frame newtype (R3).
///
/// Keeps the 512-byte [`DmxInbound`] off the [`ProtocolEvent`] hot path (the
/// enum carries only an 8-byte pointer for this variant) AND keeps the raw
/// `Box` off the public surface, so ADR-039-C's performance prototype can swap
/// box-per-frame for an arena/slab pool or latest-frame coalescing without an
/// API break.
#[derive(Debug, Clone, PartialEq)]
pub struct DmxFrame(Box<DmxInbound>);

impl DmxFrame {
    /// Box an inbound DMX frame.
    pub fn new(inbound: DmxInbound) -> Self {
        DmxFrame(Box::new(inbound))
    }

    /// The universe this frame targets.
    pub fn universe(&self) -> u16 {
        self.0.universe
    }

    /// The 512 channel values.
    pub fn channels(&self) -> &[u8; DMX_UNIVERSE_SIZE] {
        &self.0.channels
    }

    /// Receipt timestamp.
    pub fn time(&self) -> Instant {
        self.0.time
    }

    /// Borrow the inner payload.
    pub fn inner(&self) -> &DmxInbound {
        &self.0
    }
}

impl From<DmxInbound> for DmxFrame {
    fn from(inbound: DmxInbound) -> Self {
        DmxFrame::new(inbound)
    }
}

/// Protocol-tagged wrapper over every inbound event the unified pump carries
/// (ADR-039 substrate, spec §2).
///
/// - [`ProtocolEvent::Input`] — MIDI + HID. Keeps the existing small hot path.
/// - [`ProtocolEvent::Osc`] — small (address + arg vec), inline.
/// - [`ProtocolEvent::Dmx`] — a full universe, BOXED via [`DmxFrame`] so it does
///   not inflate the common `Input` case.
#[derive(Debug, Clone, PartialEq)]
pub enum ProtocolEvent {
    /// MIDI / HID event (the hot path; unchanged in behaviour).
    Input(InputEvent),
    /// Inbound OSC message.
    Osc(OscInbound),
    /// Inbound Art-Net/DMX universe frame (boxed).
    Dmx(DmxFrame),
}

impl ProtocolEvent {
    /// Borrow the inner [`InputEvent`] when this is the MIDI/HID variant.
    ///
    /// The #1758 pump wraps existing MIDI/HID ingress as
    /// `ProtocolEvent::Input(..)` and unwraps here before the (still
    /// `InputEvent`-shaped) route / event-processing stage.
    pub fn as_input(&self) -> Option<&InputEvent> {
        match self {
            ProtocolEvent::Input(event) => Some(event),
            _ => None,
        }
    }

    /// Consume and return the inner [`InputEvent`] when this is the MIDI/HID
    /// variant, otherwise return `self` unchanged via the `Err` arm.
    pub fn into_input(self) -> Result<InputEvent, ProtocolEvent> {
        match self {
            ProtocolEvent::Input(event) => Ok(event),
            other => Err(other),
        }
    }

    /// Stable label for the protocol tag — debugging / metrics.
    pub fn protocol_label(&self) -> &'static str {
        match self {
            ProtocolEvent::Input(_) => "Input",
            ProtocolEvent::Osc(_) => "Osc",
            ProtocolEvent::Dmx(_) => "Dmx",
        }
    }
}

impl From<InputEvent> for ProtocolEvent {
    fn from(event: InputEvent) -> Self {
        ProtocolEvent::Input(event)
    }
}

#[cfg(test)]
mod protocol_event_tests {
    use super::*;
    use std::mem::size_of;

    #[test]
    fn dmx_frame_is_boxed_off_the_hot_path() {
        // A full universe is large...
        assert!(
            size_of::<DmxInbound>() >= DMX_UNIVERSE_SIZE,
            "DmxInbound ({}) should carry a full {}-channel universe",
            size_of::<DmxInbound>(),
            DMX_UNIVERSE_SIZE
        );
        // ...but ProtocolEvent must NOT inline it (R2 P0): the Dmx variant is a
        // pointer, so the enum stays far smaller than a raw universe frame.
        assert!(
            size_of::<ProtocolEvent>() < size_of::<DmxInbound>(),
            "ProtocolEvent ({}) inlined the {}-byte DMX frame — hot-path regression",
            size_of::<ProtocolEvent>(),
            size_of::<DmxInbound>()
        );
    }

    #[test]
    fn protocol_event_input_hot_path_stays_small() {
        // The Input (MIDI/HID) variant adds only the enum tag over InputEvent;
        // the whole enum stays within ~2 cache lines. This is the acceptance
        // guard against a future variant (or un-boxing) bloating the hot path.
        assert!(
            size_of::<ProtocolEvent>() <= 128,
            "ProtocolEvent grew to {} bytes (> 128) — hot-path regression",
            size_of::<ProtocolEvent>()
        );
        assert!(
            size_of::<ProtocolEvent>() >= size_of::<InputEvent>(),
            "ProtocolEvent ({}) should be at least InputEvent ({})",
            size_of::<ProtocolEvent>(),
            size_of::<InputEvent>()
        );
    }

    #[test]
    fn input_event_round_trips_through_protocol_event() {
        let time = Instant::now();
        let input = InputEvent::PadPressed {
            pad: 36,
            velocity: 100,
            channel: Some(9),
            time,
        };
        let wrapped: ProtocolEvent = input.clone().into();
        assert_eq!(wrapped.protocol_label(), "Input");
        assert_eq!(wrapped.as_input(), Some(&input));
        assert_eq!(wrapped.into_input().unwrap(), input);
    }

    #[test]
    fn non_input_variants_do_not_unwrap_as_input() {
        let time = Instant::now();
        let osc = ProtocolEvent::Osc(OscInbound {
            address: "/1/fader1".to_string(),
            args: vec![crate::actions::OscArg::Float(0.5)],
            time,
        });
        assert_eq!(osc.protocol_label(), "Osc");
        assert!(osc.as_input().is_none());

        let dmx = ProtocolEvent::Dmx(DmxFrame::new(DmxInbound {
            universe: 0,
            channels: [0u8; DMX_UNIVERSE_SIZE],
            time,
        }));
        assert_eq!(dmx.protocol_label(), "Dmx");
        assert!(dmx.as_input().is_none());
        if let ProtocolEvent::Dmx(frame) = &dmx {
            assert_eq!(frame.universe(), 0);
            assert_eq!(frame.channels().len(), DMX_UNIVERSE_SIZE);
        }
    }

    #[test]
    fn dmx_frame_from_impl_and_all_accessors() {
        // Edge coverage: the `From<DmxInbound>` boxing path + every DmxFrame
        // accessor (universe/channels/time/inner), not just the two exercised
        // above.
        let time = Instant::now();
        let frame: DmxFrame = DmxInbound {
            universe: 3,
            channels: [7u8; DMX_UNIVERSE_SIZE],
            time,
        }
        .into();
        assert_eq!(frame.universe(), 3);
        assert_eq!(frame.channels()[0], 7);
        assert_eq!(frame.channels()[DMX_UNIVERSE_SIZE - 1], 7);
        assert_eq!(frame.time(), time);
        assert_eq!(frame.inner().universe, 3);
        // Round-trip through the ProtocolEvent::Dmx variant preserves the frame.
        let ev = ProtocolEvent::Dmx(frame);
        assert!(ev.into_input().is_err());
    }

    #[test]
    fn non_input_into_input_returns_original_variant() {
        // Edge coverage: into_input()'s Err arm hands back the *same* variant
        // (the pump's recv loop relies on this to log + drop without losing the
        // protocol tag — the cloud-review concern).
        let time = Instant::now();
        let osc = ProtocolEvent::Osc(OscInbound {
            address: "/cue/go".to_string(),
            args: vec![],
            time,
        });
        match osc.into_input() {
            Err(ProtocolEvent::Osc(inb)) => assert_eq!(inb.address, "/cue/go"),
            other => panic!("expected Err(Osc), got {other:?}"),
        }
    }
}
