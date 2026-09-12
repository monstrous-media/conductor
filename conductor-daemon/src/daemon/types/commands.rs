// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! In-process daemon command enum (with oneshot reply channels).

use super::*;

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
