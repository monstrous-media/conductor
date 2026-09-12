// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Configuration types for Conductor.
//!
//! This module defines the data structures used to represent MIDI mappings,
//! triggers, and actions in the configuration file.

use crate::Condition;
use crate::identity::DeviceMatcher;
use crate::transform::MidiTransform;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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

/// LED configuration section
///
/// Controls LED feedback behavior for supported hardware devices.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LedConfig {
    /// Whether LED feedback is enabled
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Global brightness (0-127)
    #[serde(default = "default_brightness")]
    pub brightness: u8,
    /// Default lighting scheme
    #[serde(default = "default_scheme")]
    pub scheme: String,
    /// Idle timeout in seconds before dimming (0 = never)
    #[serde(default)]
    pub idle_timeout_secs: u32,
    /// Per-mode color overrides
    #[serde(default)]
    pub mode_colors: std::collections::BTreeMap<String, RgbColor>,
    /// MIDI LED configuration
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub midi: Option<MidiLedConfig>,
    /// HID LED configuration (config-driven device profiles)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hid: Option<HidLedConfig>,
    /// Velocity-to-color mapping
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub velocity_colors: Option<VelocityColorMap>,
    /// Default fade time in milliseconds for reactive LED feedback
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_fade_ms: Option<u64>,
}

impl Default for LedConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            brightness: default_brightness(),
            scheme: default_scheme(),
            idle_timeout_secs: 0,
            mode_colors: std::collections::BTreeMap::new(),
            midi: None,
            hid: None,
            velocity_colors: None,
            default_fade_ms: None,
        }
    }
}

/// MIDI LED configuration
///
/// Configures how MIDI messages control device LEDs. Supports velocity-based
/// color mapping, custom per-pad overrides, and device-specific protocols.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MidiLedConfig {
    /// MIDI channel for LED control (1-16, R673)
    #[serde(default = "default_midi_led_channel")]
    pub channel: u8,
    /// Velocity value for LED on (0-127, R674)
    #[serde(default = "default_note_on_velocity")]
    pub note_on_velocity: u8,
    /// Velocity value for LED off (0-127, R675)
    #[serde(default)]
    pub note_off_velocity: u8,
    /// Velocity-based color palette (R676, R678)
    #[serde(default)]
    pub colors: MidiLedColors,
    /// Custom per-pad LED mappings (R679-R681)
    #[serde(default)]
    pub custom_mappings: Vec<MidiLedCustomMapping>,
}

impl Default for MidiLedConfig {
    fn default() -> Self {
        Self {
            channel: default_midi_led_channel(),
            note_on_velocity: default_note_on_velocity(),
            note_off_velocity: 0,
            colors: MidiLedColors::default(),
            custom_mappings: Vec::new(),
        }
    }
}

fn default_midi_led_channel() -> u8 {
    1
}

fn default_note_on_velocity() -> u8 {
    127
}

/// Velocity-based color values for MIDI LED control (R676, R678)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MidiLedColors {
    #[serde(default = "default_color_red")]
    pub red: u8,
    #[serde(default = "default_color_green")]
    pub green: u8,
    #[serde(default = "default_color_blue")]
    pub blue: u8,
    #[serde(default = "default_color_yellow")]
    pub yellow: u8,
    #[serde(default = "default_color_amber")]
    pub amber: u8,
    #[serde(default)]
    pub off: u8,
}

impl MidiLedColors {
    /// Map an RGB color to the nearest configured palette velocity.
    /// Uses simple hue-based matching: predominantly red → red velocity, etc.
    pub fn rgb_to_velocity(&self, r: u8, g: u8, b: u8) -> u8 {
        if r == 0 && g == 0 && b == 0 {
            return self.off;
        }
        // Blue dominant
        if b > r && b > g {
            return self.blue;
        }
        // Yellow: r and g both significant and close together, with blue low
        if r > 0 && g > 0 && r.abs_diff(g) < 30 && b < r.min(g) / 2 {
            return self.yellow;
        }
        if r > g && r > b {
            return self.red;
        }
        if g > r && g > b {
            return self.green;
        }
        self.amber
    }
}

impl Default for MidiLedColors {
    fn default() -> Self {
        Self {
            red: default_color_red(),
            green: default_color_green(),
            blue: default_color_blue(),
            yellow: default_color_yellow(),
            amber: default_color_amber(),
            off: 0,
        }
    }
}

fn default_color_red() -> u8 {
    5
}
fn default_color_green() -> u8 {
    21
}
fn default_color_yellow() -> u8 {
    13
}
fn default_color_blue() -> u8 {
    45
}
fn default_color_amber() -> u8 {
    9
}

/// A custom per-pad LED mapping (R679-R681)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MidiLedCustomMapping {
    /// Pad number this mapping applies to
    pub pad: u8,
    /// MIDI message to send when LED should be on
    pub led_on: MidiLedMessage,
    /// MIDI message to send when LED should be off
    pub led_off: MidiLedMessage,
}

// ────────────────────────────────────────────────────────────────
// HID LED Configuration
// ────────────────────────────────────────────────────────────────

/// HID LED configuration (config-driven device profiles)
///
/// Configures HID-based LED control for devices like NI Maschine Mikro MK3.
/// Fields use `Option<T>` so profile merging can distinguish "user set this"
/// from "use profile default". Call `resolve_profile()` to get a fully-populated
/// `ResolvedHidLedConfig` for runtime use.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct HidLedConfig {
    /// Built-in device profile name (e.g. "mikro-mk3")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hid_profile: Option<String>,
    /// USB Vendor ID
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor_id: Option<u16>,
    /// USB Product ID
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_id: Option<u16>,
    /// HID interface number to open (0-255)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface_number: Option<u8>,
    /// HID report ID for LED output
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub led_report_id: Option<u8>,
    /// Size of the LED output buffer (bytes)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buffer_size: Option<usize>,
    /// Byte offset where pad LEDs start in the buffer
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pad_led_offset: Option<usize>,
    /// Number of pads on the device
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pad_count: Option<u8>,
    /// Indexed color palette (maps index to RGB for indexed-color devices)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_palette: Option<Vec<RgbColor>>,
    /// Pad layout: logical pad index → physical LED position.
    /// If None, identity mapping is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pad_layout: Option<Vec<u8>>,
}

/// Resolved HID LED config — all fields populated after profile merge.
/// Used by the HID feedback implementation at runtime.
#[derive(Debug, Clone)]
pub struct ResolvedHidLedConfig {
    pub vendor_id: u16,
    pub product_id: u16,
    pub interface_number: u8,
    pub led_report_id: u8,
    pub buffer_size: usize,
    pub pad_led_offset: usize,
    pub pad_count: u8,
    pub color_palette: Vec<RgbColor>,
    pub pad_layout: Vec<u8>,
}

impl HidLedConfig {
    /// Built-in Mikro MK3 device profile
    pub fn mikro_mk3() -> Self {
        Self {
            hid_profile: Some("mikro-mk3".to_string()),
            vendor_id: Some(0x17CC),
            product_id: Some(0x1700),
            interface_number: Some(0),
            led_report_id: Some(0x80),
            buffer_size: Some(80),
            pad_led_offset: Some(39),
            pad_count: Some(16),
            color_palette: Some(mikro_mk3_palette()),
            pad_layout: Some(mikro_mk3_pad_layout()),
        }
    }

    /// Look up a built-in profile by name.
    pub fn from_profile(name: &str) -> Option<Self> {
        match name {
            "mikro-mk3" => Some(Self::mikro_mk3()),
            _ => None,
        }
    }

    /// Merge profile defaults into unset fields and return a fully-populated
    /// `ResolvedHidLedConfig`. Validates buffer bounds, pad layout, etc.
    pub fn resolve_profile(&self) -> Result<ResolvedHidLedConfig, String> {
        let profile = self
            .hid_profile
            .as_ref()
            .map(|name| {
                Self::from_profile(name).ok_or_else(|| format!("Unknown HID profile: '{}'", name))
            })
            .transpose()?;

        // User values take priority; fall back to profile; then sensible defaults
        let vendor_id = self
            .vendor_id
            .or(profile.as_ref().and_then(|p| p.vendor_id))
            .ok_or("vendor_id is required (set explicitly or use hid_profile)")?;
        let product_id = self
            .product_id
            .or(profile.as_ref().and_then(|p| p.product_id))
            .ok_or("product_id is required (set explicitly or use hid_profile)")?;
        let interface_number = self
            .interface_number
            .or(profile.as_ref().and_then(|p| p.interface_number))
            .unwrap_or(0);
        let led_report_id = self
            .led_report_id
            .or(profile.as_ref().and_then(|p| p.led_report_id))
            .unwrap_or(0);
        let buffer_size = self
            .buffer_size
            .or(profile.as_ref().and_then(|p| p.buffer_size))
            .unwrap_or(80);
        let pad_led_offset = self
            .pad_led_offset
            .or(profile.as_ref().and_then(|p| p.pad_led_offset))
            .unwrap_or(0);
        let pad_count = self
            .pad_count
            .or(profile.as_ref().and_then(|p| p.pad_count))
            .unwrap_or(16);
        let color_palette = self
            .color_palette
            .clone()
            .or(profile.as_ref().and_then(|p| p.color_palette.clone()))
            .unwrap_or_default();
        let pad_layout = self
            .pad_layout
            .clone()
            .or(profile.as_ref().and_then(|p| p.pad_layout.clone()))
            .unwrap_or_default();

        // Validate buffer bounds (use checked_add for overflow safety)
        if buffer_size == 0 {
            return Err("buffer_size must be > 0".to_string());
        }
        if pad_led_offset >= buffer_size {
            return Err(format!(
                "pad_led_offset ({}) must be less than buffer_size ({})",
                pad_led_offset, buffer_size
            ));
        }
        // Note: assumes 1 byte per pad (indexed color). Devices needing
        // multi-byte LED data (e.g. RGB) would need a bytes_per_pad field.
        let pad_end = pad_led_offset
            .checked_add(pad_count as usize)
            .ok_or_else(|| "pad config causes integer overflow".to_string())?;
        if pad_end > buffer_size {
            return Err(format!(
                "pad range (offset {} + count {}) exceeds buffer_size ({})",
                pad_led_offset, pad_count, buffer_size
            ));
        }

        // Validate pad_layout or materialize identity mapping
        let pad_layout = if pad_layout.is_empty() {
            // Identity mapping: logical index == physical position
            (0..pad_count).collect()
        } else {
            if pad_layout.len() != pad_count as usize {
                return Err(format!(
                    "pad_layout has {} entries but pad_count is {}",
                    pad_layout.len(),
                    pad_count
                ));
            }
            let mut seen = std::collections::HashSet::new();
            for (i, &pos) in pad_layout.iter().enumerate() {
                if pos >= pad_count {
                    return Err(format!(
                        "pad_layout[{}]: position {} >= pad_count {}",
                        i, pos, pad_count
                    ));
                }
                if !seen.insert(pos) {
                    return Err(format!(
                        "pad_layout[{}]: duplicate physical position {}",
                        i, pos
                    ));
                }
            }
            pad_layout
        };

        Ok(ResolvedHidLedConfig {
            vendor_id,
            product_id,
            interface_number,
            led_report_id,
            buffer_size,
            pad_led_offset,
            pad_count,
            color_palette,
            pad_layout,
        })
    }
}

/// MK3 indexed color palette (maps PadColor values to approximate RGB)
fn mikro_mk3_palette() -> Vec<RgbColor> {
    vec![
        RgbColor { r: 0, g: 0, b: 0 },   // 0: Off
        RgbColor { r: 255, g: 0, b: 0 }, // 1: Red
        RgbColor {
            r: 255,
            g: 128,
            b: 0,
        }, // 2: Orange
        RgbColor {
            r: 255,
            g: 180,
            b: 0,
        }, // 3: LightOrange
        RgbColor {
            r: 255,
            g: 210,
            b: 0,
        }, // 4: WarmYellow
        RgbColor {
            r: 255,
            g: 255,
            b: 0,
        }, // 5: Yellow
        RgbColor {
            r: 128,
            g: 255,
            b: 0,
        }, // 6: Lime
        RgbColor { r: 0, g: 255, b: 0 }, // 7: Green
        RgbColor {
            r: 0,
            g: 255,
            b: 128,
        }, // 8: Mint
        RgbColor {
            r: 0,
            g: 255,
            b: 255,
        }, // 9: Cyan
        RgbColor {
            r: 0,
            g: 200,
            b: 255,
        }, // 10: Turquoise
        RgbColor {
            r: 0,
            g: 128,
            b: 255,
        }, // 11: Blue
        RgbColor {
            r: 128,
            g: 0,
            b: 255,
        }, // 12: Plum
        RgbColor {
            r: 160,
            g: 0,
            b: 255,
        }, // 13: Violet
        RgbColor {
            r: 200,
            g: 0,
            b: 255,
        }, // 14: Purple
        RgbColor {
            r: 255,
            g: 0,
            b: 200,
        }, // 15: Magenta
        RgbColor {
            r: 255,
            g: 0,
            b: 128,
        }, // 16: Fuchsia
        RgbColor {
            r: 255,
            g: 255,
            b: 255,
        }, // 17: White
    ]
}

/// MK3 pad layout: vertical flip (logical bottom-to-top → physical top-to-bottom)
fn mikro_mk3_pad_layout() -> Vec<u8> {
    vec![12, 13, 14, 15, 8, 9, 10, 11, 4, 5, 6, 7, 0, 1, 2, 3]
}

// ────────────────────────────────────────────────────────────────
// Velocity-to-Color Mapping
// ────────────────────────────────────────────────────────────────

/// Velocity-to-color mapping
///
/// Maps velocity ranges to colors for LED feedback.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VelocityColorMap {
    pub ranges: Vec<VelocityRange>,
}

impl Default for VelocityColorMap {
    fn default() -> Self {
        Self {
            ranges: vec![
                VelocityRange {
                    min: 0,
                    max: 39,
                    color: RgbColor { r: 0, g: 255, b: 0 },
                },
                VelocityRange {
                    min: 40,
                    max: 79,
                    color: RgbColor {
                        r: 255,
                        g: 255,
                        b: 0,
                    },
                },
                VelocityRange {
                    min: 80,
                    max: 127,
                    color: RgbColor { r: 255, g: 0, b: 0 },
                },
            ],
        }
    }
}

impl VelocityColorMap {
    /// Look up the color for a given velocity. Returns None if no range matches.
    pub fn color_for_velocity(&self, velocity: u8) -> Option<&RgbColor> {
        self.ranges
            .iter()
            .find(|r| velocity >= r.min && velocity <= r.max)
            .map(|r| &r.color)
    }
}

/// A velocity range with associated color
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VelocityRange {
    pub min: u8,
    pub max: u8,
    pub color: RgbColor,
}

/// A MIDI message descriptor for LED control
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MidiLedMessage {
    NoteOn { note: u8, velocity: u8 },
    NoteOff { note: u8, velocity: u8 },
    Cc { cc: u8, value: u8 },
}

fn default_brightness() -> u8 {
    100
}

fn default_scheme() -> String {
    "reactive".to_string()
}

/// RGB color for LED configuration
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct RgbColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

// RgbColor ↔ RGB conversions live in mikro_leds.rs to avoid coupling config to hardware

// ────────────────────────────────────────────────────────
// Signal Routing Graph — ADR-031 D1
// ────────────────────────────────────────────────────────

/// Direction of a connector endpoint.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
pub enum ConnectorDirection {
    Input,
    Output,
    #[default]
    Bidirectional,
}

/// Protocol spoken by a connector.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
pub enum ConnectorProtocol {
    #[default]
    Midi,
    Osc,
    ArtNet,
    Hid,
}

/// ADR-042 Phase A — per-listener network-security policy, shared by the
/// `OscEndpoint` and `ArtNetEndpoint` payloads (flattened onto the endpoint
/// table on the wire).
///
/// Phase A is **loopback-only**: these fields are *parsed and shape-validated*
/// (forward-compat so Phase B-early can lift the gate) but a non-loopback
/// listener `host` is a config-load error regardless of `allow_network`. The
/// fields only become operative once a non-loopback bind exists in Phase
/// B-early. `allow_sensitive_actions` (D17) is the exception — it is the
/// action-class gate that is **active in Phase A** for loopback OSC/Art-Net.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetworkSecurityConfig {
    /// Operator intent to accept non-loopback traffic. Phase A still rejects
    /// the bind (loopback-only); shape-validated for forward compatibility.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_network: bool,

    /// Allow-list of source CIDRs (parsed via [`crate::security::NetworkAcl`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub network_acl: Vec<String>,

    /// Optional narrower allow-list of individual sender IPs (checked in
    /// addition to `network_acl` at the listener edge).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sender_acl: Vec<String>,

    /// Total inbound packet budget (token-bucket). `None` = default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_total: Option<u32>,

    /// Per-sender inbound packet budget (checked before the total).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_per_sender: Option<u32>,

    /// Acknowledge the amplification risk of a broad broadcast ACL (D11).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub i_understand_amplification_risk: bool,

    /// D17 action-class gate: when `false` (default), network-origin triggers
    /// from this listener — **including loopback OSC/Art-Net** — may NOT
    /// dispatch `Shell`/`Launch`/`Keystroke`. Active in Phase A.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_sensitive_actions: bool,

    /// Phase B-late per-listener strict-mode (session-token replay defence).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict_mode: Option<StrictMode>,
}

/// Phase B-late strict-mode policy (parsed in Phase A for forward-compat).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum StrictMode {
    /// Require a session-token nonce in a designated OSC argument.
    SessionToken {
        /// OSC argument index carrying the nonce.
        arg_index: usize,
        /// Validity window in seconds (default 30).
        #[serde(default = "default_strict_window_sec")]
        window_sec: u64,
        /// Replay-cache size (default 1000).
        #[serde(default = "default_strict_replay_window")]
        replay_window: usize,
    },
}

fn default_strict_window_sec() -> u64 {
    30
}
fn default_strict_replay_window() -> usize {
    1000
}

/// Protocol-specific endpoint identification (ADR-031 § 3.1 / ADR-035 §4.1).
///
/// Promoted from the former nested `EndpointConfig` enum (ADR-035):
/// the `EndpointConfig` name now belongs to the unified `[[endpoints]]`
/// wrapper struct below; this enum is the type-specific payload (`kind`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum EndpointKind {
    /// MIDI/HID: match by port name, USB ID, SysEx identity.
    /// Reuses existing `DeviceMatcher` infrastructure (ADR-022).
    Matcher {
        /// Symmetric matchers — used in both directions, or as the sole
        /// direction. Empty is permitted only when an asymmetric
        /// `input_matchers`/`output_matchers` is populated (validated via
        /// the non-empty invariant).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        matchers: Vec<DeviceMatcher>,
        /// Asymmetric override for a Bidirectional endpoint whose input
        /// port differs from its output (ADR-035 §4.4, R2 — replaces the
        /// synthetic `-out` alias). Empty = fall back to `matchers`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        input_matchers: Vec<DeviceMatcher>,
        /// Asymmetric output-side matchers. Empty = fall back to `matchers`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        output_matchers: Vec<DeviceMatcher>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        no_probe: bool,
    },
    /// OSC: network endpoint.
    OscEndpoint {
        host: String,
        port: u16,
        /// ADR-042 Phase A network-security policy (loopback-only in Phase A;
        /// flattened so the fields sit at the endpoint level on the wire).
        #[serde(flatten)]
        security: NetworkSecurityConfig,
    },
    /// Art-Net: universe on a network interface.
    ArtNetEndpoint {
        universe: u16,
        #[serde(default = "default_artnet_host")]
        host: String,
        #[serde(default = "default_artnet_port")]
        port: u16,
        /// Art-Net broadcast (255.255.255.255 / directed broadcast). When a
        /// listener (`direction = Input`) sets this, the ACL amplification
        /// budget (ADR-042 D11) is enforced.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        allow_broadcast: bool,
        /// ADR-042 Phase A network-security policy (shared with OSC).
        #[serde(flatten)]
        security: NetworkSecurityConfig,
    },
    /// Virtual MIDI port created by Conductor (ADR-031 D10 — DAW proxy model).
    ///
    /// The daemon creates a virtual MIDI port with this name via CoreMIDI
    /// (macOS) or ALSA (Linux). DAWs and other apps see it as a standard
    /// MIDI port. Virtual ports are lazily created only when referenced
    /// by an enabled route.
    MidiVirtualPort {
        /// Name of the virtual port as it appears to the OS and other apps.
        /// Convention: `"Conductor: {device_alias}"` (e.g., `"Conductor: Mikro"`).
        port_name: String,
    },
}

fn default_artnet_host() -> String {
    "255.255.255.255".to_string()
}
fn default_artnet_port() -> u16 {
    6454
}

impl EndpointKind {
    /// Direction-aware matcher selection (ADR-035 §4.1, R3 "empty-matchers
    /// hazard"). `Input` → `input_matchers` (falling back to `matchers`);
    /// `Output` → `output_matchers` (falling back to `matchers`);
    /// `Bidirectional` → `matchers`. Non-`Matcher` kinds carry no matchers.
    ///
    /// Probing, conflict-overlap detection, and metrics/labels MUST use this
    /// instead of reading `.matchers` directly — an output-only endpoint
    /// lowers to `Matcher` with empty `matchers` + populated
    /// `output_matchers`, so `matchers[0]` would panic.
    pub fn effective_matchers(&self, dir: ConnectorDirection) -> &[DeviceMatcher] {
        match self {
            EndpointKind::Matcher {
                matchers,
                input_matchers,
                output_matchers,
                ..
            } => match dir {
                ConnectorDirection::Input if !input_matchers.is_empty() => input_matchers,
                ConnectorDirection::Output if !output_matchers.is_empty() => output_matchers,
                _ => matchers,
            },
            _ => &[],
        }
    }

    /// The `EndpointKind::Matcher` non-empty invariant (ADR-035 §4.1, R3):
    /// a `Matcher` must carry at least one matcher across the three lists.
    /// Returns `true` when the invariant holds (vacuously `true` for
    /// non-`Matcher` kinds). The validator turns a `false` here
    /// into a clear load-time "endpoint has no matchers" error.
    pub fn has_any_matcher(&self) -> bool {
        match self {
            EndpointKind::Matcher {
                matchers,
                input_matchers,
                output_matchers,
                ..
            } => !matchers.is_empty() || !input_matchers.is_empty() || !output_matchers.is_empty(),
            _ => true,
        }
    }

    /// `no_probe` flag for a `Matcher` endpoint (ADR-026 Phase 4.2). Always
    /// `false` for non-`Matcher` kinds (probing only applies to MIDI/HID).
    pub fn no_probe(&self) -> bool {
        matches!(self, EndpointKind::Matcher { no_probe: true, .. })
    }

    /// `true` iff any matcher across `matchers` / `input_matchers` /
    /// `output_matchers` is `DeviceMatcher::SysExIdentity`. Used by the
    /// `no_probe` override warning + `device_should_skip_auto_probe`
    /// (probe_on_connect). Phase 4.2.
    pub fn has_any_sysex_identity_matcher(&self) -> bool {
        let is_sysex = |m: &DeviceMatcher| matches!(m, DeviceMatcher::SysExIdentity { .. });
        match self {
            EndpointKind::Matcher {
                matchers,
                input_matchers,
                output_matchers,
                ..
            } => {
                matchers.iter().any(is_sysex)
                    || input_matchers.iter().any(is_sysex)
                    || output_matchers.iter().any(is_sysex)
            }
            _ => false,
        }
    }

    /// Protocol implied by this endpoint kind, used when an
    /// [`EndpointConfig`] omits an explicit `protocol` override.
    /// `OscEndpoint` → `Osc`, `ArtNetEndpoint` → `ArtNet`,
    /// `Matcher`/`MidiVirtualPort` → `Midi` (the default). A `Matcher`
    /// can also be HID, but that distinction only arrives via the explicit
    /// `protocol` override — kind-inference alone defaults it to `Midi`.
    pub fn protocol(&self) -> ConnectorProtocol {
        match self {
            EndpointKind::OscEndpoint { .. } => ConnectorProtocol::Osc,
            EndpointKind::ArtNetEndpoint { .. } => ConnectorProtocol::ArtNet,
            EndpointKind::Matcher { .. } | EndpointKind::MidiVirtualPort { .. } => {
                ConnectorProtocol::Midi
            }
        }
    }
}

/// A single unified I/O endpoint (ADR-035). Collapses the legacy
/// `[[bindings]]` (`DeviceIdentityConfig`, input-only) and `[[connectors]]`
/// (`ConnectorConfig`) blocks into one `[[endpoints]]` schema with an
/// explicit `direction` + `type` discriminator.
///
/// Authored under `[[endpoints]]`; legacy blocks are lowered into this shape
/// in memory (ADR-035), never parsed through this struct's strict
/// deserializer directly (which requires `direction`).
///
/// **Deserialization is hand-written** (see the `Deserialize` impl below):
/// `#[serde(flatten)]` over an internally-tagged enum is unsafe with the
/// `toml` crate — it silently disables unknown-field detection (a `prot =`
/// typo would be dropped) and mangles scalar parsing. The derived `Serialize`
/// keeps `#[serde(flatten)]` for output; a parity test covers the round-trip.
#[derive(Debug, Clone, Serialize)]
pub struct EndpointConfig {
    /// Unique across the merged endpoint set (endpoints + lowered bindings +
    /// lowered connectors).
    pub alias: String,

    /// Input / Output / Bidirectional. REQUIRED when authored as
    /// `[[endpoints]]` (no serde default — R2 P1): forcing it avoids
    /// accidentally binding a network listener as implicitly Bidirectional.
    pub direction: ConnectorDirection,

    /// Override protocol auto-detection. Inferred from `kind` when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<ConnectorProtocol>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[serde(default = "default_true")]
    pub enabled: bool,

    /// MIDI channel scope (0–15). Empty = all channels.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<u8>,

    /// Type-specific payload. On the wire (`Serialize`), `#[serde(flatten)]`
    /// lifts the `type = "..."` tag and the variant's fields to the same
    /// TOML level as the common fields above.
    #[serde(flatten)]
    pub kind: EndpointKind,
}

impl EndpointConfig {
    /// Effective protocol: the explicit `protocol` override when authored,
    /// otherwise inferred from `kind` (see [`EndpointKind::protocol`]).
    ///
    /// This is the single source of truth for "what protocol does this
    /// endpoint speak" — used by the connector registry (runtime
    /// projection) and the output resolver (MIDI-output-map filter) so an
    /// `OscEndpoint`/`ArtNetEndpoint`/HID endpoint never lands in the MIDI
    /// output port map.
    pub fn effective_protocol(&self) -> ConnectorProtocol {
        self.protocol.unwrap_or_else(|| self.kind.protocol())
    }
}

impl<'de> Deserialize<'de> for EndpointConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // ADR-035 §4.1 / R3: route through `toml::Value` rather than
        // `#[serde(flatten)]`. Config is TOML-only; a `toml::Value` round-trip
        // preserves typed scalars (unlike serde's flatten Content buffer) and
        // lets us reject stray/typo'd keys with a contextual error. See the
        // repo's `toml::from_str::<Value>` gotcha note.
        //
        // The GUI's `save_config` round-trips config through serde_json,
        // and TOML has no `null` — so a plain `toml::Value::deserialize` rejects
        // ANY JSON `null` outright ("invalid type: null, expected any valid TOML
        // value"), even for an optional field the GUI legitimately sends as null.
        // Deserialize each top-level field as `Option<toml::Value>` (a JSON null
        // → `None`) and hand the map — nulls included — to the strict parser,
        // which treats a null as absent for value extraction but still rejects a
        // null on an unknown/typo'd key (null is not a strictness
        // escape hatch). The config-load (TOML) path has no nulls, so every entry
        // is `Some(_)` and behaviour there is byte-for-byte unchanged.
        let raw: std::collections::BTreeMap<String, Option<toml::Value>> =
            std::collections::BTreeMap::deserialize(deserializer)?;
        endpoint_from_toml_value(raw).map_err(serde::de::Error::custom)
    }
}

/// Strict, contextual parse of one `[[endpoints]]` table into an
/// [`EndpointConfig`]. Factored out of the `Deserialize` impl so the
/// strictness rules are unit-testable directly. Returns a human-readable
/// error string (the impl wraps it via `serde::de::Error::custom`).
fn endpoint_from_toml_value(
    mut table: std::collections::BTreeMap<String, Option<toml::Value>>,
) -> Result<EndpointConfig, String> {
    // `table` maps field name → `Some(value)` for a present field, or `None` for
    // a field the caller sent as JSON `null`. A null is treated as
    // absent for value extraction (`take_opt` → `None`; `take` → missing-field
    // error), but the KEY stays in the map until consumed, so the strict
    // leftover-key check below still rejects a null on an unknown/typo'd field
    // (null must not bypass strictness). The TOML config-load
    // path produces no nulls, so every entry is `Some(_)` and behaviour is
    // unchanged. (A non-table endpoint errors earlier, when the caller
    // deserializes into the map.)
    type RawTable = std::collections::BTreeMap<String, Option<toml::Value>>;

    fn take<T: serde::de::DeserializeOwned>(t: &mut RawTable, key: &str) -> Result<T, String> {
        match t.remove(key) {
            // Absent, or present-but-null → the field is missing.
            None | Some(None) => Err(format!("missing field `{key}`")),
            Some(Some(v)) => v.try_into().map_err(|e| format!("invalid `{key}`: {e}")),
        }
    }
    fn take_opt<T: serde::de::DeserializeOwned>(
        t: &mut RawTable,
        key: &str,
    ) -> Result<Option<T>, String> {
        match t.remove(key) {
            // Absent OR explicit null → `None`.
            None | Some(None) => Ok(None),
            Some(Some(v)) => v
                .try_into()
                .map(Some)
                .map_err(|e| format!("invalid `{key}`: {e}")),
        }
    }

    // ADR-042 Phase A — the shared network-security fields (flattened on the
    // wire) for OSC / Art-Net listeners. Each is taken individually so the
    // strict "leftover key" check below still catches typos.
    fn take_network_security(t: &mut RawTable) -> Result<NetworkSecurityConfig, String> {
        Ok(NetworkSecurityConfig {
            allow_network: take_opt(t, "allow_network")?.unwrap_or(false),
            network_acl: take_opt(t, "network_acl")?.unwrap_or_default(),
            sender_acl: take_opt(t, "sender_acl")?.unwrap_or_default(),
            rate_limit_total: take_opt(t, "rate_limit_total")?,
            rate_limit_per_sender: take_opt(t, "rate_limit_per_sender")?,
            i_understand_amplification_risk: take_opt(t, "i_understand_amplification_risk")?
                .unwrap_or(false),
            allow_sensitive_actions: take_opt(t, "allow_sensitive_actions")?.unwrap_or(false),
            strict_mode: take_opt(t, "strict_mode")?,
        })
    }

    let alias: String = take(&mut table, "alias")?;
    let direction: ConnectorDirection = take(&mut table, "direction")?;
    let protocol: Option<ConnectorProtocol> = take_opt(&mut table, "protocol")?;
    let description: Option<String> = take_opt(&mut table, "description")?;
    let enabled: bool = take_opt(&mut table, "enabled")?.unwrap_or(true);
    let channels: Vec<u8> = take_opt(&mut table, "channels")?.unwrap_or_default();

    let type_tag: String = take(&mut table, "type")?;
    let kind = match type_tag.as_str() {
        "Matcher" => EndpointKind::Matcher {
            matchers: take_opt(&mut table, "matchers")?.unwrap_or_default(),
            input_matchers: take_opt(&mut table, "input_matchers")?.unwrap_or_default(),
            output_matchers: take_opt(&mut table, "output_matchers")?.unwrap_or_default(),
            no_probe: take_opt(&mut table, "no_probe")?.unwrap_or(false),
        },
        "MidiVirtualPort" => EndpointKind::MidiVirtualPort {
            port_name: take(&mut table, "port_name")?,
        },
        "OscEndpoint" => EndpointKind::OscEndpoint {
            host: take(&mut table, "host")?,
            port: take(&mut table, "port")?,
            security: take_network_security(&mut table)?,
        },
        "ArtNetEndpoint" => EndpointKind::ArtNetEndpoint {
            universe: take(&mut table, "universe")?,
            host: take_opt(&mut table, "host")?.unwrap_or_else(default_artnet_host),
            port: take_opt(&mut table, "port")?.unwrap_or_else(default_artnet_port),
            allow_broadcast: take_opt(&mut table, "allow_broadcast")?.unwrap_or(false),
            security: take_network_security(&mut table)?,
        },
        other => {
            return Err(format!(
                "unknown endpoint `type` \"{other}\" (expected one of: Matcher, \
                 MidiVirtualPort, OscEndpoint, ArtNetEndpoint)"
            ));
        }
    };

    // Strict: any leftover key is an unknown / typo'd field — the whole
    // reason this impl is hand-written (§4.1). `prot =` instead of
    // `protocol =`, or a stray `host` on `MidiVirtualPort`, errors here
    // instead of being silently dropped. A leftover key whose value was
    // `null` is rejected too (it stayed in the map), so JSON null can't be a
    // strictness escape hatch on an unknown field.
    if let Some(unknown) = table.keys().next() {
        return Err(format!(
            "unknown field `{unknown}` for endpoint type \"{type_tag}\" (alias \"{alias}\")"
        ));
    }

    Ok(EndpointConfig {
        alias,
        direction,
        protocol,
        description,
        enabled,
        channels,
        kind,
    })
}

/// A named I/O endpoint in the signal routing graph (ADR-031 D1).
///
/// Connectors extend `DeviceIdentityConfig` (ADR-022 bindings, input-only)
/// to cover output endpoints and bidirectional devices. Validation
/// (per spec § 3.3) enforces alias uniqueness across bindings + connectors.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorConfig {
    /// User-defined alias (unique across connectors AND bindings).
    pub alias: String,

    /// Connector direction.
    #[serde(default)]
    pub direction: ConnectorDirection,

    /// Protocol this connector speaks.
    #[serde(default)]
    pub protocol: ConnectorProtocol,

    /// How to find this connector's physical port(s).
    pub endpoint: EndpointKind,

    /// Optional human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Whether this connector is active.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Per-connector MIDI channel scope (ADR-031 § 3.1).
    /// Empty = match all channels. Values are 0-indexed (0-15).
    /// Events on channels not in this list are dropped at the connector
    /// boundary. Channels are only meaningful for `protocol = "Midi"`;
    /// the validator warns when set on non-MIDI protocols.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<u8>,
}

// ────────────────────────────────────────────────────────
// Signal Routing Graph — ADR-031 D2 / Phase 2 (Routes)
// ────────────────────────────────────────────────────────

/// A signal path from one connector/binding to another (ADR-031 D2).
///
/// Routes operate below the mapping engine — signals flow through routes
/// unless intercepted by a trigger/action mapping. ADR-036 unifies routes
/// with the legacy `Trigger::Raw` mechanism by adding `modes` (scope).
/// Bare routes (no `modes`) remain mode-independent for backward
/// compatibility. All routes are post-mapping (ADR-036 Phase 3 removed the
/// `pre_mapping` escape hatch).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteConfig {
    /// Source connector or binding alias.
    pub from: String,
    /// Destination connector or binding alias. (Validation accepts
    /// either — bindings can host MIDI output ports too.)
    pub to: String,
    /// Optional transform applied to signals in transit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform: Option<SignalTransform>,
    /// Optional filter. Only signals matching the filter are routed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<SignalFilter>,
    /// Whether this route is active.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Optional human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Mode scope (ADR-036 D1). Empty = fires in all modes (legacy
    /// bare-route behaviour). Non-empty = fires only when one of the
    /// listed mode names is active.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modes: Vec<String>,
}

/// Filter applied to signals in a route (ADR-031 D4).
/// Only signals matching ALL populated criteria pass through.
/// Empty/None fields are unconstrained (match everything).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalFilter {
    /// MIDI message types to include (empty = all).
    /// Reuses `MidiMessageType` from ADR-030 P1 — same enum used by
    /// `Trigger::Raw.message_types` so users learn one vocabulary.
    /// The validator (Phase 2A § 4.3) inherits ADR-030 §D7's restriction:
    /// `ChannelPressure` and `SysEx` are rejected (not yet emitted by the
    /// input pipeline). When/if those land, drop the restriction in both
    /// places at once.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub message_types: Vec<MidiMessageType>,
    /// MIDI channel filter (empty = all channels). 0-indexed (0-15).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<u8>,
    /// MIDI CC range to include `[min, max]` (inclusive).
    /// Validator MUST reject `min > max` (per spec § 4.1 — same pattern
    /// as `CcValueInRange` rejection at config load). Otherwise the
    /// filter would silently match nothing and a route would never
    /// fire — the same failure mode, but harder to diagnose because no
    /// trigger is involved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cc_range: Option<(u8, u8)>,
    /// MIDI note range to include `[min, max]` (inclusive). Same
    /// `min > max` rejection applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note_range: Option<(u8, u8)>,
    /// OSC address prefix to match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub osc_address_prefix: Option<String>,
}

/// Transform applied to signals in a route (ADR-031 D3).
/// Protocol-specific, not generic.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SignalTransform {
    /// MIDI → MIDI transform (reuses existing `MidiTransform` from
    /// ADR-009 Gap 2 — channel/CC/note remap, velocity
    /// scale/offset, value invert, value curve).
    Midi(crate::transform::MidiTransform),

    /// MIDI → OSC cross-protocol translation (Phase 5).
    MidiToOsc {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cc_to_address: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note_to_address: Option<String>,
        #[serde(default)]
        value_to_float: bool,
    },

    /// OSC → MIDI cross-protocol translation (Phase 5).
    OscToMidi {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        address_to_cc: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        address_to_note: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel: Option<u8>,
    },

    /// MIDI → Art-Net (CC/note values to DMX channel levels, Phase 5).
    ///
    /// `cc_to_dmx` / `note_to_dmx` are `HashMap<u8, u16>` in Rust
    /// (typed for safety + 7-bit MIDI semantics), but TOML requires
    /// string-typed table keys — so we serialise/deserialise via the
    /// `u8_string_map` helper which emits decimal-string keys
    /// (e.g. `7u8` → `"7"`) and parses them back via `u8::from_str`.
    MidiToArtNet {
        #[serde(with = "crate::config::u8_string_map")]
        cc_to_dmx: std::collections::HashMap<u8, u16>,
        #[serde(
            default,
            with = "crate::config::u8_string_map",
            skip_serializing_if = "std::collections::HashMap::is_empty"
        )]
        note_to_dmx: std::collections::HashMap<u8, u16>,
    },

    /// HID → Art-Net (analog axis values to DMX channel levels, Phase 5).
    HidToArtNet {
        trigger_to_channel: std::collections::HashMap<String, u16>,
    },

    /// OSC → Art-Net (ADR-039-A): extract the DMX channel from
    /// the OSC **address** via a template carrying a single `{dmx}` placeholder
    /// (same fallible-extraction convention as `OscToMidi`'s `{cc}`/`{note}` —
    /// the capture is attacker-controlled, so it is parsed fallibly and
    /// range-checked to the DMX universe 1-512 before any update is built).
    /// The first OSC argument becomes the 8-bit DMX level: `Float` is treated
    /// as normalised 0.0-1.0 → 0-255, `Int` clamps to 0-255.
    OscToArtNet {
        /// Address template with a `{dmx}` placeholder, e.g. `"/dmx/{dmx}"`.
        /// Validated at config-load (must start with `/` and contain exactly
        /// one `{dmx}`).
        address_to_dmx: String,
    },

    /// HID → MIDI (ADR-039-B): map a gamepad trigger name to a MIDI
    /// Control Change. The trigger's 7-bit value (button velocity / axis 0-127)
    /// becomes the CC value verbatim; emitted on `channel` (0-indexed 0-15).
    /// Keys are canonical gamepad trigger names (`south`, `left_stick_x`, …);
    /// `String` keys so TOML tables work directly (no `u8_string_map` needed).
    HidToMidi {
        trigger_to_cc: std::collections::HashMap<String, u8>,
        /// MIDI channel (0-indexed 0-15) for the emitted CC. Defaults to 0.
        #[serde(default)]
        channel: u8,
    },

    /// HID → OSC (ADR-039-B): map a gamepad trigger name to an OSC
    /// address; the trigger's 7-bit value (button velocity / axis 0-127) is the
    /// single OSC argument — a normalized `Float` 0.0-1.0 when `value_to_float`
    /// (the OSC convention), else a raw `Int`. Mirrors `MidiToOsc`'s
    /// `value_to_float` toggle. Keys are canonical gamepad trigger names; values
    /// are OSC addresses (must start with `/`, validated at config-load).
    HidToOsc {
        trigger_to_address: std::collections::HashMap<String, String>,
        /// Emit the value as a normalized `Float` (0.0-1.0); else a raw `Int`.
        #[serde(default)]
        value_to_float: bool,
    },
}

pub(crate) fn default_true() -> bool {
    true
}

/// Event console configuration (R925, R926-R928)
///
/// Controls event monitoring buffer and capture toggles.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EventConsoleConfig {
    /// Event buffer size — how many events to keep in memory (R925)
    #[serde(default = "default_buffer_size")]
    pub buffer_size: usize,
    /// Maximum events per second before throttling (R924).
    /// 0 = unlimited. Default: 0 (no limit).
    #[serde(default)]
    pub max_events_per_second: u32,
    /// Capture raw MIDI events (R926)
    #[serde(default = "default_true")]
    pub capture_midi: bool,
    /// Capture processed/interpreted events (R927)
    #[serde(default = "default_true")]
    pub capture_processed: bool,
    /// Capture action execution events (R928)
    #[serde(default = "default_true")]
    pub capture_actions: bool,
    /// Named filters for quick selection (R911-R913)
    #[serde(default)]
    pub filters: std::collections::BTreeMap<String, crate::config::types::NamedEventFilter>,
    /// Event-based triggers (R915-R917)
    #[serde(default)]
    pub triggers: std::collections::BTreeMap<String, EventTrigger>,
    /// Enable performance profiling (R918)
    #[serde(default)]
    pub enable_profiling: bool,
    /// Track per-event processing latency (R919)
    #[serde(default)]
    pub track_latency: bool,
    /// Track memory usage (R920)
    #[serde(default)]
    pub track_memory: bool,
}

impl Default for EventConsoleConfig {
    fn default() -> Self {
        Self {
            buffer_size: default_buffer_size(),
            max_events_per_second: 0,
            capture_midi: true,
            capture_processed: true,
            capture_actions: true,
            filters: std::collections::BTreeMap::new(),
            triggers: std::collections::BTreeMap::new(),
            enable_profiling: false,
            track_latency: false,
            track_memory: false,
        }
    }
}

/// Event trigger configuration (R915-R917)
///
/// Watches the event stream and fires an action when a condition is met.
/// Conditions are evaluated over a rolling time window.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EventTrigger {
    /// Trigger condition expression (R916)
    /// Format: "<metric> <op> <threshold> <window>"
    /// Examples: "error_rate > 5 per_minute", "event_count > 100 per_second"
    pub condition: String,
    /// Action to fire when condition is met (R917)
    pub action: TriggerAction,
    /// Optional cooldown in seconds to prevent repeated firing
    #[serde(default)]
    pub cooldown_secs: Option<u64>,
}

/// Action to take when an event trigger fires (R917)
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum TriggerAction {
    /// Log a message to the event console
    #[serde(alias = "log", alias = "Log")]
    Log { message: String },
    /// Send a desktop notification
    #[serde(alias = "notification", alias = "Notification")]
    Notification { message: String },
}

/// Named event filter for config-based presets (R911-R914)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NamedEventFilter {
    /// Optional description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Event type filter (comma-separated)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_type: Option<String>,
    /// MIDI channel filter (0-15)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<u8>,
    /// Min note number
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note_min: Option<u8>,
    /// Max note number
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note_max: Option<u8>,
    /// Device ID filter
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
}

fn default_buffer_size() -> usize {
    1000
}

/// Listen mode for multi-device architecture (ADR-009)
///
/// Default is `All` — opens every available MIDI port so that unconfigured
/// hardware is immediately visible in the GUI Devices page. Users who want
/// to restrict listening to declared `[[devices]]` can set `"Configured"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
pub enum ListenMode {
    /// Listen to all available MIDI ports (default)
    #[default]
    All,
    /// Listen only to ports matching configured device identities
    Configured,
}

/// Logging configuration
///
/// Defines how the application should log diagnostic information.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct LoggingConfig {
    /// Log level: "off", "error", "warn", "info", "debug", "trace"
    #[serde(default = "default_log_level")]
    pub level: String,
    /// Enable file logging
    #[serde(default)]
    pub file: Option<String>,
}

fn default_log_level() -> String {
    "info".to_string()
}

/// Advanced settings for event processing and timing
///
/// Fine-tunes behavior of event detection algorithms.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AdvancedSettings {
    /// Time window in milliseconds for chord detection (default: 50ms)
    #[serde(default = "default_chord_timeout_ms")]
    pub chord_timeout_ms: u64,
    /// Time window in milliseconds for chord detection while MIDI Learn is
    /// active (default: 150ms). Independent of [`Self::chord_timeout_ms`] — the
    /// default is wider so chords register readily while mapping, but a user may
    /// set it to any value (smaller or larger). Was a hardcoded `150` in the
    /// daemon's Learn path; promoted to config so the daemon is the
    /// single source of truth for the value the Settings panel displays.
    #[serde(default = "default_chord_learn_timeout_ms")]
    pub chord_learn_timeout_ms: u64,
    /// Time window in milliseconds for double-tap detection (default: 300ms)
    #[serde(default = "default_double_tap_timeout_ms")]
    pub double_tap_timeout_ms: u64,
    /// Hold threshold in milliseconds for long press detection (default: 2000ms)
    #[serde(default = "default_hold_threshold_ms")]
    pub hold_threshold_ms: u64,
    /// Short→Medium press classification boundary in milliseconds (default:
    /// 200ms) — the "Medium Press Threshold" setting. A press shorter
    /// than this is `ShortPress`; at/above it (and below the Long boundary) it
    /// is `MediumPress`. Distinct from `hold_threshold_ms` (the `HoldDetected`
    /// while-held event the "Long Press Threshold" slider drives).
    #[serde(default = "default_short_press_ms")]
    pub short_press_ms: u64,
    /// Listen mode for multi-device (ADR-009). Default: All
    #[serde(default)]
    pub listen_mode: ListenMode,
    /// Port names to ignore when listening (ADR-009)
    #[serde(default)]
    pub ignore_ports: Vec<String>,
    /// Maximum number of MIDI ports to open simultaneously (ADR-009). Default: 32
    #[serde(default = "default_max_midi_ports")]
    pub max_midi_ports: usize,
    /// Default per-device event rate limit in events/sec (ADR-009 D9). Default: 10000
    #[serde(default = "default_max_events_per_sec")]
    pub max_events_per_sec: u32,
    /// Input mode: MidiOnly, GamepadOnly, or Both. Default: Both
    #[serde(default)]
    pub input_mode: InputMode,
    /// Dead zone for analog sticks as a fraction (0.0-1.0). Default: 0.1 (10%)
    #[serde(default = "default_stick_deadzone")]
    pub stick_deadzone: f32,
    /// Dead zone for analog triggers as a fraction (0.0-1.0). Default: 0.1 (10%)
    #[serde(default = "default_trigger_deadzone")]
    pub trigger_deadzone: f32,
    /// Global enable/disable switch for SysEx Universal Device
    /// Identity probing. Default: `true`.
    ///
    /// When set to `false`, SysEx identity probing is disabled for
    /// every entry point gated by this setting (auto-on-bind,
    /// manual probe tools, GUI Identify button). See ADR-026 and
    /// `docs/sysex-device-identity/` for rollout and integration
    /// details.
    #[serde(default = "default_sysex_identity_probing")]
    pub sysex_identity_probing: bool,
    /// Auto-probe each newly-bound MIDI port on connect.
    /// Default: `true`.
    ///
    /// Independent of `sysex_identity_probing` so users can keep
    /// manual probing available while disabling just the
    /// auto-on-bind background task. The global flag wins:
    /// `sysex_identity_probing = false` disables probing
    /// regardless of this setting. See ADR-026 D6.
    #[serde(default = "default_probe_on_connect")]
    pub probe_on_connect: bool,
    /// Policy applied to Shell actions whose resolved binary is a
    /// known interpreter (sh, bash, python, ruby, perl, node, awk,
    /// lua, tclsh, php — see [`InterpreterFamily`]).
    ///
    /// Default: [`InterpreterPolicy::Warn`] — the validator emits a
    /// warning at config load surfacing the interpreter invocation.
    /// Power users who deliberately want shell scripting can opt into
    /// [`InterpreterPolicy::Allow`] to silence the warning;
    /// security-paranoid deployments can use [`InterpreterPolicy::Deny`]
    /// to reject any config that invokes an interpreter (including via
    /// `env`/`sudo`/`nice`/`nohup` wrappers — the policy applies to the
    /// effective binary after wrapper-chain resolution per ADR-027 D3
    /// §3.2).
    ///
    /// [`InterpreterFamily`]: crate::security::InterpreterFamily
    #[serde(default)]
    pub allow_interpreters: InterpreterPolicy,
    /// When `false` (the default), suppress all incoming MIDI on a
    /// port for [`Self::cascade_ttl_ms`] milliseconds after a
    /// `SendMidi` or `MidiForward` action sends to that port. This
    /// is broader than the per-message echo guard (ADR-015 D8 /
    /// `MidiRecursionGuard`, which fingerprints exact bytes) — it
    /// suppresses any MIDI input that arrives shortly after output,
    /// blocking the cross-note cascade case where mapping A sends
    /// note 63 and mapping B is triggered by note 63 looping back.
    ///
    /// Set `true` to opt in to cascades — useful for setups that
    /// deliberately chain mappings through MIDI routing. Only the
    /// per-message echo guard runs in that mode.
    #[serde(default)]
    pub allow_cascade: bool,
    /// TTL window in milliseconds for the [`Self::allow_cascade`]
    /// blanket suppression. Default: 100ms (matches the existing
    /// `MidiRecursionGuard` per-message TTL). Ignored when
    /// `allow_cascade = true`.
    ///
    /// Values larger than 60 000 (60 seconds) are silently clamped to
    /// 60 s at runtime by `MidiRecursionGuard::set_blanket_suppression`
    /// (`BLANKET_TTL_MAX_MS`). The clamp exists because cascade
    /// suppression is a tight-loop guard — minute-scale port muting is
    /// almost certainly a misconfiguration, and the bound keeps the
    /// `Instant + Duration` arithmetic well clear of overflow even
    /// for adversarial config values. If you genuinely need >60 s of
    /// suppression, that's a different feature.
    #[serde(default = "default_cascade_ttl_ms")]
    pub cascade_ttl_ms: u64,
    /// Maximum route-dispatch chain depth before the re-entrancy guard
    /// drops a route output (ADR-036 D4.3). A route's destination can be
    /// another route's source (fan-out chains); this bounds how many hops
    /// a single input event may traverse, catching cycles the static
    /// A→B+B→A validator can't (e.g. A→B→C→A) without a full graph walk.
    /// Default: 8.
    #[serde(default = "default_max_route_depth")]
    pub max_route_depth: usize,
    /// Capacity of the daemon's in-memory dispatch-trace ring buffer
    /// (ADR-036 §8 / spec §10 Open Item #3). Each routed event records one
    /// ~500-byte entry; the oldest is evicted when full. Default: 1000
    /// (≈500 KB). Validation rejects `0` and values above
    /// [`MAX_TRACE_BUFFER_SIZE`] (1_000_000) — see `validation.rs`.
    #[serde(default = "default_trace_buffer_size")]
    pub trace_buffer_size: usize,
    /// Poll interval (ms) for focused-window-title detection (ADR-040 §4.3).
    /// Decoupled from the frontmost-app poll. Default 500ms; values
    /// below the safe floor [`MIN_WINDOW_TITLE_POLL_MS`] (100ms) are clamped up
    /// at the poller to avoid hammering the Accessibility API. Only consulted
    /// when `[per_app_modes].window_rules` are present (lazy — no title poller,
    /// and no OS permission prompt, otherwise).
    #[serde(default = "default_window_title_poll_ms")]
    pub window_title_poll_ms: u64,
}

/// Safe floor (ms) for [`AdvancedSettings::window_title_poll_ms`]. The title
/// poller clamps any smaller value up to this, so a typo like `window_title_poll_ms = 1`
/// can't spin the Accessibility API (ADR-040 §4.3 "safe floor 100ms").
pub const MIN_WINDOW_TITLE_POLL_MS: u64 = 100;

/// Upper bound for [`AdvancedSettings::trace_buffer_size`]. 1,000,000
/// entries at ~500 bytes each ≈ 500 MB — far past any legitimate
/// observability need and a clear misconfiguration above this. Enforced
/// in `validation.rs`.
pub const MAX_TRACE_BUFFER_SIZE: usize = 1_000_000;

/// Policy applied to Shell actions whose resolved binary is a known
/// interpreter family (ADR-027 D3 §3.2, Phase 2).
///
/// `#[non_exhaustive]` so future policy granularity (e.g. per-family
/// allowlists, plan-and-confirm requirement) can be added additively.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum InterpreterPolicy {
    /// Allow interpreter invocations without diagnostic — explicit
    /// opt-in for users who deliberately rely on shell scripting.
    Allow,
    /// Default. Emit a validation warning at config load when an
    /// interpreter is detected; the config still loads. Surfaces the
    /// new gate without breaking existing configs.
    #[default]
    Warn,
    /// Reject the config at load with a validation error. For
    /// security-paranoid deployments that should not permit shell
    /// scripting via Shell actions.
    Deny,
}

impl Default for AdvancedSettings {
    fn default() -> Self {
        Self {
            chord_timeout_ms: default_chord_timeout_ms(),
            chord_learn_timeout_ms: default_chord_learn_timeout_ms(),
            double_tap_timeout_ms: default_double_tap_timeout_ms(),
            hold_threshold_ms: default_hold_threshold_ms(),
            short_press_ms: default_short_press_ms(),
            listen_mode: ListenMode::default(),
            ignore_ports: Vec::new(),
            max_midi_ports: default_max_midi_ports(),
            max_events_per_sec: default_max_events_per_sec(),
            input_mode: InputMode::default(),
            stick_deadzone: default_stick_deadzone(),
            trigger_deadzone: default_trigger_deadzone(),
            sysex_identity_probing: default_sysex_identity_probing(),
            probe_on_connect: default_probe_on_connect(),
            allow_interpreters: InterpreterPolicy::default(),
            allow_cascade: false,
            cascade_ttl_ms: default_cascade_ttl_ms(),
            max_route_depth: default_max_route_depth(),
            trace_buffer_size: default_trace_buffer_size(),
            window_title_poll_ms: default_window_title_poll_ms(),
        }
    }
}

fn default_chord_timeout_ms() -> u64 {
    50
}

/// Default MIDI Learn chord window — the historical hardcoded value, now
/// a config default so Learn and normal windows are both daemon-owned.
fn default_chord_learn_timeout_ms() -> u64 {
    150
}

fn default_window_title_poll_ms() -> u64 {
    500
}

fn default_double_tap_timeout_ms() -> u64 {
    300
}

fn default_hold_threshold_ms() -> u64 {
    2000
}

/// Default Short→Medium press boundary — the historical
/// `event_processor::SHORT_PRESS_MS` constant, now a config default.
fn default_short_press_ms() -> u64 {
    200
}

fn default_stick_deadzone() -> f32 {
    crate::gamepad_events::DEFAULT_STICK_DEADZONE
}

fn default_trigger_deadzone() -> f32 {
    crate::gamepad_events::DEFAULT_TRIGGER_DEADZONE
}

fn default_max_midi_ports() -> usize {
    32
}

fn default_max_events_per_sec() -> u32 {
    10_000
}

/// MIDI cascade-suppression TTL in milliseconds. 100ms
/// matches the existing per-message echo guard's window, giving the
/// blanket and fingerprint paths a single intuitive timing knob.
fn default_cascade_ttl_ms() -> u64 {
    100
}

fn default_max_route_depth() -> usize {
    8
}

/// Dispatch-trace ring buffer capacity default (ADR-036 §8 / spec §10).
/// 1000 entries ≈ 500 KB resident.
fn default_trace_buffer_size() -> usize {
    1000
}

/// SysEx Universal Device Identity probing default (ADR-026 D6).
/// On by default.
fn default_sysex_identity_probing() -> bool {
    true
}

/// Probe-on-connect default (ADR-026 D6). On by default.
fn default_probe_on_connect() -> bool {
    true
}

/// Input mode selection for device management
///
/// Determines which input protocols are active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
pub enum InputMode {
    /// Use MIDI device only
    MidiOnly,
    /// Use gamepad/HID device only
    GamepadOnly,
    /// Use both MIDI and gamepad simultaneously (default for best compatibility)
    #[default]
    Both,
}

/// A mode defines a set of mappings that can be switched between at runtime
///
/// Each mode has its own mapping set and optional visual identifier (color).
/// Users can switch between modes using special triggers (e.g., encoder rotation).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Mode {
    /// Mode name (used for mode switching triggers)
    pub name: String,
    /// Optional color for visual identification (e.g., "blue", "green", "#FF0000")
    pub color: Option<String>,
    /// Mappings active only in this mode
    #[serde(default)]
    pub mappings: Vec<Mapping>,
}

/// A mapping connects a MIDI trigger to an action
///
/// When a trigger is detected, the associated action is executed.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Mapping {
    /// The MIDI trigger that activates this mapping
    pub trigger: Trigger,
    /// The action to execute when the trigger is detected
    pub action: ActionConfig,
    /// Optional human-readable description of this mapping
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// ADR-038: fire the action AND let the event continue to the route stage.
    ///
    /// Does NOT cause the event to match other mappings — first-match-wins is
    /// preserved. `let_through` is metadata on the winning mapping, consumed only
    /// at the event pump's route-disposition gate. Default `false` (swallow, the
    /// pre-ADR-038 behaviour).
    ///
    /// Skipped from serialization when `false` so a default mapping serializes
    /// byte-identically to a pre-ADR-038 config — keeping the feature purely
    /// additive and the canonical-serialise golden hash stable. (Deviation from
    /// the spec's literal `#[serde(default)]`-only attribute; see
    /// `docs/let-through/ADR-038-implementation-spec.md` §4.1.)
    #[serde(default, skip_serializing_if = "is_false")]
    pub let_through: bool,
}

/// `skip_serializing_if` predicate for boolean fields that default to `false`
/// and should be omitted from serialized output in the default case.
///
/// Takes `&bool` because serde's `skip_serializing_if` requires `fn(&T) -> bool`;
/// the `trivially_copy_pass_by_ref` pedantic lint is therefore expected and allowed.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(b: &bool) -> bool {
    !*b
}

/// MIDI message type filter for `Trigger::Raw` (ADR-030 D3)
///
/// Restricts which MIDI message types a Raw trigger matches. When the
/// filter list is empty, all MIDI message types match.
///
/// Distinct from `crate::actions::MidiMessageType`, which describes the
/// payload of a `SendMidi` action (different variant set: `ControlChange`
/// vs. `CC`, no `ChannelPressure`/`SysEx`). Kept separate to avoid
/// breaking action serialization while giving Raw filters their own
/// vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub enum MidiMessageType {
    NoteOn,
    NoteOff,
    CC,
    ProgramChange,
    Aftertouch,
    PitchBend,
    ChannelPressure,
    /// Polyphonic aftertouch. Distinct from `Aftertouch`
    /// (channel-wide) because Raw filters / overlap detection
    /// must be able to discriminate per-note pressure from
    /// channel-pressure events.
    PolyAftertouch,
    SysEx,
}

/// MIDI trigger types
///
/// Defines different ways a MIDI message can activate a mapping.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum Trigger {
    /// Basic note trigger with optional velocity threshold
    ///
    /// # Examples
    /// ```toml
    /// [trigger]
    /// type = "Note"
    /// note = 60
    /// velocity_min = 1
    /// ```
    Note {
        /// MIDI note number (0-127)
        note: u8,
        /// Minimum velocity to trigger (0-127), None = any velocity
        velocity_min: Option<u8>,
        /// MIDI channel filter (0-indexed: 0-15), None = match any channel
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel: Option<u8>,
        /// Device filter: only match events from this device alias (ADR-009)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },

    /// Velocity-sensitive trigger with different actions per velocity level
    ///
    /// Classifies note presses into soft, medium, and hard based on velocity thresholds.
    /// Used with `VelocityRange` action type for velocity-dependent behavior.
    VelocityRange {
        /// MIDI note number (0-127)
        note: u8,
        /// Maximum velocity for soft (default 40), velocities below this are soft
        soft_max: Option<u8>,
        /// Maximum velocity for medium (default 80), velocities below this are medium (after soft_max)
        medium_max: Option<u8>,
        /// MIDI channel filter (0-indexed: 0-15), None = match any channel
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel: Option<u8>,
        /// Device filter (ADR-009)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },

    /// Long press detection (hold threshold in ms)
    ///
    /// Triggers when a note is held for longer than the specified duration.
    LongPress {
        /// MIDI note number (0-127)
        note: u8,
        /// Duration in milliseconds to trigger long press (default 2000ms)
        duration_ms: Option<u64>,
        /// MIDI channel filter (0-indexed: 0-15), None = match any channel
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel: Option<u8>,
        /// Device filter (ADR-009)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },

    /// Double-tap detection
    ///
    /// Triggers when a note is pressed and released quickly twice within a time window.
    DoubleTap {
        /// MIDI note number (0-127)
        note: u8,
        /// Time window in milliseconds for detecting double-tap (default 300ms)
        timeout_ms: Option<u64>,
        /// MIDI channel filter (0-indexed: 0-15), None = match any channel
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel: Option<u8>,
        /// Device filter (ADR-009)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },

    /// Chord detection (multiple notes pressed simultaneously)
    ///
    /// Triggers when all specified notes are pressed within a narrow time window.
    NoteChord {
        /// List of MIDI note numbers that form this chord
        notes: Vec<u8>,
        /// Time window in milliseconds for detecting simultaneous presses (default 50ms)
        timeout_ms: Option<u64>,
        /// MIDI channel filter (0-indexed: 0-15), None = match any channel
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel: Option<u8>,
        /// Device filter (ADR-009)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },

    /// Encoder turn with direction
    ///
    /// Triggers on continuous controller (CC) messages from encoder/knob rotation.
    /// Can filter by direction (clockwise/counter-clockwise) or respond to both.
    EncoderTurn {
        /// Control Change number (0-127)
        cc: u8,
        /// Direction filter: "Clockwise", "CounterClockwise", or None for either
        direction: Option<String>,
        /// MIDI channel filter (0-indexed: 0-15), None = match any channel
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel: Option<u8>,
        /// Device filter (ADR-009)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },

    /// Aftertouch/pressure sensitivity
    ///
    /// Triggers based on channel pressure (aftertouch) values.
    Aftertouch {
        /// Minimum pressure value to trigger (0-127)
        pressure_min: Option<u8>,
        /// MIDI channel filter (0-indexed: 0-15), None = match any channel
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel: Option<u8>,
        /// Device filter (ADR-009)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },

    /// Polyphonic aftertouch (per-note pressure).
    ///
    /// Distinct from `Aftertouch` (channel-wide). Matches MIDI
    /// status `0xA0` events on a SPECIFIC note. Native to MPE
    /// controllers (Roli Seaboard, Linnstrument, MPK Mini Plus).
    PolyAftertouch {
        /// Note number (0-127) the trigger fires on. Required —
        /// channel-wide poly aftertouch is meaningless; the
        /// whole point of poly is per-note discrimination.
        note: u8,
        /// Minimum pressure value to trigger (0-127). `None`
        /// fires on any pressure (including 0 / finger-release).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pressure_min: Option<u8>,
        /// MIDI channel filter (0-indexed: 0-15), `None` = any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel: Option<u8>,
        /// Device filter (ADR-009).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },

    /// Pitch bend
    ///
    /// Triggers based on pitch bend messages from touch strips or pitch bend wheels.
    PitchBend {
        /// Minimum value range (0-16383)
        value_min: Option<u16>,
        /// Maximum value range (0-16383)
        value_max: Option<u16>,
        /// MIDI channel filter (0-indexed: 0-15), None = match any channel
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel: Option<u8>,
        /// Device filter (ADR-009)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },

    /// Control Change (generic CC)
    ///
    /// Triggers on any control change message matching the specified CC number.
    CC {
        /// Control Change number (0-127)
        cc: u8,
        /// Minimum value to trigger (0-127)
        value_min: Option<u8>,
        /// MIDI channel filter (0-indexed: 0-15), None = match any channel
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel: Option<u8>,
        /// Device filter (ADR-009)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },

    /// Program Change (ADR-025 Phase 1)
    ///
    /// Triggers on PC messages. Primary use case is multi-function
    /// expression pedals (FCB1010-style) that send a PC on stomp and
    /// then send CCs that should route differently per preset.
    ProgramChange {
        /// Specific program number (0-127), or `None` to match any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pc: Option<u8>,
        /// MIDI channel filter (0-indexed: 0-15), `None` = any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel: Option<u8>,
        /// Device filter.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },

    // ===== Gamepad Triggers =====
    /// Gamepad button press
    ///
    /// Triggers when a gamepad button is pressed. Button IDs use the range 128-255
    /// to avoid conflicts with MIDI note numbers (0-127).
    ///
    /// # Examples
    /// ```toml
    /// [trigger]
    /// type = "GamepadButton"
    /// button = 128  # South button (A/Cross/B)
    /// velocity_min = 1
    /// ```
    GamepadButton {
        /// Gamepad button ID (128-255)
        /// Face buttons: 128-131 (South/East/West/North)
        /// D-Pad: 132-135 (Up/Down/Left/Right)
        /// Shoulders: 136-137 (L1/R1)
        /// Stick clicks: 138-139 (L3/R3)
        /// Menu buttons: 140-142 (Start/Select/Guide)
        /// Trigger buttons: 143-144 (L2/R2 digital)
        button: u8,
        /// Minimum velocity to trigger (0-127), None = any velocity
        velocity_min: Option<u8>,
        /// Device filter (ADR-009)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },

    /// Gamepad button chord (multiple buttons pressed simultaneously)
    ///
    /// Triggers when all specified gamepad buttons are pressed within a narrow time window.
    /// Similar to NoteChord but for gamepad buttons.
    ///
    /// # Examples
    /// ```toml
    /// [trigger]
    /// type = "GamepadButtonChord"
    /// buttons = [128, 129]  # South + East (A+B / Cross+Circle)
    /// timeout_ms = 50
    /// ```
    GamepadButtonChord {
        /// List of gamepad button IDs that form this chord (128-255)
        buttons: Vec<u8>,
        /// Time window in milliseconds for detecting simultaneous presses (default 50ms)
        timeout_ms: Option<u64>,
        /// Device filter (ADR-009)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },

    /// Gamepad analog stick movement
    ///
    /// Triggers on analog stick axis movement. Axis IDs use the range 128-131:
    /// - 128: Left stick X-axis
    /// - 129: Left stick Y-axis
    /// - 130: Right stick X-axis
    /// - 131: Right stick Y-axis
    ///
    /// # Examples
    /// ```toml
    /// [trigger]
    /// type = "GamepadAnalogStick"
    /// axis = 128  # Left stick X-axis
    /// direction = "Clockwise"  # Moving right
    /// ```
    GamepadAnalogStick {
        /// Analog stick axis ID (128-131)
        axis: u8,
        /// Direction filter: "Clockwise" (right/up), "CounterClockwise" (left/down), or None for either
        direction: Option<String>,
        /// Device filter (ADR-009)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },

    /// Gamepad analog trigger pull
    ///
    /// Triggers on analog trigger (L2/R2) pull. Trigger IDs:
    /// - 132: Left trigger (L2/LT)
    /// - 133: Right trigger (R2/RT)
    ///
    /// # Examples
    /// ```toml
    /// [trigger]
    /// type = "GamepadTrigger"
    /// trigger = 132  # Left trigger (L2/LT)
    /// threshold = 64  # Minimum pull value (0-127)
    /// ```
    GamepadTrigger {
        /// Analog trigger ID (132-133)
        trigger: u8,
        /// Minimum pull value to trigger (0-127), None = any value
        threshold: Option<u8>,
        /// Device filter (ADR-009)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },

    /// OSC message with an exact address match (ADR-039-A).
    ///
    /// Matches an inbound OSC message whose address equals `address` exactly.
    /// OSC-origin events carry a network-listener taint: sensitive actions
    /// (`Shell`/`Launch`/`Keystroke`, incl. statically nested) are refused +
    /// audited unless the originating endpoint sets
    /// `allow_sensitive_actions = true` (ADR-042 D17).
    ///
    /// # Examples
    /// ```toml
    /// [trigger]
    /// type = "OscMessage"
    /// address = "/eos/go"
    /// ```
    OscMessage {
        /// Exact OSC address (must start with '/').
        address: String,
        /// Device filter: the OSC listener endpoint alias (recommended —
        /// without it the trigger matches messages from any OSC listener).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },

    /// OSC address pattern trigger (ADR-039-A).
    ///
    /// Native OSC 1.0 wildcards — `?` (one char), `*` (within a `/` part),
    /// `[a-z]`/`[!…]` (char class), `{a,b}` (alternation) — NOT regex.
    /// Validated and compiled at config-load (`osc_pattern::OscPattern`).
    ///
    /// # Examples
    /// ```toml
    /// [trigger]
    /// type = "OscAddressPattern"
    /// pattern = "/eos/fader/*"
    /// ```
    OscAddressPattern {
        /// OSC 1.0 address pattern (must start with '/').
        pattern: String,
        /// Device filter: the OSC listener endpoint alias.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },

    /// OSC argument range trigger (ADR-039-A).
    ///
    /// Matches any inbound OSC message whose argument at `arg_index` is
    /// numeric (Int or Float) and within `min..=max`. Combine with a
    /// `device` filter to scope to one listener; for address + value
    /// conditions prefer an `OscAddressPattern` mapping whose action is
    /// `Conditional`.
    OscArgRange {
        /// Zero-based argument index.
        arg_index: usize,
        /// Inclusive lower bound.
        min: f32,
        /// Inclusive upper bound.
        max: f32,
        /// Device filter: the OSC listener endpoint alias.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },
}

impl Trigger {
    /// Get the device filter from any trigger variant (ADR-009)
    pub fn device(&self) -> Option<&String> {
        match self {
            Trigger::Note { device, .. }
            | Trigger::VelocityRange { device, .. }
            | Trigger::LongPress { device, .. }
            | Trigger::DoubleTap { device, .. }
            | Trigger::NoteChord { device, .. }
            | Trigger::EncoderTurn { device, .. }
            | Trigger::Aftertouch { device, .. }
            | Trigger::PolyAftertouch { device, .. }
            | Trigger::PitchBend { device, .. }
            | Trigger::CC { device, .. }
            | Trigger::ProgramChange { device, .. }
            | Trigger::GamepadButton { device, .. }
            | Trigger::GamepadButtonChord { device, .. }
            | Trigger::GamepadAnalogStick { device, .. }
            | Trigger::GamepadTrigger { device, .. }
            | Trigger::OscMessage { device, .. }
            | Trigger::OscAddressPattern { device, .. }
            | Trigger::OscArgRange { device, .. } => device.as_ref(),
        }
    }

    /// Set the device filter on any trigger variant (used for alias cascading).
    pub fn set_device(&mut self, new_device: Option<String>) {
        match self {
            Trigger::Note { device, .. }
            | Trigger::VelocityRange { device, .. }
            | Trigger::LongPress { device, .. }
            | Trigger::DoubleTap { device, .. }
            | Trigger::NoteChord { device, .. }
            | Trigger::EncoderTurn { device, .. }
            | Trigger::Aftertouch { device, .. }
            | Trigger::PolyAftertouch { device, .. }
            | Trigger::PitchBend { device, .. }
            | Trigger::CC { device, .. }
            | Trigger::ProgramChange { device, .. }
            | Trigger::GamepadButton { device, .. }
            | Trigger::GamepadButtonChord { device, .. }
            | Trigger::GamepadAnalogStick { device, .. }
            | Trigger::GamepadTrigger { device, .. }
            | Trigger::OscMessage { device, .. }
            | Trigger::OscAddressPattern { device, .. }
            | Trigger::OscArgRange { device, .. } => *device = new_device,
        }
    }

    /// Get the MIDI channel filter from any trigger variant.
    /// Returns None for gamepad triggers (they have no MIDI channel).
    pub fn channel(&self) -> Option<u8> {
        match self {
            Trigger::Note { channel, .. }
            | Trigger::VelocityRange { channel, .. }
            | Trigger::LongPress { channel, .. }
            | Trigger::DoubleTap { channel, .. }
            | Trigger::NoteChord { channel, .. }
            | Trigger::EncoderTurn { channel, .. }
            | Trigger::Aftertouch { channel, .. }
            | Trigger::PolyAftertouch { channel, .. }
            | Trigger::PitchBend { channel, .. }
            | Trigger::CC { channel, .. }
            | Trigger::ProgramChange { channel, .. } => *channel,
            // Gamepad and OSC triggers have no MIDI channel
            Trigger::GamepadButton { .. }
            | Trigger::GamepadButtonChord { .. }
            | Trigger::GamepadAnalogStick { .. }
            | Trigger::GamepadTrigger { .. }
            | Trigger::OscMessage { .. }
            | Trigger::OscAddressPattern { .. }
            | Trigger::OscArgRange { .. } => None,
        }
    }

    /// Returns `true` iff `self`'s match set is a (non-strict) superset
    /// of `other`'s — i.e., every event that fires `other` also fires `self`.
    /// Used by the validator to detect shadowed mappings: if mapping A appears
    /// before mapping B in the same mode and `A.trigger.shadows(&B.trigger)`,
    /// B will never fire because the rule engine matches first-match-wins.
    ///
    /// **Conservative scope.** Only the four trigger types most often
    /// involved in shadow bugs are analysed (Note, CC, Aftertouch,
    /// PolyAftertouch). Cross-type pairs and unanalyzed variants
    /// (LongPress, DoubleTap, NoteChord, EncoderTurn, PitchBend,
    /// ProgramChange, Gamepad*, Raw) return `false` rather than risk a
    /// false-positive warning. Follow-up subset analysis for the remaining
    /// variants can extend this method without changing the validator
    /// wiring.
    pub fn shadows(&self, other: &Trigger) -> bool {
        // device_match: self's filter is broader iff it accepts everything
        // (None) or matches the same alias other requires.
        fn device_covers(broad: Option<&String>, strict: Option<&String>) -> bool {
            match (broad, strict) {
                (None, _) => true,
                (Some(_), None) => false,
                (Some(a), Some(b)) => a == b,
            }
        }
        // channel_match: same shape as device.
        fn channel_covers(broad: Option<u8>, strict: Option<u8>) -> bool {
            match (broad, strict) {
                (None, _) => true,
                (Some(_), None) => false,
                (Some(a), Some(b)) => a == b,
            }
        }
        // numeric-min cover: A covers B iff A's threshold is ≤ B's, treating
        // None as 0 (matches anything ≥ 0). E.g. Note{velocity_min=None}
        // covers Note{velocity_min=80}; Note{velocity_min=10} covers
        // Note{velocity_min=10} but NOT Note{velocity_min=5}.
        fn min_covers(broad: Option<u8>, strict: Option<u8>) -> bool {
            broad.unwrap_or(0) <= strict.unwrap_or(0)
        }
        match (self, other) {
            (
                Trigger::Note {
                    note: a_note,
                    velocity_min: a_vmin,
                    channel: a_ch,
                    device: a_dev,
                },
                Trigger::Note {
                    note: b_note,
                    velocity_min: b_vmin,
                    channel: b_ch,
                    device: b_dev,
                },
            ) => {
                a_note == b_note
                    && min_covers(*a_vmin, *b_vmin)
                    && channel_covers(*a_ch, *b_ch)
                    && device_covers(a_dev.as_ref(), b_dev.as_ref())
            }
            (
                Trigger::CC {
                    cc: a_cc,
                    value_min: a_vmin,
                    channel: a_ch,
                    device: a_dev,
                },
                Trigger::CC {
                    cc: b_cc,
                    value_min: b_vmin,
                    channel: b_ch,
                    device: b_dev,
                },
            ) => {
                a_cc == b_cc
                    && min_covers(*a_vmin, *b_vmin)
                    && channel_covers(*a_ch, *b_ch)
                    && device_covers(a_dev.as_ref(), b_dev.as_ref())
            }
            (
                Trigger::Aftertouch {
                    pressure_min: a_pmin,
                    channel: a_ch,
                    device: a_dev,
                },
                Trigger::Aftertouch {
                    pressure_min: b_pmin,
                    channel: b_ch,
                    device: b_dev,
                },
            ) => {
                min_covers(*a_pmin, *b_pmin)
                    && channel_covers(*a_ch, *b_ch)
                    && device_covers(a_dev.as_ref(), b_dev.as_ref())
            }
            (
                Trigger::PolyAftertouch {
                    note: a_note,
                    pressure_min: a_pmin,
                    channel: a_ch,
                    device: a_dev,
                },
                Trigger::PolyAftertouch {
                    note: b_note,
                    pressure_min: b_pmin,
                    channel: b_ch,
                    device: b_dev,
                },
            ) => {
                a_note == b_note
                    && min_covers(*a_pmin, *b_pmin)
                    && channel_covers(*a_ch, *b_ch)
                    && device_covers(a_dev.as_ref(), b_dev.as_ref())
            }
            // Cross-type pairs and unanalyzed variants are conservatively NOT
            // flagged as shadowing in v1.
            _ => false,
        }
    }
}

/// Action configuration types
///
/// Defines different actions that can be executed when a trigger is detected.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum ActionConfig {
    /// Simulate keyboard keystroke(s) with optional modifiers
    ///
    /// # Examples
    /// ```toml
    /// [action]
    /// type = "Keystroke"
    /// keys = "space"
    /// modifiers = ["cmd"]
    /// ```
    Keystroke {
        /// Key name or sequence (e.g., "space", "Return", "Escape")
        keys: String,
        /// Modifier keys (e.g., "cmd", "shift", "alt", "ctrl")
        #[serde(default)]
        modifiers: Vec<String>,
    },

    /// Type a text string
    ///
    /// Simulates typing the provided text character by character.
    Text {
        /// Text to type
        text: String,
    },

    /// Launch an application
    ///
    /// Attempts to open the specified application by name or path.
    Launch {
        /// Application name or path to executable
        app: String,
    },

    /// Execute a shell command
    ///
    /// Runs an arbitrary shell command. Be cautious with untrusted config files.
    ///
    /// Two schema shapes (ADR-027 D3 §3.1):
    ///
    /// - **Legacy single-string** (`command = "echo hello world"`, `args` omitted):
    ///   the executor whitespace-splits `command` into argv at run time.
    ///   Kept for backward compatibility — every existing config in the
    ///   wild uses this shape.
    /// - **Argv form** (`command = "/bin/sh"`, `args = ["-c", "..."]`):
    ///   `command` is the resolved binary; `args` is argv[1..], passed
    ///   straight to `Command::args` (the spawn produces an OS argv of
    ///   `[command] ++ args` — i.e. `command` is itself argv[0]; the
    ///   caller does NOT repeat it inside `args`). No whitespace
    ///   tokenisation, no parser-defined quote handling, no
    ///   `parse_command_line`.
    ///
    /// `args = Some(vec![])` is distinct from `args = None` — `Some([])`
    /// means "argv-form invocation with zero arguments", `None` means
    /// "legacy whitespace-split form". Round-trips through serde
    /// preserve the distinction; the serialiser omits `args` entirely
    /// for `None` (no `args = null` leak that would confuse diff
    /// rendering or downstream tooling).
    ///
    /// **Validator behaviour.** Config validation extends the same
    /// shell-metacharacter blocklist that `command` already gets to
    /// every entry of `args` — so an argv form like
    /// `command = "/bin/sh", args = ["-c", "env > /tmp/leak"]`
    /// deserialises successfully (the schema accepts the shape) but
    /// will be rejected at config load with a `Shell argument contains
    /// dangerous pattern '>'` error. Argv form does **not** unlock
    /// redirects, pipes, command chaining, or substitution — Phase 2's
    /// `allow_interpreters` policy adds the additional guard against
    /// explicit interpreter invocation. See
    /// `conductor-core/src/config/validation.rs::validate_shell_arg`
    /// for the exact blocklist applied to args.
    Shell {
        /// Shell command — legacy form: full command line including
        /// args; argv form: resolved binary path (use `args` for the
        /// actual arguments).
        command: String,
        /// Argv array (argv form only). When `Some`, the executor passes
        /// these directly to `Command::args` and skips
        /// `parse_command_line`. When `None`, the legacy whitespace-split
        /// path runs against `command`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        args: Option<Vec<String>>,
        /// Per-action timeout in milliseconds (ADR-027 D7).
        /// `None` falls back to `DEFAULT_SHELL_TIMEOUT_MS` (30s). The
        /// validator clamps to [1000, 300000] to keep the watchdog
        /// useful (sub-second timeouts kill kid-script shells, multi-
        /// minute timeouts defeat the purpose).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
        /// Per-action OS-sandbox profile override (ADR-027 §D10b). When
        /// present, widens the default deny-write / deny-network confinement
        /// (macOS Seatbelt / Linux Landlock) for this action only. `None`
        /// uses the default profile.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sandbox: Option<ShellSandboxConfig>,
    },

    /// Execute a sequence of actions in order
    ///
    /// Executes multiple actions sequentially (useful for complex behaviors).
    Sequence {
        /// List of actions to execute in order
        actions: Vec<ActionConfig>,
    },

    /// Delay for a specified duration (in milliseconds)
    ///
    /// Pauses execution for the given duration. Useful in sequences.
    Delay {
        /// Delay duration in milliseconds
        ms: u64,
    },

    /// Simulate mouse click
    ///
    /// Clicks at the current or specified location with the specified button.
    MouseClick {
        /// Mouse button: "left", "right", "middle"
        button: String,
        /// X coordinate (optional, uses current mouse position if not specified)
        x: Option<i32>,
        /// Y coordinate (optional, uses current mouse position if not specified)
        y: Option<i32>,
    },

    /// Control system volume
    ///
    /// Adjusts or sets the system volume.
    VolumeControl {
        /// Operation: "Up", "Down", "Mute", "Unmute", "Set"
        operation: String,
        /// Volume level (0-100) for "Set" operation
        #[serde(default)]
        value: Option<u8>,
    },

    /// Switch to a different mode
    ///
    /// Changes the active mapping mode by name.
    ModeChange {
        /// Name of the mode to switch to
        mode: String,
    },

    /// Repeat an action multiple times
    ///
    /// Executes the specified action the given number of times.
    Repeat {
        /// Action to repeat
        action: Box<ActionConfig>,
        /// Number of times to repeat
        count: usize,
        /// Optional delay in milliseconds between repetitions
        #[serde(default)]
        delay_ms: Option<u64>,
    },

    /// Conditional action execution
    ///
    /// Executes different actions based on a condition.
    /// Supports time-based, app-based, mode-based conditions and logical operators.
    Conditional {
        /// Condition to evaluate at runtime
        condition: Condition,
        /// Action to execute if condition is true
        then_action: Box<ActionConfig>,
        /// Optional action to execute if condition is false
        #[serde(default)]
        else_action: Option<Box<ActionConfig>>,
    },

    // `PcContextSwitch.mappings` below uses a helper to round-trip
    // `IndexMap<u8, ...>` through TOML string keys. See
    // [`string_keyed_pc_map`] at the bottom of this file.
    /// Program-change context switch (ADR-025 Phase 2.D).
    ///
    /// Dispatches to one of several inner actions based on the most-
    /// recently-observed Program Change on the given `(device, channel)`
    /// tuple. Config-layer sugar — lowers at compile time (task #24) to
    /// a nested `Action::Conditional` chain keyed by `ActivePcIs`, or
    /// to a specialised `Action::ContextSwitchTable` when the branch
    /// count exceeds `MAX_LINEAR_BRANCHES` (task #25).
    ///
    /// Intended use: one-pedal-many-functions routing for MIDI foot
    /// controllers like the Behringer FCB1010 — a single expression-
    /// pedal CC drives different `SendMidi` actions per preset stomp.
    PcContextSwitch {
        /// MIDI channel of the target device to watch for PC.
        channel: u8,
        /// Device alias or binding ref whose physical state is read.
        device: String,
        /// Per-PC branches. `IndexMap` preserves TOML authoring order
        /// so earlier branches take priority after lowering.
        #[serde(with = "string_keyed_pc_map")]
        mappings: indexmap::IndexMap<u8, Box<ActionConfig>>,
        /// Fallback action when the active PC matches no branch and
        /// no PC has been observed yet. Omit for no-op fallback.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default: Option<Box<ActionConfig>>,
    },

    /// CC-value-range context switch (ADR-025 Phase 2.D).
    ///
    /// Dispatches to one of several inner actions based on the most-
    /// recently-observed value of a given CC. Each range is an
    /// inclusive `[min, max]` window; the first matching range wins
    /// after lowering. Intended use: zoned controllers (modwheel,
    /// expression pedal soft/hard zones, ribbon controllers).
    CcContextSwitch {
        /// CC number whose value is consulted (0-127).
        cc: u8,
        /// MIDI channel of the target device to watch for the CC.
        channel: u8,
        /// Device alias or binding ref whose physical state is read.
        device: String,
        /// Ordered list of value ranges and their actions. Validator
        /// (task #26) will flag overlapping ranges and min > max.
        ranges: Vec<CcRange>,
        /// Fallback action when no range matches and no CC observed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default: Option<Box<ActionConfig>>,
    },

    /// Send MIDI message
    ///
    /// Sends a MIDI message to a virtual or physical output port.
    /// Supports Note, CC, Program Change, Pitch Bend, and Aftertouch messages.
    SendMidi {
        /// Target MIDI output port name
        port: String,
        /// MIDI message type: "NoteOn", "NoteOff", "CC", "ProgramChange", "PitchBend", "Aftertouch"
        message_type: String,
        /// MIDI channel (0-15)
        channel: u8,
        /// Note number (0-127) for Note messages
        #[serde(default)]
        note: Option<u8>,
        /// Velocity (0-127) for Note messages
        #[serde(default)]
        velocity: Option<u8>,
        /// Controller number (0-127) for CC messages
        #[serde(default)]
        controller: Option<u8>,
        /// Controller value (0-127) for CC messages
        #[serde(default)]
        value: Option<u8>,
        /// Program number (0-127) for Program Change messages
        #[serde(default)]
        program: Option<u8>,
        /// Pitch bend value (-8192 to +8191) for Pitch Bend messages
        #[serde(default)]
        pitch: Option<i16>,
        /// Aftertouch pressure (0-127) for Aftertouch messages
        #[serde(default)]
        pressure: Option<u8>,
    },

    /// Forward MIDI data to an output port with optional transform (ADR-009 Gap 2)
    ///
    /// Passes raw MIDI bytes from the triggering event through an optional
    /// transform and sends them to the named output port.
    MidiForward {
        /// Target MIDI output port name
        target: String,
        /// Optional transform to apply before forwarding
        #[serde(default)]
        transform: Option<MidiTransform>,
    },

    /// Forward a gamepad (HID) event to a cross-protocol output endpoint
    /// (ADR-039-B).
    ///
    /// The mapping-triggered analogue of a HID route: where a route forwards
    /// a gamepad input endpoint unconditionally, `HidForward` fires only when
    /// its mapping's trigger condition is met (e.g. a long-press or chord on a
    /// gamepad button), then translates the *structured* triggering event to
    /// the target's protocol and sends it.
    ///
    /// `transform` is REQUIRED (unlike `MidiForward`'s optional MIDI→MIDI
    /// passthrough): a HID event cannot exist on a MIDI wire without
    /// translation, and the gamepad→MIDI byte serialization is lossy
    /// (button 128 → note 0), so an explicit structured transform is
    /// mandatory. **V1 forwards to a MIDI output only** — the transform must
    /// be `HidToMidi` and `target` must resolve to a MIDI output endpoint,
    /// validated at config-load. `HidToOsc`/`HidToArtNet` via an *action* are
    /// rejected at load: HID→OSC/Art-Net is route-only for now (OSC-by-alias
    /// needs output-endpoint resolution the action executor does not carry,
    /// and there is no Art-Net output capability yet). V1 is strictly
    /// per-event.
    HidForward {
        /// Target MIDI output endpoint alias.
        target: String,
        /// Structured HID→MIDI transform (`HidToMidi`). Required; other
        /// variants are rejected at config-load in V1.
        transform: SignalTransform,
    },

    /// Forward an inbound OSC message to an OSC **output** endpoint
    /// (ADR-039-A).
    ///
    /// The mapping-triggered analogue of an OSC route: fires only when its
    /// mapping's typed OSC trigger matches, then re-sends the *triggering*
    /// OSC message (address + args) to the `target` OSC output endpoint by
    /// alias. **Gated at dispatch, not load**: the executor needs the inbound
    /// `OscInbound` from the trigger context, so a MIDI/HID-triggered mapping
    /// (which has none) is a benign runtime no-op rather than a load error.
    /// `target` must resolve to an OSC **output** endpoint (checked at load).
    ///
    /// V1 is **pass-through**: the message is forwarded verbatim. `transform`
    /// is reserved for a future OSC→OSC address/arg remap and MUST be `None`
    /// in V1 (a non-`None` value is rejected at config-load) — mirroring how
    /// `HidForward` V1 restricts its transform.
    ///
    /// Not a sensitive action class (ADR-042 D17): it emits an OSC packet,
    /// not a host effect. It still rides the network-origin taint —
    /// an OSC-origin `OscForward` is fine; the taint gates sensitive actions,
    /// not packet forwarding.
    OscForward {
        /// Target OSC output endpoint alias.
        target: String,
        /// Reserved OSC→OSC transform. Must be `None` in V1.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transform: Option<SignalTransform>,
    },

    /// Send an OSC message over UDP (ADR-009 Gap H)
    ///
    /// # Examples
    /// ```toml
    /// [action]
    /// type = "OscSend"
    /// host = "127.0.0.1"
    /// port = 9000
    /// address = "/track/1/volume"
    /// args = [
    ///   { type = "Float", value = 0.75 },
    /// ]
    /// ```
    OscSend {
        /// Target host (e.g. "127.0.0.1")
        host: String,
        /// Target UDP port (e.g. 9000)
        port: u16,
        /// OSC address pattern (e.g. "/track/1/volume")
        address: String,
        /// OSC arguments
        #[serde(default)]
        args: Vec<crate::actions::OscArg>,
    },

    /// Execute a plugin action
    ///
    /// Runs a WASM plugin by name with optional parameters.
    /// The plugin must be installed and enabled in the plugin manager.
    ///
    /// # Examples
    /// ```toml
    /// [action]
    /// type = "Plugin"
    /// plugin = "spotify-control"
    /// params = { command = "play_pause" }
    /// ```
    Plugin {
        /// Plugin identifier (must match installed plugin name)
        plugin: String,
        /// Plugin-specific parameters
        #[serde(default)]
        params: serde_json::Value,
    },

    /// Observation sugar (ADR-038 §4.1).
    ///
    /// Carries a `message` template (with `{value}`/`{note}`/`{cc}`/`{velocity}`
    /// substitution) and completes with no signal side-effect. Pair with
    /// `let_through = true` to observe an event without consuming it.
    ///
    /// **Current behaviour:** the daemon executor only debug-logs the raw
    /// template. The substitution and event-stream / trace emission described
    /// above are not yet implemented — until then, `Tap` is side-effect-free
    /// beyond the debug log.
    ///
    /// # Examples
    /// ```toml
    /// [action]
    /// type = "Tap"
    /// message = "note {note} velocity {velocity}"
    /// ```
    Tap {
        /// Template emitted on each match. Supports `{value}`, `{note}`, `{cc}`,
        /// and `{velocity}` substitution (not yet resolved by the Tap executor).
        message: String,
    },
}

/// A single `[min, max]` inclusive range in a [`ActionConfig::CcContextSwitch`]
/// action, together with the action to dispatch when the watched CC value
/// falls in that window.
///
/// Ordering matters: the first matching range wins after lowering (task #24).
/// The validator (task #26) will flag overlapping ranges and `min > max`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CcRange {
    /// Inclusive lower bound (0-127).
    pub min: u8,
    /// Inclusive upper bound (0-127).
    pub max: u8,
    /// Action to dispatch when the watched CC value is in `[min, max]`.
    pub action: Box<ActionConfig>,
}

/// Serde helper for `IndexMap<u8, Box<ActionConfig>>` in
/// [`ActionConfig::PcContextSwitch::mappings`].
///
/// TOML table keys are always strings, so a naive `IndexMap<u8, ...>`
/// would reject `[mappings.12]` with `"invalid type: string, expected u8"`.
/// This module serialises u8 keys as their decimal string and parses
/// them back on the way in, while preserving insertion order via
/// `IndexMap` — critical for the ordering contract that earlier PC
/// branches win after lowering (task #24).
///
/// JSON round-trips the same way, so MCP tool arguments and chat
/// persistence (tasks #27, #28) see the same shape.
mod string_keyed_pc_map {
    use super::ActionConfig;
    use indexmap::IndexMap;
    use serde::{
        Deserializer, Serializer,
        de::{self, MapAccess, Visitor},
        ser::SerializeMap,
    };
    use std::fmt;

    pub(super) fn serialize<S>(
        map: &IndexMap<u8, Box<ActionConfig>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut m = serializer.serialize_map(Some(map.len()))?;
        for (k, v) in map {
            m.serialize_entry(&k.to_string(), v)?;
        }
        m.end()
    }

    pub(super) fn deserialize<'de, D>(
        deserializer: D,
    ) -> Result<IndexMap<u8, Box<ActionConfig>>, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct PcMapVisitor;
        impl<'de> Visitor<'de> for PcMapVisitor {
            type Value = IndexMap<u8, Box<ActionConfig>>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a map whose keys are PC numbers (0..=127) as strings")
            }
            fn visit_map<M>(self, mut access: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut out = IndexMap::with_capacity(access.size_hint().unwrap_or(0));
                while let Some(key) = access.next_key::<String>()? {
                    // u8::from_str accepts 0..=255, but MIDI Program Change
                    // is unambiguously 0..=127 by spec. Reject out-of-range
                    // keys at the deserialisation boundary so config authors
                    // get a clear error with the offending key on the same
                    // line as the TOML/JSON location, rather than having it
                    // slip past parsing and surface later (or not at all)
                    // via the structural validator (task #26).
                    let parsed: u16 = key.parse().map_err(|_| {
                        de::Error::custom(format!(
                            "invalid PC key '{}': must be an integer 0-127",
                            key
                        ))
                    })?;
                    if parsed > 127 {
                        return Err(de::Error::custom(format!(
                            "PC key '{}' out of range: must be 0-127 (MIDI Program Change spec)",
                            key
                        )));
                    }
                    let pc = parsed as u8;
                    let value: Box<ActionConfig> = access.next_value()?;
                    // Reject normalized-duplicate keys. `1` and `01` are
                    // DISTINCT TOML keys (so TOML's own duplicate-key check
                    // doesn't fire) but both normalize to PC 1. A plain
                    // `insert` would silently drop one authored branch and leave
                    // the routing table out of sync with the config text, so
                    // fail loudly naming both the key and the normalized PC.
                    if out.insert(pc, value).is_some() {
                        return Err(de::Error::custom(format!(
                            "duplicate PC key '{}' normalizes to Program Change {} which is \
                             already mapped (e.g. '1' and '01' are distinct TOML keys but the \
                             same PC); use each PC number at most once",
                            key, pc
                        )));
                    }
                }
                Ok(out)
            }
        }
        deserializer.deserialize_map(PcMapVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR-035: the legacy `[device]` block is no longer a Config field, so
    /// serde silently ignores it (no `deny_unknown_fields`). A config that
    /// still carries one parses fine; only `[[endpoints]]`/modes/etc. are read.
    #[test]
    fn test_config_deserialize_ignores_legacy_device_block() {
        let toml_str = r#"
[device]
name = "Test Device"
auto_connect = true

[[modes]]
name = "Default"
color = "blue"

[[modes.mappings]]
description = "Test mapping"

[modes.mappings.trigger]
type = "Note"
note = 60
velocity_min = 1

[modes.mappings.action]
type = "Keystroke"
keys = "space"
modifiers = ["cmd"]
"#;

        let config: Config = toml::from_str(toml_str).expect("Failed to parse config");
        assert_eq!(config.modes.len(), 1);
        assert_eq!(config.modes[0].name, "Default");
    }

    /// ListenMode default is All — listen-first so all hardware is visible
    #[test]
    fn test_listen_mode_default_is_all() {
        assert_eq!(ListenMode::default(), ListenMode::All);
    }

    /// AdvancedSettings default uses All listen_mode
    #[test]
    fn test_advanced_settings_default_listen_mode() {
        let settings = AdvancedSettings::default();
        assert_eq!(settings.listen_mode, ListenMode::All);
    }

    /// spec §10 Open Item #3: trace_buffer_size defaults to 1000 and is
    /// populated by serde when omitted from a config document.
    #[test]
    fn test_advanced_settings_chord_learn_timeout_ms() {
        // Learn-mode chord window is its own config field (default 150ms,
        // the historical hardcoded daemon value) so the daemon is the single
        // source of truth — not a UI `chord_timeout_ms × 3` fiction.
        assert_eq!(AdvancedSettings::default().chord_learn_timeout_ms, 150);
        // Omitted in TOML → serde default fills it in (independent of the
        // normal-mode chord window).
        let parsed: AdvancedSettings = toml::from_str("chord_timeout_ms = 500").unwrap();
        assert_eq!(parsed.chord_learn_timeout_ms, 150);
        // Present in TOML → honoured.
        let parsed: AdvancedSettings = toml::from_str("chord_learn_timeout_ms = 220").unwrap();
        assert_eq!(parsed.chord_learn_timeout_ms, 220);
    }

    #[test]
    fn test_advanced_settings_default_trace_buffer_size() {
        assert_eq!(AdvancedSettings::default().trace_buffer_size, 1000);
        // Omitted in TOML → serde default fills it in.
        let parsed: AdvancedSettings = toml::from_str("chord_timeout_ms = 50").unwrap();
        assert_eq!(parsed.trace_buffer_size, 1000);
        // Present in TOML → honoured.
        let parsed: AdvancedSettings = toml::from_str("trace_buffer_size = 256").unwrap();
        assert_eq!(parsed.trace_buffer_size, 256);
    }

    /// Cascade-suppression defaults — `allow_cascade = false`
    /// so cross-note feedback is blocked out of the box; users opt in
    /// when they deliberately chain mappings via MIDI routing. The TTL
    /// matches the existing per-message echo guard (100ms).
    #[test]
    fn test_advanced_settings_default_cascade_suppression() {
        let settings = AdvancedSettings::default();
        assert!(
            !settings.allow_cascade,
            "default must be `false` so cascades are blocked out of the box"
        );
        assert_eq!(settings.cascade_ttl_ms, 100);
    }

    /// `allow_cascade` and `cascade_ttl_ms` round-trip cleanly through
    /// TOML serde — both fields can be omitted (defaults apply) or set
    /// explicitly without affecting other settings.
    #[test]
    fn test_advanced_settings_cascade_serde_roundtrip() {
        // Omitted in TOML → defaults
        let toml_default: AdvancedSettings = toml::from_str("").unwrap();
        assert!(!toml_default.allow_cascade);
        assert_eq!(toml_default.cascade_ttl_ms, 100);

        // Explicitly set in TOML → values applied
        let toml_explicit: AdvancedSettings =
            toml::from_str("allow_cascade = true\ncascade_ttl_ms = 250").unwrap();
        assert!(toml_explicit.allow_cascade);
        assert_eq!(toml_explicit.cascade_ttl_ms, 250);
    }

    // ─────────────────────────────────────────────────────────────────
    // ADR-026 Phase 3.C.1 — SysEx identity probing flags
    // ─────────────────────────────────────────────────────────────────

    /// Both flags default to `true` so probe-on-connect is enabled
    /// out of the box per ADR-026 D6 ("default-on, settings-gated"). 3.C.2
    /// will plug the actual probing logic into these gates.
    #[test]
    fn test_advanced_settings_default_sysex_identity_probing_is_on() {
        let settings = AdvancedSettings::default();
        assert!(settings.sysex_identity_probing);
        assert!(settings.probe_on_connect);
    }

    /// Configs that omit the flags inherit the on-by-default
    /// behaviour. Existing user TOML files (which never had these
    /// fields) MUST keep working unchanged.
    #[test]
    fn test_config_omitting_sysex_flags_uses_on_defaults() {
        let toml_str = r#"
[[modes]]
name = "Default"
"#;
        let config: Config = toml::from_str(toml_str).expect("parse");
        assert!(config.advanced_settings.sysex_identity_probing);
        assert!(config.advanced_settings.probe_on_connect);
    }

    /// Users can disable identity probing globally via
    /// `sysex_identity_probing = false` (the kill-switch — Phase 4
    /// surfaces this as a Settings UI toggle). 3.C.2's wiring will
    /// short-circuit when this is off.
    #[test]
    fn test_config_can_disable_sysex_identity_probing_globally() {
        let toml_str = r#"
[advanced_settings]
sysex_identity_probing = false

[[modes]]
name = "Default"
"#;
        let config: Config = toml::from_str(toml_str).expect("parse");
        assert!(!config.advanced_settings.sysex_identity_probing);
        // Probe-on-connect default still on — users can disable
        // *just* the auto-on-bind flow without disabling identity
        // probing entirely (e.g. they want manual probes only).
        assert!(config.advanced_settings.probe_on_connect);
    }

    /// `probe_on_connect = false` keeps SysEx probing available but
    /// stops the auto-on-bind background task from firing. Users
    /// invoke probes manually via the GUI Identify button (Phase
    /// 3.D) or the MCP tool.
    #[test]
    fn test_config_can_disable_only_probe_on_connect() {
        let toml_str = r#"
[advanced_settings]
probe_on_connect = false

[[modes]]
name = "Default"
"#;
        let config: Config = toml::from_str(toml_str).expect("parse");
        assert!(config.advanced_settings.sysex_identity_probing);
        assert!(!config.advanced_settings.probe_on_connect);
    }

    /// Roundtrip: serialise an AdvancedSettings with the flags
    /// flipped, parse it back, ensure the flags survive. Pins the
    /// serde field names so the migration to TOML doesn't silently
    /// rename to `sysexIdentityProbing` etc. (which would break
    /// every existing config the moment the field is added).
    #[test]
    fn test_advanced_settings_sysex_flags_serde_roundtrip() {
        let settings = AdvancedSettings {
            sysex_identity_probing: false,
            probe_on_connect: false,
            ..AdvancedSettings::default()
        };
        let toml_str = toml::to_string(&settings).expect("serialise");
        // Spot-check the serialised TOML uses snake_case names.
        assert!(
            toml_str.contains("sysex_identity_probing = false"),
            "expected snake_case field in serialised TOML; got:\n{}",
            toml_str
        );
        assert!(
            toml_str.contains("probe_on_connect = false"),
            "expected snake_case field in serialised TOML; got:\n{}",
            toml_str
        );
        let parsed: AdvancedSettings = toml::from_str(&toml_str).expect("re-parse");
        assert!(!parsed.sysex_identity_probing);
        assert!(!parsed.probe_on_connect);
    }

    /// Config with no listen_mode uses All default
    #[test]
    fn test_config_default_listen_mode() {
        let toml_str = r#"
[[modes]]
name = "Default"
"#;
        let config: Config = toml::from_str(toml_str).expect("parse");
        assert_eq!(config.advanced_settings.listen_mode, ListenMode::All);
    }

    /// Explicit listen_mode = "All" is accepted (matches default)
    #[test]
    fn test_config_explicit_listen_mode_all() {
        let toml_str = r#"
[advanced_settings]
listen_mode = "All"

[[modes]]
name = "Default"
"#;
        let config: Config = toml::from_str(toml_str).expect("parse");
        assert_eq!(config.advanced_settings.listen_mode, ListenMode::All);
    }

    /// Explicit listen_mode = "Configured" overrides All default
    #[test]
    fn test_config_explicit_listen_mode_configured() {
        let toml_str = r#"
[advanced_settings]
listen_mode = "Configured"

[[modes]]
name = "Default"
"#;
        let config: Config = toml::from_str(toml_str).expect("parse");
        assert_eq!(config.advanced_settings.listen_mode, ListenMode::Configured);
    }

    #[test]
    fn test_trigger_note() {
        let trigger = Trigger::Note {
            note: 60,
            velocity_min: Some(1),
            channel: None,
            device: None,
        };
        assert!(matches!(trigger, Trigger::Note { note: 60, .. }));
    }

    #[test]
    fn test_action_keystroke() {
        let action = ActionConfig::Keystroke {
            keys: "space".to_string(),
            modifiers: vec!["cmd".to_string()],
        };
        assert!(matches!(action, ActionConfig::Keystroke { .. }));
    }

    #[test]
    fn test_action_plugin_toml_roundtrip() {
        let toml_str = r#"
type = "Plugin"
plugin = "spotify-control"

[params]
command = "play_pause"
"#;
        let action: ActionConfig = toml::from_str(toml_str).expect("parse Plugin action");
        match &action {
            ActionConfig::Plugin { plugin, params } => {
                assert_eq!(plugin, "spotify-control");
                assert_eq!(params["command"], "play_pause");
            }
            _ => panic!("Expected Plugin variant"),
        }

        // Roundtrip
        let serialized = toml::to_string(&action).expect("serialize Plugin action");
        let deserialized: ActionConfig = toml::from_str(&serialized).expect("re-parse");
        assert!(matches!(deserialized, ActionConfig::Plugin { .. }));
    }

    #[test]
    fn test_action_plugin_no_params() {
        let toml_str = r#"
type = "Plugin"
plugin = "my-plugin"
"#;
        let action: ActionConfig = toml::from_str(toml_str).expect("parse Plugin without params");
        match &action {
            ActionConfig::Plugin { plugin, params } => {
                assert_eq!(plugin, "my-plugin");
                assert!(params.is_null());
            }
            _ => panic!("Expected Plugin variant"),
        }
    }

    // ========== MidiForward Config Tests (ADR-009 Gap 2) ==========

    #[test]
    fn test_midi_forward_config_parse() {
        let toml_str = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "Forward CC to synth"

[modes.mappings.trigger]
type = "CC"
cc = 74

[modes.mappings.action]
type = "MidiForward"
target = "Synth Output"
"#;
        let config: Config = toml::from_str(toml_str).expect("Failed to parse MidiForward config");
        let action = &config.modes[0].mappings[0].action;
        match action {
            ActionConfig::MidiForward { target, transform } => {
                assert_eq!(target, "Synth Output");
                assert!(transform.is_none());
            }
            _ => panic!("Expected MidiForward action"),
        }
    }

    #[test]
    fn test_midi_forward_config_with_transform() {
        let toml_str = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "Forward with channel remap"

[modes.mappings.trigger]
type = "CC"
cc = 74

[modes.mappings.action]
type = "MidiForward"
target = "Synth Output"

[modes.mappings.action.transform]
channel = 5
cc = 1
velocity_scale = 1.5
invert_value = false
"#;
        let config: Config =
            toml::from_str(toml_str).expect("Failed to parse MidiForward with transform");
        let action = &config.modes[0].mappings[0].action;
        match action {
            ActionConfig::MidiForward { target, transform } => {
                assert_eq!(target, "Synth Output");
                let t = transform.as_ref().unwrap();
                assert_eq!(t.channel, Some(5));
                assert_eq!(t.cc, Some(1));
                assert_eq!(t.velocity_scale, Some(1.5));
                assert!(!t.invert_value);
            }
            _ => panic!("Expected MidiForward action"),
        }
    }

    #[test]
    fn test_midi_forward_action_conversion() {
        use crate::actions::Action;

        let config = ActionConfig::MidiForward {
            target: "Synth".to_string(),
            transform: None,
        };
        let action: Action = config.into();
        match action {
            Action::MidiForward { target, transform } => {
                assert_eq!(target, "Synth");
                assert!(transform.is_none());
            }
            _ => panic!("Expected MidiForward action"),
        }
    }

    // ========== OscSend Config Tests (ADR-009 Gap H) ==========

    #[test]
    fn test_osc_send_config_parse() {
        let toml_str = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
[modes.mappings.trigger]
type = "Note"
note = 36

[modes.mappings.action]
type = "OscSend"
host = "127.0.0.1"
port = 9000
address = "/track/1/volume"
args = [
  { type = "Float", value = 0.75 },
]
"#;
        let config: Config = toml::from_str(toml_str).expect("Failed to parse OscSend config");
        let action = &config.modes[0].mappings[0].action;
        match action {
            ActionConfig::OscSend {
                host,
                port,
                address,
                args,
            } => {
                assert_eq!(host, "127.0.0.1");
                assert_eq!(*port, 9000);
                assert_eq!(address, "/track/1/volume");
                assert_eq!(args.len(), 1);
                assert_eq!(args[0], crate::actions::OscArg::Float(0.75));
            }
            _ => panic!("Expected OscSend action"),
        }
    }

    #[test]
    fn test_osc_send_config_no_args() {
        let toml_str = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
[modes.mappings.trigger]
type = "Note"
note = 36

[modes.mappings.action]
type = "OscSend"
host = "127.0.0.1"
port = 8000
address = "/heartbeat"
"#;
        let config: Config = toml::from_str(toml_str).expect("Failed to parse OscSend config");
        let action = &config.modes[0].mappings[0].action;
        match action {
            ActionConfig::OscSend { args, .. } => {
                assert!(args.is_empty());
            }
            _ => panic!("Expected OscSend action"),
        }
    }

    #[test]
    fn test_osc_send_config_multiple_args() {
        let toml_str = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
[modes.mappings.trigger]
type = "Note"
note = 36

[modes.mappings.action]
type = "OscSend"
host = "10.0.0.1"
port = 7000
address = "/fx/param"
args = [
  { type = "Int", value = 1 },
  { type = "Float", value = 0.5 },
  { type = "String", value = "reverb" },
]
"#;
        let config: Config = toml::from_str(toml_str).expect("Failed to parse OscSend config");
        let action = &config.modes[0].mappings[0].action;
        match action {
            ActionConfig::OscSend { args, .. } => {
                assert_eq!(args.len(), 3);
                assert_eq!(args[0], crate::actions::OscArg::Int(1));
                assert_eq!(args[1], crate::actions::OscArg::Float(0.5));
                assert_eq!(
                    args[2],
                    crate::actions::OscArg::String("reverb".to_string())
                );
            }
            _ => panic!("Expected OscSend action"),
        }
    }

    // ========== LED Config Tests ==========

    #[test]
    fn test_led_config_parse() {
        let toml_str = r#"
[[modes]]
name = "Default"

[led]
enabled = true
brightness = 80
scheme = "rainbow"
idle_timeout_secs = 30

[led.mode_colors.Default]
r = 255
g = 0
b = 128
"#;
        let config: Config = toml::from_str(toml_str).expect("Failed to parse LED config");
        let led = config.led.unwrap();
        assert!(led.enabled);
        assert_eq!(led.brightness, 80);
        assert_eq!(led.scheme, "rainbow");
        assert_eq!(led.idle_timeout_secs, 30);
        let color = led.mode_colors.get("Default").unwrap();
        assert_eq!(color.r, 255);
        assert_eq!(color.g, 0);
        assert_eq!(color.b, 128);
    }

    #[test]
    fn test_led_config_defaults() {
        let toml_str = r#"
[[modes]]
name = "Default"

[led]
"#;
        let config: Config = toml::from_str(toml_str).expect("Failed to parse LED config");
        let led = config.led.unwrap();
        assert!(led.enabled);
        assert_eq!(led.brightness, 100);
        assert_eq!(led.scheme, "reactive");
        assert_eq!(led.idle_timeout_secs, 0);
        assert!(led.mode_colors.is_empty());
    }

    #[test]
    fn test_led_config_missing_backward_compat() {
        let toml_str = r#"
[[modes]]
name = "Default"
"#;
        let config: Config = toml::from_str(toml_str).expect("Failed to parse config");
        assert!(config.led.is_none());
    }

    #[test]
    fn test_led_config_roundtrip() {
        let led = LedConfig {
            enabled: true,
            brightness: 50,
            scheme: "breathing".to_string(),
            idle_timeout_secs: 60,
            mode_colors: {
                let mut m = std::collections::BTreeMap::new();
                m.insert("Default".to_string(), RgbColor { r: 0, g: 255, b: 0 });
                m
            },
            midi: None,
            hid: None,
            velocity_colors: None,
            default_fade_ms: None,
        };
        let toml_str = toml::to_string(&led).expect("serialize");
        let parsed: LedConfig = toml::from_str(&toml_str).expect("deserialize");
        assert_eq!(parsed.brightness, 50);
        assert_eq!(parsed.scheme, "breathing");
        assert_eq!(parsed.idle_timeout_secs, 60);
        assert_eq!(parsed.mode_colors.get("Default").unwrap().g, 255);
    }

    #[test]
    fn test_led_config_skip_serializing_none() {
        let config = Config {
            config_meta: Default::default(),
            endpoints: vec![],
            modes: vec![Mode {
                name: "Default".to_string(),
                color: None,
                mappings: vec![],
            }],
            led: None,
            ..Config::default_config()
        };
        let toml_str = toml::to_string(&config).expect("serialize");
        assert!(!toml_str.contains("[led]"));
    }

    #[test]
    fn test_rgb_color_conversion() {
        let rgb_color = RgbColor {
            r: 10,
            g: 20,
            b: 30,
        };
        let mikro_rgb: crate::mikro_leds::RGB = rgb_color.clone().into();
        assert_eq!(mikro_rgb.r, 10);
        assert_eq!(mikro_rgb.g, 20);
        assert_eq!(mikro_rgb.b, 30);

        let back: RgbColor = mikro_rgb.into();
        assert_eq!(back.r, 10);
        assert_eq!(back.g, 20);
        assert_eq!(back.b, 30);
    }

    #[test]
    fn test_rgb_to_velocity_colors() {
        let colors = MidiLedColors::default();

        // Pure colors
        assert_eq!(colors.rgb_to_velocity(255, 0, 0), colors.red);
        assert_eq!(colors.rgb_to_velocity(0, 255, 0), colors.green);
        assert_eq!(colors.rgb_to_velocity(0, 0, 255), colors.blue);
        assert_eq!(colors.rgb_to_velocity(0, 0, 0), colors.off);

        // Mixed colors
        assert_eq!(colors.rgb_to_velocity(200, 200, 0), colors.yellow);
        assert_eq!(colors.rgb_to_velocity(200, 180, 0), colors.yellow);
        assert_eq!(colors.rgb_to_velocity(100, 50, 50), colors.red);
        assert_eq!(colors.rgb_to_velocity(50, 100, 50), colors.green);
        assert_eq!(colors.rgb_to_velocity(50, 50, 100), colors.blue);

        // Edge cases — equal RGB is gray, not yellow (blue is not low enough)
        assert_eq!(colors.rgb_to_velocity(100, 100, 100), colors.amber);
        assert_eq!(colors.rgb_to_velocity(1, 0, 0), colors.red);
    }

    // ========== MIDI LED Config Tests ==========

    #[test]
    fn test_midi_led_config_defaults() {
        let config = MidiLedConfig::default();
        assert_eq!(config.channel, 1);
        assert_eq!(config.note_on_velocity, 127);
        assert_eq!(config.note_off_velocity, 0);
        assert_eq!(config.colors.red, 5);
        assert_eq!(config.colors.green, 21);
        assert_eq!(config.colors.yellow, 13);
        assert_eq!(config.colors.amber, 9);
        assert_eq!(config.colors.off, 0);
        assert!(config.custom_mappings.is_empty());
    }

    #[test]
    fn test_midi_led_config_parse() {
        let toml_str = r#"
[[modes]]
name = "Default"

[led]
enabled = true

[led.midi]
channel = 2
note_on_velocity = 100
note_off_velocity = 10

[led.midi.colors]
red = 3
green = 17
"#;
        let config: Config = toml::from_str(toml_str).expect("Failed to parse MIDI LED config");
        let midi = config.led.unwrap().midi.unwrap();
        assert_eq!(midi.channel, 2);
        assert_eq!(midi.note_on_velocity, 100);
        assert_eq!(midi.note_off_velocity, 10);
        assert_eq!(midi.colors.red, 3);
        assert_eq!(midi.colors.green, 17);
        // Defaults for unset fields
        assert_eq!(midi.colors.yellow, 13);
        assert_eq!(midi.colors.amber, 9);
    }

    #[test]
    fn test_midi_led_config_backward_compat() {
        let toml_str = r#"
[[modes]]
name = "Default"

[led]
brightness = 80
"#;
        let config: Config = toml::from_str(toml_str).expect("parse");
        assert!(config.led.unwrap().midi.is_none());
    }

    #[test]
    fn test_midi_led_custom_mapping_parse() {
        let toml_str = r#"
[[modes]]
name = "Default"

[led.midi]
channel = 1

[[led.midi.custom_mappings]]
pad = 36
led_on = { type = "note_on", note = 36, velocity = 127 }
led_off = { type = "note_off", note = 36, velocity = 0 }

[[led.midi.custom_mappings]]
pad = 37
led_on = { type = "cc", cc = 37, value = 127 }
led_off = { type = "cc", cc = 37, value = 0 }
"#;
        let config: Config = toml::from_str(toml_str).expect("parse custom mappings");
        let midi = config.led.unwrap().midi.unwrap();
        assert_eq!(midi.custom_mappings.len(), 2);
        assert_eq!(midi.custom_mappings[0].pad, 36);
        assert!(matches!(
            midi.custom_mappings[0].led_on,
            MidiLedMessage::NoteOn {
                note: 36,
                velocity: 127
            }
        ));
        assert!(matches!(
            midi.custom_mappings[1].led_on,
            MidiLedMessage::Cc { cc: 37, value: 127 }
        ));
    }

    #[test]
    fn test_midi_led_config_roundtrip() {
        let config = MidiLedConfig {
            channel: 3,
            note_on_velocity: 100,
            note_off_velocity: 5,
            colors: MidiLedColors::default(),
            custom_mappings: vec![MidiLedCustomMapping {
                pad: 40,
                led_on: MidiLedMessage::NoteOn {
                    note: 40,
                    velocity: 100,
                },
                led_off: MidiLedMessage::NoteOff {
                    note: 40,
                    velocity: 0,
                },
            }],
        };
        let json = serde_json::to_string(&config).expect("serialize");
        let parsed: MidiLedConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.channel, 3);
        assert_eq!(parsed.custom_mappings.len(), 1);
    }

    #[test]
    fn test_event_console_config_defaults() {
        let config = EventConsoleConfig::default();
        assert_eq!(config.buffer_size, 1000);
        assert_eq!(config.max_events_per_second, 0);
        assert!(config.capture_midi);
        assert!(config.capture_processed);
        assert!(config.capture_actions);
        assert!(config.filters.is_empty());
    }

    #[test]
    fn test_event_console_config_toml_roundtrip() {
        let toml_str = r#"
buffer_size = 5000
max_events_per_second = 30
capture_midi = true
capture_processed = false
capture_actions = true

[filters.pads_only]
description = "Only pad note events"
event_type = "note_on,note_off"
note_min = 36
note_max = 51
"#;
        let config: EventConsoleConfig = toml::from_str(toml_str).expect("parse");
        assert_eq!(config.buffer_size, 5000);
        assert_eq!(config.max_events_per_second, 30);
        assert!(config.capture_midi);
        assert!(!config.capture_processed);
        assert!(config.capture_actions);
        assert_eq!(config.filters.len(), 1);
        let pads = &config.filters["pads_only"];
        assert_eq!(pads.description.as_deref(), Some("Only pad note events"));
        assert_eq!(pads.event_type.as_deref(), Some("note_on,note_off"));
        assert_eq!(pads.note_min, Some(36));
        assert_eq!(pads.note_max, Some(51));
        assert!(pads.channel.is_none());
        assert!(pads.device_id.is_none());
    }

    #[test]
    fn test_event_console_config_with_triggers() {
        let toml_str = r#"
buffer_size = 1000
enable_profiling = true
track_latency = true
track_memory = true

[triggers.high_errors]
condition = "error_rate > 5 per_minute"
cooldown_secs = 120

[triggers.high_errors.action]
type = "Notification"
message = "High error rate detected!"

[triggers.event_flood]
condition = "event_count > 100 per_second"

[triggers.event_flood.action]
type = "log"
message = "Event flood warning"
"#;
        let config: EventConsoleConfig = toml::from_str(toml_str).expect("parse");
        assert!(config.enable_profiling);
        assert!(config.track_latency);
        assert!(config.track_memory);
        assert_eq!(config.triggers.len(), 2);

        let errors = &config.triggers["high_errors"];
        assert_eq!(errors.condition, "error_rate > 5 per_minute");
        assert_eq!(errors.cooldown_secs, Some(120));
        assert!(matches!(errors.action, TriggerAction::Notification { .. }));

        let flood = &config.triggers["event_flood"];
        assert_eq!(flood.condition, "event_count > 100 per_second");
        assert!(matches!(flood.action, TriggerAction::Log { .. }));
    }

    #[test]
    fn test_event_console_config_omitted_uses_defaults() {
        // When [event_console] is absent, Config should have None
        let toml_str = r#"
[[modes]]
name = "Default"
mappings = []
"#;
        let config: super::super::Config = toml::from_str(toml_str).expect("parse");
        assert!(config.event_console.is_none());
    }

    // ========== HID LED Config Tests ==========

    #[test]
    fn test_hid_led_config_all_fields_none_by_default() {
        let config = HidLedConfig::default();
        assert!(config.hid_profile.is_none());
        assert!(config.vendor_id.is_none());
        assert!(config.product_id.is_none());
        assert!(config.interface_number.is_none());
        assert!(config.led_report_id.is_none());
        assert!(config.buffer_size.is_none());
        assert!(config.pad_led_offset.is_none());
        assert!(config.pad_count.is_none());
        assert!(config.color_palette.is_none());
        assert!(config.pad_layout.is_none());
    }

    #[test]
    fn test_hid_led_config_mikro_mk3_profile_values() {
        let mk3 = HidLedConfig::mikro_mk3();
        assert_eq!(mk3.vendor_id, Some(0x17CC));
        assert_eq!(mk3.product_id, Some(0x1700));
        assert_eq!(mk3.interface_number, Some(0));
        assert_eq!(mk3.led_report_id, Some(0x80));
        assert_eq!(mk3.buffer_size, Some(80));
        assert_eq!(mk3.pad_led_offset, Some(39));
        assert_eq!(mk3.pad_count, Some(16));
        assert!(mk3.color_palette.as_ref().unwrap().len() >= 18);
        assert_eq!(mk3.pad_layout.as_ref().unwrap().len(), 16);
    }

    #[test]
    fn test_hid_led_config_from_profile_known() {
        assert!(HidLedConfig::from_profile("mikro-mk3").is_some());
    }

    #[test]
    fn test_hid_led_config_from_profile_unknown() {
        assert!(HidLedConfig::from_profile("nonexistent").is_none());
    }

    #[test]
    fn test_hid_led_config_resolve_profile_fills_defaults() {
        let config = HidLedConfig {
            hid_profile: Some("mikro-mk3".to_string()),
            ..Default::default()
        };
        let resolved = config.resolve_profile().unwrap();
        assert_eq!(resolved.vendor_id, 0x17CC);
        assert_eq!(resolved.product_id, 0x1700);
        assert_eq!(resolved.interface_number, 0);
        assert_eq!(resolved.led_report_id, 0x80);
        assert_eq!(resolved.buffer_size, 80);
        assert_eq!(resolved.pad_led_offset, 39);
        assert_eq!(resolved.pad_count, 16);
        assert!(!resolved.color_palette.is_empty());
        assert!(!resolved.pad_layout.is_empty());
    }

    #[test]
    fn test_hid_led_config_resolve_user_overrides_kept() {
        let config = HidLedConfig {
            hid_profile: Some("mikro-mk3".to_string()),
            vendor_id: Some(0xBEEF),
            // Override pad_count AND clear pad_layout (since profile's 16-entry
            // layout wouldn't match 8 pads)
            pad_count: Some(8),
            pad_layout: Some(vec![7, 6, 5, 4, 3, 2, 1, 0]),
            ..Default::default()
        };
        let resolved = config.resolve_profile().unwrap();
        assert_eq!(resolved.vendor_id, 0xBEEF); // user override kept
        assert_eq!(resolved.product_id, 0x1700); // from profile
        assert_eq!(resolved.pad_count, 8); // user override kept
        assert_eq!(resolved.pad_layout.len(), 8); // user override kept
    }

    #[test]
    fn test_hid_led_config_resolve_no_profile_requires_vendor_product() {
        let config = HidLedConfig::default(); // no profile, no vendor/product
        assert!(config.resolve_profile().is_err());
    }

    #[test]
    fn test_hid_led_config_resolve_explicit_works_without_profile() {
        let config = HidLedConfig {
            vendor_id: Some(0x1234),
            product_id: Some(0x5678),
            buffer_size: Some(100),
            pad_led_offset: Some(10),
            pad_count: Some(8),
            ..Default::default()
        };
        let resolved = config.resolve_profile().unwrap();
        assert_eq!(resolved.vendor_id, 0x1234);
        assert_eq!(resolved.product_id, 0x5678);
        assert_eq!(resolved.buffer_size, 100);
    }

    #[test]
    fn test_hid_led_config_resolve_unknown_profile_fails() {
        let config = HidLedConfig {
            hid_profile: Some("nonexistent".to_string()),
            ..Default::default()
        };
        assert!(config.resolve_profile().is_err());
    }

    #[test]
    fn test_hid_led_config_resolve_buffer_overflow_fails() {
        let config = HidLedConfig {
            vendor_id: Some(0x1234),
            product_id: Some(0x5678),
            buffer_size: Some(10),
            pad_led_offset: Some(5),
            pad_count: Some(8), // 5 + 8 = 13 > 10
            ..Default::default()
        };
        assert!(config.resolve_profile().is_err());
    }

    #[test]
    fn test_hid_led_config_resolve_pad_layout_duplicate_fails() {
        let config = HidLedConfig {
            vendor_id: Some(0x1234),
            product_id: Some(0x5678),
            buffer_size: Some(20),
            pad_led_offset: Some(0),
            pad_count: Some(4),
            pad_layout: Some(vec![0, 1, 1, 3]), // duplicate position 1
            ..Default::default()
        };
        assert!(config.resolve_profile().is_err());
    }

    #[test]
    fn test_hid_led_config_resolve_pad_layout_out_of_bounds_fails() {
        let config = HidLedConfig {
            vendor_id: Some(0x1234),
            product_id: Some(0x5678),
            buffer_size: Some(20),
            pad_led_offset: Some(0),
            pad_count: Some(4),
            pad_layout: Some(vec![0, 1, 2, 99]), // 99 >= pad_count
            ..Default::default()
        };
        assert!(config.resolve_profile().is_err());
    }

    #[test]
    fn test_hid_led_config_resolve_buffer_size_zero_fails() {
        // resolve_profile() itself rejects buffer_size=0, proving
        // any downstream check is redundant.
        let config = HidLedConfig {
            vendor_id: Some(0x1234),
            product_id: Some(0x5678),
            buffer_size: Some(0),
            pad_led_offset: Some(0),
            pad_count: Some(1),
            ..Default::default()
        };
        let err = config.resolve_profile().unwrap_err();
        assert!(
            err.contains("buffer_size must be > 0"),
            "expected buffer_size error, got: {}",
            err
        );
    }

    #[test]
    fn test_hid_led_config_toml_parse_profile() {
        let toml_str = r#"
[[modes]]
name = "Default"

[led.hid]
hid_profile = "mikro-mk3"
"#;
        let config: super::super::Config = toml::from_str(toml_str).expect("parse");
        let hid = config.led.unwrap().hid.unwrap();
        assert_eq!(hid.hid_profile.as_deref(), Some("mikro-mk3"));
    }

    #[test]
    fn test_hid_led_config_toml_parse_explicit() {
        let toml_str = r#"
[[modes]]
name = "Default"

[led.hid]
vendor_id = 6092
product_id = 5888
led_report_id = 128
buffer_size = 80
pad_led_offset = 39
pad_count = 16
"#;
        let config: super::super::Config = toml::from_str(toml_str).expect("parse");
        let hid = config.led.unwrap().hid.unwrap();
        assert_eq!(hid.vendor_id, Some(6092));
        assert_eq!(hid.product_id, Some(5888));
    }

    #[test]
    fn test_hid_led_config_backward_compat_no_hid_section() {
        let toml_str = r#"
[[modes]]
name = "Default"

[led]
brightness = 80
"#;
        let config: super::super::Config = toml::from_str(toml_str).expect("parse");
        assert!(config.led.unwrap().hid.is_none());
    }

    #[test]
    fn test_hid_led_config_skip_serializing_none() {
        let config = HidLedConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        assert_eq!(json, "{}");
    }

    #[test]
    fn test_mikro_mk3_pad_layout_vertical_flip() {
        let mk3 = HidLedConfig::mikro_mk3();
        let layout = mk3.pad_layout.unwrap();
        // MK3 pads are bottom-to-top logical, top-to-bottom physical
        // Pad 0 (bottom-left) → position 12 (4th row in physical)
        assert_eq!(layout[0], 12);
        // Pad 15 (top-right) → position 3 (1st row in physical)
        assert_eq!(layout[15], 3);
    }

    // ========== Velocity Color Map Tests ==========

    #[test]
    fn test_velocity_color_map_default_three_ranges() {
        let vcm = VelocityColorMap::default();
        assert_eq!(vcm.ranges.len(), 3);
        // soft = green (0-39)
        assert_eq!(vcm.ranges[0].min, 0);
        assert_eq!(vcm.ranges[0].max, 39);
        assert_eq!(vcm.ranges[0].color, RgbColor { r: 0, g: 255, b: 0 });
        // medium = yellow (40-79)
        assert_eq!(vcm.ranges[1].min, 40);
        assert_eq!(vcm.ranges[1].max, 79);
        assert_eq!(
            vcm.ranges[1].color,
            RgbColor {
                r: 255,
                g: 255,
                b: 0
            }
        );
        // hard = red (80-127)
        assert_eq!(vcm.ranges[2].min, 80);
        assert_eq!(vcm.ranges[2].max, 127);
        assert_eq!(vcm.ranges[2].color, RgbColor { r: 255, g: 0, b: 0 });
    }

    #[test]
    fn test_velocity_color_map_lookup_each_range() {
        let vcm = VelocityColorMap::default();
        // Soft range
        let c = vcm.color_for_velocity(0).unwrap();
        assert_eq!(c.g, 255);
        assert_eq!(c.r, 0);
        // Medium range
        let c = vcm.color_for_velocity(60).unwrap();
        assert_eq!(c.r, 255);
        assert_eq!(c.g, 255);
        // Hard range
        let c = vcm.color_for_velocity(127).unwrap();
        assert_eq!(c.r, 255);
        assert_eq!(c.g, 0);
    }

    #[test]
    fn test_velocity_color_map_lookup_no_match() {
        let vcm = VelocityColorMap {
            ranges: vec![VelocityRange {
                min: 10,
                max: 20,
                color: RgbColor { r: 255, g: 0, b: 0 },
            }],
        };
        assert!(vcm.color_for_velocity(5).is_none());
        assert!(vcm.color_for_velocity(25).is_none());
        assert!(vcm.color_for_velocity(15).is_some());
    }

    #[test]
    fn test_velocity_color_map_toml_parse() {
        let toml_str = r#"
[[modes]]
name = "Default"

[[led.velocity_colors.ranges]]
min = 0
max = 63
color = { r = 0, g = 255, b = 0 }

[[led.velocity_colors.ranges]]
min = 64
max = 127
color = { r = 255, g = 0, b = 0 }
"#;
        let config: super::super::Config = toml::from_str(toml_str).expect("parse");
        let led = config.led.unwrap();
        let vcm = led.velocity_colors.unwrap();
        assert_eq!(vcm.ranges.len(), 2);
        assert_eq!(vcm.ranges[0].max, 63);
        assert_eq!(vcm.ranges[1].min, 64);
    }

    #[test]
    fn test_led_config_default_fade_ms() {
        let toml_str = r#"
[[modes]]
name = "Default"

[led]
default_fade_ms = 500
"#;
        let config: super::super::Config = toml::from_str(toml_str).expect("parse");
        assert_eq!(config.led.unwrap().default_fade_ms, Some(500));
    }

    #[test]
    fn test_led_config_default_fade_ms_absent() {
        let toml_str = r#"
[[modes]]
name = "Default"

[led]
brightness = 100
"#;
        let config: super::super::Config = toml::from_str(toml_str).expect("parse");
        assert!(config.led.unwrap().default_fade_ms.is_none());
    }
}
