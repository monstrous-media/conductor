// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

use super::*;

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
async fn test_engine_manager_creation() {
    let config = create_test_config();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);

    let manager = EngineManager::new(
        config,
        PathBuf::from("/tmp/test.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    );

    assert!(manager.is_ok());
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new (Enigo) requires display server
async fn network_listeners_start_and_stop() {
    // ADR-042 Phase A (Slice A.6b-3b): a loopback OSC Input endpoint binds a
    // listener on connect and is torn down on stop. Port 0 → OS-assigned.
    let config: Config = toml::from_str(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "osc_in"
direction = "Input"
type = "OscEndpoint"
host = "127.0.0.1"
port = 0
"#,
    )
    .expect("config parses");

    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);
    let mut mgr = EngineManager::new(
        config.clone(),
        PathBuf::from("/tmp/test_net_listeners.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("engine manager builds");

    mgr.start_network_listeners(&config)
        .await
        .expect("listeners start");
    let status = mgr.network_listener_status();
    assert_eq!(status.len(), 1, "one OSC listener bound");
    assert_eq!(status[0].0, "osc_in");
    assert_eq!(
        status[0].1.ip(),
        "127.0.0.1".parse::<std::net::IpAddr>().unwrap()
    );
    assert_ne!(status[0].1.port(), 0, "OS assigned a concrete port");

    mgr.stop_network_listeners();
    assert!(
        mgr.network_listener_status().is_empty(),
        "listeners stopped"
    );
}

/// ADR-042 Phase B-early bind gate (#1899): a non-loopback listener with no
/// approval is **withheld** (never binds), while a loopback listener in the same
/// config binds unconditionally. Uses an injected gate whose keychain is
/// unavailable → fail-closed; this proves the bind loop honours the gate verdict
/// (the gate's own unit tests cover approved / tampered / expired outcomes).
#[cfg(unix)]
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new (Enigo) requires display server
async fn b_early_non_loopback_listener_withheld_without_approval() {
    use crate::security::{KeychainProvider, NetworkBindGate};
    use conductor_core::security::keychain::{KeychainError, KeychainStore};
    use std::sync::Arc;

    struct DenyProvider;
    impl KeychainProvider for DenyProvider {
        // Full `std::result::Result` — `super::*` pulls in the daemon's
        // `Result<T>` alias which would shadow the two-arg form.
        fn keychain(&self) -> std::result::Result<Box<dyn KeychainStore>, KeychainError> {
            Err(KeychainError::Backend("test: gate denies".into()))
        }
    }

    let config: Config = toml::from_str(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "osc_lo"
direction = "Input"
type = "OscEndpoint"
host = "127.0.0.1"
port = 0

[[endpoints]]
alias = "osc_lan"
direction = "Input"
type = "OscEndpoint"
host = "0.0.0.0"
port = 19347
allow_network = true
network_acl = ["192.168.1.0/24"]
"#,
    )
    .expect("config parses");

    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);
    let mut mgr = EngineManager::new(
        config.clone(),
        PathBuf::from("/tmp/test_b_early_gate.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("engine manager builds");

    // Inject a fail-closed gate BEFORE binding (replaces the platform gate so the
    // test never touches the real OS keychain).
    let tmp = tempfile::tempdir().unwrap();
    let gate = NetworkBindGate::new(
        Arc::new(DenyProvider),
        tmp.path().join("network_approvals.json"),
        tmp.path().join("security"),
    );
    mgr.set_network_bind_gate(Arc::new(gate));

    mgr.start_network_listeners(&config)
        .await
        .expect("start_network_listeners returns Ok (withholding is not an error)");

    let status = mgr.network_listener_status();
    assert_eq!(
        status.len(),
        1,
        "only the loopback listener binds; the non-loopback one is withheld: {status:?}"
    );
    assert_eq!(
        status[0].0, "osc_lo",
        "the loopback listener is the bound one"
    );
    assert!(
        !status.iter().any(|(alias, _)| alias == "osc_lan"),
        "the non-loopback listener must be withheld fail-closed"
    );

    mgr.stop_network_listeners();
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new (Enigo) requires display server
async fn network_listeners_rebind_on_config_change() {
    // The reload path calls start_network_listeners(&new_config); re-evaluating
    // against a changed config hot-unbinds removed listeners and binds new ones.
    let with_osc: Config = toml::from_str(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "osc_in"
direction = "Input"
type = "OscEndpoint"
host = "127.0.0.1"
port = 0
"#,
    )
    .expect("config parses");
    let without: Config = toml::from_str(
        r#"
[[modes]]
name = "Default"
"#,
    )
    .expect("config parses");

    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);
    let mut mgr = EngineManager::new(
        with_osc.clone(),
        PathBuf::from("/tmp/test_net_rebind.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("engine manager builds");

    mgr.start_network_listeners(&with_osc).await.unwrap();
    assert_eq!(mgr.network_listener_status().len(), 1, "bound");

    // reload to a config with the listener removed → hot-unbind
    mgr.start_network_listeners(&without).await.unwrap();
    assert!(
        mgr.network_listener_status().is_empty(),
        "unbound on removal"
    );

    // reload to re-add it → hot-bind
    mgr.start_network_listeners(&with_osc).await.unwrap();
    assert_eq!(mgr.network_listener_status().len(), 1, "re-bound");
}

// ===== ADR-042 Phase A — Slice A.7 end-to-end (MERGE GATE) =====

#[cfg(feature = "audit-db")]
/// Phase A end-to-end: a loopback OSC listener binds, a real UDP packet sent
/// to it is accepted by the edge, and a NetworkListenerActivity audit row is
/// persisted — exercising EngineManager → spawn_listener → receive loop →
/// edge.admit → drain task → log_network_event end to end. Restarting
/// re-binds cleanly (no leaked state). (The "loopback OSC → Shell refused"
/// case awaits the ADR-039 parser; the gate itself is covered by the
/// action_executor gate unit tests.)
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new (Enigo) requires display server
async fn phase_a_e2e_loopback_osc_binds_accepts_and_audits() {
    use std::net::IpAddr;
    use std::time::Duration;

    let config: Config = toml::from_str(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "osc_in"
direction = "Input"
type = "OscEndpoint"
host = "127.0.0.1"
port = 0
"#,
    )
    .expect("config parses");

    let logger = std::sync::Arc::new(
        crate::daemon::audit::AuditLogger::in_memory().expect("in-memory audit logger"),
    );

    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);
    let mut mgr = EngineManager::new(
        config.clone(),
        PathBuf::from("/tmp/test_phase_a_e2e.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("engine manager builds");
    mgr.set_audit_logger_for_test(std::sync::Arc::clone(&logger));

    // Bind the loopback listener.
    mgr.start_network_listeners(&config).await.unwrap();
    let status = mgr.network_listener_status();
    assert_eq!(status.len(), 1, "loopback OSC listener bound");
    let (_alias, addr) = &status[0];
    assert_eq!(addr.ip(), "127.0.0.1".parse::<IpAddr>().unwrap());

    // Send a real UDP packet to the bound port.
    let client = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    client.send_to(b"/ping\0\0\0", *addr).await.unwrap();

    // The drain task emits a NetworkListenerActivity audit row for the
    // accepted packet. Poll the audit DB (bounded — no fixed sleep).
    let query = crate::daemon::audit::AuditQuery {
        event_type: Some(crate::daemon::audit::AuditEventType::NetworkListenerActivity),
        ..Default::default()
    };
    let mut found = None;
    for _ in 0..200 {
        let entries = logger.query(&query).expect("query");
        if let Some(e) = entries.into_iter().next() {
            found = Some(e);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let entry = found.expect("NetworkListenerActivity audit row for the accepted packet");
    assert_eq!(entry.tool_name.as_deref(), Some("osc_in"));

    // Restart: stop then start re-binds cleanly (no leaked socket state).
    mgr.stop_network_listeners();
    assert!(mgr.network_listener_status().is_empty());
    mgr.start_network_listeners(&config).await.unwrap();
    assert_eq!(
        mgr.network_listener_status().len(),
        1,
        "re-binds after restart"
    );
}

/// ADR-042 Phase B-early A.2 lift: a non-loopback listener that sets
/// `allow_network` plus a `network_acl` is no longer a config-load error — it is
/// permitted at config-load and gated on an HMAC-verified approval at BIND time.
/// Without `allow_network` it would still be a config-load error (covered in
/// conductor-core's network_listener_validation_test).
#[test]
fn b_early_non_loopback_listener_with_allow_network_passes_config_load() {
    let config: Config = toml::from_str(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "osc_in"
direction = "Input"
type = "OscEndpoint"
host = "0.0.0.0"
port = 9000
allow_network = true
network_acl = ["192.168.1.0/24"]
"#,
    )
    .expect("config parses (validation is separate)");

    let report = conductor_core::config::validation::validate_config(&config);
    assert!(
        report.is_valid(),
        "B-early lift: allow_network + network_acl permits a non-loopback listener \
         at config-load (gated at bind): {:?}",
        report.errors
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new enumerates MIDI ports
async fn prepare_runtime_rejects_malformed_listener_config() {
    // #2100 / ADR-044 Phase 1: PREPARE (`prepare_runtime`) is the fallible
    // seam — it parses the network-listener set (among compiling the mapping
    // engine and normalizing endpoints) without installing anything. A config
    // whose listener ACL can't be parsed must fail PREPARE. Phase 2 runs this
    // BEFORE the LiveConfig commit so such a config is rejected atomically,
    // never leaving config committed but runtime stale.
    let initial = create_test_config();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);
    let manager = EngineManager::new(
        initial,
        PathBuf::from("/tmp/test-2100-prep-bad.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("EngineManager constructs");

    let bad: Config = toml::from_str(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "osc_bad"
direction = "Input"
type = "OscEndpoint"
host = "127.0.0.1"
port = 0
network_acl = ["not-a-valid-cidr"]
"#,
    )
    .expect("config parses (listener ACL validity is checked in PREPARE, not parse)");

    let result = manager.prepare_runtime(&bad).await;
    assert!(
        result.is_err(),
        "#2100: prepare_runtime must reject a malformed listener ACL (the fallible PREPARE seam)"
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new enumerates MIDI ports
async fn prepare_runtime_builds_for_valid_config() {
    // #2100 / ADR-044 Phase 1: PREPARE succeeds for a valid config (compiles
    // the mapping engine, normalizes endpoints, parses the loopback OSC
    // listener) — producing artifacts the infallible APPLY phase installs.
    let initial = create_test_config();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);
    let manager = EngineManager::new(
        initial,
        PathBuf::from("/tmp/test-2100-prep-ok.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("EngineManager constructs");

    let good: Config = toml::from_str(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "osc_in"
direction = "Input"
type = "OscEndpoint"
host = "127.0.0.1"
port = 0
"#,
    )
    .expect("config parses");

    assert!(
        manager.prepare_runtime(&good).await.is_ok(),
        "#2100: prepare_runtime must build for a valid config (loopback OSC listener)"
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new enumerates MIDI ports
async fn save_config_rejects_unbuildable_config_atomically() {
    // #2100 Phase 2 (ADR-044) — ATOMICITY: a SaveConfig whose config can't
    // build (malformed listener ACL) is rejected BEFORE the commit, so
    // `live_config` is left untouched (no committed-but-stale window). PREPARE
    // runs before the mutate; its failure returns an error without publishing.
    let initial = create_test_config();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);
    let mut manager = EngineManager::new(
        initial,
        PathBuf::from("/tmp/test-2100-atomic.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("EngineManager constructs");

    let base_generation = manager.live_config.load().state_generation;

    let bad: Config = toml::from_str(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "osc_bad"
direction = "Input"
type = "OscEndpoint"
host = "127.0.0.1"
port = 0
network_acl = ["not-a-valid-cidr"]
"#,
    )
    .expect("config parses (the ACL is invalid, caught in PREPARE)");

    let request = crate::daemon::types::IpcRequest {
        id: "save-atomic".into(),
        command: IpcCommand::SaveConfig,
        args: json!({
            "base_generation": base_generation,
            "config": serde_json::to_value(&bad).unwrap(),
        }),
    };
    let response = manager.handle_ipc_request(request, None).await;

    assert!(
        matches!(response.status, ResponseStatus::Error),
        "#2100: SaveConfig of an unbuildable config must be rejected"
    );
    assert_eq!(
        manager.live_config.load().state_generation,
        base_generation,
        "#2100 ATOMICITY: a PREPARE failure must NOT commit — live_config generation unchanged"
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new enumerates MIDI ports
async fn rejected_save_config_does_not_mutate_shell_security() {
    // #2100 Phase 2 (ADR-044) R2 (Council) — `set_shell_security` is a side
    // effect, so it must run only in the infallible APPLY (post-commit), never
    // pre-commit. A SaveConfig whose config flips `allow_unsandboxed` to false
    // but can't build (malformed ACL) is rejected in PREPARE; the executor's
    // policy must be left untouched (no half-applied security change).
    let initial = create_test_config();
    assert!(
        initial.security.shell.allow_unsandboxed,
        "precondition: default config allows unsandboxed shell"
    );
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);
    let mut manager = EngineManager::new(
        initial,
        PathBuf::from("/tmp/test-2100-shellsec.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("EngineManager constructs");

    assert!(
        manager
            .action_executor
            .lock()
            .await
            .shell_allow_unsandboxed(),
        "precondition: executor starts with the default (unsandboxed allowed)"
    );

    let base_generation = manager.live_config.load().state_generation;

    // Flips shell policy to false AND is unbuildable (invalid CIDR in PREPARE).
    let bad: Config = toml::from_str(
        r#"
[security.shell]
allow_unsandboxed = false

[[modes]]
name = "Default"

[[endpoints]]
alias = "osc_bad"
direction = "Input"
type = "OscEndpoint"
host = "127.0.0.1"
port = 0
network_acl = ["not-a-valid-cidr"]
"#,
    )
    .expect("config parses (the ACL is invalid, caught in PREPARE)");
    assert!(
        !bad.security.shell.allow_unsandboxed,
        "the rejected config does request a policy change"
    );

    let request = crate::daemon::types::IpcRequest {
        id: "save-shellsec".into(),
        command: IpcCommand::SaveConfig,
        args: json!({
            "base_generation": base_generation,
            "config": serde_json::to_value(&bad).unwrap(),
        }),
    };
    let response = manager.handle_ipc_request(request, None).await;

    assert!(
        matches!(response.status, ResponseStatus::Error),
        "#2100: SaveConfig of an unbuildable config must be rejected"
    );
    assert!(
        manager
            .action_executor
            .lock()
            .await
            .shell_allow_unsandboxed(),
        "#2100 R2: a rejected (pre-commit-failed) SaveConfig must NOT mutate \
         the shell-sandbox policy — set_shell_security belongs in APPLY"
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new enumerates MIDI ports
async fn apply_committed_guarded_reprepares_on_revision_mismatch() {
    // #2100 Phase 2 (ADR-044) — the revision-equivalence guard. If the config
    // committed between PREPARE and APPLY differs from what was prepared (a
    // future content-transforming ConfigOp, or — defensively — a competing
    // commit), the prepared bundle is DISCARDED and re-prepared from the
    // committed snapshot, so the runtime tracks what was actually committed.
    use conductor_core::config::types::{ConnectorDirection, EndpointConfig, EndpointKind};
    let mk_endpoint = |alias: &str| EndpointConfig {
        alias: alias.to_string(),
        direction: ConnectorDirection::Input,
        protocol: None,
        description: None,
        enabled: true,
        channels: vec![],
        kind: EndpointKind::Matcher {
            matchers: vec![conductor_core::identity::DeviceMatcher::name_contains(
                "Mikro",
            )],
            input_matchers: vec![],
            output_matchers: vec![],
            no_probe: false,
        },
    };

    let initial = create_test_config();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);
    let mut manager = EngineManager::new(
        initial,
        PathBuf::from("/tmp/test-2100-guard.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("EngineManager constructs");
    manager
        .transition_state(LifecycleState::Starting)
        .await
        .unwrap();
    manager
        .transition_state(LifecycleState::Running)
        .await
        .unwrap();

    // PREPARE config A ("alias_a") — but never commit it.
    let mut config_a = create_test_config();
    config_a.endpoints.push(mk_endpoint("alias_a"));
    let prepared_a = manager.prepare_runtime(&config_a).await.expect("prepare A");

    // Commit a DIFFERENT config B ("alias_b") directly through the seam.
    let mut config_b = create_test_config();
    config_b.endpoints.push(mk_endpoint("alias_b"));
    {
        let live = std::sync::Arc::clone(&manager.live_config);
        let snap = live.load();
        live.mutate(
            manager.default_cli_provenance(),
            snap.state_generation,
            crate::daemon::live_config::ConfigOp::ReplaceWhole {
                config: Box::new(config_b),
            },
        )
        .await
        .expect("commit B");
    }

    // APPLY the stale prepared-A: the guard must detect the revision mismatch
    // and re-prepare from the committed B.
    // Infallible (ADR-044 / Council #2168 R3): returns an ApplyReport, not a Result.
    manager
        .apply_committed_guarded(prepared_a, "test-guard")
        .await;

    let registry = manager.connector_registry.read().await;
    assert!(
        registry.contains("alias_b"),
        "#2100 guard: runtime must reflect the COMMITTED config B (re-prepared)"
    );
    assert!(
        !registry.contains("alias_a"),
        "#2100 guard: the discarded prepared-A must NOT be installed"
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new enumerates MIDI ports
async fn failed_reload_config_restores_state_not_stuck_reloading() {
    // #2100 Phase 2 (Council #2168 R1) — a FAILED reload must restore the
    // pre-reload lifecycle state, not leave the daemon stuck in `Reloading`.
    // Otherwise the top-of-`reload_config` "skip while Reloading" guard would
    // silently no-op every subsequent reload (run_loop only logs the error and
    // does not reset state) — bricking config reload until restart.
    use std::io::Write;
    use tempfile::NamedTempFile;

    let mut tf = NamedTempFile::new().unwrap();
    write!(tf, "{}", toml::to_string(&create_test_config()).unwrap()).unwrap();
    let path = tf.path().to_path_buf();

    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);
    let mut manager = EngineManager::new(
        create_test_config(),
        path.clone(),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("EngineManager constructs");
    manager
        .transition_state(LifecycleState::Starting)
        .await
        .unwrap();
    manager
        .transition_state(LifecycleState::Running)
        .await
        .unwrap();

    // Write a config that fails reload: a non-loopback (0.0.0.0) network
    // listener WITHOUT `allow_network` is a config-validation error (the B-early
    // A.2 lift only permits non-loopback when `allow_network` + a `network_acl`
    // are set; without them it is still rejected at config-load).
    std::fs::write(
        &path,
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "osc_in"
direction = "Input"
type = "OscEndpoint"
host = "0.0.0.0"
port = 9000
"#,
    )
    .unwrap();

    let result = manager.reload_config().await;
    assert!(result.is_err(), "reload of an invalid config should fail");

    let state = *manager.state.read().await;
    assert_ne!(
        state,
        LifecycleState::Reloading,
        "#2100 R1: a failed reload must NOT leave the daemon stuck in Reloading"
    );
    assert_eq!(
        state,
        LifecycleState::Running,
        "#2100 R1: a failed reload restores the pre-reload Running state"
    );
}

#[tokio::test]
async fn rebuild_connector_registry_replaces_contents_in_place() {
    // ADR-031 P1B slice 2 — `reload_config()` rebuilds the registry
    // from the new config in place. Test the helper directly: start
    // with a registry containing alias "old", call the helper with
    // a config that has only "new", verify the swap.
    //
    // Side effect: any per-connector runtime state (bound_port,
    // metrics) is reset on reload — same trade-off the existing
    // device_output_map already accepts. Carry-over of bound ports
    // across reloads is a future refinement, not Phase 1B scope.
    use conductor_core::config::types::{
        ConnectorDirection, ConnectorProtocol, EndpointConfig, EndpointKind,
    };

    let initial_endpoint = EndpointConfig {
        alias: "old".to_string(),
        direction: ConnectorDirection::Output,
        protocol: Some(ConnectorProtocol::Midi),
        description: None,
        enabled: true,
        channels: vec![],
        kind: EndpointKind::Matcher {
            input_matchers: Vec::new(),
            output_matchers: Vec::new(),
            matchers: vec![],
            no_probe: false,
        },
    };
    let registry = Arc::new(RwLock::new(
        crate::connector_registry::ConnectorRegistry::from_config(&[initial_endpoint]),
    ));
    assert!(registry.read().await.contains("old"));

    let new_endpoint = EndpointConfig {
        alias: "new".to_string(),
        direction: ConnectorDirection::Output,
        protocol: Some(ConnectorProtocol::Midi),
        description: None,
        enabled: true,
        channels: vec![],
        kind: EndpointKind::Matcher {
            input_matchers: Vec::new(),
            output_matchers: Vec::new(),
            matchers: vec![],
            no_probe: false,
        },
    };
    rebuild_connector_registry(&registry, &[new_endpoint]).await;

    let r = registry.read().await;
    assert!(r.contains("new"), "new connector must be present");
    assert!(
        !r.contains("old"),
        "old connector must be gone after rebuild"
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
async fn test_engine_manager_populates_connector_registry_from_config() {
    // ADR-031 P1B slice 1 — the daemon must own a ConnectorRegistry
    // built from config, exposed via SharedDaemonStateRefs so MCP /
    // IPC code paths can read the live signal-routing graph.
    //
    // Smallest end-to-end check: build a config with one binding +
    // one explicit connector, construct an EngineManager, pull the
    // shared state refs, lock the registry, verify both aliases
    // are present.
    use conductor_core::config::types::{
        ConnectorDirection, ConnectorProtocol, EndpointConfig, EndpointKind,
    };

    let mut config = create_test_config();
    config.endpoints.push(EndpointConfig {
        alias: "pads".to_string(),
        direction: ConnectorDirection::Input,
        protocol: None,
        description: None,
        enabled: true,
        channels: vec![],
        kind: EndpointKind::Matcher {
            matchers: vec![conductor_core::identity::DeviceMatcher::name_contains(
                "Mikro",
            )],
            input_matchers: vec![],
            output_matchers: vec![],
            no_probe: false,
        },
    });
    config.endpoints.push(EndpointConfig {
        alias: "absynth".to_string(),
        direction: ConnectorDirection::Output,
        protocol: Some(ConnectorProtocol::Midi),
        description: None,
        enabled: true,
        channels: vec![],
        kind: EndpointKind::Matcher {
            input_matchers: Vec::new(),
            output_matchers: Vec::new(),
            matchers: vec![],
            no_probe: false,
        },
    });

    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);
    let manager = EngineManager::new(
        config,
        PathBuf::from("/tmp/test.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("EngineManager constructs");

    let refs = manager.get_shared_state_refs();
    let registry = refs.connector_registry.read().await;
    assert!(
        registry.contains("pads"),
        "binding alias must be present in registry"
    );
    assert!(
        registry.contains("absynth"),
        "connector alias must be present in registry"
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new enumerates MIDI ports
async fn reload_from_cached_config_rebuilds_connector_registry() {
    // #1365 — `reload_from_cached_config` (the profile-switch /
    // batch-apply reload path) rebuilt the route engine but NOT the
    // connector registry. Connectors added via `conductor_batch_changes`
    // never reached the runtime registry that `conductor_get_resolved_routing_graph`
    // reads, so the tool returned `{"connectors": []}` despite the
    // config file having connectors. Reproduced in 3 UI-test sessions.
    //
    // This test: start with a connector-free config, reload (cached)
    // with a config that has a binding + an explicit connector, and
    // assert both aliases are now in the registry.
    use conductor_core::config::types::{
        ConnectorDirection, ConnectorProtocol, EndpointConfig, EndpointKind,
    };

    let initial = create_test_config(); // no connectors, no bindings
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);
    let mut manager = EngineManager::new(
        initial,
        PathBuf::from("/tmp/test-1365.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("EngineManager constructs");

    // Sanity: registry starts empty.
    {
        let registry = manager.connector_registry.read().await;
        assert!(
            !registry.contains("pads") && !registry.contains("absynth"),
            "registry should start empty for a connector-free config"
        );
    }

    // reload_from_cached_config only runs from Running.
    manager
        .transition_state(LifecycleState::Starting)
        .await
        .unwrap();
    manager
        .transition_state(LifecycleState::Running)
        .await
        .unwrap();

    // New config: one binding ("pads") + one explicit connector ("absynth").
    let mut new_config = create_test_config();
    new_config.endpoints.push(EndpointConfig {
        alias: "pads".to_string(),
        direction: ConnectorDirection::Input,
        protocol: None,
        description: None,
        enabled: true,
        channels: vec![],
        kind: EndpointKind::Matcher {
            matchers: vec![conductor_core::identity::DeviceMatcher::name_contains(
                "Mikro",
            )],
            input_matchers: vec![],
            output_matchers: vec![],
            no_probe: false,
        },
    });
    new_config.endpoints.push(EndpointConfig {
        alias: "absynth".to_string(),
        direction: ConnectorDirection::Output,
        protocol: Some(ConnectorProtocol::Midi),
        description: None,
        enabled: true,
        channels: vec![],
        kind: EndpointKind::Matcher {
            input_matchers: Vec::new(),
            output_matchers: Vec::new(),
            matchers: vec![conductor_core::identity::DeviceMatcher::name_contains(
                "Absynth",
            )],
            no_probe: false,
        },
    });

    manager
        .reload_from_cached_config(new_config)
        .await
        .expect("cached reload should succeed");

    let registry = manager.connector_registry.read().await;
    assert!(
        registry.contains("pads"),
        "#1365: binding lowered to connector must be in the registry after cached reload"
    );
    assert!(
        registry.contains("absynth"),
        "#1365: explicit connector must be in the registry after cached reload"
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new enumerates MIDI ports
async fn reload_config_rebuilds_connector_registry() {
    // #1602 Gap A — symmetric twin of `reload_from_cached_config_rebuilds_connector_registry`.
    // `reload_config` (the non-cached / file-watcher / IPC reload
    // path) was silently skipping the registry rebuild — the #1367
    // fix comment falsely claimed it already did, when in fact only
    // the cached path did. Result: after any non-cached reload,
    // `list_connectors` returned the old view until daemon restart.
    // This test pins the symmetric fix so the gap doesn't recur.
    use std::io::Write;
    use tempfile::NamedTempFile;

    // Initial config on disk + matching path on the manager.
    let mut temp_file = NamedTempFile::new().unwrap();
    write!(
        temp_file,
        "{}",
        toml::to_string(&create_test_config()).unwrap()
    )
    .unwrap();
    let config_path = temp_file.path().to_path_buf();

    let initial = create_test_config(); // no connectors, no bindings
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);
    let mut manager = EngineManager::new(initial, config_path.clone(), cmd_rx, cmd_tx, shutdown_tx)
        .expect("EngineManager constructs");

    // Sanity: registry starts empty.
    {
        let registry = manager.connector_registry.read().await;
        assert!(
            !registry.contains("pads_rc") && !registry.contains("absynth_rc"),
            "registry should start empty for a connector-free config"
        );
    }

    manager
        .transition_state(LifecycleState::Starting)
        .await
        .unwrap();
    manager
        .transition_state(LifecycleState::Running)
        .await
        .unwrap();

    // Write a NEW config to disk: one binding + one explicit
    // (Output-direction, per #1602 §3.4) connector.
    use conductor_core::config::types::{ConnectorDirection, EndpointConfig, EndpointKind};
    // ADR-035 Phase 2: Config::load now rejects legacy [[bindings]]/
    // [[connectors]], so the round-trip-through-disk fixture must be
    // endpoints-only. The input Matcher endpoint is the binding-derived
    // connector; the output Matcher endpoint is the explicit one.
    let mut new_config = create_test_config();
    new_config.endpoints.push(EndpointConfig {
        alias: "pads_rc".to_string(),
        direction: ConnectorDirection::Input,
        protocol: None,
        description: None,
        enabled: true,
        channels: vec![],
        kind: EndpointKind::Matcher {
            matchers: vec![conductor_core::identity::DeviceMatcher::name_contains(
                "Mikro",
            )],
            input_matchers: vec![],
            output_matchers: vec![],
            no_probe: false,
        },
    });
    new_config.endpoints.push(EndpointConfig {
        alias: "absynth_rc".to_string(),
        direction: ConnectorDirection::Output,
        protocol: None,
        description: None,
        enabled: true,
        channels: vec![],
        kind: EndpointKind::Matcher {
            matchers: vec![conductor_core::identity::DeviceMatcher::name_contains(
                "Absynth",
            )],
            input_matchers: vec![],
            output_matchers: vec![],
            no_probe: false,
        },
    });
    std::fs::write(&config_path, toml::to_string(&new_config).unwrap()).expect("write new config");

    manager
        .reload_config()
        .await
        .expect("non-cached reload should succeed");

    let registry = manager.connector_registry.read().await;
    assert!(
        registry.contains("pads_rc"),
        "#1602 Gap A: binding-derived connector must be in the registry after non-cached reload"
    );
    assert!(
        registry.contains("absynth_rc"),
        "#1602 Gap A: explicit connector must be in the registry after non-cached reload"
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new enumerates MIDI ports
async fn known_good_backup_comes_from_validated_in_memory_config() {
    // #2173: the post-validation `*.toml.known_good` backup must be written from
    // the VALIDATED IN-MEMORY config, not `std::fs::copy`-ied from the on-disk
    // file (which a concurrent edit could mutate between load+validate and the
    // copy — TOCTOU). Deterministic witness: the on-disk config carries a
    // distinctive comment; serializing the in-memory `Config` drops comments,
    // so the backup must NOT contain it. (The old fs::copy preserved the raw
    // bytes including the comment → this test is red on the pre-fix code.)
    use std::io::Write as _;
    use tempfile::NamedTempFile;

    const MARKER: &str = "# DISTINCTIVE_TOCTOU_MARKER_2173";

    let mut temp_file = NamedTempFile::new().unwrap();
    write!(
        temp_file,
        "{}\n{}",
        MARKER,
        toml::to_string(&create_test_config()).unwrap()
    )
    .unwrap();
    let config_path = temp_file.path().to_path_buf();
    let known_good_path = config_path.with_extension("toml.known_good");

    let initial = create_test_config();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);
    let mut manager = EngineManager::new(initial, config_path, cmd_rx, cmd_tx, shutdown_tx)
        .expect("EngineManager constructs");
    manager
        .transition_state(LifecycleState::Starting)
        .await
        .unwrap();
    manager
        .transition_state(LifecycleState::Running)
        .await
        .unwrap();

    manager
        .reload_config()
        .await
        .expect("reload should succeed");

    let backup = std::fs::read_to_string(&known_good_path)
        .expect("#2173: known_good backup must be written on a successful reload");
    assert!(
        !backup.contains("DISTINCTIVE_TOCTOU_MARKER_2173"),
        "#2173: known_good must be the serialized in-memory config (no source comments), \
         not a raw file copy — got:\n{backup}"
    );
    // And it must be a valid, parseable config (the validated content).
    conductor_core::Config::load(known_good_path.to_str().unwrap())
        .expect("#2173: the known_good backup must itself be a loadable config");
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new enumerates MIDI ports
async fn save_config_ipc_rebuilds_connector_registry() {
    // #2051 (ADR-043 D2) — the `SaveConfig` IPC handler committed the new
    // config via `live_config.mutate(ReplaceWhole)` but returned success
    // WITHOUT rebuilding the daemon's runtime connector registry /
    // device_output_map / route engine. A GUI-created endpoint therefore
    // never reached the running routing graph (empty graph, idle status,
    // LLM-blind) until a restart. The fix shares `reload_config`'s
    // post-commit rebuild via `apply_committed_config`, called on the
    // SaveConfig success path. This test pins it: save a config that adds
    // a new Input endpoint and assert the alias is in the registry without
    // any reload/restart.
    use conductor_core::config::types::{ConnectorDirection, EndpointConfig, EndpointKind, Mode};

    let initial = create_test_config(); // no extra endpoints
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);
    let mut manager = EngineManager::new(
        initial,
        PathBuf::from("/tmp/test-2051.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("EngineManager constructs");

    let initial_mode_count = manager.mapping_engine.read().await.mode_count();
    let base_generation = manager.live_config.load().state_generation;

    // New config = baseline + one new Input endpoint alias "new_pads".
    let mut new_config = create_test_config();
    new_config.modes.push(Mode {
        name: "SaveExtraMode".to_string(),
        color: None,
        mappings: vec![],
    });
    new_config.endpoints.push(EndpointConfig {
        alias: "new_pads".to_string(),
        direction: ConnectorDirection::Input,
        protocol: None,
        description: None,
        enabled: true,
        channels: vec![],
        kind: EndpointKind::Matcher {
            matchers: vec![conductor_core::identity::DeviceMatcher::name_contains(
                "Mikro",
            )],
            input_matchers: vec![],
            output_matchers: vec![],
            no_probe: false,
        },
    });

    let request = crate::daemon::types::IpcRequest {
        id: "save-2051".into(),
        command: IpcCommand::SaveConfig,
        args: json!({
            "base_generation": base_generation,
            "config": serde_json::to_value(&new_config).unwrap(),
        }),
    };
    let response = manager.handle_ipc_request(request, None).await;
    assert!(
        matches!(response.status, ResponseStatus::Success),
        "SaveConfig should succeed; error: {:?}",
        response.error
    );
    assert!(
        manager.mapping_engine.read().await.mode_count() > initial_mode_count,
        "SaveConfig must rebuild mapping_engine from the committed config"
    );

    let refs = manager.get_shared_state_refs();
    let registry = refs.connector_registry.read().await;
    assert!(
        registry.contains("new_pads"),
        "SaveConfig must rebuild the connector registry without a restart (#2051)"
    );
}

#[cfg(feature = "llm-executor")]
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new enumerates MIDI ports
async fn sync_config_after_apply_rebuilds_connector_registry() {
    // #1598 Phase 2 Step B regression — third twin of
    // `reload_config_rebuilds_connector_registry` (#1602 Gap A)
    // and `reload_from_cached_config_rebuilds_connector_registry`
    // (#1365 via #1367). The LLM plan-apply path
    // (`sync_config_after_apply`, called from `IpcCommand::ApplyPlan`
    // at engine_manager.rs:2239) updates live_config + route_engine
    // + mapping_engine but was NOT rebuilding `connector_registry`.
    //
    // Pre-Step-B this latency was invisible because the GUI canvas
    // re-derived its view from `configStore` after apply. Step B
    // makes the canvas read the daemon's resolved view — so a stale
    // registry surfaces as a stale routing graph until the user
    // restarts the GUI (or the file-watcher's debounced reload
    // eventually catches up, then no fetch is triggered because
    // configStore had already updated).
    //
    // This test pins the symmetric fix so the gap doesn't recur
    // on the apply path the way it did on the reload paths.
    use std::io::Write;
    use tempfile::NamedTempFile;

    let mut temp_file = NamedTempFile::new().unwrap();
    write!(
        temp_file,
        "{}",
        toml::to_string(&create_test_config()).unwrap()
    )
    .unwrap();
    let config_path = temp_file.path().to_path_buf();

    let initial = create_test_config(); // connector-free
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);
    let mut manager = EngineManager::new(initial, config_path.clone(), cmd_rx, cmd_tx, shutdown_tx)
        .expect("EngineManager constructs");

    // Sanity: registry starts empty.
    {
        let registry = manager.connector_registry.read().await;
        assert!(
            !registry.contains("pads_apply") && !registry.contains("absynth_apply"),
            "registry should start empty for a connector-free config"
        );
    }

    manager
        .transition_state(LifecycleState::Starting)
        .await
        .unwrap();
    manager
        .transition_state(LifecycleState::Running)
        .await
        .unwrap();

    use conductor_core::config::types::{
        ConnectorDirection, ConnectorProtocol, EndpointConfig, EndpointKind,
    };
    let mut new_config = create_test_config();
    new_config.endpoints.push(EndpointConfig {
        alias: "pads_apply".to_string(),
        direction: ConnectorDirection::Input,
        protocol: None,
        description: None,
        enabled: true,
        channels: vec![],
        kind: EndpointKind::Matcher {
            matchers: vec![conductor_core::identity::DeviceMatcher::name_contains(
                "Mikro",
            )],
            input_matchers: vec![],
            output_matchers: vec![],
            no_probe: false,
        },
    });
    new_config.endpoints.push(EndpointConfig {
        alias: "absynth_apply".to_string(),
        direction: ConnectorDirection::Output,
        protocol: Some(ConnectorProtocol::Midi),
        description: None,
        enabled: true,
        channels: vec![],
        kind: EndpointKind::Matcher {
            input_matchers: Vec::new(),
            output_matchers: Vec::new(),
            matchers: vec![conductor_core::identity::DeviceMatcher::name_contains(
                "Absynth",
            )],
            no_probe: false,
        },
    });

    // The plan-apply path (vs reload_config) hands the new config
    // directly to sync_config_after_apply — it doesn't go through
    // disk. Mimic that here.
    manager
        .sync_config_after_apply(new_config)
        .await
        .expect("sync_config_after_apply should succeed");

    let registry = manager.connector_registry.read().await;
    assert!(
        registry.contains("pads_apply"),
        "#1598 Phase 2 Step B: binding-derived connector must be in the registry after plan apply"
    );
    assert!(
        registry.contains("absynth_apply"),
        "#1598 Phase 2 Step B: explicit connector must be in the registry after plan apply"
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new enumerates MIDI ports
async fn reload_from_cached_config_reapplies_probe_toggle() {
    // #2071 (ADR-043 D2) — the cached profile-switch path
    // (`reload_from_cached_config`) committed the config and rebuilt the
    // connector registry but did NOT re-apply the master SysEx probe
    // toggle (nor the network listeners, port rescan, device_output_map,
    // or device status). A profile that flips `sysex_identity_probing =
    // false` therefore left probing ENABLED until a full file reload, so a
    // cache-HIT profile switch produced different runtime state than a
    // cache-MISS. Routing every committed mutation through the canonical
    // `apply_committed_config` (via `reconcile_runtime_to_live`) closes the
    // divergence; the probe toggle is the smallest provable witness.
    let initial = create_test_config(); // default: sysex_identity_probing = true
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);
    let mut manager = EngineManager::new(
        initial,
        PathBuf::from("/tmp/test-2071-cached-probe.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("EngineManager constructs");
    assert!(
        manager.probe_coordinator.is_enabled(),
        "probing starts enabled (config default)"
    );

    manager
        .transition_state(LifecycleState::Starting)
        .await
        .unwrap();
    manager
        .transition_state(LifecycleState::Running)
        .await
        .unwrap();

    let mut new_config = create_test_config();
    new_config.advanced_settings.sysex_identity_probing = false;
    manager
        .reload_from_cached_config(new_config)
        .await
        .expect("cached reload should succeed");

    assert!(
        !manager.probe_coordinator.is_enabled(),
        "#2071: cached profile switch must re-apply the SysEx probe toggle \
         (full runtime rebuild via apply_committed_config)"
    );
}

#[cfg(feature = "llm-executor")]
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new enumerates MIDI ports
async fn sync_config_after_apply_reapplies_probe_toggle() {
    // #2071 — the LLM plan-apply path (`sync_config_after_apply`) skipped
    // even MORE of the rebuild than the cached path: no probe toggle,
    // listeners, rate limiter, capture flags, port rescan,
    // device_output_map, or device status. The same unification fix routes
    // it through `apply_committed_config`; the probe toggle witnesses it.
    // `sync_config_after_apply` saves to disk, so config_path must be a
    // save-allowed location (mirror the registry twin's NamedTempFile setup).
    use std::io::Write;
    use tempfile::NamedTempFile;
    let mut temp_file = NamedTempFile::new().unwrap();
    write!(
        temp_file,
        "{}",
        toml::to_string(&create_test_config()).unwrap()
    )
    .unwrap();
    let config_path = temp_file.path().to_path_buf();

    let initial = create_test_config();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);
    let mut manager = EngineManager::new(initial, config_path, cmd_rx, cmd_tx, shutdown_tx)
        .expect("EngineManager constructs");
    assert!(manager.probe_coordinator.is_enabled());

    manager
        .transition_state(LifecycleState::Starting)
        .await
        .unwrap();
    manager
        .transition_state(LifecycleState::Running)
        .await
        .unwrap();

    let mut new_config = create_test_config();
    new_config.advanced_settings.sysex_identity_probing = false;
    manager
        .sync_config_after_apply(new_config)
        .await
        .expect("sync_config_after_apply should succeed");

    assert!(
        !manager.probe_coordinator.is_enabled(),
        "#2071: plan-apply must re-apply the SysEx probe toggle \
         (full runtime rebuild via apply_committed_config)"
    );
}

#[cfg(feature = "llm-executor")]
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new enumerates MIDI ports
async fn sync_config_after_apply_does_not_write_profile_on_prepare_failure() {
    // #2316 — split-brain regression. `sync_config_after_apply` (the LLM
    // plan-apply path) previously saved the new config to the profile file on
    // disk BEFORE calling the fallible `prepare_runtime`. A config that fails
    // PREPARE (e.g. a malformed listener ACL) therefore left the profile file
    // holding the new config while the daemon's live config + runtime kept the
    // old one — a split-brain that surfaces as the wrong config on the next
    // boot/reload. The fix moves PREPARE ahead of the write (mirrors
    // `handle_save_config`; ADR-044 Phase 2). This pins it: a failed apply must
    // leave the on-disk profile byte-for-byte unchanged AND not commit.
    use std::io::Write;
    use tempfile::NamedTempFile;

    let mut temp_file = NamedTempFile::new().unwrap();
    write!(
        temp_file,
        "{}",
        toml::to_string(&create_test_config()).unwrap()
    )
    .unwrap();
    let config_path = temp_file.path().to_path_buf();
    let original_on_disk = std::fs::read_to_string(&config_path).unwrap();

    let initial = create_test_config();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);
    let mut manager = EngineManager::new(initial, config_path.clone(), cmd_rx, cmd_tx, shutdown_tx)
        .expect("EngineManager constructs");

    manager
        .transition_state(LifecycleState::Starting)
        .await
        .unwrap();
    manager
        .transition_state(LifecycleState::Running)
        .await
        .unwrap();

    let base_generation = manager.live_config.load().state_generation;

    let bad: Config = toml::from_str(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "osc_bad"
direction = "Input"
type = "OscEndpoint"
host = "127.0.0.1"
port = 0
network_acl = ["not-a-valid-cidr"]
"#,
    )
    .expect("config parses (the ACL is invalid, caught in PREPARE)");

    let result = manager.sync_config_after_apply(bad).await;

    assert!(
        result.is_err(),
        "#2316: sync_config_after_apply must reject a config that fails PREPARE"
    );
    assert_eq!(
        std::fs::read_to_string(&config_path).unwrap(),
        original_on_disk,
        "#2316 SPLIT-BRAIN: a PREPARE failure must NOT write the profile to disk \
         (PREPARE runs before the save)"
    );
    assert_eq!(
        manager.live_config.load().state_generation,
        base_generation,
        "#2316 ATOMICITY: a PREPARE failure must NOT commit — live_config generation unchanged"
    );
}

#[cfg(feature = "llm-executor")]
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new enumerates MIDI ports
async fn sync_config_after_apply_does_not_write_the_user_file() {
    // ADR-043 Option C (#2554): plan-apply commits to `live.toml` (the sole
    // durable authority) and rebuilds the runtime, but must NOT write back to the
    // user/profile file. (Previously #2320 covered best-effort handling of a §D11
    // profile write-through *failure*; that write-through is now removed — the
    // authority is `live.toml`, which boot resumes and the GUI reads via
    // GetConfigBody.) The config flips the probe toggle so APPLY is observable.
    use tempfile::NamedTempFile;

    let tmp = NamedTempFile::new().unwrap();
    let config_path = tmp.path().to_path_buf();
    // #2554 (cloud-review): seed the user file with a sentinel so "unchanged" is a
    // strong assertion — a write-back would replace it (not relying on "empty").
    let user_file_sentinel = "# untouched-by-plan-apply-option-c\n";
    std::fs::write(&config_path, user_file_sentinel).unwrap();

    let initial = create_test_config();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);
    let mut manager = EngineManager::new(initial, config_path.clone(), cmd_rx, cmd_tx, shutdown_tx)
        .expect("EngineManager constructs");
    assert!(
        manager.probe_coordinator.is_enabled(),
        "probing starts enabled"
    );

    manager
        .transition_state(LifecycleState::Starting)
        .await
        .unwrap();
    manager
        .transition_state(LifecycleState::Running)
        .await
        .unwrap();

    let mut new_config = create_test_config();
    new_config.advanced_settings.sysex_identity_probing = false;
    let result = manager.sync_config_after_apply(new_config).await;

    assert!(result.is_ok(), "plan-apply must succeed");
    assert!(
        !manager.probe_coordinator.is_enabled(),
        "COMMIT + APPLY must have run (probe toggle flipped)"
    );
    // The user/profile file on disk is UNTOUCHED — no write-back (Option C).
    assert_eq!(
        std::fs::read_to_string(&config_path).unwrap(),
        user_file_sentinel,
        "plan-apply must NOT write the user/profile file (Option C removes the §D11 write-through)"
    );
    // #2554 (Council): no stale self-write-suppress arm left behind on the success
    // path — the mutate writes only live.toml (unwatched post-#2551).
    assert!(
        manager.config_write_suppress.lock().await.is_none(),
        "plan-apply must not arm config_write_suppress (the live.toml write is unwatched)"
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new enumerates MIDI ports
async fn ipc_dispatch_backstop_rebuilds_after_out_of_band_commit() {
    // #2071 / ADR-043 Q2 — the rebuild guarantee must be STRUCTURAL: a
    // committed `LiveConfig` mutation must not be able to leave the runtime
    // registry/bindings stale even if the committing handler forgets to
    // reconcile. `handle_ipc_request` reconciles after EVERY command
    // (content-guarded), so the next IPC command — even a read-only one —
    // repairs an out-of-band commit. This simulates a future handler that
    // commits without reconciling and proves the dispatch backstop catches
    // it (the Q2 "structural, not caller-remembered" requirement).
    use conductor_core::config::types::{ConnectorDirection, EndpointConfig, EndpointKind};

    let initial = create_test_config();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);
    let mut manager = EngineManager::new(
        initial,
        PathBuf::from("/tmp/test-2071-backstop.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("EngineManager constructs");

    manager
        .transition_state(LifecycleState::Starting)
        .await
        .unwrap();
    manager
        .transition_state(LifecycleState::Running)
        .await
        .unwrap();

    // Commit a config carrying a new endpoint DIRECTLY through the mutate
    // seam, bypassing every reconcile call site (the "forgotten handler").
    let mut new_config = create_test_config();
    new_config.endpoints.push(EndpointConfig {
        alias: "backstop_pads".to_string(),
        direction: ConnectorDirection::Input,
        protocol: None,
        description: None,
        enabled: true,
        channels: vec![],
        kind: EndpointKind::Matcher {
            matchers: vec![conductor_core::identity::DeviceMatcher::name_contains(
                "Mikro",
            )],
            input_matchers: vec![],
            output_matchers: vec![],
            no_probe: false,
        },
    });
    {
        let live = std::sync::Arc::clone(&manager.live_config);
        let snap = live.load();
        live.mutate(
            manager.default_cli_provenance(),
            snap.state_generation,
            crate::daemon::live_config::ConfigOp::ReplaceWhole {
                config: Box::new(new_config),
            },
        )
        .await
        .expect("direct mutate commits");
    }

    // The runtime registry is now STALE — no reconcile ran for that commit.
    assert!(
        !manager
            .connector_registry
            .read()
            .await
            .contains("backstop_pads"),
        "precondition: an out-of-band commit leaves the registry stale"
    );

    // Any subsequent IPC command's dispatch backstop must reconcile it.
    let request = crate::daemon::types::IpcRequest {
        id: "backstop-status".into(),
        command: IpcCommand::Status,
        args: json!({}),
    };
    let response = manager.handle_ipc_request(request, None).await;
    assert!(
        matches!(response.status, ResponseStatus::Success),
        "Status should succeed; error: {:?}",
        response.error
    );

    assert!(
        manager
            .connector_registry
            .read()
            .await
            .contains("backstop_pads"),
        "#2071/Q2: the IPC dispatch backstop must rebuild the runtime after an \
         out-of-band commit — structural guarantee, not caller-remembered"
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new enumerates MIDI ports
async fn reload_config_extends_device_output_map_with_connectors() {
    // #1611 — output-side twin of #1602.
    //
    // Symptom (issue #1611): `MidiForward { target = "absynth_output" }`
    // emitted "MIDI output port 'absynth_output' not found" even though
    // a matching output connector existed in config, because
    // `resolve_output_port` only consulted `device_output_map`, which
    // was being populated solely from `[[devices]]` bindings.
    //
    // Fix: `device_output_map` now carries (output/bidirectional-alias →
    // physical-port) entries from the unified `build_output_map` over the
    // normalized endpoint set, at every store site (reload_config /
    // reload_from_cached_config / hot-plug rescan) — ADR-035 Slice 9.5.
    //
    // Smallest provable shape uses `EndpointKind::MidiVirtualPort`
    // — alias maps directly to `port_name` without needing a real
    // host port to be present.
    use std::io::Write;
    use tempfile::NamedTempFile;

    let mut temp_file = NamedTempFile::new().unwrap();
    write!(
        temp_file,
        "{}",
        toml::to_string(&create_test_config()).unwrap()
    )
    .unwrap();
    let config_path = temp_file.path().to_path_buf();

    let initial = create_test_config();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);
    let mut manager = EngineManager::new(initial, config_path.clone(), cmd_rx, cmd_tx, shutdown_tx)
        .expect("EngineManager constructs");

    // Sanity: nothing connector-shaped in the map yet.
    {
        let map = manager.device_output_map.load();
        assert!(
            !map.contains_key("absynth_1611"),
            "device_output_map should not contain the connector alias before reload"
        );
    }

    manager
        .transition_state(LifecycleState::Starting)
        .await
        .unwrap();
    manager
        .transition_state(LifecycleState::Running)
        .await
        .unwrap();

    use conductor_core::config::types::{ConnectorDirection, EndpointConfig, EndpointKind};
    // ADR-035 Phase 2: Config::load rejects legacy [[connectors]]; author
    // the output endpoint as a unified [[endpoints]] MidiVirtualPort.
    let mut new_config = create_test_config();
    new_config.endpoints.push(EndpointConfig {
        alias: "absynth_1611".to_string(),
        direction: ConnectorDirection::Output,
        protocol: None,
        description: None,
        enabled: true,
        channels: vec![],
        kind: EndpointKind::MidiVirtualPort {
            port_name: "absynth_virtual_1611".to_string(),
        },
    });
    std::fs::write(&config_path, toml::to_string(&new_config).unwrap()).expect("write new config");

    manager
        .reload_config()
        .await
        .expect("non-cached reload should succeed");

    let map = manager.device_output_map.load();
    assert_eq!(
        map.get("absynth_1611").map(String::as_str),
        Some("absynth_virtual_1611"),
        "#1611: output connector alias must resolve via device_output_map \
             after reload so ActionExecutor::resolve_output_port can match it"
    );
}

#[tokio::test]
#[ignore] // Requires CoreMIDI (macOS) / ALSA seq (Linux) — creates real OS virtual ports
#[cfg(not(target_os = "windows"))]
async fn reload_creates_and_tears_down_virtual_midi_port() {
    // #2063: a MidiVirtualPort endpoint must materialize a real OS MIDI port on
    // reload (so routes resolve + external apps see it), and removing the
    // endpoint must tear the OS port down. Mirrors the #1611 reload setup.
    use conductor_core::config::types::{ConnectorDirection, EndpointConfig, EndpointKind};
    use std::io::Write;
    use tempfile::NamedTempFile;

    // Skip silently if the host can't create virtual ports (headless runner).
    if !conductor_core::midi_output::MidiOutputManager::virtual_ports_available() {
        return;
    }

    let mut temp_file = NamedTempFile::new().unwrap();
    write!(
        temp_file,
        "{}",
        toml::to_string(&create_test_config()).unwrap()
    )
    .unwrap();
    let config_path = temp_file.path().to_path_buf();

    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);
    let mut manager = EngineManager::new(
        create_test_config(),
        config_path.clone(),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("EngineManager constructs");
    manager
        .transition_state(LifecycleState::Starting)
        .await
        .unwrap();
    manager
        .transition_state(LifecycleState::Running)
        .await
        .unwrap();

    // Reload with a MidiVirtualPort endpoint → the OS port is created.
    let mut with_port = create_test_config();
    with_port.endpoints.push(EndpointConfig {
        alias: "daw_proxy_2063".to_string(),
        direction: ConnectorDirection::Output,
        protocol: None,
        description: None,
        enabled: true,
        channels: vec![],
        kind: EndpointKind::MidiVirtualPort {
            port_name: "Conductor 2063 Test".to_string(),
        },
    });
    std::fs::write(&config_path, toml::to_string(&with_port).unwrap()).unwrap();
    manager.reload_config().await.expect("reload with port");

    assert!(
        manager
            .action_executor
            .lock()
            .await
            .virtual_port_names()
            .contains(&"Conductor 2063 Test".to_string()),
        "#2063: the MidiVirtualPort endpoint must create an enumerable OS port"
    );

    // Reload without it → the OS port is torn down.
    std::fs::write(
        &config_path,
        toml::to_string(&create_test_config()).unwrap(),
    )
    .unwrap();
    manager.reload_config().await.expect("reload without port");

    assert!(
        !manager
            .action_executor
            .lock()
            .await
            .virtual_port_names()
            .contains(&"Conductor 2063 Test".to_string()),
        "#2063: removing the endpoint must tear the OS port down"
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
async fn test_engine_manager_populates_route_engine_from_config() {
    // ADR-031 P2B slice 5 — the daemon must own a RouteEngine
    // built from `config.routes`, exposed at the stage-9 hook
    // (slice 6 wires the actual call site). Smallest end-to-end
    // check: build a config with one route, construct an
    // EngineManager, look up via the route_engine field's
    // `route_destinations()` for the source alias.
    use conductor_core::config::types::{
        ConnectorDirection, ConnectorProtocol, EndpointConfig, EndpointKind, RouteConfig,
    };

    let mut config = create_test_config();
    // Add two endpoints so the route's from/to validate (the
    // RouteEngine itself doesn't validate — that's config-load —
    // but using realistic aliases keeps the test readable).
    config.endpoints.push(EndpointConfig {
        alias: "pads".to_string(),
        direction: ConnectorDirection::Bidirectional,
        protocol: Some(ConnectorProtocol::Midi),
        description: None,
        enabled: true,
        channels: vec![],
        kind: EndpointKind::Matcher {
            input_matchers: Vec::new(),
            output_matchers: Vec::new(),
            matchers: vec![],
            no_probe: false,
        },
    });
    config.endpoints.push(EndpointConfig {
        alias: "absynth".to_string(),
        direction: ConnectorDirection::Output,
        protocol: Some(ConnectorProtocol::Midi),
        description: None,
        enabled: true,
        channels: vec![],
        kind: EndpointKind::Matcher {
            input_matchers: Vec::new(),
            output_matchers: Vec::new(),
            matchers: vec![],
            no_probe: false,
        },
    });
    config.routes.push(RouteConfig {
        from: "pads".to_string(),
        to: "absynth".to_string(),
        transform: None,
        filter: None,
        enabled: true,
        description: None,
        modes: Vec::new(),
    });

    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);
    let manager = EngineManager::new(
        config,
        PathBuf::from("/tmp/test.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("EngineManager constructs");

    // Pull the route_engine via the ArcSwap and verify the
    // route is indexed by source alias.
    let engine = manager.route_engine.load();
    let outputs = engine.route_destinations_midi("pads", &[0x90, 60, 64], "Default");
    assert_eq!(outputs.len(), 1, "single route should fire");
    assert_eq!(outputs[0].to_alias, "absynth");
    assert_eq!(outputs[0].bytes, vec![0x90, 60, 64]);
    assert_eq!(
        outputs[0].kind,
        crate::route_engine::RouteOutputKind::Midi,
        "no transform → MIDI passthrough"
    );

    // Unknown source returns empty — sanity check.
    assert!(
        engine
            .route_destinations_midi("ghost", &[0x90, 60, 64], "Default")
            .is_empty()
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
async fn run_probe_device_identity_returns_no_paired_output_when_port_unknown() {
    use conductor_core::device_intelligence::probe::ProbeStartError;

    // Fresh EngineManager: no InputManager attached, empty
    // device_output_map. Probing any port name should short-circuit
    // to `Err(ProbeStartError::NoPairedOutput)` (Phase 3.B.1) without
    // touching the coordinator's actual send path — confirms the
    // resolve_probe_output_port early return is wired up correctly.
    let config = create_test_config();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);

    let manager = EngineManager::new(
        config,
        PathBuf::from("/tmp/test.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .unwrap();

    let outcome = manager.run_probe_device_identity("does-not-exist").await;
    match outcome {
        Err(ProbeStartError::NoPairedOutput { port_name }) => {
            assert_eq!(port_name, "does-not-exist");
        }
        other => panic!("expected Err(NoPairedOutput), got {:?}", other),
    }
}

/// ADR-045 D5 invariant 3 (#2493): with NO audit sink available, network
/// listeners refuse to start (fail-closed) — their audit trail is a
/// security control (ADR-042), not telemetry. The rest of the daemon keeps
/// running (fail-open is exercised implicitly by every other test).
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new enumerates MIDI ports
async fn network_listeners_refuse_to_start_without_audit_sink() {
    let config: Config = toml::from_str(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "osc_in"
direction = "Input"
type = "OscEndpoint"
host = "127.0.0.1"
port = 0
"#,
    )
    .expect("config parses");

    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);
    let mut mgr = EngineManager::new(
        config.clone(),
        PathBuf::from("/tmp/test_failclosed_listeners.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("engine manager builds");

    mgr.clear_audit_sink_for_test();
    mgr.start_network_listeners(&config)
        .await
        .expect("start_network_listeners itself succeeds (infallible half)");
    let status = mgr.network_listener_status();
    assert!(
        status.is_empty(),
        "fail-closed: no listener may bind without an audit sink, got {status:?}"
    );
}
