// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Engine manager with atomic config reloading and device reconnection

use crate::action_executor::{ActionExecutor, TriggerContext};
use crate::daemon::device_rate_limiter::DeviceRateLimiter;
use crate::daemon::error::{DaemonError, IpcErrorCode, Result};
use crate::daemon::ipc::create_success_response;
#[cfg(feature = "llm-executor")]
use crate::daemon::llm::executor::ToolExecutor;
use crate::daemon::state::{ConfigInfo, EngineInfo, calculate_checksum};
use crate::daemon::types::{
    ActiveProfileInfo, DaemonCommand, DaemonState, DaemonStatistics, DevicePortStatus,
    DeviceStatus, ErrorDetails, ErrorEntry, IpcCommand, IpcRequest, IpcResponse, LifecycleState,
    MidiDeviceInfo, MonitorEvent, ReloadMetrics, ResponseStatus,
};
use crate::gamepad_device::HidDeviceManager; // v3.0
use crate::input_manager::{InputManager, InputMode};
use crate::listeners::AuditRateLimiter;
use crate::listeners::{
    AcceptedPacket, AuditEventKind, EdgeAuditSink, ListenerManager, NetworkListener, spawn_listener,
};
use arc_swap::{ArcSwap, ArcSwapOption};
use conductor_core::config::control_state_analyzer;
use conductor_core::config::port_binding::DeviceDirection;
#[cfg(test)]
use conductor_core::config::types::Mode;
use conductor_core::control_state::PhysicalControlStateStore;
use conductor_core::device_intelligence::fingerprint::EventStats;
use conductor_core::dispatch::{ActionEnvelope, DispatchOutcome};
use conductor_core::events::{InputEvent, ProtocolEvent};
use conductor_core::identity::{DeviceEvent, DeviceId};
use conductor_core::rule_set::ModeState;
use conductor_core::{
    Config, ConfigSource, EventProcessor, EventType, FiredActionInfo, FiredTriggerInfo,
    MappingEngine, MappingFiredPayload, PatternType, UserFilePolicy, action_type_string,
    summarize_action,
};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, RwLock, broadcast, mpsc};
use tracing::{debug, error, info, trace, warn};

// ── Submodules (refactor #2073: decompose engine_manager) ────────────────
// The inherent `impl EngineManager` is split across these files; each is a
// separate `impl EngineManager { … }` block over `use super::*`. Free helpers
// live in `helpers` and are re-exported so `engine_manager::X` paths (used by
// `tests` and other submodules) keep resolving.
mod accessors;
mod construct;
mod devices;
mod devices_connect;
mod devices_probe;
mod events;
mod events_device;
mod events_dispatch;
mod events_processing;
mod helpers;
mod ipc_config;
mod ipc_config_versioning;
mod ipc_devices;
mod ipc_dispatch;
mod ipc_learn;
#[cfg(feature = "llm-executor")]
mod ipc_llm;
mod ipc_plugins;
mod ipc_profile;
mod ipc_status;
mod lifecycle;
pub(crate) use helpers::*;
mod mode_autoswitch;
mod mode_lock;
pub(crate) use mode_lock::{LockSource, ModeLock, SetModeError};
mod monitor;
mod monitor_capture;
mod profile_switch;
mod reload;
mod reload_cached;
mod run_loop;
mod shared_refs;
pub use shared_refs::SharedDaemonStateRefs;

/// Maximum events stored in MIDI Learn buffer (ring buffer, oldest dropped first)
const MIDI_LEARN_MAX_EVENTS: usize = 100;

/// Maximum events stored in event monitor buffer (Issue #326)
const EVENT_MONITOR_MAX_EVENTS: usize = 1000;

/// Engine manager coordinating Conductor engine with daemon lifecycle
///
/// # Lock Ordering Invariant (v4.14.0 - LLM Council Feedback)
///
/// When acquiring multiple locks, they MUST be acquired in the following order
/// to prevent deadlocks. This order is based on the dependency graph and typical
/// access patterns:
///
/// 1. `state` (RwLock) - Lifecycle state, checked frequently for transitions
/// 2. `config` (RwLock) - Configuration, needed for mapping lookups
/// 3. `current_mode` (ArcSwap) - Mode selection, LOCK-FREE (v4.21.0 - ADR-009 Phase 3)
/// 4. `event_processor` (RwLock) - Event processing, uses config state
/// 5. `rule_set` (ArcSwap) - Rule matching, LOCK-FREE (v4.21.0 - ADR-009 Phase 3)
/// 6. `device_status` (RwLock) - Device info, updated after mapping ops
/// 7. `statistics` (RwLock) - Stats tracking, updated last
/// 8. `error_log` (RwLock) - Error recording, independent but low priority
/// 9. `input_manager` (Mutex) - Device I/O, held briefly
/// 10. `action_executor` (Mutex) - Action execution, held during action
/// 11. `midi_learn_events` (Mutex) - Event buffer, brief access only
/// 12. `pending_chord_event` (Mutex) - Chord debounce, brief access only (v4.26.64)
/// 13. `event_processors` (DashMap) - Per-device processors, shard-level locking (v4.20.0)
///
/// ## Key Principles
///
/// - **RwLock read locks** are generally safe to acquire out of order if no write
///   locks are held on any lock in the chain
/// - **RwLock write locks** must follow ordering strictly to prevent deadlock
/// - **Mutex locks** are always exclusive - treat as write locks for ordering
/// - **Avoid holding multiple locks** across await points when possible
/// - **Release locks as soon as possible** - extract needed data and drop guard
///
/// ## Common Access Patterns
///
/// Notation: (r) = RwLock read, (w) = RwLock write, (x) = Mutex exclusive
///
/// - `process_device_event`: current_mode(load) → rule_set(load) →
///   action_dispatcher.try_dispatch() — LOCK-FREE hot path (v4.21.0, ADR-015;
///   #885 unified the legacy single-device path into this one)
/// - `reload_config`: state(w) → config(w) → rule_set(store) →
///   current_mode(store) → statistics(w) — atomic swap, never blocks readers
/// - `Status IPC`: state(r) → config(r) → device_status(r) → statistics(r) →
///   input_manager(x)
///
/// ## Design Notes
///
/// The locks are intentionally fine-grained (11 separate locks) rather than using
/// a single RwLock for the entire struct. This allows:
/// - Concurrent reads during normal operation
/// - Isolated writes for specific subsystems
/// - Better performance under contention
///
/// The trade-off is complexity in ensuring correct ordering, which this
/// documentation addresses.
pub struct EngineManager {
    /// Daemon-managed live configuration (ADR-034 §D1).
    /// D4.A.3.1 introduced this alongside the legacy
    /// `Arc<RwLock<Config>>`; D4.A.3.2 routed writers through
    /// `live_config.mutate()`; D4.A.3.3.A retired the legacy field and
    /// promoted `live_config` to the sole source of truth.
    live_config: Arc<super::live_config::LiveConfig>,
    config_path: PathBuf,
    /// Operator-editable **user file** (`config.toml` / active profile) — the
    /// §D9 watch target (#2551). Distinct from `config_path`, which is the
    /// daemon's loaded **authority** (`live.toml` when present). The "Overwrite
    /// user.toml" drift action writes HERE, not to `config_path` (#2553).
    /// Defaults to `config_path` (the `new()` single-file case); `service.rs`
    /// overrides it via [`set_user_file_path`](EngineManager::set_user_file_path)
    /// when the authority and user file differ.
    user_file_path: PathBuf,
    /// #2564: where `active_profile.json` (daemon-owned durable profile
    /// IDENTITY) lives — the daemon state dir, wired by `service.rs` via
    /// [`set_active_profile_persist_dir`](EngineManager::set_active_profile_persist_dir).
    /// `None` (tests / not wired) ⇒ profile switches update the in-memory
    /// `active_profile` only, with no persistence side-effect.
    active_profile_persist_dir: Option<PathBuf>,

    // Engine components.
    // (#885: legacy singleton `event_processor` removed — every device
    // gets its own entry in `event_processors: DashMap` below.)
    // (D4.A.3.3.A: legacy `rule_set: Arc<ArcSwap<CompiledRuleSet>>` retired —
    // every read is now sourced from `live_config.load().rules`.)
    /// Lock-free compiled route engine (ADR-031 § 4.4 / Phase 2B) —
    /// intended as stage-9 of the post-#1118 8-stage matcher (hot-path
    /// read after rule_set misses is wired in Phase 2C). Rebuilt on
    /// config reload via `RouteEngine::compile(&config.routes)`.
    /// ArcSwap matches the rule_set pattern: infrequent atomic swaps on
    /// reload, wait-free loads on the event-processing hot path.
    /// `RouteEngine` has no interior mutability (vs `ConnectorRegistry`
    /// which uses RwLock for `bind_port`/`disconnect`/`record_activity`),
    /// so ArcSwap is the right fit here.
    route_engine: Arc<ArcSwap<crate::route_engine::RouteEngine>>,
    /// Bounded ring buffer of recent route-dispatch decisions
    /// (ADR-036 §8 / Slice 9). Written by `dispatch_route_outputs`; shared
    /// with the MCP executor via `SharedDaemonStateRefs.dispatch_trace`.
    dispatch_trace: Arc<super::dispatch_trace::DispatchTraceRing>,
    /// Backward compat: MappingEngine kept for MCP tools / external consumers
    mapping_engine: Arc<RwLock<MappingEngine>>,
    /// NON-DISPATCH executor — plugin lifecycle (load/enable/disable) and
    /// synchronous SysEx probe sends ONLY. **This is NOT the executor that runs
    /// action dispatch** (that is `action_dispatcher`'s thread-owned executor,
    /// ADR-015 D1). #2396: do NOT push dispatch config here — virtual ports, OSC
    /// output endpoints, and the ADR-042 D17 allow-map must reach the dispatch
    /// thread (via `shared_action_config` / `executor_vport_tx`), not this
    /// instance. Setting them here was the #2396 bug. Full type-split to make
    /// this structurally unrepresentable is tracked as a follow-up.
    action_executor: Arc<Mutex<ActionExecutor>>,

    /// Dedicated action executor thread (ADR-015 D1) — the ONLY action-dispatch
    /// path; owns the executor whose config (`shared_action_config` +
    /// `executor_vport_tx`) is the dispatch authority (#2396).
    action_dispatcher: super::executor_thread::ActionDispatcher,

    /// Device alias → output port name (ADR-021 Phase 2A)
    /// Shared with ActionExecutor via ArcSwap for lock-free hot-path reads
    device_output_map: Arc<ArcSwap<HashMap<String, String>>>,

    /// #2396 / ADR-015 D2 (revised) + ADR-021 D4: read-mostly dispatch config
    /// (OSC output endpoints + ADR-042 D17 allow-map) shared lock-free with the
    /// dispatch-thread executor via a SINGLE ArcSwap (atomic across both maps).
    /// EngineManager `store`s the full config; the dispatch path reads it. Built
    /// from live config on connect / reload / listener-bind via
    /// `store_shared_action_config`.
    shared_action_config: Arc<ArcSwap<crate::action_executor::SharedActionConfig>>,

    /// #2396: latest-wins desired virtual-port names → the executor thread,
    /// which creates/tears down the OS midir ports between actions (thread
    /// affinity, ADR-009 D1). The `watch` sender lives for the daemon lifetime
    /// (reused across reload — the channel is never recreated).
    executor_vport_tx: tokio::sync::watch::Sender<Vec<String>>,

    /// Signal-routing-graph runtime state (ADR-031 § 3.4).
    /// Built from `(config.devices, config.connectors)` on `new()` and
    /// rebuilt on `reload_config()`. Shared via `SharedDaemonStateRefs`
    /// so MCP / IPC code paths can read connectors and their bound
    /// ports without holding any other daemon lock.
    connector_registry: Arc<RwLock<crate::connector_registry::ConnectorRegistry>>,

    /// MIDI recursion guard (ADR-015 D8) — prevents SendMidi output echo
    recursion_guard: std::sync::Mutex<super::recursion_guard::MidiRecursionGuard>,

    /// #2397 (epic #2395): coalesces `midi_*_suppressed` MonitorEvents so a
    /// feedback loop / chord storm doesn't flood the monitor stream 1:1.
    /// Telemetry cadence only — does not affect the suppression decision.
    suppression_throttle: super::suppression_throttle::SuppressionThrottle,

    /// Unified input device manager (MIDI + Gamepad) (v3.0)
    input_manager: Arc<Mutex<Option<InputManager>>>,

    /// Shared SysEx probe coordinator (ADR-026 Phase 1.B).
    /// Owned here so Phase 2's MCP tool path and every port the
    /// `InputManager` opens both reach the same coordinator instance.
    probe_coordinator: Arc<conductor_core::device_intelligence::probe::ProbeCoordinator>,

    /// Set of input port names whose configured-binding state has
    /// already been observed by the probe-on-connect dispatcher
    /// (ADR-026 Phase 3.C.2). Diff'd against the current
    /// `InputManager` bindings on every dispatch tick to compute
    /// the set of newly-opened ports that should fire a probe.
    /// Steady-state rescans don't re-probe ports already in here,
    /// avoiding spurious load + per-port rate-limit churn. Cleared
    /// when a port disconnects (a re-plug should re-probe).
    last_known_configured_ports: Arc<Mutex<HashSet<String>>>,

    /// Unified input event channel — every connected device emits
    /// `DeviceEvent<ProtocolEvent>` here, regardless of whether the config
    /// declares `[[bindings]]`. (v4.20.0 - ADR-009 Phase 2; legacy
    /// single-device channel removed in #885.)
    ///
    /// ADR-039 #1758: the element type is `ProtocolEvent`, not `InputEvent` —
    /// MIDI/HID sources wrap their events as `ProtocolEvent::Input(..)` at
    /// ingress; the recv loop (`run_loop.rs`) unwraps back to `InputEvent`
    /// before the (still `InputEvent`-shaped) `process_device_event` stage.
    /// This gives the new `ProtocolEvent` enum a live consumer; the route stage
    /// starts taking `&ProtocolEvent` in #1759 and the pump rewrite lands in
    /// #1760.
    device_event_tx: mpsc::Sender<DeviceEvent<ProtocolEvent>>,
    device_event_rx: mpsc::Receiver<DeviceEvent<ProtocolEvent>>,

    /// Per-device EventProcessors (v4.20.0 - ADR-009 Phase 2, D14)
    /// DashMap provides shard-level locking — concurrent access to different devices
    /// doesn't contend. New devices are added on first event (lazy creation).
    event_processors: Arc<DashMap<DeviceId, EventProcessor>>,

    /// Per-device event statistics for fingerprinting (ADR-022 D7)
    /// Keyed by device_id string. Updated on every input event.
    event_stats: Arc<DashMap<String, EventStats>>,

    /// Cache: device_id (alias) → port_name. Populated during device status updates.
    /// Avoids O(n) linear scan on DevicePortStatus in the hot path.
    device_port_name_cache: DashMap<String, String>,

    /// Lifecycle state
    state: Arc<RwLock<LifecycleState>>,

    /// Device status
    device_status: Arc<RwLock<DeviceStatus>>,

    /// Statistics
    statistics: Arc<RwLock<DaemonStatistics>>,
    start_time: Instant,

    /// Error log (keep last 10 errors)
    error_log: Arc<RwLock<Vec<ErrorEntry>>>,

    /// Command receiver
    command_rx: mpsc::Receiver<DaemonCommand>,

    /// Command sender (for self-commands and reconnection)
    command_tx: mpsc::Sender<DaemonCommand>,

    /// #2390: set while a spawned hot-plug enumeration is in flight, so a burst
    /// of `HotPlugCheck` senders can't pile up concurrent off-loop CoreMIDI
    /// scans. Cleared by the spawned task when enumeration finishes.
    hot_plug_in_flight: std::sync::Arc<std::sync::atomic::AtomicBool>,

    /// #2404: guards the one-time spawn of the daemon-lifetime timer-tick (D12)
    /// and hot-plug (Phase 4) background tasks. They poll `command_tx` for the
    /// daemon's whole life and exit only on shutdown — they are NOT tied to a
    /// connect session. `connect_multi_device` runs again on every MIDI
    /// `DeviceReconnected`, so spawning them there leaked a fresh pair per
    /// reconnect. Set-once via `swap`; once `true` the tasks are already running.
    background_tasks_spawned: AtomicBool,

    /// Shutdown broadcaster
    shutdown_tx: broadcast::Sender<()>,

    /// MIDI Learn mode active flag (v4.2)
    midi_learn_active: Arc<AtomicBool>,

    /// Event monitor active flag (Issue #326) — zero cost when disabled
    event_monitor_active: Arc<AtomicBool>,

    /// Event monitor ring buffer (Issue #326) — capacity 1000
    /// Uses std::sync::Mutex with try_lock() on hot path (council review: avoid async in event loop)
    event_monitor_buffer: Arc<std::sync::Mutex<VecDeque<MonitorEvent>>>,
    event_monitor_max: usize,

    /// Broadcast channel for push-based event monitoring (#394)
    /// Subscribers receive events as they arrive — no polling needed.
    /// Capacity 256: if a subscriber falls behind, it receives `Lagged` error and catches up.
    event_broadcast_tx: broadcast::Sender<MonitorEvent>,

    /// Monotonic event sequence counter (#2410). Stamped onto every
    /// `MonitorEvent` in `push_monitor_event`, giving the GUI a total emission
    /// order to sort by — `mapping_fired` is pushed from a different run-loop
    /// `select!` arm than its raw event, so push order (not arrival order) is
    /// authoritative. One relaxed atomic increment per event.
    event_seq: std::sync::atomic::AtomicU64,

    /// Capture toggles from [event_console] config (R926-R928)
    /// Refreshed on config reload/profile switch to match the new config.
    /// Default to true when no [event_console] section exists.
    capture_midi: bool,
    capture_processed: bool,
    capture_actions: bool,
    /// Fixed-window rate limiter for monitor events (R924). None = unlimited.
    /// Derived from [event_console] at startup and requires daemon restart to change
    /// (it is not updated by reload_config()).
    monitor_rate_limiter: Option<MonitorRateLimiter>,
    /// Event trigger engine (R915-R917)
    trigger_engine: std::sync::Mutex<super::event_triggers::TriggerEngine>,
    /// Enable latency tracking in monitor events (R919: track_latency)
    /// Derived from [event_console] at startup; requires daemon restart to change.
    track_latency: bool,
    /// Enable memory tracking in monitor events (R920: track_memory)
    /// Derived from [event_console] at startup; requires daemon restart to change.
    track_memory: bool,

    /// MIDI Learn event buffer (v4.2) - stores events for GUI polling
    ///
    /// ARCHITECTURE NOTE (v4.13.2 - LLM Council Review):
    /// Producer-Consumer pattern with Mutex protection:
    /// - Producer: EngineManager.process_device_event() pushes events
    /// - Consumer: ToolExecutor.conductor_stop_midi_learn() drains events
    /// - Bounding: push_back() preceded by pop_front() when at capacity
    /// - Thread-safe via Arc<Mutex<>> - both access patterns require lock
    ///
    /// Bounding logic is implemented at PUSH TIME (not by VecDeque capacity):
    /// ```ignore
    /// if events.len() >= MIDI_LEARN_MAX_EVENTS {
    ///     events.pop_front();  // Drop oldest
    /// }
    /// events.push_back(event);  // Add newest
    /// ```
    midi_learn_events: Arc<Mutex<VecDeque<MidiLearnEvent>>>,

    /// Pending chord event for MIDI Learn debouncing (v4.26.64)
    /// When a ChordDetected arrives, we store it here instead of pushing immediately.
    /// If a larger chord supersedes it within 150ms, we replace. After 150ms or on
    /// a non-chord event, we flush the pending chord to the ring buffer.
    pending_chord_event: Arc<Mutex<Option<(MidiLearnEvent, Instant)>>>,

    /// Current mode state — atomic swap via ArcSwap for lock-free reads (v4.21.0 - ADR-009 Phase 3)
    /// Initialized from config.last_selected_mode, defaults to index 0
    current_mode: Arc<ArcSwap<ModeState>>,

    /// Manual-override mode lock (ADR-040 D4 §4.2). `Some` ⇒ auto-switching is
    /// suppressed and the pinned mode holds; `None` ⇒ auto-switch active.
    /// **Transient** — initialised to `None` every boot and never persisted, so
    /// a forgotten lock can't silently disable auto-switching after a restart.
    mode_lock: Arc<ArcSwapOption<ModeLock>>,

    /// Serialises the mode-mutation critical sections (`set_mode_manual`,
    /// `unlock_mode`, `apply_auto_switch`) so the lock check and the lock-state
    /// mutation around `persist_mode_change`'s await can't interleave across
    /// concurrent callers (CLI/MCP task vs. the Slice-5 app-detector task).
    /// Mode changes are infrequent, so holding it across the persist I/O is fine.
    mode_mutation_lock: tokio::sync::Mutex<()>,

    /// Rule set version counter for debugging/logging (v4.21.0)
    rule_set_version: std::sync::atomic::AtomicU64,

    /// Tool executor for LLM integration (ADR-007 Phase 2).
    /// ADR-045 D1 (#2492): IPC-only write machinery — `llm-executor` builds.
    #[cfg(feature = "llm-executor")]
    tool_executor: Arc<ToolExecutor>,

    /// Audit logger (ADR-027 D13a, #1167). The same `Arc` is shared
    /// with `tool_executor`; kept here so the IPC layer can serve
    /// `QueryAudit` (one-shot) and hand the broadcast handle to
    /// `SubscribeAudit` streams. `None` when audit init failed.
    /// ADR-045 D1 (#2492): SQLite audit only exists in `audit-db` builds.
    /// Kept (alongside `audit_sink`) for the read side — `QueryAudit`
    /// needs SQL. Write-side consumers use `audit_sink`.
    #[cfg(feature = "audit-db")]
    audit_logger: Option<Arc<super::audit::AuditLogger>>,

    /// ADR-045 D5 (#2493): the write-side audit seam — SQLite when
    /// `audit-db` is compiled (same instance as `audit_logger`), the
    /// always-compiled JSONL sink otherwise. `None` only when no sink
    /// could be initialized (network listeners then refuse to start).
    audit_sink: Option<Arc<dyn super::audit::AuditSink>>,

    /// Running network listeners (ADR-042 Phase A). Each holds a bound loopback
    /// UDP socket + its receive task; started in `connect_multi_device`, aborted
    /// in `disconnect_input_devices`. Empty when no OSC/Art-Net Input endpoints.
    network_listeners: Vec<NetworkListener>,

    /// ADR-042 Phase B-early bind gate (#1899). Decides whether a **non-loopback**
    /// OSC/Art-Net listener may bind (HMAC-verified approval required; fail-closed
    /// otherwise). Loopback listeners bypass it. `None` when the home dir can't be
    /// resolved (then all non-loopback binds are withheld). Unix-only: the approval
    /// registry + keychain-init wiring rely on hardened-file APIs
    /// (`O_NOFOLLOW`/`fstat`/`flock`) that are Unix-only, so on other platforms
    /// non-loopback listeners are always withheld (handled in
    /// `bind_network_listeners` without this field).
    #[cfg(unix)]
    network_bind_gate: Option<std::sync::Arc<crate::security::NetworkBindGate>>,

    /// Per-device event rate limiter (v4.26.0 - ADR-009 D9)
    device_rate_limiter: DeviceRateLimiter,

    /// Active profile info (Phase 1 - Issue #323)
    /// None means default/no profile (backward compatible)
    active_profile: Arc<ArcSwap<Option<ActiveProfileInfo>>>,

    /// GUI's current UI mode ("llm" | "studio") — ADR-032 P4 (#1089).
    /// `None` means the GUI has not (yet) reported. The Status response
    /// omits the field when None so consumers without a connected GUI
    /// see no shape change. Updated via `IpcCommand::SetUiMode`.
    ui_mode: Arc<RwLock<Option<String>>>,

    /// Channel to re-target ConfigWatcher after profile switch (Phase 2 - Issue #353)
    watcher_retarget_tx: Option<mpsc::Sender<PathBuf>>,

    /// Automatic profile switching by frontmost app (Phase 2 - Issue #353)
    app_detector: Option<Arc<crate::daemon::app_detector::DaemonAppDetector>>,

    /// Suppression timestamp for config watcher feedback loop (council review fix).
    /// Set to `Instant::now()` before writing config; the reload handler suppresses
    /// events arriving within the suppression window (500ms) to avoid race conditions.
    config_write_suppress: Arc<tokio::sync::Mutex<Option<Instant>>>,

    /// Profile cache for fast switching (Issue #355 — R770: <50ms target).
    /// Pre-parsed and validated configs avoid file I/O on the hot path.
    profile_cache: crate::daemon::profile_cache::ProfileCache,

    /// Physical control state store (ADR-025 Phase 1).
    /// Device-keyed last-known state of PC/CC/NoteHeld/PitchBend/aftertouch,
    /// observed from raw ingress before any `MidiTransform`. Shared with
    /// MCP tools and (Phase 2+) the condition evaluator.
    control_state: Arc<PhysicalControlStateStore>,

    /// ADR-025 Phase 3.F runtime check (#886). Handle for the deferred
    /// observed-vs-expected PC-tuple warning. Scheduled once per config-swap
    /// (startup / reload / profile-switch / plan-apply), aborted and
    /// replaced on the next swap or at daemon shutdown.
    pending_pc_observation_check: Option<tokio::task::JoinHandle<()>>,

    /// ADR-025 Phase 3.F cooperative cancellation flag (#886).
    /// `JoinHandle::abort()` only fires at `.await` points — it can't stop
    /// the synchronous tail of the check (config clone + analyzer +
    /// `tracing::warn!`) once the sleep has resolved. This flag closes
    /// that race: the task checks it before the log call and bails if the
    /// abort helper has flipped it to `true`. Paired with the handle
    /// above so both are taken atomically on abort.
    pending_pc_observation_cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,

    /// Content revision (#2071, ADR-043 D2/Q2) of the last `LiveConfig`
    /// snapshot whose committed config we have rebuilt the runtime for.
    ///
    /// `reconcile_runtime_to_live` rebuilds the runtime (registry,
    /// bindings, output map, listeners, rate limiter, probe toggle,
    /// capture flags, device status, mode) only when the live snapshot's
    /// `revision` differs from this — so it is a cheap no-op for read-only
    /// IPC commands and for byte-identical commits (e.g. `MarkKnownGood`,
    /// which bumps `state_generation` but not content). Seeded at
    /// construction with the initial snapshot's revision because the
    /// constructor already builds the runtime to match it.
    last_reconciled_revision: Option<conductor_core::config::ConfigRevision>,
}

/// MIDI event for MIDI Learn feature (v4.2, extended v4.7.0 for patterns)
///
/// Contains raw event data plus optional pattern detection fields.
/// When the EventProcessor detects multi-event patterns (LongPress, DoubleTap,
/// Chord, etc.), the pattern_* fields are populated for GUI auto-detection.
/// v4.9.0: Use typed enums instead of strings (ADR-004)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MidiLearnEvent {
    /// Event type using type-safe enum (v4.9.0 - ADR-004)
    pub event_type: EventType,
    /// Source device identity (v4.20.0 - ADR-009 Phase 2)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    /// MIDI note number (0-127)
    pub note: Option<u8>,
    /// MIDI velocity (0-127)
    pub velocity: Option<u8>,
    /// MIDI CC number (0-127)
    pub cc: Option<u8>,
    /// MIDI CC or other value (0-127)
    pub value: Option<u8>,
    /// MIDI channel (0-15, default 0)
    #[serde(default)]
    pub channel: u8,
    /// Event timestamp in milliseconds
    #[serde(default)]
    pub timestamp: u64,

    /// Program Change program number (0-127) — ADR-025 Phase 1.
    /// Set when `event_type == ProgramChange`. Stored separately from
    /// `value` so a Learn analyser can distinguish PC events from CC
    /// values without re-parsing event_type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pc: Option<u8>,

    // Gamepad fields (v4.6.0)
    /// Gamepad button ID (128-255)
    pub button: Option<u8>,
    /// Gamepad analog stick axis
    pub axis: Option<u8>,
    /// Gamepad analog trigger
    pub trigger: Option<u8>,

    // Pattern detection fields (v4.7.0 - ADR-002)
    /// Detected pattern type using type-safe enum (v4.9.0 - ADR-004)
    pub pattern_type: Option<PatternType>,
    /// For Chord patterns: list of notes pressed simultaneously
    pub pattern_notes: Option<Vec<u8>>,
    /// For GamepadChord patterns: list of buttons pressed simultaneously
    pub pattern_buttons: Option<Vec<u8>>,
    /// For LongPress patterns: duration held in milliseconds
    pub pattern_duration_ms: Option<u64>,
    /// For DoubleTap/Chord patterns: detection window in milliseconds
    pub pattern_timeout_ms: Option<u64>,
}

// `extract_raw_midi` was extracted to crate::midi_bytes (#1119
// follow-up) so the small surface (one function + 9 tests) can be
// reviewed in full by tooling that has input-size limits — this file
// is too large for the Council's balanced-tier 30k cap.
use crate::midi_bytes::extract_raw_midi;

#[cfg(test)]
mod tests;
