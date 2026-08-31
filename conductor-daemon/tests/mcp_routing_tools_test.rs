// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Integration tests for the Slice 9 (#1667) ReadOnly routing MCP tools
//! — `conductor_explain_route_match` and `conductor_get_dispatch_trace`
//! — driven end-to-end through the in-process `ToolExecutor` (the same
//! dispatch path the GUI chat panel uses).
//!
//! These exercise the executor handlers (argument parsing, shared-refs
//! access, JSON payload shape), complementing the unit tests on the
//! underlying primitives (`route_engine` explain, `dispatch_trace` ring).
//!
//! Requires the `test-helpers` feature for
//! `SharedDaemonStateRefs::for_routing_tools_test` (CI + pre-push gates
//! run `cargo test --all-features`, same as the live_config suite).

// ADR-045 D1 (#2492): drives the LLM ToolExecutor; llm-executor builds only.
#![cfg(feature = "llm-executor")]

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use arc_swap::ArcSwap;
use tokio::sync::Mutex;

use conductor_core::config::Config;
use conductor_core::config::types::{RouteConfig, SignalFilter};
use conductor_daemon::daemon::dispatch_trace::{DispatchTraceEntry, DispatchTraceRing, now_ms};
use conductor_daemon::daemon::engine_manager::SharedDaemonStateRefs;
use conductor_daemon::daemon::live_config::LiveConfig;
use conductor_daemon::daemon::llm::executor::{ExecutionResult, ToolExecutor};
use conductor_daemon::daemon::mcp_types::ToolContent;
use conductor_daemon::route_engine::RouteEngine;
use serde_json::{Value, json};

/// Minimal valid config so `LiveConfig::new` succeeds — the routing
/// tools read `route_engine` / `dispatch_trace` off the shared refs,
/// NOT `live_config`, so this stays empty of routes/connectors.
fn minimal_config() -> Config {
    toml::from_str(
        r#"
[[modes]]
name = "Default"
"#,
    )
    .expect("minimal config parses")
}

fn route(from: &str, to: &str) -> RouteConfig {
    RouteConfig {
        from: from.into(),
        to: to.into(),
        transform: None,
        filter: None,
        enabled: true,
        description: None,
        modes: Vec::new(),
    }
}

/// Build a `ToolExecutor` whose shared refs carry `route_engine` and
/// `dispatch_trace` — everything the two Slice 9 tools read.
fn executor_with(route_engine: RouteEngine, trace: DispatchTraceRing) -> ToolExecutor {
    let live_config = Arc::new(LiveConfig::new(minimal_config()).expect("live config init"));
    let refs = SharedDaemonStateRefs::for_routing_tools_test(
        Arc::new(ArcSwap::from_pointee(route_engine)),
        Arc::new(trace),
    );
    ToolExecutor::with_daemon_state(
        live_config,
        Arc::new(AtomicBool::new(false)),
        Arc::new(Mutex::new(VecDeque::new())),
        refs,
    )
}

/// Extract + parse the JSON payload from a `Success` result.
fn success_json(result: ExecutionResult) -> Value {
    let ExecutionResult::Success { result } = result else {
        panic!("expected ExecutionResult::Success, got {result:?}");
    };
    let text = result
        .content
        .iter()
        .find_map(|c| match c {
            ToolContent::Text { text } => Some(text.clone()),
            _ => None,
        })
        .expect("Text content payload");
    serde_json::from_str(&text).expect("payload parses as JSON")
}

#[tokio::test]
async fn explain_route_match_reports_fired_and_skipped() {
    // Two routes from "pads": one unconstrained (fires), one scoped to
    // mode "Drums" (skipped when active mode is "Keys").
    let routes = vec![
        route("pads", "absynth"),
        RouteConfig {
            modes: vec!["Drums".into()],
            ..route("pads", "drum_synth")
        },
    ];
    let executor = executor_with(RouteEngine::compile(&routes), DispatchTraceRing::default());

    let args = json!({
        "event": { "device": "pads", "type": "note_on", "channel": 0, "data1": 60, "data2": 64 },
        "active_mode": "Keys"
    });
    let payload = success_json(
        executor
            .execute("conductor_explain_route_match", Some(args), None)
            .await,
    );

    assert_eq!(payload["device"], "pads");
    assert_eq!(payload["active_mode"], "Keys");
    let routes_out = payload["routes"].as_array().expect("routes array");
    assert_eq!(routes_out.len(), 2, "one entry per candidate route");

    let absynth = routes_out
        .iter()
        .find(|r| r["to_alias"] == "absynth")
        .expect("absynth explained");
    assert_eq!(absynth["fired"], true);

    let drum = routes_out
        .iter()
        .find(|r| r["to_alias"] == "drum_synth")
        .expect("drum_synth explained");
    assert_eq!(drum["fired"], false);
    assert_eq!(drum["skip_reason"]["kind"], "mode_ineligible");
    assert_eq!(drum["skip_reason"]["active_mode"], "Keys");
}

#[tokio::test]
async fn explain_route_match_unknown_device_is_empty_with_note() {
    let executor = executor_with(
        RouteEngine::compile(&[route("pads", "absynth")]),
        DispatchTraceRing::default(),
    );
    let args = json!({
        "event": { "device": "ghost", "type": "note_on", "channel": 0, "data1": 60, "data2": 64 },
        "active_mode": "Default"
    });
    let payload = success_json(
        executor
            .execute("conductor_explain_route_match", Some(args), None)
            .await,
    );
    assert!(
        payload["routes"]
            .as_array()
            .expect("routes array")
            .is_empty(),
        "no routes from an unknown device"
    );
    assert!(
        payload["note"]
            .as_str()
            .is_some_and(|s| s.contains("ghost")),
        "note explains there are no routes from 'ghost': {:?}",
        payload["note"]
    );
}

#[tokio::test]
async fn explain_route_match_filter_mismatch_reports_dimension() {
    // Route filtered to channel 5; event on channel 0 → filter_mismatch
    // on the channel dimension.
    let routes = vec![RouteConfig {
        filter: Some(SignalFilter {
            channels: vec![5],
            message_types: vec![],
            cc_range: None,
            note_range: None,
            osc_address_prefix: None,
        }),
        ..route("pads", "ch5_only")
    }];
    let executor = executor_with(RouteEngine::compile(&routes), DispatchTraceRing::default());
    let args = json!({
        "event": { "device": "pads", "type": "note_on", "channel": 0, "data1": 60, "data2": 64 },
        "active_mode": "Default"
    });
    let payload = success_json(
        executor
            .execute("conductor_explain_route_match", Some(args), None)
            .await,
    );
    let r = &payload["routes"][0];
    assert_eq!(r["fired"], false);
    assert_eq!(r["skip_reason"]["kind"], "filter_mismatch");
    assert_eq!(r["skip_reason"]["dimension"], "channel");
}

#[tokio::test]
async fn explain_route_match_rejects_malformed_event() {
    let executor = executor_with(
        RouteEngine::compile(&[route("pads", "absynth")]),
        DispatchTraceRing::default(),
    );
    // Missing event.device.
    let args = json!({
        "event": { "type": "note_on", "channel": 0, "data1": 60 },
        "active_mode": "Default"
    });
    let result = executor
        .execute("conductor_explain_route_match", Some(args), None)
        .await;
    assert!(
        matches!(result, ExecutionResult::Error { .. }),
        "missing event.device is a structured error, got {result:?}"
    );
}

#[tokio::test]
async fn get_dispatch_trace_returns_last_n_newest_last() {
    let trace = DispatchTraceRing::default();
    for i in 0..5 {
        trace.record(DispatchTraceEntry {
            timestamp_ms: now_ms(),
            device_id: "pads".into(),
            active_mode: "Default".into(),
            event: format!("NoteOn ch0 note{i} vel64"),
            destinations: vec!["absynth".into()],
            matched_mapping: None,
            let_through: false,
        });
    }
    let executor = executor_with(RouteEngine::compile(&[]), trace);

    let payload = success_json(
        executor
            .execute(
                "conductor_get_dispatch_trace",
                Some(json!({ "last": 2 })),
                None,
            )
            .await,
    );
    assert_eq!(payload["count"], 2);
    assert_eq!(payload["requested"], 2);
    let entries = payload["entries"].as_array().expect("entries array");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["event"], "NoteOn ch0 note3 vel64");
    assert_eq!(entries[1]["event"], "NoteOn ch0 note4 vel64", "newest last");
    assert_eq!(entries[1]["device_id"], "pads");
    assert_eq!(entries[1]["destinations"][0], "absynth");
    // Wire-shape guarantee: `matched_mapping` must be PRESENT in the serialized
    // payload (serde_json indexing returns Null for a missing key too, so assert
    // the key exists before checking it's null — Copilot review on PR #1882).
    let entry1 = entries[1]
        .as_object()
        .expect("trace entry is a JSON object");
    assert!(
        entry1.contains_key("matched_mapping"),
        "matched_mapping must be serialized into the trace entry"
    );
    assert!(
        entry1["matched_mapping"].is_null(),
        "a no-match route entry carries matched_mapping = null"
    );
    assert!(
        entry1.contains_key("let_through"),
        "let_through must be serialized into the trace entry"
    );
    assert_eq!(entry1["let_through"], false);
}

#[tokio::test]
async fn get_dispatch_trace_defaults_and_caps_request() {
    let trace = DispatchTraceRing::default();
    trace.record(DispatchTraceEntry {
        timestamp_ms: now_ms(),
        device_id: "pads".into(),
        active_mode: "Default".into(),
        event: "CC ch0 cc7 val100".into(),
        destinations: vec!["fx".into()],
        matched_mapping: None,
        let_through: false,
    });
    let executor = executor_with(RouteEngine::compile(&[]), trace);

    // No `last` → default 32 requested; only 1 entry exists.
    let default_payload = success_json(
        executor
            .execute("conductor_get_dispatch_trace", Some(json!({})), None)
            .await,
    );
    assert_eq!(default_payload["requested"], 32);
    assert_eq!(default_payload["count"], 1);

    // `last` above the 256 cap is clamped to 256.
    let capped_payload = success_json(
        executor
            .execute(
                "conductor_get_dispatch_trace",
                Some(json!({ "last": 9999 })),
                None,
            )
            .await,
    );
    assert_eq!(capped_payload["requested"], 256, "request capped at 256");
    assert_eq!(capped_payload["count"], 1);
}
