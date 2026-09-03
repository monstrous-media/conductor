// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! OSC → Art-Net structured transform (ADR-039-A).
//!
//! Mirrors [`crate::transforms::midi_to_artnet`] on the output side (produces
//! a single [`DmxUpdate`] the Art-Net sender merges into its per-connector
//! frame) and [`crate::transforms::osc_to_midi`] on the input side (STRUCTURED
//! — reads the already-decoded [`OscInbound`] threaded through the route
//! engine; no byte re-parse).
//!
//! ## Address → DMX channel
//!
//! The DMX channel is extracted from the OSC **address** via a template
//! carrying a single `{dmx}` placeholder — the same prefix/suffix literal
//! match as `osc_to_midi`'s `{cc}`/`{note}` (no regex, no ReDoS surface).
//! `address_to_dmx = "/dmx/{dmx}"` + address `/dmx/12` ⇒ DMX channel 12.
//!
//! ## Security: the extracted channel is attacker-controlled
//!
//! The `{dmx}` capture comes straight off the wire, so it is parsed
//! **fallibly** and range-checked to the DMX universe `1..=512` **before**
//! any update is built — `/dmx/0`, `/dmx/513`, `/dmx/99999`, `/dmx/left` all
//! yield `None` (route skips), never a panic. Same security
//! reasoning as `osc_to_midi`.
//!
//! ## Value coercion
//!
//! The 8-bit DMX level comes from the first OSC argument:
//! - `Float`: NaN/±Inf ⇒ `None`; else `round(clamp(f, 0, 1) * 255)` (OSC
//!   floats are normalised by convention — same as `osc_to_midi`, scaled to
//!   the 8-bit DMX range instead of 7-bit MIDI).
//! - `Int`: `clamp(0, 255)`.
//! - `String`: `None` (not coercible).

use conductor_core::actions::OscArg;
use conductor_core::config::types::SignalTransform;
use conductor_core::events::OscInbound;

use super::midi_to_artnet::DmxUpdate;

/// Apply a `SignalTransform::OscToArtNet` to a decoded OSC message.
///
/// Returns `Some(DmxUpdate)` or `None` if:
/// - `transform` is not `OscToArtNet` (defensive guard so callers can
///   dispatch uniformly without pre-matching the enum),
/// - the address doesn't fit the template's prefix/suffix,
/// - the extracted `{dmx}` is not parseable or is outside `1..=512`,
/// - `osc.args` is empty or `args[0]` is not coercible to an 8-bit level.
pub fn apply(transform: &SignalTransform, osc: &OscInbound) -> Option<DmxUpdate> {
    let SignalTransform::OscToArtNet { address_to_dmx } = transform else {
        return None;
    };

    let channel = extract_dmx_channel(address_to_dmx, &osc.address)?;
    let value = coerce_level(osc.args.first()?)?;

    Some(DmxUpdate { channel, value })
}

/// Extract the DMX channel from `address` per `template`'s `{dmx}` slot.
/// Returns `None` if the template has no placeholder, the address doesn't fit
/// the prefix/suffix, the capture isn't a non-negative integer, or it is
/// outside the DMX universe `1..=512` (the attacker-controlled bounds check).
fn extract_dmx_channel(template: &str, address: &str) -> Option<u16> {
    let idx = template.find("{dmx}")?;
    let prefix = &template[..idx];
    let suffix = &template[idx + "{dmx}".len()..];
    let captured = address.strip_prefix(prefix)?.strip_suffix(suffix)?;
    // Parse fallibly into a wide type, THEN bounds-check — never feed an
    // unchecked wire value into a DMX frame index.
    let n: u32 = captured.parse().ok()?;
    if !(1..=512).contains(&n) {
        return None;
    }
    Some(n as u16)
}

/// Coerce the first OSC argument to an 8-bit DMX level (see module doc).
fn coerce_level(arg: &OscArg) -> Option<u8> {
    match arg {
        OscArg::Float(f) => {
            if !f.is_finite() {
                None
            } else {
                Some((f.clamp(0.0, 1.0) * 255.0).round() as u8)
            }
        }
        OscArg::Int(i) => Some((*i).clamp(0, 255) as u8),
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

    fn transform(template: &str) -> SignalTransform {
        SignalTransform::OscToArtNet {
            address_to_dmx: template.to_string(),
        }
    }

    #[test]
    fn float_arg_maps_to_scaled_dmx_level() {
        let t = transform("/dmx/{dmx}");
        let u = apply(&t, &osc("/dmx/12", vec![OscArg::Float(1.0)])).expect("matched");
        assert_eq!(u.channel, 12);
        assert_eq!(u.value, 255);

        // 0.5 * 255 = 127.5 → round → 128
        let u = apply(&t, &osc("/dmx/12", vec![OscArg::Float(0.5)])).unwrap();
        assert_eq!(u.value, 128);
    }

    #[test]
    fn float_clamps_to_unit_range() {
        let t = transform("/dmx/{dmx}");
        assert_eq!(
            apply(&t, &osc("/dmx/1", vec![OscArg::Float(2.0)]))
                .unwrap()
                .value,
            255
        );
        assert_eq!(
            apply(&t, &osc("/dmx/1", vec![OscArg::Float(-1.0)]))
                .unwrap()
                .value,
            0
        );
    }

    #[test]
    fn int_arg_clamps_to_dmx_level() {
        let t = transform("/dmx/{dmx}");
        assert_eq!(
            apply(&t, &osc("/dmx/1", vec![OscArg::Int(300)]))
                .unwrap()
                .value,
            255
        );
        assert_eq!(
            apply(&t, &osc("/dmx/1", vec![OscArg::Int(-5)]))
                .unwrap()
                .value,
            0
        );
        assert_eq!(
            apply(&t, &osc("/dmx/1", vec![OscArg::Int(64)]))
                .unwrap()
                .value,
            64
        );
    }

    /// The attacker-controlled bounds check: out-of-universe channels yield
    /// `None`, never panic and never index a frame out of range.
    #[test]
    fn out_of_range_dmx_channel_yields_none_not_panic() {
        let t = transform("/dmx/{dmx}");
        assert!(
            apply(&t, &osc("/dmx/0", vec![OscArg::Int(1)])).is_none(),
            "0 below universe"
        );
        assert!(
            apply(&t, &osc("/dmx/513", vec![OscArg::Int(1)])).is_none(),
            "513 above universe"
        );
        assert!(
            apply(&t, &osc("/dmx/99999999999", vec![OscArg::Int(1)])).is_none(),
            "u32 overflow parses fallibly"
        );
    }

    #[test]
    fn boundary_channels_accepted() {
        let t = transform("/dmx/{dmx}");
        assert_eq!(
            apply(&t, &osc("/dmx/1", vec![OscArg::Int(1)]))
                .unwrap()
                .channel,
            1
        );
        assert_eq!(
            apply(&t, &osc("/dmx/512", vec![OscArg::Int(1)]))
                .unwrap()
                .channel,
            512
        );
    }

    #[test]
    fn non_numeric_capture_yields_none() {
        let t = transform("/dmx/{dmx}");
        assert!(apply(&t, &osc("/dmx/left", vec![OscArg::Int(1)])).is_none());
        assert!(
            apply(&t, &osc("/dmx/-3", vec![OscArg::Int(1)])).is_none(),
            "sign rejected"
        );
    }

    #[test]
    fn address_not_matching_template_yields_none() {
        let t = transform("/dmx/{dmx}");
        assert!(apply(&t, &osc("/other/7", vec![OscArg::Int(1)])).is_none());
        assert!(apply(&t, &osc("/dmx/7/extra", vec![OscArg::Int(1)])).is_none());
    }

    #[test]
    fn template_with_suffix_matches() {
        let t = transform("/fixture/{dmx}/level");
        let u = apply(&t, &osc("/fixture/42/level", vec![OscArg::Float(1.0)])).unwrap();
        assert_eq!(u.channel, 42);
    }

    #[test]
    fn nan_inf_string_and_empty_args_yield_none() {
        let t = transform("/dmx/{dmx}");
        assert!(apply(&t, &osc("/dmx/1", vec![OscArg::Float(f32::NAN)])).is_none());
        assert!(apply(&t, &osc("/dmx/1", vec![OscArg::Float(f32::INFINITY)])).is_none());
        assert!(apply(&t, &osc("/dmx/1", vec![OscArg::String("x".into())])).is_none());
        assert!(apply(&t, &osc("/dmx/1", vec![])).is_none());
    }

    #[test]
    fn wrong_transform_variant_yields_none() {
        let t = SignalTransform::OscToMidi {
            address_to_cc: Some("/f/{cc}".into()),
            address_to_note: None,
            channel: None,
        };
        assert!(apply(&t, &osc("/dmx/1", vec![OscArg::Int(1)])).is_none());
    }

    #[test]
    fn template_without_placeholder_yields_none() {
        let t = transform("/dmx/static");
        assert!(apply(&t, &osc("/dmx/static", vec![OscArg::Int(1)])).is_none());
    }
}
