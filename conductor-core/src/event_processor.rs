// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use crate::events::InputEvent; // Protocol-agnostic event processing
use midi_msg::{ChannelVoiceMsg, ControlChange, MidiMsg};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tracing::{debug, trace};

/// Default Short→Medium press classification boundary in ms. This is
/// only the *default* — the runtime boundary is `EventProcessor::short_press_threshold`,
/// set from `advanced_settings.short_press_ms` via `EventTimings`. The const is
/// kept so `EventProcessor::new()` has a named source for its initial value.
pub const SHORT_PRESS_MS: u128 = 200;
/// Press duration threshold: >= LONG_PRESS_MS → LongPress, else MediumPress (above
/// `short_press_threshold`). Hardcoded; the Short→Medium boundary is configurable
/// but Medium→Long is not.
pub const LONG_PRESS_MS: u128 = 1000;

#[derive(Debug, Clone)]
pub enum MidiEvent {
    NoteOn {
        note: u8,
        velocity: u8,
        /// MIDI channel (0-indexed: 0-15)
        channel: u8,
        time: Instant,
    },
    NoteOff {
        note: u8,
        /// MIDI channel (0-indexed: 0-15)
        channel: u8,
        time: Instant,
    },
    ControlChange {
        cc: u8,
        value: u8,
        /// MIDI channel (0-indexed: 0-15)
        channel: u8,
        time: Instant,
    },
    PolyPressure {
        note: u8,
        pressure: u8,
        /// MIDI channel (0-indexed: 0-15)
        channel: u8,
        time: Instant,
    },
    Aftertouch {
        pressure: u8,
        /// MIDI channel (0-indexed: 0-15)
        channel: u8,
        time: Instant,
    },
    PitchBend {
        value: u16,
        /// MIDI channel (0-indexed: 0-15)
        channel: u8,
        time: Instant,
    },
    ProgramChange {
        program: u8,
        /// MIDI channel (0-indexed: 0-15)
        channel: u8,
        time: Instant,
    },
}

impl MidiEvent {
    /// Parse raw MIDI bytes into a MidiEvent using the midi-msg library.
    ///
    /// This centralizes MIDI message parsing across the codebase, ensuring
    /// consistent handling of MIDI messages and providing better error messages.
    ///
    /// # Arguments
    /// * `msg` - Raw MIDI message bytes (typically 1-3 bytes)
    ///
    /// # Returns
    /// * `Ok(MidiEvent)` - Successfully parsed MIDI event with current timestamp
    /// * `Err(String)` - Error message describing why parsing failed
    ///
    /// # Example
    /// ```rust
    /// use conductor_core::event_processor::MidiEvent;
    ///
    /// // Note On: C4 (60) with velocity 100
    /// let note_on = MidiEvent::from_midi_msg(&[0x90, 60, 100]).unwrap();
    ///
    /// // Control Change: CC 7 (volume) with value 127
    /// let cc = MidiEvent::from_midi_msg(&[0xB0, 7, 127]).unwrap();
    /// ```
    pub fn from_midi_msg(msg: &[u8]) -> Result<Self, String> {
        let now = Instant::now();

        match MidiMsg::from_midi(msg) {
            Ok((
                MidiMsg::ChannelVoice {
                    msg: voice_msg,
                    channel: midi_channel,
                },
                consumed,
            ))
            | Ok((
                MidiMsg::RunningChannelVoice {
                    msg: voice_msg,
                    channel: midi_channel,
                },
                consumed,
            )) => {
                // `MidiMsg::from_midi` reports how many bytes it consumed.
                // A channel-voice message is a fixed length (2 bytes for
                // ProgramChange/ChannelPressure, 3 for the rest), so any extra
                // trailing byte the caller handed us is NOT part of this message.
                // The old code ignored `consumed`, silently dropping the extra
                // byte and accepting a malformed message; reject it instead so a
                // corrupt/over-long frame surfaces rather than being reinterpreted.
                if consumed != msg.len() {
                    return Err(format!(
                        "Malformed MIDI message: parser consumed {consumed} of {} byte(s); \
                         {} trailing byte(s) would be silently dropped",
                        msg.len(),
                        msg.len() - consumed
                    ));
                }
                let channel = midi_channel as u8;
                match voice_msg {
                    ChannelVoiceMsg::NoteOn { note, velocity } => {
                        if velocity > 0 {
                            Ok(MidiEvent::NoteOn {
                                note,
                                velocity,
                                channel,
                                time: now,
                            })
                        } else {
                            // Note On with velocity 0 is treated as Note Off
                            Ok(MidiEvent::NoteOff {
                                note,
                                channel,
                                time: now,
                            })
                        }
                    }

                    ChannelVoiceMsg::NoteOff { note, .. } => Ok(MidiEvent::NoteOff {
                        note,
                        channel,
                        time: now,
                    }),

                    ChannelVoiceMsg::ControlChange { control } => {
                        // Extract CC number and value from ControlChange enum
                        if let ControlChange::CC { control: cc, value } = control {
                            Ok(MidiEvent::ControlChange {
                                cc,
                                value,
                                channel,
                                time: now,
                            })
                        } else {
                            Err(format!("Unsupported ControlChange variant: {:?}", control))
                        }
                    }

                    ChannelVoiceMsg::PolyPressure { note, pressure } => {
                        Ok(MidiEvent::PolyPressure {
                            note,
                            pressure,
                            channel,
                            time: now,
                        })
                    }

                    ChannelVoiceMsg::ChannelPressure { pressure } => Ok(MidiEvent::Aftertouch {
                        pressure,
                        channel,
                        time: now,
                    }),

                    ChannelVoiceMsg::PitchBend { bend } => Ok(MidiEvent::PitchBend {
                        value: bend,
                        channel,
                        time: now,
                    }),

                    ChannelVoiceMsg::ProgramChange { program } => Ok(MidiEvent::ProgramChange {
                        program,
                        channel,
                        time: now,
                    }),

                    _ => Err(format!(
                        "Unsupported MIDI voice message type: {:?}",
                        voice_msg
                    )),
                }
            }

            Ok((MidiMsg::SystemCommon { .. }, _)) => {
                Err("System Common messages not supported".to_string())
            }

            Ok((MidiMsg::SystemRealTime { .. }, _)) => {
                Err("System Real-Time messages not supported".to_string())
            }

            Err(e) => Err(format!("Failed to parse MIDI message: {:?}", e)),

            _ => Err(format!("Unknown MIDI message type: {:02X?}", msg)),
        }
    }
}

#[derive(Debug, Clone)]
pub enum ProcessedEvent {
    ShortPress {
        note: u8,
        /// MIDI channel (None for gamepad events)
        channel: Option<u8>,
    },
    MediumPress {
        note: u8,
        duration_ms: u128,
        /// MIDI channel (None for gamepad events)
        channel: Option<u8>,
    },
    LongPress {
        note: u8,
        duration_ms: u128,
        /// MIDI channel (None for gamepad events)
        channel: Option<u8>,
    },
    /// Hold detected while note is still held (enriched with velocity/duration - D22)
    HoldDetected {
        note: u8,
        /// Velocity of the original press that started this hold
        press_velocity: u8,
        /// How long the note has been held (ms)
        duration_ms: u128,
        /// MIDI channel (None for gamepad events)
        channel: Option<u8>,
    },
    PadPressed {
        note: u8,
        velocity: u8,
        velocity_level: VelocityLevel,
        /// MIDI channel (None for gamepad events)
        channel: Option<u8>,
    },
    PadReleased {
        note: u8,
        hold_duration_ms: u128,
        /// MIDI channel (None for gamepad events)
        channel: Option<u8>,
    },
    EncoderTurned {
        cc: u8,
        value: u8,
        direction: EncoderDirection,
        delta: u8,
        /// MIDI channel (None for gamepad events)
        channel: Option<u8>,
    },
    /// Raw CC event received (for pedals/buttons that send fixed CC values)
    CCReceived {
        cc: u8,
        value: u8,
        /// MIDI channel (None for gamepad events)
        channel: Option<u8>,
    },
    /// Double-tap detected (enriched with velocity/timing - D22)
    DoubleTap {
        note: u8,
        /// Velocity of the first tap
        first_velocity: u8,
        /// Velocity of the second tap
        second_velocity: u8,
        /// Interval between taps in milliseconds
        interval_ms: u128,
        /// MIDI channel (None for gamepad events)
        channel: Option<u8>,
    },
    /// Chord detected (enriched with per-note velocities - D22)
    ChordDetected {
        notes: Vec<u8>,
        /// Per-note velocities (same order as notes)
        velocities: Vec<u8>,
        /// MIDI channel of the most recent note in the chord (None for gamepad)
        channel: Option<u8>,
    },
    AftertouchChanged {
        pressure: u8,
        /// MIDI channel (None for gamepad events)
        channel: Option<u8>,
    },
    /// Polyphonic aftertouch — per-note pressure.
    ///
    /// Distinct from `AftertouchChanged` (channel-wide pressure):
    /// `PolyAftertouchChanged` carries a `note` field and is matched
    /// only by `Trigger::PolyAftertouch { note, ... }`. Native to
    /// MPE controllers (Roli Seaboard, Linnstrument).
    PolyAftertouchChanged {
        note: u8,
        pressure: u8,
        /// MIDI channel (None for non-MIDI sources).
        channel: Option<u8>,
    },
    PitchBendMoved {
        value: u16,
        /// MIDI channel (None for gamepad events)
        channel: Option<u8>,
    },
    /// Program Change received (ADR-025 Phase 1).
    ///
    /// Emitted passthrough-style from `InputEvent::ProgramChange`. The
    /// `PhysicalControlStateStore` observes the raw `MidiEvent::ProgramChange`
    /// upstream; this variant is what `Trigger::ProgramChange` matches.
    ProgramChange {
        program: u8,
        /// MIDI channel (None for gamepad/non-MIDI sources)
        channel: Option<u8>,
    },
    /// Raw input event passthrough (D23)
    ///
    /// Emitted before gesture detection for forward-compatibility with
    /// future raw trigger types. Does not match existing triggers.
    Raw(InputEvent),
    /// Inbound OSC message (ADR-039-A).
    ///
    /// Produced directly from a decoded `OscInbound` — OSC has no press/hold
    /// gesture semantics, so it bypasses gesture detection. Matched only by
    /// the `OscMessage` / `OscAddressPattern` / `OscArgRange` triggers. A
    /// mapping fired from this variant MUST dispatch with the network-origin
    /// taint set (ADR-042 D17) — the OSC source is an unauthenticated
    /// network listener.
    OscReceived {
        /// The OSC address as received off the wire.
        address: String,
        /// Decoded arguments (already bounded by the OSC parser caps).
        args: Vec<crate::actions::OscArg>,
    },
}

impl ProcessedEvent {
    /// Extract the MIDI channel from any variant.
    /// Returns None for Raw passthrough (and gamepad events which set channel=None).
    pub fn channel(&self) -> Option<u8> {
        match self {
            Self::ShortPress { channel, .. }
            | Self::MediumPress { channel, .. }
            | Self::LongPress { channel, .. }
            | Self::HoldDetected { channel, .. }
            | Self::PadPressed { channel, .. }
            | Self::PadReleased { channel, .. }
            | Self::DoubleTap { channel, .. }
            | Self::EncoderTurned { channel, .. }
            | Self::CCReceived { channel, .. }
            | Self::AftertouchChanged { channel, .. }
            | Self::PolyAftertouchChanged { channel, .. }
            | Self::PitchBendMoved { channel, .. }
            | Self::ProgramChange { channel, .. }
            | Self::ChordDetected { channel, .. } => *channel,
            Self::Raw(_) | Self::OscReceived { .. } => None,
        }
    }

    /// Returns true if this event originated from a MIDI source (not gamepad).
    /// Raw triggers only match MIDI events — gamepad events have their own
    /// trigger types. (ADR-030 D3)
    pub fn is_midi(&self) -> bool {
        match self {
            // Gamepad events have note IDs >= 128
            Self::PadPressed { note, .. } | Self::PadReleased { note, .. } => *note < 128,
            Self::EncoderTurned { cc, .. } | Self::CCReceived { cc, .. } => *cc < 128,
            Self::ChordDetected { notes, .. } => notes.iter().all(|n| *n < 128),
            // These are always MIDI-only events
            Self::AftertouchChanged { .. }
            | Self::PolyAftertouchChanged { .. }
            | Self::PitchBendMoved { .. }
            | Self::ProgramChange { .. } => true,
            // Press-duration events — check note range
            Self::ShortPress { note, .. }
            | Self::MediumPress { note, .. }
            | Self::LongPress { note, .. }
            | Self::HoldDetected { note, .. }
            | Self::DoubleTap { note, .. } => *note < 128,
            // OSC events are never MIDI.
            Self::OscReceived { .. } => false,
            // Raw passthrough reflects the underlying input source.
            Self::Raw(event) => match event {
                InputEvent::PadPressed { channel, .. }
                | InputEvent::PadReleased { channel, .. }
                | InputEvent::EncoderTurned { channel, .. }
                | InputEvent::PolyPressure { channel, .. }
                | InputEvent::Aftertouch { channel, .. }
                | InputEvent::PitchBend { channel, .. }
                | InputEvent::ProgramChange { channel, .. }
                | InputEvent::ControlChange { channel, .. } => channel.is_some(),
            },
        }
    }

    /// Classify this event's MIDI message type for `Trigger::Raw` filtering.
    /// Returns `None` for non-MIDI events and for `ProcessedEvent::Raw`
    /// (the passthrough variant has no derived classification — its inner
    /// `InputEvent` is the source if a future caller needs per-Raw
    /// filtering). (ADR-030 D3)
    pub fn midi_message_type(&self) -> Option<crate::config::MidiMessageType> {
        use crate::config::MidiMessageType;
        match self {
            Self::PadPressed { note, .. } if *note < 128 => Some(MidiMessageType::NoteOn),
            Self::PadReleased { note, .. } if *note < 128 => Some(MidiMessageType::NoteOff),
            Self::EncoderTurned { cc, .. } if *cc < 128 => Some(MidiMessageType::CC),
            Self::CCReceived { cc, .. } if *cc < 128 => Some(MidiMessageType::CC),
            Self::AftertouchChanged { .. } => Some(MidiMessageType::Aftertouch),
            Self::PolyAftertouchChanged { .. } => Some(MidiMessageType::PolyAftertouch),
            Self::PitchBendMoved { .. } => Some(MidiMessageType::PitchBend),
            Self::ProgramChange { .. } => Some(MidiMessageType::ProgramChange),
            // Duration-based events derive from NoteOff (release)
            Self::ShortPress { note, .. }
            | Self::MediumPress { note, .. }
            | Self::LongPress { note, .. }
                if *note < 128 =>
            {
                Some(MidiMessageType::NoteOff)
            }
            // Sustain-derived events derive from NoteOn (press)
            Self::HoldDetected { note, .. } if *note < 128 => Some(MidiMessageType::NoteOn),
            Self::DoubleTap { note, .. } if *note < 128 => Some(MidiMessageType::NoteOn),
            Self::ChordDetected { notes, .. } if notes.iter().all(|n| *n < 128) => {
                Some(MidiMessageType::NoteOn)
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VelocityLevel {
    Soft,
    Medium,
    Hard,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EncoderDirection {
    Clockwise,
    CounterClockwise,
}

/// Sentinel channel key for gamepad events (avoids collision with MIDI channels 0-15)
const GAMEPAD_CHANNEL_KEY: u8 = 0xFF;

/// The configurable [`EventProcessor`] timing knobs, sourced from
/// `advanced_settings`. A single named surface so the daemon applies
/// chord / double-tap / hold / short-press uniformly at processor creation
/// and on config reload — instead of each knob being wired (chord) or
/// silently ignored (hold/double-tap) independently. `short_press_threshold`
/// is configurable; `LONG_PRESS_MS` remains a hardcoded constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventTimings {
    /// Chord-detection window (Learn-aware: the daemon passes the Learn window
    /// while MIDI Learn is active, else the normal window).
    pub chord_timeout: Duration,
    /// Double-tap detection window.
    pub double_tap_timeout: Duration,
    /// Hold-detection threshold for `HoldDetected`.
    pub hold_threshold: Duration,
    /// Short→Medium press classification boundary: a press shorter than
    /// this is `ShortPress`, at/above it (and below `LONG_PRESS_MS`) is
    /// `MediumPress`. Exposed as the "Medium Press Threshold" setting. Distinct
    /// from `hold_threshold` (the `HoldDetected` while-held event).
    pub short_press_threshold: Duration,
}

pub struct EventProcessor {
    /// Note press times: (channel_key, note) -> press_time
    /// channel_key is MIDI channel (0-15) or GAMEPAD_CHANNEL_KEY (0xFF) for gamepad
    note_press_times: HashMap<(u8, u8), Instant>,
    /// Held notes: (channel_key, note) -> (press_time, press_velocity, original_channel)
    /// original_channel preserves Option<u8> for correct emission in check_holds()
    held_notes: HashMap<(u8, u8), (Instant, u8, Option<u8>)>,
    /// Last CC value: (channel_key, cc) -> last_value
    /// channel_key is MIDI channel (0-15) or GAMEPAD_CHANNEL_KEY (0xFF) for gamepad
    last_cc_values: HashMap<(u8, u8), u8>,
    /// Last note tap: (channel_key, note) -> (tap_time, tap_velocity)
    last_note_tap: HashMap<(u8, u8), (Instant, u8)>,
    /// Chord buffer: (channel_key, note, time, velocity)
    chord_buffer: Vec<(u8, u8, Instant, u8)>,
    chord_timeout: Duration,
    double_tap_timeout: Duration,
    hold_threshold: Duration,
    short_press_threshold: Duration,
}

impl Default for EventProcessor {
    fn default() -> Self {
        Self::new()
    }
}

impl EventProcessor {
    pub fn new() -> Self {
        Self {
            note_press_times: HashMap::new(),
            held_notes: HashMap::new(),
            last_cc_values: HashMap::new(),
            last_note_tap: HashMap::new(),
            chord_buffer: Vec::new(),
            chord_timeout: Duration::from_millis(50),
            double_tap_timeout: Duration::from_millis(300),
            hold_threshold: Duration::from_secs(2),
            short_press_threshold: Duration::from_millis(SHORT_PRESS_MS as u64),
        }
    }

    /// Create an EventProcessor with a custom chord timeout.
    /// Useful for MIDI Learn where a more forgiving window (e.g. 150ms)
    /// helps capture 3+ note chords played sequentially by humans.
    pub fn with_chord_timeout(chord_timeout_ms: u64) -> Self {
        Self {
            chord_timeout: Duration::from_millis(chord_timeout_ms),
            ..Self::new()
        }
    }

    /// Dynamically update the chord timeout. Used to extend the window
    /// when MIDI Learn starts (150ms) and restore default when it stops (50ms).
    pub fn set_chord_timeout(&mut self, timeout: Duration) {
        self.chord_timeout = timeout;
    }

    /// Returns the current chord timeout.
    pub fn chord_timeout(&self) -> Duration {
        self.chord_timeout
    }

    /// Set the hold-detection threshold — the minimum time a note must be held
    /// before [`check_holds`](Self::check_holds) emits `HoldDetected`. Mirrors
    /// the `hold_threshold_ms` advanced setting. Also lets tests deterministically
    /// exercise the positive hold path (e.g. set to `Duration::ZERO` so a press
    /// is immediately past threshold) without sleeping for the 2 s default.
    pub fn set_hold_threshold(&mut self, threshold: Duration) {
        self.hold_threshold = threshold;
    }

    /// Returns the current hold-detection threshold.
    pub fn hold_threshold(&self) -> Duration {
        self.hold_threshold
    }

    /// Returns the current double-tap timeout.
    pub fn double_tap_timeout(&self) -> Duration {
        self.double_tap_timeout
    }

    /// Construct an `EventProcessor` with all configurable timing knobs set from
    /// `timings`. The daemon uses this at processor creation so every
    /// knob — not just chord — reflects `advanced_settings`.
    pub fn with_timings(timings: EventTimings) -> Self {
        Self {
            chord_timeout: timings.chord_timeout,
            double_tap_timeout: timings.double_tap_timeout,
            hold_threshold: timings.hold_threshold,
            short_press_threshold: timings.short_press_threshold,
            ..Self::new()
        }
    }

    /// Re-apply all configurable timing knobs from `timings` to an existing
    /// processor. The daemon calls this on config reload so a slider
    /// change reaches already-created processors, not just new ones. (The
    /// chord-only `set_chord_timeout` remains for the MIDI Learn start/stop
    /// path, which toggles only the chord window.)
    pub fn apply_timings(&mut self, timings: EventTimings) {
        self.chord_timeout = timings.chord_timeout;
        self.double_tap_timeout = timings.double_tap_timeout;
        self.hold_threshold = timings.hold_threshold;
        self.short_press_threshold = timings.short_press_threshold;
    }

    /// Returns the current Short→Medium press classification boundary.
    pub fn short_press_threshold(&self) -> Duration {
        self.short_press_threshold
    }

    pub fn process(&mut self, event: MidiEvent) -> Vec<ProcessedEvent> {
        let mut results = Vec::new();

        match event {
            MidiEvent::NoteOn {
                note,
                velocity,
                channel,
                time,
            } => {
                let ch = Some(channel);
                let ch_key = channel;
                self.note_press_times.insert((ch_key, note), time);
                self.held_notes
                    .insert((ch_key, note), (time, velocity, Some(channel)));

                // Check for double-tap (enriched with velocities/interval - D22)
                if let Some(&(last_tap_time, first_velocity)) =
                    self.last_note_tap.get(&(ch_key, note))
                {
                    let interval = time.duration_since(last_tap_time);
                    if interval < self.double_tap_timeout {
                        results.push(ProcessedEvent::DoubleTap {
                            note,
                            first_velocity,
                            second_velocity: velocity,
                            interval_ms: interval.as_millis(),
                            channel: ch,
                        });
                        self.last_note_tap.remove(&(ch_key, note));
                    } else {
                        self.last_note_tap.insert((ch_key, note), (time, velocity));
                    }
                } else {
                    self.last_note_tap.insert((ch_key, note), (time, velocity));
                }

                // Detect velocity levels
                let velocity_level = match velocity {
                    0..=40 => VelocityLevel::Soft,
                    41..=80 => VelocityLevel::Medium,
                    81..=127 => VelocityLevel::Hard,
                    _ => VelocityLevel::Medium,
                };

                results.push(ProcessedEvent::PadPressed {
                    note,
                    velocity,
                    velocity_level,
                    channel: ch,
                });

                // Add to chord buffer (enriched with velocity - D22)
                self.chord_buffer.push((ch_key, note, time, velocity));

                // Check for chord (multiple notes on same channel pressed within chord_timeout)
                self.chord_buffer
                    .retain(|(_, _, t, _)| time.duration_since(*t) < self.chord_timeout);

                // Filter chord candidates to same channel
                let same_ch: Vec<_> = self
                    .chord_buffer
                    .iter()
                    .filter(|(c, _, _, _)| *c == ch_key)
                    .collect();
                if same_ch.len() > 1 {
                    let notes: Vec<u8> = same_ch.iter().map(|(_, n, _, _)| *n).collect();
                    let velocities: Vec<u8> = same_ch.iter().map(|(_, _, _, v)| *v).collect();
                    results.push(ProcessedEvent::ChordDetected {
                        notes,
                        velocities,
                        channel: ch,
                    });
                }
            }

            MidiEvent::NoteOff {
                note,
                channel,
                time,
            } => {
                let ch = Some(channel);
                let ch_key = channel;
                if let Some(press_time) = self.note_press_times.remove(&(ch_key, note)) {
                    let duration = time.duration_since(press_time);
                    let duration_ms = duration.as_millis();

                    results.push(ProcessedEvent::PadReleased {
                        note,
                        hold_duration_ms: duration_ms,
                        channel: ch,
                    });

                    if duration_ms < self.short_press_threshold.as_millis() {
                        results.push(ProcessedEvent::ShortPress { note, channel: ch });
                    } else if duration_ms < LONG_PRESS_MS {
                        results.push(ProcessedEvent::MediumPress {
                            note,
                            duration_ms,
                            channel: ch,
                        });
                    } else {
                        results.push(ProcessedEvent::LongPress {
                            note,
                            duration_ms,
                            channel: ch,
                        });
                    }
                }
                self.held_notes.remove(&(ch_key, note));

                // Remove from chord buffer
                self.chord_buffer
                    .retain(|(c, n, _, _)| !(*c == ch_key && *n == note));
            }

            MidiEvent::ControlChange {
                cc, value, channel, ..
            } => {
                let ch = Some(channel);
                let cc_key = (channel, cc);
                // Always emit CCReceived for pedals/buttons that send fixed values
                results.push(ProcessedEvent::CCReceived {
                    cc,
                    value,
                    channel: ch,
                });

                // Also detect encoder direction for rotating encoders
                if let Some(&last_value) = self.last_cc_values.get(&cc_key) {
                    let direction = if value > last_value {
                        EncoderDirection::Clockwise
                    } else if value < last_value {
                        EncoderDirection::CounterClockwise
                    } else {
                        // No change - already emitted CCReceived above
                        self.last_cc_values.insert(cc_key, value);
                        return results;
                    };

                    let delta = (value as i16 - last_value as i16).unsigned_abs() as u8;

                    results.push(ProcessedEvent::EncoderTurned {
                        cc,
                        value,
                        direction,
                        delta,
                        channel: ch,
                    });
                }
                self.last_cc_values.insert(cc_key, value);
            }

            MidiEvent::PolyPressure {
                note,
                pressure,
                channel,
                ..
            } => {
                results.push(ProcessedEvent::PolyAftertouchChanged {
                    note,
                    pressure,
                    channel: Some(channel),
                });
            }

            MidiEvent::Aftertouch {
                pressure, channel, ..
            } => {
                results.push(ProcessedEvent::AftertouchChanged {
                    pressure,
                    channel: Some(channel),
                });
            }

            MidiEvent::PitchBend { value, channel, .. } => {
                results.push(ProcessedEvent::PitchBendMoved {
                    value,
                    channel: Some(channel),
                });
            }

            // The raw-MidiEvent `process` path dropped ProgramChange via
            // the `_` arm, so callers parsing raw MIDI into `MidiEvent` and using
            // this public path produced no event for program/bank changes and
            // `Trigger::ProgramChange` mappings could not route. Mirror
            // `process_input`'s `InputEvent::ProgramChange` handling.
            MidiEvent::ProgramChange {
                program, channel, ..
            } => {
                results.push(ProcessedEvent::ProgramChange {
                    program,
                    channel: Some(channel),
                });
            }
        }

        results
    }

    /// Process a protocol-agnostic InputEvent
    ///
    /// This method mirrors `process()` but handles InputEvent instead of MidiEvent,
    /// enabling support for multiple input protocols (MIDI, HID gamepad, etc.) through
    /// a unified processing pipeline.
    ///
    /// # Arguments
    ///
    /// * `event` - Protocol-agnostic input event (from MIDI, gamepad, etc.)
    ///
    /// # Returns
    ///
    /// Vector of ProcessedEvent results (short press, long press, chord detection, etc.)
    ///
    /// # Example
    ///
    /// ```rust
    /// use conductor_core::{EventProcessor, events::InputEvent};
    /// use std::time::Instant;
    ///
    /// let mut processor = EventProcessor::new();
    ///
    /// // Gamepad button press (button ID 128 = South/A/Cross/B)
    /// let event = InputEvent::PadPressed {
    ///     pad: 128,
    ///     velocity: 100,
    ///     channel: None,
    ///     time: Instant::now(),
    /// };
    ///
    /// let processed = processor.process_input(event);
    /// // Will detect velocity level, double-tap, chords, etc.
    /// ```
    pub fn process_input(&mut self, event: InputEvent) -> Vec<ProcessedEvent> {
        let mut results = Vec::new();

        // D23: Emit Raw event before gesture detection
        results.push(ProcessedEvent::Raw(event.clone()));

        match event {
            InputEvent::PadPressed {
                pad,
                velocity,
                channel,
                time,
            } => {
                let ch_key = channel.unwrap_or(GAMEPAD_CHANNEL_KEY);
                self.note_press_times.insert((ch_key, pad), time);
                self.held_notes
                    .insert((ch_key, pad), (time, velocity, channel));

                // Check for double-tap (enriched - D22)
                if let Some(&(last_tap_time, first_velocity)) =
                    self.last_note_tap.get(&(ch_key, pad))
                {
                    let interval = time.duration_since(last_tap_time);
                    if interval < self.double_tap_timeout {
                        results.push(ProcessedEvent::DoubleTap {
                            note: pad,
                            first_velocity,
                            second_velocity: velocity,
                            interval_ms: interval.as_millis(),
                            channel,
                        });
                        self.last_note_tap.remove(&(ch_key, pad));
                    } else {
                        self.last_note_tap.insert((ch_key, pad), (time, velocity));
                    }
                } else {
                    self.last_note_tap.insert((ch_key, pad), (time, velocity));
                }

                // Detect velocity levels
                let velocity_level = match velocity {
                    0..=40 => VelocityLevel::Soft,
                    41..=80 => VelocityLevel::Medium,
                    81..=127 => VelocityLevel::Hard,
                    _ => VelocityLevel::Medium,
                };

                results.push(ProcessedEvent::PadPressed {
                    note: pad,
                    velocity,
                    velocity_level,
                    channel,
                });

                // Add to chord buffer (enriched with velocity - D22)
                self.chord_buffer.push((ch_key, pad, time, velocity));

                // Check for chord (multiple pads on same channel pressed within chord_timeout)
                self.chord_buffer
                    .retain(|(_, _, t, _)| time.duration_since(*t) < self.chord_timeout);

                // Filter chord candidates to same channel
                let same_ch: Vec<_> = self
                    .chord_buffer
                    .iter()
                    .filter(|(c, _, _, _)| *c == ch_key)
                    .collect();
                if same_ch.len() > 1 {
                    let notes: Vec<u8> = same_ch.iter().map(|(_, n, _, _)| *n).collect();
                    let velocities: Vec<u8> = same_ch.iter().map(|(_, _, _, v)| *v).collect();
                    results.push(ProcessedEvent::ChordDetected {
                        notes,
                        velocities,
                        channel,
                    });
                }
            }

            InputEvent::PadReleased { pad, channel, time } => {
                let ch_key = channel.unwrap_or(GAMEPAD_CHANNEL_KEY);
                if let Some(press_time) = self.note_press_times.remove(&(ch_key, pad)) {
                    let duration = time.duration_since(press_time);
                    let duration_ms = duration.as_millis();

                    results.push(ProcessedEvent::PadReleased {
                        note: pad,
                        hold_duration_ms: duration_ms,
                        channel,
                    });

                    if duration_ms < self.short_press_threshold.as_millis() {
                        results.push(ProcessedEvent::ShortPress { note: pad, channel });
                    } else if duration_ms < LONG_PRESS_MS {
                        results.push(ProcessedEvent::MediumPress {
                            note: pad,
                            duration_ms,
                            channel,
                        });
                    } else {
                        results.push(ProcessedEvent::LongPress {
                            note: pad,
                            duration_ms,
                            channel,
                        });
                    }
                }
                self.held_notes.remove(&(ch_key, pad));

                // Remove from chord buffer
                self.chord_buffer
                    .retain(|(c, n, _, _)| !(*c == ch_key && *n == pad));
            }

            InputEvent::EncoderTurned {
                encoder,
                value,
                channel,
                ..
            } => {
                let cc_key = (channel.unwrap_or(GAMEPAD_CHANNEL_KEY), encoder);
                // Detect encoder direction
                if let Some(&last_value) = self.last_cc_values.get(&cc_key) {
                    let direction = if value > last_value {
                        EncoderDirection::Clockwise
                    } else if value < last_value {
                        EncoderDirection::CounterClockwise
                    } else {
                        // No change
                        return results;
                    };

                    let delta = (value as i16 - last_value as i16).unsigned_abs() as u8;

                    results.push(ProcessedEvent::EncoderTurned {
                        cc: encoder,
                        value,
                        direction,
                        delta,
                        channel,
                    });
                }
                self.last_cc_values.insert(cc_key, value);
            }

            InputEvent::PolyPressure {
                pad,
                pressure,
                channel,
                ..
            } => {
                results.push(ProcessedEvent::PolyAftertouchChanged {
                    note: pad,
                    pressure,
                    channel,
                });
            }

            InputEvent::Aftertouch {
                pressure, channel, ..
            } => {
                results.push(ProcessedEvent::AftertouchChanged { pressure, channel });
            }

            InputEvent::PitchBend { value, channel, .. } => {
                results.push(ProcessedEvent::PitchBendMoved { value, channel });
            }

            InputEvent::ControlChange {
                control,
                value,
                channel,
                ..
            } => {
                let cc_key = (channel.unwrap_or(GAMEPAD_CHANNEL_KEY), control);
                // Always emit CCReceived for pedals/buttons that send fixed values
                results.push(ProcessedEvent::CCReceived {
                    cc: control,
                    value,
                    channel,
                });

                // Also detect encoder direction for rotating encoders
                if let Some(&last_value) = self.last_cc_values.get(&cc_key) {
                    let direction = if value > last_value {
                        EncoderDirection::Clockwise
                    } else if value < last_value {
                        EncoderDirection::CounterClockwise
                    } else {
                        // No change - already emitted CCReceived above
                        self.last_cc_values.insert(cc_key, value);
                        return results;
                    };

                    let delta = (value as i16 - last_value as i16).unsigned_abs() as u8;

                    results.push(ProcessedEvent::EncoderTurned {
                        cc: control,
                        value,
                        direction,
                        delta,
                        channel,
                    });
                }
                self.last_cc_values.insert(cc_key, value);
            }

            InputEvent::ProgramChange {
                program, channel, ..
            } => {
                // ADR-025 Phase 1: emit as a matchable ProcessedEvent so
                // Trigger::ProgramChange can fire. Raw-state observation
                // happens upstream via PhysicalControlStateStore.
                results.push(ProcessedEvent::ProgramChange { program, channel });
            }
        }

        results
    }

    pub fn check_holds(&mut self) -> Vec<ProcessedEvent> {
        let mut results = Vec::new();
        let now = Instant::now();

        for (&(_ch_key, note), &(press_time, press_velocity, original_channel)) in &self.held_notes
        {
            let duration = now.duration_since(press_time);
            if duration >= self.hold_threshold {
                results.push(ProcessedEvent::HoldDetected {
                    note,
                    press_velocity,
                    duration_ms: duration.as_millis(),
                    channel: original_channel,
                });
                // Note: We might want to track which holds we've already reported
                // to avoid repeated triggers
            }
        }

        results
    }

    pub fn log_processed_event(event: &ProcessedEvent, mode: u8) {
        match event {
            ProcessedEvent::OscReceived { address, args } => {
                debug!(mode, address = %address, args = args.len(), "OSC message received");
            }
            ProcessedEvent::PadPressed {
                note,
                velocity,
                velocity_level,
                ..
            } => {
                let level_str = match velocity_level {
                    VelocityLevel::Soft => "SOFT",
                    VelocityLevel::Medium => "MED",
                    VelocityLevel::Hard => "HARD",
                };
                debug!(mode, note, velocity, level = level_str, "Pad pressed");
            }
            ProcessedEvent::PadReleased {
                note,
                hold_duration_ms,
                ..
            } => {
                debug!(mode, note, hold_duration_ms, "Pad released");
            }
            ProcessedEvent::ShortPress { note, .. } => {
                debug!(mode, note, "Short tap detected");
            }
            ProcessedEvent::MediumPress {
                note, duration_ms, ..
            } => {
                debug!(mode, note, duration_ms, "Medium press detected");
            }
            ProcessedEvent::LongPress {
                note, duration_ms, ..
            } => {
                debug!(mode, note, duration_ms, "Long press detected");
            }
            ProcessedEvent::HoldDetected {
                note,
                press_velocity,
                duration_ms,
                ..
            } => {
                debug!(mode, note, press_velocity, duration_ms, "Hold detected");
            }
            ProcessedEvent::DoubleTap {
                note,
                first_velocity,
                second_velocity,
                interval_ms,
                ..
            } => {
                debug!(
                    mode,
                    note, first_velocity, second_velocity, interval_ms, "Double tap detected"
                );
            }
            ProcessedEvent::ChordDetected {
                notes, velocities, ..
            } => {
                debug!(mode, ?notes, ?velocities, "Chord detected");
            }
            ProcessedEvent::EncoderTurned {
                cc,
                value,
                direction,
                delta,
                ..
            } => {
                let direction_str = match direction {
                    EncoderDirection::Clockwise => "clockwise",
                    EncoderDirection::CounterClockwise => "counter-clockwise",
                };
                debug!(
                    mode,
                    cc,
                    value,
                    direction = direction_str,
                    delta,
                    "Encoder turned"
                );
            }
            ProcessedEvent::CCReceived { cc, value, .. } => {
                trace!(mode, cc, value, "CC received");
            }
            ProcessedEvent::AftertouchChanged { pressure, .. } => {
                trace!(mode, pressure, "Aftertouch changed");
            }
            ProcessedEvent::PolyAftertouchChanged { note, pressure, .. } => {
                trace!(mode, note, pressure, "Poly aftertouch changed");
            }
            ProcessedEvent::PitchBendMoved { value, .. } => {
                trace!(mode, value, "Pitch bend moved");
            }
            ProcessedEvent::ProgramChange { program, .. } => {
                trace!(mode, program, "Program change received");
            }
            ProcessedEvent::Raw(_) => {
                trace!(mode, "Raw event passthrough");
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::{Duration, EventProcessor, EventTimings};

    fn timings(chord_ms: u64, double_tap_ms: u64, hold_ms: u64, short_ms: u64) -> EventTimings {
        EventTimings {
            chord_timeout: Duration::from_millis(chord_ms),
            double_tap_timeout: Duration::from_millis(double_tap_ms),
            hold_threshold: Duration::from_millis(hold_ms),
            short_press_threshold: Duration::from_millis(short_ms),
        }
    }

    #[test]
    fn with_timings_sets_every_knob() {
        // Construction sets ALL configurable knobs — not just
        // chord — so a fresh processor reflects advanced_settings.
        let p = EventProcessor::with_timings(timings(80, 250, 1500, 175));
        assert_eq!(p.chord_timeout(), Duration::from_millis(80));
        assert_eq!(p.double_tap_timeout(), Duration::from_millis(250));
        assert_eq!(p.hold_threshold(), Duration::from_millis(1500));
        assert_eq!(p.short_press_threshold(), Duration::from_millis(175));
    }

    #[test]
    fn apply_timings_updates_existing_processor() {
        // Reload re-applies all knobs to an existing processor so
        // a runtime slider change reaches already-created devices.
        let mut p = EventProcessor::new();
        // new() defaults: chord 50, double-tap 300, hold 2000, short-press 200.
        assert_eq!(p.hold_threshold(), Duration::from_secs(2));
        assert_eq!(p.short_press_threshold(), Duration::from_millis(200));
        p.apply_timings(timings(120, 400, 800, 90));
        assert_eq!(p.chord_timeout(), Duration::from_millis(120));
        assert_eq!(p.double_tap_timeout(), Duration::from_millis(400));
        assert_eq!(p.hold_threshold(), Duration::from_millis(800));
        assert_eq!(p.short_press_threshold(), Duration::from_millis(90));
    }

    #[test]
    fn short_press_threshold_drives_short_vs_medium_classification() {
        // A press whose duration is below the configured short-press
        // threshold is a ShortPress; at/above it (and below LONG_PRESS_MS) it is
        // a MediumPress. Lowering the threshold to 50ms reclassifies a press
        // that the default 200ms threshold would call Short.
        use crate::event_processor::{MidiEvent, ProcessedEvent};
        use std::time::Instant;

        let mut p = EventProcessor::with_timings(timings(50, 300, 2000, 50));
        let t0 = Instant::now();
        // NoteOn then NoteOff ~120ms later: < 200 (default Short) but >= 50
        // (configured) → MediumPress.
        p.process(MidiEvent::NoteOn {
            note: 60,
            velocity: 100,
            channel: 0,
            time: t0,
        });
        let out = p.process(MidiEvent::NoteOff {
            note: 60,
            channel: 0,
            time: t0 + Duration::from_millis(120),
        });
        assert!(
            out.iter()
                .any(|e| matches!(e, ProcessedEvent::MediumPress { .. })),
            "press of 120ms with a 50ms short-press threshold must classify as MediumPress, got {out:?}"
        );
        assert!(
            !out.iter()
                .any(|e| matches!(e, ProcessedEvent::ShortPress { .. })),
            "must NOT also emit ShortPress"
        );
    }
}
