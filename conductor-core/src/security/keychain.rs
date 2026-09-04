// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-042 Phase B-early — OS keychain abstraction for the network-approval
//! HMAC key.
//!
//! Phase A binds loopback only and uses no secret storage; Phase B-early is the
//! first phase to bind a non-loopback socket, gating it behind an HMAC-signed
//! approval registry. The HMAC key lives in the OS keychain — this
//! module is the storage abstraction. [`select_keychain`] is reached only in
//! Phase B (never Phase A).
//!
//! Backend: the [`keyring`] crate vendors the platform stores (macOS Keychain /
//! Windows credential store / Linux kernel keyutils), with the feature set
//! `conductor-gui` already builds across CI. A hardened Unix file-perms fallback
//! covers headless Linux with no OS store, gated behind the explicit
//! `CONDUCTOR_LINUX_FILE_PERMS_FALLBACK=1` opt-in (strict via
//! `CONDUCTOR_SECRET_SERVICE_REQUIRED=1`). Deviation from spec §4.6 (which
//! sketches hand-wired FFIs) is documented in the spec annotation at §4.6.

use std::fmt;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

/// Length, in bytes, of the network-approval HMAC key.
pub const HMAC_KEY_LEN: usize = 32;

/// keyring service/account coordinates for the persisted HMAC key.
const KEYRING_SERVICE: &str = "media.monstrous.conductor.network-hmac";
const KEYRING_ACCOUNT: &str = "network-approvals-hmac-key";

// ---------------------------------------------------------------------------
// HmacKey
// ---------------------------------------------------------------------------

/// A 256-bit HMAC key. Key material is scrubbed from memory on drop.
///
/// `Debug` never renders the raw bytes — only the (non-secret) fingerprint.
#[derive(Clone)]
pub struct HmacKey {
    bytes: [u8; HMAC_KEY_LEN],
}

impl HmacKey {
    /// Wrap existing key bytes.
    pub fn from_bytes(bytes: [u8; HMAC_KEY_LEN]) -> Self {
        Self { bytes }
    }

    /// Generate a fresh key from the OS CSPRNG.
    pub fn generate() -> Result<Self, KeychainError> {
        let mut bytes = [0u8; HMAC_KEY_LEN];
        getrandom::fill(&mut bytes).map_err(|e| KeychainError::Entropy(e.to_string()))?;
        Ok(Self { bytes })
    }

    /// The raw key bytes (for HMAC computation).
    pub fn as_bytes(&self) -> &[u8; HMAC_KEY_LEN] {
        &self.bytes
    }

    /// Stable, non-secret fingerprint: first 8 bytes of `SHA-256(key)`, hex.
    ///
    /// Safe to log and to surface in `conductorctl status`; rotating the key
    /// changes the fingerprint, which is how operators confirm a rotation took.
    pub fn fingerprint(&self) -> String {
        let digest = Sha256::digest(self.bytes);
        to_hex(&digest[..8])
    }
}

impl Drop for HmacKey {
    fn drop(&mut self) {
        self.bytes.zeroize();
    }
}

impl fmt::Debug for HmacKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HmacKey")
            .field("fingerprint", &self.fingerprint())
            .field("bytes", &"<redacted>")
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Metadata
// ---------------------------------------------------------------------------

/// Non-secret metadata about the stored key, for rotation-cadence checks
/// and operator visibility.
#[derive(Debug, Clone)]
pub struct KeyMetadata {
    /// Key fingerprint (see [`HmacKey::fingerprint`]).
    pub fingerprint: String,
    /// Wall-clock creation time. Display/audit only.
    pub created_at: SystemTime,
    /// Monotonic instant reconstituted at load time. Only meaningful within
    /// this process lifetime; expiry checks fail-closed across restarts.
    pub created_at_monotonic: Instant,
    /// Whole days since creation (wall-clock, fail-closed: future-dated → 0).
    pub age_days: u64,
}

impl KeyMetadata {
    fn from_stored(stored: &StoredKey) -> Result<Self, KeychainError> {
        let created_at = UNIX_EPOCH + Duration::from_secs(stored.created_at_secs);
        let now = SystemTime::now();
        // Fail-closed: a future-dated key reads as age 0 (never "old enough to
        // skip rotation prompts" by virtue of a backwards clock jump).
        let age = now
            .duration_since(created_at)
            .unwrap_or(Duration::ZERO)
            .as_secs();
        let age_days = age / 86_400;
        // Reconstitute the monotonic anchor: now_monotonic - elapsed_wall.
        let created_at_monotonic = Instant::now()
            .checked_sub(Duration::from_secs(age))
            .unwrap_or_else(Instant::now);
        let key = stored.key()?; // propagate Corrupt rather than zero-default
        Ok(Self {
            fingerprint: key.fingerprint(),
            created_at,
            created_at_monotonic,
            age_days,
        })
    }
}

// ---------------------------------------------------------------------------
// Stored blob
// ---------------------------------------------------------------------------

/// On-storage representation: hex key + wall-clock creation time. Lives inside
/// the OS keychain secret (keyring backends) or the hardened fallback file.
#[derive(Serialize, Deserialize)]
struct StoredKey {
    /// 64 lowercase hex chars.
    key_hex: String,
    /// Unix seconds at creation. Display/audit + age; never load-bearing for
    /// security on its own (see [`KeyMetadata`]).
    created_at_secs: u64,
}

impl StoredKey {
    fn new(key: &HmacKey) -> Self {
        let created_at_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self {
            key_hex: to_hex(key.as_bytes()),
            created_at_secs,
        }
    }

    /// Decode the key, returning [`KeychainError::Corrupt`] if the hex is not
    /// exactly 32 bytes. **Never** falls back to a default/zero key — a
    /// predictable HMAC key would silently defeat the approval signature.
    fn key(&self) -> Result<HmacKey, KeychainError> {
        from_hex32(&self.key_hex)
            .map(HmacKey::from_bytes)
            .ok_or_else(|| KeychainError::Corrupt("stored key is not 32 hex-encoded bytes".into()))
    }

    fn parse(bytes: &[u8]) -> Result<Self, KeychainError> {
        let stored: StoredKey = serde_json::from_slice(bytes)
            .map_err(|e| KeychainError::Corrupt(format!("malformed stored key: {e}")))?;
        if from_hex32(&stored.key_hex).is_none() {
            return Err(KeychainError::Corrupt(
                "stored key is not 32 hex-encoded bytes".into(),
            ));
        }
        Ok(stored)
    }

    /// Serialize to JSON bytes. The returned buffer holds secret key material;
    /// callers scrub it (`Zeroize`) once it has been persisted.
    fn to_bytes(&self) -> Result<Vec<u8>, KeychainError> {
        serde_json::to_vec(self).map_err(|e| KeychainError::Backend(e.to_string()))
    }

    fn into_key(self) -> Result<HmacKey, KeychainError> {
        self.key()
    }
}

impl Drop for StoredKey {
    fn drop(&mut self) {
        // `key_hex` is the hex-encoded secret; scrub it like `HmacKey::bytes`.
        self.key_hex.zeroize();
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Failures from the keychain layer.
#[derive(Debug, thiserror::Error)]
pub enum KeychainError {
    /// No secure backend is available and no fallback was opted into.
    #[error("no secure secret-storage backend available: {suggestion}")]
    NoSecureBackend {
        /// Operator-facing remediation hint.
        suggestion: String,
    },
    /// The underlying OS keychain backend errored.
    #[error("keychain backend error: {0}")]
    Backend(String),
    /// The OS CSPRNG failed.
    #[error("entropy source failure: {0}")]
    Entropy(String),
    /// Stored key material is malformed.
    #[error("stored key is corrupt: {0}")]
    Corrupt(String),
    /// Filesystem I/O failure on the fallback path.
    #[error("keychain io error: {0}")]
    Io(String),
    /// The fallback key file failed a hardening check (owner / type / mode /
    /// symlink). Fail-closed: the daemon must not trust the file.
    #[error("insecure key file at {path}: {detail}")]
    InsecurePermissions {
        /// The offending path.
        path: String,
        /// What the check found.
        detail: String,
    },
}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// OS-keychain-backed storage for the network-approval HMAC key.
pub trait KeychainStore: Send + Sync {
    /// Return the stored key, creating + persisting a fresh one on first use.
    fn get_or_create_hmac_key(&self) -> Result<HmacKey, KeychainError>;
    /// Replace the stored key with a freshly-generated one and return it.
    fn rotate_hmac_key(&self) -> Result<HmacKey, KeychainError>;
    /// Return non-secret metadata (fingerprint, age) about the stored key.
    fn key_metadata(&self) -> Result<KeyMetadata, KeychainError>;
}

// ---------------------------------------------------------------------------
// keyring-backed store (macOS / Windows / Linux keyutils)
// ---------------------------------------------------------------------------

/// OS-keychain store backed by the [`keyring`] crate.
pub struct KeyringKeychain {
    service: String,
    account: String,
}

impl KeyringKeychain {
    /// Store keyed at the conductor network-HMAC coordinates.
    pub fn new() -> Self {
        Self {
            service: KEYRING_SERVICE.to_string(),
            account: KEYRING_ACCOUNT.to_string(),
        }
    }

    fn entry(&self) -> Result<keyring::Entry, KeychainError> {
        keyring::Entry::new(&self.service, &self.account)
            .map_err(|e| KeychainError::Backend(e.to_string()))
    }

    fn persist(&self, stored: &StoredKey) -> Result<(), KeychainError> {
        let mut bytes = stored.to_bytes()?;
        let result = self
            .entry()?
            .set_secret(&bytes)
            .map_err(|e| KeychainError::Backend(e.to_string()));
        bytes.zeroize(); // scrub the serialized secret buffer
        result
    }
}

impl Default for KeyringKeychain {
    fn default() -> Self {
        Self::new()
    }
}

impl KeychainStore for KeyringKeychain {
    fn get_or_create_hmac_key(&self) -> Result<HmacKey, KeychainError> {
        match self.entry()?.get_secret() {
            Ok(mut bytes) => {
                let parsed = StoredKey::parse(&bytes);
                bytes.zeroize(); // scrub the retrieved secret buffer
                parsed?.into_key()
            }
            Err(keyring::Error::NoEntry) => {
                let key = HmacKey::generate()?;
                self.persist(&StoredKey::new(&key))?;
                Ok(key)
            }
            Err(e) => Err(KeychainError::Backend(e.to_string())),
        }
    }

    fn rotate_hmac_key(&self) -> Result<HmacKey, KeychainError> {
        let key = HmacKey::generate()?;
        self.persist(&StoredKey::new(&key))?;
        Ok(key)
    }

    fn key_metadata(&self) -> Result<KeyMetadata, KeychainError> {
        let mut bytes = self
            .entry()?
            .get_secret()
            .map_err(|e| KeychainError::Backend(e.to_string()))?;
        let parsed = StoredKey::parse(&bytes);
        bytes.zeroize(); // scrub the retrieved secret buffer
        KeyMetadata::from_stored(&parsed?)
    }
}

// ---------------------------------------------------------------------------
// Hardened file-perms fallback (Unix; explicit opt-in on Linux)
// ---------------------------------------------------------------------------

#[cfg(unix)]
mod file_perms {
    use super::*;
    use std::fs::{self, OpenOptions};
    use std::io::{Read, Write};
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use zeroize::Zeroize;

    const KEY_FILE: &str = "network_hmac_key.json";

    /// Per-process counter making temp-file names unique across concurrent
    /// writers in the same process.
    static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

    /// File-permissions-only HMAC storage for headless / CI / container Linux
    /// where no OS secret backend is reachable. Requires explicit opt-in via
    /// `CONDUCTOR_LINUX_FILE_PERMS_FALLBACK=1` (enforced in [`select_keychain`]).
    pub struct FilePermsKeychain {
        dir: PathBuf,
    }

    impl FilePermsKeychain {
        /// Construct over `~/.conductor/security`.
        pub fn new() -> Result<Self, KeychainError> {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .ok_or_else(|| KeychainError::Io("HOME is not set".into()))?;
            Self::new_at(home.join(".conductor").join("security"))
        }

        /// Construct over an explicit security directory (test seam).
        pub fn new_at(dir: PathBuf) -> Result<Self, KeychainError> {
            ensure_secure_dir(&dir)?;
            Ok(Self { dir })
        }

        fn key_path(&self) -> PathBuf {
            self.dir.join(KEY_FILE)
        }

        /// Write `stored` to a fresh hardened (`O_EXCL`/`O_NOFOLLOW`, 0600) temp
        /// file, fully flushed, and return its path. The caller publishes it via
        /// `rename` (overwrite) or `hard_link` (exclusive create). The
        /// per-process sequence keeps concurrent temp names unique.
        fn write_temp(&self, stored: &StoredKey) -> Result<PathBuf, KeychainError> {
            let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
            let tmp = self
                .dir
                .join(format!("{KEY_FILE}.tmp.{}.{seq}", std::process::id()));
            // Clear any stale temp left by a crashed prior run with this name.
            let _ = fs::remove_file(&tmp);
            let mut f = OpenOptions::new()
                .write(true)
                .create_new(true)
                .custom_flags(libc::O_NOFOLLOW)
                .mode(0o600)
                .open(&tmp)
                .map_err(|e| KeychainError::Io(format!("create temp key file: {e}")))?;
            // Defensively re-assert 0600 in case of a permissive umask path.
            fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))
                .map_err(|e| KeychainError::Io(format!("chmod temp key file: {e}")))?;
            let mut bytes = stored.to_bytes()?;
            // Scrub the serialized secret buffer regardless of which step fails.
            let write_res = (|| {
                f.write_all(&bytes)
                    .map_err(|e| KeychainError::Io(format!("write key file: {e}")))?;
                f.sync_all()
                    .map_err(|e| KeychainError::Io(format!("fsync key file: {e}")))
            })();
            bytes.zeroize();
            if let Err(e) = write_res {
                let _ = fs::remove_file(&tmp);
                return Err(e);
            }
            Ok(tmp)
        }

        /// Atomically replace the key file (rotation): the temp is `rename`d
        /// over the target, so a crash leaves either the old or new key, never
        /// nothing (closes the rotate-then-crash key-loss window).
        fn write_atomic(&self, stored: &StoredKey) -> Result<(), KeychainError> {
            let tmp = self.write_temp(stored)?;
            fs::rename(&tmp, self.key_path()).map_err(|e| {
                let _ = fs::remove_file(&tmp);
                KeychainError::Io(format!("rename key file: {e}"))
            })?;
            Ok(())
        }

        /// Atomically create the key file *only if absent*, via `hard_link`
        /// (fails `EEXIST` if the target exists) — so two racing creators can't
        /// each install a different key (closes the check-then-create TOCTOU).
        fn create_exclusive(&self, stored: &StoredKey) -> Result<CreateOutcome, KeychainError> {
            let tmp = self.write_temp(stored)?;
            let outcome = match fs::hard_link(&tmp, self.key_path()) {
                Ok(()) => CreateOutcome::Created,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    CreateOutcome::AlreadyExists
                }
                Err(e) => {
                    let _ = fs::remove_file(&tmp);
                    return Err(KeychainError::Io(format!("link key file: {e}")));
                }
            };
            let _ = fs::remove_file(&tmp); // drop the temp link; target persists
            Ok(outcome)
        }

        /// Hardened read; `Ok(None)` if the file is absent (so the caller can
        /// create it). Symlink / wrong-owner / wrong-mode / non-regular all
        /// fail closed as `InsecurePermissions`.
        fn read_hardened_opt(&self) -> Result<Option<StoredKey>, KeychainError> {
            let path = self.key_path();
            let display = path.display().to_string();
            // O_NOFOLLOW: a symlink at `path` yields ELOOP rather than following.
            let mut f = match OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&path)
            {
                Ok(f) => f,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => {
                    return Err(match e.raw_os_error() {
                        Some(code) if code == libc::ELOOP => KeychainError::InsecurePermissions {
                            path: display,
                            detail: "path is a symlink (O_NOFOLLOW)".into(),
                        },
                        _ => KeychainError::Io(format!("open key file: {e}")),
                    });
                }
            };
            let md = f
                .metadata()
                .map_err(|e| KeychainError::Io(format!("stat key file: {e}")))?;
            if !md.file_type().is_file() {
                return Err(KeychainError::InsecurePermissions {
                    path: display,
                    detail: "not a regular file".into(),
                });
            }
            let euid = unsafe { libc::geteuid() };
            if md.uid() != euid {
                return Err(KeychainError::InsecurePermissions {
                    path: display,
                    detail: format!("owned by uid {} (expected {euid})", md.uid()),
                });
            }
            let mode = md.mode() & 0o777;
            if mode != 0o600 {
                return Err(KeychainError::InsecurePermissions {
                    path: display,
                    detail: format!("mode {mode:#o} (expected 0o600)"),
                });
            }
            let mut buf = Vec::new();
            f.read_to_end(&mut buf)
                .map_err(|e| KeychainError::Io(format!("read key file: {e}")))?;
            let parsed = StoredKey::parse(&buf);
            buf.zeroize(); // scrub the on-disk secret bytes read into memory
            Ok(Some(parsed?))
        }

        /// Like [`Self::read_hardened_opt`] but errors when the file is absent.
        fn read_hardened(&self) -> Result<StoredKey, KeychainError> {
            self.read_hardened_opt()?
                .ok_or_else(|| KeychainError::Io("key file does not exist".into()))
        }
    }

    /// Outcome of an exclusive create attempt.
    enum CreateOutcome {
        Created,
        AlreadyExists,
    }

    impl KeychainStore for FilePermsKeychain {
        fn get_or_create_hmac_key(&self) -> Result<HmacKey, KeychainError> {
            // Read-or-exclusively-create, retrying on a lost create race.
            // Bounded against pathological churn. No `exists()`-then-write gap:
            // creation is atomic via `create_exclusive`.
            for _ in 0..8 {
                if let Some(stored) = self.read_hardened_opt()? {
                    return stored.into_key();
                }
                let key = HmacKey::generate()?;
                match self.create_exclusive(&StoredKey::new(&key))? {
                    CreateOutcome::Created => return Ok(key),
                    CreateOutcome::AlreadyExists => continue, // raced; re-read
                }
            }
            Err(KeychainError::Io(
                "keychain create-or-read did not converge".into(),
            ))
        }

        fn rotate_hmac_key(&self) -> Result<HmacKey, KeychainError> {
            // Atomic overwrite (temp + rename). The target is never removed, so
            // a crash leaves the previous key intact rather than losing it.
            let key = HmacKey::generate()?;
            self.write_atomic(&StoredKey::new(&key))?;
            Ok(key)
        }

        fn key_metadata(&self) -> Result<KeyMetadata, KeychainError> {
            KeyMetadata::from_stored(&self.read_hardened()?)
        }
    }

    /// Create (or validate) the security dir as a real, owner-only directory.
    fn ensure_secure_dir(dir: &Path) -> Result<(), KeychainError> {
        if !dir.exists() {
            fs::create_dir_all(dir)
                .map_err(|e| KeychainError::Io(format!("create security dir: {e}")))?;
        }
        let md = fs::symlink_metadata(dir)
            .map_err(|e| KeychainError::Io(format!("stat security dir: {e}")))?;
        if !md.file_type().is_dir() {
            return Err(KeychainError::InsecurePermissions {
                path: dir.display().to_string(),
                detail: "security path is not a directory".into(),
            });
        }
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .map_err(|e| KeychainError::Io(format!("chmod security dir: {e}")))?;
        Ok(())
    }
}

#[cfg(unix)]
pub use file_perms::FilePermsKeychain;

// ---------------------------------------------------------------------------
// Backend selection
// ---------------------------------------------------------------------------

/// Probe whether an OS-backed secret store is reachable (Linux).
///
/// Despite the spec name, with the keyring-vendored backend (`linux-native`)
/// this probes the active OS keyring (kernel keyutils), not the D-Bus Secret
/// Service. Honours `CONDUCTOR_SKIP_DBUS_CHECK` (force unavailable) and
/// container-environment detection; the probe runs on a throwaway thread with
/// a 2-second timeout so a wedged backend cannot stall daemon startup.
#[cfg(target_os = "linux")]
pub fn secret_service_available() -> bool {
    if std::env::var_os("CONDUCTOR_SKIP_DBUS_CHECK").is_some() {
        return false;
    }
    if std::path::Path::new("/run/.containerenv").exists()
        || std::path::Path::new("/.dockerenv").exists()
    {
        tracing::debug!("container environment detected; OS secret backend treated as unavailable");
        return false;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let available = match keyring::Entry::new(KEYRING_SERVICE, "__conductor_probe__") {
            Ok(entry) => matches!(entry.get_secret(), Ok(_) | Err(keyring::Error::NoEntry)),
            Err(_) => false,
        };
        let _ = tx.send(available);
    });
    rx.recv_timeout(Duration::from_secs(2)).unwrap_or(false)
}

/// Detect a headless deployment where the file-perms fallback is reasonable
/// (with explicit opt-in). SSH sessions count as interactive (a human can
/// answer prompts even without display variables).
#[cfg(target_os = "linux")]
pub fn is_headless_environment() -> bool {
    let is_ssh = std::env::var_os("SSH_CONNECTION").is_some()
        || std::env::var_os("SSH_CLIENT").is_some()
        || std::env::var_os("SSH_TTY").is_some();
    if is_ssh {
        return false;
    }
    let no_display =
        std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none();
    let container = std::path::Path::new("/run/.containerenv").exists()
        || std::path::Path::new("/.dockerenv").exists();
    let systemd_unit = std::env::var_os("INVOCATION_ID").is_some();
    no_display && (container || systemd_unit)
}

/// The Linux backend decision, factored out of [`select_keychain`] so the
/// fail-closed precedence (strict mode > opt-in fallback > error) is unit
/// testable without mutating process environment.
#[cfg(target_os = "linux")]
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LinuxBackendChoice {
    Keyring,
    FilePerms,
}

#[cfg(target_os = "linux")]
pub(crate) fn resolve_linux_backend(
    secret_backend_available: bool,
    file_perms_fallback_optin: bool,
    secret_service_required: bool,
) -> Result<LinuxBackendChoice, KeychainError> {
    if secret_backend_available {
        return Ok(LinuxBackendChoice::Keyring);
    }
    if secret_service_required {
        return Err(KeychainError::NoSecureBackend {
            suggestion: "CONDUCTOR_SECRET_SERVICE_REQUIRED=1 is set but no OS secret backend \
                         responded; refusing the file-permissions fallback."
                .into(),
        });
    }
    if file_perms_fallback_optin {
        return Ok(LinuxBackendChoice::FilePerms);
    }
    Err(KeychainError::NoSecureBackend {
        suggestion: "no OS secret backend available. Set \
                     CONDUCTOR_LINUX_FILE_PERMS_FALLBACK=1 to opt in to the \
                     file-permissions fallback for headless / CI / container \
                     deployments."
            .into(),
    })
}

/// Select the platform keychain store. Called only in Phase B-early / B-late.
///
/// - macOS / Windows: the OS keychain via [`keyring`].
/// - Linux: the OS keyring when reachable; otherwise the hardened file-perms
///   fallback **only** with `CONDUCTOR_LINUX_FILE_PERMS_FALLBACK=1`, else
///   fail-closed. `CONDUCTOR_SECRET_SERVICE_REQUIRED=1` forbids the fallback.
pub fn select_keychain() -> Result<Box<dyn KeychainStore>, KeychainError> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        Ok(Box::new(KeyringKeychain::new()))
    }

    #[cfg(target_os = "linux")]
    {
        let optin = std::env::var_os("CONDUCTOR_LINUX_FILE_PERMS_FALLBACK").is_some();
        let required = std::env::var_os("CONDUCTOR_SECRET_SERVICE_REQUIRED").is_some();
        match resolve_linux_backend(secret_service_available(), optin, required)? {
            LinuxBackendChoice::Keyring => Ok(Box::new(KeyringKeychain::new())),
            LinuxBackendChoice::FilePerms => {
                // The opt-in stays authoritative (spec §4.6), but the fallback
                // is meant for headless deployments; warn loudly when it
                // degrades an interactive session rather than silently weaken.
                if !is_headless_environment() {
                    tracing::warn!(
                        "CONDUCTOR_LINUX_FILE_PERMS_FALLBACK set with no OS secret backend on \
                         an interactive session: HMAC key falls back to file-permissions-only \
                         storage. Verify this is intentional, or unset it to fail closed."
                    );
                }
                Ok(Box::new(FilePermsKeychain::new()?))
            }
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        // Other Unix targets: hardened file-perms only.
        #[cfg(unix)]
        {
            Ok(Box::new(FilePermsKeychain::new()?))
        }
        #[cfg(not(unix))]
        {
            Err(KeychainError::NoSecureBackend {
                suggestion: "no keychain backend is implemented for this platform".into(),
            })
        }
    }
}

// ---------------------------------------------------------------------------
// hex helpers (kept local to avoid enabling the optional `hex` feature)
// ---------------------------------------------------------------------------

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn from_hex32(s: &str) -> Option<[u8; HMAC_KEY_LEN]> {
    if s.len() != HMAC_KEY_LEN * 2 {
        return None;
    }
    let mut out = [0u8; HMAC_KEY_LEN];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_stable_and_distinguishes_keys() {
        let a = HmacKey::from_bytes([7u8; 32]);
        let b = HmacKey::from_bytes([9u8; 32]);
        assert_eq!(a.fingerprint(), a.fingerprint());
        assert_ne!(a.fingerprint(), b.fingerprint());
        assert_eq!(a.fingerprint().len(), 16);
    }

    #[test]
    fn debug_does_not_leak_key_bytes() {
        let key = HmacKey::from_bytes([0xABu8; 32]);
        let rendered = format!("{key:?}");
        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains("abababab"));
        assert!(rendered.contains(&key.fingerprint()));
    }

    #[test]
    fn corrupt_stored_key_never_yields_zero_key() {
        // Regression: `key()` must NOT fall back to an all-zero (predictable)
        // HMAC key on a decode failure — it must error.
        let bad = StoredKey {
            key_hex: "not-valid-hex".into(),
            created_at_secs: 0,
        };
        assert!(matches!(bad.key(), Err(KeychainError::Corrupt(_))));
        assert!(matches!(bad.into_key(), Err(KeychainError::Corrupt(_))));

        let zero = StoredKey {
            key_hex: "zz".repeat(32), // 64 chars, all invalid nibbles
            created_at_secs: 0,
        };
        assert!(matches!(zero.key(), Err(KeychainError::Corrupt(_))));

        // A valid blob decodes to exactly its bytes (proving no zeroing).
        let good = StoredKey {
            key_hex: to_hex(&[0x11u8; HMAC_KEY_LEN]),
            created_at_secs: 0,
        };
        assert_eq!(good.key().unwrap().as_bytes(), &[0x11u8; HMAC_KEY_LEN]);
    }

    #[test]
    fn hex_roundtrip() {
        let bytes = [0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef];
        assert_eq!(to_hex(&bytes), "0123456789abcdef");
        let key = [0x5au8; 32];
        assert_eq!(from_hex32(&to_hex(&key)), Some(key));
        assert_eq!(from_hex32("zz"), None);
        assert_eq!(from_hex32("00"), None); // wrong length
    }

    #[test]
    fn stored_key_rejects_short_hex() {
        let bad = br#"{"key_hex":"00","created_at_secs":0}"#;
        assert!(matches!(
            StoredKey::parse(bad),
            Err(KeychainError::Corrupt(_))
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_backend_precedence_is_fail_closed() {
        // Available backend always wins.
        assert_eq!(
            resolve_linux_backend(true, false, false).unwrap(),
            LinuxBackendChoice::Keyring
        );
        // Strict mode forbids the fallback when unavailable.
        assert!(matches!(
            resolve_linux_backend(false, true, true),
            Err(KeychainError::NoSecureBackend { .. })
        ));
        // Opt-in fallback when unavailable + not strict.
        assert_eq!(
            resolve_linux_backend(false, true, false).unwrap(),
            LinuxBackendChoice::FilePerms
        );
        // No backend, no opt-in → fail closed.
        assert!(matches!(
            resolve_linux_backend(false, false, false),
            Err(KeychainError::NoSecureBackend { .. })
        ));
    }
}
