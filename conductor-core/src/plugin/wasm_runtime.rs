// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! WASM plugin runtime for sandboxed plugin execution (v2.5)
//!
//! This module provides a secure, isolated runtime for executing plugins compiled
//! to WebAssembly. WASM plugins run in a sandboxed environment with:
//! - Memory isolation (cannot access daemon memory)
//! - Resource limits (CPU, memory, execution time)
//! - Capability-based permissions (WASI)
//! - Platform independence (same .wasm runs on macOS/Linux/Windows)
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────┐
//! │  Conductor Daemon (Rust)              │
//! │  ┌───────────────────────────────┐  │
//! │  │  WASM Runtime (wasmtime)      │  │
//! │  │  ┌─────────────────────────┐  │  │
//! │  │  │  Plugin.wasm            │  │  │
//! │  │  │  - Sandboxed execution  │  │  │
//! │  │  │  - Resource limits      │  │  │
//! │  │  │  - Capability system    │  │  │
//! │  │  └─────────────────────────┘  │  │
//! │  └───────────────────────────────┘  │
//! └─────────────────────────────────────┘
//! ```
//!
//! ## Security Features
//!
//! 1. **Process Isolation**: Plugin runs in separate memory space
//! 2. **Resource Limits**: Configurable limits on memory, CPU, execution time
//! 3. **Capability System**: Fine-grained permission control via WASI
//! 4. **Timeout Protection**: Plugins cannot run indefinitely
//! 5. **Crash Isolation**: Plugin crash doesn't affect daemon
//!
//! ## Example Usage
//!
//! ```rust,ignore
//! use conductor_core::plugin::wasm_runtime::{WasmPlugin, WasmConfig};
//! use conductor_core::plugin::types::Capability;
//! use std::path::Path;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // WasmConfig is #[non_exhaustive] and has no Default impl: external
//! // callers MUST construct it via `WasmConfig::new` (which seeds the
//! // conservative defaults a previous `Default` produced) and then assign
//! // public fields to override.
//! let mut config = WasmConfig::new("example-plugin")?;
//! config.max_memory_bytes = 128 * 1024 * 1024;
//! config.max_execution_time = std::time::Duration::from_secs(5);
//! config.max_fuel = 100_000_000;
//! config.capabilities = vec![Capability::Network];
//!
//! let mut plugin = WasmPlugin::load(
//!     Path::new("plugins/spotify/plugin.wasm"),
//!     config,
//! ).await?;
//!
//! plugin.execute("play", &[], &Default::default()).await?;
//! # Ok(())
//! # }
//! ```

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use wasmtime::*;
use wasmtime_wasi::WasiCtxBuilder;
use wasmtime_wasi::p1::WasiP1Ctx;

use crate::error::EngineError;
use crate::events::ProcessedEvent;
use crate::plugin::{Capability, PluginMetadata, TriggerContext};

/// ADR-027 D10c — convenience wrapper for the runtime side of
/// the install/runtime-shared plugin-id validator.
///
/// Plugin ids ultimately come from plugin manifests, which are
/// untrusted at install-TOFU time (D9). If a plugin sets its id
/// to `"../other-plugin"` and we naively `join` it onto the
/// plugin-data root, the resulting path escapes the per-plugin
/// sandbox and gives the plugin read/write access to a sibling
/// plugin's data (or worse, anywhere the daemon's UID can write).
///
/// Delegates entirely to
/// [`crate::plugin_id::validate_plugin_id`] — the **single
/// source of truth** for both character set AND length cap (PR
/// #1029 round-3, 2026-05-02). Having two validators (or one
/// validator that only enforced part of the rules) created
/// install/runtime divergence: an id accepted at install would
/// then fail at runtime (or, for length, the reverse). Now both
/// call sites apply the same rules.
pub(crate) fn is_safe_plugin_id(id: &str) -> bool {
    crate::plugin_id::validate_plugin_id(id).is_ok()
}

/// ADR-027 D10c — build the per-plugin data subdirectory under
/// the plugin-data root. Each plugin gets its own subdir so a
/// plugin with the Filesystem capability can only read/write its
/// own data, not its peers'. Replaces the pre-D10c shared
/// `plugin-data/` mount.
///
/// `plugin_id` is rejected via [`is_safe_plugin_id`] before any
/// path joining, so a malicious manifest can't escape the sandbox.
pub(crate) fn plugin_data_subdir(
    plugin_id: &str,
    plugin_data_root: &Path,
) -> Result<PathBuf, EngineError> {
    if !is_safe_plugin_id(plugin_id) {
        return Err(EngineError::PluginLoadFailed(format!(
            "plugin id {:?} is unsafe for use as a filesystem subdirectory \
             (must be 1–128 chars of `[A-Za-z0-9_-]` per \
             plugin_registry::validate_plugin_id); refusing to preopen to \
             avoid path-traversal out of the plugin-data root",
            plugin_id,
        )));
    }
    Ok(plugin_data_root.join(plugin_id))
}

/// ADR-027 D10c — validate the plugin id supplied in
/// [`WasmConfig.plugin_id`] before it's used as a filesystem
/// subdirectory name. Returns `Ok(plugin_id.to_string())` for
/// safe ids, `EngineError::PluginLoadFailed` otherwise.
///
/// `plugin_id` is now non-optional on [`WasmConfig`] (PR #1029
/// round-3, 2026-05-02): it was previously `Option<String>` with
/// a file-stem fallback when None, but two plugins loaded from
/// `/tmp/a/plugin.wasm` and `/opt/b/plugin.wasm` would resolve
/// to the same `"plugin"` id and share a sandbox. Round-2
/// removed the fallback at runtime; round-3 promotes the
/// invariant to the type system so a `Default::default()` user
/// can't construct an invalid config in the first place.
///
/// `plugin_path` is retained in the error message so operators
/// can identify which plugin failed when debugging
/// configuration issues.
pub(crate) fn resolve_plugin_id(
    plugin_id: &str,
    plugin_path: &Path,
) -> Result<String, EngineError> {
    if !is_safe_plugin_id(plugin_id) {
        return Err(EngineError::PluginLoadFailed(format!(
            "ADR-027 D10c: WasmConfig.plugin_id {:?} for plugin \
             at {:?} is unsafe; must be 1–128 chars of \
             `[A-Za-z0-9_-]` per \
             plugin_registry::validate_plugin_id.",
            plugin_id, plugin_path,
        )));
    }
    Ok(plugin_id.to_string())
}

/// Configuration for WASM plugin runtime.
///
/// `#[non_exhaustive]` (PR #1029 review, 2026-05-02): security-
/// relevant additions to this struct (like the D10c `plugin_id`
/// field) shouldn't break downstream callers' struct literals.
/// External crates must construct via [`WasmConfig::new`]
/// (which requires the security-mandatory `plugin_id`) and
/// chained setters; same-crate callers may use a struct literal
/// directly. Note: `WasmConfig` has **no `Default` impl**
/// (round-3 dropped it because every constructor needs to pick
/// an explicit per-plugin id — see `plugin_id` field docs).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct WasmConfig {
    /// Maximum memory in bytes (default: 128 MB)
    pub max_memory_bytes: u64,

    /// Maximum execution time per call (default: 5 seconds)
    pub max_execution_time: Duration,

    /// Maximum fuel (instruction count) per call (default: 100M instructions)
    /// 1 fuel ≈ 1 WASM instruction
    pub max_fuel: u64,

    /// Capabilities granted to this plugin
    pub capabilities: Vec<Capability>,

    /// Require cryptographic signature for plugin loading (default: false for backward compatibility)
    #[cfg(feature = "plugin-signing")]
    pub require_signature: bool,

    /// Allow plugins signed by any key (self-signed), or require trusted keys (default: false)
    #[cfg(feature = "plugin-signing")]
    pub allow_self_signed: bool,

    /// Custom path to trusted_keys.toml file (default: ~/.config/conductor/trusted_keys.toml)
    #[cfg(feature = "plugin-signing")]
    pub trusted_keys_path: Option<std::path::PathBuf>,

    /// ADR-027 D9 — custom path to the revocation list (CRL) `revoked_keys.toml`
    /// (default: ~/.config/conductor/revoked_keys.toml). A signing key whose
    /// fingerprint is listed is refused at load on **every** signature path.
    #[cfg(feature = "plugin-signing")]
    pub revoked_keys_path: Option<std::path::PathBuf>,

    /// ADR-027 D10c — plugin identifier used as the filesystem
    /// subdirectory name for this plugin's preopened data dir.
    ///
    /// **Required** (no Default value, no fallback). The
    /// security guarantee is enforced at LOAD TIME, not at the
    /// type level (PR #1029 round-4 review, 2026-05-02): the
    /// field is `pub String`, so callers within or outside this
    /// crate can technically assign any value (including unsafe
    /// strings like `"../escape"`). The check that prevents
    /// path traversal lives in [`resolve_plugin_id`], called
    /// from [`WasmPlugin::load`] — an unsafe id at load time
    /// produces `EngineError::PluginLoadFailed`, never a
    /// directory creation outside the plugin-data root.
    ///
    /// Why not make this private with a validating setter? It
    /// would forbid `WasmConfig::new("invalid")` at construction
    /// time too, which is a bigger ergonomic + breakage cost
    /// than the load-time check carries. The threat model is:
    /// "A manifest contains a hostile id; can it traverse the
    /// filesystem at runtime?" Answer either way: no, because
    /// `resolve_plugin_id` rejects it before any path join.
    ///
    /// The daemon's `plugin_manager` passes the registry key
    /// here, which has already been validated by
    /// [`crate::plugin_id::validate_plugin_id`] at install time.
    pub plugin_id: String,
}

impl WasmConfig {
    /// Construct a `WasmConfig` for the given plugin id, with
    /// all other fields at their conservative defaults.
    ///
    /// **Validates the plugin id at construction time** (PR
    /// #1029 round-7, 2026-05-02). Pre-fix this was infallible
    /// and the security check happened at `WasmPlugin::load`,
    /// which let `WasmConfig::new("../escape")` succeed at
    /// construction and only fail at load time. Now the
    /// validation moves to construction so an unsafe id is
    /// rejected immediately.
    ///
    /// The field is still `pub`, so a determined caller can
    /// `config.plugin_id = "../escape".to_string()` post-
    /// construction. The runtime check in `WasmPlugin::load`
    /// is retained as a defence-in-depth backstop for that
    /// case (and for direct struct-literal construction within
    /// `conductor-core`). Making the field private would
    /// require a substantial API refactor for marginal added
    /// safety; the construction-time validation closes the
    /// "callers create an invalid config and discover it later"
    /// gap that was the actual review concern.
    ///
    /// Use the chained `with_*` setters to override the other
    /// defaults. The daemon's `plugin_manager` passes the
    /// registry key (already install-validated); tests pass a
    /// stable per-test id.
    pub fn new(plugin_id: impl Into<String>) -> Result<Self, EngineError> {
        let plugin_id = plugin_id.into();
        if !is_safe_plugin_id(&plugin_id) {
            return Err(EngineError::PluginLoadFailed(format!(
                "ADR-027 D10c: WasmConfig::new rejected plugin_id \
                 {plugin_id:?} (must be 1–{} chars of `[A-Za-z0-9_-]` \
                 per crate::plugin_id::validate_plugin_id)",
                crate::plugin_id::MAX_PLUGIN_ID_LEN,
            )));
        }
        Ok(Self {
            max_memory_bytes: 128 * 1024 * 1024, // 128 MB
            max_execution_time: Duration::from_secs(5),
            max_fuel: 100_000_000, // 100M instructions
            capabilities: Vec::new(),
            #[cfg(feature = "plugin-signing")]
            require_signature: false, // Backward compatible: signatures optional by default
            #[cfg(feature = "plugin-signing")]
            allow_self_signed: false, // Require trusted keys by default
            #[cfg(feature = "plugin-signing")]
            trusted_keys_path: None, // Use default ~/.config/conductor/trusted_keys.toml
            #[cfg(feature = "plugin-signing")]
            revoked_keys_path: None, // Use default ~/.config/conductor/revoked_keys.toml
            plugin_id,
        })
    }

    pub fn with_max_memory_bytes(mut self, bytes: u64) -> Self {
        self.max_memory_bytes = bytes;
        self
    }

    pub fn with_max_execution_time(mut self, dur: Duration) -> Self {
        self.max_execution_time = dur;
        self
    }

    pub fn with_max_fuel(mut self, fuel: u64) -> Self {
        self.max_fuel = fuel;
        self
    }

    pub fn with_capabilities(mut self, caps: Vec<Capability>) -> Self {
        self.capabilities = caps;
        self
    }

    #[cfg(feature = "plugin-signing")]
    pub fn with_require_signature(mut self, require: bool) -> Self {
        self.require_signature = require;
        self
    }

    #[cfg(feature = "plugin-signing")]
    pub fn with_allow_self_signed(mut self, allow: bool) -> Self {
        self.allow_self_signed = allow;
        self
    }
}

/// Resource limiter for WASM instances
struct PluginResourceLimiter {
    memory_limit: u64,
}

impl ResourceLimiter for PluginResourceLimiter {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> Result<bool> {
        if desired as u64 > self.memory_limit {
            Ok(false) // Deny allocation
        } else {
            Ok(true) // Allow allocation
        }
    }

    fn table_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> Result<bool> {
        // Limit table size to prevent DoS
        Ok(desired <= 10000)
    }
}

/// Host state for WASM plugin
struct PluginHostState {
    wasi: WasiP1Ctx,
    limiter: PluginResourceLimiter,
}

/// WASM plugin instance with sandboxed execution
pub struct WasmPlugin {
    engine: Engine,
    module: Module,
    linker: Linker<PluginHostState>,
    config: WasmConfig,
    metadata: Option<PluginMetadata>,
    /// ADR-027 D10c — sanitised plugin identifier used as the
    /// per-plugin filesystem subdirectory name. Resolved at load
    /// time from `config.plugin_id` (required field, no
    /// fallback as of PR #1029 round-2 — same-filename plugins
    /// in different directories used to collide here). Always
    /// passes [`is_safe_plugin_id`] before reaching this field.
    plugin_id: String,
}

impl WasmPlugin {
    /// Load a WASM plugin from file
    ///
    /// This initializes the WASM runtime, loads the module, and sets up
    /// the sandboxed environment with configured resource limits and capabilities.
    ///
    /// If plugin signing is enabled and configured, this will verify the plugin's
    /// cryptographic signature before loading.
    pub async fn load(path: &Path, config: WasmConfig) -> Result<Self, EngineError> {
        // Verify plugin signature if signing feature is enabled
        #[cfg(feature = "plugin-signing")]
        {
            use crate::plugin::key_rotation::{
                Fingerprint, fingerprint_of, resolve_active_signing_key,
            };
            use crate::plugin::revocation::{
                load_revoked_fingerprints, load_revoked_fingerprints_from,
            };
            use crate::plugin::signing::{
                load_trusted_keys, load_trusted_keys_from, verify_plugin_signature,
            };
            use std::collections::HashSet;

            let sig_path = path.with_extension("wasm.sig");

            if sig_path.exists() {
                // Signature file exists - verify it
                let trusted_keys = if config.allow_self_signed {
                    // Allow any key (self-signed)
                    vec![] // Empty list means skip trust check in verify_plugin_signature
                } else if let Some(custom_path) = &config.trusted_keys_path {
                    // Caller supplied an explicit trust store — honour it
                    // (#1447 / #1596). Previously the field was silently
                    // ignored and the default location was always used.
                    load_trusted_keys_from(custom_path)?
                } else {
                    // Load trusted keys from the default location
                    load_trusted_keys()?
                };

                // ADR-027 D9 (CRL): load the revocation list. A signing key whose
                // fingerprint is listed is refused on EVERY signature path below —
                // rotation, bare trusted-key, and self-signed — so revocation can
                // never be bypassed by simply dropping the rotation manifest. A
                // malformed CRL is a hard error (fail-safe), surfaced here.
                let revoked: HashSet<Fingerprint> = match &config.revoked_keys_path {
                    Some(custom_path) => load_revoked_fingerprints_from(custom_path)?,
                    None => load_revoked_fingerprints()?,
                };

                // Up-front signer-key revocation check, common to all paths: read
                // the signer key from the sidecar and refuse immediately if it is
                // revoked. (The rotation path additionally hands `revoked` to the
                // chain validator below, so a revoked *predecessor* burns the whole
                // chain — not just the active signer.)
                if !revoked.is_empty() {
                    let sig_json = std::fs::read_to_string(&sig_path).map_err(|e| {
                        EngineError::PluginLoadFailed(format!("Failed to read signature: {}", e))
                    })?;
                    let sig_metadata: crate::plugin::signing::SignatureMetadata =
                        serde_json::from_str(&sig_json).map_err(|e| {
                            EngineError::PluginLoadFailed(format!(
                                "Invalid signature format: {}",
                                e
                            ))
                        })?;
                    if let Some(signer_fp) = hex::decode(&sig_metadata.public_key)
                        .ok()
                        .and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok())
                        .map(|pk| fingerprint_of(&pk))
                        && revoked.contains(&signer_fp)
                    {
                        return Err(EngineError::PluginLoadFailed(
                            "Plugin signing key is revoked (ADR-027 D9 CRL); refusing to load"
                                .to_string(),
                        ));
                    }
                }

                // Skip trust check if allow_self_signed is true by passing signature's own key
                if config.allow_self_signed {
                    // For self-signed mode, we still verify the signature is valid,
                    // but we extract the public key from the signature and trust it
                    let sig_json = std::fs::read_to_string(&sig_path).map_err(|e| {
                        EngineError::PluginLoadFailed(format!("Failed to read signature: {}", e))
                    })?;
                    let sig_metadata: crate::plugin::signing::SignatureMetadata =
                        serde_json::from_str(&sig_json).map_err(|e| {
                            EngineError::PluginLoadFailed(format!(
                                "Invalid signature format: {}",
                                e
                            ))
                        })?;

                    // Trust the key from the signature itself
                    verify_plugin_signature(path, &sig_path, &[sig_metadata.public_key])?;
                } else {
                    // Normal mode: require trusted keys.
                    //
                    // ADR-027 D9 — if a rotation manifest (`<plugin>.keys.json`)
                    // sits beside the plugin, the signing key may be a *rotated*
                    // successor the user never trusted directly. Validate the
                    // chain against the trusted roots and bind the signature to
                    // the key whose active window contains *now*, so trust flows
                    // transitively from the root yet only the currently-active
                    // key is accepted. A broken / untrusted chain — or a
                    // signature by a rotated-away-from predecessor key — is a
                    // HARD FAIL (degrade to "pinned, non-rotating"); never fall
                    // back to the bare trusted-key check.
                    let manifest_path = path.with_extension("keys.json");
                    if manifest_path.exists() {
                        let manifest_json =
                            std::fs::read_to_string(&manifest_path).map_err(|e| {
                                EngineError::PluginLoadFailed(format!(
                                    "Failed to read key rotation manifest {:?}: {}",
                                    manifest_path, e
                                ))
                            })?;

                        // Trusted roots = fingerprints of the user's directly
                        // trusted keys. Malformed entries are skipped (a bad
                        // trust-store line shouldn't crash the loader); an empty
                        // root set simply leaves the chain unanchored → hard fail.
                        let roots: HashSet<Fingerprint> = trusted_keys
                            .iter()
                            .filter_map(|k| hex::decode(k).ok())
                            .filter_map(|b| <[u8; 32]>::try_from(b.as_slice()).ok())
                            .map(|pk| fingerprint_of(&pk))
                            .collect();

                        // Bind to the key active at the verifier's clock — not
                        // the signature's forgeable `signed_at` — so a
                        // compromised predecessor key whose window has closed can
                        // never sign an accepted artifact. `revoked` is the loaded
                        // CRL: a revoked key anywhere in the chain (root → head)
                        // burns the whole chain. Anti-rollback high-water-mark
                        // persistence is not yet wired (ADR-027 D9 follow-up), so
                        // `None` for now.
                        let now_unix = chrono::Utc::now().timestamp();
                        let active_key = resolve_active_signing_key(
                            &manifest_json,
                            &roots,
                            &revoked,
                            None,
                            now_unix,
                        )
                        .map_err(|e| EngineError::PluginLoadFailed(e.to_string()))?;

                        verify_plugin_signature(path, &sig_path, &[active_key])?;
                    } else {
                        verify_plugin_signature(path, &sig_path, &trusted_keys)?;
                    }
                }
            } else if config.require_signature {
                // Signature required but not found
                return Err(EngineError::PluginLoadFailed(format!(
                    "Plugin signature required but not found: {:?}.sig",
                    path
                )));
            }
            // If signature file doesn't exist and not required, continue loading without verification
        }

        // Configure WASM engine. (wasmtime 45: async support is always on —
        // the former `async_support(true)` call is a no-op and was removed.)
        let mut engine_config = Config::new();
        engine_config.wasm_component_model(false); // Use core WASM for now
        engine_config.consume_fuel(true); // Enable execution metering

        let engine = Engine::new(&engine_config)
            .map_err(|e| EngineError::PluginLoadFailed(e.to_string()))?;

        // Load WASM module
        let module = Module::from_file(&engine, path).map_err(|e| {
            EngineError::PluginLoadFailed(format!("Failed to load WASM module: {}", e))
        })?;

        // Create linker for WASI functions (using preview1 for core modules)
        let mut linker = Linker::new(&engine);
        wasmtime_wasi::p1::add_to_linker_async(&mut linker, |state: &mut PluginHostState| {
            &mut state.wasi
        })
        .map_err(|e| EngineError::PluginLoadFailed(format!("Failed to setup WASI: {}", e)))?;

        // ADR-027 D10c — validate the per-plugin id (required;
        // see WasmConfig.plugin_id docstring) before it's used as
        // a filesystem subdirectory name. There is no file-stem
        // fallback (PR #1029 round-2 dropped it because two
        // plugins with the same filename in different
        // directories would have collided into a shared sandbox).
        let plugin_id = resolve_plugin_id(&config.plugin_id, path)?;

        Ok(WasmPlugin {
            engine,
            module,
            linker,
            config,
            metadata: None,
            plugin_id,
        })
    }

    /// Initialize plugin and retrieve metadata
    ///
    /// This calls the `init` export to get plugin metadata (name, version, etc.)
    pub async fn init(&mut self) -> Result<PluginMetadata, EngineError> {
        let mut store = self.create_store()?;

        // Instantiate module
        let instance = self
            .linker
            .instantiate_async(&mut store, &self.module)
            .await
            .map_err(|e| EngineError::PluginLoadFailed(format!("Failed to instantiate: {}", e)))?;

        // Call init() to get metadata
        // Note: init() returns u64 with ptr in high 32 bits, len in low 32 bits
        let init_func = instance
            .get_typed_func::<(), u64>(&mut store, "init")
            .map_err(|e| EngineError::PluginLoadFailed(format!("Missing init export: {}", e)))?;

        // Enforce the same timeout `execute()` uses — `init()` is plugin-
        // controlled code and would otherwise hang plugin loading
        // indefinitely on a spinning init (#1596 / #1445 follow-up).
        let init_timeout = self.config.max_execution_time;
        let packed = tokio::time::timeout(init_timeout, init_func.call_async(&mut store, ()))
            .await
            .map_err(|_| {
                EngineError::PluginLoadFailed(format!(
                    "init() timeout ({}s)",
                    init_timeout.as_secs()
                ))
            })?
            .map_err(|e| EngineError::PluginLoadFailed(format!("init() failed: {}", e)))?;

        // Unpack ptr and len from u64
        let ptr = (packed >> 32) as u32;
        let len = (packed & 0xFFFFFFFF) as u32;

        // Read metadata from WASM memory
        let metadata_json = self.read_string_from_memory(&instance, &mut store, ptr, len)?;

        let metadata: PluginMetadata = serde_json::from_str(&metadata_json)
            .map_err(|e| EngineError::PluginLoadFailed(format!("Invalid metadata: {}", e)))?;

        self.metadata = Some(metadata.clone());
        Ok(metadata)
    }

    /// Execute plugin action
    ///
    /// This calls the `execute` export with the action name, positional
    /// `parameters`, and trigger context, enforcing timeout and resource
    /// limits. `parameters` are forwarded to the guest's
    /// `ActionRequest.parameters` field (#1446) — pass an empty slice for
    /// parameter-less actions.
    pub async fn execute(
        &self,
        action: &str,
        parameters: &[String],
        context: &TriggerContext,
    ) -> Result<(), EngineError> {
        let mut store = self.create_store()?;

        // Instantiate module
        let instance = self
            .linker
            .instantiate_async(&mut store, &self.module)
            .await
            .map_err(|e| {
                EngineError::PluginExecutionFailed(format!("Failed to instantiate: {}", e))
            })?;

        // Serialize request
        let request = ActionRequest {
            action: action.to_string(),
            context: context.clone(),
            parameters: parameters.to_vec(),
        };
        let request_json = serde_json::to_string(&request).map_err(|e| {
            EngineError::PluginExecutionFailed(format!("Serialization failed: {}", e))
        })?;

        // Write request to WASM memory
        let (ptr, len) = self
            .write_string_to_memory(&instance, &mut store, &request_json)
            .await?;

        // Call execute(ptr, len) -> result_code
        let execute_func = instance
            .get_typed_func::<(u32, u32), i32>(&mut store, "execute")
            .map_err(|e| {
                EngineError::PluginExecutionFailed(format!("Missing execute export: {}", e))
            })?;

        // Execute with timeout
        let timeout = self.config.max_execution_time;
        let result = tokio::time::timeout(timeout, execute_func.call_async(&mut store, (ptr, len)))
            .await
            .map_err(|_| {
                EngineError::PluginExecutionFailed(format!(
                    "Execution timeout ({}s)",
                    timeout.as_secs()
                ))
            })?
            .map_err(|e| EngineError::PluginExecutionFailed(format!("Execution failed: {}", e)))?;

        if result != 0 {
            return Err(EngineError::PluginExecutionFailed(format!(
                "Plugin returned error code: {}",
                result
            )));
        }

        Ok(())
    }

    /// Get plugin metadata
    pub fn metadata(&self) -> Option<&PluginMetadata> {
        self.metadata.as_ref()
    }

    /// Update the capability set this plugin runs with.
    ///
    /// PR #1029 round-8 review (2026-05-02): the round-7
    /// change passed the manager's *granted* capability set
    /// into `WasmConfig` at load time, which fixed
    /// "init() runs with full manifest set" but turned the
    /// capability list into a one-time snapshot. The
    /// `PluginManager` API contracts `grant_capability` /
    /// `revoke_capability` to take effect at runtime; without
    /// a setter, those calls only update
    /// `ManagedPlugin.granted_capabilities` but never reach
    /// the WASI context, so post-load grants are no-ops.
    ///
    /// `create_store` reads from `self.config.capabilities`,
    /// so updating that field via this setter makes the next
    /// `init()` / `execute()` call use the updated set. The
    /// manager calls this from `grant_capability` /
    /// `revoke_capability` to keep the runtime view in sync.
    pub fn set_capabilities(&mut self, capabilities: Vec<Capability>) {
        self.config.capabilities = capabilities;
    }

    // --- Private helper methods ---

    /// Create a new store with WASI context and resource limits
    fn create_store(&self) -> Result<Store<PluginHostState>, EngineError> {
        // Build WASI context with capabilities.
        //
        // We deliberately do NOT call `inherit_stdio()` or `inherit_args()`:
        // a sandboxed plugin must not have ambient access to the daemon's
        // stdin/stdout/stderr or process arguments (#1596 / #1445 follow-up).
        // WasiCtxBuilder defaults to null stdio, which is what we want.
        let mut wasi_builder = WasiCtxBuilder::new();

        // Grant filesystem access if capability is present
        if self.config.capabilities.contains(&Capability::Filesystem) {
            // ADR-027 D10c — per-plugin data subdirectory under the
            // shared plugin-data root, NOT the shared root itself.
            // Pre-D10c every plugin with the Filesystem capability
            // got `<data>/conductor/plugin-data/` mounted at `/` in
            // its WASI fs, which let any such plugin read or
            // overwrite any sibling plugin's data. Now each plugin
            // gets its own subdir scoped to its sanitised id, so a
            // misbehaving (or compromised) plugin can only damage
            // its own data — wasmtime-wasi enforces preopen-rooted
            // path access and rejects `..` traversal beyond the
            // mounted directory.
            let plugin_data_root = dirs::data_dir()
                .ok_or_else(|| EngineError::PluginLoadFailed("No data directory".to_string()))?
                .join("conductor")
                .join("plugin-data");
            let plugin_data_dir = plugin_data_subdir(&self.plugin_id, &plugin_data_root)?;

            std::fs::create_dir_all(&plugin_data_dir).map_err(|e| {
                EngineError::PluginLoadFailed(format!("Failed to create plugin data dir: {}", e))
            })?;

            // Preopen directory with read/write access (wasmtime v26 API)
            // This allows the plugin to access only this specific directory
            use wasmtime_wasi::DirPerms;
            use wasmtime_wasi::FilePerms;

            let dir_perms = DirPerms::all();
            let file_perms = FilePerms::all();

            wasi_builder
                .preopened_dir(
                    plugin_data_dir,
                    "/", // Mount at root of WASI filesystem
                    dir_perms,
                    file_perms,
                )
                .map_err(|e| {
                    EngineError::PluginLoadFailed(format!("Failed to preopen directory: {}", e))
                })?;
        }

        // Capability::Network — opt-in to host network access (#1600).
        //
        // The previous comment here claimed network was "implicit in WASI",
        // but wasmtime-wasi's default is no host network (no inherited
        // sockets, no IP name lookup). Without this gate, the capability
        // was declarative only — granted or not, plugins got the same
        // (no-network) sandbox. Now `Capability::Network` actually maps
        // to inherited sockets + name resolution; absence keeps the
        // wasmtime-wasi default of no host network.
        if self.config.capabilities.contains(&Capability::Network) {
            wasi_builder.inherit_network();
            wasi_builder.allow_ip_name_lookup(true);
        }

        let wasi_ctx = wasi_builder.build_p1();
        let limiter = PluginResourceLimiter {
            memory_limit: self.config.max_memory_bytes,
        };
        let host_state = PluginHostState {
            wasi: wasi_ctx,
            limiter,
        };
        let mut store = Store::new(&self.engine, host_state);

        // Set resource limiter (wasmtime v26 API)
        // This enforces memory and table growth limits to prevent DoS attacks
        store.limiter(|state| &mut state.limiter);

        // Set fuel limit (instruction count limit from config)
        // 1 fuel ≈ 1 WASM instruction
        // NOTE: Fuel must be enabled in Config before creating engine (done in load())
        store
            .set_fuel(self.config.max_fuel)
            .map_err(|e| EngineError::PluginLoadFailed(format!("Failed to set fuel: {}", e)))?;

        Ok(store)
    }

    /// Write string to WASM linear memory
    async fn write_string_to_memory(
        &self,
        instance: &Instance,
        store: &mut Store<PluginHostState>,
        data: &str,
    ) -> Result<(u32, u32), EngineError> {
        let memory = instance
            .get_memory(&mut *store, "memory")
            .ok_or_else(|| EngineError::PluginExecutionFailed("No memory export".to_string()))?;

        // Allocate memory in WASM
        let alloc_func = instance
            .get_typed_func::<u32, u32>(&mut *store, "alloc")
            .map_err(|_| EngineError::PluginExecutionFailed("No alloc export".to_string()))?;

        let len = data.len() as u32;
        let ptr = alloc_func
            .call_async(&mut *store, len)
            .await
            .map_err(|e| EngineError::PluginExecutionFailed(format!("Allocation failed: {}", e)))?;

        // Write data to memory
        memory
            .write(&mut *store, ptr as usize, data.as_bytes())
            .map_err(|e| {
                EngineError::PluginExecutionFailed(format!("Memory write failed: {}", e))
            })?;

        Ok((ptr, len))
    }

    /// Read string from WASM linear memory
    fn read_string_from_memory(
        &self,
        instance: &Instance,
        store: &mut Store<PluginHostState>,
        ptr: u32,
        len: u32,
    ) -> Result<String, EngineError> {
        let memory = instance
            .get_memory(&mut *store, "memory")
            .ok_or_else(|| EngineError::PluginExecutionFailed("No memory export".to_string()))?;

        // Validate the plugin-controlled (ptr, len) against the actual WASM
        // memory size and a metadata cap BEFORE allocating a host buffer.
        // The WASM memory limiter bounds only *guest* memory growth, so a
        // malicious len (e.g. u32::MAX) would otherwise force a multi-GB
        // host allocation here (#1445).
        let (ptr, len) = validate_plugin_range(ptr, len, memory.data_size(&*store))?;

        let mut buffer = vec![0u8; len];
        memory.read(&*store, ptr, &mut buffer).map_err(|e| {
            EngineError::PluginExecutionFailed(format!("Memory read failed: {}", e))
        })?;

        String::from_utf8(buffer)
            .map_err(|e| EngineError::PluginExecutionFailed(format!("Invalid UTF-8: {}", e)))
    }
}

/// Maximum byte length the host will read out of WASM linear memory for a
/// single plugin string (metadata JSON). Generous for metadata — the point
/// is only to keep a malicious length from forcing an unbounded allocation.
const MAX_PLUGIN_STRING_BYTES: usize = 1024 * 1024;

/// Validate a plugin-supplied `(ptr, len)` range before the host allocates a
/// buffer for it (#1445).
///
/// `init()` returns a packed ptr/len fully controlled by the plugin. The WASM
/// memory limiter bounds only *guest* memory, so without this check a plugin
/// returning `len = u32::MAX` would force a multi-GB host `Vec` allocation
/// before `Memory::read` ever rejects the out-of-bounds range.
///
/// Returns the range as `usize` on success; rejects an oversized length, a
/// `ptr + len` overflow, or a range past the end of WASM memory.
fn validate_plugin_range(
    ptr: u32,
    len: u32,
    memory_size: usize,
) -> Result<(usize, usize), EngineError> {
    let (ptr, len) = (ptr as usize, len as usize);
    if len > MAX_PLUGIN_STRING_BYTES {
        return Err(EngineError::PluginLoadFailed(format!(
            "Plugin string length {len} exceeds the {MAX_PLUGIN_STRING_BYTES}-byte limit"
        )));
    }
    // On 64-bit hosts ptr + len (both <= u32::MAX, len further capped above)
    // cannot overflow usize; checked_add is the portable guard for 32-bit.
    let end = ptr.checked_add(len).ok_or_else(|| {
        EngineError::PluginLoadFailed("Plugin string range overflows usize".to_string())
    })?;
    if end > memory_size {
        return Err(EngineError::PluginLoadFailed(format!(
            "Plugin string range {ptr}..{end} exceeds WASM memory size {memory_size}"
        )));
    }
    Ok((ptr, len))
}

/// Request structure for plugin execution
#[derive(Debug, Serialize, Deserialize)]
struct ActionRequest {
    pub action: String,
    pub context: TriggerContext,
    /// Positional parameters for the action (#1446). Every bundled WASM
    /// guest deserializes a `parameters: Vec<String>` field (OBS
    /// switch_scene → `["scene"]`, Spotify set_volume → `["50"]`, …) with
    /// `#[serde(default)]`. The host previously never serialized this field,
    /// so parameterized actions always saw an empty list and failed. Always
    /// emitted (empty for parameter-less actions) to match that wire shape.
    #[serde(default)]
    pub parameters: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_plugin_range_accepts_an_in_bounds_range() {
        assert_eq!(
            validate_plugin_range(0, 64, 65_536).expect("in-bounds range"),
            (0, 64)
        );
        assert_eq!(
            validate_plugin_range(100, 200, 65_536).expect("in-bounds range"),
            (100, 200)
        );
    }

    #[test]
    fn validate_plugin_range_rejects_an_oversized_length() {
        // A malicious init() returning len = u32::MAX must be rejected
        // before any host allocation (#1445).
        let err = validate_plugin_range(0, u32::MAX, 65_536).unwrap_err();
        assert!(matches!(err, EngineError::PluginLoadFailed(_)), "{err:?}");
    }

    #[test]
    fn validate_plugin_range_rejects_a_range_past_memory_end() {
        // Length is within the cap, but ptr + len runs off the end of the
        // exported WASM memory.
        let err = validate_plugin_range(65_000, 1_000, 65_536).unwrap_err();
        assert!(matches!(err, EngineError::PluginLoadFailed(_)), "{err:?}");
    }

    #[test]
    fn validate_plugin_range_cap_is_inclusive() {
        // 2 MiB of WASM memory — room for a cap-sized range.
        let big_memory = 2 * 1024 * 1024;
        // Exactly at the cap is accepted (the check is `len > cap`)...
        assert_eq!(
            validate_plugin_range(0, MAX_PLUGIN_STRING_BYTES as u32, big_memory)
                .expect("cap-sized range"),
            (0, MAX_PLUGIN_STRING_BYTES)
        );
        // ...one byte over the cap is rejected.
        let err =
            validate_plugin_range(0, MAX_PLUGIN_STRING_BYTES as u32 + 1, big_memory).unwrap_err();
        assert!(matches!(err, EngineError::PluginLoadFailed(_)), "{err:?}");
    }

    #[test]
    fn test_wasm_config_new_uses_default_field_values() {
        // PR #1029 round-3: `Default` impl removed; `new(id)` is
        // the only constructor and seeds the conservative
        // defaults the old `Default` produced.
        let config = WasmConfig::new("test-plugin").expect("safe id");
        assert_eq!(config.max_memory_bytes, 128 * 1024 * 1024);
        assert_eq!(config.max_execution_time, Duration::from_secs(5));
        assert!(config.capabilities.is_empty());
        assert_eq!(config.plugin_id, "test-plugin");
    }

    #[test]
    fn wasm_config_new_rejects_unsafe_plugin_id() {
        // PR #1029 round-7: construction-time validation. An
        // unsafe id is rejected immediately rather than waiting
        // for `WasmPlugin::load` to fail at runtime.
        for unsafe_id in ["..", "../escape", "/abs", "name with spaces", ""] {
            let result = WasmConfig::new(unsafe_id);
            assert!(
                result.is_err(),
                "WasmConfig::new({unsafe_id:?}) must reject unsafe id at \
                 construction time; got {:?}",
                result.map(|c| c.plugin_id),
            );
        }
    }

    #[test]
    fn test_resource_limiter() {
        let mut limiter = PluginResourceLimiter {
            memory_limit: 1024 * 1024, // 1 MB
        };

        // Should allow allocations under limit
        assert!(limiter.memory_growing(0, 512 * 1024, None).unwrap());

        // Should deny allocations over limit
        assert!(!limiter.memory_growing(0, 2 * 1024 * 1024, None).unwrap());
    }

    // ─── #1557: NEGATIVE resource-limit enforcement ────────────────
    // The resource_limiting_test.rs integration suite only asserts that normal
    // plugins succeed within adequate limits — a regression that stopped
    // enforcing a limit would still pass it. These tests assert the ENFORCEMENT
    // path directly: over-limit memory/table growth is DENIED, and a runaway
    // loop TRAPS once its fuel budget is spent. They are deterministic and need
    // no built WASM fixtures, so they run in CI under `--features plugin-wasm`.

    #[test]
    fn resource_limiter_denies_memory_growth_beyond_limit() {
        let mut limiter = PluginResourceLimiter {
            memory_limit: 1024 * 1024, // 1 MiB
        };

        // Boundary: exactly at the limit is allowed, one byte over is denied.
        assert!(
            limiter.memory_growing(0, 1024 * 1024, None).unwrap(),
            "growth to exactly the limit must be allowed"
        );
        assert!(
            !limiter.memory_growing(0, 1024 * 1024 + 1, None).unwrap(),
            "growth ONE byte over the limit must be denied"
        );
        assert!(
            !limiter.memory_growing(0, 64 * 1024 * 1024, None).unwrap(),
            "a large over-limit request must be denied"
        );
    }

    #[test]
    fn resource_limiter_denies_table_growth_beyond_cap() {
        let mut limiter = PluginResourceLimiter {
            memory_limit: 128 * 1024 * 1024,
        };

        // The 10_000-element table cap guards against table-growth DoS and was
        // previously unasserted entirely.
        assert!(
            limiter.table_growing(0, 10_000, None).unwrap(),
            "table growth up to the cap must be allowed"
        );
        assert!(
            !limiter.table_growing(0, 10_001, None).unwrap(),
            "table growth ONE element past the cap must be denied"
        );
        assert!(
            !limiter.table_growing(0, 1_000_000, None).unwrap(),
            "a large over-cap table request must be denied"
        );
    }

    #[test]
    fn fuel_metering_traps_runaway_loop() {
        // Prove fuel enforcement at the wasmtime layer WasmPlugin configures:
        // `consume_fuel(true)` + `set_fuel(budget)`. A module that loops far
        // more than the budget must TRAP with out-of-fuel rather than run to
        // completion. A regression that stopped calling `set_fuel`, or disabled
        // metering, would let this run to completion (or spin forever).
        let mut engine_config = Config::new();
        engine_config.consume_fuel(true); // the same metering switch as load()
        let engine = Engine::new(&engine_config).expect("engine");

        // ~3 instructions/iteration × 100M iterations — vastly more than the
        // 10k-fuel budget below, so it cannot finish before the budget runs out.
        let wat = r#"
            (module
              (func (export "burn")
                (local $i i32)
                (local.set $i (i32.const 100000000))
                (loop $l
                  (local.set $i (i32.sub (local.get $i) (i32.const 1)))
                  (br_if $l (local.get $i)))))
        "#;
        let module = Module::new(&engine, wat).expect("compile wat");

        let mut store = Store::new(&engine, ());
        store.set_fuel(10_000).expect("set fuel budget");

        let instance = Instance::new(&mut store, &module, &[]).expect("instantiate");
        let burn = instance
            .get_typed_func::<(), ()>(&mut store, "burn")
            .expect("burn export");

        let err = burn
            .call(&mut store, ())
            .expect_err("a runaway loop must exhaust the fuel budget and trap");

        // The trap must SPECIFICALLY be out-of-fuel, not some unrelated error.
        assert!(
            matches!(err.downcast_ref::<Trap>(), Some(Trap::OutOfFuel)),
            "expected an out-of-fuel trap; got {err:?}"
        );

        // And the budget really was fully consumed.
        assert_eq!(
            store.get_fuel().expect("fuel readable"),
            0,
            "all fuel should be spent when the out-of-fuel trap fires"
        );
    }

    // ─── ADR-027 D10c — per-plugin filesystem scope ────────────────

    #[test]
    fn is_safe_plugin_id_accepts_normal_names() {
        // Character set is the same as
        // plugin_registry::validate_plugin_id (PR #1029 round-2):
        // ASCII alphanumerics + `_` + `-`. NOT `.` — versioned
        // ids belong in the manifest's `version` field, not in
        // the plugin id used for filesystem scoping. This
        // alignment keeps install-time and runtime validators
        // from diverging.
        for id in [
            "spotify",
            "obs-control",
            "system_utils",
            "Plugin42",
            "a", // single char is fine
        ] {
            assert!(
                is_safe_plugin_id(id),
                "ADR-027 D10c: `{id}` should be a valid plugin id",
            );
        }
    }

    #[test]
    fn is_safe_plugin_id_rejects_path_traversal_attempts() {
        // Each of these would let a manifest-controlled id escape
        // the per-plugin sandbox via `..`, absolute paths, or
        // separators. The whole point of D10c is that this is
        // impossible — these MUST be rejected at the helper layer
        // before any path join.
        for id in [
            "..",
            ".",
            "../other-plugin",
            "../../../etc/passwd",
            "/absolute/path",
            "plugin/with/slashes",
            "windows\\style",
            ".hidden",           // dotfile prefix (`.` not in allowlist)
            "midi-looper-1.2.3", // PR #1029 round-2: `.` no longer allowed
            "",                  // empty
            "name with spaces",
            "name\twith\ttab",
            "name\nwith\nnewline",
            "name;with;semicolons",
            "name|pipe",
            "name`backtick`",
            "name$dollar",
        ] {
            assert!(
                !is_safe_plugin_id(id),
                "ADR-027 D10c: `{id:?}` MUST be rejected — would let a \
                 hostile manifest escape the per-plugin sandbox",
            );
        }
    }

    #[test]
    fn is_safe_plugin_id_rejects_overly_long_names() {
        // Pathological-length ids could cause issues at filesystem
        // layer; cap at 128 chars. 128 itself is fine; 129 isn't.
        let exactly_128: String = "a".repeat(128);
        let too_long: String = "a".repeat(129);
        assert!(is_safe_plugin_id(&exactly_128));
        assert!(!is_safe_plugin_id(&too_long));
    }

    #[test]
    fn plugin_data_subdir_joins_safe_id() {
        let root = std::path::PathBuf::from("/tmp/conductor/plugin-data");
        let got = plugin_data_subdir("spotify", &root).expect("safe id");
        assert_eq!(got, root.join("spotify"));
    }

    #[test]
    fn plugin_data_subdir_rejects_unsafe_id() {
        // Defence-in-depth: even if a caller bypassed the safe-id
        // check elsewhere, plugin_data_subdir refuses to do the
        // join. Any caller that wants the path MUST go through this
        // helper — the wasm_runtime preopen path does.
        let root = std::path::PathBuf::from("/tmp/conductor/plugin-data");
        for unsafe_id in ["../escape", "/abs", "..", "."] {
            let result = plugin_data_subdir(unsafe_id, &root);
            assert!(
                result.is_err(),
                "ADR-027 D10c: plugin_data_subdir({unsafe_id:?}) should \
                 return Err to prevent path-traversal; got {result:?}",
            );
        }
    }

    #[test]
    fn resolve_plugin_id_returns_safe_id() {
        let path = std::path::PathBuf::from("/tmp/anywhere/spotify.wasm");
        let id = resolve_plugin_id("custom-id", &path).expect("safe id");
        assert_eq!(id, "custom-id");
    }

    #[test]
    fn resolve_plugin_id_errors_when_unsafe() {
        // Hostile manifest sets an unsafe plugin_id; round-2
        // dropped the file-stem fallback (it collapsed two
        // plugins with the same filename in different
        // directories into a shared sandbox); round-3 promoted
        // the invariant to the type system. Reaching this code
        // path means a caller bypassed the type's validation
        // somehow (set the field directly to an unsafe value);
        // we still defend in depth with a hard error.
        let path = std::path::PathBuf::from("/tmp/plugins/legit.wasm");
        let result = resolve_plugin_id("../escape", &path);
        assert!(
            result.is_err(),
            "ADR-027 D10c: an unsafe plugin_id must error rather \
             than be silently translated into a filesystem path \
             component. Got: {result:?}",
        );
    }

    #[test]
    fn resolve_plugin_id_errors_when_empty() {
        // Same defense-in-depth case: a caller could in theory
        // set `WasmConfig.plugin_id = String::new()` directly;
        // resolve_plugin_id refuses.
        let path = std::path::PathBuf::from("/tmp/plugins/legit.wasm");
        let result = resolve_plugin_id("", &path);
        assert!(
            result.is_err(),
            "empty plugin_id must error; got {result:?}",
        );
    }

    #[test]
    fn resolve_plugin_id_distinct_for_same_filename_different_paths() {
        // PR #1029 review regression test: two plugins loaded
        // from `/tmp/a/plugin.wasm` and `/opt/b/plugin.wasm`
        // (same filename, different directories) MUST get
        // distinct sandbox subdirs — otherwise they'd share a
        // filesystem sandbox.
        //
        // Round-3 (this PR): the type system now requires an
        // explicit `plugin_id` on `WasmConfig`, eliminating any
        // possibility of path-stem-derived collisions.
        // resolve_plugin_id never sees the file path as a fallback
        // source.
        let path_a = std::path::PathBuf::from("/tmp/a/plugin.wasm");
        let path_b = std::path::PathBuf::from("/opt/b/plugin.wasm");
        let id_a = resolve_plugin_id("plugin-a", &path_a).expect("safe");
        let id_b = resolve_plugin_id("plugin-b", &path_b).expect("safe");
        assert_ne!(
            id_a, id_b,
            "distinct explicit ids must produce distinct results",
        );
    }

    // ─── builder pattern + non_exhaustive (PR #1029 round-2/3) ────

    #[test]
    fn wasm_config_new_sets_plugin_id() {
        let config = WasmConfig::new("spotify").expect("safe id");
        assert_eq!(config.plugin_id, "spotify");
    }

    #[test]
    fn wasm_config_builder_chains() {
        let config = WasmConfig::new("obs-control")
            .expect("safe id")
            .with_max_memory_bytes(64 * 1024 * 1024)
            .with_max_fuel(50_000_000)
            .with_max_execution_time(Duration::from_secs(2));
        assert_eq!(config.plugin_id, "obs-control");
        assert_eq!(config.max_memory_bytes, 64 * 1024 * 1024);
        assert_eq!(config.max_fuel, 50_000_000);
        assert_eq!(config.max_execution_time, Duration::from_secs(2));
    }

    /// #1446 regression. The host wire format MUST carry `parameters` so the
    /// guest's `parameters: Vec<String>` field is populated. Pre-fix the host
    /// `ActionRequest` only serialized `action` + `context`, so every guest
    /// saw `parameters: []` and parameterized actions (OBS switch_scene,
    /// Spotify set_volume, …) failed.
    #[test]
    fn action_request_serializes_positional_parameters() {
        let request = ActionRequest {
            action: "switch_scene".to_string(),
            context: TriggerContext::default(),
            parameters: vec!["Scene 1".to_string(), "extra".to_string()],
        };
        let json = serde_json::to_string(&request).expect("serialize");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert_eq!(
            value["parameters"],
            serde_json::json!(["Scene 1", "extra"]),
            "host must serialize positional `parameters` for the guest (#1446); got {json}"
        );

        // Round-trips into the exact shape every bundled guest deserializes.
        #[derive(serde::Deserialize)]
        struct GuestActionRequest {
            action: String,
            #[serde(default)]
            parameters: Vec<String>,
        }
        let guest: GuestActionRequest =
            serde_json::from_str(&json).expect("guest-shape deserialize");
        assert_eq!(guest.action, "switch_scene");
        assert_eq!(guest.parameters, vec!["Scene 1", "extra"]);
    }

    /// Backward/forward compatible: a request JSON that omits `parameters`
    /// still deserializes (the field defaults to empty), so parameter-less
    /// actions and any older wire payloads keep working.
    #[test]
    fn action_request_parameters_default_empty_when_omitted() {
        let ctx_json = serde_json::to_string(&TriggerContext::default()).expect("ctx json");
        let json = format!(r#"{{"action":"play","context":{ctx_json}}}"#);
        let request: ActionRequest =
            serde_json::from_str(&json).expect("deserialize request without parameters");
        assert!(
            request.parameters.is_empty(),
            "a request without `parameters` must default to an empty list (#1446)"
        );
    }
}
