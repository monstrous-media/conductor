// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! State persistence across daemon restarts

use crate::daemon::error::{DaemonError, Result};
use crate::daemon::types::{DaemonStatistics, DeviceStatus, ErrorEntry, LifecycleState};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Persisted daemon state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedState {
    pub version: u8,
    pub saved_at: u64, // Unix timestamp in seconds
    pub daemon: DaemonInfo,
    pub config: ConfigInfo,
    pub engine: EngineInfo,
    pub statistics: DaemonStatistics,
    pub last_errors: Vec<ErrorEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonInfo {
    pub lifecycle_state: LifecycleState,
    pub started_at: u64,
    pub pid: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigInfo {
    pub path: PathBuf,
    pub loaded_at: u64,
    pub checksum: String, // SHA256 hash of config content
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineInfo {
    pub current_mode: String,
    pub current_mode_index: usize,
    pub device_status: DeviceStatus,
}

impl PersistedState {
    /// Create a new persisted state
    pub fn new(
        daemon_info: DaemonInfo,
        config_info: ConfigInfo,
        engine_info: EngineInfo,
        statistics: DaemonStatistics,
        last_errors: Vec<ErrorEntry>,
    ) -> Self {
        Self {
            version: 1,
            saved_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            daemon: daemon_info,
            config: config_info,
            engine: engine_info,
            statistics,
            last_errors,
        }
    }
}

/// State manager for atomic persistence
#[derive(Clone)]
pub struct StateManager {
    state_file: PathBuf,
    state: Arc<RwLock<Option<PersistedState>>>,
}

impl StateManager {
    /// Create a new state manager
    pub fn new() -> Result<Self> {
        let state_dir = get_state_dir()?;
        let state_file = state_dir.join("state.json");

        Ok(Self {
            state_file,
            state: Arc::new(RwLock::new(None)),
        })
    }

    /// Load state from disk
    pub async fn load(&self) -> Result<Option<PersistedState>> {
        if !self.state_file.exists() {
            debug!("No state file found at {:?}", self.state_file);
            return Ok(None);
        }

        match tokio::fs::read_to_string(&self.state_file).await {
            Ok(json) => match serde_json::from_str::<PersistedState>(&json) {
                Ok(state) => {
                    info!("Loaded daemon state from {:?}", self.state_file);
                    *self.state.write().await = Some(state.clone());
                    Ok(Some(state))
                }
                Err(e) => {
                    warn!(
                        "Failed to parse state file {:?}, ignoring: {}",
                        self.state_file, e
                    );
                    Ok(None)
                }
            },
            Err(e) => {
                warn!(
                    "Failed to read state file {:?}, ignoring: {}",
                    self.state_file, e
                );
                Ok(None)
            }
        }
    }

    /// Save state to disk with atomic write
    pub async fn save(&self, state: PersistedState) -> Result<()> {
        let state_dir = self
            .state_file
            .parent()
            .ok_or_else(|| DaemonError::StatePersistence("No parent directory".to_string()))?;

        let temp_file = state_dir.join(".state.json.tmp");

        // 1. Serialize to string
        let json = serde_json::to_string_pretty(&state)?;

        // 2. Write to temporary file
        tokio::fs::write(&temp_file, json.as_bytes()).await?;

        // 3. Set secure permissions (owner-only read/write) on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = tokio::fs::metadata(&temp_file).await?.permissions();
            perms.set_mode(0o600); // rw-------
            tokio::fs::set_permissions(&temp_file, perms).await?;
        }

        // 4. Fsync to ensure data is on disk
        let file = tokio::fs::OpenOptions::new()
            .write(true)
            .open(&temp_file)
            .await?;
        file.sync_all().await?;

        // 5. Atomic rename (POSIX guarantees atomicity)
        tokio::fs::rename(&temp_file, &self.state_file).await?;

        debug!("Saved daemon state to {:?}", self.state_file);

        // Update in-memory state
        *self.state.write().await = Some(state);

        Ok(())
    }

    /// Get current in-memory state
    pub async fn get(&self) -> Option<PersistedState> {
        self.state.read().await.clone()
    }

    /// Emergency save (synchronous, for panic handler)
    pub fn save_emergency(&self, state: &PersistedState) -> Result<()> {
        let state_dir = self
            .state_file
            .parent()
            .ok_or_else(|| DaemonError::StatePersistence("No parent directory".to_string()))?;

        let temp_file = state_dir.join(".state.json.tmp");

        // Serialize
        let json = serde_json::to_string_pretty(state)?;

        // Write to temp file
        std::fs::write(&temp_file, json.as_bytes())?;

        // Set secure permissions (owner-only read/write) on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&temp_file)?.permissions();
            perms.set_mode(0o600); // rw-------
            std::fs::set_permissions(&temp_file, perms)?;
        }

        // Atomic rename
        std::fs::rename(&temp_file, &self.state_file)?;

        warn!("Emergency state saved to {:?}", self.state_file);

        Ok(())
    }
}

/// Get platform-specific state directory
pub fn get_state_dir() -> Result<PathBuf> {
    let dir = if cfg!(target_os = "macos") || cfg!(target_os = "linux") {
        dirs::home_dir()
            .ok_or_else(|| DaemonError::StatePersistence("No home directory".to_string()))?
            .join(".conductor")
    } else if cfg!(target_os = "windows") {
        dirs::data_dir()
            .ok_or_else(|| DaemonError::StatePersistence("No AppData directory".to_string()))?
            .join("conductor")
    } else {
        return Err(DaemonError::StatePersistence(
            "Unsupported platform".to_string(),
        ));
    };

    // Check if directory exists and validate ownership/permissions
    if dir.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;

            let metadata = std::fs::metadata(&dir)?;

            // Validate directory ownership matches current user
            let current_uid = unsafe { libc::getuid() };
            let dir_uid = metadata.uid();

            if dir_uid != current_uid {
                return Err(DaemonError::StatePersistence(format!(
                    "State directory {:?} is owned by UID {} but current user is UID {}. \
                     This is a security risk. Please remove the directory or fix ownership.",
                    dir, dir_uid, current_uid
                )));
            }

            // Validate directory permissions are secure (at most rwx------)
            let mode = metadata.mode();
            let world_perms = mode & 0o007;
            let group_perms = mode & 0o070;

            if world_perms != 0 || group_perms != 0 {
                warn!(
                    "State directory {:?} has insecure permissions {:o}. Fixing to 0700...",
                    dir,
                    mode & 0o777
                );

                use std::os::unix::fs::PermissionsExt;
                let mut perms = metadata.permissions();
                perms.set_mode(0o700); // rwx------
                std::fs::set_permissions(&dir, perms)?;
            }
        }
    } else {
        // Create directory with secure permissions
        std::fs::create_dir_all(&dir)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&dir)?.permissions();
            perms.set_mode(0o700); // rwx------
            std::fs::set_permissions(&dir, perms)?;
        }
    }

    Ok(dir)
}

/// Get platform-specific IPC socket path with secure user isolation
///
/// # Security Implementation
///
/// This function prevents multiple security vulnerabilities:
///
/// 1. **Multi-user isolation**: Uses user-specific directories, not shared /tmp
/// 2. **Symlink attacks**: Validates directory ownership before use
/// 3. **Race conditions**: Creates directories atomically with secure permissions
/// 4. **Privilege escalation**: Verifies directory ownership matches current user
///
/// # Path Selection Strategy
///
/// - **Linux**: Prefers XDG_RUNTIME_DIR (standard for user sockets), falls back to ~/.conductor/run
/// - **macOS**: Uses ~/Library/Application Support/conductor/run (platform convention)
/// - **Windows**: Uses named pipe (isolated per-user by OS)
///
/// # Directory Permissions
///
/// - Unix: 0700 (rwx------) - owner read/write/execute only
/// - Windows: Per-user named pipe (inherent OS-level isolation)
///
/// # Security Considerations
///
/// - XDG_RUNTIME_DIR (Linux) is tmpfs, auto-cleaned on logout, mode 0700 by systemd
/// - Fallback directories created with restricted permissions
/// - Ownership validation prevents takeover attacks
/// - Named pipes (Windows) are user-namespaced by OS
///
/// # Returns
///
/// User-specific socket/pipe path with parent directories created
///
/// # Errors
///
/// - Returns error if home directory cannot be determined
/// - Returns error if directory creation fails
/// - Returns error if permission setting fails
/// - Returns error if directory is owned by different user
pub fn get_socket_path() -> Result<PathBuf> {
    if cfg!(target_os = "windows") {
        // Named pipes on Windows are inherently user-isolated
        Ok(PathBuf::from(r"\\.\pipe\conductor"))
    } else {
        // Unix domain sockets (Linux/macOS)
        let socket_dir = get_runtime_dir()?;

        // Create directory with secure permissions if it doesn't exist
        if !socket_dir.exists() {
            std::fs::create_dir_all(&socket_dir)?;

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let perms = std::fs::Permissions::from_mode(0o700);
                std::fs::set_permissions(&socket_dir, perms)?;
            }

            debug!("Created secure runtime directory: {:?}", socket_dir);
        } else {
            // Directory exists - validate ownership and permissions for security
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                use std::os::unix::fs::PermissionsExt;

                let metadata = std::fs::metadata(&socket_dir)?;

                // Security check: Verify directory is owned by current user
                // This prevents attacks where another user creates the directory first
                let current_uid = unsafe { libc::getuid() };
                let dir_uid = metadata.uid();

                if dir_uid != current_uid {
                    return Err(DaemonError::StatePersistence(format!(
                        "Socket directory {:?} is owned by UID {} but current user is UID {}. \
                         This is a security risk. Please remove the directory or fix ownership.",
                        socket_dir, dir_uid, current_uid
                    )));
                }

                // Validate and fix insecure permissions
                let mode = metadata.mode();
                let world_perms = mode & 0o007;
                let group_perms = mode & 0o070;

                if world_perms != 0 || group_perms != 0 {
                    warn!(
                        "Socket directory {:?} has insecure permissions {:o}. Fixing to 0700...",
                        socket_dir,
                        mode & 0o777
                    );

                    let mut perms = metadata.permissions();
                    perms.set_mode(0o700);
                    std::fs::set_permissions(&socket_dir, perms)?;
                }
            }
        }

        Ok(socket_dir.join("conductor.sock"))
    }
}

/// Get platform-specific runtime directory for IPC sockets
///
/// Implements XDG Base Directory Specification for Linux and platform
/// conventions for macOS.
///
/// # Linux Path Priority
///
/// 1. **XDG_RUNTIME_DIR/conductor** - Standard per-user tmpfs (mode 0700, systemd-managed)
/// 2. **~/.conductor/run** - Fallback if XDG_RUNTIME_DIR not set
///
/// # macOS Path
///
/// **~/Library/Application Support/conductor/run** - Platform convention for app data
///
/// # Returns
///
/// Path to runtime directory (parent of socket file)
fn get_runtime_dir() -> Result<PathBuf> {
    if cfg!(target_os = "linux") {
        // Prefer XDG_RUNTIME_DIR (standard location for user sockets on Linux)
        // This is a per-user tmpfs managed by systemd with mode 0700
        if let Ok(xdg_runtime) = std::env::var("XDG_RUNTIME_DIR") {
            let dir = PathBuf::from(xdg_runtime).join("conductor");
            return Ok(dir);
        }

        // Fall back to ~/.conductor/run if XDG_RUNTIME_DIR not available
        let home = dirs::home_dir()
            .ok_or_else(|| DaemonError::StatePersistence("No home directory found".to_string()))?;
        Ok(home.join(".conductor").join("run"))
    } else if cfg!(target_os = "macos") {
        // Use Application Support directory (macOS convention)
        // This is typically ~/Library/Application Support
        let support_dir = dirs::data_dir().ok_or_else(|| {
            DaemonError::StatePersistence("No Application Support directory found".to_string())
        })?;
        Ok(support_dir.join("conductor").join("run"))
    } else {
        // Other Unix systems - use home directory fallback
        let home = dirs::home_dir()
            .ok_or_else(|| DaemonError::StatePersistence("No home directory found".to_string()))?;
        Ok(home.join(".conductor").join("run"))
    }
}

/// Calculate SHA256 checksum of a file
pub async fn calculate_checksum(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};

    let content = tokio::fs::read(path).await?;
    let mut hasher = Sha256::new();
    hasher.update(&content);
    let result = hasher.finalize();

    Ok(format!("sha256:{:x}", result))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_state_manager_save_and_load() {
        let state = PersistedState::new(
            DaemonInfo {
                lifecycle_state: LifecycleState::Running,
                started_at: 1000000,
                pid: 12345,
            },
            ConfigInfo {
                path: PathBuf::from("/tmp/config.toml"),
                loaded_at: 1000100,
                checksum: "sha256:abc123".to_string(),
            },
            EngineInfo {
                current_mode: "Default".to_string(),
                current_mode_index: 0,
                device_status: DeviceStatus::default(),
            },
            DaemonStatistics::default(),
            vec![],
        );

        let manager = StateManager::new().unwrap();

        // Save
        manager.save(state.clone()).await.unwrap();

        // Load
        let loaded = manager.load().await.unwrap().unwrap();

        assert_eq!(loaded.daemon.lifecycle_state, LifecycleState::Running);
        assert_eq!(loaded.daemon.pid, 12345);
        assert_eq!(loaded.config.path, PathBuf::from("/tmp/config.toml"));
        assert_eq!(loaded.engine.current_mode, "Default");
    }

    #[tokio::test]
    async fn test_state_manager_nonexistent_file() {
        // Use a unique temp dir to avoid races with other tests that
        // share the default ~/.conductor/state.json path.
        let tmp = tempfile::tempdir().unwrap();
        let manager = StateManager {
            state_file: tmp.path().join("nonexistent_state.json"),
            state: Arc::new(RwLock::new(None)),
        };

        let loaded = manager.load().await.unwrap();
        assert!(loaded.is_none());
    }

    #[test]
    fn test_get_state_dir() {
        let dir = get_state_dir().unwrap();

        if cfg!(target_os = "macos") || cfg!(target_os = "linux") {
            assert!(dir.ends_with(".conductor"));
        } else if cfg!(target_os = "windows") {
            assert!(dir.ends_with("conductor"));
        }

        // Directory should be created
        assert!(dir.exists());
    }

    #[test]
    fn test_get_socket_path() {
        let path = get_socket_path().unwrap();

        if cfg!(target_os = "windows") {
            // Windows uses named pipes
            assert_eq!(path, PathBuf::from(r"\\.\pipe\conductor"));
        } else {
            // Unix platforms use user-specific directories
            assert!(path.ends_with("conductor.sock"));

            // Verify the path is NOT in /tmp (security requirement)
            assert!(
                !path.starts_with("/tmp"),
                "Socket path should not be in shared /tmp directory: {:?}",
                path
            );

            // Verify path includes user-specific component
            if cfg!(target_os = "linux") {
                // Should be $XDG_RUNTIME_DIR/conductor/conductor.sock or
                // ~/.conductor/run/conductor.sock — the only two paths
                // `get_runtime_dir()` produces on Linux. Resolve
                // $XDG_RUNTIME_DIR to its value before checking — the pre-fix
                // code did `path_str.contains("XDG_RUNTIME_DIR")`, which
                // doesn't reliably detect XDG paths because it matches the
                // env-var name rather than its value.
                //
                // Assert the socket path's parent dir is EXACTLY
                // `$XDG_RUNTIME_DIR/conductor` — `Path::starts_with` alone
                // would pass for any deeper path like
                // `$XDG_RUNTIME_DIR/conductor/extra/conductor.sock`, even
                // though the impl produces only `$XDG_RUNTIME_DIR/conductor/conductor.sock`.
                // Parent-equality catches both the sibling-dir regressions
                // (rejected via the missing-subdir case) and any accidental
                // extra-subdir regressions.
                //
                // Intentionally NOT using a loose `path_str.contains("/run/user/")`
                // heuristic — when XDG_RUNTIME_DIR is set (the typical case
                // on systemd-based distros) the `is_xdg` check covers it
                // exactly; when it's unset the impl falls through to
                // `~/.conductor/run/`. Adding a /run/user/ contains-check
                // would mask regressions that emit a sibling directory under
                // the user runtime root (e.g. `/run/user/1000/conductor.sock`
                // missing the `/conductor/` subdir).
                let xdg_runtime = std::env::var("XDG_RUNTIME_DIR").ok();
                let is_xdg = xdg_runtime.as_deref().is_some_and(|v| {
                    let expected_parent = PathBuf::from(v).join("conductor");
                    path.parent() == Some(expected_parent.as_path())
                });
                let path_str = path.to_string_lossy();
                let is_home_fallback = path_str.contains(".conductor/run");
                assert!(
                    is_xdg || is_home_fallback,
                    "Linux socket path should use $XDG_RUNTIME_DIR/conductor or ~/.conductor/run: {:?}",
                    path
                );
            } else if cfg!(target_os = "macos") {
                // Should be in Application Support
                assert!(
                    path.to_string_lossy().contains("Application Support")
                        || path.to_string_lossy().contains(".conductor/run"),
                    "macOS socket path should be in Application Support or ~/.conductor/run: {:?}",
                    path
                );
            }

            // Verify directory was created with secure permissions
            let socket_dir = path.parent().expect("Socket should have parent directory");
            assert!(socket_dir.exists(), "Socket directory should be created");

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let metadata = std::fs::metadata(socket_dir).expect("Should get metadata");
                let mode = metadata.permissions().mode();
                let perms = mode & 0o777;

                assert_eq!(
                    perms, 0o700,
                    "Socket directory should have 0700 permissions, got {:o}",
                    perms
                );
            }
        }
    }

    #[test]
    fn test_get_runtime_dir() {
        let dir = get_runtime_dir().unwrap();

        if cfg!(target_os = "linux") {
            // Assert exact equality with `$XDG_RUNTIME_DIR/conductor` —
            // `get_runtime_dir()` returns the runtime dir itself (not a
            // sub-path), so a `starts_with`/prefix match would be too
            // permissive (would accept e.g. `$XDG_RUNTIME_DIR/conductor/run`).
            // See test_get_socket_path for the rationale on dropping the
            // loose `/run/user/` contains-heuristic.
            let xdg_runtime = std::env::var("XDG_RUNTIME_DIR").ok();
            let is_xdg = xdg_runtime
                .as_deref()
                .is_some_and(|v| dir == PathBuf::from(v).join("conductor"));
            let dir_str = dir.to_string_lossy();
            let is_home = dir_str.contains(".conductor/run");
            assert!(
                is_xdg || is_home,
                "Linux runtime dir should be $XDG_RUNTIME_DIR/conductor or ~/.conductor/run: {:?}",
                dir
            );
        } else if cfg!(target_os = "macos") {
            // Should be in Application Support
            assert!(
                dir.to_string_lossy().contains("Application Support")
                    || dir.to_string_lossy().contains(".conductor/run"),
                "macOS runtime dir should be in Application Support: {:?}",
                dir
            );
        }
    }

    /// Regression test: lock in the corrected predicate.
    ///
    /// The pre-fix code used `path_str.contains("XDG_RUNTIME_DIR")` to
    /// "verify" the XDG branch. That predicate doesn't reliably detect XDG
    /// paths because it matches the env-var *name* rather than its value:
    /// for any normal `XDG_RUNTIME_DIR` setting (e.g. `/run/user/1000`)
    /// the literal substring `XDG_RUNTIME_DIR` never appears in the resolved
    /// path. The assertion only ever passed via the unrelated `/run/user/`
    /// heuristic (which happens to match because `XDG_RUNTIME_DIR` is
    /// conventionally `/run/user/{uid}` on systemd-based distros).
    ///
    /// This test exercises the predicate logic directly with synthetic
    /// strings — no env-var manipulation, no global state — to lock in
    /// that the post-fix predicate (`Path::starts_with` against
    /// `$XDG_RUNTIME_DIR/conductor`) correctly identifies XDG-rooted paths
    /// even when `XDG_RUNTIME_DIR` is set to a non-conventional location
    /// like `/run/custom-xdg`, AND that it rejects sibling directories
    /// that share the XDG prefix but skip the `conductor` subdir.
    #[test]
    fn test_xdg_predicate_resolves_env_var_value_not_literal() {
        // Synthetic: simulate XDG_RUNTIME_DIR=/run/custom-xdg
        let xdg_runtime = "/run/custom-xdg";
        let socket_path = PathBuf::from("/run/custom-xdg/conductor/conductor.sock");
        let runtime_dir_expected = PathBuf::from(xdg_runtime).join("conductor");

        // Pre-fix predicate (literal-string match on env-var NAME) does not
        // reliably detect XDG paths
        let path_str = socket_path.to_string_lossy();
        assert!(
            !path_str.contains("XDG_RUNTIME_DIR"),
            "literal-string check should not reliably match a resolved XDG path"
        );

        // ===== Socket-path contract: parent EQUALS $XDG/conductor =====
        // The integration test (`test_get_socket_path`) uses
        // `path.parent() == Some($XDG/conductor)`. Equality is stricter
        // than `Path::starts_with` and catches the extra-subdir
        // regression class that `starts_with` would have masked
        // (e.g. `$XDG/conductor/extra/conductor.sock`).

        // ACCEPT: real socket path produced by get_socket_path()
        assert_eq!(
            socket_path.parent(),
            Some(runtime_dir_expected.as_path()),
            "post-fix predicate should accept XDG-rooted socket path: {:?}",
            socket_path
        );

        // Sanity: synthetic path does NOT match either of the other branches
        assert!(
            !path_str.contains("/run/user/"),
            "should not match the conventional /run/user/ heuristic"
        );
        assert!(
            !path_str.contains(".conductor/run"),
            "should not match the home-dir fallback"
        );

        // REJECT: missing /conductor/ subdir entirely
        let bad_no_subdir = PathBuf::from("/run/custom-xdg/conductor.sock");
        assert_ne!(
            bad_no_subdir.parent(),
            Some(runtime_dir_expected.as_path()),
            "predicate must reject XDG-rooted path without /conductor/ subdir: {:?}",
            bad_no_subdir
        );

        // REJECT: sibling subdir under XDG
        let bad_sibling = PathBuf::from("/run/custom-xdg/something-else/conductor.sock");
        assert_ne!(
            bad_sibling.parent(),
            Some(runtime_dir_expected.as_path()),
            "predicate must reject sibling subdir under XDG: {:?}",
            bad_sibling
        );

        // REJECT: path-component prefix-match without exact `conductor` segment.
        // `Path` semantics treat `conductorX` and `conductor` as different
        // components, so this is correctly rejected — locks in that we use
        // `Path` rather than `str` semantics, where `conductorX` would
        // string-prefix-match `conductor`.
        let bad_prefix_match = PathBuf::from("/run/custom-xdg/conductorX/conductor.sock");
        assert_ne!(
            bad_prefix_match.parent(),
            Some(runtime_dir_expected.as_path()),
            "predicate must reject path-component prefix-match without exact `conductor` segment: {:?}",
            bad_prefix_match
        );

        // REJECT: extra subdir under `conductor` (the round-4 Copilot case).
        // `Path::starts_with` would have falsely accepted this — equality
        // on the parent dir catches it.
        let bad_extra_subdir = PathBuf::from("/run/custom-xdg/conductor/extra/conductor.sock");
        assert_ne!(
            bad_extra_subdir.parent(),
            Some(runtime_dir_expected.as_path()),
            "predicate must reject XDG path with extra subdir between conductor and conductor.sock: {:?}",
            bad_extra_subdir
        );

        // ===== Runtime-dir contract: dir EQUALS $XDG/conductor =====
        // The integration test (`test_get_runtime_dir`) uses
        // `dir == $XDG/conductor` — get_runtime_dir() returns the runtime
        // dir itself, not a sub-path, so equality is the right contract
        // (Path::starts_with would falsely accept deeper sub-paths).

        // ACCEPT: real runtime dir
        let real_runtime_dir = PathBuf::from("/run/custom-xdg/conductor");
        assert_eq!(
            real_runtime_dir, runtime_dir_expected,
            "post-fix runtime-dir predicate should accept XDG-rooted dir: {:?}",
            real_runtime_dir
        );

        // REJECT: deeper subdir under `conductor` (the round-4 Copilot case).
        // `Path::starts_with` would have falsely accepted this.
        let bad_deep_runtime = PathBuf::from("/run/custom-xdg/conductor/run");
        assert_ne!(
            bad_deep_runtime, runtime_dir_expected,
            "predicate must reject XDG runtime dir with deeper subdir: {:?}",
            bad_deep_runtime
        );

        // ===== Conventional Linux XDG case =====
        // Prove the tightened predicate covers `/run/user/{uid}/conductor`
        // exactly without needing the dropped `/run/user/` contains-heuristic.
        let conventional_xdg = "/run/user/1000";
        let conventional_socket =
            PathBuf::from(format!("{}/conductor/conductor.sock", conventional_xdg));
        let conventional_runtime_dir = PathBuf::from(conventional_xdg).join("conductor");
        assert_eq!(
            conventional_socket.parent(),
            Some(conventional_runtime_dir.as_path()),
            "tightened predicate alone must accept conventional /run/user/{{uid}}/conductor/conductor.sock — proves the dropped /run/user/ heuristic was redundant"
        );

        // And the tightened predicate ALSO rejects the regression that the
        // dropped heuristic would have masked: /run/user/{uid}/conductor.sock
        // (no /conductor/ subdir).
        let bad_under_run_user = PathBuf::from(format!("{}/conductor.sock", conventional_xdg));
        assert_ne!(
            bad_under_run_user.parent(),
            Some(conventional_runtime_dir.as_path()),
            "tightened predicate must reject /run/user/{{uid}}/conductor.sock (no /conductor/ subdir): {:?}",
            bad_under_run_user
        );
    }
}
