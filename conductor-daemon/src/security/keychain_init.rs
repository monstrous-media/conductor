// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-042 Phase B-early Slice B.7 — keychain init-race protection + escalating
//! HMAC-key rotation cadence.
//!
//! Two responsibilities:
//! 1. **Init race:** a concurrent first-run (e.g. the daemon and a `conductorctl`
//!    command racing) must not have *both* generate a key — exactly one wins.
//!    `init_keychain` serialises `get_or_create_hmac_key` behind an advisory
//!    `flock` on a lock file under `~/.conductor/security/`.
//! 2. **Rotation cadence (spec §5 B.7):** the key's age drives an escalating
//!    warning ladder; at 730 days the daemon refuses to start until rotation.

use std::fs::OpenOptions;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use conductor_core::security::keychain::{
    HmacKey, KeyMetadata, KeychainError, KeychainStore, select_keychain,
};

/// Rotation-cadence thresholds (days), spec §5 B.7. Used both by the
/// classifier and the messages so the ladder stays single-sourced.
const ROTATION_CONSIDER_DAYS: u64 = 180;
const ROTATION_SHOULD_DAYS: u64 = 270;
const ROTATION_APPROACHING_DAYS: u64 = 300;
/// 365-day "standard expiry" — past this the key is deprecated.
const ROTATION_EXPIRY_DAYS: u64 = 365;
/// 730-day hard expiry — the daemon refuses to start.
const ROTATION_HARD_EXPIRY_DAYS: u64 = 730;
/// How long to wait for the init lock before giving up.
const INIT_LOCK_TIMEOUT: Duration = Duration::from_secs(5);

/// Escalating rotation-warning level derived from key age (spec §5 B.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationLevel {
    /// < 180 days — healthy.
    Ok,
    /// 180–269 days — informational "consider rotation".
    ConsiderRotation,
    /// 270–299 days — warn "should rotate soon".
    ShouldRotate,
    /// 300–364 days — warn "approaching the 365-day standard expiry".
    ApproachingExpiry,
    /// 365–729 days — deprecated; loud startup warning to rotate.
    Deprecated,
    /// ≥ 730 days — hard expiry; the daemon refuses to start.
    HardExpired,
}

impl RotationLevel {
    /// Classify a key age (in whole days) into a warning level, against the
    /// named cadence thresholds (no magic numbers).
    pub fn from_age_days(age_days: u64) -> Self {
        match age_days {
            d if d < ROTATION_CONSIDER_DAYS => RotationLevel::Ok,
            d if d < ROTATION_SHOULD_DAYS => RotationLevel::ConsiderRotation,
            d if d < ROTATION_APPROACHING_DAYS => RotationLevel::ShouldRotate,
            d if d < ROTATION_EXPIRY_DAYS => RotationLevel::ApproachingExpiry,
            d if d < ROTATION_HARD_EXPIRY_DAYS => RotationLevel::Deprecated,
            _ => RotationLevel::HardExpired,
        }
    }

    /// At/over the 730-day hard expiry — the daemon must refuse to start.
    pub fn is_hard_expired(self) -> bool {
        matches!(self, RotationLevel::HardExpired)
    }

    /// Short machine-readable tag for `conductorctl status` / the MCP security
    /// status tool. `None` for a healthy key.
    pub fn status_tag(self) -> Option<&'static str> {
        match self {
            RotationLevel::Ok => None,
            RotationLevel::ConsiderRotation => Some("consider_rotation"),
            RotationLevel::ShouldRotate => Some("should_rotate"),
            RotationLevel::ApproachingExpiry => Some("approaching_expiry"),
            RotationLevel::Deprecated => Some("deprecated"),
            RotationLevel::HardExpired => Some("hard_expired"),
        }
    }

    /// One-line human message (also printed to stderr at startup).
    pub fn message(self, age_days: u64) -> Option<String> {
        match self {
            RotationLevel::Ok => None,
            RotationLevel::ConsiderRotation => Some(format!(
                "network-approval HMAC key is {age_days} days old; consider rotation \
                 (`conductorctl security rotate-hmac`)."
            )),
            RotationLevel::ShouldRotate => Some(format!(
                "network-approval HMAC key is {age_days} days old; you should rotate it \
                 soon (`conductorctl security rotate-hmac`)."
            )),
            RotationLevel::ApproachingExpiry => Some(format!(
                "network-approval HMAC key is {age_days} days old; approaching the \
                 {ROTATION_EXPIRY_DAYS}-day standard expiry — rotate via \
                 `conductorctl security rotate-hmac`."
            )),
            RotationLevel::Deprecated => Some(format!(
                "network-approval HMAC key is {age_days} days old (past the \
                 {ROTATION_EXPIRY_DAYS}-day expiry); rotate now via \
                 `conductorctl security rotate-hmac`."
            )),
            RotationLevel::HardExpired => Some(format!(
                "network-approval HMAC key is {age_days} days old (>= \
                 {ROTATION_HARD_EXPIRY_DAYS}-day hard expiry); the daemon will not \
                 start until you rotate via `conductorctl security rotate-hmac`."
            )),
        }
    }
}

/// Failures from keychain init.
#[derive(Debug, thiserror::Error)]
pub enum KeychainInitError {
    /// The keychain backend errored.
    #[error(transparent)]
    Keychain(#[from] KeychainError),
    /// The init lock could not be acquired within the timeout.
    #[error("timed out acquiring keychain init lock after {0:?}")]
    LockTimeout(Duration),
    /// Lock-file / lock-dir I/O failure (preserves the underlying error source).
    #[error("keychain init: {context}")]
    Io {
        /// What we were doing.
        context: String,
        /// The underlying OS error.
        #[source]
        source: std::io::Error,
    },
    /// The key is at/over the 730-day hard expiry — refuse to start.
    #[error(
        "network-approval HMAC key is {age_days} days old (>= {ROTATION_HARD_EXPIRY_DAYS}-day \
         hard expiry); rotate via `conductorctl security rotate-hmac` before starting"
    )]
    HardExpired {
        /// The offending key age in days.
        age_days: u64,
    },
}

/// The result of a successful keychain init.
#[derive(Debug)]
pub struct KeychainInit {
    /// The (created or loaded) HMAC key.
    pub key: HmacKey,
    /// Non-secret key metadata (fingerprint, age).
    pub metadata: KeyMetadata,
    /// The rotation-warning level for the key's current age.
    pub rotation: RotationLevel,
}

/// Non-secret, read-only rotation status of the network-approval HMAC key, for
/// operator-visibility surfaces (`conductorctl security status`, the
/// `conductor_security_status` MCP tool, etc.).
///
/// Unlike [`init_keychain`], this **never** refuses on hard expiry — it *reports*
/// the level (including [`RotationLevel::HardExpired`]) so an operator can see
/// why the daemon won't start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyRotationStatus {
    /// Non-secret key fingerprint.
    pub fingerprint: String,
    /// Key age in whole days.
    pub age_days: u64,
    /// The rotation-warning level.
    pub level: RotationLevel,
}

impl KeyRotationStatus {
    /// `Some(tag)` (e.g. `"deprecated"`) when a warning applies, else `None`.
    pub fn warning_tag(&self) -> Option<&'static str> {
        self.level.status_tag()
    }
}

/// Read the current key's rotation status without creating a key and without the
/// hard-expiry refusal. Returns the keychain backend error if no key exists yet
/// or the backend is unavailable (the caller renders that as "unavailable").
pub fn key_rotation_status(
    keychain: &dyn KeychainStore,
) -> Result<KeyRotationStatus, KeychainError> {
    let metadata = keychain.key_metadata()?;
    Ok(KeyRotationStatus {
        level: RotationLevel::from_age_days(metadata.age_days),
        age_days: metadata.age_days,
        fingerprint: metadata.fingerprint,
    })
}

/// Production entry point: read the rotation status from the platform keychain.
pub fn key_rotation_status_default() -> Result<KeyRotationStatus, KeychainError> {
    let keychain = select_keychain()?;
    key_rotation_status(keychain.as_ref())
}

/// Held advisory lock — `flock`-unlocks on drop.
struct InitLock {
    _file: std::fs::File,
}

/// Acquire the advisory init lock (`LOCK_EX`) on a lock file in `dir`, retrying
/// non-blocking until [`INIT_LOCK_TIMEOUT`] so a wedged holder can't hang startup
/// forever.
fn acquire_init_lock(dir: &Path) -> Result<InitLock, KeychainInitError> {
    // Create the dir at 0700 (DirBuilder applies the mode as each component is
    // created — no create-then-loose window). `recursive(true)` is idempotent
    // (no error if it already exists), so there's no exists()-then-create TOCTOU.
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| KeychainInitError::Io {
            context: format!("create lock dir {} (0700)", dir.display()),
            source: e,
        })?;
    // Re-assert 0700 so an *existing* dir (e.g. created by an older version or
    // under a permissive umask) is tightened too. On an existing dir this is a
    // single op (no creation window); on a freshly-created one it's a redundant
    // no-op. Co-locates with the keychain/registry security dir.
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| {
        KeychainInitError::Io {
            context: format!("tighten lock dir {} to 0700", dir.display()),
            source: e,
        }
    })?;
    let path = dir.join(".keychain_init.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| KeychainInitError::Io {
            context: "open init lock file".into(),
            source: e,
        })?;
    let fd = file.as_raw_fd();
    let deadline = Instant::now() + INIT_LOCK_TIMEOUT;
    loop {
        // Deadline is checked first, every iteration, so even an EINTR storm
        // (which retries immediately) can never spin past the timeout.
        if Instant::now() >= deadline {
            return Err(KeychainInitError::LockTimeout(INIT_LOCK_TIMEOUT));
        }
        // SAFETY: `fd` is a valid open file descriptor owned by `file`.
        let rc = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(InitLock { _file: file });
        }
        let err = std::io::Error::last_os_error();
        match err.raw_os_error() {
            // Interrupted by a signal — retry immediately (deadline re-checked
            // at the top of the loop, so this can't hang).
            Some(c) if c == libc::EINTR => continue,
            // Lock currently held: EWOULDBLOCK (== EAGAIN on Linux/macOS, but
            // check both for platforms where they differ) — back off and poll.
            Some(c) if c == libc::EWOULDBLOCK || c == libc::EAGAIN => {
                std::thread::sleep(Duration::from_millis(25));
            }
            // Any other error is real.
            _ => {
                return Err(KeychainInitError::Io {
                    context: "flock init lock".into(),
                    source: err,
                });
            }
        }
    }
}

impl Drop for InitLock {
    fn drop(&mut self) {
        // flock is released automatically when the fd closes, but unlock
        // explicitly so the window is as short as possible.
        let _ = unsafe { libc::flock(self._file.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// Read the key and its metadata so they describe the **same** stored key.
///
/// The init `flock` serialises init-vs-init but not init-vs-`rotate-hmac`, so a
/// rotation can land between `get_or_create_hmac_key()` and `key_metadata()`.
/// If that happens the held key and the metadata's `age_days` would describe
/// *different* keys — and the hard-expiry decision must apply to the key we
/// actually hold (else a hard-expired key could slip through). Retry until the
/// held key's fingerprint matches the metadata's; bounded against a rotation
/// storm.
fn read_consistent_key_metadata(
    keychain: &dyn KeychainStore,
) -> Result<(HmacKey, KeyMetadata), KeychainInitError> {
    for _ in 0..8 {
        let key = keychain.get_or_create_hmac_key()?;
        let metadata = keychain.key_metadata()?;
        if key.fingerprint() == metadata.fingerprint {
            return Ok((key, metadata));
        }
        // A rotation raced between the two reads — try again.
    }
    Err(KeychainInitError::Io {
        context: "key and metadata kept disagreeing (concurrent rotation storm)".into(),
        source: std::io::Error::other("inconsistent keychain key/metadata reads"),
    })
}

/// Initialise the keychain with init-race protection + rotation evaluation,
/// against an explicit keychain + lock dir (test seam).
pub fn init_keychain_with(
    keychain: &dyn KeychainStore,
    lock_dir: &Path,
) -> Result<KeychainInit, KeychainInitError> {
    // Serialise create-or-load so two concurrent first-runs can't both create.
    let _lock = acquire_init_lock(lock_dir)?;
    let (key, metadata) = read_consistent_key_metadata(keychain)?;
    let rotation = RotationLevel::from_age_days(metadata.age_days);
    if rotation.is_hard_expired() {
        tracing::error!(
            rotation_level = "hard_expired",
            age_days = metadata.age_days,
            "network-approval HMAC key is past the {ROTATION_HARD_EXPIRY_DAYS}-day hard \
             expiry; refusing to start — rotate via `conductorctl security rotate-hmac`"
        );
        return Err(KeychainInitError::HardExpired {
            age_days: metadata.age_days,
        });
    }
    if let Some(msg) = rotation.message(metadata.age_days) {
        // Consistent structured fields across every level.
        let level = rotation.status_tag().unwrap_or("ok");
        let age = metadata.age_days;
        match rotation {
            RotationLevel::ConsiderRotation => {
                tracing::info!(rotation_level = level, age_days = age, "{msg}")
            }
            _ => tracing::warn!(rotation_level = level, age_days = age, "{msg}"),
        }
    }
    Ok(KeychainInit {
        key,
        metadata,
        rotation,
    })
}

/// Initialise the OS keychain (production entry point): selects the platform
/// keychain and the default `~/.conductor/security` lock dir.
pub fn init_keychain() -> Result<KeychainInit, KeychainInitError> {
    let keychain = select_keychain()?;
    init_keychain_with(keychain.as_ref(), &default_lock_dir()?)
}

fn default_lock_dir() -> Result<PathBuf, KeychainInitError> {
    let home =
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| KeychainInitError::Io {
                context: "resolve home directory".into(),
                source: std::io::Error::new(std::io::ErrorKind::NotFound, "HOME is not set"),
            })?;
    Ok(home.join(".conductor").join("security"))
}

#[cfg(test)]
#[path = "keychain_init_tests.rs"]
mod tests;
