// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Unit tests for the network-approval envelope/registry. In a `#[path]`
//! sibling of `network_approvals.rs` (still a submodule, so it keeps access to
//! the module-private crypto helpers) — kept out of the implementation file so
//! that file stays within the Council verify content budget.

use super::*;

fn test_key() -> HmacKey {
    HmacKey::from_bytes([0x42u8; 32])
}

fn sample_key() -> ApprovalKey {
    ApprovalKey {
        alias: "osc-in".into(),
        host: "192.168.1.20".into(),
        port: 9000,
        acl_hash: "abc123".into(),
    }
}

#[test]
fn default_registry_is_valid_current_schema() {
    // Regression: a default-constructed registry must use the current version
    // (not 0) so it round-trips through sign/verify rather than failing the
    // load-time version check.
    let reg = ApprovalRegistry::default();
    assert_eq!(reg, ApprovalRegistry::new());
    let key = test_key();
    let envelope = sign_envelope(&reg, &key).unwrap();
    assert!(verify_envelope(envelope.as_bytes(), &key).is_ok());
}

#[test]
fn storage_key_is_injective_across_delimiter_boundaries() {
    // A field containing the separator must not forge another listener's key.
    let a = ApprovalKey {
        alias: "a\u{1f}b".into(),
        host: "c".into(),
        port: 1,
        acl_hash: "h".into(),
    };
    let b = ApprovalKey {
        alias: "a".into(),
        host: "b\u{1f}c".into(),
        port: 1,
        acl_hash: "h".into(),
    };
    assert_ne!(a.storage_key(), b.storage_key());

    let mut reg = ApprovalRegistry::new();
    reg.add_listener_approval(&a, ApprovingSurface::Cli);
    assert!(reg.listener_is_approved(&a));
    assert!(
        !reg.listener_is_approved(&b),
        "must not alias to a different key"
    );
}

#[test]
fn envelope_roundtrip_recovers_registry() {
    let key = test_key();
    let mut reg = ApprovalRegistry::new();
    reg.add_listener_approval(&sample_key(), ApprovingSurface::Cli);
    let envelope = sign_envelope(&reg, &key).unwrap();
    let recovered = verify_envelope(envelope.as_bytes(), &key).unwrap();
    assert_eq!(reg, recovered);
    assert!(recovered.listener_is_approved(&sample_key()));
}

#[test]
fn tampered_data_byte_fails_closed() {
    let key = test_key();
    let reg = ApprovalRegistry::new();
    let envelope = sign_envelope(&reg, &key).unwrap();
    let mut env: serde_json::Value = serde_json::from_str(&envelope).unwrap();
    // Flip the data without re-MACing.
    env["data"] = serde_json::Value::String(format!("{} ", env["data"].as_str().unwrap()));
    let tampered = serde_json::to_vec(&env).unwrap();
    assert!(matches!(
        verify_envelope(&tampered, &key),
        Err(RegistryError::MacMismatch)
    ));
}

#[test]
fn tampered_mac_byte_fails_closed() {
    let key = test_key();
    let reg = ApprovalRegistry::new();
    let envelope = sign_envelope(&reg, &key).unwrap();
    let mut env: serde_json::Value = serde_json::from_str(&envelope).unwrap();
    let mut mac: Vec<char> = env["mac"].as_str().unwrap().chars().collect();
    mac[0] = if mac[0] == 'a' { 'b' } else { 'a' };
    env["mac"] = serde_json::Value::String(mac.into_iter().collect());
    let tampered = serde_json::to_vec(&env).unwrap();
    assert!(matches!(
        verify_envelope(&tampered, &key),
        Err(RegistryError::MacMismatch)
    ));
}

#[test]
fn alg_confusion_is_rejected() {
    let key = test_key();
    let reg = ApprovalRegistry::new();
    let envelope = sign_envelope(&reg, &key).unwrap();
    let mut env: serde_json::Value = serde_json::from_str(&envelope).unwrap();
    for bad in ["none", "None", "hs256", ""] {
        env["alg"] = serde_json::Value::String(bad.into());
        let bytes = serde_json::to_vec(&env).unwrap();
        assert!(
            matches!(verify_envelope(&bytes, &key), Err(RegistryError::BadAlg)),
            "alg={bad:?} must be rejected"
        );
    }
}

#[test]
fn wrong_key_fails_closed() {
    let reg = ApprovalRegistry::new();
    let envelope = sign_envelope(&reg, &test_key()).unwrap();
    let other = HmacKey::from_bytes([0x99u8; 32]);
    assert!(matches!(
        verify_envelope(envelope.as_bytes(), &other),
        Err(RegistryError::MacMismatch)
    ));
}

#[test]
fn short_mac_never_matches() {
    let key = test_key();
    let reg = ApprovalRegistry::new();
    let envelope = sign_envelope(&reg, &key).unwrap();
    let mut env: serde_json::Value = serde_json::from_str(&envelope).unwrap();
    env["mac"] = serde_json::Value::String("00".into()); // not 32 bytes
    let bytes = serde_json::to_vec(&env).unwrap();
    assert!(matches!(
        verify_envelope(&bytes, &key),
        Err(RegistryError::MacMismatch)
    ));
}

#[test]
fn duplicate_listener_key_rejected_after_mac() {
    // Craft a payload with a duplicate map key, then MAC it correctly so it
    // passes the envelope check — the strict parser must still reject it.
    let key = test_key();
    let data = r#"{"version":1,"listeners":{"k":{"approved_at_secs":1,"approving_surface":"Cli","amplification_ack_at_secs":null},"k":{"approved_at_secs":2,"approving_surface":"Cli","amplification_ack_at_secs":null}},"trusted_networks":{}}"#;
    let mac = to_hex(&hmac_sha256(&key, data.as_bytes()));
    let env = format!(r#"{{"alg":"hmac-sha256","data":{data:?},"mac":"{mac}"}}"#);
    assert!(matches!(
        verify_envelope(env.as_bytes(), &key),
        Err(RegistryError::Parse(_))
    ));
}

#[test]
fn acl_change_invalidates_approval() {
    let mut reg = ApprovalRegistry::new();
    reg.add_listener_approval(&sample_key(), ApprovingSurface::Cli);
    let mut changed = sample_key();
    changed.acl_hash = "different".into();
    assert!(reg.listener_is_approved(&sample_key()));
    assert!(!reg.listener_is_approved(&changed));
}

#[test]
fn decision_matrix() {
    let mut reg = ApprovalRegistry::new();
    let k = sample_key();
    // Loopback short-circuits regardless of registry contents.
    assert_eq!(
        reg.decision(&k, true, false),
        ApprovalDecision::AutoApproveLoopback
    );
    // Non-loopback, no approval → prompt.
    assert_eq!(
        reg.decision(&k, false, false),
        ApprovalDecision::PromptRequired
    );
    reg.add_listener_approval(&k, ApprovingSurface::Cli);
    assert_eq!(reg.decision(&k, false, false), ApprovalDecision::Approved);
}

#[test]
fn amplification_ack_gates_the_decision() {
    // The D11 amplification flag must actually gate `decision()`, not be
    // ignored.
    let mut reg = ApprovalRegistry::new();
    let k = sample_key();
    reg.add_listener_approval(&k, ApprovingSurface::Cli);

    // Approved, but an amplifying listener with no ack must re-prompt.
    assert_eq!(reg.decision(&k, false, false), ApprovalDecision::Approved);
    assert_eq!(
        reg.decision(&k, false, true),
        ApprovalDecision::PromptRequired,
        "amplifying listener without an ack must not silently bind"
    );

    // Acknowledging clears it.
    assert!(reg.acknowledge_amplification(&k));
    assert_eq!(reg.decision(&k, false, true), ApprovalDecision::Approved);

    // An expired ack re-prompts again.
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    reg.listeners
        .get_mut(&k.storage_key())
        .unwrap()
        .amplification_ack_at_secs = Some(now_secs - 91 * 24 * 3600);
    assert_eq!(
        reg.decision(&k, false, true),
        ApprovalDecision::PromptRequired
    );

    // acknowledge on an unknown listener is a no-op.
    let mut other = sample_key();
    other.alias = "nope".into();
    assert!(!reg.acknowledge_amplification(&other));
}

#[test]
fn duplicate_top_level_field_rejected() {
    // serde's derived Deserialize rejects duplicate *named* fields, so the
    // top-level envelope/registry structs are not duplicate-key-vulnerable.
    let key = test_key();
    // A correctly-MAC'd registry payload with a duplicate "version" field.
    let data = r#"{"version":1,"version":2,"listeners":{},"trusted_networks":{}}"#;
    let mac = to_hex(&hmac_sha256(&key, data.as_bytes()));
    let env = format!(r#"{{"alg":"hmac-sha256","data":{data:?},"mac":"{mac}"}}"#);
    assert!(matches!(
        verify_envelope(env.as_bytes(), &key),
        Err(RegistryError::Parse(_))
    ));

    // A duplicate top-level *envelope* field is likewise rejected.
    let good = sign_envelope(&ApprovalRegistry::new(), &key).unwrap();
    let dup_env = good.replacen("\"mac\":", "\"mac\":\"x\",\"mac\":", 1);
    assert!(verify_envelope(dup_env.as_bytes(), &key).is_err());
}

#[test]
fn unknown_field_rejected() {
    let key = test_key();
    let data = r#"{"version":1,"listeners":{},"trusted_networks":{},"evil":true}"#;
    let mac = to_hex(&hmac_sha256(&key, data.as_bytes()));
    let env = format!(r#"{{"alg":"hmac-sha256","data":{data:?},"mac":"{mac}"}}"#);
    assert!(matches!(
        verify_envelope(env.as_bytes(), &key),
        Err(RegistryError::Parse(_))
    ));
}

#[test]
fn unknown_version_rejected_on_load() {
    let key = test_key();
    let data = r#"{"version":99,"listeners":{},"trusted_networks":{}}"#;
    let mac = to_hex(&hmac_sha256(&key, data.as_bytes()));
    let env = format!(r#"{{"alg":"hmac-sha256","data":{data:?},"mac":"{mac}"}}"#);
    assert!(
        matches!(verify_envelope(env.as_bytes(), &key), Err(RegistryError::Parse(m)) if m.contains("version")),
        "an unknown schema version must fail closed"
    );
}

#[test]
fn amplification_flag_self_expires() {
    let now = SystemTime::now();
    let now_secs = now.duration_since(UNIX_EPOCH).unwrap().as_secs();
    let mut rec = ApprovalRecord {
        approved_at_secs: now_secs,
        approving_surface: ApprovingSurface::Cli,
        amplification_ack_at_secs: None,
    };
    assert!(!rec.amplification_acknowledged(now), "no ack → false");
    rec.amplification_ack_at_secs = Some(now_secs);
    assert!(rec.amplification_acknowledged(now), "fresh ack → true");
    rec.amplification_ack_at_secs = Some(now_secs - 91 * 24 * 3600);
    assert!(!rec.amplification_acknowledged(now), "91d-old ack → false");
    rec.amplification_ack_at_secs = Some(now_secs + 3600);
    assert!(!rec.amplification_acknowledged(now), "future ack → false");
}
