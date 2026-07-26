// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! MIDI transform pipeline (v4.25.0 - ADR-009 Gap 2)
//!
//! Provides real-time MIDI message transformation for the `MidiForward` action.
//! Transforms are applied to raw MIDI bytes before forwarding to an output port.

use serde::{Deserialize, Serialize};

/// Transform applied to a MIDI message before forwarding
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MidiTransform {
    /// Remap to a different MIDI channel (0-15)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<u8>,
    /// Remap CC number (for ControlChange messages)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cc: Option<u8>,
    /// Remap note number (for Note messages)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<u8>,
    /// Scale velocity/value (multiplier, e.g. 1.5 = 150%)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub velocity_scale: Option<f32>,
    /// Offset velocity/value (added after scaling)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub velocity_offset: Option<i8>,
    /// Invert the data value (127 - value)
    #[serde(default)]
    pub invert_value: bool,
    /// Apply a value curve
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curve: Option<ValueCurve>,
}

/// Value curve types for non-linear transformations
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ValueCurve {
    /// Linear (identity — no transformation)
    Linear,
    /// Logarithmic (compress high values)
    Logarithmic,
    /// Exponential (expand high values)
    Exponential,
    /// Lookup table: 128-entry mapping from input (0-127) to output (0-127)
    Lut(#[serde(with = "lut_serde")] Box<[u8; 128]>),
}

/// Custom serde for `Box<[u8; 128]>` — serializes as a JSON/TOML array of 128 u8 values.
/// Uses a bounded visitor to prevent unbounded memory allocation during deserialization.
mod lut_serde {
    use serde::de::{self, SeqAccess, Visitor};
    use serde::{Deserializer, Serialize, Serializer};
    use std::fmt;

    pub fn serialize<S: Serializer>(data: &[u8; 128], ser: S) -> Result<S::Ok, S::Error> {
        data.as_slice().serialize(ser)
    }

    /// Visitor that reads exactly 128 u8 values without unbounded allocation.
    struct Lut128Visitor;

    impl<'de> Visitor<'de> for Lut128Visitor {
        type Value = Box<[u8; 128]>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "an array of exactly 128 u8 values")
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut arr = [0u8; 128];
            for (i, entry) in arr.iter_mut().enumerate() {
                *entry = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::invalid_length(i, &self))?;
            }
            // Reject if there are extra elements
            if seq.next_element::<u8>()?.is_some() {
                return Err(de::Error::invalid_length(129, &self));
            }
            Ok(Box::new(arr))
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Box<[u8; 128]>, D::Error> {
        de.deserialize_seq(Lut128Visitor)
    }
}

/// Typed MIDI message for structured parsing and transformation (v4.26.0 - ADR-009 Gap I)
///
/// Provides a type-safe representation of standard MIDI channel messages.
/// Used internally by `MidiTransform::apply()` to avoid inline status byte parsing.
#[derive(Debug, Clone, PartialEq)]
pub enum MidiMessage {
    NoteOff { channel: u8, note: u8, velocity: u8 },
    NoteOn { channel: u8, note: u8, velocity: u8 },
    PolyAftertouch { channel: u8, note: u8, pressure: u8 },
    ControlChange { channel: u8, cc: u8, value: u8 },
    ProgramChange { channel: u8, program: u8 },
    ChannelAftertouch { channel: u8, pressure: u8 },
    PitchBend { channel: u8, lsb: u8, msb: u8 },
}

impl MidiMessage {
    /// Parse raw MIDI bytes into a typed message.
    ///
    /// Returns `None` for:
    /// - Empty messages
    /// - Messages exceeding 3 bytes (SysEx)
    /// - System messages (0xF0-0xFF)
    /// - Truncated messages
    /// - Invalid status bytes
    /// - Data bytes with bit 7 set (invalid per MIDI spec)
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.is_empty() || bytes.len() > MAX_MIDI_MESSAGE_LEN {
            return None;
        }

        let status_byte = bytes[0];
        if !(0x80..0xF0).contains(&status_byte) {
            return None;
        }

        // Validate all data bytes have bit 7 clear (MIDI spec requirement)
        for &b in &bytes[1..] {
            if b & 0x80 != 0 {
                return None;
            }
        }

        let status = status_byte & 0xF0;
        let channel = status_byte & 0x0F;

        match status {
            0x80 if bytes.len() >= 3 => Some(MidiMessage::NoteOff {
                channel,
                note: bytes[1],
                velocity: bytes[2],
            }),
            0x90 if bytes.len() >= 3 => Some(MidiMessage::NoteOn {
                channel,
                note: bytes[1],
                velocity: bytes[2],
            }),
            0xA0 if bytes.len() >= 3 => Some(MidiMessage::PolyAftertouch {
                channel,
                note: bytes[1],
                pressure: bytes[2],
            }),
            0xB0 if bytes.len() >= 3 => Some(MidiMessage::ControlChange {
                channel,
                cc: bytes[1],
                value: bytes[2],
            }),
            0xC0 if bytes.len() >= 2 => Some(MidiMessage::ProgramChange {
                channel,
                program: bytes[1],
            }),
            0xD0 if bytes.len() >= 2 => Some(MidiMessage::ChannelAftertouch {
                channel,
                pressure: bytes[1],
            }),
            0xE0 if bytes.len() >= 3 => Some(MidiMessage::PitchBend {
                channel,
                lsb: bytes[1],
                msb: bytes[2],
            }),
            _ => None,
        }
    }

    /// Convert back to raw MIDI bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            MidiMessage::NoteOff {
                channel,
                note,
                velocity,
            } => {
                vec![0x80 | (channel & 0x0F), note & 0x7F, velocity & 0x7F]
            }
            MidiMessage::NoteOn {
                channel,
                note,
                velocity,
            } => {
                vec![0x90 | (channel & 0x0F), note & 0x7F, velocity & 0x7F]
            }
            MidiMessage::PolyAftertouch {
                channel,
                note,
                pressure,
            } => {
                vec![0xA0 | (channel & 0x0F), note & 0x7F, pressure & 0x7F]
            }
            MidiMessage::ControlChange { channel, cc, value } => {
                vec![0xB0 | (channel & 0x0F), cc & 0x7F, value & 0x7F]
            }
            MidiMessage::ProgramChange { channel, program } => {
                vec![0xC0 | (channel & 0x0F), program & 0x7F]
            }
            MidiMessage::ChannelAftertouch { channel, pressure } => {
                vec![0xD0 | (channel & 0x0F), pressure & 0x7F]
            }
            MidiMessage::PitchBend { channel, lsb, msb } => {
                vec![0xE0 | (channel & 0x0F), lsb & 0x7F, msb & 0x7F]
            }
        }
    }
}

/// Maximum MIDI message size we'll process (standard messages are 1-3 bytes;
/// cap at 3 to reject SysEx and prevent unbounded allocation).
const MAX_MIDI_MESSAGE_LEN: usize = 3;

impl MidiTransform {
    /// Validate configuration field ranges.
    ///
    /// Returns a list of validation errors, empty if valid.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if let Some(ch) = self.channel
            && ch > 15
        {
            errors.push(format!("channel must be 0-15, got {}", ch));
        }
        if let Some(cc) = self.cc
            && cc > 127
        {
            errors.push(format!("cc must be 0-127, got {}", cc));
        }
        if let Some(note) = self.note
            && note > 127
        {
            errors.push(format!("note must be 0-127, got {}", note));
        }
        if let Some(scale) = self.velocity_scale
            && (!scale.is_finite() || scale < 0.0)
        {
            errors.push(format!(
                "velocity_scale must be a non-negative finite number, got {}",
                scale
            ));
        }
        errors
    }
}

impl MidiTransform {
    /// Apply this transform to raw MIDI bytes.
    ///
    /// Parses the raw bytes into a typed `MidiMessage`, applies transforms,
    /// and serializes back to bytes. Returns an empty Vec for invalid or
    /// unsupported messages (SysEx, System, truncated).
    pub fn apply(&self, message: &[u8]) -> Vec<u8> {
        let Some(parsed) = MidiMessage::parse(message) else {
            return Vec::new();
        };

        let transformed = match parsed {
            MidiMessage::NoteOff {
                channel,
                note,
                velocity,
            } => {
                let ch = self.channel.unwrap_or(channel);
                let n = self.note.unwrap_or(note);
                // Preserve velocity 0 semantic (NoteOff)
                let v = if velocity > 0 {
                    self.transform_value(velocity)
                } else {
                    0
                };
                MidiMessage::NoteOff {
                    channel: ch,
                    note: n,
                    velocity: v,
                }
            }
            MidiMessage::NoteOn {
                channel,
                note,
                velocity,
            } => {
                let ch = self.channel.unwrap_or(channel);
                let n = self.note.unwrap_or(note);
                // MIDI spec: NoteOn with velocity 0 = NoteOff.
                // Don't transform zero velocity to non-zero ("stuck notes").
                let v = if velocity > 0 {
                    self.transform_value(velocity)
                } else {
                    0
                };
                MidiMessage::NoteOn {
                    channel: ch,
                    note: n,
                    velocity: v,
                }
            }
            MidiMessage::PolyAftertouch {
                channel,
                note,
                pressure,
            } => {
                let ch = self.channel.unwrap_or(channel);
                let n = self.note.unwrap_or(note);
                let p = self.transform_value(pressure);
                MidiMessage::PolyAftertouch {
                    channel: ch,
                    note: n,
                    pressure: p,
                }
            }
            MidiMessage::ControlChange { channel, cc, value } => {
                let ch = self.channel.unwrap_or(channel);
                let c = self.cc.unwrap_or(cc);
                let v = self.transform_value(value);
                MidiMessage::ControlChange {
                    channel: ch,
                    cc: c,
                    value: v,
                }
            }
            MidiMessage::ProgramChange { channel, program } => {
                let ch = self.channel.unwrap_or(channel);
                MidiMessage::ProgramChange {
                    channel: ch,
                    program,
                }
            }
            MidiMessage::ChannelAftertouch { channel, pressure } => {
                let ch = self.channel.unwrap_or(channel);
                let p = self.transform_value(pressure);
                MidiMessage::ChannelAftertouch {
                    channel: ch,
                    pressure: p,
                }
            }
            MidiMessage::PitchBend { channel, lsb, msb } => {
                let ch = self.channel.unwrap_or(channel);
                MidiMessage::PitchBend {
                    channel: ch,
                    lsb,
                    msb,
                }
            }
        };

        transformed.to_bytes()
    }

    /// Transform a single 7-bit value through the pipeline:
    /// curve → scale → offset → invert → clamp
    fn transform_value(&self, value: u8) -> u8 {
        let mut v = f32::from(value);

        // 1. Apply curve
        if let Some(ref curve) = self.curve {
            v = match curve {
                ValueCurve::Linear => v,
                ValueCurve::Logarithmic => {
                    // log(1 + x) / log(128) * 127
                    (1.0 + v).ln() / 128.0_f32.ln() * 127.0
                }
                ValueCurve::Exponential => {
                    // (2^(x/127) - 1) / (2 - 1) * 127
                    ((2.0_f32).powf(v / 127.0) - 1.0) * 127.0
                }
                ValueCurve::Lut(table) => {
                    // Defense-in-depth: bounds check even though parse() validates 0-127
                    f32::from(*table.get(value as usize).unwrap_or(&value))
                }
            };
        }

        // 2. Apply scale (sanitize: NaN/Inf treated as no-op)
        if let Some(scale) = self.velocity_scale
            && scale.is_finite()
        {
            v *= scale;
        }

        // 3. Apply offset
        if let Some(offset) = self.velocity_offset {
            v += f32::from(offset);
        }

        // 4. Apply invert
        if self.invert_value {
            v = 127.0 - v;
        }

        // 5. Guard against NaN/Inf from curve arithmetic, then clamp
        if !v.is_finite() {
            return value; // Fall back to original input on bad math
        }

        // 6. Clamp to 7-bit MIDI range
        v.round().clamp(0.0, 127.0) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity_transform() -> MidiTransform {
        MidiTransform {
            channel: None,
            cc: None,
            note: None,
            velocity_scale: None,
            velocity_offset: None,
            invert_value: false,
            curve: None,
        }
    }

    #[test]
    fn test_identity_transform() {
        let t = identity_transform();
        let msg = vec![0x90, 60, 100]; // NoteOn ch0, note 60, vel 100
        assert_eq!(t.apply(&msg), vec![0x90, 60, 100]);
    }

    #[test]
    fn test_empty_message() {
        let t = identity_transform();
        assert_eq!(t.apply(&[]), Vec::<u8>::new());
    }

    #[test]
    fn test_channel_remap() {
        let t = MidiTransform {
            channel: Some(5),
            ..identity_transform()
        };
        let msg = vec![0x90, 60, 100]; // NoteOn ch0
        let result = t.apply(&msg);
        assert_eq!(result[0], 0x95); // NoteOn ch5
        assert_eq!(result[1], 60);
        assert_eq!(result[2], 100);
    }

    #[test]
    fn test_note_remap() {
        let t = MidiTransform {
            note: Some(72),
            ..identity_transform()
        };
        let msg = vec![0x90, 60, 100];
        let result = t.apply(&msg);
        assert_eq!(result[1], 72);
    }

    #[test]
    fn test_cc_remap() {
        let t = MidiTransform {
            cc: Some(1), // Remap to modulation wheel
            ..identity_transform()
        };
        let msg = vec![0xB0, 74, 64]; // CC 74 (filter) -> CC 1 (mod wheel)
        let result = t.apply(&msg);
        assert_eq!(result[1], 1);
        assert_eq!(result[2], 64);
    }

    #[test]
    fn test_velocity_scale() {
        let t = MidiTransform {
            velocity_scale: Some(0.5),
            ..identity_transform()
        };
        let msg = vec![0x90, 60, 100];
        let result = t.apply(&msg);
        assert_eq!(result[2], 50); // 100 * 0.5 = 50
    }

    #[test]
    fn test_velocity_offset() {
        let t = MidiTransform {
            velocity_offset: Some(20),
            ..identity_transform()
        };
        let msg = vec![0x90, 60, 80];
        let result = t.apply(&msg);
        assert_eq!(result[2], 100); // 80 + 20 = 100
    }

    #[test]
    fn test_velocity_offset_clamps() {
        let t = MidiTransform {
            velocity_offset: Some(50),
            ..identity_transform()
        };
        let msg = vec![0x90, 60, 120];
        let result = t.apply(&msg);
        assert_eq!(result[2], 127); // 120 + 50 = 170, clamped to 127
    }

    #[test]
    fn test_negative_offset_clamps() {
        let t = MidiTransform {
            velocity_offset: Some(-100),
            ..identity_transform()
        };
        let msg = vec![0x90, 60, 50];
        let result = t.apply(&msg);
        assert_eq!(result[2], 0); // 50 - 100 = -50, clamped to 0
    }

    #[test]
    fn test_invert_value() {
        let t = MidiTransform {
            invert_value: true,
            ..identity_transform()
        };
        let msg = vec![0x90, 60, 100];
        let result = t.apply(&msg);
        assert_eq!(result[2], 27); // 127 - 100 = 27
    }

    #[test]
    fn test_invert_value_zero() {
        let t = MidiTransform {
            invert_value: true,
            ..identity_transform()
        };
        let msg = vec![0xB0, 1, 0];
        let result = t.apply(&msg);
        assert_eq!(result[2], 127); // 127 - 0 = 127
    }

    #[test]
    fn test_combined_transforms() {
        let t = MidiTransform {
            channel: Some(3),
            cc: Some(7), // Remap to volume
            velocity_scale: Some(2.0),
            velocity_offset: Some(-10),
            invert_value: false,
            curve: None,
            note: None,
        };
        let msg = vec![0xB0, 74, 50]; // CC 74 ch0, value 50
        let result = t.apply(&msg);
        assert_eq!(result[0], 0xB3); // CC ch3
        assert_eq!(result[1], 7); // CC number 7
        assert_eq!(result[2], 90); // 50 * 2.0 - 10 = 90
    }

    #[test]
    fn test_logarithmic_curve() {
        let t = MidiTransform {
            curve: Some(ValueCurve::Logarithmic),
            ..identity_transform()
        };
        // Low values should be boosted, high values compressed
        let low = t.apply(&[0x90, 60, 10]);
        let high = t.apply(&[0x90, 60, 120]);

        // Logarithmic: low input → higher output relative to linear
        assert!(low[2] > 10);
        assert!(high[2] < 127);
    }

    #[test]
    fn test_exponential_curve() {
        let t = MidiTransform {
            curve: Some(ValueCurve::Exponential),
            ..identity_transform()
        };
        // Exponential curve: (2^(v/127) - 1) * 127
        // Low values are reduced, high values approach 127
        let zero = t.apply(&[0x90, 60, 0]);
        let mid = t.apply(&[0x90, 60, 64]);
        let max = t.apply(&[0x90, 60, 127]);

        assert_eq!(zero[2], 0); // 0 stays 0
        assert!(mid[2] < 64); // Mid-range is compressed downward
        assert_eq!(max[2], 127); // 127 stays 127
    }

    #[test]
    fn test_linear_curve_is_identity() {
        let t = MidiTransform {
            curve: Some(ValueCurve::Linear),
            ..identity_transform()
        };
        let msg = vec![0x90, 60, 100];
        let result = t.apply(&msg);
        assert_eq!(result[2], 100);
    }

    #[test]
    fn test_poly_aftertouch_transform() {
        let t = MidiTransform {
            channel: Some(2),
            note: Some(48),
            invert_value: true,
            ..identity_transform()
        };
        let msg = vec![0xA0, 60, 80]; // PolyAftertouch ch0, note 60, pressure 80
        let result = t.apply(&msg);
        assert_eq!(result[0], 0xA2); // ch2
        assert_eq!(result[1], 48); // remapped note
        assert_eq!(result[2], 47); // 127 - 80 = 47
    }

    #[test]
    fn test_aftertouch_transform() {
        let t = MidiTransform {
            invert_value: true,
            ..identity_transform()
        };
        let msg = vec![0xD0, 80]; // Channel aftertouch, pressure 80
        let result = t.apply(&msg);
        assert_eq!(result[0], 0xD0);
        assert_eq!(result[1], 47); // 127 - 80 = 47
    }

    #[test]
    fn test_note_off_transform() {
        let t = MidiTransform {
            channel: Some(2),
            note: Some(48),
            ..identity_transform()
        };
        let msg = vec![0x80, 60, 64]; // NoteOff ch0, note 60
        let result = t.apply(&msg);
        assert_eq!(result[0], 0x82); // NoteOff ch2
        assert_eq!(result[1], 48); // Remapped note
    }

    #[test]
    fn test_serde_roundtrip() {
        let t = MidiTransform {
            channel: Some(5),
            cc: Some(1),
            note: None,
            velocity_scale: Some(1.5),
            velocity_offset: Some(-10),
            invert_value: true,
            curve: Some(ValueCurve::Logarithmic),
        };
        let json = serde_json::to_string(&t).unwrap();
        let deserialized: MidiTransform = serde_json::from_str(&json).unwrap();
        assert_eq!(t, deserialized);
    }

    #[test]
    fn test_serde_toml_roundtrip() {
        let toml_str = r#"
channel = 5
cc = 1
velocity_scale = 1.5
velocity_offset = -10
invert_value = true
curve = "Logarithmic"
"#;
        let t: MidiTransform = toml::from_str(toml_str).unwrap();
        assert_eq!(t.channel, Some(5));
        assert_eq!(t.cc, Some(1));
        assert_eq!(t.velocity_scale, Some(1.5));
        assert_eq!(t.velocity_offset, Some(-10));
        assert!(t.invert_value);
        assert_eq!(t.curve, Some(ValueCurve::Logarithmic));
    }

    #[test]
    fn test_serde_defaults() {
        let toml_str = r#"
invert_value = false
"#;
        let t: MidiTransform = toml::from_str(toml_str).unwrap();
        assert_eq!(t.channel, None);
        assert_eq!(t.cc, None);
        assert_eq!(t.note, None);
        assert_eq!(t.velocity_scale, None);
        assert_eq!(t.velocity_offset, None);
        assert!(!t.invert_value);
        assert_eq!(t.curve, None);
    }

    #[test]
    fn test_oversized_message_rejected() {
        // SysEx or other variable-length messages are rejected (empty Vec)
        // to prevent unbounded heap allocation from arbitrary input.
        let t = MidiTransform {
            channel: Some(5),
            velocity_scale: Some(2.0),
            ..identity_transform()
        };
        let sysex = vec![0xF0, 0x7E, 0x7F, 0x09, 0x01, 0xF7]; // 6 bytes
        let result = t.apply(&sysex);
        assert!(result.is_empty(), "SysEx should be rejected (empty)");
    }

    #[test]
    fn test_note_on_velocity_zero_preserved() {
        // MIDI spec: NoteOn with velocity 0 = NoteOff.
        // Transform must NOT change vel 0 to non-zero ("stuck note" bug).
        let t = MidiTransform {
            velocity_scale: Some(2.0),
            velocity_offset: Some(10),
            ..identity_transform()
        };
        let msg = vec![0x90, 60, 0]; // NoteOn vel 0 = NoteOff
        let result = t.apply(&msg);
        assert_eq!(
            result[2], 0,
            "NoteOn vel 0 must remain 0 (NoteOff semantic)"
        );
    }

    #[test]
    fn test_system_message_rejected() {
        // System messages (0xF0-0xFF) should be rejected
        let t = MidiTransform {
            channel: Some(5),
            ..identity_transform()
        };
        // System Reset (0xFF) — potentially dangerous
        assert!(t.apply(&[0xFF]).is_empty());
        // Timing Clock (0xF8)
        assert!(t.apply(&[0xF8]).is_empty());
        // Active Sensing (0xFE)
        assert!(t.apply(&[0xFE]).is_empty());
    }

    #[test]
    fn test_nan_scale_treated_as_noop() {
        let t = MidiTransform {
            velocity_scale: Some(f32::NAN),
            ..identity_transform()
        };
        let msg = vec![0x90, 60, 100];
        let result = t.apply(&msg);
        assert_eq!(result[2], 100, "NaN scale should be treated as no-op");
    }

    #[test]
    fn test_infinity_scale_fallback() {
        let t = MidiTransform {
            velocity_scale: Some(f32::INFINITY),
            ..identity_transform()
        };
        let msg = vec![0x90, 60, 100];
        let result = t.apply(&msg);
        // Infinity * 100 = Infinity, which is not finite → fallback to original value
        assert_eq!(
            result[2], 100,
            "Inf result should fall back to original value"
        );
    }

    #[test]
    fn test_neg_infinity_scale_fallback() {
        let t = MidiTransform {
            velocity_scale: Some(f32::NEG_INFINITY),
            ..identity_transform()
        };
        let msg = vec![0x90, 60, 50];
        let result = t.apply(&msg);
        assert_eq!(
            result[2], 50,
            "Neg inf result should fall back to original value"
        );
    }

    #[test]
    fn test_truncated_note_on_rejected() {
        // NoteOn (0x90) requires 3 bytes — 1 or 2 byte messages are truncated
        let t = identity_transform();
        assert!(
            t.apply(&[0x90]).is_empty(),
            "1-byte NoteOn should be rejected"
        );
        assert!(
            t.apply(&[0x90, 60]).is_empty(),
            "2-byte NoteOn should be rejected"
        );
    }

    #[test]
    fn test_truncated_cc_rejected() {
        let t = identity_transform();
        assert!(t.apply(&[0xB0]).is_empty(), "1-byte CC should be rejected");
        assert!(
            t.apply(&[0xB0, 1]).is_empty(),
            "2-byte CC should be rejected"
        );
    }

    #[test]
    fn test_program_change_two_bytes_ok() {
        // ProgramChange (0xC0) only needs 2 bytes
        let t = MidiTransform {
            channel: Some(3),
            ..identity_transform()
        };
        let msg = vec![0xC0, 42]; // ProgramChange, program 42
        let result = t.apply(&msg);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], 0xC3); // Remapped to channel 3
        assert_eq!(result[1], 42);
    }

    #[test]
    fn test_truncated_program_change_rejected() {
        let t = identity_transform();
        assert!(
            t.apply(&[0xC0]).is_empty(),
            "1-byte ProgramChange should be rejected"
        );
    }

    #[test]
    fn test_invalid_data_bytes_rejected() {
        // Data bytes with bit 7 set are invalid per MIDI spec.
        // parse() rejects them rather than silently masking.
        let t = identity_transform();
        let msg = vec![0x90, 0xFF, 0xFF]; // Both data bytes have bit 7 set
        let result = t.apply(&msg);
        assert!(
            result.is_empty(),
            "Invalid data bytes should cause rejection"
        );
    }

    #[test]
    fn test_single_invalid_data_byte_rejected() {
        let t = identity_transform();
        // First data byte valid, second invalid
        let msg = vec![0x90, 60, 0x80];
        assert!(
            t.apply(&msg).is_empty(),
            "Single invalid data byte should reject"
        );

        // First data byte invalid, second valid
        let msg2 = vec![0x90, 0x80, 100];
        assert!(
            t.apply(&msg2).is_empty(),
            "Single invalid data byte should reject"
        );
    }

    #[test]
    fn test_to_bytes_clamps_as_defense_in_depth() {
        // to_bytes() still clamps with & 0x7F as defense-in-depth for output,
        // even though parse() now validates input data bytes.
        let msg = MidiMessage::NoteOn {
            channel: 0,
            note: 200,
            velocity: 200,
        };
        let bytes = msg.to_bytes();
        assert_eq!(bytes[1] & 0x80, 0, "to_bytes should clamp note");
        assert_eq!(bytes[2] & 0x80, 0, "to_bytes should clamp velocity");
    }

    #[test]
    fn test_non_channel_message_rejected() {
        // Bytes < 0x80 are data bytes, not valid status bytes
        let t = identity_transform();
        assert!(
            t.apply(&[0x60]).is_empty(),
            "Data byte as status should be rejected"
        );
        assert!(t.apply(&[0x00]).is_empty(), "Zero byte should be rejected");
    }

    // MidiMessage parse/to_bytes tests (Gap I)

    #[test]
    fn test_midi_message_parse_note_on() {
        let msg = MidiMessage::parse(&[0x93, 60, 100]);
        assert_eq!(
            msg,
            Some(MidiMessage::NoteOn {
                channel: 3,
                note: 60,
                velocity: 100
            })
        );
    }

    #[test]
    fn test_midi_message_parse_note_off() {
        let msg = MidiMessage::parse(&[0x82, 48, 64]);
        assert_eq!(
            msg,
            Some(MidiMessage::NoteOff {
                channel: 2,
                note: 48,
                velocity: 64
            })
        );
    }

    #[test]
    fn test_midi_message_parse_poly_aftertouch() {
        let msg = MidiMessage::parse(&[0xA1, 60, 80]);
        assert_eq!(
            msg,
            Some(MidiMessage::PolyAftertouch {
                channel: 1,
                note: 60,
                pressure: 80
            })
        );
    }

    #[test]
    fn test_midi_message_parse_cc() {
        let msg = MidiMessage::parse(&[0xB5, 74, 127]);
        assert_eq!(
            msg,
            Some(MidiMessage::ControlChange {
                channel: 5,
                cc: 74,
                value: 127
            })
        );
    }

    #[test]
    fn test_midi_message_parse_program_change() {
        let msg = MidiMessage::parse(&[0xC0, 42]);
        assert_eq!(
            msg,
            Some(MidiMessage::ProgramChange {
                channel: 0,
                program: 42
            })
        );
    }

    #[test]
    fn test_midi_message_parse_aftertouch() {
        let msg = MidiMessage::parse(&[0xD1, 80]);
        assert_eq!(
            msg,
            Some(MidiMessage::ChannelAftertouch {
                channel: 1,
                pressure: 80
            })
        );
    }

    #[test]
    fn test_midi_message_parse_pitch_bend() {
        let msg = MidiMessage::parse(&[0xE4, 0, 64]);
        assert_eq!(
            msg,
            Some(MidiMessage::PitchBend {
                channel: 4,
                lsb: 0,
                msb: 64
            })
        );
    }

    #[test]
    fn test_midi_message_roundtrip() {
        let cases: Vec<Vec<u8>> = vec![
            vec![0x90, 60, 100],
            vec![0x80, 48, 64],
            vec![0xA0, 60, 80],
            vec![0xB0, 74, 127],
            vec![0xC5, 42],
            vec![0xD3, 80],
            vec![0xE7, 0, 64],
        ];
        for bytes in cases {
            let parsed = MidiMessage::parse(&bytes).unwrap();
            assert_eq!(
                parsed.to_bytes(),
                bytes,
                "Round-trip failed for {:?}",
                bytes
            );
        }
    }

    #[test]
    fn test_midi_message_parse_invalid() {
        assert!(MidiMessage::parse(&[]).is_none());
        assert!(MidiMessage::parse(&[0xF0, 0x7E, 0x7F, 0x09]).is_none());
        assert!(MidiMessage::parse(&[0xFF]).is_none());
        assert!(MidiMessage::parse(&[0x60]).is_none());
        assert!(MidiMessage::parse(&[0x90, 60]).is_none());
    }

    #[test]
    fn test_midi_message_parse_rejects_high_bit_data_bytes() {
        // MIDI data bytes must have bit 7 clear (0x00-0x7F)
        assert!(
            MidiMessage::parse(&[0x90, 0x80, 100]).is_none(),
            "note with bit 7 set"
        );
        assert!(
            MidiMessage::parse(&[0x90, 60, 0x80]).is_none(),
            "velocity with bit 7 set"
        );
        assert!(
            MidiMessage::parse(&[0xB0, 0xFF, 64]).is_none(),
            "CC number with bit 7 set"
        );
        assert!(
            MidiMessage::parse(&[0xC0, 0x80]).is_none(),
            "program with bit 7 set"
        );
        assert!(
            MidiMessage::parse(&[0xD0, 0x80]).is_none(),
            "pressure with bit 7 set"
        );
        assert!(
            MidiMessage::parse(&[0xE0, 0x80, 64]).is_none(),
            "pitch bend lsb with bit 7 set"
        );
    }

    // ValueCurve::Lut tests (Gap J)

    #[test]
    fn test_lut_identity_table() {
        let mut table = [0u8; 128];
        for (i, entry) in table.iter_mut().enumerate() {
            *entry = i as u8;
        }
        let t = MidiTransform {
            curve: Some(ValueCurve::Lut(Box::new(table))),
            ..identity_transform()
        };
        let msg = vec![0x90, 60, 100];
        let result = t.apply(&msg);
        assert_eq!(result[2], 100);
    }

    #[test]
    fn test_lut_reversed_table() {
        let mut table = [0u8; 128];
        for (i, entry) in table.iter_mut().enumerate() {
            *entry = (127 - i) as u8;
        }
        let t = MidiTransform {
            curve: Some(ValueCurve::Lut(Box::new(table))),
            ..identity_transform()
        };
        let msg = vec![0x90, 60, 100];
        let result = t.apply(&msg);
        assert_eq!(result[2], 27); // 127 - 100 = 27
    }

    #[test]
    fn test_lut_constant_table() {
        let table = [64u8; 128];
        let t = MidiTransform {
            curve: Some(ValueCurve::Lut(Box::new(table))),
            ..identity_transform()
        };
        // NoteOn vel 0 = NoteOff, not transformed
        let msg = vec![0x90, 60, 0];
        assert_eq!(t.apply(&msg)[2], 0);

        // CC value is transformed
        let msg2 = vec![0xB0, 1, 50];
        assert_eq!(t.apply(&msg2)[2], 64);
    }

    #[test]
    fn test_lut_serde_json_roundtrip() {
        let mut table = [0u8; 128];
        for (i, entry) in table.iter_mut().enumerate() {
            *entry = i as u8;
        }
        let curve = ValueCurve::Lut(Box::new(table));
        let json = serde_json::to_string(&curve).unwrap();
        let deserialized: ValueCurve = serde_json::from_str(&json).unwrap();
        assert_eq!(curve, deserialized);
    }

    // MidiTransform::validate() tests

    #[test]
    fn test_validate_valid_transform() {
        let t = MidiTransform {
            channel: Some(15),
            cc: Some(127),
            note: Some(127),
            velocity_scale: Some(2.0),
            velocity_offset: Some(-10),
            invert_value: true,
            curve: Some(ValueCurve::Linear),
        };
        assert!(t.validate().is_empty());
    }

    #[test]
    fn test_validate_channel_out_of_range() {
        let t = MidiTransform {
            channel: Some(16),
            ..identity_transform()
        };
        let errors = t.validate();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("channel must be 0-15"));
    }

    #[test]
    fn test_validate_cc_out_of_range() {
        let t = MidiTransform {
            cc: Some(200),
            ..identity_transform()
        };
        let errors = t.validate();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("cc must be 0-127"));
    }

    #[test]
    fn test_validate_note_out_of_range() {
        let t = MidiTransform {
            note: Some(128),
            ..identity_transform()
        };
        let errors = t.validate();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("note must be 0-127"));
    }

    #[test]
    fn test_validate_negative_scale() {
        let t = MidiTransform {
            velocity_scale: Some(-1.0),
            ..identity_transform()
        };
        let errors = t.validate();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("velocity_scale"));
    }

    #[test]
    fn test_validate_nan_scale() {
        let t = MidiTransform {
            velocity_scale: Some(f32::NAN),
            ..identity_transform()
        };
        assert!(!t.validate().is_empty());
    }

    #[test]
    fn test_validate_multiple_errors() {
        let t = MidiTransform {
            channel: Some(20),
            note: Some(200),
            velocity_scale: Some(f32::INFINITY),
            ..identity_transform()
        };
        assert_eq!(t.validate().len(), 3);
    }

    #[test]
    fn test_lut_serde_rejects_wrong_length() {
        // Too few entries
        let json = r#"{"Lut":[0,1,2]}"#;
        let result: Result<ValueCurve, _> = serde_json::from_str(json);
        assert!(result.is_err());

        // Too many entries (129)
        let arr: Vec<u8> = (0..129).map(|i| i as u8).collect();
        let json = format!("{{\"Lut\":{}}}", serde_json::to_string(&arr).unwrap());
        let result: Result<ValueCurve, _> = serde_json::from_str(&json);
        assert!(result.is_err());
    }
}
