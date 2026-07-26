// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Preferences and daemon settings types (ADR-017)
//!
//! Two separate TOML files in `~/.conductor/`:
//! - `preferences.toml` — GUI preferences (theme, buffer sizes, etc.)
//! - `daemon.toml` — Daemon runtime settings (log level, analytics)
//!
//! Both files use `#[serde(default)]` everywhere so partial files work.
//! Missing files return `Default::default()`.

use serde::{Deserialize, Serialize};
use std::path::Path;

use super::types::default_true;

// ─── Default helpers ─────────────────────────────────────────────────────────

fn default_version() -> u8 {
    1
}

fn default_event_buffer_size() -> u32 {
    1000
}

fn default_midi_learn_timeout() -> u32 {
    10
}

fn default_log_level() -> String {
    "info".into()
}

// ─── GUI Preferences (preferences.toml) ─────────────────────────────────────

/// Top-level preferences file structure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuiPreferences {
    #[serde(default = "default_version")]
    pub version: u8,
    #[serde(default)]
    pub gui: GuiSettings,
    #[serde(default)]
    pub telemetry: TelemetrySettings,
}

impl Default for GuiPreferences {
    fn default() -> Self {
        Self {
            version: default_version(),
            gui: GuiSettings::default(),
            telemetry: TelemetrySettings::default(),
        }
    }
}

/// Crash-reporting & telemetry consent (ADR-048), nested under `[telemetry]`.
///
/// **GUI-only** — the OSS daemon carries no telemetry (ADR-048 D6). Every
/// flag defaults **off**: nothing is collected until the user explicitly
/// opts in via the first-run consent card. Crash reporting and usage
/// analytics are independent toggles (ADR-048 D2). The `install_id` is a
/// per-install random UUID (rotatable, never hardware-derived, never
/// correlated with licence/payment data — ADR-048 D5); `None` until first
/// generated, reset via Settings → Privacy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TelemetrySettings {
    /// Send crash/panic reports (Sentry). Opt-in.
    #[serde(default)]
    pub crash_reporting: bool,
    /// Send anonymous usage analytics (Aptabase). Opt-in.
    #[serde(default)]
    pub usage_analytics: bool,
    /// Whether the first-run consent card has been shown and answered.
    #[serde(default)]
    pub consent_prompted: bool,
    /// Per-install random UUID (rotatable, not hardware-derived).
    #[serde(default)]
    pub install_id: Option<String>,
}

/// GUI-specific settings nested under `[gui]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuiSettings {
    #[serde(default = "default_true")]
    pub minimize_to_tray: bool,
    #[serde(default = "default_true")]
    pub auto_save_configs: bool,
    #[serde(default = "default_true")]
    pub check_for_updates: bool,
    #[serde(default = "default_event_buffer_size")]
    pub event_buffer_size: u32,
    #[serde(default = "default_midi_learn_timeout")]
    pub midi_learn_timeout: u32,
    #[serde(default)]
    pub daemon_binary_path: String,
    /// ADR-029 Phase 4 — once set true, the macOS Input Monitoring
    /// onboarding sheet no longer auto-opens on launch. The user
    /// flips this by clicking "Don't show again" on the sheet.
    /// Default `false` so the sheet shows on first launch where TCC
    /// isn't granted yet. Loaded/saved alongside the rest of GUI
    /// prefs because adding a separate `get_setting`/`set_setting`
    /// command pair just for one bool would double the surface area.
    #[serde(default)]
    pub permissions_onboarding_dismissed: bool,
}

impl Default for GuiSettings {
    fn default() -> Self {
        Self {
            minimize_to_tray: true,
            auto_save_configs: true,
            check_for_updates: true,
            event_buffer_size: default_event_buffer_size(),
            midi_learn_timeout: default_midi_learn_timeout(),
            daemon_binary_path: String::new(),
            permissions_onboarding_dismissed: false,
        }
    }
}

// ─── Daemon Settings (daemon.toml) ──────────────────────────────────────────

/// Top-level daemon settings file structure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DaemonSettings {
    #[serde(default = "default_version")]
    pub version: u8,
    #[serde(default)]
    pub logging: DaemonLogging,
    #[serde(default)]
    pub analytics: DaemonAnalytics,
}

impl Default for DaemonSettings {
    fn default() -> Self {
        Self {
            version: default_version(),
            logging: DaemonLogging::default(),
            analytics: DaemonAnalytics::default(),
        }
    }
}

/// Daemon logging settings under `[logging]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DaemonLogging {
    #[serde(default = "default_log_level")]
    pub level: String,
}

impl Default for DaemonLogging {
    fn default() -> Self {
        Self {
            level: default_log_level(),
        }
    }
}

/// Daemon analytics settings under `[analytics]`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DaemonAnalytics {
    #[serde(default)]
    pub usage_tracking: bool,
}

// ─── File I/O helpers ───────────────────────────────────────────────────────

/// Load `preferences.toml` from the given directory.
///
/// Returns `Default` if the file is missing. Returns `Err` if the file
/// exists but cannot be parsed.
pub fn load_preferences(dir: &Path) -> Result<GuiPreferences, String> {
    let path = dir.join("preferences.toml");
    match std::fs::read_to_string(&path) {
        Ok(contents) => toml::from_str(&contents)
            .map_err(|e| format!("Failed to parse {}: {}", path.display(), e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(GuiPreferences::default()),
        Err(e) => Err(format!("Failed to read {}: {}", path.display(), e)),
    }
}

/// Load `daemon.toml` from the given directory.
///
/// Returns `Default` if the file is missing. Returns `Err` if the file
/// exists but cannot be parsed.
pub fn load_daemon_settings(dir: &Path) -> Result<DaemonSettings, String> {
    let path = dir.join("daemon.toml");
    match std::fs::read_to_string(&path) {
        Ok(contents) => toml::from_str(&contents)
            .map_err(|e| format!("Failed to parse {}: {}", path.display(), e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DaemonSettings::default()),
        Err(e) => Err(format!("Failed to read {}: {}", path.display(), e)),
    }
}

/// Atomically write a TOML file: serialize → temp file → fsync → rename → dir fsync.
///
/// Uses synchronous `std::fs` so it can be called from `spawn_blocking`.
/// Temp files use a random suffix to avoid collisions under concurrent writes.
/// Cleans up temp files on failure.
pub fn atomic_write_toml<T: Serialize>(path: &Path, data: &T) -> Result<(), String> {
    use std::io::Write;

    let parent = path
        .parent()
        .ok_or_else(|| "No parent directory".to_string())?;

    // Ensure directory exists
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("Failed to create directory {}: {}", parent.display(), e))?;

    // Generate unique temp file name using PID + nanos to avoid collisions.
    // Uses create_new(true) / O_EXCL below to guarantee no overwrites.
    let base_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("settings");
    let random_suffix: u64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
        ^ (std::process::id() as u64);

    let temp_path = parent.join(format!(".{}.{:016x}.tmp", base_name, random_suffix));

    let toml_str =
        toml::to_string_pretty(data).map_err(|e| format!("Failed to serialize TOML: {}", e))?;

    // Inner function does the write+fsync; caller cleans up temp on error
    let result = (|| -> Result<(), String> {
        // Create temp file exclusively (O_EXCL) to prevent symlink attacks
        // and collision overwrites. Restrictive permissions on Unix.
        #[cfg(unix)]
        let mut file = {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp_path)
                .map_err(|e| format!("Failed to create temp file: {}", e))?
        };
        #[cfg(not(unix))]
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .map_err(|e| format!("Failed to create temp file: {}", e))?;

        file.write_all(toml_str.as_bytes())
            .map_err(|e| format!("Failed to write temp file: {}", e))?;

        // Fsync the writable file descriptor to ensure data is on disk
        file.sync_all()
            .map_err(|e| format!("Failed to fsync: {}", e))?;
        drop(file);

        // Atomic rename — on Unix, rename(2) atomically replaces the target.
        // On Windows, std::fs::rename also replaces existing files atomically
        // (uses MoveFileExW with MOVEFILE_REPLACE_EXISTING internally).
        std::fs::rename(&temp_path, path)
            .map_err(|e| format!("Failed to rename temp file: {}", e))?;

        // Fsync parent directory to ensure the rename is durable (POSIX requirement)
        #[cfg(unix)]
        {
            if let Ok(dir) = std::fs::File::open(parent) {
                let _ = dir.sync_all();
            }
        }

        Ok(())
    })();

    // Clean up temp file on any failure
    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gui_preferences_default() {
        let prefs = GuiPreferences::default();
        assert_eq!(prefs.version, 1);
        assert!(prefs.gui.minimize_to_tray);
        assert!(prefs.gui.auto_save_configs);
        assert!(prefs.gui.check_for_updates);
        assert_eq!(prefs.gui.event_buffer_size, 1000);
        assert_eq!(prefs.gui.midi_learn_timeout, 10);
        assert!(prefs.gui.daemon_binary_path.is_empty());
        // ADR-029 Phase 4: the onboarding sheet defaults to "not yet
        // dismissed" so first-launch users see the macOS Input
        // Monitoring guidance.
        assert!(!prefs.gui.permissions_onboarding_dismissed);
    }

    #[test]
    fn test_gui_preferences_omitted_dismissed_field_defaults_false() {
        // Old preferences.toml files written before ADR-029 Phase 4 won't
        // contain `permissions_onboarding_dismissed`; serde must default
        // it to false rather than rejecting the file.
        let toml_str = "version = 1\n\
                        [gui]\n\
                        minimize_to_tray = true\n\
                        auto_save_configs = true\n\
                        check_for_updates = true\n\
                        event_buffer_size = 1000\n\
                        midi_learn_timeout = 10\n\
                        daemon_binary_path = \"\"\n";
        let prefs: GuiPreferences = toml::from_str(toml_str).unwrap();
        assert!(!prefs.gui.permissions_onboarding_dismissed);
    }

    #[test]
    fn test_daemon_settings_default() {
        let settings = DaemonSettings::default();
        assert_eq!(settings.version, 1);
        assert_eq!(settings.logging.level, "info");
        assert!(!settings.analytics.usage_tracking);
    }

    #[test]
    fn test_gui_preferences_roundtrip() {
        let prefs = GuiPreferences {
            version: 1,
            gui: GuiSettings {
                minimize_to_tray: false,
                auto_save_configs: true,
                check_for_updates: false,
                event_buffer_size: 500,
                midi_learn_timeout: 30,
                daemon_binary_path: "/usr/local/bin/conductor".into(),
                permissions_onboarding_dismissed: true,
            },
            telemetry: TelemetrySettings {
                crash_reporting: true,
                usage_analytics: false,
                consent_prompted: true,
                install_id: Some("00000000-0000-4000-8000-000000000000".into()),
            },
        };
        let toml_str = toml::to_string_pretty(&prefs).unwrap();
        let parsed: GuiPreferences = toml::from_str(&toml_str).unwrap();
        assert_eq!(prefs, parsed);
    }

    #[test]
    fn test_telemetry_defaults_off() {
        // ADR-048 D2: nothing collected until explicit opt-in.
        let t = TelemetrySettings::default();
        assert!(!t.crash_reporting, "crash reporting must default off");
        assert!(!t.usage_analytics, "usage analytics must default off");
        assert!(!t.consent_prompted, "consent must start un-prompted");
        assert!(t.install_id.is_none(), "no install id until generated");
    }

    #[test]
    fn test_telemetry_absent_section_defaults_off() {
        // An old preferences.toml with no [telemetry] section must
        // deserialize with telemetry fully off (forward-compat).
        let prefs: GuiPreferences = toml::from_str("version = 1\n").unwrap();
        assert_eq!(prefs.telemetry, TelemetrySettings::default());
    }

    #[test]
    fn test_daemon_settings_roundtrip() {
        let settings = DaemonSettings {
            version: 1,
            logging: DaemonLogging {
                level: "debug".into(),
            },
            analytics: DaemonAnalytics {
                usage_tracking: true,
            },
        };
        let toml_str = toml::to_string_pretty(&settings).unwrap();
        let parsed: DaemonSettings = toml::from_str(&toml_str).unwrap();
        assert_eq!(settings, parsed);
    }

    #[test]
    fn test_partial_parse_gui_preferences() {
        let toml_str = "version = 1\n";
        let prefs: GuiPreferences = toml::from_str(toml_str).unwrap();
        assert_eq!(prefs.version, 1);
        assert_eq!(prefs.gui, GuiSettings::default());
    }

    #[test]
    fn test_partial_parse_daemon_settings() {
        let toml_str = "[logging]\nlevel = \"debug\"\n";
        let settings: DaemonSettings = toml::from_str(toml_str).unwrap();
        assert_eq!(settings.logging.level, "debug");
        assert!(!settings.analytics.usage_tracking);
    }

    #[test]
    fn test_malformed_toml_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("preferences.toml"), "{{{").unwrap();
        let result = load_preferences(dir.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Failed to parse"));
    }

    #[test]
    fn test_missing_file_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let prefs = load_preferences(dir.path()).unwrap();
        assert_eq!(prefs, GuiPreferences::default());
        let settings = load_daemon_settings(dir.path()).unwrap();
        assert_eq!(settings, DaemonSettings::default());
    }

    #[test]
    fn test_atomic_write_toml_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preferences.toml");
        let prefs = GuiPreferences::default();
        atomic_write_toml(&path, &prefs).unwrap();
        let loaded = load_preferences(dir.path()).unwrap();
        assert_eq!(prefs, loaded);
    }

    #[test]
    fn test_atomic_write_creates_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("subdir").join("preferences.toml");
        let prefs = GuiPreferences::default();
        atomic_write_toml(&path, &prefs).unwrap();
        assert!(path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn test_atomic_write_sets_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preferences.toml");
        let prefs = GuiPreferences::default();
        atomic_write_toml(&path, &prefs).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
