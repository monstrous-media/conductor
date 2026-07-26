// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! `McpToolExecutor` — construction + the `execute` dispatch.
//! Split out of `mcp_tools.rs` in #2601; the bulky per-tool query
//! builders live in `executor_queries.rs` (second `impl` block).

use super::GUI_ONLY_TOOL_ERROR;
use crate::daemon::mcp_types::ToolCallResult;
use conductor_core::config::{ActionConfig, Config, Trigger};
use conductor_core::device_intelligence::fingerprint::{
    DeviceCategory, EventStats, suggest_binding as compute_suggestion,
};
use dashmap::DashMap;
use serde_json::{Value, json};

pub struct McpToolExecutor {
    // Will hold references to daemon state in future phases
}

impl McpToolExecutor {
    pub fn new() -> Self {
        Self {}
    }

    /// Build ranked alternative suggestions using core's suggest_binding (P18).
    /// Returns up to 2 alternatives with lower confidence than the primary.
    pub(super) fn build_alternatives(
        primary_category: &DeviceCategory,
        primary_confidence: f64,
        port_name: &str,
    ) -> Vec<serde_json::Value> {
        // No alternatives when primary has no confidence (Unknown/no data)
        if primary_confidence <= 0.0 {
            return Vec::new();
        }

        // Minimal synthetic EventStats to trigger each category.
        // Uses core's compute_suggestion to keep alias/protocol in sync.
        let categories = [
            (DeviceCategory::PadController, 36u8, 40u8, false, false),
            (DeviceCategory::Keyboard, 21, 100, false, false),
            (DeviceCategory::FaderController, 0, 1, true, false),
            (DeviceCategory::EncoderController, 0, 0, true, true),
            (DeviceCategory::GameController, 0, 0, false, false),
        ];
        let mut alts = Vec::new();
        let mut rank = 0usize;
        for (cat, a, b, is_cc, is_encoder) in &categories {
            if cat == primary_category {
                continue;
            }
            if rank >= 2 {
                break;
            }
            let mut stats = EventStats::new();
            if *cat == DeviceCategory::GameController {
                stats.record_gamepad();
                stats.record_gamepad();
            } else if *is_encoder {
                // Encoder: high event density on few CCs (>10 hits, <=4 unique
                // CCs) with relative-code values (#1451: classification now
                // requires relative-value evidence, not just hit density).
                for i in 0..12 {
                    stats.record_cc(0, if i % 2 == 0 { 1 } else { 127 });
                }
            } else if *is_cc {
                stats.record_cc(*a, 64);
                stats.record_cc(*b, 64);
            } else {
                stats.record_note(*a, 80);
                stats.record_note(*b, 80);
            }
            let suggestion = compute_suggestion(&stats, port_name);
            let cat_value = serde_json::to_value(cat)
                .unwrap_or(serde_json::Value::String("Unknown".to_string()));
            let decay = if rank == 0 { 0.3 } else { 0.15 };
            let alt_conf = (primary_confidence * decay).min(0.2);
            alts.push(json!({
                "category": cat_value,
                "suggested_alias": suggestion.suggested_alias,
                "suggested_protocol": suggestion.suggested_protocol,
                "confidence": alt_conf,
            }));
            rank += 1;
        }
        alts
    }

    /// Look up last event timestamp for a port from the fingerprint stats.
    pub(super) fn lookup_last_event_ms(
        event_stats: Option<&DashMap<String, EventStats>>,
        port_name: Option<&str>,
    ) -> Option<u64> {
        let stats_map = event_stats?;
        let name = port_name?;
        stats_map.get(name).and_then(|s| s.last_event_ms)
    }

    /// Execute a tool call
    pub async fn execute(
        &self,
        tool_name: &str,
        arguments: Option<Value>,
        status_data: Option<Value>,
        devices_data: Option<Value>,
        config: Option<&Config>,
        event_stats: Option<&DashMap<String, EventStats>>,
    ) -> ToolCallResult {
        match tool_name {
            "conductor_get_status" => self.get_status(status_data),
            "conductor_list_devices" => self.list_devices(devices_data),
            "conductor_get_config" => self.get_config(config),
            "conductor_list_mappings" => self.list_mappings(arguments, config),
            "conductor_get_mapping" => self.get_mapping(arguments, config),
            "conductor_list_discovered_ports" => {
                self.list_discovered_ports(devices_data, status_data, config)
            }
            "conductor_get_binding_health" => {
                self.get_binding_health(arguments, status_data, config, event_stats)
            }
            "conductor_render_artifact" => {
                let artifact_type = arguments
                    .as_ref()
                    .and_then(|a| a.get("artifact_type"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("");
                if artifact_type.is_empty() {
                    return ToolCallResult::error("Missing or empty artifact_type");
                }
                let title = arguments
                    .as_ref()
                    .and_then(|a| a.get("title"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("Artifact");
                let _data = arguments
                    .as_ref()
                    .and_then(|a| a.get("data").cloned())
                    .unwrap_or(json!({}));
                let id = format!("artifact-{}", uuid::Uuid::new_v4());

                ToolCallResult::json(&json!({
                    "status": "rendered",
                    "artifact_id": id,
                    "artifact_type": artifact_type,
                    "title": title,
                    "instructions": "Artifact projected in workspace canvas"
                }))
            }
            "conductor_dismiss_artifact" => {
                let id = arguments
                    .as_ref()
                    .and_then(|a| a.get("artifact_id"))
                    .and_then(|i| i.as_str())
                    .unwrap_or("");
                if id.is_empty() {
                    return ToolCallResult::error("Missing or empty artifact_id");
                }
                ToolCallResult::json(&json!({
                    "status": "dismissed",
                    "artifact_id": id
                }))
            }
            "conductor_get_workspace_state" => {
                // Workspace state is frontend-only; the daemon has no knowledge of GUI view state.
                // The actual T4 context is injected client-side in chat.js via the system prompt.
                ToolCallResult::error(
                    "conductor_get_workspace_state must be handled client-side. \
                     The workspace view state is only available in the GUI frontend. \
                     Use the T4 context in the system prompt for workspace state information.",
                )
            }
            "conductor_list_device_bindings" => self.list_device_bindings(status_data),
            "conductor_list_routes" => self.list_routes(config),
            "conductor_get_routing_graph" => self.get_routing_graph(config),
            "conductor_validate_config" => self.validate_config(config),
            "conductor_get_topology_summary" => self.get_topology_summary(status_data, config),
            "conductor_switch_mode" => self.switch_mode(arguments, config),
            // ADR-040 4c — config-only validation fallback. The real execution
            // (lock/unlock + live status) runs via the daemon path (mcp/tools_call.rs
            // / llm/executor.rs), which special-case these before this executor; this
            // arm only validates the mode for clients that hit the executor
            // directly (mirrors conductor_switch_mode).
            "conductor_set_mode" => self.switch_mode(arguments, config),
            "conductor_unlock_mode" => ToolCallResult::json(
                &json!({ "status": "validated", "note": "no unlock performed here — the lock is released via the daemon's routed execution path" }),
            ),
            "conductor_mode_status" => ToolCallResult::json(
                &json!({ "status": "validated", "note": "live mode status is served via the daemon path" }),
            ),
            "conductor_get_active_profile" => self.get_active_profile(status_data),
            "conductor_switch_profile" => {
                // switch_profile is handled by the LLM executor path which has daemon state refs;
                // McpToolExecutor (stateless) cannot execute it directly.
                ToolCallResult::error(
                    "conductor_switch_profile must be executed via the LLM executor path (requires daemon state)",
                )
            }
            "conductor_list_plugins"
            | "conductor_plugin_info"
            | "conductor_enable_plugin"
            | "conductor_disable_plugin" => {
                // Plugin tools require daemon state (PluginManager); handled via LLM executor path
                ToolCallResult::error(&format!(
                    "{} must be executed via the LLM executor path (requires daemon state)",
                    tool_name
                ))
            }
            "conductor_suggest_binding" => self.suggest_binding(arguments, event_stats),
            // ADR-026 Phase 2: probe + identity tools are handled in
            // mcp/tools_call.rs::handle_tools_call (Unix-socket MCP path)
            // and llm/executor.rs (LLM executor path). Both branches need
            // SharedDaemonStateRefs; this generic executor doesn't have
            // that. If a caller reaches this branch they bypassed both
            // routes — surface a clear error rather than silently
            // returning empty results.
            "conductor_probe_device_identity"
            | "conductor_get_device_identity"
            | "conductor_list_device_identities" => ToolCallResult::error(&format!(
                "{} requires the daemon's routed-execution path (Unix-socket MCP special-case, or the GUI/LLM executor path); the generic executor cannot reach the ProbeCoordinator",
                tool_name
            )),
            // GUI-only profile tools — should be intercepted frontend-side (ADR-023)
            "conductor_list_profiles" | "conductor_create_profile" | "conductor_delete_profile" => {
                ToolCallResult::error(GUI_ONLY_TOOL_ERROR)
            }
            _ => ToolCallResult::error(&format!("Unknown tool: {}", tool_name)),
        }
    }
}

impl Default for McpToolExecutor {
    fn default() -> Self {
        Self::new()
    }
}

// Topology analysis helpers (ADR-016 Chunk 1C - #565)

/// Classify a trigger into (type_name, optional_channel).
pub(super) fn classify_trigger(trigger: &Trigger) -> (&'static str, Option<u8>) {
    match trigger {
        Trigger::Note { channel, .. }
        | Trigger::LongPress { channel, .. }
        | Trigger::DoubleTap { channel, .. }
        | Trigger::VelocityRange { channel, .. } => ("Note", *channel),
        Trigger::CC { channel, .. } | Trigger::EncoderTurn { channel, .. } => ("CC", *channel),
        Trigger::NoteChord { channel, .. } => ("Note", *channel),
        Trigger::Aftertouch { channel, .. } => ("Aftertouch", *channel),
        Trigger::PolyAftertouch { channel, .. } => ("PolyAftertouch", *channel),
        Trigger::PitchBend { channel, .. } => ("PitchBend", *channel),
        Trigger::ProgramChange { channel, .. } => ("ProgramChange", *channel),
        Trigger::GamepadButton { .. }
        | Trigger::GamepadButtonChord { .. }
        | Trigger::GamepadAnalogStick { .. }
        | Trigger::GamepadTrigger { .. } => ("Gamepad", None),
        Trigger::OscMessage { .. }
        | Trigger::OscAddressPattern { .. }
        | Trigger::OscArgRange { .. } => ("Osc", None),
    }
}

/// Map a SendMidi message_type string to the trigger type that would match on the receiving end.
pub(super) fn midi_message_to_trigger_type(message_type: &str) -> Option<&'static str> {
    let normalized = message_type.to_lowercase().replace('-', "_");
    match normalized.as_str() {
        "noteon" | "note_on" => Some("Note"),
        "cc" | "controlchange" | "control_change" => Some("CC"),
        "pitchbend" | "pitch_bend" | "pb" => Some("PitchBend"),
        "aftertouch" | "at" => Some("Aftertouch"),
        "polyaftertouch" | "poly_aftertouch" | "pat" => Some("PolyAftertouch"),
        // NoteOff excluded — backend Trigger::Note only matches note-on
        // ProgramChange excluded — no corresponding trigger type
        _ => None,
    }
}

/// Infer what MIDI message type a MidiForward would send based on the mapping trigger.
pub(super) fn infer_message_type_from_trigger(trigger: &Trigger) -> Option<&'static str> {
    match trigger {
        Trigger::Note { .. }
        | Trigger::LongPress { .. }
        | Trigger::DoubleTap { .. }
        | Trigger::VelocityRange { .. }
        | Trigger::NoteChord { .. } => Some("NoteOn"),
        Trigger::CC { .. } | Trigger::EncoderTurn { .. } => Some("CC"),
        Trigger::Aftertouch { .. } => Some("Aftertouch"),
        Trigger::PolyAftertouch { .. } => Some("PolyAftertouch"),
        Trigger::PitchBend { .. } => Some("PitchBend"),
        _ => None,
    }
}

/// Classify an action and collect routing entries.
pub(super) fn classify_action(
    action: &ActionConfig,
    summary: &mut Value,
    routing: &mut Vec<Value>,
    from_device: Option<&str>,
    trigger: &Trigger,
) {
    match action {
        ActionConfig::Sequence { actions } => {
            increment(summary, "fan_out");
            increment(summary, "sequences");
            for inner in actions {
                collect_routing(inner, routing, from_device, trigger);
            }
        }
        ActionConfig::Conditional {
            then_action,
            else_action,
            ..
        } => {
            increment(summary, "fan_out");
            increment(summary, "conditionals");
            collect_routing(then_action, routing, from_device, trigger);
            if let Some(else_act) = else_action {
                collect_routing(else_act, routing, from_device, trigger);
            }
        }
        _ => {
            increment(summary, "simple");
            collect_routing(action, routing, from_device, trigger);
        }
    }
}

/// Collect routing entries from an action.
fn collect_routing(
    action: &ActionConfig,
    routing: &mut Vec<Value>,
    from_device: Option<&str>,
    trigger: &Trigger,
) {
    match action {
        ActionConfig::SendMidi {
            port,
            message_type,
            channel,
            ..
        } => {
            routing.push(json!({
                "from_device": from_device,
                "to_port": port,
                "action_type": "SendMidi",
                "message_type": message_type,
                "channel": channel,
            }));
        }
        ActionConfig::MidiForward { target, transform } => {
            let (_, trigger_channel) = classify_trigger(trigger);
            let channel = transform
                .as_ref()
                .and_then(|t| t.channel)
                .or(trigger_channel);
            routing.push(json!({
                "from_device": from_device,
                "to_port": target,
                "action_type": "MidiForward",
                "message_type": infer_message_type_from_trigger(trigger),
                "channel": channel,
            }));
        }
        ActionConfig::HidForward { target, transform } => {
            // ADR-039-B #1762 step 4b: gamepad event → MIDI output (HidToMidi).
            let channel = match transform {
                conductor_core::config::types::SignalTransform::HidToMidi { channel, .. } => {
                    Some(*channel)
                }
                _ => None,
            };
            routing.push(json!({
                "from_device": from_device,
                "to_port": target,
                "action_type": "HidForward",
                "message_type": "CC",
                "channel": channel,
            }));
        }
        ActionConfig::OscSend { host, port, .. } => {
            routing.push(json!({
                "from_device": from_device,
                "to_port": format!("{}:{}", host, port),
                "action_type": "OscSend",
            }));
        }
        ActionConfig::OscForward { target, .. } => {
            routing.push(json!({
                "from_device": from_device,
                "to_port": target,
                "action_type": "OscForward",
            }));
        }
        ActionConfig::Sequence { actions } => {
            for inner in actions {
                collect_routing(inner, routing, from_device, trigger);
            }
        }
        ActionConfig::Conditional {
            then_action,
            else_action,
            ..
        } => {
            collect_routing(then_action, routing, from_device, trigger);
            if let Some(else_act) = else_action {
                collect_routing(else_act, routing, from_device, trigger);
            }
        }
        ActionConfig::Repeat { action, .. } => {
            collect_routing(action, routing, from_device, trigger);
        }
        _ => {}
    }
}

/// Increment a JSON counter field.
fn increment(summary: &mut Value, key: &str) {
    if let Some(val) = summary.get_mut(key) {
        *val = json!(val.as_u64().unwrap_or(0) + 1);
    }
}
