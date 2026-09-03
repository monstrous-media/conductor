// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-042 Phase B-early — network-listener approval registry.
//!
//! The first phase that binds a non-loopback socket gates it behind an
//! HMAC-signed, manually-approved registry. This module is the registry: the
//! on-disk **envelope** (`{alg, data, mac}`, spec §4.7.1), its sign/verify, the
//! per-listener approval records (§4.5), and the bind-time [`ApprovalDecision`].
//!
//! # Envelope (§4.7.1)
//!
//! The authenticated unit is one opaque `data` string (canonical RFC 8785 JCS).
//! The verifier MACs the *exact bytes* of `data` and parses them only **after**
//! the MAC verifies — no parse-mutate-reserialize round trip, no canonical
//! injection surface. `alg` is pinned to a compile-time constant (never selects
//! the verifier → no `alg:none`). Constant-time and fail-closed throughout.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use conductor_core::security::keychain::HmacKey;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

/// The only MAC algorithm we accept. Compared as a constant; never used to
/// select the verifier (no `alg:none` / alg-confusion).
const ALG: &str = "hmac-sha256";

/// Current registry schema version.
const REGISTRY_VERSION: u32 = 1;

/// 90-day self-expiry for the D11 amplification acknowledgement flag.
const AMPLIFICATION_ACK_TTL: Duration = Duration::from_secs(90 * 24 * 3600);

/// Upper bound on the on-disk registry (a few KB in practice). A larger file is
/// refused rather than allocated — defends against a memory-DoS via a giant
/// `network_approvals.json`.
const MAX_REGISTRY_BYTES: u64 = 1 << 20; // 1 MiB

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Failures from the approval registry.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    /// The envelope declared an algorithm other than `hmac-sha256`.
    #[error("unsupported envelope alg (expected {ALG})")]
    BadAlg,
    /// The MAC did not verify — the registry has been tampered with.
    #[error("approval registry HMAC mismatch — registry tampered, failing closed")]
    MacMismatch,
    /// The envelope or payload JSON was malformed (or had duplicate keys).
    #[error("malformed approval registry: {0}")]
    Parse(String),
    /// Canonical serialization failed.
    #[error("canonical serialization failed: {0}")]
    Serialize(String),
    /// Filesystem I/O failure.
    #[error("approval registry io error: {0}")]
    Io(String),
    /// The registry file failed a hardening check (owner / type / mode /
    /// symlink). Fail-closed.
    #[error("insecure approval registry file at {path}: {detail}")]
    InsecurePermissions {
        /// Offending path.
        path: String,
        /// What the check found.
        detail: String,
    },
}

// ---------------------------------------------------------------------------
// Envelope
// ---------------------------------------------------------------------------

/// On-disk authenticated envelope. `serde`'s derived `Deserialize` rejects
/// duplicate *named* fields, so a second `data`/`mac` is an error — and the
/// verifier MACs and parses the **same** `data` value regardless.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    alg: String,
    data: String,
    mac: String,
}

fn hmac_sha256(key: &HmacKey, data: &[u8]) -> [u8; 32] {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(key.as_bytes())
        .expect("HMAC accepts a key of any length");
    mac.update(data);
    let tag = mac.finalize().into_bytes();
    let mut out = [0u8; 32];
    out.copy_from_slice(&tag);
    out
}

/// Serialize + sign the registry into an envelope JSON string.
fn sign_envelope(registry: &ApprovalRegistry, key: &HmacKey) -> Result<String, RegistryError> {
    // Canonical (deterministic) payload bytes become the authenticated `data`.
    let data =
        serde_jcs::to_string(registry).map_err(|e| RegistryError::Serialize(e.to_string()))?;
    let mac_hex = to_hex(&hmac_sha256(key, data.as_bytes()));
    let env = Envelope {
        alg: ALG.to_string(),
        data,
        mac: mac_hex,
    };
    serde_json::to_string(&env).map_err(|e| RegistryError::Serialize(e.to_string()))
}

/// Verify an envelope and return the authenticated registry. Fail-closed on any
/// mismatch. MACs the **raw `data` bytes**; never re-serializes `data`.
fn verify_envelope(file_bytes: &[u8], key: &HmacKey) -> Result<ApprovalRegistry, RegistryError> {
    let env: Envelope =
        serde_json::from_slice(file_bytes).map_err(|e| RegistryError::Parse(e.to_string()))?;
    // Pin the algorithm — do NOT select a verifier from the attacker-supplied field.
    if env.alg != ALG {
        return Err(RegistryError::BadAlg);
    }
    let expected = hmac_sha256(key, env.data.as_bytes());
    // The on-file mac is hex; decode to bytes and constant-time compare. A mac
    // that is not exactly 32 hex-encoded bytes can never match.
    let actual = match from_hex32(&env.mac) {
        Some(bytes) => bytes,
        None => return Err(RegistryError::MacMismatch),
    };
    if expected.ct_eq(&actual).unwrap_u8() != 1 {
        return Err(RegistryError::MacMismatch);
    }
    // Authenticated: only now parse the payload, rejecting duplicate keys.
    let registry = parse_registry_strict(&env.data)?;
    // Reject an unknown schema version (fail-closed; do not silently load a
    // payload written by a newer/older format we don't understand).
    if registry.version != REGISTRY_VERSION {
        return Err(RegistryError::Parse(format!(
            "unsupported registry version {} (expected {REGISTRY_VERSION})",
            registry.version
        )));
    }
    Ok(registry)
}

// ---------------------------------------------------------------------------
// Registry payload (§4.5)
// ---------------------------------------------------------------------------

/// The signed payload. Maps are `BTreeMap` (deterministic key order — never
/// `HashMap`, per spec P0c) so canonical serialization is stable.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRegistry {
    /// Schema version.
    pub version: u32,
    /// Per-listener approvals, keyed by [`ApprovalKey::storage_key`].
    #[serde(default, deserialize_with = "strict_btreemap")]
    pub listeners: BTreeMap<String, ApprovalRecord>,
    /// Trusted-network approvals — always empty in Phase B-early (auto-approval
    /// is Phase B-late). Kept for forward-compatible signing.
    #[serde(default, deserialize_with = "strict_btreemap")]
    pub trusted_networks: BTreeMap<String, TrustedNetworkApproval>,
}

/// Identifies a listener approval. Approval is bound to the exact
/// `(alias, host, port, acl_hash)` — any ACL change invalidates it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalKey {
    /// Listener alias.
    pub alias: String,
    /// Bind host.
    pub host: String,
    /// Bind port.
    pub port: u16,
    /// SHA-256 over the sorted ACL entries.
    pub acl_hash: String,
}

/// Hash of a listener's ACL entries (sorted, deduped, then SHA-256, hex).
///
/// This is the `acl_hash` component of an [`ApprovalKey`]: it binds an approval
/// to the exact allow-list, so editing the ACL invalidates the approval.
/// Sorting and deduping make the hash order- and duplicate-independent, so the
/// same logical allow-list always hashes the same.
pub fn compute_acl_hash(acl_entries: &[String]) -> String {
    use sha2::Digest;
    let mut entries: Vec<&str> = acl_entries.iter().map(|s| s.as_str()).collect();
    entries.sort_unstable();
    entries.dedup();
    let mut hasher = Sha256::new();
    for e in entries {
        // Length-prefix each entry so concatenation is injective.
        hasher.update((e.len() as u64).to_le_bytes());
        hasher.update(e.as_bytes());
    }
    to_hex(hasher.finalize().as_slice())
}

impl ApprovalKey {
    /// Build the key for a configured listener: its `acl_hash` is derived from
    /// the listener's ACL entries via [`compute_acl_hash`].
    pub fn for_listener(alias: &str, host: &str, port: u16, acl_entries: &[String]) -> Self {
        Self {
            alias: alias.to_string(),
            host: host.to_string(),
            port,
            acl_hash: compute_acl_hash(acl_entries),
        }
    }

    /// Stable, **injective** composite storage key. Each variable-length field
    /// is length-prefixed so no field value can forge a different listener's key
    /// via an embedded separator — e.g. `(alias="a\x1fb", host="c")` and
    /// `(alias="a", host="b\x1fc")` map to distinct keys.
    pub fn storage_key(&self) -> String {
        format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
            self.alias.len(),
            self.alias,
            self.host.len(),
            self.host,
            self.port,
            self.acl_hash.len(),
            self.acl_hash
        )
    }
}

/// A recorded approval.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRecord {
    /// Wall-clock approval time (unix seconds). Display/audit.
    pub approved_at_secs: u64,
    /// Which surface approved it.
    pub approving_surface: ApprovingSurface,
    /// When the amplification risk was acknowledged (unix seconds), if at all.
    /// Self-expires after 90 days (fail-closed; see [`Self::amplification_acknowledged`]).
    #[serde(default)]
    pub amplification_ack_at_secs: Option<u64>,
}

impl ApprovalRecord {
    /// Whether the D11 amplification flag is currently acknowledged. Fail-closed:
    /// a missing, future-dated, or >90-day-old ack reads as **not** acknowledged.
    pub fn amplification_acknowledged(&self, now: SystemTime) -> bool {
        let Some(ack_secs) = self.amplification_ack_at_secs else {
            return false;
        };
        let ack = UNIX_EPOCH + Duration::from_secs(ack_secs);
        match now.duration_since(ack) {
            Ok(age) => age <= AMPLIFICATION_ACK_TTL, // within TTL
            Err(_) => false,                         // future-dated → fail closed
        }
    }
}

/// Which surface recorded an approval.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ApprovingSurface {
    /// The GUI approval modal.
    Gui,
    /// Interactive `conductorctl listener approve`.
    Cli,
    /// A non-interactive automation path.
    ConductorctlAutomated,
}

/// A trusted-network approval (Phase B-late; type defined for forward-compat).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TrustedNetworkApproval {
    /// The approved CIDR.
    pub cidr: String,
    /// Approval time (unix seconds).
    pub approved_at_secs: u64,
    /// Approving surface.
    pub approving_surface: ApprovingSurface,
}

/// The bind-time decision for a listener.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalDecision {
    /// Loopback always auto-approves (Phase A + B).
    AutoApproveLoopback,
    /// An HMAC-verified manual approval is on file.
    Approved,
    /// No approval on file — await the operator; do not bind.
    PromptRequired,
    /// HMAC verification failed — fail closed; affected listeners re-prompt.
    RegistryTampered,
}

// ---------------------------------------------------------------------------
// Registry operations
// ---------------------------------------------------------------------------

impl Default for ApprovalRegistry {
    /// A default registry is a valid *current-schema* one — never `version: 0`,
    /// which would fail the load-time version check and be unreadable.
    fn default() -> Self {
        Self::new()
    }
}

impl ApprovalRegistry {
    /// A fresh, empty registry at the current schema version.
    pub fn new() -> Self {
        Self {
            version: REGISTRY_VERSION,
            listeners: BTreeMap::new(),
            trusted_networks: BTreeMap::new(),
        }
    }

    /// Record (or refresh) a manual per-listener approval.
    pub fn add_listener_approval(&mut self, key: &ApprovalKey, surface: ApprovingSurface) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.listeners.insert(
            key.storage_key(),
            ApprovalRecord {
                approved_at_secs: now,
                approving_surface: surface,
                amplification_ack_at_secs: None,
            },
        );
    }

    /// Remove a listener approval. Returns whether one existed.
    pub fn remove_listener_approval(&mut self, key: &ApprovalKey) -> bool {
        self.listeners.remove(&key.storage_key()).is_some()
    }

    /// Whether a verified approval exists for this exact listener identity.
    pub fn listener_is_approved(&self, key: &ApprovalKey) -> bool {
        self.listeners.contains_key(&key.storage_key())
    }

    /// Record that the operator acknowledged the amplification risk for this
    /// listener, resetting the D11 90-day clock. Returns `false` (no-op) if the
    /// listener isn't approved.
    pub fn acknowledge_amplification(&mut self, key: &ApprovalKey) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        match self.listeners.get_mut(&key.storage_key()) {
            Some(record) => {
                record.amplification_ack_at_secs = Some(now);
                true
            }
            None => false,
        }
    }

    /// Bind-time decision for a loaded, already-authenticated registry. Loopback
    /// auto-approves. For an approved non-loopback listener,
    /// `requires_amplification_ack` (true for Art-Net `allow_broadcast`
    /// amplifiers) additionally demands an **unexpired** D11 ack; a
    /// missing/expired ack re-prompts rather than silently binding an amplifier.
    pub fn decision(
        &self,
        key: &ApprovalKey,
        host_is_loopback: bool,
        requires_amplification_ack: bool,
    ) -> ApprovalDecision {
        if host_is_loopback {
            return ApprovalDecision::AutoApproveLoopback;
        }
        let Some(record) = self.listeners.get(&key.storage_key()) else {
            return ApprovalDecision::PromptRequired;
        };
        if requires_amplification_ack && !record.amplification_acknowledged(SystemTime::now()) {
            return ApprovalDecision::PromptRequired;
        }
        ApprovalDecision::Approved
    }

    /// Load + verify the registry from `path`. Returns:
    /// - `Ok(registry)` on a verified file,
    /// - `Ok(empty)` if the file does not exist yet (first run),
    /// - `Err(_)` on any fail-closed condition. All errors mean "do not trust a
    ///   cached/loaded approval" — the caller treats the affected listeners as
    ///   [`ApprovalDecision::RegistryTampered`] (await re-approval):
    ///   - [`RegistryError::MacMismatch`] / [`RegistryError::BadAlg`] — HMAC
    ///     tampering;
    ///   - [`RegistryError::InsecurePermissions`] — symlinked / wrong-owner /
    ///     wrong-mode file or parent dir;
    ///   - [`RegistryError::Parse`] — malformed, duplicate-key, oversized, or
    ///     unknown-version payload.
    pub fn load(path: &Path, key: &HmacKey) -> Result<Self, RegistryError> {
        match read_hardened(path)? {
            Some(bytes) => verify_envelope(&bytes, key),
            None => Ok(Self::new()),
        }
    }

    /// Sign + atomically write the registry to `path` (mode 0600). Refuses to
    /// write a registry larger than [`MAX_REGISTRY_BYTES`] (symmetric with the
    /// read-side cap) so a runaway registry can't be written then rejected on
    /// the next load.
    pub fn save(&self, path: &Path, key: &HmacKey) -> Result<(), RegistryError> {
        let envelope = sign_envelope(self, key)?;
        if envelope.len() as u64 > MAX_REGISTRY_BYTES {
            return Err(RegistryError::Parse(format!(
                "serialized registry is {} bytes (max {MAX_REGISTRY_BYTES})",
                envelope.len()
            )));
        }
        write_atomic(path, envelope.as_bytes())
    }
}

/// Parse the canonical payload string into a registry, rejecting duplicate keys.
fn parse_registry_strict(data: &str) -> Result<ApprovalRegistry, RegistryError> {
    serde_json::from_str(data).map_err(|e| RegistryError::Parse(e.to_string()))
}

/// `BTreeMap` deserializer that errors on duplicate keys (defense in depth;
/// `serde_json`'s default map handling silently keeps the last value).
fn strict_btreemap<'de, D, V>(deserializer: D) -> Result<BTreeMap<String, V>, D::Error>
where
    D: serde::Deserializer<'de>,
    V: Deserialize<'de>,
{
    use serde::de::{Error, MapAccess, Visitor};
    use std::fmt;
    use std::marker::PhantomData;

    struct StrictVisitor<V>(PhantomData<V>);
    impl<'de, V: Deserialize<'de>> Visitor<'de> for StrictVisitor<V> {
        type Value = BTreeMap<String, V>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a map with unique keys")
        }
        fn visit_map<M: MapAccess<'de>>(self, mut access: M) -> Result<Self::Value, M::Error> {
            let mut out = BTreeMap::new();
            while let Some((k, v)) = access.next_entry::<String, V>()? {
                if out.insert(k.clone(), v).is_some() {
                    return Err(M::Error::custom(format!("duplicate key: {k}")));
                }
            }
            Ok(out)
        }
    }
    deserializer.deserialize_map(StrictVisitor(PhantomData))
}

// ---------------------------------------------------------------------------
// Hardened file I/O (mirrors the keychain fallback: O_NOFOLLOW, fstat, 0600,
// atomic temp + rename)
// ---------------------------------------------------------------------------

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Hardened read; `Ok(None)` if absent. Fails closed on symlink / wrong owner /
/// wrong mode / non-regular.
fn read_hardened(path: &Path) -> Result<Option<Vec<u8>>, RegistryError> {
    let display = path.display().to_string();
    let f = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(match e.raw_os_error() {
                Some(code) if code == libc::ELOOP => RegistryError::InsecurePermissions {
                    path: display,
                    detail: "path is a symlink (O_NOFOLLOW)".into(),
                },
                _ => RegistryError::Io(format!("open registry: {e}")),
            });
        }
    };
    let md = f
        .metadata()
        .map_err(|e| RegistryError::Io(format!("stat registry: {e}")))?;
    if !md.file_type().is_file() {
        return Err(RegistryError::InsecurePermissions {
            path: display,
            detail: "not a regular file".into(),
        });
    }
    let euid = unsafe { libc::geteuid() };
    if md.uid() != euid {
        return Err(RegistryError::InsecurePermissions {
            path: display,
            detail: format!("owned by uid {} (expected {euid})", md.uid()),
        });
    }
    if md.mode() & 0o777 != 0o600 {
        return Err(RegistryError::InsecurePermissions {
            path: display,
            detail: format!("mode {:#o} (expected 0o600)", md.mode() & 0o777),
        });
    }
    // Bound the read: the registry is a few KB; refuse a pathologically large
    // file rather than allocating it (memory-DoS). Check the fstat'd size and
    // also cap the actual read as belt-and-suspenders.
    if md.len() > MAX_REGISTRY_BYTES {
        return Err(RegistryError::Parse(format!(
            "registry file is {} bytes (max {MAX_REGISTRY_BYTES})",
            md.len()
        )));
    }
    let mut buf = Vec::new();
    f.take(MAX_REGISTRY_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|e| RegistryError::Io(format!("read registry: {e}")))?;
    if buf.len() as u64 > MAX_REGISTRY_BYTES {
        return Err(RegistryError::Parse(
            "registry file exceeds size cap".into(),
        ));
    }
    Ok(Some(buf))
}

/// Ensure the parent dir exists and is a real, owner-only directory. A
/// pre-existing dir is validated, not trusted: a symlink / non-directory / one
/// owned by another uid is rejected (else a local user could redirect or tamper
/// with the registry despite `O_NOFOLLOW` on the file), and loose perms are
/// tightened to 0700.
fn ensure_secure_dir(dir: &Path) -> Result<(), RegistryError> {
    if !dir.exists() {
        // Create with 0700 atomically (DirBuilder applies the mode per
        // component — no create-then-chmod window).
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|e| RegistryError::Io(format!("create dir: {e}")))?;
    }
    // symlink_metadata does NOT follow a final symlink, so a symlinked dir is
    // reported as a symlink (not a dir) and rejected below.
    let md = fs::symlink_metadata(dir)
        .map_err(|e| RegistryError::Io(format!("stat registry dir: {e}")))?;
    if !md.file_type().is_dir() {
        return Err(RegistryError::InsecurePermissions {
            path: dir.display().to_string(),
            detail: "registry parent is not a directory (or is a symlink)".into(),
        });
    }
    let euid = unsafe { libc::geteuid() };
    if md.uid() != euid {
        return Err(RegistryError::InsecurePermissions {
            path: dir.display().to_string(),
            detail: format!("dir owned by uid {} (expected {euid})", md.uid()),
        });
    }
    if md.mode() & 0o777 != 0o700 {
        // We own it (checked above); tighten to owner-only.
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .map_err(|e| RegistryError::Io(format!("chmod registry dir: {e}")))?;
    }
    Ok(())
}

/// Atomically write `bytes` to `path` (mode 0600) via a hardened temp + rename.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), RegistryError> {
    let dir = path
        .parent()
        .ok_or_else(|| RegistryError::Io("registry path has no parent dir".into()))?;
    ensure_secure_dir(dir)?;
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(
        ".network_approvals.tmp.{}.{seq}",
        std::process::id()
    ));
    let _ = fs::remove_file(&tmp);
    // O_CREAT|O_EXCL|O_NOFOLLOW with mode 0600 creates the temp atomically at the
    // right permissions; no by-path chmod (which would be a TOCTOU surface).
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| RegistryError::Io(format!("create temp registry: {e}")))?;
    let res = (|| {
        f.write_all(bytes)
            .map_err(|e| RegistryError::Io(format!("write registry: {e}")))?;
        f.sync_all()
            .map_err(|e| RegistryError::Io(format!("fsync registry: {e}")))
    })();
    if let Err(e) = res {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    drop(f);
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        RegistryError::Io(format!("rename registry: {e}"))
    })
}

// ---------------------------------------------------------------------------
// hex helpers
// ---------------------------------------------------------------------------

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn from_hex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
#[path = "network_approvals_tests.rs"]
mod tests;
