// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! MCP tool handlers for ADR-025 Phase 1 physical control state.
//!
//! Three tools:
//!   - `conductor_get_control_state` (ReadOnly) — full snapshot, optional device filter
//!   - `conductor_get_active_pc`     (ReadOnly) — PC-only shorthand, returns device/channel→pc map
//!   - `conductor_reset_control_state` (Stateful) — channel or device reset
//!
//! Reset is store-only: it does NOT emit outbound MIDI (no All Notes Off
//! transmission). Users expecting a panic button should also fire a
//! `conductor_send_sysex` / hardware-appropriate reset message through
//! the HardwareIO tier explicitly.

use conductor_core::control_state::{PhysicalControlStateStore, ResetScope, StateKey};
use serde_json::{Value, json};

use crate::daemon::mcp_types::ToolCallResult;

/// Try to handle a ReadOnly control-state tool. Returns `None` if
/// `tool_name` is not one of ours — caller falls back to the standard
/// MCP executor path.
pub fn handle_readonly(
    tool_name: &str,
    arguments: Option<&Value>,
    store: Option<&PhysicalControlStateStore>,
) -> Option<ToolCallResult> {
    match tool_name {
        "conductor_get_control_state" => Some(get_control_state(arguments, store)),
        "conductor_get_active_pc" => Some(get_active_pc(arguments, store)),
        _ => None,
    }
}

/// Try to handle a Stateful control-state tool.
pub fn handle_stateful(
    tool_name: &str,
    arguments: Option<&Value>,
    store: Option<&PhysicalControlStateStore>,
) -> Option<ToolCallResult> {
    match tool_name {
        "conductor_reset_control_state" => Some(reset_control_state(arguments, store)),
        _ => None,
    }
}

// ────────────────────────────────────────────────────────────────
// conductor_get_control_state
// ────────────────────────────────────────────────────────────────

fn get_control_state(
    arguments: Option<&Value>,
    store: Option<&PhysicalControlStateStore>,
) -> ToolCallResult {
    let Some(store) = store else {
        return ToolCallResult::error("Control state store not available (daemon not running)");
    };

    let device_filter = arguments
        .and_then(|a| a.get("device"))
        .and_then(|v| v.as_str());

    let entries = store.snapshot(device_filter);
    let entries_json: Vec<Value> = entries.into_iter().map(serialize_entry).collect();

    ToolCallResult::json(&json!({
        "entries": entries_json,
        "device_filter": device_filter,
        "total": entries_json.len(),
    }))
}

// ────────────────────────────────────────────────────────────────
// conductor_get_active_pc
// ────────────────────────────────────────────────────────────────

fn get_active_pc(
    arguments: Option<&Value>,
    store: Option<&PhysicalControlStateStore>,
) -> ToolCallResult {
    let Some(store) = store else {
        return ToolCallResult::error("Control state store not available (daemon not running)");
    };

    let device_filter = arguments
        .and_then(|a| a.get("device"))
        .and_then(|v| v.as_str());

    let entries = store.snapshot(device_filter);

    // Keep only ProgramChange entries, shape as [{device, channel, pc}].
    let pcs: Vec<Value> = entries
        .into_iter()
        .filter_map(|(key, value)| match key {
            StateKey::ProgramChange { device, channel } => Some(json!({
                "device": device,
                "channel": channel,
                "pc": value.value as u8,
            })),
            _ => None,
        })
        .collect();

    ToolCallResult::json(&json!({
        "program_changes": pcs,
        "device_filter": device_filter,
    }))
}

// ────────────────────────────────────────────────────────────────
// conductor_reset_control_state
// ────────────────────────────────────────────────────────────────

fn reset_control_state(
    arguments: Option<&Value>,
    store: Option<&PhysicalControlStateStore>,
) -> ToolCallResult {
    let Some(store) = store else {
        return ToolCallResult::error("Control state store not available (daemon not running)");
    };

    let Some(args) = arguments else {
        return ToolCallResult::error(
            "Missing arguments. Provide {device, channel?, scope} for channel reset, \
             or {device} alone to drop the entire device.",
        );
    };

    // Accept either `device` (documented primary name) or `device_id`
    // (commonly used across other MCP tools — list_device_bindings, monitor
    // events, etc.). `device` wins if both are provided.
    let Some(device) = args
        .get("device")
        .and_then(|v| v.as_str())
        .or_else(|| args.get("device_id").and_then(|v| v.as_str()))
    else {
        return ToolCallResult::error("Missing required argument: device (or device_id)");
    };

    // Channel-level reset if channel is provided. Otherwise drop the device.
    match args.get("channel").and_then(|v| v.as_u64()) {
        Some(channel) => {
            if channel > 15 {
                return ToolCallResult::error(&format!(
                    "channel out of range: {} (must be 0-15)",
                    channel
                ));
            }
            let scope_str = args.get("scope").and_then(|v| v.as_str()).unwrap_or("all");
            let scope = match scope_str {
                "all_sound_off" | "cc120" => ResetScope::AllSoundOff,
                "reset_all_controllers" | "cc121" => ResetScope::ResetAllControllers,
                "all_notes_off" | "cc123" => ResetScope::AllNotesOff,
                "all" | "channel" => ResetScope::DropDevice,
                other => {
                    return ToolCallResult::error(&format!(
                        "unknown scope '{}'. Valid: all_sound_off, reset_all_controllers, \
                         all_notes_off, all",
                        other
                    ));
                }
            };
            store.clear_channel(device, channel as u8, scope);
            ToolCallResult::json(&json!({
                "status": "cleared",
                "target": "channel",
                "device": device,
                "channel": channel,
                "scope": scope_str,
                "store_only": true,
                "note": "Does not emit outbound MIDI. Use conductor_send_midi / conductor_send_sysex for hardware panic.",
            }))
        }
        None => {
            store.drop_device(device);
            ToolCallResult::json(&json!({
                "status": "cleared",
                "target": "device",
                "device": device,
                "store_only": true,
                "note": "Does not emit outbound MIDI. Use conductor_send_midi / conductor_send_sysex for hardware panic.",
            }))
        }
    }
}

// ────────────────────────────────────────────────────────────────

fn serialize_entry((key, value): (StateKey, conductor_core::control_state::ControlValue)) -> Value {
    let (kind, device, channel, extra) = match key {
        StateKey::ProgramChange { device, channel } => ("program_change", device, channel, None),
        StateKey::ControlChange {
            device,
            channel,
            cc,
        } => ("control_change", device, channel, Some(("cc", json!(cc)))),
        StateKey::NoteHeld {
            device,
            channel,
            note,
        } => ("note_held", device, channel, Some(("note", json!(note)))),
        StateKey::PolyAftertouch {
            device,
            channel,
            note,
        } => (
            "poly_aftertouch",
            device,
            channel,
            Some(("note", json!(note))),
        ),
        StateKey::ChannelAftertouch { device, channel } => {
            ("channel_aftertouch", device, channel, None)
        }
        StateKey::PitchBend { device, channel } => ("pitch_bend", device, channel, None),
    };

    let mut obj = json!({
        "kind": kind,
        "device": device,
        "channel": channel,
        "value": value.value,
    });
    if let Some((k, v)) = extra {
        obj.as_object_mut().unwrap().insert(k.to_string(), v);
    }
    obj
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::mcp_types::ToolContent;
    use conductor_core::event_processor::MidiEvent;
    use std::time::Instant;

    fn mk_store() -> PhysicalControlStateStore {
        PhysicalControlStateStore::default()
    }

    fn text_of(result: &ToolCallResult) -> String {
        result
            .content
            .iter()
            .filter_map(|c| match c {
                ToolContent::Text { text } => Some(text.as_str()),
                ToolContent::Resource { text, .. } => text.as_deref(),
                ToolContent::Image { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    fn json_of(result: &ToolCallResult) -> Value {
        let raw = text_of(result);
        serde_json::from_str(&raw).unwrap_or_else(|e| panic!("not JSON: {} — body: {}", e, raw))
    }

    fn pc(program: u8, channel: u8) -> MidiEvent {
        MidiEvent::ProgramChange {
            program,
            channel,
            time: Instant::now(),
        }
    }

    fn cc(cc: u8, value: u8, channel: u8) -> MidiEvent {
        MidiEvent::ControlChange {
            cc,
            value,
            channel,
            time: Instant::now(),
        }
    }

    fn note_on(note: u8, velocity: u8, channel: u8) -> MidiEvent {
        MidiEvent::NoteOn {
            note,
            velocity,
            channel,
            time: Instant::now(),
        }
    }

    // ────────────────────────────────────────────────────────────
    // handle_readonly routing
    // ────────────────────────────────────────────────────────────

    #[test]
    fn handle_readonly_returns_none_for_unknown_tool() {
        let store = mk_store();
        assert!(handle_readonly("not_our_tool", None, Some(&store)).is_none());
    }

    #[test]
    fn handle_stateful_returns_none_for_unknown_tool() {
        let store = mk_store();
        assert!(handle_stateful("not_our_tool", None, Some(&store)).is_none());
    }

    // ────────────────────────────────────────────────────────────
    // conductor_get_control_state
    // ────────────────────────────────────────────────────────────

    #[test]
    fn get_control_state_returns_error_when_store_missing() {
        let result = handle_readonly("conductor_get_control_state", None, None).unwrap();
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn get_control_state_returns_empty_when_no_events() {
        let store = mk_store();
        let result = handle_readonly("conductor_get_control_state", None, Some(&store)).unwrap();
        let body = json_of(&result);
        assert_eq!(body["total"].as_u64().unwrap(), 0);
    }

    #[test]
    fn get_control_state_includes_pc_cc_notehold() {
        let store = mk_store();
        store.observe_event("fcb1010", &pc(12, 0));
        store.observe_event("fcb1010", &cc(7, 100, 0));
        store.observe_event("k1", &note_on(60, 100, 0));

        let result = handle_readonly("conductor_get_control_state", None, Some(&store)).unwrap();
        let body = json_of(&result);
        let kinds: Vec<&str> = body["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["kind"].as_str().unwrap())
            .collect();
        assert!(kinds.contains(&"program_change"));
        assert!(kinds.contains(&"control_change"));
        assert!(kinds.contains(&"note_held"));
    }

    #[test]
    fn get_control_state_filters_by_device() {
        let store = mk_store();
        store.observe_event("fcb1010", &pc(12, 0));
        store.observe_event("mc6", &pc(42, 0));

        let args = json!({"device": "fcb1010"});
        let result =
            handle_readonly("conductor_get_control_state", Some(&args), Some(&store)).unwrap();
        let body = json_of(&result);
        let devices: Vec<&str> = body["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["device"].as_str().unwrap())
            .collect();
        assert!(devices.contains(&"fcb1010"));
        assert!(!devices.contains(&"mc6"));
    }

    // ────────────────────────────────────────────────────────────
    // conductor_get_active_pc
    // ────────────────────────────────────────────────────────────

    #[test]
    fn get_active_pc_returns_only_program_changes() {
        let store = mk_store();
        store.observe_event("fcb1010", &pc(12, 0));
        store.observe_event("fcb1010", &cc(7, 100, 0));
        store.observe_event("fcb1010", &note_on(60, 100, 0));

        let result = handle_readonly("conductor_get_active_pc", None, Some(&store)).unwrap();
        let body = json_of(&result);
        let pcs = body["program_changes"].as_array().unwrap();
        assert_eq!(pcs.len(), 1);
        assert_eq!(pcs[0]["pc"].as_u64().unwrap(), 12);
        assert_eq!(pcs[0]["device"].as_str().unwrap(), "fcb1010");
        assert_eq!(pcs[0]["channel"].as_u64().unwrap(), 0);
    }

    #[test]
    fn get_active_pc_empty_when_no_pc_received() {
        let store = mk_store();
        store.observe_event("fcb1010", &cc(7, 100, 0));
        let result = handle_readonly("conductor_get_active_pc", None, Some(&store)).unwrap();
        let body = json_of(&result);
        assert!(body["program_changes"].as_array().unwrap().is_empty());
    }

    // ────────────────────────────────────────────────────────────
    // conductor_reset_control_state — STORE-ONLY
    // ────────────────────────────────────────────────────────────

    #[test]
    fn reset_control_state_requires_device() {
        let store = mk_store();
        let args = json!({});
        let result =
            handle_stateful("conductor_reset_control_state", Some(&args), Some(&store)).unwrap();
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn reset_control_state_drop_device() {
        let store = mk_store();
        store.observe_event("fcb1010", &pc(12, 0));
        store.observe_event("fcb1010", &cc(7, 100, 0));
        store.observe_event("mc6", &pc(42, 0)); // survives

        let args = json!({"device": "fcb1010"});
        let result =
            handle_stateful("conductor_reset_control_state", Some(&args), Some(&store)).unwrap();
        assert_eq!(result.is_error, None);

        assert!(
            store
                .get(&StateKey::ProgramChange {
                    device: "fcb1010".into(),
                    channel: 0
                })
                .is_none()
        );
        assert!(
            store
                .get(&StateKey::ProgramChange {
                    device: "mc6".into(),
                    channel: 0
                })
                .is_some()
        );
    }

    #[test]
    fn reset_control_state_channel_scope_all_notes_off() {
        let store = mk_store();
        store.observe_event("k1", &note_on(60, 100, 0));
        store.observe_event("k1", &cc(7, 100, 0));
        store.observe_event("k1", &note_on(72, 100, 3)); // other channel survives

        let args = json!({"device": "k1", "channel": 0, "scope": "all_notes_off"});
        let result =
            handle_stateful("conductor_reset_control_state", Some(&args), Some(&store)).unwrap();
        assert_eq!(result.is_error, None);

        assert!(
            store
                .get(&StateKey::NoteHeld {
                    device: "k1".into(),
                    channel: 0,
                    note: 60
                })
                .is_none()
        );
        assert!(
            store
                .get(&StateKey::ControlChange {
                    device: "k1".into(),
                    channel: 0,
                    cc: 7
                })
                .is_some()
        );
        assert!(
            store
                .get(&StateKey::NoteHeld {
                    device: "k1".into(),
                    channel: 3,
                    note: 72
                })
                .is_some()
        );
    }

    #[test]
    fn reset_control_state_rejects_bad_channel() {
        let store = mk_store();
        let args = json!({"device": "k1", "channel": 42});
        let result =
            handle_stateful("conductor_reset_control_state", Some(&args), Some(&store)).unwrap();
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn reset_control_state_accepts_device_id_alias() {
        // Every other MCP tool in the codebase refers to "device_id"
        // (conductor_list_device_bindings, monitor events, etc.) so LLMs
        // reasonably try that arg name. Accept it as an alias for "device"
        // instead of erroring out.
        let store = mk_store();
        store.observe_event("fcb1010", &pc(12, 0));

        let args = json!({"device_id": "fcb1010"});
        let result =
            handle_stateful("conductor_reset_control_state", Some(&args), Some(&store)).unwrap();
        assert_eq!(result.is_error, None, "device_id alias should work");

        assert!(
            store
                .get(&StateKey::ProgramChange {
                    device: "fcb1010".into(),
                    channel: 0,
                })
                .is_none(),
            "state should be cleared after reset via device_id"
        );
    }

    #[test]
    fn reset_control_state_device_preferred_over_device_id_when_both_given() {
        // If both are supplied, `device` wins — it's the documented name.
        let store = mk_store();
        store.observe_event("fcb1010", &pc(12, 0));
        store.observe_event("mc6", &pc(42, 0));

        let args = json!({"device": "fcb1010", "device_id": "mc6"});
        let _ =
            handle_stateful("conductor_reset_control_state", Some(&args), Some(&store)).unwrap();

        assert!(
            store
                .get(&StateKey::ProgramChange {
                    device: "fcb1010".into(),
                    channel: 0,
                })
                .is_none(),
            "fcb1010 should be cleared (device took precedence)"
        );
        assert!(
            store
                .get(&StateKey::ProgramChange {
                    device: "mc6".into(),
                    channel: 0,
                })
                .is_some(),
            "mc6 should be untouched (device_id ignored when device is present)"
        );
    }

    #[test]
    fn reset_control_state_rejects_unknown_scope() {
        let store = mk_store();
        let args = json!({"device": "k1", "channel": 0, "scope": "smash_everything"});
        let result =
            handle_stateful("conductor_reset_control_state", Some(&args), Some(&store)).unwrap();
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn reset_control_state_response_advertises_store_only() {
        // Contract: the response MUST say it doesn't emit MIDI. LLMs
        // reading the tool output rely on this to know they also need
        // to send a hardware panic if they want the device reset too.
        let store = mk_store();
        let args = json!({"device": "k1"});
        let result =
            handle_stateful("conductor_reset_control_state", Some(&args), Some(&store)).unwrap();
        let body = json_of(&result);
        assert!(body["store_only"].as_bool().unwrap());
        assert!(
            body["note"]
                .as_str()
                .unwrap()
                .contains("Does not emit outbound MIDI"),
            "body: {:?}",
            body
        );
    }
}
