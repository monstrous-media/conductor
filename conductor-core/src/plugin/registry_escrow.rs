// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-027 §D10d-source — M-of-N catastrophic-recovery **escrow** for the
//! registry ROOT key (default **2-of-3**).
//!
//! The registry trust anchor is a compile-time **pinned root key**
//! ([`crate::plugin::registry_trust`]). Ordinary key turnover is handled by the
//! §D9 rotation chain (a current key rotation-signed by its predecessor, rooted
//! at the pin). But rotation assumes the *current* signer is still available to
//! endorse its successor. If the root (and the live rotation head) are **lost or
//! compromised** — no key left that the chain trusts — rotation cannot recover,
//! and re-pinning would require shipping a new build to every client.
//!
//! Escrow is the break-glass path for exactly that case: a **quorum** of offline
//! escrow holders co-sign a **root-key-override** document that re-anchors the
//! registry root to a fresh key. No single holder can do it alone (the threshold
//! is `>= 2`), so one compromised escrow key cannot hijack the registry.
//!
//! # Why 2-of-3
//!
//! `N = 3` holders, `M = 2` threshold. This survives the loss of any one escrow
//! key (two remaining holders still meet the threshold) *and* the compromise of
//! any one escrow key (an attacker with one key is still one short). A single
//! key (1-of-1) would be a new single point of failure; requiring all three
//! (3-of-3) cannot tolerate a lost holder. 2-of-3 is the smallest quorum that is
//! both fault-tolerant and compromise-tolerant. See
//! `docs/security/registry-key-management.md` for custody and recovery
//! procedure.
//!
//! # Wire format — the root-override document
//!
//! ```json
//! { "key_id": "conductor-registry-v1",
//!   "override_seq": 1,
//!   "new_root_key": "<hex ed25519 — the NEW root>",
//!   "signatures": [
//!     { "signer_key": "<hex ed25519 escrow pubkey>", "signature": "<hex>" },
//!     { "signer_key": "<hex ed25519 escrow pubkey>", "signature": "<hex>" }
//!   ] }
//! ```
//!
//! Each signature is **pure Ed25519** (`verify_strict`) over
//! `DOMAIN || key_id || NUL || override_seq(be) || new_root_key` (see
//! [`escrow_override_message`]) — the same domain-separated, no-app-pre-hash
//! discipline as the document and rotation signatures. Binding `new_root_key`
//! into the signed bytes means the escrow holders authorize **that specific**
//! new root; an attacker cannot keep the signatures and swap in a different root.
//!
//! # Anti-rollback
//!
//! `override_seq` is required and strictly increasing (1-indexed). A client that
//! has accepted override `k` rejects any override `<= k`, so a *prior* override
//! (e.g. one that re-anchored to a now-compromised key) cannot be replayed to
//! revert a later recovery. This mirrors the document `sequence_number` guard.
//!
//! # Threshold counting
//!
//! The threshold counts **distinct** escrow keys with a valid signature.
//! Signatures from keys outside the configured escrow set are ignored (they do
//! not count, and are not an error); a duplicate signature from the same holder
//! counts once. The signature list is size-capped before any verification work
//! (DoS bound).

use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Domain-separation tag for escrow signatures. Distinct from the document and
/// rotation domains so a signature from one context can never be replayed in
/// another. The trailing NUL delimits the tag from the following bytes.
const ESCROW_OVERRIDE_DOMAIN: &[u8] = b"conductor-registry-root-override-v1\0";

/// The `N` escrow public keys (hex Ed25519, 64 chars each). **Phase-1
/// placeholders** (empty) until the signing-infra follow-up generates and
/// distributes the real escrow key material to its custodians. While empty,
/// [`configured_escrow_keys`] returns no keys and [`verify_root_override`]
/// against them fails closed ([`RootOverrideError::NoEscrowConfigured`]) — there
/// is no recovery path until real escrow keys are baked, which is correct: an
/// empty placeholder must never satisfy a threshold.
pub const REGISTRY_ESCROW_KEYS_HEX: [&str; 3] = ["", "", ""];

/// `M` — distinct valid escrow signatures required to accept an override.
pub const REGISTRY_ESCROW_THRESHOLD: usize = 2;

/// `N` — total escrow holders.
pub const REGISTRY_ESCROW_N: usize = REGISTRY_ESCROW_KEYS_HEX.len();

/// Hard cap on signatures processed per override document. Bounds the
/// verification work an attacker can force by padding the list with bogus
/// signatures before the threshold is met (DoS). A real override carries at most
/// `N` signatures; the cap leaves generous headroom.
pub const MAX_ESCROW_SIGNATURES: usize = 32;

/// Maximum accepted root-override document size, in bytes, enforced by
/// [`parse_root_override`] BEFORE any JSON parsing. A real override (a key id, a
/// 64-hex key, and a handful of signatures) is well under a kilobyte; the cap
/// bounds the parse work an attacker can force with oversized/unbounded string
/// fields (DoS-on-parse, mirroring the registry document's
/// `MAX_REGISTRY_DOC_BYTES`).
pub const MAX_ROOT_OVERRIDE_BYTES: usize = 64 * 1024;

/// Const byte-equality (no `==` on `&[u8]` in const context) for the build-time
/// distinctness check below.
const fn bytes_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

// Build-time safety gate: a 1-of-N escrow re-introduces a single point of
// failure (one compromised holder could re-anchor the root), and a threshold
// above N is unsatisfiable. Each baked key must be empty (Phase-1) or exactly
// 64 chars long — a truncated paste fails the build rather than silently
// dropping a holder from the quorum. NOTE: this check validates **length only**;
// hex-ness and curve-point validity are checked at runtime by
// [`configured_escrow_keys`] (which drops malformed keys and logs at `error!`
// so a misconfigured build is never silent). Non-empty keys must also be
// DISTINCT: a duplicated key collapses the effective holder count, weakening the
// quorum, so a copy-paste slip fails the build. This distinctness compares the
// key strings VERBATIM, and the runtime decode ([`decode_key_bytes`]) is
// likewise non-trimming, so the two agree exactly — a whitespace-padded
// near-duplicate cannot pass this raw-string check yet collapse to the same key
// at runtime.
const _: () = {
    assert!(
        REGISTRY_ESCROW_THRESHOLD >= 2,
        "REGISTRY_ESCROW_THRESHOLD must be >= 2 (a single holder must not re-anchor the root)"
    );
    assert!(
        REGISTRY_ESCROW_THRESHOLD <= REGISTRY_ESCROW_N,
        "REGISTRY_ESCROW_THRESHOLD cannot exceed the number of escrow holders"
    );
    let mut i = 0;
    while i < REGISTRY_ESCROW_KEYS_HEX.len() {
        let k = REGISTRY_ESCROW_KEYS_HEX[i];
        assert!(
            k.is_empty() || k.len() == 64,
            "each REGISTRY_ESCROW_KEYS_HEX entry must be empty (Phase-1) or exactly 64 chars (hex validity checked at runtime)"
        );
        // Distinctness: every non-empty key differs from every later non-empty key.
        let mut j = i + 1;
        while j < REGISTRY_ESCROW_KEYS_HEX.len() {
            let other = REGISTRY_ESCROW_KEYS_HEX[j];
            assert!(
                k.is_empty() || other.is_empty() || !bytes_eq(k.as_bytes(), other.as_bytes()),
                "REGISTRY_ESCROW_KEYS_HEX entries must be distinct (a duplicate weakens the quorum)"
            );
            j += 1;
        }
        i += 1;
    }
};

/// One escrow holder's signature over the root-override message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscrowSignature {
    /// Hex Ed25519 public key of the escrow holder. Must be one of the
    /// configured escrow keys to count toward the threshold; a signature whose
    /// `signer_key` is not in the escrow set is ignored (not an error).
    pub signer_key: String,
    /// Hex Ed25519 signature over [`escrow_override_message`].
    pub signature: String,
}

/// A root-key-override document: a quorum of escrow holders re-anchoring the
/// registry root to [`RootOverride::new_root_key`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RootOverride {
    /// The registry `key_id` this override re-anchors (bound into the signed
    /// bytes so an override for one registry id cannot be replayed against
    /// another).
    pub key_id: String,
    /// Strictly-increasing, 1-indexed override sequence (anti-rollback).
    /// `#[serde(default)]` so an OMITTED field deserialises to `0` and is
    /// rejected by the `>= 1` guard (with [`RootOverrideError::MissingOverrideSeq`])
    /// rather than producing a bare serde "missing field" error.
    #[serde(default)]
    pub override_seq: u64,
    /// Hex Ed25519 public key of the NEW root being anchored.
    pub new_root_key: String,
    /// Escrow signatures; `>= threshold` DISTINCT valid ones are required.
    /// `#[serde(default)]` so an omitted list is an empty vec (→
    /// [`RootOverrideError::InsufficientSignatures`]), not a parse error.
    #[serde(default)]
    pub signatures: Vec<EscrowSignature>,
}

/// The record of an accepted override, persisted in registry trust state. It
/// fixes the effective root (overriding the compile-time pin) and carries the
/// `override_seq` high-water mark for anti-rollback.
///
/// The blessed way to obtain one is a successful [`verify_root_override`] (via
/// [`VerifiedRootOverride::record`]); it is otherwise only reconstituted by
/// deserialising previously-persisted trust state. There is deliberately **no
/// helper that mints a record from a bare [`RootOverride`]** (an earlier
/// `record_of(doc)` was removed) — that footgun let a caller persist an override
/// it never verified. Hand-constructing one via its public fields is equivalent
/// to editing the on-disk trust state directly, which rests on the same
/// `~/.conductor/` file-permission integrity boundary as the rest of
/// [`crate::plugin::registry_trust::RegistryTrustState`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedRootOverride {
    /// Highest `override_seq` accepted to date.
    pub override_seq: u64,
    /// Hex Ed25519 public key of the now-effective root.
    pub new_root_key: String,
}

/// The result of a **successful** [`verify_root_override`]: the new root to
/// trust plus the [`AcceptedRootOverride`] to persist. Bundling them means the
/// only way to get a `record` is to have passed verification — a record cannot
/// be minted from an unverified document.
#[derive(Debug, Clone)]
pub struct VerifiedRootOverride {
    /// The NEW root [`VerifyingKey`] the registry should trust from now on.
    pub new_root: VerifyingKey,
    /// The record to persist in registry trust state (effective root +
    /// `override_seq` high-water mark).
    pub record: AcceptedRootOverride,
}

/// Failure reasons for root-override validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootOverrideError {
    /// No escrow keys are configured (Phase-1 placeholder, or a misconfigured
    /// build). There is no recovery path to satisfy — fail closed.
    NoEscrowConfigured,
    /// The override's `key_id` did not match the expected registry id. Defends
    /// against an override authored for a different registry being applied here
    /// (cross-registry replay), independent of the signature's `key_id` binding.
    KeyIdMismatch { expected: String, found: String },
    /// `new_root_key` was not valid hex / not a 32-byte Ed25519 point.
    BadNewRootKey(String),
    /// Fewer than `threshold` DISTINCT escrow keys produced a valid signature.
    InsufficientSignatures { got: usize, needed: usize },
    /// `override_seq` was not strictly greater than the last accepted override.
    RollbackOverrideSeq { last: u64, got: u64 },
    /// The override omitted the required `override_seq` (or used the reserved
    /// value `0`). It is mandatory and 1-indexed.
    MissingOverrideSeq,
    /// The signature list exceeded [`MAX_ESCROW_SIGNATURES`] — refused before any
    /// verification work (DoS bound).
    TooManySignatures { got: usize, max: usize },
    /// The raw override document exceeded [`MAX_ROOT_OVERRIDE_BYTES`] — refused by
    /// [`parse_root_override`] before any JSON parsing (DoS-on-parse bound).
    DocumentTooLarge { size: usize, max: usize },
    /// The raw override document did not parse as a [`RootOverride`] envelope.
    Malformed(String),
}

impl std::fmt::Display for RootOverrideError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoEscrowConfigured => write!(
                f,
                "no registry escrow keys are configured; root override cannot be verified"
            ),
            Self::KeyIdMismatch { expected, found } => write!(
                f,
                "root override key_id mismatch: expected `{expected}`, document `{found}`"
            ),
            Self::BadNewRootKey(e) => write!(f, "root override new_root_key invalid: {e}"),
            Self::InsufficientSignatures { got, needed } => write!(
                f,
                "root override has {got} valid distinct escrow signature(s); {needed} required"
            ),
            Self::RollbackOverrideSeq { last, got } => write!(
                f,
                "root override rollback: override_seq {got} <= last accepted {last}"
            ),
            Self::MissingOverrideSeq => write!(
                f,
                "root override is missing the required override_seq (must be >= 1)"
            ),
            Self::TooManySignatures { got, max } => write!(
                f,
                "root override carries {got} signatures, exceeding the {max} limit"
            ),
            Self::DocumentTooLarge { size, max } => write!(
                f,
                "root override document too large: {size} bytes exceeds the {max}-byte limit"
            ),
            Self::Malformed(e) => write!(f, "root override document did not parse: {e}"),
        }
    }
}

impl std::error::Error for RootOverrideError {}

/// The exact bytes each escrow holder signs:
/// `DOMAIN || key_id || NUL || override_seq(big-endian u64) || new_root_key`.
///
/// Signed/verified with **pure Ed25519** (its own internal SHA-512), so there is
/// no application-level pre-hash for signer and verifier to disagree on. Binding
/// `key_id`, `override_seq`, and the 32-byte `new_root_key` into the message
/// means the quorum authorizes one specific (registry, sequence, new-root)
/// triple — none of them can be swapped while keeping the signatures valid.
pub fn escrow_override_message(
    key_id: &str,
    override_seq: u64,
    new_root_key: &[u8; 32],
) -> Vec<u8> {
    let mut m = Vec::with_capacity(
        ESCROW_OVERRIDE_DOMAIN.len() + key_id.len() + 1 + 8 + new_root_key.len(),
    );
    m.extend_from_slice(ESCROW_OVERRIDE_DOMAIN);
    m.extend_from_slice(key_id.as_bytes());
    m.push(0);
    m.extend_from_slice(&override_seq.to_be_bytes());
    m.extend_from_slice(new_root_key);
    m
}

/// Verify a root-override document against the **configured** escrow keys
/// ([`configured_escrow_keys`]).
///
/// This is the production entry point: the trust anchor (the escrow key set) is
/// the build-baked [`REGISTRY_ESCROW_KEYS_HEX`], **not** a caller parameter, so a
/// caller cannot substitute attacker-chosen escrow keys. (The keys-injectable
/// core is private and reachable only by this module's own tests.) In Phase-1
/// the configured set is empty, so this returns
/// [`RootOverrideError::NoEscrowConfigured`] — fail closed — until real escrow
/// keys are baked.
///
/// On success returns a [`VerifiedRootOverride`] — the NEW root plus the
/// [`AcceptedRootOverride`] record to persist. `expected_key_id` is the registry
/// id this client trusts (e.g.
/// [`crate::plugin::registry_trust::REGISTRY_PINNED_KEY_ID`]); `last_override_seq`
/// is the high-water mark from prior accepted overrides (`None` if none yet).
pub fn verify_root_override(
    doc: &RootOverride,
    expected_key_id: &str,
    last_override_seq: Option<u64>,
) -> Result<VerifiedRootOverride, RootOverrideError> {
    verify_root_override_with_keys(
        doc,
        &configured_escrow_keys(),
        expected_key_id,
        last_override_seq,
    )
}

/// Keys-injectable verification core. **Private** — production callers must use
/// [`verify_root_override`] (which binds [`configured_escrow_keys`]); only this
/// module's tests reach this directly to exercise the logic with deterministic
/// keys (the configured set is empty in Phase-1). Enforces, in order: escrow
/// non-empty → `key_id` matches `expected_key_id` → size cap → `override_seq`
/// present (>= 1) and strictly greater than `last_override_seq` → `new_root_key`
/// is a valid point → at least [`REGISTRY_ESCROW_THRESHOLD`] DISTINCT escrow keys
/// each produced a valid signature over [`escrow_override_message`].
///
/// The threshold is **not** a parameter — it is fixed to the build-gated
/// [`REGISTRY_ESCROW_THRESHOLD`] constant (>= 2), so it cannot be weakened
/// (e.g. to `0`, which would accept an override with no valid signatures).
fn verify_root_override_with_keys(
    doc: &RootOverride,
    escrow_keys: &[VerifyingKey],
    expected_key_id: &str,
    last_override_seq: Option<u64>,
) -> Result<VerifiedRootOverride, RootOverrideError> {
    if escrow_keys.is_empty() {
        return Err(RootOverrideError::NoEscrowConfigured);
    }
    if doc.key_id != expected_key_id {
        return Err(RootOverrideError::KeyIdMismatch {
            expected: expected_key_id.to_string(),
            found: doc.key_id.clone(),
        });
    }
    if doc.signatures.len() > MAX_ESCROW_SIGNATURES {
        return Err(RootOverrideError::TooManySignatures {
            got: doc.signatures.len(),
            max: MAX_ESCROW_SIGNATURES,
        });
    }
    // override_seq is REQUIRED and 1-indexed. With `#[serde(default)]` on the
    // field, a missing JSON field deserialises to 0 (and an explicit 0 is also
    // here) — both rejected — then strictly monotonic vs the high-water mark.
    if doc.override_seq < 1 {
        return Err(RootOverrideError::MissingOverrideSeq);
    }
    if let Some(last) = last_override_seq
        && doc.override_seq <= last
    {
        return Err(RootOverrideError::RollbackOverrideSeq {
            last,
            got: doc.override_seq,
        });
    }

    let new_root_bytes =
        decode_key_bytes(&doc.new_root_key).map_err(RootOverrideError::BadNewRootKey)?;
    let new_root = VerifyingKey::from_bytes(&new_root_bytes)
        .map_err(|_| RootOverrideError::BadNewRootKey("not a valid Ed25519 point".to_string()))?;
    let message = escrow_override_message(&doc.key_id, doc.override_seq, &new_root_bytes);

    let escrow_set: HashSet<[u8; 32]> = escrow_keys.iter().map(VerifyingKey::to_bytes).collect();
    // DISTINCT escrow keys with a valid signature. Dedupe by key so a holder's
    // signature replayed twice counts once; ignore signers outside the set.
    let mut counted: HashSet<[u8; 32]> = HashSet::new();
    for s in &doc.signatures {
        let Ok(signer_bytes) = decode_key_bytes(&s.signer_key) else {
            continue;
        };
        if !escrow_set.contains(&signer_bytes) || counted.contains(&signer_bytes) {
            continue;
        }
        let Ok(signer) = VerifyingKey::from_bytes(&signer_bytes) else {
            continue;
        };
        let Ok(sig_bytes) = decode_sig_bytes(&s.signature) else {
            continue;
        };
        let signature = Signature::from_bytes(&sig_bytes);
        if signer.verify_strict(&message, &signature).is_ok() {
            counted.insert(signer_bytes);
        }
    }

    if counted.len() >= REGISTRY_ESCROW_THRESHOLD {
        Ok(VerifiedRootOverride {
            new_root,
            record: AcceptedRootOverride {
                override_seq: doc.override_seq,
                // Persist the CANONICAL hex of the verified key bytes, not the
                // document's raw `new_root_key` string — so the stored form never
                // echoes a non-canonical (e.g. upper-case) rendering of the
                // untrusted input; it always round-trips to `new_root`.
                new_root_key: hex::encode(new_root.to_bytes()),
            },
        })
    } else {
        Err(RootOverrideError::InsufficientSignatures {
            got: counted.len(),
            needed: REGISTRY_ESCROW_THRESHOLD,
        })
    }
}

/// Parse a raw root-override document, size-capped BEFORE any JSON work.
///
/// The future override transport MUST use this (not a bare `serde_json::from_str`)
/// so an oversized/unbounded-string document is refused before parsing
/// ([`RootOverrideError::DocumentTooLarge`]; DoS-on-parse bound), mirroring the
/// registry document's pre-parse size gate. The returned [`RootOverride`] is
/// still untrusted until [`verify_root_override`] succeeds.
pub fn parse_root_override(raw: &str) -> Result<RootOverride, RootOverrideError> {
    if raw.len() > MAX_ROOT_OVERRIDE_BYTES {
        return Err(RootOverrideError::DocumentTooLarge {
            size: raw.len(),
            max: MAX_ROOT_OVERRIDE_BYTES,
        });
    }
    serde_json::from_str(raw).map_err(|e| RootOverrideError::Malformed(e.to_string()))
}

/// The configured escrow verifying keys (the build-baked
/// [`REGISTRY_ESCROW_KEYS_HEX`]). Empty Phase-1 placeholders are skipped. A
/// NON-empty entry that fails to parse (the build-time gate enforces length but
/// cannot validate hex-ness or that the bytes are a curve point) is a deploy
/// misconfiguration: it is dropped — so the quorum simply cannot be met (fails
/// closed) — but logged at `error!` so a silently-shrunk quorum is never
/// invisible.
pub fn configured_escrow_keys() -> Vec<VerifyingKey> {
    REGISTRY_ESCROW_KEYS_HEX
        .iter()
        .enumerate()
        .filter(|(_, h)| !h.is_empty())
        .filter_map(|(idx, h)| {
            match decode_key_bytes(h).and_then(|b| {
                VerifyingKey::from_bytes(&b).map_err(|_| "not a valid Ed25519 point".to_string())
            }) {
                Ok(key) => Some(key),
                Err(e) => {
                    tracing::error!(
                        escrow_key_index = idx,
                        error = %e,
                        "configured registry escrow key is malformed and was dropped; \
                         the recovery quorum is reduced (fix the baked REGISTRY_ESCROW_KEYS_HEX)"
                    );
                    None
                }
            }
        })
        .collect()
}

/// The resolved registry trust anchor (see [`effective_root`).
///
/// Three outcomes are kept **distinct** so the caller never conflates "no anchor
/// to verify against" (migration — unverified acceptance is permissible) with
/// "an override is recorded but unusable" (a corrupt/tampered state, which must
/// fail closed and NOT degrade to unverified). Collapsing the latter into a bare
/// `None` was a downgrade vector: a corrupted stored override would drop the
/// client to the no-pin/unverified path and accept unsigned documents.
#[derive(Debug, Clone)]
pub enum EffectiveRoot {
    /// Verify documents against this key (the accepted override's new root, or
    /// the compile-time pin when no override is recorded).
    Anchor(VerifyingKey),
    /// No pin and no override — Phase-1 migration. Unverified acceptance is
    /// permissible (there is no trust anchor to strip).
    Migration,
    /// An override IS recorded but its stored key is unusable (corrupt state).
    /// The caller MUST reject — neither accept unverified nor silently fall back
    /// to the pin the override was meant to replace.
    OverrideUnusable,
}

/// Resolve the effective registry root: the accepted override's new root if one
/// has been recorded, else the compile-time pinned root. A recorded-but-malformed
/// override resolves to [`EffectiveRoot::OverrideUnusable`] (fail closed), never
/// to migration or the overridden pin.
pub fn effective_root(
    pinned: Option<VerifyingKey>,
    accepted: Option<&AcceptedRootOverride>,
) -> EffectiveRoot {
    match accepted {
        Some(o) => match decode_key_bytes(&o.new_root_key)
            .ok()
            .and_then(|b| VerifyingKey::from_bytes(&b).ok())
        {
            Some(key) => EffectiveRoot::Anchor(key),
            None => EffectiveRoot::OverrideUnusable,
        },
        None => match pinned {
            Some(key) => EffectiveRoot::Anchor(key),
            None => EffectiveRoot::Migration,
        },
    }
}

/// Decode a 32-byte key from hex. Deliberately does **not** `.trim()`: the
/// build-time distinctness gate compares the baked key strings VERBATIM, so the
/// runtime decode must use the identical (untrimmed) bytes — otherwise a
/// whitespace-padded duplicate could pass the raw-string distinctness check yet
/// trim to the same key at runtime, silently collapsing the quorum. Not trimming
/// also makes wire-supplied keys (signer_key / new_root_key) strict: surrounding
/// whitespace is a decode failure, not silently accepted.
fn decode_key_bytes(hex_str: &str) -> Result<[u8; 32], String> {
    let bytes = hex::decode(hex_str).map_err(|e| e.to_string())?;
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| "expected 32-byte key".to_string())
}

/// Decode a 64-byte signature from hex. Not trimmed, for the same strictness
/// rationale as [`decode_key_bytes`].
fn decode_sig_bytes(hex_str: &str) -> Result<[u8; 64], String> {
    let bytes = hex::decode(hex_str).map_err(|e| e.to_string())?;
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| "expected 64-byte signature".to_string())
}

#[cfg(test)]
#[path = "registry_escrow_tests.rs"]
mod registry_escrow_tests;
