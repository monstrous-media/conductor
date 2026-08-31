// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Tests for the `mcp` module family (moved verbatim in #2601).

use conductor_core::config::Config;

use super::super::mcp_types::McpRequestId;
use super::*;
use conductor_core::config::{ActionConfig, Mapping, Mode, Trigger};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

// Counter for unique test socket paths
static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);

fn get_test_socket_path() -> PathBuf {
    let count = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!(
        "conductor-mcp-test-{}-{}.sock",
        std::process::id(),
        count
    ))
}

/// D4.A.3.3.B.1 test helper: wrap a `Config` into `Arc<LiveConfig>`.
/// Mirrors the executor.rs test helper of the same name.
fn live_config_arc(config: Config) -> Arc<crate::daemon::live_config::LiveConfig> {
    Arc::new(
        crate::daemon::live_config::LiveConfig::new(config)
            .expect("LiveConfig::new failed in test"),
    )
}

fn create_test_config() -> Config {
    Config {
        mcp: Default::default(),
        per_app_modes: None,
        config_meta: Default::default(),
        security: Default::default(),
        endpoints: vec![],
        modes: vec![Mode {
            name: "Default".to_string(),
            color: Some("blue".to_string()),
            mappings: vec![Mapping {
                trigger: Trigger::Note {
                    note: 36,
                    velocity_min: Some(1),
                    channel: None,
                    device: None,
                },
                action: ActionConfig::Keystroke {
                    keys: "c".to_string(),
                    modifiers: vec!["cmd".to_string()],
                },
                description: Some("Copy".to_string()),
                let_through: false,
            }],
        }],
        global_mappings: vec![],
        logging: None,
        advanced_settings: Default::default(),
        last_selected_mode: None,
        default_mode: None,
        led: None,
        event_console: None,
        routes: vec![],
    }
}

#[test]
fn test_mcp_socket_path() {
    let path = get_mcp_socket_path().unwrap();
    assert!(path.to_string_lossy().contains("conductor"));
    assert!(path.to_string_lossy().contains("mcp"));
}

#[test]
fn test_handle_initialize() {
    let request = McpRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(McpRequestId::Number(1)),
        method: "initialize".to_string(),
        params: Some(json!({})),
    };

    let mut initialized = false;
    let response = handle_initialize(&request, &mut initialized);

    assert!(initialized);
    assert!(response.error.is_none());
    assert!(response.result.is_some());

    let result = response.result.unwrap();
    assert!(result.get("protocolVersion").is_some());
    assert!(result.get("capabilities").is_some());
    assert!(result.get("serverInfo").is_some());
}

#[test]
fn test_handle_tools_list() {
    let request = McpRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(McpRequestId::Number(2)),
        method: "tools/list".to_string(),
        params: None,
    };

    let response = handle_tools_list(&request);

    assert!(response.error.is_none());
    assert!(response.result.is_some());

    let result = response.result.unwrap();
    let tools = result.get("tools").unwrap().as_array().unwrap();
    // ADR-035 Phase 2 #1748: −5 legacy tools removed (conductor_create_endpoint is the unified replacement); +1 ADR-042 #1899 B.7 conductor_security_status; −1 #2052 conductor_list_connectors removed; +3 ADR-040 4c mode tools (conductor_set_mode/conductor_unlock_mode/conductor_mode_status).
    // ADR-045 D2 (#2492): full catalog is 54; compositions without
    // `mcp-write` advertise only the 27 ReadOnly inspection tools.
    #[cfg(feature = "mcp-write")]
    assert_eq!(tools.len(), 54);
    #[cfg(not(feature = "mcp-write"))]
    assert_eq!(tools.len(), 27);
}

#[tokio::test]
async fn test_handle_tools_call() {
    use crate::daemon::types::DaemonState;
    let config = live_config_arc(create_test_config());
    let tool_executor = McpToolExecutor::new();
    let daemon_state: Option<DaemonState> = None;
    let shared_state: Option<Arc<SharedDaemonStateRefs>> = None;

    let request = McpRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(McpRequestId::Number(3)),
        method: "tools/call".to_string(),
        params: Some(json!({
            "name": "conductor_get_config",
            "arguments": {}
        })),
    };

    let response = handle_tools_call(
        &request,
        &config,
        &daemon_state,
        &tool_executor,
        &shared_state,
        None, // #1311: tests don't simulate registry; unregistered ceiling
    )
    .await;

    assert!(response.error.is_none());
    assert!(response.result.is_some());

    let result = response.result.unwrap();
    let content = result.get("content").unwrap().as_array().unwrap();
    assert!(!content.is_empty());
}

/// ADR-045 D2 (#2492): in a composition without `mcp-write`, a call to a
/// write-tier tool (absent from the compiled catalog) returns the
/// standard "not available in this build" error naming Conductor Studio
/// — not a tier-ceiling denial, not a bare "Unknown tool".
#[cfg(not(feature = "mcp-write"))]
#[tokio::test]
async fn tools_call_absent_tool_returns_studio_error() {
    use crate::daemon::types::DaemonState;
    let config = live_config_arc(create_test_config());
    let tool_executor = McpToolExecutor::new();
    let daemon_state: Option<DaemonState> = None;
    let shared_state: Option<Arc<SharedDaemonStateRefs>> = None;

    for gated in ["conductor_create_mapping", "conductor_send_midi"] {
        let request = McpRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(McpRequestId::Number(99)),
            method: "tools/call".to_string(),
            params: Some(json!({ "name": gated, "arguments": {} })),
        };
        let response = handle_tools_call(
            &request,
            &config,
            &daemon_state,
            &tool_executor,
            &shared_state,
            None,
        )
        .await;

        // Tool-level errors travel as successful JSON-RPC responses
        // with `isError` content (MCP convention).
        assert!(response.error.is_none(), "{gated}: JSON-RPC error");
        let result = response.result.unwrap();
        let text = result.to_string();
        assert!(
            text.contains("not available in this build"),
            "{gated}: missing standard error text: {text}"
        );
        assert!(
            text.contains("Conductor Studio"),
            "{gated}: error must name Conductor Studio: {text}"
        );
    }
}

/// ADR-045 / Council R1 #2 — BUNDLE profile (`llm-executor` without
/// `mcp-write`): the write tools' risk tiers resolve correctly (the IPC
/// path compiled them), so this pins the rejection ORDERING — the
/// is_compiled_tool check must fire BEFORE tier-based dispatch could
/// route a ConfigChange call to the write path. The socket answers with
/// the Studio error, proving it is read-only in the Studio bundle.
#[cfg(all(feature = "llm-executor", not(feature = "mcp-write")))]
#[tokio::test]
async fn bundle_profile_rejects_write_tools_despite_known_tier() {
    use crate::daemon::mcp_tools::get_tool_risk_tier;
    use crate::daemon::mcp_types::ToolRiskTier;
    use crate::daemon::types::DaemonState;

    // Precondition that makes the ordering meaningful: the tier is
    // KNOWN (not the Privileged unknown-tool fallback).
    assert_eq!(
        get_tool_risk_tier("conductor_create_mapping"),
        ToolRiskTier::ConfigChange
    );

    let config = live_config_arc(create_test_config());
    let tool_executor = McpToolExecutor::new();
    let daemon_state: Option<DaemonState> = None;
    let shared_state: Option<Arc<SharedDaemonStateRefs>> = None;
    let request = McpRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(McpRequestId::Number(101)),
        method: "tools/call".to_string(),
        params: Some(json!({ "name": "conductor_create_mapping", "arguments": {} })),
    };
    let response = handle_tools_call(
        &request,
        &config,
        &daemon_state,
        &tool_executor,
        &shared_state,
        None,
    )
    .await;

    assert!(response.error.is_none());
    let text = response.result.unwrap().to_string();
    assert!(text.contains("not available in this build"), "{text}");
    assert!(text.contains("Conductor Studio"), "{text}");
}

/// ADR-007 Phase 2: Test that handle_tools_call uses real daemon state
#[tokio::test]
async fn test_handle_tools_call_with_daemon_state() {
    use crate::daemon::types::{DaemonState, DaemonStatistics, DeviceStatus, LifecycleState};

    let config = live_config_arc(create_test_config());
    let tool_executor = McpToolExecutor::new();
    let shared_state: Option<Arc<SharedDaemonStateRefs>> = None;

    // Create a daemon state with real data
    let daemon_state = Some(DaemonState {
        lifecycle_state: Some(LifecycleState::Running),
        device_status: Some(DeviceStatus {
            connected: true,
            name: Some("Test Device".to_string()),
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
    });

    let request = McpRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(McpRequestId::Number(4)),
        method: "tools/call".to_string(),
        params: Some(json!({
            "name": "conductor_get_status",
            "arguments": {}
        })),
    };

    let response = handle_tools_call(
        &request,
        &config,
        &daemon_state,
        &tool_executor,
        &shared_state,
        None, // #1311: tests don't simulate registry; unregistered ceiling
    )
    .await;

    assert!(response.error.is_none());
    assert!(response.result.is_some());

    let result = response.result.unwrap();
    let content = result.get("content").unwrap().as_array().unwrap();
    assert!(!content.is_empty());

    // Verify the response contains real state data
    let text = content[0].get("text").unwrap().as_str().unwrap();
    assert!(text.contains("Running"), "Should contain lifecycle state");
}

// -----------------------------------------------------------------
// ADR-026 Phase 2 — SysEx identity tools via the MCP server path
// -----------------------------------------------------------------

#[cfg(feature = "mcp-write")]
#[tokio::test]
async fn test_handle_tools_call_probe_no_shared_state() {
    // Without SharedDaemonStateRefs the probe tool returns a clear
    // "Shared state not available" error rather than the generic
    // "must be executed via the LLM executor path" sentinel.
    let config = live_config_arc(create_test_config());
    let tool_executor = McpToolExecutor::new();
    let daemon_state: Option<crate::daemon::types::DaemonState> = None;
    let shared_state: Option<Arc<SharedDaemonStateRefs>> = None;

    let request = McpRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(McpRequestId::Number(100)),
        method: "tools/call".to_string(),
        params: Some(json!({
            "name": "conductor_probe_device_identity",
            "arguments": { "port_name": "fake-port" }
        })),
    };

    let response = handle_tools_call(
        &request,
        &config,
        &daemon_state,
        &tool_executor,
        &shared_state,
        // #1311: conductor_probe_device_identity is HardwareIO tier,
        // so unregistered (None) is denied. Use registered
        // HardwareIO ceiling here to test the original dispatch
        // contract (no shared_state → returns isError + readable
        // message about Manager not being available).
        Some(crate::daemon::audit::AuditRiskTier::HardwareIO),
    )
    .await;

    assert!(response.error.is_none());
    let result = response.result.unwrap();
    // is_error: true
    assert_eq!(result.get("isError").and_then(|v| v.as_bool()), Some(true));
    let text = result.get("content").unwrap().as_array().unwrap()[0]
        .get("text")
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        text.contains("Shared state"),
        "should mention shared state availability; got: {}",
        text
    );
}

#[tokio::test]
async fn test_handle_tools_call_get_device_identity_no_shared_state() {
    // Smoke test: the tool dispatches through the SysEx-cache
    // branch and surfaces a clean "Shared state not available"
    // error when no `SharedDaemonStateRefs` is attached. The
    // implementation checks `shared_state` *before* arguments, so
    // even with empty arguments we never reach the port_name
    // validation here — that path is covered by
    // `test_handle_tools_call_get_device_identity_missing_port_with_shared_state`.
    let config = live_config_arc(create_test_config());
    let tool_executor = McpToolExecutor::new();
    let daemon_state: Option<crate::daemon::types::DaemonState> = None;
    let shared_state: Option<Arc<SharedDaemonStateRefs>> = None;

    let request = McpRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(McpRequestId::Number(101)),
        method: "tools/call".to_string(),
        params: Some(json!({
            "name": "conductor_get_device_identity",
            "arguments": {}
        })),
    };

    let response = handle_tools_call(
        &request,
        &config,
        &daemon_state,
        &tool_executor,
        &shared_state,
        None, // #1311: tests don't simulate registry; unregistered ceiling
    )
    .await;

    assert!(response.error.is_none());
    let result = response.result.unwrap();
    assert_eq!(result.get("isError").and_then(|v| v.as_bool()), Some(true));
    let text = result.get("content").unwrap().as_array().unwrap()[0]
        .get("text")
        .unwrap()
        .as_str()
        .unwrap();
    assert!(
        text.contains("Shared state"),
        "expected 'Shared state' error, got: {}",
        text
    );
}

#[tokio::test]
async fn test_handle_tools_call_get_device_identity_missing_port_with_shared_state() {
    // With `SharedDaemonStateRefs` attached the handler proceeds
    // past the no-state-refs guard and reaches actual argument
    // parsing. Omitting `port_name` must surface a "Missing
    // required argument: port_name" error — pinning the
    // documented contract that the tool requires that field.
    // This is the test that the previous "missing_port" name
    // claimed to be but wasn't actually exercising; PR #910
    // 9th-pass review feedback.
    let config = live_config_arc(create_test_config());
    let tool_executor = McpToolExecutor::new();
    let daemon_state: Option<crate::daemon::types::DaemonState> = None;
    let (refs, _coord) = build_test_shared_refs();
    let shared_state = Some(refs);

    let request = McpRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(McpRequestId::Number(401)),
        method: "tools/call".to_string(),
        params: Some(json!({
            "name": "conductor_get_device_identity",
            "arguments": {}
        })),
    };

    let response = handle_tools_call(
        &request,
        &config,
        &daemon_state,
        &tool_executor,
        &shared_state,
        None, // #1311: tests don't simulate registry; unregistered ceiling
    )
    .await;

    assert!(response.error.is_none());
    let result = response.result.unwrap();
    assert_eq!(result.get("isError").and_then(|v| v.as_bool()), Some(true));
    let text = result.get("content").unwrap().as_array().unwrap()[0]
        .get("text")
        .unwrap()
        .as_str()
        .unwrap();
    assert!(
        text.contains("port_name"),
        "expected missing-argument error mentioning port_name, got: {}",
        text
    );
}

/// Build an empty `SharedDaemonStateRefs` suitable for verifying the
/// JSON shape returned by ReadOnly cache tools. The
/// `probe_coordinator` is owned by the test caller (returned
/// alongside the refs) so the test can populate the cache before
/// invoking `handle_tools_call`.
fn build_test_shared_refs() -> (
    Arc<SharedDaemonStateRefs>,
    Arc<conductor_core::device_intelligence::probe::ProbeCoordinator>,
) {
    use crate::daemon::types::{DaemonStatistics, DeviceStatus, LifecycleState};
    use arc_swap::ArcSwap;
    use conductor_core::control_state::PhysicalControlStateStore;
    use conductor_core::device_intelligence::fingerprint::EventStats;
    use conductor_core::device_intelligence::probe::ProbeCoordinator;
    use dashmap::DashMap;
    use std::time::Instant;
    use tokio::sync::{Mutex, RwLock, mpsc};

    let probe_coordinator = Arc::new(ProbeCoordinator::new());
    let (cmd_tx, _cmd_rx) = mpsc::channel(8);
    // _cmd_rx is intentionally dropped — these tests don't dispatch
    // probe commands; they only read the cache.
    let refs = SharedDaemonStateRefs {
        lifecycle_state: Arc::new(RwLock::new(LifecycleState::Running)),
        device_status: Arc::new(RwLock::new(DeviceStatus::default())),
        statistics: Arc::new(RwLock::new(DaemonStatistics::default())),
        input_manager: Arc::new(Mutex::new(None)),
        config_path: PathBuf::from("/tmp/test.toml"),
        start_time: Instant::now(),
        command_tx: cmd_tx,
        active_profile: Arc::new(ArcSwap::from_pointee(None)),
        event_stats: Arc::new(DashMap::<String, EventStats>::new()),
        control_state: Arc::new(PhysicalControlStateStore::default()),
        probe_coordinator: Arc::clone(&probe_coordinator),
        connector_registry: Arc::new(RwLock::new(
            crate::connector_registry::ConnectorRegistry::from_config(&[]),
        )),
        device_output_map: Arc::new(ArcSwap::from_pointee(HashMap::new())),
        route_engine: Arc::new(ArcSwap::from_pointee(
            crate::route_engine::RouteEngine::compile(&[]),
        )),
        dispatch_trace: Arc::new(crate::daemon::dispatch_trace::DispatchTraceRing::default()),
    };
    (Arc::new(refs), probe_coordinator)
}

#[tokio::test]
async fn test_handle_tools_call_get_device_identity_returns_wrapper_with_null() {
    // Tool spec: response shape is `{ port_name, identity }` even
    // when the cache miss — `identity` is null, NOT a top-level
    // null. This pins the documented contract so a future
    // refactor can't silently flatten the wrapper.
    let config = live_config_arc(create_test_config());
    let tool_executor = McpToolExecutor::new();
    let daemon_state: Option<crate::daemon::types::DaemonState> = None;
    let (refs, _coord) = build_test_shared_refs();
    let shared_state = Some(refs);

    let request = McpRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(McpRequestId::Number(201)),
        method: "tools/call".to_string(),
        params: Some(json!({
            "name": "conductor_get_device_identity",
            "arguments": { "port_name": "Mikro IN" }
        })),
    };

    let response = handle_tools_call(
        &request,
        &config,
        &daemon_state,
        &tool_executor,
        &shared_state,
        None, // #1311: tests don't simulate registry; unregistered ceiling
    )
    .await;

    assert!(response.error.is_none());
    let result = response.result.unwrap();
    assert_ne!(
        result.get("isError").and_then(|v| v.as_bool()),
        Some(true),
        "expected success, got: {:?}",
        result
    );
    // ToolCallResult::json wraps payload as MCP content[0].text JSON string.
    let text = result.get("content").unwrap().as_array().unwrap()[0]
        .get("text")
        .unwrap()
        .as_str()
        .unwrap();
    let payload: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(
        payload.get("port_name").and_then(|v| v.as_str()),
        Some("Mikro IN")
    );
    assert!(payload.get("identity").unwrap().is_null());
    // Phase 3.A: response shape carries `confidence` paired with
    // `identity`. On cache miss both must be JSON null so callers
    // can null-check either field interchangeably.
    assert!(
        payload.get("confidence").is_some(),
        "Phase 3.A response shape must include `confidence` field"
    );
    assert!(payload.get("confidence").unwrap().is_null());
}

/// Phase 3.A positive case: after a successful probe, the cache
/// holds `(identity, IdentityConfidence::DirectPairedPort)` and the
/// MCP response surfaces both. This pins the wire format
/// (`confidence: "direct_paired_port"`, snake_case per
/// `#[serde(rename_all = "snake_case")]`) so a future refactor
/// can't silently change the serialised representation that
/// downstream LLM/GUI callers parse.
#[tokio::test]
async fn test_handle_tools_call_get_device_identity_surfaces_confidence_after_probe() {
    use conductor_core::device_intelligence::sysex_identity::IDENTITY_REQUEST;

    let config = live_config_arc(create_test_config());
    let tool_executor = McpToolExecutor::new();
    let daemon_state: Option<crate::daemon::types::DaemonState> = None;
    let (refs, coord) = build_test_shared_refs();

    // Drive a real probe to populate the cache. The send-fn
    // signals readiness *after* the pending-probe slot has been
    // registered (probe() registers before calling send_fn), so
    // the reply thread can deliver immediately without polling.
    let coord_bg = Arc::clone(&coord);
    let (registered_tx, registered_rx) = std::sync::mpsc::sync_channel::<()>(0);
    let reply_handle = std::thread::spawn(move || {
        let _ = registered_rx.recv();
        // Synthetic Korg reply: F0 7E 00 06 02 42 ... F7 (15 bytes)
        let reply: [u8; 15] = [
            0xF0, 0x7E, 0x00, 0x06, 0x02, 0x42, 0x34, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
            0xF7,
        ];
        coord_bg.observe_reply("Probed Port", &reply);
    });
    let outcome = coord.probe("Probed Port", |bytes| {
        assert_eq!(bytes, &IDENTITY_REQUEST);
        registered_tx.send(()).unwrap();
        Ok(())
    });
    reply_handle.join().unwrap();
    assert!(matches!(
        outcome,
        Ok(conductor_core::device_intelligence::probe::ProbeResult::Identified { .. })
    ));

    let shared_state = Some(refs);
    let request = McpRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(McpRequestId::Number(203)),
        method: "tools/call".to_string(),
        params: Some(json!({
            "name": "conductor_get_device_identity",
            "arguments": { "port_name": "Probed Port" }
        })),
    };
    let response = handle_tools_call(
        &request,
        &config,
        &daemon_state,
        &tool_executor,
        &shared_state,
        None, // #1311: tests don't simulate registry; unregistered ceiling
    )
    .await;

    let result = response.result.unwrap();
    assert_ne!(
        result.get("isError").and_then(|v| v.as_bool()),
        Some(true),
        "expected success, got: {:?}",
        result
    );
    let text = result.get("content").unwrap().as_array().unwrap()[0]
        .get("text")
        .unwrap()
        .as_str()
        .unwrap();
    let payload: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(
        payload.get("port_name").and_then(|v| v.as_str()),
        Some("Probed Port")
    );
    assert!(
        payload.get("identity").unwrap().is_object(),
        "identity must be a populated object after a successful probe"
    );
    assert_eq!(
        payload.get("confidence").and_then(|v| v.as_str()),
        Some("direct_paired_port"),
        "Phase 3.A: every freshly-stored identity has DirectPairedPort confidence"
    );
}

#[tokio::test]
async fn test_handle_tools_call_list_device_identities_returns_wrapper_with_array() {
    // Tool spec: response shape is `{ identities: [...] }` — the
    // empty case must still be the wrapper, not a bare array.
    let config = live_config_arc(create_test_config());
    let tool_executor = McpToolExecutor::new();
    let daemon_state: Option<crate::daemon::types::DaemonState> = None;
    let (refs, _coord) = build_test_shared_refs();
    let shared_state = Some(refs);

    let request = McpRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(McpRequestId::Number(202)),
        method: "tools/call".to_string(),
        params: Some(json!({
            "name": "conductor_list_device_identities",
            "arguments": {}
        })),
    };

    let response = handle_tools_call(
        &request,
        &config,
        &daemon_state,
        &tool_executor,
        &shared_state,
        None, // #1311: tests don't simulate registry; unregistered ceiling
    )
    .await;

    assert!(response.error.is_none());
    let result = response.result.unwrap();
    assert_ne!(
        result.get("isError").and_then(|v| v.as_bool()),
        Some(true),
        "expected success, got: {:?}",
        result
    );
    let text = result.get("content").unwrap().as_array().unwrap()[0]
        .get("text")
        .unwrap()
        .as_str()
        .unwrap();
    let payload: serde_json::Value = serde_json::from_str(text).unwrap();
    let arr = payload
        .get("identities")
        .and_then(|v| v.as_array())
        .expect("payload must contain `identities` array");
    assert!(arr.is_empty(), "fresh coordinator → empty array");
}

/// Phase 3.A: each entry in the `identities` array carries
/// `{ port_name, identity, confidence }`. After a successful
/// probe, the confidence string is `"direct_paired_port"`.
#[tokio::test]
async fn test_handle_tools_call_list_device_identities_includes_confidence_per_entry() {
    let config = live_config_arc(create_test_config());
    let tool_executor = McpToolExecutor::new();
    let daemon_state: Option<crate::daemon::types::DaemonState> = None;
    let (refs, coord) = build_test_shared_refs();

    let coord_bg = Arc::clone(&coord);
    let (registered_tx, registered_rx) = std::sync::mpsc::sync_channel::<()>(0);
    let reply_handle = std::thread::spawn(move || {
        let _ = registered_rx.recv();
        let reply: [u8; 15] = [
            0xF0, 0x7E, 0x00, 0x06, 0x02, 0x42, 0x34, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
            0xF7,
        ];
        coord_bg.observe_reply("List Probed Port", &reply);
    });
    let _ = coord.probe("List Probed Port", |_| {
        registered_tx.send(()).unwrap();
        Ok(())
    });
    reply_handle.join().unwrap();

    let shared_state = Some(refs);
    let request = McpRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(McpRequestId::Number(204)),
        method: "tools/call".to_string(),
        params: Some(json!({
            "name": "conductor_list_device_identities",
            "arguments": {}
        })),
    };
    let response = handle_tools_call(
        &request,
        &config,
        &daemon_state,
        &tool_executor,
        &shared_state,
        None, // #1311: tests don't simulate registry; unregistered ceiling
    )
    .await;

    let result = response.result.unwrap();
    let text = result.get("content").unwrap().as_array().unwrap()[0]
        .get("text")
        .unwrap()
        .as_str()
        .unwrap();
    let payload: serde_json::Value = serde_json::from_str(text).unwrap();
    let arr = payload
        .get("identities")
        .and_then(|v| v.as_array())
        .expect("payload must contain `identities` array");
    assert_eq!(arr.len(), 1);
    let entry = &arr[0];
    assert_eq!(
        entry.get("port_name").and_then(|v| v.as_str()),
        Some("List Probed Port")
    );
    assert!(entry.get("identity").unwrap().is_object());
    assert_eq!(
        entry.get("confidence").and_then(|v| v.as_str()),
        Some("direct_paired_port"),
    );
}

#[tokio::test]
async fn test_handle_tools_call_list_device_identities_no_shared_state() {
    let config = live_config_arc(create_test_config());
    let tool_executor = McpToolExecutor::new();
    let daemon_state: Option<crate::daemon::types::DaemonState> = None;
    let shared_state: Option<Arc<SharedDaemonStateRefs>> = None;

    let request = McpRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(McpRequestId::Number(102)),
        method: "tools/call".to_string(),
        params: Some(json!({
            "name": "conductor_list_device_identities",
            "arguments": {}
        })),
    };

    let response = handle_tools_call(
        &request,
        &config,
        &daemon_state,
        &tool_executor,
        &shared_state,
        None, // #1311: tests don't simulate registry; unregistered ceiling
    )
    .await;

    assert!(response.error.is_none());
    let result = response.result.unwrap();
    assert_eq!(result.get("isError").and_then(|v| v.as_bool()), Some(true));
    let text = result.get("content").unwrap().as_array().unwrap()[0]
        .get("text")
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    assert!(text.contains("Shared state"), "got: {}", text);
}

#[tokio::test]
async fn test_mcp_server_starts() {
    let (shutdown_tx, shutdown_rx) = broadcast::channel(1);
    let config = live_config_arc(create_test_config());
    let socket_path = get_test_socket_path();

    let mut server = McpServer::new_with_path(socket_path.clone(), shutdown_rx, config);

    // Start server in background
    let server_handle = tokio::spawn(async move { server.run().await });

    // Give server time to start
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Send shutdown
    let _ = shutdown_tx.send(());

    // Wait for server to stop
    tokio::time::timeout(std::time::Duration::from_secs(2), server_handle)
        .await
        .expect("Server should shut down")
        .expect("Server should complete without panic")
        .expect("Server run should succeed");

    // Cleanup socket file
    let _ = std::fs::remove_file(&socket_path);
}

/// #1480 — connections beyond `MAX_CONCURRENT_CLIENTS` must be refused
/// and dropped immediately (permit acquired BEFORE spawn), not accepted
/// and parked on a handler task blocked in `acquire()`. Saturate the cap
/// with held-open clients, then assert the next connection is closed
/// promptly (read → EOF) rather than queued.
#[tokio::test]
async fn test_mcp_refuses_connections_over_cap() {
    let (shutdown_tx, shutdown_rx) = broadcast::channel(1);
    let config = live_config_arc(create_test_config());
    let socket_path = get_test_socket_path();
    let mut server = McpServer::new_with_path(socket_path.clone(), shutdown_rx, config);
    let server_handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    // Saturate the cap. Each client connects and sends nothing, so its
    // server-side handler blocks reading the first request while holding
    // its permit. Keep the client streams alive in `held`.
    let mut held = Vec::new();
    for _ in 0..MAX_CONCURRENT_CLIENTS {
        held.push(
            UnixStream::connect(&socket_path)
                .await
                .expect("connections under the cap should be accepted"),
        );
    }
    // Let the accept loop process all of them and take their permits.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // One more connection is over-cap. The OS completes connect(), but
    // the server drops the stream immediately, so a read returns EOF
    // (0 bytes) promptly. Pre-fix, the stream was parked on a handler
    // blocked in `sem.acquire()`, so this read would block until the
    // 2s timeout below fires.
    let mut over = UnixStream::connect(&socket_path)
        .await
        .expect("connect completes at the OS level even when over-cap");
    let mut buf = [0u8; 1];
    let n = tokio::time::timeout(std::time::Duration::from_secs(2), over.read(&mut buf))
        .await
        .expect("over-cap connection must be dropped promptly, not parked on a blocked task")
        .expect("reading the dropped connection should succeed with EOF");
    assert_eq!(
        n, 0,
        "over-cap connection should be closed (EOF), not queued"
    );

    drop(held);
    let _ = shutdown_tx.send(());
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), server_handle).await;
    let _ = std::fs::remove_file(&socket_path);
}

#[tokio::test]
async fn test_mcp_server_handles_initialize() {
    let (shutdown_tx, shutdown_rx) = broadcast::channel(1);
    let config = live_config_arc(create_test_config());
    let socket_path = get_test_socket_path();

    let mut server = McpServer::new_with_path(socket_path.clone(), shutdown_rx, config);

    // Start server in background
    let server_handle = tokio::spawn(async move { server.run().await });

    // Give server time to start
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Connect client
    let mut stream = UnixStream::connect(&socket_path).await.unwrap();

    // Send initialize request
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {}
    });
    stream
        .write_all(format!("{}\n", request).as_bytes())
        .await
        .unwrap();

    // Read response
    let mut response = vec![0u8; 4096];
    let n = stream.read(&mut response).await.unwrap();
    let response_str = String::from_utf8_lossy(&response[..n]);

    assert!(response_str.contains("protocolVersion"));
    assert!(response_str.contains("conductor"));

    // Cleanup
    let _ = shutdown_tx.send(());
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), server_handle).await;
    let _ = std::fs::remove_file(&socket_path);
}

#[tokio::test]
async fn test_mcp_server_returns_tools_list() {
    let (shutdown_tx, shutdown_rx) = broadcast::channel(1);
    let config = live_config_arc(create_test_config());
    let socket_path = get_test_socket_path();

    let mut server = McpServer::new_with_path(socket_path.clone(), shutdown_rx, config);

    // Start server in background
    let server_handle = tokio::spawn(async move { server.run().await });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Connect client
    let mut stream = UnixStream::connect(&socket_path).await.unwrap();

    // Initialize first
    let init_request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {}
    });
    stream
        .write_all(format!("{}\n", init_request).as_bytes())
        .await
        .unwrap();

    // Read init response
    let mut buf = vec![0u8; 4096];
    let _ = stream.read(&mut buf).await.unwrap();

    // Send tools/list request
    let list_request = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list"
    });
    stream
        .write_all(format!("{}\n", list_request).as_bytes())
        .await
        .unwrap();

    // Read response
    let mut response = vec![0u8; 8192];
    let n = stream.read(&mut response).await.unwrap();
    let response_str = String::from_utf8_lossy(&response[..n]);

    assert!(response_str.contains("conductor_get_status"));
    assert!(response_str.contains("conductor_list_devices"));
    assert!(response_str.contains("conductor_get_config"));
    assert!(response_str.contains("conductor_list_mappings"));
    assert!(response_str.contains("conductor_get_mapping"));

    // Cleanup
    let _ = shutdown_tx.send(());
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), server_handle).await;
    let _ = std::fs::remove_file(&socket_path);
}

// -------------------------------------------------------------
// ADR-031 P3 / #1274 slice 20 — ConfigChange dispatch tests
// -------------------------------------------------------------

#[cfg(feature = "mcp-write")]
/// Slice 20 AC #2: `conductor_batch_changes` over the daemon MCP
/// dispatch returns the auto-applied plan payload, NOT
/// "Unknown tool".
///
/// This was the gap the issue named explicitly — before slice 20,
/// every ConfigChange tool fell through to the generic executor's
/// "Unknown tool" arm even though `tools/list` advertised them.
#[tokio::test]
async fn test_handle_tools_call_dispatches_batch_changes() {
    use crate::daemon::types::DaemonState;
    let config = live_config_arc(create_test_config());
    let tool_executor = McpToolExecutor::new();
    let daemon_state: Option<DaemonState> = None;
    let shared_state: Option<Arc<SharedDaemonStateRefs>> = None;

    let request = McpRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(McpRequestId::Number(99)),
        method: "tools/call".to_string(),
        params: Some(json!({
            "name": "conductor_batch_changes",
            "arguments": {
                "operations": [{
                    // ADR-035 Phase 2 #1748 removed the connector batch ops;
                    // exercise the ConfigChange dispatch arm with a create_mode
                    // op (no alias cross-references to resolve at apply time).
                    "type": "create_mode",
                    "name": "TestModeSlice20"
                }]
            }
        })),
    };

    // #1311 update: pass an explicit ConfigChange ceiling so
    // this test still exercises the dispatch arm (it represents
    // a peer that was registered via `conductorctl mcp register
    // --tier ConfigChange`). The unregistered variant of this
    // test lives below as `test_handle_tools_call_unregistered_peer_denied_config_change`.
    let response = handle_tools_call(
        &request,
        &config,
        &daemon_state,
        &tool_executor,
        &shared_state,
        Some(crate::daemon::audit::AuditRiskTier::ConfigChange),
    )
    .await;

    assert!(
        response.error.is_none(),
        "no JSON-RPC error expected (got: {:?})",
        response.error
    );
    let result = response
        .result
        .as_ref()
        .expect("tools/call must return a result");

    // The dispatch returns a ToolCallResult JSON wrapping the
    // auto-applied plan payload. Pull the inner JSON text the
    // wrapper exposes — `content[0].text` per the MCP spec.
    let content = result
        .get("content")
        .and_then(|c| c.as_array())
        .expect("content array required");
    let inner_text = content
        .first()
        .and_then(|c| c.get("text"))
        .and_then(|t| t.as_str())
        .expect("first content entry must have text field");

    // CRITICAL: this is the "Unknown tool" gap the issue named.
    // If the dispatch arm regresses, this assertion fires.
    assert!(
        !inner_text.contains("Unknown tool"),
        "ConfigChange dispatch must NOT return \"Unknown tool\" \
             for conductor_batch_changes (got: {inner_text})"
    );

    // Auto-apply contract: the payload carries the apply
    // outcome, not a deferred plan id awaiting separate apply.
    // The change is in the LiveConfig snapshot at this point.
    let inner: serde_json::Value =
        serde_json::from_str(inner_text).expect("inner content text must be JSON");
    assert_eq!(
        inner["status"], "applied",
        "auto-apply contract requires status=\"applied\" in response \
             (got: {inner})"
    );
    assert_eq!(
        inner["changes_count"], 1,
        "exactly 1 change (create_mode) must have been applied \
             (got: {inner})"
    );

    // Verify the mutation actually landed in LiveConfig.
    let snap = config.load();
    assert!(
        snap.config
            .modes
            .iter()
            .any(|m| m.name == "TestModeSlice20"),
        "create_mode op must have mutated LiveConfig.modes \
             (got modes: {:?})",
        snap.config
            .modes
            .iter()
            .map(|m| m.name.clone())
            .collect::<Vec<_>>()
    );
}

#[cfg(feature = "mcp-write")]
/// #1311 regression test: an UNREGISTERED MCP peer (no entry in
/// `McpRegistry`, ceiling = `None`) MUST be denied
/// `ConfigChange` tools. Pre-fix, the dispatch synthesised
/// `CallerContext::internal_trusted()` and let any same-UID
/// process invoke `conductor_batch_changes`. Post-fix, the
/// tier-ceiling check at the top of `handle_tools_call` returns
/// a permission_denied JSON-RPC error before the dispatch arm
/// runs.
#[tokio::test]
async fn test_handle_tools_call_unregistered_peer_denied_config_change() {
    use crate::daemon::types::DaemonState;
    let config = live_config_arc(create_test_config());
    let tool_executor = McpToolExecutor::new();
    let daemon_state: Option<DaemonState> = None;
    let shared_state: Option<Arc<SharedDaemonStateRefs>> = None;

    let request = McpRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(McpRequestId::Number(1311)),
        method: "tools/call".to_string(),
        params: Some(json!({
            "name": "conductor_batch_changes",
            "arguments": {
                "operations": [{
                    // ADR-035 Phase 2 #1748: connector batch ops removed.
                    // The op is irrelevant here — the peer is denied BEFORE
                    // dispatch — but use a live op (create_mode) anyway.
                    "type": "create_mode",
                    "name": "should_not_land"
                }]
            }
        })),
    };

    // Snapshot mode count BEFORE the call so we can assert
    // the mutation was actually blocked (not just the response).
    let modes_before = config.load().config.modes.len();

    let response = handle_tools_call(
        &request,
        &config,
        &daemon_state,
        &tool_executor,
        &shared_state,
        None, // unregistered peer — the #1311 attack scenario
    )
    .await;

    // JSON-RPC error must be present with permission_denied
    // semantics. The error message must reference #1311 so it's
    // greppable in any future audit.
    let err = response
        .error
        .as_ref()
        .expect("unregistered ConfigChange call MUST return JSON-RPC error (#1311)");
    assert!(
        err.message.contains("Permission denied") || err.message.contains("#1311"),
        "expected permission-denied error referencing #1311; got: {}",
        err.message
    );

    // Mutation must NOT have landed in LiveConfig — the security
    // hole was that ConfigChange ran before the check.
    let modes_after = config.load().config.modes.len();
    assert_eq!(
        modes_after, modes_before,
        "unregistered peer's ConfigChange tool MUST be rejected BEFORE \
             touching LiveConfig (#1311 was: dispatched as internal_trusted)"
    );
}

#[cfg(feature = "mcp-write")]
/// Slice 20 AC #4 (schema-drift regression): every ConfigChange
/// tool in `get_tool_definitions()` must dispatch through the
/// ConfigChange arm, NOT fall through to "Unknown tool".
///
/// This is the cousin of `signal_routing_prompt_sync_test.rs` —
/// when a new ConfigChange tool is added to `tools/list` without
/// being wired into a dispatch path, this test fires.
///
/// Sends a tools/call with empty args for each ConfigChange tool
/// and asserts the response is NOT "Unknown tool". The tool may
/// legitimately fail with a validation error (missing required
/// args) — that's a successful dispatch, just bad input. The
/// signal we care about is dispatch wiring, not happy-path
/// success.
#[tokio::test]
async fn test_all_configchange_tools_are_dispatched() {
    use crate::daemon::mcp_tools::{get_tool_definitions, get_tool_risk_tier};
    use crate::daemon::mcp_types::ToolRiskTier;
    use crate::daemon::types::DaemonState;

    let configchange_tools: Vec<String> = get_tool_definitions()
        .into_iter()
        .filter(|t| get_tool_risk_tier(&t.name) == ToolRiskTier::ConfigChange)
        .map(|t| t.name)
        .collect();

    assert!(
        !configchange_tools.is_empty(),
        "regression canary: get_tool_definitions() must include at \
             least one ConfigChange tool. If this fires, the registry \
             changed and the test needs revisiting."
    );

    // ALL ConfigChange tools must dispatch — collect failures and
    // assert at the end so a single test run reports every tool
    // that regressed, not just the first.
    let mut undispatched: Vec<String> = Vec::new();

    for tool_name in &configchange_tools {
        let config = live_config_arc(create_test_config());
        let tool_executor = McpToolExecutor::new();
        let daemon_state: Option<DaemonState> = None;
        let shared_state: Option<Arc<SharedDaemonStateRefs>> = None;

        let request = McpRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(McpRequestId::Number(0)),
            method: "tools/call".to_string(),
            params: Some(json!({
                "name": tool_name,
                "arguments": {}
            })),
        };

        let response = handle_tools_call(
            &request,
            &config,
            &daemon_state,
            &tool_executor,
            &shared_state,
            None, // #1311: tests don't simulate registry
        )
        .await;

        let payload_text = response
            .result
            .as_ref()
            .and_then(|r| r.get("content"))
            .and_then(|c| c.as_array())
            .and_then(|arr| arr.first())
            .and_then(|c| c.get("text"))
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();

        if payload_text.contains("Unknown tool") {
            undispatched.push(tool_name.clone());
        }
    }

    assert!(
        undispatched.is_empty(),
        "ConfigChange tools must all be dispatched by handle_tools_call. \
             These returned \"Unknown tool\" — wire them into the ConfigChange \
             arm or special-case them above it: {undispatched:?}"
    );
}

// ── ADR-031 P4 § 6.5 / #1144 slice 8 — integration tests ─────────
//
// End-to-end coverage that ties the P4 backend slices (1-5) together
// through the public MCP surface:
// - `test_mcp_get_connector_metrics_returns_recorded_activity` proves
//   the runtime metrics from slices 1-3 actually flow through the
//   MCP tool added in slice 4. Other audit-table tests
//   (test_metrics_throughput_calculation, test_metrics_error_count,
//   test_record_activity_wired_into_route_forward) are already
//   pinned by `connector_registry::tests` (slice 3) and
//   `daemon::engine_manager::tests::test_stage9_*` (slice 3a).
// - `test_metrics_persist_across_disconnect_and_rebind` closes
//   AC #1 from #1144: "ConnectorMetrics updates on every route
//   forward; survives connector reconnect".

/// Helper that constructs SharedDaemonStateRefs with a pre-populated
/// connector — needed by the metrics tests because the existing
/// `build_test_shared_refs` builds an EMPTY registry.
fn build_test_shared_refs_with_connector(alias: &str) -> Arc<SharedDaemonStateRefs> {
    use crate::connector_registry::ConnectorRegistry;
    use crate::daemon::types::{DaemonStatistics, DeviceStatus, LifecycleState};
    use arc_swap::ArcSwap;
    use conductor_core::config::types::{
        ConnectorDirection, ConnectorProtocol, EndpointConfig, EndpointKind,
    };
    use conductor_core::control_state::PhysicalControlStateStore;
    use conductor_core::device_intelligence::fingerprint::EventStats;
    use conductor_core::device_intelligence::probe::ProbeCoordinator;
    use conductor_core::identity::DeviceMatcher;
    use dashmap::DashMap;
    use std::time::Instant;
    use tokio::sync::{Mutex, RwLock, mpsc};

    let endpoint = EndpointConfig {
        alias: alias.to_string(),
        direction: ConnectorDirection::Output,
        protocol: Some(ConnectorProtocol::Midi),
        kind: EndpointKind::Matcher {
            input_matchers: Vec::new(),
            output_matchers: Vec::new(),
            matchers: vec![DeviceMatcher::ExactName {
                value: format!("{alias}-port"),
            }],
            no_probe: true,
        },
        description: None,
        enabled: true,
        channels: vec![],
    };
    let registry = ConnectorRegistry::from_config(&[endpoint]);

    let (cmd_tx, _cmd_rx) = mpsc::channel(8);
    Arc::new(SharedDaemonStateRefs {
        lifecycle_state: Arc::new(RwLock::new(LifecycleState::Running)),
        device_status: Arc::new(RwLock::new(DeviceStatus::default())),
        statistics: Arc::new(RwLock::new(DaemonStatistics::default())),
        input_manager: Arc::new(Mutex::new(None)),
        config_path: PathBuf::from("/tmp/test.toml"),
        start_time: Instant::now(),
        command_tx: cmd_tx,
        active_profile: Arc::new(ArcSwap::from_pointee(None)),
        event_stats: Arc::new(DashMap::<String, EventStats>::new()),
        control_state: Arc::new(PhysicalControlStateStore::default()),
        probe_coordinator: Arc::new(ProbeCoordinator::new()),
        connector_registry: Arc::new(RwLock::new(registry)),
        device_output_map: Arc::new(ArcSwap::from_pointee(HashMap::new())),
        route_engine: Arc::new(ArcSwap::from_pointee(
            crate::route_engine::RouteEngine::compile(&[]),
        )),
        dispatch_trace: Arc::new(crate::daemon::dispatch_trace::DispatchTraceRing::default()),
    })
}

/// Slice 8 / AC #2: `conductor_get_connector_metrics` over the MCP
/// dispatch returns the live registry counters — proves the
/// slices-1-through-4 chain (record_activity wiring →
/// ConnectorMetrics → MCP tool payload) works end-to-end.
#[tokio::test]
async fn test_mcp_get_connector_metrics_returns_recorded_activity() {
    let config = live_config_arc(create_test_config());
    let tool_executor = McpToolExecutor::new();
    let daemon_state: Option<crate::daemon::types::DaemonState> = None;
    let refs = build_test_shared_refs_with_connector("absynth");
    let shared_state = Some(refs.clone());

    // Pre-record 5 forwards on the connector. record_activity also
    // feeds the ThroughputWindow (slice 3 / gap C), so the MCP
    // tool's throughput field should reflect non-zero rate.
    {
        let mut registry = refs.connector_registry.write().await;
        for _ in 0..5 {
            registry.record_activity("absynth");
        }
        // Also record one error to verify the error_count surfaces.
        registry.record_error("absynth");
    }

    let request = McpRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(McpRequestId::Number(500)),
        method: "tools/call".to_string(),
        params: Some(json!({
            "name": "conductor_get_connector_metrics",
            "arguments": {}
        })),
    };

    let response = handle_tools_call(
        &request,
        &config,
        &daemon_state,
        &tool_executor,
        &shared_state,
        None, // peer_ceiling — test path doesn't enforce tier ceiling
    )
    .await;

    assert!(response.error.is_none(), "no JSON-RPC error expected");
    let result = response.result.as_ref().expect("must return result");
    let content = result["content"].as_array().expect("content must be array");
    let inner_text = content[0]["text"]
        .as_str()
        .expect("first content text must be string");
    let inner: serde_json::Value =
        serde_json::from_str(inner_text).expect("inner content must parse as JSON");

    assert_eq!(
        inner["state_available"], true,
        "shared_state present in this test, so state_available MUST be true"
    );
    let connectors = inner["connectors"]
        .as_array()
        .expect("connectors must be array");
    let absynth = connectors
        .iter()
        .find(|c| c["alias"] == "absynth")
        .expect("absynth connector must appear in payload");

    // Slice 1-3 wiring all visible through slice 4's tool surface:
    assert_eq!(
        absynth["total_messages"], 5,
        "5 record_activity calls must surface as total_messages=5 via the MCP tool"
    );
    assert_eq!(
        absynth["error_count"], 1,
        "1 record_error call must surface as error_count=1 via the MCP tool"
    );
    // Throughput: 5 events all within a single second → 0.5 msg/s
    // (5 / 10-second window). Could be slightly less if the test
    // crosses a second boundary, so accept >= 0.4 as the floor.
    let rate = absynth["throughput_msgs_per_sec"]
        .as_f64()
        .expect("throughput_msgs_per_sec must be a number");
    assert!(
        rate >= 0.4,
        "5 events in <1s must yield throughput >= 0.4 (got {rate})"
    );
    // last_activity_ago_ms: just recorded, must be < 5000 (the
    // test is well-bounded — we recorded synchronously then
    // called the tool immediately).
    let last_ms = absynth["last_activity_ago_ms"]
        .as_u64()
        .expect("last_activity_ago_ms must be a number after activity");
    assert!(
        last_ms < 5000,
        "just-recorded activity must report last_activity_ago_ms < 5000 (got {last_ms})"
    );
}

/// Slice 8 / AC #1: ConnectorMetrics survives a disconnect + rebind
/// cycle. The registry's port-binding lifecycle is independent of
/// metric state — counters keep counting across hot-plug, otherwise
/// throughput dashboards lie every time a USB cable is wiggled.
#[tokio::test]
async fn test_metrics_persist_across_disconnect_and_rebind() {
    let refs = build_test_shared_refs_with_connector("controller");

    // Record some baseline activity + errors.
    {
        let mut registry = refs.connector_registry.write().await;
        for _ in 0..3 {
            registry.record_activity("controller");
        }
        registry.record_error("controller");
    }

    // Bind a port (simulates hot-plug-on).
    {
        let mut registry = refs.connector_registry.write().await;
        registry.bind_port("controller", "Controller MIDI".to_string(), 0, true);
        assert!(
            registry.get("controller").unwrap().connected,
            "bind_port must flip `connected` to true"
        );
    }

    // Snapshot pre-disconnect metrics.
    let (pre_total, pre_errors, pre_last) = {
        let registry = refs.connector_registry.read().await;
        let m = &registry.get("controller").unwrap().metrics;
        (m.total_messages, m.error_count, m.last_activity)
    };
    assert_eq!(pre_total, 3);
    assert_eq!(pre_errors, 1);
    assert!(pre_last.is_some());

    // Disconnect (simulates hot-unplug). The contract: bound_port
    // + connected reset, but METRICS PRESERVED.
    {
        let mut registry = refs.connector_registry.write().await;
        registry.disconnect("controller");
        let c = registry.get("controller").unwrap();
        assert!(!c.connected, "disconnect must flip `connected` to false");
        assert!(c.bound_port.is_none(), "disconnect must clear bound_port");
    }

    // The actual AC: metrics still report the baseline counts.
    {
        let registry = refs.connector_registry.read().await;
        let m = &registry.get("controller").unwrap().metrics;
        assert_eq!(
            m.total_messages, pre_total,
            "total_messages MUST survive disconnect (got {} vs pre={})",
            m.total_messages, pre_total
        );
        assert_eq!(
            m.error_count, pre_errors,
            "error_count MUST survive disconnect (got {} vs pre={})",
            m.error_count, pre_errors
        );
        assert_eq!(
            m.last_activity, pre_last,
            "last_activity timestamp MUST survive disconnect"
        );
    }

    // Rebind (simulates hot-replug). Metrics should still be intact
    // AND activity on the rebound port should accumulate ON TOP of
    // the preserved baseline, not reset to zero.
    {
        let mut registry = refs.connector_registry.write().await;
        registry.bind_port("controller", "Controller MIDI #2".to_string(), 1, true);
        registry.record_activity("controller"); // +1 forward post-rebind
    }
    {
        let registry = refs.connector_registry.read().await;
        let m = &registry.get("controller").unwrap().metrics;
        assert_eq!(
            m.total_messages,
            pre_total + 1,
            "post-rebind activity must accumulate on top of preserved counters \
                 (got {} vs expected {} + 1)",
            m.total_messages,
            pre_total
        );
        assert_eq!(
            m.error_count, pre_errors,
            "error_count untouched by post-rebind record_activity"
        );
    }
}
