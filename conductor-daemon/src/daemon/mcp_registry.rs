// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-027 §D18 — MCP client registration.
//!
//! With D1 (peer-credential IPC auth, Phase 1A) the daemon knows the
//! connecting binary's exe path on every connect. D18 turns that
//! knowledge into a trust policy: the user explicitly registers
//! which binaries are trusted MCP clients and at what tier.
//!
//! ```ignore
//! conductorctl mcp register --name "Claude Desktop" \
//!   --exe-path "/Applications/Claude.app/Contents/MacOS/Claude" \
//!   --tier ReadOnly
//! ```
//!
//! Unregistered MCP clients should be restricted to `ReadOnly`
//! regardless of the tools they request. Registered clients get a
//! per-client tier ceiling.
//!
//! This module ships the storage + operator-CLI surface only. The
//! daemon-side enforcement (consulting the registry at IPC connect
//! and clamping the per-connection tier ceiling) is a follow-up;
//! the registration file shape is intentionally stable so the
//! follow-up wires through unchanged.
//!
//! Storage is a JSON file under `~/.local/share/conductor/` (or
//! platform-equivalent). Atomic writes (temp-file + rename) so a
//! concurrent reader never observes a torn file.

use crate::daemon::audit::AuditRiskTier;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// One registered MCP client. Identity is `exe_path` — the canonical
/// path to the client binary as the kernel reports it
/// (`PinnedPeer::initial_exe` at connect time). `name` is operator
/// metadata for `conductorctl mcp list`; `tier` is the per-client
/// ceiling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpRegistration {
    /// Human-readable label shown by `conductorctl mcp list`.
    pub name: String,
    /// Absolute path to the client binary. The connect-time
    /// `peer.initial_exe` is matched against this exact path.
    pub exe_path: PathBuf,
    /// Per-client tier ceiling — the maximum tier this client is
    /// allowed to request, regardless of the tool's own tier.
    pub tier: AuditRiskTier,
}

/// The on-disk registration table. Versioned so future schema
/// changes (e.g. adding a `name_regex` field) can add fields without
/// breaking existing files.
///
/// **`Default` is hand-rolled, NOT derived**: a derived
/// `Default` would set `version: 0`, but the on-disk invariant
/// is `version >= 1`. The manual impl mirrors [`Self::new`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpRegistry {
    /// Schema version. Current: 1. `load()` rejects versions above
    /// [`CURRENT_SCHEMA_VERSION`] so a newer daemon downgrade
    /// doesn't silently read a forward-incompatible file.
    #[serde(default = "default_schema_version")]
    pub version: u32,
    /// All registered clients.
    #[serde(default)]
    pub entries: Vec<McpRegistration>,
}

/// Highest schema version this build knows how to read.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

fn default_schema_version() -> u32 {
    CURRENT_SCHEMA_VERSION
}

impl Default for McpRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Dedupe a list of registrations by `exe_path`, keeping the LAST
/// occurrence of each exe (matching the "append at bottom of file
/// to override" mental model). Bounded work: one pass to record
/// surviving indices, one pass to project.
fn dedupe_keep_last_by_exe(entries: Vec<McpRegistration>) -> Vec<McpRegistration> {
    // Walk back-to-front so the last occurrence wins.
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut keep_reversed: Vec<McpRegistration> = Vec::with_capacity(entries.len());
    for entry in entries.into_iter().rev() {
        if seen.insert(entry.exe_path.clone()) {
            keep_reversed.push(entry);
        }
    }
    keep_reversed.reverse();
    keep_reversed
}

/// Default platform path for the registry file.
///
/// On macOS / Linux: `~/.local/share/conductor/mcp_registry.json`
/// (per `dirs::data_local_dir()`).
pub fn default_registry_path() -> Option<PathBuf> {
    dirs::data_local_dir().map(|d| d.join("conductor").join("mcp_registry.json"))
}

impl McpRegistry {
    /// Construct an empty registry at schema v1.
    pub fn new() -> Self {
        Self {
            version: 1,
            entries: Vec::new(),
        }
    }

    /// Load from a JSON file. Returns an empty registry (NOT an
    /// error) when the file doesn't exist — first-run UX shouldn't
    /// fail loudly for a missing optional config.
    ///
    /// Rejects files with `version > CURRENT_SCHEMA_VERSION` so a
    /// downgrade after a future schema bump doesn't silently
    /// drop fields it doesn't understand.
    ///
    /// **Deduplicates by `exe_path` on load** (last entry wins).
    /// A hand-edited file could legitimately contain two entries
    /// with the same exe — without dedup, `lookup_tier` would
    /// return whichever serde decoded first, which is
    /// non-deterministic for the operator. Last-wins matches the
    /// natural "append to bottom of file to override" mental
    /// model.
    pub fn load(path: &Path) -> io::Result<Self> {
        match fs::read_to_string(path) {
            Ok(s) => {
                let mut registry: Self = serde_json::from_str(&s)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                if registry.version > CURRENT_SCHEMA_VERSION {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "MCP registry schema version {} is newer than this build supports \
                             (max: {}). Upgrade conductor-daemon before reading this file.",
                            registry.version, CURRENT_SCHEMA_VERSION
                        ),
                    ));
                }
                registry.entries = dedupe_keep_last_by_exe(registry.entries);
                Ok(registry)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::new()),
            Err(e) => Err(e),
        }
    }

    /// Save atomically, addressing three concerns with the prior
    /// naive `fs::write(tmp) + rename`:
    ///
    /// 1. **Tmp filename race**: hardcoded `<path>.json.tmp` meant
    ///    two concurrent `conductorctl mcp register` invocations
    ///    would clobber each other's tmp. Switched to
    ///    `tempfile::NamedTempFile::new_in(parent)` which uses a
    ///    randomised name; tempfile also cleans up on drop if the
    ///    persist fails.
    ///
    /// 2. **Missing fsync**: `fs::write` doesn't fsync, so a crash
    ///    after rename could surface a zero-length file in the FS
    ///    journal. Explicit `file.sync_all()` before persist
    ///    closes that window.
    ///
    /// 3. **Parent dir fsync**: ideally fsync the parent dir too
    ///    after rename, but std doesn't expose the syscall
    ///    portably. Accepted limitation; the rename itself is
    ///    atomic on the same filesystem (we create_dir_all the
    ///    parent first so the temp + target share a directory).
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let parent = path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "MCP registry path has no parent directory",
            )
        })?;
        fs::create_dir_all(parent)?;

        let json = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        let tmp = tempfile::NamedTempFile::new_in(parent)?;
        {
            use std::io::Write;
            // Take a `&File` reference rather than moving the inner
            // file — we still need the `NamedTempFile` handle for
            // the persist below.
            let mut file = tmp.as_file();
            file.write_all(json.as_bytes())?;
            file.sync_all()?;
        }
        // `persist` is atomic-rename on the same filesystem; same
        // parent dir guarantees that.
        tmp.persist(path).map_err(|e| e.error)?;
        Ok(())
    }

    /// Register a new client. If an entry with the same `exe_path`
    /// exists, its `name` and `tier` are updated in-place — this
    /// makes `conductorctl mcp register` idempotent and gives the
    /// user a natural "update the tier of an existing client" path.
    ///
    /// **Caller invariant**: `registration.exe_path` MUST be
    /// canonical (no symlinks, no `.`/`..`, absolute). Comparison
    /// at `lookup_tier` is byte-exact, so a non-canonical path
    /// stored here will never match the kernel-canonical
    /// `PinnedPeer.initial_exe` at connect time. The CLI
    /// (`conductorctl mcp register`) enforces this via
    /// `fs::canonicalize` before calling here; programmatic
    /// callers that bypass the CLI must do the same.
    pub fn register(&mut self, registration: McpRegistration) {
        if let Some(existing) = self
            .entries
            .iter_mut()
            .find(|e| e.exe_path == registration.exe_path)
        {
            existing.name = registration.name;
            existing.tier = registration.tier;
        } else {
            self.entries.push(registration);
        }
    }

    /// Revoke by `exe_path`. Returns true if an entry was removed,
    /// false if no matching entry existed (idempotent: revoking a
    /// non-registered exe is a no-op, not an error).
    pub fn revoke(&mut self, exe_path: &Path) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| e.exe_path != exe_path);
        self.entries.len() < before
    }

    /// Look up the per-client tier ceiling for a peer's exe path.
    ///
    /// Returns `Some(tier)` for a registered client (= the operator
    /// has granted this exe up to this tier); `None` for
    /// unregistered (= caller should clamp to the safe default,
    /// which is `ReadOnly` per ADR-027 §D18).
    pub fn lookup_tier(&self, exe_path: &Path) -> Option<AuditRiskTier> {
        self.entries
            .iter()
            .find(|e| e.exe_path == exe_path)
            .map(|e| e.tier)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample(name: &str, exe: &str, tier: AuditRiskTier) -> McpRegistration {
        McpRegistration {
            name: name.to_string(),
            exe_path: PathBuf::from(exe),
            tier,
        }
    }

    #[test]
    fn empty_registry_lookups_return_none() {
        let r = McpRegistry::new();
        assert_eq!(r.lookup_tier(Path::new("/anything")), None);
    }

    #[test]
    fn register_then_lookup() {
        let mut r = McpRegistry::new();
        r.register(sample("Claude", "/Apps/Claude", AuditRiskTier::ReadOnly));
        assert_eq!(
            r.lookup_tier(Path::new("/Apps/Claude")),
            Some(AuditRiskTier::ReadOnly)
        );
        assert_eq!(r.lookup_tier(Path::new("/Apps/Other")), None);
    }

    #[test]
    fn register_is_idempotent_and_updates_tier() {
        let mut r = McpRegistry::new();
        r.register(sample("Claude", "/Apps/Claude", AuditRiskTier::ReadOnly));
        // Re-register with a higher tier — should update, not duplicate.
        r.register(sample("Claude", "/Apps/Claude", AuditRiskTier::Stateful));
        assert_eq!(r.entries.len(), 1);
        assert_eq!(
            r.lookup_tier(Path::new("/Apps/Claude")),
            Some(AuditRiskTier::Stateful)
        );
    }

    #[test]
    fn revoke_removes_entry() {
        let mut r = McpRegistry::new();
        r.register(sample("Claude", "/Apps/Claude", AuditRiskTier::ReadOnly));
        assert!(r.revoke(Path::new("/Apps/Claude")));
        assert_eq!(r.lookup_tier(Path::new("/Apps/Claude")), None);
    }

    #[test]
    fn revoke_unknown_is_noop() {
        let mut r = McpRegistry::new();
        r.register(sample("Claude", "/Apps/Claude", AuditRiskTier::ReadOnly));
        // Revoking a path that isn't registered returns false but
        // doesn't error or affect the existing entry.
        assert!(!r.revoke(Path::new("/Apps/Other")));
        assert_eq!(
            r.lookup_tier(Path::new("/Apps/Claude")),
            Some(AuditRiskTier::ReadOnly)
        );
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("registry.json");
        let mut r = McpRegistry::new();
        r.register(sample("Claude", "/Apps/Claude", AuditRiskTier::ReadOnly));
        r.register(sample("Cursor", "/Apps/Cursor", AuditRiskTier::Stateful));
        r.save(&path).expect("save");

        let loaded = McpRegistry::load(&path).expect("load");
        assert_eq!(loaded.entries.len(), 2);
        assert_eq!(
            loaded.lookup_tier(Path::new("/Apps/Claude")),
            Some(AuditRiskTier::ReadOnly)
        );
        assert_eq!(
            loaded.lookup_tier(Path::new("/Apps/Cursor")),
            Some(AuditRiskTier::Stateful)
        );
    }

    #[test]
    fn load_missing_file_returns_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("does-not-exist.json");
        // First-run UX: no file → empty registry, no error.
        let r = McpRegistry::load(&path).expect("missing file should be Ok");
        assert!(r.entries.is_empty());
    }

    #[test]
    fn save_leaves_no_temp_files_behind() {
        // `save` uses `tempfile::NamedTempFile::new_in` with a
        // randomised name to avoid a tmp-filename race with the
        // prior `<path>.json.tmp` hardcoded name. Assert
        // that the registry-parent dir contains EXACTLY the
        // target file after save — no stray tmp lingering.
        let dir = tempdir().unwrap();
        let path = dir.path().join("registry.json");
        let mut r = McpRegistry::new();
        r.register(sample("Claude", "/Apps/Claude", AuditRiskTier::ReadOnly));
        r.save(&path).unwrap();

        assert!(path.exists(), "target file must exist");
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .collect();
        assert_eq!(
            entries.len(),
            1,
            "save dir should contain only the target file, got: {:?}",
            entries,
        );
        assert_eq!(entries[0], "registry.json");
    }

    /// The derived `Default` would have returned `version: 0` and
    /// made `Self::default()` produce a malformed registry. Manual
    /// impl now mirrors `new()`.
    #[test]
    fn default_uses_current_schema_version() {
        let r = McpRegistry::default();
        assert_eq!(r.version, CURRENT_SCHEMA_VERSION);
        assert!(r.entries.is_empty());
    }

    /// A hand-edited file with duplicate `exe_path` entries must
    /// dedupe deterministically. Last-wins matches the "append at
    /// bottom to override" mental model.
    #[test]
    fn load_dedupes_duplicate_exe_paths_keeping_last() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("dupes.json");
        let json = serde_json::json!({
            "version": CURRENT_SCHEMA_VERSION,
            "entries": [
                {"name": "first",  "exe_path": "/Apps/Claude", "tier": "read_only"},
                {"name": "middle", "exe_path": "/Apps/Cursor", "tier": "stateful"},
                {"name": "last",   "exe_path": "/Apps/Claude", "tier": "stateful"},
            ]
        });
        std::fs::write(&path, json.to_string()).unwrap();

        let loaded = McpRegistry::load(&path).expect("load");
        assert_eq!(loaded.entries.len(), 2, "duplicates should be deduped");
        // Last entry for "/Apps/Claude" wins (name=last, tier=stateful).
        let claude = loaded
            .entries
            .iter()
            .find(|e| e.exe_path == Path::new("/Apps/Claude"))
            .expect("Claude entry present");
        assert_eq!(claude.name, "last");
        assert_eq!(claude.tier, AuditRiskTier::Stateful);
    }

    /// `load` must reject files written by a future schema bump
    /// rather than silently dropping fields the current build
    /// doesn't understand.
    #[test]
    fn load_rejects_future_schema_version() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("future.json");
        let future_json = serde_json::json!({
            "version": CURRENT_SCHEMA_VERSION + 1,
            "entries": []
        });
        std::fs::write(&path, future_json.to_string()).unwrap();

        let err = McpRegistry::load(&path).expect_err("future version should be rejected");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(
            err.to_string().contains("newer than this build supports"),
            "error message should explain the version mismatch, got: {}",
            err
        );
    }
}
