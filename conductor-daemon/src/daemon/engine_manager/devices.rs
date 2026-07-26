// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! `EngineManager` methods extracted from `engine_manager::mod` (refactor #2073).

use super::*;

/// A non-loopback listener withheld by the ADR-042 B-early bind gate (#1899):
/// the fail-closed reason as an audit `summary` (with `acl_hash` for forensics)
/// plus a prominent operator message. Present in the withheld map ⇒ do not bind.
struct WithheldListener {
    audit_summary: String,
    message: String,
}

impl EngineManager {
    /// Materialize the configured `MidiVirtualPort` endpoints as real OS MIDI
    /// ports, tearing down any no longer configured (#2063 / ADR-035, ADR-031
    /// D10 "DAW proxy model").
    ///
    /// Called from every path that (re)builds the output map — initial connect,
    /// the post-commit reload APPLY, and the hot-plug rescan — so the OS ports
    /// always track the live `[[endpoints]]`. Without this a route to a virtual
    /// alias fails with "port not found" and external apps can't see it.
    ///
    /// Infallible (runs in the ADR-044 APPLY phase).
    ///
    /// #2396: the OS virtual ports MUST be created on the DISPATCH-thread
    /// executor (midir handles are thread-affine, ADR-009 D1), not on the
    /// mutex-guarded `self.action_executor` (which never dispatches — the bug).
    /// We send the desired names over the `watch` channel; the executor thread
    /// applies `sync_virtual_ports` between actions and logs the create/teardown
    /// report there. Latest-wins coalesces bursts; the receiver is alive for the
    /// daemon lifetime, so a transient "no receivers" only happens during
    /// shutdown (logged at debug).
    pub(crate) fn sync_virtual_ports(
        &self,
        endpoints: &[conductor_core::config::types::EndpointConfig],
    ) {
        let desired = crate::daemon::output_resolver::desired_virtual_port_names(endpoints);
        if self.executor_vport_tx.send(desired).is_err() {
            debug!("Virtual-port sync skipped: executor thread receiver gone (shutdown?)");
        }
    }

    /// List available MIDI devices (ADR-007 Phase 2)
    pub fn list_midi_devices() -> Result<Vec<MidiDeviceInfo>> {
        Self::enumerate_midi_devices()
    }

    /// Connect to input devices based on config (v4.27.0: always multi-device)
    ///
    /// Legacy single-device path removed — always uses `connect_multi_device()`.
    pub(crate) async fn connect_input_devices(&mut self) -> Result<()> {
        let config = (*self.live_config.load().config).clone();
        self.connect_multi_device(&config).await
    }

    /// Disconnect from input devices (v3.0)
    pub(crate) async fn disconnect_input_devices(&mut self) {
        info!("Disconnecting input devices");

        // Drop device manager (closes connections) (v3.0)
        if let Some(mut manager) = self.input_manager.lock().await.take() {
            manager.disconnect();
        }

        // ADR-042 Phase A: abort any running network listeners.
        self.stop_network_listeners();

        // Update device status
        self.update_device_status(false, None, None).await;

        info!("Input devices disconnected");
    }

    /// Start a network listener for every OSC/Art-Net Input endpoint in the
    /// config (ADR-042 Phase A, loopback-only). Idempotent — stops any running
    /// listeners first. A malformed network ACL fails startup (propagated); an
    /// individual socket bind failure is logged (`NetworkListenerBindFailed`)
    /// and skipped (best-effort). Accepted packets are drained to a Phase A
    /// placeholder (ADR-039 wires the OSC/Art-Net parser); rejections emit
    /// dedup'd tracing only (reject-audit decision).
    ///
    /// Thin wrapper over [`Self::bind_network_listeners`]: the only fallible
    /// step is the config/ACL parse (`ListenerManager::from_config`); the bind
    /// itself is non-fatal. (#2100 / ADR-044 split the parse out so the
    /// post-commit APPLY phase can bind an already-parsed set infallibly.)
    pub(crate) async fn start_network_listeners(&mut self, config: &Config) -> Result<()> {
        let manager = ListenerManager::from_config(config)
            .map_err(|e| DaemonError::Ipc(format!("network listener config error: {e}")))?;
        self.bind_network_listeners(manager, config).await;
        Ok(())
    }

    /// Bind an already-parsed [`ListenerManager`] — the **infallible** half of
    /// listener (re)start (#2100 / ADR-044). Stops the current set, refreshes
    /// the action-class allow-map from `config`, then binds each edge; a bind
    /// failure is non-fatal (logged + audited) and counted. Returns
    /// `(bound, skipped)`.
    pub(crate) async fn bind_network_listeners(
        &mut self,
        manager: ListenerManager,
        config: &Config,
    ) -> (usize, usize) {
        self.stop_network_listeners();

        // ADR-045 D5 invariant 3 (#2493): network listeners' audit trail is a
        // security control (ADR-042), not telemetry — with NO audit sink
        // available, refuse to start ANY listener (fail-closed). Everything
        // else in the daemon stays up (fail-open with the warning logged at
        // sink-init time).
        if self.audit_sink.is_none() && !manager.is_empty() {
            let withheld = manager.aliases().count();
            tracing::error!(
                listeners = withheld,
                "ADR-045 D5: no audit sink available — refusing to start \
                 network listeners (fail-closed; audit is a security control \
                 for ADR-042 listeners)"
            );
            return (0, withheld);
        }

        // ADR-042 D17 (Slice A.6.6) + ADR-039-A Slice 3 (#2326): refresh the
        // dispatch-thread executor's read-mostly config from the live endpoints:
        //   - per-listener `allow_sensitive_actions` (alias → bool) for the
        //     action-class gate;
        //   - OscForward output endpoints (alias → (host, port)).
        // #2396: store BOTH atomically in ONE `SharedActionConfig` via the shared
        // ArcSwap, so a dispatch can never observe new endpoints against an old
        // allow-map (no cross-map torn read), and — critically — so they reach
        // the executor that actually DISPATCHES (previously set on the
        // non-dispatching `self.action_executor`). Done before any early return
        // so removing all listeners also clears the maps.
        {
            use conductor_core::config::types::{ConnectorDirection, EndpointKind};
            let mut network_sensitive_allow: HashMap<String, bool> = HashMap::new();
            let mut osc_output_endpoints: HashMap<String, (String, u16)> = HashMap::new();
            for ep in &config.endpoints {
                if !ep.enabled {
                    continue;
                }
                let is_listener = matches!(
                    ep.direction,
                    ConnectorDirection::Input | ConnectorDirection::Bidirectional
                );
                let is_output = matches!(
                    ep.direction,
                    ConnectorDirection::Output | ConnectorDirection::Bidirectional
                );
                // Only OSC/Art-Net *listeners* (Input/Bidirectional) become a
                // network origin (Copilot review on #1953).
                if is_listener
                    && let EndpointKind::OscEndpoint { security, .. }
                    | EndpointKind::ArtNetEndpoint { security, .. } = &ep.kind
                {
                    network_sensitive_allow
                        .insert(ep.alias.clone(), security.allow_sensitive_actions);
                }
                // OscForward targets must be OSC *output* endpoints.
                if is_output && let EndpointKind::OscEndpoint { host, port, .. } = &ep.kind {
                    osc_output_endpoints.insert(ep.alias.clone(), (host.clone(), *port));
                }
            }
            self.shared_action_config.store(std::sync::Arc::new(
                crate::action_executor::SharedActionConfig {
                    network_sensitive_allow,
                    osc_output_endpoints,
                },
            ));
        }

        if manager.is_empty() {
            return (0, 0);
        }

        // Tracing-only audit sink for edge rejections.
        struct ListenerTracingAudit;
        impl EdgeAuditSink for ListenerTracingAudit {
            fn emit(&self, listener: &str, source: std::net::IpAddr, kind: AuditEventKind) {
                warn!(listener = %listener, %source, ?kind, "network listener rejected packet");
            }
        }
        let audit: Arc<dyn EdgeAuditSink> = Arc::new(ListenerTracingAudit);

        // Per-listener protocol map (alias → protocol) so the shared drain task
        // dispatches each accepted packet to the right parser. Built before
        // `into_edges()` consumes `manager`.
        let listener_protocols: std::collections::HashMap<
            String,
            conductor_core::config::types::ConnectorProtocol,
        > = manager
            .aliases()
            .filter_map(|a| manager.edge(a).map(|e| (a.to_string(), e.protocol())))
            .collect();

        // Accepted-packet consumer. ADR-039-A Slice 1 (#1361): OSC listeners'
        // packets are decoded to `OscInbound` and forwarded onto the unified
        // pump as `ProtocolEvent::Osc`; the route engine consumes them
        // (route-only — never the mapping engine). Non-OSC listeners (Art-Net,
        // ADR-039-C) still have no consumer — drained + activity-audited, then
        // dropped, exactly as before.
        let activity_logger = self.audit_sink.clone();
        let pump_tx = self.device_event_tx.clone();
        let (packet_tx, mut packet_rx) = mpsc::channel::<AcceptedPacket>(1024);
        tokio::spawn(async move {
            let activity_dedup = AuditRateLimiter::new();
            while let Some(pkt) = packet_rx.recv().await {
                trace!(
                    listener = %pkt.listener,
                    source = %pkt.source,
                    bytes = pkt.data.len(),
                    "accepted network packet"
                );
                if let Some(logger) = &activity_logger
                    && activity_dedup.should_emit(
                        &pkt.listener,
                        pkt.source,
                        AuditEventKind::Activity,
                    )
                {
                    logger.log_network_event(
                        crate::daemon::audit::AuditEventType::NetworkListenerActivity,
                        &pkt.listener,
                        pkt.source,
                        Some("accepted"),
                    );
                }

                // ADR-039-A: only OSC listeners have a consumer in Slice 1.
                if listener_protocols.get(&pkt.listener)
                    != Some(&conductor_core::config::types::ConnectorProtocol::Osc)
                {
                    continue;
                }
                match crate::osc_parser::parse_osc_datagram(&pkt.data, std::time::Instant::now()) {
                    Ok(parsed) => {
                        if parsed.amplification_capped
                            && let Some(logger) = &activity_logger
                            && activity_dedup.should_emit(
                                &pkt.listener,
                                pkt.source,
                                AuditEventKind::RateLimited,
                            )
                        {
                            // Reuse the RateLimited audit kind: an amplification
                            // trip is a shed-load event on this listener.
                            logger.log_network_event(
                                crate::daemon::audit::AuditEventType::NetworkListenerActivity,
                                &pkt.listener,
                                pkt.source,
                                Some("osc_amplification_capped"),
                            );
                        }
                        let device_id = DeviceId::from_alias(&pkt.listener);
                        for msg in parsed.messages {
                            let event =
                                DeviceEvent::new(device_id.clone(), ProtocolEvent::Osc(msg));
                            if pump_tx.try_send(event).is_err() {
                                // Drop-newest backpressure (same discipline as the
                                // MIDI/HID pump). Visible via the dedup'd audit so
                                // sustained shed load is observable.
                                if let Some(logger) = &activity_logger
                                    && activity_dedup.should_emit(
                                        &pkt.listener,
                                        pkt.source,
                                        AuditEventKind::RateLimited,
                                    )
                                {
                                    logger.log_network_event(
                                        crate::daemon::audit::AuditEventType::NetworkListenerActivity,
                                        &pkt.listener,
                                        pkt.source,
                                        Some("osc_pump_full_dropped"),
                                    );
                                }
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        debug!(listener = %pkt.listener, source = %pkt.source, ?e, "OSC decode failed; dropping datagram");
                    }
                }
            }
        });

        let edges: Vec<_> = manager.into_edges().collect();
        // ADR-042 B-early (#1899): which non-loopback listeners are withheld
        // pending an HMAC-verified approval (fail-closed). Loopback + approved
        // listeners are absent from the map and bind normally.
        let withheld = self.network_bind_decisions(&edges, config).await;

        let mut bound = 0usize;
        let mut skipped = 0usize;
        for edge in edges {
            let edge = Arc::new(edge);
            // Withhold non-loopback listeners that did not clear the bind gate:
            // never spawn the socket; surface a prominent operator warning + audit.
            if let Some(w) = withheld.get(edge.alias()) {
                skipped += 1;
                warn!(listener = %edge.alias(), "{}", w.message);
                if let Some(logger) = &self.audit_sink {
                    logger.log_network_event(
                        crate::daemon::audit::AuditEventType::NetworkListenerApproval,
                        edge.alias(),
                        edge.host(),
                        Some(&w.audit_summary),
                    );
                }
                continue;
            }
            match spawn_listener(
                Arc::clone(&edge),
                packet_tx.clone(),
                Arc::clone(&audit),
                self.shutdown_tx.subscribe(),
            )
            .await
            {
                Ok(listener) => {
                    info!(
                        listener = %listener.alias(),
                        addr = %listener.local_addr(),
                        "network listener started"
                    );
                    self.network_listeners.push(listener);
                    bound += 1;
                }
                Err(e) => {
                    skipped += 1;
                    // ADR-042 Phase A (Slice A.6.1): a port already in use is an
                    // orphaned / conflicting listener (likely another conductor
                    // instance); other bind errors are genuine bind failures.
                    // Detection only — the listener is skipped, never force-killed.
                    if crate::listeners::startup_cleanup::is_orphaned_port(&e) {
                        warn!(
                            listener = %edge.alias(),
                            port = edge.port(),
                            "network listener port already in use — {}",
                            crate::listeners::startup_cleanup::ORPHAN_HINT
                        );
                        if let Some(logger) = &self.audit_sink {
                            logger.log_network_event(
                                crate::daemon::audit::AuditEventType::ListenerOrphanedAtStartup,
                                edge.alias(),
                                edge.host(),
                                Some(crate::listeners::startup_cleanup::ORPHAN_HINT),
                            );
                        }
                    } else {
                        warn!(listener = %edge.alias(), error = %e, "network listener bind failed");
                        if let Some(logger) = &self.audit_sink {
                            logger.log_network_event(
                                crate::daemon::audit::AuditEventType::NetworkListenerBindFailed,
                                edge.alias(),
                                edge.host(),
                                Some(&e.to_string()),
                            );
                        }
                    }
                }
            }
        }
        (bound, skipped)
    }

    /// ADR-042 B-early (#1899): decide which **non-loopback** listeners are
    /// withheld pending an HMAC-verified approval. Returns only the withheld
    /// aliases (loopback + approved are absent → they bind). Every gate failure
    /// — no gate, keychain unavailable/expired, registry tampered, no approval —
    /// is fail-closed (the listener is withheld), so a non-loopback socket never
    /// binds approval-less.
    ///
    /// Gate edges are built from [`list_listeners`](crate::security::approval_admin::list_listeners)
    /// — the **same** source `conductorctl listener approve` keys on (raw config
    /// host string + raw `network_acl`) — so approve-time and bind-time approval
    /// keys match exactly.
    #[cfg(unix)]
    async fn network_bind_decisions(
        &self,
        edges: &[crate::listeners::ListenerEdge],
        config: &Config,
    ) -> std::collections::HashMap<String, WithheldListener> {
        use crate::security::{BindVerdict, GateEdge, WithholdReason, compute_acl_hash};
        use conductor_core::security::NetworkAcl;

        // Only non-loopback edges are gated; loopback binds unconditionally.
        let nonloopback: std::collections::HashSet<String> = edges
            .iter()
            .filter(|e| !NetworkAcl::is_loopback_address(&e.host()))
            .map(|e| e.alias().to_string())
            .collect();
        if nonloopback.is_empty() {
            return std::collections::HashMap::new();
        }

        // No gate (no resolvable home dir) → withhold all non-loopback, fail-closed.
        let Some(gate) = self.network_bind_gate.clone() else {
            return nonloopback
                .iter()
                .map(|alias| {
                    let r = WithholdReason::KeychainUnavailable;
                    (
                        alias.clone(),
                        WithheldListener {
                            audit_summary: r.audit_summary().to_string(),
                            message: r.operator_message(alias),
                        },
                    )
                })
                .collect();
        };

        let infos: Vec<crate::security::approval_admin::ListenerInfo> =
            crate::security::approval_admin::list_listeners(config)
                .into_iter()
                .filter(|i| nonloopback.contains(&i.alias))
                .collect();
        let fallback_aliases: Vec<String> = infos.iter().map(|i| i.alias.clone()).collect();

        // The gate read (keychain + registry I/O) is blocking — run it off the
        // async runtime. A join error (panic) is itself fail-closed below.
        let decisions: Vec<(String, BindVerdict, String)> =
            tokio::task::spawn_blocking(move || {
                let gate_edges: Vec<GateEdge> = infos
                    .iter()
                    .map(|i| GateEdge {
                        alias: &i.alias,
                        host: &i.host,
                        port: i.port,
                        acl_entries: &i.acl_entries,
                        requires_amplification_ack: i.requires_amplification_ack,
                    })
                    .collect();
                let verdicts = gate.evaluate(&gate_edges);
                infos
                    .iter()
                    .zip(verdicts)
                    .map(|(i, v)| (i.alias.clone(), v, compute_acl_hash(&i.acl_entries)))
                    .collect()
            })
            .await
            .unwrap_or_else(|_| {
                fallback_aliases
                    .into_iter()
                    .map(|a| {
                        (
                            a,
                            BindVerdict::Withhold(WithholdReason::KeychainUnavailable),
                            String::new(),
                        )
                    })
                    .collect()
            });

        decisions
            .into_iter()
            .filter_map(|(alias, verdict, acl_hash)| match verdict {
                BindVerdict::Bind => None,
                BindVerdict::Withhold(reason) => Some((
                    alias.clone(),
                    WithheldListener {
                        audit_summary: format!("{}; acl_hash={acl_hash}", reason.audit_summary()),
                        message: reason.operator_message(&alias),
                    },
                )),
            })
            .collect()
    }

    /// Non-Unix: the approval registry + keychain-init wiring rely on hardened-file
    /// APIs (`O_NOFOLLOW`/`fstat`/`flock`) that are Unix-only, so the approval
    /// mechanism doesn't exist on this platform — every non-loopback listener is
    /// withheld fail-closed.
    #[cfg(not(unix))]
    async fn network_bind_decisions(
        &self,
        edges: &[crate::listeners::ListenerEdge],
        _config: &Config,
    ) -> std::collections::HashMap<String, WithheldListener> {
        use conductor_core::security::NetworkAcl;
        edges
            .iter()
            .filter(|e| !NetworkAcl::is_loopback_address(&e.host()))
            .map(|e| {
                (
                    e.alias().to_string(),
                    WithheldListener {
                        audit_summary: "keychain_unavailable".to_string(),
                        message: format!(
                            "network listener '{}' withheld: keychain-gated approval (Unix-only \
                             hardened-file registry) is unavailable on this platform",
                            e.alias()
                        ),
                    },
                )
            })
            .collect()
    }

    /// Test-only injection of the bind gate (a mock-keychain-backed gate).
    #[cfg(all(test, unix))]
    pub(crate) fn set_network_bind_gate(
        &mut self,
        gate: std::sync::Arc<crate::security::NetworkBindGate>,
    ) {
        self.network_bind_gate = Some(gate);
    }

    /// Abort all running network listeners (disconnect / reload / shutdown).
    pub(crate) fn stop_network_listeners(&mut self) {
        for listener in self.network_listeners.drain(..) {
            listener.abort();
        }
    }

    /// `(alias, bound addr)` for each running network listener. Backs the
    /// `GetListenerStatus` IPC (A.6b-3c) and tests.
    pub fn network_listener_status(&self) -> Vec<(String, std::net::SocketAddr)> {
        self.network_listeners
            .iter()
            .map(|l| (l.alias().to_string(), l.local_addr()))
            .collect()
    }

    /// Switch to a different MIDI device by port index
    ///
    /// This method switches the MIDI connection to a different port while preserving
    /// the InputManager and gamepad connections (if any).
    ///
    /// # Arguments
    ///
    /// * `port_index` - The MIDI port index to connect to
    ///
    /// # Returns
    ///
    /// A tuple of (port_name, port_index) on success
    pub(crate) async fn switch_device(&mut self, port_index: usize) -> Result<(String, usize)> {
        info!("Switching to MIDI device at port {}", port_index);

        // Get the input manager
        let mut input_manager_guard = self.input_manager.lock().await;
        let input_manager = input_manager_guard
            .as_mut()
            .ok_or_else(|| DaemonError::Ipc("Input manager not initialized".to_string()))?;

        // Reconnect to the specified port (#885: legacy listener now emits
        // DeviceEvent<InputEvent> on the unified channel)
        let (port_idx, port_name) = input_manager
            .reconnect_midi_port(
                port_index,
                self.device_event_tx.clone(),
                self.command_tx.clone(),
            )
            .map_err(|e| DaemonError::Ipc(format!("Failed to connect to port: {}", e)))?;

        // Update device status
        self.update_device_status(true, Some(port_name.clone()), Some(port_idx))
            .await;

        info!(
            "Connected to MIDI device: {} (port {})",
            port_name, port_idx
        );
        Ok((port_name, port_idx))
    }

    /// Process hot-plug check — rescan MIDI ports for new/removed devices (v4.22.0 - ADR-009 Phase 4)
    /// Apply a hot-plug rescan from ports ALREADY enumerated off the run-loop
    /// (#2390). `HotPlugCheck` spawns the slow CoreMIDI enumeration and
    /// re-delivers the result as `HotPlugApply { port_infos, gamepad_available }`;
    /// this does only the cheap diff/open, so it's safe to run inline on the
    /// run-loop.
    ///
    /// #2392: `gamepad_available` is the result of the (fixed ~500ms when no
    /// controller) gilrs probe, run off-loop by the spawning task. The connect
    /// itself (`rescan_gamepad`) is cheap when a controller is actually present,
    /// so it stays here under the lock — but ONLY when one was found.
    pub(crate) async fn process_hot_plug_apply(
        &mut self,
        port_infos: Vec<conductor_core::resolver::PortInfo>,
        gamepad_available: bool,
    ) -> Result<()> {
        // Only rescan during Running state — skip during Reloading, Reconnecting, etc.
        let current_state = *self.state.read().await;
        if current_state != LifecycleState::Running {
            return Ok(());
        }
        let config = (*self.live_config.load().config).clone();

        // Bound the input_manager lock to this scope so it's
        // guaranteed released before `dispatch_probe_on_connect_for_new_ports`
        // runs below — that method re-acquires the same lock and
        // would deadlock if mgr_guard were held across it. PR #930
        // review caught this on the no-change path (the explicit
        // `drop(mgr_guard);` only fires inside the
        // `opened > 0 || removed > 0` branch). Wrapping in a block
        // makes the release path uniform across all branches: NLL
        // drops the guard at the closing `}` if it wasn't moved
        // earlier, and the inner explicit `drop` (still needed
        // because the ADR-021 await chain runs *inside* the
        // changes-branch and must release before those awaits)
        // simply runs first when applicable.
        {
            let mut mgr_guard = self.input_manager.lock().await;
            if let Some(ref mut mgr) = *mgr_guard {
                match mgr.rescan_ports(port_infos, &config, &self.device_event_tx, &self.command_tx)
                {
                    Ok((opened, removed, rekeyed)) => {
                        if opened > 0 || removed > 0 || rekeyed > 0 {
                            info!(
                                opened = opened,
                                removed = removed,
                                rekeyed = rekeyed,
                                "Hot-plug rescan: {} new, {} removed, {} re-keyed",
                                opened,
                                removed,
                                rekeyed
                            );

                            // Capture binding data and enabled state while holding the lock
                            let device_bindings = mgr.get_device_bindings();
                            let enabled_states: Vec<bool> = device_bindings
                                .iter()
                                .map(|(device_id, _, _, _)| mgr.is_device_enabled(device_id))
                                .collect();

                            // Release input_manager lock before MIDI I/O and device_status lock
                            drop(mgr_guard);

                            // ADR-021 Phase 1B: Re-enumerate output ports on hot-plug (async, off-thread)
                            let output_ports =
                                crate::daemon::output_resolver::enumerate_output_ports_async()
                                    .await;
                            let input_bindings: Vec<(String, String)> = device_bindings
                                .iter()
                                .filter(|(_, _, _, is_configured)| *is_configured)
                                .map(|(device_id, port_name, _, _)| {
                                    (device_id.as_str().to_string(), port_name.clone())
                                })
                                .collect();
                            // ADR-035 Slice 9.5: unified endpoint set drives both
                            // the output map and hot-plug pickup for output/
                            // bidirectional endpoints (subsumes the legacy device
                            // + #1611 connector builders).
                            let (endpoints, _findings) =
                                conductor_core::config::loader::normalize_to_endpoints(&config)?;
                            let output_map = crate::daemon::output_resolver::build_output_map(
                                &endpoints,
                                &input_bindings,
                                &output_ports,
                            );

                            // ADR-021 Phase 2A: Update shared device_output_map on hot-plug rescan
                            let flat_map: HashMap<String, String> = output_map
                                .iter()
                                .map(|(alias, res)| (alias.clone(), res.port_name.clone()))
                                .collect();
                            self.device_output_map.store(Arc::new(flat_map));

                            // #2063: recreate/teardown OS virtual MIDI ports on
                            // hot-plug rescan so they survive device changes.
                            self.sync_virtual_ports(&endpoints);

                            let device_statuses: Vec<DevicePortStatus> = device_bindings
                                .iter()
                                .zip(enabled_states.iter())
                                .map(
                                    |(
                                        (device_id, port_name, connected, is_configured),
                                        enabled,
                                    )| {
                                        let alias = device_id.as_str();
                                        let io = resolve_device_io(
                                            alias,
                                            *is_configured,
                                            &endpoints,
                                            &output_map,
                                        );
                                        DevicePortStatus {
                                            device_id: alias.to_string(),
                                            port_name: port_name.clone(),
                                            port_index: 0,
                                            connected: *connected,
                                            enabled: *enabled,
                                            last_event_at: None,
                                            is_configured: *is_configured,
                                            direction: io.direction,
                                            output_connected: io.output_connected,
                                            output_port_name: io.output_port_name,
                                            output_auto_paired: io.output_auto_paired,
                                            // TODO(#742): Currently MIDI-only. Derive from binding source
                                            // when HID/OSC devices are added to the binding path.
                                            protocol: "midi".to_string(),
                                        }
                                    },
                                )
                                .collect();

                            self.rebuild_port_name_cache(&device_statuses);

                            let any_connected = device_bindings.iter().any(|(_, _, c, _)| *c);
                            let first_name =
                                device_bindings.first().map(|(_, name, _, _)| name.clone());

                            let mut status = self.device_status.write().await;
                            status.connected = any_connected;
                            status.name = first_name;
                            status.devices = device_statuses;
                        }
                    }
                    Err(e) => {
                        debug!("Hot-plug rescan failed: {}", e);
                    }
                }
            }
        } // mgr_guard guaranteed released here — no deadlock with the dispatch below

        // ADR-039-B #2293: gamepad hot-plug. The MIDI rescan above is MIDI-only
        // (`rescan_ports`), so a gamepad switched on / paired AFTER daemon start
        // was never picked up (the gamepad was only connected at startup).
        //
        // #2392: the SLOW part — the gilrs `list_gamepads` probe, a FIXED ~500ms
        // window when no controller is connected — now runs OFF the run-loop in
        // the task `HotPlugCheck` spawns; its boolean result arrives as
        // `gamepad_available`. Here we only do the cheap connect, and only when a
        // controller was actually found. We re-check `needs_gamepad_rescan` under
        // the lock because state may have changed since the off-loop probe (e.g.
        // a connect already happened on a prior tick). `rescan_gamepad`/connect
        // finds the already-present controller immediately, so it does not pay
        // the discovery window here.
        if gamepad_available {
            let mut guard = self.input_manager.lock().await;
            if let Some(ref mut mgr) = *guard
                && mgr.needs_gamepad_rescan()
            {
                mgr.rescan_gamepad(&self.device_event_tx, &self.command_tx);
            }
        }

        // ADR-026 Phase 3.C.2: fire probe-on-connect for any
        // newly-bound configured ports the rescan revealed.
        // Internally diff'd against `last_known_configured_ports`,
        // so ports that were already bound before this rescan stay
        // unprobed (avoiding rate-limit churn). The dispatcher
        // re-locks `input_manager` to read fresh bindings, hence
        // the explicit scope above.
        self.dispatch_probe_on_connect_for_new_ports(&config).await;

        Ok(())
    }

    /// Enumerate available MIDI devices
    pub(crate) fn enumerate_midi_devices() -> Result<Vec<crate::daemon::types::MidiDeviceInfo>> {
        // Delegate to shared utility with warmup pattern (#104)
        Ok(crate::daemon::device_utils::enumerate_midi_devices_fresh())
    }
}
