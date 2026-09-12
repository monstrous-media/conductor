// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Event console, logging, and advanced settings.

use super::*;

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
