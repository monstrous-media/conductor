// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Integration tests for ADR-031 P3 § 5.6 named tests + #1143 AC #5
//! (the keyboard-split decomposition integration test).
//!
//! - Introduced in slice 15 (PR #1272) — gaps **I** + **J** from
//!   the 2026-05-16 mid-flight audit on #1143. Landed 5 of the 6
//!   spec § 5.6 named tests.
//! - Extended in slice 16 (PR #1275) — gap **A**. Added the 6th
//!   test (`test_mcp_list_routing_graph`) alongside the new
//!   `conductor_get_routing_graph` MCP tool.
//!
//! All 6 spec § 5.6 named tests now present.
//!
//! # Dispatch path
//!
//! These tests invoke the LLM tool dispatch via in-process executor
//! handles, not a real Unix MCP socket connection:
//!
//! - Mutation tests (create / delete / update) use
//!   `conductor_daemon::daemon::llm::executor::ToolExecutor` — the
//!   in-process executor the GUI chat panel uses to author config
//!   changes (and the one that actually handles
//!   `conductor_batch_changes` per `executor.rs:2373`).
//! - Read-only topology tests (slice 16) use
//!   `conductor_daemon::daemon::mcp_tools::McpToolExecutor` directly,
//!   passing the synthetic config as the `Option<&Config>` parameter.
//!   Same pattern as the existing in-crate test
//!   `daemon::mcp_tools::tests::test_list_routes_returns_empty_array_for_default_config`
//!   — McpToolExecutor's contract IS "given (tool_name, args,
//!   status_data, devices_data, config, event_stats), return a
//!   ToolCallResult", so the right unit of testing is that function
//!   call, not a real socket round-trip. The full end-to-end socket
//!   path is exercised by `daemon::mcp::tests::test_mcp_server_*`.
//!
//! There is a separate, pre-existing limitation: the daemon-socket
//! dispatch covers ReadOnly + some Stateful tools but does NOT
//! dispatch any ConfigChange tool (`conductor_create_mapping`,
//! `conductor_batch_changes`, etc.) — the tools/list advertises them
//! (since slice 12 / gap F for `conductor_batch_changes`), but the
//! `tools/call` handler returns "Unknown tool" if invoked. That is
//! gap **K** in the #1143 audit table, tracked separately at #1274.
//! Until that gap is closed, mutation tests here use the GUI-path
//! `ToolExecutor` — the only place where the spec § 5.6 mutation
//! behaviours actually execute today.

// ADR-045 D1 (#2492): drives the LLM ToolExecutor mutation path; llm-executor builds only.
#![cfg(feature = "llm-executor")]

use std::sync::Arc;

use conductor_core::config::types::{
    ConnectorDirection, ConnectorProtocol, EndpointConfig, EndpointKind, RouteConfig,
};
use conductor_core::config::{Config, Mode};
use conductor_daemon::daemon::llm::executor::ExecutionResult;
use conductor_daemon::daemon::llm::{ConfigChange, PlanError, ToolExecutor};
use conductor_daemon::daemon::mcp_tools::McpToolExecutor;
use conductor_daemon::daemon::mcp_types::ToolContent;
use serde_json::{Value, json};

/// Build a minimal Config — no endpoints, no routes, one mode.
/// Tests that need richer setup `.push` directly into `endpoints` /
/// `routes`. Mirrors the `create_test_config()` helper in
/// `executor.rs` tests but defined here so this integration file
/// stays self-contained.
fn make_config() -> Config {
    Config {
        mcp: Default::default(),
        config_meta: Default::default(),
        security: Default::default(),
        endpoints: vec![],
        modes: vec![Mode {
            name: "Default".to_string(),
            color: Some("blue".to_string()),
            mappings: vec![],
        }],
        global_mappings: vec![],
        logging: None,
        advanced_settings: Default::default(),
        last_selected_mode: None,
        per_app_modes: None,
        default_mode: None,
        led: None,
        event_console: None,
        routes: vec![],
    }
}

/// Build an output `Matcher` endpoint with the given alias — the ADR-035
/// successor to a former output `[[connectors]]`/`ConnectorConfig` fixture.
fn output_endpoint(alias: &str) -> EndpointConfig {
    EndpointConfig {
        alias: alias.to_string(),
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
    }
}

/// An Input endpoint — needed so a route's `from` alias resolves to a declared
/// source. ADR-035 Rule 1a (`validate_routes`) ERRORs on a route whose `from`
/// is not a declared `[[endpoints]]` entry, so any fixture that routes *from* a
/// source must declare it (#2161).
fn input_endpoint(alias: &str) -> EndpointConfig {
    EndpointConfig {
        alias: alias.to_string(),
        direction: ConnectorDirection::Input,
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
    }
}

/// Spec § 5.6 row 2: `test_mcp_create_route` — `conductor_batch_changes`
/// with a `create_route` op carrying a filter + transform produces a
/// valid plan with one `CreateRoute` change. The filter is preserved
/// through serde round-trip; the transform too.
#[tokio::test]
async fn test_mcp_create_route() {
    let executor = ToolExecutor::new(Arc::new(
        conductor_daemon::daemon::live_config::LiveConfig::new(make_config()).unwrap(),
    ));

    let args = json!({
        "operations": [{
            "type": "create_route",
            "from": "mikro",
            "to": "absynth",
            "filter": { "channels": [0, 1], "note_range": [21, 60] },
            // MidiTransform single-channel remap (ADR-009 Gap 2 / v4.25.0):
            // remap inbound channel to channel 1, no other transforms.
            "transform": { "type": "Midi", "channel": 1 },
            "enabled": true,
            "description": "Lower-half passthrough"
        }]
    });

    let result = executor
        .execute("conductor_batch_changes", Some(args), None)
        .await;

    match result {
        ExecutionResult::PlanCreated { plan } => {
            assert_eq!(plan.changes.len(), 1, "expected 1 change in plan");
            match &plan.changes[0] {
                ConfigChange::CreateRoute {
                    from,
                    to,
                    filter,
                    transform,
                    enabled,
                    ..
                } => {
                    assert_eq!(from, "mikro");
                    assert_eq!(to, "absynth");
                    // Round-trip content check, not just `is_some`.
                    // (Council review on PR #1275 flagged the prior
                    // `.is_some()` form as missing content verification:
                    // a regression that defaulted the inner fields
                    // would still pass.)
                    let filter = filter
                        .as_ref()
                        .expect("filter must round-trip into the plan");
                    assert_eq!(
                        filter.channels,
                        vec![0u8, 1u8],
                        "filter.channels must round-trip exactly"
                    );
                    assert_eq!(
                        filter.note_range,
                        Some((21u8, 60u8)),
                        "filter.note_range must round-trip exactly"
                    );
                    let transform = transform
                        .as_ref()
                        .expect("transform must round-trip into the plan");
                    // The Midi transform variant carries a single
                    // channel remap (per `conductor_core::transform::MidiTransform`);
                    // round-trip the variant tag + the specific
                    // `channel: 1` that the request sent.
                    let transform_json =
                        serde_json::to_value(transform).expect("transform must serialize");
                    assert_eq!(
                        transform_json["type"].as_str(),
                        Some("Midi"),
                        "transform must round-trip as Midi variant; got: {transform_json}"
                    );
                    assert_eq!(
                        transform_json["channel"].as_u64(),
                        Some(1),
                        "transform.channel must round-trip the request's channel=1; \
                         got: {transform_json}"
                    );
                    assert!(*enabled, "enabled stays true");
                }
                other => panic!("Expected CreateRoute, got {other:?}"),
            }
        }
        other => panic!("Expected PlanCreated, got {other:?}"),
    }
}

/// Spec § 5.6 row 3: `test_mcp_reject_route_to_nonexistent` — a route
/// referencing an unknown alias must be rejected before it can become
/// part of the live config. The executor lets the LLM AUTHOR a bogus
/// plan (so the user can review what was attempted); the validation gate
/// inside `ConfigPlan::apply` then catches the dangling reference and
/// rejects the plan before the caller's config is ever mutated.
///
/// This test exercises BOTH layers:
///   1. `conductor_batch_changes` with a `create_route` op referencing
///      unknown aliases creates a plan successfully (no eager
///      rejection — that's the executor's design contract).
///   2. `plan.apply` rejects the post-mutation config (via the
///      `validate_config` gate inside `apply_atomic`) and returns
///      `PlanError::ValidationFailed` naming both offending aliases.
///      The caller's config is left untouched (#2115 / clawpatch #2103).
///
/// Council review on PR #1275 flagged the prior version as bypassing
/// the executor path; this version exercises both layers explicitly.
#[tokio::test]
async fn test_mcp_reject_route_to_nonexistent() {
    let mut config = make_config();
    // One real endpoint — but the route below references different
    // ("ghost" / "mikro") aliases that do not appear in `[[endpoints]]`.
    config.endpoints.push(output_endpoint("absynth"));

    // Layer 1: executor path. Authoring a bogus-alias route via
    // batch_changes must SUCCEED at plan creation (the executor
    // doesn't eagerly validate aliases — only TOCTOU base_state_hash
    // and the per-op shape).
    let executor = ToolExecutor::new(Arc::new(
        conductor_daemon::daemon::live_config::LiveConfig::new(config.clone()).unwrap(),
    ));
    let args = json!({
        "operations": [{
            "type": "create_route",
            "from": "mikro",
            "to": "ghost",
        }]
    });
    let result = executor
        .execute("conductor_batch_changes", Some(args), None)
        .await;
    let plan = match result {
        ExecutionResult::PlanCreated { plan } => {
            assert_eq!(plan.changes.len(), 1, "plan should carry the bogus route");
            assert!(
                matches!(plan.changes[0], ConfigChange::CreateRoute { .. }),
                "the bogus route lands in the plan unchanged"
            );
            plan
        }
        other => panic!(
            "Plan creation must succeed (executor doesn't eagerly \
             validate aliases — that's the validator's job); got {other:?}"
        ),
    };

    // Layer 2: APPLY the generated plan to the config (true E2E flow
    // — not a manually-constructed parallel config).
    //
    // #2115 (clawpatch #2103): `ConfigPlan::apply` now enforces the same
    // post-mutation `validate_config` gate as `apply_atomic` instead of
    // bypassing it. The bogus-alias route is therefore rejected AT apply
    // time — the unsafe config never reaches the caller — rather than
    // being silently applied and only flagged by a later, separate
    // `validate_config` pass. The atomic gate also leaves the caller's
    // config untouched on rejection.
    //
    // (Council review on PR #1275 round-4 flagged the prior shape —
    // parallel manually-constructed config — as breaking the
    // integration test's end-to-end validity. Apply the plan we
    // actually got back from the executor so the test exercises the
    // real bytes the user would approve.)
    let apply_result = plan.apply(&mut config);
    let errors = match apply_result {
        Err(PlanError::ValidationFailed { errors }) => errors,
        other => panic!(
            "apply must reject a plan whose post-state references nonexistent route \
             aliases (post-apply validation, not a bypass); got {other:?}"
        ),
    };

    // Atomicity: the rejected apply must not mutate the caller's config.
    assert_eq!(
        config.routes.len(),
        0,
        "rejected apply must leave the config untouched (no route added)"
    );

    // The apply-time rejection (which runs the structural validator on
    // the post-mutation config) must name BOTH unknown aliases — the
    // validator surfaces `from` and `to` as separate findings.
    assert!(
        errors.contains("ghost"),
        "apply rejection must name the unknown 'to' alias 'ghost'; got: {errors}"
    );
    assert!(
        errors.contains("mikro"),
        "apply rejection must name the unknown 'from' alias 'mikro'; got: {errors}"
    );
}

/// Spec § 5.6 row 4 + #1143 AC #5 (the keyboard-split decomposition
/// integration test). One `conductor_batch_changes` call carrying 2×
/// `create_route` (keyboard → synth on lower half, keyboard → drums on
/// upper half) must produce ONE atomic plan with 2 route changes.
///
/// ADR-035 Phase 2 #1748: the original test also batched a
/// `create_connector` op for the synth's output port — that batch op was
/// removed alongside the legacy connector tools (endpoints are authored
/// via the singleton `conductor_create_endpoint`). This test now pins the
/// multi-route atomic-batch behaviour, which is unchanged.
#[tokio::test]
async fn test_mcp_batch_multi_route_setup() {
    // NOTE (#2161): unlike `test_mcp_list_routing_graph`, this fixture
    // INTENTIONALLY leaves `keyboard`/`absynth`/`drums` undeclared. It pins the
    // executor's plan-AUTHORING contract, which by design does NOT eagerly
    // validate aliases — ADR-035 Rule 1a only runs at apply time (proven by
    // `test_mcp_reject_route_to_nonexistent`). The plan is authored but never
    // applied here, so declaring endpoints would add nothing and would muddy
    // what this test isolates. Do not "fix" these to declared aliases.
    let executor = ToolExecutor::new(Arc::new(
        conductor_daemon::daemon::live_config::LiveConfig::new(make_config()).unwrap(),
    ));

    let args = json!({
        "operations": [
            {
                "type": "create_route",
                "from": "keyboard",
                "to": "absynth",
                "filter": { "note_range": [21, 59] }
            },
            {
                "type": "create_route",
                "from": "keyboard",
                "to": "drums",
                "filter": { "note_range": [60, 108] }
            }
        ]
    });

    let result = executor
        .execute("conductor_batch_changes", Some(args), None)
        .await;

    match result {
        ExecutionResult::PlanCreated { plan } => {
            assert_eq!(
                plan.changes.len(),
                2,
                "expected 2 route changes; got {}: {:?}",
                plan.changes.len(),
                plan.changes
            );
            // Both routes — order preserved, BOTH destinations must
            // appear. (Council review on PR #1272 flagged the
            // per-iteration `to == "absynth" || to == "drums"` loop as
            // vacuous: a regression where the LLM authored two routes
            // to "absynth" and dropped "drums" would still pass.)
            //
            // #2133: the destinations alone do NOT verify the *split* — the
            // split IS the per-route `note_range` filter (lower half → synth,
            // upper half → drums). The previous assertion matched
            // `CreateRoute { from, to, .. }`, discarding `filter`, so a
            // regression that dropped or swapped the note ranges still passed.
            // Capture and pin each route's note_range alongside its
            // destination.
            let mut routes: Vec<(String, Option<(u8, u8)>)> = Vec::new();
            for (i, change) in plan.changes.iter().enumerate() {
                match change {
                    ConfigChange::CreateRoute {
                        from, to, filter, ..
                    } => {
                        assert_eq!(from, "keyboard", "route[{i}].from");
                        routes.push((to.clone(), filter.as_ref().and_then(|f| f.note_range)));
                    }
                    other => panic!("change[{i}] must be CreateRoute; got {other:?}"),
                }
            }
            let mut sorted = routes.clone();
            sorted.sort_by(|a, b| a.0.cmp(&b.0));
            assert_eq!(
                sorted,
                vec![
                    ("absynth".to_string(), Some((21, 59))),
                    ("drums".to_string(), Some((60, 108))),
                ],
                "the keyboard split must route notes 21–59 → absynth and \
                 60–108 → drums (destinations AND note_range filters); got {routes:?}"
            );
            // Plan description should mention the route changes
            assert!(
                plan.description.contains("route"),
                "plan description must summarise the multi-route setup; got: {}",
                plan.description
            );
        }
        other => panic!("Expected PlanCreated, got {other:?}"),
    }
}

/// Spec § 5.6 row 5: `test_mcp_list_routing_graph` — the combined
/// `conductor_get_routing_graph` topology view (slice 16 / gap A)
/// returns the active config's endpoints + routes in one round-trip.
///
/// Dispatched via `McpToolExecutor` (daemon-socket path) — this is one
/// of the few ReadOnly routing tools that actually dispatches there
/// (most ConfigChange tools are blocked on gap K = #1274). The test
/// pins:
///   - `endpoints` array (ADR-035) carries every declared endpoint with
///     full fields (alias / direction / protocol / flattened type tag).
///   - `routes` array carries every declared route with from/to.
///   - `excluded` array is currently empty (deferred per the
///     `excluded_note` field — same caveat as `conductor_list_routes`
///     until RouteEngine wiring lands on `SharedDaemonStateRefs`).
///   - The deferred-status caveat is surfaced so callers don't
///     misinterpret an empty `excluded` array as "no routes excluded".
#[tokio::test]
async fn test_mcp_list_routing_graph() {
    let mut config = make_config();
    // The shared route source: both routes below are `from: keyboard`, so it
    // must be a declared Input endpoint (#2161).
    config.endpoints.push(input_endpoint("keyboard"));
    config.endpoints.push(EndpointConfig {
        description: Some("Absynth output".to_string()),
        ..output_endpoint("absynth")
    });
    config.endpoints.push(output_endpoint("drums"));
    config.routes.push(RouteConfig {
        from: "keyboard".to_string(),
        to: "absynth".to_string(),
        transform: None,
        filter: None,
        enabled: true,
        description: None,
        modes: Vec::new(),
    });
    config.routes.push(RouteConfig {
        from: "keyboard".to_string(),
        to: "drums".to_string(),
        transform: None,
        filter: None,
        enabled: true,
        description: None,
        modes: Vec::new(),
    });

    // Regression guard (#2161): every route in the routing-graph fixture must
    // source/sink a *declared* endpoint. Both routes here are `from: keyboard`,
    // so "keyboard" must be a declared endpoint — otherwise ADR-035 Rule 1a
    // (`validate_routes`) rejects the route's `from`/`to` as an unknown alias
    // and the graph is being exercised against a config real config-loading
    // would never accept. Scoped to the route-reference errors (the #2161
    // finding) rather than full `is_valid()`, so an unrelated fixture nit can't
    // mask a regression of *this* bug. Fails until "keyboard" is declared above.
    let report = conductor_core::config::validation::validate_config(&config);
    let undeclared_route_refs: Vec<_> = report
        .errors
        .iter()
        .filter(|e| {
            (e.path.ends_with(".from") || e.path.ends_with(".to"))
                && e.message.contains("unknown alias")
        })
        .collect();
    assert!(
        undeclared_route_refs.is_empty(),
        "routing-graph fixture routes from/to undeclared endpoints (ADR-035 Rule 1a); \
         declare them as [[endpoints]]: {:#?}",
        undeclared_route_refs
    );

    let executor = McpToolExecutor::new();
    let result = executor
        .execute(
            "conductor_get_routing_graph",
            None,
            None,
            None,
            Some(&config),
            None,
        )
        .await;

    // `is_error: None` and `is_error: Some(false)` both mean success;
    // `unwrap_or(false)` collapses to a single boolean for the
    // assertion (avoids the vacuous-OR pattern the file's own
    // policy in slice 15's Council review called out).
    assert!(
        !result.is_error.unwrap_or(false),
        "tool must return success; got is_error={:?}",
        result.is_error
    );

    // The tool's content is a single Text item carrying the JSON
    // payload (McpToolExecutor's `ToolCallResult::json` helper wraps
    // serialised values in a Text content frame).
    let text_payload = result
        .content
        .iter()
        .find_map(|c| match c {
            ToolContent::Text { text } => Some(text.clone()),
            _ => None,
        })
        .expect("response must include a Text content payload");
    let payload: Value = serde_json::from_str(&text_payload).expect("payload must parse as JSON");

    // endpoints array (ADR-035: `conductor_get_routing_graph` now serializes
    // `config.endpoints` under the `endpoints` key): both aliases must be
    // present, in any order, AND every entry must round-trip alias + direction
    // + protocol + the flattened endpoint `type` tag (the comment above
    // promises this; pin it. Council review on PR #1275 flagged the
    // comment-vs-assertion gap).
    let connector_entries = payload["endpoints"]
        .as_array()
        .expect("endpoints must be an array");
    let conn_aliases: Vec<&str> = connector_entries
        .iter()
        .filter_map(|c| c["alias"].as_str())
        .collect();
    let mut conn_aliases_sorted = conn_aliases.clone();
    conn_aliases_sorted.sort();
    assert_eq!(
        conn_aliases_sorted,
        vec!["absynth", "drums", "keyboard"],
        "endpoints array must list ALL declared endpoints (incl. the `keyboard` \
         route source); got {:?}",
        conn_aliases
    );
    // Per-entry shape check: every endpoint must serialize the
    // load-bearing fields (alias / direction / protocol / type) since the
    // LLM uses them to reason about routing topology. A regression dropping
    // any of these would silently degrade the graph view. (`EndpointConfig`
    // flattens its `kind` payload, so the kind tag surfaces as `type` at the
    // top level rather than a nested `endpoint` object.)
    for entry in connector_entries {
        assert!(
            entry["alias"].is_string(),
            "endpoint entry missing 'alias' string field; got: {entry}"
        );
        assert!(
            entry["direction"].is_string(),
            "endpoint entry missing 'direction' field; got: {entry}"
        );
        assert!(
            entry["protocol"].is_string(),
            "endpoint entry missing 'protocol' field; got: {entry}"
        );
        assert!(
            entry["type"].is_string(),
            "endpoint entry missing flattened 'type' tag; got: {entry}"
        );
    }

    // Direction VALUES must round-trip, not just be present (#2161): the
    // route source `keyboard` is an Input endpoint and the two sinks are
    // Outputs. Asserting the actual direction guards a regression that
    // includes `keyboard` in the graph but mislabels its direction — which
    // the weaker is_string() check above would silently pass.
    let direction_of = |alias: &str| -> &str {
        connector_entries
            .iter()
            .find(|e| e["alias"].as_str() == Some(alias))
            .and_then(|e| e["direction"].as_str())
            .unwrap_or_else(|| panic!("endpoint '{alias}' missing from graph endpoints"))
    };
    assert_eq!(
        direction_of("keyboard"),
        "Input",
        "the route source `keyboard` must serialize as an Input endpoint"
    );
    assert_eq!(direction_of("absynth"), "Output", "`absynth` is a sink");
    assert_eq!(direction_of("drums"), "Output", "`drums` is a sink");

    // routes array: both routes must appear, both pointing from "keyboard"
    let routes = payload["routes"]
        .as_array()
        .expect("routes must be an array");
    assert_eq!(
        routes.len(),
        2,
        "expected exactly 2 routes; got {}",
        routes.len()
    );
    // Collect (from, to) pairs first so any panic carries the full
    // serialized route on the iterator's error line rather than a
    // confusing closure-panic. (Council review on PR #1275 flagged
    // the prior `assert_eq!` inside `filter_map` as obscuring test-
    // failure semantics.)
    let route_pairs: Vec<(String, String)> = routes
        .iter()
        .map(|r| {
            let from = r["from"]
                .as_str()
                .unwrap_or_else(|| panic!("route is missing 'from'; got: {r}"))
                .to_string();
            let to = r["to"]
                .as_str()
                .unwrap_or_else(|| panic!("route is missing 'to'; got: {r}"))
                .to_string();
            (from, to)
        })
        .collect();
    // Every route in this fixture must have from=keyboard
    for (from, _to) in &route_pairs {
        assert_eq!(
            from,
            "keyboard",
            "every route in this fixture has from=keyboard; got pair: {:?}",
            (from, &route_pairs)
        );
    }
    let mut route_dests: Vec<&str> = route_pairs.iter().map(|(_, to)| to.as_str()).collect();
    route_dests.sort();
    assert_eq!(
        route_dests,
        vec!["absynth", "drums"],
        "the 2 route destinations must be exactly absynth + drums; got {:?}",
        route_dests
    );

    // excluded array stays empty (RouteEngine plumbing deferred), and
    // the `excluded_note` caveat must be present so callers don't
    // misinterpret the empty array.
    assert_eq!(
        payload["excluded"]
            .as_array()
            .expect("excluded must be an array")
            .len(),
        0,
        "excluded is intentionally empty until RouteEngine wiring lands"
    );
    let note = payload["excluded_note"]
        .as_str()
        .expect("excluded_note must be a string");
    // The note must mention BOTH 'RouteEngine' (what's missing) AND
    // 'SharedDaemonStateRefs' (where it needs to land) so the LLM /
    // user understands the precise architectural gap. Splitting from
    // the prior `||` form per Council's slice-15 policy (vacuous-OR
    // assertions hide regressions that drop one of the named terms).
    assert!(
        note.contains("RouteEngine"),
        "excluded_note must name 'RouteEngine' (the missing component); got: {}",
        note
    );
    assert!(
        note.contains("SharedDaemonStateRefs"),
        "excluded_note must name 'SharedDaemonStateRefs' (the wiring \
         target); got: {}",
        note
    );
}
