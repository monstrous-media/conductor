// Copyright 2026 Amiable
// SPDX-License-Identifier: MIT

//! ADR-042 Phase B-early network bind gate (#1899).
//!
//! Decides whether a **non-loopback** OSC/Art-Net listener may bind, based on
//! the HMAC-signed approval registry (`~/.conductor/network_approvals.json`,
//! key in the OS keychain). Loopback listeners never reach this gate — the
//! caller binds them unconditionally (Phase A behaviour). Every failure mode is
//! **fail-closed**: a withheld listener stays unbound and the daemon never
//! falls back to a cached or assumed approval.
//!
//! Design follows the LLM Council reasoning-tier review of the bind-gate plan:
//! - **Provider-trait keychain seam.** The provider *inside* the gate is a
//!   non-optional `Arc<dyn KeychainProvider>` — there is no `Option` on the seam
//!   that a `None` could fail-open through. Production lazily runs
//!   `select_keychain()`; tests inject deterministic unavailable / expired /
//!   working states without touching the OS keychain (which can block on a macOS
//!   access prompt). The *gate itself* may be absent — [`NetworkBindGate::platform`]
//!   returns `None` when no home dir resolves, and the engine manager holds it as
//!   `Option` — but every absent-gate case is handled **fail-closed** by the
//!   caller (all non-loopback listeners are withheld), so the optionality at that
//!   layer never opens a bind.
//! - **Cache the key, re-read the registry.** The OS keychain is read **once**
//!   (lazily, on the first non-loopback bind) and the key + its creation time
//!   are cached for the daemon's life. On each reload the registry is re-read
//!   and re-verified from disk (catching tamper), and the 730-day expiry is
//!   re-evaluated **in memory** from the cached creation time — so the control
//!   holds without re-hitting the keychain (no repeated DBus block / macOS
//!   prompt on reload).
//! - **Distinct fail-closed reasons** (keychain-unavailable vs keychain-expired
//!   vs registry-tampered vs awaiting-approval) for forensic audit + a prominent
//!   operator warning.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use conductor_core::security::NetworkAcl;
use conductor_core::security::keychain::{HmacKey, KeychainError, KeychainStore, select_keychain};

use super::keychain_init::{KeychainInitError, init_keychain_with};
use super::network_approvals::{ApprovalDecision, ApprovalKey, ApprovalRegistry, RegistryError};

/// Classify an [`ApprovalRegistry::load`] error as **tamper** vs merely
/// **unreadable** (both fail-closed; the split only sharpens the audit trail).
///
/// Tamper = a cryptographic-integrity or file-security violation: a failed MAC,
/// a wrong/forbidden algorithm, or an insecure file (symlink / wrong owner /
/// wrong mode). A *parse* failure is **not** tamper — an attacker who can't
/// forge the MAC can't produce an authenticated-but-malformed payload, and a
/// post-verification parse error means authentic-yet-corrupt bytes (e.g. a
/// version mismatch). So a parse error (like a transient I/O error) is
/// "unreadable", not "tampered".
fn is_integrity_failure(e: &RegistryError) -> bool {
    matches!(
        e,
        RegistryError::MacMismatch
            | RegistryError::BadAlg
            | RegistryError::InsecurePermissions { .. }
    )
}

/// 730-day hard expiry, mirroring `keychain_init`'s hard-expiry bound. Applied
/// on the first keychain read by `init_keychain_with`, then re-evaluated
/// in-memory on every subsequent reload from the cached key-creation time.
const HARD_EXPIRY: Duration = Duration::from_secs(730 * 24 * 60 * 60);

/// Supplies the network-approval keychain. A trait (not a bare store) so
/// production lazily runs [`select_keychain`] while tests inject deterministic
/// states without ever touching the OS keychain.
pub trait KeychainProvider: Send + Sync {
    /// Acquire the keychain store. Production returns the platform keychain;
    /// `Err` is treated as [`WithholdReason::KeychainUnavailable`] (fail-closed).
    fn keychain(&self) -> Result<Box<dyn KeychainStore>, KeychainError>;
}

/// Production provider: the platform keychain, selected lazily on first use.
pub struct PlatformKeychainProvider;

impl KeychainProvider for PlatformKeychainProvider {
    fn keychain(&self) -> Result<Box<dyn KeychainStore>, KeychainError> {
        select_keychain()
    }
}

/// Why a non-loopback listener was withheld. Every variant is fail-closed (the
/// listener stays unbound); the discriminator drives the audit `summary` and a
/// prominent operator warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WithholdReason {
    /// No HMAC-verified approval on file — run `conductorctl listener approve`.
    AwaitingApproval,
    /// The approval registry failed HMAC verification (tamper) — all approvals
    /// invalidated, fail-closed re-prompt.
    RegistryTampered,
    /// The approval registry exists but could not be read (transient I/O error,
    /// not an integrity failure) — fail-closed until it reads cleanly.
    RegistryUnreadable,
    /// The keychain backend is unavailable (no key / Secret Service absent / I/O).
    KeychainUnavailable,
    /// The HMAC key is past the 730-day hard expiry — rotate via
    /// `conductorctl security rotate-hmac`.
    KeychainExpired,
}

impl WithholdReason {
    /// Stable discriminator for the `NetworkListenerApproval` audit `summary`.
    pub fn audit_summary(self) -> &'static str {
        match self {
            Self::AwaitingApproval => "awaiting_approval",
            Self::RegistryTampered => "registry_tampered",
            Self::RegistryUnreadable => "registry_unreadable",
            Self::KeychainUnavailable => "keychain_unavailable",
            Self::KeychainExpired => "keychain_expired",
        }
    }

    /// Prominent one-line operator warning (logged at WARN per withheld listener).
    pub fn operator_message(self, alias: &str) -> String {
        match self {
            Self::AwaitingApproval => format!(
                "network listener '{alias}' withheld pending approval — run \
                 `conductorctl listener approve {alias}` to bind it"
            ),
            Self::RegistryTampered => format!(
                "network listener '{alias}' withheld: the approval registry could not be \
                 verified (HMAC mismatch, bad algorithm, or an insecure file) — all network \
                 approvals are invalidated; re-approve via `conductorctl listener approve {alias}`"
            ),
            Self::RegistryUnreadable => format!(
                "network listener '{alias}' withheld: the approval registry could not be read \
                 (transient I/O error); it will bind once the registry reads cleanly"
            ),
            Self::KeychainUnavailable => format!(
                "network listener '{alias}' withheld: the approval keychain is unavailable; \
                 no non-loopback listener can bind until it is reachable"
            ),
            Self::KeychainExpired => format!(
                "network listener '{alias}' withheld: the approval HMAC key is past its \
                 730-day hard expiry — rotate via `conductorctl security rotate-hmac`"
            ),
        }
    }
}

/// Per-edge verdict from the gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindVerdict {
    /// Approved (or loopback handled by the caller) — bind it.
    Bind,
    /// Fail-closed — stay unbound for the given reason.
    Withhold(WithholdReason),
}

/// One non-loopback listener edge the gate evaluates. Borrows from the
/// already-validated in-memory config (no disk re-read at bind time — closes the
/// config-load→bind TOCTOU).
pub struct GateEdge<'a> {
    /// Listener alias (approval key component + audit/operator label).
    pub alias: &'a str,
    /// Bound host (IP literal; a concrete non-loopback address here).
    pub host: &'a str,
    /// Bound UDP port.
    pub port: u16,
    /// The listener's `network_acl` source CIDR strings (approval key component:
    /// widening the ACL changes the `acl_hash` → fail-closed `AwaitingApproval`).
    pub acl_entries: &'a [String],
    /// True for an Art-Net `allow_broadcast` amplifier: an approved record must
    /// additionally carry an unexpired D11 amplification ack, else re-prompt.
    pub requires_amplification_ack: bool,
}

/// The cached HMAC key + the key's OS-creation time, captured once on the first
/// keychain read. `created_at` drives the in-memory expiry re-evaluation.
struct CachedKey {
    key: HmacKey,
    created_at: SystemTime,
}

impl CachedKey {
    /// In-memory 730-day expiry check from the cached creation time. A
    /// backwards/unmeasurable clock (`duration_since` `Err`) is treated as
    /// **not** expired here: `init_keychain_with` already applied the
    /// fail-closed hard-expiry bound when the key was first read, and a clock
    /// rollback must not silently expire a key the operator hasn't aged out.
    fn is_hard_expired(&self, now: SystemTime) -> bool {
        now.duration_since(self.created_at)
            .map(|age| age >= HARD_EXPIRY)
            .unwrap_or(false)
    }
}

/// The Phase B-early network bind gate. Holds the keychain provider, the on-disk
/// registry + lock-dir paths, and the lazily-cached key.
pub struct NetworkBindGate {
    provider: Arc<dyn KeychainProvider>,
    registry_path: PathBuf,
    lock_dir: PathBuf,
    cached: Mutex<Option<CachedKey>>,
    /// Single-flight guard for the slow-path keychain read. Held (separately from
    /// `cached`) only while one thread performs the read, so concurrent first
    /// callers serialise here and the OS keychain is read **exactly once** — no
    /// duplicate access prompts — while fast-path cache reads stay unblocked.
    init_lock: Mutex<()>,
}

impl NetworkBindGate {
    /// Construct with an explicit provider + paths (tests inject a mock provider
    /// and tempdir paths; production uses [`Self::platform`]).
    pub fn new(
        provider: Arc<dyn KeychainProvider>,
        registry_path: PathBuf,
        lock_dir: PathBuf,
    ) -> Self {
        Self {
            provider,
            registry_path,
            lock_dir,
            cached: Mutex::new(None),
            init_lock: Mutex::new(()),
        }
    }

    /// Production constructor: platform keychain + the default
    /// `~/.conductor/network_approvals.json` registry and `~/.conductor/security`
    /// lock dir. Returns `None` if the home directory can't be resolved (the
    /// caller then withholds all non-loopback binds — no home means no key
    /// store, fail-closed).
    pub fn platform() -> Option<Self> {
        let home = dirs::home_dir()?;
        let conductor = home.join(".conductor");
        Some(Self::new(
            Arc::new(PlatformKeychainProvider),
            conductor.join("network_approvals.json"),
            conductor.join("security"),
        ))
    }

    /// Evaluate every non-loopback edge in one pass. Loads the key (cached) and
    /// the registry **once**; returns a verdict per edge in the same order.
    ///
    /// Synchronous and blocking (keychain + file I/O) — the async caller must run
    /// it via `tokio::task::spawn_blocking` so it never blocks the runtime.
    pub fn evaluate(&self, edges: &[GateEdge]) -> Vec<BindVerdict> {
        if edges.is_empty() {
            return Vec::new();
        }
        // 1. Key — cached, lazy, with in-memory expiry re-evaluation. Any failure
        //    withholds ALL non-loopback edges (fail-closed).
        let key = match self.ensure_key() {
            Ok(k) => k,
            Err(reason) => return vec![BindVerdict::Withhold(reason); edges.len()],
        };
        // 2. Registry — load + verify once. Any error (tamper, bad perms, parse)
        //    invalidates ALL approvals (fail-closed, never a cached fallback).
        let registry = match ApprovalRegistry::load(&self.registry_path, &key) {
            Ok(r) => r,
            Err(e) => {
                // Fail-closed either way, but label tamper vs a transient read
                // error accurately (observability — don't cry "tamper" on I/O).
                let reason = if is_integrity_failure(&e) {
                    WithholdReason::RegistryTampered
                } else {
                    WithholdReason::RegistryUnreadable
                };
                return vec![BindVerdict::Withhold(reason); edges.len()];
            }
        };
        // 3. Per-edge decision against the authenticated registry. Each decision
        //    maps to its semantically-correct verdict (no illogical fallthrough).
        edges
            .iter()
            .map(|e| {
                // Defence-in-depth: the gate determines loopback-ness itself from
                // the edge's host (IpAddr-parsed; `0.0.0.0`/`::` are non-loopback)
                // rather than assuming the caller pre-filtered, and decides the
                // loopback case **here** — a genuine loopback edge auto-approves
                // (Phase A) without ever consulting the registry. An unparseable
                // host is treated as non-loopback → needs approval (fail-closed).
                let host_is_loopback = e
                    .host
                    .parse::<IpAddr>()
                    .map(|ip| NetworkAcl::is_loopback_address(&ip))
                    .unwrap_or(false);
                if host_is_loopback {
                    return BindVerdict::Bind;
                }
                let akey = ApprovalKey::for_listener(e.alias, e.host, e.port, e.acl_entries);
                // Non-loopback: pass `host_is_loopback = false`, so `decision`
                // can never return `AutoApproveLoopback` — there is no path here
                // that binds a non-loopback socket on a loopback auto-approval.
                match registry.decision(&akey, false, e.requires_amplification_ack) {
                    ApprovalDecision::Approved => BindVerdict::Bind,
                    // No record, or an expired amplification ack → await approval.
                    ApprovalDecision::PromptRequired => {
                        BindVerdict::Withhold(WithholdReason::AwaitingApproval)
                    }
                    // Both unreachable for a non-loopback edge (we passed `false`,
                    // and `load()` already converted tamper to the Err arm above);
                    // mapped **fail-closed** so a contract violation can never
                    // fail-open.
                    ApprovalDecision::AutoApproveLoopback | ApprovalDecision::RegistryTampered => {
                        BindVerdict::Withhold(WithholdReason::RegistryTampered)
                    }
                }
            })
            .collect()
    }

    /// Acquire the HMAC key: return the cached key (re-evaluating expiry in
    /// memory), else read the keychain **once** via the provider and cache it.
    fn ensure_key(&self) -> Result<HmacKey, WithholdReason> {
        // Fast path: cached key, re-evaluating the 730-day expiry in memory. The
        // lock is released (scope ends) before any blocking I/O below.
        {
            let guard = self.lock_cache();
            if let Some(cached) = guard.as_ref() {
                return if cached.is_hard_expired(SystemTime::now()) {
                    Err(WithholdReason::KeychainExpired)
                } else {
                    Ok(cached.key.clone())
                };
            }
        }
        // Slow path: serialise on `init_lock` so exactly ONE thread reads the
        // keychain (no concurrent OS access prompts), without holding the cache
        // mutex across the blocking read (fast-path reads stay unblocked).
        let _init = self
            .init_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Re-check under the init lock: a thread that held it before us may have
        // already populated the cache while we waited — then we never read again.
        {
            let guard = self.lock_cache();
            if let Some(cached) = guard.as_ref() {
                return if cached.is_hard_expired(SystemTime::now()) {
                    Err(WithholdReason::KeychainExpired)
                } else {
                    Ok(cached.key.clone())
                };
            }
        }
        // We hold `init_lock` and the cache is empty → we are the single reader.
        // The read can block (flock + OS keychain); `init_keychain_with` enforces
        // the hard-expiry refusal on this first read.
        let store = self
            .provider
            .keychain()
            .map_err(|_| WithholdReason::KeychainUnavailable)?;
        let fresh = match init_keychain_with(store.as_ref(), &self.lock_dir) {
            Ok(init) => CachedKey {
                key: init.key,
                created_at: init.metadata.created_at,
            },
            Err(KeychainInitError::HardExpired { .. }) => {
                return Err(WithholdReason::KeychainExpired);
            }
            // Lock timeout / I/O / backend error — fail-closed as unavailable.
            Err(_) => return Err(WithholdReason::KeychainUnavailable),
        };
        let key = fresh.key.clone();
        *self.lock_cache() = Some(fresh);
        Ok(key)
    }

    /// Lock the key cache, recovering the guard on poisoning rather than
    /// propagating the panic (a poisoned mutex must not DoS a security gate; the
    /// cached value is a plain `Option<CachedKey>` with no invariant a panic could
    /// corrupt mid-write).
    fn lock_cache(&self) -> std::sync::MutexGuard<'_, Option<CachedKey>> {
        self.cached
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Registry path (for the caller's audit `arguments`).
    pub fn registry_path(&self) -> &Path {
        &self.registry_path
    }
}

#[cfg(test)]
#[path = "network_bind_gate_tests.rs"]
mod tests;
