// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Wire-format MIDI byte reconstruction from `InputEvent`.
//!
//! Used by `MidiForward` (engine_manager dispatch path) to forward
//! incoming MIDI to a target output port. The source channel is
//! preserved in the status byte's low nibble; gamepad / synthetic
//! events with `channel: None` default to channel 0.
//!
//! Extracted from `engine_manager.rs` so the small surface (one
//! function + tests) can be reviewed in full by tooling that has
//! input-size limits — the original engine_manager.rs is ~7400 lines
//! and Council-style review tools truncate that. (#1119 follow-up.)

use conductor_core::events::InputEvent;

/// Extract raw MIDI bytes from an InputEvent for MidiForward.
///
/// Reconstructs standard MIDI bytes from the InputEvent. The source
/// channel (added to InputEvent variants by PR #434, the MIDI channel
/// pipeline) is preserved in the status-byte low nibble. `channel:
/// None` (gamepad or synthetic events) defaults to channel 0.
///
/// `EncoderTurned` returns `None` because encoder gestures are a
/// derived ProcessedEvent without a wire-format MIDI representation.
/// MidiForward callers handle `None` by surfacing
/// `"MidiForward requires raw MIDI bytes in trigger context"` rather
/// than emitting garbage.
///
/// History (#1119): pre-fix this function discarded `channel` via `..`
/// and hardcoded the status byte to 0, silently breaking MidiForward
/// to any receiver configured to ignore channel 1.
pub(crate) fn extract_raw_midi(event: &InputEvent) -> Option<Vec<u8>> {
    // Channel-voice messages encode channel in the status byte's low
    // nibble. Helper keeps the per-variant arms terse. Defensive
    // masks on both inputs (`status_high & 0xF0` keeps only the high
    // nibble; `ch & 0x0F` clamps channel to 4 bits) make the bit-or
    // pattern correct even if a caller passes a malformed status
    // byte or an out-of-range channel value. Council follow-up on
    // PR #1124.
    let with_ch = |status_high: u8, ch: Option<u8>| (status_high & 0xF0) | (ch.unwrap_or(0) & 0x0F);

    match event {
        InputEvent::PadPressed {
            pad,
            velocity,
            channel,
            ..
        } => Some(vec![with_ch(0x90, *channel), *pad & 0x7F, *velocity & 0x7F]),
        InputEvent::PadReleased { pad, channel, .. } => {
            Some(vec![with_ch(0x80, *channel), *pad & 0x7F, 0])
        }
        InputEvent::ControlChange {
            control,
            value,
            channel,
            ..
        } => Some(vec![
            with_ch(0xB0, *channel),
            *control & 0x7F,
            *value & 0x7F,
        ]),
        InputEvent::Aftertouch {
            pressure, channel, ..
        } => Some(vec![with_ch(0xD0, *channel), *pressure & 0x7F]),
        InputEvent::PitchBend { value, channel, .. } => {
            let lsb = (*value & 0x7F) as u8;
            let msb = ((*value >> 7) & 0x7F) as u8;
            Some(vec![with_ch(0xE0, *channel), lsb, msb])
        }
        InputEvent::ProgramChange {
            program, channel, ..
        } => Some(vec![with_ch(0xC0, *channel), *program & 0x7F]),
        InputEvent::PolyPressure {
            pad,
            pressure,
            channel,
            ..
        } => Some(vec![with_ch(0xA0, *channel), *pad & 0x7F, *pressure & 0x7F]),
        InputEvent::EncoderTurned { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn time_now() -> Instant {
        Instant::now()
    }

    // ── Channel preservation per InputEvent variant (#1119) ──
    //
    // Pre-fix the function dropped channel from every variant via `..`
    // and hardcoded the status nibble to 0 (channel 1 on the wire).
    // ADR-009 Gap 2's stale comment claimed "InputEvent strips channel
    // info" — that was true once, but PR #434 (MIDI channel pipeline)
    // added `channel: Option<u8>` to every InputEvent variant. These
    // tests pin the post-fix behaviour: channel-N source produces
    // channel-N status byte; channel: None defaults to 0.

    #[test]
    fn extract_raw_midi_preserves_channel_pad_pressed() {
        for ch in [0u8, 5, 9, 15] {
            let ev = InputEvent::PadPressed {
                pad: 60,
                velocity: 100,
                channel: Some(ch),
                time: time_now(),
            };
            let bytes = extract_raw_midi(&ev).expect("PadPressed must produce bytes");
            assert_eq!(
                bytes[0],
                0x90 | ch,
                "Channel {} must be encoded into status byte; got 0x{:02X}",
                ch,
                bytes[0]
            );
            assert_eq!(bytes[1], 60, "Note number unchanged");
            assert_eq!(bytes[2], 100, "Velocity unchanged");
        }
    }

    #[test]
    fn extract_raw_midi_preserves_channel_pad_released() {
        for ch in [0u8, 7, 15] {
            let ev = InputEvent::PadReleased {
                pad: 60,
                channel: Some(ch),
                time: time_now(),
            };
            let bytes = extract_raw_midi(&ev).expect("PadReleased must produce bytes");
            assert_eq!(bytes[0], 0x80 | ch, "ch {} → status 0x{:02X}", ch, bytes[0]);
        }
    }

    #[test]
    fn extract_raw_midi_preserves_channel_control_change() {
        for ch in [0u8, 3, 15] {
            let ev = InputEvent::ControlChange {
                control: 7,
                value: 64,
                channel: Some(ch),
                time: time_now(),
            };
            let bytes = extract_raw_midi(&ev).expect("CC must produce bytes");
            assert_eq!(bytes[0], 0xB0 | ch, "ch {} → status 0x{:02X}", ch, bytes[0]);
        }
    }

    #[test]
    fn extract_raw_midi_preserves_channel_aftertouch() {
        for ch in [0u8, 9, 15] {
            let ev = InputEvent::Aftertouch {
                pressure: 80,
                channel: Some(ch),
                time: time_now(),
            };
            let bytes = extract_raw_midi(&ev).expect("Aftertouch must produce bytes");
            assert_eq!(bytes[0], 0xD0 | ch, "ch {} → status 0x{:02X}", ch, bytes[0]);
        }
    }

    #[test]
    fn extract_raw_midi_preserves_channel_pitch_bend() {
        for ch in [0u8, 5, 15] {
            let ev = InputEvent::PitchBend {
                value: 8192,
                channel: Some(ch),
                time: time_now(),
            };
            let bytes = extract_raw_midi(&ev).expect("PitchBend must produce bytes");
            assert_eq!(bytes[0], 0xE0 | ch, "ch {} → status 0x{:02X}", ch, bytes[0]);
        }
    }

    #[test]
    fn extract_raw_midi_preserves_channel_program_change() {
        for ch in [0u8, 11, 15] {
            let ev = InputEvent::ProgramChange {
                program: 42,
                channel: Some(ch),
                time: time_now(),
            };
            let bytes = extract_raw_midi(&ev).expect("PC must produce bytes");
            assert_eq!(bytes[0], 0xC0 | ch, "ch {} → status 0x{:02X}", ch, bytes[0]);
            assert_eq!(bytes[1], 42, "Program number unchanged");
            assert_eq!(bytes.len(), 2, "PC is a 2-byte message");
        }
    }

    #[test]
    fn extract_raw_midi_preserves_channel_poly_pressure() {
        for ch in [0u8, 4, 15] {
            let ev = InputEvent::PolyPressure {
                pad: 60,
                pressure: 90,
                channel: Some(ch),
                time: time_now(),
            };
            let bytes = extract_raw_midi(&ev).expect("PolyPressure must produce bytes");
            assert_eq!(bytes[0], 0xA0 | ch, "ch {} → status 0x{:02X}", ch, bytes[0]);
        }
    }

    #[test]
    fn extract_raw_midi_defaults_to_channel_0_when_none() {
        // Gamepad / synthetic events have channel: None — extract_raw_midi
        // should default to channel 0 (status nibble = 0).
        let ev = InputEvent::PadPressed {
            pad: 60,
            velocity: 100,
            channel: None,
            time: time_now(),
        };
        let bytes = extract_raw_midi(&ev).expect("PadPressed without channel still produces bytes");
        assert_eq!(
            bytes[0], 0x90,
            "channel: None must default to status 0x90 (channel 0)"
        );
    }

    #[test]
    fn extract_raw_midi_pitch_bend_byte_order_lsb_first() {
        // Pin the MIDI wire-format byte order for PitchBend: status,
        // then LSB (low 7 bits), then MSB (high 7 bits of the 14-bit
        // value). Council follow-up on PR #1124 — the prior tests
        // didn't assert byte positions explicitly, only the status.
        // Use 0x2123 (decimal 8483) so LSB and MSB have different
        // values that wouldn't pass a swapped check.
        // 0x2123 = 0b010_0001_0010_0011
        // low 7 bits  (LSB) = 0b0100011 = 0x23
        // high 7 bits (MSB) = 0b1000010 = 0x42
        let ev = InputEvent::PitchBend {
            value: 0x2123,
            channel: Some(0),
            time: time_now(),
        };
        let bytes = extract_raw_midi(&ev).expect("PitchBend must produce bytes");
        assert_eq!(bytes.len(), 3, "PitchBend is a 3-byte message");
        assert_eq!(bytes[0], 0xE0, "Status byte: 0xE0 (channel 0)");
        assert_eq!(bytes[1], 0x23, "byte[1] = LSB (low 7 bits of value)");
        assert_eq!(bytes[2], 0x42, "byte[2] = MSB (next 7 bits of value)");
    }

    #[test]
    fn extract_raw_midi_clamps_out_of_range_channel() {
        // Channel field is `Option<u8>` but upstream code is supposed
        // to pass 0-15. Defensive: even if someone passes an
        // out-of-range value (16+), the `& 0x0F` mask in `with_ch`
        // clamps to 4 bits — never produces a garbled status byte
        // with bits leaking into the high nibble. Council follow-up
        // on PR #1124.
        let ev = InputEvent::PadPressed {
            pad: 60,
            velocity: 100,
            channel: Some(99), // out of range; binary 0110_0011
            time: time_now(),
        };
        let bytes = extract_raw_midi(&ev).expect("PadPressed must produce bytes");
        // 99 & 0x0F = 0x03, so status byte should be 0x90 | 0x03 = 0x93,
        // NOT 0x90 | 0x63 = 0xF3 (which would be a System Real-Time
        // message — actively dangerous to emit).
        assert_eq!(
            bytes[0], 0x93,
            "Out-of-range channel must be clamped to low 4 bits, not bleed into status nibble"
        );
        assert_eq!(
            bytes[0] & 0xF0,
            0x90,
            "Status nibble must remain 0x9 (NoteOn), never escape into 0xF (system messages)"
        );
    }

    #[test]
    fn extract_raw_midi_returns_none_for_encoder_turned() {
        // Encoders don't have standard MIDI bytes — they're a derived
        // ProcessedEvent gesture, not a wire-format MIDI message. The
        // function intentionally returns None so MidiForward errors out
        // with the explicit "MidiForward requires raw MIDI bytes in
        // trigger context" instead of silently emitting garbage.
        // (Copilot review on PR #1124 — pin on every channel value to
        // make the intent unambiguous and catch a future contributor
        // who tries to "fix" None by reconstructing CC bytes.)
        for ch in [None, Some(0u8), Some(15u8)] {
            let ev = InputEvent::EncoderTurned {
                encoder: 1,
                value: 64,
                channel: ch,
                analog: None,
                time: time_now(),
            };
            assert!(
                extract_raw_midi(&ev).is_none(),
                "EncoderTurned (channel = {:?}) must return None",
                ch
            );
        }
    }
}
