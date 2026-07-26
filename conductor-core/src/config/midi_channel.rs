// Copyright 2026 Amiable
// SPDX-License-Identifier: MIT

//! 1-indexed MIDI channel serde helpers (#845 — Phase 1).
//!
//! The MIDI 1.0 spec presents channels as **1-16**; the status byte
//! encodes channel as `(channel - 1)` in the lower 4 bits. Every piece
//! of hardware, every DAW, and every MIDI tool the user touches
//! displays 1-16. Today (pre-#845) Conductor's config TOML expects
//! 0-indexed (0-15) values for `channel` fields, which forces users
//! to mentally offset by one — Channel 10 (the General MIDI percussion
//! channel) becomes `channel = 9` in TOML.
//!
//! # Migration plan
//!
//! - **Phase 1 (this module).** Add the serde wrappers and unit-test
//!   them. Nothing is applied yet, so config semantics are unchanged.
//!   Marked `#[allow(dead_code)]` — the application PR (Phase 2)
//!   removes the attribute as it wires the helpers into the trigger /
//!   action / device-identity variants.
//! - **Phase 2.** Apply `#[serde(deserialize_with = ..., serialize_with = ...)]`
//!   to every `channel` field on `Trigger`, `ActionConfig::SendMidi`,
//!   and `DeviceIdentityConfig`. Bump every test/example fixture
//!   `channel = N` value by +1. Update the LLM skill docs
//!   (`skills/conductor-midi-mapping/SKILL.md` and references) and
//!   the docs site daw-control guide. Update the MIDI Learn trigger
//!   suggestion to emit 1-indexed channels.
//! - **Phase 3.** Update the MCP `conductor_reset_control_state` tool
//!   to accept 1-16 too (it currently disagrees with
//!   `conductor_send_midi` which already takes 1-16). Update
//!   `validate_trigger`'s range error messages from "0-15" to "1-16".
//!
//! Internal Rust types (`MidiEvent`, `InputEvent`, `ProcessedEvent`,
//! `CompiledTrigger`) keep their 0-15 representation throughout —
//! that matches the raw MIDI byte encoding and avoids churn in the
//! engine hot path. The conversion lives at the serde boundary only.
//!
//! # Usage (Phase 2 onward)
//!
//! ```ignore
//! use serde::{Deserialize, Serialize};
//! use conductor_core::config::midi_channel;
//!
//! #[derive(Deserialize, Serialize)]
//! struct MyTrigger {
//!     #[serde(
//!         default,
//!         skip_serializing_if = "Option::is_none",
//!         deserialize_with = "midi_channel::deserialize_one_indexed_optional_u8",
//!         serialize_with = "midi_channel::serialize_optional_u8_as_one_indexed",
//!     )]
//!     channel: Option<u8>,
//! }
//! ```
//!
//! TOML `channel = 1` deserialises to `Some(0)`; TOML `channel = 16`
//! deserialises to `Some(15)`. `channel = 0` and `channel = 17` are
//! rejected with a clear error. `channel` omitted (or explicitly
//! null) → `None`, matching the "match any channel" semantics of
//! every trigger today.

use serde::de::{Deserialize, Deserializer, Error as _, Unexpected};
use serde::ser::{Error as _, Serializer};

const MIDI_CHANNEL_MIN_ONE_INDEXED: u8 = 1;
const MIDI_CHANNEL_MAX_ONE_INDEXED: u8 = 16;
const INTERNAL_CHANNEL_MAX: u8 = 15;

/// Deserialise a 1-indexed (1-16) `channel` field from TOML/JSON and
/// return the internal 0-indexed representation (0-15).
///
/// Out-of-range inputs (0, ≥17) return a structured serde error
/// instead of silently clamping — the operator's config is wrong and
/// they need to know.
#[allow(dead_code)] // Phase 2 (#845) wires this into Trigger/SendMidi/DeviceIdentityConfig
pub fn deserialize_one_indexed_optional_u8<'de, D>(deserializer: D) -> Result<Option<u8>, D::Error>
where
    D: Deserializer<'de>,
{
    let opt: Option<u8> = Option::deserialize(deserializer)?;
    match opt {
        None => Ok(None),
        Some(v) if (MIDI_CHANNEL_MIN_ONE_INDEXED..=MIDI_CHANNEL_MAX_ONE_INDEXED).contains(&v) => {
            Ok(Some(v - 1))
        }
        Some(v) => Err(D::Error::invalid_value(
            Unexpected::Unsigned(v as u64),
            &"a MIDI channel in the range 1-16 (1-indexed; MIDI Channel 10 = General MIDI percussion)",
        )),
    }
}

/// Serialise a 0-indexed internal `channel` (0-15) as a 1-indexed TOML
/// value (1-16). `None` round-trips as a skipped field — callers
/// pair this with `#[serde(skip_serializing_if = "Option::is_none")]`.
///
/// Returns a serializer error if the internal value is `> 15`. Saturating
/// at 16 would mask invariant violations: a buggy caller constructing
/// `Some(20)` would produce `channel = 17` in TOML which the next load
/// would reject — better to surface the bug at write time.
#[allow(dead_code)] // Phase 2 (#845) wires this into Trigger/SendMidi/DeviceIdentityConfig
pub fn serialize_optional_u8_as_one_indexed<S>(
    value: &Option<u8>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match value {
        None => serializer.serialize_none(),
        Some(v) if *v <= INTERNAL_CHANNEL_MAX => serializer.serialize_u8(v + 1),
        Some(v) => Err(S::Error::custom(format!(
            "internal MIDI channel must be 0-15 (got {v}); a value out of range here \
             indicates a bug in code that constructed this struct without going through \
             the deserializer"
        ))),
    }
}

/// Non-Option variant for fields like `LedConfig.channel` and
/// `SendMidi.channel` that are always present.
#[allow(dead_code)] // Phase 2 (#845) wires this into LedConfig.channel and SendMidi.channel
pub fn deserialize_one_indexed_u8<'de, D>(deserializer: D) -> Result<u8, D::Error>
where
    D: Deserializer<'de>,
{
    let v = u8::deserialize(deserializer)?;
    if (MIDI_CHANNEL_MIN_ONE_INDEXED..=MIDI_CHANNEL_MAX_ONE_INDEXED).contains(&v) {
        Ok(v - 1)
    } else {
        Err(D::Error::invalid_value(
            Unexpected::Unsigned(v as u64),
            &"a MIDI channel in the range 1-16 (1-indexed; MIDI Channel 10 = General MIDI percussion)",
        ))
    }
}

/// Symmetric to `deserialize_one_indexed_u8` — returns a serializer
/// error rather than saturating, for the same reason as
/// `serialize_optional_u8_as_one_indexed`.
#[allow(dead_code)] // Phase 2 (#845) wires this into LedConfig.channel and SendMidi.channel
pub fn serialize_u8_as_one_indexed<S>(value: &u8, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if *value <= INTERNAL_CHANNEL_MAX {
        serializer.serialize_u8(value + 1)
    } else {
        Err(S::Error::custom(format!(
            "internal MIDI channel must be 0-15 (got {value}); a value out of range here \
             indicates a bug in code that constructed this struct without going through \
             the deserializer"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    /// Probe type for the optional helper. Mirrors the
    /// `#[serde(default, skip_serializing_if, deserialize_with,
    /// serialize_with)]` shape that Phase 2 will use on every
    /// `Trigger` variant's `channel` field.
    #[derive(Debug, PartialEq, Deserialize, Serialize)]
    struct ProbeOpt {
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "deserialize_one_indexed_optional_u8",
            serialize_with = "serialize_optional_u8_as_one_indexed"
        )]
        channel: Option<u8>,
    }

    /// Probe type for the non-Option helper. Mirrors what Phase 2 will
    /// use on `LedConfig.channel` (always present, no skip-if-none).
    #[derive(Debug, PartialEq, Deserialize, Serialize)]
    struct ProbeReq {
        #[serde(
            deserialize_with = "deserialize_one_indexed_u8",
            serialize_with = "serialize_u8_as_one_indexed"
        )]
        channel: u8,
    }

    // ── Optional helper ────────────────────────────────────────────

    #[test]
    fn optional_omitted_field_round_trips_as_none() {
        let parsed: ProbeOpt = toml::from_str("").unwrap();
        assert_eq!(parsed.channel, None);
        let toml = toml::to_string(&parsed).unwrap();
        assert_eq!(toml, "", "None must round-trip as an omitted field");
    }

    #[test]
    fn optional_channel_1_deserialises_as_internal_0() {
        let parsed: ProbeOpt = toml::from_str("channel = 1").unwrap();
        assert_eq!(parsed.channel, Some(0));
    }

    #[test]
    fn optional_channel_10_deserialises_as_internal_9_gm_percussion() {
        // Channel 10 = General MIDI percussion. The whole point of
        // this migration: a user writing `channel = 10` to filter
        // percussion shouldn't have to mentally subtract one.
        let parsed: ProbeOpt = toml::from_str("channel = 10").unwrap();
        assert_eq!(parsed.channel, Some(9));
    }

    #[test]
    fn optional_channel_16_deserialises_as_internal_15() {
        let parsed: ProbeOpt = toml::from_str("channel = 16").unwrap();
        assert_eq!(parsed.channel, Some(15));
    }

    #[test]
    fn optional_channel_0_rejected() {
        // 0 was the old internal-channel-1 value. Phase 2 must reject
        // it loudly so users with old configs see an error, not
        // silent semantic drift.
        let err = toml::from_str::<ProbeOpt>("channel = 0").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("1-16"),
            "rejection message must name the valid range; got: {msg}"
        );
    }

    #[test]
    fn optional_channel_17_rejected() {
        let err = toml::from_str::<ProbeOpt>("channel = 17").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("1-16"),
            "rejection message must name the valid range; got: {msg}"
        );
    }

    #[test]
    fn optional_serialize_internal_0_emits_toml_1() {
        let probe = ProbeOpt { channel: Some(0) };
        let toml = toml::to_string(&probe).unwrap();
        assert_eq!(toml.trim(), "channel = 1");
    }

    #[test]
    fn optional_serialize_internal_9_emits_toml_10() {
        let probe = ProbeOpt { channel: Some(9) };
        let toml = toml::to_string(&probe).unwrap();
        assert_eq!(toml.trim(), "channel = 10");
    }

    #[test]
    fn optional_round_trip_all_valid_channels() {
        // 1-16 must all round-trip through TOML stably.
        for internal in 0u8..=15 {
            let p1 = ProbeOpt {
                channel: Some(internal),
            };
            let toml = toml::to_string(&p1).unwrap();
            let p2: ProbeOpt = toml::from_str(&toml).unwrap();
            assert_eq!(p1, p2, "round-trip failed for internal={internal}");
        }
    }

    // ── Non-Option helper ──────────────────────────────────────────

    #[test]
    fn required_channel_1_deserialises_as_internal_0() {
        let parsed: ProbeReq = toml::from_str("channel = 1").unwrap();
        assert_eq!(parsed.channel, 0);
    }

    #[test]
    fn required_channel_16_deserialises_as_internal_15() {
        let parsed: ProbeReq = toml::from_str("channel = 16").unwrap();
        assert_eq!(parsed.channel, 15);
    }

    #[test]
    fn required_channel_0_rejected() {
        let err = toml::from_str::<ProbeReq>("channel = 0").unwrap_err();
        assert!(err.to_string().contains("1-16"));
    }

    #[test]
    fn required_channel_17_rejected() {
        let err = toml::from_str::<ProbeReq>("channel = 17").unwrap_err();
        assert!(err.to_string().contains("1-16"));
    }

    #[test]
    fn required_serialize_internal_0_emits_toml_1() {
        let probe = ProbeReq { channel: 0 };
        let toml = toml::to_string(&probe).unwrap();
        assert_eq!(toml.trim(), "channel = 1");
    }

    #[test]
    fn required_round_trip_all_valid_channels() {
        for internal in 0u8..=15 {
            let p1 = ProbeReq { channel: internal };
            let toml = toml::to_string(&p1).unwrap();
            let p2: ProbeReq = toml::from_str(&toml).unwrap();
            assert_eq!(p1, p2, "round-trip failed for internal={internal}");
        }
    }

    // ── Serializer error path (Copilot review round 1) ─────────────
    //
    // The deserializer rejects out-of-range INPUT, but a buggy caller
    // could construct `Some(20)` directly (skipping the deserializer)
    // and ask serde to serialise it. saturating_add(1) would have
    // emitted `channel = 17` to TOML — a value the deserializer would
    // then reject on the next load. Better to surface the bug at
    // write time. These tests pin that.

    #[test]
    fn optional_serialize_internal_out_of_range_errors() {
        let probe = ProbeOpt { channel: Some(20) };
        let result = toml::to_string(&probe);
        assert!(
            result.is_err(),
            "serializer must reject out-of-range internal value, not saturate"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("0-15"),
            "error message must name the valid internal range; got: {msg}"
        );
    }

    #[test]
    fn required_serialize_internal_out_of_range_errors() {
        let probe = ProbeReq { channel: 20 };
        let result = toml::to_string(&probe);
        assert!(
            result.is_err(),
            "serializer must reject out-of-range internal value, not saturate"
        );
    }

    #[test]
    fn optional_serialize_internal_15_succeeds() {
        // Boundary check: 15 is the maximum valid internal value.
        let probe = ProbeOpt { channel: Some(15) };
        assert!(toml::to_string(&probe).is_ok());
    }

    #[test]
    fn required_serialize_internal_15_succeeds() {
        let probe = ProbeReq { channel: 15 };
        assert!(toml::to_string(&probe).is_ok());
    }
}
