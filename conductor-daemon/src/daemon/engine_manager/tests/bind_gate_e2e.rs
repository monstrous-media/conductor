// Copyright 2026 Amiable
// SPDX-License-Identifier: MIT

//! ADR-042 Phase B-early — Slice B.8-early end-to-end bind-gate test (#1899).
//!
//! The Phase B-early MERGE GATE: drives the full lifecycle through the real
//! `EngineManager` bind path with an injected mock keychain (a fixed key — never
//! the OS keychain) and a real on-disk approval registry:
//!
//! 1. A non-loopback listener with no approval stays **unbound** (withheld).
//! 2. `approval_admin::approve` writes an HMAC-signed registry entry; on the
//!    next bind/reload the listener **binds**.
//! 3. Changing the listener's `network_acl` invalidates the approval (the
//!    `acl_hash` changes) → it is withheld again (fail-closed).
//!
//! Loopback binding, UDP accept/drop at the edge, and the action-class gate are
//! covered by `phase_a_e2e_loopback_osc_binds_accepts_and_audits`,
//! `listener_runtime_test`, and `action_class_gate_test` respectively; this gate
//! adds the approval round-trip the bind gate introduces.

#![cfg(unix)]

use super::*;
use crate::security::{ApprovingSurface, KeychainProvider, NetworkBindGate, approval_admin};
use conductor_core::security::keychain::{HmacKey, KeyMetadata, KeychainError, KeychainStore};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

/// The fixed key the mock keychain hands out (and that approvals are signed
/// with). A healthy 1-day age, with metadata fingerprint matching the key as
/// `init_keychain_with`'s read-consistency check requires.
fn e2e_key() -> HmacKey {
    HmacKey::from_bytes([5u8; 32])
}

struct WorkingKeychain;
// Full `std::result::Result` throughout — `super::*` pulls in the daemon's
// `Result<T>` alias which would shadow the two-argument form.
impl KeychainStore for WorkingKeychain {
    fn get_or_create_hmac_key(&self) -> std::result::Result<HmacKey, KeychainError> {
        Ok(e2e_key())
    }
    fn rotate_hmac_key(&self) -> std::result::Result<HmacKey, KeychainError> {
        HmacKey::generate()
    }
    fn key_metadata(&self) -> std::result::Result<KeyMetadata, KeychainError> {
        Ok(KeyMetadata {
            fingerprint: e2e_key().fingerprint(),
            // Keep created_at consistent with age_days = 1 (one day ago).
            created_at: SystemTime::now()
                .checked_sub(Duration::from_secs(86_400))
                .unwrap_or(SystemTime::UNIX_EPOCH),
            created_at_monotonic: Instant::now(),
            age_days: 1,
        })
    }
}

struct WorkingProvider;
impl KeychainProvider for WorkingProvider {
    // Full `std::result::Result` — `super::*` pulls in the daemon `Result<T>` alias.
    fn keychain(&self) -> std::result::Result<Box<dyn KeychainStore>, KeychainError> {
        Ok(Box::new(WorkingKeychain))
    }
}

/// Build a non-loopback OSC listener config with the given `network_acl`.
/// `host = "0.0.0.0"` is bindable (INADDR_ANY) and non-loopback, so it is gated;
/// `port = 0` lets the OS assign an ephemeral port on bind.
fn lan_listener_config(acl: &str) -> Config {
    let toml = format!(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "osc_lan"
direction = "Input"
type = "OscEndpoint"
host = "0.0.0.0"
port = 0
allow_network = true
network_acl = [{acl}]
"#
    );
    toml::from_str(&toml).expect("config parses")
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new (Enigo) requires display server
async fn b8_early_withheld_then_approved_then_acl_change_reinvalidates() {
    let tmp = tempfile::tempdir().unwrap();
    let registry_path = tmp.path().join("sec").join("network_approvals.json");
    let lock_dir = tmp.path().join("security");

    let config = lan_listener_config(r#""192.168.1.0/24""#);

    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);
    let mut mgr = EngineManager::new(
        config.clone(),
        tmp.path().join("config.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("engine manager builds");

    // Inject a gate backed by a working mock keychain + the on-disk registry the
    // test also writes approvals to (the key the gate reads == the signing key).
    let gate = NetworkBindGate::new(Arc::new(WorkingProvider), registry_path.clone(), lock_dir);
    mgr.set_network_bind_gate(Arc::new(gate));

    // 1. No approval → the non-loopback listener is WITHHELD (stays unbound).
    mgr.start_network_listeners(&config)
        .await
        .expect("start ok");
    assert!(
        mgr.network_listener_status().is_empty(),
        "non-loopback listener must be withheld before approval: {:?}",
        mgr.network_listener_status()
    );

    // 2. Approve it (writes an HMAC-signed registry entry under the e2e key).
    approval_admin::approve(
        &config,
        &registry_path,
        &e2e_key(),
        "osc_lan",
        ApprovingSurface::Cli,
    )
    .expect("approve writes the registry");

    // Re-bind (as a reload would): the gate re-reads the registry → APPROVED →
    // the listener binds.
    mgr.start_network_listeners(&config)
        .await
        .expect("start ok");
    let status = mgr.network_listener_status();
    assert_eq!(status.len(), 1, "approved listener binds: {status:?}");
    assert_eq!(status[0].0, "osc_lan");

    // 3. Change the ACL (widen it). The approval was keyed on the old acl_hash,
    //    so it no longer matches → the listener is withheld again (fail-closed).
    let widened = lan_listener_config(r#""192.168.1.0/24", "10.0.0.0/8""#);
    mgr.start_network_listeners(&widened)
        .await
        .expect("start ok");
    assert!(
        mgr.network_listener_status().is_empty(),
        "an ACL change must invalidate the approval (fail-closed): {:?}",
        mgr.network_listener_status()
    );

    // ...and the withholding is keyed on the ACL specifically (not a bind-loop
    // teardown artifact): approving the WIDENED config binds the same listener.
    approval_admin::approve(
        &widened,
        &registry_path,
        &e2e_key(),
        "osc_lan",
        ApprovingSurface::Cli,
    )
    .expect("approve the widened config");
    mgr.start_network_listeners(&widened)
        .await
        .expect("start ok");
    assert_eq!(
        mgr.network_listener_status().len(),
        1,
        "the widened ACL binds once its own approval exists: {:?}",
        mgr.network_listener_status()
    );

    mgr.stop_network_listeners();
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new (Enigo) requires display server
async fn b8_early_registry_tamper_invalidates_approval() {
    // approve + bind, then tamper the signed registry's MAC → on the next bind
    // the HMAC verify fails and ALL approvals are invalidated (fail-closed; the
    // daemon never falls back to the prior approval).
    let tmp = tempfile::tempdir().unwrap();
    let registry_path = tmp.path().join("sec").join("network_approvals.json");
    let lock_dir = tmp.path().join("security");
    let config = lan_listener_config(r#""192.168.1.0/24""#);

    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);
    let mut mgr = EngineManager::new(
        config.clone(),
        tmp.path().join("config.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("engine manager builds");
    let gate = NetworkBindGate::new(Arc::new(WorkingProvider), registry_path.clone(), lock_dir);
    mgr.set_network_bind_gate(Arc::new(gate));

    approval_admin::approve(
        &config,
        &registry_path,
        &e2e_key(),
        "osc_lan",
        ApprovingSurface::Cli,
    )
    .expect("approve writes the registry");
    mgr.start_network_listeners(&config)
        .await
        .expect("start ok");
    assert_eq!(
        mgr.network_listener_status().len(),
        1,
        "approved listener binds before tamper"
    );

    // Tamper: flip one hex char of the envelope `mac` (valid JSON, wrong MAC) →
    // MacMismatch on verify.
    let mut env: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&registry_path).unwrap()).unwrap();
    let mut mac: Vec<char> = env["mac"].as_str().unwrap().chars().collect();
    let last = mac.len() - 1;
    mac[last] = if mac[last] == '0' { '1' } else { '0' };
    env["mac"] = serde_json::Value::String(mac.into_iter().collect());
    std::fs::write(&registry_path, serde_json::to_vec(&env).unwrap()).unwrap();

    mgr.start_network_listeners(&config)
        .await
        .expect("start ok");
    assert!(
        mgr.network_listener_status().is_empty(),
        "a tampered registry must invalidate the approval (fail-closed): {:?}",
        mgr.network_listener_status()
    );

    mgr.stop_network_listeners();
}
