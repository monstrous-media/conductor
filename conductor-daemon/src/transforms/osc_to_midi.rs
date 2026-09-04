// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! OSC → MIDI structured transform (ADR-039-A).
//!
//! The reverse of [`crate::transforms::midi_to_osc`]: an inbound OSC message
//! becomes a 3-byte MIDI Control Change (or Note On). STRUCTURED — it reads the
//! already-decoded [`OscInbound`] threaded through `RouteEvalContext` (no byte
//! re-parse), mirroring the ADR-039-B HID transforms.
//!
//! ## Address → CC/note
//!
//! The CC (or note) number is extracted from the OSC **address** via a template
//! carrying a single `{cc}` (or `{note}`) placeholder — the inverse of the
//! forward `midi_template::substitute` convention, NOT regex (symmetry with
//! `MidiToOsc`, and no ReDoS surface on attacker-supplied addresses).
//! `address_to_cc = "/eos/fader/{cc}"` + address `/eos/fader/7` ⇒ CC 7.
//!
//! ## Security: the extracted value is attacker-controlled
//!
//! The `{cc}`/`{note}` capture comes straight off the wire, so it is parsed
//! **fallibly** and range-checked to `0..=127` **before** any MIDI byte is
//! built — `/eos/fader/999` yields `None` (route skips), never a panic. The
//! argument value is likewise coerced and clamped.

use conductor_core::actions::OscArg;
use conductor_core::config::types::SignalTransform;
use conductor_core::events::OscInbound;

/// Apply a `SignalTransform::OscToMidi` to a decoded OSC message.
///
/// Returns `Some([status, data1, value])` — a 3-byte CC `[0xB0|ch, cc, value]`
/// or Note On `[0x90|ch, note, value]` — or `None` if:
/// - `transform` is not `OscToMidi` (defensive guard),
/// - no rule's address template matches `osc.address`,
/// - the extracted `{cc}`/`{note}` is not parseable or is out of `0..=127`,
/// - `osc.args` is empty or `args[0]` is not coercible to a 7-bit value.
///
/// `address_to_cc` is tried before `address_to_note`; first match wins.
pub fn apply(transform: &SignalTransform, osc: &OscInbound) -> Option<Vec<u8>> {
    let SignalTransform::OscToMidi {
        address_to_cc,
        address_to_note,
        channel,
    } = transform
    else {
        return None;
    };

    // Channel 0-15 is validated at config-load; clamp as defense-in-depth so a
    // bypass can never produce a malformed status byte (saturate, not bit-wrap).
    let ch = channel.unwrap_or(0).min(15);

    // The 7-bit value comes from the first argument.
    let value = coerce_value(osc.args.first()?)?;

    if let Some(template) = address_to_cc.as_deref()
        && let Some(cc) = extract_7bit(template, &osc.address, "{cc}")
    {
        return Some(vec![0xB0 | ch, cc, value]);
    }
    if let Some(template) = address_to_note.as_deref()
        && let Some(note) = extract_7bit(template, &osc.address, "{note}")
    {
        return Some(vec![0x90 | ch, note, value]);
    }
    None
}

/// Extract the integer in `placeholder`'s slot from `address`, matching the
/// literal prefix/suffix of `template`. Returns `None` if the template has no
/// placeholder, the address doesn't fit the prefix/suffix, the capture isn't a
/// non-negative integer, or it exceeds 127 (the attacker-controlled bounds
/// check). Single placeholder only for now (ADR-039-A).
fn extract_7bit(template: &str, address: &str, placeholder: &str) -> Option<u8> {
    let idx = template.find(placeholder)?;
    let prefix = &template[..idx];
    let suffix = &template[idx + placeholder.len()..];
    let captured = address.strip_prefix(prefix)?.strip_suffix(suffix)?;
    // Parse fallibly into a wide type, THEN bounds-check — never feed an
    // unchecked value into a 7-bit MIDI byte.
    let n: u32 = captured.parse().ok()?;
    if n > 127 {
        return None;
    }
    Some(n as u8)
}

/// Coerce the first OSC argument to a 7-bit MIDI value.
/// - `Float`: NaN/±Inf ⇒ `None`; else `round(clamp(f, 0, 1) * 127)` (OSC floats
///   are normalised by convention).
/// - `Int`: `clamp(0, 127)`.
/// - `String`: `None` (not coercible).
fn coerce_value(arg: &OscArg) -> Option<u8> {
    match arg {
        OscArg::Float(f) => {
            if !f.is_finite() {
                None
            } else {
                Some((f.clamp(0.0, 1.0) * 127.0).round() as u8)
            }
        }
        OscArg::Int(i) => Some((*i).clamp(0, 127) as u8),
        OscArg::String(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn osc(address: &str, args: Vec<OscArg>) -> OscInbound {
        OscInbound {
            address: address.to_string(),
            args,
            time: Instant::now(),
        }
    }

    fn osc_to_midi(cc: Option<&str>, note: Option<&str>, channel: Option<u8>) -> SignalTransform {
        SignalTransform::OscToMidi {
            address_to_cc: cc.map(String::from),
            address_to_note: note.map(String::from),
            channel,
        }
    }

    #[test]
    fn fader_float_maps_to_cc() {
        let t = osc_to_midi(Some("/eos/fader/{cc}"), None, None);
        let out = apply(&t, &osc("/eos/fader/7", vec![OscArg::Float(1.0)])).expect("matched");
        assert_eq!(out, vec![0xB0, 7, 127]);
    }

    #[test]
    fn channel_is_honoured() {
        let t = osc_to_midi(Some("/f/{cc}"), None, Some(3));
        let out = apply(&t, &osc("/f/10", vec![OscArg::Float(0.0)])).unwrap();
        assert_eq!(out, vec![0xB0 | 3, 10, 0]);
    }

    #[test]
    fn note_template_emits_note_on() {
        let t = osc_to_midi(None, Some("/note/{note}"), Some(0));
        let out = apply(&t, &osc("/note/60", vec![OscArg::Int(100)])).unwrap();
        assert_eq!(out, vec![0x90, 60, 100]);
    }

    /// The blocker-4 regression lock: an out-of-range address value must yield
    /// `None`, never panic.
    #[test]
    fn out_of_range_cc_in_address_yields_none_not_panic() {
        let t = osc_to_midi(Some("/eos/fader/{cc}"), None, None);
        assert!(apply(&t, &osc("/eos/fader/999", vec![OscArg::Float(0.5)])).is_none());
    }

    #[test]
    fn non_numeric_capture_yields_none() {
        let t = osc_to_midi(Some("/eos/fader/{cc}"), None, None);
        assert!(apply(&t, &osc("/eos/fader/left", vec![OscArg::Float(0.5)])).is_none());
    }

    #[test]
    fn address_no_match_yields_none() {
        let t = osc_to_midi(Some("/eos/fader/{cc}"), None, None);
        assert!(apply(&t, &osc("/other/7", vec![OscArg::Float(0.5)])).is_none());
    }

    #[test]
    fn float_clamps_and_rounds() {
        let t = osc_to_midi(Some("/f/{cc}"), None, None);
        // 0.5 * 127 = 63.5 → round → 64; >1.0 clamps to 127.
        assert_eq!(
            apply(&t, &osc("/f/1", vec![OscArg::Float(0.5)])).unwrap()[2],
            64
        );
        assert_eq!(
            apply(&t, &osc("/f/1", vec![OscArg::Float(2.0)])).unwrap()[2],
            127
        );
    }

    #[test]
    fn nan_and_inf_args_yield_none() {
        let t = osc_to_midi(Some("/f/{cc}"), None, None);
        assert!(apply(&t, &osc("/f/1", vec![OscArg::Float(f32::NAN)])).is_none());
        assert!(apply(&t, &osc("/f/1", vec![OscArg::Float(f32::INFINITY)])).is_none());
    }

    #[test]
    fn int_arg_clamps() {
        let t = osc_to_midi(Some("/f/{cc}"), None, None);
        assert_eq!(
            apply(&t, &osc("/f/1", vec![OscArg::Int(200)])).unwrap()[2],
            127
        );
        assert_eq!(
            apply(&t, &osc("/f/1", vec![OscArg::Int(-5)])).unwrap()[2],
            0
        );
    }

    #[test]
    fn string_arg_and_empty_args_yield_none() {
        let t = osc_to_midi(Some("/f/{cc}"), None, None);
        assert!(apply(&t, &osc("/f/1", vec![OscArg::String("x".into())])).is_none());
        assert!(apply(&t, &osc("/f/1", vec![])).is_none());
    }

    #[test]
    fn cc_tried_before_note() {
        // Both templates could match different addresses; for a cc-matching
        // address the CC arm wins.
        let t = osc_to_midi(Some("/x/{cc}"), Some("/x/{note}"), None);
        let out = apply(&t, &osc("/x/5", vec![OscArg::Int(64)])).unwrap();
        assert_eq!(out[0] & 0xF0, 0xB0, "CC status, not Note On");
    }

    #[test]
    fn wrong_transform_variant_yields_none() {
        let t = SignalTransform::HidToMidi {
            trigger_to_cc: std::collections::HashMap::new(),
            channel: 0,
        };
        assert!(apply(&t, &osc("/f/1", vec![OscArg::Int(1)])).is_none());
    }

    #[test]
    fn channel_overflow_clamped_defensively() {
        let t = osc_to_midi(Some("/f/{cc}"), None, Some(0xFF));
        let out = apply(&t, &osc("/f/1", vec![OscArg::Int(1)])).unwrap();
        assert_eq!(out[0], 0xB0 | 15, "channel saturates to 15");
    }
}
