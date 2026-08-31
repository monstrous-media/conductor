// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-027 **D9** — plugin signing-key rotation with a verifiable trust chain.
//!
//! v2.7 pins one Ed25519 key per plugin; rotating it forces every user to
//! re-trust out of band. D9 publishes an ordered *rotation chain* where each new
//! key is cryptographically endorsed by its predecessor, so trusting the root
//! **transitively** trusts every validly-rotated successor — no prompt (users
//! can't verify continuity by eye; the chain proves it). This is the **pure
//! validation core** (no I/O); the loader supplies parsed bytes + the trust set.
//!
//! ## Cryptographic scheme (LLM-Council reviewed, high tier)
//!
//! Each non-root entry's `rotation_signature` is an Ed25519 signature **by the
//! predecessor** over a **domain-separated fixed-width binary** payload (never
//! JSON/JCS — binary concat has no canonicalisation malleability):
//!
//! ```text
//! b"CONDUCTOR_KEY_ROTATION_V1\0" ‖ chain_id[32] ‖ seq(u32 BE)
//!     ‖ valid_from_unix(i64 BE) ‖ predecessor_public_key[32] ‖ new_public_key[32]
//! ```
//!
//! Both keys are bound by their **full** public key (binds identity via Ed25519,
//! not SHA-256 collision-resistance); `chain_id` (root fp) blocks cross-chain
//! lifting; `seq` blocks reordering/dropping interior entries. Chains are capped
//! at [`MAX_CHAIN_LENGTH`] to bound signature-verification work (DoS).
//!
//! Anti-rollback (Council Phase 5) is **enforced** here: [`validate_chain_pinned`]
//! rejects a chain whose head `seq` is below the caller's persisted high-water
//! mark. The **revocation list (CRL)** is enforced by [`validate_chain_full`]:
//! any chain containing a revoked fingerprint (root through head) is refused.
//! Only the *storage* of the high-water mark and the CRL is the loader's job
//! (this core does no I/O). Deferred follow-up: Phase 0 artifact hash+sidecar
//! checks (already in [`crate::plugin::signing::verify_plugin_signature`]).

use std::collections::HashSet;

use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};

/// Domain-separation tag prefixing every rotation payload. The trailing NUL
/// guarantees the tag can't be a prefix of any following field.
pub const ROTATION_DOMAIN_TAG: &[u8] = b"CONDUCTOR_KEY_ROTATION_V1\0";

/// Maximum number of entries in a rotation chain. Each non-root entry costs one
/// Ed25519 verification, so an unbounded chain is a CPU-exhaustion (DoS) vector;
/// a malicious manifest is rejected up front. 64 rotations is far beyond any
/// realistic plugin's lifetime. (Also keeps `seq` well within `u32`.)
pub const MAX_CHAIN_LENGTH: usize = 64;

/// A 32-byte key fingerprint: `SHA-256(public_key)`.
pub type Fingerprint = [u8; 32];
/// A raw 32-byte Ed25519 public key.
pub type PublicKeyBytes = [u8; 32];

/// `SHA-256(public_key)` — the canonical fingerprint of a signing key.
pub fn fingerprint_of(public_key: &PublicKeyBytes) -> Fingerprint {
    let mut h = Sha256::new();
    h.update(public_key);
    h.finalize().into()
}

/// Build the exact bytes a predecessor signs to endorse a successor key. Kept
/// public so the signing CLI (`conductor-sign rotate-key`) produces byte-for-byte
/// identical input.
///
/// Both endpoints are bound by their **full** public key (not a fingerprint):
/// the predecessor — the signer — and the new key being introduced. Binding the
/// full keys roots identity in Ed25519 itself rather than SHA-256 collision
/// resistance.
pub fn rotation_payload(
    chain_id: &Fingerprint,
    seq: u32,
    valid_from_unix: i64,
    predecessor_public_key: &PublicKeyBytes,
    new_public_key: &PublicKeyBytes,
) -> Vec<u8> {
    let mut msg = Vec::with_capacity(ROTATION_DOMAIN_TAG.len() + 32 + 4 + 8 + 32 + 32);
    msg.extend_from_slice(ROTATION_DOMAIN_TAG);
    msg.extend_from_slice(chain_id);
    msg.extend_from_slice(&seq.to_be_bytes());
    msg.extend_from_slice(&valid_from_unix.to_be_bytes());
    msg.extend_from_slice(predecessor_public_key);
    msg.extend_from_slice(new_public_key);
    msg
}

/// One entry of a plugin's signing-key rotation chain. Timestamps are already
/// normalised to Unix seconds by the loader (this core is date-format-agnostic).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SigningKeyEntry {
    /// Position in the chain. Root is `0`; each successor is `+1`.
    pub seq: u32,
    /// The raw 32-byte Ed25519 public key.
    pub public_key: PublicKeyBytes,
    /// When this key becomes the active signer (Unix seconds).
    pub valid_from_unix: i64,
    /// Fingerprint of the predecessor that endorsed this key. `None` iff root.
    pub rotation_signed_by: Option<Fingerprint>,
    /// Predecessor's Ed25519 signature over [`rotation_payload`]. `None` iff root.
    pub rotation_signature: Option<[u8; 64]>,
}

impl SigningKeyEntry {
    /// This entry's fingerprint, `SHA-256(public_key)`.
    pub fn fingerprint(&self) -> Fingerprint {
        fingerprint_of(&self.public_key)
    }
}

/// A plugin's parsed rotation chain (transport-decoded; not yet validated).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginKeyManifest {
    /// Key entries. Order is not trusted; validation sorts and checks by `seq`.
    pub keys: Vec<SigningKeyEntry>,
}

impl PluginKeyManifest {
    /// A non-rotating, root-only manifest — the migration shape for a legacy
    /// v2.7 single-key plugin.
    pub fn single_key(public_key: PublicKeyBytes, valid_from_unix: i64) -> Self {
        Self {
            keys: vec![SigningKeyEntry {
                seq: 0,
                public_key,
                valid_from_unix,
                rotation_signed_by: None,
                rotation_signature: None,
            }],
        }
    }
}

/// A key whose place in the chain has been cryptographically verified, with its
/// resolved active window `[valid_from, valid_until)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedKey {
    /// Position in the chain.
    pub seq: u32,
    /// `SHA-256(public_key)`.
    pub fingerprint: Fingerprint,
    /// The raw 32-byte Ed25519 public key.
    pub public_key: PublicKeyBytes,
    /// Start of this key's active window (Unix seconds).
    pub valid_from_unix: i64,
    /// End of the window — the successor's `valid_from`, or `None` for the head.
    pub valid_until_unix: Option<i64>,
}

/// A fully-validated rotation chain. Construction implies every entry is sound
/// and transitively trusted from a directly-trusted root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedChain {
    /// Root (seq 0) fingerprint — the chain identity.
    pub chain_id: Fingerprint,
    /// Verified keys in `seq` order.
    pub keys: Vec<VerifiedKey>,
}

/// Why a rotation chain was rejected. Every variant is a **hard fail**: a chain
/// that does not validate degrades the plugin to "pinned, non-rotating".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RotationError {
    /// The manifest has no key entries.
    Empty,
    /// The chain exceeds [`MAX_CHAIN_LENGTH`] — rejected before any signature
    /// verification to bound CPU cost (DoS).
    ChainTooLong { len: usize },
    /// No entry with `seq == 0`.
    NoRoot,
    /// More than one entry claims `seq == 0`.
    MultipleRoots,
    /// The root carries `rotation_signed_by` (a root is self-standing).
    RootHasPredecessor,
    /// The root carries a `rotation_signature`.
    RootHasSignature,
    /// A non-root entry is missing `rotation_signed_by`.
    NonRootMissingPredecessor { seq: u32 },
    /// A non-root entry is missing `rotation_signature`.
    NonRootMissingSignature { seq: u32 },
    /// A non-root entry names itself as its predecessor.
    SelfSignedNonRoot { seq: u32 },
    /// `seq` values are not a gap-free `0,1,2,…` run.
    SeqGap { expected: u32, got: u32 },
    /// Two entries share a `seq`.
    DuplicateSeq { seq: u32 },
    /// A public key (fingerprint) appears more than once in the chain. Reuse is
    /// forbidden: a "rotation" to the same key is meaningless, and a repeated
    /// key would make a signer's active window ambiguous (the earliest match
    /// could falsely reject a later, legitimate signature).
    DuplicateKey { seq: u32 },
    /// `valid_from` is not strictly increasing with `seq`.
    NonMonotonicValidFrom { seq: u32 },
    /// A public key is not a valid Ed25519 point.
    InvalidPublicKey { seq: u32 },
    /// `rotation_signed_by` does not match the predecessor's fingerprint.
    BrokenLink { seq: u32 },
    /// The predecessor's signature over the payload did not verify.
    BadRotationSignature { seq: u32 },
    /// The root fingerprint is not in the user's directly-trusted set.
    NoTrustAnchor,
    /// A key in the chain is on the revocation list.
    Revoked { seq: u32 },
    /// The chain's head `seq` is lower than the caller's persisted high-water
    /// mark — a truncated / rolled-back chain (a valid historical prefix
    /// replayed to force an older, possibly-compromised key).
    Rollback {
        pinned_max_seq: u32,
        chain_max_seq: u32,
    },
    /// The artifact signer's key is not part of the verified chain.
    SignerNotInChain,
    /// The artifact was signed outside its signer key's active window.
    SignedOutsideWindow,
    /// The artifact's `signed_at` is in the future relative to the verifier's
    /// clock — a bogus / forged timestamp.
    SignedInFuture,
}

impl std::fmt::Display for RotationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "plugin key-rotation chain rejected: {:?}", self)
    }
}

impl std::error::Error for RotationError {}

/// Validate a rotation chain (Council Phases 1–4) and resolve active windows.
///
/// `trusted` is the set of fingerprints the user directly trusts. On success the
/// returned [`VerifiedChain`] carries every key with its active window; use
/// [`VerifiedChain::resolve_signer`] for signer binding.
///
/// This form does **no** anti-rollback check (first-contact: there is no prior
/// high-water mark). Once a host has accepted a chain, it MUST use
/// [`validate_chain_pinned`] with the persisted head `seq` to reject truncated /
/// rolled-back chains.
pub fn validate_chain(
    manifest: &PluginKeyManifest,
    trusted: &HashSet<Fingerprint>,
) -> Result<VerifiedChain, RotationError> {
    validate_chain_pinned(manifest, trusted, None)
}

/// [`validate_chain`] plus **anti-rollback** enforcement (Council Phase 5).
///
/// `pinned_max_seq` is the highest chain head `seq` this host has previously
/// accepted for this chain — the caller's persisted high-water mark. A manifest
/// whose head `seq` is lower is a truncated / rolled-back chain (a valid
/// historical prefix replayed to force an older, possibly-compromised key) and
/// is rejected with [`RotationError::Rollback`].
///
/// Persistence of the mark is the **caller's** responsibility (this core does no
/// I/O): on success, read [`VerifiedChain::head_seq`] and store
/// `max(previous, head_seq)`.
pub fn validate_chain_pinned(
    manifest: &PluginKeyManifest,
    trusted: &HashSet<Fingerprint>,
    pinned_max_seq: Option<u32>,
) -> Result<VerifiedChain, RotationError> {
    validate_chain_full(manifest, trusted, &HashSet::new(), pinned_max_seq)
}

/// The full validator: [`validate_chain_pinned`] plus a **revocation list**
/// (CRL). `revoked` is the set of fingerprints known to be compromised; a
/// fingerprint on it is invalid **for all purposes** — signing *or* parenting —
/// so a chain containing any revoked key (root through head) is rejected with
/// [`RotationError::Revoked`]. Rotation chains handle ordinary updates; the CRL
/// is the separate, fast path for burning a compromised key (ADR-027 §D9,
/// Council R1). The list is supplied by the caller (this core does no I/O).
pub fn validate_chain_full(
    manifest: &PluginKeyManifest,
    trusted: &HashSet<Fingerprint>,
    revoked: &HashSet<Fingerprint>,
    pinned_max_seq: Option<u32>,
) -> Result<VerifiedChain, RotationError> {
    // ── Phase 1: structure & math ───────────────────────────────────────────
    if manifest.keys.is_empty() {
        return Err(RotationError::Empty);
    }
    // Bound the work BEFORE touching any entry: a huge manifest would otherwise
    // force one Ed25519 verification per link (CPU-exhaustion DoS).
    if manifest.keys.len() > MAX_CHAIN_LENGTH {
        return Err(RotationError::ChainTooLong {
            len: manifest.keys.len(),
        });
    }
    let root_count = manifest.keys.iter().filter(|e| e.seq == 0).count();
    match root_count {
        0 => return Err(RotationError::NoRoot),
        1 => {}
        _ => return Err(RotationError::MultipleRoots),
    }

    // Order is not trusted; sort by seq and require a gap-free 0,1,2,… run.
    let mut chain: Vec<&SigningKeyEntry> = manifest.keys.iter().collect();
    chain.sort_by_key(|e| e.seq);
    for (i, e) in chain.iter().enumerate() {
        let expected = i as u32;
        if e.seq != expected {
            if i > 0 && chain[i - 1].seq == e.seq {
                return Err(RotationError::DuplicateSeq { seq: e.seq });
            }
            return Err(RotationError::SeqGap {
                expected,
                got: e.seq,
            });
        }
    }

    // No key may appear twice. Reuse is meaningless as a "rotation" and would
    // make a signer's active window ambiguous: `resolve_signer` matches by key,
    // so a repeated key could resolve to the wrong (earlier) window and falsely
    // reject a legitimate later signature. Forbidding reuse keeps that lookup
    // unambiguous.
    let mut seen_fingerprints: HashSet<Fingerprint> = HashSet::with_capacity(chain.len());
    for e in &chain {
        if !seen_fingerprints.insert(e.fingerprint()) {
            return Err(RotationError::DuplicateKey { seq: e.seq });
        }
    }

    // Per-entry shape, self-signing, and strictly-increasing valid_from. (All
    // cheap — no cryptography yet; point validation is deferred to Phase 3 so it
    // runs only after the trust + rollback gates below.)
    for (i, e) in chain.iter().enumerate() {
        if i == 0 {
            if e.rotation_signed_by.is_some() {
                return Err(RotationError::RootHasPredecessor);
            }
            if e.rotation_signature.is_some() {
                return Err(RotationError::RootHasSignature);
            }
        } else {
            if e.rotation_signed_by.is_none() {
                return Err(RotationError::NonRootMissingPredecessor { seq: e.seq });
            }
            if e.rotation_signature.is_none() {
                return Err(RotationError::NonRootMissingSignature { seq: e.seq });
            }
            if e.rotation_signed_by == Some(e.fingerprint()) {
                return Err(RotationError::SelfSignedNonRoot { seq: e.seq });
            }
            if e.valid_from_unix <= chain[i - 1].valid_from_unix {
                return Err(RotationError::NonMonotonicValidFrom { seq: e.seq });
            }
        }
    }

    // ── Phase 2: cheap gates BEFORE any cryptography ─────────────────────────
    // Trust anchor and anti-rollback are O(1)/O(n) hash + integer checks. Run
    // them first so an untrusted or rolled-back chain is rejected without
    // spending Ed25519 point-decompression / signature work (DoS hardening).
    let chain_id = chain[0].fingerprint();
    if !trusted.contains(&chain_id) {
        return Err(RotationError::NoTrustAnchor);
    }
    // Revocation list: a compromised key is invalid for ALL purposes (signing
    // or parenting), so any chain that contains a revoked fingerprint — root
    // through head — is refused outright. Checked before crypto (cheap set
    // lookups) and before rollback so a known-bad chain fails fast.
    if !revoked.is_empty() {
        for e in &chain {
            if revoked.contains(&e.fingerprint()) {
                return Err(RotationError::Revoked { seq: e.seq });
            }
        }
    }
    // The chain is gap-free `0..n`, so the head seq is `len-1`. Reject a chain
    // older than what this host has already accepted (a truncated prefix
    // replayed to resurrect a rotated-away key).
    let chain_max_seq = chain[chain.len() - 1].seq;
    if let Some(pinned) = pinned_max_seq
        && chain_max_seq < pinned
    {
        return Err(RotationError::Rollback {
            pinned_max_seq: pinned,
            chain_max_seq,
        });
    }

    // ── Phase 3: cryptography (gated by Phase 2) ─────────────────────────────
    // Validate every key as a real Ed25519 point first (incl. the head, which is
    // never a predecessor), then walk the endorsement signatures.
    for e in &chain {
        VerifyingKey::from_bytes(&e.public_key)
            .map_err(|_| RotationError::InvalidPublicKey { seq: e.seq })?;
    }
    for i in 1..chain.len() {
        let entry = chain[i];
        let predecessor = chain[i - 1];
        let predecessor_fp = predecessor.fingerprint();

        // The "from" link must name the actual predecessor.
        if entry.rotation_signed_by != Some(predecessor_fp) {
            return Err(RotationError::BrokenLink { seq: entry.seq });
        }

        let pred_vk = VerifyingKey::from_bytes(&predecessor.public_key).map_err(|_| {
            RotationError::InvalidPublicKey {
                seq: predecessor.seq,
            }
        })?;

        // The payload binds the predecessor by its FULL public key (the
        // verifying key below), not just `predecessor_fp` — identity is rooted
        // in Ed25519, not SHA-256 collision-resistance.
        let payload = rotation_payload(
            &chain_id,
            entry.seq,
            entry.valid_from_unix,
            &predecessor.public_key,
            &entry.public_key,
        );
        // Presence is guaranteed by Phase 1, but resolve it fallibly rather than
        // `expect()` — a panic inside cryptographic validation is itself a DoS.
        let sig_bytes = entry
            .rotation_signature
            .as_ref()
            .ok_or(RotationError::NonRootMissingSignature { seq: entry.seq })?;
        let sig = Signature::from_bytes(sig_bytes);
        pred_vk
            .verify_strict(&payload, &sig)
            .map_err(|_| RotationError::BadRotationSignature { seq: entry.seq })?;
    }

    // ── Resolve active windows: [valid_from, next.valid_from) ────────────────
    let keys = chain
        .iter()
        .enumerate()
        .map(|(i, e)| VerifiedKey {
            seq: e.seq,
            fingerprint: e.fingerprint(),
            public_key: e.public_key,
            valid_from_unix: e.valid_from_unix,
            valid_until_unix: chain.get(i + 1).map(|n| n.valid_from_unix),
        })
        .collect();

    Ok(VerifiedChain { chain_id, keys })
}

impl VerifiedChain {
    /// The head (newest) key's `seq` — the value a caller persists as the
    /// anti-rollback high-water mark (see [`validate_chain_pinned`]).
    pub fn head_seq(&self) -> u32 {
        self.keys.last().map(|k| k.seq).unwrap_or(0)
    }

    /// The key whose active window `[valid_from, valid_until)` contains
    /// `now_unix` — the only key permitted to have signed a plugin that the
    /// loader accepts *now*. For a valid (strictly-increasing) chain at most one
    /// key matches; returns `None` when `now_unix` precedes the root's
    /// `valid_from` (the chain is not yet active).
    ///
    /// Binding to the *verifier's* clock — rather than the signature's
    /// self-asserted `signed_at`, which lies outside the signed payload and is
    /// thus forgeable (see [`resolve_active_signing_key`]) — is what prevents a
    /// compromised, rotated-away-from predecessor key from being used to sign a
    /// fresh artifact: that key's window has closed, so it is never the active
    /// signer.
    pub fn active_key_at(&self, now_unix: i64) -> Option<&VerifiedKey> {
        self.keys.iter().find(|k| {
            now_unix >= k.valid_from_unix && k.valid_until_unix.is_none_or(|until| now_unix < until)
        })
    }

    /// Phase 4 — confirm the artifact's signer key is in this chain, that the
    /// claimed `signed_at` is not in the future relative to `now_unix` (the
    /// verifier's clock), and that it falls inside that key's active window.
    pub fn resolve_signer(
        &self,
        signer_public_key: &PublicKeyBytes,
        signed_at_unix: i64,
        now_unix: i64,
    ) -> Result<(), RotationError> {
        // `find` is unambiguous because `validate_chain` rejects any chain that
        // reuses a key (`RotationError::DuplicateKey`), so at most one entry can
        // match — no risk of resolving to the wrong active window.
        let key = self
            .keys
            .iter()
            .find(|k| &k.public_key == signer_public_key)
            .ok_or(RotationError::SignerNotInChain)?;

        // A signature claimed in the future is bogus — an attacker could
        // otherwise pre-date an artifact into a not-yet-active key's window, or
        // forge a far-future timestamp to dodge later revocation windows.
        if signed_at_unix > now_unix {
            return Err(RotationError::SignedInFuture);
        }

        // `signed_at` must fall inside the key's active window
        // `[valid_from, valid_until)` — the head's window is open-ended.
        if signed_at_unix < key.valid_from_unix {
            return Err(RotationError::SignedOutsideWindow);
        }
        if let Some(until) = key.valid_until_unix
            && signed_at_unix >= until
        {
            return Err(RotationError::SignedOutsideWindow);
        }
        Ok(())
    }
}

// ── Transport: JSON manifest → engine types ─────────────────────────────────
//
// The on-disk/over-the-wire manifest carries hex strings and ISO-8601
// timestamps. Per the Council scheme, transport encoding is decoded to raw
// bytes / Unix seconds *before* any cryptographic check — the signed payload is
// pure binary, so the JSON layer can never introduce signature malleability.

/// One `signing_keys[]` entry as it appears in the JSON manifest.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct SigningKeyEntryJson {
    /// Position in the chain (root = 0).
    pub seq: u32,
    /// Hex-encoded 32-byte Ed25519 public key.
    pub public_key: String,
    /// ISO-8601 timestamp when this key becomes active.
    pub valid_from: String,
    /// Hex-encoded 32-byte predecessor fingerprint (absent for the root).
    #[serde(default)]
    pub rotation_signed_by: Option<String>,
    /// Hex-encoded 64-byte predecessor signature (absent for the root).
    #[serde(default)]
    pub rotation_signature: Option<String>,
}

/// The `{ "signing_keys": [...] }` manifest as parsed from JSON.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PluginKeyManifestJson {
    /// Ordered key entries.
    pub signing_keys: Vec<SigningKeyEntryJson>,
}

/// Hard cap on a serialized manifest, enforced *before* `serde_json` allocates.
/// `MAX_CHAIN_LENGTH` entries of hex keys/signatures + timestamps fit comfortably
/// in 64 KiB; anything larger is a memory-exhaustion (DoS) attempt.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;

/// A transport-layer (pre-crypto) manifest decoding failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestParseError {
    /// The serialized manifest exceeds [`MAX_MANIFEST_BYTES`] (DoS guard).
    TooLarge { bytes: usize },
    /// The manifest declares more than [`MAX_CHAIN_LENGTH`] keys (DoS guard).
    TooManyKeys { count: usize },
    /// The JSON did not parse / match the schema.
    Json(String),
    /// A hex field was not valid hex.
    BadHex { field: &'static str, seq: u32 },
    /// A decoded field had the wrong byte length.
    BadLength {
        field: &'static str,
        seq: u32,
        expected: usize,
        got: usize,
    },
    /// A `valid_from` timestamp was not parseable ISO-8601.
    BadTimestamp { seq: u32 },
}

impl std::fmt::Display for ManifestParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "plugin key manifest parse error: {:?}", self)
    }
}

impl std::error::Error for ManifestParseError {}

fn decode_hex_array<const N: usize>(
    s: &str,
    field: &'static str,
    seq: u32,
) -> Result<[u8; N], ManifestParseError> {
    let bytes = hex::decode(s).map_err(|_| ManifestParseError::BadHex { field, seq })?;
    if bytes.len() != N {
        return Err(ManifestParseError::BadLength {
            field,
            seq,
            expected: N,
            got: bytes.len(),
        });
    }
    let mut out = [0u8; N];
    out.copy_from_slice(&bytes);
    Ok(out)
}

impl PluginKeyManifestJson {
    /// Parse a JSON manifest string. The byte cap is checked *before*
    /// `serde_json` allocates, so an oversized blob can't exhaust memory.
    pub fn parse(json: &str) -> Result<Self, ManifestParseError> {
        if json.len() > MAX_MANIFEST_BYTES {
            return Err(ManifestParseError::TooLarge { bytes: json.len() });
        }
        let parsed: Self =
            serde_json::from_str(json).map_err(|e| ManifestParseError::Json(e.to_string()))?;
        if parsed.signing_keys.len() > MAX_CHAIN_LENGTH {
            return Err(ManifestParseError::TooManyKeys {
                count: parsed.signing_keys.len(),
            });
        }
        Ok(parsed)
    }

    /// Decode hex / ISO-8601 transport fields into the binary engine
    /// [`PluginKeyManifest`]. No cryptography happens here.
    pub fn into_manifest(self) -> Result<PluginKeyManifest, ManifestParseError> {
        if self.signing_keys.len() > MAX_CHAIN_LENGTH {
            return Err(ManifestParseError::TooManyKeys {
                count: self.signing_keys.len(),
            });
        }
        let mut keys = Vec::with_capacity(self.signing_keys.len());
        for e in self.signing_keys {
            let public_key = decode_hex_array::<32>(&e.public_key, "public_key", e.seq)?;
            let valid_from_unix = chrono::DateTime::parse_from_rfc3339(&e.valid_from)
                .map_err(|_| ManifestParseError::BadTimestamp { seq: e.seq })?
                .timestamp();
            let rotation_signed_by = match e.rotation_signed_by {
                Some(s) => Some(decode_hex_array::<32>(&s, "rotation_signed_by", e.seq)?),
                None => None,
            };
            let rotation_signature = match e.rotation_signature {
                Some(s) => Some(decode_hex_array::<64>(&s, "rotation_signature", e.seq)?),
                None => None,
            };
            keys.push(SigningKeyEntry {
                seq: e.seq,
                public_key,
                valid_from_unix,
                rotation_signed_by,
                rotation_signature,
            });
        }
        Ok(PluginKeyManifest { keys })
    }
}

/// Why resolving a plugin's active signing key via its rotation manifest
/// failed. Every variant is a **hard fail**: the loader must refuse the plugin
/// (degrade to "pinned, non-rotating"), never silently fall back to the bare
/// signature check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestTrustError {
    /// The `.keys.json` manifest could not be decoded (transport layer).
    Parse(ManifestParseError),
    /// The decoded chain failed cryptographic / trust validation.
    Rotation(RotationError),
    /// The chain validated, but no key's active window `[valid_from,
    /// valid_until)` contains the verifier's current time — e.g. the root's
    /// `valid_from` is still in the future. With no key active *now*, there is
    /// nothing a plugin signature may legitimately match, so this is a hard
    /// fail rather than a fall-through to a stale predecessor key.
    NoActiveKey { now_unix: i64 },
}

impl std::fmt::Display for ManifestTrustError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(e) => write!(f, "{e}"),
            Self::Rotation(e) => write!(f, "{e}"),
            Self::NoActiveKey { now_unix } => write!(
                f,
                "plugin key-rotation chain rejected: no signing key is active at {now_unix}"
            ),
        }
    }
}

impl std::error::Error for ManifestTrustError {}

/// Resolve the single public key a plugin's signature must use **right now**, by
/// validating its `.keys.json` **rotation manifest** against the directly
/// trusted root fingerprints and binding to the key whose active window
/// contains `now_unix`.
///
/// On success, returns the hex-encoded public key of the key active at
/// `now_unix` — for a valid (strictly-increasing) chain whose head window is
/// open-ended, that is the head once its `valid_from` has passed. Because a
/// validly rotated successor is endorsed by its predecessor (rooted in a
/// trusted key), the head is trusted **without** the user ever adding it
/// directly — the whole point of D9 rotation: rotating the signing key does not
/// force every user to re-trust out of band.
///
/// Crucially, only the *active* key is returned, **not** the full historical
/// set. A rotated-away-from predecessor key — which an attacker who compromised
/// it could otherwise use to sign a fresh malicious artifact — is no longer an
/// accepted signer. Binding is to the *verifier's* clock (`now_unix`), not the
/// signature's self-asserted `signed_at`: the latter sits outside the signed
/// payload (see [`crate::plugin::signing::sign_plugin`], which signs only the
/// binary hash) and is therefore forgeable, so it cannot be trusted to pin the
/// signer's window.
///
/// `revoked` and `pinned_max_seq` thread straight through to
/// [`validate_chain_full`] (CRL + anti-rollback). The load path passes an empty
/// revocation set and `None` until those stores are wired (ADR-027 §D9
/// follow-up); the parameters are present now so that wiring is purely additive.
///
/// Any decode, validation, or no-active-key failure is a **hard fail**
/// ([`ManifestTrustError`]).
pub fn resolve_active_signing_key(
    manifest_json: &str,
    trusted: &HashSet<Fingerprint>,
    revoked: &HashSet<Fingerprint>,
    pinned_max_seq: Option<u32>,
    now_unix: i64,
) -> Result<String, ManifestTrustError> {
    let manifest = PluginKeyManifestJson::parse(manifest_json)
        .map_err(ManifestTrustError::Parse)?
        .into_manifest()
        .map_err(ManifestTrustError::Parse)?;
    let chain = validate_chain_full(&manifest, trusted, revoked, pinned_max_seq)
        .map_err(ManifestTrustError::Rotation)?;
    let active = chain
        .active_key_at(now_unix)
        .ok_or(ManifestTrustError::NoActiveKey { now_unix })?;
    Ok(hex::encode(active.public_key))
}

#[cfg(test)]
#[path = "key_rotation_tests.rs"]
mod tests;
