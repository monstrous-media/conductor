// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Unit tests for [`super`] (ADR-027 D9 key-rotation validator). Kept in a
//! separate file (included via `#[path]`) so the engine module stays small
//! enough to fit a single multi-model review window.

use super::*;
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};

/// A test signer: keypair + derived fingerprint.
struct Key {
    sk: SigningKey,
    pk: PublicKeyBytes,
    fp: Fingerprint,
}

fn make_key(seed: u8) -> Key {
    let sk = SigningKey::from_bytes(&[seed; 32]);
    let pk = sk.verifying_key().to_bytes();
    let fp = fingerprint_of(&pk);
    Key { sk, pk, fp }
}

/// Build a valid `n`-key chain (root + n-1 rotations) with real signatures.
/// Returns the manifest, the trusted set (root fp), and the keys.
fn valid_chain(n: usize) -> (PluginKeyManifest, HashSet<Fingerprint>, Vec<Key>) {
    assert!(n >= 1);
    let keys: Vec<Key> = (0..n).map(|i| make_key(i as u8 + 1)).collect();
    let chain_id = keys[0].fp;
    let mut entries = Vec::new();
    for (i, k) in keys.iter().enumerate() {
        let valid_from = 1_000 + i as i64 * 1_000;
        if i == 0 {
            entries.push(SigningKeyEntry {
                seq: 0,
                public_key: k.pk,
                valid_from_unix: valid_from,
                rotation_signed_by: None,
                rotation_signature: None,
            });
        } else {
            let pred = &keys[i - 1];
            let payload = rotation_payload(&chain_id, i as u32, valid_from, &pred.pk, &k.pk);
            let sig = pred.sk.sign(&payload).to_bytes();
            entries.push(SigningKeyEntry {
                seq: i as u32,
                public_key: k.pk,
                valid_from_unix: valid_from,
                rotation_signed_by: Some(pred.fp),
                rotation_signature: Some(sig),
            });
        }
    }
    let trusted: HashSet<Fingerprint> = [keys[0].fp].into_iter().collect();
    (PluginKeyManifest { keys: entries }, trusted, keys)
}

fn invalid_public_key() -> PublicKeyBytes {
    for byte in u8::MIN..=u8::MAX {
        let candidate = [byte; 32];
        if VerifyingKey::from_bytes(&candidate).is_err() {
            return candidate;
        }
    }
    panic!("expected at least one invalid Ed25519 public key encoding");
}

#[test]
fn valid_single_root_chain_is_trusted() {
    let (m, trusted, keys) = valid_chain(1);
    let vc = validate_chain(&m, &trusted).expect("root-only chain valid");
    assert_eq!(vc.chain_id, keys[0].fp);
    assert_eq!(vc.keys.len(), 1);
    assert_eq!(vc.keys[0].valid_until_unix, None); // head is open-ended
}

#[test]
fn valid_three_key_rotation_is_transitively_trusted() {
    let (m, trusted, keys) = valid_chain(3);
    let vc = validate_chain(&m, &trusted).expect("3-key chain valid");
    assert_eq!(vc.chain_id, keys[0].fp);
    assert_eq!(vc.keys.len(), 3);
    // Interior windows are closed by the successor's valid_from.
    assert_eq!(vc.keys[0].valid_until_unix, Some(2_000));
    assert_eq!(vc.keys[1].valid_until_unix, Some(3_000));
    assert_eq!(vc.keys[2].valid_until_unix, None);
}

#[test]
fn untrusted_root_is_rejected() {
    let (m, _t, _k) = valid_chain(2);
    let empty = HashSet::new();
    assert_eq!(
        validate_chain(&m, &empty).unwrap_err(),
        RotationError::NoTrustAnchor
    );
}

#[test]
fn tampered_rotation_signature_is_rejected() {
    let (mut m, trusted, _k) = valid_chain(2);
    // Flip a byte in the successor's rotation signature.
    let sig = m.keys[1].rotation_signature.as_mut().unwrap();
    sig[0] ^= 0xFF;
    assert_eq!(
        validate_chain(&m, &trusted).unwrap_err(),
        RotationError::BadRotationSignature { seq: 1 }
    );
}

#[test]
fn broken_link_predecessor_mismatch_is_rejected() {
    let (mut m, trusted, _k) = valid_chain(2);
    // Point rotation_signed_by at the wrong fingerprint.
    m.keys[1].rotation_signed_by = Some([0xAB; 32]);
    assert_eq!(
        validate_chain(&m, &trusted).unwrap_err(),
        RotationError::BrokenLink { seq: 1 }
    );
}

#[test]
fn substituted_successor_key_breaks_signature() {
    let (mut m, trusted, _k) = valid_chain(2);
    // Swap in an attacker key while keeping the predecessor's signature:
    // the signed payload bound the original public key, so it must fail.
    let evil = make_key(99);
    m.keys[1].public_key = evil.pk;
    assert_eq!(
        validate_chain(&m, &trusted).unwrap_err(),
        RotationError::BadRotationSignature { seq: 1 }
    );
}

#[test]
fn missing_root_is_rejected() {
    let (mut m, trusted, _k) = valid_chain(2);
    m.keys.remove(0); // drop seq 0
    let err = validate_chain(&m, &trusted).unwrap_err();
    assert!(matches!(
        err,
        RotationError::NoRoot | RotationError::SeqGap { .. }
    ));
}

#[test]
fn multiple_roots_rejected() {
    let (mut m, trusted, _k) = valid_chain(2);
    m.keys[1].seq = 0;
    assert_eq!(
        validate_chain(&m, &trusted).unwrap_err(),
        RotationError::MultipleRoots
    );
}

#[test]
fn seq_gap_rejected() {
    let (mut m, trusted, _k) = valid_chain(3);
    m.keys[2].seq = 5; // 0,1,5
    assert!(matches!(
        validate_chain(&m, &trusted).unwrap_err(),
        RotationError::SeqGap { .. }
    ));
}

#[test]
fn non_monotonic_valid_from_rejected() {
    let (mut m, trusted, _k) = valid_chain(2);
    m.keys[1].valid_from_unix = 500; // < root's 1000
    assert_eq!(
        validate_chain(&m, &trusted).unwrap_err(),
        RotationError::NonMonotonicValidFrom { seq: 1 }
    );
}

#[test]
fn self_signed_non_root_rejected() {
    let (mut m, trusted, keys) = valid_chain(2);
    m.keys[1].rotation_signed_by = Some(keys[1].fp); // points at itself
    assert_eq!(
        validate_chain(&m, &trusted).unwrap_err(),
        RotationError::SelfSignedNonRoot { seq: 1 }
    );
}

#[test]
fn root_with_signature_rejected() {
    let (mut m, trusted, _k) = valid_chain(1);
    m.keys[0].rotation_signature = Some([0u8; 64]);
    assert_eq!(
        validate_chain(&m, &trusted).unwrap_err(),
        RotationError::RootHasSignature
    );
}

#[test]
fn revoked_root_key_rejected() {
    // CRL: a compromised key is invalid for all purposes. A revoked ROOT burns
    // the whole chain.
    let (m, trusted, keys) = valid_chain(3);
    let revoked: HashSet<Fingerprint> = [keys[0].fp].into_iter().collect();
    assert_eq!(
        validate_chain_full(&m, &trusted, &revoked, None).unwrap_err(),
        RotationError::Revoked { seq: 0 }
    );
}

#[test]
fn revoked_interior_key_rejected() {
    // A revoked key anywhere in the chain (here an interior, rotated-away key)
    // invalidates the chain — it must not even be usable as a parent.
    let (m, trusted, keys) = valid_chain(3);
    let revoked: HashSet<Fingerprint> = [keys[1].fp].into_iter().collect();
    assert_eq!(
        validate_chain_full(&m, &trusted, &revoked, None).unwrap_err(),
        RotationError::Revoked { seq: 1 }
    );
}

#[test]
fn revocation_unrelated_key_does_not_affect_chain() {
    // A CRL listing keys not in this chain leaves a valid chain valid.
    let (m, trusted, _keys) = valid_chain(3);
    let stranger = make_key(123);
    let revoked: HashSet<Fingerprint> = [stranger.fp].into_iter().collect();
    assert!(validate_chain_full(&m, &trusted, &revoked, None).is_ok());
}

#[test]
fn empty_manifest_rejected() {
    let m = PluginKeyManifest { keys: vec![] };
    assert_eq!(
        validate_chain(&m, &HashSet::new()).unwrap_err(),
        RotationError::Empty
    );
}

#[test]
fn over_length_chain_rejected_before_crypto() {
    // Council R-high: an oversized manifest is a CPU-exhaustion (DoS) vector —
    // it must be rejected up front, before any signature verification. The
    // entries need not be valid; the length check fires first.
    let n = MAX_CHAIN_LENGTH + 1;
    let keys: Vec<SigningKeyEntry> = (0..n)
        .map(|i| SigningKeyEntry {
            seq: i as u32,
            public_key: [i as u8; 32],
            valid_from_unix: 1_000 + i as i64,
            rotation_signed_by: None,
            rotation_signature: None,
        })
        .collect();
    let m = PluginKeyManifest { keys };
    assert_eq!(
        validate_chain(&m, &HashSet::new()).unwrap_err(),
        RotationError::ChainTooLong { len: n }
    );
}

#[test]
fn invalid_root_public_key_rejected() {
    let (mut m, _trusted, _k) = valid_chain(1);
    let bad = invalid_public_key();
    m.keys[0].public_key = bad;
    // Trust the (malformed) root's fingerprint so validation passes the trust
    // gate and reaches point validation — otherwise the changed fingerprint
    // would (correctly) trip NoTrustAnchor first under the cheap-gates-first
    // ordering.
    let trusted: HashSet<Fingerprint> = [fingerprint_of(&bad)].into_iter().collect();
    assert_eq!(
        validate_chain(&m, &trusted).unwrap_err(),
        RotationError::InvalidPublicKey { seq: 0 }
    );
}

#[test]
fn invalid_head_public_key_rejected() {
    let (mut m, trusted, _k) = valid_chain(2);
    m.keys[1].public_key = invalid_public_key();
    assert_eq!(
        validate_chain(&m, &trusted).unwrap_err(),
        RotationError::InvalidPublicKey { seq: 1 }
    );
}

#[test]
fn duplicate_public_key_in_chain_rejected() {
    // Council R-high: a key reused LATER in the chain (non-adjacent, so it
    // evades the self-signed check) creates an ambiguous active window —
    // resolve_signer's `.find()` would match the earlier instance and
    // falsely reject a legitimate later signature. Forbid reuse outright.
    let (mut m, trusted, keys) = valid_chain(3);
    let chain_id = keys[0].fp;
    let vf = m.keys[2].valid_from_unix;
    // seq 2 reuses the ROOT (seq 0) public key; re-sign by seq 2's real
    // predecessor (seq 1) so the rotation signature itself stays valid,
    // isolating the duplicate-key rule from the signature check.
    let payload = rotation_payload(&chain_id, 2, vf, &keys[1].pk, &keys[0].pk);
    m.keys[2].public_key = keys[0].pk;
    m.keys[2].rotation_signed_by = Some(keys[1].fp);
    m.keys[2].rotation_signature = Some(keys[1].sk.sign(&payload).to_bytes());
    assert_eq!(
        validate_chain(&m, &trusted).unwrap_err(),
        RotationError::DuplicateKey { seq: 2 }
    );
}

#[test]
fn truncated_chain_rejected_by_high_water_mark() {
    // Council R-high: an attacker replays a valid historical PREFIX (fewer
    // entries → lower head seq) to resurrect a rotated-away key. The host's
    // persisted high-water mark catches it.
    let (full, trusted, _k) = valid_chain(3); // head seq 2
    let truncated = PluginKeyManifest {
        keys: full.keys[..2].to_vec(), // valid 2-key prefix, head seq 1
    };
    // First contact (no mark) accepts it.
    assert!(validate_chain_pinned(&truncated, &trusted, None).is_ok());
    // But once seq 2 has been seen, the rollback is rejected.
    assert_eq!(
        validate_chain_pinned(&truncated, &trusted, Some(2)).unwrap_err(),
        RotationError::Rollback {
            pinned_max_seq: 2,
            chain_max_seq: 1
        }
    );
}

#[test]
fn current_or_newer_chain_accepted_against_mark() {
    let (full, trusted, _k) = valid_chain(3); // head seq 2
    let vc = validate_chain_pinned(&full, &trusted, Some(2)).expect("equal mark ok");
    assert_eq!(vc.head_seq(), 2);
    // A chain newer than the mark is fine too.
    assert!(validate_chain_pinned(&full, &trusted, Some(1)).is_ok());
}

#[test]
fn resolve_signer_accepts_head_key_in_window() {
    let (m, trusted, keys) = valid_chain(3);
    let vc = validate_chain(&m, &trusted).unwrap();
    // Head key (seq 2) active from 3000, open-ended.
    assert!(vc.resolve_signer(&keys[2].pk, 5_000, 1_000_000).is_ok());
}

#[test]
fn resolve_signer_accepts_interior_key_in_its_window() {
    let (m, trusted, keys) = valid_chain(3);
    let vc = validate_chain(&m, &trusted).unwrap();
    // Interior key seq 1 active [2000, 3000).
    assert!(vc.resolve_signer(&keys[1].pk, 2_500, 1_000_000).is_ok());
}

#[test]
fn resolve_signer_rejects_signature_after_window_closed() {
    let (m, trusted, keys) = valid_chain(3);
    let vc = validate_chain(&m, &trusted).unwrap();
    // Key seq 1's window closed at 3000; a signature at 4000 is invalid.
    assert_eq!(
        vc.resolve_signer(&keys[1].pk, 4_000, 1_000_000)
            .unwrap_err(),
        RotationError::SignedOutsideWindow
    );
}

#[test]
fn resolve_signer_rejects_signed_before_root_valid_from() {
    let (m, trusted, keys) = valid_chain(1);
    let vc = validate_chain(&m, &trusted).unwrap();
    // Root valid_from 1000; a signature at 500 predates it.
    assert_eq!(
        vc.resolve_signer(&keys[0].pk, 500, 1_000_000).unwrap_err(),
        RotationError::SignedOutsideWindow
    );
}

#[test]
fn resolve_signer_rejects_key_not_in_chain() {
    let (m, trusted, _k) = valid_chain(2);
    let vc = validate_chain(&m, &trusted).unwrap();
    let stranger = make_key(200);
    assert_eq!(
        vc.resolve_signer(&stranger.pk, 5_000, 1_000_000)
            .unwrap_err(),
        RotationError::SignerNotInChain
    );
}

#[test]
fn resolve_signer_rejects_future_signature() {
    // Council R-high: a signature claimed in the future (signed_at > now) is
    // bogus and must be rejected even when it lands in an active window.
    let (m, trusted, keys) = valid_chain(3);
    let vc = validate_chain(&m, &trusted).unwrap();
    assert_eq!(
        vc.resolve_signer(&keys[2].pk, 5_000, 4_000).unwrap_err(),
        RotationError::SignedInFuture
    );
}

#[test]
fn oversized_manifest_json_rejected_before_parse() {
    // Council R-high: cap the byte size before serde_json allocates (DoS).
    let big = format!(
        "{{ \"signing_keys\": [] , \"pad\": \"{}\" }}",
        "A".repeat(MAX_MANIFEST_BYTES)
    );
    assert!(matches!(
        PluginKeyManifestJson::parse(&big).unwrap_err(),
        ManifestParseError::TooLarge { .. }
    ));
}

#[test]
fn too_many_keys_manifest_rejected() {
    // A manifest declaring more than MAX_CHAIN_LENGTH keys is rejected at the
    // transport layer, before any decode / crypto work.
    let entry = r#"{ "seq": 0, "public_key": "00", "valid_from": "2026-01-01T00:00:00Z" }"#;
    let entries = std::iter::repeat_n(entry, MAX_CHAIN_LENGTH + 1)
        .collect::<Vec<_>>()
        .join(", ");
    let json = format!("{{ \"signing_keys\": [ {} ] }}", entries);
    assert!(matches!(
        PluginKeyManifestJson::parse(&json).unwrap_err(),
        ManifestParseError::TooManyKeys { count } if count == MAX_CHAIN_LENGTH + 1
    ));
}

/// Render an engine manifest back to the JSON transport shape (hex + ISO).
fn manifest_to_json(m: &PluginKeyManifest) -> String {
    let to_iso = |unix: i64| {
        chrono::DateTime::from_timestamp(unix, 0)
            .unwrap()
            .to_rfc3339()
    };
    let entries: Vec<String> = m
        .keys
        .iter()
        .map(|e| {
            let mut fields = vec![
                format!("\"seq\": {}", e.seq),
                format!("\"public_key\": \"{}\"", hex::encode(e.public_key)),
                format!("\"valid_from\": \"{}\"", to_iso(e.valid_from_unix)),
            ];
            if let Some(p) = e.rotation_signed_by {
                fields.push(format!("\"rotation_signed_by\": \"{}\"", hex::encode(p)));
            }
            if let Some(s) = e.rotation_signature {
                fields.push(format!("\"rotation_signature\": \"{}\"", hex::encode(s)));
            }
            format!("{{ {} }}", fields.join(", "))
        })
        .collect();
    format!("{{ \"signing_keys\": [ {} ] }}", entries.join(", "))
}

#[test]
fn json_manifest_round_trips_and_validates() {
    let (m, trusted, keys) = valid_chain(3);
    let json = manifest_to_json(&m);
    let parsed = PluginKeyManifestJson::parse(&json)
        .unwrap()
        .into_manifest()
        .unwrap();
    assert_eq!(parsed, m); // transport decode is lossless
    let vc = validate_chain(&parsed, &trusted).expect("validates from JSON");
    assert_eq!(vc.chain_id, keys[0].fp);
}

#[test]
fn json_manifest_bad_hex_public_key_rejected() {
    let json = r#"{ "signing_keys": [ { "seq": 0, "public_key": "zz", "valid_from": "2026-01-01T00:00:00Z" } ] }"#;
    let err = PluginKeyManifestJson::parse(json)
        .unwrap()
        .into_manifest()
        .unwrap_err();
    assert_eq!(
        err,
        ManifestParseError::BadHex {
            field: "public_key",
            seq: 0
        }
    );
}

#[test]
fn json_manifest_wrong_key_length_rejected() {
    // 2 bytes of valid hex, but a public key must be 32.
    let json = r#"{ "signing_keys": [ { "seq": 0, "public_key": "abcd", "valid_from": "2026-01-01T00:00:00Z" } ] }"#;
    let err = PluginKeyManifestJson::parse(json)
        .unwrap()
        .into_manifest()
        .unwrap_err();
    assert!(matches!(
        err,
        ManifestParseError::BadLength {
            field: "public_key",
            expected: 32,
            got: 2,
            ..
        }
    ));
}

#[test]
fn json_manifest_bad_timestamp_rejected() {
    let pk = hex::encode([1u8; 32]);
    let json = format!(
        r#"{{ "signing_keys": [ {{ "seq": 0, "public_key": "{}", "valid_from": "not-a-date" }} ] }}"#,
        pk
    );
    let err = PluginKeyManifestJson::parse(&json)
        .unwrap()
        .into_manifest()
        .unwrap_err();
    assert_eq!(err, ManifestParseError::BadTimestamp { seq: 0 });
}

// ── active_key_at: window selection on a verified chain ─────────────────────
// valid_chain(n) places key i at valid_from = 1000 + i*1000, so for a 3-key
// chain the windows are [1000,2000), [2000,3000), [3000,∞).

#[test]
fn active_key_at_selects_the_window_containing_now() {
    let (m, trusted, keys) = valid_chain(3);
    let vc = validate_chain(&m, &trusted).expect("3-key chain valid");
    assert_eq!(
        vc.active_key_at(1_500).map(|k| k.public_key),
        Some(keys[0].pk)
    );
    assert_eq!(
        vc.active_key_at(2_500).map(|k| k.public_key),
        Some(keys[1].pk)
    );
    assert_eq!(
        vc.active_key_at(3_500).map(|k| k.public_key),
        Some(keys[2].pk)
    );
    // Boundary: valid_from is inclusive, valid_until exclusive.
    assert_eq!(
        vc.active_key_at(2_000).map(|k| k.public_key),
        Some(keys[1].pk)
    );
    // Before the root is active: no key.
    assert_eq!(vc.active_key_at(500), None);
}

// ── resolve_active_signing_key: load-path trust resolution ──────────────────
// The loader hands a `.keys.json` manifest string + the user's directly-trusted
// root fingerprints + the verifier's clock; the helper validates the chain and
// returns the single hex public key active *now*. Only the active key — never a
// rotated-away-from predecessor — is returned. Any failure is a hard fail.

#[test]
fn resolve_active_key_root_only_returns_root() {
    let (m, trusted, keys) = valid_chain(1);
    let json = manifest_to_json(&m);
    let resolved = resolve_active_signing_key(&json, &trusted, &HashSet::new(), None, 1_500)
        .expect("root-only chain resolves");
    assert_eq!(resolved, hex::encode(keys[0].pk));
}

#[test]
fn resolve_active_key_transitive_trust_returns_head_not_predecessors() {
    // Trust only the ROOT; the head was never directly trusted by the user.
    // After rotation (now past the head's valid_from) ONLY the head is the
    // active signer — predecessor keys are deliberately NOT accepted, which is
    // what closes the compromised-predecessor-key hole.
    let (m, trusted, keys) = valid_chain(3);
    let json = manifest_to_json(&m);
    let resolved = resolve_active_signing_key(&json, &trusted, &HashSet::new(), None, 5_000)
        .expect("rotated chain resolves transitively from a trusted root");
    assert_eq!(resolved, hex::encode(keys[2].pk)); // head
    assert_ne!(resolved, hex::encode(keys[0].pk)); // root predecessor rejected
    assert_ne!(resolved, hex::encode(keys[1].pk)); // interior predecessor rejected
}

#[test]
fn resolve_active_key_binds_to_window_not_blind_head() {
    // When `now` falls in an interior key's window, that interior key — not the
    // head — is the active signer.
    let (m, trusted, keys) = valid_chain(3);
    let json = manifest_to_json(&m);
    let resolved = resolve_active_signing_key(&json, &trusted, &HashSet::new(), None, 2_500)
        .expect("interior-window chain resolves");
    assert_eq!(resolved, hex::encode(keys[1].pk));
}

#[test]
fn resolve_active_key_untrusted_root_hard_fails() {
    let (m, _trusted, _keys) = valid_chain(2);
    let json = manifest_to_json(&m);
    // Empty trust set: nothing roots the chain.
    let err = resolve_active_signing_key(&json, &HashSet::new(), &HashSet::new(), None, 5_000)
        .expect_err("an untrusted root must hard-fail");
    assert!(matches!(
        err,
        ManifestTrustError::Rotation(RotationError::NoTrustAnchor)
    ));
}

#[test]
fn resolve_active_key_revoked_in_chain_hard_fails() {
    let (m, trusted, keys) = valid_chain(3);
    let json = manifest_to_json(&m);
    let revoked: HashSet<Fingerprint> = [keys[1].fp].into_iter().collect();
    let err = resolve_active_signing_key(&json, &trusted, &revoked, None, 5_000)
        .expect_err("a revoked key in the chain must hard-fail");
    assert!(matches!(
        err,
        ManifestTrustError::Rotation(RotationError::Revoked { .. })
    ));
}

#[test]
fn resolve_active_key_rollback_below_pin_hard_fails() {
    let (m, trusted, _keys) = valid_chain(2); // head seq = 1
    let json = manifest_to_json(&m);
    // We have already accepted seq 5; replaying this older chain is a rollback.
    let err = resolve_active_signing_key(&json, &trusted, &HashSet::new(), Some(5), 5_000)
        .expect_err("a chain head below the high-water mark must hard-fail");
    assert!(matches!(
        err,
        ManifestTrustError::Rotation(RotationError::Rollback { .. })
    ));
}

#[test]
fn resolve_active_key_before_root_valid_from_no_active() {
    // The chain validates, but `now` predates the root's valid_from — no key is
    // active yet, so there is no legitimate signer. Hard fail (not a fall-back
    // to the head).
    let (m, trusted, _keys) = valid_chain(2);
    let json = manifest_to_json(&m);
    let err = resolve_active_signing_key(&json, &trusted, &HashSet::new(), None, 500)
        .expect_err("a not-yet-active chain must hard-fail");
    assert!(matches!(
        err,
        ManifestTrustError::NoActiveKey { now_unix: 500 }
    ));
}

#[test]
fn resolve_active_key_malformed_json_hard_fails() {
    let err = resolve_active_signing_key("not json", &HashSet::new(), &HashSet::new(), None, 5_000)
        .expect_err("malformed manifest JSON must hard-fail");
    assert!(matches!(err, ManifestTrustError::Parse(_)));
}
