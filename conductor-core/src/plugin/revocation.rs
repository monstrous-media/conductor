// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-027 **D9** — plugin signing-key **revocation list (CRL)** store.
//!
//! Rotation chains ([`crate::plugin::key_rotation`]) cover routine key
//! *updates*; they do **not** answer immediate *compromise*. The Council R1
//! review flagged a fast-revocation mechanism as a critical missing control: a
//! key known to be compromised must be refused at load **regardless** of how it
//! sits in a chain (root, interior, or head) and regardless of whether a
//! rotation manifest is even present.
//!
//! This module is the on-disk side of that control: a small TOML file of
//! revoked key **fingerprints** (`SHA-256(public_key)`), loaded into the
//! `HashSet<Fingerprint>` that [`crate::plugin::key_rotation::validate_chain_full`]
//! already consumes, and that the loader also checks against a directly-trusted
//! (non-rotating) signer.
//!
//! ## Fail-safe parsing
//!
//! Unlike the *trusted*-keys loader — where a malformed line is skipped because
//! dropping a trusted key only ever *removes* trust (fail-safe) — a malformed
//! **revocation** entry is a **hard error**. Silently skipping a revocation
//! would leave a compromised key *trusted* (fail-**open**), the exact opposite
//! of the control's purpose. A CRL we cannot fully parse is a CRL we cannot
//! honour, so the loader refuses it.
//!
//! ## Threat model & integrity
//!
//! The CRL's integrity rests on **filesystem permissions**, exactly like
//! `trusted_keys.toml`: a local attacker who can write the CRL could also write
//! the trust store, so the CRL introduces no new trust boundary. (Writes are
//! restricted to `0600` on Unix as defence-in-depth.) Cryptographic CRL signing
//! — so the list can be distributed over an untrusted channel — is the
//! registry-trust mechanism (ADR-027 D10d) and is intentionally **out of scope**
//! here.
//!
//! Loading is deliberately **fail-closed**: an unreadable / unparseable CRL is a
//! hard error that refuses plugin loading rather than ignoring revocations. To
//! keep that posture from being a self-inflicted denial of service, the writer
//! ([`add_revoked_fingerprint_to`]) is **atomic** (temp file + `rename`), so the
//! loader never observes a torn or truncated file.
//!
//! ## File format (`revoked_keys.toml`)
//!
//! ```toml
//! [[revoked]]
//! fingerprint = "<64 hex chars = SHA-256 of the revoked Ed25519 public key>"
//! reason = "key compromised — disclosed 2026-06-04"   # optional
//! revoked_at = "2026-06-04T00:00:00Z"                  # optional
//! ```

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::EngineError;
use crate::plugin::key_rotation::Fingerprint;

/// One entry in `revoked_keys.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevokedKey {
    /// `SHA-256(public_key)` of the revoked key, hex-encoded (64 chars).
    pub fingerprint: String,
    /// Optional human-readable reason / disclosure note.
    #[serde(default)]
    pub reason: String,
    /// Optional ISO-8601 timestamp of when the entry was added.
    #[serde(default)]
    pub revoked_at: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RevokedKeysFile {
    #[serde(default)]
    revoked: Vec<RevokedKey>,
}

/// Default CRL location: `<config_dir>/conductor/revoked_keys.toml`.
fn default_revoked_keys_path() -> Result<PathBuf, EngineError> {
    Ok(dirs::config_dir()
        .ok_or_else(|| EngineError::PluginLoadFailed("No config directory found".to_string()))?
        .join("conductor")
        .join("revoked_keys.toml"))
}

/// Upper bound on the CRL file size we will read. At ~100 bytes per entry this
/// is ~10k revocations — far beyond any realistic list — and bounds the memory a
/// (local, user-owned) CRL can consume under the fail-closed loader.
const MAX_CRL_BYTES: u64 = 1 << 20; // 1 MiB

/// Read a CRL file with a size cap. `Ok(None)` ⇒ the file does not exist
/// (`NotFound`, treated as "nothing revoked"); an oversized file is a hard
/// error. No `exists()` pre-check, so there is no TOCTOU window.
fn read_crl_capped(path: &Path) -> Result<Option<String>, EngineError> {
    use std::io::Read;
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(EngineError::PluginLoadFailed(format!(
                "Failed to read revoked keys: {e}"
            )));
        }
    };
    let len = file
        .metadata()
        .map_err(|e| EngineError::PluginLoadFailed(format!("Failed to stat revoked keys: {e}")))?
        .len();
    if len > MAX_CRL_BYTES {
        return Err(EngineError::PluginLoadFailed(format!(
            "Revoked keys file too large: {len} bytes (max {MAX_CRL_BYTES})"
        )));
    }
    let mut s = String::new();
    // `take` caps the read even if the metadata size raced/lied.
    file.take(MAX_CRL_BYTES)
        .read_to_string(&mut s)
        .map_err(|e| EngineError::PluginLoadFailed(format!("Failed to read revoked keys: {e}")))?;
    Ok(Some(s))
}

/// Decode one hex fingerprint string into a 32-byte [`Fingerprint`].
///
/// A bad entry is a **hard error** (fail-safe): see the module docs.
fn parse_fingerprint(hex_fp: &str) -> Result<Fingerprint, EngineError> {
    let bytes = hex::decode(hex_fp.trim()).map_err(|e| {
        EngineError::PluginLoadFailed(format!(
            "Invalid revoked-key fingerprint {hex_fp:?} (not hex): {e}"
        ))
    })?;
    <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| {
        EngineError::PluginLoadFailed(format!(
            "Invalid revoked-key fingerprint {hex_fp:?}: expected 32 bytes (64 hex chars), got {}",
            bytes.len()
        ))
    })
}

/// Load revoked fingerprints from the default location. An absent file yields an
/// empty set (nothing revoked yet); a present-but-malformed file is a hard
/// error (see module docs).
pub fn load_revoked_fingerprints() -> Result<HashSet<Fingerprint>, EngineError> {
    load_revoked_fingerprints_from(&default_revoked_keys_path()?)
}

/// Load revoked fingerprints from an explicit path. Sibling of
/// [`load_revoked_fingerprints`] for callers that override the location (e.g. a
/// `WasmConfig.revoked_keys_path`). Returns an empty set when the file does not
/// exist; **errors** on any malformed fingerprint entry (fail-safe).
pub fn load_revoked_fingerprints_from(path: &Path) -> Result<HashSet<Fingerprint>, EngineError> {
    let toml_str = match read_crl_capped(path)? {
        Some(s) => s,
        None => return Ok(HashSet::new()),
    };
    let file: RevokedKeysFile = toml::from_str(&toml_str)
        .map_err(|e| EngineError::PluginLoadFailed(format!("Invalid revoked keys format: {e}")))?;
    file.revoked
        .iter()
        .map(|r| parse_fingerprint(&r.fingerprint))
        .collect()
}

/// Append a revoked fingerprint to the CRL at `path` (creating the file and its
/// parent directory if needed), preserving existing entries. A fingerprint
/// already present is a no-op (idempotent). The fingerprint is validated before
/// writing so the store never contains an entry the loader would later reject.
///
/// This is the producer counterpart consumed by the `conductor-sign trust
/// revoke` CLI; kept here so the format has a single source of truth.
///
/// **Concurrency:** this is a read-modify-write with last-writer-wins semantics
/// (no file lock). It targets an operator-run CLI, which is single-writer in
/// practice; it is already strictly more robust than the sibling
/// `trusted_keys.toml` writer (a plain non-atomic `fs::write`). Advisory locking
/// for truly-concurrent writers is deferred — a dropped *revocation* would be
/// caught when the operator re-runs / lists, and the load path stays correct.
pub fn add_revoked_fingerprint_to(
    path: &Path,
    fingerprint_hex: &str,
    reason: &str,
    revoked_at: &str,
) -> Result<(), EngineError> {
    // Validate up front — never persist an entry the loader would hard-fail on.
    let fp = parse_fingerprint(fingerprint_hex)?;
    let canonical = hex::encode(fp);

    // Capped read; NotFound ⇒ start empty (no `exists()` TOCTOU).
    let mut file: RevokedKeysFile = match read_crl_capped(path)? {
        Some(toml_str) => toml::from_str(&toml_str).map_err(|e| {
            EngineError::PluginLoadFailed(format!("Invalid revoked keys format: {e}"))
        })?,
        None => RevokedKeysFile::default(),
    };

    // Validate every *existing* entry before re-writing. The loader is
    // fail-closed: a single malformed fingerprint makes it reject the whole CRL,
    // refusing all plugin loading. If such an entry is already on disk, appending
    // a new (valid) revocation and re-writing would silently carry it forward and
    // brick loading. Refuse rather than persist a file we know the loader rejects
    // — this is what makes the function's "the store never contains an entry the
    // loader would later reject" contract hold across writes, not just for the
    // newly-added entry. (#1894 — Copilot review)
    for entry in &file.revoked {
        parse_fingerprint(&entry.fingerprint)?;
    }

    // Idempotent: compare on the canonical (decoded → re-encoded) fingerprint so
    // case / whitespace differences don't create a duplicate entry.
    let already = file.revoked.iter().any(
        |r| matches!(parse_fingerprint(&r.fingerprint), Ok(fp) if hex::encode(fp) == canonical),
    );
    if !already {
        file.revoked.push(RevokedKey {
            fingerprint: canonical,
            reason: reason.to_string(),
            revoked_at: revoked_at.to_string(),
        });
    }

    let toml_str = toml::to_string_pretty(&file).map_err(|e| {
        EngineError::PluginLoadFailed(format!("Failed to serialize revoked keys: {e}"))
    })?;
    write_atomic_private(path, &toml_str)
}

/// Atomically and securely write `contents` to `path`.
///
/// Uses [`tempfile::NamedTempFile`] to create a sibling scratch file with a
/// **randomised** name via `O_EXCL` and `0600` perms *at creation* (no
/// write-then-chmod window), then atomically `persist`es it over `path` with a
/// `rename`. This closes the classic insecure-temp-file vectors (CWE-377) a
/// hand-rolled predictable-name temp would reopen: an attacker can neither
/// pre-create a symlink at the (now unguessable) scratch path to redirect the
/// write, nor read a briefly world-readable temp.
///
/// Atomicity matters because the loader treats an unparseable CRL as a *hard
/// error* (fail-closed, see module docs): a non-atomic write interrupted
/// mid-flight (crash, full disk) would otherwise leave a corrupt CRL that bricks
/// **all** plugin loading. `rename(2)` within a directory is atomic, so a reader
/// only ever sees the old complete file or the new complete file — never a torn
/// one. The temp lives in the *same* directory as `path` so the rename stays on
/// one filesystem; `NamedTempFile` removes the scratch file on any early return.
fn write_atomic_private(path: &Path, contents: &str) -> Result<(), EngineError> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    if let Some(parent) = parent {
        std::fs::create_dir_all(parent).map_err(|e| {
            EngineError::PluginLoadFailed(format!("Failed to create config directory: {e}"))
        })?;
    }
    let dir = parent.unwrap_or_else(|| Path::new("."));

    use std::io::Write;
    let mut tmp = tempfile::Builder::new()
        .prefix(".revoked_keys-")
        .suffix(".tmp")
        .tempfile_in(dir)
        .map_err(|e| {
            EngineError::PluginLoadFailed(format!("Failed to create revoked-keys temp file: {e}"))
        })?;
    tmp.write_all(contents.as_bytes())
        .map_err(|e| EngineError::PluginLoadFailed(format!("Failed to write revoked keys: {e}")))?;
    // NamedTempFile already creates the file 0600 on Unix; set it explicitly so
    // the guarantee is in this code, not an implementation detail of the crate.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tmp.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|e| {
                EngineError::PluginLoadFailed(format!(
                    "Failed to set revoked-keys permissions: {e}"
                ))
            })?;
    }
    // fsync the contents before the rename so a crash can't leave the renamed
    // file durably present but empty/partial — which, under the fail-closed
    // loader, would brick plugin loading.
    tmp.as_file()
        .sync_all()
        .map_err(|e| EngineError::PluginLoadFailed(format!("Failed to flush revoked keys: {e}")))?;
    tmp.persist(path).map_err(|e| {
        EngineError::PluginLoadFailed(format!("Failed to install revoked keys: {e}"))
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::key_rotation::fingerprint_of;

    fn fp_hex(seed: u8) -> String {
        hex::encode(fingerprint_of(&[seed; 32]))
    }

    #[test]
    fn absent_file_yields_empty_set() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("revoked_keys.toml");
        let set = load_revoked_fingerprints_from(&path).expect("absent file is empty, not error");
        assert!(set.is_empty());
    }

    #[test]
    fn loads_fingerprints_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("revoked_keys.toml");
        let (a, b) = (fp_hex(1), fp_hex(2));
        std::fs::write(
            &path,
            format!(
                "[[revoked]]\nfingerprint = \"{a}\"\n\n[[revoked]]\nfingerprint = \"{b}\"\nreason = \"compromised\"\n"
            ),
        )
        .unwrap();
        let set = load_revoked_fingerprints_from(&path).expect("valid CRL loads");
        assert_eq!(set.len(), 2);
        assert!(set.contains(&fingerprint_of(&[1; 32])));
        assert!(set.contains(&fingerprint_of(&[2; 32])));
    }

    #[test]
    fn oversized_file_is_hard_error() {
        // Bounded read: a CRL larger than MAX_CRL_BYTES is refused rather than
        // read unboundedly into memory (fail-closed loader DoS guard).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("revoked_keys.toml");
        let big = "#".repeat((MAX_CRL_BYTES as usize) + 1);
        std::fs::write(&path, big).unwrap();
        let err = load_revoked_fingerprints_from(&path).expect_err("oversized CRL must hard-fail");
        assert!(err.to_string().contains("too large"), "got: {err}");
    }

    #[test]
    fn malformed_fingerprint_is_hard_error_not_skipped() {
        // Fail-safe: a revocation we cannot parse must not be silently dropped
        // (which would leave a compromised key trusted).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("revoked_keys.toml");
        std::fs::write(&path, "[[revoked]]\nfingerprint = \"not-hex-zz\"\n").unwrap();
        assert!(load_revoked_fingerprints_from(&path).is_err());
    }

    #[test]
    fn wrong_length_fingerprint_is_hard_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("revoked_keys.toml");
        // 16 hex chars = 8 bytes, not 32.
        std::fs::write(&path, "[[revoked]]\nfingerprint = \"0011223344556677\"\n").unwrap();
        assert!(load_revoked_fingerprints_from(&path).is_err());
    }

    #[test]
    fn add_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("revoked_keys.toml");
        add_revoked_fingerprint_to(&path, &fp_hex(7), "compromised", "2026-06-04T00:00:00Z")
            .expect("add succeeds");
        let set = load_revoked_fingerprints_from(&path).expect("round-trips");
        assert_eq!(set.len(), 1);
        assert!(set.contains(&fingerprint_of(&[7; 32])));
    }

    #[test]
    fn add_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("revoked_keys.toml");
        add_revoked_fingerprint_to(&path, &fp_hex(7), "", "").unwrap();
        add_revoked_fingerprint_to(&path, &fp_hex(7), "again", "").unwrap();
        let set = load_revoked_fingerprints_from(&path).unwrap();
        assert_eq!(set.len(), 1, "re-revoking the same fingerprint is a no-op");
    }

    #[test]
    fn add_rejects_malformed_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("revoked_keys.toml");
        // Never persist an entry the loader would later hard-fail on.
        assert!(add_revoked_fingerprint_to(&path, "xyz", "", "").is_err());
        assert!(!path.exists(), "a rejected add must not create the file");
    }

    #[test]
    fn add_rejects_when_existing_entry_is_malformed() {
        // A pre-existing malformed entry must not be silently carried forward:
        // the loader hard-fails on it (fail-closed), so re-writing the file while
        // appending a *valid* revocation would leave a CRL that bricks all plugin
        // loading. The writer must refuse rather than persist a file it knows the
        // loader will reject. (#1894 — Copilot review)
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("revoked_keys.toml");
        std::fs::write(&path, "[[revoked]]\nfingerprint = \"not-hex-zz\"\n").unwrap();
        let err = add_revoked_fingerprint_to(&path, &fp_hex(9), "compromised", "")
            .expect_err("must refuse to write atop a malformed existing CRL");
        assert!(
            err.to_string().contains("fingerprint"),
            "error should name the malformed entry, got: {err}"
        );
        // The file is still the loader-rejecting original — not a silently
        // 'repaired' or appended-to CRL.
        assert!(load_revoked_fingerprints_from(&path).is_err());
    }

    #[test]
    fn add_leaves_no_temp_scratch_files() {
        // The atomic write (temp + rename) must not leave `.tmp.*` files behind,
        // and the directory must contain exactly the final CRL afterwards.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("revoked_keys.toml");
        add_revoked_fingerprint_to(&path, &fp_hex(1), "", "").unwrap();
        add_revoked_fingerprint_to(&path, &fp_hex(2), "", "").unwrap();
        let entries: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            entries,
            vec!["revoked_keys.toml".to_string()],
            "only the final CRL should remain, got: {entries:?}"
        );
        // And it must be valid (the rename installed a complete file).
        assert_eq!(load_revoked_fingerprints_from(&path).unwrap().len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn add_sets_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("revoked_keys.toml");
        add_revoked_fingerprint_to(&path, &fp_hex(3), "", "").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "CRL must be owner-only (0600), got {mode:o}");
    }
}
