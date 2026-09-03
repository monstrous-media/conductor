// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Content-addressed configuration revision per ADR-034 §D5.
//!
//! `ConfigRevision` is the SHA-256 of a [`Config`](super::Config)'s
//! [canonical TOML bytes](super::canonical::serialise). Display + serde
//! representations are lowercase hex (64 chars). The newtype prevents
//! accidental mixing with arbitrary hashes and gives `Eq`/`Hash` for
//! free.
//!
//! `ConfigRevision` is *informational* for the CAS protocol —
//! ADR-034 §D1 keys CAS on a monotonic `state_generation` instead, to
//! avoid the ABA defect where byte-identical mutations (e.g.
//! `MarkKnownGood`, identity-preserving `Rollback`) would produce
//! colliding revisions. `ConfigRevision` is used for audit display,
//! on-disk file verification, and detecting cross-restart tampering.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::fmt;

/// 32-byte SHA-256 of canonical TOML bytes.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConfigRevision([u8; 32]);

impl ConfigRevision {
    /// Compute the revision of a slice of canonical TOML bytes.
    ///
    /// Caller is responsible for passing canonical-form bytes — see
    /// [`super::canonical::serialise`].
    pub fn from_canonical_bytes(bytes: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let digest = hasher.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&digest);
        Self(out)
    }

    /// Construct from a 32-byte digest directly. Test helper.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Borrow the underlying digest.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for ConfigRevision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in &self.0 {
            write!(f, "{:02x}", byte)?;
        }
        Ok(())
    }
}

impl fmt::Debug for ConfigRevision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ConfigRevision({self})")
    }
}

impl Serialize for ConfigRevision {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ConfigRevision {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        // Strict validation: 64 lowercase hex chars only. This:
        //   (1) guarantees the canonical-lowercase contract holds,
        //   (2) makes byte indexing into the string safe (every ASCII
        //       char is 1 byte; `&s[i*2..i*2+2]` cannot straddle a
        //       UTF-8 boundary and panic),
        //   (3) rejects `u8::from_str_radix`-accepted edge cases like
        //       `+` prefixes that would otherwise sneak through.
        // A 64-byte string with multi-byte UTF-8 chars (e.g. `aé` +
        // 61×`a`) is 64 bytes but <64 chars; the prior implementation
        // panicked → DoS.
        if s.len() != 64 || !s.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')) {
            return Err(serde::de::Error::custom(
                "ConfigRevision must be exactly 64 lowercase hex chars",
            ));
        }
        let mut out = [0u8; 32];
        for i in 0..32 {
            out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)
                .map_err(|e| serde::de::Error::custom(format!("invalid hex byte: {e}")))?;
        }
        Ok(Self(out))
    }
}
