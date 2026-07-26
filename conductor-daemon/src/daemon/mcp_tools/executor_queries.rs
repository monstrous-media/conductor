// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! `McpToolExecutor` query builders (second `impl` block; #2601).

use crate::daemon::mcp_types::ToolCallResult;
use conductor_core::config::Config;
use conductor_core::device_intelligence::fingerprint::{
    EventStats, suggest_binding as compute_suggestion,
};
use dashmap::DashMap;
use serde_json::{Value, json};

use super::executor::{
    McpToolExecutor, classify_action, classify_trigger, midi_message_to_trigger_type,
};

impl McpToolExecutor {
    /// Get daemon status
    pub(super) fn get_status(&self, status_data: Option<Value>) -> ToolCallResult {
        match status_data {
            Some(data) => ToolCallResult::json(&data),
            None => ToolCallResult::json(&json!({
                "daemon_running": true, // Daemon is responding to this request (#105)
                "lifecycle_state": "Unknown",
                "connected": false,
                "device_connected": false,
                "uptime_secs": 0,
                "message": "Status data not available - daemon is running but state snapshot unavailable"
            })),
        }
    }

    /// List available devices
    pub(super) fn list_devices(&self, devices_data: Option<Value>) -> ToolCallResult {
        match devices_data {
            Some(data) => ToolCallResult::json(&data),
            None => ToolCallResult::json(&json!({
                "midi_devices": [],
                "hid_devices": [],
                "message": "Device data not available"
            })),
        }
    }

    /// List multi-device bindings (v4.23.0 - ADR-009 Phase 5)
    pub(super) fn list_device_bindings(&self, status_data: Option<Value>) -> ToolCallResult {
        let bindings = status_data
            .as_ref()
            .and_then(|data| data.get("device_bindings"))
            .and_then(|b| b.as_array());

        let binding_list = match bindings {
            Some(arr) => arr.clone(),
            None => vec![],
        };

        let total = binding_list.len();
        let connected_count = binding_list
            .iter()
            .filter(|b| {
                b.get("connected")
                    .and_then(|c| c.as_bool())
                    .unwrap_or(false)
            })
            .count();
        let muted_count = binding_list
            .iter()
            .filter(|b| b.get("enabled").and_then(|e| e.as_bool()) == Some(false))
            .count();

        ToolCallResult::json(&json!({
            "device_bindings": binding_list,
            "multi_device_active": !binding_list.is_empty(),
            "total_devices": total,
            "connected_count": connected_count,
            "muted_count": muted_count
        }))
    }

    /// List all discovered ports with binding status (ADR-022 Phase 1C)
    /// Get binding health diagnostic (ADR-022 Phase 3C)
    pub(super) fn get_binding_health(
        &self,
        arguments: Option<Value>,
        status_data: Option<Value>,
        config: Option<&Config>,
        event_stats: Option<&DashMap<String, EventStats>>,
    ) -> ToolCallResult {
        let alias = match arguments
            .as_ref()
            .and_then(|a| a.get("alias"))
            .and_then(|a| a.as_str())
        {
            Some(a) => a,
            None => return ToolCallResult::error("Missing required parameter: alias"),
        };

        // Find endpoint in config (ADR-035 — [[endpoints]])
        let device_cfg = config.and_then(|c| c.endpoints.iter().find(|e| e.alias == alias));
        if device_cfg.is_none() {
            return ToolCallResult::json(&json!({
                "alias": alias,
                "health": "red",
                "error": "Endpoint not found in configuration",
                "issues": ["Endpoint alias not found in [[endpoints]] config"]
            }));
        }
        let device_cfg = device_cfg.unwrap();

        // Check binding status
        let binding_status = status_data
            .as_ref()
            .and_then(|s| s.get("device_bindings"))
            .and_then(|b| b.as_array())
            .and_then(|arr| {
                arr.iter()
                    .find(|b| b.get("device_id").and_then(|d| d.as_str()) == Some(alias))
            });

        let input_connected = binding_status
            .and_then(|b| b.get("connected"))
            .and_then(|c| c.as_bool())
            .unwrap_or(false);
        let input_port = binding_status
            .and_then(|b| b.get("port_name"))
            .and_then(|p| p.as_str());
        let output_connected = binding_status
            .and_then(|b| b.get("output_connected"))
            .and_then(|c| c.as_bool())
            .unwrap_or(false);
        let output_port = binding_status
            .and_then(|b| b.get("output_port_name"))
            .and_then(|p| p.as_str());
        let enabled = binding_status
            .and_then(|b| b.get("enabled"))
            .and_then(|e| e.as_bool())
            .unwrap_or(device_cfg.enabled); // Fall back to config value

        // ADR-035: input/output capability is the endpoint's direction.
        use conductor_core::config::types::ConnectorDirection;
        let has_input = matches!(
            device_cfg.direction,
            ConnectorDirection::Input | ConnectorDirection::Bidirectional
        );
        let has_output = matches!(
            device_cfg.direction,
            ConnectorDirection::Output | ConnectorDirection::Bidirectional
        );

        // Collect issues
        let mut issues = Vec::new();
        if !enabled {
            issues.push("Binding is muted (enabled = false)".to_string());
        }
        if has_input && !input_connected {
            issues.push("Input port not connected — no matching port found".to_string());
        }
        if has_output && !output_connected {
            issues.push("Output port not connected — no matching port found".to_string());
        }

        // Count mappings referencing this binding
        let mapping_count = config.map_or(0, |c| {
            c.modes
                .iter()
                .flat_map(|m| m.mappings.iter())
                .chain(c.global_mappings.iter())
                .filter(|m| m.trigger.device().is_some_and(|d| d == alias))
                .count()
        });

        // Determine health
        let health = if !enabled {
            "amber"
        } else if (has_input && !input_connected) || (has_output && !output_connected) {
            "red"
        } else if issues.is_empty() {
            "green"
        } else {
            "amber"
        };

        // Auto-paired: output was resolved via auto-pairing (not explicit config)
        let auto_paired = binding_status
            .and_then(|b| b.get("output_auto_paired"))
            .and_then(|a| a.as_bool())
            .unwrap_or(false);

        // Interaction pattern — prefer runtime direction from status, fallback to config
        let interaction_pattern = if let Some(direction) = binding_status
            .and_then(|b| b.get("direction"))
            .and_then(|d| d.as_str())
        {
            match direction {
                "Bidirectional" | "bidirectional" => "bidirectional",
                "Output" | "output" | "send" => "send",
                _ => "receive",
            }
        } else if auto_paired && has_input {
            "bidirectional" // auto-paired output upgrades to bidirectional
        } else {
            match (has_input, has_output) {
                (true, true) => "bidirectional",
                (true, false) => "receive",
                (false, true) => "send",
                (false, false) => "receive",
            }
        };

        ToolCallResult::json(&json!({
            "alias": alias,
            "health": health,
            "enabled": enabled,
            "interaction_pattern": interaction_pattern,
            "auto_paired": auto_paired,
            "last_event_timestamp": Self::lookup_last_event_ms(event_stats, input_port),
            "input": {
                "configured": has_input,
                "connected": input_connected,
                "port_name": input_port,
            },
            "output": {
                "configured": has_output,
                "connected": output_connected,
                "port_name": output_port,
            },
            "mapping_count": mapping_count,
            "issues": issues,
        }))
    }

    pub(super) fn list_discovered_ports(
        &self,
        devices_data: Option<Value>,
        status_data: Option<Value>,
        config: Option<&Config>,
    ) -> ToolCallResult {
        let mut ports = Vec::new();

        // Resolve binding alias for a port name by checking config matchers.
        // Falls back to status_data port_name matching if no config available.
        let find_binding = |port_name: &str| -> Option<String> {
            // Primary: check endpoint input matchers (ADR-035)
            use conductor_core::config::types::ConnectorDirection;
            if let Some(cfg) = config {
                for ep in &cfg.endpoints {
                    let input_matchers = ep.kind.effective_matchers(ConnectorDirection::Input);
                    if input_matchers.iter().any(|m| m.matches(port_name)) {
                        return Some(ep.alias.clone());
                    }
                }
            }
            // Fallback: check status_data bindings
            let bindings = status_data
                .as_ref()
                .and_then(|data| data.get("device_bindings"))
                .and_then(|b| b.as_array())?;
            bindings.iter().find_map(|b| {
                let bound_port = b.get("port_name").and_then(|p| p.as_str())?;
                if bound_port == port_name {
                    b.get("device_id")
                        .and_then(|d| d.as_str())
                        .map(String::from)
                } else {
                    None
                }
            })
        };

        // Prefer devices_data; fall back to status_data when MCP returns empty devices
        let device_source = devices_data.as_ref().or(status_data.as_ref());
        if let Some(data) = device_source {
            // MIDI input ports
            if let Some(midi_devices) = data.get("midi_devices").and_then(|d| d.as_array()) {
                for dev in midi_devices {
                    let name = dev
                        .get("port_name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("Unknown");
                    let binding = find_binding(name);
                    let connected = dev
                        .get("connected")
                        .and_then(|c| c.as_bool())
                        .unwrap_or(true);
                    ports.push(json!({
                        "name": name,
                        "protocol": "midi",
                        "direction": "Input",
                        "binding": binding,
                        "connected": connected,
                        "metadata": {}
                    }));
                }
            }

            // TODO: MIDI output port enumeration — data.output_ports is not currently
            // populated by the daemon status response. Output bindings are inferred from
            // config rather than live port state. Add when daemon exposes output port list.

            // HID devices
            if let Some(hid_devices) = data.get("hid_devices").and_then(|d| d.as_array()) {
                for dev in hid_devices {
                    let name = dev
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("Unknown HID");
                    let binding = find_binding(name);
                    ports.push(json!({
                        "name": name,
                        "protocol": "hid",
                        "direction": "Input",
                        "binding": binding,
                        "connected": true,
                        "metadata": {}
                    }));
                }
            }
        }

        let total = ports.len();
        let bound = ports
            .iter()
            .filter(|p| p.get("binding").is_some_and(|b| !b.is_null()))
            .count();

        ToolCallResult::json(&json!({
            "ports": ports,
            "summary": {
                "total": total,
                "bound": bound,
                "unbound": total - bound
            }
        }))
    }

    /// List all signal-routing routes from the active config
    /// (ADR-031 § 3.4, P3 slice 3 / #1143).
    ///
    /// Reads `config.routes` directly — the config is the single
    /// source of truth for the *declared* route set. Runtime exclusion
    /// detail (which routes the engine compiled out for unsupported
    /// cross-protocol transforms or OSC-on-MIDI filters) is reported
    /// in a separate `excluded` array; populating it requires reading
    /// the live `RouteEngine`, which lands in a later slice when the
    /// `route_engine` Arc gets exposed via `SharedDaemonStateRefs`.
    /// Until then the field is always an empty array — documented in
    /// the response shape so the LLM doesn't treat absence as
    /// "everything is active."
    pub(super) fn list_routes(&self, config: Option<&Config>) -> ToolCallResult {
        let Some(cfg) = config else {
            return ToolCallResult::error("Configuration not loaded");
        };
        match serde_json::to_value(&cfg.routes) {
            Ok(routes) => ToolCallResult::json(&json!({
                "routes": routes,
                "excluded": [],
                "excluded_note": "Runtime exclusion detail not yet wired \
                                  (RouteEngine on SharedDaemonStateRefs); \
                                  declared routes only.",
            })),
            Err(e) => ToolCallResult::error(&format!("Failed to serialize routes: {}", e)),
        }
    }

    /// Combined topology view (ADR-031 § 3.7 P3 slice 16 / gap A):
    /// `connectors` + `routes` from the active config in one round-trip,
    /// so the LLM doesn't have to also call `conductor_list_routes`
    /// separately for "show me my routing graph" questions.
    ///
    /// `excluded` and per-connector live-status remain deferred — same
    /// caveat as `list_routes` above. Slice 16's scope is the
    /// config-derived view; the RouteEngine + connector_registry runtime
    /// data plumbing through SharedDaemonStateRefs is its own follow-up.
    pub(super) fn get_routing_graph(&self, config: Option<&Config>) -> ToolCallResult {
        let Some(cfg) = config else {
            return ToolCallResult::error("Configuration not loaded");
        };
        let endpoints = match serde_json::to_value(&cfg.endpoints) {
            Ok(v) => v,
            Err(e) => {
                return ToolCallResult::error(&format!("Failed to serialize endpoints: {}", e));
            }
        };
        let routes = match serde_json::to_value(&cfg.routes) {
            Ok(v) => v,
            Err(e) => {
                return ToolCallResult::error(&format!("Failed to serialize routes: {}", e));
            }
        };
        ToolCallResult::json(&json!({
            "endpoints": endpoints,
            "routes": routes,
            "excluded": [],
            "excluded_note": "Runtime exclusion detail not yet wired \
                              (RouteEngine on SharedDaemonStateRefs); \
                              declared connectors + routes only. \
                              Per-connector bound-port / connected status \
                              also deferred to a follow-up slice.",
        }))
    }

    /// Validate config against protocol schemas (v4.26.66)
    pub(super) fn validate_config(&self, config: Option<&Config>) -> ToolCallResult {
        match config {
            Some(cfg) => {
                let report = conductor_core::config::validator::validate_config(cfg);
                match serde_json::to_value(&report) {
                    Ok(val) => ToolCallResult::json(&val),
                    Err(e) => ToolCallResult::error(&format!(
                        "Failed to serialize validation report: {}",
                        e
                    )),
                }
            }
            None => ToolCallResult::error("Configuration not loaded"),
        }
    }

    /// Get current configuration
    pub(super) fn get_config(&self, config: Option<&Config>) -> ToolCallResult {
        match config {
            Some(cfg) => {
                // Serialize config to JSON
                match serde_json::to_value(cfg) {
                    Ok(value) => ToolCallResult::json(&value),
                    Err(e) => ToolCallResult::error(&format!("Failed to serialize config: {}", e)),
                }
            }
            None => ToolCallResult::error("Configuration not loaded"),
        }
    }

    /// List mappings, optionally filtered by mode
    pub(super) fn list_mappings(
        &self,
        arguments: Option<Value>,
        config: Option<&Config>,
    ) -> ToolCallResult {
        let config = match config {
            Some(c) => c,
            None => return ToolCallResult::error("Configuration not loaded"),
        };

        let mode_filter = arguments
            .as_ref()
            .and_then(|args| args.get("mode"))
            .and_then(|m| m.as_str());

        let mut result = json!({
            "modes": []
        });

        let modes_array = result["modes"].as_array_mut().unwrap();

        for mode in &config.modes {
            // Skip if mode filter specified and doesn't match
            if let Some(filter) = mode_filter
                && mode.name != filter
            {
                continue;
            }

            let mappings: Vec<Value> = mode
                .mappings
                .iter()
                .enumerate()
                .map(|(idx, mapping)| {
                    json!({
                        "index": idx,
                        "trigger": mapping.trigger,
                        "action": mapping.action,
                        "description": mapping.description
                    })
                })
                .collect();

            modes_array.push(json!({
                "name": mode.name,
                "color": mode.color,
                "mapping_count": mode.mappings.len(),
                "mappings": mappings
            }));
        }

        // Also include global mappings
        if mode_filter.is_none() {
            let global_mappings: Vec<Value> = config
                .global_mappings
                .iter()
                .enumerate()
                .map(|(idx, mapping)| {
                    json!({
                        "index": idx,
                        "trigger": mapping.trigger,
                        "action": mapping.action,
                        "description": mapping.description
                    })
                })
                .collect();

            result["global_mappings"] = json!(global_mappings);
        }

        ToolCallResult::json(&result)
    }

    /// Get a specific mapping by mode and index
    pub(super) fn get_mapping(
        &self,
        arguments: Option<Value>,
        config: Option<&Config>,
    ) -> ToolCallResult {
        let config = match config {
            Some(c) => c,
            None => return ToolCallResult::error("Configuration not loaded"),
        };

        let args = match arguments {
            Some(a) => a,
            None => return ToolCallResult::error("Missing required arguments: mode, index"),
        };

        let mode_name = match args.get("mode").and_then(|m| m.as_str()) {
            Some(m) => m,
            None => return ToolCallResult::error("Missing required argument: mode"),
        };

        let index = match args.get("index").and_then(|i| i.as_u64()) {
            Some(i) => i as usize,
            None => return ToolCallResult::error("Missing required argument: index"),
        };

        // Find the mode
        let mode = match config.modes.iter().find(|m| m.name == mode_name) {
            Some(m) => m,
            None => {
                return ToolCallResult::error(&format!(
                    "Mode not found: {}. Available modes: {}",
                    mode_name,
                    config
                        .modes
                        .iter()
                        .map(|m| m.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        };

        // Get the mapping
        match mode.mappings.get(index) {
            Some(mapping) => ToolCallResult::json(&json!({
                "mode": mode_name,
                "index": index,
                "trigger": mapping.trigger,
                "action": mapping.action,
                "description": mapping.description
            })),
            None => ToolCallResult::error(&format!(
                "Mapping index {} out of range for mode '{}' (has {} mappings)",
                index,
                mode_name,
                mode.mappings.len()
            )),
        }
    }

    /// Switch the active mapping mode by name (LLM executor path only — validation and response).
    /// The MCP server path uses `mcp/tools_call.rs` directly via `DispatchOutcome::ModeChangeRequested`.
    pub(super) fn switch_mode(
        &self,
        arguments: Option<Value>,
        config: Option<&Config>,
    ) -> ToolCallResult {
        let config = match config {
            Some(c) => c,
            None => return ToolCallResult::error("Configuration not loaded"),
        };

        let args = match arguments {
            Some(a) => a,
            None => return ToolCallResult::error("Missing required arguments: mode"),
        };

        let mode_name = match args.get("mode").and_then(|m| m.as_str()) {
            Some(m) => m,
            None => return ToolCallResult::error("Missing required argument: mode"),
        };

        // Find the mode index via the canonical validator in conductor-core
        // (#1567) — the same code the mode-management integration tests use, so
        // the "Mode not found … Available modes …" contract has one source.
        let mode_index = match config.resolve_mode_switch(mode_name) {
            Ok(idx) => idx,
            Err(e) => return ToolCallResult::error(&e),
        };

        // LLM executor validation path only (READ-ONLY): validates the mode exists and returns
        // its index. Does NOT trigger an actual mode switch — the MCP server path (mcp/tools_call.rs)
        // handles real mode switching via DispatchOutcome::ModeChangeRequested.
        ToolCallResult::json(&json!({
            "mode_name": mode_name,
            "mode_index": mode_index,
            "status": "validated"
        }))
    }
    /// Get signal topology summary (ADR-016 Chunk 1C - #565)
    ///
    /// Analyzes config + device bindings to produce a structured topology summary
    /// including device status, mapping classification, routing paths, and loop warnings.
    pub(super) fn get_topology_summary(
        &self,
        status_data: Option<Value>,
        config: Option<&Config>,
    ) -> ToolCallResult {
        let config = match config {
            Some(cfg) => cfg,
            None => {
                return ToolCallResult::json(&json!({
                    "devices": [],
                    "mappings": { "total": 0, "simple": 0, "fan_out": 0, "sequences": 0, "conditionals": 0 },
                    "routing": [],
                    "warnings": [],
                    "message": "Configuration not loaded"
                }));
            }
        };

        // Extract device bindings from status data
        let bindings = status_data
            .as_ref()
            .and_then(|d| d.get("device_bindings"))
            .and_then(|b| b.as_array())
            .cloned()
            .unwrap_or_default();

        // Build device summary
        let devices: Vec<Value> = bindings
            .iter()
            .map(|b| {
                json!({
                    "device_id": b.get("device_id").and_then(|v| v.as_str()).unwrap_or("unknown"),
                    "alias": b.get("alias").and_then(|v| v.as_str())
                        .or_else(|| b.get("device_id").and_then(|v| v.as_str()))
                        .unwrap_or("unknown"),
                    "port_name": b.get("port_name").and_then(|v| v.as_str()).unwrap_or("unknown"),
                    "connected": b.get("connected").and_then(|v| v.as_bool()).unwrap_or(false),
                    "enabled": b.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true),
                })
            })
            .collect();

        // Build enabled ports set for loop detection
        // Match by port_name, alias, and device_id (superset of frontend which matches port_name + alias)
        let mut enabled_ports: Vec<(&str, &Value)> = Vec::new();
        for b in &bindings {
            if !b.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true) {
                continue;
            }
            if let Some(port_name) = b.get("port_name").and_then(|v| v.as_str()) {
                enabled_ports.push((port_name, b));
            }
            if let Some(alias) = b.get("alias").and_then(|v| v.as_str()) {
                enabled_ports.push((alias, b));
            }
            if let Some(device_id) = b.get("device_id").and_then(|v| v.as_str()) {
                enabled_ports.push((device_id, b));
            }
        }

        // Analyze all mappings across all modes
        let mut mapping_summary = json!({
            "total": 0,
            "simple": 0,
            "fan_out": 0,
            "sequences": 0,
            "conditionals": 0,
        });
        let mut routing: Vec<Value> = Vec::new();
        let mut warnings: Vec<Value> = Vec::new();

        // Collect all triggers for loop detection: (device_id, type, channel)
        // device_id=None means device-agnostic trigger (matches events from any device)
        let mut all_triggers: Vec<(Option<&str>, &str, Option<u8>)> = Vec::new();
        for mode in &config.modes {
            for mapping in &mode.mappings {
                let (trigger_type, trigger_channel) = classify_trigger(&mapping.trigger);
                let device_id = mapping.trigger.device().map(|s| s.as_str());
                all_triggers.push((device_id, trigger_type, trigger_channel));
            }
        }
        for mapping in &config.global_mappings {
            let (trigger_type, trigger_channel) = classify_trigger(&mapping.trigger);
            let device_id = mapping.trigger.device().map(|s| s.as_str());
            all_triggers.push((device_id, trigger_type, trigger_channel));
        }

        for mode in &config.modes {
            for mapping in &mode.mappings {
                *mapping_summary.get_mut("total").unwrap() =
                    json!(mapping_summary["total"].as_u64().unwrap_or(0) + 1);

                let from_device = mapping.trigger.device().map(|d| d.to_string());
                classify_action(
                    &mapping.action,
                    &mut mapping_summary,
                    &mut routing,
                    from_device.as_deref(),
                    &mapping.trigger,
                );
            }
        }
        for mapping in &config.global_mappings {
            *mapping_summary.get_mut("total").unwrap() =
                json!(mapping_summary["total"].as_u64().unwrap_or(0) + 1);

            let from_device = mapping.trigger.device().map(|d| d.to_string());
            classify_action(
                &mapping.action,
                &mut mapping_summary,
                &mut routing,
                from_device.as_deref(),
                &mapping.trigger,
            );
        }

        // Detect loop warnings
        for route in &routing {
            let to_port = route.get("to_port").and_then(|v| v.as_str()).unwrap_or("");
            let action_type = route
                .get("action_type")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let route_channel = route
                .get("channel")
                .and_then(|v| v.as_u64())
                .map(|c| c as u8);
            let message_type = route.get("message_type").and_then(|v| v.as_str());

            // Check if target port is an enabled input device
            if let Some((_port, binding)) = enabled_ports.iter().find(|(p, _)| *p == to_port) {
                let from = route
                    .get("from_device")
                    .and_then(|v| v.as_str())
                    .unwrap_or("source");
                let device_alias = binding
                    .get("alias")
                    .or_else(|| binding.get("device_id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or(to_port);

                // Check for confirmed loop: sent event type matches a trigger type+channel
                let sent_trigger_type = if action_type == "SendMidi" || action_type == "MidiForward"
                {
                    message_type.and_then(midi_message_to_trigger_type)
                } else {
                    None
                };

                let matched_device_id = binding.get("device_id").and_then(|v| v.as_str());

                let is_confirmed = sent_trigger_type.is_some_and(|sent_type| {
                    all_triggers.iter().any(|(t_device, t_type, t_channel)| {
                        // Device match: trigger is device-agnostic (None) or matches target device
                        let device_matches = t_device.is_none()
                            || matched_device_id.is_some_and(|did| *t_device == Some(did));
                        // Type match
                        let type_matches = *t_type == sent_type;
                        // Channel match: trigger has no channel filter (wildcard) or channels match
                        let channel_matches = t_channel.is_none() || *t_channel == route_channel;
                        device_matches && type_matches && channel_matches
                    })
                });

                if is_confirmed {
                    warnings.push(json!({
                        "severity": "red",
                        "description": format!(
                            "Feedback loop detected: {} → {} {} → {} has matching trigger",
                            from, action_type, to_port, device_alias
                        ),
                        "from": from,
                        "to": to_port,
                    }));
                } else {
                    warnings.push(json!({
                        "severity": "amber",
                        "description": format!(
                            "Potential loop: {} → {} (port is also an enabled input device)",
                            from, to_port
                        ),
                        "from": from,
                        "to": to_port,
                    }));
                }
            }
        }

        ToolCallResult::json(&json!({
            "devices": devices,
            "mappings": mapping_summary,
            "routing": routing,
            "warnings": warnings,
        }))
    }

    /// Get the active profile info from status data (Phase 1 - Issue #323)
    pub(super) fn get_active_profile(&self, status_data: Option<Value>) -> ToolCallResult {
        let active_profile = status_data
            .as_ref()
            .and_then(|d| d.get("active_profile"))
            .cloned();

        ToolCallResult::json(&json!({
            "active_profile": active_profile
        }))
    }

    /// Suggest a binding configuration based on event fingerprinting (#755).
    ///
    /// Analyzes the port name and returns a binding suggestion with device
    /// category, confidence, suggested alias, and reasoning.
    /// Suggest a device binding based on event fingerprinting and port name heuristics.
    /// When real event stats are available (from daemon hot path), they take priority.
    /// Falls back to port-name heuristics when no event data is available.
    pub(super) fn suggest_binding(
        &self,
        arguments: Option<Value>,
        event_stats: Option<&DashMap<String, EventStats>>,
    ) -> ToolCallResult {
        let port_name = match arguments
            .as_ref()
            .and_then(|a| a.get("port_name"))
            .and_then(|v| v.as_str())
        {
            Some(name) if !name.trim().is_empty() => name,
            _ => {
                return ToolCallResult::error(
                    "Missing or empty required parameter 'port_name' for conductor_suggest_binding",
                );
            }
        };

        // Try real event stats first (ADR-022 D7)
        if let Some(stats_map) = event_stats {
            // Helper: build response from stats
            let make_response = |stats: &EventStats| -> Option<ToolCallResult> {
                if stats.confidence() > 0.0 {
                    let suggestion = compute_suggestion(stats, port_name);
                    let category_value = serde_json::to_value(&suggestion.category)
                        .unwrap_or(serde_json::Value::String("Unknown".to_string()));
                    let alternatives = Self::build_alternatives(
                        &suggestion.category,
                        suggestion.confidence,
                        port_name,
                    );
                    Some(ToolCallResult::json(&json!({
                        "port_name": port_name,
                        "category": category_value,
                        "confidence": suggestion.confidence,
                        "suggested_alias": suggestion.suggested_alias,
                        "suggested_protocol": suggestion.suggested_protocol,
                        "reasoning": suggestion.reasoning,
                        "method": "event_fingerprint",
                        "event_count": stats.note_count + stats.cc_count + stats.gamepad_count,
                        "alternatives": alternatives,
                        "note": "Classification based on observed event patterns"
                    })))
                } else {
                    None
                }
            };

            // Try exact match first
            if let Some(entry) = stats_map.get(port_name)
                && let Some(result) = make_response(entry.value())
            {
                return result;
            }

            // Then try substring match — select best candidate with cheap comparisons,
            // clone the winning stats, then build response outside the DashMap iteration
            // to minimize shard lock hold time.
            {
                let mut best_stats: Option<EventStats> = None;
                let mut best_key_len: usize = 0;
                let mut best_confidence: f64 = 0.0;
                let mut best_key: String = String::new();
                for entry in stats_map.iter() {
                    let key = entry.key();
                    if key.contains(port_name) || port_name.contains(key.as_str()) {
                        let conf = entry.value().confidence();
                        if conf <= 0.0 {
                            continue;
                        }
                        let better = key.len() > best_key_len
                            || (key.len() == best_key_len && conf > best_confidence)
                            || (key.len() == best_key_len
                                && (conf - best_confidence).abs() < f64::EPSILON
                                && key.as_str() < best_key.as_str());
                        if better {
                            best_key_len = key.len();
                            best_confidence = conf;
                            best_key = key.clone();
                            best_stats = Some(entry.value().clone());
                        }
                    }
                }
                // Build response outside the loop (no shard locks held)
                if let Some(stats) = best_stats
                    && let Some(result) = make_response(&stats)
                {
                    return result;
                }
            }
        }

        // Fall through to port-name heuristic code
        let mut stats = EventStats::new();

        // Heuristic: detect common device types from port name
        let lower = port_name.to_lowercase();
        if lower.contains("pad")
            || lower.contains("mikro")
            || lower.contains("maschine")
            || lower.contains("apc")
            || lower.contains("launchpad")
        {
            // Simulate pad events
            for note in 36..52 {
                stats.record_note(note, 100);
            }
        } else if lower.contains("key") || lower.contains("piano") || lower.contains("arturia") {
            // Simulate keyboard events
            for note in 21..109 {
                stats.record_note(note, 80);
            }
        } else if lower.contains("fader")
            || lower.contains("nano")
            || lower.contains("control")
            || lower.contains("bcf")
        {
            // Simulate fader events (absolute values spread across the range).
            for cc in 0..8 {
                stats.record_cc(cc, cc * 15);
            }
        } else if lower.contains("gamepad")
            || lower.contains("xbox")
            || lower.contains("playstation")
        {
            for _ in 0..20 {
                stats.record_gamepad();
            }
        }

        let suggestion = compute_suggestion(&stats, port_name);

        // Cap confidence for heuristic-only classification (no real event data)
        let heuristic_confidence = suggestion.confidence.min(0.5);

        let category_value = serde_json::to_value(&suggestion.category)
            .unwrap_or(serde_json::Value::String("Unknown".to_string()));

        let alternatives =
            Self::build_alternatives(&suggestion.category, heuristic_confidence, port_name);

        ToolCallResult::json(&json!({
            "port_name": port_name,
            "category": category_value,
            "confidence": heuristic_confidence,
            "suggested_alias": suggestion.suggested_alias,
            "suggested_protocol": suggestion.suggested_protocol,
            "reasoning": suggestion.reasoning,
            "method": "port_name_heuristic",
            "alternatives": alternatives,
            "note": "Based on port name heuristics. Confidence is capped at 0.5 without real event data."
        }))
    }
}
