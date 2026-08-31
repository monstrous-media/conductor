// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! MIDI → OSC transform (ADR-031 § 7.1 / #1145 P5 slice 1).
//!
//! Translates raw MIDI bytes into encoded OSC packets based on a
//! `SignalTransform::MidiToOsc` config. Pure function — no I/O. The
//! caller (slice 3's stage-9 cross-protocol dispatch) wires the
//! resulting bytes to an OSC sender (slice 2's registry-managed
//! `UdpSocket`).
//!
//! ## Supported MIDI inputs
//!
//! - `ControlChange` — translates if `cc_to_address` template is set
//! - `NoteOn` (velocity > 0) — translates if `note_to_address` template is set
//!
//! Other MIDI types (NoteOff, PolyAftertouch, ProgramChange,
//! ChannelAftertouch, PitchBend) are valid MIDI but have no
//! template-driven OSC mapping in `MidiToOsc`. They return `None`.
//! Future template fields (e.g. `pitchbend_to_address`) would slot in
//! the same shape.
//!
//! ## Template substitution
//!
//! Address templates substitute `{cc}` / `{value}` (CC) or `{note}` /
//! `{velocity}` (NoteOn) with the integer values from the MIDI message.
//! Templates that omit the placeholders are valid — `/eos/cue/fire` is
//! a perfectly reasonable target for any CC matching the route.
//!
//! ## `value_to_float`
//!
//! When `true`, the OSC payload is a single `Float` arg in 0.0-1.0
//! (linear scale from MIDI 0-127). When `false`, payload is a single
//! `Int` arg with the raw 0-127 value. Lighting consoles like
//! Eos/Hog want floats; some MaxMSP patches want ints.

use conductor_core::config::types::SignalTransform;
use conductor_core::transform::MidiMessage;
use rosc::encoder;
use rosc::{OscMessage, OscPacket, OscType};

/// Apply a `SignalTransform::MidiToOsc` to raw MIDI bytes.
///
/// Returns `Some(osc_packet_bytes)` for a successful translation, or
/// `None` if:
/// - `transform` is not `SignalTransform::MidiToOsc` (caller routed
///   the wrong variant — defensive guard so callers can dispatch
///   uniformly without pre-matching the enum)
/// - The MIDI bytes don't parse (invalid / truncated / SysEx)
/// - The MIDI message type has no matching template field set
///   (e.g. CC message with `cc_to_address: None`)
/// - The encoded OSC packet fails to serialize (shouldn't happen in
///   practice — `rosc::encoder::encode` only fails on malformed
///   inputs, but we don't `unwrap()` because it's a system boundary)
///
/// The pure-function shape (`config + bytes → Option<bytes>`) mirrors
/// `MidiTransform::apply` so slice 3's stage-9 dispatch can branch on
/// transform variant uniformly.
pub fn apply(transform: &SignalTransform, midi_bytes: &[u8]) -> Option<Vec<u8>> {
    let SignalTransform::MidiToOsc {
        cc_to_address,
        note_to_address,
        value_to_float,
    } = transform
    else {
        return None;
    };

    let parsed = MidiMessage::parse(midi_bytes)?;

    let (address_template, arg_value) = match parsed {
        MidiMessage::ControlChange { cc, value, .. } => {
            let template = cc_to_address.as_deref()?;
            let address = substitute_cc(template, cc, value);
            (address, value)
        }
        MidiMessage::NoteOn { note, velocity, .. } => {
            // NoteOn with velocity=0 is functionally NoteOff per MIDI
            // spec. Don't synthesize OSC for those — let the route
            // skip silently rather than emit a "note pressed at 0
            // velocity" OSC message that downstream consumers
            // misinterpret.
            if velocity == 0 {
                return None;
            }
            let template = note_to_address.as_deref()?;
            let address = substitute_note(template, note, velocity);
            (address, velocity)
        }
        // Other MIDI types have no template field in MidiToOsc today.
        // See module-level doc — they fall through here intentionally.
        _ => return None,
    };

    let arg = if *value_to_float {
        OscType::Float(f32::from(arg_value) / 127.0)
    } else {
        OscType::Int(i32::from(arg_value))
    };

    let packet = OscPacket::Message(OscMessage {
        addr: address_template,
        args: vec![arg],
    });

    // `rosc::encoder::encode` returns Result; the only realistic
    // failure is an oversized address string. Bubble it as None so
    // the dispatch path can record a route-side error rather than
    // panic.
    encoder::encode(&packet).ok()
}

/// Substitute `{cc}` and `{value}` placeholders in a CC template.
///
/// Simple string-replace — no regex, no shell-style escaping. A
/// template like `/eos/fader/{cc}/{value}` becomes `/eos/fader/7/100`
/// for CC 7 value 100. Templates without placeholders are returned
/// as-is.
fn substitute_cc(template: &str, cc: u8, value: u8) -> String {
    // Shared substitution primitive (ADR-038 R2 P5) so the placeholder
    // vocabulary stays in sync with the Tap action.
    crate::midi_template::substitute(template, &[("{cc}", cc), ("{value}", value)])
}

/// Substitute `{note}` and `{velocity}` placeholders in a Note
/// template.
fn substitute_note(template: &str, note: u8, velocity: u8) -> String {
    crate::midi_template::substitute(template, &[("{note}", note), ("{velocity}", velocity)])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: parse an OSC packet's first message and return
    /// `(address, args)`. Panics on malformed input — tests only.
    fn decode_message(bytes: &[u8]) -> (String, Vec<OscType>) {
        let packet = rosc::decoder::decode_udp(bytes)
            .expect("test fixture must produce decodable OSC")
            .1;
        match packet {
            OscPacket::Message(msg) => (msg.addr, msg.args),
            OscPacket::Bundle(_) => panic!("MidiToOsc::apply must produce a Message, not Bundle"),
        }
    }

    fn cc_to_osc(template: &str, value_to_float: bool) -> SignalTransform {
        SignalTransform::MidiToOsc {
            cc_to_address: Some(template.to_string()),
            note_to_address: None,
            value_to_float,
        }
    }

    fn note_to_osc(template: &str, value_to_float: bool) -> SignalTransform {
        SignalTransform::MidiToOsc {
            cc_to_address: None,
            note_to_address: Some(template.to_string()),
            value_to_float,
        }
    }

    #[test]
    fn cc_message_translates_with_cc_and_value_substitution() {
        // CC 7 (volume) on channel 0, value 100 (0xB0 0x07 0x64).
        let transform = cc_to_osc("/eos/fader/{cc}/{value}", false);
        let bytes = apply(&transform, &[0xB0, 0x07, 0x64]).expect("must produce OSC packet");
        let (addr, args) = decode_message(&bytes);
        assert_eq!(addr, "/eos/fader/7/100");
        assert_eq!(args, vec![OscType::Int(100)]);
    }

    #[test]
    fn cc_message_value_to_float_quantizes_0_to_127_into_0_to_1() {
        // Value 127 (max) → 1.0; value 0 → 0.0; value 64 → ~0.504.
        let transform = cc_to_osc("/x", true);

        let (_, args) = decode_message(&apply(&transform, &[0xB0, 0x07, 0x7F]).unwrap());
        assert_eq!(args, vec![OscType::Float(1.0)]);

        let (_, args) = decode_message(&apply(&transform, &[0xB0, 0x07, 0x00]).unwrap());
        assert_eq!(args, vec![OscType::Float(0.0)]);

        let (_, args) = decode_message(&apply(&transform, &[0xB0, 0x07, 0x40]).unwrap());
        match args[0] {
            OscType::Float(v) => assert!(
                (v - (64.0 / 127.0)).abs() < 1e-6,
                "value 64 must quantize to 64/127 (got {v})"
            ),
            _ => panic!("expected Float arg"),
        }
    }

    #[test]
    fn note_on_translates_with_note_and_velocity_substitution() {
        // NoteOn channel 0, note 60 (middle C), velocity 80
        // (0x90 0x3C 0x50).
        let transform = note_to_osc("/synth/note/{note}/vel/{velocity}", false);
        let bytes = apply(&transform, &[0x90, 0x3C, 0x50]).unwrap();
        let (addr, args) = decode_message(&bytes);
        assert_eq!(addr, "/synth/note/60/vel/80");
        assert_eq!(args, vec![OscType::Int(80)]);
    }

    #[test]
    fn note_on_velocity_zero_returns_none() {
        // NoteOn vel=0 is MIDI-spec NoteOff. Don't synthesize an OSC
        // event for it — the route should skip silently rather than
        // emit a misleading "0-velocity press".
        let transform = note_to_osc("/synth/note/{note}", false);
        assert!(apply(&transform, &[0x90, 0x3C, 0x00]).is_none());
    }

    #[test]
    fn cc_without_cc_to_address_returns_none() {
        // Transform configured for NoteOn only; CC bytes fall through.
        let transform = note_to_osc("/synth/note/{note}", false);
        assert!(apply(&transform, &[0xB0, 0x07, 0x64]).is_none());
    }

    #[test]
    fn note_without_note_to_address_returns_none() {
        // Transform configured for CC only; NoteOn bytes fall through.
        let transform = cc_to_osc("/eos/fader/{cc}/{value}", false);
        assert!(apply(&transform, &[0x90, 0x3C, 0x50]).is_none());
    }

    #[test]
    fn non_midi_to_osc_variant_returns_none() {
        // Defensive: the dispatch path can call `apply` with any
        // transform variant; only MidiToOsc produces output. This
        // guards against accidental cross-variant dispatch routing.
        use conductor_core::transform::MidiTransform as MidiTransformCore;
        let transform = SignalTransform::Midi(MidiTransformCore {
            channel: None,
            cc: None,
            note: None,
            velocity_scale: None,
            velocity_offset: None,
            invert_value: false,
            curve: None,
        });
        assert!(apply(&transform, &[0xB0, 0x07, 0x64]).is_none());
    }

    #[test]
    fn invalid_midi_bytes_return_none() {
        // SysEx (0xF0) is rejected by MidiMessage::parse; truncated
        // CC (just status byte) is also rejected.
        let transform = cc_to_osc("/x", false);
        assert!(
            apply(&transform, &[0xF0, 0x7E, 0x00]).is_none(),
            "SysEx must not transform"
        );
        assert!(
            apply(&transform, &[0xB0]).is_none(),
            "truncated CC must not transform"
        );
        assert!(apply(&transform, &[]).is_none(), "empty must not transform");
    }

    #[test]
    fn templates_without_placeholders_round_trip_literally() {
        // /eos/cue/fire has no `{cc}` or `{value}` — that's fine,
        // valid OSC address, sent as-is on every matching CC.
        let transform = cc_to_osc("/eos/cue/fire", false);
        let bytes = apply(&transform, &[0xB0, 0x07, 0x64]).unwrap();
        let (addr, _) = decode_message(&bytes);
        assert_eq!(addr, "/eos/cue/fire");
    }

    #[test]
    fn partial_template_substitutions_leave_unknown_placeholders_intact() {
        // A typo or future-placeholder (e.g. `{channel}`, not in
        // MidiToOsc today) passes through unchanged rather than
        // crashing. Conservative — keeps the template-substitution
        // rules forward-compatible.
        let transform = cc_to_osc("/x/{cc}/{channel}", false);
        let bytes = apply(&transform, &[0xB0, 0x07, 0x64]).unwrap();
        let (addr, _) = decode_message(&bytes);
        assert_eq!(addr, "/x/7/{channel}");
    }

    #[test]
    fn polyaftertouch_program_change_pitchbend_return_none() {
        // These MIDI types have no template field in MidiToOsc today.
        // Pinning the "fall through to None" behavior so a future
        // change that accidentally synthesizes OSC for them surfaces
        // here.
        let transform = cc_to_osc("/x", false);
        assert!(
            apply(&transform, &[0xA0, 0x3C, 0x50]).is_none(),
            "PolyAftertouch"
        );
        assert!(apply(&transform, &[0xC0, 0x05]).is_none(), "ProgramChange");
        assert!(
            apply(&transform, &[0xE0, 0x00, 0x40]).is_none(),
            "PitchBend"
        );
        assert!(
            apply(&transform, &[0xD0, 0x50]).is_none(),
            "ChannelAftertouch"
        );
        // NoteOff: also no template path in MidiToOsc.
        assert!(apply(&transform, &[0x80, 0x3C, 0x50]).is_none(), "NoteOff");
    }
}
