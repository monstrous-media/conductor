// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Plugin registry client for discovering and installing plugins
//!
//! This module is only available when the `plugin-registry` feature is enabled.

#[cfg(feature = "plugin-registry")]
use serde::{Deserialize, Serialize};
#[cfg(feature = "plugin-registry")]
use std::collections::HashMap;
#[cfg(feature = "plugin-registry")]
use std::path::{Path, PathBuf};

#[cfg(feature = "plugin-registry")]
use crate::security::egress::{EgressConfig, EgressDecision, EgressMode, EgressPolicy};
#[cfg(feature = "plugin-registry")]
use sha2::Digest;
#[cfg(feature = "plugin-registry")]
use std::net::IpAddr;

/// Error returned when ADR-027 D17 egress enforcement blocks an outbound
/// request. Carries enough context for the caller (daemon) to emit a
/// structured `EgressBlocked` audit event.
#[cfg(feature = "plugin-registry")]
#[derive(Debug, Clone, thiserror::Error)]
#[error("egress blocked for tool '{tool}' to host '{host}': {reason}")]
pub struct EgressError {
    /// The tool / call-site whose request was blocked.
    pub tool: String,
    /// The target host.
    pub host: String,
    /// Human-readable reason (allowlist miss, or internal-IP / DNS-rebinding).
    pub reason: String,
}

/// Plugin registry metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginRegistry {
    pub version: String,
    pub last_updated: String,
    pub plugins: Vec<PluginRegistryEntry>,
    #[serde(default)]
    pub featured_plugins: Vec<String>,
    pub categories: Vec<PluginCategory>,
}

/// Individual plugin entry in the registry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginRegistryEntry {
    pub id: String,
    pub name: String,
    pub description: String,
    pub author: String,
    pub version: String,
    pub category: String,
    pub tags: Vec<String>,
    pub capabilities: Vec<String>,
    pub download_url: String,
    pub signature_url: String,
    pub checksum: String,
    pub size_bytes: u64,
    pub license: String,
    pub repository: String,
    pub documentation: String,
    /// Minimum required conductor version (semver string).
    ///
    /// Accepts the legacy field name `min_midimon_version` via `serde(alias)`
    /// so unmigrated registry data (pre-rename in PR #946) still
    /// deserializes. Once the live registry at `conductor-plugin-registry`
    /// has been migrated to emit `min_conductor_version`, the alias can be
    /// removed — but until then it's load-bearing for the GUI plugins view.
    #[serde(alias = "min_midimon_version")]
    pub min_conductor_version: String,
    pub signed: bool,
    pub verified: bool,

    /// Enriched capability metadata for GUI display (#1074).
    ///
    /// Populated server-side after deserialise (see
    /// `enrich_registry_capabilities()`). Not part of the registry.json wire
    /// format — `skip_deserializing` keeps it out of the inbound parse,
    /// `default` makes it an empty Vec until the enrichment pass runs.
    /// `skip_serializing_if = Vec::is_empty` keeps the cache file lean
    /// when the registry hasn't been enriched yet (e.g. CLI install
    /// path that doesn't need the GUI metadata).
    #[serde(default, skip_deserializing, skip_serializing_if = "Vec::is_empty")]
    pub capabilities_enriched: Vec<crate::plugin::EnrichedCapability>,
}

/// Plugin category metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginCategory {
    pub id: String,
    pub name: String,
    pub description: String,
}

/// Plugin registry client
pub struct PluginRegistryClient {
    registry_url: String,
    cache_dir: PathBuf,
    /// ADR-027 D17 egress policy. **Defaults to `Warn`** so the client does not
    /// break plugin downloads (which target arbitrary release hosts and are
    /// gated by signature verification, not the egress allowlist) when no
    /// `[security.egress]` is configured — while still enforcing the
    /// DNS-rebinding / internal-IP defense (which applies in every mode except
    /// `Off`). The daemon injects the operator-configured (typically `Strict`)
    /// policy via [`PluginRegistryClient::with_egress_policy`]. The client's own
    /// `registry_url` host is always exempt from the *allowlist* (operator-
    /// configured, code-defined trusted endpoint) but is still subject to the
    /// rebinding check.
    egress: EgressPolicy,
}

impl PluginRegistryClient {
    /// Create a new plugin registry client
    pub fn new(registry_url: impl Into<String>, cache_dir: impl Into<PathBuf>) -> Self {
        Self {
            registry_url: registry_url.into(),
            cache_dir: cache_dir.into(),
            egress: EgressPolicy::from_config(&EgressConfig {
                mode: EgressMode::Warn,
                ..Default::default()
            }),
        }
    }

    /// Inject the file-only `[security.egress]` policy (built by the daemon
    /// from [`crate::security::egress::SecurityConfig`]). Builder-style so the
    /// existing constructors stay source-compatible.
    pub fn with_egress_policy(mut self, egress: EgressPolicy) -> Self {
        self.egress = egress;
        self
    }

    /// Build an HTTP client with automatic redirects DISABLED. Council D17 P0:
    /// a 301/302 to an off-allowlist or internal host would bypass the egress
    /// check that ran against the original URL, so redirects must not be
    /// followed transparently.
    fn http_client() -> Result<reqwest::Client, reqwest::Error> {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
    }

    /// Reject any non-success response. With redirects disabled, reqwest returns
    /// 3xx responses verbatim (it does NOT raise an error), and
    /// `error_for_status` only covers 4xx/5xx — so an unhandled redirect would
    /// be fed to `.json()` as an empty/short body. We treat any 3xx as an error
    /// (its target was never egress-checked) and surface 4xx/5xx via
    /// `error_for_status`.
    fn check_response(
        response: reqwest::Response,
        url: &str,
    ) -> Result<reqwest::Response, Box<dyn std::error::Error>> {
        if response.status().is_redirection() {
            return Err(format!(
                "egress: refusing to follow redirect ({}) from {} — target not allowlist-checked",
                response.status(),
                url
            )
            .into());
        }
        Ok(response.error_for_status()?)
    }

    /// Returns the host of the configured registry endpoint, lowercased.
    fn registry_host(&self) -> Option<String> {
        reqwest::Url::parse(&self.registry_url)
            .ok()
            .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))
    }

    /// ADR-027 D17 egress gate. Runs BEFORE any request leaves the host:
    /// 1. Allowlist check via [`EgressPolicy::check_host`] (skipped for the
    ///    trusted registry endpoint host).
    /// 2. DNS-rebinding / internal-IP defense via
    ///    [`EgressPolicy::check_resolved_ips`] — applied to ALL hosts, since a
    ///    trusted public domain can still be rebound to an internal address.
    ///
    /// Known residual (documented, accepted for v1): there is a TOCTOU window
    /// between this resolution and reqwest's own resolution at connect time. A
    /// fast-flux rebind between the two could still connect to a different IP.
    /// Pinning reqwest to the resolved address (`Client::resolve`) closes this
    /// and is tracked as a follow-up.
    async fn enforce_egress(&self, url: &str, tool: &str) -> Result<(), EgressError> {
        let err = |reason: String, host: String| EgressError {
            tool: tool.to_string(),
            host,
            reason,
        };
        let parsed = reqwest::Url::parse(url)
            .map_err(|e| err(format!("invalid URL: {e}"), url.to_string()))?;
        let host = parsed
            .host_str()
            .ok_or_else(|| err("request has no host component".to_string(), url.to_string()))?
            .to_ascii_lowercase();

        // The configured registry endpoint is trusted operator config, exempt
        // from the allowlist (but NOT from the rebinding check below).
        let is_registry_host = self.registry_host().as_deref() == Some(host.as_str());
        if !is_registry_host {
            match self.egress.check_host(tool, &host) {
                EgressDecision::Allow => {}
                EgressDecision::AllowWithWarning { reason } => {
                    tracing::warn!(target: "egress", tool, host = %host, %reason, "egress allowed under warn mode (allowlist)");
                }
                EgressDecision::Block { reason } => {
                    tracing::warn!(target: "egress", tool, host = %host, %reason, "egress blocked (allowlist)");
                    return Err(err(reason, host));
                }
            }
        }

        // DNS-rebinding / internal-IP defense for every host. Resolution failure
        // yields an empty set, which `check_resolved_ips` handles per mode
        // (Strict fails CLOSED — we cannot verify the host is not internal, and
        // reqwest may resolve it when our lookup didn't; Council #1912).
        let port = parsed.port_or_known_default().unwrap_or(443);
        let ips: Vec<IpAddr> = match tokio::net::lookup_host((host.as_str(), port)).await {
            Ok(addrs) => addrs.map(|a| a.ip()).collect(),
            Err(e) => {
                tracing::debug!(target: "egress", host = %host, error = %e, "egress: DNS resolution failed");
                Vec::new()
            }
        };
        match self.egress.check_resolved_ips(tool, &host, &ips) {
            EgressDecision::Allow => {}
            EgressDecision::AllowWithWarning { reason } => {
                tracing::warn!(target: "egress", tool, host = %host, %reason, "egress allowed under warn mode (rebinding)");
            }
            EgressDecision::Block { reason } => {
                tracing::warn!(target: "egress", tool, host = %host, %reason, "egress blocked (DNS-rebinding)");
                return Err(err(reason, host));
            }
        }
        Ok(())
    }

    /// Create default registry client
    pub fn default_registry() -> Self {
        let cache_dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("conductor")
            .join("plugin_cache");

        Self::new(
            "https://raw.githubusercontent.com/monstrous-media/conductor-plugin-registry/main/registry.json",
            cache_dir,
        )
    }

    /// Fetch the latest plugin registry
    pub async fn fetch_registry(&self) -> Result<PluginRegistry, Box<dyn std::error::Error>> {
        self.enforce_egress(&self.registry_url, "plugin_registry")
            .await?;
        let response = Self::http_client()?.get(&self.registry_url).send().await?;
        let response = Self::check_response(response, &self.registry_url)?;
        // ADR-027 D10d-source: read the RAW bytes and verify the document
        // signature (when signed + a key is pinned) BEFORE trusting the parsed
        // content. The raw text is cached verbatim so the signature survives
        // for later cache-integrity checks.
        let text = response.text().await?;
        let registry = self.parse_fetched_registry(&text)?;

        if let Err(e) = self.cache_registry_raw(&text).await {
            tracing::warn!("Failed to cache registry: {}", e);
        }

        Ok(registry)
    }

    /// Load registry from cache
    pub async fn load_cached_registry(&self) -> Result<PluginRegistry, Box<dyn std::error::Error>> {
        let cache_file = self.cache_dir.join("registry.json");
        let contents = tokio::fs::read_to_string(cache_file).await?;
        // ADR-027 D10d-source: integrity-check the cached document (signature
        // only — no rollback advance; the cache holds the last-fetched doc).
        let registry = self.parse_cached_registry(&contents)?;
        Ok(registry)
    }

    /// Cache the raw (possibly signed) registry document verbatim.
    async fn cache_registry_raw(&self, raw: &str) -> Result<(), Box<dyn std::error::Error>> {
        tokio::fs::create_dir_all(&self.cache_dir).await?;
        let cache_file = self.cache_dir.join("registry.json");
        tokio::fs::write(cache_file, raw).await?;
        Ok(())
    }

    /// Parse a freshly-fetched registry document (ADR-027 D10d-source). The
    /// trust decision is gated on whether a registry key is **pinned**, NOT on
    /// the document's shape — otherwise a network attacker could strip the
    /// signature envelope and force the unsigned path (signature-stripping
    /// downgrade; Council R1). With a key pinned, the signature + monotonic
    /// rollback guards are enforced and a stripped/bare document is rejected.
    /// While the Phase-1 pinned key is empty there is no trust anchor, so the
    /// document is accepted unverified with a migration warning.
    #[cfg(feature = "plugin-signing")]
    fn parse_fetched_registry(
        &self,
        text: &str,
    ) -> Result<PluginRegistry, Box<dyn std::error::Error>> {
        use crate::plugin::registry_trust as rt;
        let state = self.load_registry_trust_state();
        match rt::decide_fetch(
            text,
            rt::pinned_registry_key().as_ref(),
            rt::REGISTRY_PINNED_KEY_ID,
            &state,
        )? {
            rt::FetchDecision::Verified(verified) => {
                self.save_registry_trust_state(&verified.new_state);
                Ok(serde_json::from_str(&verified.payload)?)
            }
            rt::FetchDecision::UnverifiedNoPin { payload } => {
                tracing::warn!(
                    "registry document is unverifiable — no registry key pinned \
                     (ADR-027 D10d-source migration)"
                );
                Ok(serde_json::from_str(&payload)?)
            }
        }
    }

    #[cfg(not(feature = "plugin-signing"))]
    fn parse_fetched_registry(
        &self,
        text: &str,
    ) -> Result<PluginRegistry, Box<dyn std::error::Error>> {
        Ok(serde_json::from_str(text)?)
    }

    /// Parse a cached registry document. Same pinned-key gating as the fetch
    /// path; integrity-only (no rollback advance) since the cache holds the
    /// last-fetched document.
    #[cfg(feature = "plugin-signing")]
    fn parse_cached_registry(
        &self,
        text: &str,
    ) -> Result<PluginRegistry, Box<dyn std::error::Error>> {
        use crate::plugin::registry_trust as rt;
        let state = self.load_registry_trust_state();
        let outcome = rt::decide_cache(
            text,
            rt::pinned_registry_key().as_ref(),
            rt::REGISTRY_PINNED_KEY_ID,
            &state,
        )?;
        if !outcome.verified {
            tracing::warn!(
                "cached registry document is unverifiable — no registry key pinned (migration)"
            );
        }
        Ok(serde_json::from_str(&outcome.payload)?)
    }

    #[cfg(not(feature = "plugin-signing"))]
    fn parse_cached_registry(
        &self,
        text: &str,
    ) -> Result<PluginRegistry, Box<dyn std::error::Error>> {
        Ok(serde_json::from_str(text)?)
    }

    /// Path of the persisted registry trust state (rollback high-water mark).
    /// Kept SEPARATE from the plugin trust store (`trusted_keys.json`) so
    /// registry trust evolves independently (ADR-027 D10d-source / Council R1).
    #[cfg(feature = "plugin-signing")]
    fn registry_state_path(&self) -> PathBuf {
        self.cache_dir.join("registry_state.json")
    }

    #[cfg(feature = "plugin-signing")]
    fn load_registry_trust_state(&self) -> crate::plugin::registry_trust::RegistryTrustState {
        std::fs::read_to_string(self.registry_state_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    #[cfg(feature = "plugin-signing")]
    fn save_registry_trust_state(&self, state: &crate::plugin::registry_trust::RegistryTrustState) {
        match serde_json::to_string_pretty(state) {
            Ok(json) => {
                let _ = std::fs::create_dir_all(&self.cache_dir);
                if let Err(e) = std::fs::write(self.registry_state_path(), json) {
                    tracing::warn!("failed to persist registry trust state: {e}");
                }
            }
            Err(e) => tracing::warn!("failed to serialise registry trust state: {e}"),
        }
    }

    /// Get plugin by ID
    pub fn find_plugin<'a>(
        &self,
        registry: &'a PluginRegistry,
        plugin_id: &str,
    ) -> Option<&'a PluginRegistryEntry> {
        registry.plugins.iter().find(|p| p.id == plugin_id)
    }

    /// Search plugins by query
    pub fn search_plugins<'a>(
        &self,
        registry: &'a PluginRegistry,
        query: &str,
    ) -> Vec<&'a PluginRegistryEntry> {
        let query_lower = query.to_lowercase();
        registry
            .plugins
            .iter()
            .filter(|p| {
                p.name.to_lowercase().contains(&query_lower)
                    || p.description.to_lowercase().contains(&query_lower)
                    || p.tags
                        .iter()
                        .any(|tag| tag.to_lowercase().contains(&query_lower))
            })
            .collect()
    }

    /// Filter plugins by category
    pub fn filter_by_category<'a>(
        &self,
        registry: &'a PluginRegistry,
        category: &str,
    ) -> Vec<&'a PluginRegistryEntry> {
        registry
            .plugins
            .iter()
            .filter(|p| p.category == category)
            .collect()
    }

    /// Get download URL for plugin
    pub fn get_download_url<'a>(&self, plugin: &'a PluginRegistryEntry) -> &'a String {
        &plugin.download_url
    }

    /// Get checksum for plugin
    pub fn get_checksum<'a>(&self, plugin: &'a PluginRegistryEntry) -> &'a String {
        &plugin.checksum
    }

    /// Detect current platform
    fn current_platform(&self) -> String {
        #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
        return "macos-x86_64".to_string();

        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        return "macos-aarch64".to_string();

        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        return "linux-x86_64".to_string();

        #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
        return "windows-x86_64".to_string();

        #[cfg(not(any(
            all(target_os = "macos", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "windows", target_arch = "x86_64")
        )))]
        return "unknown".to_string();
    }

    /// Download plugin binary
    pub async fn download_plugin(
        &self,
        plugin: &PluginRegistryEntry,
        destination: impl AsRef<Path>,
    ) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let url = self.get_download_url(plugin).to_string();

        self.enforce_egress(&url, "plugin_download").await?;
        let response = Self::http_client()?.get(&url).send().await?;
        let response = Self::check_response(response, &url)?;
        let bytes = response.bytes().await?;

        // Verify checksum
        let expected_checksum = self.get_checksum(plugin);
        let digest = sha2::Sha256::digest(&bytes);
        let actual_checksum = format!("sha256:{:x}", digest);
        if actual_checksum != *expected_checksum {
            return Err(format!(
                "Checksum mismatch: expected {}, got {}",
                expected_checksum, actual_checksum
            )
            .into());
        }

        // Write to destination
        let dest_path = destination.as_ref().to_path_buf();
        tokio::fs::create_dir_all(dest_path.parent().unwrap()).await?;
        tokio::fs::write(&dest_path, bytes).await?;

        Ok(dest_path)
    }

    /// Install plugin from registry
    ///
    /// Layout matches what `PluginDiscovery::scan()` expects (#948):
    /// `<plugins_dir>/<plugin_id>/` containing both the binary
    /// (`libconductor_<id>_plugin.<ext>`) and a generated `plugin.toml`
    /// manifest. Pre-#948 this wrote a flat binary directly into
    /// `plugins_dir`, which the discovery scan silently ignored.
    pub async fn install_plugin(
        &self,
        plugin_id: &str,
        plugins_dir: impl AsRef<Path>,
    ) -> Result<PathBuf, Box<dyn std::error::Error>> {
        // Fetch latest registry
        let registry = self.fetch_registry().await?;

        // Find plugin
        let plugin = self
            .find_plugin(&registry, plugin_id)
            .ok_or(format!("Plugin '{}' not found in registry", plugin_id))?;

        // Determine file extension for platform
        let ext = if cfg!(target_os = "macos") {
            "dylib"
        } else if cfg!(target_os = "linux") {
            "so"
        } else if cfg!(target_os = "windows") {
            "dll"
        } else {
            return Err("Unsupported platform".into());
        };

        // Subdirectory layout: <plugins_dir>/<plugin_id>/
        // (`plugin_install_dir` validates the id against path traversal —
        // a malicious registry entry can't escape `plugins_dir`)
        let install_dir = plugin_install_dir(plugins_dir.as_ref(), &plugin.id)?;

        // Symmetric to the uninstall flow: refuse to install into a
        // pre-existing symlink at `<plugins_dir>/<id>/`. `create_dir_all`
        // and the subsequent writes would otherwise follow the symlink
        // and could overwrite files outside `plugins_dir`.
        refuse_symlink_or_non_dir_at(&install_dir).await?;

        tokio::fs::create_dir_all(&install_dir).await?;

        // Download binary into the subdirectory
        let binary_filename = plugin_binary_filename(&plugin.id, ext)?;
        let binary_path = install_dir.join(&binary_filename);

        // Even with `install_dir` validated as a real directory above, an
        // attacker could pre-create a symlink at the binary path itself —
        // `download_plugin` writes via `tokio::fs::write` which follows
        // symlinks. Refuse symlinks here too.
        refuse_symlink_at(&binary_path).await?;

        tracing::info!("Downloading {} v{}...", plugin.name, plugin.version);
        let installed_path = self.download_plugin(plugin, &binary_path).await?;
        tracing::info!("Installed {} to {:?}", plugin.name, installed_path);

        // Same protection at the manifest path before writing. With the
        // atomic write below this is defense-in-depth (rename(2) replaces
        // the symlink itself rather than following it), but a friendly
        // fail-closed error here beats a confusing rename failure later.
        let manifest_path = install_dir.join("plugin.toml");
        refuse_symlink_at(&manifest_path).await?;

        // Write the plugin.toml manifest atomically via the canonical
        // helper from `config::preferences` (temp file + fsync + rename
        // + parent dir fsync). Atomicity matters: a partial write would
        // leave a corrupt manifest that `PluginDiscovery::scan()` would
        // skip, hiding the install from the UI. The helper is sync (uses
        // `std::fs` so it can be called from `spawn_blocking`), so we
        // move the I/O off the executor.
        let manifest = manifest_from_registry_entry(plugin, binary_filename);
        let mp = manifest_path.clone();
        match tokio::task::spawn_blocking(move || {
            crate::config::preferences::atomic_write_toml(&mp, &manifest)
        })
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(e.into()),
            Err(je) if je.is_panic() => {
                return Err(format!("Manifest write task panicked: {}", je).into());
            }
            Err(je) => {
                return Err(format!("Manifest write task was cancelled: {}", je).into());
            }
        }

        Ok(installed_path)
    }
}

/// Refuse if `path` exists and is a symlink, or exists but is not a
/// directory. Used at install-dir creation time to prevent
/// `create_dir_all` from following a symlink and writing outside
/// `plugins_dir`.
///
/// `NotFound` is OK (caller will create the directory). Other I/O
/// errors propagate.
#[cfg(feature = "plugin-registry")]
async fn refuse_symlink_or_non_dir_at(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(format!(
            "Refusing to operate on {:?}: is a symlink (potential path-traversal attack)",
            path
        )
        .into()),
        Ok(metadata) if !metadata.is_dir() => Err(format!(
            "Refusing to operate on {:?}: exists but is not a directory",
            path
        )
        .into()),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("Failed to stat {:?}: {}", path, e).into()),
    }
}

/// Refuse if `path` exists and is a symlink. Used at file-write time
/// (binary, manifest) to prevent `tokio::fs::write` from following a
/// pre-created symlink and overwriting files outside `plugins_dir`.
///
/// `NotFound` is OK (caller will create the file). Regular files and
/// other non-symlink types are allowed (caller's `tokio::fs::write`
/// will overwrite them, which is the intended behaviour for re-install).
#[cfg(feature = "plugin-registry")]
async fn refuse_symlink_at(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(format!(
            "Refusing to write to {:?}: is a symlink (potential file-overwrite attack)",
            path
        )
        .into()),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("Failed to stat {:?}: {}", path, e).into()),
    }
}

// ============================================================================
// Pure helpers for plugin install layout (#948)
// ============================================================================
//
// These compute paths and build the manifest from a registry entry. Extracted
// as free functions so they can be unit-tested without network/disk access —
// the integration test (`test_install_layout_round_trips_through_plugin_discovery`)
// wires them up against `tempfile::tempdir()` to verify the full subdirectory
// + manifest layout matches what `PluginDiscovery::scan()` expects.

/// Validate that a plugin identifier is safe to use in path construction.
///
/// Plugin ids are used to build subdirectory paths (`<plugins_dir>/<id>/`)
/// and binary filenames (`libconductor_<id>_plugin.<ext>`). A malicious or
/// malformed id like `..`, `foo/bar`, `/etc/passwd`, or one containing
/// shell metacharacters could escape the plugins directory or smuggle
/// subpaths into the binary filename.
///
/// # Errors
///
/// Returns `io::ErrorKind::InvalidInput` for:
///
/// - empty ids
/// - any character outside `[A-Za-z0-9_-]`
/// - ids longer than [`crate::plugin_id::MAX_PLUGIN_ID_LEN`] (128 bytes)
///   (PR #1029 review fix: previous docs only mentioned the
///   character-set rule; the length cap moved into the shared
///   validator in round-4 so the install path now also enforces
///   it. Updating these docs to match.)
pub fn validate_plugin_id(plugin_id: &str) -> std::io::Result<()> {
    // PR #1029 round-2: delegate to the shared
    // `crate::plugin_id::validate_plugin_id` so install (here)
    // and runtime (`plugin::wasm_runtime::is_safe_plugin_id`)
    // can never diverge on the character set OR length cap.
    crate::plugin_id::validate_plugin_id(plugin_id)
}

/// Compute the install directory for a given plugin id under `plugins_dir`.
///
/// Layout: `<plugins_dir>/<plugin_id>/` — matches the subdirectory shape
/// `PluginDiscovery::scan()` looks for in `conductor-core/src/plugin/discovery.rs`.
///
/// # Errors
///
/// Returns `io::Error` if `plugin_id` fails [`validate_plugin_id`] (e.g.
/// contains path separators, `..`, or other characters that could escape
/// `plugins_dir`).
pub fn plugin_install_dir(plugins_dir: &Path, plugin_id: &str) -> std::io::Result<PathBuf> {
    validate_plugin_id(plugin_id)?;
    Ok(plugins_dir.join(plugin_id))
}

/// Compute the binary filename for a plugin id + platform extension.
///
/// Format: `libconductor_<id>_plugin.<ext>` — matches the GUI uninstall /
/// list logic in `conductor-gui/src-tauri/src/plugin_commands.rs`.
///
/// # Errors
///
/// Returns `io::Error` if `plugin_id` fails [`validate_plugin_id`] (e.g.
/// contains path separators that could smuggle subpaths into the
/// resulting filename).
pub fn plugin_binary_filename(plugin_id: &str, ext: &str) -> std::io::Result<String> {
    validate_plugin_id(plugin_id)?;
    Ok(format!("libconductor_{}_plugin.{}", plugin_id, ext))
}

/// Populate `capabilities_enriched` on every plugin entry by mapping
/// each raw capability string through `enrich_capability` (#1074).
///
/// Idempotent: re-enriching an already-enriched entry overwrites
/// with the same data, so this is safe to call after `fetch_registry`
/// or `load_cached_registry` on every code path that hands the
/// registry to the GUI.
pub fn enrich_registry_capabilities(registry: &mut PluginRegistry) {
    use crate::plugin::enrich_capability;
    for entry in &mut registry.plugins {
        entry.capabilities_enriched = entry
            .capabilities
            .iter()
            .map(|s| enrich_capability(s))
            .collect();
    }
}

/// Convert registry capability strings (e.g. `"network"`) to
/// `ManifestCapabilities` (the typed-bool struct that plugin.toml
/// deserializes to).
///
/// Mapping rules (case-insensitive lookup, `storage` → Filesystem,
/// `system_control` / `systemcontrol` → SystemControl) live in the
/// shared `Capability::from_registry_str` so the registry-side bool
/// fan-out and the GUI-side `enrich_capability` (#1074) can never
/// diverge. Unknown strings are silently dropped — this is forward-
/// compatible for future capabilities the registry may declare
/// before this client knows about them.
pub fn capabilities_from_registry(caps: &[String]) -> crate::plugin::ManifestCapabilities {
    use crate::plugin::{Capability, ManifestCapabilities};
    let mut out = ManifestCapabilities::default();
    for c in caps {
        match Capability::from_registry_str(c) {
            Some(Capability::Network) => out.network = true,
            Some(Capability::Filesystem) => out.filesystem = true,
            Some(Capability::Audio) => out.audio = true,
            Some(Capability::Midi) => out.midi = true,
            Some(Capability::Subprocess) => out.subprocess = true,
            Some(Capability::SystemControl) => out.system_control = true,
            None => {}
        }
    }
    out
}

/// Build a `PluginManifest` (plugin.toml shape) from a registry entry +
/// the binary filename produced by `plugin_binary_filename`.
///
/// Field mapping:
/// - `entry.id` → `manifest.plugin.name` (the canonical identifier the
///   discovery scan uses to register the plugin)
/// - `entry.repository` → `manifest.plugin.homepage` (None when empty)
/// - Plugin type defaults to `Action` (the registry doesn't carry a type
///   field today; vast majority of plugins are action plugins)
pub fn manifest_from_registry_entry(
    entry: &PluginRegistryEntry,
    binary_filename: String,
) -> crate::plugin::PluginManifest {
    use crate::plugin::PluginType;
    use crate::plugin::{ManifestPlugin, PluginManifest};
    PluginManifest {
        plugin: ManifestPlugin {
            name: entry.id.clone(),
            version: entry.version.clone(),
            description: entry.description.clone(),
            author: entry.author.clone(),
            homepage: if entry.repository.is_empty() {
                None
            } else {
                Some(entry.repository.clone())
            },
            license: entry.license.clone(),
            plugin_type: PluginType::Action,
            binary: binary_filename,
            capabilities: capabilities_from_registry(&entry.capabilities),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: PR #946 hard-renamed the `min_midimon_version` JSON field to
    /// `min_conductor_version`, which broke deserialization against the live
    /// registry data at `conductor-plugin-registry/main/registry.json` (still
    /// emits the old field name). User-visible failure: "Failed to fetch
    /// plugin registry: error decoding response body" in the GUI plugins view.
    ///
    /// Fixed by adding `#[serde(alias = "min_midimon_version")]` so both
    /// spellings deserialize. This test pins the contract using the exact
    /// shape currently served by the live registry — if it passes, the GUI
    /// plugins view loads against unmigrated registry data.
    #[test]
    fn test_registry_deserializes_legacy_min_midimon_version_field() {
        // Trimmed real-world shape from the live registry (one entry).
        // The critical field is `min_midimon_version` — the rest is filler.
        let json = r#"{
            "version": "1.0.0",
            "last_updated": "2025-11-19T11:30:00Z",
            "plugins": [
                {
                    "id": "spotify",
                    "name": "Spotify Web API",
                    "version": "0.1.0",
                    "author": "Conductor Contributors",
                    "description": "Control Spotify playback",
                    "category": "music",
                    "tags": ["spotify"],
                    "capabilities": ["Network"],
                    "download_url": "https://example.com/spotify.wasm",
                    "signature_url": "https://example.com/spotify.wasm.sig",
                    "checksum": "sha256:abc",
                    "size_bytes": 69632,
                    "license": "MIT",
                    "repository": "https://github.com/example/repo",
                    "documentation": "https://example.com/docs",
                    "min_midimon_version": "2.5.0",
                    "signed": false,
                    "verified": false
                }
            ],
            "categories": []
        }"#;

        let registry: PluginRegistry = serde_json::from_str(json)
            .expect("legacy `min_midimon_version` field must deserialize via serde alias");
        assert_eq!(registry.plugins.len(), 1);
        assert_eq!(registry.plugins[0].min_conductor_version, "2.5.0");
    }

    /// Forward-compatible: the new spelling `min_conductor_version` continues
    /// to deserialize. Together with the legacy-field test above, this pins
    /// the alias contract from both directions.
    #[test]
    fn test_registry_deserializes_new_min_conductor_version_field() {
        let json = r#"{
            "version": "1.0.0",
            "last_updated": "2026-04-29T00:00:00Z",
            "plugins": [
                {
                    "id": "obs",
                    "name": "OBS",
                    "version": "0.1.0",
                    "author": "Test",
                    "description": "OBS control",
                    "category": "streaming",
                    "tags": [],
                    "capabilities": [],
                    "download_url": "https://example.com/obs.wasm",
                    "signature_url": "https://example.com/obs.wasm.sig",
                    "checksum": "sha256:def",
                    "size_bytes": 1024,
                    "license": "MIT",
                    "repository": "https://example.com",
                    "documentation": "https://example.com",
                    "min_conductor_version": "5.0.0",
                    "signed": false,
                    "verified": false
                }
            ],
            "categories": []
        }"#;

        let registry: PluginRegistry =
            serde_json::from_str(json).expect("new `min_conductor_version` field must deserialize");
        assert_eq!(registry.plugins[0].min_conductor_version, "5.0.0");
    }

    #[test]
    fn test_current_platform() {
        let client = PluginRegistryClient::default_registry();
        let platform = client.current_platform();

        // Should be one of the supported platforms
        assert!(matches!(
            platform.as_str(),
            "macos-x86_64" | "macos-aarch64" | "linux-x86_64" | "windows-x86_64"
        ));
    }

    #[test]
    fn test_search_plugins() {
        let registry = PluginRegistry {
            version: "1.0.0".to_string(),
            last_updated: "2025-01-18T00:00:00Z".to_string(),
            plugins: vec![PluginRegistryEntry {
                id: "spotify".to_string(),
                name: "Spotify Control".to_string(),
                description: "Control Spotify playback".to_string(),
                author: "Test".to_string(),
                version: "0.1.0".to_string(),
                category: "media".to_string(),
                tags: vec!["spotify".to_string(), "music".to_string()],
                capabilities: vec!["network".to_string()],
                download_url: "https://example.com/spotify.wasm".to_string(),
                signature_url: "https://example.com/spotify.wasm.sig".to_string(),
                checksum: "abc123".to_string(),
                size_bytes: 1024,
                license: "MIT".to_string(),
                repository: "https://github.com/example/spotify-plugin".to_string(),
                documentation: "https://example.com/docs".to_string(),
                min_conductor_version: "2.3.0".to_string(),
                signed: true,
                verified: true,
                capabilities_enriched: Vec::new(),
            }],
            featured_plugins: vec![],
            categories: vec![],
        };

        let client = PluginRegistryClient::default_registry();
        let results = client.search_plugins(&registry, "spotify");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "spotify");
    }

    // ========================================================================
    // #948: subdirectory + manifest install layout
    // ========================================================================

    fn make_test_entry(id: &str) -> PluginRegistryEntry {
        PluginRegistryEntry {
            id: id.to_string(),
            name: format!("Test {}", id),
            description: format!("Test plugin {}", id),
            author: "Test Author".to_string(),
            version: "1.2.3".to_string(),
            category: "test".to_string(),
            tags: vec![],
            capabilities: vec!["network".to_string()],
            download_url: format!("https://example.com/{}.dylib", id),
            signature_url: String::new(),
            checksum: String::new(),
            size_bytes: 0,
            license: "MIT".to_string(),
            repository: format!("https://github.com/example/{}-plugin", id),
            documentation: String::new(),
            min_conductor_version: "5.0.0".to_string(),
            signed: false,
            verified: false,
            capabilities_enriched: Vec::new(),
        }
    }

    #[test]
    fn test_plugin_install_dir_is_subdir_of_plugins_dir() {
        let plugins_dir = Path::new("/tmp/plugins");
        assert_eq!(
            plugin_install_dir(plugins_dir, "spotify").unwrap(),
            PathBuf::from("/tmp/plugins/spotify")
        );
        assert_eq!(
            plugin_install_dir(plugins_dir, "obs").unwrap(),
            PathBuf::from("/tmp/plugins/obs")
        );
    }

    #[test]
    fn test_plugin_binary_filename_matches_gui_loader_pattern() {
        // Pattern must match `libconductor_*_plugin.{ext}` — the same shape
        // GUI uninstall/list expects in `conductor-gui/src-tauri/src/plugin_commands.rs`.
        assert_eq!(
            plugin_binary_filename("spotify", "dylib").unwrap(),
            "libconductor_spotify_plugin.dylib"
        );
        assert_eq!(
            plugin_binary_filename("obs", "so").unwrap(),
            "libconductor_obs_plugin.so"
        );
        assert_eq!(
            plugin_binary_filename("http", "dll").unwrap(),
            "libconductor_http_plugin.dll"
        );
    }

    // ========================================================================
    // Path-traversal protection (Copilot review on PR #950)
    // ========================================================================

    #[test]
    fn test_validate_plugin_id_accepts_valid_ids() {
        // Allowed: ASCII alphanumeric + underscore + hyphen
        assert!(validate_plugin_id("spotify").is_ok());
        assert!(validate_plugin_id("obs-studio").is_ok());
        assert!(validate_plugin_id("plugin_name").is_ok());
        assert!(validate_plugin_id("Plugin123").is_ok());
        assert!(validate_plugin_id("MIXED-Case_With-99").is_ok());
        assert!(validate_plugin_id("a").is_ok()); // single char
    }

    #[test]
    fn test_validate_plugin_id_rejects_empty() {
        let err = validate_plugin_id("").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn test_validate_plugin_id_rejects_overlong_via_facade() {
        // PR #1029 round-7 review (2026-05-02): the 128-byte
        // limit lives in `crate::plugin_id` and was previously
        // covered only by tests there. This regression test
        // pins the limit at the `plugin_registry` facade —
        // if someone later inlines a different validator here
        // (or a future refactor unwires the delegation), this
        // test catches the install/runtime divergence
        // immediately. The boundary case `len == 128` is
        // accepted; `len == 129` is rejected.
        let exactly_max: String = "a".repeat(crate::plugin_id::MAX_PLUGIN_ID_LEN);
        let too_long: String = "a".repeat(crate::plugin_id::MAX_PLUGIN_ID_LEN + 1);
        assert!(
            validate_plugin_id(&exactly_max).is_ok(),
            "{}-char id must be accepted via the facade",
            crate::plugin_id::MAX_PLUGIN_ID_LEN,
        );
        let err = validate_plugin_id(&too_long).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn test_validate_plugin_id_rejects_path_traversal() {
        // The actual security-critical cases: anything that could escape
        // `plugins_dir` or smuggle a subpath into the binary filename.
        for malicious in &[
            "..",                // parent
            "../escape",         // relative escape
            "../../etc/passwd",  // deeper escape
            "foo/bar",           // forward slash
            "foo\\bar",          // backslash (Windows-style)
            "/absolute/path",    // unix absolute
            "C:\\windows\\path", // windows absolute
            ".",                 // current dir
            "./local",           // explicit current
        ] {
            let result = validate_plugin_id(malicious);
            assert!(
                result.is_err(),
                "validate_plugin_id should reject {:?}: {:?}",
                malicious,
                result
            );
        }
    }

    #[test]
    fn test_validate_plugin_id_rejects_other_special_chars() {
        // Everything outside [A-Za-z0-9_-] must be rejected — covers
        // shell metacharacters, null bytes, control chars, etc.
        for malicious in &[
            "plugin name",  // space
            "plugin\0null", // null byte
            "plugin\nname", // newline
            "plugin;rm",    // shell metacharacter
            "plugin&rm",    // shell metacharacter
            "plugin|rm",    // pipe
            "plugin$VAR",   // shell var expansion
            "plugin`cmd`",  // backtick command sub
            "plugin@host",  // at-sign
            "plugin:port",  // colon
            "plugin.toml",  // period (could collide with manifest)
        ] {
            let result = validate_plugin_id(malicious);
            assert!(
                result.is_err(),
                "validate_plugin_id should reject {:?}: {:?}",
                malicious,
                result
            );
        }
    }

    #[test]
    fn test_plugin_install_dir_propagates_validation_error() {
        let plugins_dir = Path::new("/tmp/plugins");
        // Malicious ids must error, NOT produce an escaped path
        assert!(plugin_install_dir(plugins_dir, "../escape").is_err());
        assert!(plugin_install_dir(plugins_dir, "foo/bar").is_err());
        assert!(plugin_install_dir(plugins_dir, "/absolute").is_err());
        assert!(plugin_install_dir(plugins_dir, "").is_err());
        // Valid ids still work
        assert!(plugin_install_dir(plugins_dir, "valid-id").is_ok());
    }

    #[test]
    fn test_plugin_binary_filename_propagates_validation_error() {
        // Malicious ids must error, NOT produce a smuggled filename
        assert!(plugin_binary_filename("../escape", "dylib").is_err());
        assert!(plugin_binary_filename("foo/bar", "dylib").is_err());
        assert!(plugin_binary_filename("", "dylib").is_err());
        // Valid ids still work
        assert!(plugin_binary_filename("valid-id", "dylib").is_ok());
    }

    #[test]
    fn test_capabilities_from_registry_known_strings() {
        let caps = vec!["network".to_string(), "filesystem".to_string()];
        let out = capabilities_from_registry(&caps);
        assert!(out.network);
        assert!(out.filesystem);
        assert!(!out.audio);
        assert!(!out.midi);
        assert!(!out.subprocess);
        assert!(!out.system_control);
    }

    #[test]
    fn test_capabilities_from_registry_case_insensitive() {
        let caps = vec!["Network".to_string(), "FILESYSTEM".to_string()];
        let out = capabilities_from_registry(&caps);
        assert!(out.network);
        assert!(out.filesystem);
    }

    #[test]
    fn test_capabilities_from_registry_accepts_both_system_control_spellings() {
        let snake = capabilities_from_registry(&["system_control".to_string()]);
        assert!(snake.system_control);
        let camel = capabilities_from_registry(&["SystemControl".to_string()]);
        assert!(camel.system_control);
    }

    #[test]
    fn test_capabilities_from_registry_accepts_storage_alias_for_filesystem() {
        // The in-repo registry data (`plugins/registry/registry.json`)
        // uses "storage" for the filesystem capability — Spotify entry
        // declares `"capabilities": ["network", "storage"]`. Pre-fix the
        // converter dropped "storage" silently, generating manifests
        // without the intended filesystem capability and leading to
        // missing permission prompts.
        let storage = capabilities_from_registry(&["storage".to_string()]);
        assert!(
            storage.filesystem,
            "registry capability 'storage' must map to filesystem"
        );
        // Case-insensitive (already covered by other tests, but lock in
        // the canonical "Storage" spelling used in registry data)
        let titled = capabilities_from_registry(&["Storage".to_string()]);
        assert!(titled.filesystem);
        // "filesystem" still works — both spellings are accepted
        let fs = capabilities_from_registry(&["filesystem".to_string()]);
        assert!(fs.filesystem);
    }

    #[test]
    fn test_capabilities_from_registry_unknown_strings_dropped() {
        // Forward-compat: an unknown capability string from a future registry
        // entry shouldn't error or panic, just be dropped silently.
        let caps = vec!["future_capability".to_string(), "network".to_string()];
        let out = capabilities_from_registry(&caps);
        assert!(out.network);
        // No way to assert "future_capability is dropped" — just that the
        // call returned without panic and the recognised flag was set.
    }

    #[test]
    fn test_manifest_from_registry_entry_uses_id_as_canonical_name() {
        // PluginDiscovery::scan() registers plugins by manifest.plugin.name.
        // We use entry.id as that name, NOT entry.name (which is human-readable
        // display text like "Spotify Control") — the identifier must match
        // the binary filename and be stable across UI relabelling.
        let entry = make_test_entry("spotify");
        let manifest =
            manifest_from_registry_entry(&entry, "libconductor_spotify_plugin.dylib".to_string());
        assert_eq!(manifest.plugin.name, "spotify");
        assert_ne!(manifest.plugin.name, entry.name); // name != display
    }

    #[test]
    fn test_manifest_from_registry_entry_preserves_metadata() {
        let entry = make_test_entry("spotify");
        let manifest =
            manifest_from_registry_entry(&entry, "libconductor_spotify_plugin.dylib".to_string());
        assert_eq!(manifest.plugin.version, "1.2.3");
        assert_eq!(manifest.plugin.description, "Test plugin spotify");
        assert_eq!(manifest.plugin.author, "Test Author");
        assert_eq!(manifest.plugin.license, "MIT");
        assert_eq!(
            manifest.plugin.homepage,
            Some("https://github.com/example/spotify-plugin".to_string())
        );
        assert_eq!(manifest.plugin.binary, "libconductor_spotify_plugin.dylib");
        assert!(manifest.plugin.capabilities.network);
    }

    #[test]
    fn test_manifest_from_registry_entry_omits_homepage_when_repository_empty() {
        let mut entry = make_test_entry("test");
        entry.repository = String::new();
        let manifest = manifest_from_registry_entry(&entry, "x.dylib".to_string());
        assert_eq!(manifest.plugin.homepage, None);
    }

    /// Round-trip integration test: build the install layout the same way
    /// `install_plugin` does (subdirectory + binary + manifest), then verify
    /// `PluginDiscovery::scan()` finds it. This is the regression test for
    /// #948 — pre-fix, the flat-file install produced a layout discovery
    /// silently ignored.
    /// Atomic manifest write contract (Copilot review on PR #950, comment 3157097707).
    ///
    /// `install_plugin` now writes the manifest via
    /// `config::preferences::atomic_write_toml` (temp file + rename + fsync)
    /// instead of the non-atomic `tokio::fs::write`. This test exercises that
    /// helper at the same call shape `install_plugin` uses, and asserts:
    ///
    /// 1. The manifest is parseable as `PluginManifest` after the write.
    /// 2. No leftover temp files remain in the install directory.
    /// 3. The file content survives a `PluginDiscovery::scan()` round-trip.
    ///
    /// The full install_plugin path can't be unit-tested without a mock HTTP
    /// server; this test isolates the manifest-write step that changed.
    #[test]
    fn test_atomic_manifest_write_is_parseable_and_leaves_no_temp_files() {
        use crate::plugin::{PluginDiscovery, PluginManifest};
        let temp = tempfile::tempdir().expect("create tempdir");
        let install_dir = temp.path().join("test_plugin");
        std::fs::create_dir_all(&install_dir).expect("create install dir");

        let entry = make_test_entry("test_plugin");
        let manifest = manifest_from_registry_entry(
            &entry,
            plugin_binary_filename("test_plugin", "dylib").expect("valid id"),
        );

        // Same call shape as install_plugin (post-fix)
        let manifest_path = install_dir.join("plugin.toml");
        crate::config::preferences::atomic_write_toml(&manifest_path, &manifest)
            .expect("atomic manifest write should succeed");

        // 1. Manifest parses
        let content = std::fs::read_to_string(&manifest_path).expect("read manifest");
        let parsed: PluginManifest = toml::from_str(&content).expect("parse manifest");
        assert_eq!(parsed.plugin.name, "test_plugin");
        assert_eq!(parsed.plugin.version, "1.2.3");

        // 2. No leftover .tmp files (atomic_write_toml uses `.<name>.<rand>.tmp`
        // and cleans up on success via rename and on failure via remove_file)
        let entries: Vec<String> = std::fs::read_dir(&install_dir)
            .expect("read dir")
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
            .collect();
        let temp_files: Vec<_> = entries
            .iter()
            .filter(|n| n.ends_with(".tmp") || n.contains(".tmp."))
            .collect();
        assert!(
            temp_files.is_empty(),
            "atomic write must clean up temp files; found: {:?} in {:?}",
            temp_files,
            entries
        );

        // 3. PluginDiscovery::scan() round-trip — the manifest must be
        // discoverable via the same code the daemon uses at startup. (We
        // also need a binary file present — discovery records the path
        // but doesn't load it.)
        std::fs::write(
            install_dir.join("libconductor_test_plugin_plugin.dylib"),
            b"fake binary",
        )
        .expect("write fake binary");
        let discovery = PluginDiscovery::new(temp.path().to_path_buf());
        let registry = discovery.scan().expect("scan should succeed");
        assert!(
            registry.list_names().contains(&"test_plugin".to_string()),
            "manifest written via atomic_write_toml must be discoverable; got: {:?}",
            registry.list_names()
        );
    }

    #[test]
    fn test_install_layout_round_trips_through_plugin_discovery() {
        use crate::plugin::PluginDiscovery;
        let temp = tempfile::tempdir().expect("create tempdir");
        let plugins_dir = temp.path();

        // Build the layout for two synthetic plugins
        for plugin_id in &["spotify", "obs"] {
            let entry = make_test_entry(plugin_id);
            let install_dir = plugin_install_dir(plugins_dir, plugin_id).expect("valid plugin id");
            std::fs::create_dir_all(&install_dir).expect("create install dir");

            let binary_filename =
                plugin_binary_filename(plugin_id, "dylib").expect("valid plugin id");
            // Fake binary — discovery doesn't load it, just records the path
            std::fs::write(install_dir.join(&binary_filename), b"fake binary")
                .expect("write fake binary");

            let manifest = manifest_from_registry_entry(&entry, binary_filename);
            let toml_str = toml::to_string(&manifest).expect("serialize manifest");
            std::fs::write(install_dir.join("plugin.toml"), toml_str).expect("write manifest");
        }

        // Run the actual discovery scan
        let discovery = PluginDiscovery::new(plugins_dir.to_path_buf());
        let registry = discovery.scan().expect("scan should succeed");

        // Both plugins should be discoverable by their id
        let names: Vec<_> = registry.list_names().into_iter().collect();
        assert!(
            names.contains(&"spotify".to_string()),
            "spotify not found in: {:?}",
            names
        );
        assert!(
            names.contains(&"obs".to_string()),
            "obs not found in: {:?}",
            names
        );

        // Pre-#948 sanity: a flat binary without a subdirectory + manifest
        // is silently ignored by discovery. Adding a stray flat file here
        // shouldn't be picked up.
        std::fs::write(
            plugins_dir.join("libconductor_orphan_plugin.dylib"),
            b"flat binary",
        )
        .expect("write flat binary");
        let registry2 = discovery.scan().expect("re-scan");
        assert!(
            !registry2.list_names().contains(&"orphan".to_string()),
            "flat-file install must NOT be picked up by discovery (pre-#948 behaviour was the bug)"
        );
    }
}
