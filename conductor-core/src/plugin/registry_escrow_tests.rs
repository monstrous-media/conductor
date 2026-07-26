// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! ADR-027 §D10d-source — unit tests for the M-of-N (2-of-3) registry root-key
//! escrow.

use super::*;
use ed25519_dalek::{Signer, SigningKey};

const KEY_ID: &str = "conductor-registry-v1";

fn signing_key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

/// The three escrow holders (deterministic seeds) and a fresh new-root key.
fn escrow_set() -> [SigningKey; 3] {
    [signing_key(11), signing_key(12), signing_key(13)]
}

fn verifying(keys: &[SigningKey]) -> Vec<VerifyingKey> {
    keys.iter().map(|k| k.verifying_key()).collect()
}

/// Produce an `EscrowSignature` from `holder` over `(key_id, seq, new_root)`.
fn sign_override(
    holder: &SigningKey,
    key_id: &str,
    override_seq: u64,
    new_root: &VerifyingKey,
) -> EscrowSignature {
    let msg = escrow_override_message(key_id, override_seq, &new_root.to_bytes());
    EscrowSignature {
        signer_key: hex::encode(holder.verifying_key().to_bytes()),
        signature: hex::encode(holder.sign(&msg).to_bytes()),
    }
}

/// Build a root-override doc signed by the given holders.
fn override_doc(
    holders: &[&SigningKey],
    key_id: &str,
    override_seq: u64,
    new_root: &VerifyingKey,
) -> RootOverride {
    RootOverride {
        key_id: key_id.to_string(),
        override_seq,
        new_root_key: hex::encode(new_root.to_bytes()),
        signatures: holders
            .iter()
            .map(|h| sign_override(h, key_id, override_seq, new_root))
            .collect(),
    }
}

// ───────────────────────── threshold ─────────────────────────

#[test]
fn two_of_three_accepted() {
    let escrow = escrow_set();
    let new_root = signing_key(50).verifying_key();
    let doc = override_doc(&[&escrow[0], &escrow[1]], KEY_ID, 1, &new_root);
    let resolved = verify_root_override_with_keys(&doc, &verifying(&escrow), KEY_ID, None)
        .expect("a 2-of-3 quorum must re-anchor the root");
    assert_eq!(resolved.new_root.to_bytes(), new_root.to_bytes());
    // The verified result also carries the record to persist.
    assert_eq!(resolved.record.override_seq, 1);
    assert_eq!(
        resolved.record.new_root_key,
        hex::encode(new_root.to_bytes())
    );
}

#[test]
fn three_of_three_accepted() {
    let escrow = escrow_set();
    let new_root = signing_key(50).verifying_key();
    let doc = override_doc(&[&escrow[0], &escrow[1], &escrow[2]], KEY_ID, 1, &new_root);
    let resolved = verify_root_override_with_keys(&doc, &verifying(&escrow), KEY_ID, None).unwrap();
    assert_eq!(resolved.new_root.to_bytes(), new_root.to_bytes());
}

#[test]
fn one_of_three_rejected() {
    // A single holder must NOT be able to re-anchor the root.
    let escrow = escrow_set();
    let new_root = signing_key(50).verifying_key();
    let doc = override_doc(&[&escrow[0]], KEY_ID, 1, &new_root);
    let err = verify_root_override_with_keys(&doc, &verifying(&escrow), KEY_ID, None).unwrap_err();
    assert_eq!(
        err,
        RootOverrideError::InsufficientSignatures { got: 1, needed: 2 }
    );
}

#[test]
fn duplicate_signer_counts_once() {
    // The same holder's signature replayed twice is one distinct signer, not two.
    let escrow = escrow_set();
    let new_root = signing_key(50).verifying_key();
    let one = sign_override(&escrow[0], KEY_ID, 1, &new_root);
    let doc = RootOverride {
        key_id: KEY_ID.to_string(),
        override_seq: 1,
        new_root_key: hex::encode(new_root.to_bytes()),
        signatures: vec![one.clone(), one],
    };
    let err = verify_root_override_with_keys(&doc, &verifying(&escrow), KEY_ID, None).unwrap_err();
    assert_eq!(
        err,
        RootOverrideError::InsufficientSignatures { got: 1, needed: 2 }
    );
}

#[test]
fn signature_from_non_escrow_key_ignored() {
    // A valid signature from a key OUTSIDE the escrow set does not count.
    let escrow = escrow_set();
    let outsider = signing_key(99);
    let new_root = signing_key(50).verifying_key();
    // One real escrow sig + one outsider sig → still only 1 distinct escrow key.
    let doc = RootOverride {
        key_id: KEY_ID.to_string(),
        override_seq: 1,
        new_root_key: hex::encode(new_root.to_bytes()),
        signatures: vec![
            sign_override(&escrow[0], KEY_ID, 1, &new_root),
            sign_override(&outsider, KEY_ID, 1, &new_root),
        ],
    };
    let err = verify_root_override_with_keys(&doc, &verifying(&escrow), KEY_ID, None).unwrap_err();
    assert_eq!(
        err,
        RootOverrideError::InsufficientSignatures { got: 1, needed: 2 }
    );
}

#[test]
fn forged_signature_does_not_count() {
    // A signature that does not verify (wrong bytes) is silently skipped — the
    // remaining valid one is below threshold.
    let escrow = escrow_set();
    let new_root = signing_key(50).verifying_key();
    let mut forged = sign_override(&escrow[1], KEY_ID, 1, &new_root);
    // Corrupt the signature hex deterministically.
    forged.signature = forged.signature.replacen("a", "b", 1);
    let doc = RootOverride {
        key_id: KEY_ID.to_string(),
        override_seq: 1,
        new_root_key: hex::encode(new_root.to_bytes()),
        signatures: vec![sign_override(&escrow[0], KEY_ID, 1, &new_root), forged],
    };
    let err = verify_root_override_with_keys(&doc, &verifying(&escrow), KEY_ID, None).unwrap_err();
    assert!(matches!(
        err,
        RootOverrideError::InsufficientSignatures { needed: 2, .. }
    ));
}

// ───────────────────────── binding ─────────────────────────

#[test]
fn signatures_bound_to_new_root() {
    // Signatures minted for new_root A must not validate an override that swaps
    // in new_root B (binding the new root into the signed bytes).
    let escrow = escrow_set();
    let root_a = signing_key(50).verifying_key();
    let root_b = signing_key(51).verifying_key();
    let mut doc = override_doc(&[&escrow[0], &escrow[1]], KEY_ID, 1, &root_a);
    // Swap the advertised new root to B while keeping A's signatures.
    doc.new_root_key = hex::encode(root_b.to_bytes());
    let err = verify_root_override_with_keys(&doc, &verifying(&escrow), KEY_ID, None).unwrap_err();
    assert!(matches!(
        err,
        RootOverrideError::InsufficientSignatures { got: 0, needed: 2 }
    ));
}

#[test]
fn signatures_bound_to_key_id() {
    // Signatures minted for key_id A must not validate an override claiming a
    // different key_id (binding key_id into the signed bytes).
    let escrow = escrow_set();
    let new_root = signing_key(50).verifying_key();
    let mut doc = override_doc(&[&escrow[0], &escrow[1]], "other-registry", 1, &new_root);
    doc.key_id = KEY_ID.to_string(); // claim a different id than was signed
    let err = verify_root_override_with_keys(&doc, &verifying(&escrow), KEY_ID, None).unwrap_err();
    assert!(matches!(
        err,
        RootOverrideError::InsufficientSignatures { got: 0, .. }
    ));
}

#[test]
fn signatures_bound_to_override_seq() {
    // Signatures minted for override_seq 1 must not validate at override_seq 2.
    let escrow = escrow_set();
    let new_root = signing_key(50).verifying_key();
    let mut doc = override_doc(&[&escrow[0], &escrow[1]], KEY_ID, 1, &new_root);
    doc.override_seq = 2; // re-target a different sequence than was signed
    let err = verify_root_override_with_keys(&doc, &verifying(&escrow), KEY_ID, None).unwrap_err();
    assert!(matches!(
        err,
        RootOverrideError::InsufficientSignatures { got: 0, .. }
    ));
}

#[test]
fn key_id_mismatch_rejected_before_crypto() {
    // An override authored for a DIFFERENT registry id is rejected up front
    // (cross-registry replay defense), independent of the signature binding.
    let escrow = escrow_set();
    let new_root = signing_key(50).verifying_key();
    // Fully valid 2-of-3 override for "other-registry"...
    let doc = override_doc(&[&escrow[0], &escrow[1]], "other-registry", 1, &new_root);
    // ...but this client expects KEY_ID.
    let err = verify_root_override_with_keys(&doc, &verifying(&escrow), KEY_ID, None).unwrap_err();
    assert_eq!(
        err,
        RootOverrideError::KeyIdMismatch {
            expected: KEY_ID.to_string(),
            found: "other-registry".to_string(),
        }
    );
}

// ───────────────────────── anti-rollback ─────────────────────────

#[test]
fn override_seq_must_be_present() {
    let escrow = escrow_set();
    let new_root = signing_key(50).verifying_key();
    let doc = override_doc(&[&escrow[0], &escrow[1]], KEY_ID, 0, &new_root);
    let err = verify_root_override_with_keys(&doc, &verifying(&escrow), KEY_ID, None).unwrap_err();
    assert_eq!(err, RootOverrideError::MissingOverrideSeq);
}

#[test]
fn replay_of_older_override_rejected() {
    // Client already accepted override 2; an earlier override 1 (e.g. one that
    // re-anchored to a since-compromised key) cannot be replayed to revert.
    let escrow = escrow_set();
    let new_root = signing_key(50).verifying_key();
    let doc = override_doc(&[&escrow[0], &escrow[1]], KEY_ID, 1, &new_root);
    let err =
        verify_root_override_with_keys(&doc, &verifying(&escrow), KEY_ID, Some(2)).unwrap_err();
    assert_eq!(
        err,
        RootOverrideError::RollbackOverrideSeq { last: 2, got: 1 }
    );
}

#[test]
fn same_override_seq_rejected() {
    let escrow = escrow_set();
    let new_root = signing_key(50).verifying_key();
    let doc = override_doc(&[&escrow[0], &escrow[1]], KEY_ID, 3, &new_root);
    let err =
        verify_root_override_with_keys(&doc, &verifying(&escrow), KEY_ID, Some(3)).unwrap_err();
    assert_eq!(
        err,
        RootOverrideError::RollbackOverrideSeq { last: 3, got: 3 }
    );
}

#[test]
fn newer_override_seq_accepted() {
    let escrow = escrow_set();
    let new_root = signing_key(50).verifying_key();
    let doc = override_doc(&[&escrow[0], &escrow[1]], KEY_ID, 4, &new_root);
    let resolved =
        verify_root_override_with_keys(&doc, &verifying(&escrow), KEY_ID, Some(3)).unwrap();
    assert_eq!(resolved.new_root.to_bytes(), new_root.to_bytes());
}

// ───────────────────────── DoS / config ─────────────────────────

#[test]
fn too_many_signatures_rejected_before_verify() {
    let escrow = escrow_set();
    let new_root = signing_key(50).verifying_key();
    let mut doc = override_doc(&[&escrow[0], &escrow[1]], KEY_ID, 1, &new_root);
    // Pad past the cap with junk entries.
    while doc.signatures.len() <= MAX_ESCROW_SIGNATURES {
        doc.signatures.push(EscrowSignature {
            signer_key: "00".to_string(),
            signature: "00".to_string(),
        });
    }
    let err = verify_root_override_with_keys(&doc, &verifying(&escrow), KEY_ID, None).unwrap_err();
    assert!(matches!(
        err,
        RootOverrideError::TooManySignatures {
            max: MAX_ESCROW_SIGNATURES,
            ..
        }
    ));
}

#[test]
fn no_escrow_keys_fails_closed() {
    let new_root = signing_key(50).verifying_key();
    let doc = override_doc(&[&signing_key(11), &signing_key(12)], KEY_ID, 1, &new_root);
    let err = verify_root_override_with_keys(&doc, &[], KEY_ID, None).unwrap_err();
    assert_eq!(err, RootOverrideError::NoEscrowConfigured);
}

#[test]
fn bad_new_root_key_rejected() {
    let escrow = escrow_set();
    let new_root = signing_key(50).verifying_key();
    let mut doc = override_doc(&[&escrow[0], &escrow[1]], KEY_ID, 1, &new_root);
    doc.new_root_key = "not-hex".to_string();
    let err = verify_root_override_with_keys(&doc, &verifying(&escrow), KEY_ID, None).unwrap_err();
    assert!(matches!(err, RootOverrideError::BadNewRootKey(_)));
}

#[test]
fn parse_root_override_round_trips() {
    let escrow = escrow_set();
    let new_root = signing_key(50).verifying_key();
    let doc = override_doc(&[&escrow[0], &escrow[1]], KEY_ID, 3, &new_root);
    let raw = serde_json::to_string(&doc).unwrap();
    let parsed = parse_root_override(&raw).expect("a well-formed override parses");
    assert_eq!(parsed.override_seq, 3);
    assert_eq!(parsed.key_id, KEY_ID);
}

#[test]
fn parse_root_override_rejects_oversized_before_parse() {
    // A document over the cap is refused without parsing (DoS-on-parse bound).
    let big = format!("{{\"key_id\":\"{}\"}}", "x".repeat(MAX_ROOT_OVERRIDE_BYTES));
    let err = parse_root_override(&big).unwrap_err();
    assert!(matches!(err, RootOverrideError::DocumentTooLarge { .. }));
}

#[test]
fn parse_root_override_rejects_malformed() {
    let err = parse_root_override("{ not json").unwrap_err();
    assert!(matches!(err, RootOverrideError::Malformed(_)));
}

#[test]
fn persisted_record_is_canonical_hex() {
    // Even if the document's new_root_key uses non-canonical (upper-case) hex,
    // the persisted record carries the canonical lower-case rendering of the
    // VERIFIED key bytes — so the stored form always round-trips to new_root.
    let escrow = escrow_set();
    let new_root = signing_key(50).verifying_key();
    let mut doc = override_doc(&[&escrow[0], &escrow[1]], KEY_ID, 1, &new_root);
    doc.new_root_key = hex::encode(new_root.to_bytes()).to_uppercase();
    let verified = verify_root_override_with_keys(&doc, &verifying(&escrow), KEY_ID, None).unwrap();
    assert_eq!(
        verified.record.new_root_key,
        hex::encode(new_root.to_bytes())
    );
    assert!(
        verified
            .record
            .new_root_key
            .chars()
            .all(|c| !c.is_ascii_uppercase())
    );
}

#[test]
fn whitespace_padded_signer_key_does_not_count() {
    // Key decoding is strict (no trim), so a signer_key with surrounding
    // whitespace fails to decode and is ignored — it cannot masquerade as a
    // valid distinct holder. This keeps the runtime decode consistent with the
    // verbatim build-time distinctness gate (no whitespace-dup quorum collapse).
    let escrow = escrow_set();
    let new_root = signing_key(50).verifying_key();
    let mut padded = sign_override(&escrow[1], KEY_ID, 1, &new_root);
    padded.signer_key = format!(" {}", padded.signer_key); // leading space
    let doc = RootOverride {
        key_id: KEY_ID.to_string(),
        override_seq: 1,
        new_root_key: hex::encode(new_root.to_bytes()),
        signatures: vec![sign_override(&escrow[0], KEY_ID, 1, &new_root), padded],
    };
    // Only escrow[0] counts; the padded escrow[1] entry does not decode.
    let err = verify_root_override_with_keys(&doc, &verifying(&escrow), KEY_ID, None).unwrap_err();
    assert_eq!(
        err,
        RootOverrideError::InsufficientSignatures { got: 1, needed: 2 }
    );
}

#[test]
fn parse_root_override_missing_seq_then_rejected_by_verify() {
    // A document omitting override_seq parses (serde default 0) and is then
    // rejected by verification as MissingOverrideSeq — not a serde parse error.
    let escrow = escrow_set();
    let new_root = signing_key(50).verifying_key();
    let sigs = [&escrow[0], &escrow[1]]
        .iter()
        .map(|h| sign_override(h, KEY_ID, 0, &new_root))
        .collect::<Vec<_>>();
    let raw = format!(
        r#"{{"key_id":"{KEY_ID}","new_root_key":"{}","signatures":{}}}"#,
        hex::encode(new_root.to_bytes()),
        serde_json::to_string(&sigs).unwrap()
    );
    let parsed = parse_root_override(&raw).expect("omitted override_seq defaults, still parses");
    assert_eq!(parsed.override_seq, 0);
    let err =
        verify_root_override_with_keys(&parsed, &verifying(&escrow), KEY_ID, None).unwrap_err();
    assert_eq!(err, RootOverrideError::MissingOverrideSeq);
}

// ───────────────────────── message / config invariants ─────────────────────────

#[test]
fn override_message_binds_domain_keyid_seq_and_root() {
    let root = signing_key(50).verifying_key();
    let m = escrow_override_message(KEY_ID, 7, &root.to_bytes());
    assert!(m.starts_with(b"conductor-registry-root-override-v1\0"));
    assert!(m.ends_with(&root.to_bytes()));
    // Distinct domain from the document signature domain.
    assert!(!m.starts_with(b"conductor-registry-document-v1\0"));
    // Changing any bound field changes the bytes.
    assert_ne!(
        escrow_override_message(KEY_ID, 7, &root.to_bytes()),
        escrow_override_message("other", 7, &root.to_bytes())
    );
    assert_ne!(
        escrow_override_message(KEY_ID, 7, &root.to_bytes()),
        escrow_override_message(KEY_ID, 8, &root.to_bytes())
    );
    let other_root = signing_key(51).verifying_key();
    assert_ne!(
        escrow_override_message(KEY_ID, 7, &root.to_bytes()),
        escrow_override_message(KEY_ID, 7, &other_root.to_bytes())
    );
}

#[test]
fn phase1_escrow_keys_absent() {
    // Phase-1 ships empty placeholders → no configured escrow keys, and a
    // verify against them fails closed.
    assert!(configured_escrow_keys().is_empty());
    assert_eq!(REGISTRY_ESCROW_THRESHOLD, 2);
    assert_eq!(REGISTRY_ESCROW_N, 3);
}

#[test]
fn verified_result_carries_persistable_record() {
    // The record to persist is obtainable ONLY from a successful verification —
    // there is no public constructor that mints one from an unverified document.
    let escrow = escrow_set();
    let new_root = signing_key(50).verifying_key();
    let doc = override_doc(&[&escrow[0], &escrow[1]], KEY_ID, 9, &new_root);
    let verified = verify_root_override_with_keys(&doc, &verifying(&escrow), KEY_ID, None).unwrap();
    assert_eq!(verified.record.override_seq, 9);
    assert_eq!(
        verified.record.new_root_key,
        hex::encode(new_root.to_bytes())
    );
}

// ───────────────────────── effective root ─────────────────────────

/// Extract the anchor key, panicking on any non-Anchor outcome.
fn anchor(e: EffectiveRoot) -> VerifyingKey {
    match e {
        EffectiveRoot::Anchor(k) => k,
        other => panic!("expected Anchor, got {other:?}"),
    }
}

#[test]
fn effective_root_uses_override_when_present() {
    let pinned = signing_key(7).verifying_key();
    let new_root = signing_key(50).verifying_key();
    let rec = AcceptedRootOverride {
        override_seq: 1,
        new_root_key: hex::encode(new_root.to_bytes()),
    };
    let resolved = anchor(effective_root(Some(pinned), Some(&rec)));
    assert_eq!(resolved.to_bytes(), new_root.to_bytes());
}

#[test]
fn effective_root_falls_back_to_pin_without_override() {
    let pinned = signing_key(7).verifying_key();
    let resolved = anchor(effective_root(Some(pinned), None));
    assert_eq!(resolved.to_bytes(), pinned.to_bytes());
}

#[test]
fn effective_root_migration_when_no_pin_and_no_override() {
    assert!(matches!(
        effective_root(None, None),
        EffectiveRoot::Migration
    ));
}

#[test]
fn effective_root_fails_closed_on_malformed_override() {
    // A corrupted stored override must NOT silently fall back to the pin it was
    // meant to replace, nor degrade to migration — it resolves to OverrideUnusable
    // so the caller fails closed.
    let pinned = signing_key(7).verifying_key();
    let rec = AcceptedRootOverride {
        override_seq: 1,
        new_root_key: "garbage".to_string(),
    };
    assert!(matches!(
        effective_root(Some(pinned), Some(&rec)),
        EffectiveRoot::OverrideUnusable
    ));
}

#[test]
fn production_verify_uses_configured_keys_phase1_empty() {
    // The production entry binds the build-baked escrow set (not a caller
    // parameter). In Phase-1 that set is empty, so it fails closed regardless of
    // how many valid signatures a forged document carries.
    let escrow = escrow_set();
    let new_root = signing_key(50).verifying_key();
    let doc = override_doc(&[&escrow[0], &escrow[1]], KEY_ID, 1, &new_root);
    let err = verify_root_override(&doc, KEY_ID, None).unwrap_err();
    assert_eq!(err, RootOverrideError::NoEscrowConfigured);
}
