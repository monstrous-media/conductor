// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! ADR-027 §D10d-source — plugin-registry **document** trust (Phase 1: client
//! validation + rollback protection).
//!
//! D10d-last-mile / D10d-boundary (URL hygiene + typed-field validation,
//! #1077 / #1083) already constrain *where* the registry is fetched and *what
//! shape* each entry has. This module adds integrity to the registry
//! **document itself**: a `registry.json` served from a CDN is only trusted if
//! it carries a valid signature from the pinned Conductor registry key, and is
//! not a rollback/replay of an older document.
//!
//! # Wire format — signed envelope (sign-raw-bytes)
//!
//! A signed registry is a JSON envelope:
//!
//! ```json
//! { "payload": "<registry json as an opaque string>",
//!   "signature": "<hex ed25519>",
//!   "key_id": "conductor-registry-v1" }
//! ```
//!
//! The signature is **pure Ed25519** over `DOMAIN || key_id || NUL || payload`
//! (see [`registry_signed_message`]). The signer signs the **verbatim**
//! `payload` string and the client verifies the **verbatim** received string —
//! it is never re-serialised — so there is no JSON-canonicalisation
//! malleability to get wrong. This deliberately mirrors the D9 sibling
//! ([`crate::plugin::key_rotation`]), which rejected JCS for the same reason
//! ("binary concat has no canonicalisation malleability"). `key_id` is bound
//! INTO the signed bytes so it cannot be swapped. The client verifies the
//! signature **before** parsing the inner payload (validate-offline-then-parse
//! — no fetch-and-execute TOCTOU; Council R1).
//!
//! # Rollback / replay protection
//!
//! Two monotonic guards, both carried *inside* the signed `payload` (so an
//! attacker cannot strip them without breaking the signature):
//!
//! * `sequence_number` — strictly increasing; the client tracks the last-seen
//!   value and rejects anything `<=` it. This is the Council R1 P0 control: a
//!   `published_at` alone rejects *older* documents but not a replay of a valid
//!   past document as "current"; the sequence number closes that.
//! * `published_at` (RFC 3339) — must not move backwards, compared as parsed
//!   instants (never as raw strings — lexical compare of RFC 3339 is
//!   bypassable).
//!
//! # Scope
//!
//! Client verification + rollback protection + key rotation (reusing
//! [`crate::plugin::key_rotation`]) + the M-of-N (2-of-3) catastrophic-recovery
//! escrow (via [`crate::plugin::registry_escrow`], which resolves the effective
//! root anchor). Signing CI / baking the real keys remains a follow-up under
//! #1897. Production ships an empty pinned key (the live registry is not yet
//! signed): [`decide_fetch`] returns [`FetchDecision::UnverifiedNoPin`] and the
//! caller warns-and-allows (migration). An *invalid* signature against a pinned
//! key is always rejected.

use crate::plugin::key_rotation::{PluginKeyManifestJson, fingerprint_of, validate_chain_pinned};
use crate::plugin::registry_escrow::{AcceptedRootOverride, EffectiveRoot, effective_root};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Domain-separation tag prefixing the bytes the registry key signs. The
/// trailing NUL makes the tag unambiguously delimited from the payload.
const REGISTRY_SIG_DOMAIN: &[u8] = b"conductor-registry-document-v1\0";

/// Compile-time pinned registry public key (hex Ed25519, 32 bytes / 64 hex
/// chars) and its id. **Phase 1 placeholder**: empty until the signing-infra
/// follow-up bakes in the real key material. While empty,
/// [`pinned_registry_key`] returns `None` and the fetch path treats documents
/// as unsigned (migration). Tests inject their own key via the pure functions.
const REGISTRY_PINNED_KEY_HEX: &str = "";
/// Key id matching [`REGISTRY_PINNED_KEY_HEX`].
pub const REGISTRY_PINNED_KEY_ID: &str = "conductor-registry-v1";

// Build-time safety gate (Council R2): the baked key must be either empty
// (Phase-1 placeholder) or a full 32-byte / 64-hex Ed25519 key. A truncated or
// malformed key pasted in by the signing-infra follow-up fails the build here
// rather than silently degrading to "no pin" at runtime.
const _: () = assert!(
    REGISTRY_PINNED_KEY_HEX.is_empty() || REGISTRY_PINNED_KEY_HEX.len() == 64,
    "REGISTRY_PINNED_KEY_HEX must be empty (Phase-1) or exactly 64 hex chars"
);

/// Maximum accepted registry document size, in bytes. A registry of thousands
/// of plugins is well under this; the cap bounds the memory/CPU an attacker can
/// force the client to spend parsing/verifying an oversized document BEFORE the
/// signature is checked (Council R2 — DoS via unbounded payload).
pub const MAX_REGISTRY_DOC_BYTES: usize = 8 * 1024 * 1024;

/// The signed-registry envelope (see module docs).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedRegistryEnvelope {
    /// The registry document JSON carried verbatim as an opaque string.
    pub payload: String,
    /// Hex-encoded Ed25519 signature over [`registry_signed_message`]
    /// (`DOMAIN || key_id || NUL || payload`).
    pub signature: String,
    /// Id of the signing key (must match the pinned key in Phase 1).
    pub key_id: String,
    /// Optional registry-key **rotation manifest** (ADR-027 D10d-source key
    /// rotation), as the `{"signing_keys":[...]}` JSON (reusing the §D9 chain
    /// format). When present, the chain is validated against the pinned **root**
    /// key and the document must be signed by the chain **head** (current) key;
    /// when absent, the document is verified directly against the pinned key.
    /// Carried in the envelope so a single fetch self-describes its trust chain;
    /// its integrity is proved by the chain validation (root-pinned +
    /// rotation-signed-by links), not by being inside the document signature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_manifest: Option<String>,
}

/// The rollback-relevant header fields parsed out of a verified payload. Other
/// registry fields are ignored here — the caller parses the full document from
/// the returned [`VerifiedRegistry::payload`] once trust is established.
#[derive(Debug, Clone, Deserialize)]
struct RegistryTrustHeader {
    /// `Option` (not a `#[serde(default)] u64`) so an OMITTED field is
    /// detectable and rejected — a defaulted `0` would slip through the cache
    /// path's `>=` check on fresh state (`0 >= 0`; Council on #2049).
    sequence_number: Option<u64>,
    published_at: Option<String>,
}

/// Per-client trust state persisted across fetches for rollback/replay
/// protection. Lives in a file SEPARATE from the plugin trust store
/// (`trusted_keys.json`) so registry trust can evolve independently (Council
/// R1 isolation note).
///
/// **Integrity boundary (Phase 1):** the on-disk state file is plain JSON; its
/// integrity rests on the OS file permissions of `~/.conductor/` — the same
/// trust root that holds every other Conductor trust artifact, and which D10a's
/// persistence veto now guards against shell-action writes. A local attacker
/// who can rewrite this file could roll the high-water marks back; binding the
/// file with a device-secret-keyed HMAC (keychain) closes that, but a device
/// secret is an ADR-042 Phase-B concern and is tracked as a #1897 follow-up,
/// not part of this client-validation slice (Council R4).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryTrustState {
    /// Id of the key whose documents have been accepted so far.
    #[serde(default)]
    pub key_id: Option<String>,
    /// Highest `sequence_number` accepted to date.
    #[serde(default)]
    pub last_sequence_number: u64,
    /// Most recent `published_at` accepted (RFC 3339).
    #[serde(default)]
    pub last_published_at: Option<String>,
    /// Highest rotation-chain head `seq` accepted (ADR-027 D10d-source key
    /// rotation anti-rollback). A manifest whose head `seq` is lower is a
    /// truncated/rolled-back chain and is rejected. `None` until a rotated
    /// document is first accepted.
    #[serde(default)]
    pub last_chain_head_seq: Option<u32>,
    /// An accepted M-of-N escrow **root override** (ADR-027 D10d-source
    /// catastrophic recovery). When set, it re-anchors the effective registry
    /// root to a fresh key (overriding the compile-time pin) and carries the
    /// `override_seq` high-water mark for anti-rollback. `None` until an override
    /// is accepted. See [`crate::plugin::registry_escrow`].
    #[serde(default)]
    pub root_override: Option<AcceptedRootOverride>,
}

impl RegistryTrustState {
    /// The effective registry ROOT this state trusts: the accepted escrow root
    /// override's new key if one has been recorded, else the supplied
    /// compile-time pin. Returns an [`EffectiveRoot`] so a recorded-but-malformed
    /// override ([`EffectiveRoot::OverrideUnusable`]) is fail-closed by the caller
    /// rather than silently degrading to unverified or reverting to the pin.
    pub fn effective_root(&self, pinned: Option<VerifyingKey>) -> EffectiveRoot {
        effective_root(pinned, self.root_override.as_ref())
    }
}

/// A document whose signature verified against the pinned key.
#[derive(Debug, Clone)]
pub struct VerifiedRegistry {
    /// The trusted inner registry JSON (caller parses this into the registry).
    pub payload: String,
    /// Trust state advanced past this document (persist after fetch).
    pub new_state: RegistryTrustState,
}

/// Failure reasons for registry document trust.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryTrustError {
    /// The bytes are not a `{payload, signature, key_id}` envelope.
    NotAnEnvelope,
    /// `signature` was not valid hex / wrong length.
    BadSignatureEncoding(String),
    /// The Ed25519 signature did not verify against the pinned key.
    SignatureInvalid,
    /// `key_id` did not match the pinned/known key.
    KeyIdMismatch { expected: String, found: String },
    /// `sequence_number` was not strictly greater than the last accepted.
    RollbackSequence { last: u64, got: u64 },
    /// The signed document omitted the required `sequence_number` (or used the
    /// reserved value `0`). It is mandatory and 1-indexed so an omitted field
    /// cannot default to `0` and slip through the cache path's `>=` check.
    MissingSequenceNumber,
    /// `published_at` moved backwards relative to the last accepted.
    RollbackPublishedAt { last: String, got: String },
    /// A `published_at` value was not parseable as an RFC 3339 timestamp, so a
    /// safe temporal ordering could not be established.
    BadTimestamp(String),
    /// A signed document omitted the required `published_at` field. It is
    /// mandatory so the temporal rollback guard always engages — otherwise a
    /// document that simply never carries the field permanently disables it.
    MissingPublishedAt,
    /// The raw document exceeded [`MAX_REGISTRY_DOC_BYTES`] — refused before any
    /// parsing/verification work (DoS bound).
    DocumentTooLarge { size: usize, max: usize },
    /// The verified payload did not parse as a registry header.
    PayloadParse(String),
    /// The embedded rotation manifest could not be decoded (DoS guard, bad hex,
    /// schema). Carries the underlying §D9 `ManifestParseError` rendering.
    ManifestParse(String),
    /// The rotation chain failed validation — not rooted at the pinned key, a
    /// broken/forged link, a rolled-back head `seq`, etc. Carries the underlying
    /// §D9 `RotationError` rendering.
    RotationChainInvalid(String),
    /// An escrow root override is recorded in trust state but its stored key is
    /// unusable (corrupt/tampered state). Fail closed — the client neither
    /// accepts the document unverified nor reverts to the overridden pin, since
    /// either would be a downgrade past the recovery (escrow §D10d-source).
    RootOverrideUnusable,
    /// A manifest-absent (verify-against-root) document arrived after a rotation
    /// had already been accepted. Once the chain has advanced, the root is no
    /// longer the active signer, so dropping the manifest would let an old-root
    /// holder downgrade past the rotation (Copilot review on #2049). After a
    /// rotation, the document MUST carry the chain so the head key is required.
    RotationManifestRequired { last_chain_head_seq: u32 },
}

impl std::fmt::Display for RegistryTrustError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAnEnvelope => write!(f, "registry document is not a signed envelope"),
            Self::BadSignatureEncoding(e) => write!(f, "registry signature encoding invalid: {e}"),
            Self::SignatureInvalid => {
                write!(
                    f,
                    "registry signature did not verify against the pinned key"
                )
            }
            Self::KeyIdMismatch { expected, found } => write!(
                f,
                "registry key_id mismatch: pinned `{expected}`, document `{found}`"
            ),
            Self::RollbackSequence { last, got } => write!(
                f,
                "registry rollback/replay: sequence_number {got} <= last accepted {last}"
            ),
            Self::MissingSequenceNumber => write!(
                f,
                "signed registry document is missing the required sequence_number (must be >= 1)"
            ),
            Self::RollbackPublishedAt { last, got } => write!(
                f,
                "registry rollback: published_at `{got}` precedes last accepted `{last}`"
            ),
            Self::BadTimestamp(e) => write!(f, "registry published_at is not valid RFC 3339: {e}"),
            Self::MissingPublishedAt => {
                write!(
                    f,
                    "signed registry document is missing the required published_at field"
                )
            }
            Self::DocumentTooLarge { size, max } => write!(
                f,
                "registry document too large: {size} bytes exceeds the {max}-byte limit"
            ),
            Self::PayloadParse(e) => write!(f, "verified registry payload did not parse: {e}"),
            Self::ManifestParse(e) => write!(f, "registry rotation manifest did not parse: {e}"),
            Self::RotationChainInvalid(e) => {
                write!(f, "registry rotation chain failed validation: {e}")
            }
            Self::RootOverrideUnusable => write!(
                f,
                "registry root override is recorded but its stored key is unusable; \
                 refusing to verify (failing closed rather than downgrading)"
            ),
            Self::RotationManifestRequired {
                last_chain_head_seq,
            } => write!(
                f,
                "registry document omits the rotation manifest after a rotation \
                 (accepted head seq {last_chain_head_seq}); the manifest is required \
                 so the current head key signs, not the rotated-away root"
            ),
        }
    }
}

impl std::error::Error for RegistryTrustError {}

/// The exact bytes the registry key signs: `DOMAIN || key_id || NUL ||
/// payload`.
///
/// Signed and verified with **pure Ed25519** (which performs its own internal
/// SHA-512 hashing of the message), so there is no separate application-level
/// pre-hash that signer and verifier could disagree on — closes the
/// double-hashing footgun (Council R1). The domain tag prevents a signature
/// minted for some other Conductor artifact from being replayed here, and
/// binding `key_id` INTO the signed bytes (Council R2) means an attacker cannot
/// swap the envelope's `key_id` to point the verifier at a different key while
/// keeping a valid signature — the NUL separator keeps `key_id` and `payload`
/// unambiguously delimited.
pub fn registry_signed_message(key_id: &str, payload: &str) -> Vec<u8> {
    let mut m = Vec::with_capacity(REGISTRY_SIG_DOMAIN.len() + key_id.len() + 1 + payload.len());
    m.extend_from_slice(REGISTRY_SIG_DOMAIN);
    m.extend_from_slice(key_id.as_bytes());
    m.push(0);
    m.extend_from_slice(payload.as_bytes());
    m
}

/// Verify a hex Ed25519 signature over [`registry_signed_message`]. Uses
/// `verify_strict` (rejects non-canonical / small-order keys — no signature
/// malleability), matching the D9 sibling.
pub fn verify_registry_signature(
    key_id: &str,
    payload: &str,
    signature_hex: &str,
    key: &VerifyingKey,
) -> Result<(), RegistryTrustError> {
    let sig_bytes = hex::decode(signature_hex.trim())
        .map_err(|e| RegistryTrustError::BadSignatureEncoding(e.to_string()))?;
    let sig_array: [u8; 64] = sig_bytes
        .as_slice()
        .try_into()
        .map_err(|_| RegistryTrustError::BadSignatureEncoding("expected 64 bytes".to_string()))?;
    let signature = Signature::from_bytes(&sig_array);
    let message = registry_signed_message(key_id, payload);
    key.verify_strict(&message, &signature)
        .map_err(|_| RegistryTrustError::SignatureInvalid)
}

/// Resolve the verifying key a *rotated* registry document must be signed by.
///
/// Validates the embedded rotation manifest against the pinned **root** key
/// (reusing the §D9 chain engine: each successor rotation-signed by its
/// predecessor, gap-free monotonic `seq` / `valid_from`, no rollback below
/// `pinned_max_seq`), then returns the chain **head** (current) key and its
/// `seq`. Only the head key may sign a current registry — a rotated-away key's
/// window has closed, so a compromised predecessor cannot sign fresh documents.
fn resolve_rotation_head_key(
    manifest_json: &str,
    pinned_root: &VerifyingKey,
    pinned_max_seq: Option<u32>,
) -> Result<(VerifyingKey, u32), RegistryTrustError> {
    let manifest = PluginKeyManifestJson::parse(manifest_json)
        .and_then(|j| j.into_manifest())
        .map_err(|e| RegistryTrustError::ManifestParse(format!("{e:?}")))?;
    let mut trusted = HashSet::new();
    trusted.insert(fingerprint_of(&pinned_root.to_bytes()));
    let chain = validate_chain_pinned(&manifest, &trusted, pinned_max_seq)
        .map_err(|e| RegistryTrustError::RotationChainInvalid(format!("{e:?}")))?;
    // Defensive: `validate_chain_pinned` rejects an empty manifest
    // (`RotationError::Empty`) before building a `VerifiedChain`, so the success
    // path always has at least the root — this guard is unreachable but kept for
    // explicitness (Copilot review on #2049).
    let head = chain
        .keys
        .last()
        .ok_or_else(|| RegistryTrustError::RotationChainInvalid("empty verified chain".into()))?;
    let head_key = VerifyingKey::from_bytes(&head.public_key).map_err(|_| {
        RegistryTrustError::RotationChainInvalid("head key is not a valid Ed25519 point".into())
    })?;
    Ok((head_key, chain.head_seq()))
}

/// Cheap predicate: do these bytes parse as a signed envelope with a non-empty
/// signature? Lets a caller branch signed-vs-legacy without committing to the
/// verify path. An over-large input returns `false` without parsing (DoS bound,
/// Council R3) — the trust decision itself goes through [`decide_fetch`] /
/// [`decide_cache`], which gate on the pinned key rather than on this shape
/// predicate.
pub fn is_signed_envelope(raw: &str) -> bool {
    if raw.len() > MAX_REGISTRY_DOC_BYTES {
        return false;
    }
    serde_json::from_str::<SignedRegistryEnvelope>(raw)
        .map(|e| !e.signature.trim().is_empty())
        .unwrap_or(false)
}

/// The pinned registry verifying key, or `None` while the Phase-1 placeholder
/// is empty.
pub fn pinned_registry_key() -> Option<VerifyingKey> {
    parse_verifying_key(REGISTRY_PINNED_KEY_HEX).ok()
}

fn parse_verifying_key(hex_str: &str) -> Result<VerifyingKey, RegistryTrustError> {
    let bytes = hex::decode(hex_str.trim())
        .map_err(|e| RegistryTrustError::BadSignatureEncoding(e.to_string()))?;
    let arr: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
        RegistryTrustError::BadSignatureEncoding("expected 32-byte key".to_string())
    })?;
    VerifyingKey::from_bytes(&arr).map_err(|_| {
        RegistryTrustError::BadSignatureEncoding("not a valid Ed25519 key".to_string())
    })
}

/// The shared verification core for BOTH the fetch and cache paths, so the
/// cache can never be used to downgrade past a guard the fetch enforces
/// (Council on #2049). Enforces, in order: size cap → envelope parse → `key_id`
/// → rotation chain (validated against the pinned root, chain anti-rollback vs
/// `state.last_chain_head_seq`, manifest-required-after-rotation) → signature
/// against the resolved key → document `sequence_number` → required
/// `published_at`. Returns the trusted payload and the would-be advanced state.
///
/// `strict_sequence` is `true` for the fetch path (a new document must have
/// `sequence_number` STRICTLY greater than last) and `false` for the cache path
/// (the cache holds the current document, whose sequence equals last — `>=`
/// allows it while still rejecting an older swapped-in document).
fn verify_envelope(
    raw: &str,
    pinned_root: &VerifyingKey,
    pinned_key_id: &str,
    state: &RegistryTrustState,
    strict_sequence: bool,
) -> Result<(String, RegistryTrustState), RegistryTrustError> {
    check_doc_size(raw)?;
    let envelope: SignedRegistryEnvelope =
        serde_json::from_str(raw).map_err(|_| RegistryTrustError::NotAnEnvelope)?;
    if envelope.key_id != pinned_key_id {
        return Err(RegistryTrustError::KeyIdMismatch {
            expected: pinned_key_id.to_string(),
            found: envelope.key_id,
        });
    }
    // Determine the key the document must be signed by. With a rotation
    // manifest, that is the chain HEAD (validated against the pinned root, with
    // chain anti-rollback vs the high-water mark); without one, it is the pinned
    // key directly — but only if no rotation has been accepted yet.
    let (verifying_key, new_chain_head_seq) = match &envelope.key_manifest {
        Some(manifest_json) => {
            let (head_key, head_seq) =
                resolve_rotation_head_key(manifest_json, pinned_root, state.last_chain_head_seq)?;
            (head_key, Some(head_seq))
        }
        None => {
            // Once a rotation has been accepted, the root is no longer the
            // active signer — a manifest-absent document would be verified
            // against the (rotated-away) root, letting an old-root holder
            // downgrade past the rotation (Copilot review on #2049). Pre-rotation
            // single-key documents (no rotation ever seen) remain accepted
            // against the pinned root.
            if let Some(last) = state.last_chain_head_seq {
                return Err(RegistryTrustError::RotationManifestRequired {
                    last_chain_head_seq: last,
                });
            }
            (*pinned_root, None)
        }
    };
    // Verify BEFORE parsing the inner payload (TOCTOU-safe). The signed bytes
    // bind the (already pinned-checked) key_id, so it cannot be swapped.
    verify_registry_signature(
        &envelope.key_id,
        &envelope.payload,
        &envelope.signature,
        &verifying_key,
    )?;

    let header: RegistryTrustHeader = serde_json::from_str(&envelope.payload)
        .map_err(|e| RegistryTrustError::PayloadParse(e.to_string()))?;

    // `sequence_number` is REQUIRED and 1-indexed: an omitted field (which would
    // serde-default to `0`) or an explicit `0` is rejected, so it cannot slip
    // through the cache path's `>=` check on fresh state (`0 >= 0`; Council on
    // #2049). Then enforce no-rollback: the cache path uses `>=` since it holds
    // the current document (seq == last); the fetch path uses strict `>`.
    let seq = header
        .sequence_number
        .filter(|&n| n >= 1)
        .ok_or(RegistryTrustError::MissingSequenceNumber)?;
    let seq_ok = if strict_sequence {
        seq > state.last_sequence_number
    } else {
        seq >= state.last_sequence_number
    };
    if !seq_ok {
        return Err(RegistryTrustError::RollbackSequence {
            last: state.last_sequence_number,
            got: seq,
        });
    }
    // published_at is REQUIRED (Council R3/R4): omitting it would permanently
    // disable the temporal guard. Compared as parsed INSTANTS — lexical RFC 3339
    // compare is bypassable (`+00:00` vs `Z`, fractional seconds; Council R2).
    let got = header
        .published_at
        .as_deref()
        .ok_or(RegistryTrustError::MissingPublishedAt)?;
    let got_dt = parse_rfc3339(got)?;
    // The INCOMING value must be valid RFC 3339 (above). The PERSISTED value is
    // compared tolerantly: a corrupted/tampered state file with a malformed
    // last_published_at must NOT brick every fetch (DoS via poisoned state;
    // Council #2049 R3) — treat an unparseable stored value as "no prior". The
    // sequence_number high-water mark remains the primary monotonic guard.
    if let Some(last) = &state.last_published_at
        && let Ok(last_dt) = parse_rfc3339(last)
        && got_dt < last_dt
    {
        return Err(RegistryTrustError::RollbackPublishedAt {
            last: last.clone(),
            got: got.to_string(),
        });
    }

    let new_state = RegistryTrustState {
        key_id: Some(pinned_key_id.to_string()),
        last_sequence_number: seq,
        last_published_at: Some(got.to_string()),
        last_chain_head_seq: new_chain_head_seq,
        // An accepted escrow root override persists across document fetches — it
        // re-anchored the trust root; a routine document does not clear it.
        root_override: state.root_override.clone(),
    };
    Ok((envelope.payload, new_state))
}

/// Full fetch-time verification: signature + key_id + rotation chain + monotonic
/// rollback guards (strict `>` sequence), returning the trusted payload and the
/// advanced trust state. `pinned_key_id` is the expected `key_id`; `key` is the
/// pinned ROOT; `state` is the last-accepted trust state (default on first fetch).
pub fn verify_signed_registry(
    raw: &str,
    key: &VerifyingKey,
    pinned_key_id: &str,
    state: &RegistryTrustState,
) -> Result<VerifiedRegistry, RegistryTrustError> {
    let (payload, new_state) = verify_envelope(raw, key, pinned_key_id, state, true)?;
    Ok(VerifiedRegistry { payload, new_state })
}

/// Cache-path verification for a locally-cached document. Runs the SAME guards
/// as the fetch path ([`verify_envelope`]) — including the rotation-manifest
/// requirement and chain/sequence anti-rollback — but with `>=` sequence
/// semantics (the cache holds the current document, whose sequence equals the
/// last accepted). This closes the cache-downgrade hole (Council on #2049): a
/// local attacker swapping the cache file for an older rotated or
/// manifest-absent root-signed document is now rejected. No state advance — the
/// returned would-be state is discarded; the cache only yields the trusted
/// payload. `state` is the current persisted trust state.
pub fn verify_registry_integrity(
    raw: &str,
    key: &VerifyingKey,
    pinned_key_id: &str,
    state: &RegistryTrustState,
) -> Result<String, RegistryTrustError> {
    let (payload, _new_state) = verify_envelope(raw, key, pinned_key_id, state, false)?;
    Ok(payload)
}

/// Refuse an over-large raw document before any parse/verify work (DoS bound).
fn check_doc_size(raw: &str) -> Result<(), RegistryTrustError> {
    if raw.len() > MAX_REGISTRY_DOC_BYTES {
        return Err(RegistryTrustError::DocumentTooLarge {
            size: raw.len(),
            max: MAX_REGISTRY_DOC_BYTES,
        });
    }
    Ok(())
}

/// Parse an RFC 3339 timestamp into a comparable instant; a malformed value is
/// a hard error so ordering is never decided on un-parseable input.
fn parse_rfc3339(s: &str) -> Result<chrono::DateTime<chrono::FixedOffset>, RegistryTrustError> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map_err(|e| RegistryTrustError::BadTimestamp(format!("`{s}`: {e}")))
}

/// Outcome of the fetch-time trust decision.
#[derive(Debug, Clone)]
pub enum FetchDecision {
    /// A registry key is pinned and the document verified against it.
    Verified(VerifiedRegistry),
    /// No registry key is pinned (Phase-1 migration) — the document cannot be
    /// cryptographically verified. `payload` is the (UNTRUSTED) inner registry
    /// JSON; the caller uses it only with a migration warning.
    UnverifiedNoPin { payload: String },
}

/// Cache-path trust outcome (integrity only; no rollback advance).
#[derive(Debug, Clone)]
pub struct CacheOutcome {
    /// The (trusted iff `verified`) registry JSON.
    pub payload: String,
    /// Whether the cached document's signature was verified against a pin.
    pub verified: bool,
}

/// Fetch-time trust decision.
///
/// **CRITICAL (Council R1):** whether to verify is gated on whether a key is
/// **pinned**, NOT on whether the *document* looks signed. Branching on the
/// document shape (e.g. `is_signed_envelope`) let a network attacker **strip**
/// the envelope and force the unsigned path — a signature-stripping downgrade.
/// When a key is pinned, a stripped/bare document fails `verify_signed_registry`
/// with `NotAnEnvelope` and is rejected; an unsigned path is reachable ONLY
/// when no key is pinned (Phase-1, where there is no trust anchor to strip).
pub fn decide_fetch(
    raw: &str,
    pinned: Option<&VerifyingKey>,
    pinned_key_id: &str,
    state: &RegistryTrustState,
) -> Result<FetchDecision, RegistryTrustError> {
    check_doc_size(raw)?;
    // Resolve the effective trust anchor: an accepted escrow root override
    // re-anchors trust to a fresh root (catastrophic recovery), otherwise the
    // compile-time pin. Gating on the *effective* root (not the raw pin) means a
    // recovered registry verifies against the new root while a stripped document
    // is still rejected whenever ANY anchor exists. A recorded-but-unusable
    // override fails closed — it must NOT fall through to the unverified path.
    match state.effective_root(pinned.copied()) {
        EffectiveRoot::Anchor(key) => Ok(FetchDecision::Verified(verify_signed_registry(
            raw,
            &key,
            pinned_key_id,
            state,
        )?)),
        EffectiveRoot::Migration => Ok(FetchDecision::UnverifiedNoPin {
            payload: unverified_payload(raw),
        }),
        EffectiveRoot::OverrideUnusable => Err(RegistryTrustError::RootOverrideUnusable),
    }
}

/// Cache-time trust decision. Same pinned-key gating as [`decide_fetch`]
/// (integrity only — the cache holds the last-fetched document, so no rollback
/// advance).
pub fn decide_cache(
    raw: &str,
    pinned: Option<&VerifyingKey>,
    pinned_key_id: &str,
    state: &RegistryTrustState,
) -> Result<CacheOutcome, RegistryTrustError> {
    check_doc_size(raw)?;
    // Same effective-root resolution as the fetch path (escrow override → fresh
    // root, else the pin; a recorded-but-unusable override fails closed).
    match state.effective_root(pinned.copied()) {
        EffectiveRoot::Anchor(key) => Ok(CacheOutcome {
            payload: verify_registry_integrity(raw, &key, pinned_key_id, state)?,
            verified: true,
        }),
        EffectiveRoot::Migration => Ok(CacheOutcome {
            payload: unverified_payload(raw),
            verified: false,
        }),
        EffectiveRoot::OverrideUnusable => Err(RegistryTrustError::RootOverrideUnusable),
    }
}

/// Best-effort inner payload when there is no pin: the envelope's inner payload
/// if it is an envelope, else the raw bytes. Used only on the no-trust-anchor
/// path — the result is explicitly untrusted.
fn unverified_payload(raw: &str) -> String {
    serde_json::from_str::<SignedRegistryEnvelope>(raw)
        .map(|e| e.payload)
        .unwrap_or_else(|_| raw.to_string())
}

#[cfg(test)]
#[path = "registry_trust_tests.rs"]
mod registry_trust_tests;
