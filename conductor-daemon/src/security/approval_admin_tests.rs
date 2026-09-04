// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Unit tests for the approval-admin logic. In a `#[path]` sibling so
//! the impl file stays within the Council verify content budget.

use super::*;
use crate::security::compute_acl_hash;
use crate::security::network_approvals::ApprovalRegistry;
use conductor_core::security::keychain::{HmacKey, KeyMetadata, KeychainError, KeychainStore};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Mutex;

fn parse(toml: &str) -> Config {
    toml::from_str(toml).expect("parse config")
}

/// Config with one loopback OSC listener, one non-loopback OSC listener (with an
/// ACL), and one Art-Net broadcast listener (amplifying).
fn sample_config() -> Config {
    parse(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "osc_local"
direction = "Input"
type = "OscEndpoint"
host = "127.0.0.1"
port = 9000

[[endpoints]]
alias = "osc_lan"
direction = "Input"
type = "OscEndpoint"
host = "192.168.1.10"
port = 9001
allow_network = true
network_acl = ["192.168.1.0/24"]

[[endpoints]]
alias = "artnet_b"
direction = "Input"
type = "ArtNetEndpoint"
universe = 0
host = "192.168.1.20"
port = 6454
allow_broadcast = true
allow_network = true
network_acl = ["192.168.1.0/24"]
"#,
    )
}

fn registry_path() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("sec").join("network_approvals.json");
    (tmp, path)
}

/// In-memory keychain whose `rotate` changes the key — for the rotation test.
struct MockKeychain {
    key: Mutex<HmacKey>,
}
impl MockKeychain {
    fn new() -> Self {
        Self {
            key: Mutex::new(HmacKey::from_bytes([1u8; 32])),
        }
    }
}
impl KeychainStore for MockKeychain {
    fn get_or_create_hmac_key(&self) -> Result<HmacKey, KeychainError> {
        Ok(self.key.lock().unwrap().clone())
    }
    fn rotate_hmac_key(&self) -> Result<HmacKey, KeychainError> {
        let new = HmacKey::generate()?;
        *self.key.lock().unwrap() = new.clone();
        Ok(new)
    }
    fn key_metadata(&self) -> Result<KeyMetadata, KeychainError> {
        Err(KeychainError::Backend("not used in tests".into()))
    }
}

// --- listing / resolution -------------------------------------------------

#[test]
fn list_listeners_classifies_loopback_and_amplification() {
    let cfg = sample_config();
    let listeners = list_listeners(&cfg);
    assert_eq!(listeners.len(), 3);

    let local = listeners.iter().find(|l| l.alias == "osc_local").unwrap();
    assert!(local.is_loopback);
    assert!(!local.requires_amplification_ack);

    let lan = listeners.iter().find(|l| l.alias == "osc_lan").unwrap();
    assert!(!lan.is_loopback);
    assert!(!lan.requires_amplification_ack);
    assert_eq!(lan.acl_entries, vec!["192.168.1.0/24".to_string()]);

    let art = listeners.iter().find(|l| l.alias == "artnet_b").unwrap();
    assert!(!art.is_loopback);
    assert!(
        art.requires_amplification_ack,
        "allow_broadcast → needs ack"
    );
}

#[test]
fn resolve_unknown_listener_errors() {
    let cfg = sample_config();
    assert!(matches!(
        resolve_listener(&cfg, "nope"),
        Err(AdminError::NoSuchListener(_))
    ));
}

#[test]
fn acl_hash_is_order_and_dup_independent() {
    let a = compute_acl_hash(&["10.0.0.0/8".into(), "192.168.0.0/16".into()]);
    let b = compute_acl_hash(&[
        "192.168.0.0/16".into(),
        "10.0.0.0/8".into(),
        "10.0.0.0/8".into(), // dup
    ]);
    assert_eq!(a, b);
    let c = compute_acl_hash(&["10.0.0.0/8".into()]);
    assert_ne!(a, c);
}

// --- approve / deny / status ----------------------------------------------

#[test]
fn approve_then_deny_a_non_loopback_listener() {
    let cfg = sample_config();
    let (_g, path) = registry_path();
    let key = HmacKey::from_bytes([9u8; 32]);

    // Initially not approved.
    let before = statuses(&cfg, &path, &key);
    let lan_before = before
        .iter()
        .find(|s| s.listener.alias == "osc_lan")
        .unwrap();
    assert!(!lan_before.approved);

    approve(&cfg, &path, &key, "osc_lan", ApprovingSurface::Cli).unwrap();
    let after = statuses(&cfg, &path, &key);
    let lan_after = after
        .iter()
        .find(|s| s.listener.alias == "osc_lan")
        .unwrap();
    assert!(lan_after.approved);

    assert!(deny(&cfg, &path, &key, "osc_lan").unwrap());
    let denied = statuses(&cfg, &path, &key);
    assert!(
        !denied
            .iter()
            .find(|s| s.listener.alias == "osc_lan")
            .unwrap()
            .approved
    );
}

#[test]
fn loopback_listener_is_always_approved_without_a_record() {
    let cfg = sample_config();
    let (_g, path) = registry_path();
    let key = HmacKey::from_bytes([9u8; 32]);

    // approve() on a loopback listener is a no-op that still succeeds.
    approve(&cfg, &path, &key, "osc_local", ApprovingSurface::Cli).unwrap();
    let st = statuses(&cfg, &path, &key);
    let local = st.iter().find(|s| s.listener.alias == "osc_local").unwrap();
    assert!(local.approved);
    // No registry file needed to exist for loopback approval.
    assert!(!path.exists() || ApprovalRegistry::load(&path, &key).is_ok());
}

#[test]
fn acl_change_drops_approval_in_status() {
    let cfg = sample_config();
    let (_g, path) = registry_path();
    let key = HmacKey::from_bytes([9u8; 32]);
    approve(&cfg, &path, &key, "osc_lan", ApprovingSurface::Cli).unwrap();

    // A config whose osc_lan ACL changed → its approval_key differs → not approved.
    let mut changed = cfg;
    for ep in &mut changed.endpoints {
        if ep.alias == "osc_lan"
            && let conductor_core::config::types::EndpointKind::OscEndpoint { security, .. } =
                &mut ep.kind
        {
            security.network_acl = vec!["10.0.0.0/8".into()];
        }
    }
    let st = statuses(&changed, &path, &key);
    assert!(
        !st.iter()
            .find(|s| s.listener.alias == "osc_lan")
            .unwrap()
            .approved,
        "an ACL change must invalidate the approval"
    );
}

#[test]
fn tampered_registry_reads_as_unapproved_fail_closed() {
    let cfg = sample_config();
    let (_g, path) = registry_path();
    let key = HmacKey::from_bytes([9u8; 32]);
    approve(&cfg, &path, &key, "osc_lan", ApprovingSurface::Cli).unwrap();

    // Verify with a *different* key → load fails → fail-closed.
    let wrong = HmacKey::from_bytes([2u8; 32]);
    let st = statuses(&cfg, &path, &wrong);
    let lan = st.iter().find(|s| s.listener.alias == "osc_lan").unwrap();
    assert!(lan.registry_tampered);
    assert!(!lan.approved);
    // Loopback still reads approved even under a tampered registry.
    assert!(
        st.iter()
            .find(|s| s.listener.alias == "osc_local")
            .unwrap()
            .approved
    );
}

#[test]
fn approving_an_amplifying_listener_sets_the_amplification_ack() {
    // Approving an Art-Net allow_broadcast listener must also satisfy the D11
    // amplification gate — otherwise the bind gate would re-prompt with no CLI
    // way to acknowledge it.
    use crate::security::network_approvals::ApprovalDecision;
    let cfg = sample_config();
    let (_g, path) = registry_path();
    let key = HmacKey::from_bytes([9u8; 32]);

    let info = approve(&cfg, &path, &key, "artnet_b", ApprovingSurface::Cli).unwrap();
    assert!(info.requires_amplification_ack);

    let registry = ApprovalRegistry::load(&path, &key).unwrap();
    // requires_amplification_ack = true → still Approved (ack was set).
    assert_eq!(
        registry.decision(&info.approval_key(), false, true),
        ApprovalDecision::Approved,
        "an approved amplifying listener must not re-prompt for amplification"
    );
}

#[test]
fn deny_loopback_is_a_noop() {
    // Symmetric with approve(): denying a loopback listener is a no-op (it is
    // always auto-approved) and must not touch the registry.
    let cfg = sample_config();
    let (_g, path) = registry_path();
    let key = HmacKey::from_bytes([9u8; 32]);
    assert!(!deny(&cfg, &path, &key, "osc_local").unwrap());
    assert!(
        !path.exists(),
        "deny on loopback must not create a registry"
    );
}

// --- rotation -------------------------------------------------------------

#[test]
fn rotate_with_no_registry_succeeds_and_writes_nothing() {
    let (_g, path) = registry_path();
    let keychain = MockKeychain::new();
    let before = keychain.get_or_create_hmac_key().unwrap().fingerprint();
    let fp = rotate_hmac(&keychain, &path).unwrap();
    assert_ne!(before, fp, "the key still rotates");
    assert!(
        !path.exists(),
        "no registry should be created out of nothing"
    );
}

#[test]
fn rotate_does_not_resurrect_a_tampered_registry() {
    let cfg = sample_config();
    let (_g, path) = registry_path();
    let keychain = MockKeychain::new();
    let key = keychain.get_or_create_hmac_key().unwrap();
    approve(&cfg, &path, &key, "osc_lan", ApprovingSurface::Cli).unwrap();

    // Corrupt the registry on disk (still 0600) so it no longer verifies.
    std::fs::write(&path, br#"{"alg":"hmac-sha256","data":"{}","mac":"00"}"#).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

    let new_fp = rotate_hmac(&keychain, &path).unwrap();
    let new_key = keychain.get_or_create_hmac_key().unwrap();
    assert_eq!(new_fp, new_key.fingerprint());
    // The tampered file was NOT re-signed into a valid one under the new key.
    let st = statuses(&cfg, &path, &new_key);
    let lan = st.iter().find(|s| s.listener.alias == "osc_lan").unwrap();
    assert!(
        !lan.approved,
        "a tampered registry must not survive rotation"
    );
}

#[test]
fn rotate_hmac_preserves_approvals_under_the_new_key() {
    let cfg = sample_config();
    let (_g, path) = registry_path();
    let keychain = MockKeychain::new();
    let key = keychain.get_or_create_hmac_key().unwrap();

    approve(&cfg, &path, &key, "osc_lan", ApprovingSurface::Cli).unwrap();

    let new_fp = rotate_hmac(&keychain, &path).unwrap();
    let new_key = keychain.get_or_create_hmac_key().unwrap();
    assert_eq!(new_fp, new_key.fingerprint());
    assert_ne!(key.fingerprint(), new_key.fingerprint());

    // The registry was re-signed: it loads + the approval survives under the new key.
    let st = statuses(&cfg, &path, &new_key);
    assert!(
        st.iter()
            .find(|s| s.listener.alias == "osc_lan")
            .unwrap()
            .approved,
        "a routine rotation must preserve manual approvals"
    );
    // The OLD key no longer verifies it.
    assert!(ApprovalRegistry::load(&path, &key).is_err());
}
