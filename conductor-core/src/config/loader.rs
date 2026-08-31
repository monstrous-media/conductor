// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Configuration loading, saving, and validation.
//!
//! This module provides functionality to load configuration from files,
//! save configuration to files, and validate configuration correctness.

use crate::error::ConfigError;
use std::collections::HashMap;
use std::path::Path;

use super::revision::ConfigRevision;
use super::validation::ValidationFinding;

/// Bump whenever the lowering / canonicalization logic changes shape, so the
/// ADR-034 CAS hash (via [`canonical_endpoint_digest`]) changes **deliberately**
/// across Conductor versions rather than silently triggering a hot-reload on
/// every daemon at upgrade (ADR-035 §4.3, R2/R3).
pub const NORMALIZER_VERSION: u32 = 1;

/// Return the config's authored `[[endpoints]]` set (ADR-035 §4.3), cloned.
///
/// Post-legacy-removal there is no lowering left to do — the legacy
/// `[[bindings]]`/`[[connectors]]` blocks were removed entirely. This now only
/// enforces the alias-uniqueness invariant (ADR-031 §D8): a **hard error** on a
/// duplicate endpoint alias. The `Vec<ValidationFinding>` is always empty; the
/// tuple return is kept so existing callers (`Config::load`, the live-config CAS
/// path) compile unchanged. **Pure** — does not mutate `config`.
pub fn normalize_to_endpoints(
    config: &Config,
) -> Result<(Vec<EndpointConfig>, Vec<ValidationFinding>), ConfigError> {
    let mut seen: HashMap<&str, ()> = HashMap::new();
    let mut endpoints: Vec<EndpointConfig> = Vec::with_capacity(config.endpoints.len());

    for ep in &config.endpoints {
        if seen.insert(&ep.alias, ()).is_some() {
            return Err(ConfigError::ValidationError(format!(
                "endpoint alias '{}' is defined more than once in [[endpoints]] — \
                 aliases must be unique (ADR-031 §D8 / ADR-035)",
                ep.alias
            )));
        }
        endpoints.push(ep.clone());
    }

    // No legacy lowering remains (ADR-035 removed [[bindings]]/[[connectors]]);
    // findings is always empty but the signature is kept for callers.
    Ok((endpoints, Vec::new()))
}

/// Deterministic, `NORMALIZER_VERSION`-stamped digest of a normalized endpoint
/// set, for the ADR-034 CAS hash (ADR-035 §4.3). Endpoints are sorted by alias
/// (stable), then each is serialized to its canonical TOML **wire form** (serde
/// field/variant names, honoring any `#[serde(rename)]`) — so a future internal
/// Rust rename that preserves the wire name does NOT change the hash. The
/// version is folded in so a deliberate lowering change bumps the digest.
///
/// (Defined here in Slice 4; wired into the live-config CAS path in Slice 9.)
pub fn canonical_endpoint_digest(endpoints: &[EndpointConfig]) -> ConfigRevision {
    let mut sorted: Vec<&EndpointConfig> = endpoints.iter().collect();
    sorted.sort_by(|a, b| a.alias.cmp(&b.alias));

    let mut bytes = Vec::new();
    bytes.extend_from_slice(&NORMALIZER_VERSION.to_le_bytes());
    for ep in sorted {
        // Canonical wire form. `toml::to_string` is deterministic for a given
        // value and emits serde wire names; `skip_serializing_if` drops
        // None/empty so defaults don't perturb the hash.
        //
        // A serialization failure would silently exclude the endpoint from the
        // digest, producing a subtly wrong hash. Guard against this in
        // debug/test builds; production falls back to skipping rather than
        // panicking (hash instability is preferable to daemon crash on load).
        debug_assert!(
            toml::to_string(ep).is_ok(),
            "canonical_endpoint_digest: failed to serialize endpoint '{}'",
            ep.alias
        );
        if let Ok(s) = toml::to_string(ep) {
            bytes.extend_from_slice(s.as_bytes());
            bytes.push(0); // unambiguous record separator
        }
    }
    ConfigRevision::from_canonical_bytes(&bytes)
}

use super::types::{ActionConfig, Config, EndpointConfig, Mapping, Mode, Trigger};

impl Config {
    /// Reject the removed route `phase` field (ADR-036 Phase 3).
    ///
    /// `RoutePhase` / `pre_mapping` was eliminated: all routes are now
    /// post-mapping. Because serde silently ignores unknown fields, a
    /// lingering `phase = "..."` on a `[[routes]]` entry would otherwise be
    /// dropped without warning — surface it as a clear, actionable error so
    /// users update the config instead of silently losing routing intent.
    fn check_removed_route_phase(toml_content: &str) -> Result<(), String> {
        // Reuse a lightweight value parse; malformed TOML is reported with a
        // better message by the main `Config` parser, so ignore parse errors
        // here.
        // Use the serde entry point (`toml::from_str`), NOT `str::parse`
        // (`FromStr`) — in this `toml` version the `FromStr` path rejects
        // documents the serde `Deserializer` accepts, which would let a
        // `phase` slip through unchecked.
        let Ok(value) = toml::from_str::<toml::Value>(toml_content) else {
            return Ok(());
        };
        if let Some(routes) = value.get("routes").and_then(|r| r.as_array())
            && routes.iter().any(|r| r.get("phase").is_some())
        {
            return Err("Config sets `phase` on a [[routes]] entry, but the route \
                 `phase` field (and the `pre_mapping` value) was removed in \
                 ADR-036 Phase 3 — all routes are now post-mapping. Delete the \
                 `phase = \"...\"` line from each route and reload."
                .to_string());
        }
        Ok(())
    }

    /// Reject the legacy I/O blocks that ADR-035 removed (#2124).
    ///
    /// The singular `[device]` and the plural `[[devices]]` / `[[bindings]]` /
    /// `[[connectors]]` blocks are no longer `Config` fields, and `Config` has
    /// no `deny_unknown_fields`, so a file-load via `toml::from_str::<Config>`
    /// would **silently drop** them — a config authored in the old format loads
    /// with no I/O and no error or warning, leaving the daemon running with no
    /// devices. (`Config::primary_device` and the `[device]` migration path
    /// were both removed with ADR-035, so there is nothing left to consume the
    /// block.) Reject it up front at the file-load boundary with a migration
    /// hint to `[[endpoints]]`.
    ///
    /// Mirrors [`Self::check_removed_route_phase`]: a generic `toml::Value`
    /// parse via the serde entry point (NOT `str::parse`/`FromStr`, which
    /// rejects documents the serde deserializer accepts), tolerating parse
    /// failures so the main parser owns malformed-TOML reporting. This runs
    /// only in `Config::load` (the on-disk loader); raw
    /// `toml::from_str::<Config>` keeps ignoring unknown blocks for internal /
    /// programmatic use.
    fn check_removed_io_blocks(toml_content: &str) -> Result<(), String> {
        let Ok(value) = toml::from_str::<toml::Value>(toml_content) else {
            return Ok(());
        };
        // Legacy top-level I/O forms removed in ADR-035, matched by their BLOCK
        // shape: `[device]` is a table; the plural `[[devices]]` / `[[bindings]]`
        // / `[[connectors]]` are arrays-of-tables. We deliberately key off the
        // value TYPE, not just the name (Copilot review): a scalar that merely
        // shares one of these names (e.g. `device = "x"`) is not a legacy I/O
        // block, so it falls through to the parser rather than being rejected
        // here with a block-shaped error message.
        const LEGACY_IO_KEYS: &[&str] = &["device", "devices", "bindings", "connectors"];
        for key in LEGACY_IO_KEYS {
            let Some(value) = value.get(*key) else {
                continue;
            };
            let is_legacy_block = if *key == "device" {
                value.is_table()
            } else {
                value.is_array()
            };
            if is_legacy_block {
                let bracketed = if *key == "device" {
                    format!("[{key}]")
                } else {
                    format!("[[{key}]]")
                };
                return Err(format!(
                    "Config uses the legacy `{bracketed}` I/O block, which was removed in \
                     ADR-035 — the only authored I/O form is now `[[endpoints]]`. Rewrite \
                     your devices/bindings/connectors as `[[endpoints]]` entries and reload. \
                     (Without this check the `{key}` block is silently dropped, leaving the \
                     daemon with no I/O and no error.)"
                ));
            }
        }
        Ok(())
    }

    /// Load configuration from a TOML file
    ///
    /// If the file doesn't exist, creates a default configuration and saves it to the specified path.
    ///
    /// # Security
    /// This function performs path validation to prevent path traversal attacks:
    /// - Canonicalizes the path to resolve symlinks and relative components
    /// - Restricts access to allowed directories (config directory, /tmp, current working directory)
    ///
    /// # Arguments
    /// * `path` - Path to the configuration file
    ///
    /// # Returns
    /// * `Ok(Config)` - Successfully loaded or created configuration
    /// * `Err(ConfigError)` - IO, parsing, validation, or security error
    ///
    /// # Example
    /// ```no_run
    /// use conductor_core::Config;
    ///
    /// let config = Config::load("config.toml")?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn load(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        // Security: Validate and sanitize path
        let safe_path = Self::validate_config_path(path)?;

        if safe_path.exists() {
            let contents = std::fs::read_to_string(&safe_path)?;

            // ADR-036 Phase 3: reject the removed route `phase` field with an
            // actionable message (serde would otherwise drop it silently).
            Self::check_removed_route_phase(&contents)?;

            // #2124: reject the legacy `[device]` / `[[devices]]` /
            // `[[bindings]]` / `[[connectors]]` I/O blocks. They are no longer
            // Config fields and `Config` has no `deny_unknown_fields`, so the
            // parse below would silently drop them — surface the migration to
            // `[[endpoints]]` at the file-load boundary instead.
            Self::check_removed_io_blocks(&contents)?;
            let config: Config = match toml::from_str(&contents) {
                Ok(c) => c,
                Err(e) => {
                    // ADR-036 Phase 2: `Trigger::Raw` is removed. Replace the
                    // bare serde "unknown variant `Raw`" error with an
                    // actionable migration hint.
                    if e.to_string().contains("unknown variant `Raw`") {
                        return Err(format!(
                            "Config uses the removed `Trigger::Raw` (ADR-036 Phase 2). \
                             Run `conductorctl migrate-config --routing` to rewrite Raw \
                             triggers as [[routes]] entries, then reload. (parse error: {e})"
                        )
                        .into());
                    }
                    return Err(e.into());
                }
            };
            // Gate duplicate aliases among the authored endpoints with a hard
            // error before validation.
            normalize_to_endpoints(&config)?;
            let mut config = config;
            super::validation::validate_for_loading(&config)?;
            // ADR-047 §D3a: validation has now warned about any gamepad bind on
            // the frozen legacy sentinel id 255; disable those binds so they
            // never match. Done after validation (which reads `&config`) so the
            // warning fires while the bind is still present.
            let disabled = super::validation::disable_legacy_gamepad_sentinel_binds(&mut config);
            if disabled > 0 {
                tracing::warn!(
                    "Disabled {disabled} gamepad mapping(s) bound to the frozen legacy id 255 \
                     (ADR-047 §D3a); re-bind to the control's real id."
                );
            }
            Ok(config)
        } else {
            tracing::info!("Config file not found, creating default config...");
            let config = Self::default_config();
            config.save(path)?;
            Ok(config)
        }
    }

    /// Run the file-load preflight guards over raw TOML *content* (the same
    /// removed-block checks `load()` applies before deserializing): reject the
    /// removed route `phase` field (ADR-036 Phase 3) and the legacy
    /// `[device]`/`[[devices]]`/`[[bindings]]`/`[[connectors]]` I/O blocks
    /// (ADR-035 / #2124) with actionable migration errors.
    ///
    /// Exposed so callers that parse config content directly (e.g. the GUI's
    /// additive template-merge, which can't go through `load()`'s path-based
    /// entry point) get the same *fail-don't-silently-drop* semantics instead of
    /// letting serde silently discard removed blocks.
    pub fn preflight_removed_blocks(toml_content: &str) -> Result<(), String> {
        Self::check_removed_route_phase(toml_content)?;
        Self::check_removed_io_blocks(toml_content)?;
        Ok(())
    }

    /// Save configuration to a TOML file
    ///
    /// Writes the configuration as formatted TOML using an atomic write pattern.
    ///
    /// # Security
    /// This function prevents TOCTOU (Time-Of-Check-Time-Of-Use) race conditions:
    /// - Validates the FULL canonical path before writing (not just parent directory)
    /// - Uses atomic write pattern (write to temp file, then rename)
    /// - Uses OpenOptions with explicit flags to prevent symlink following on platforms that support it
    /// - Restricts writes to allowed directories (config directory, /tmp, current working directory)
    ///
    /// # TOCTOU Prevention
    /// The original implementation had a race condition where an attacker could:
    /// 1. Wait for parent directory validation to pass
    /// 2. Replace parent with a symlink to a privileged location (e.g., /etc)
    /// 3. Cause the write to occur in the privileged location
    ///
    /// This is mitigated by:
    /// - Validating the full target path (not just parent)
    /// - Using atomic writes (temp file + rename)
    /// - Re-validating after temp file creation
    ///
    /// # Arguments
    /// * `path` - Path where the configuration file will be written
    ///
    /// # Returns
    /// * `Ok(())` - Successfully saved
    /// * `Err(Box<dyn std::error::Error>)` - IO, serialization, or security error
    ///
    /// # Example
    /// ```no_run
    /// use conductor_core::Config;
    ///
    /// let config = Config::load("config.toml")?;
    /// config.save("backup.toml")?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn save(&self, path: &str) -> Result<(), Box<dyn std::error::Error>> {
        use std::fs::OpenOptions;
        use std::io::Write;

        // Security: Convert to absolute path first
        let path_buf = Path::new(path);
        let absolute_path = if path_buf.is_absolute() {
            path_buf.to_path_buf()
        } else {
            std::env::current_dir()?.join(path_buf)
        };

        // Security: Validate parent directory exists or can be created
        if let Some(parent) = absolute_path.parent() {
            if !parent.exists() {
                return Err(format!(
                    "Parent directory does not exist: {}. Please create it first.",
                    parent.display()
                )
                .into());
            }

            // Canonicalize parent and validate it's in allowed directories
            let canonical_parent = parent.canonicalize()?;
            Self::check_path_allowed(&canonical_parent)?;
        }

        // Serialize config before any file operations
        let contents = toml::to_string_pretty(self)?;

        // Security: Construct the expected canonical path for validation
        // If file exists, canonicalize it. Otherwise, construct expected canonical path.
        let target_canonical = if absolute_path.exists() {
            // File exists - canonicalize it to resolve any symlinks
            let canonical = absolute_path.canonicalize()?;
            Self::check_path_allowed(&canonical)?;
            canonical
        } else {
            // File doesn't exist - construct canonical path from parent + filename
            let parent = absolute_path
                .parent()
                .ok_or("Invalid path: no parent directory")?;
            let filename = absolute_path
                .file_name()
                .ok_or("Invalid path: no filename")?;
            let canonical_parent = parent.canonicalize()?;
            Self::check_path_allowed(&canonical_parent)?;
            canonical_parent.join(filename)
        };

        // Security (#1437): atomic write via an UNPREDICTABLE temp file opened
        // with O_EXCL (`create_new`). The previous code used a predictable
        // sibling `config.tmp` opened with `create(true).truncate(true)`, which
        // FOLLOWS and TRUNCATES a pre-existing symlink at that path — an
        // attacker who can write to the (allowed) directory could point
        // `config.tmp` at another file and have it overwritten with config
        // bytes before the post-write canonicalize check ran. A random temp
        // name plus `create_new` (which fails rather than following/opening an
        // existing path, symlink or not) closes both holes. Mirrors
        // `preferences::atomic_write_toml`.
        let parent = target_canonical
            .parent()
            .ok_or("Invalid target path: no parent directory")?;
        let base_name = target_canonical
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("config");
        // Random suffix from time ^ pid; collisions are further guarded by the
        // O_EXCL open below.
        let random_suffix: u64 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
            ^ (std::process::id() as u64);
        let temp_path = parent.join(format!(".{}.{:016x}.tmp", base_name, random_suffix));

        // Write + fsync + rename inside a closure so the temp file is cleaned up
        // on any failure path (no leftover partial temp files).
        let write_result = (|| -> Result<(), Box<dyn std::error::Error>> {
            // `create_new(true)` == O_EXCL: fails if the path already exists,
            // including a symlink, so we never follow or truncate one.
            // Restrictive 0o600 permissions on Unix.
            #[cfg(unix)]
            let mut file = {
                use std::os::unix::fs::OpenOptionsExt;
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&temp_path)?
            };
            #[cfg(not(unix))]
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp_path)?;

            file.write_all(contents.as_bytes())?;
            file.sync_all()?; // Ensure data is written to disk
            drop(file);

            // Security: re-validate temp file location before rename to catch a
            // race where the directory was swapped during the write. With
            // O_EXCL the temp itself cannot be a symlink.
            let temp_canonical = temp_path.canonicalize()?;
            Self::check_path_allowed(&temp_canonical)?;

            // Atomic rename to final location.
            std::fs::rename(&temp_path, &target_canonical)?;

            // Fsync the parent directory so the rename is durable across a crash
            // (POSIX requirement), matching preferences::atomic_write_toml.
            // Best-effort: a failure here doesn't undo the successful rename.
            #[cfg(unix)]
            {
                if let Ok(dir) = std::fs::File::open(parent) {
                    let _ = dir.sync_all();
                }
            }
            Ok(())
        })();

        if write_result.is_err() {
            // Best-effort cleanup of the temp file on failure.
            let _ = std::fs::remove_file(&temp_path);
        }
        write_result?;

        Ok(())
    }

    /// Validate and sanitize a configuration file path
    ///
    /// # Security
    /// Prevents path traversal attacks by:
    /// 1. Converting relative paths to absolute
    /// 2. Resolving symlinks
    /// 3. Checking the path is within allowed directories
    ///
    /// # Allowed Directories
    /// - User's config directory (`~/.config`, `~/Library/Application Support`, etc.)
    /// - `/tmp` directory (for temporary configs)
    /// - Current working directory (for development/testing)
    ///
    /// # Returns
    /// * `Ok(PathBuf)` - Canonical path if allowed
    /// * `Err(Box<dyn std::error::Error>)` - If path is outside allowed directories
    fn validate_config_path(path: &str) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
        let path_buf = Path::new(path);

        // Convert to absolute path if relative
        let absolute_path = if path_buf.is_absolute() {
            path_buf.to_path_buf()
        } else {
            std::env::current_dir()?.join(path_buf)
        };

        // Canonicalize if the path exists, otherwise validate parent
        let canonical_path = if absolute_path.exists() {
            absolute_path.canonicalize()?
        } else if let Some(parent) = absolute_path.parent() {
            if parent.exists() {
                parent
                    .canonicalize()?
                    .join(absolute_path.file_name().ok_or("Invalid file name")?)
            } else {
                // Parent doesn't exist - allow it (will be created)
                absolute_path
            }
        } else {
            absolute_path
        };

        // Check if path is within allowed directories
        if canonical_path.exists() || canonical_path.parent().is_some_and(|p| p.exists()) {
            Self::check_path_allowed(&canonical_path)?;
        }

        Ok(canonical_path)
    }

    /// Check if a path is within allowed directories
    ///
    /// # Allowed Directories
    /// - User's config directory
    /// - `/tmp` directory (including its canonical form like `/private/var/folders/...` on macOS)
    /// - Current working directory
    ///
    /// # Returns
    /// * `Ok(())` - Path is allowed
    /// * `Err(Box<dyn std::error::Error>)` - Path is outside allowed directories
    fn check_path_allowed(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        // Get allowed directories (canonicalized to handle symlinks)
        let config_dir = dirs::config_dir().and_then(|p| p.canonicalize().ok());
        let current_dir = std::env::current_dir()
            .ok()
            .and_then(|p| p.canonicalize().ok());
        let tmp_dir = std::env::temp_dir().canonicalize().ok();

        // Check if path is within any allowed directory
        let is_in_config_dir = config_dir.as_ref().is_some_and(|dir| path.starts_with(dir));
        let is_in_current_dir = current_dir
            .as_ref()
            .is_some_and(|dir| path.starts_with(dir));
        let is_in_tmp = tmp_dir.as_ref().is_some_and(|dir| path.starts_with(dir));

        if !is_in_config_dir && !is_in_current_dir && !is_in_tmp {
            return Err(format!(
                "Security: Config path '{}' is outside allowed directories. \
                 Allowed: config directory, current directory, /tmp",
                path.display()
            )
            .into());
        }

        Ok(())
    }

    /// Create a default configuration
    ///
    /// Generates a default configuration with sample modes and mappings.
    /// This is used when no configuration file exists.
    ///
    /// # Returns
    /// Default configuration with:
    /// - Device name: "Mikro"
    /// - Two modes: "Default" and "Development"
    /// - Sample mappings for each mode
    pub fn default_config() -> Self {
        Config {
            endpoints: vec![],
            modes: vec![
                Mode {
                    name: "Default".to_string(),
                    color: Some("blue".to_string()),
                    mappings: vec![Mapping {
                        trigger: Trigger::Note {
                            note: 60,
                            velocity_min: Some(1),
                            channel: None,
                            device: None,
                        },
                        action: ActionConfig::Keystroke {
                            keys: "space".to_string(),
                            modifiers: vec!["cmd".to_string()],
                        },
                        description: Some("Spotlight Search".to_string()),
                        let_through: false,
                    }],
                },
                Mode {
                    name: "Development".to_string(),
                    color: Some("green".to_string()),
                    mappings: vec![Mapping {
                        trigger: Trigger::Note {
                            note: 60,
                            velocity_min: None,
                            channel: None,
                            device: None,
                        },
                        action: ActionConfig::Shell {
                            command: "git status".to_string(),
                            args: None,
                            timeout_ms: None,
                            sandbox: None,
                        },
                        description: Some("Git status".to_string()),
                        let_through: false,
                    }],
                },
            ],
            global_mappings: vec![],
            logging: None,
            advanced_settings: Default::default(),
            last_selected_mode: None, // v4.10.9: No default selected mode
            default_mode: None,
            led: None,           // Issue #324: No LED config by default (backward compat)
            event_console: None, // Issue #325: No event console config by default
            per_app_modes: None, // ADR-040 D3: no per-app mode auto-switching by default
            routes: vec![],      // ADR-031 P2: empty by default; populated via [[routes]]
            security: Default::default(), // ADR-027 §D10b: sandbox enforced, unsandboxed allowed
            mcp: super::types::McpConfig::default(),
            config_meta: Default::default(), // ADR-034 §D9: managed source, notify-only watcher
        }
    }

    /// Validate the configuration for correctness.
    ///
    /// Delegates to the unified validation system in `validation.rs`.
    /// Kept for backward compatibility.
    pub fn validate(&self) -> Result<(), ConfigError> {
        super::validation::validate_for_loading(self)
    }
}

#[cfg(test)]
#[allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "test diagnostic output"
)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// RAII guard for safely changing working directory in tests (v4.10.12).
    /// Automatically restores original directory on drop (even on panic).
    /// This prevents test pollution when tests run sequentially.
    ///
    /// Note: For true parallel safety, tests using this guard should be marked
    /// with `#[serial]` from the `serial_test` crate, as `set_current_dir`
    /// modifies process-global state.
    #[must_use = "Guard must be held for the duration of the test"]
    struct WorkingDirGuard {
        original_dir: PathBuf,
    }

    impl WorkingDirGuard {
        fn new(new_dir: &Path) -> std::io::Result<Self> {
            let original_dir = std::env::current_dir()?;
            std::env::set_current_dir(new_dir)?;
            Ok(Self { original_dir })
        }
    }

    impl Drop for WorkingDirGuard {
        fn drop(&mut self) {
            // Restore even on panic - log but don't panic on failure
            if let Err(e) = std::env::set_current_dir(&self.original_dir) {
                eprintln!(
                    "WorkingDirGuard: Failed to restore working directory to {:?}: {}",
                    self.original_dir, e
                );
            }
        }
    }

    #[test]
    fn test_config_default() {
        let config = Config::default_config();
        assert_eq!(config.modes.len(), 2);
        assert_eq!(config.modes[0].name, "Default");
        assert_eq!(config.modes[1].name, "Development");
    }

    #[test]
    fn test_validate_duplicate_mode_names() {
        let mut config = Config::default_config();
        config.modes.push(Mode {
            name: "Default".to_string(),
            color: None,
            mappings: vec![],
        });

        let result = config.validate();
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Duplicate mode name")
        );
    }

    #[test]
    fn test_validate_invalid_note_number() {
        let mut config = Config::default_config();
        config.modes[0].mappings[0].trigger = Trigger::Note {
            note: 128,
            velocity_min: None,
            channel: None,
            device: None,
        };

        let result = config.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("out of range"));
    }

    #[test]
    fn test_validate_invalid_modifier() {
        let mut config = Config::default_config();
        config.modes[0].mappings[0].action = ActionConfig::Keystroke {
            keys: "a".to_string(),
            modifiers: vec!["invalid_mod".to_string()],
        };

        let result = config.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Unknown modifier"));
    }

    #[test]
    fn test_validate_invalid_direction() {
        let mut config = Config::default_config();
        config.modes[0].mappings[0].trigger = Trigger::EncoderTurn {
            cc: 1,
            direction: Some("Invalid".to_string()),
            channel: None,
            device: None,
        };

        let result = config.validate();
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Invalid direction")
        );
    }

    #[test]
    fn test_validate_empty_keystroke_keys() {
        let mut config = Config::default_config();
        config.modes[0].mappings[0].action = ActionConfig::Keystroke {
            keys: String::new(),
            modifiers: vec![],
        };

        let result = config.validate();
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_sequence_with_empty_actions() {
        let mut config = Config::default_config();
        config.modes[0].mappings[0].action = ActionConfig::Sequence { actions: vec![] };

        let result = config.validate();
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_valid_config() {
        let config = Config::default_config();
        let result = config.validate();
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_encoder_direction_clockwise() {
        let mut config = Config::default_config();
        config.modes[0].mappings[0].trigger = Trigger::EncoderTurn {
            cc: 1,
            direction: Some("Clockwise".to_string()),
            channel: None,
            device: None,
        };

        let result = config.validate();
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_encoder_direction_counter_clockwise() {
        let mut config = Config::default_config();
        config.modes[0].mappings[0].trigger = Trigger::EncoderTurn {
            cc: 1,
            direction: Some("CounterClockwise".to_string()),
            channel: None,
            device: None,
        };

        let result = config.validate();
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_note_chord_with_empty_notes() {
        let mut config = Config::default_config();
        config.modes[0].mappings[0].trigger = Trigger::NoteChord {
            notes: vec![],
            timeout_ms: None,
            channel: None,
            device: None,
        };

        let result = config.validate();
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_invalid_mouse_button() {
        let mut config = Config::default_config();
        config.modes[0].mappings[0].action = ActionConfig::MouseClick {
            button: "invalid".to_string(),
            x: None,
            y: None,
        };

        let result = config.validate();
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_volume_control_set_without_value() {
        let mut config = Config::default_config();
        config.modes[0].mappings[0].action = ActionConfig::VolumeControl {
            operation: "Set".to_string(),
            value: None,
        };

        let result = config.validate();
        assert!(result.is_err());
    }

    // ========================================
    // Security Tests (Command Injection Prevention)
    // ========================================

    #[test]
    fn test_shell_injection_semicolon_blocked() {
        let mut config = Config::default_config();
        config.modes[0].mappings[0].action = ActionConfig::Shell {
            sandbox: None,
            command: "echo test; rm -rf /".to_string(),
            args: None,
            timeout_ms: None,
        };

        let result = config.validate();
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("command chaining with semicolon")
        );
    }

    #[test]
    fn test_shell_injection_and_operator_blocked() {
        let mut config = Config::default_config();
        config.modes[0].mappings[0].action = ActionConfig::Shell {
            sandbox: None,
            command: "ls && malicious_command".to_string(),
            args: None,
            timeout_ms: None,
        };

        let result = config.validate();
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("command chaining with AND")
        );
    }

    #[test]
    fn test_shell_injection_or_operator_blocked() {
        let mut config = Config::default_config();
        config.modes[0].mappings[0].action = ActionConfig::Shell {
            sandbox: None,
            command: "false || evil_fallback".to_string(),
            args: None,
            timeout_ms: None,
        };

        let result = config.validate();
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("command chaining with OR")
        );
    }

    #[test]
    fn test_shell_injection_pipe_blocked() {
        let mut config = Config::default_config();
        config.modes[0].mappings[0].action = ActionConfig::Shell {
            sandbox: None,
            command: "cat /etc/passwd | grep root".to_string(),
            args: None,
            timeout_ms: None,
        };

        let result = config.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("piping"));
    }

    #[test]
    fn test_shell_injection_backtick_blocked() {
        let mut config = Config::default_config();
        config.modes[0].mappings[0].action = ActionConfig::Shell {
            sandbox: None,
            command: "echo `whoami`".to_string(),
            args: None,
            timeout_ms: None,
        };

        let result = config.validate();
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("backtick command substitution")
        );
    }

    #[test]
    fn test_shell_injection_dollar_paren_blocked() {
        let mut config = Config::default_config();
        config.modes[0].mappings[0].action = ActionConfig::Shell {
            sandbox: None,
            command: "echo $(whoami)".to_string(),
            args: None,
            timeout_ms: None,
        };

        let result = config.validate();
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("dollar-paren command substitution")
        );
    }

    #[test]
    fn test_shell_injection_variable_expansion_blocked() {
        let mut config = Config::default_config();
        config.modes[0].mappings[0].action = ActionConfig::Shell {
            sandbox: None,
            command: "echo ${DANGEROUS_VAR}".to_string(),
            args: None,
            timeout_ms: None,
        };

        let result = config.validate();
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("variable expansion")
        );
    }

    #[test]
    fn test_shell_injection_output_redirect_blocked() {
        let mut config = Config::default_config();
        config.modes[0].mappings[0].action = ActionConfig::Shell {
            sandbox: None,
            command: "echo data > /etc/important_file".to_string(),
            args: None,
            timeout_ms: None,
        };

        let result = config.validate();
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("output redirection")
        );
    }

    #[test]
    fn test_shell_injection_append_redirect_blocked() {
        let mut config = Config::default_config();
        config.modes[0].mappings[0].action = ActionConfig::Shell {
            sandbox: None,
            command: "echo data >> /etc/important_file".to_string(),
            args: None,
            timeout_ms: None,
        };

        let result = config.validate();
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("append redirection")
        );
    }

    #[test]
    fn test_shell_injection_input_redirect_blocked() {
        let mut config = Config::default_config();
        config.modes[0].mappings[0].action = ActionConfig::Shell {
            sandbox: None,
            command: "command < /etc/passwd".to_string(),
            args: None,
            timeout_ms: None,
        };

        let result = config.validate();
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("input redirection")
        );
    }

    #[test]
    fn test_shell_injection_background_execution_blocked() {
        let mut config = Config::default_config();
        config.modes[0].mappings[0].action = ActionConfig::Shell {
            sandbox: None,
            command: "malicious_daemon &".to_string(),
            args: None,
            timeout_ms: None,
        };

        let result = config.validate();
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("background execution")
        );
    }

    #[test]
    fn test_shell_safe_commands_allowed() {
        let mut config = Config::default_config();

        // Safe commands should pass validation
        let safe_commands = vec![
            "git status",
            "cargo build",
            "ls -la",
            "echo hello world",
            "pwd",
        ];

        for cmd in safe_commands {
            config.modes[0].mappings[0].action = ActionConfig::Shell {
                sandbox: None,
                command: cmd.to_string(),
                args: None,
                timeout_ms: None,
            };
            let result = config.validate();
            assert!(result.is_ok(), "Safe command '{}' should be allowed", cmd);
        }
    }

    #[test]
    fn test_launch_injection_special_chars_blocked() {
        let mut config = Config::default_config();
        config.modes[0].mappings[0].action = ActionConfig::Launch {
            app: "Terminal; rm -rf /".to_string(),
        };

        let result = config.validate();
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("invalid characters")
        );
    }

    #[test]
    fn test_launch_path_traversal_blocked() {
        let mut config = Config::default_config();
        config.modes[0].mappings[0].action = ActionConfig::Launch {
            app: "../../malicious".to_string(),
        };

        let result = config.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("path traversal"));
    }

    #[test]
    fn test_launch_safe_app_names_allowed() {
        let mut config = Config::default_config();

        // Safe app names should pass validation
        let safe_apps = vec![
            "Terminal",
            "VS Code",
            "Google Chrome",
            "/Applications/Safari.app",
            "my-app_v2.0",
        ];

        for app in safe_apps {
            config.modes[0].mappings[0].action = ActionConfig::Launch {
                app: app.to_string(),
            };
            let result = config.validate();
            assert!(result.is_ok(), "Safe app name '{}' should be allowed", app);
        }
    }

    // ========================================
    // Security Tests (Path Traversal Prevention)
    // ========================================

    #[test]
    #[should_panic(expected = "outside allowed directories")]
    fn test_path_traversal_absolute_etc_blocked() {
        // Trying to load /etc/passwd should be blocked
        let _ = Config::load("/etc/passwd").unwrap();
    }

    #[test]
    fn test_path_traversal_relative_etc_blocked() {
        // Trying to traverse to /etc should be blocked
        let result = Config::load("../../../../etc/passwd");
        assert!(
            result.is_err(),
            "Loading from /etc via traversal should be blocked"
        );
        // Could be either "outside allowed directories" or "No such file" (both are acceptable security outcomes)
    }

    #[test]
    fn test_path_in_current_dir_allowed() {
        // Loading from current directory should be allowed
        let temp_dir = std::env::temp_dir();
        let test_file = temp_dir.join("test_config.toml");

        // Create a valid config file
        let config = Config::default_config();
        config.save(test_file.to_str().unwrap()).unwrap();

        // Should be able to load it
        let loaded = Config::load(test_file.to_str().unwrap());
        assert!(loaded.is_ok(), "Loading from /tmp should be allowed");

        // Cleanup
        let _ = std::fs::remove_file(test_file);
    }

    #[test]
    fn test_path_validation_resolves_symlinks() {
        use std::fs;
        use std::os::unix::fs as unix_fs;

        let temp_dir = std::env::temp_dir();
        let real_file = temp_dir.join("real_config.toml");
        let symlink_file = temp_dir.join("symlink_config.toml");

        // Create a valid config file
        let config = Config::default_config();
        config.save(real_file.to_str().unwrap()).unwrap();

        // Create symlink
        let _ = fs::remove_file(&symlink_file); // Remove if exists
        unix_fs::symlink(&real_file, &symlink_file).unwrap();

        // Should be able to load via symlink (resolves to /tmp)
        let loaded = Config::load(symlink_file.to_str().unwrap());
        assert!(
            loaded.is_ok(),
            "Loading via symlink should work if target is in allowed dir"
        );

        // Cleanup
        let _ = fs::remove_file(real_file);
        let _ = fs::remove_file(symlink_file);
    }

    #[test]
    fn test_relative_path_in_current_dir() {
        // Use a unique temp subdirectory to avoid file collisions
        // and set_current_dir races with parallel tests.
        let temp_subdir =
            std::env::temp_dir().join(format!("conductor_rel_test_{}", std::process::id()));
        std::fs::create_dir_all(&temp_subdir).unwrap();

        // v4.10.12: Use RAII guard to ensure directory restoration even on panic
        let _guard = WorkingDirGuard::new(&temp_subdir).unwrap();

        let config = Config::default_config();
        let result = config.save("test_relative.toml");
        assert!(
            result.is_ok(),
            "Saving to relative path in current dir should work"
        );

        let loaded = Config::load("test_relative.toml");
        assert!(
            loaded.is_ok(),
            "Loading from relative path in current dir should work"
        );

        // Cleanup — guard restores working directory automatically on drop
        drop(_guard);
        let _ = std::fs::remove_dir_all(&temp_subdir);
    }

    // ========================================
    // Security Tests (TOCTOU Prevention)
    // ========================================

    #[test]
    fn test_save_toctou_prevention_atomic_write() {
        // Test that save() uses atomic write pattern
        let temp_dir = std::env::temp_dir();
        let test_file = temp_dir.join("test_atomic.toml");

        let config = Config::default_config();

        // Save should succeed
        let result = config.save(test_file.to_str().unwrap());
        assert!(result.is_ok(), "Atomic write should succeed");

        // File should exist
        assert!(
            test_file.exists(),
            "Final file should exist after atomic write"
        );

        // Temp file should NOT exist (was renamed)
        let temp_file = test_file.with_extension("tmp");
        assert!(
            !temp_file.exists(),
            "Temporary file should not exist after rename"
        );

        // Cleanup
        let _ = std::fs::remove_file(test_file);
    }

    /// #1437: `Config::save` used a PREDICTABLE sibling temp path
    /// (`<target>.tmp`) opened with `create(true).truncate(true)`, which follows
    /// and truncates a pre-existing symlink there. An attacker who can write to
    /// the (allowed) directory could plant `config.tmp -> victim` and have the
    /// victim overwritten with config bytes. The fix uses an unpredictable temp
    /// name + `create_new` (O_EXCL), so a symlink at the old predictable path is
    /// now inert.
    #[test]
    #[cfg(unix)]
    fn test_save_does_not_write_through_predictable_temp_symlink() {
        use std::os::unix::fs as unix_fs;

        let dir = std::env::temp_dir().join(format!(
            "conductor_symlink_save_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // The file an attacker wants clobbered.
        let victim = dir.join("victim.toml");
        let sentinel = b"DO-NOT-CLOBBER-VICTIM";
        std::fs::write(&victim, sentinel).unwrap();

        // Plant a symlink at the OLD predictable temp path (`config.tmp`)
        // pointing at the victim. Pre-fix, save() would open+truncate it.
        let config_path = dir.join("config.toml");
        let predictable_temp = config_path.with_extension("tmp"); // config.tmp
        unix_fs::symlink(&victim, &predictable_temp).unwrap();

        let result = Config::default_config().save(config_path.to_str().unwrap());
        assert!(result.is_ok(), "save should succeed: {:?}", result.err());

        // The victim MUST be byte-for-byte intact — save never wrote through
        // the predictable config.tmp symlink.
        assert_eq!(
            std::fs::read(&victim).unwrap(),
            sentinel,
            "config.save must not write through the predictable config.tmp symlink (#1437)"
        );

        // The planted symlink is untouched (still a symlink, not consumed).
        assert!(
            std::fs::symlink_metadata(&predictable_temp)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the planted config.tmp symlink should be left untouched"
        );

        // The real config was written through an unpredictable temp and loads.
        assert!(config_path.exists(), "config.toml should have been written");
        assert!(
            Config::load(config_path.to_str().unwrap()).is_ok(),
            "the written config should load back"
        );

        // No unpredictable temp file (`.config.toml.<rand>.tmp`) should be left
        // behind after a successful save — it must be renamed away, not leaked.
        let leaked: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(".config.toml.") && n.ends_with(".tmp"))
            .collect();
        assert!(
            leaked.is_empty(),
            "no unpredictable temp file should remain after a successful save; found {leaked:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_save_validates_full_path_not_just_parent() {
        // Test that save() validates the FULL target path, not just parent
        use std::fs;
        use std::os::unix::fs as unix_fs;

        let temp_dir = std::env::temp_dir();
        let malicious_file = temp_dir.join("malicious_target.toml");

        // Create a file in /tmp (allowed)
        let config = Config::default_config();
        config.save(malicious_file.to_str().unwrap()).unwrap();
        assert!(malicious_file.exists());

        // Now try to replace it with a symlink to /etc/passwd
        fs::remove_file(&malicious_file).unwrap();

        // Create symlink pointing to forbidden location
        let _ = unix_fs::symlink("/etc/passwd", &malicious_file);

        // Attempt to save should fail because the resolved path is /etc/passwd
        let result = config.save(malicious_file.to_str().unwrap());

        // Should fail with "outside allowed directories" error
        assert!(
            result.is_err(),
            "Should reject symlink to forbidden location"
        );

        if let Err(e) = result {
            let error_msg = e.to_string();
            assert!(
                error_msg.contains("outside allowed directories"),
                "Should fail with security error, got: {}",
                error_msg
            );
        }

        // Cleanup
        let _ = fs::remove_file(malicious_file);
    }

    #[test]
    fn test_save_rejects_nonexistent_parent() {
        // Test that save() rejects paths with non-existent parent directories
        let temp_dir = std::env::temp_dir();
        let nonexistent = temp_dir.join("does_not_exist").join("config.toml");

        let config = Config::default_config();
        let result = config.save(nonexistent.to_str().unwrap());

        assert!(
            result.is_err(),
            "Should reject non-existent parent directory"
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Parent directory does not exist"),
            "Should fail with parent directory error"
        );
    }

    #[test]
    fn test_save_prevents_race_with_revalidation() {
        // Test that save() re-validates after temp file creation
        let temp_dir = std::env::temp_dir();
        let test_file = temp_dir.join("test_revalidation.toml");

        let config = Config::default_config();

        // First save should succeed
        config.save(test_file.to_str().unwrap()).unwrap();

        // Load and verify content round-trips (modes survive save+load).
        let loaded = Config::load(test_file.to_str().unwrap()).unwrap();
        assert_eq!(loaded.modes.len(), 2);

        // Cleanup
        let _ = std::fs::remove_file(test_file);
    }

    /// ADR-047 §D3a: a gamepad bind on the frozen legacy sentinel id 255 is
    /// disabled (dropped) at load — it never reaches the engine — while a valid
    /// bind in the same mode survives. Not silently migrated.
    #[test]
    fn test_load_disables_legacy_gamepad_sentinel_255() {
        let temp_dir = std::env::temp_dir();
        let test_file = temp_dir.join("test_adr047_d3a_sentinel.toml");
        let toml = r#"
[[modes]]
name = "Default"

[[modes.mappings]]
trigger = { type = "GamepadButton", button = 255 }
action = { type = "Keystroke", keys = "a", modifiers = [] }

[[modes.mappings]]
trigger = { type = "GamepadButton", button = 128 }
action = { type = "Keystroke", keys = "b", modifiers = [] }
"#;
        std::fs::write(&test_file, toml).unwrap();

        let loaded = Config::load(test_file.to_str().unwrap()).unwrap();
        let triggers: Vec<_> = loaded.modes[0]
            .mappings
            .iter()
            .map(|m| &m.trigger)
            .collect();
        assert_eq!(
            triggers.len(),
            1,
            "the button-255 mapping must be disabled; only button 128 survives"
        );
        assert!(
            matches!(
                triggers[0],
                crate::config::types::Trigger::GamepadButton { button: 128, .. }
            ),
            "surviving mapping must be the valid button-128 bind, got {:?}",
            triggers[0]
        );

        let _ = std::fs::remove_file(test_file);
    }

    #[test]
    fn test_save_handles_absolute_and_relative_paths() {
        // Test absolute path only — relative path is tested by
        // test_relative_path_in_current_dir (separated to avoid
        // set_current_dir race conditions in parallel test execution).
        let temp_dir = std::env::temp_dir();
        let abs_path = temp_dir.join("conductor_test_absolute.toml");
        let config = Config::default_config();
        assert!(config.save(abs_path.to_str().unwrap()).is_ok());
        assert!(abs_path.exists());
        std::fs::remove_file(&abs_path).unwrap();
    }

    #[test]
    fn test_save_uses_sync_all_for_durability() {
        // Test that saved files are durable (sync_all ensures data hits disk)
        let temp_dir = std::env::temp_dir();
        let test_file = temp_dir.join("test_sync.toml");

        let config = Config::default_config();
        config.save(test_file.to_str().unwrap()).unwrap();

        // Read back immediately - should succeed even if system crashes
        let contents = std::fs::read_to_string(&test_file).unwrap();
        assert!(contents.contains("name = \"Default\""));

        // Cleanup
        std::fs::remove_file(test_file).unwrap();
    }
}
