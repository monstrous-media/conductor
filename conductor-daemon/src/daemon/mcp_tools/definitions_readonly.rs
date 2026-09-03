// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ReadOnly-tier (inspection) MCP tool definitions.
//! Split out of `mcp_tools.rs` (file exceeded the review window).

use super::super::mcp_types::ToolDefinition;
use serde_json::json;

pub(super) fn readonly_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "conductor_get_status".to_string(),
            description: "Get Conductor daemon status including lifecycle state, device connection, and uptime".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        ToolDefinition {
            name: "conductor_list_devices".to_string(),
            description: "List available MIDI and HID (gamepad) devices that can be connected".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        ToolDefinition {
            name: "conductor_get_config".to_string(),
            description: "Get the current Conductor configuration including device settings, modes, and mappings".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        ToolDefinition {
            name: "conductor_list_mappings".to_string(),
            description: "List all mappings in a specific mode or all modes".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "mode": {
                        "type": "string",
                        "description": "Mode name to filter mappings (optional, returns all modes if not specified)"
                    }
                },
                "required": []
            }),
        },
        ToolDefinition {
            name: "conductor_get_mapping".to_string(),
            description: "Get a specific mapping by mode name and index".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "mode": {
                        "type": "string",
                        "description": "Name of the mode containing the mapping"
                    },
                    "index": {
                        "type": "integer",
                        "description": "Zero-based index of the mapping within the mode"
                    }
                },
                "required": ["mode", "index"]
            }),
        },
        ToolDefinition {
            name: "conductor_validate_config".to_string(),
            description: "Validate the current config against MIDI/HID/OSC protocol schemas and report coverage metrics".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        // ADR-031 § 3.7 — signal routing graph (Phase 1B + Phase 3).
        // (`conductor_list_connectors` was removed: its runtime
        // connectors + `connected`/`bound_port` view is a strict subset of
        // `conductor_get_resolved_routing_graph` below; the static view is in
        // `conductor_get_routing_graph`.)
        ToolDefinition {
            name: "conductor_list_routes".to_string(),
            description: "List all signal-routing routes from the active config (ADR-031 § 3.4). Returns the declared `[[routes]]` entries — `from`, `to`, `enabled`, optional `filter` (note_range/cc_range/channels/message_types/osc_address_prefix), optional `transform` (Midi remap or cross-protocol). Routes are mode-independent and fan-out by default; evaluation priority is per-event mappings > routes — a more specific layer always shadows a broader one. Pair with `conductor_get_routing_graph` (or `conductor_get_resolved_routing_graph`) for the endpoints side of the full routing graph; for which routes are *active* at runtime (some may be excluded for cross-protocol-not-yet-supported or OSC-filter-on-MIDI-route reasons), see the `excluded` array in the response.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        // ADR-031 P3 (gap A) — combined topology view. Spec § 3.7 row 3
        // promised this tool in Phase 1; it was deferred pending
        // RouteEngine on SharedDaemonStateRefs. This tool ships the
        // config-derived view (connectors + routes) so the LLM can
        // answer "what does my routing graph look like?" in one
        // round-trip; the runtime `excluded` array and per-connector
        // bound-port status remain deferred (same caveat note as
        // `conductor_list_routes`'s `excluded_note` field).
        ToolDefinition {
            name: "conductor_get_routing_graph".to_string(),
            description: "Return the full signal-routing graph (ADR-031 § 3.4) in one call: all declared [[endpoints]] + all declared [[routes]] from the active config, so the LLM can answer 'show me my routing topology' in one call instead of also calling `conductor_list_routes`. Response shape: { endpoints: [...], routes: [...], excluded: [], excluded_note: '...' }. The `excluded` array surfaces routes excluded at runtime (cross-protocol-not-yet-supported, OSC-filter-on-MIDI-route, etc.); currently always empty pending RouteEngine wiring on SharedDaemonStateRefs (see `excluded_note`). Routes are mode-independent and fan-out by default; evaluation priority is per-event mappings > routes.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        // ADR-031 §3.4 Phase 1 — runtime-resolved routing graph.
        // `conductor_get_routing_graph` (above) returns the DECLARED
        // (config) view; this tool returns the RESOLVED view from the
        // runtime `connector_registry` (binding state, lowering applied)
        // and resolves each route's `from`/`to` against that registry
        // so `from_missing`/`to_missing` surface validator-bypassed
        // routes. The intended canonical source for the routing-graph
        // GUI view (extending ADR-031 line 855's resolver-of-record
        // principle from action execution to graph rendering).
        ToolDefinition {
            name: "conductor_get_resolved_routing_graph".to_string(),
            description: "Return the RUNTIME-RESOLVED routing graph (ADR-031 §3.4) from the daemon's `connector_registry` + `config.routes`. Distinct from `conductor_get_routing_graph` which returns the DECLARED (config) view: this tool reflects the live state — bindings lowered into input connectors, explicit `[[connectors]]` folded in, each route's `from`/`to` resolved against the registry so `from_missing`/`to_missing` surface validator-bypassed routes. Response: { connectors: [{ alias, direction, protocol, enabled, connected, bound_port, description, channels }], routes: [{ key, from_alias, to_alias, from_missing, to_missing, enabled, filter, transform, description }] }. The canonical source for routing-graph rendering (#1598) — replaces GUI-side `projectRoutingGraph` / `bindingToConnector` re-implementations.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        // ADR-036 D5 — route-match introspection.
        // Evaluates a hypothetical MIDI event against the live compiled
        // RouteEngine (pre- AND post-mapping phases) and explains, per
        // candidate route, whether it fired or was skipped + why. Reads
        // `SharedDaemonStateRefs.route_engine` via the executor.
        ToolDefinition {
            name: "conductor_explain_route_match".to_string(),
            description: "Explain why each [[routes]] entry fires or is skipped for a hypothetical MIDI event in a given mode (ADR-036 D5). Evaluates the event against the LIVE compiled RouteEngine (all routes are post-mapping), returning one entry per candidate route whose `from` matches the event's source `device`: { to_alias, modes, fired, skip_reason }. `skip_reason` (when not fired) is one of: `mode_ineligible` (route is mode-scoped and the active mode isn't listed — includes active_mode + route_modes), `filter_mismatch` (the route's filter rejected the event — includes the failing `dimension`: channel | message_type | note_range | cc_range | system_message | empty), or `transform_produced_no_output`. Use it to answer 'why didn't my route fire?' without sending real MIDI. The `fired` set exactly equals what the event pump would dispatch. An unknown source device returns an empty list (no routes from it). Input: `event` { device: source binding alias, type: note_on|note_off|cc|program_change|aftertouch|poly_aftertouch|pitch_bend, channel: 0-15, data1: 0-127 (note/cc/program), data2?: 0-127 (velocity/value) } and `active_mode`.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "event": {
                        "type": "object",
                        "description": "The hypothetical MIDI event to evaluate.",
                        "properties": {
                            "device": { "type": "string", "description": "Source binding alias the event arrives on (matched against each route's `from`)." },
                            "type": {
                                "type": "string",
                                "enum": ["note_on", "note_off", "cc", "program_change", "aftertouch", "poly_aftertouch", "pitch_bend"],
                                "description": "MIDI message type."
                            },
                            "channel": { "type": "integer", "minimum": 0, "maximum": 15, "description": "0-indexed MIDI channel (0-15)." },
                            "data1": { "type": "integer", "minimum": 0, "maximum": 127, "description": "First data byte: note number / CC number / program number." },
                            "data2": { "type": "integer", "minimum": 0, "maximum": 127, "description": "Second data byte: velocity / value. Omit for program_change / aftertouch." }
                        },
                        "required": ["device", "type", "channel", "data1"]
                    },
                    "active_mode": { "type": "string", "description": "The active mode name to evaluate mode scope against." }
                },
                "required": ["event", "active_mode"]
            }),
        },
        // ADR-036 §8 — bounded dispatch-trace ring.
        // Reads `SharedDaemonStateRefs.dispatch_trace` via the executor.
        ToolDefinition {
            name: "conductor_get_dispatch_trace".to_string(),
            description: "Return the most recent route-dispatch decisions from the daemon's in-memory ring buffer (ADR-036 §8). Each entry is one event that was actually routed to one or more destinations: { timestamp_ms, device_id, active_mode, event (human summary, e.g. 'NoteOn ch0 note36 vel100'), destinations (connector aliases) }. Newest entry last. Only events that routed somewhere are recorded (the buffer holds up to 1000; oldest evicted). Use it to answer 'what did the router just do?' or to confirm a route fired after sending MIDI. Cleared on daemon restart. Input: optional `last` (1-256, default 32) — the number of most-recent entries to return.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "last": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 256,
                        "description": "Number of most-recent dispatch entries to return (capped at 256; default 32)."
                    }
                },
                "required": []
            }),
        },
        // ADR-031 P4 § 6.2 — live per-connector
        // throughput metrics. Reads from the runtime
        // `connector_registry` (NOT config) so the LLM sees actual
        // activity — total messages forwarded, current sliding-window
        // msg/s rate, last activity timestamp, error count. Dispatched
        // via the shared_state special-case path in
        // `mcp.rs::handle_tools_call` because `McpToolExecutor` is
        // stateless and can't reach the live registry.
        ToolDefinition {
            name: "conductor_get_connector_metrics".to_string(),
            description: "Return per-connector live activity metrics from the runtime signal-routing graph (ADR-031 § 6.1). For each connector: `total_messages` (cumulative forward count since daemon start), `throughput_msgs_per_sec` (10-second sliding window rate), `last_activity_ago_ms` (millis since last forward, or null if never active), `error_count` (cumulative dispatch failures — currently the action-executor queue-full back-pressure path; downstream send failures are a follow-up). All zeros for a fresh daemon with no forwarding activity. Use this to answer 'how much is connector X handling?' or 'is connector Y idle?' — distinct from `conductor_get_routing_graph` which returns the static config view.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        ToolDefinition {
            name: "conductor_mode_status".to_string(),
            description: "Report the active mode and lock state (mode, index, locked, lock origin).".to_string(),
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        // ReadOnly: Signal topology summary (ADR-016)
        ToolDefinition {
            name: "conductor_get_topology_summary".to_string(),
            description: "Returns a structured summary of the current signal routing topology: devices, mappings, cross-device routing, detected feedback loops, and warnings. Use this for detailed signal flow analysis.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        // ReadOnly: Get active profile (Phase 1)
        ToolDefinition {
            name: "conductor_get_active_profile".to_string(),
            description: "Get the currently active profile name and config path. Returns null if no profile is active (using default config).".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        // Binding health (ADR-022 Phase 3C)
        ToolDefinition {
            name: "conductor_get_binding_health".to_string(),
            description: "Get detailed health diagnostic for a specific binding. Returns port connection state, issues, and mapping count.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "alias": { "type": "string", "description": "Binding alias to check" }
                },
                "required": ["alias"]
            }),
        },
        // Discovery tools (ADR-022 Phase 1C)
        ToolDefinition {
            name: "conductor_list_discovered_ports".to_string(),
            description: "List all ports visible to Conductor across all protocols, with binding status. Currently returns MIDI receive ports and HID devices; MIDI send ports not yet included. Indicates which ports are bound to a config alias.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        // Workspace state (ADR-017 Phase 2B)
        ToolDefinition {
            name: "conductor_get_workspace_state".to_string(),
            description: "Get the current workspace view and editing context. Returns what the user is currently viewing or editing in the GUI.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        // Multi-device tools (ADR-009 Phase 5)
        ToolDefinition {
            name: "conductor_list_device_bindings".to_string(),
            description: "List multi-device binding status including device IDs, port names, connection state, and mute state".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        // Plugin management
        ToolDefinition {
            name: "conductor_list_plugins".to_string(),
            description: "List available and loaded plugins".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        ToolDefinition {
            name: "conductor_plugin_info".to_string(),
            description: "Get plugin metadata including name, version, capabilities, and status"
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Plugin name"
                    }
                },
                "required": ["name"]
            }),
        },
        // Event fingerprinting (ADR-022 Phase 5D)
        ToolDefinition {
            name: "conductor_suggest_binding".to_string(),
            description: "Suggest a binding configuration for a port. Uses live event fingerprinting when events have been observed (method: event_fingerprint), falls back to port-name heuristics (method: port_name_heuristic, confidence capped at 0.5). Returns primary suggestion (category, confidence, alias, protocol, reasoning) plus ranked alternatives array with lower-confidence fallback categories.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "port_name": {
                        "type": "string",
                        "description": "Name of the port to analyze (from conductor_list_discovered_ports)"
                    }
                },
                "required": ["port_name"]
            }),
        },
        // SysEx identity cache reads (ADR-026 Phase 2 ReadOnly peers)
        ToolDefinition {
            name: "conductor_get_device_identity".to_string(),
            description: "Return the cached SysEx identity + confidence label for a MIDI input port if it has been probed during this daemon session. ReadOnly — does not initiate a new probe; pair with conductor_probe_device_identity to populate the cache. Response shape: `{ port_name, identity, confidence }` where `identity` is null AND `confidence` is null when the port has not been probed. `confidence` is `\"direct_paired_port\"` for the standard single-cable case (auto-promotion safe) or `\"shared_route\"` when a thru-box / merger may be in play (caller should confirm before binding).".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "port_name": {
                        "type": "string",
                        "description": "MIDI input port name as reported by the daemon (NOT a device alias). Same semantics as conductor_probe_device_identity."
                    }
                },
                "required": ["port_name"]
            }),
        },
        ToolDefinition {
            name: "conductor_list_device_identities".to_string(),
            description: "List every device whose SysEx identity has been cached during this daemon session. Returns `{ identities: [{ port_name, identity, confidence }, ...] }`, where each `identity` carries the parsed Identity Reply fields (manufacturer / family / model / version) and each `confidence` is `\"direct_paired_port\"` (auto-promotion safe) or `\"shared_route\"` (caller should confirm before binding). ReadOnly. Cache is in-memory only and clears on daemon restart or device disconnect.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        // Physical control state (ADR-025 Phase 1)
        ToolDefinition {
            name: "conductor_get_control_state".to_string(),
            description: "Return the current physical control state for all devices, optionally filtered to one. Includes the most recently received Program Change, Control Change values, held notes, pitch bend, and aftertouch. Read-only snapshot of hardware reality — in-memory only, cleared on daemon restart.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "device": {
                        "type": "string",
                        "description": "Optional device alias to filter by. Omit to return state for all devices."
                    }
                },
                "required": []
            }),
        },
        ToolDefinition {
            name: "conductor_get_active_pc".to_string(),
            description: "Shorthand for listing every (device, channel) → active Program Change number. Useful for 'what preset am I on right now?' queries, particularly for multi-function foot controllers like the Behringer FCB1010 where pedal routing depends on the active PC.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "device": {
                        "type": "string",
                        "description": "Optional device alias to filter by."
                    }
                },
                "required": []
            }),
        },
        // Network-listener security (ADR-042 Phase B-early, B.7 visibility)
        ToolDefinition {
            name: "conductor_security_status".to_string(),
            description: "Report the network-approval HMAC key's rotation status. Returns `{ hmac_key_fingerprint, hmac_key_age_days, hmac_key_warning }` where `hmac_key_warning` is one of `ok` / `consider_rotation` (>=180d) / `should_rotate` (>=270d) / `approaching_expiry` (>=300d) / `deprecated` (>=365d) / `hard_expired` (>=730d — the daemon refuses to start) / `unavailable` (no key initialised yet or backend unavailable; fingerprint and age are null, plus a `detail` string explaining why). ReadOnly and report-only: never refuses, even for a hard-expired key. Mirrors `conductorctl security status --json`.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
    ]
}
