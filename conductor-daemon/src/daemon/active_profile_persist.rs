// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Durable active-profile IDENTITY, owned by the daemon (#2564, ADR-034).
//!
//! `<state_dir>/active_profile.json` records *which profile is active* —
//! `{version, profile_id, name, config_path}` — written atomically by the
//! daemon on every successful profile switch and read back at boot so the
//! identity survives restarts. This is deliberately NOT:
//!
//! - `state.json` — that is a best-effort shutdown/crash snapshot; a profile
//!   switch is a user action needing immediate per-switch durability;
//! - `live.toml` — that is config CONTENT; identity metadata doesn't belong in
//!   the config domain. `config_path` here is descriptive identity + the §D9
//!   watcher / "Overwrite user.toml" target — the daemon still reloads content
//!   from `live.toml` (the sole authority), never from this pointer.
//!
//! Design notes (Council-consulted, #2564):
//! - **Corrupt ≠ absent.** A present-but-unparseable file is `Corrupt` and the
//!   boot precedence falls through to the *default config*, NOT the GUI's
//!   `profiles.json` migration fallback — otherwise a corrupt daemon file would
//!   permanently hand identity authority back to the GUI. `Absent` strictly
//!   means first-run / pre-upgrade, which is the only case that migrates.
//! - **"Default" is explicit state, never file deletion.** Switching to the
//!   built-in Default persists `{profile_id: null, name: "Default", ...}`;
//!   deleting the file instead would re-trigger the absent-file migration on
//!   the next boot and resurrect a stale manifest id.
//! - **`config_path` is absolutized on write** — the daemon's CWD can differ
//!   between boots (launchd/systemd).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::warn;

/// The persisted identity record (schema of `active_profile.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedActiveProfile {
    pub version: u8,
    /// The GUI's profile id (`profile-<timestamp>`); `None` for the built-in
    /// Default (explicit state — see module docs) and for pre-#2564 switches
    /// that didn't carry an id.
    pub profile_id: Option<String>,
    pub name: String,
    pub config_path: PathBuf,
}

/// Outcome of reading `active_profile.json` at boot. `Corrupt` is deliberately
/// distinct from `Absent`: only `Absent` may fall back to the GUI-manifest
/// migration path (see module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadOutcome {
    Valid(PersistedActiveProfile),
    Absent,
    Corrupt,
}

impl From<PersistedActiveProfile> for crate::daemon::types::ActiveProfileInfo {
    fn from(p: PersistedActiveProfile) -> Self {
        Self {
            id: p.profile_id,
            name: p.name,
            config_path: p.config_path.display().to_string(),
        }
    }
}

/// File name under the daemon state dir.
pub const ACTIVE_PROFILE_FILE: &str = "active_profile.json";

fn file_path(state_dir: &Path) -> PathBuf {
    state_dir.join(ACTIVE_PROFILE_FILE)
}

/// Read the persisted identity. Never errors: unreadable/unparseable content is
/// `Corrupt` (warned), a missing file is `Absent`.
pub fn load(state_dir: &Path) -> LoadOutcome {
    let path = file_path(state_dir);
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return LoadOutcome::Absent,
        Err(e) => {
            warn!(
                "active_profile.json unreadable at {} ({}); treating as corrupt",
                path.display(),
                e
            );
            return LoadOutcome::Corrupt;
        }
    };
    match serde_json::from_str::<PersistedActiveProfile>(&contents) {
        // Version gate (Council #2565): only the schema we know. A future
        // version that happens to deserialize must not be silently
        // reinterpreted as v1 — treat as Corrupt (→ default config at boot,
        // never the manifest migration).
        Ok(p) if p.version == 1 => LoadOutcome::Valid(p),
        Ok(p) => {
            warn!(
                "active_profile.json at {} has unsupported version {}; treating as corrupt",
                path.display(),
                p.version
            );
            LoadOutcome::Corrupt
        }
        Err(e) => {
            warn!(
                "active_profile.json unparseable at {} ({}); treating as corrupt",
                path.display(),
                e
            );
            LoadOutcome::Corrupt
        }
    }
}

/// Atomically persist the identity, aligned with the `StateManager::save`
/// pattern (Copilot #2565): tmp write → owner-only perms on Unix → fsync →
/// atomic rename, so a crash/power loss right after a profile switch can't
/// lose or expose the identity update. `config_path` is absolutized against
/// the current dir if relative. Errors are returned for the caller to WARN
/// on — a persist failure must never fail the profile switch itself (the file
/// then keeps the previous, still-true identity; benign UI lag on next boot).
pub fn persist(state_dir: &Path, profile: &PersistedActiveProfile) -> std::io::Result<()> {
    use std::io::Write;

    let mut record = profile.clone();
    // Version symmetry with load()'s `version == 1` gate (Council #2565 R2): a
    // caller-supplied stray version would write a file the next boot declares
    // Corrupt — a self-inflicted default-config boot. The writer owns the
    // schema version; force it.
    record.version = 1;
    if record.config_path.is_relative() {
        record.config_path = std::env::current_dir()?.join(&record.config_path);
    }
    let json = serde_json::to_string_pretty(&record)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let path = file_path(state_dir);
    // Pid-suffixed tmp name (Council #2565): callers are the single-threaded
    // engine loop + pre-engine startup migration, so there is no in-process
    // concurrency today — but a unique name makes a cross-process collision
    // (however the singleton flock is bypassed) harmless rather than a torn
    // write. load() only ever reads the canonical name.
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));

    // Create with owner-only mode ATOMICALLY (Council #2565: creating then
    // chmod-ing leaves a window where the record is briefly world-readable
    // under a permissive umask — create-with-0600 removes it) and with
    // `create_new` (Council R2: fails on a pre-existing path, so a planted
    // symlink or crash-stale tmp can't be silently followed/overwritten). A
    // stale tmp from a crashed earlier run of the SAME pid is removed and
    // retried once — the pid suffix makes a live concurrent writer at that
    // path impossible, so the remove-retry is race-safe within the
    // daemon's lifecycle (an external actor planting a path between remove
    // and retry is outside the threat model; create_new still refuses to
    // follow it).
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600); // rw------- from the first byte
    }
    let mut file = match opts.open(&tmp) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            std::fs::remove_file(&tmp)?;
            opts.open(&tmp)?
        }
        Err(e) => return Err(e),
    };
    file.write_all(json.as_bytes())?;
    // Fsync the tmp so the rename can never promote an empty/torn file.
    file.sync_all()?;
    drop(file);

    std::fs::rename(&tmp, &path)?;

    // Fsync the parent directory so the rename itself is durable across power
    // loss (Council #2565: POSIX only guarantees the rename's durability once
    // the containing directory is synced).
    #[cfg(unix)]
    {
        std::fs::File::open(state_dir)?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(id: Option<&str>) -> PersistedActiveProfile {
        PersistedActiveProfile {
            version: 1,
            profile_id: id.map(str::to_owned),
            name: "test-1".to_string(),
            config_path: PathBuf::from("/abs/profiles/profile-123.toml"),
        }
    }

    #[test]
    fn roundtrip_persist_then_load() {
        let tmp = tempfile::tempdir().unwrap();
        let p = sample(Some("profile-123"));
        persist(tmp.path(), &p).unwrap();
        assert_eq!(load(tmp.path()), LoadOutcome::Valid(p));
        // No tmp residue after the atomic rename — the dir holds ONLY the
        // canonical file (covers the pid-suffixed tmp name too).
        let entries: Vec<String> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, vec![ACTIVE_PROFILE_FILE.to_string()]);
    }

    #[test]
    fn persist_forces_version_1_regardless_of_caller_input() {
        // Council #2565 R2: writer/reader version symmetry — a stray caller
        // version must not produce a file the next boot declares Corrupt.
        let tmp = tempfile::tempdir().unwrap();
        let mut p = sample(Some("profile-123"));
        p.version = 7;
        persist(tmp.path(), &p).unwrap();
        match load(tmp.path()) {
            LoadOutcome::Valid(loaded) => assert_eq!(loaded.version, 1),
            other => panic!("expected Valid v1, got {other:?}"),
        }
    }

    #[test]
    fn persist_recovers_from_a_crash_stale_tmp() {
        // Council #2565 R2: create_new fails on a pre-existing tmp (stale from
        // a crashed run / planted path) — persist removes it and retries once.
        let tmp = tempfile::tempdir().unwrap();
        let stale = tmp
            .path()
            .join("active_profile.json")
            .with_extension(format!("json.tmp.{}", std::process::id()));
        std::fs::write(&stale, "stale garbage").unwrap();

        persist(tmp.path(), &sample(Some("profile-123"))).unwrap();
        match load(tmp.path()) {
            LoadOutcome::Valid(p) => assert_eq!(p.profile_id.as_deref(), Some("profile-123")),
            other => panic!("expected Valid after stale-tmp recovery, got {other:?}"),
        }
        assert!(!stale.exists(), "stale tmp must be gone after persist");
    }

    #[test]
    fn unknown_version_is_corrupt_not_valid() {
        // Council #2565: a future schema version that happens to deserialize
        // must not be silently reinterpreted as v1 — boot falls to the default
        // config (never the manifest migration).
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join(ACTIVE_PROFILE_FILE),
            r#"{"version": 2, "profile_id": "p1", "name": "Future", "config_path": "/x.toml"}"#,
        )
        .unwrap();
        assert_eq!(load(tmp.path()), LoadOutcome::Corrupt);
    }

    #[test]
    fn missing_file_is_absent_not_corrupt() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(load(tmp.path()), LoadOutcome::Absent);
    }

    #[test]
    fn garbage_json_is_corrupt_not_absent() {
        // Corrupt ≠ absent: only Absent may migrate from the GUI manifest (D2).
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(ACTIVE_PROFILE_FILE), "{ not json").unwrap();
        assert_eq!(load(tmp.path()), LoadOutcome::Corrupt);
    }

    #[test]
    fn wrong_shape_is_corrupt() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join(ACTIVE_PROFILE_FILE),
            r#"{"version": "one", "name": 42}"#,
        )
        .unwrap();
        assert_eq!(load(tmp.path()), LoadOutcome::Corrupt);
    }

    #[test]
    fn default_profile_is_explicit_state_with_null_id() {
        // D3: "Default" persists as {profile_id: null, ...}, never file deletion.
        let tmp = tempfile::tempdir().unwrap();
        let default = PersistedActiveProfile {
            version: 1,
            profile_id: None,
            name: "Default".to_string(),
            config_path: PathBuf::from("/abs/config.toml"),
        };
        persist(tmp.path(), &default).unwrap();
        match load(tmp.path()) {
            LoadOutcome::Valid(p) => {
                assert_eq!(p.profile_id, None);
                assert_eq!(p.name, "Default");
            }
            other => panic!("expected Valid, got {other:?}"),
        }
    }

    #[test]
    fn relative_config_path_is_absolutized_on_write() {
        let tmp = tempfile::tempdir().unwrap();
        let mut p = sample(Some("profile-123"));
        p.config_path = PathBuf::from("relative/profile.toml");
        persist(tmp.path(), &p).unwrap();
        match load(tmp.path()) {
            LoadOutcome::Valid(loaded) => assert!(
                loaded.config_path.is_absolute(),
                "config_path must be absolutized, got {}",
                loaded.config_path.display()
            ),
            other => panic!("expected Valid, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn persist_sets_owner_only_permissions() {
        // Copilot #2565: align with StateManager::save — the state dir holds
        // user-private data, so the identity file is rw------- like state.json.
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        persist(tmp.path(), &sample(Some("profile-123"))).unwrap();
        let mode = std::fs::metadata(tmp.path().join(ACTIVE_PROFILE_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "expected rw-------, got {mode:o}");
    }

    #[test]
    fn persist_overwrites_previous_identity() {
        let tmp = tempfile::tempdir().unwrap();
        persist(tmp.path(), &sample(Some("profile-old"))).unwrap();
        persist(tmp.path(), &sample(Some("profile-new"))).unwrap();
        match load(tmp.path()) {
            LoadOutcome::Valid(p) => assert_eq!(p.profile_id.as_deref(), Some("profile-new")),
            other => panic!("expected Valid, got {other:?}"),
        }
    }
}
