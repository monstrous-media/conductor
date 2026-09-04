// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Core types for daemon operations

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::oneshot;

/// Event filter for real-time monitoring
///
/// Reusable filter that can be applied to `MonitorEvent` streams.
/// Used by both CLI (`conductorctl events`) and GUI (LiveEventConsole).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EventFilter {
    /// Filter by event type(s) — comma-separated (e.g., "note_on,note_off")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_type: Option<String>,
    /// Filter by MIDI channel (0-15)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<u8>,
    /// Filter by minimum note number (inclusive)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note_min: Option<u8>,
    /// Filter by maximum note number (inclusive)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note_max: Option<u8>,
    /// Filter by device ID
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    /// Filter events newer than this timestamp (milliseconds since epoch)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_ms: Option<u64>,
}

impl EventFilter {
    /// Check if a `MonitorEvent` matches this filter.
    /// Returns `true` if the event passes all filter criteria.
    pub fn matches(&self, event: &MonitorEvent) -> bool {
        // Event type filter
        if let Some(ref filter) = self.event_type
            && !filter.split(',').any(|f| f.trim() == event.event_type)
        {
            return false;
        }

        // Channel filter
        if let Some(ch) = self.channel {
            match event.channel {
                Some(event_ch) if event_ch == ch => {}
                Some(_) => return false,
                // If event has no channel info, don't filter it out
                None => {}
            }
        }

        // Note range filter
        if let Some(min) = self.note_min {
            match event.note {
                Some(n) if n >= min => {}
                Some(_) => return false,
                None => return false, // No note = doesn't match note range filter
            }
        }
        if let Some(max) = self.note_max {
            match event.note {
                Some(n) if n <= max => {}
                Some(_) => return false,
                None => return false,
            }
        }

        // Device ID filter
        if let Some(ref dev) = self.device_id {
            match &event.device_id {
                Some(event_dev) if event_dev == dev => {}
                Some(_) => return false,
                None => return false,
            }
        }

        // Time filter (R891)
        if let Some(since) = self.since_ms
            && event.timestamp_ms < since
        {
            return false;
        }

        true
    }
}

/// Event statistics for monitoring dashboard
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EventStats {
    /// Total events received
    pub total_events: u64,
    /// Events in the last second
    pub events_per_second: f64,
    /// Average velocity across note events
    pub avg_velocity: f64,
    /// Most frequently triggered note
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub most_active_note: Option<u8>,
    /// Count of the most active note
    pub most_active_note_count: u64,
    /// Error count
    pub error_count: u64,
}

/// Event for real-time event monitoring
///
/// A simplified, serializable event type for CLI monitoring.
/// Captures MIDI and gamepad events with device attribution.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MonitorEvent {
    /// Event timestamp in milliseconds since epoch
    pub timestamp_ms: u64,
    /// Event type string. Raw MIDI: "note_on", "note_off", "cc", "encoder",
    /// "pitch_bend", "aftertouch", "poly_pressure",
    /// "gamepad_button", "gamepad_button_release", "gamepad_axis", "gamepad_trigger".
    /// Processed gestures: "pad_pressed", "pad_released", "short_press",
    /// "medium_press", "long_press", "hold_detected", "double_tap",
    /// "chord_detected", "encoder_turn", "cc_received".
    /// Actions: "action_executed", "action_error", "mode_change".
    pub event_type: String,
    /// Source device identity (multi-device mode)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    /// MIDI channel (0-15) for raw MIDI events. `None` for non-channel events
    /// (gamepad, gestures, action results) and for variants where the source
    /// `InputEvent` happened to carry no channel.
    ///
    /// Populated by `EngineManager::create_monitor_event` for the seven MIDI
    /// `InputEvent` variants (PadPressed, PadReleased, ControlChange,
    /// EncoderTurned, PitchBend, Aftertouch, PolyPressure) when they are not
    /// re-routed to gamepad/HID surfaces by their pad/encoder ID range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<u8>,
    /// MIDI note number (0-127)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<u8>,
    /// MIDI velocity (0-127)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub velocity: Option<u8>,
    /// MIDI CC number (0-127)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cc: Option<u8>,
    /// MIDI CC or other value (0-127 for most events, 0-16383 for pitch bend)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<u16>,
    /// Gamepad button ID
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub button: Option<u8>,
    /// Gamepad axis ID
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub axis: Option<u8>,
    /// Raw analog value from HID input, before MIDI quantisation.
    /// "gamepad_axis": -1.0 to +1.0 (center 0.0).
    /// "gamepad_trigger": 0.0 to 1.0 (released 0.0).
    /// `None` for all other event types.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analog_value: Option<f32>,
    /// Human-readable detail/message (action results, errors, etc.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Structured payload for typed events (e.g., mapping_fired) (ADR-014)
    ///
    /// When present, contains the full structured data for the event type.
    /// Consumers should prefer this over parsing `detail` as JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    /// Processing time in microseconds (R919: track_latency)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub processing_us: Option<u64>,
    /// Resident memory in bytes at event time (R920: track_memory)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_bytes: Option<u64>,
    /// Canonical MIDI 1.0 wire bytes for raw channel-voice events, e.g.
    /// `[0xB0, 0x07, 0x4E]` for CC 7 = 78 on channel 1. Reconstructed from the
    /// parsed fields (the original `InputEvent` no longer carries the source
    /// bytes by the time it reaches monitoring), so it is byte-identical to the
    /// canonical form of the message rather than a literal capture (running
    /// status is expanded; note-on velocity 0 stays note-on). `None` for
    /// gamepad/gesture/action events and for channel-voice events whose source
    /// carried no channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_bytes: Option<Vec<u8>>,
    /// Monotonic emission sequence number stamped at `push_monitor_event` time.
    /// Gives a total order across every event type and both Tauri
    /// channels (`midi-events` + `mapping-fired`) so the GUI can render true
    /// daemon-emission order regardless of cross-channel delivery timing.
    /// `#[serde(default)]` keeps old payloads and the many
    /// `..Default::default()` construction sites working unchanged.
    #[serde(default)]
    pub seq: u64,
}

/// Validate a plugin name from IPC input
///
/// Plugin names must be non-empty and contain only alphanumeric chars, hyphens, underscores, and dots.
/// This prevents directory traversal and other injection attacks.
pub(crate) fn validate_plugin_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Missing 'name' parameter".into());
    }
    if name == "." || name == ".." || name.contains("..") {
        return Err(format!(
            "Invalid plugin name '{}': path traversal not allowed",
            name
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err(format!(
            "Invalid plugin name '{}': must contain only ASCII alphanumeric, hyphens, underscores, or dots",
            name
        ));
    }
    Ok(())
}

/// Validate a profile config path (Phase 1)
///
/// This function validates that the path is:
/// - Absolute
/// - Points to an existing file
/// - Has a .toml extension
/// - Can be canonicalized (resolves symlinks)
pub(crate) fn validate_profile_path(path_str: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(path_str);
    // Must be absolute
    if !path.is_absolute() {
        return Err("Profile config path must be absolute".into());
    }
    // Canonicalize (resolves symlinks, validates existence)
    let canonical =
        std::fs::canonicalize(&path).map_err(|e| format!("Profile config path invalid: {}", e))?;
    // Must be a file
    if !canonical.is_file() {
        return Err("Profile config path is not a file".into());
    }
    // Must be .toml — case-insensitive ASCII, matching the startup path's
    // `active_profile_config_path` contract. A profile named e.g.
    // `studio.TOML` resolves correctly at boot but was previously rejected here
    // at runtime (IPC / MCP / app-detection profile switch), an api-contract
    // mismatch.
    let is_toml = canonical
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"));
    if !is_toml {
        return Err("Profile config must be a .toml file".into());
    }
    Ok(canonical)
}

/// Active profile information (Phase 1)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveProfileInfo {
    /// The GUI's profile id (`profile-<timestamp>`), when the switch carried
    /// one (additive). `None` for the built-in Default and for
    /// legacy callers that only send name + path.
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    pub config_path: String,
}

/// Daemon lifecycle states
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LifecycleState {
    /// Initial state, loading configuration and connecting to devices
    Init,

    /// ADR-034 §D4.2 / D4.B.3 — **RESERVED, not reachable today.**
    ///
    /// Was intended as a startup idle mode: when no live config is found
    /// (neither `live.toml` nor `live.toml.known_good` parsed), the daemon
    /// would stay up and accept a small bootstrap IPC accept-list so an
    /// operator could bootstrap one over IPC. That boot path was never
    /// wired — nothing transitions the daemon into this state — so the
    /// contract was unreachable and is downgraded to reserved. A fresh
    /// install with no resolvable config now exits with a descriptive error
    /// (see `main.rs`) instead. The variant, its
    /// transitions, `IpcCommand::allowed_during_awaiting_config`, and
    /// `IpcErrorCode::DaemonAwaitingConfig` are retained as scaffolding so
    /// the mode can be reinstated without a wire-format break if
    /// headless/zero-touch provisioning becomes a real goal.
    AwaitingConfig,

    /// Starting up, initializing all components
    Starting,

    /// Running normally, processing events
    Running,

    /// Reloading configuration
    Reloading,

    /// Device disconnected, attempting to reconnect
    Degraded,

    /// ADR-034 §D8.2 / D4.C.1 — audit outbox has hit 8+
    /// consecutive flush failures and broken its hash chain.
    /// ConfigChange mutations reject with
    /// `IpcErrorCode::AuditUnavailable = 5004` until the operator
    /// runs `conductorctl audit resume`. ReadOnly IPCs still
    /// succeed — the daemon is functionally up, just unable to
    /// durably attest mutations. Distinct from `Degraded` (which
    /// is device-level): a daemon can be `AuditDegraded` while
    /// connected to all devices and vice versa. Transitions:
    /// `Running ↔ AuditDegraded`; either may go to `Stopping`.
    AuditDegraded,

    /// Attempting to reconnect to device
    Reconnecting,

    /// Shutting down gracefully
    Stopping,

    /// Stopped, daemon has exited
    Stopped,
}

impl std::fmt::Display for LifecycleState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Init => write!(f, "Init"),
            Self::AwaitingConfig => write!(f, "AwaitingConfig"),
            Self::Starting => write!(f, "Starting"),
            Self::Running => write!(f, "Running"),
            Self::Reloading => write!(f, "Reloading"),
            Self::Degraded => write!(f, "Degraded"),
            Self::AuditDegraded => write!(f, "AuditDegraded"),
            Self::Reconnecting => write!(f, "Reconnecting"),
            Self::Stopping => write!(f, "Stopping"),
            Self::Stopped => write!(f, "Stopped"),
        }
    }
}

impl LifecycleState {
    /// Check if a state transition is valid
    pub fn can_transition_to(&self, new_state: Self) -> bool {
        matches!(
            (self, new_state),
            (Self::Init, Self::Starting)
                // D4.B.3 — RESERVED: the AwaitingConfig idle mode
                // was never wired, so these edges are unreachable today.
                // Retained so the mode can be reinstated without a
                // transition-table change. (Would-be routing: startup load
                // failure → AwaitingConfig; `Init { source }` →
                // Starting; SIGTERM → Stopping.)
                | (Self::Init, Self::AwaitingConfig)
                | (Self::AwaitingConfig, Self::Starting)
                | (Self::AwaitingConfig, Self::Stopping)
                | (Self::Starting, Self::Running)
                | (Self::Starting, Self::Degraded) // Allow Starting → Degraded when device connection fails
                | (Self::Starting, Self::Stopping) // Allow Starting → Stopping for clean shutdown during startup
                | (Self::Running, Self::Reloading)
                | (Self::Running, Self::Degraded)
                | (Self::Running, Self::Stopping)
                | (Self::Reloading, Self::Running)
                | (Self::Reloading, Self::Degraded)
                | (Self::Reloading, Self::Reconnecting) // Device lost during config reload
                | (Self::Reloading, Self::Stopping) // Clean shutdown during config reload
                | (Self::Degraded, Self::Reconnecting)
                | (Self::Degraded, Self::Stopping)
                // D4.C.1: audit outbox lifecycle. Running
                // demotes to AuditDegraded on 8+ consecutive flush
                // failures; resumes back to Running after operator
                // runs `conductorctl audit resume`. Always allowed
                // to Stopping for clean shutdown from either state.
                | (Self::Running, Self::AuditDegraded)
                | (Self::AuditDegraded, Self::Running)
                | (Self::AuditDegraded, Self::Stopping)
                | (Self::Reconnecting, Self::Running)
                | (Self::Reconnecting, Self::Degraded)
                | (Self::Reconnecting, Self::Stopping) // Clean shutdown during reconnect
                | (Self::Stopping, Self::Stopped)
        )
    }
}

/// Commands that can be sent to the daemon
#[derive(Debug)]
pub enum DaemonCommand {
    /// ConfigWatcher detected an external write to the watched config file.
    ///
    /// ADR-034 §D9: this is the PASSIVE watcher path — the handler reloads
    /// only in legacy `source = "file"` mode, otherwise it surfaces drift
    /// (`config_drift_detected`) without reloading. For an EXPLICIT
    /// operator-initiated reload, use [`DaemonCommand::SignalReload`].
    ConfigFileChanged(PathBuf),

    /// Explicit operator request to reload config from disk now (SIGHUP).
    ///
    /// ADR-034 §D9: SIGHUP is an explicit reload intent (Unix convention),
    /// equivalent to `conductorctl config reload` but without CAS. It
    /// deliberately bypasses the passive-watcher demotion so `kill -HUP`
    /// keeps reloading in the managed default. Reloads from the daemon's
    /// configured `config_path`.
    SignalReload,

    /// IPC request from client
    IpcRequest {
        request: IpcRequest,
        /// ADR-027 D1 wiring: pinned + classified peer identity from
        /// the IPC accept loop. `None` for three sources:
        /// 1. **Peer pinning failed at accept** (logged in
        ///    `ipc.rs`) — kernel < 5.3 with no `pidfd_open`,
        ///    same-uid TCC anomaly, etc.
        /// 2. **Synthetic in-process IPC constructions in tests**
        ///    that don't simulate a real peer pin.
        /// 3. **Daemon-internal dispatch** — e.g. the
        ///    `execute_plugin_command` site in `executor.rs`
        ///    sends an internal `IpcRequest` whose origin is the
        ///    LLM call already gate-checked at the outer
        ///    boundary; there's no external peer to pin.
        ///
        /// The engine-manager handler hands this to
        /// `tool_executor.execute` so `gate::enforce` can consult
        /// the trust band before dispatching. The gate is currently skipped
        /// when this is `None`; a planned flag flip will
        /// distinguish (1) (deny as Untrusted) from (3) (allow as
        /// internal-trusted) — see the `TODO(gate-bypass
        /// on None)` comment in `executor.rs::execute`.
        caller_ctx: Option<crate::security::CallerContext>,
        response_tx: oneshot::Sender<IpcResponse>,
    },

    /// Menu bar action
    MenuBarAction(MenuBarAction),

    /// Device disconnected
    DeviceDisconnected,

    /// Device reconnected
    DeviceReconnected,

    /// Mode change requested (Phase 2)
    ModeChange { mode: String },

    /// ADR-040 Slice 5 (§4.5/§4.7) — resolve the active mode from a frontmost
    /// context snapshot and apply it lock-aware. The app detector emits this on
    /// every app change, AFTER any same-change `ProfileSwitch` (FIFO ordering on
    /// this channel guarantees the resolve runs against the newly-loaded
    /// profile's `[per_app_modes]`, §4.7). `window_title` is the *reconciled*
    /// title — `None` once an app change has invalidated the stale cached title
    /// (§4.5); window-title detection fills it in Slice 6.
    ResolveContextMode {
        app: String,
        window_title: Option<String>,
    },

    /// ADR-040 D4 §4.2 (Slice 4c) — set the active mode and optionally lock it
    /// against auto-switching (origin `Mcp`). The MCP server / LLM executor run
    /// in their own tasks, so they send this and await the oneshot rather than
    /// calling `set_mode_manual` directly. `Ok(())` on success, `Err(message)`
    /// for an unknown mode or internal fault.
    SetModeLocked {
        mode: String,
        lock: bool,
        response_tx: tokio::sync::oneshot::Sender<Result<(), String>>,
    },

    /// ADR-040 D4 §4.2 (Slice 4c) — release the manual mode lock (origin `Mcp`).
    /// Replies `true` if a lock was held.
    ReleaseModeLock {
        response_tx: tokio::sync::oneshot::Sender<bool>,
    },

    /// ADR-040 D4 §4.2 (Slice 4c) — report the active mode + lock state. Replies
    /// a JSON object (mode, index, locked, lock_origin, lock_mode).
    QueryModeStatus {
        response_tx: tokio::sync::oneshot::Sender<serde_json::Value>,
    },

    /// Switch to a named profile's config (Phase 2)
    /// Includes a oneshot sender for synchronous result feedback to the caller.
    ProfileSwitch {
        profile_name: String,
        config_path: String,
        /// The GUI's profile id (additive), when the caller has one,
        /// so the daemon can persist/report the identity the GUI keys by.
        profile_id: Option<String>,
        /// When provided, sends back Ok(profile_name) on success or Err(message) on failure.
        /// Phase 1 callers may pass `None` for fire-and-forget behavior.
        result_tx: Option<tokio::sync::oneshot::Sender<Result<String, String>>>,
    },

    /// Query the currently active profile
    ProfileQuery {
        response_tx: tokio::sync::oneshot::Sender<Option<ActiveProfileInfo>>,
    },

    /// Refresh app detector mappings from profiles manifest (Phase 2)
    RefreshAppMappings,

    /// Gamepad reconnected
    ReconnectGamepad,

    /// Device reconnection failed after max attempts
    DeviceReconnectionFailed,

    /// Fatal error occurred
    FatalError(String),

    /// Graceful shutdown requested
    Shutdown,

    /// Timer tick for hold detection across all devices (ADR-009 Phase 2, D12)
    TimerTick,

    /// Enable/disable a specific device (ADR-009 Phase 2, D8)
    SetDeviceEnabled { device_id: String, enabled: bool },

    /// Periodic port rescan for hot-plug detection (ADR-009 Phase 4).
    /// The run-loop handles this by SPAWNING the slow CoreMIDI
    /// enumeration off-loop and re-delivering [`Self::HotPlugApply`] — it no
    /// longer enumerates inline (which parked the event loop ~500ms every 5s).
    HotPlugCheck,

    /// Apply a hot-plug rescan with the port list ALREADY enumerated off
    /// the run-loop (by the task `HotPlugCheck` spawns). Keeping the slow
    /// enumeration off the event loop, the run-loop does only the cheap
    /// diff/open with these ports.
    ///
    /// `gamepad_available` carries the result of the (fixed ~500ms when
    /// no controller) gilrs `list_gamepads` probe, which the spawning task ALSO
    /// runs off-loop. The previous code probed inline in `process_hot_plug_apply`
    /// and parked the run-loop ~535ms every 5s whenever a gamepad endpoint was
    /// configured but no controller connected. The run-loop now only does the
    /// cheap connect when a controller was actually found.
    HotPlugApply {
        port_infos: Vec<conductor_core::resolver::PortInfo>,
        gamepad_available: bool,
    },

    /// Run a SysEx Identity probe against an input port (ADR-026 Phase 2).
    /// The MCP executor sends this rather than calling the probe coordinator
    /// directly because the executor doesn't have access to the daemon's
    /// `MidiOutputManager` — `EngineManager` resolves the paired output
    /// port and runs the (sync) probe via `tokio::task::spawn_blocking`,
    /// then sends the outcome back through `response_tx`.
    ProbeDeviceIdentity {
        port_name: String,
        response_tx: tokio::sync::oneshot::Sender<
            Result<
                conductor_core::device_intelligence::probe::ProbeResult,
                conductor_core::device_intelligence::probe::ProbeStartError,
            >,
        >,
    },
}

/// IPC request from client
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcRequest {
    pub id: String,
    pub command: IpcCommand,
    #[serde(default)]
    pub args: serde_json::Value,
}

/// IPC commands
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum IpcCommand {
    Ping,
    Status,
    Reload,
    Stop,
    ValidateConfig,

    /// ADR-042 Phase A: list bound network (OSC/Art-Net) listeners.
    GetListenerStatus,

    // Device management
    ListDevices,
    SetDevice,
    GetDevice,

    // MIDI Learn
    StartMidiLearn,
    StopMidiLearn,
    GetMidiLearnEvents,

    // LLM Plan/Apply (ADR-007 Phase 2)
    /// Apply a pending configuration plan
    ApplyPlan,
    /// Reject a pending configuration plan
    RejectPlan,
    /// List pending configuration plans
    ListPendingPlans,
    /// Execute an MCP tool directly
    ExecuteMcpTool,

    // Multi-device management (ADR-009 Phase 2)
    /// Enable or disable a specific device by device_id
    SetDeviceEnabled,

    // ADR-026 Phase 4.3b — diagnostic read of the per-port probe-attempt
    // ring buffer kept in `ProbeCoordinator`. Returns the last N
    // completed probes (timestamp + outcome wire shape) so the GUI's
    // identity-history panel can render them. Read-only; no risk-tier
    // gate needed.
    GetProbeHistory,

    // ADR-029 Phase 4 — query the daemon's macOS Input Monitoring
    // TCC grant state. The GUI uses this on first launch (and from the
    // HidDeviceList warning surface) to drive the onboarding sheet that
    // points the user at System Settings if the daemon's grant is
    // missing. Read-only. On non-macOS, returns "not_applicable".
    CheckPermissions,

    // Profile management (Phase 1, Phase 2)
    /// Switch to a named profile's config
    SwitchProfile,
    /// Get the currently active profile
    GetActiveProfile,
    /// Refresh app-profile mappings from manifest (Phase 2)
    RefreshAppMappings,

    // Config versioning
    /// Rollback config to last known-good version. Routed through
    /// `live_config.mutate(ConfigOp::Rollback)` post-D4.B.4 — CAS
    /// checked; rejected during AwaitingConfig (see
    /// `allowed_during_awaiting_config`).
    RollbackConfig,

    // ADR-034 §D1.2.1 / D4.B.4 — config provenance lifecycle
    /// Promote the current `LiveConfig` snapshot's `revision` to
    /// `known_good_revision`. Persists to `live.toml.known_good`
    /// via the per-op persist matrix; the in-memory snapshot
    /// advances generation but does NOT change content. CLI-only
    /// per spec — Gui / Llm peers get rejected at the accept-list
    /// (added once peer-context plumbing lands in D4.B.4 follow-up).
    MarkKnownGood,

    /// Break-glass non-CAS rollback. Routes through
    /// `live_config.mutate(ConfigOp::RollbackForce { reason })`.
    /// `reason` is a required non-empty string in `args`. CLI-only
    /// per ADR-034 §D6 — non-CLI peers are rejected at the
    /// handler with `IpcErrorCode::PermissionDenied`. Rejected
    /// during AwaitingConfig like other mutations.
    RollbackConfigForce,

    // Daemon settings (ADR-017)
    /// Set the daemon log level dynamically
    SetLogLevel,

    // LED control
    /// Set LED lighting scheme
    SetLedScheme,
    /// Set LED brightness
    SetLedBrightness,
    /// Get current LED status
    GetLedStatus,

    // Event monitoring
    /// Start real-time event monitoring
    StartEventMonitor,
    /// Stop real-time event monitoring
    StopEventMonitor,
    /// Get buffered monitor events (drains buffer)
    GetMonitorEvents,
    /// Subscribe to real-time event stream (push model)
    /// Keeps the connection open and streams events as newline-delimited JSON batches.
    SubscribeEvents,

    // Mapping simulation (ADR-014 Phase 5B)
    /// Simulate a mapping execution by mode + index
    SimulateMapping,

    // Plugin management
    /// List available and loaded plugins
    ListPlugins,
    /// Get metadata for a specific plugin
    GetPluginInfo,
    /// Enable a plugin by name
    EnablePlugin,
    /// Disable a plugin by name
    DisablePlugin,

    // Mode switching
    /// Switch the daemon's active mode by name
    SwitchMode,

    // Mode lock (ADR-040 D4 §4.2 — Slice 4b)
    /// Set the active mode and optionally lock it against auto-switching.
    /// `args`: `{ "mode": "<name>", "lock": <bool> }` (lock defaults true).
    SetMode,
    /// Release the manual mode lock, resuming auto-switching.
    UnlockMode,
    /// Report the active mode + lock state (mode, locked, lock origin).
    ModeStatus,

    // ADR-032 P4 — UI mode awareness
    /// Publish the GUI's current UI mode ("llm" | "studio") so the
    /// daemon's `Status` response (and the MCP `conductor_status` tool
    /// passthrough) can include it. Fire-and-forget: the daemon accepts
    /// the value and acknowledges; LLMs read the latest value when they
    /// query `conductor_status`. The GUI publishes on every mode toggle.
    SetUiMode,

    // ADR-027 D19 — GUI launch handshake
    /// First-message handshake from a daemon-spawned GUI. Args:
    /// `{ "nonce": "<base64-url-safe-no-pad>" }`. On match, the
    /// connection's `CallerContext` is elevated to `GuiTrusted`
    /// for the lifetime of the connection. On mismatch or no
    /// pending registration, the connection stays at its default
    /// tier. The nonce is consumed (single-use): a replay from
    /// the same PID returns `NoPending`. See
    /// [`crate::daemon::gui_handshake`] for the registry layer.
    Handshake,

    // ADR-034 §D4.2 / D4.B.3.B — was specced as the AwaitingConfig
    // idle-mode bootstrap IPC. The idle mode is reserved/unreachable,
    // but the `Init` handler itself is wired and runs in whatever
    // state the daemon is already in.
    /// Replace the whole live config from a `ConfigSource`:
    /// `{ "source": Defaults | FromPath { path } }`. PREPAREs the new
    /// config, commits it via the live-config mutate seam
    /// (`ConfigOp::ReplaceWhole`), then APPLYs — the same path a
    /// `SaveConfig` takes. Originally specced as the
    /// `LifecycleState::AwaitingConfig` bootstrap command (transition
    /// `AwaitingConfig → Starting`); since that idle mode is
    /// reserved/unreachable, `Init` simply runs in the daemon's
    /// current state. (The §D4.2 CLI-only peer restriction was to land with
    /// the AwaitingConfig integration and is likewise not yet enforced.)
    Init,
    /// Query the current `LiveConfig` snapshot **metadata** — returns
    /// `{ state_generation, revision, known_good_revision, applied_at }`. The
    /// config **body is intentionally omitted** (it is large and this is the hot
    /// CAS/status path); use [`IpcCommand::GetConfigBody`] when the canonical
    /// config tree itself is needed. (The sentinel `{ state_generation: 0, … }`
    /// response was reserved for the unreachable `AwaitingConfig` mode.)
    /// Accepted in every lifecycle state.
    GetConfigSnapshot,
    /// Query the current `LiveConfig` snapshot **including the config body** —
    /// returns `{ state_generation, config, revision, applied_at }`. ReadOnly.
    /// The GUI's `get_config` reads this so it reflects the daemon's canonical
    /// in-memory tree (live LLM/IPC mutations included) rather than a stale
    /// on-disk `config.toml` (ADR-034 §D4 / ADR-043). The returned
    /// `state_generation` is the CAS base a client can thread back as the next
    /// `SaveConfig` `base_generation` for a coherent single-snapshot CAS — a
    /// follow-up wires the GUI to do so (today `save_config` still fetches its
    /// base separately). The `AwaitingConfig` sentinel (`state_generation = 0`)
    /// returns `config: null` so the client falls back to the on-disk read.
    /// Accepted in every lifecycle state.
    GetConfigBody,

    // ADR-034 §D2 / D4.C.1 — strict IPC mutation surface.
    // Type-only landing; the dispatch table in
    // `engine_manager::handle_ipc_request` returns `UnknownCommand`
    // for these until the per-handler slices land (D4.C.6+).
    /// Persist a full config tree through the live-config mutate
    /// seam. Args: `{ "config": <Config>, "base_generation": u64,
    /// "base_revision"?: String }`.
    /// CAS-checked against the current snapshot's
    /// `state_generation`; stale base returns
    /// `IpcErrorCode::StaleBaseGeneration = 5002`. The optional
    /// `base_revision` (the `GetConfigBody` content hash the client
    /// displayed) adds an anti-clobber content guard: if it no
    /// longer matches the live revision the save is rejected with
    /// `StaleBaseContent = 5007` **before** commit — content-hash, not
    /// generation, so a daemon self-write that bumps the generation
    /// without changing content does not trip it. Payload capped
    /// at 256 KiB pre-deserialisation (`PayloadTooLarge = 5003`).
    /// Replaces the GUI's direct `Config::save(path)` write —
    /// post-D4.C, `user.toml` only changes via this IPC.
    /// Rejected during AwaitingConfig.
    SaveConfig,
    /// Re-read `live.toml` (or `--path <P>` for diagnostic loads)
    /// and republish via the mutate seam. Args:
    /// `{ "base_generation": u64, "path"?: <PathBuf> }`. Path
    /// (when present) is allowlist-validated per §D2.2. Operator
    /// recovery flow for "I hand-edited `user.toml`, please pick
    /// it up." Rejected during AwaitingConfig.
    ReloadFromDisk,
    /// Import a config from an arbitrary allowlisted path
    /// (e.g. promoting a stash file or applying a profile snapshot).
    /// Args: `{ "base_generation": u64, "path": <PathBuf> }`. Same
    /// path-validation and CAS semantics as `ReloadFromDisk`;
    /// distinct command so audit trail can distinguish operator
    /// reload vs targeted import. Rejected during AwaitingConfig.
    ImportConfig,
    /// Report whether `user.toml` on disk has drifted from the
    /// daemon's live snapshot. Returns `{ "drift": bool,
    /// "user_toml_hash"?, "live_revision"? }`. Pure read; safe
    /// during AwaitingConfig (which is why it's on the accept-list
    /// per §D4.2 — the operator can probe "is there a config to
    /// load" before invoking `Init { source: FromPath }`).
    ConfigDriftStatus,
    /// Structured diff of the daemon's in-memory live config vs the on-disk
    /// config (the drift source) — ReadOnly (ADR-034 §D4.D). Returns
    /// `{ differs, changed_sections, live, target }`: `changed_sections` is the
    /// set of top-level keys that differ, `live`/`target` the full trees for
    /// the GUI to render. Precursor for a future drift-banner Review-diff /
    /// Overwrite. No args (V1 diffs against the daemon's own `config_path`).
    /// Accepted in every lifecycle state (pure read, like `ConfigDriftStatus`).
    GetConfigDiff,
    /// Overwrite the on-disk config file with the daemon's live config — the
    /// "Overwrite user.toml" drift-banner action (ADR-034 §D4.D): "my live
    /// config wins". No args; writes the live snapshot to the daemon's own
    /// `config_path` via the §D9-suppressed write path (the watcher must not
    /// re-surface it as external drift). Returns `{ "revision" }`. A dedicated op
    /// because `SaveConfig` with the live body is a no-op (semantic-identical ⇒
    /// same revision ⇒ no CAS bump ⇒ no write-through), so it would NOT overwrite
    /// the drifted file. NOT on the AwaitingConfig accept-list — there is no live
    /// config to persist before the initial load.
    OverwriteConfigFile,

    // ADR-027 D13a — audit denial observability
    /// One-shot query of the persistent audit log. Args:
    /// `{ "denied_only": bool, "limit": u32 }`. Returns the most
    /// recent matching `AuditEntry` rows as a JSON array. Backs
    /// `conductorctl audit tail` / `audit denied` (non-follow mode).
    QueryAudit,
    /// Subscribe to the live audit-event stream (push model, like
    /// `SubscribeEvents`). Args: `{ "denied_only": bool }`. Takes
    /// over the connection and streams newline-delimited JSON
    /// batches of `AuditEntry`. Backs `conductorctl audit tail -f`.
    SubscribeAudit,

    /// ADR-034 §D8 — operator recovery from the fail-closed
    /// audit-unavailable state. When the audit outbox failed to open (corrupt
    /// chain / I/O), config mutations are refused (`AuditDegraded`); this
    /// reopens it, rotating a corrupt file aside to
    /// `audit-outbox.log.corrupt-<ms>` and starting a fresh chain whose first
    /// record is a `ChainReset` attestation, then transitions
    /// `AuditDegraded → Running`. Privileged (same pinned-peer surface as the
    /// ConfigChange commands). Returns `{ "recovered": bool, "rotated_path"? }`.
    /// Backs `conductorctl audit resume`.
    ResumeAudit,
}

impl IpcCommand {
    /// ADR-034 §D4.2 / D4.B.3.B — IPC accept-list for the
    /// `LifecycleState::AwaitingConfig` idle mode.
    ///
    /// **RESERVED: the daemon never enters `AwaitingConfig`
    /// today, so this predicate has no runtime consumer** — the dispatch
    /// filter that called it was removed as dead code. It is retained as
    /// the canonical accept-list spec (and is exercised by the truth-table
    /// tests below) so the idle mode can be reinstated without re-deriving
    /// the list.
    ///
    /// Spec accept-list (§D4.2):
    /// - `Init { source }` — would transition AwaitingConfig → Starting
    /// - `Status` / GetStatus — daemon health probe
    /// - `GetConfigSnapshot` — would return sentinel state_generation=0
    /// - `GetConfigBody` — pure read; returns the `config: null`
    ///   sentinel during AwaitingConfig
    /// - `ConfigDriftStatus` (D4.C.1) — pure read
    /// - `GetConfigDiff` — pure read; live-vs-on-disk config diff
    /// - `Ping` — defensive aliveness probe
    pub fn allowed_during_awaiting_config(&self) -> bool {
        matches!(
            self,
            Self::Init
                | Self::Status
                | Self::GetConfigSnapshot
                | Self::GetConfigBody
                | Self::ConfigDriftStatus
                | Self::GetConfigDiff
                | Self::Ping
        )
    }
}

/// ADR-034 §D4.2 / D4.B.3.B — source for the `Init` IPC during
/// `AwaitingConfig`.
///
/// `Defaults` boots the daemon with a hard-coded minimal-but-sane
/// config (no modes / no mappings; user must then `SaveConfig` to
/// populate). `FromPath` imports a TOML file the operator points at
/// (typically a legacy `~/.config/conductor/config.toml` they want
/// to promote to the canonical `$XDG_STATE_HOME/conductor/live.toml`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ConfigSource {
    /// Built-in minimal config — fresh-install bootstrap path.
    Defaults,
    /// Import from an absolute filesystem path. Path must be
    /// absolute (no `..`/relative); path validation in D4.C
    /// hardens this further. CLI-only.
    FromPath { path: std::path::PathBuf },
}

/// IPC response to client
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcResponse {
    pub id: String,
    pub status: ResponseStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorDetails>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResponseStatus {
    Success,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorDetails {
    pub code: u16,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

/// Menu bar actions
#[derive(Debug, Clone)]
pub enum MenuBarAction {
    ReloadConfig,
    OpenConfigFile,
    ViewStatus,
    Quit,
}

/// Device status information
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeviceStatus {
    pub connected: bool,
    pub name: Option<String>,
    pub port: Option<usize>,
    pub last_event_at: Option<u64>, // Unix timestamp in seconds
    /// Multi-device port statuses (ADR-009 Phase 2)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub devices: Vec<DevicePortStatus>,
}

/// Per-device port status for multi-device mode (ADR-009 Phase 2)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevicePortStatus {
    pub device_id: String,
    pub port_name: String,
    pub port_index: usize,
    pub connected: bool,
    /// Whether the device is enabled (not muted) (D8)
    pub enabled: bool,
    pub last_event_at: Option<u64>,
    /// Whether this device is bound to a configured `[[devices]]` identity (ADR-009 D19)
    ///
    /// `true` when the device was resolved via `BindingResult::Bound`.
    /// `false` when the device was opened as an unconfigured port (e.g. in `ListenMode::All`).
    #[serde(default)]
    pub is_configured: bool,
    /// Device direction: Input, Output, or Bidirectional (ADR-021)
    #[serde(default)]
    pub direction: conductor_core::config::DeviceDirection,
    /// Resolved output port name, if an output port was successfully matched for this device.
    /// May be `None` even if the device has output configured, when no matching port is
    /// currently available in the MIDI output enumeration. (ADR-021)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_port_name: Option<String>,
    /// Whether an output port was resolved and is currently available for this device.
    /// Set to `true` when `build_output_map` successfully matches the endpoint's output
    /// matchers (or auto-pairs) to a port in the current MIDI output enumeration. Conductor uses
    /// on-demand output connections, so this reflects resolution + availability, not an open
    /// connection. (ADR-021)
    #[serde(default)]
    pub output_connected: bool,
    /// Whether the output port was auto-paired (ADR-021)
    #[serde(default)]
    pub output_auto_paired: bool,
    /// Device protocol: "midi", "hid", "osc", or "artnet"
    #[serde(default = "default_protocol_midi")]
    pub protocol: String,
}

fn default_protocol_midi() -> String {
    "midi".to_string()
}

/// MIDI device information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MidiDeviceInfo {
    pub port_index: usize,
    pub port_name: String,
    pub manufacturer: Option<String>,
    pub connected: bool,
}

/// Daemon statistics
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DaemonStatistics {
    pub events_processed: u64,
    pub actions_executed: u64,
    pub errors_since_start: u64,
    pub config_reloads: u64,
    pub uptime_secs: u64,
    pub last_reload_duration_ms: Option<u64>,
    pub fastest_reload_ms: Option<u64>,
    pub slowest_reload_ms: Option<u64>,
    pub avg_reload_ms: Option<u64>,
}

impl DaemonStatistics {
    /// Update reload statistics with new metrics
    pub fn update_reload_metrics(&mut self, metrics: &ReloadMetrics) {
        self.config_reloads += 1;
        self.last_reload_duration_ms = Some(metrics.duration_ms);

        // Update fastest
        self.fastest_reload_ms = Some(match self.fastest_reload_ms {
            None => metrics.duration_ms,
            Some(fastest) => fastest.min(metrics.duration_ms),
        });

        // Update slowest
        self.slowest_reload_ms = Some(match self.slowest_reload_ms {
            None => metrics.duration_ms,
            Some(slowest) => slowest.max(metrics.duration_ms),
        });

        // Update average using cumulative average formula
        // new_avg = ((count - 1) * old_avg + new_value) / count
        // Use u128 for intermediate calculations to prevent overflow
        self.avg_reload_ms = Some(match self.avg_reload_ms {
            None => metrics.duration_ms,
            Some(avg) => {
                let count = self.config_reloads as u128;
                let old_avg = avg as u128;
                let new_value = metrics.duration_ms as u128;
                // ((count - 1) * old_avg + new_value) / count
                (((count - 1) * old_avg + new_value) / count) as u64
            }
        });
    }
}

/// Error entry for error log
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorEntry {
    pub timestamp: u64, // Unix timestamp in seconds
    pub kind: String,
    pub message: String,
}

impl ErrorEntry {
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            kind: kind.into(),
            message: message.into(),
        }
    }
}

/// Config reload metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReloadMetrics {
    pub duration_ms: u64,
    pub modes_loaded: usize,
    pub mappings_loaded: usize,
    pub config_load_ms: u64,
    pub mapping_compile_ms: u64,
    pub swap_ms: u64,
}

impl ReloadMetrics {
    /// Check if reload met performance targets
    pub fn met_target(&self) -> bool {
        self.duration_ms < 50 // Target: <50ms
    }

    /// Get performance grade (A/B/C/D/F)
    pub fn performance_grade(&self) -> char {
        match self.duration_ms {
            0..=20 => 'A',    // Excellent
            21..=50 => 'B',   // Good (target)
            51..=100 => 'C',  // Acceptable
            101..=200 => 'D', // Poor
            _ => 'F',         // Unacceptable
        }
    }
}

/// Daemon state for MCP tools (ADR-007 Phase 2)
///
/// This struct provides a snapshot of daemon state for MCP tool execution.
/// It's created on demand and passed to MCP tool handlers.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DaemonState {
    /// Current lifecycle state
    pub lifecycle_state: Option<LifecycleState>,
    /// Device connection status
    pub device_status: Option<DeviceStatus>,
    /// Daemon statistics
    pub statistics: Option<DaemonStatistics>,
    /// Input mode (MidiOnly, GamepadOnly, Both)
    pub input_mode: Option<String>,
    /// Connected HID/gamepad devices
    pub hid_devices: Vec<Value>,
    /// Uptime in seconds
    pub uptime_secs: u64,
    /// Config path
    pub config_path: Option<String>,
    /// Active profile info (Phase 1)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_profile: Option<ActiveProfileInfo>,
}

impl DaemonState {
    /// Create a new empty daemon state
    pub fn new() -> Self {
        Self::default()
    }

    /// Convert to JSON for MCP status tool
    pub fn to_status_json(&self) -> Value {
        let device_connected = self
            .device_status
            .as_ref()
            .map(|d| d.connected)
            .unwrap_or(false);

        // Include device_bindings from multi-device status
        // Use dps.is_configured field instead of broken starts_with("raw:") check (D19)
        let device_bindings: Vec<Value> = self
            .device_status
            .as_ref()
            .map(|d| {
                d.devices
                    .iter()
                    .map(|dps| {
                        json!({
                            "device_id": dps.device_id,
                            "port_name": dps.port_name,
                            "connected": dps.connected,
                            "enabled": dps.enabled,
                            "is_configured": dps.is_configured,
                            "direction": dps.direction,
                            "output_port_name": dps.output_port_name,
                            "output_connected": dps.output_connected,
                            "output_auto_paired": dps.output_auto_paired,
                            "protocol": dps.protocol
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        json!({
            "daemon_running": true, // Always true when daemon is responding
            "lifecycle_state": self.lifecycle_state.map(|s| format!("{}", s)).unwrap_or_else(|| "Unknown".to_string()),
            "connected": device_connected, // Backward compat: device connection state
            "device_connected": device_connected, // Explicit alias
            "device": self.device_status.as_ref().map(|d| json!({
                "name": d.name,
                "port": d.port,
                "last_event_at": d.last_event_at
            })).unwrap_or(json!(null)),
            "uptime_secs": self.uptime_secs,
            "config_path": self.config_path,
            "input_mode": self.input_mode,
            "statistics": self.statistics.as_ref().map(|s| json!({
                "events_processed": s.events_processed,
                "actions_executed": s.actions_executed,
                "errors_since_start": s.errors_since_start,
                "config_reloads": s.config_reloads
            })).unwrap_or(json!(null)),
            "device_bindings": device_bindings,
            "active_profile": self.active_profile.as_ref().map(|p| json!({
                "name": p.name,
                "config_path": p.config_path
            }))
        })
    }

    /// Convert to JSON for MCP devices tool
    pub fn to_devices_json(&self, midi_devices: Vec<MidiDeviceInfo>) -> Value {
        // Include device_bindings from multi-device status
        // Use dps.is_configured field instead of broken starts_with("raw:") check (D19)
        let device_bindings: Vec<Value> = self
            .device_status
            .as_ref()
            .map(|d| {
                d.devices
                    .iter()
                    .map(|dps| {
                        json!({
                            "device_id": dps.device_id,
                            "port_name": dps.port_name,
                            "connected": dps.connected,
                            "enabled": dps.enabled,
                            "is_configured": dps.is_configured,
                            "direction": dps.direction,
                            "output_port_name": dps.output_port_name,
                            "output_connected": dps.output_connected,
                            "output_auto_paired": dps.output_auto_paired,
                            "protocol": dps.protocol
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        json!({
            "midi_devices": midi_devices.iter().map(|d| json!({
                "port_index": d.port_index,
                "port_name": d.port_name,
                "manufacturer": d.manufacturer,
                "connected": d.connected
            })).collect::<Vec<_>>(),
            "hid_devices": self.hid_devices,
            "device_bindings": device_bindings
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_profile_path_accepts_uppercase_toml_extension() {
        // `validate_profile_path` must match the startup path's
        // case-INSENSITIVE `.toml` contract. A profile named `studio.TOML`
        // resolves at boot but was previously rejected here at runtime
        // (IPC/MCP/app-detection switch), an api-contract mismatch.
        use std::io::Write as _;
        let dir = tempfile::tempdir().unwrap();

        // Uppercase extension — must now be accepted.
        let upper = dir.path().join("studio.TOML");
        write!(std::fs::File::create(&upper).unwrap(), "# profile").unwrap();
        assert!(
            validate_profile_path(upper.to_str().unwrap()).is_ok(),
            "#1294: an uppercase `.TOML` extension must validate (matches startup)"
        );

        // Mixed case too.
        let mixed = dir.path().join("studio.ToMl");
        write!(std::fs::File::create(&mixed).unwrap(), "# profile").unwrap();
        assert!(validate_profile_path(mixed.to_str().unwrap()).is_ok());

        // Plain lowercase still works.
        let lower = dir.path().join("studio.toml");
        write!(std::fs::File::create(&lower).unwrap(), "# profile").unwrap();
        assert!(validate_profile_path(lower.to_str().unwrap()).is_ok());

        // A genuinely non-TOML extension is still rejected.
        let txt = dir.path().join("studio.txt");
        write!(std::fs::File::create(&txt).unwrap(), "# profile").unwrap();
        let err = validate_profile_path(txt.to_str().unwrap()).unwrap_err();
        assert!(
            err.contains(".toml"),
            "non-toml extension must be rejected, got: {err}"
        );
    }

    #[test]
    fn test_lifecycle_state_transitions() {
        assert!(LifecycleState::Init.can_transition_to(LifecycleState::Starting));
        assert!(LifecycleState::Starting.can_transition_to(LifecycleState::Running));
        assert!(LifecycleState::Running.can_transition_to(LifecycleState::Reloading));
        assert!(LifecycleState::Running.can_transition_to(LifecycleState::Degraded));
        assert!(LifecycleState::Running.can_transition_to(LifecycleState::Stopping));
        // D4.B.3 — AwaitingConfig transitions
        assert!(LifecycleState::Init.can_transition_to(LifecycleState::AwaitingConfig));
        assert!(LifecycleState::AwaitingConfig.can_transition_to(LifecycleState::Starting));
        assert!(LifecycleState::AwaitingConfig.can_transition_to(LifecycleState::Stopping));

        // Invalid transitions
        assert!(!LifecycleState::Init.can_transition_to(LifecycleState::Running));
        assert!(!LifecycleState::Running.can_transition_to(LifecycleState::Starting));
        assert!(!LifecycleState::Stopped.can_transition_to(LifecycleState::Running));
    }

    // ────────────────────────────────────────────────────────────────
    // ADR-034 §D4.2 / D4.B.3.B — IPC accept-list spec for AwaitingConfig.
    // RESERVED: the idle mode is unreachable and the dispatch
    // filter was removed, but these pin the accept-list truth table so the
    // spec is preserved for an eventual reinstatement.
    // ────────────────────────────────────────────────────────────────

    #[test]
    fn awaiting_config_accept_list_covers_spec_set() {
        // Spec §D4.2 accept-list: Init, Status, GetConfigSnapshot,
        // GetConfigBody (ReadOnly read that returns the
        // `config: null` sentinel during AwaitingConfig), ConfigDriftStatus
        // (D4.C.1), Ping (defensive aliveness probe; no side effects).
        for cmd in [
            IpcCommand::Init,
            IpcCommand::Status,
            IpcCommand::GetConfigSnapshot,
            IpcCommand::GetConfigBody,
            IpcCommand::ConfigDriftStatus,
            IpcCommand::GetConfigDiff,
            IpcCommand::Ping,
        ] {
            assert!(
                cmd.allowed_during_awaiting_config(),
                "spec accept-list command {cmd:?} must be allowed during AwaitingConfig"
            );
        }
    }

    #[test]
    fn awaiting_config_rejects_mutation_commands() {
        // The whole point: mutating IPCs MUST reject with 5005
        // until the daemon has loaded a config. Reject-list sampling
        // covers the high-risk ConfigChange / HardwareIO tiers.
        for cmd in [
            IpcCommand::Reload,
            IpcCommand::Stop,
            IpcCommand::ApplyPlan,
            IpcCommand::RejectPlan,
            IpcCommand::ExecuteMcpTool,
            IpcCommand::SetLedScheme,
            IpcCommand::SetLedBrightness,
            IpcCommand::RollbackConfig,
            IpcCommand::RollbackConfigForce,
            // D4.C.1 — new strict-IPC mutation surface:
            // CAS-checked saves and reloads MUST also reject
            // during AwaitingConfig (there's no snapshot to
            // base_generation against).
            IpcCommand::SaveConfig,
            IpcCommand::ReloadFromDisk,
            IpcCommand::ImportConfig,
            // Overwrite writes the live config to disk; there's no live
            // config to persist before the initial load, so it must reject too.
            IpcCommand::OverwriteConfigFile,
            IpcCommand::SwitchProfile,
            IpcCommand::SwitchMode,
            IpcCommand::StartMidiLearn,
            IpcCommand::StopMidiLearn,
        ] {
            assert!(
                !cmd.allowed_during_awaiting_config(),
                "mutating command {cmd:?} MUST be rejected during AwaitingConfig"
            );
        }
    }

    // ────────────────────────────────────────────────────────────────
    // ADR-034 §D2 / D4.C.1 — strict IPC mutation surface
    // wire-format tests. Pin SCREAMING_SNAKE_CASE so downstream
    // consumers (GUI, CLI, MCP) can plug handlers without protocol
    // drift across slices.
    // ────────────────────────────────────────────────────────────────

    #[test]
    fn d4c1_save_config_command_serialises_to_save_config_screaming() {
        let request = IpcRequest {
            id: "save-1".to_string(),
            command: IpcCommand::SaveConfig,
            args: serde_json::json!({"base_generation": 7}),
        };
        let json = serde_json::to_string(&request).unwrap();
        assert!(
            json.contains("\"command\":\"SAVE_CONFIG\""),
            "expected SAVE_CONFIG token, got: {json}"
        );
        assert!(json.contains("base_generation"));
    }

    #[test]
    fn d4c1_reload_from_disk_command_serialises_screaming() {
        let request = IpcRequest {
            id: "reload-1".to_string(),
            command: IpcCommand::ReloadFromDisk,
            args: serde_json::json!({"base_generation": 7}),
        };
        let json = serde_json::to_string(&request).unwrap();
        assert!(
            json.contains("\"command\":\"RELOAD_FROM_DISK\""),
            "expected RELOAD_FROM_DISK token, got: {json}"
        );
    }

    #[test]
    fn d4c1_import_config_command_serialises_screaming() {
        let request = IpcRequest {
            id: "import-1".to_string(),
            command: IpcCommand::ImportConfig,
            args: serde_json::json!({
                "base_generation": 7,
                "path": "/abs/path/to/snapshot.toml"
            }),
        };
        let json = serde_json::to_string(&request).unwrap();
        assert!(
            json.contains("\"command\":\"IMPORT_CONFIG\""),
            "expected IMPORT_CONFIG token, got: {json}"
        );
    }

    #[test]
    fn d4c1_config_drift_status_command_serialises_screaming() {
        let request = IpcRequest {
            id: "drift-1".to_string(),
            command: IpcCommand::ConfigDriftStatus,
            args: serde_json::json!({}),
        };
        let json = serde_json::to_string(&request).unwrap();
        assert!(
            json.contains("\"command\":\"CONFIG_DRIFT_STATUS\""),
            "expected CONFIG_DRIFT_STATUS token, got: {json}"
        );
    }

    #[test]
    fn resume_audit_command_serialises_screaming() {
        // `conductorctl audit resume` → IpcCommand::ResumeAudit.
        let request = IpcRequest {
            id: "resume-1".to_string(),
            command: IpcCommand::ResumeAudit,
            args: serde_json::json!({}),
        };
        let json = serde_json::to_string(&request).unwrap();
        assert!(
            json.contains("\"command\":\"RESUME_AUDIT\""),
            "expected RESUME_AUDIT token, got: {json}"
        );
    }

    #[test]
    fn d4c1_strict_ipc_commands_round_trip_through_serde() {
        for (raw, expected_disc) in [
            (
                r#"{"id":"r1","command":"SAVE_CONFIG","args":{}}"#,
                "SaveConfig",
            ),
            (
                r#"{"id":"r2","command":"RELOAD_FROM_DISK","args":{}}"#,
                "ReloadFromDisk",
            ),
            (
                r#"{"id":"r3","command":"IMPORT_CONFIG","args":{}}"#,
                "ImportConfig",
            ),
            (
                r#"{"id":"r4","command":"CONFIG_DRIFT_STATUS","args":{}}"#,
                "ConfigDriftStatus",
            ),
            (
                r#"{"id":"r5","command":"RESUME_AUDIT","args":{}}"#,
                "ResumeAudit",
            ),
        ] {
            let req: IpcRequest =
                serde_json::from_str(raw).unwrap_or_else(|e| panic!("parse {raw}: {e}"));
            let disc = format!("{:?}", req.command);
            assert!(
                disc.starts_with(expected_disc),
                "expected {expected_disc}, got {disc}"
            );
        }
    }

    /// Test that Starting can transition to Degraded when device connection fails
    #[test]
    fn test_starting_can_transition_to_degraded() {
        // Starting → Degraded is valid when device connection fails at startup
        assert!(LifecycleState::Starting.can_transition_to(LifecycleState::Degraded));
    }

    /// Test that Starting can transition to Stopping for clean shutdown during startup
    #[test]
    fn test_starting_can_transition_to_stopping() {
        // Starting → Stopping is valid when shutdown is requested during startup
        assert!(LifecycleState::Starting.can_transition_to(LifecycleState::Stopping));
    }

    /// ADR-034 §D8.2 / D4.C.1: AuditDegraded transitions.
    /// Running demotes on flush-failure threshold (subsequent slice
    /// wires the threshold check); resumes after operator action.
    /// Clean shutdown allowed from AuditDegraded directly.
    #[test]
    fn d4c1_audit_degraded_transitions_are_allowed() {
        assert!(
            LifecycleState::Running.can_transition_to(LifecycleState::AuditDegraded),
            "Running → AuditDegraded must be allowed (flush-failure demotion)"
        );
        assert!(
            LifecycleState::AuditDegraded.can_transition_to(LifecycleState::Running),
            "AuditDegraded → Running must be allowed (operator audit resume)"
        );
        assert!(
            LifecycleState::AuditDegraded.can_transition_to(LifecycleState::Stopping),
            "AuditDegraded → Stopping must be allowed (clean shutdown)"
        );
    }

    /// Negative pin: AuditDegraded is distinct from Degraded (device-
    /// level). Don't accidentally allow transitions that would let
    /// the daemon "heal" device problems by way of audit recovery.
    #[test]
    fn d4c1_audit_degraded_does_not_cross_into_device_states() {
        assert!(
            !LifecycleState::AuditDegraded.can_transition_to(LifecycleState::Degraded),
            "AuditDegraded must not transition directly to device Degraded"
        );
        assert!(
            !LifecycleState::AuditDegraded.can_transition_to(LifecycleState::Reconnecting),
            "AuditDegraded must not transition to Reconnecting"
        );
        assert!(
            !LifecycleState::Degraded.can_transition_to(LifecycleState::AuditDegraded),
            "Degraded must not promote to AuditDegraded (they're orthogonal)"
        );
    }

    /// Display impl must cover AuditDegraded — surfaced in IPC
    /// Status responses and the menu-bar lifecycle indicator.
    #[test]
    fn d4c1_audit_degraded_display_is_camel_case() {
        assert_eq!(
            format!("{}", LifecycleState::AuditDegraded),
            "AuditDegraded"
        );
    }

    #[test]
    fn test_ipc_request_serialization() {
        let request = IpcRequest {
            id: "test-123".to_string(),
            command: IpcCommand::Ping,
            args: serde_json::json!({}),
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"command\":\"PING\""));
        assert!(json.contains("\"id\":\"test-123\""));
    }

    #[test]
    fn test_ipc_response_serialization() {
        let response = IpcResponse {
            id: "test-456".to_string(),
            status: ResponseStatus::Success,
            data: Some(serde_json::json!({"message": "pong"})),
            error: None,
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"status\":\"success\""));
        assert!(!json.contains("error")); // Should be skipped when None
    }

    #[test]
    fn test_error_entry_creation() {
        let entry = ErrorEntry::new("DeviceDisconnected", "MIDI device unplugged");
        assert_eq!(entry.kind, "DeviceDisconnected");
        assert_eq!(entry.message, "MIDI device unplugged");
        assert!(entry.timestamp > 0);
    }

    #[test]
    fn test_reload_metrics_average_increases() {
        let mut stats = DaemonStatistics::default();

        // First reload: 10ms
        stats.update_reload_metrics(&ReloadMetrics {
            duration_ms: 10,
            modes_loaded: 1,
            mappings_loaded: 5,
            config_load_ms: 5,
            mapping_compile_ms: 3,
            swap_ms: 2,
        });
        assert_eq!(stats.avg_reload_ms, Some(10));

        // Second reload: 20ms (higher than avg)
        stats.update_reload_metrics(&ReloadMetrics {
            duration_ms: 20,
            modes_loaded: 1,
            mappings_loaded: 5,
            config_load_ms: 10,
            mapping_compile_ms: 5,
            swap_ms: 5,
        });
        // Average should increase: 10 + (20-10)/2 = 15
        assert_eq!(stats.avg_reload_ms, Some(15));
    }

    #[test]
    fn test_reload_metrics_average_decreases() {
        let mut stats = DaemonStatistics::default();

        // First reload: 100ms
        stats.update_reload_metrics(&ReloadMetrics {
            duration_ms: 100,
            modes_loaded: 1,
            mappings_loaded: 5,
            config_load_ms: 50,
            mapping_compile_ms: 30,
            swap_ms: 20,
        });
        assert_eq!(stats.avg_reload_ms, Some(100));

        // Second reload: 10ms (lower than avg)
        stats.update_reload_metrics(&ReloadMetrics {
            duration_ms: 10,
            modes_loaded: 1,
            mappings_loaded: 5,
            config_load_ms: 5,
            mapping_compile_ms: 3,
            swap_ms: 2,
        });
        // Average should decrease: 100 - (100-10)/2 = 55
        assert_eq!(stats.avg_reload_ms, Some(55));
        // Verify it actually decreased
        assert!(
            stats.avg_reload_ms.unwrap() < 100,
            "Average should decrease when new value is lower"
        );
    }

    #[test]
    fn test_reload_metrics_tracks_fastest_slowest() {
        let mut stats = DaemonStatistics::default();

        stats.update_reload_metrics(&ReloadMetrics {
            duration_ms: 50,
            modes_loaded: 1,
            mappings_loaded: 5,
            config_load_ms: 25,
            mapping_compile_ms: 15,
            swap_ms: 10,
        });
        assert_eq!(stats.fastest_reload_ms, Some(50));
        assert_eq!(stats.slowest_reload_ms, Some(50));

        // Faster reload
        stats.update_reload_metrics(&ReloadMetrics {
            duration_ms: 10,
            modes_loaded: 1,
            mappings_loaded: 5,
            config_load_ms: 5,
            mapping_compile_ms: 3,
            swap_ms: 2,
        });
        assert_eq!(stats.fastest_reload_ms, Some(10));
        assert_eq!(stats.slowest_reload_ms, Some(50));

        // Slower reload
        stats.update_reload_metrics(&ReloadMetrics {
            duration_ms: 200,
            modes_loaded: 1,
            mappings_loaded: 5,
            config_load_ms: 100,
            mapping_compile_ms: 60,
            swap_ms: 40,
        });
        assert_eq!(stats.fastest_reload_ms, Some(10));
        assert_eq!(stats.slowest_reload_ms, Some(200));
    }

    #[test]
    fn test_start_midi_learn_command_serialization() {
        let request = IpcRequest {
            id: "midi-learn-1".to_string(),
            command: IpcCommand::StartMidiLearn,
            args: serde_json::json!({}),
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"command\":\"START_MIDI_LEARN\""));
    }

    #[test]
    fn test_stop_midi_learn_command_serialization() {
        let request = IpcRequest {
            id: "midi-learn-2".to_string(),
            command: IpcCommand::StopMidiLearn,
            args: serde_json::json!({}),
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"command\":\"STOP_MIDI_LEARN\""));
    }

    #[test]
    fn test_midi_learn_command_deserialization() {
        // Test deserializing START_MIDI_LEARN
        let json = r#"{"id":"test","command":"START_MIDI_LEARN","args":{}}"#;
        let request: IpcRequest = serde_json::from_str(json).unwrap();
        assert!(matches!(request.command, IpcCommand::StartMidiLearn));

        // Test deserializing STOP_MIDI_LEARN
        let json = r#"{"id":"test","command":"STOP_MIDI_LEARN","args":{}}"#;
        let request: IpcRequest = serde_json::from_str(json).unwrap();
        assert!(matches!(request.command, IpcCommand::StopMidiLearn));
    }

    #[test]
    fn test_get_midi_learn_events_command_serialization() {
        let request = IpcRequest {
            id: "midi-events-1".to_string(),
            command: IpcCommand::GetMidiLearnEvents,
            args: serde_json::json!({}),
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"command\":\"GET_MIDI_LEARN_EVENTS\""));

        // Also test deserialization
        let json = r#"{"id":"test","command":"GET_MIDI_LEARN_EVENTS","args":{}}"#;
        let request: IpcRequest = serde_json::from_str(json).unwrap();
        assert!(matches!(request.command, IpcCommand::GetMidiLearnEvents));
    }

    /// ADR-007 Phase 2: Test DaemonState to JSON conversion
    #[test]
    fn test_daemon_state_to_status_json() {
        let state = DaemonState {
            lifecycle_state: Some(LifecycleState::Running),
            device_status: Some(DeviceStatus {
                connected: true,
                name: Some("Maschine Mikro".to_string()),
                port: Some(2),
                last_event_at: Some(1234567890),
                ..Default::default()
            }),
            statistics: Some(DaemonStatistics {
                events_processed: 100,
                actions_executed: 50,
                errors_since_start: 2,
                config_reloads: 3,
                ..Default::default()
            }),
            input_mode: Some("MidiOnly".to_string()),
            hid_devices: vec![],
            uptime_secs: 3600,
            config_path: Some("/path/to/config.toml".to_string()),
            active_profile: None,
        };

        let json = state.to_status_json();
        assert_eq!(json["lifecycle_state"], "Running");
        assert_eq!(json["connected"], true);
        assert_eq!(json["uptime_secs"], 3600);
        assert_eq!(json["statistics"]["events_processed"], 100);
    }

    /// Test status JSON includes daemon_running when no device connected
    #[test]
    fn test_status_json_includes_daemon_running_field() {
        let state = DaemonState {
            lifecycle_state: Some(LifecycleState::Running),
            device_status: None, // No device connected
            statistics: None,
            input_mode: Some("MidiOnly".to_string()),
            hid_devices: vec![],
            uptime_secs: 100,
            config_path: None,
            active_profile: None,
        };

        let json = state.to_status_json();
        assert_eq!(json["daemon_running"], true);
        assert_eq!(json["connected"], false);
        assert_eq!(json["device_connected"], false);
    }

    /// Test status JSON daemon_running with device connected
    #[test]
    fn test_status_json_daemon_running_with_device() {
        let state = DaemonState {
            lifecycle_state: Some(LifecycleState::Running),
            device_status: Some(DeviceStatus {
                connected: true,
                name: Some("Maschine Mikro".to_string()),
                port: Some(2),
                last_event_at: Some(1234567890),
                ..Default::default()
            }),
            statistics: None,
            input_mode: None,
            hid_devices: vec![],
            uptime_secs: 3600,
            config_path: None,
            active_profile: None,
        };

        let json = state.to_status_json();
        assert_eq!(json["daemon_running"], true);
        assert_eq!(json["connected"], true);
        assert_eq!(json["device_connected"], true);
    }

    /// ADR-007 Phase 2: Test DaemonState devices JSON conversion
    #[test]
    fn test_daemon_state_to_devices_json() {
        let state = DaemonState {
            hid_devices: vec![json!({"id": 0, "name": "Xbox Controller"})],
            ..Default::default()
        };

        let midi_devices = vec![MidiDeviceInfo {
            port_index: 0,
            port_name: "Maschine Mikro".to_string(),
            manufacturer: Some("Native Instruments".to_string()),
            connected: true,
        }];

        let json = state.to_devices_json(midi_devices);
        assert_eq!(json["midi_devices"][0]["port_name"], "Maschine Mikro");
        assert_eq!(json["hid_devices"][0]["name"], "Xbox Controller");
    }

    /// ADR-007 Phase 2: Test ApplyPlan IPC command serialization
    #[test]
    fn test_apply_plan_command_serialization() {
        let request = IpcRequest {
            id: "plan-apply-1".to_string(),
            command: IpcCommand::ApplyPlan,
            args: serde_json::json!({"plan_id": "550e8400-e29b-41d4-a716-446655440000"}),
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"command\":\"APPLY_PLAN\""));
        assert!(json.contains("plan_id"));
    }

    /// ADR-007 Phase 2: Test RejectPlan IPC command serialization
    #[test]
    fn test_reject_plan_command_serialization() {
        let request = IpcRequest {
            id: "plan-reject-1".to_string(),
            command: IpcCommand::RejectPlan,
            args: serde_json::json!({"plan_id": "550e8400-e29b-41d4-a716-446655440000"}),
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"command\":\"REJECT_PLAN\""));
    }

    /// ADR-007 Phase 2: Test LLM IPC commands deserialization
    #[test]
    fn test_llm_ipc_commands_deserialization() {
        // ApplyPlan
        let json = r#"{"id":"test","command":"APPLY_PLAN","args":{"plan_id":"uuid"}}"#;
        let request: IpcRequest = serde_json::from_str(json).unwrap();
        assert!(matches!(request.command, IpcCommand::ApplyPlan));

        // RejectPlan
        let json = r#"{"id":"test","command":"REJECT_PLAN","args":{"plan_id":"uuid"}}"#;
        let request: IpcRequest = serde_json::from_str(json).unwrap();
        assert!(matches!(request.command, IpcCommand::RejectPlan));

        // ListPendingPlans
        let json = r#"{"id":"test","command":"LIST_PENDING_PLANS","args":{}}"#;
        let request: IpcRequest = serde_json::from_str(json).unwrap();
        assert!(matches!(request.command, IpcCommand::ListPendingPlans));

        // ExecuteMcpTool
        let json = r#"{"id":"test","command":"EXECUTE_MCP_TOOL","args":{"tool_name":"conductor_get_status"}}"#;
        let request: IpcRequest = serde_json::from_str(json).unwrap();
        assert!(matches!(request.command, IpcCommand::ExecuteMcpTool));
    }

    /// Test is_configured propagates correctly from DevicePortStatus to JSON (D19)
    #[test]
    fn test_is_configured_propagates_to_json() {
        let state = DaemonState {
            lifecycle_state: Some(LifecycleState::Running),
            device_status: Some(DeviceStatus {
                connected: true,
                devices: vec![
                    DevicePortStatus {
                        device_id: "pads".to_string(),
                        port_name: "Mikro MK3 MIDI".to_string(),
                        port_index: 0,
                        connected: true,
                        enabled: true,
                        last_event_at: None,
                        is_configured: true,
                        direction: conductor_core::config::DeviceDirection::Input,
                        output_port_name: None,
                        output_connected: false,
                        output_auto_paired: false,
                        protocol: "midi".to_string(),
                    },
                    DevicePortStatus {
                        device_id: "Launchpad MIDI".to_string(),
                        port_name: "Launchpad MIDI".to_string(),
                        port_index: 1,
                        connected: true,
                        enabled: true,
                        last_event_at: None,
                        is_configured: false,
                        direction: conductor_core::config::DeviceDirection::Input,
                        output_port_name: None,
                        output_connected: false,
                        output_auto_paired: false,
                        protocol: "midi".to_string(),
                    },
                ],
                ..Default::default()
            }),
            ..Default::default()
        };

        // Test to_status_json
        let status_json = state.to_status_json();
        let bindings = status_json["device_bindings"].as_array().unwrap();
        assert_eq!(bindings.len(), 2);
        assert_eq!(bindings[0]["device_id"], "pads");
        assert_eq!(bindings[0]["is_configured"], true);
        assert_eq!(bindings[1]["device_id"], "Launchpad MIDI");
        assert_eq!(bindings[1]["is_configured"], false);

        // Test to_devices_json
        let devices_json = state.to_devices_json(vec![]);
        let bindings = devices_json["device_bindings"].as_array().unwrap();
        assert_eq!(bindings.len(), 2);
        assert_eq!(bindings[0]["is_configured"], true);
        assert_eq!(bindings[1]["is_configured"], false);

        // Verify protocol field is serialized in JSON output
        let status_json = state.to_status_json();
        let bindings = status_json["device_bindings"].as_array().unwrap();
        assert_eq!(bindings[0]["protocol"], "midi");
        assert_eq!(bindings[1]["protocol"], "midi");
    }

    /// Test is_configured defaults to false for serde backward compat (D19)
    #[test]
    fn test_is_configured_serde_default() {
        // Old JSON without is_configured field should default to false
        let json = r#"{
            "device_id": "test",
            "port_name": "Test Port",
            "port_index": 0,
            "connected": true,
            "enabled": true,
            "last_event_at": null
        }"#;
        let status: DevicePortStatus = serde_json::from_str(json).unwrap();
        assert!(
            !status.is_configured,
            "Missing is_configured should default to false"
        );
    }

    /// Test protocol defaults to "midi" for serde backward compat
    #[test]
    fn test_protocol_serde_default() {
        let json = r#"{
            "device_id": "test",
            "port_name": "Test Port",
            "port_index": 0,
            "connected": true,
            "enabled": true,
            "last_event_at": null
        }"#;
        let status: DevicePortStatus = serde_json::from_str(json).unwrap();
        assert_eq!(
            status.protocol, "midi",
            "Missing protocol should default to \"midi\""
        );
    }

    /// Phase 1: Test ActiveProfileInfo serialization
    #[test]
    fn test_active_profile_info_serialization() {
        let info = ActiveProfileInfo {
            id: None,
            name: "Logic Pro".to_string(),
            config_path: "/Users/test/profiles/logic-pro.toml".to_string(),
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("Logic Pro"));
        assert!(json.contains("logic-pro.toml"));

        let deserialized: ActiveProfileInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "Logic Pro");
    }

    /// Phase 1: Test SwitchProfile IPC command serialization
    #[test]
    fn test_switch_profile_command_serialization() {
        let request = IpcRequest {
            id: "profile-1".to_string(),
            command: IpcCommand::SwitchProfile,
            args: serde_json::json!({
                "profile_name": "Logic Pro",
                "config_path": "/path/to/profile.toml"
            }),
        };
        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"command\":\"SWITCH_PROFILE\""));
    }

    /// Phase 1: Test GetActiveProfile IPC command serialization
    #[test]
    fn test_get_active_profile_command_serialization() {
        let request = IpcRequest {
            id: "profile-2".to_string(),
            command: IpcCommand::GetActiveProfile,
            args: serde_json::json!({}),
        };
        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"command\":\"GET_ACTIVE_PROFILE\""));
    }

    /// Phase 1: Test DaemonState with active profile in status JSON
    #[test]
    fn test_status_json_with_active_profile() {
        let state = DaemonState {
            lifecycle_state: Some(LifecycleState::Running),
            device_status: None,
            statistics: None,
            input_mode: None,
            hid_devices: vec![],
            uptime_secs: 100,
            config_path: None,
            active_profile: Some(ActiveProfileInfo {
                id: None,
                name: "Ableton".to_string(),
                config_path: "/profiles/ableton.toml".to_string(),
            }),
        };

        let json = state.to_status_json();
        assert_eq!(json["active_profile"]["name"], "Ableton");
        assert_eq!(
            json["active_profile"]["config_path"],
            "/profiles/ableton.toml"
        );
    }

    /// Phase 1: Test DaemonState without active profile (backward compat)
    #[test]
    fn test_status_json_without_active_profile() {
        let state = DaemonState {
            lifecycle_state: Some(LifecycleState::Running),
            device_status: None,
            statistics: None,
            input_mode: None,
            hid_devices: vec![],
            uptime_secs: 100,
            config_path: None,
            active_profile: None,
        };

        let json = state.to_status_json();
        assert!(json["active_profile"].is_null());
    }

    /// Phase 1: Test DaemonState active_profile defaults to None via serde
    #[test]
    fn test_active_profile_serde_default() {
        // Old JSON without active_profile field should default to None
        let json = r#"{
            "uptime_secs": 100,
            "hid_devices": []
        }"#;
        let state: DaemonState = serde_json::from_str(json).unwrap();
        assert!(state.active_profile.is_none());
    }

    #[test]
    fn test_validate_plugin_name_valid() {
        assert!(validate_plugin_name("my-plugin").is_ok());
        assert!(validate_plugin_name("plugin_v2").is_ok());
        assert!(validate_plugin_name("com.example.plugin").is_ok());
        assert!(validate_plugin_name("a").is_ok());
    }

    #[test]
    fn test_validate_plugin_name_empty() {
        assert!(validate_plugin_name("").is_err());
    }

    #[test]
    fn test_validate_plugin_name_traversal() {
        assert!(validate_plugin_name("../etc/passwd").is_err());
        assert!(validate_plugin_name("../../secret").is_err());
        assert!(validate_plugin_name("foo/bar").is_err());
        assert!(validate_plugin_name("..").is_err());
        assert!(validate_plugin_name(".").is_err());
        assert!(validate_plugin_name("foo..bar").is_err());
    }

    #[test]
    fn test_validate_plugin_name_special_chars() {
        assert!(validate_plugin_name("plugin name").is_err());
        assert!(validate_plugin_name("plugin;rm").is_err());
        assert!(validate_plugin_name("plugin$var").is_err());
        assert!(validate_plugin_name("plugin\0null").is_err());
    }

    /// Phase 2: ProfileSwitch with result_tx sends back success
    #[tokio::test]
    async fn test_profile_switch_result_channel_success() {
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        let cmd = DaemonCommand::ProfileSwitch {
            profile_name: "Logic Pro".to_string(),
            config_path: "/profiles/logic.toml".to_string(),
            profile_id: None,
            result_tx: Some(result_tx),
        };

        // Simulate engine manager sending success
        if let DaemonCommand::ProfileSwitch {
            result_tx: Some(tx),
            profile_name,
            ..
        } = cmd
        {
            tx.send(Ok(profile_name)).unwrap();
        }

        let result = result_rx.await.unwrap();
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "Logic Pro");
    }

    /// Phase 2: ProfileSwitch with result_tx sends back failure
    #[tokio::test]
    async fn test_profile_switch_result_channel_failure() {
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        let cmd = DaemonCommand::ProfileSwitch {
            profile_name: "Bad Profile".to_string(),
            config_path: "/nonexistent.toml".to_string(),
            profile_id: None,
            result_tx: Some(result_tx),
        };

        if let DaemonCommand::ProfileSwitch {
            result_tx: Some(tx),
            ..
        } = cmd
        {
            tx.send(Err("Config file not found".to_string())).unwrap();
        }

        let result = result_rx.await.unwrap();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    /// Phase 2: ProfileSwitch with None result_tx (fire-and-forget)
    #[test]
    fn test_profile_switch_fire_and_forget() {
        let _cmd = DaemonCommand::ProfileSwitch {
            profile_name: "Test".to_string(),
            config_path: "/test.toml".to_string(),
            profile_id: None,
            result_tx: None,
        };
        // Should compile and work without result_tx
    }

    // =========================================================================
    // SwitchMode IPC command tests
    // =========================================================================

    #[test]
    fn test_switch_mode_ipc_serialization() {
        let request = IpcRequest {
            id: "mode-test".to_string(),
            command: IpcCommand::SwitchMode,
            args: serde_json::json!({ "mode": "Edit" }),
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"command\":\"SWITCH_MODE\""));

        // Round-trip deserialization
        let parsed: IpcRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed.command, IpcCommand::SwitchMode));
        assert_eq!(parsed.args["mode"], "Edit");
    }

    #[test]
    fn test_handshake_ipc_serialization() {
        let request = IpcRequest {
            id: "handshake-test".to_string(),
            command: IpcCommand::Handshake,
            args: serde_json::json!({ "nonce": "Zm9v" }),
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"command\":\"HANDSHAKE\""));

        let parsed: IpcRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed.command, IpcCommand::Handshake));
        assert_eq!(parsed.args["nonce"], "Zm9v");
    }

    // =========================================================================
    // EventFilter tests
    // =========================================================================

    fn make_note_on(note: u8, vel: u8, channel: Option<u8>) -> MonitorEvent {
        MonitorEvent {
            timestamp_ms: 1000,
            event_type: "note_on".to_string(),
            note: Some(note),
            velocity: Some(vel),
            channel,
            ..Default::default()
        }
    }

    fn make_cc(cc: u8, value: u16, channel: Option<u8>) -> MonitorEvent {
        MonitorEvent {
            timestamp_ms: 1000,
            event_type: "cc".to_string(),
            cc: Some(cc),
            value: Some(value),
            channel,
            ..Default::default()
        }
    }

    #[test]
    fn test_event_filter_empty_matches_all() {
        let filter = EventFilter::default();
        assert!(filter.matches(&make_note_on(60, 100, Some(0))));
        assert!(filter.matches(&make_cc(1, 64, Some(0))));
    }

    #[test]
    fn test_event_filter_by_type() {
        let filter = EventFilter {
            event_type: Some("note_on".to_string()),
            ..Default::default()
        };
        assert!(filter.matches(&make_note_on(60, 100, Some(0))));
        assert!(!filter.matches(&make_cc(1, 64, Some(0))));
    }

    #[test]
    fn test_event_filter_by_type_comma_separated() {
        let filter = EventFilter {
            event_type: Some("note_on,note_off".to_string()),
            ..Default::default()
        };
        assert!(filter.matches(&make_note_on(60, 100, None)));
        assert!(filter.matches(&MonitorEvent {
            event_type: "note_off".to_string(),
            note: Some(60),
            ..Default::default()
        }));
        assert!(!filter.matches(&make_cc(1, 64, None)));
    }

    #[test]
    fn test_event_filter_by_channel() {
        let filter = EventFilter {
            channel: Some(0),
            ..Default::default()
        };
        assert!(filter.matches(&make_note_on(60, 100, Some(0))));
        assert!(!filter.matches(&make_note_on(60, 100, Some(1))));
        // Events without channel info pass through (forward compat)
        assert!(filter.matches(&make_note_on(60, 100, None)));
    }

    #[test]
    fn test_event_filter_by_note_range() {
        let filter = EventFilter {
            note_min: Some(36),
            note_max: Some(51),
            ..Default::default()
        };
        assert!(filter.matches(&make_note_on(36, 100, None)));
        assert!(filter.matches(&make_note_on(51, 100, None)));
        assert!(filter.matches(&make_note_on(42, 100, None)));
        assert!(!filter.matches(&make_note_on(35, 100, None)));
        assert!(!filter.matches(&make_note_on(52, 100, None)));
        // CC events have no note — filtered out by note range
        assert!(!filter.matches(&make_cc(1, 64, None)));
    }

    #[test]
    fn test_event_filter_by_note_min_only() {
        let filter = EventFilter {
            note_min: Some(60),
            ..Default::default()
        };
        assert!(filter.matches(&make_note_on(60, 100, None)));
        assert!(filter.matches(&make_note_on(127, 100, None)));
        assert!(!filter.matches(&make_note_on(59, 100, None)));
    }

    #[test]
    fn test_event_filter_by_note_max_only() {
        let filter = EventFilter {
            note_max: Some(51),
            ..Default::default()
        };
        assert!(filter.matches(&make_note_on(0, 100, None)));
        assert!(filter.matches(&make_note_on(51, 100, None)));
        assert!(!filter.matches(&make_note_on(52, 100, None)));
    }

    #[test]
    fn test_event_filter_by_device() {
        let filter = EventFilter {
            device_id: Some("launchpad-mini".to_string()),
            ..Default::default()
        };
        assert!(filter.matches(&MonitorEvent {
            event_type: "note_on".to_string(),
            device_id: Some("launchpad-mini".to_string()),
            note: Some(60),
            ..Default::default()
        }));
        assert!(!filter.matches(&MonitorEvent {
            event_type: "note_on".to_string(),
            device_id: Some("other-device".to_string()),
            note: Some(60),
            ..Default::default()
        }));
        // No device ID → filtered out
        assert!(!filter.matches(&make_note_on(60, 100, None)));
    }

    #[test]
    fn test_event_filter_combined() {
        let filter = EventFilter {
            event_type: Some("note_on".to_string()),
            channel: Some(0),
            note_min: Some(36),
            note_max: Some(51),
            ..Default::default()
        };
        // Matches all criteria
        assert!(filter.matches(&make_note_on(42, 100, Some(0))));
        // Wrong type
        assert!(!filter.matches(&make_cc(1, 64, Some(0))));
        // Wrong channel
        assert!(!filter.matches(&make_note_on(42, 100, Some(1))));
        // Out of note range
        assert!(!filter.matches(&make_note_on(60, 100, Some(0))));
    }

    #[test]
    fn test_event_filter_by_since() {
        let filter = EventFilter {
            since_ms: Some(5000),
            ..Default::default()
        };
        // Event at 6000ms — after since
        let mut event = make_note_on(60, 100, None);
        event.timestamp_ms = 6000;
        assert!(filter.matches(&event));

        // Event at 5000ms — exactly at since (inclusive)
        event.timestamp_ms = 5000;
        assert!(filter.matches(&event));

        // Event at 4999ms — before since
        event.timestamp_ms = 4999;
        assert!(!filter.matches(&event));
    }

    #[test]
    fn test_event_stats_default() {
        let stats = EventStats::default();
        assert_eq!(stats.total_events, 0);
        assert_eq!(stats.events_per_second, 0.0);
        assert_eq!(stats.avg_velocity, 0.0);
        assert!(stats.most_active_note.is_none());
        assert_eq!(stats.error_count, 0);
    }
}
