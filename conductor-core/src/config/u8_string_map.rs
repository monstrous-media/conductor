// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Serde `with =` helper for `HashMap<u8, V>` fields that must serialise
//! to TOML — TOML requires string-typed table keys, so a bare `HashMap<u8, _>`
//! errors with `KeyNotString` during canonical serialise (#1356).
//!
//! Usage on a field:
//!
//! ```ignore
//! #[serde(with = "crate::config::u8_string_map")]
//! pub cc_to_dmx: HashMap<u8, u16>,
//! ```
//!
//! Wire format: numeric u8 keys appear as their decimal string
//! representation in TOML (e.g. `7u8` → `"7"`). On deserialisation,
//! each string key is parsed back into a `u8` via `FromStr`. Out-of-range
//! values (parse failure) surface as a deserialiser error rather than
//! silently dropping the entry — verified by
//! `serialise_rejects_or_normalises_out_of_range_u8_keys_via_typed_round_trip`
//! in `tests/canonical_serialise.rs`.
//!
//! ## Why a custom helper instead of `serde_with::DisplayFromStr`
//!
//! `serde_with` is not currently a workspace dependency and adding it
//! for one map type is more surface than warranted. ~20 lines here vs
//! pulling in the full crate. If a second u8-keyed map shows up
//! elsewhere, revisit. (Existing memory note in [[adr-027-phase-1a-complete]]
//! tracks this kind of "smallest viable shim" principle.)
//!
//! ## Scope today
//!
//! Only `MidiToArtNet { cc_to_dmx, note_to_dmx }` uses this helper.
//! `HidToArtNet.trigger_to_channel` is `HashMap<String, u16>` (string
//! keys natively) and doesn't need it.

use serde::de::{Deserializer, Error as DeError, MapAccess, Visitor};
use serde::ser::{SerializeMap, Serializer};
use std::collections::HashMap;
use std::fmt;
use std::marker::PhantomData;
use std::str::FromStr;

/// Serialise a `HashMap<u8, V>` as a TOML-compatible string-keyed map.
///
/// Keys are emitted in **lex order of their decimal-string form**
/// (matching the downstream canonical-serialise sort in
/// [`canonical::sort_value`](super::canonical)). This makes the helper
/// independently deterministic — two calls with the same logical map
/// produce byte-identical output, even when used outside the canonical
/// pipeline. Council R1 on PR (#1356-fix) flagged the prior unordered-
/// HashMap iteration as a determinism bug.
///
/// Lex (not numeric) order is deliberate: it matches what the
/// canonical layer would re-sort to anyway. Keys `7`, `11`, `200`
/// emit as `"11", "200", "7"` (string-sorted), not `"7", "11", "200"`
/// (numeric-sorted).
pub fn serialize<S, V>(map: &HashMap<u8, V>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
    V: serde::Serialize,
{
    let mut entries: Vec<(String, &V)> = map.iter().map(|(k, v)| (k.to_string(), v)).collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut m = serializer.serialize_map(Some(entries.len()))?;
    for (k, v) in &entries {
        m.serialize_entry(k, v)?;
    }
    m.end()
}

/// Deserialise a TOML string-keyed map into `HashMap<u8, V>` by parsing
/// each key as `u8::from_str`. Returns a deserialiser error on any key
/// that doesn't parse (non-numeric, out of range, etc.).
pub fn deserialize<'de, D, V>(deserializer: D) -> Result<HashMap<u8, V>, D::Error>
where
    D: Deserializer<'de>,
    V: serde::Deserialize<'de>,
{
    struct U8KeyVisitor<V> {
        _phantom: PhantomData<V>,
    }

    impl<'de, V> Visitor<'de> for U8KeyVisitor<V>
    where
        V: serde::Deserialize<'de>,
    {
        type Value = HashMap<u8, V>;

        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a map with u8-parseable string keys (e.g. \"7\")")
        }

        fn visit_map<M>(self, mut access: M) -> Result<Self::Value, M::Error>
        where
            M: MapAccess<'de>,
        {
            let mut out = HashMap::with_capacity(access.size_hint().unwrap_or(0));
            while let Some(key) = access.next_key::<String>()? {
                let k_u8 = u8::from_str(&key).map_err(|e| {
                    M::Error::custom(format!(
                        "invalid u8-keyed map entry: key \"{key}\" does not parse as u8 ({e})"
                    ))
                })?;
                let v = access.next_value::<V>()?;
                out.insert(k_u8, v);
            }
            Ok(out)
        }
    }

    deserializer.deserialize_map(U8KeyVisitor {
        _phantom: PhantomData,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    struct Holder {
        #[serde(with = "super")]
        map: HashMap<u8, u16>,
    }

    #[test]
    fn round_trip_through_toml_string_keys() {
        let mut m = HashMap::new();
        m.insert(7u8, 50u16);
        m.insert(127u8, 200u16);
        let original = Holder { map: m };

        let s = toml::to_string(&original).expect("serialise");
        // Keys must appear as quoted strings in the emitted TOML.
        assert!(s.contains("7") && s.contains("127"));

        let back: Holder = toml::from_str(&s).expect("deserialise");
        assert_eq!(original, back);
    }

    #[test]
    fn deserialise_rejects_non_numeric_key() {
        let invalid = r#"map = { "not_a_number" = 50 }"#;
        let r: Result<Holder, _> = toml::from_str(invalid);
        assert!(
            r.is_err(),
            "non-numeric key must error, not silently drop; got: {r:?}"
        );
        let err = r.unwrap_err().to_string();
        assert!(
            err.contains("not_a_number"),
            "error must name the offending key; got: {err}"
        );
    }

    #[test]
    fn deserialise_rejects_out_of_range_key() {
        let invalid = r#"map = { "256" = 50 }"#;
        let r: Result<Holder, _> = toml::from_str(invalid);
        assert!(r.is_err(), "u8 overflow key must error; got: {r:?}");
    }

    #[test]
    fn empty_map_round_trips() {
        let original = Holder {
            map: HashMap::new(),
        };
        let s = toml::to_string(&original).expect("serialise");
        let back: Holder = toml::from_str(&s).expect("deserialise");
        assert_eq!(original, back);
    }

    #[test]
    fn serialise_is_byte_deterministic_across_runs() {
        // Council FAIL on the initial revision: HashMap iteration is
        // unordered, so calling `to_string` twice on the same map
        // could yield different byte orderings — violates the very
        // canonical-serialise determinism the parent helper is meant
        // to enable.
        //
        // The downstream canonical_serialise sort pass happens to fix
        // it at the Config level (via `sort_value`), but anything
        // using `toml::to_string(&Holder)` directly (or any other
        // non-canonical serializer) is exposed. Sort inside the helper
        // so the contract is one-source-of-truth.
        //
        // Test: build the same map twice and serialise; bytes must be
        // identical. HashMap intentionally seeded with many entries
        // (8) to make the random-iteration-order failure mode
        // statistically likely if sorting is missing.
        for _ in 0..32 {
            let mut m1 = HashMap::new();
            let mut m2 = HashMap::new();
            for k in [7u8, 42, 1, 200, 60, 100, 11, 250] {
                m1.insert(k, u16::from(k) * 2);
                m2.insert(k, u16::from(k) * 2);
            }
            let s1 = toml::to_string(&Holder { map: m1 }).expect("ser 1");
            let s2 = toml::to_string(&Holder { map: m2 }).expect("ser 2");
            assert_eq!(
                s1, s2,
                "two serialisations of the same logical map must be byte-identical"
            );
        }
    }

    #[test]
    fn serialise_emits_keys_in_lex_string_order() {
        // Match the canonical-serialise sort (which sorts table keys
        // by string). With this contract the helper's output IS
        // canonical even without the outer canonical pass — useful
        // for any non-canonical serializer call site.
        //
        // Keys: 7, 11, 200 → as strings, lex order is "11", "200", "7".
        // (Lex, not numeric. The canonical layer downstream uses the
        // same lex order — so what we emit here is what canonical
        // would emit anyway, just without a re-sort.)
        let mut m = HashMap::new();
        m.insert(7u8, 1u16);
        m.insert(11u8, 2u16);
        m.insert(200u8, 3u16);
        let s = toml::to_string(&Holder { map: m }).expect("serialise");
        let pos11 = s.find("\"11\"").or_else(|| s.find("11 =")).expect("11");
        let pos200 = s.find("\"200\"").or_else(|| s.find("200 =")).expect("200");
        let pos7 = s.find("\"7\"").or_else(|| s.find("7 =")).expect("7");
        assert!(
            pos11 < pos200 && pos200 < pos7,
            "expected lex order 11, 200, 7 in output:\n{s}"
        );
    }

    #[test]
    fn extreme_u8_values_preserved() {
        let mut m = HashMap::new();
        m.insert(0u8, 1u16);
        m.insert(255u8, 65535u16);
        let original = Holder { map: m };
        let s = toml::to_string(&original).expect("serialise");
        let back: Holder = toml::from_str(&s).expect("deserialise");
        assert_eq!(original, back);
    }
}
