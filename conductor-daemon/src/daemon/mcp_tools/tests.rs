// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Tests for the `mcp_tools` module family.

use crate::daemon::mcp_types::ToolRiskTier;
use conductor_core::config::Config;
use conductor_core::device_intelligence::fingerprint::EventStats;
use dashmap::DashMap;
use serde_json::{Value, json};

use super::executor::{infer_message_type_from_trigger, midi_message_to_trigger_type};

use super::super::mcp_types::ToolContent;
use super::*;
use conductor_core::config::{ActionConfig, Mapping, Mode, Trigger};

fn create_test_config() -> Config {
    Config {
        mcp: Default::default(),
        per_app_modes: None,
        config_meta: Default::default(),
        security: Default::default(),
        endpoints: vec![],
        modes: vec![
            Mode {
                name: "Default".to_string(),
                color: Some("blue".to_string()),
                mappings: vec![
                    Mapping {
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
                    },
                    Mapping {
                        trigger: Trigger::Note {
                            note: 37,
                            velocity_min: Some(1),
                            channel: None,
                            device: None,
                        },
                        action: ActionConfig::Keystroke {
                            keys: "v".to_string(),
                            modifiers: vec!["cmd".to_string()],
                        },
                        description: Some("Paste".to_string()),
                        let_through: false,
                    },
                ],
            },
            Mode {
                name: "DJ".to_string(),
                color: Some("purple".to_string()),
                mappings: vec![],
            },
        ],
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

// ── LLM-facing schema descriptions — trigger + action ──
//
// The LLM only sends shapes it sees in
// `input_schema.trigger.description` / `input_schema.action.description`.
// These tests pin both strings so a future contributor who adds another
// trigger/action variant remembers to mention it for LLM discoverability —
// and (ADR-036 Phase 2) so removed variants like Raw are NOT re-advertised.

#[test]
fn test_security_status_schema_is_a_no_arg_readonly_tool() {
    // ADR-042 B.7 — `conductor_security_status` exposes the
    // network-approval HMAC key's rotation status to LLM callers. Pin the
    // same invariants as the other ReadOnly status tools: object schema,
    // no required args, description mentions the key/rotation, ReadOnly
    // tier (it only reads the keychain).
    let tools = get_tool_definitions();
    let def = tools
        .iter()
        .find(|t| t.name == "conductor_security_status")
        .expect("conductor_security_status tool definition must exist");

    assert_eq!(
        def.input_schema["type"].as_str(),
        Some("object"),
        "schema must declare `type: object`"
    );
    let required = def.input_schema["required"]
        .as_array()
        .expect("schema must include a `required` array");
    assert!(required.is_empty(), "no-arg tool: required must be empty");

    let desc = def.description.as_str();
    assert!(
        desc.contains("rotation") || desc.contains("HMAC") || desc.contains("key"),
        "description must mention the HMAC key / rotation; got: {desc}"
    );

    assert_eq!(
        get_tool_risk_tier("conductor_security_status"),
        ToolRiskTier::ReadOnly,
        "conductor_security_status must be ReadOnly"
    );
}

#[test]
fn test_list_routes_schema_is_a_no_arg_readonly_tool() {
    // ADR-031 § 3.7 P3 — `conductor_list_routes` is a
    // ReadOnly routing-graph tool. Pin the
    // same invariants:
    //   - object schema with no required args (no-arg tool)
    //   - description mentions "route(s)" so the LLM can find it
    //     when the user asks about routing
    //   - risk tier stays ReadOnly (the tool reads `config.routes`;
    //     mutations land via `conductor_batch_changes` in a later
    //     slice and have their own ConfigChange tier).
    // Failure of any of these means a future edit silently broke
    // discoverability or quietly upgraded the tier — both are the
    // class of regression this test guards against.
    let tools = get_tool_definitions();
    let def = tools
        .iter()
        .find(|t| t.name == "conductor_list_routes")
        .expect("conductor_list_routes tool definition must exist");

    assert_eq!(
        def.input_schema["type"].as_str(),
        Some("object"),
        "schema must declare `type: object`"
    );
    let required = def.input_schema["required"]
        .as_array()
        .expect("schema must include a `required` array");
    assert!(required.is_empty(), "no-arg tool: required must be empty");

    let desc = def.description.as_str();
    assert!(
        desc.contains("route") || desc.contains("Route"),
        "description must mention route/Route for LLM discoverability; got: {}",
        desc
    );
    // Mention the evaluation-priority rule so the LLM knows routes
    // sit BELOW per-event mappings (keep the tool description
    // consistent with the prompt surfaces).
    assert!(
        desc.contains("evaluation priority"),
        "description must mention evaluation priority so the LLM \
             keeps routes / mappings straight; got: {}",
        desc
    );

    assert_eq!(
        get_tool_risk_tier("conductor_list_routes"),
        ToolRiskTier::ReadOnly,
        "conductor_list_routes must be ReadOnly — mutations land \
             via conductor_batch_changes (ConfigChange) per ADR-031 § 5.4"
    );
}

#[test]
fn test_get_routing_graph_schema_is_a_no_arg_readonly_tool() {
    // ADR-031 § 3.7 P3 (gap A) — `conductor_get_routing_graph`
    // is a ReadOnly tool in the routing-graph family alongside
    // `conductor_list_routes`. It returns the COMBINED topology
    // (connectors + routes) so the LLM can answer "what does my
    // routing graph look like?" in one round-trip.
    //
    // Same invariants as the sibling list_routes test:
    //   - object schema with no required args
    //   - description mentions "routing graph" / "connectors" /
    //     "routes" so the LLM finds it for topology questions
    //   - risk tier stays ReadOnly
    //   - response shape is documented as `{ connectors, routes,
    //     excluded, excluded_note }` with the same deferred-status
    //     caveat as list_routes (RouteEngine plumbing for live
    //     exclusions is its own follow-up, out of scope here).
    let tools = get_tool_definitions();
    let def = tools
        .iter()
        .find(|t| t.name == "conductor_get_routing_graph")
        .expect("conductor_get_routing_graph tool definition must exist");

    assert_eq!(
        def.input_schema["type"].as_str(),
        Some("object"),
        "schema must declare `type: object`"
    );
    let required = def.input_schema["required"]
        .as_array()
        .expect("schema must include a `required` array");
    assert!(required.is_empty(), "no-arg tool: required must be empty");

    let desc = def.description.as_str();
    assert!(
        desc.contains("endpoint") && desc.contains("route"),
        "description must mention BOTH endpoint + route so the LLM \
             finds it for full-topology questions (ADR-035: [[endpoints]] \
             replaced [[connectors]]; payload key is `endpoints`); got: {}",
        desc
    );
    assert!(
        desc.contains("routing graph") || desc.contains("topology"),
        "description must mention 'routing graph' or 'topology' so \
             the LLM knows this is the combined view (not the two list_* \
             tools); got: {}",
        desc
    );

    assert_eq!(
        get_tool_risk_tier("conductor_get_routing_graph"),
        ToolRiskTier::ReadOnly,
        "conductor_get_routing_graph must be ReadOnly"
    );
}

#[test]
fn test_get_resolved_routing_graph_schema_is_a_no_arg_readonly_tool() {
    // ADR-031 Phase 1 — `conductor_get_resolved_routing_graph`
    // returns the RUNTIME-RESOLVED view from the daemon's
    // `connector_registry` + resolved routes. Distinct from
    // `conductor_get_routing_graph` (declared/config view) — this
    // is the canonical source the GUI should render against,
    // replacing GUI-side `projectRoutingGraph` re-implementation.
    // Same invariant set as the sibling routing-graph tools:
    //   - object schema with no required args (Phase 1 takes no
    //     params; Phase 2 will add an optional mode_index)
    //   - description mentions "resolved" + "routing graph" so the
    //     LLM picks this one for current-state questions
    //   - description distinguishes it from the declared view
    //   - risk tier stays ReadOnly
    let tools = get_tool_definitions();
    let def = tools
        .iter()
        .find(|t| t.name == "conductor_get_resolved_routing_graph")
        .expect("conductor_get_resolved_routing_graph tool definition must exist");

    assert_eq!(
        def.input_schema["type"].as_str(),
        Some("object"),
        "schema must declare `type: object`"
    );
    let required = def.input_schema["required"]
        .as_array()
        .expect("schema must include a `required` array");
    assert!(required.is_empty(), "Phase 1: no required args");

    let desc = def.description.as_str();
    assert!(
        desc.contains("RESOLVED") || desc.contains("resolved"),
        "description must mention 'resolved' to distinguish from the \
             declared-view tool `conductor_get_routing_graph`; got: {desc}"
    );
    assert!(
        desc.contains("routing graph") || desc.contains("topology"),
        "description must mention 'routing graph' or 'topology'; got: {desc}"
    );

    assert_eq!(
        get_tool_risk_tier("conductor_get_resolved_routing_graph"),
        ToolRiskTier::ReadOnly,
        "conductor_get_resolved_routing_graph must be ReadOnly"
    );
}

#[test]
fn test_get_connector_metrics_schema_is_a_no_arg_readonly_tool() {
    // ADR-031 § 6.2 P4 — `conductor_get_connector_metrics`
    // reads the runtime connector_registry for live per-connector
    // throughput. Same invariant set as the other routing-graph
    // tools: no-arg object schema, ReadOnly tier, description
    // mentions metrics/throughput so the LLM can find it when
    // asked "how busy is connector X?" or "is connector Y idle?".
    // Failure of any of these means a future edit silently broke
    // discoverability or quietly upgraded the tier — both are the
    // class of regression this test guards against.
    let tools = get_tool_definitions();
    let def = tools
        .iter()
        .find(|t| t.name == "conductor_get_connector_metrics")
        .expect("conductor_get_connector_metrics tool definition must exist");

    assert_eq!(
        def.input_schema["type"].as_str(),
        Some("object"),
        "schema must declare `type: object`"
    );
    let required = def.input_schema["required"]
        .as_array()
        .expect("schema must include a `required` array");
    assert!(required.is_empty(), "no-arg tool: required must be empty");

    let desc = def.description.as_str();
    // Discoverability — description must mention the four
    // payload fields so the LLM knows what's available without a
    // round-trip.
    for field in [
        "total_messages",
        "throughput_msgs_per_sec",
        "last_activity_ago_ms",
        "error_count",
    ] {
        assert!(
            desc.contains(field),
            "description must mention payload field `{}` so the LLM \
                 can plan queries without a round-trip; got: {}",
            field,
            desc
        );
    }

    assert_eq!(
        get_tool_risk_tier("conductor_get_connector_metrics"),
        ToolRiskTier::ReadOnly,
        "conductor_get_connector_metrics must be ReadOnly — it reads \
             runtime metrics, no mutations"
    );
}

#[cfg(feature = "mcp-write")]
#[test]
fn test_create_mapping_schema_does_not_advertise_raw_trigger() {
    // ADR-036 Phase 2: `Trigger::Raw` is removed and the parser
    // rejects it. The create-mapping schema must NOT advertise Raw as a
    // valid trigger type, or the LLM would author configs the daemon
    // rejects on load. Regression guard.
    let tools = get_tool_definitions();
    let create = tools
        .iter()
        .find(|t| t.name == "conductor_create_mapping")
        .expect("conductor_create_mapping tool definition must exist");
    let trigger_desc = create.input_schema["properties"]["trigger"]["description"]
        .as_str()
        .expect("trigger.description must be a string");
    assert!(
        !trigger_desc.contains("Raw"),
        "Trigger description must NOT advertise Raw (removed in ADR-036 Phase 2); got: {}",
        trigger_desc
    );
}

#[cfg(feature = "mcp-write")]
#[test]
fn test_create_mapping_schema_documents_midi_forward_action() {
    let tools = get_tool_definitions();
    let create = tools
        .iter()
        .find(|t| t.name == "conductor_create_mapping")
        .expect("conductor_create_mapping tool definition must exist");
    let action_desc = create.input_schema["properties"]["action"]["description"]
        .as_str()
        .expect("action.description must be a string");
    assert!(
        action_desc.contains("MidiForward"),
        "Action description must mention MidiForward (ADR-009 Gap 2 / ADR-030 pairing); got: {}",
        action_desc
    );
}

// Mirror the above two for conductor_update_mapping so that descriptions
// for both create and update tools are kept in sync.

#[cfg(feature = "mcp-write")]
#[test]
fn test_update_mapping_schema_does_not_advertise_raw_trigger() {
    // ADR-036 Phase 2 — mirror of the create-mapping guard.
    let tools = get_tool_definitions();
    let update = tools
        .iter()
        .find(|t| t.name == "conductor_update_mapping")
        .expect("conductor_update_mapping tool definition must exist");
    let trigger_desc = update.input_schema["properties"]["trigger"]["description"]
        .as_str()
        .expect("trigger.description must be a string");
    assert!(
        !trigger_desc.contains("Raw"),
        "Update trigger description must NOT advertise Raw (removed in ADR-036 Phase 2); got: {}",
        trigger_desc
    );
}

#[cfg(feature = "mcp-write")]
#[test]
fn test_update_mapping_schema_documents_midi_forward_action() {
    let tools = get_tool_definitions();
    let update = tools
        .iter()
        .find(|t| t.name == "conductor_update_mapping")
        .expect("conductor_update_mapping tool definition must exist");
    let action_desc = update.input_schema["properties"]["action"]["description"]
        .as_str()
        .expect("action.description must be a string");
    assert!(
        action_desc.contains("MidiForward"),
        "Update action description must mention MidiForward (ADR-009 Gap 2 / ADR-030 pairing); got: {}",
        action_desc
    );
}

// ProgramChange.pc is Option<u8>, but the description showed `pc: 0-127`
// as required.
// Pin the optional shape so future copy-paste doesn't regress.

#[cfg(feature = "mcp-write")]
#[test]
fn test_create_mapping_schema_marks_program_change_pc_optional() {
    let tools = get_tool_definitions();
    let create = tools
        .iter()
        .find(|t| t.name == "conductor_create_mapping")
        .expect("conductor_create_mapping tool definition must exist");
    let trigger_desc = create.input_schema["properties"]["trigger"]["description"]
        .as_str()
        .expect("trigger.description must be a string");
    // The optional marker is `pc?` — anything else (no `?`) implies required.
    assert!(
        trigger_desc.contains("ProgramChange"),
        "ProgramChange must appear in trigger description"
    );
    assert!(
        trigger_desc.contains("pc?:"),
        "ProgramChange.pc is Option<u8>; description must show `pc?:` not `pc:`. Got: {}",
        trigger_desc
    );
}

// VolumeControl description used `action: Up|Down|Mute|Set` but the actual
// ActionConfig variant uses `operation` and includes `Unmute`. Pin both
// pieces.

#[cfg(feature = "mcp-write")]
#[test]
fn test_create_mapping_schema_volume_control_uses_correct_field_name() {
    let tools = get_tool_definitions();
    let create = tools
        .iter()
        .find(|t| t.name == "conductor_create_mapping")
        .expect("conductor_create_mapping tool definition must exist");
    let action_desc = create.input_schema["properties"]["action"]["description"]
        .as_str()
        .expect("action.description must be a string");
    assert!(
        action_desc.contains("VolumeControl {operation"),
        "VolumeControl shape uses `operation` field, not `action`; got: {}",
        action_desc
    );
    assert!(
        action_desc.contains("Unmute"),
        "VolumeControl operation set must include Unmute; got: {}",
        action_desc
    );
}

#[cfg(feature = "mcp-write")]
#[test]
fn test_get_tool_definitions() {
    let tools = get_tool_definitions();
    assert_eq!(tools.len(), 54); // ADR-035 Phase 2: −5 legacy tools (create_binding/create_connector/create_device_identity/update_device_identity/delete_device_identity); conductor_create_endpoint is the unified replacement; +1 ADR-042 B.7 conductor_security_status; −1 conductor_list_connectors removed; +3 ADR-040 4c (conductor_set_mode/unlock_mode/mode_status)

    let names: Vec<_> = tools.iter().map(|t| t.name.as_str()).collect();
    // ADR-040 4c mode-lock tools
    assert!(names.contains(&"conductor_set_mode"));
    assert!(names.contains(&"conductor_unlock_mode"));
    assert!(names.contains(&"conductor_mode_status"));
    assert_eq!(
        get_tool_risk_tier("conductor_set_mode"),
        ToolRiskTier::Stateful
    );
    assert_eq!(
        get_tool_risk_tier("conductor_unlock_mode"),
        ToolRiskTier::Stateful
    );
    assert_eq!(
        get_tool_risk_tier("conductor_mode_status"),
        ToolRiskTier::ReadOnly
    );
    // ReadOnly tools (Phase 1B + Phase 5)
    assert!(names.contains(&"conductor_get_status"));
    assert!(names.contains(&"conductor_list_devices"));
    assert!(names.contains(&"conductor_get_config"));
    assert!(names.contains(&"conductor_list_mappings"));
    assert!(names.contains(&"conductor_get_mapping"));
    assert!(names.contains(&"conductor_list_device_bindings"));
    assert!(names.contains(&"conductor_list_discovered_ports"));
    assert!(names.contains(&"conductor_get_workspace_state"));
    assert!(names.contains(&"conductor_list_routes")); // ADR-031 P3
    assert!(names.contains(&"conductor_get_routing_graph")); // ADR-031 P3 (gap A)
    assert!(names.contains(&"conductor_get_resolved_routing_graph")); // ADR-031 Phase 1
    assert!(names.contains(&"conductor_get_connector_metrics")); // ADR-031 P4 (gap D)
    // ConfigChange tools (Phase 2 + Gap 3 + ADR-031 P3)
    assert!(names.contains(&"conductor_create_mapping"));
    assert!(names.contains(&"conductor_update_mapping"));
    assert!(names.contains(&"conductor_delete_mapping"));
    assert!(names.contains(&"conductor_create_endpoint")); // ADR-035 — unified I/O endpoint authoring
    assert!(names.contains(&"conductor_set_context_mapping"));
    assert!(names.contains(&"conductor_batch_changes")); // ADR-031 P3 (gap F)
    // ADR-035 Phase 2: legacy authoring tools removed (no longer registered)
    for removed in [
        "conductor_create_binding",
        "conductor_create_connector",
        "conductor_create_device_identity",
        "conductor_update_device_identity",
        "conductor_delete_device_identity",
    ] {
        assert!(
            !names.contains(&removed),
            "legacy tool '{removed}' must be removed in ADR-035 Phase 2"
        );
    }
    // Stateful tools (Phase 2 + Phase 5)
    assert!(names.contains(&"conductor_start_learn"));
    assert!(names.contains(&"conductor_stop_learn"));
    assert!(names.contains(&"conductor_set_device_enabled"));
    assert!(names.contains(&"conductor_scan_ports"));
    assert!(names.contains(&"conductor_switch_profile"));
    // ReadOnly
    assert!(names.contains(&"conductor_get_active_profile"));
    // HardwareIO tools (Phase 4)
    assert!(names.contains(&"conductor_send_sysex"));
    assert!(names.contains(&"conductor_device_reset"));
    // Fingerprinting (ADR-022)
    assert!(names.contains(&"conductor_suggest_binding"));
}

#[cfg(feature = "mcp-write")]
#[test]
fn test_batch_changes_schema_documents_route_ops() {
    // ADR-031 P3 § 5.4 — `conductor_batch_changes` is a ConfigChange-tier
    // batch tool. Per § 5.4 route mutations go through batch_changes
    // exclusively (no singleton route tools by design), so the schema MUST
    // advertise the 3 route op-types on top of the 5 mapping/mode ops.
    //
    // This test pins the enum entries in
    // `properties.operations.items.properties.type.enum`. A future op-type
    // added to the executor must also land here or the LLM sees a stale
    // schema and is told to call an op the published tool doesn't promise.
    //
    // ADR-035 Phase 2: the connector ops (create/update/delete_connector)
    // were removed alongside the legacy connector tools — endpoints are now
    // authored via the singleton conductor_create_endpoint tool. This test
    // also pins their ABSENCE so they can't silently reappear.
    let tools = get_tool_definitions();
    let def = tools
        .iter()
        .find(|t| t.name == "conductor_batch_changes")
        .expect("conductor_batch_changes tool definition must exist");

    // Tool tier — ConfigChange (mutating tool, Plan/Apply required)
    assert_eq!(
        get_tool_risk_tier("conductor_batch_changes"),
        ToolRiskTier::ConfigChange,
        "conductor_batch_changes must be ConfigChange — never auto-confirmed"
    );

    // Op-type enum — pin all 8 op-types
    let type_enum =
        def.input_schema["properties"]["operations"]["items"]["properties"]["type"]["enum"]
            .as_array()
            .expect("operations.items.properties.type.enum must be an array");
    let type_strs: Vec<&str> = type_enum.iter().filter_map(|v| v.as_str()).collect();
    for op in &[
        "create_mapping",
        "update_mapping",
        "delete_mapping",
        "create_mode",
        "delete_mode",
        "create_route",
        "update_route",
        "delete_route",
    ] {
        assert!(
            type_strs.contains(op),
            "operations.items.type.enum must contain '{}'; got: {:?}",
            op,
            type_strs
        );
    }
    // ADR-035 Phase 2 — connector ops removed; pin their absence so
    // they can't silently reappear in the published schema.
    for removed in &["create_connector", "update_connector", "delete_connector"] {
        assert!(
            !type_strs.contains(removed),
            "operations.items.type.enum must NOT contain removed connector op '{}'; got: {:?}",
            removed,
            type_strs
        );
    }
}

#[tokio::test]
async fn test_list_routes_returns_empty_array_for_default_config() {
    // Default test config has no routes — tool should return
    // `{ routes: [], excluded: [], excluded_note: "..." }`
    // rather than erroring or omitting the field.
    let executor = McpToolExecutor::new();
    let config = create_test_config();
    let result = executor
        .execute(
            "conductor_list_routes",
            None,
            None,
            None,
            Some(&config),
            None,
        )
        .await;
    assert!(
        result.is_error.is_none() || !result.is_error.unwrap_or(false),
        "list_routes must succeed on a default config with no routes"
    );
    let content = &result.content[0];
    if let ToolContent::Text { text } = content {
        let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
        assert!(parsed["routes"].is_array(), "routes must be an array");
        assert_eq!(
            parsed["routes"].as_array().unwrap().len(),
            0,
            "default config has no routes"
        );
        assert!(
            parsed["excluded"].is_array(),
            "excluded must be an array (always present so the LLM \
                 doesn't treat absence as 'everything is active')"
        );
    } else {
        panic!("Expected text content");
    }
}

#[tokio::test]
async fn test_list_routes_serializes_route_fields() {
    // Inject a route that exercises every optional field
    // (filter + transform + description + modes) so the test fails if
    // a future serde rename drops a field from the wire format. An earlier
    // version of this test used `modes: Vec::new()` and didn't assert
    // modes, letting a serde regression slip through. (Phase 3 removed
    // `phase`; all routes are post-mapping.)
    use conductor_core::config::types::RouteConfig;
    let executor = McpToolExecutor::new();
    let mut config = create_test_config();
    config.routes.push(RouteConfig {
        from: "mikro".to_string(),
        to: "absynth".to_string(),
        transform: None,
        filter: None,
        enabled: true,
        description: Some("test split route".to_string()),
        modes: vec!["Drums".to_string()],
    });
    let result = executor
        .execute(
            "conductor_list_routes",
            None,
            None,
            None,
            Some(&config),
            None,
        )
        .await;
    let content = &result.content[0];
    if let ToolContent::Text { text } = content {
        let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
        let routes = parsed["routes"].as_array().unwrap();
        assert_eq!(routes.len(), 1);
        let r = &routes[0];
        assert_eq!(r["from"], "mikro");
        assert_eq!(r["to"], "absynth");
        assert_eq!(r["enabled"], true);
        assert_eq!(r["description"], "test split route");
        assert_eq!(
            r["modes"],
            serde_json::json!(["Drums"]),
            "modes must round-trip through the tool's JSON output"
        );
        assert!(
            r.get("phase").is_none(),
            "Phase 3 removed `phase` from route output; got: {r}"
        );
    } else {
        panic!("Expected text content");
    }
}

#[tokio::test]
async fn test_list_routes_errors_when_config_missing() {
    // No config → user-friendly error rather than panic.
    let executor = McpToolExecutor::new();
    let result = executor
        .execute("conductor_list_routes", None, None, None, None, None)
        .await;
    assert_eq!(result.is_error, Some(true));
}

#[tokio::test]
async fn test_get_binding_health_includes_new_fields() {
    let executor = McpToolExecutor::new();
    let mut config = create_test_config();
    config
        .endpoints
        .push(conductor_core::config::types::EndpointConfig {
            alias: "pads".to_string(),
            direction: conductor_core::config::types::ConnectorDirection::Input,
            protocol: None,
            description: None,
            enabled: true,
            channels: vec![],
            kind: conductor_core::config::types::EndpointKind::Matcher {
                matchers: vec![conductor_core::identity::DeviceMatcher::NameContains {
                    value: "Mikro".to_string(),
                }],
                input_matchers: Vec::new(),
                output_matchers: Vec::new(),
                no_probe: false,
            },
        });
    let result = executor
        .execute(
            "conductor_get_binding_health",
            Some(json!({"alias": "pads"})),
            Some(json!({
                "device_bindings": [{
                    "device_id": "pads",
                    "connected": true,
                    "port_name": "Mikro Input",
                    "direction": "Bidirectional",
                    "output_auto_paired": true,
                }]
            })),
            None,
            Some(&config),
            None,
        )
        .await;
    assert!(result.is_error.is_none() || !result.is_error.unwrap_or(false));
    let content = &result.content[0];
    if let ToolContent::Text { text } = content {
        let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
        // New fields
        assert_eq!(parsed["interaction_pattern"], "bidirectional");
        assert_eq!(parsed["auto_paired"], true);
        assert!(parsed.get("last_event_timestamp").is_some());
    } else {
        panic!("Expected text content");
    }
}

#[tokio::test]
async fn test_get_binding_health_with_event_stats_timestamp() {
    let executor = McpToolExecutor::new();
    let mut config = create_test_config();
    config
        .endpoints
        .push(conductor_core::config::types::EndpointConfig {
            alias: "pads".to_string(),
            direction: conductor_core::config::types::ConnectorDirection::Input,
            protocol: None,
            description: None,
            enabled: true,
            channels: vec![],
            kind: conductor_core::config::types::EndpointKind::Matcher {
                matchers: vec![conductor_core::identity::DeviceMatcher::NameContains {
                    value: "Mikro".to_string(),
                }],
                input_matchers: Vec::new(),
                output_matchers: Vec::new(),
                no_probe: false,
            },
        });

    // Create event_stats with a known timestamp
    let stats_map = DashMap::new();
    let mut stats = EventStats::new();
    stats.record_note(60, 100);
    stats.touch(1700000000000); // known timestamp
    stats_map.insert("Mikro Input".to_string(), stats);

    let result = executor
        .execute(
            "conductor_get_binding_health",
            Some(json!({"alias": "pads"})),
            Some(json!({
                "device_bindings": [{
                    "device_id": "pads",
                    "connected": true,
                    "port_name": "Mikro Input",
                    "direction": "Bidirectional",
                    "output_auto_paired": true,
                }]
            })),
            None,
            Some(&config),
            Some(&stats_map),
        )
        .await;
    assert!(result.is_error.is_none() || !result.is_error.unwrap_or(false));
    let content = &result.content[0];
    if let ToolContent::Text { text } = content {
        let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(
            parsed["last_event_timestamp"], 1700000000000u64,
            "Should contain real timestamp from EventStats"
        );
    } else {
        panic!("Expected text content");
    }
}

#[tokio::test]
async fn test_suggest_binding_with_known_port() {
    let executor = McpToolExecutor::new();
    let result = executor
        .execute(
            "conductor_suggest_binding",
            Some(json!({"port_name": "Maschine Mikro MK3"})),
            None,
            None,
            None,
            None,
        )
        .await;
    assert!(result.is_error.is_none());
    let content = &result.content[0];
    if let ToolContent::Text { text } = content {
        let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["category"], "PadController");
        assert!(parsed["confidence"].as_f64().unwrap() <= 0.5);
        assert_eq!(parsed["method"], "port_name_heuristic");
        // P18: ranked alternatives
        let alts = parsed["alternatives"]
            .as_array()
            .expect("should have alternatives");
        assert!(!alts.is_empty(), "should have at least 1 alternative");
        let mut prev_conf = parsed["confidence"].as_f64().unwrap();
        for alt in alts {
            assert!(alt["category"].is_string());
            assert!(alt["suggested_alias"].is_string());
            let alt_conf = alt["confidence"].as_f64().unwrap();
            assert!(alt_conf > 0.0, "alternative confidence should be positive");
            assert!(
                alt_conf <= prev_conf,
                "alternatives should have decreasing confidence"
            );
            prev_conf = alt_conf;
        }
    } else {
        panic!("Expected text content");
    }
}

#[tokio::test]
async fn test_suggest_binding_missing_port_name() {
    let executor = McpToolExecutor::new();
    let result = executor
        .execute(
            "conductor_suggest_binding",
            Some(json!({})),
            None,
            None,
            None,
            None,
        )
        .await;
    assert!(result.is_error.unwrap_or(false));
}

#[tokio::test]
async fn test_suggest_binding_with_real_event_stats() {
    let executor = McpToolExecutor::new();
    let stats_map = DashMap::new();
    let mut stats = EventStats::new();
    // Simulate pad controller events (notes 36-51, multiple hits for confidence > 0.5)
    for _ in 0..4 {
        for note in 36..52 {
            stats.record_note(note, 100);
        }
    }
    stats_map.insert("Maschine Mikro MK3 MIDI".to_string(), stats);

    let result = executor
        .execute(
            "conductor_suggest_binding",
            Some(json!({"port_name": "Maschine Mikro MK3 MIDI"})),
            None,
            None,
            None,
            Some(&stats_map),
        )
        .await;
    assert!(result.is_error.is_none());
    let content = &result.content[0];
    if let ToolContent::Text { text } = content {
        let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["method"], "event_fingerprint");
        assert!(parsed["confidence"].as_f64().unwrap() > 0.5);
        assert!(parsed["event_count"].as_u64().unwrap() > 0);
    } else {
        panic!("Expected text content");
    }
}

#[tokio::test]
async fn test_suggest_binding_substring_key_match() {
    let executor = McpToolExecutor::new();
    let stats_map = DashMap::new();
    let mut stats = EventStats::new();
    for cc in 0..8 {
        stats.record_cc(cc, cc * 15);
    }
    stats_map.insert("nanoKONTROL2 MIDI".to_string(), stats);

    // Query with substring that matches
    let result = executor
        .execute(
            "conductor_suggest_binding",
            Some(json!({"port_name": "nanoKONTROL2"})),
            None,
            None,
            None,
            Some(&stats_map),
        )
        .await;
    assert!(result.is_error.is_none());
    let content = &result.content[0];
    if let ToolContent::Text { text } = content {
        let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["method"], "event_fingerprint");
    } else {
        panic!("Expected text content");
    }
}

#[tokio::test]
async fn test_suggest_binding_unknown_port_returns_empty_alternatives() {
    let executor = McpToolExecutor::new();
    // Unknown port name — primary confidence is 0, so alternatives should be empty
    let result = executor
        .execute(
            "conductor_suggest_binding",
            Some(json!({"port_name": "Totally Unknown Device XYZ"})),
            None,
            None,
            None,
            None,
        )
        .await;
    assert!(result.is_error.is_none());
    let content = &result.content[0];
    if let ToolContent::Text { text } = content {
        let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["category"], "Unknown");
        let alts = parsed["alternatives"]
            .as_array()
            .expect("should have alternatives array");
        assert!(
            alts.is_empty(),
            "Unknown port (0 confidence) should have no alternatives"
        );
    } else {
        panic!("Expected text content");
    }
}

#[tokio::test]
async fn test_suggest_binding_falls_back_with_empty_stats() {
    let executor = McpToolExecutor::new();
    let stats_map: DashMap<String, EventStats> = DashMap::new();

    let result = executor
        .execute(
            "conductor_suggest_binding",
            Some(json!({"port_name": "Maschine Mikro MK3"})),
            None,
            None,
            None,
            Some(&stats_map),
        )
        .await;
    assert!(result.is_error.is_none());
    let content = &result.content[0];
    if let ToolContent::Text { text } = content {
        let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["method"], "port_name_heuristic");
        assert!(parsed["confidence"].as_f64().unwrap() <= 0.5);
    } else {
        panic!("Expected text content");
    }
}

#[test]
fn test_get_status_returns_lifecycle_state() {
    let executor = McpToolExecutor::new();
    let status_data = json!({
        "lifecycle_state": "Running",
        "connected": true,
        "uptime_secs": 100
    });

    let result = executor.get_status(Some(status_data));
    assert!(result.is_error.is_none());

    let content = &result.content[0];
    if let ToolContent::Text { text } = content {
        assert!(text.contains("Running"));
        assert!(text.contains("connected"));
    } else {
        panic!("Expected text content");
    }
}

/// Test get_status fallback includes daemon_running
#[test]
fn test_get_status_fallback_includes_daemon_running() {
    let executor = McpToolExecutor::new();
    let result = executor.get_status(None);
    assert!(result.is_error.is_none());

    let content = &result.content[0];
    if let ToolContent::Text { text } = content {
        let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["daemon_running"], true);
        assert_eq!(parsed["connected"], false);
        assert_eq!(parsed["device_connected"], false);
    } else {
        panic!("Expected text content");
    }
}

/// Test get_status with daemon_running field
#[test]
fn test_get_status_includes_daemon_running() {
    let executor = McpToolExecutor::new();
    let status_data = json!({
        "lifecycle_state": "Running",
        "daemon_running": true,
        "connected": false,
        "device_connected": false,
        "uptime_secs": 100
    });

    let result = executor.get_status(Some(status_data));
    assert!(result.is_error.is_none());

    let content = &result.content[0];
    if let ToolContent::Text { text } = content {
        let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["daemon_running"], true);
        assert_eq!(parsed["connected"], false);
    } else {
        panic!("Expected text content");
    }
}

#[test]
fn test_list_devices_includes_midi_and_hid() {
    let executor = McpToolExecutor::new();
    let devices_data = json!({
        "midi_devices": [
            {"index": 0, "name": "Maschine Mikro"}
        ],
        "hid_devices": [
            {"index": 0, "name": "Xbox Controller"}
        ]
    });

    let result = executor.list_devices(Some(devices_data));
    assert!(result.is_error.is_none());

    let content = &result.content[0];
    if let ToolContent::Text { text } = content {
        assert!(text.contains("midi_devices"));
        assert!(text.contains("hid_devices"));
        assert!(text.contains("Maschine Mikro"));
        assert!(text.contains("Xbox Controller"));
    } else {
        panic!("Expected text content");
    }
}

#[tokio::test]
async fn test_get_config_returns_full_config() {
    let executor = McpToolExecutor::new();
    let config = create_test_config();

    let result = executor
        .execute(
            "conductor_get_config",
            None,
            None,
            None,
            Some(&config),
            None,
        )
        .await;

    assert!(result.is_error.is_none());

    let content = &result.content[0];
    if let ToolContent::Text { text } = content {
        // ADR-035: the legacy `[device]` block is gone; assert the mode
        // names round-trip through the serialized config instead.
        assert!(text.contains("Default"));
        assert!(text.contains("DJ"));
    } else {
        panic!("Expected text content");
    }
}

#[tokio::test]
async fn test_list_mappings_all_modes() {
    let executor = McpToolExecutor::new();
    let config = create_test_config();

    let result = executor
        .execute(
            "conductor_list_mappings",
            None,
            None,
            None,
            Some(&config),
            None,
        )
        .await;

    assert!(result.is_error.is_none());

    let content = &result.content[0];
    if let ToolContent::Text { text } = content {
        assert!(text.contains("Default"));
        assert!(text.contains("DJ"));
        assert!(text.contains("mapping_count"));
    } else {
        panic!("Expected text content");
    }
}

#[tokio::test]
async fn test_list_mappings_filtered_by_mode() {
    let executor = McpToolExecutor::new();
    let config = create_test_config();

    let result = executor
        .execute(
            "conductor_list_mappings",
            Some(json!({"mode": "Default"})),
            None,
            None,
            Some(&config),
            None,
        )
        .await;

    assert!(result.is_error.is_none());

    let content = &result.content[0];
    if let ToolContent::Text { text } = content {
        assert!(text.contains("Default"));
        // DJ mode should not be included
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["modes"].as_array().unwrap().len(), 1);
    } else {
        panic!("Expected text content");
    }
}

#[tokio::test]
async fn test_get_mapping_validates_mode_exists() {
    let executor = McpToolExecutor::new();
    let config = create_test_config();

    // Valid mode and index
    let result = executor
        .execute(
            "conductor_get_mapping",
            Some(json!({"mode": "Default", "index": 0})),
            None,
            None,
            Some(&config),
            None,
        )
        .await;

    assert!(result.is_error.is_none());

    let content = &result.content[0];
    if let ToolContent::Text { text } = content {
        assert!(text.contains("Copy"));
        assert!(text.contains("Note"));
    } else {
        panic!("Expected text content");
    }

    // Invalid mode
    let result = executor
        .execute(
            "conductor_get_mapping",
            Some(json!({"mode": "NonExistent", "index": 0})),
            None,
            None,
            Some(&config),
            None,
        )
        .await;

    assert!(result.is_error == Some(true));

    // Invalid index
    let result = executor
        .execute(
            "conductor_get_mapping",
            Some(json!({"mode": "Default", "index": 999})),
            None,
            None,
            Some(&config),
            None,
        )
        .await;

    assert!(result.is_error == Some(true));
}

#[cfg(feature = "mcp-write")]
#[test]
fn test_tool_risk_tiers() {
    assert_eq!(
        get_tool_risk_tier("conductor_get_status"),
        ToolRiskTier::ReadOnly
    );
    assert_eq!(
        get_tool_risk_tier("conductor_list_devices"),
        ToolRiskTier::ReadOnly
    );
    assert_eq!(
        get_tool_risk_tier("conductor_get_config"),
        ToolRiskTier::ReadOnly
    );
    assert_eq!(
        get_tool_risk_tier("conductor_list_mappings"),
        ToolRiskTier::ReadOnly
    );
    assert_eq!(
        get_tool_risk_tier("conductor_get_mapping"),
        ToolRiskTier::ReadOnly
    );

    // ConfigChange tools (Phase 2)
    assert_eq!(
        get_tool_risk_tier("conductor_create_mapping"),
        ToolRiskTier::ConfigChange
    );
    assert_eq!(
        get_tool_risk_tier("conductor_update_mapping"),
        ToolRiskTier::ConfigChange
    );
    assert_eq!(
        get_tool_risk_tier("conductor_delete_mapping"),
        ToolRiskTier::ConfigChange
    );

    // ConfigChange: unified endpoint authoring (ADR-035 — replaces the
    // removed create_binding / *_device_identity / create_connector tools)
    assert_eq!(
        get_tool_risk_tier("conductor_create_endpoint"),
        ToolRiskTier::ConfigChange
    );

    // Stateful tools (Phase 2)
    assert_eq!(
        get_tool_risk_tier("conductor_start_learn"),
        ToolRiskTier::Stateful
    );
    assert_eq!(
        get_tool_risk_tier("conductor_stop_learn"),
        ToolRiskTier::Stateful
    );
    // Deprecated aliases still resolve
    assert_eq!(
        get_tool_risk_tier("conductor_start_midi_learn"),
        ToolRiskTier::Stateful
    );
    assert_eq!(
        get_tool_risk_tier("conductor_stop_midi_learn"),
        ToolRiskTier::Stateful
    );

    // GUI-only profile tools
    assert_eq!(
        get_tool_risk_tier("conductor_list_profiles"),
        ToolRiskTier::ReadOnly
    );
    assert_eq!(
        get_tool_risk_tier("conductor_create_profile"),
        ToolRiskTier::Stateful
    );
    assert_eq!(
        get_tool_risk_tier("conductor_delete_profile"),
        ToolRiskTier::Stateful
    );

    // ArtifactRender tools
    assert_eq!(
        get_tool_risk_tier("conductor_render_artifact"),
        ToolRiskTier::ArtifactRender
    );
    assert_eq!(
        get_tool_risk_tier("conductor_dismiss_artifact"),
        ToolRiskTier::ArtifactRender
    );

    // HardwareIO tools (Phase 4)
    assert_eq!(
        get_tool_risk_tier("conductor_send_sysex"),
        ToolRiskTier::HardwareIO
    );
    assert_eq!(
        get_tool_risk_tier("conductor_device_reset"),
        ToolRiskTier::HardwareIO
    );

    // Multi-device tools (ADR-009 Phase 5)
    assert_eq!(
        get_tool_risk_tier("conductor_list_device_bindings"),
        ToolRiskTier::ReadOnly
    );
    assert_eq!(
        get_tool_risk_tier("conductor_set_device_enabled"),
        ToolRiskTier::Stateful
    );
    assert_eq!(
        get_tool_risk_tier("conductor_scan_ports"),
        ToolRiskTier::Stateful
    );
}

/// Test list_device_bindings with binding data
#[test]
fn test_list_device_bindings_with_data() {
    let executor = McpToolExecutor::new();
    let status_data = json!({
        "device_bindings": [
            {
                "device_id": "pads",
                "port_name": "Mikro MK3 MIDI",
                "connected": true,
                "enabled": true,
                "is_configured": true
            },
            {
                "device_id": "raw:Launchpad",
                "port_name": "Launchpad MIDI",
                "connected": true,
                "enabled": false,
                "is_configured": false
            }
        ]
    });

    let result = executor.list_device_bindings(Some(status_data));
    assert!(result.is_error.is_none());

    let content = &result.content[0];
    if let ToolContent::Text { text } = content {
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["multi_device_active"], true);
        assert_eq!(parsed["total_devices"], 2);
        assert_eq!(parsed["connected_count"], 2);
        assert_eq!(parsed["muted_count"], 1);
        assert_eq!(parsed["device_bindings"].as_array().unwrap().len(), 2);
    } else {
        panic!("Expected text content");
    }
}

/// Test list_device_bindings with empty data
#[test]
fn test_list_device_bindings_empty() {
    let executor = McpToolExecutor::new();
    let result = executor.list_device_bindings(None);
    assert!(result.is_error.is_none());

    let content = &result.content[0];
    if let ToolContent::Text { text } = content {
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["multi_device_active"], false);
        assert_eq!(parsed["total_devices"], 0);
        assert_eq!(parsed["connected_count"], 0);
        assert_eq!(parsed["muted_count"], 0);
    } else {
        panic!("Expected text content");
    }
}

/// Test list_device_bindings summary counts
#[test]
fn test_list_device_bindings_summary_counts() {
    let executor = McpToolExecutor::new();
    let status_data = json!({
        "device_bindings": [
            { "device_id": "a", "connected": true, "enabled": true },
            { "device_id": "b", "connected": false, "enabled": true },
            { "device_id": "c", "connected": true, "enabled": false }
        ]
    });

    let result = executor.list_device_bindings(Some(status_data));
    let content = &result.content[0];
    if let ToolContent::Text { text } = content {
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["total_devices"], 3);
        assert_eq!(parsed["connected_count"], 2);
        assert_eq!(parsed["muted_count"], 1);
    } else {
        panic!("Expected text content");
    }
}

// ── ADR-022 Phase 1C: list_discovered_ports ──

#[test]
fn test_list_discovered_ports_with_data() {
    let executor = McpToolExecutor::new();
    let devices_data = json!({
        "midi_devices": [
            { "port_name": "Maschine Mikro MK3 MIDI", "port_index": 0, "connected": true },
            { "port_name": "nanoKONTROL2 MIDI", "port_index": 1, "connected": true }
        ],
        "hid_devices": [
            { "name": "Xbox Controller", "id": "abc123" }
        ]
    });
    let status_data = json!({
        "device_bindings": [
            { "device_id": "pads", "port_name": "Maschine Mikro MK3 MIDI", "connected": true }
        ]
    });

    let result = executor.list_discovered_ports(Some(devices_data), Some(status_data), None);
    let content = &result.content[0];
    if let ToolContent::Text { text } = content {
        let parsed: Value = serde_json::from_str(text).unwrap();
        let ports = parsed["ports"].as_array().unwrap();

        // 2 MIDI input + 1 HID = 3 ports
        assert_eq!(ports.len(), 3);

        // First port should be bound
        assert_eq!(ports[0]["name"], "Maschine Mikro MK3 MIDI");
        assert_eq!(ports[0]["protocol"], "midi");
        assert_eq!(ports[0]["direction"], "Input");
        assert_eq!(ports[0]["binding"], "pads");

        // Second port should be unbound
        assert_eq!(ports[1]["name"], "nanoKONTROL2 MIDI");
        assert!(ports[1]["binding"].is_null());

        // HID port
        assert_eq!(ports[2]["protocol"], "hid");
        assert_eq!(ports[2]["name"], "Xbox Controller");

        // Summary
        assert_eq!(parsed["summary"]["total"], 3);
        assert_eq!(parsed["summary"]["bound"], 1);
        assert_eq!(parsed["summary"]["unbound"], 2);
    } else {
        panic!("Expected text content");
    }
}

#[test]
fn test_list_discovered_ports_empty() {
    let executor = McpToolExecutor::new();
    let result = executor.list_discovered_ports(None, None, None);
    let content = &result.content[0];
    if let ToolContent::Text { text } = content {
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["ports"].as_array().unwrap().len(), 0);
        assert_eq!(parsed["summary"]["total"], 0);
    } else {
        panic!("Expected text content");
    }
}

#[test]
fn test_list_discovered_ports_risk_tier() {
    assert_eq!(
        get_tool_risk_tier("conductor_list_discovered_ports"),
        ToolRiskTier::ReadOnly
    );
}

/// Test get_status includes device_bindings
#[test]
fn test_get_status_includes_device_bindings() {
    use crate::daemon::types::{DaemonState, DevicePortStatus, DeviceStatus, LifecycleState};

    let state = DaemonState {
        lifecycle_state: Some(LifecycleState::Running),
        device_status: Some(DeviceStatus {
            connected: true,
            devices: vec![DevicePortStatus {
                device_id: "pads".to_string(),
                port_name: "Mikro".to_string(),
                port_index: 0,
                connected: true,
                enabled: true,
                last_event_at: None,
                is_configured: true,
                direction: conductor_core::config::DeviceDirection::Input,
                output_port_name: None,
                output_connected: false,
                output_auto_paired: false,
                protocol: "midi".to_string(),
            }],
            ..Default::default()
        }),
        ..Default::default()
    };

    let status_json = state.to_status_json();
    let bindings = status_json["device_bindings"].as_array().unwrap();
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0]["device_id"], "pads");
    assert_eq!(bindings[0]["is_configured"], true);
}

/// Test list_devices includes device_bindings
/// Fixed to use is_configured field instead of "raw:" prefix (D19)
#[test]
fn test_list_devices_includes_device_bindings() {
    use crate::daemon::types::{DaemonState, DevicePortStatus, DeviceStatus, MidiDeviceInfo};

    let state = DaemonState {
        device_status: Some(DeviceStatus {
            connected: true,
            devices: vec![DevicePortStatus {
                device_id: "Port 1".to_string(),
                port_name: "Port 1".to_string(),
                port_index: 0,
                connected: true,
                enabled: true,
                last_event_at: None,
                is_configured: false,
                direction: conductor_core::config::DeviceDirection::Input,
                output_port_name: None,
                output_connected: false,
                output_auto_paired: false,
                protocol: "midi".to_string(),
            }],
            ..Default::default()
        }),
        ..Default::default()
    };

    let devices_json = state.to_devices_json(vec![MidiDeviceInfo {
        port_index: 0,
        port_name: "Port 1".to_string(),
        manufacturer: None,
        connected: true,
    }]);

    let bindings = devices_json["device_bindings"].as_array().unwrap();
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0]["device_id"], "Port 1");
    assert_eq!(bindings[0]["is_configured"], false);
}

// Tests for conductor_switch_mode validation path.
// These test the LLM executor's read-only validation logic only.
// End-to-end MCP mode switching (via mcp.rs → DispatchOutcome) is covered
// by mode_management_integration_test.
#[tokio::test]
async fn test_switch_mode_validates_mode_exists() {
    let executor = McpToolExecutor::new();
    let config = create_test_config();

    let result = executor
        .execute(
            "conductor_switch_mode",
            Some(json!({"mode": "Default"})),
            None,
            None,
            Some(&config),
            None,
        )
        .await;

    assert!(result.is_error.is_none());

    let content = &result.content[0];
    if let ToolContent::Text { text } = content {
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["mode_name"], "Default");
        assert_eq!(parsed["mode_index"], 0);
        assert_eq!(parsed["status"], "validated");
    } else {
        panic!("Expected text content");
    }
}

#[tokio::test]
async fn test_switch_mode_missing_arguments_error() {
    let executor = McpToolExecutor::new();
    let config = create_test_config();

    let result = executor
        .execute(
            "conductor_switch_mode",
            None,
            None,
            None,
            Some(&config),
            None,
        )
        .await;

    assert!(result.is_error == Some(true));
}

#[tokio::test]
async fn test_switch_mode_unknown_mode_error() {
    let executor = McpToolExecutor::new();
    let config = create_test_config();

    let result = executor
        .execute(
            "conductor_switch_mode",
            Some(json!({"mode": "NonExistent"})),
            None,
            None,
            Some(&config),
            None,
        )
        .await;

    assert!(result.is_error == Some(true));

    let content = &result.content[0];
    if let ToolContent::Text { text } = content {
        assert!(text.contains("Mode not found: NonExistent"));
        assert!(text.contains("Available modes: Default, DJ"));
    } else {
        panic!("Expected text content");
    }
}

// Topology summary tests (ADR-016)

#[test]
fn test_topology_summary_tool_registered() {
    let tools = get_tool_definitions();
    let names: Vec<_> = tools.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"conductor_get_topology_summary"));
}

#[test]
fn test_topology_summary_risk_tier() {
    assert_eq!(
        get_tool_risk_tier("conductor_get_topology_summary"),
        ToolRiskTier::ReadOnly
    );
}

#[test]
fn test_topology_summary_no_config() {
    let executor = McpToolExecutor::new();
    let result = executor.get_topology_summary(None, None);
    assert!(result.is_error.is_none());

    let content = &result.content[0];
    if let ToolContent::Text { text } = content {
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["mappings"]["total"], 0);
        assert!(parsed["message"].as_str().is_some());
    } else {
        panic!("Expected text content");
    }
}

#[test]
fn test_topology_summary_basic_config() {
    let executor = McpToolExecutor::new();
    let config = create_test_config();

    let result = executor.get_topology_summary(None, Some(&config));
    assert!(result.is_error.is_none());

    let content = &result.content[0];
    if let ToolContent::Text { text } = content {
        let parsed: Value = serde_json::from_str(text).unwrap();
        // 2 mappings in Default mode
        assert_eq!(parsed["mappings"]["total"], 2);
        assert_eq!(parsed["mappings"]["simple"], 2);
        assert_eq!(parsed["mappings"]["fan_out"], 0);
        assert!(parsed["routing"].as_array().unwrap().is_empty());
        assert!(parsed["warnings"].as_array().unwrap().is_empty());
    } else {
        panic!("Expected text content");
    }
}

#[test]
fn test_topology_summary_with_routing() {
    let executor = McpToolExecutor::new();
    let config = Config {
        mcp: Default::default(),
        per_app_modes: None,
        config_meta: Default::default(),
        security: Default::default(),
        endpoints: vec![],
        modes: vec![Mode {
            name: "Default".to_string(),
            color: None,
            mappings: vec![
                Mapping {
                    trigger: Trigger::CC {
                        cc: 1,
                        value_min: None,
                        channel: Some(0),
                        device: Some("pads".to_string()),
                    },
                    action: ActionConfig::SendMidi {
                        port: "Synth Output".to_string(),
                        message_type: "CC".to_string(),
                        channel: 0,
                        note: None,
                        velocity: None,
                        controller: Some(1),
                        value: Some(64),
                        program: None,
                        pitch: None,
                        pressure: None,
                    },
                    description: None,
                    let_through: false,
                },
                Mapping {
                    trigger: Trigger::Note {
                        note: 36,
                        velocity_min: Some(1),
                        channel: None,
                        device: None,
                    },
                    action: ActionConfig::Sequence {
                        actions: vec![
                            ActionConfig::Keystroke {
                                keys: "a".to_string(),
                                modifiers: vec![],
                            },
                            ActionConfig::Keystroke {
                                keys: "b".to_string(),
                                modifiers: vec![],
                            },
                        ],
                    },
                    description: None,
                    let_through: false,
                },
            ],
        }],
        global_mappings: vec![],
        logging: None,
        advanced_settings: Default::default(),
        last_selected_mode: None,
        default_mode: None,
        led: None,
        event_console: None,
        routes: vec![],
    };

    let result = executor.get_topology_summary(None, Some(&config));
    let content = &result.content[0];
    if let ToolContent::Text { text } = content {
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["mappings"]["total"], 2);
        assert_eq!(parsed["mappings"]["simple"], 1); // SendMidi
        assert_eq!(parsed["mappings"]["sequences"], 1);
        assert_eq!(parsed["mappings"]["fan_out"], 1);
        // SendMidi creates a routing entry
        let routing = parsed["routing"].as_array().unwrap();
        assert_eq!(routing.len(), 1);
        assert_eq!(routing[0]["to_port"], "Synth Output");
        assert_eq!(routing[0]["action_type"], "SendMidi");
        assert_eq!(routing[0]["from_device"], "pads");
    } else {
        panic!("Expected text content");
    }
}

#[test]
fn test_topology_summary_with_devices_and_warnings() {
    let executor = McpToolExecutor::new();
    let config = Config {
        mcp: Default::default(),
        per_app_modes: None,
        config_meta: Default::default(),
        security: Default::default(),
        endpoints: vec![],
        modes: vec![Mode {
            name: "Default".to_string(),
            color: None,
            mappings: vec![Mapping {
                trigger: Trigger::CC {
                    cc: 1,
                    value_min: None,
                    channel: None,
                    device: None,
                },
                action: ActionConfig::SendMidi {
                    port: "Mikro MK3 MIDI".to_string(),
                    message_type: "CC".to_string(),
                    channel: 0,
                    note: None,
                    velocity: None,
                    controller: Some(1),
                    value: Some(64),
                    program: None,
                    pitch: None,
                    pressure: None,
                },
                description: None,
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
    };

    let status_data = json!({
        "device_bindings": [
            {
                "device_id": "pads",
                "alias": "Mikro",
                "port_name": "Mikro MK3 MIDI",
                "connected": true,
                "enabled": true,
            }
        ]
    });

    let result = executor.get_topology_summary(Some(status_data), Some(&config));
    let content = &result.content[0];
    if let ToolContent::Text { text } = content {
        let parsed: Value = serde_json::from_str(text).unwrap();
        // Device summary
        let devices = parsed["devices"].as_array().unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0]["device_id"], "pads");
        assert_eq!(devices[0]["connected"], true);
        // Warning: CC→Mikro which has CC trigger = red (confirmed) loop
        let warnings = parsed["warnings"].as_array().unwrap();
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0]["severity"], "red");
        assert!(
            warnings[0]["description"]
                .as_str()
                .unwrap()
                .contains("Feedback loop")
        );
    } else {
        panic!("Expected text content");
    }
}

#[test]
fn test_topology_summary_amber_warning() {
    let executor = McpToolExecutor::new();
    // Send PitchBend to a port that only has Note triggers → amber (not confirmed)
    let config = Config {
        mcp: Default::default(),
        per_app_modes: None,
        config_meta: Default::default(),
        security: Default::default(),
        endpoints: vec![],
        modes: vec![Mode {
            name: "Default".to_string(),
            color: None,
            mappings: vec![Mapping {
                trigger: Trigger::Note {
                    note: 36,
                    velocity_min: Some(1),
                    channel: None,
                    device: None,
                },
                action: ActionConfig::SendMidi {
                    port: "Mikro MK3 MIDI".to_string(),
                    message_type: "PitchBend".to_string(),
                    channel: 0,
                    note: None,
                    velocity: None,
                    controller: None,
                    value: None,
                    program: None,
                    pitch: Some(8192),
                    pressure: None,
                },
                description: None,
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
    };

    let status_data = json!({
        "device_bindings": [
            {
                "device_id": "pads",
                "port_name": "Mikro MK3 MIDI",
                "connected": true,
                "enabled": true,
            }
        ]
    });

    let result = executor.get_topology_summary(Some(status_data), Some(&config));
    let content = &result.content[0];
    if let ToolContent::Text { text } = content {
        let parsed: Value = serde_json::from_str(text).unwrap();
        let warnings = parsed["warnings"].as_array().unwrap();
        assert_eq!(warnings.len(), 1);
        // PitchBend sent but only Note trigger exists → amber
        assert_eq!(warnings[0]["severity"], "amber");
    } else {
        panic!("Expected text content");
    }
}

#[tokio::test]
async fn test_topology_summary_via_execute() {
    let executor = McpToolExecutor::new();
    let config = create_test_config();

    let result = executor
        .execute(
            "conductor_get_topology_summary",
            None,
            None,
            None,
            Some(&config),
            None,
        )
        .await;

    assert!(result.is_error.is_none());
    let content = &result.content[0];
    if let ToolContent::Text { text } = content {
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["mappings"]["total"], 2);
    } else {
        panic!("Expected text content");
    }
}

#[tokio::test]
async fn test_gui_only_profile_tools_return_clear_error() {
    let executor = McpToolExecutor::new();
    for tool_name in [
        "conductor_list_profiles",
        "conductor_create_profile",
        "conductor_delete_profile",
    ] {
        let result = executor
            .execute(tool_name, None, None, None, None, None)
            .await;
        assert_eq!(
            result.is_error,
            Some(true),
            "{tool_name} should return error"
        );
        let content = &result.content[0];
        if let ToolContent::Text { text } = content {
            assert!(
                text.contains("managed by the GUI"),
                "{tool_name} should return GUI-only error, got: {text}"
            );
        } else {
            panic!("Expected text content for {tool_name}");
        }
    }
}

// ── PolyAftertouch routing-helper symmetry ──────────────────────────────
// classify_trigger emits "PolyAftertouch" but the sibling helpers used for
// MCP topology / SendMidi message-type lookup did not recognise it, so
// MidiForward routing entries and reverse string lookups returned None.

#[test]
fn midi_message_to_trigger_type_recognises_poly_aftertouch() {
    assert_eq!(
        midi_message_to_trigger_type("polyaftertouch"),
        Some("PolyAftertouch")
    );
    assert_eq!(
        midi_message_to_trigger_type("poly_aftertouch"),
        Some("PolyAftertouch")
    );
    assert_eq!(midi_message_to_trigger_type("pat"), Some("PolyAftertouch"));
}

#[test]
fn infer_message_type_from_trigger_returns_poly_aftertouch() {
    let trigger = Trigger::PolyAftertouch {
        note: 60,
        pressure_min: None,
        channel: None,
        device: None,
    };
    assert_eq!(
        infer_message_type_from_trigger(&trigger),
        Some("PolyAftertouch")
    );
}
