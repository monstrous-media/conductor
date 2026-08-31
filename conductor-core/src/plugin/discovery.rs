// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Plugin discovery system
//!
//! Scans the plugins directory for plugin manifests and builds a registry
//! of available plugins. Handles duplicate detection and version conflicts.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

use super::{Capability, PluginMetadata, PluginType};

/// Plugin discovery error types
#[derive(Debug, Error)]
pub enum DiscoveryError {
    /// IO error while scanning directory
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    /// Failed to parse manifest
    #[error("Failed to parse manifest at {path}: {reason}")]
    ManifestParseError { path: String, reason: String },

    /// Manifest binary path escapes the plugin directory (#2117)
    #[error("Insecure binary path in manifest at {path}: {reason}")]
    InsecureBinaryPath { path: String, reason: String },

    /// Duplicate plugin found
    #[error("Duplicate plugin '{name}' found at {path1} and {path2}")]
    DuplicatePlugin {
        name: String,
        path1: String,
        path2: String,
    },

    /// Invalid plugin directory
    #[error("Invalid plugins directory: {0}")]
    InvalidDirectory(String),
}

/// Plugin discovery result type
pub type DiscoveryResult<T> = Result<T, DiscoveryError>;

/// True if `binary` is safe to `join` onto a plugin directory (#2117 /
/// clawpatch #2103): a relative path whose components are only
/// `Component::Normal` or `Component::CurDir` (`.`) — never an absolute
/// prefix / root or a `..` parent traversal — and that names at least one real
/// component (a bare `.` or empty string is rejected).
///
/// This is a **lexical** guarantee: it ensures the *textual* result of
/// `plugin_dir.join(binary)` stays under `plugin_dir`, without touching the
/// filesystem. It deliberately does NOT resolve symlinks, so it is not a
/// defense against a symlink *inside* the plugin directory that points
/// elsewhere — but that requires write access to the plugin directory, which
/// the plugin author already has (they could ship the malicious binary
/// directly), so it grants no extra capability. The threat this closes is a
/// manifest pointing the loader *out* of the plugin directory via an absolute
/// path or `..`, with no out-of-tree write access required. Do not rely on this
/// for stronger, resolution-time path containment than that.
fn is_plugin_relative(binary: &str) -> bool {
    use std::path::Component;
    let p = Path::new(binary);
    if p.is_absolute() {
        return false;
    }
    let mut saw_normal = false;
    for component in p.components() {
        match component {
            Component::Normal(_) => saw_normal = true,
            Component::CurDir => {}
            // ParentDir / RootDir / Prefix can all escape the plugin directory.
            _ => return false,
        }
    }
    saw_normal
}

/// Plugin manifest (plugin.toml)
///
/// This struct represents the contents of a plugin manifest file.
/// The manifest provides metadata about the plugin that can be read
/// without loading the binary.
///
/// # Example plugin.toml
///
/// ```toml
/// [plugin]
/// name = "http_request"
/// version = "1.0.0"
/// description = "Make HTTP requests as actions"
/// author = "Conductor Team"
/// homepage = "https://github.com/monstrous-media/conductor-http-plugin"
/// license = "MIT"
/// type = "action"
/// binary = "http_request.so"
///
/// [plugin.capabilities]
/// network = true
/// ```
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct PluginManifest {
    pub plugin: ManifestPlugin,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ManifestPlugin {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    pub license: String,
    #[serde(rename = "type")]
    pub plugin_type: PluginType,
    pub binary: String,
    #[serde(default)]
    pub capabilities: ManifestCapabilities,
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct ManifestCapabilities {
    #[serde(default)]
    pub network: bool,
    #[serde(default)]
    pub filesystem: bool,
    #[serde(default)]
    pub audio: bool,
    #[serde(default)]
    pub midi: bool,
    #[serde(default)]
    pub subprocess: bool,
    #[serde(default)]
    pub system_control: bool,
}

impl ManifestCapabilities {
    /// Convert to Vec<Capability>
    pub fn to_capabilities(&self) -> Vec<Capability> {
        let mut caps = Vec::new();
        if self.network {
            caps.push(Capability::Network);
        }
        if self.filesystem {
            caps.push(Capability::Filesystem);
        }
        if self.audio {
            caps.push(Capability::Audio);
        }
        if self.midi {
            caps.push(Capability::Midi);
        }
        if self.subprocess {
            caps.push(Capability::Subprocess);
        }
        if self.system_control {
            caps.push(Capability::SystemControl);
        }
        caps
    }
}

/// Plugin registry
///
/// Contains discovered plugins indexed by name.
/// Used by the plugin manager to enumerate available plugins.
pub struct PluginRegistry {
    /// Plugins indexed by name
    plugins: HashMap<String, PluginMetadata>,

    /// Path to plugins directory
    plugins_dir: PathBuf,
}

impl PluginRegistry {
    /// Create a new empty registry
    pub fn new(plugins_dir: PathBuf) -> Self {
        Self {
            plugins: HashMap::new(),
            plugins_dir,
        }
    }

    /// Get plugin metadata by name
    pub fn get(&self, name: &str) -> Option<&PluginMetadata> {
        self.plugins.get(name)
    }

    /// List all plugin names
    pub fn list_names(&self) -> Vec<String> {
        self.plugins.keys().cloned().collect()
    }

    /// List all plugins
    pub fn list_plugins(&self) -> Vec<&PluginMetadata> {
        self.plugins.values().collect()
    }

    /// Get number of registered plugins
    pub fn count(&self) -> usize {
        self.plugins.len()
    }

    /// Check if plugin exists
    pub fn contains(&self, name: &str) -> bool {
        self.plugins.contains_key(name)
    }

    /// Add plugin to registry
    pub fn add(&mut self, metadata: PluginMetadata) -> DiscoveryResult<()> {
        let name = metadata.name.clone();
        if let Some(existing) = self.plugins.get(&name) {
            return Err(DiscoveryError::DuplicatePlugin {
                name,
                path1: existing.binary_path.display().to_string(),
                path2: metadata.binary_path.display().to_string(),
            });
        }
        self.plugins.insert(name, metadata);
        Ok(())
    }

    /// Get plugins directory
    pub fn plugins_dir(&self) -> &Path {
        &self.plugins_dir
    }
}

/// Plugin discovery scanner
///
/// Scans the plugins directory for plugin manifests and builds a registry.
pub struct PluginDiscovery {
    /// Path to plugins directory (~/.conductor/plugins/)
    plugins_dir: PathBuf,
}

impl PluginDiscovery {
    /// Create a new plugin discovery scanner
    ///
    /// # Arguments
    ///
    /// * `plugins_dir` - Path to plugins directory (e.g., ~/.conductor/plugins/)
    pub fn new(plugins_dir: PathBuf) -> Self {
        Self { plugins_dir }
    }

    /// Scan for plugins and build registry
    ///
    /// Scans the plugins directory for subdirectories containing plugin.toml
    /// manifests. Each subdirectory is expected to contain:
    /// - plugin.toml (manifest)
    /// - plugin binary (.so/.dylib/.dll)
    ///
    /// # Errors
    ///
    /// Returns error if:
    /// - Plugins directory doesn't exist or is not readable
    /// - Manifest parsing fails
    /// - Duplicate plugins found
    pub fn scan(&self) -> DiscoveryResult<PluginRegistry> {
        let mut registry = PluginRegistry::new(self.plugins_dir.clone());

        // Check if plugins directory exists
        if !self.plugins_dir.exists() {
            // Create it if it doesn't exist
            fs::create_dir_all(&self.plugins_dir)?;
            return Ok(registry); // Empty registry
        }

        if !self.plugins_dir.is_dir() {
            return Err(DiscoveryError::InvalidDirectory(format!(
                "{} is not a directory",
                self.plugins_dir.display()
            )));
        }

        // Scan subdirectories
        for entry in fs::read_dir(&self.plugins_dir)? {
            let entry = entry?;
            let path = entry.path();

            // Skip files, only process directories
            if !path.is_dir() {
                continue;
            }

            // Look for plugin.toml in this directory
            let manifest_path = path.join("plugin.toml");
            if !manifest_path.exists() {
                continue;
            }

            // Parse manifest
            match self.parse_manifest(&manifest_path, &path) {
                Ok(metadata) => {
                    registry.add(metadata)?;
                }
                Err(e) => {
                    // Log error but continue scanning
                    tracing::warn!("Failed to parse plugin manifest: {}", e);
                }
            }
        }

        Ok(registry)
    }

    /// Parse a plugin manifest file
    fn parse_manifest(
        &self,
        manifest_path: &Path,
        plugin_dir: &Path,
    ) -> DiscoveryResult<PluginMetadata> {
        // Read manifest file
        let content = fs::read_to_string(manifest_path)?;

        // Parse TOML
        let manifest: PluginManifest =
            toml::from_str(&content).map_err(|e| DiscoveryError::ManifestParseError {
                path: manifest_path.display().to_string(),
                reason: e.to_string(),
            })?;

        // Build binary path. SECURITY (#2117 / clawpatch #2103): the manifest's
        // `binary` field is attacker-controlled (a third-party / downloaded
        // plugin). `plugin_dir.join(binary)` would resolve a binary OUTSIDE the
        // plugin's own directory if `binary` is an absolute path (`join`
        // discards the base and takes the absolute path verbatim) or contains a
        // `..` traversal. Require a relative path using only normal components
        // (optionally separated by `.` components) so the result is guaranteed
        // to stay inside the plugin directory — validated lexically, no
        // filesystem access or canonicalization needed.
        if !is_plugin_relative(&manifest.plugin.binary) {
            return Err(DiscoveryError::InsecureBinaryPath {
                path: manifest_path.display().to_string(),
                reason: format!(
                    "binary path '{}' must be a relative path inside the plugin \
                     directory (no absolute path, no `..` traversal)",
                    manifest.plugin.binary
                ),
            });
        }
        let binary_path = plugin_dir.join(&manifest.plugin.binary);

        // Create metadata
        let metadata = PluginMetadata::new(
            manifest.plugin.name,
            manifest.plugin.version,
            manifest.plugin.description,
            manifest.plugin.author,
            manifest.plugin.license,
            manifest.plugin.plugin_type,
        )
        .with_binary_path(binary_path)
        .with_capabilities(manifest.plugin.capabilities.to_capabilities());

        // TODO: Calculate checksum of binary

        Ok(metadata)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_registry_creation() {
        let temp_dir = TempDir::new().unwrap();
        let registry = PluginRegistry::new(temp_dir.path().to_path_buf());

        assert_eq!(registry.count(), 0);
        assert_eq!(registry.list_names().len(), 0);
    }

    #[test]
    fn test_registry_add_plugin() {
        let temp_dir = TempDir::new().unwrap();
        let mut registry = PluginRegistry::new(temp_dir.path().to_path_buf());

        let metadata = PluginMetadata::new(
            "test_plugin".to_string(),
            "1.0.0".to_string(),
            "Test".to_string(),
            "Author".to_string(),
            "MIT".to_string(),
            PluginType::Action,
        );

        registry.add(metadata).unwrap();
        assert_eq!(registry.count(), 1);
        assert!(registry.contains("test_plugin"));
    }

    #[test]
    fn test_registry_duplicate_detection() {
        let temp_dir = TempDir::new().unwrap();
        let mut registry = PluginRegistry::new(temp_dir.path().to_path_buf());

        let metadata1 = PluginMetadata::new(
            "test_plugin".to_string(),
            "1.0.0".to_string(),
            "Test".to_string(),
            "Author".to_string(),
            "MIT".to_string(),
            PluginType::Action,
        );

        let metadata2 = metadata1.clone();

        registry.add(metadata1).unwrap();
        let result = registry.add(metadata2);
        assert!(result.is_err());

        if let Err(DiscoveryError::DuplicatePlugin { name, .. }) = result {
            assert_eq!(name, "test_plugin");
        } else {
            panic!("Expected DuplicatePlugin error");
        }
    }

    #[test]
    fn test_discovery_nonexistent_dir() {
        let temp_dir = TempDir::new().unwrap();
        let plugins_dir = temp_dir.path().join("plugins");

        let discovery = PluginDiscovery::new(plugins_dir.clone());
        let registry = discovery.scan().unwrap();

        // Should create directory and return empty registry
        assert!(plugins_dir.exists());
        assert_eq!(registry.count(), 0);
    }

    #[test]
    fn test_manifest_capabilities_conversion() {
        let caps = ManifestCapabilities {
            network: true,
            filesystem: false,
            audio: true,
            midi: false,
            subprocess: false,
            system_control: true,
        };

        let vec = caps.to_capabilities();
        assert_eq!(vec.len(), 3);
        assert!(vec.contains(&Capability::Network));
        assert!(vec.contains(&Capability::Audio));
        assert!(vec.contains(&Capability::SystemControl));
    }

    #[test]
    fn test_discovery_with_valid_manifest() {
        let temp_dir = TempDir::new().unwrap();
        let plugins_dir = temp_dir.path().join("plugins");
        fs::create_dir(&plugins_dir).unwrap();

        // Create a test plugin directory
        let plugin_dir = plugins_dir.join("test_plugin");
        fs::create_dir(&plugin_dir).unwrap();

        // Create manifest
        let manifest = r#"
[plugin]
name = "test_plugin"
version = "1.0.0"
description = "A test plugin"
author = "Test Author"
license = "MIT"
type = "action"
binary = "test_plugin.so"

[plugin.capabilities]
network = true
"#;
        fs::write(plugin_dir.join("plugin.toml"), manifest).unwrap();

        // Scan
        let discovery = PluginDiscovery::new(plugins_dir);
        let registry = discovery.scan().unwrap();

        assert_eq!(registry.count(), 1);
        let plugin = registry.get("test_plugin").unwrap();
        assert_eq!(plugin.name, "test_plugin");
        assert_eq!(plugin.version, "1.0.0");
        assert_eq!(plugin.capabilities, vec![Capability::Network]);
    }

    /// Regression test for #2117 (clawpatch #2103): a plugin manifest's
    /// `binary` field is attacker-controlled. The loader builds the binary
    /// path as `plugin_dir.join(binary)` — so an ABSOLUTE path
    /// (`Path::join` discards the base and uses the absolute path verbatim) or
    /// a `..` traversal would resolve a binary OUTSIDE the plugin's own
    /// directory. Such manifests must be rejected; only a relative path that
    /// stays inside the plugin directory is allowed.
    #[test]
    fn test_manifest_binary_path_cannot_escape_plugin_dir() {
        let temp_dir = TempDir::new().unwrap();
        let plugins_dir = temp_dir.path().join("plugins");
        let plugin_dir = plugins_dir.join("evil");
        fs::create_dir_all(&plugin_dir).unwrap();
        let manifest_path = plugin_dir.join("plugin.toml");
        let discovery = PluginDiscovery::new(plugins_dir);

        let manifest_with = |binary: &str| {
            format!(
                "[plugin]\n\
                 name = \"evil\"\n\
                 version = \"1.0.0\"\n\
                 description = \"x\"\n\
                 author = \"x\"\n\
                 license = \"MIT\"\n\
                 type = \"action\"\n\
                 binary = \"{binary}\"\n"
            )
        };

        // Absolute path: `plugin_dir.join("/etc/passwd")` == "/etc/passwd".
        fs::write(&manifest_path, manifest_with("/etc/passwd")).unwrap();
        assert!(
            matches!(
                discovery.parse_manifest(&manifest_path, &plugin_dir),
                Err(DiscoveryError::InsecureBinaryPath { .. })
            ),
            "an absolute binary path must be rejected (it escapes the plugin dir)"
        );

        // Parent-dir traversal escapes the plugin directory.
        fs::write(&manifest_path, manifest_with("../../../../tmp/evil.so")).unwrap();
        assert!(
            matches!(
                discovery.parse_manifest(&manifest_path, &plugin_dir),
                Err(DiscoveryError::InsecureBinaryPath { .. })
            ),
            "a `..` traversal binary path must be rejected"
        );

        // A plain relative filename stays inside the plugin directory → allowed,
        // and resolves under it.
        fs::write(&manifest_path, manifest_with("plugin.so")).unwrap();
        let metadata = discovery
            .parse_manifest(&manifest_path, &plugin_dir)
            .expect("a relative binary inside the plugin dir is allowed");
        assert_eq!(metadata.binary_path, plugin_dir.join("plugin.so"));
        assert!(
            metadata.binary_path.starts_with(&plugin_dir),
            "the resolved binary must stay within the plugin directory"
        );
    }
}
