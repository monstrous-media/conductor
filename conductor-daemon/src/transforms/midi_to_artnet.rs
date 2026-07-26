// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! MIDI → Art-Net transform (ADR-031 § 7.3 / #1145 P5 slice 5).
//!
//! Translates raw MIDI bytes into a single DMX channel-update record
//! based on a `SignalTransform::MidiToArtNet` config. Pure function —
//! no I/O, no state. The caller (slice 6's Art-Net sender) merges the
//! update into a persistent per-connector DMX frame and emits OpDmx
//! UDP packets.
//!
//! ## Supported MIDI inputs
//!
//! - `ControlChange` — translates if the CC number is in `cc_to_dmx`
//! - `NoteOn` (velocity > 0) — translates if the note number is in `note_to_dmx`
//!
//! Other MIDI types (NoteOff, PolyAftertouch, ProgramChange,
//! ChannelAftertouch, PitchBend) have no mapping table in
//! `MidiToArtNet` today. They return `None`. NoteOn with velocity=0
//! is MIDI-spec NoteOff and also returns None (consistent with
//! `midi_to_osc` slice 1 — don't synthesize a fake "0-intensity"
//! DMX update from what's semantically a release).
//!
//! ## Value scaling
//!
//! MIDI values are 7-bit (0-127); DMX channel levels are 8-bit (0-255).
//! The scale formula `(v as u16 * 255 / 127)` is exact: 0 → 0,
//! 127 → 255, 64 → 128 (≈ 50% intensity). No floating-point.
//!
//! ## MIDI channel awareness
//!
//! The current `SignalTransform::MidiToArtNet` config keys mappings by
//! CC number / note number ONLY — it ignores the MIDI channel. A `CC 7`
//! event on channel 0 and channel 9 both look up `cc_to_dmx[7]` and
//! produce the same DMX update. This is the design today; per-channel
//! mappings would require a config-shape change
//! (`HashMap<(u8, u8), u16>` keyed by `(channel, cc)`). If the user
//! needs per-channel DMX routing, the workaround is two `[[routes]]`
//! with `filter = { channels = [N] }` and different transform configs,
//! since the filter applies BEFORE the transform.
//!
//! ## What this slice does NOT do
//!
//! - **No Art-Net frame construction** — that's slice 6 (`artnet_send`
//!   in `connector_registry`). The transform produces a single channel
//!   update; the sender holds the frame state and emits OpDmx packets.
//! - **No Art-Net validation** — DMX channel addresses (1-512 per
//!   universe) and universe values (0-32767) are config-time concerns;
//!   the validator catches out-of-range values at config-load. This
//!   transform trusts its input.

use conductor_core::config::types::SignalTransform;
use conductor_core::transform::MidiMessage;

/// A single DMX channel update produced by `MidiToArtNet`. Slice 6's
/// Art-Net sender accumulates these into a per-connector DMX frame
/// before emitting OpDmx packets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmxUpdate {
    /// DMX channel address (1-512 per universe; the validator
    /// enforces the range at config-load — this struct trusts its
    /// input).
    pub channel: u16,
    /// DMX channel level, 0-255. Scaled from the 7-bit MIDI value
    /// per the formula in the module-level doc.
    pub value: u8,
}

/// Apply a `SignalTransform::MidiToArtNet` to raw MIDI bytes.
///
/// Returns `Some(DmxUpdate)` for a successful translation, or `None`
/// if:
/// - `transform` is not `SignalTransform::MidiToArtNet` (caller routed
///   the wrong variant — defensive guard so callers can dispatch
///   uniformly without pre-matching the enum)
/// - The MIDI bytes don't parse (invalid / truncated / SysEx)
/// - The MIDI message type has no mapping table in `MidiToArtNet`
///   (e.g. PitchBend, ProgramChange)
/// - The CC number / note number is not in the relevant mapping table
/// - The MIDI message is NoteOn with velocity 0 (MIDI-spec NoteOff)
///
/// Shape mirrors `midi_to_osc::apply` so slice 6's stage-9 dispatch
/// can branch on transform variant uniformly.
pub fn apply(transform: &SignalTransform, midi_bytes: &[u8]) -> Option<DmxUpdate> {
    let SignalTransform::MidiToArtNet {
        cc_to_dmx,
        note_to_dmx,
    } = transform
    else {
        return None;
    };

    let parsed = MidiMessage::parse(midi_bytes)?;

    let (channel, raw_value) = match parsed {
        MidiMessage::ControlChange { cc, value, .. } => {
            let mapped = *cc_to_dmx.get(&cc)?;
            (mapped, value)
        }
        MidiMessage::NoteOn { note, velocity, .. } => {
            if velocity == 0 {
                // MIDI-spec NoteOff equivalent. Don't synthesize a
                // "0-intensity" DMX update from what's semantically a
                // release — let the route skip silently. Same
                // convention as midi_to_osc::apply.
                return None;
            }
            let mapped = *note_to_dmx.get(&note)?;
            (mapped, velocity)
        }
        // Other MIDI types have no mapping table in MidiToArtNet today.
        // See module-level doc — they fall through here intentionally.
        _ => return None,
    };

    Some(DmxUpdate {
        channel,
        value: scale_7bit_to_8bit(raw_value),
    })
}

/// Scale a 7-bit MIDI value (0-127) to an 8-bit DMX level (0-255).
///
/// Exact rational formula: `floor(v * 255 / 127)`. The 16-bit
/// intermediate prevents overflow (max product is `127 * 255 = 32385`).
/// Result is always representable as u8 because `v ≤ 127` ⇒
/// `v * 255 / 127 ≤ 255` exactly.
///
/// Endpoint mapping (pinned by tests): 0 → 0, 127 → 255, 64 → 128.
///
/// **Invariant**: `v ≤ 127`. Upstream `MidiMessage::parse()` rejects
/// data bytes with bit 7 set per MIDI spec, so this should hold for
/// every call from `apply()`. Defense-in-depth via `debug_assert!` —
/// if a future caller bypasses the parser (or the parser invariant
/// silently regresses), debug builds panic loudly here rather than
/// silently producing wrapped DMX values via the `as u8` cast
/// (Council review on PR #1349). Release builds clamp via `.min(127)`
/// so the cast can never wrap regardless.
fn scale_7bit_to_8bit(v: u8) -> u8 {
    debug_assert!(
        v <= 127,
        "scale_7bit_to_8bit called with out-of-range value {v}; \
         upstream MidiMessage::parse() should reject these"
    );
    let clamped = v.min(127);
    ((u16::from(clamped) * 255) / 127) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn cc_transform(mappings: &[(u8, u16)]) -> SignalTransform {
        let mut cc_to_dmx = HashMap::new();
        for (cc, ch) in mappings {
            cc_to_dmx.insert(*cc, *ch);
        }
        SignalTransform::MidiToArtNet {
            cc_to_dmx,
            note_to_dmx: HashMap::new(),
        }
    }

    fn note_transform(mappings: &[(u8, u16)]) -> SignalTransform {
        let mut note_to_dmx = HashMap::new();
        for (note, ch) in mappings {
            note_to_dmx.insert(*note, *ch);
        }
        SignalTransform::MidiToArtNet {
            cc_to_dmx: HashMap::new(),
            note_to_dmx,
        }
    }

    #[test]
    fn cc_with_mapping_returns_update_with_scaled_value() {
        // CC 7 (volume) on channel 0, value 100 (0xB0 0x07 0x64) →
        // DMX channel 3, scaled value floor(100*255/127) = 200.
        let transform = cc_transform(&[(7, 3)]);
        let update = apply(&transform, &[0xB0, 0x07, 0x64]).expect("must translate");
        assert_eq!(update.channel, 3);
        assert_eq!(update.value, (100u16 * 255 / 127) as u8);
    }

    #[test]
    fn cc_without_mapping_returns_none() {
        // cc_to_dmx maps CC 7; an event for CC 99 has no mapping → None.
        let transform = cc_transform(&[(7, 3)]);
        assert!(apply(&transform, &[0xB0, 0x63, 0x40]).is_none());
    }

    #[test]
    fn note_on_with_mapping_returns_update_with_scaled_velocity() {
        // NoteOn channel 0, note 60, velocity 80 → DMX channel 17,
        // value floor(80*255/127) = 160.
        let transform = note_transform(&[(60, 17)]);
        let update = apply(&transform, &[0x90, 0x3C, 0x50]).expect("must translate");
        assert_eq!(update.channel, 17);
        assert_eq!(update.value, (80u16 * 255 / 127) as u8);
    }

    #[test]
    fn note_on_velocity_zero_returns_none() {
        // NoteOn vel=0 is MIDI-spec NoteOff. Don't synthesize a
        // "0-intensity" DMX update.
        let transform = note_transform(&[(60, 17)]);
        assert!(apply(&transform, &[0x90, 0x3C, 0x00]).is_none());
    }

    #[test]
    fn note_off_returns_none() {
        // Explicit NoteOff (0x80) has no mapping table in MidiToArtNet.
        let transform = note_transform(&[(60, 17)]);
        assert!(apply(&transform, &[0x80, 0x3C, 0x50]).is_none());
    }

    #[test]
    fn non_midi_to_artnet_variant_returns_none() {
        // Defensive: caller routed the wrong variant. Only
        // MidiToArtNet produces output.
        let transform = SignalTransform::MidiToOsc {
            cc_to_address: Some("/x".to_string()),
            note_to_address: None,
            value_to_float: false,
        };
        assert!(apply(&transform, &[0xB0, 0x07, 0x40]).is_none());
    }

    #[test]
    fn invalid_midi_returns_none() {
        let transform = cc_transform(&[(7, 3)]);
        assert!(apply(&transform, &[0xF0, 0x7E, 0x00]).is_none(), "SysEx");
        assert!(apply(&transform, &[0xB0]).is_none(), "truncated CC");
        assert!(apply(&transform, &[]).is_none(), "empty");
    }

    #[test]
    fn pitchbend_program_change_aftertouch_return_none() {
        let transform = cc_transform(&[(7, 3)]);
        assert!(
            apply(&transform, &[0xE0, 0x00, 0x40]).is_none(),
            "PitchBend"
        );
        assert!(apply(&transform, &[0xC0, 0x05]).is_none(), "ProgramChange");
        assert!(
            apply(&transform, &[0xD0, 0x50]).is_none(),
            "ChannelAftertouch"
        );
        assert!(
            apply(&transform, &[0xA0, 0x3C, 0x50]).is_none(),
            "PolyAftertouch"
        );
    }

    #[test]
    fn scale_endpoints_are_exact() {
        // The module-level doc pins these three values; if the scale
        // formula ever drifts (e.g. someone swaps to a bit-shift
        // approximation), the endpoints will surface here.
        assert_eq!(scale_7bit_to_8bit(0), 0);
        assert_eq!(scale_7bit_to_8bit(127), 255);
        assert_eq!(scale_7bit_to_8bit(64), 128);
    }

    /// Defense-in-depth (Council R1 on PR #1349): if upstream MIDI
    /// parsing ever lets a value > 127 through, release builds must
    /// clamp rather than silently wrap via `as u8`. Debug builds
    /// panic via the `debug_assert!` (covered by a separate
    /// `#[should_panic]` test). Tested via direct call (bypassing
    /// `apply` because the public entry point is gated by
    /// `MidiMessage::parse`).
    ///
    /// **CI coverage gap**: standard `cargo test` runs in debug, so
    /// this test does NOT run in the default CI matrix. The
    /// `should_panic` test below DOES run in CI and covers the
    /// debug-build contract. To run THIS test, invoke
    /// `cargo test --release transforms::midi_to_artnet`. Council R2
    /// on PR #1349 flagged this gap; documenting rather than rerouting
    /// CI to run both modes (broader infrastructure change).
    #[cfg(not(debug_assertions))]
    #[test]
    fn scale_clamps_out_of_range_in_release_builds() {
        // Without the `.min(127)` clamp, `255 * 255 / 127 = 512`, and
        // `512 as u8` truncates to 0 (low 8 bits of 0x200 = 0x00),
        // NOT 254 as a quick read of "wraps" might suggest. The clamp
        // converts 255 → 127 → 255 (max valid DMX level).
        assert_eq!(scale_7bit_to_8bit(255), 255);
        assert_eq!(scale_7bit_to_8bit(200), 255);
        assert_eq!(scale_7bit_to_8bit(128), 255);
    }

    /// Companion: in debug builds the `debug_assert!` fires. Pin
    /// that the assertion message mentions the offending value so
    /// the diagnostic is actionable.
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "scale_7bit_to_8bit called with out-of-range value")]
    fn scale_panics_on_out_of_range_in_debug_builds() {
        let _ = scale_7bit_to_8bit(200);
    }

    #[test]
    fn scale_monotonic_no_skipped_levels() {
        // Each MIDI value step produces a DMX value at least as large
        // as the previous step — no negative slope from rounding bugs.
        let mut prev = 0u8;
        for v in 0..=127u8 {
            let dmx = scale_7bit_to_8bit(v);
            assert!(
                dmx >= prev,
                "DMX value at MIDI {} ({}) regressed below previous ({})",
                v,
                dmx,
                prev
            );
            prev = dmx;
        }
    }

    #[test]
    fn both_cc_and_note_in_same_transform_dispatch_independently() {
        // A MidiToArtNet config with BOTH tables populated. CC events
        // hit cc_to_dmx; Note events hit note_to_dmx. Cross-table
        // lookup MUST NOT happen (CC number 60 must not accidentally
        // dispatch via the note_to_dmx[60] entry).
        let transform = SignalTransform::MidiToArtNet {
            cc_to_dmx: HashMap::from([(60u8, 100u16)]),
            note_to_dmx: HashMap::from([(60u8, 200u16)]),
        };
        // CC 60 → channel 100 (from cc_to_dmx)
        let cc_update = apply(&transform, &[0xB0, 0x3C, 0x7F]).unwrap();
        assert_eq!(
            cc_update.channel, 100,
            "CC must use cc_to_dmx, not note_to_dmx"
        );
        // NoteOn note 60 → channel 200 (from note_to_dmx)
        let note_update = apply(&transform, &[0x90, 0x3C, 0x7F]).unwrap();
        assert_eq!(
            note_update.channel, 200,
            "Note must use note_to_dmx, not cc_to_dmx"
        );
    }
}
