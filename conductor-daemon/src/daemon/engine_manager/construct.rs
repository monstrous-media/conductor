// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! `EngineManager::new` constructor, extracted from `engine_manager::mod`
//! (refactor #2073).

use super::*;

impl EngineManager {
    /// Create a new engine manager
    pub fn new(
        config: Config,
        config_path: PathBuf,
        command_rx: mpsc::Receiver<DaemonCommand>,
        command_tx: mpsc::Sender<DaemonCommand>,
        shutdown_tx: broadcast::Sender<()>,
    ) -> Result<Self> {
        // ADR-025 Phase 3.F — log which PC-state tuples this config depends
        // on so users can cross-check with `conductor-state` that the
        // expected stomps are actually reaching the daemon.
        control_state_analyzer::log_expected_pc_tuples(&config, "startup");

        let mut mapping_engine = MappingEngine::new();
        mapping_engine.load_from_config(&config);
        let device_output_map = Arc::new(ArcSwap::from_pointee(HashMap::new()));
        // ADR-031 P1B: build the signal-routing-graph runtime from config.
        // Lowers `[[bindings]]` to Input connectors + folds in explicit
        // `[[connectors]]`. Wrapped in `RwLock` (not `ArcSwap`) because
        // the registry has interior mutability for `bind_port` /
        // `disconnect` / `record_activity` — current readers are MCP
        // / IPC queries (infrequent), so the lock cost is negligible
        // compared to the ergonomic win over a swap-on-every-mutate
        // ArcSwap pattern. ActionExecutor does NOT consult the registry
        // today (Phase 1B § 3.5 deferred — the hot path stays on the
        // lock-free `device_output_map`); reconsider this choice if
        // that ever changes.
        // ADR-035 Slice 6: build the registry from the unified endpoint set
        // (authored `[[endpoints]]` + lowered legacy bindings/connectors).
        // `Config::load` already ran `normalize_to_endpoints` and hard-failed on
        // any alias collision, so this re-normalization is guaranteed to
        // succeed; the `?` is defensive (DaemonError: From<ConfigError>).
        let (endpoints, _findings) =
            conductor_core::config::loader::normalize_to_endpoints(&config)?;
        let connector_registry = Arc::new(RwLock::new(
            crate::connector_registry::ConnectorRegistry::from_config(&endpoints),
        ));
        // ADR-025 Phase 1: shared physical control state store.
        // Constructed here (ahead of ActionExecutor) so the executor can
        // capture a reference for Phase 2 state conditions.
        let control_state = Arc::new(PhysicalControlStateStore::default());
        // #2396 / ADR-015 D2 (revised) + ADR-021 D4: the read-mostly dispatch
        // config (OSC output endpoints + ADR-042 D17 allow-map) is shared
        // lock-free with the dispatch-thread executor via a single ArcSwap.
        // Created BEFORE both executors and at its fail-safe default (empty maps
        // = DENY), so the dispatch thread never observes an uninitialized config
        // (no fail-open window). Virtual-port NAMES flow via a separate `watch`
        // (thread-affine create/teardown applied between actions).
        let shared_action_config = Arc::new(arc_swap::ArcSwap::from_pointee(
            crate::action_executor::SharedActionConfig::default(),
        ));
        let (executor_vport_tx, executor_vport_rx) =
            tokio::sync::watch::channel(Vec::<String>::new());
        // ADR-027 §D10b: apply the shell-sandbox policy (`allow_unsandboxed`)
        // from the loaded config at startup.
        let action_executor = ActionExecutor::new(Arc::clone(&device_output_map))
            .with_control_state(Arc::clone(&control_state))
            .with_shared_action_config(Arc::clone(&shared_action_config))
            .with_shell_security(&config.security.shell);
        // Log the loaded policy at startup in BOTH branches so operators can
        // always confirm it (Copilot review).
        if config.security.shell.allow_unsandboxed {
            info!(
                "ADR-027 D10b: security.shell.allow_unsandboxed = true — shell actions are \
                 OS-sandboxed where supported; on platforms that can't sandbox they spawn \
                 unconfined with a per-action warning"
            );
        } else {
            info!(
                "ADR-027 D10b: security.shell.allow_unsandboxed = false — shell actions that \
                 cannot be OS-sandboxed will be refused (fail-closed)"
            );
        }

        // D4.A.3.3.A: lock-free rule set is owned by `LiveConfig` —
        // the first compile fires inside `LiveConfig::new_published()` below (#2533)
        // (RealRuleCompiler). Engine-local rule_set field retired.

        // ADR-031 § 4.4 / Phase 2B: compile route engine (intended as
        // stage-9 of the post-#1118 8-stage matcher; hot-path invocation
        // after rule_set misses is wired in Phase 2C). ArcSwap matches
        // the rule_set pattern; rebuilt in `reload_config()` alongside it.
        let initial_route_engine = crate::route_engine::RouteEngine::compile(&config.routes);
        // `compile()` is pure — opt into the one-time startup warning
        // for excluded routes (cross-protocol transforms and OSC-only
        // filters, both deferred until cross-protocol routing lands).
        initial_route_engine.log_exclusions();
        let route_engine = Arc::new(ArcSwap::from_pointee(initial_route_engine));
        // ADR-036 §8 / Slice 9: shared dispatch-trace ring buffer.
        // Capacity is configurable via `advanced_settings.trace_buffer_size`
        // (spec §10 Open Item #3); validation bounds it to [1, 1_000_000].
        let dispatch_trace = Arc::new(
            crate::daemon::dispatch_trace::DispatchTraceRing::with_capacity(
                config.advanced_settings.trace_buffer_size,
            ),
        );

        // Unified input event channel (v4.20.0 multi-device, #885 consolidated):
        // every connected device emits `DeviceEvent<ProtocolEvent>` here (ADR-039
        // #1758 — MIDI/HID wrap as `ProtocolEvent::Input`), including legacy
        // single-device configs. Buffer size 1000 handles high-frequency MIDI
        // devices (~1000 events/sec) without dropping events during config
        // reload (<10ms) or IPC processing.
        let (device_event_tx, device_event_rx) = mpsc::channel::<DeviceEvent<ProtocolEvent>>(1000);

        // Per-device EventProcessors (v4.20.0 - ADR-009 Phase 2)
        let event_processors: Arc<DashMap<DeviceId, EventProcessor>> = Arc::new(DashMap::new());

        // Per-device event stats for fingerprinting (ADR-022 D7)
        let event_stats: Arc<DashMap<String, EventStats>> = Arc::new(DashMap::new());

        // Initialize current mode using proper fallback chain (Phase 3 of Issue #321)
        let initial_mode_index = resolve_startup_mode(&config);
        let initial_mode_name = config
            .modes
            .get(initial_mode_index)
            .map(|m| m.name.clone())
            .unwrap_or_default();
        let initial_mode_state = Arc::new(ModeState {
            index: initial_mode_index,
            name: initial_mode_name,
        });
        debug!(
            "Initializing with mode index {} (fallback chain: last_selected_mode: {:?}, default_mode: {:?}, modes_count: {})",
            initial_mode_index,
            config.last_selected_mode,
            config.default_mode,
            config.modes.len()
        );

        // Create MIDI Learn state (v4.2) - shared with ToolExecutor
        // Ring buffer with MIDI_LEARN_MAX_EVENTS capacity - oldest events dropped when full
        let midi_learn_active = Arc::new(AtomicBool::new(false));
        let midi_learn_events =
            Arc::new(Mutex::new(VecDeque::with_capacity(MIDI_LEARN_MAX_EVENTS)));

        // Event monitor state (Issue #326) — disabled by default, zero cost
        // Buffer size configurable via [event_console] config (R925)
        let ec_ref = config.event_console.as_ref();
        let monitor_buffer_size =
            ec_ref.map_or(EVENT_MONITOR_MAX_EVENTS, |ec| ec.buffer_size.max(1));
        let capture_midi = ec_ref.is_none_or(|ec| ec.capture_midi);
        let capture_processed = ec_ref.is_none_or(|ec| ec.capture_processed);
        let capture_actions = ec_ref.is_none_or(|ec| ec.capture_actions);
        let monitor_rate_limiter = ec_ref.and_then(|ec| {
            if ec.max_events_per_second > 0 {
                Some(MonitorRateLimiter::new(ec.max_events_per_second))
            } else {
                None
            }
        });
        let trigger_engine = crate::daemon::event_triggers::TriggerEngine::new(
            &ec_ref.map_or_else(Default::default, |ec| ec.triggers.clone()),
        );
        let track_latency = ec_ref.is_some_and(|ec| ec.enable_profiling || ec.track_latency);
        let track_memory = ec_ref.is_some_and(|ec| ec.enable_profiling || ec.track_memory);
        let event_monitor_active = Arc::new(AtomicBool::new(false));
        let event_monitor_buffer = Arc::new(std::sync::Mutex::new(VecDeque::with_capacity(
            monitor_buffer_size,
        )));
        // Push-based event broadcast channel (#394) — capacity 256
        // Lagged subscribers skip missed events rather than blocking the hot path
        let (event_broadcast_tx, _) = broadcast::channel::<MonitorEvent>(256);

        // Create shared state Arcs FIRST so both ToolExecutor and Self share them (#107)
        let state = Arc::new(RwLock::new(LifecycleState::Init));
        let device_status = Arc::new(RwLock::new(DeviceStatus::default()));
        let statistics = Arc::new(RwLock::new(DaemonStatistics::default()));
        let input_manager: Arc<Mutex<Option<InputManager>>> = Arc::new(Mutex::new(None));
        let start_time = Instant::now();

        // Create tool executor for LLM integration (ADR-007 Phase 2)
        // Pass MIDI Learn state to enable conductor_start/stop_midi_learn tools
        // Pass daemon state refs to enable live status reporting (#107)
        //
        // NOTE: Config sync design (intentional for TOCTOU protection):
        // The ToolExecutor receives its own config Arc (initially None) rather than
        // sharing EngineManager's config Arc. This is by design for Plan/Apply:
        // - Config is synced via set_config() before each tool execution
        // - This creates a snapshot at execution time for TOCTOU protection
        // - Plans compute their base_config_hash against this snapshot
        // - If config changes between plan creation and apply, hash mismatch fails apply
        // See: ApplyPlan and ExecuteMcpTool handlers below for sync points
        // Per-device rate limiter (v4.26.0 - ADR-009 D9)
        let device_rate_limiter =
            DeviceRateLimiter::new(config.advanced_settings.max_events_per_sec);

        // Phase 4.1: snapshot the master SysEx toggle before `config` is
        // moved into `config_arc` below. Without this, a config shipping
        // with `sysex_identity_probing = false` would still permit
        // probes until the first `reload_config` ran.
        let initial_sysex_probing_enabled = config.advanced_settings.sysex_identity_probing;

        // Phase 4.2: log per-device `no_probe = true` settings that are
        // silently overridden because the same binding declares a
        // `SysExIdentity` matcher. The matcher could not resolve
        // without a probe, so the daemon ignores `no_probe` for those
        // devices. Spec §4.2 requires a load-time warning so users can
        // spot the conflict without inspecting daemon behaviour. The
        // message describes the override only — probing may still be
        // suppressed by `sysex_identity_probing = false`,
        // `probe_on_connect = false`, missing paired output, or rate
        // limit (#976 review).
        for alias in config.endpoints_with_no_probe_sysex_override() {
            warn!(
                device = %alias,
                "`no_probe = true` is overridden for this device because its bindings \
                 include a SysExIdentity matcher (which cannot resolve without a probe); \
                 actual probing is still subject to the global / per-connect / \
                 rate-limit gates"
            );
        }

        // ADR-034 §D1 — daemon-managed live config seam.
        // D4.A.3.3.A: sole config Arc (the legacy `Arc<RwLock<Config>>`
        // mirror retired).
        let live_config = {
            // #2533: the boot-loaded config IS the first published snapshot →
            // `new_published` seeds it at state_generation = 1 (ADR-034 KI-A2/R6-A8),
            // NOT the gen-0 sentinel that `handle_get_config_body` blanks. Without
            // this a cold-booted daemon serves an empty `GetConfigBody` and the GUI
            // falls back to stale `config.toml`.
            let lc = crate::daemon::live_config::LiveConfig::new_published(config)
                .map_err(|e| DaemonError::Ipc(format!("live_config init: {e}")))?;
            // ADR-034 §D8: record config mutations to the durable audit outbox.
            // Now fail-closed (#2296 sub-slice C): an open failure makes the daemon
            // refuse config mutations (`AuditUnavailable`).
            //
            // Skipped in IN-CRATE unit-test builds (`cfg(test)`): `LiveConfig::new`
            // resolves the outbox from `$XDG_STATE_HOME` (a single shared file), so
            // the many parallel `EngineManager::new` unit tests would race on it —
            // interleaved appends corrupt the hash chain, the next open fails, and
            // since #2296 that now (correctly) refuses mutations, breaking tests
            // unrelated to auditing. Production and the isolated integration tests
            // in `tests/live_config_persist.rs` (which build their own per-`TempDir`
            // outbox via `new_with_paths().with_audit_outbox()`) keep full audit
            // recording and exercise the fail-closed path directly.
            #[cfg(not(test))]
            let lc = lc.with_audit_outbox();
            Arc::new(lc)
        };
        // #2071 (ADR-043 D2/Q2): seed the reconcile guard with the initial
        // snapshot's content revision. The constructor above already builds
        // the runtime (mapping engine, route engine, connector registry) to
        // match this config, so the first `reconcile_runtime_to_live` only
        // fires once a genuinely different config is committed.
        let initial_revision = live_config.load().revision;
        // D4.A.3.3.B.1: tool_executor now holds Arc<LiveConfig> directly —
        // the legacy Arc<RwLock<Option<Config>>> sync mailbox retired.
        let active_profile = Arc::new(ArcSwap::from_pointee(None));
        // ADR-026 Phase 2: shared SysEx probe coordinator. Constructed
        // here so both `SharedDaemonStateRefs` (for the MCP executor's
        // ReadOnly cache reads) and the `EngineManager` field (for the
        // command-handler write path) point at the same Arc.
        let probe_coordinator: Arc<conductor_core::device_intelligence::probe::ProbeCoordinator> =
            Arc::new(conductor_core::device_intelligence::probe::ProbeCoordinator::new());
        probe_coordinator.set_enabled(initial_sysex_probing_enabled);
        // `control_state` was constructed earlier (see ActionExecutor init)
        // so it is shared with the executor for ADR-025 state conditions.
        #[cfg(feature = "llm-executor")]
        let tool_state_refs = SharedDaemonStateRefs {
            lifecycle_state: Arc::clone(&state),
            device_status: Arc::clone(&device_status),
            statistics: Arc::clone(&statistics),
            input_manager: Arc::clone(&input_manager),
            config_path: config_path.clone(),
            start_time,
            command_tx: command_tx.clone(),
            active_profile: Arc::clone(&active_profile),
            event_stats: Arc::clone(&event_stats),
            control_state: Arc::clone(&control_state),
            probe_coordinator: Arc::clone(&probe_coordinator),
            connector_registry: Arc::clone(&connector_registry),
            device_output_map: Arc::clone(&device_output_map),
            route_engine: Arc::clone(&route_engine),
            dispatch_trace: Arc::clone(&dispatch_trace),
        };
        // Issue #1038: wire the disk-backed AuditLogger into the
        // ToolExecutor BEFORE the Arc-wrap so `set_audit_logger`
        // (which takes `&mut self`) can still reach it. Pre-fix
        // D13b/D13c had complete code paths but no production
        // call site instantiated `AuditLogger::new` — the audit
        // DB never appeared on disk and every audit-write site
        // was a silent no-op. `default_audit_logger()` returns
        // `None` on init failure (logging an `error!`); we still
        // proceed so the daemon stays up — losing audit while
        // keeping the daemon up is the lesser harm per ADR-027
        // §D13b ("tamper-evidence, not tamper-prevention").
        #[cfg(feature = "llm-executor")]
        let mut tool_executor_inner = ToolExecutor::with_daemon_state(
            Arc::clone(&live_config),
            midi_learn_active.clone(),
            midi_learn_events.clone(),
            tool_state_refs,
        );
        // ADR-027 D13a (#1167): build the audit logger once and
        // share the `Arc` — the ToolExecutor writes to it, and
        // EngineManager keeps a clone so the IPC layer can serve
        // `QueryAudit` and `SubscribeAudit` off the same instance
        // (one broadcast channel, one SQLite handle).
        #[cfg(feature = "audit-db")]
        let audit_logger = crate::daemon::audit::default_audit_logger();
        // ADR-045 D5 (#2493): sink selection. `audit-db` compositions use the
        // SQLite sink; without it — or when SQLite init failed — fall back to
        // the always-compiled JSONL sink so audit stays composition-
        // independent. `None` only when BOTH are unavailable (the D5
        // fail-closed trigger for network listeners).
        let audit_sink: Option<Arc<dyn crate::daemon::audit::AuditSink>> = {
            #[cfg(feature = "audit-db")]
            let sqlite = audit_logger
                .clone()
                .map(|l| l as Arc<dyn crate::daemon::audit::AuditSink>);
            #[cfg(not(feature = "audit-db"))]
            let sqlite: Option<Arc<dyn crate::daemon::audit::AuditSink>> = None;
            sqlite.or_else(|| {
                crate::daemon::audit::default_jsonl_sink()
                    .map(|s| s as Arc<dyn crate::daemon::audit::AuditSink>)
            })
        };
        #[cfg(feature = "llm-executor")]
        if let Some(ref sink) = audit_sink {
            tool_executor_inner.set_audit_logger(sink.clone());
        }
        #[cfg(feature = "llm-executor")]
        let tool_executor = Arc::new(tool_executor_inner);

        Ok(Self {
            live_config,
            // #2553: defaults to the authority path (the single-file `new()`
            // case). `service.rs` overrides via `set_user_file_path` when the
            // authority (`live.toml`) and user file (`config.toml`) differ.
            user_file_path: config_path.clone(),
            // #2564: no identity persistence until service.rs wires the state dir.
            active_profile_persist_dir: None,
            config_path,
            route_engine,
            dispatch_trace,
            mapping_engine: Arc::new(RwLock::new(mapping_engine)),
            action_executor: Arc::new(Mutex::new(action_executor)),
            // ADR-025 Phase 2 (fix #844): the ActionDispatcher spawns a
            // dedicated thread that owns its own ActionExecutor. Must
            // thread the control-state store through `spawn_with_state`
            // so Conditional actions evaluated on this (the production
            // hot) path can read `ActivePcIs` / `CcValueInRange` /
            // `NoteHeld` from live state. Without this the dispatcher's
            // executor has `control_state = None` and every state
            // condition silently returns `false`.
            // #2396: spawn_with_config injects the SHARED config ArcSwap + the
            // virtual-port watch receiver so the dispatch-thread executor reads
            // config EngineManager actually updates (OscForward endpoints, D17
            // allow-map) and creates virtual ports on-thread.
            action_dispatcher: crate::daemon::executor_thread::ActionDispatcher::spawn_with_config(
                Arc::clone(&device_output_map),
                Some(Arc::clone(&control_state)),
                Arc::clone(&shared_action_config),
                executor_vport_rx,
            ),
            shared_action_config,
            executor_vport_tx,
            device_output_map,
            connector_registry,
            recursion_guard: std::sync::Mutex::new(
                crate::daemon::recursion_guard::MidiRecursionGuard::new(),
            ),
            suppression_throttle: crate::daemon::suppression_throttle::SuppressionThrottle::new(),
            input_manager,
            device_event_tx,
            device_event_rx,
            event_processors,
            event_stats,
            device_port_name_cache: DashMap::new(),
            state,
            device_status,
            statistics,
            start_time,
            error_log: Arc::new(RwLock::new(Vec::new())),
            command_rx,
            command_tx,
            hot_plug_in_flight: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            background_tasks_spawned: AtomicBool::new(false),
            shutdown_tx,
            network_listeners: Vec::new(),
            // ADR-042 B-early (#1899): platform keychain gate; `None` if no home
            // dir (→ non-loopback binds fail-closed). Tests inject via
            // `set_network_bind_gate`.
            #[cfg(unix)]
            network_bind_gate: crate::security::NetworkBindGate::platform()
                .map(std::sync::Arc::new),
            midi_learn_active,
            midi_learn_events,
            event_monitor_active,
            event_monitor_buffer,
            event_monitor_max: monitor_buffer_size,
            event_broadcast_tx,
            event_seq: std::sync::atomic::AtomicU64::new(0),
            capture_midi,
            capture_processed,
            capture_actions,
            monitor_rate_limiter,
            trigger_engine: std::sync::Mutex::new(trigger_engine),
            track_latency,
            track_memory,
            pending_chord_event: Arc::new(Mutex::new(None)),
            current_mode: Arc::new(ArcSwap::new(initial_mode_state)),
            // ADR-040 D4 §4.2: locks are transient — always start unlocked, even
            // if a mode was persisted. The persisted mode is a fallback, never a
            // lock restore.
            mode_lock: Arc::new(ArcSwapOption::empty()),
            mode_mutation_lock: tokio::sync::Mutex::new(()),
            rule_set_version: std::sync::atomic::AtomicU64::new(1),
            #[cfg(feature = "llm-executor")]
            tool_executor,
            #[cfg(feature = "audit-db")]
            audit_logger,
            audit_sink,
            device_rate_limiter,
            active_profile,
            ui_mode: Arc::new(RwLock::new(None)), // ADR-032 P4 (#1089)
            watcher_retarget_tx: None,
            app_detector: None,
            config_write_suppress: Arc::new(tokio::sync::Mutex::new(None)),
            profile_cache: crate::daemon::profile_cache::ProfileCache::new(16),
            control_state,
            pending_pc_observation_check: None,
            pending_pc_observation_cancel: None,
            probe_coordinator,
            last_known_configured_ports: Arc::new(Mutex::new(HashSet::new())),
            last_reconciled_revision: Some(initial_revision),
        })
    }
}
