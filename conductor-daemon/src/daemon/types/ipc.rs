// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! IPC wire types: requests, the command surface, responses.

use super::*;

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
