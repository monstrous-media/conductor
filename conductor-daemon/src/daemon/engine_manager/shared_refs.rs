// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! `SharedDaemonStateRefs` struct + impls, extracted from
//! `engine_manager::mod` (refactor #2073).

use super::*;

/// Shared state references for MCP server (ADR-007 Phase 2)
///
/// These are clones of Arc references that allow the MCP server
/// to read daemon state without blocking the engine manager.
pub struct SharedDaemonStateRefs {
    pub lifecycle_state: Arc<RwLock<LifecycleState>>,
    pub device_status: Arc<RwLock<DeviceStatus>>,
    pub statistics: Arc<RwLock<DaemonStatistics>>,
    pub input_manager: Arc<Mutex<Option<InputManager>>>,
    pub config_path: PathBuf,
    pub start_time: Instant,
    /// Command channel for triggering daemon actions (v4.23.0 - ADR-009 Phase 5)
    pub command_tx: mpsc::Sender<DaemonCommand>,
    /// Active profile info (Phase 1 - Issue #323)
    pub active_profile: Arc<ArcSwap<Option<ActiveProfileInfo>>>,
    /// Per-device event fingerprint stats (ADR-022 D7)
    pub event_stats: Arc<DashMap<String, EventStats>>,
    /// Physical control state store (ADR-025 Phase 1)
    pub control_state: Arc<PhysicalControlStateStore>,
    /// SysEx probe coordinator (ADR-026 Phase 1.B). The MCP executor
    /// reads cached identities and the snapshot directly; the actual
    /// probe (write-side) goes through `DaemonCommand::ProbeDeviceIdentity`
    /// because the executor doesn't own the output `MidiOutputManager`.
    pub probe_coordinator: Arc<conductor_core::device_intelligence::probe::ProbeCoordinator>,
    /// Signal-routing-graph runtime state (ADR-031 § 3.4 / P1B).
    /// Source of truth for connector status + bound ports. Currently
    /// read by the `conductor_get_resolved_routing_graph` MCP tool. Future
    /// readers (`conductor_get_routing_graph`, IPC `GetRoutingGraph` /
    /// `GetConnectorStatus`) are deferred — see Phase 1B PR #1156
    /// "Deferred" section.
    pub connector_registry: Arc<RwLock<crate::connector_registry::ConnectorRegistry>>,
    /// Resolved alias → port_name map shared with ActionExecutor
    /// (#1611). Exposed on the refs so the
    /// `conductor_get_resolved_routing_graph` MCP tool (#1598 Phase 2
    /// Step C) can compute `bound_port` / `connected` from the SAME
    /// authoritative source the Bindings panel + ActionExecutor read
    /// — avoids the duplication that bit Step C in its first round
    /// (registry's runtime fields were never populated, so the GUI
    /// showed every connector as unbound while Bindings showed them
    /// connected). See [[tdd-must-exercise-production-data-path]].
    pub device_output_map: Arc<ArcSwap<HashMap<String, String>>>,
    /// Compiled route engine (ADR-036 / Slice 9). Wait-free snapshot of
    /// the live routes, rebuilt on config reload alongside the rule set.
    /// Read by the `conductor_explain_route_match` MCP tool to evaluate a
    /// candidate event against the same routes the event pump dispatches.
    pub route_engine: Arc<ArcSwap<crate::route_engine::RouteEngine>>,
    /// Bounded ring buffer of recent route-dispatch decisions
    /// (ADR-036 §8 / Slice 9). Written by the event pump's
    /// `dispatch_route_outputs`; read by `conductor_get_dispatch_trace`.
    pub dispatch_trace: Arc<crate::daemon::dispatch_trace::DispatchTraceRing>,
}

/// Test-only constructors for `SharedDaemonStateRefs`.
///
/// Gated behind the `test-helpers` feature (#1608 pattern) so production
/// code can't accidentally build a refs bundle with inert/default state.
/// Used by the routing-tool integration tests (ADR-036 Slice 9) which
/// only exercise the `route_engine` + `dispatch_trace` reads.
#[cfg(any(test, feature = "test-helpers"))]
impl SharedDaemonStateRefs {
    /// Build a minimal refs bundle for routing-tool integration tests.
    /// Every field except `route_engine` / `dispatch_trace` gets an inert
    /// default; the caller supplies the two the explain / trace tools
    /// actually read. The command channel's receiver is dropped — these
    /// tests don't dispatch daemon commands.
    pub fn for_routing_tools_test(
        route_engine: Arc<ArcSwap<crate::route_engine::RouteEngine>>,
        dispatch_trace: Arc<crate::daemon::dispatch_trace::DispatchTraceRing>,
    ) -> Self {
        let (command_tx, _command_rx) = mpsc::channel(8);
        Self {
            lifecycle_state: Arc::new(RwLock::new(LifecycleState::Running)),
            device_status: Arc::new(RwLock::new(DeviceStatus::default())),
            statistics: Arc::new(RwLock::new(DaemonStatistics::default())),
            input_manager: Arc::new(Mutex::new(None)),
            config_path: PathBuf::from("/tmp/conductor-routing-test.toml"),
            start_time: Instant::now(),
            command_tx,
            active_profile: Arc::new(ArcSwap::from_pointee(None)),
            event_stats: Arc::new(DashMap::new()),
            control_state: Arc::new(
                conductor_core::control_state::PhysicalControlStateStore::default(),
            ),
            probe_coordinator: Arc::new(
                conductor_core::device_intelligence::probe::ProbeCoordinator::new(),
            ),
            connector_registry: Arc::new(RwLock::new(
                crate::connector_registry::ConnectorRegistry::from_config(&[]),
            )),
            device_output_map: Arc::new(ArcSwap::from_pointee(HashMap::new())),
            route_engine,
            dispatch_trace,
        }
    }
}

impl SharedDaemonStateRefs {
    /// Get a DaemonState snapshot from the shared refs
    pub async fn get_daemon_state(&self) -> DaemonState {
        let lifecycle_state = *self.lifecycle_state.read().await;
        let device_status = self.device_status.read().await.clone();
        let statistics = self.statistics.read().await.clone();

        // Get input mode from input manager
        // Return None when input_manager is not initialized
        // (LLM Council feedback v4.13.3: don't falsely report "MidiOnly")
        // v4.21.0: Extract raw data under lock, do JSON conversion after release
        let (input_mode, hid_devices) = {
            let raw_data = {
                let guard = self.input_manager.lock().await;
                guard.as_ref().map(|mgr| {
                    let mode = mgr.mode();
                    let gamepads = mgr.get_connected_gamepads();
                    (mode, gamepads)
                })
            };
            match raw_data {
                Some((mode, gamepads)) => {
                    let mode_str = match mode {
                        InputMode::MidiOnly => "MidiOnly".to_string(),
                        InputMode::GamepadOnly => "GamepadOnly".to_string(),
                        InputMode::Both => "Both".to_string(),
                    };
                    let devices = gamepads
                        .into_iter()
                        .map(|(id, name)| json!({"id": id, "name": name, "connected": true}))
                        .collect::<Vec<_>>();
                    (Some(mode_str), devices)
                }
                None => (None, vec![]),
            }
        };

        DaemonState {
            lifecycle_state: Some(lifecycle_state),
            device_status: Some(device_status),
            statistics: Some(statistics),
            input_mode,
            hid_devices,
            uptime_secs: self.start_time.elapsed().as_secs(),
            config_path: self.config_path.to_str().map(|s| s.to_string()),
            active_profile: (**self.active_profile.load()).clone(),
        }
    }
}
