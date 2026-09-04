// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Canonical TOML serialisation for [`Config`] per ADR-034 §D5.
//!
//! Produces a deterministic byte output for a given `Config` value,
//! used as the input to [`ConfigRevision`](super::ConfigRevision)
//! hashing.
//!
//! Invariants this module enforces directly:
//! - Lexicographic key ordering at every table nesting level (via
//!   [`sort_value`]).
//! - `Vec<T>` insertion order preserved for `[[arrays.of.tables]]` —
//!   array order is positionally significant in TOML.
//! - No comments (serde never preserves them anyway).
//!
//! Invariants delegated to the `toml` crate emitter:
//! - UTF-8 encoding, no BOM.
//! - LF line endings.
//! - Lowercase booleans (TOML spec requirement).
//! - Spacing, string escaping, inline-vs-expanded table layout,
//!   and number formatting.
//!
//! **Cross-version stability is not absolute.** A `toml` crate upgrade
//! that changes any delegated invariant produces a new canonical form
//! and a new [`ConfigRevision`]. The
//! [`tests/canonical_serialise.rs`](../../../conductor-core/tests/canonical_serialise.rs)
//! golden-hash test detects such drift; the workspace's pinned `toml =
//! "0.9"` is the contract. A true cross-crate-version guarantee would
//! require a custom emitter — out of scope for D4.A.1.

use crate::config::types::Config;
use thiserror::Error;

/// Errors emitted by [`serialise`].
#[derive(Debug, Error)]
pub enum CanonicalError {
    /// Failed to convert `Config` to a `toml::Value` tree.
    #[error("convert to toml::Value failed")]
    ToValue(#[from] toml::ser::Error),
    /// Failed to emit the sorted `toml::Value` tree to a TOML string.
    /// Source preserved for diagnostics.
    #[error("emit canonical TOML string failed")]
    Emit(#[source] toml::ser::Error),
}

/// Serialise a [`Config`] to canonical TOML bytes.
///
/// See module docs for the canonical-form invariants this function
/// guarantees.
pub fn serialise(config: &Config) -> Result<Vec<u8>, CanonicalError> {
    let value = toml::Value::try_from(config)?;
    let sorted = sort_value(value);
    let s = toml::to_string(&sorted).map_err(CanonicalError::Emit)?;
    Ok(s.into_bytes())
}

/// Recursively rebuild a `toml::Value` with lex-sorted table keys.
///
/// Arrays preserve positional order (TOML spec). Scalars are identity.
fn sort_value(value: toml::Value) -> toml::Value {
    match value {
        toml::Value::Table(table) => {
            let mut entries: Vec<(String, toml::Value)> =
                table.into_iter().map(|(k, v)| (k, sort_value(v))).collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            let mut sorted_table = toml::map::Map::new();
            for (k, v) in entries {
                sorted_table.insert(k, v);
            }
            toml::Value::Table(sorted_table)
        }
        toml::Value::Array(arr) => toml::Value::Array(arr.into_iter().map(sort_value).collect()),
        scalar => scalar,
    }
}
