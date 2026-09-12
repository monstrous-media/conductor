// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Root config family: Config, ConfigMeta, sources, security blocks.

use super::*;

/// Top-level configuration structure
///
/// Contains mode definitions, global mappings, I/O endpoints, routes, and
/// logging configuration.
///
/// ## Unified I/O (ADR-035)
///
/// Devices and connectors are defined as a single `[[endpoints]]` array. The
/// legacy `[device]` / `[[bindings]]` / `[[connectors]]` blocks were removed in
/// ADR-035 (no migration path — `[[endpoints]]` is the only authored form).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    /// List of mapping modes (each with its own set of mappings)
    pub modes: Vec<Mode>,
    /// Global mappings that work in all modes (applied before mode-specific mappings)
    #[serde(default)]
    pub global_mappings: Vec<Mapping>,
    /// Logging configuration
    #[serde(default)]
    pub logging: Option<LoggingConfig>,
    /// Advanced settings for event processing
    #[serde(default)]
    pub advanced_settings: AdvancedSettings,
    /// Last selected mode name in the GUI (persists across app restarts)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_selected_mode: Option<String>,
    /// Default startup mode (daemon starts in this mode instead of index 0)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_mode: Option<String>,
    /// LED feedback configuration
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub led: Option<LedConfig>,
    /// Event console configuration
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_console: Option<EventConsoleConfig>,
    /// Per-app mode auto-switching (ADR-040 D3/D5). Symmetric to
    /// `[per_app_profiles]` but switches the active *mode* (lightweight)
    /// rather than reloading a whole profile. Purely additive; absent ⇒ no
    /// auto-switching. The daemon resolver/poller land in later slices.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_app_modes: Option<PerAppModes>,
    /// Unified I/O endpoints (ADR-035). Preferred over `bindings`/`connectors`,
    /// which are lowered into this set at load. Authored entries
    /// go through the strict hand-written [`EndpointConfig`] deserializer.
    #[serde(default)]
    pub endpoints: Vec<EndpointConfig>,

    /// Signal routes between connectors (ADR-031 D2 / Phase 2).
    /// Routes operate below the mapping engine (stage 9 of the
    /// 8-stage matcher) — unmatched events flow through routes if one
    /// exists for the source connector/binding. Mode-independent;
    /// fan-out by default. See ADR-031 spec § 4.1.
    #[serde(default)]
    pub routes: Vec<RouteConfig>,

    /// Security policy (ADR-027). Currently carries the shell-action
    /// sandbox toggle (§D10b); absent ⇒ defaults (sandbox enforced where
    /// the OS supports it, unsandboxed spawns allowed elsewhere).
    ///
    /// `skip_serializing_if` when default keeps the canonical form (and thus
    /// every `ConfigRevision`) byte-identical for configs that don't set
    /// `[security]` — a default block must not silently shift content hashes.
    #[serde(default, skip_serializing_if = "SecurityConfig::is_default")]
    pub security: SecurityConfig,

    /// ADR-034 `[config]` metadata block — config source mode (§D7) and
    /// the external-write policy for `user.toml` (§D9, ConfigWatcher
    /// demotion).
    ///
    /// `skip_serializing_if` when default keeps the canonical form (and
    /// thus every `ConfigRevision`) byte-identical for configs that don't
    /// author a `[config]` block — a default block must not silently shift
    /// content hashes (mirrors the `[security]` precedent above).
    #[serde(
        default,
        rename = "config",
        skip_serializing_if = "ConfigMeta::is_default"
    )]
    pub config_meta: ConfigMeta,

    /// ADR-045 D4 `[mcp]` block — runtime toggle for binding the
    /// (read-only) MCP socket. Even inspection-only MCP is a local socket
    /// surface; `enabled = false` leaves it unbound entirely (ADR-027
    /// minimal-surface posture). Default ON.
    ///
    /// `skip_serializing_if` when default keeps the canonical form (and
    /// thus every `ConfigRevision`) byte-identical for configs that don't
    /// author an `[mcp]` block (mirrors the `[security]` precedent above).
    #[serde(default, skip_serializing_if = "McpConfig::is_default")]
    pub mcp: McpConfig,
}

/// `[mcp]` — runtime MCP socket toggle (ADR-045 D4).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct McpConfig {
    /// Bind the MCP Unix socket at daemon startup. Default `true`; takes
    /// effect at startup (toggling requires a daemon restart — when
    /// disabled the socket is never bound, not merely refused).
    #[serde(default = "default_mcp_enabled")]
    pub enabled: bool,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            enabled: default_mcp_enabled(),
        }
    }
}

impl McpConfig {
    /// True when every field carries its default (serde skip helper).
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

fn default_mcp_enabled() -> bool {
    true
}

/// `[per_app_modes]` — mode auto-switching by frontmost app / window title
/// (ADR-040 D3/D5). Symmetric to `[per_app_profiles]` but lightweight: it
/// switches the active *mode*, not the whole config. Resolution precedence
/// (manual lock > window-title > app-name > default) and the title poller
/// land in later slices; this is the schema + validation only.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct PerAppModes {
    /// Mode when no rule matches. Falls back to the first `[[modes]]` if absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// App-name → mode. Lowest specificity above `default`.
    #[serde(default)]
    pub rules: std::collections::HashMap<String, String>,
    /// Window-title rules (higher specificity than `rules`).
    #[serde(default)]
    pub window_rules: Vec<WindowRule>,
    /// Privacy (ADR-040 §4.1/§4.3): when false (default), window titles are
    /// masked in logs (`<title:len=N>`). Set true only to debug raw titles.
    #[serde(default)]
    pub log_titles: bool,
}

/// A single window-title rule inside `[per_app_modes]` (ADR-040 D5).
///
/// `title_pattern` (glob, default) and `title_regex` (power users) are
/// mutually exclusive; a rule with neither is an app-only fallback. The
/// regex is validated at config load.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WindowRule {
    /// App name this rule applies to.
    pub app: String,
    /// Glob pattern on the window title. Mutually exclusive with `title_regex`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_pattern: Option<String>,
    /// Regex on the window title (power users). Validated at config load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_regex: Option<String>,
    /// Mode to switch to when this rule matches.
    pub mode: String,
}

/// ADR-034 `[config]` metadata block.
///
/// Distinct from the runtime [`Provenance`](crate::config::Provenance)
/// `Source` enum — this is the *authored* TOML section that tells the
/// daemon how to source its live config (§D7) and how to treat external
/// writes to `user.toml` (§D9). `schema_version` (§D7 migration) is
/// deferred; unknown keys are ignored (no `deny_unknown_fields`) so a
/// future `schema_version` does not break older daemons.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ConfigMeta {
    /// How the daemon sources its live config (§D7).
    #[serde(default)]
    pub source: ConfigSource,
    /// Policy for external writes to `user.toml` while running (§D9).
    #[serde(default)]
    pub user_file_policy: UserFilePolicy,
}

impl ConfigMeta {
    /// `true` when this equals the default block — used by
    /// `skip_serializing_if` so a default `[config]` block is omitted
    /// from the canonical form (preserving `ConfigRevision` stability).
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// ADR-034 §D7 — how the daemon sources its live config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigSource {
    /// Daemon owns the in-memory tree; external `user.toml` edits never
    /// reload silently — mutation flows only through authenticated IPC
    /// (ADR-034 default).
    #[default]
    Managed,
    /// Legacy pre-ADR-034 behaviour: external `user.toml` edits
    /// auto-reload. Deprecated; emits a per-reload warning and will be
    /// removed in a future release (§D4.E).
    File,
}

/// ADR-034 §D9 — policy for handling external writes to `user.toml`
/// while the daemon is running.
///
/// Precedence: [`Ignore`](UserFilePolicy::Ignore) is **authoritative over
/// [`ConfigSource`]** — it disables the watcher entirely (at startup, and the
/// runtime decision honours it even for legacy [`ConfigSource::File`]), so
/// "ignore" means the daemon never reacts to `user.toml` edits in ANY source
/// mode. Under [`Notify`](UserFilePolicy::Notify) the source mode then decides:
/// [`ConfigSource::Managed`] surfaces drift only, legacy [`ConfigSource::File`]
/// auto-reloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserFilePolicy {
    /// Watcher detects edits to `user.toml` and surfaces drift
    /// (`MonitorEvent::ConfigDriftDetected`) WITHOUT reloading. The live
    /// in-memory tree stays authoritative until an explicit IPC reload.
    #[default]
    Notify,
    /// Watcher disabled — external edits are neither reloaded nor
    /// surfaced. Zero inotify slots consumed.
    Ignore,
}

/// ADR-027 security policy block (`[security]`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SecurityConfig {
    /// Shell-action sandboxing policy (§D10b).
    #[serde(default)]
    pub shell: ShellSecurityConfig,
}

impl SecurityConfig {
    /// `true` when this equals the default policy — used by
    /// `skip_serializing_if` so a default `[security]` block is omitted from
    /// the canonical form (preserving `ConfigRevision` stability).
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// ADR-027 §D10b — global shell-sandbox policy (`[security.shell]`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellSecurityConfig {
    /// Allow shell actions to run UNSANDBOXED on platforms that lack an
    /// OS sandbox (Windows; Linux kernels < 5.13 without Landlock). When
    /// `true` (the default) the daemon logs the policy at startup and then
    /// spawns each unsandboxable action with a warning at spawn time; when
    /// `false` it fails closed and refuses to spawn shell actions it cannot
    /// sandbox.
    #[serde(default = "default_true")]
    pub allow_unsandboxed: bool,
}

impl Default for ShellSecurityConfig {
    fn default() -> Self {
        Self {
            allow_unsandboxed: true,
        }
    }
}

/// ADR-027 §D10b — per-action sandbox profile override.
///
/// The default profile denies all filesystem writes and network egress.
/// These fields widen it for a single shell action.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ShellSandboxConfig {
    /// Filesystem subtrees the action may WRITE to (reads stay broadly
    /// allowed). Paths are `~`-expanded and required to be absolute by the
    /// daemon before being compiled into the OS profile. They are NOT
    /// canonicalised — symlinks in the path are not resolved (§D2.2 safe-walk
    /// is a separate concern); relative paths are dropped.
    #[serde(default)]
    pub fs_write: Vec<String>,
    /// Allow network egress from the sandboxed action. Default `false`
    /// (deny). On Linux this is only enforceable on kernels with Landlock
    /// network support (ABI ≥ 4 / 6.7+). On older kernels the daemon cannot
    /// restrict network via Landlock: it logs a no-op when
    /// `allow_unsandboxed = true`, and fails closed (refuses to spawn) when
    /// `allow_unsandboxed = false`.
    #[serde(default)]
    pub network: bool,
}

impl Config {
    /// ADR-026 Phase 4.2 / ADR-035 — aliases of *enabled* endpoints where
    /// `no_probe = true` is silently overridden because the endpoint ALSO
    /// declares a `SysExIdentity` matcher (which can never resolve without a
    /// probe, so the daemon ignores `no_probe`). Disabled endpoints are
    /// excluded — the `PortResolver` skips them, so they're never probed
    /// regardless. The daemon logs each entry at config load + reload.
    pub fn endpoints_with_no_probe_sysex_override(&self) -> Vec<&str> {
        self.endpoints
            .iter()
            .filter(|e| e.enabled && e.kind.no_probe() && e.kind.has_any_sysex_identity_matcher())
            .map(|e| e.alias.as_str())
            .collect()
    }

    /// Resolve the mode index the daemon should start in, applying the
    /// canonical fallback chain: `last_selected_mode` → `default_mode` →
    /// mode index 0 (→ global-mappings-only when there are no modes).
    ///
    /// This is the single source of truth for startup-mode resolution, shared
    /// by the daemon's engine manager and the mode-management integration tests
    /// so both observe the same behaviour rather than a re-implemented
    /// copy. Returns `0` when `modes` is empty (the daemon then runs with global
    /// mappings only).
    pub fn resolve_startup_mode(&self) -> usize {
        if self.modes.is_empty() {
            return 0; // Global mappings only
        }
        // Step 1: last_selected_mode, if it names an existing mode.
        if let Some(ref name) = self.last_selected_mode
            && let Some(idx) = self.modes.iter().position(|m| &m.name == name)
        {
            return idx;
        }
        // Step 2: default_mode, if it names an existing mode.
        if let Some(ref name) = self.default_mode
            && let Some(idx) = self.modes.iter().position(|m| &m.name == name)
        {
            return idx;
        }
        // Step 3: first mode.
        0
    }

    /// Validate a request to switch to `mode_name`, returning its index on
    /// success or a human-facing error listing the available modes.
    ///
    /// This is the canonical "switch mode" validation shared by the MCP
    /// `switch_mode` tool and the mode-management integration tests, so
    /// the error contract (`"Mode not found: <name>. Available modes: <list>"`)
    /// lives in one place.
    pub fn resolve_mode_switch(&self, mode_name: &str) -> Result<usize, String> {
        match self.modes.iter().position(|m| m.name == mode_name) {
            Some(idx) => Ok(idx),
            None => Err(format!(
                "Mode not found: {}. Available modes: {}",
                mode_name,
                self.modes
                    .iter()
                    .map(|m| m.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }
}
