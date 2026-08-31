// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-027 §D10d-source — unit tests for registry document trust.

use super::*;
use ed25519_dalek::{Signer, SigningKey};

const KEY_ID: &str = "conductor-registry-v1";

fn signing_key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

/// Build a registry payload JSON with the given rollback header fields.
fn payload(seq: u64, published_at: &str) -> String {
    format!(
        r#"{{"version":"1","last_updated":"2026-05-27","sequence_number":{seq},"published_at":"{published_at}","plugins":[],"categories":[]}}"#
    )
}

/// Build a signed envelope JSON for `payload`, signed by `key` under `key_id`.
fn envelope(key: &SigningKey, payload: &str, key_id: &str) -> String {
    let sig = key.sign(&registry_signed_message(key_id, payload));
    let env = SignedRegistryEnvelope {
        payload: payload.to_string(),
        signature: hex::encode(sig.to_bytes()),
        key_id: key_id.to_string(),
        key_manifest: None,
    };
    serde_json::to_string(&env).unwrap()
}

// ───────────────────────── signature ─────────────────────────

#[test]
fn valid_signature_accepted_and_state_advances() {
    let key = signing_key(7);
    let raw = envelope(&key, &payload(5, "2026-05-27T00:00:00Z"), KEY_ID);
    let v = verify_signed_registry(
        &raw,
        &key.verifying_key(),
        KEY_ID,
        &RegistryTrustState::default(),
    )
    .expect("valid signed registry must verify");
    assert_eq!(v.new_state.last_sequence_number, 5);
    assert_eq!(
        v.new_state.last_published_at.as_deref(),
        Some("2026-05-27T00:00:00Z")
    );
    assert_eq!(v.new_state.key_id.as_deref(), Some(KEY_ID));
    // The trusted payload round-trips back to the registry doc.
    assert!(v.payload.contains("\"plugins\""));
}

#[test]
fn tampered_payload_rejected() {
    let key = signing_key(7);
    let raw = envelope(&key, &payload(5, "2026-05-27T00:00:00Z"), KEY_ID);
    // Corrupt a byte inside the (escaped) payload AFTER signing — `plugins`
    // appears only in the payload, so the signature must no longer verify.
    let tampered = raw.replace("plugins", "pluginz");
    assert_ne!(tampered, raw, "tamper must change the bytes");
    let err = verify_signed_registry(
        &tampered,
        &key.verifying_key(),
        KEY_ID,
        &RegistryTrustState::default(),
    )
    .unwrap_err();
    assert_eq!(err, RegistryTrustError::SignatureInvalid);
}

#[test]
fn wrong_key_rejected() {
    let signer = signing_key(7);
    let attacker_view = signing_key(9).verifying_key();
    let raw = envelope(&signer, &payload(5, "2026-05-27T00:00:00Z"), KEY_ID);
    let err = verify_signed_registry(&raw, &attacker_view, KEY_ID, &RegistryTrustState::default())
        .unwrap_err();
    assert_eq!(err, RegistryTrustError::SignatureInvalid);
}

#[test]
fn key_id_mismatch_rejected_before_crypto() {
    let key = signing_key(7);
    let raw = envelope(&key, &payload(5, "2026-05-27T00:00:00Z"), "rogue-key");
    let err = verify_signed_registry(
        &raw,
        &key.verifying_key(),
        KEY_ID,
        &RegistryTrustState::default(),
    )
    .unwrap_err();
    assert!(matches!(err, RegistryTrustError::KeyIdMismatch { .. }));
}

#[test]
fn bad_signature_encoding_rejected() {
    let key = signing_key(7);
    let p = payload(5, "2026-05-27T00:00:00Z");
    let env = SignedRegistryEnvelope {
        payload: p,
        signature: "not-hex!!".to_string(),
        key_id: KEY_ID.to_string(),
        key_manifest: None,
    };
    let raw = serde_json::to_string(&env).unwrap();
    let err = verify_signed_registry(
        &raw,
        &key.verifying_key(),
        KEY_ID,
        &RegistryTrustState::default(),
    )
    .unwrap_err();
    assert!(matches!(err, RegistryTrustError::BadSignatureEncoding(_)));
}

#[test]
fn non_envelope_rejected() {
    let key = signing_key(7);
    let err = verify_signed_registry(
        r#"{"version":"1","plugins":[]}"#,
        &key.verifying_key(),
        KEY_ID,
        &RegistryTrustState::default(),
    )
    .unwrap_err();
    assert_eq!(err, RegistryTrustError::NotAnEnvelope);
}

// ───────────────────────── rollback / replay ─────────────────────────

#[test]
fn replay_same_sequence_rejected() {
    let key = signing_key(7);
    let raw = envelope(&key, &payload(5, "2026-05-27T00:00:00Z"), KEY_ID);
    // Already accepted sequence 5.
    let state = RegistryTrustState {
        key_id: Some(KEY_ID.to_string()),
        last_sequence_number: 5,
        last_published_at: Some("2026-05-27T00:00:00Z".to_string()),
        last_chain_head_seq: None,
        root_override: None,
    };
    let err = verify_signed_registry(&raw, &key.verifying_key(), KEY_ID, &state).unwrap_err();
    assert_eq!(
        err,
        RegistryTrustError::RollbackSequence { last: 5, got: 5 }
    );
}

#[test]
fn older_sequence_rejected() {
    let key = signing_key(7);
    let raw = envelope(&key, &payload(3, "2026-05-20T00:00:00Z"), KEY_ID);
    let state = RegistryTrustState {
        key_id: Some(KEY_ID.to_string()),
        last_sequence_number: 5,
        last_published_at: Some("2026-05-27T00:00:00Z".to_string()),
        last_chain_head_seq: None,
        root_override: None,
    };
    let err = verify_signed_registry(&raw, &key.verifying_key(), KEY_ID, &state).unwrap_err();
    assert!(matches!(
        err,
        RegistryTrustError::RollbackSequence { last: 5, got: 3 }
    ));
}

#[test]
fn newer_sequence_accepted() {
    let key = signing_key(7);
    let raw = envelope(&key, &payload(6, "2026-05-28T00:00:00Z"), KEY_ID);
    let state = RegistryTrustState {
        key_id: Some(KEY_ID.to_string()),
        last_sequence_number: 5,
        last_published_at: Some("2026-05-27T00:00:00Z".to_string()),
        last_chain_head_seq: None,
        root_override: None,
    };
    let v = verify_signed_registry(&raw, &key.verifying_key(), KEY_ID, &state).unwrap();
    assert_eq!(v.new_state.last_sequence_number, 6);
}

#[test]
fn published_at_backwards_rejected() {
    let key = signing_key(7);
    // Sequence advances but published_at goes backwards — still a rollback.
    let raw = envelope(&key, &payload(6, "2026-05-01T00:00:00Z"), KEY_ID);
    let state = RegistryTrustState {
        key_id: Some(KEY_ID.to_string()),
        last_sequence_number: 5,
        last_published_at: Some("2026-05-27T00:00:00Z".to_string()),
        last_chain_head_seq: None,
        root_override: None,
    };
    let err = verify_signed_registry(&raw, &key.verifying_key(), KEY_ID, &state).unwrap_err();
    assert!(matches!(
        err,
        RegistryTrustError::RollbackPublishedAt { .. }
    ));
}

#[test]
fn published_at_required_when_previously_seen() {
    // A later doc that OMITS published_at is rejected — otherwise an attacker
    // drops the temporal guard by leaving it out while advancing the sequence
    // (Council R3).
    let key = signing_key(7);
    let p = r#"{"version":"1","sequence_number":6,"plugins":[],"categories":[]}"#.to_string();
    let raw = envelope(&key, &p, KEY_ID);
    let state = RegistryTrustState {
        key_id: Some(KEY_ID.to_string()),
        last_sequence_number: 5,
        last_published_at: Some("2026-05-27T00:00:00Z".to_string()),
        last_chain_head_seq: None,
        root_override: None,
    };
    let err = verify_signed_registry(&raw, &key.verifying_key(), KEY_ID, &state).unwrap_err();
    assert_eq!(err, RegistryTrustError::MissingPublishedAt);
}

#[test]
fn published_at_required_even_on_first_fetch() {
    // published_at is mandatory ALWAYS — a first document that omits it is
    // rejected, so the temporal guard can never be permanently disabled by a
    // document that simply never carries the field (Council R4).
    let key = signing_key(7);
    let p = r#"{"version":"1","sequence_number":1,"plugins":[],"categories":[]}"#.to_string();
    let raw = envelope(&key, &p, KEY_ID);
    let err = verify_signed_registry(
        &raw,
        &key.verifying_key(),
        KEY_ID,
        &RegistryTrustState::default(),
    )
    .unwrap_err();
    assert_eq!(err, RegistryTrustError::MissingPublishedAt);
}

#[test]
fn is_signed_envelope_rejects_oversized_without_parsing() {
    let big = format!(
        "{{\"payload\":\"{}\",\"signature\":\"ab\",\"key_id\":\"x\"}}",
        "y".repeat(MAX_REGISTRY_DOC_BYTES)
    );
    assert!(!is_signed_envelope(&big));
}

#[test]
fn first_fetch_accepts_positive_sequence_then_rejects_replay() {
    let key = signing_key(7);
    let raw = envelope(&key, &payload(1, "2026-05-27T00:00:00Z"), KEY_ID);
    let v = verify_signed_registry(
        &raw,
        &key.verifying_key(),
        KEY_ID,
        &RegistryTrustState::default(),
    )
    .expect("first fetch with a positive sequence establishes trust");
    assert_eq!(v.new_state.last_sequence_number, 1);
    // A replay of that same first document is then rejected.
    let err = verify_signed_registry(&raw, &key.verifying_key(), KEY_ID, &v.new_state).unwrap_err();
    assert!(matches!(err, RegistryTrustError::RollbackSequence { .. }));
}

#[test]
fn zero_sequence_rejected() {
    // sequence_number is 1-indexed: an explicit `0` is rejected (it must be
    // present and >= 1, so it can't slip through the cache `>=` check; Council
    // R1 + #2049).
    let key = signing_key(7);
    let raw = envelope(&key, &payload(0, "2026-05-27T00:00:00Z"), KEY_ID);
    let err = verify_signed_registry(
        &raw,
        &key.verifying_key(),
        KEY_ID,
        &RegistryTrustState::default(),
    )
    .unwrap_err();
    assert_eq!(err, RegistryTrustError::MissingSequenceNumber);
}

#[test]
fn absent_sequence_rejected_on_both_paths() {
    // A document OMITTING sequence_number must be rejected (not defaulted to 0)
    // on BOTH the fetch and cache paths (Council #2049).
    let key = signing_key(7);
    let p = r#"{"version":"1","published_at":"2026-05-27T00:00:00Z","plugins":[],"categories":[]}"#;
    let raw = envelope(&key, p, KEY_ID);
    let fetch_err = verify_signed_registry(
        &raw,
        &key.verifying_key(),
        KEY_ID,
        &RegistryTrustState::default(),
    )
    .unwrap_err();
    assert_eq!(fetch_err, RegistryTrustError::MissingSequenceNumber);
    let cache_err = verify_registry_integrity(
        &raw,
        &key.verifying_key(),
        KEY_ID,
        &RegistryTrustState::default(),
    )
    .unwrap_err();
    assert_eq!(cache_err, RegistryTrustError::MissingSequenceNumber);
}

// ───────────── decide_fetch / decide_cache — anti-downgrade (Council R1) ─────────────

#[test]
fn decide_fetch_with_pin_verifies() {
    let key = signing_key(7);
    let raw = envelope(&key, &payload(3, "2026-05-27T00:00:00Z"), KEY_ID);
    let vk = key.verifying_key();
    match decide_fetch(&raw, Some(&vk), KEY_ID, &RegistryTrustState::default()).unwrap() {
        FetchDecision::Verified(v) => assert_eq!(v.new_state.last_sequence_number, 3),
        other => panic!("expected Verified, got {other:?}"),
    }
}

#[test]
fn decide_fetch_with_pin_rejects_stripped_signature() {
    // Signature-stripping downgrade: an attacker serves the bare inner document
    // (no envelope). With a key pinned, this MUST be rejected — not silently
    // accepted as unsigned.
    let key = signing_key(7);
    let bare = payload(3, "2026-05-27T00:00:00Z"); // no envelope around it
    let vk = key.verifying_key();
    let err = decide_fetch(&bare, Some(&vk), KEY_ID, &RegistryTrustState::default()).unwrap_err();
    assert_eq!(err, RegistryTrustError::NotAnEnvelope);
}

#[test]
fn decide_fetch_without_pin_is_unverified() {
    // No trust anchor (Phase-1): the document is accepted but explicitly
    // unverified. There is nothing to "strip" because there is no pin.
    let key = signing_key(7);
    let raw = envelope(&key, &payload(3, "2026-05-27T00:00:00Z"), KEY_ID);
    match decide_fetch(&raw, None, KEY_ID, &RegistryTrustState::default()).unwrap() {
        FetchDecision::UnverifiedNoPin { payload } => assert!(payload.contains("\"plugins\"")),
        other => panic!("expected UnverifiedNoPin, got {other:?}"),
    }
}

#[test]
fn decide_cache_with_pin_rejects_stripped_signature() {
    let key = signing_key(7);
    let bare = payload(3, "2026-05-27T00:00:00Z");
    let vk = key.verifying_key();
    let err = decide_cache(&bare, Some(&vk), KEY_ID, &RegistryTrustState::default()).unwrap_err();
    assert_eq!(err, RegistryTrustError::NotAnEnvelope);
}

#[test]
fn decide_cache_without_pin_is_unverified() {
    let key = signing_key(7);
    let raw = envelope(&key, &payload(3, "2026-05-27T00:00:00Z"), KEY_ID);
    let outcome = decide_cache(&raw, None, KEY_ID, &RegistryTrustState::default()).unwrap();
    assert!(!outcome.verified);
    assert!(outcome.payload.contains("\"plugins\""));
}

// ───────────────────────── cache integrity ─────────────────────────

#[test]
fn cache_integrity_accepts_last_seen_without_rollback() {
    let key = signing_key(7);
    let p = payload(5, "2026-05-27T00:00:00Z");
    let raw = envelope(&key, &p, KEY_ID);
    // Cache holds sequence 5 and state is already at 5 — integrity check must
    // NOT apply the monotonic guard (it would falsely reject the cache).
    let trusted = verify_registry_integrity(
        &raw,
        &key.verifying_key(),
        KEY_ID,
        &RegistryTrustState::default(),
    )
    .expect("cached doc with valid signature must pass integrity check");
    assert_eq!(trusted, p);
}

#[test]
fn cache_integrity_still_rejects_tampering() {
    let key = signing_key(7);
    let raw = envelope(&key, &payload(5, "2026-05-27T00:00:00Z"), KEY_ID);
    let tampered = raw.replace("plugins", "pluginz");
    assert_ne!(tampered, raw, "tamper must change the bytes");
    let err = verify_registry_integrity(
        &tampered,
        &key.verifying_key(),
        KEY_ID,
        &RegistryTrustState::default(),
    )
    .unwrap_err();
    assert_eq!(err, RegistryTrustError::SignatureInvalid);
}

// ───────────────────────── helpers ─────────────────────────

#[test]
fn is_signed_envelope_detects_signed_vs_bare() {
    let key = signing_key(7);
    let signed = envelope(&key, &payload(1, "2026-05-27T00:00:00Z"), KEY_ID);
    assert!(is_signed_envelope(&signed));
    assert!(!is_signed_envelope(
        r#"{"version":"1","plugins":[],"categories":[]}"#
    ));
    // An envelope with an empty signature is not "signed".
    assert!(!is_signed_envelope(
        r#"{"payload":"{}","signature":"","key_id":"x"}"#
    ));
}

#[test]
fn signed_message_binds_domain_keyid_and_payload() {
    // The signed bytes are DOMAIN || key_id || NUL || payload — domain-prefixed,
    // key_id-bound, and payload-bound, with no application-level pre-hash.
    let m = registry_signed_message(KEY_ID, "{\"a\":1}");
    assert!(m.starts_with(b"conductor-registry-document-v1\0"));
    assert!(m.ends_with(b"{\"a\":1}"));
    assert!(m.windows(KEY_ID.len()).any(|w| w == KEY_ID.as_bytes()));
    // Changing key_id changes the signed bytes (binding).
    assert_ne!(
        registry_signed_message(KEY_ID, "{\"a\":1}"),
        registry_signed_message("other-key", "{\"a\":1}")
    );
    // Changing payload changes the signed bytes.
    assert_ne!(
        registry_signed_message(KEY_ID, "{\"a\":1}"),
        registry_signed_message(KEY_ID, "{\"a\":2}")
    );
}

#[test]
fn swapped_key_id_breaks_signature() {
    // A signature minted for key_id A, replayed inside an envelope claiming
    // key_id == pinned (A here) but verified — fine. But if the signer used a
    // DIFFERENT key_id than the envelope advertises, the bound message differs
    // and verification fails. Simulate: sign with the wrong key_id.
    let key = signing_key(7);
    let payload = payload(3, "2026-05-27T00:00:00Z");
    let sig = key.sign(&registry_signed_message("attacker-chosen", &payload));
    let env = SignedRegistryEnvelope {
        payload,
        signature: hex::encode(sig.to_bytes()),
        key_id: KEY_ID.to_string(), // claims the pinned id, but signed another
        key_manifest: None,
    };
    let raw = serde_json::to_string(&env).unwrap();
    let err = verify_signed_registry(
        &raw,
        &key.verifying_key(),
        KEY_ID,
        &RegistryTrustState::default(),
    )
    .unwrap_err();
    assert_eq!(err, RegistryTrustError::SignatureInvalid);
}

#[test]
fn oversized_document_rejected_before_parse() {
    // A document over the cap is refused without parsing/verifying (DoS bound).
    let big = format!("{{\"payload\":\"{}\"}}", "x".repeat(MAX_REGISTRY_DOC_BYTES));
    let key = signing_key(7);
    let err = verify_signed_registry(
        &big,
        &key.verifying_key(),
        KEY_ID,
        &RegistryTrustState::default(),
    )
    .unwrap_err();
    assert!(matches!(err, RegistryTrustError::DocumentTooLarge { .. }));
}

#[test]
fn unparseable_published_at_rejected() {
    // A malformed published_at must be a hard error, never a silently-"older"
    // string that bypasses the rollback guard.
    let key = signing_key(7);
    let raw = envelope(&key, &payload(6, "not-a-timestamp"), KEY_ID);
    let state = RegistryTrustState {
        key_id: Some(KEY_ID.to_string()),
        last_sequence_number: 5,
        last_published_at: Some("2026-05-27T00:00:00Z".to_string()),
        last_chain_head_seq: None,
        root_override: None,
    };
    let err = verify_signed_registry(&raw, &key.verifying_key(), KEY_ID, &state).unwrap_err();
    assert!(matches!(err, RegistryTrustError::BadTimestamp(_)));
}

#[test]
fn equivalent_timestamps_in_different_forms_not_false_rollback() {
    // `+00:00` and `Z` are the SAME instant; a lexical compare would mis-order
    // them. The parsed-instant compare must accept the advance.
    let key = signing_key(7);
    // Same instant as last, higher sequence, written with a numeric offset.
    let raw = envelope(&key, &payload(6, "2026-05-27T00:00:00+00:00"), KEY_ID);
    let state = RegistryTrustState {
        key_id: Some(KEY_ID.to_string()),
        last_sequence_number: 5,
        last_published_at: Some("2026-05-27T00:00:00Z".to_string()),
        last_chain_head_seq: None,
        root_override: None,
    };
    let v = verify_signed_registry(&raw, &key.verifying_key(), KEY_ID, &state)
        .expect("same instant in +00:00 form must not be a false rollback");
    assert_eq!(v.new_state.last_sequence_number, 6);
}

#[test]
fn pinned_key_absent_in_phase1_placeholder() {
    // Phase-1 ships an empty pinned key; production therefore treats documents
    // as unsigned (migration) until the signing-infra follow-up bakes the key.
    assert!(pinned_registry_key().is_none());
    assert_eq!(REGISTRY_PINNED_KEY_ID, "conductor-registry-v1");
}

// ───────────── key rotation (ADR-027 D10d-source; reuses §D9 chain) ─────────────

use crate::plugin::key_rotation::{fingerprint_of, rotation_payload};

/// Unix seconds for an RFC 3339 instant (so the signed `valid_from_unix`
/// matches the manifest's textual `valid_from` exactly).
fn unix(rfc3339: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .unwrap()
        .timestamp()
}

/// Build a 2-key rotation manifest JSON: `root` (seq 0) endorsing `rotated`
/// (seq 1). The endorsement signature is by `root` over the §D9
/// `rotation_payload`, so the chain validates against a pin on `root`.
fn rotation_manifest(root: &SigningKey, rotated: &SigningKey) -> String {
    let root_pub = root.verifying_key().to_bytes();
    let rotated_pub = rotated.verifying_key().to_bytes();
    let chain_id = fingerprint_of(&root_pub);
    let rotated_valid = "2026-06-01T00:00:00Z";
    let payload = rotation_payload(&chain_id, 1, unix(rotated_valid), &root_pub, &rotated_pub);
    let sig = root.sign(&payload).to_bytes();
    format!(
        concat!(
            r#"{{"signing_keys":["#,
            r#"{{"seq":0,"public_key":"{root}","valid_from":"2026-01-01T00:00:00Z"}},"#,
            r#"{{"seq":1,"public_key":"{rot}","valid_from":"{rv}","#,
            r#""rotation_signed_by":"{rfp}","rotation_signature":"{rsig}"}}"#,
            r#"]}}"#
        ),
        root = hex::encode(root_pub),
        rot = hex::encode(rotated_pub),
        rv = rotated_valid,
        rfp = hex::encode(fingerprint_of(&root_pub)),
        rsig = hex::encode(sig),
    )
}

/// Build a signed envelope whose document is signed by `signer`, carrying
/// `manifest` as the rotation chain.
fn rotated_envelope(signer: &SigningKey, payload: &str, manifest: &str) -> String {
    let sig = signer.sign(&registry_signed_message(KEY_ID, payload));
    let env = SignedRegistryEnvelope {
        payload: payload.to_string(),
        signature: hex::encode(sig.to_bytes()),
        key_id: KEY_ID.to_string(),
        key_manifest: Some(manifest.to_string()),
    };
    serde_json::to_string(&env).unwrap()
}

#[test]
fn rotated_document_verifies_against_head_key() {
    let root = signing_key(7);
    let rotated = signing_key(8);
    let manifest = rotation_manifest(&root, &rotated);
    // Document signed by the ROTATED (head) key.
    let raw = rotated_envelope(&rotated, &payload(1, "2026-06-02T00:00:00Z"), &manifest);
    let v = verify_signed_registry(
        &raw,
        &root.verifying_key(), // pin is the ROOT
        KEY_ID,
        &RegistryTrustState::default(),
    )
    .expect("rotated doc signed by head must verify against the pinned root chain");
    assert_eq!(v.new_state.last_chain_head_seq, Some(1));
    assert_eq!(v.new_state.last_sequence_number, 1);
}

#[test]
fn rotated_document_signed_by_old_root_rejected() {
    // After rotation, the ROOT key is no longer the head — a document it signs
    // must not verify (only the head key may sign a current registry).
    let root = signing_key(7);
    let rotated = signing_key(8);
    let manifest = rotation_manifest(&root, &rotated);
    let raw = rotated_envelope(&root, &payload(1, "2026-06-02T00:00:00Z"), &manifest);
    let err = verify_signed_registry(
        &raw,
        &root.verifying_key(),
        KEY_ID,
        &RegistryTrustState::default(),
    )
    .unwrap_err();
    assert_eq!(err, RegistryTrustError::SignatureInvalid);
}

#[test]
fn rotation_chain_not_rooted_at_pin_rejected() {
    // Chain rooted at an attacker key (not the pin) → chain validation fails
    // (root fingerprint not in the trusted set).
    let attacker_root = signing_key(20);
    let rotated = signing_key(8);
    let manifest = rotation_manifest(&attacker_root, &rotated);
    let raw = rotated_envelope(&rotated, &payload(1, "2026-06-02T00:00:00Z"), &manifest);
    let pinned = signing_key(7); // the real pin, NOT the attacker root
    let err = verify_signed_registry(
        &raw,
        &pinned.verifying_key(),
        KEY_ID,
        &RegistryTrustState::default(),
    )
    .unwrap_err();
    assert!(matches!(err, RegistryTrustError::RotationChainInvalid(_)));
}

#[test]
fn rotation_chain_rollback_rejected() {
    // A client that has accepted head seq 1 rejects a manifest whose head seq
    // is lower (a truncated/rolled-back chain).
    let root = signing_key(7);
    let rotated = signing_key(8);
    let manifest = rotation_manifest(&root, &rotated);
    let raw = rotated_envelope(&rotated, &payload(9, "2026-06-09T00:00:00Z"), &manifest);
    let state = RegistryTrustState {
        key_id: Some(KEY_ID.to_string()),
        last_sequence_number: 8,
        last_published_at: Some("2026-06-08T00:00:00Z".to_string()),
        last_chain_head_seq: Some(2), // already accepted a longer chain
        root_override: None,
    };
    let err = verify_signed_registry(&raw, &root.verifying_key(), KEY_ID, &state).unwrap_err();
    assert!(matches!(err, RegistryTrustError::RotationChainInvalid(_)));
}

#[test]
fn rotation_manifest_tampered_rejected() {
    // Flip a byte in the rotated key's endorsement signature → broken chain.
    let root = signing_key(7);
    let rotated = signing_key(8);
    let manifest = rotation_manifest(&root, &rotated);
    // Corrupt the rotation_signature hex (last hex field) deterministically.
    let tampered = manifest.replacen(
        "\"rotation_signature\":\"",
        "\"rotation_signature\":\"00",
        1,
    );
    let raw = rotated_envelope(&rotated, &payload(1, "2026-06-02T00:00:00Z"), &tampered);
    let err = verify_signed_registry(
        &raw,
        &root.verifying_key(),
        KEY_ID,
        &RegistryTrustState::default(),
    )
    .unwrap_err();
    assert!(matches!(
        err,
        RegistryTrustError::RotationChainInvalid(_) | RegistryTrustError::ManifestParse(_)
    ));
}

#[test]
fn cache_integrity_accepts_rotated_document() {
    // The cache path also resolves the rotation head (integrity-only).
    let root = signing_key(7);
    let rotated = signing_key(8);
    let manifest = rotation_manifest(&root, &rotated);
    let p = payload(1, "2026-06-02T00:00:00Z");
    let raw = rotated_envelope(&rotated, &p, &manifest);
    let trusted = verify_registry_integrity(
        &raw,
        &root.verifying_key(),
        KEY_ID,
        &RegistryTrustState::default(),
    )
    .expect("cached rotated doc must pass integrity check");
    assert_eq!(trusted, p);
}

#[test]
fn manifest_absent_rejected_after_rotation() {
    // After a rotation has been accepted (last_chain_head_seq Some), a
    // manifest-absent document is rejected — otherwise it would verify against
    // the rotated-away root key, a downgrade past the rotation (Copilot #2049).
    let root = signing_key(7);
    let raw = envelope(&root, &payload(9, "2026-06-09T00:00:00Z"), KEY_ID); // no manifest
    let state = RegistryTrustState {
        key_id: Some(KEY_ID.to_string()),
        last_sequence_number: 8,
        last_published_at: Some("2026-06-08T00:00:00Z".to_string()),
        last_chain_head_seq: Some(1),
        root_override: None,
    };
    let err = verify_signed_registry(&raw, &root.verifying_key(), KEY_ID, &state).unwrap_err();
    assert_eq!(
        err,
        RegistryTrustError::RotationManifestRequired {
            last_chain_head_seq: 1
        }
    );
}

#[test]
fn manifest_absent_ok_before_any_rotation() {
    // A pre-rotation client (no rotation ever accepted) still accepts a
    // single-key manifest-absent document against the pinned root.
    let root = signing_key(7);
    let raw = envelope(&root, &payload(1, "2026-05-27T00:00:00Z"), KEY_ID);
    let v = verify_signed_registry(
        &raw,
        &root.verifying_key(),
        KEY_ID,
        &RegistryTrustState::default(),
    )
    .expect("pre-rotation single-key document is accepted");
    assert_eq!(v.new_state.last_chain_head_seq, None);
}

// ───── cache path enforces anti-rollback (Council #2049, parity with fetch) ─────

#[test]
fn cache_rejects_older_than_state() {
    // A local attacker swaps the cache file for an OLDER validly-signed doc.
    // The cache path now rejects it (sequence below the high-water mark).
    let key = signing_key(7);
    let raw = envelope(&key, &payload(3, "2026-05-20T00:00:00Z"), KEY_ID);
    let state = RegistryTrustState {
        key_id: Some(KEY_ID.to_string()),
        last_sequence_number: 5,
        last_published_at: Some("2026-05-27T00:00:00Z".to_string()),
        last_chain_head_seq: None,
        root_override: None,
    };
    let err = verify_registry_integrity(&raw, &key.verifying_key(), KEY_ID, &state).unwrap_err();
    assert!(matches!(err, RegistryTrustError::RollbackSequence { .. }));
}

#[test]
fn cache_accepts_equal_sequence_current_doc() {
    // The cache holds the CURRENT document (seq == last); `>=` semantics accept
    // it (where the strict fetch `>` would reject equal).
    let key = signing_key(7);
    let p = payload(5, "2026-05-27T00:00:00Z");
    let raw = envelope(&key, &p, KEY_ID);
    let state = RegistryTrustState {
        key_id: Some(KEY_ID.to_string()),
        last_sequence_number: 5,
        last_published_at: Some("2026-05-27T00:00:00Z".to_string()),
        last_chain_head_seq: None,
        root_override: None,
    };
    let trusted = verify_registry_integrity(&raw, &key.verifying_key(), KEY_ID, &state)
        .expect("cache accepts the current (equal-sequence) document");
    assert_eq!(trusted, p);
}

#[test]
fn cache_rejects_manifest_absent_after_rotation() {
    // Cache parity with fetch: once a rotation is accepted, a manifest-absent
    // cached doc is rejected (can't downgrade to root via the cache).
    let root = signing_key(7);
    let raw = envelope(&root, &payload(9, "2026-06-09T00:00:00Z"), KEY_ID);
    let state = RegistryTrustState {
        key_id: Some(KEY_ID.to_string()),
        last_sequence_number: 8,
        last_published_at: Some("2026-06-08T00:00:00Z".to_string()),
        last_chain_head_seq: Some(1),
        root_override: None,
    };
    let err = verify_registry_integrity(&raw, &root.verifying_key(), KEY_ID, &state).unwrap_err();
    assert!(matches!(
        err,
        RegistryTrustError::RotationManifestRequired { .. }
    ));
}

#[test]
fn malformed_persisted_published_at_does_not_brick_fetch() {
    // A corrupted/tampered state.last_published_at must NOT fail every fetch
    // (DoS via poisoned state, Council #2049 R3): the stored value is compared
    // tolerantly; sequence_number remains the primary monotonic guard. The
    // INCOMING published_at is still strictly validated.
    let key = signing_key(7);
    let raw = envelope(&key, &payload(6, "2026-06-09T00:00:00Z"), KEY_ID);
    let state = RegistryTrustState {
        key_id: Some(KEY_ID.to_string()),
        last_sequence_number: 5,
        last_published_at: Some("not-a-timestamp".to_string()), // corrupted on disk
        last_chain_head_seq: None,
        root_override: None,
    };
    let v = verify_signed_registry(&raw, &key.verifying_key(), KEY_ID, &state)
        .expect("a malformed persisted published_at must not brick the fetch");
    assert_eq!(v.new_state.last_sequence_number, 6);
}

// ───── escrow root override re-anchors the effective trust root (D10d-source) ─────

use crate::plugin::registry_escrow::AcceptedRootOverride;

/// A trust state carrying an accepted root override to `new_root`.
fn state_with_override(new_root: &SigningKey) -> RegistryTrustState {
    RegistryTrustState {
        key_id: Some(KEY_ID.to_string()),
        last_sequence_number: 0,
        last_published_at: None,
        // A fresh root starts its own lineage — no rotation accepted under it yet.
        last_chain_head_seq: None,
        root_override: Some(AcceptedRootOverride {
            override_seq: 1,
            new_root_key: hex::encode(new_root.verifying_key().to_bytes()),
        }),
    }
}

#[test]
fn override_re_anchors_fetch_to_new_root() {
    // After a catastrophic recovery, documents signed by the NEW root verify
    // against the (lost) pinned root's slot via the recorded override.
    let lost_pin = signing_key(7);
    let new_root = signing_key(60);
    let raw = envelope(&new_root, &payload(1, "2026-06-10T00:00:00Z"), KEY_ID);
    let state = state_with_override(&new_root);
    match decide_fetch(&raw, Some(&lost_pin.verifying_key()), KEY_ID, &state).unwrap() {
        FetchDecision::Verified(v) => {
            assert_eq!(v.new_state.last_sequence_number, 1);
            // The override is preserved across the routine document fetch.
            assert_eq!(v.new_state.root_override, state.root_override);
        }
        other => panic!("expected Verified, got {other:?}"),
    }
}

#[test]
fn override_rejects_documents_from_lost_root() {
    // Once trust is re-anchored, a document signed by the OLD (lost/compromised)
    // pinned root must NOT verify — that is the whole point of recovery.
    let lost_pin = signing_key(7);
    let new_root = signing_key(60);
    let raw = envelope(&lost_pin, &payload(1, "2026-06-10T00:00:00Z"), KEY_ID);
    let state = state_with_override(&new_root);
    let err = decide_fetch(&raw, Some(&lost_pin.verifying_key()), KEY_ID, &state).unwrap_err();
    assert_eq!(err, RegistryTrustError::SignatureInvalid);
}

#[test]
fn override_re_anchors_cache_path_too() {
    // The cache path resolves the same effective root (parity with fetch).
    let lost_pin = signing_key(7);
    let new_root = signing_key(60);
    let p = payload(1, "2026-06-10T00:00:00Z");
    let raw = envelope(&new_root, &p, KEY_ID);
    let state = state_with_override(&new_root);
    let outcome = decide_cache(&raw, Some(&lost_pin.verifying_key()), KEY_ID, &state).unwrap();
    assert!(outcome.verified);
    assert_eq!(outcome.payload, p);
}

#[test]
fn effective_root_method_prefers_override() {
    use crate::plugin::registry_escrow::EffectiveRoot;
    let lost_pin = signing_key(7).verifying_key();
    let new_root = signing_key(60);
    let state = state_with_override(&new_root);
    match state.effective_root(Some(lost_pin)) {
        EffectiveRoot::Anchor(k) => {
            assert_eq!(k.to_bytes(), new_root.verifying_key().to_bytes())
        }
        other => panic!("expected override anchor, got {other:?}"),
    }
    // Without an override, the method returns the pin unchanged.
    match RegistryTrustState::default().effective_root(Some(lost_pin)) {
        EffectiveRoot::Anchor(k) => assert_eq!(k.to_bytes(), lost_pin.to_bytes()),
        other => panic!("expected pin anchor, got {other:?}"),
    }
}

#[test]
fn corrupt_override_fails_closed_not_downgraded() {
    // A recorded-but-malformed override must REJECT (fail closed), never fall
    // through to the unverified/migration path — otherwise corrupting the stored
    // override key would downgrade the client into accepting unsigned documents.
    use crate::plugin::registry_escrow::AcceptedRootOverride;
    let lost_pin = signing_key(7);
    let raw = envelope(&lost_pin, &payload(1, "2026-06-10T00:00:00Z"), KEY_ID);
    let state = RegistryTrustState {
        key_id: Some(KEY_ID.to_string()),
        last_sequence_number: 0,
        last_published_at: None,
        last_chain_head_seq: None,
        root_override: Some(AcceptedRootOverride {
            override_seq: 1,
            new_root_key: "corrupt-not-a-key".to_string(),
        }),
    };
    let fetch_err =
        decide_fetch(&raw, Some(&lost_pin.verifying_key()), KEY_ID, &state).unwrap_err();
    assert_eq!(fetch_err, RegistryTrustError::RootOverrideUnusable);
    let cache_err =
        decide_cache(&raw, Some(&lost_pin.verifying_key()), KEY_ID, &state).unwrap_err();
    assert_eq!(cache_err, RegistryTrustError::RootOverrideUnusable);
}
