// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Startup-time config-path resolution honouring the GUI's profile
//! state (issue #957).
//!
//! When the daemon launches without `--config <path>`, it restores the
//! active profile from its OWN durable identity record,
//! `<state_dir>/active_profile.json` (#2564) — the GUI's
//! `<config_dir>/profiles/profiles.json` `active_profile_id` is consulted
//! only as a ONE-TIME migration source when that record is absent
//! (first run / pre-#2564 upgrade), falling back to
//! `<config_dir>/config.toml` otherwise.
//!
//! Precedence — see [`resolve_startup_identity_and_path`] for the current
//! (#2564 D2) five-step order used by `main.rs`:
//! 1. `--config <explicit>` — the user's intent always wins (ephemeral: no
//!    identity restore, never persisted)
//! 2. `<state_dir>/active_profile.json` — the DAEMON's own durable identity
//!    record (#2564), the authoritative source
//! 3. a corrupt daemon record → default config (never the manifest)
//! 4. `profiles.json` `active_profile_id` — ONE-TIME migration when the
//!    daemon record is absent (first run / pre-#2564 upgrade)
//! 5. `<config_dir>/config.toml` — historical default
//!
//! The legacy [`resolve_startup_config_path`] (steps 1/4/5 only) is retained
//! for compatibility; the manifest is otherwise demoted to a GUI-local hint.
//!
//! Anything that goes wrong with the manifest read falls back to the
//! default `<config_dir>/config.toml` (this applies to the manifest
//! lookup used by both resolvers, including the legacy
//! [`resolve_startup_config_path`]). The daemon must never refuse to
//! start because the profile state file is broken — recovery is always
//! available via `--config` or by editing `config.toml`. Specifically,
//! fallback triggers on any of:
//!
//! - manifest missing or unreadable
//! - manifest is malformed JSON
//! - `active_profile_id` is null / empty / missing
//! - `active_profile_id` doesn't match any entry's `id`
//! - active entry's `config_path` is relative (CWD-dependent under
//!   launchd / systemd)
//! - active entry's `config_path` doesn't end in `.toml`
//!   (case-insensitive — `.TOML` is fine)
//! - active entry's `config_path` isn't a regular file (covers both
//!   "doesn't exist" and "exists but is a directory")

use crate::daemon::active_profile_persist::{self, LoadOutcome, PersistedActiveProfile};
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// Resolve the config path the daemon should load at startup.
///
/// `args_config` is the `--config <path>` flag value; when present
/// it always wins. `config_dir` is the OS-specific Conductor config
/// directory (e.g., `~/Library/Application Support/conductor` on
/// macOS) — caller passes it explicitly so this function can be
/// unit-tested with a tempdir.
pub fn resolve_startup_config_path(args_config: Option<PathBuf>, config_dir: &Path) -> PathBuf {
    if let Some(path) = args_config {
        debug!(path = %path.display(), "Using --config from args");
        return path;
    }

    if let Some(entry) = active_profile_manifest_entry(config_dir) {
        info!(
            path = %entry.config_path.display(),
            "Using active profile from profiles.json"
        );
        return entry.config_path;
    }

    let default = config_dir.join("config.toml");
    debug!(
        path = %default.display(),
        "No --config flag and no active profile resolved; using default config.toml"
    );
    default
}

/// #2564 (D2): resolve BOTH the startup user-config path and the active-profile
/// IDENTITY, with the daemon's own durable `active_profile.json` as the primary
/// source. Precedence (highest first):
///
/// 1. `--config <explicit>` — ephemeral: wins for the path, carries **no**
///    identity, and is never persisted (a CLI boot must not clobber the
///    durable profile selection).
/// 2. `<state_dir>/active_profile.json` (`Valid`) — the daemon's own record.
///    Its `config_path` is re-validated like a manifest entry; a stale path
///    (profile file deleted while the daemon was down) discards the identity
///    with a warn and falls through to the DEFAULT config — not the manifest.
/// 3. `Corrupt` file — falls through to the DEFAULT config, deliberately NOT
///    the GUI-manifest migration: a corrupt daemon file must not hand identity
///    authority back to the GUI (Council, #2564 design note).
/// 4. `Absent` (strictly first-run / pre-upgrade) — one-time MIGRATION from
///    the GUI's `profiles.json` `active_profile_id`: the resolved entry is
///    written into `active_profile.json` so subsequent boots take path (2).
/// 5. `<config_dir>/config.toml` — historical default, no identity.
pub fn resolve_startup_identity_and_path(
    args_config: Option<PathBuf>,
    config_dir: &Path,
    state_dir: &Path,
) -> (PathBuf, Option<PersistedActiveProfile>) {
    if let Some(path) = args_config {
        debug!(path = %path.display(), "Using --config from args (ephemeral: no identity restore)");
        return (path, None);
    }

    let default = config_dir.join("config.toml");
    match active_profile_persist::load(state_dir) {
        LoadOutcome::Valid(identity) => {
            if validate_profile_config_path(&identity.config_path) {
                info!(
                    profile = %identity.name,
                    path = %identity.config_path.display(),
                    "Restored active profile from daemon identity (active_profile.json)"
                );
                (identity.config_path.clone(), Some(identity))
            } else {
                warn!(
                    profile = %identity.name,
                    path = %identity.config_path.display(),
                    "Persisted active profile points at an invalid config; using default config"
                );
                (default, None)
            }
        }
        LoadOutcome::Corrupt => {
            // Corrupt ≠ absent: do NOT consult the GUI manifest here.
            warn!("active_profile.json is corrupt; using default config (no manifest migration)");
            (default, None)
        }
        LoadOutcome::Absent => match active_profile_manifest_entry(config_dir) {
            Some(entry) => {
                let identity = PersistedActiveProfile {
                    version: 1,
                    profile_id: Some(entry.id),
                    name: entry.name,
                    config_path: entry.config_path.clone(),
                };
                // One-time migration: seed the daemon's own record so the next
                // boot resolves from it. Best-effort — a write failure just
                // re-runs the migration next boot.
                if let Err(e) = active_profile_persist::persist(state_dir, &identity) {
                    warn!("Failed to migrate active profile into active_profile.json: {e}");
                }
                info!(
                    profile = %identity.name,
                    path = %entry.config_path.display(),
                    "Migrated active profile from profiles.json into daemon identity"
                );
                (entry.config_path, Some(identity))
            }
            None => (default, None),
        },
    }
}

/// A validated active-profile entry from the GUI's `profiles.json`.
struct ManifestEntry {
    id: String,
    name: String,
    config_path: PathBuf,
}

/// Shared validation for a profile config path — from the GUI manifest or the
/// daemon's own `active_profile.json` (#2564). The daemon must never refuse to
/// start over a broken pointer: absolute + `.toml` (case-insensitive) + regular
/// file, exactly the #958/#964 rules.
fn validate_profile_config_path(candidate: &Path) -> bool {
    if !candidate.is_absolute() {
        warn!(
            path = %candidate.display(),
            "Profile config_path is relative — rejecting"
        );
        return false;
    }
    let extension_ok = candidate
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"));
    if !extension_ok {
        warn!(
            path = %candidate.display(),
            "Profile config_path is not a .toml file — rejecting"
        );
        return false;
    }
    if !candidate.is_file() {
        warn!(
            path = %candidate.display(),
            "Profile config_path is not a regular file — rejecting"
        );
        return false;
    }
    true
}

/// Read `profiles.json` and return the validated entry (id, name,
/// `config_path`) matching `active_profile_id`. Returns `None` for any
/// failure mode — caller falls back to the default `config.toml`.
fn active_profile_manifest_entry(config_dir: &Path) -> Option<ManifestEntry> {
    let manifest_path = config_dir.join("profiles").join("profiles.json");
    if !manifest_path.exists() {
        return None;
    }

    let content = match std::fs::read_to_string(&manifest_path) {
        Ok(c) => c,
        Err(e) => {
            warn!(
                path = %manifest_path.display(),
                error = %e,
                "Failed to read profiles manifest; falling back to default config"
            );
            return None;
        }
    };

    let manifest: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            warn!(
                path = %manifest_path.display(),
                error = %e,
                "profiles.json is malformed; falling back to default config"
            );
            return None;
        }
    };

    let active_id = manifest.get("active_profile_id")?.as_str()?;
    if active_id.is_empty() {
        return None;
    }

    let profiles = manifest.get("profiles")?.as_array()?;
    let entry = profiles
        .iter()
        .find(|p| p.get("id").and_then(|i| i.as_str()) == Some(active_id))?;
    let candidate: PathBuf = entry
        .get("config_path")
        .and_then(|c| c.as_str())
        .map(PathBuf::from)?;
    // Display name for the identity record; entries always carry `name` in
    // practice, but fall back to the id rather than failing resolution.
    let name = entry
        .get("name")
        .and_then(|n| n.as_str())
        .unwrap_or(active_id)
        .to_string();

    // #958 review bug 2 — validate the candidate before honouring it.
    // Module docs promise the daemon never refuses to start because
    // the profile state file is broken; without these checks, a
    // manifest that points at a missing / wrong-extension / relative
    // path would propagate to `main` and trigger a hard exit on
    // `!config_path.exists()`. Fall back to the default in any of
    // those cases so the daemon still launches. (Validation rules —
    // absolute, `.toml` case-insensitive per Copilot #964, `is_file()`
    // per Copilot #964 — live in `validate_profile_config_path`, shared
    // with the #2564 daemon-identity restore.)
    if !validate_profile_config_path(&candidate) {
        return None;
    }

    Some(ManifestEntry {
        id: active_id.to_string(),
        name,
        config_path: candidate,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_manifest(config_dir: &Path, content: &str) {
        let profiles_dir = config_dir.join("profiles");
        fs::create_dir_all(&profiles_dir).unwrap();
        fs::write(profiles_dir.join("profiles.json"), content).unwrap();
    }

    #[test]
    fn explicit_args_config_wins_over_manifest() {
        let tmp = TempDir::new().unwrap();
        write_manifest(
            tmp.path(),
            r#"{
                "active_profile_id": "p1",
                "profiles": [{"id":"p1","config_path":"/should/be/ignored.toml"}]
            }"#,
        );
        let explicit = PathBuf::from("/explicit/path.toml");
        assert_eq!(
            resolve_startup_config_path(Some(explicit.clone()), tmp.path()),
            explicit
        );
    }

    #[test]
    fn returns_active_profile_config_path_when_manifest_resolves() {
        let tmp = TempDir::new().unwrap();
        let active_path = tmp.path().join("profiles").join("studio.toml");
        let default_path = tmp.path().join("default.toml");
        // Materialise both files so the new path-existence check
        // (#958 review) is satisfied. Switched to `manifest()` helper
        // so the JSON is correctly escaped on Windows.
        fs::create_dir_all(active_path.parent().unwrap()).unwrap();
        fs::write(&active_path, "").unwrap();
        fs::write(&default_path, "").unwrap();
        write_manifest(
            tmp.path(),
            &manifest(
                "studio",
                vec![("default", &default_path), ("studio", &active_path)],
            ),
        );
        assert_eq!(resolve_startup_config_path(None, tmp.path()), active_path);
    }

    #[test]
    fn falls_back_to_default_when_manifest_missing() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(
            resolve_startup_config_path(None, tmp.path()),
            tmp.path().join("config.toml")
        );
    }

    #[test]
    fn falls_back_to_default_when_manifest_malformed() {
        let tmp = TempDir::new().unwrap();
        write_manifest(tmp.path(), "{not valid json");
        assert_eq!(
            resolve_startup_config_path(None, tmp.path()),
            tmp.path().join("config.toml")
        );
    }

    #[test]
    fn falls_back_to_default_when_active_id_is_null() {
        let tmp = TempDir::new().unwrap();
        write_manifest(tmp.path(), r#"{"active_profile_id": null, "profiles": []}"#);
        assert_eq!(
            resolve_startup_config_path(None, tmp.path()),
            tmp.path().join("config.toml")
        );
    }

    #[test]
    fn falls_back_to_default_when_active_id_is_empty_string() {
        let tmp = TempDir::new().unwrap();
        write_manifest(tmp.path(), r#"{"active_profile_id": "", "profiles": []}"#);
        assert_eq!(
            resolve_startup_config_path(None, tmp.path()),
            tmp.path().join("config.toml")
        );
    }

    #[test]
    fn falls_back_to_default_when_active_id_doesnt_resolve() {
        let tmp = TempDir::new().unwrap();
        write_manifest(
            tmp.path(),
            r#"{
                "active_profile_id": "ghost",
                "profiles": [{"id":"other","config_path":"/other.toml"}]
            }"#,
        );
        assert_eq!(
            resolve_startup_config_path(None, tmp.path()),
            tmp.path().join("config.toml")
        );
    }

    #[test]
    fn falls_back_to_default_when_active_id_missing() {
        let tmp = TempDir::new().unwrap();
        write_manifest(tmp.path(), r#"{"profiles": []}"#);
        assert_eq!(
            resolve_startup_config_path(None, tmp.path()),
            tmp.path().join("config.toml")
        );
    }

    #[test]
    fn handles_legacy_bare_array_manifest_by_falling_back() {
        // Legacy manifests had no active_profile_id (bare array of profiles).
        // Returning the default rather than guessing at an active id is the
        // safe thing — users on legacy manifests get pre-#957 behaviour.
        let tmp = TempDir::new().unwrap();
        write_manifest(tmp.path(), r#"[{"id":"p1","config_path":"/p1.toml"}]"#);
        assert_eq!(
            resolve_startup_config_path(None, tmp.path()),
            tmp.path().join("config.toml")
        );
    }

    // -------------------------------------------------------------------
    // #958 post-merge review (Copilot): three real defects in the
    // initial implementation. Module docs promise "must never refuse to
    // start because the profile state file is broken", but pre-fix any
    // of these scenarios produced a hard exit on `!config_path.exists()`
    // in main:
    //
    //   1. profile points at a missing file
    //   2. profile points at a non-.toml extension (typo / wrong file)
    //   3. profile points at a relative path (manifest path not absolute)
    //
    // Plus a Windows test bug: `format!`+`Path::display()` produced
    // unescaped backslashes that broke `serde_json::from_str`.
    // -------------------------------------------------------------------

    fn manifest(active_id: &str, entries: Vec<(&str, &Path)>) -> String {
        // Use serde_json::json! so paths get JSON-escaped properly.
        // The previous `format!`+`display()` pattern broke on Windows
        // (#958 review bug 3).
        let profiles: Vec<serde_json::Value> = entries
            .into_iter()
            .map(|(id, path)| {
                serde_json::json!({
                    "id": id,
                    "config_path": path.to_string_lossy(),
                })
            })
            .collect();
        serde_json::json!({
            "active_profile_id": active_id,
            "profiles": profiles,
        })
        .to_string()
    }

    #[test]
    fn falls_back_when_profile_path_does_not_exist() {
        // #958 review bug 2: profile points at a missing file. Module
        // docs say startup must not fail because of broken profile
        // state, so we must fall back to the default.
        let tmp = TempDir::new().unwrap();
        let ghost_path = tmp.path().join("does_not_exist.toml");
        write_manifest(tmp.path(), &manifest("p1", vec![("p1", &ghost_path)]));

        assert_eq!(
            resolve_startup_config_path(None, tmp.path()),
            tmp.path().join("config.toml")
        );
    }

    #[test]
    fn falls_back_when_profile_path_has_wrong_extension() {
        // #958 review bug 2: a manifest entry pointing at /etc/passwd
        // or some other non-.toml file should not be honoured even
        // if it exists — defence against typos and against profile
        // entries that got corrupted to point at unrelated files.
        let tmp = TempDir::new().unwrap();
        let bad_ext = tmp.path().join("config.txt");
        fs::write(&bad_ext, "").unwrap();
        write_manifest(tmp.path(), &manifest("p1", vec![("p1", &bad_ext)]));

        assert_eq!(
            resolve_startup_config_path(None, tmp.path()),
            tmp.path().join("config.toml")
        );
    }

    #[test]
    fn falls_back_when_profile_path_is_relative() {
        // #958 review bug 2: relative paths can resolve to anywhere
        // depending on the daemon's CWD at launch (often `/` for
        // launchd / systemd). The manifest format requires absolute
        // paths; reject relatives and fall back.
        let tmp = TempDir::new().unwrap();
        write_manifest(
            tmp.path(),
            r#"{
                "active_profile_id": "p1",
                "profiles": [{"id":"p1","config_path":"profiles/p1.toml"}]
            }"#,
        );
        assert_eq!(
            resolve_startup_config_path(None, tmp.path()),
            tmp.path().join("config.toml")
        );
    }

    #[test]
    fn falls_back_when_profile_path_extension_uses_uppercase_toml() {
        // Copilot #964 review: case-insensitive filesystems (HFS+ on
        // macOS, NTFS default on Windows) routinely round-trip the
        // same file with mixed casing. Rejecting `.TOML` because the
        // matcher was case-sensitive would surprise users who renamed
        // the file via Finder / Explorer. Treat extension as
        // case-insensitive ASCII.
        let tmp = TempDir::new().unwrap();
        let upper_path = tmp.path().join("studio.TOML");
        fs::write(&upper_path, "").unwrap();
        write_manifest(tmp.path(), &manifest("p1", vec![("p1", &upper_path)]));

        assert_eq!(resolve_startup_config_path(None, tmp.path()), upper_path);
    }

    #[test]
    fn falls_back_when_profile_path_is_a_directory() {
        // Copilot #964 review: `Path::exists()` returns true for
        // directories too. A user (or corrupted manifest) with a
        // directory named `something.toml` would pass validation,
        // then `main` would fail trying to read it as a config —
        // exactly the "must never refuse to start" contract this
        // module guards against. Check `is_file()`.
        let tmp = TempDir::new().unwrap();
        let dir_pretending_to_be_file = tmp.path().join("config.toml");
        fs::create_dir(&dir_pretending_to_be_file).unwrap();
        write_manifest(
            tmp.path(),
            &manifest("p1", vec![("p1", &dir_pretending_to_be_file)]),
        );

        // The default config.toml at the same level is the
        // dir_pretending_to_be_file itself in this test — but the
        // resolver still returns the default path string regardless
        // of whether that path is itself a directory. Use a different
        // tmp layout to keep the assertion clean.
        let tmp2 = TempDir::new().unwrap();
        let dir2 = tmp2.path().join("studio.toml");
        fs::create_dir(&dir2).unwrap();
        write_manifest(tmp2.path(), &manifest("p1", vec![("p1", &dir2)]));
        assert_eq!(
            resolve_startup_config_path(None, tmp2.path()),
            tmp2.path().join("config.toml")
        );
    }

    // -------------------------------------------------------------------
    // #2564 D2 — resolve_startup_identity_and_path precedence matrix.
    // Primary source is the daemon's own active_profile.json; the GUI
    // manifest is strictly a one-time migration for the Absent case.
    // -------------------------------------------------------------------

    fn write_identity(state_dir: &Path, id: Option<&str>, name: &str, config_path: &Path) {
        let identity = PersistedActiveProfile {
            version: 1,
            profile_id: id.map(str::to_owned),
            name: name.to_string(),
            config_path: config_path.to_path_buf(),
        };
        active_profile_persist::persist(state_dir, &identity).unwrap();
    }

    #[test]
    fn identity_file_wins_over_manifest() {
        // Both sources present and DIVERGENT: the daemon's own record wins —
        // a hand-edited manifest must not reintroduce the split-brain (#2561).
        let config_dir = TempDir::new().unwrap();
        let state_dir = TempDir::new().unwrap();
        let daemon_choice = state_dir.path().join("daemon-profile.toml");
        let manifest_choice = config_dir.path().join("manifest-profile.toml");
        fs::write(&daemon_choice, "").unwrap();
        fs::write(&manifest_choice, "").unwrap();
        write_identity(
            state_dir.path(),
            Some("p-daemon"),
            "Daemon Pick",
            &daemon_choice,
        );
        write_manifest(
            config_dir.path(),
            &manifest("p-manifest", vec![("p-manifest", &manifest_choice)]),
        );

        let (path, identity) =
            resolve_startup_identity_and_path(None, config_dir.path(), state_dir.path());
        assert_eq!(path, daemon_choice);
        let identity = identity.expect("identity restored");
        assert_eq!(identity.profile_id.as_deref(), Some("p-daemon"));
        assert_eq!(identity.name, "Daemon Pick");
    }

    #[test]
    fn corrupt_identity_file_falls_back_to_default_not_manifest() {
        // Council (#2564): a present-but-corrupt daemon file must NOT hand
        // authority back to the GUI manifest — default config, no identity.
        let config_dir = TempDir::new().unwrap();
        let state_dir = TempDir::new().unwrap();
        let manifest_choice = config_dir.path().join("manifest-profile.toml");
        fs::write(&manifest_choice, "").unwrap();
        write_manifest(
            config_dir.path(),
            &manifest("p-manifest", vec![("p-manifest", &manifest_choice)]),
        );
        fs::write(
            state_dir
                .path()
                .join(active_profile_persist::ACTIVE_PROFILE_FILE),
            "{ definitely not json",
        )
        .unwrap();

        let (path, identity) =
            resolve_startup_identity_and_path(None, config_dir.path(), state_dir.path());
        assert_eq!(path, config_dir.path().join("config.toml"));
        assert!(identity.is_none());
    }

    #[test]
    fn absent_identity_file_migrates_from_manifest_write_once() {
        // First boot after upgrade: manifest resolves → identity is restored
        // from it AND written into active_profile.json for subsequent boots.
        let config_dir = TempDir::new().unwrap();
        let state_dir = TempDir::new().unwrap();
        let profile_path = config_dir.path().join("studio.toml");
        fs::write(&profile_path, "").unwrap();
        write_manifest(
            config_dir.path(),
            &manifest("p-studio", vec![("p-studio", &profile_path)]),
        );

        let (path, identity) =
            resolve_startup_identity_and_path(None, config_dir.path(), state_dir.path());
        assert_eq!(path, profile_path);
        assert_eq!(
            identity.as_ref().and_then(|i| i.profile_id.as_deref()),
            Some("p-studio")
        );
        // The migration persisted the daemon's own record.
        match active_profile_persist::load(state_dir.path()) {
            LoadOutcome::Valid(p) => {
                assert_eq!(p.profile_id.as_deref(), Some("p-studio"));
                assert_eq!(p.config_path, profile_path);
            }
            other => panic!("migration must persist the identity, got {other:?}"),
        }
    }

    #[test]
    fn absent_identity_and_no_manifest_falls_back_to_default() {
        let config_dir = TempDir::new().unwrap();
        let state_dir = TempDir::new().unwrap();
        let (path, identity) =
            resolve_startup_identity_and_path(None, config_dir.path(), state_dir.path());
        assert_eq!(path, config_dir.path().join("config.toml"));
        assert!(identity.is_none());
        // No migration write when nothing resolved.
        assert_eq!(
            active_profile_persist::load(state_dir.path()),
            LoadOutcome::Absent
        );
    }

    #[test]
    fn explicit_config_is_ephemeral_no_identity_no_persist() {
        // Council (#2564): `--config` wins for the path but carries no
        // identity and never writes active_profile.json.
        let config_dir = TempDir::new().unwrap();
        let state_dir = TempDir::new().unwrap();
        let explicit = PathBuf::from("/explicit/override.toml");
        // Even with BOTH durable sources present…
        let daemon_choice = state_dir.path().join("daemon-profile.toml");
        fs::write(&daemon_choice, "").unwrap();
        write_identity(
            state_dir.path(),
            Some("p-daemon"),
            "Daemon Pick",
            &daemon_choice,
        );

        let (path, identity) = resolve_startup_identity_and_path(
            Some(explicit.clone()),
            config_dir.path(),
            state_dir.path(),
        );
        assert_eq!(path, explicit);
        assert!(identity.is_none());
        // …the persisted record is untouched (ephemeral boot).
        match active_profile_persist::load(state_dir.path()) {
            LoadOutcome::Valid(p) => assert_eq!(p.profile_id.as_deref(), Some("p-daemon")),
            other => panic!("explicit boot must not touch the record, got {other:?}"),
        }
    }

    #[test]
    fn stale_identity_path_falls_back_to_default_not_manifest() {
        // The persisted identity points at a profile file deleted while the
        // daemon was down: discard with warn → default config. NOT the
        // manifest (migration is strictly for Absent).
        let config_dir = TempDir::new().unwrap();
        let state_dir = TempDir::new().unwrap();
        let ghost = state_dir.path().join("deleted-profile.toml");
        write_identity(state_dir.path(), Some("p-ghost"), "Ghost", &ghost);
        let manifest_choice = config_dir.path().join("manifest-profile.toml");
        fs::write(&manifest_choice, "").unwrap();
        write_manifest(
            config_dir.path(),
            &manifest("p-manifest", vec![("p-manifest", &manifest_choice)]),
        );

        let (path, identity) =
            resolve_startup_identity_and_path(None, config_dir.path(), state_dir.path());
        assert_eq!(path, config_dir.path().join("config.toml"));
        assert!(identity.is_none());
    }

    #[test]
    fn manifest_with_paths_containing_backslashes_parses_cross_platform() {
        // #958 review bug 3: when this test file used `format!` +
        // `Path::display()` to embed paths into JSON, Windows runners
        // saw backslash-separated paths that broke `from_str`. The
        // production code parses `serde_json::Value`, so we must
        // construct the manifest with `serde_json::json!` (which
        // handles escaping). This test pins the `manifest()` helper.
        let tmp = TempDir::new().unwrap();
        let active_path = tmp.path().join("studio.toml");
        fs::write(&active_path, "").unwrap();

        let manifest_str = manifest("studio", vec![("studio", &active_path)]);
        // The manifest must be valid JSON regardless of platform.
        let parsed: serde_json::Value =
            serde_json::from_str(&manifest_str).expect("manifest must be valid JSON");
        assert_eq!(parsed["active_profile_id"], "studio");

        write_manifest(tmp.path(), &manifest_str);
        assert_eq!(resolve_startup_config_path(None, tmp.path()), active_path);
    }
}
