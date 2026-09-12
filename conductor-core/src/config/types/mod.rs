// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Configuration types for Conductor.
//!
//! This module defines the data structures used to represent MIDI mappings,
//! triggers, and actions in the configuration file.

use crate::Condition;
use crate::identity::DeviceMatcher;
use crate::transform::MidiTransform;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

mod action;
mod led;
mod root;
mod routing;
mod settings;
mod trigger;

pub use action::*;
pub use led::*;
pub use root::*;
pub use routing::*;
pub use settings::*;
pub use trigger::*;

pub(crate) fn default_true() -> bool {
    true
}

/// `skip_serializing_if` predicate for boolean fields that default to `false`
/// and should be omitted from serialized output in the default case.
///
/// Takes `&bool` because serde's `skip_serializing_if` requires `fn(&T) -> bool`;
/// the `trivially_copy_pass_by_ref` pedantic lint is therefore expected and allowed.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(b: &bool) -> bool {
    !*b
}

/// Serde helper for `IndexMap<u8, Box<ActionConfig>>` in
/// [`ActionConfig::PcContextSwitch::mappings`].
///
/// TOML table keys are always strings, so a naive `IndexMap<u8, ...>`
/// would reject `[mappings.12]` with `"invalid type: string, expected u8"`.
/// This module serialises u8 keys as their decimal string and parses
/// them back on the way in, while preserving insertion order via
/// `IndexMap` — critical for the ordering contract that earlier PC
/// branches win after lowering (task #24).
///
/// JSON round-trips the same way, so MCP tool arguments and chat
/// persistence (tasks #27, #28) see the same shape.
mod string_keyed_pc_map {
    use super::ActionConfig;
    use indexmap::IndexMap;
    use serde::{
        Deserializer, Serializer,
        de::{self, MapAccess, Visitor},
        ser::SerializeMap,
    };
    use std::fmt;

    pub(super) fn serialize<S>(
        map: &IndexMap<u8, Box<ActionConfig>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut m = serializer.serialize_map(Some(map.len()))?;
        for (k, v) in map {
            m.serialize_entry(&k.to_string(), v)?;
        }
        m.end()
    }

    pub(super) fn deserialize<'de, D>(
        deserializer: D,
    ) -> Result<IndexMap<u8, Box<ActionConfig>>, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct PcMapVisitor;
        impl<'de> Visitor<'de> for PcMapVisitor {
            type Value = IndexMap<u8, Box<ActionConfig>>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a map whose keys are PC numbers (0..=127) as strings")
            }
            fn visit_map<M>(self, mut access: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut out = IndexMap::with_capacity(access.size_hint().unwrap_or(0));
                while let Some(key) = access.next_key::<String>()? {
                    // u8::from_str accepts 0..=255, but MIDI Program Change
                    // is unambiguously 0..=127 by spec. Reject out-of-range
                    // keys at the deserialisation boundary so config authors
                    // get a clear error with the offending key on the same
                    // line as the TOML/JSON location, rather than having it
                    // slip past parsing and surface later (or not at all)
                    // via the structural validator (task #26).
                    let parsed: u16 = key.parse().map_err(|_| {
                        de::Error::custom(format!(
                            "invalid PC key '{}': must be an integer 0-127",
                            key
                        ))
                    })?;
                    if parsed > 127 {
                        return Err(de::Error::custom(format!(
                            "PC key '{}' out of range: must be 0-127 (MIDI Program Change spec)",
                            key
                        )));
                    }
                    let pc = parsed as u8;
                    let value: Box<ActionConfig> = access.next_value()?;
                    // Reject normalized-duplicate keys. `1` and `01` are
                    // DISTINCT TOML keys (so TOML's own duplicate-key check
                    // doesn't fire) but both normalize to PC 1. A plain
                    // `insert` would silently drop one authored branch and leave
                    // the routing table out of sync with the config text, so
                    // fail loudly naming both the key and the normalized PC.
                    if out.insert(pc, value).is_some() {
                        return Err(de::Error::custom(format!(
                            "duplicate PC key '{}' normalizes to Program Change {} which is \
                             already mapped (e.g. '1' and '01' are distinct TOML keys but the \
                             same PC); use each PC number at most once",
                            key, pc
                        )));
                    }
                }
                Ok(out)
            }
        }
        deserializer.deserialize_map(PcMapVisitor)
    }
}

#[cfg(test)]
mod tests;
