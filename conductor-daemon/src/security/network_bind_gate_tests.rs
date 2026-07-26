// Copyright 2026 Amiable
// SPDX-License-Identifier: MIT

//! Unit tests for the ADR-042 Phase B-early network bind gate (#1899). In a
//! `#[path]` sibling so the gate module stays within the Council verify budget.
//! All tests use an injected mock keychain — never the real OS keychain (which
//! can block on a macOS access prompt).

use super::*;
use conductor_core::security::keychain::{KeyMetadata, KeychainError, KeychainStore};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use crate::security::network_approvals::{ApprovalRegistry, ApprovingSurface};

/// Fixed key the mock keychain hands out. Its metadata fingerprint matches
/// (`init_keychain_with`'s read-consistency check requires it).
fn mock_key() -> HmacKey {
    HmacKey::from_bytes([7u8; 32])
}

/// Mock keychain: fixed key, configurable age (drives hard-expiry).
struct MockKeychain {
    age_days: u64,
}
impl KeychainStore for MockKeychain {
    fn get_or_create_hmac_key(&self) -> Result<HmacKey, KeychainError> {
        Ok(mock_key())
    }
    fn rotate_hmac_key(&self) -> Result<HmacKey, KeychainError> {
        HmacKey::generate()
    }
    fn key_metadata(&self) -> Result<KeyMetadata, KeychainError> {
        Ok(KeyMetadata {
            fingerprint: mock_key().fingerprint(),
            created_at: SystemTime::now()
                .checked_sub(Duration::from_secs(self.age_days * 86_400))
                .unwrap_or(SystemTime::UNIX_EPOCH),
            created_at_monotonic: Instant::now(),
            age_days: self.age_days,
        })
    }
}

/// Provider handing out a working mock keychain; counts reads so the cache (one
/// keychain read for the daemon's life) can be asserted.
struct MockProvider {
    age_days: u64,
    calls: Arc<AtomicUsize>,
}
impl KeychainProvider for MockProvider {
    fn keychain(&self) -> Result<Box<dyn KeychainStore>, KeychainError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(MockKeychain {
            age_days: self.age_days,
        }))
    }
}

/// Provider whose keychain backend is unavailable.
struct UnavailableProvider;
impl KeychainProvider for UnavailableProvider {
    fn keychain(&self) -> Result<Box<dyn KeychainStore>, KeychainError> {
        Err(KeychainError::Backend("test: keychain unavailable".into()))
    }
}

struct Fixture {
    _tmp: tempfile::TempDir,
    registry_path: PathBuf,
    lock_dir: PathBuf,
}
fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path();
    Fixture {
        // Registry under a `sec` subdir that `save`/`ensure_secure_dir` creates
        // 0700 (mirrors approval_admin_tests so the load hardening check passes).
        registry_path: base.join("sec").join("network_approvals.json"),
        lock_dir: base.join("security"),
        _tmp: tmp,
    }
}

fn gate(provider: Arc<dyn KeychainProvider>, fx: &Fixture) -> NetworkBindGate {
    NetworkBindGate::new(provider, fx.registry_path.clone(), fx.lock_dir.clone())
}

fn working_provider(age_days: u64) -> Arc<dyn KeychainProvider> {
    Arc::new(MockProvider {
        age_days,
        calls: Arc::new(AtomicUsize::new(0)),
    })
}

fn lan_edge<'a>(alias: &'a str, acl: &'a [String]) -> GateEdge<'a> {
    GateEdge {
        alias,
        host: "192.168.1.10",
        port: 9000,
        acl_entries: acl,
        requires_amplification_ack: false,
    }
}

/// Write an HMAC-signed approval for one listener under the mock key.
fn write_approved(fx: &Fixture, alias: &str, host: &str, port: u16, acl: &[String]) {
    let mut reg = ApprovalRegistry::new();
    let key = ApprovalKey::for_listener(alias, host, port, acl);
    reg.add_listener_approval(&key, ApprovingSurface::Cli);
    reg.save(&fx.registry_path, &mock_key()).unwrap();
}

#[test]
fn empty_edges_never_touch_keychain() {
    let calls = Arc::new(AtomicUsize::new(0));
    let fx = fixture();
    let g = gate(
        Arc::new(MockProvider {
            age_days: 10,
            calls: calls.clone(),
        }),
        &fx,
    );
    assert!(g.evaluate(&[]).is_empty());
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "no edges → no keychain read"
    );
}

#[test]
fn loopback_edge_binds_without_approval() {
    // Defence-in-depth: even if a loopback edge reaches the gate (the caller
    // normally pre-filters), the gate computes loopback-ness from the host and
    // auto-approves it — no registry entry needed.
    let fx = fixture();
    let acl = vec!["127.0.0.0/8".to_string()];
    let g = gate(working_provider(10), &fx);
    let edge = GateEdge {
        alias: "osc_lo",
        host: "127.0.0.1",
        port: 9000,
        acl_entries: &acl,
        requires_amplification_ack: false,
    };
    assert_eq!(g.evaluate(&[edge]), vec![BindVerdict::Bind]);
}

#[test]
fn no_approval_withholds_awaiting() {
    let fx = fixture();
    let acl = vec!["192.168.1.0/24".to_string()];
    let g = gate(working_provider(10), &fx);
    assert_eq!(
        g.evaluate(&[lan_edge("osc_in", &acl)]),
        vec![BindVerdict::Withhold(WithholdReason::AwaitingApproval)]
    );
}

#[test]
fn approved_listener_binds() {
    let fx = fixture();
    let acl = vec!["192.168.1.0/24".to_string()];
    write_approved(&fx, "osc_in", "192.168.1.10", 9000, &acl);
    let g = gate(working_provider(10), &fx);
    assert_eq!(
        g.evaluate(&[lan_edge("osc_in", &acl)]),
        vec![BindVerdict::Bind]
    );
}

#[test]
fn widening_acl_invalidates_approval() {
    let fx = fixture();
    let narrow = vec!["192.168.1.0/24".to_string()];
    write_approved(&fx, "osc_in", "192.168.1.10", 9000, &narrow);
    // The operator (attacker) widens the ACL → different acl_hash → key miss.
    let wide = vec!["192.168.1.0/24".to_string(), "10.0.0.0/8".to_string()];
    let g = gate(working_provider(10), &fx);
    assert_eq!(
        g.evaluate(&[lan_edge("osc_in", &wide)]),
        vec![BindVerdict::Withhold(WithholdReason::AwaitingApproval)],
        "widening the ACL must invalidate the approval (fail-closed)"
    );
}

#[test]
fn tampered_registry_fails_closed() {
    // Sign the registry with a DIFFERENT key than the gate uses → the gate's HMAC
    // verify fails (MacMismatch, a genuine forgery signal) → RegistryTampered.
    let fx = fixture();
    let acl = vec!["192.168.1.0/24".to_string()];
    let mut reg = ApprovalRegistry::new();
    let key = ApprovalKey::for_listener("osc_in", "192.168.1.10", 9000, &acl);
    reg.add_listener_approval(&key, ApprovingSurface::Cli);
    let wrong_key = HmacKey::from_bytes([9u8; 32]);
    reg.save(&fx.registry_path, &wrong_key).unwrap();

    let g = gate(working_provider(10), &fx);
    assert_eq!(
        g.evaluate(&[lan_edge("osc_in", &acl)]),
        vec![BindVerdict::Withhold(WithholdReason::RegistryTampered)]
    );
}

#[test]
fn unreadable_registry_fails_closed_not_tampered() {
    // A structurally-corrupt (truncated) registry is a *parse* failure, not a
    // forged MAC — it must fail closed as `RegistryUnreadable`, not be mislabelled
    // as tamper (Council reasoning-tier review).
    let fx = fixture();
    let acl = vec!["192.168.1.0/24".to_string()];
    write_approved(&fx, "osc_in", "192.168.1.10", 9000, &acl);
    let bytes = std::fs::read(&fx.registry_path).unwrap();
    std::fs::write(&fx.registry_path, &bytes[..bytes.len() / 2]).unwrap();

    let g = gate(working_provider(10), &fx);
    assert_eq!(
        g.evaluate(&[lan_edge("osc_in", &acl)]),
        vec![BindVerdict::Withhold(WithholdReason::RegistryUnreadable)]
    );
}

#[test]
fn unavailable_keychain_fails_closed() {
    let fx = fixture();
    let acl = vec!["192.168.1.0/24".to_string()];
    let g = gate(Arc::new(UnavailableProvider), &fx);
    assert_eq!(
        g.evaluate(&[lan_edge("osc_in", &acl)]),
        vec![BindVerdict::Withhold(WithholdReason::KeychainUnavailable)]
    );
}

#[test]
fn hard_expired_key_fails_closed() {
    let fx = fixture();
    let acl = vec!["192.168.1.0/24".to_string()];
    let g = gate(working_provider(731), &fx);
    assert_eq!(
        g.evaluate(&[lan_edge("osc_in", &acl)]),
        vec![BindVerdict::Withhold(WithholdReason::KeychainExpired)]
    );
}

#[test]
fn key_is_read_once_and_cached_across_reloads() {
    let fx = fixture();
    let acl = vec!["192.168.1.0/24".to_string()];
    let calls = Arc::new(AtomicUsize::new(0));
    let g = gate(
        Arc::new(MockProvider {
            age_days: 10,
            calls: calls.clone(),
        }),
        &fx,
    );
    let _ = g.evaluate(&[lan_edge("a", &acl)]);
    let _ = g.evaluate(&[lan_edge("b", &acl)]); // simulates a config reload
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "keychain read once; a reload reuses the cached key"
    );
}

#[test]
fn multiple_edges_decided_independently() {
    let fx = fixture();
    let acl = vec!["192.168.1.0/24".to_string()];
    write_approved(&fx, "approved", "192.168.1.10", 9000, &acl);
    let g = gate(working_provider(10), &fx);
    assert_eq!(
        g.evaluate(&[lan_edge("approved", &acl), lan_edge("pending", &acl)]),
        vec![
            BindVerdict::Bind,
            BindVerdict::Withhold(WithholdReason::AwaitingApproval),
        ]
    );
}

#[test]
fn withhold_reasons_have_distinct_audit_summaries() {
    use WithholdReason::*;
    let all = [
        AwaitingApproval,
        RegistryTampered,
        RegistryUnreadable,
        KeychainUnavailable,
        KeychainExpired,
    ];
    let unique: std::collections::HashSet<_> = all.iter().map(|r| r.audit_summary()).collect();
    assert_eq!(
        unique.len(),
        all.len(),
        "each reason → distinct audit summary"
    );
    assert!(
        AwaitingApproval
            .operator_message("osc_in")
            .contains("conductorctl listener approve osc_in")
    );
}
