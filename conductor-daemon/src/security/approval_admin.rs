// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-042 Phase B-early Slice B.4 — `conductorctl listener`/`security`
//! administration logic.
//!
//! The CLI surface (`conductorctl listener approve/deny/list/status`,
//! `conductorctl security rotate-hmac`) is thin; the logic lives here so it is
//! unit-testable without spawning the binary. It resolves a listener's
//! [`ApprovalKey`] from the daemon config (alias → host/port/acl_hash) and
//! manipulates the HMAC-signed [`ApprovalRegistry`] file directly.
//!
//! Direct-file (not IPC): Phase B-early is manual approval, and the daemon does
//! not yet hold the registry as live state (the listener-bind gate is a later
//! slice). The daemon reads the registry at bind time, so an approval written
//! here is honoured on the next (re)bind. The spec's IPC surface is deferred
//! with that bind-gate wiring.

use std::path::Path;

use conductor_core::config::types::{Config, ConnectorDirection, EndpointKind};
use conductor_core::security::NetworkAcl;
use conductor_core::security::keychain::{HmacKey, KeychainStore};

use super::network_approvals::{ApprovalKey, ApprovalRegistry, ApprovingSurface, RegistryError};

/// Failures from the approval-admin operations.
#[derive(Debug, thiserror::Error)]
pub enum AdminError {
    /// No endpoint with this alias, or it is not a network listener.
    #[error("no network listener with alias '{0}'")]
    NoSuchListener(String),
    /// Registry load/save failure.
    #[error(transparent)]
    Registry(#[from] RegistryError),
}

/// A network listener resolved from config, with the data needed to key and
/// classify an approval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenerInfo {
    /// Listener alias.
    pub alias: String,
    /// Bind host.
    pub host: String,
    /// Bind port.
    pub port: u16,
    /// ACL allow-list entries (CIDRs).
    pub acl_entries: Vec<String>,
    /// Loopback host → auto-approved (never needs a registry entry).
    pub is_loopback: bool,
    /// Art-Net `allow_broadcast` listener → needs an amplification ack (D11).
    pub requires_amplification_ack: bool,
}

impl ListenerInfo {
    /// The approval key for this listener (binds the approval to its ACL).
    pub fn approval_key(&self) -> ApprovalKey {
        ApprovalKey::for_listener(&self.alias, &self.host, self.port, &self.acl_entries)
    }
}

fn host_is_loopback(host: &str) -> bool {
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| NetworkAcl::is_loopback_address(&ip))
}

/// Enumerate every network listener (OSC / Art-Net inbound endpoint) in config.
pub fn list_listeners(config: &Config) -> Vec<ListenerInfo> {
    config
        .endpoints
        .iter()
        .filter(|ep| {
            matches!(
                ep.direction,
                ConnectorDirection::Input | ConnectorDirection::Bidirectional
            )
        })
        .filter_map(|ep| {
            let (host, port, security, allow_broadcast) = match &ep.kind {
                EndpointKind::OscEndpoint {
                    host,
                    port,
                    security,
                } => (host.clone(), *port, security, false),
                EndpointKind::ArtNetEndpoint {
                    host,
                    port,
                    security,
                    allow_broadcast,
                    ..
                } => (host.clone(), *port, security, *allow_broadcast),
                _ => return None,
            };
            Some(ListenerInfo {
                alias: ep.alias.clone(),
                is_loopback: host_is_loopback(&host),
                requires_amplification_ack: allow_broadcast,
                host,
                port,
                acl_entries: security.network_acl.clone(),
            })
        })
        .collect()
}

/// Resolve a single listener by alias.
pub fn resolve_listener(config: &Config, alias: &str) -> Result<ListenerInfo, AdminError> {
    list_listeners(config)
        .into_iter()
        .find(|l| l.alias == alias)
        .ok_or_else(|| AdminError::NoSuchListener(alias.to_string()))
}

/// Per-listener approval status for `list` / `status` output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenerStatus {
    /// The resolved listener.
    pub listener: ListenerInfo,
    /// Whether a verified approval exists (loopback listeners are always true).
    pub approved: bool,
    /// True when the registry on disk failed HMAC verification (fail-closed:
    /// all listeners read as un-approved until re-approved).
    pub registry_tampered: bool,
}

/// Compute the approval status of every network listener.
///
/// `ApprovalRegistry::load` returns an empty registry for an absent file (first
/// run) — that is *not* tampering, just "nothing approved yet". `registry_tampered`
/// is set only for an **integrity** failure (HMAC mismatch / bad alg / corrupt /
/// insecure file); a transient I/O error is still fail-closed (nothing approved)
/// but not mislabelled as tampering.
pub fn statuses(config: &Config, registry_path: &Path, key: &HmacKey) -> Vec<ListenerStatus> {
    let (registry, tampered) = match ApprovalRegistry::load(registry_path, key) {
        Ok(r) => (Some(r), false),
        Err(e) => {
            let integrity_failure = matches!(
                e,
                RegistryError::MacMismatch
                    | RegistryError::BadAlg
                    | RegistryError::Parse(_)
                    | RegistryError::InsecurePermissions { .. }
            );
            // Fail-closed either way (no registry → nothing approved); only flag
            // tampering for an integrity failure, not a transient I/O error.
            (None, integrity_failure)
        }
    };
    list_listeners(config)
        .into_iter()
        .map(|listener| {
            let approved = if listener.is_loopback {
                true
            } else {
                registry
                    .as_ref()
                    .is_some_and(|r| r.listener_is_approved(&listener.approval_key()))
            };
            ListenerStatus {
                listener,
                approved,
                registry_tampered: tampered,
            }
        })
        .collect()
}

/// Approve a listener by alias. Loopback listeners are auto-approved (no-op).
/// Re-approving an already-approved listener simply refreshes its record.
///
/// For an **amplifying** listener (Art-Net `allow_broadcast`), approving it is
/// also the operator acknowledging the D11 amplification risk — so the
/// amplification ack is set here, otherwise the bind gate would re-prompt and an
/// operator would have no CLI way to satisfy it.
pub fn approve(
    config: &Config,
    registry_path: &Path,
    key: &HmacKey,
    alias: &str,
    surface: ApprovingSurface,
) -> Result<ListenerInfo, AdminError> {
    let listener = resolve_listener(config, alias)?;
    if listener.is_loopback {
        return Ok(listener); // loopback is auto-approved; nothing to record
    }
    let approval_key = listener.approval_key();
    let mut registry = ApprovalRegistry::load(registry_path, key)?;
    registry.add_listener_approval(&approval_key, surface);
    if listener.requires_amplification_ack {
        registry.acknowledge_amplification(&approval_key);
    }
    registry.save(registry_path, key)?;
    Ok(listener)
}

/// Remove a listener's approval. Returns whether one was present.
pub fn deny(
    config: &Config,
    registry_path: &Path,
    key: &HmacKey,
    alias: &str,
) -> Result<bool, AdminError> {
    let listener = resolve_listener(config, alias)?;
    // Symmetric with `approve`: a loopback listener is always auto-approved and
    // has no registry record, so there is nothing to deny (don't even touch the
    // registry / keychain).
    if listener.is_loopback {
        return Ok(false);
    }
    let mut registry = ApprovalRegistry::load(registry_path, key)?;
    let removed = registry.remove_listener_approval(&listener.approval_key());
    if removed {
        registry.save(registry_path, key)?;
    }
    Ok(removed)
}

/// D12 HMAC key rotation. Rotates the keychain key and **re-signs** the existing
/// verified registry under the new key so manual approvals survive a routine
/// rotation. Returns the new key fingerprint.
///
/// Behaviour:
/// - **Verified existing registry** → re-signed under the new key (approvals
///   preserved).
/// - **No registry yet** → nothing to re-sign.
/// - **Tampered/unverifiable registry** → *not* re-signed; it stays fail-closed
///   under the new key too, so the operator re-approves (no silent overwrite of
///   an untrusted file).
///
/// **Atomicity caveat:** the keychain and the on-disk registry are two separate
/// stores, so the rotate-then-re-sign step is not a single atomic transaction.
/// If the process dies *between* `rotate_hmac_key()` and the re-sign (or the
/// re-sign errors — which is propagated), the new key is live but the registry
/// is still signed with the old key, so it reads as un-verifiable. This is
/// **fail-closed**: the worst case is that the operator must re-approve, never a
/// silent acceptance of a stale/forged approval. Rotation is a deliberate,
/// infrequent operator action, so this narrow window is acceptable.
pub fn rotate_hmac(
    keychain: &dyn KeychainStore,
    registry_path: &Path,
) -> Result<String, RegistryError> {
    // Capture the current verified registry *before* rotating. Only an existing,
    // verified file is preserved (a missing file has nothing to preserve; a
    // tampered file is deliberately left for re-approval).
    let preserved = if registry_path.exists() {
        let old_key = keychain
            .get_or_create_hmac_key()
            .map_err(|e| RegistryError::Io(format!("keychain: {e}")))?;
        ApprovalRegistry::load(registry_path, &old_key).ok()
    } else {
        None
    };

    let new_key = keychain
        .rotate_hmac_key()
        .map_err(|e| RegistryError::Io(format!("keychain rotate: {e}")))?;

    if let Some(registry) = preserved {
        // Re-sign the preserved approvals under the new key (error propagates so
        // the operator learns a rotation half-completed).
        registry.save(registry_path, &new_key)?;
    }
    Ok(new_key.fingerprint())
}

#[cfg(test)]
#[path = "approval_admin_tests.rs"]
mod tests;
