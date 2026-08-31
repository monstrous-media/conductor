// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Stateful/ConfigChange/HardwareIO/ArtifactRender MCP tool definitions.
//! Split out of `mcp_tools.rs` in #2601 (file exceeded the review window).

use super::super::mcp_types::ToolDefinition;
use serde_json::json;

pub(super) fn write_tier_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "conductor_create_endpoint".to_string(),
            description: "Create a unified I/O endpoint in [[endpoints]] (ADR-035 — the single schema that supersedes the removed [[bindings]] and [[connectors]] blocks). Returns a ConfigPlan requiring user approval. `direction` is REQUIRED (Input/Output/Bidirectional). Alias must be unique across all endpoints. `type` selects the kind and its fields: { type: 'Matcher', matchers?: [...], input_matchers?: [...], output_matchers?: [...], no_probe?: bool } (at least one matcher list must be non-empty) | { type: 'OscEndpoint', host: string, port: int } | { type: 'ArtNetEndpoint', universe: 0-32767, host?: string, port?: int } | { type: 'MidiVirtualPort', port_name: string } (DAW proxy — daemon creates a system-visible MIDI port). `protocol` is optional (inferred from `type` when omitted).".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "alias": {
                        "type": "string",
                        "description": "Unique alias (must not collide with any binding/connector/endpoint alias)"
                    },
                    "direction": {
                        "type": "string",
                        "enum": ["Input", "Output", "Bidirectional"],
                        "description": "REQUIRED. Endpoint direction (no default — ADR-035 §4.1)."
                    },
                    "type": {
                        "type": "string",
                        "enum": ["Matcher", "OscEndpoint", "ArtNetEndpoint", "MidiVirtualPort"],
                        "description": "Endpoint kind — determines which additional fields apply (see top-level description)."
                    },
                    "protocol": {
                        "type": "string",
                        "enum": ["Midi", "Osc", "ArtNet", "Hid"],
                        "description": "Optional. Inferred from `type` when omitted (Osc/ArtNet → matching protocol; Matcher/MidiVirtualPort → Midi)."
                    },
                    "matchers": {
                        "type": "array",
                        "description": "Matcher kind: symmetric matchers (used for both directions, or the only direction)."
                    },
                    "input_matchers": {
                        "type": "array",
                        "description": "Matcher kind: asymmetric input-side matchers (Bidirectional whose input port differs from output)."
                    },
                    "output_matchers": {
                        "type": "array",
                        "description": "Matcher kind: asymmetric output-side matchers."
                    },
                    "no_probe": {
                        "type": "boolean",
                        "description": "Matcher kind: suppress auto-probe-on-connect (default false)."
                    },
                    "host": {
                        "type": "string",
                        "description": "OscEndpoint / ArtNetEndpoint: destination host."
                    },
                    "port": {
                        "type": "integer",
                        "description": "OscEndpoint / ArtNetEndpoint: destination UDP port."
                    },
                    "universe": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": 32767,
                        "description": "ArtNetEndpoint: DMX universe."
                    },
                    "port_name": {
                        "type": "string",
                        "description": "MidiVirtualPort: name of the virtual MIDI port to create."
                    },
                    "description": {
                        "type": "string",
                        "description": "Optional human-readable description"
                    },
                    "enabled": {
                        "type": "boolean",
                        "description": "Whether this endpoint is active (default: true)"
                    },
                    "channels": {
                        "type": "array",
                        "items": { "type": "integer", "minimum": 0, "maximum": 15 },
                        "description": "Optional MIDI channel scope (0-15, 0-indexed). Empty = all channels."
                    }
                },
                "required": ["alias", "direction", "type"]
            }),
        },
        // ConfigChange tools (Phase 2) - Return ConfigPlan for user approval
        ToolDefinition {
            name: "conductor_create_mapping".to_string(),
            description: "Create a new mapping in a mode. Returns a ConfigPlan requiring user approval.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "mode": {
                        "type": "string",
                        "description": "Name of the mode to add the mapping to"
                    },
                    "trigger": {
                        "type": "object",
                        "description": "Trigger configuration. All triggers accept optional 'device' (string, device alias). Valid types: Note {note: 0-127, velocity_min?: 0-127}, CC {cc: 0-127, value_min?: 0-127}, VelocityRange {note: 0-127, soft_max?: 0-127, medium_max?: 0-127}, LongPress {note: 0-127, duration_ms?: ms}, DoubleTap {note: 0-127, timeout_ms?: ms}, NoteChord {notes: [0-127, ...], timeout_ms?: ms}, EncoderTurn {cc: 0-127, direction?: 'Clockwise'|'CounterClockwise'}, Aftertouch {pressure_min?: 0-127}, PitchBend {value_min?: 0-16383, value_max?: 0-16383}, ProgramChange {pc?: 0-127 (omit to match any program), channel?: 0-15}, GamepadButton {button: 128-144, velocity_min?: 0-127}, GamepadButtonChord {buttons: [...], timeout_ms?: ms}, GamepadAnalogStick {axis: 128-131, direction?: 'Clockwise'|'CounterClockwise'}, GamepadTrigger {trigger: 132-133, threshold?: 0-255}, OscMessage {address: '/exact/osc/address'}, OscAddressPattern {pattern: '/osc/1.0/glob/* with ? [] {}'}, OscArgRange {arg_index: 0-63, min: f32, max: f32}. OSC triggers fire from OSC listener endpoints and carry a network-origin taint: sensitive actions (Shell/Launch/Keystroke, incl. nested) are refused unless the listener sets allow_sensitive_actions = true (ADR-042 D17)."
                    },
                    "action": {
                        "type": "object",
                        "description": "Action configuration. Valid types: Keystroke {keys, modifiers}, Text {text}, Launch {app}, Shell {command, args?, timeout_ms?}, VolumeControl {operation: Up|Down|Mute|Unmute|Set, value?: 0-100 (only for Set)}, ModeChange {mode}, SendMidi {port, message_type, channel, ...}, MidiForward {target: '<device-alias>' | '<port-name>' | '_source', transform?: {channel?, cc?, note?, velocity_scale?, velocity_offset?, invert_value?, curve?}} — forwards the triggering event's raw MIDI bytes to the target output; '_source' echoes back to the originating device. HidForward {target: '<midi-output-alias>', transform: {type: 'HidToMidi', trigger_to_cc: {<gamepad-trigger>: <cc>}, channel?}} — on a gamepad-triggered mapping ONLY, translates the structured gamepad event to a CC and sends it to the MIDI output target (ADR-039-B; for HID→OSC/Art-Net use a route). OscSend {host, port, address, args}, OscForward {target: '<osc-output-alias>'} — on an OSC-triggered mapping ONLY, re-sends the inbound OSC message verbatim to the target OSC output endpoint (ADR-039-A Slice 3; pass-through, no transform in V1), Sequence {actions[]}, Delay {ms}, Tap {message} — ADR-038 observation action; emits `message` (with {note}/{velocity}/{cc}/{value} substitution) to the event stream/trace without intercepting. Pair with let_through=true to observe an event while still letting it through."
                    },
                    "description": {
                        "type": "string",
                        "description": "Optional human-readable description of the mapping"
                    },
                    "let_through": {
                        "type": "boolean",
                        "description": "ADR-038: when true, fire the action AND let the event continue to the route stage (default false = consume/swallow, the pre-ADR-038 behaviour). Pair with a Tap action to observe-and-pass-through, or with any action to forward the raw event to routes while also acting on it. Rejected at validation if set on an exclusively-HID/gamepad mapping (routes are MIDI-only today)."
                    }
                },
                "required": ["mode", "trigger", "action"]
            }),
        },
        ToolDefinition {
            name: "conductor_update_mapping".to_string(),
            description: "Update an existing mapping in a mode. Returns a ConfigPlan requiring user approval.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "mode": {
                        "type": "string",
                        "description": "Name of the mode containing the mapping"
                    },
                    "index": {
                        "type": "integer",
                        "description": "Zero-based index of the mapping to update"
                    },
                    "trigger": {
                        "type": "object",
                        "description": "Trigger configuration. All triggers accept optional 'device' (string, device alias). Valid types: Note {note: 0-127, velocity_min?: 0-127}, CC {cc: 0-127, value_min?: 0-127}, VelocityRange {note: 0-127, soft_max?: 0-127, medium_max?: 0-127}, LongPress {note: 0-127, duration_ms?: ms}, DoubleTap {note: 0-127, timeout_ms?: ms}, NoteChord {notes: [0-127, ...], timeout_ms?: ms}, EncoderTurn {cc: 0-127, direction?: 'Clockwise'|'CounterClockwise'}, Aftertouch {pressure_min?: 0-127}, PitchBend {value_min?: 0-16383, value_max?: 0-16383}, ProgramChange {pc?: 0-127 (omit to match any program), channel?: 0-15}, GamepadButton {button: 128-144, velocity_min?: 0-127}, GamepadButtonChord {buttons: [...], timeout_ms?: ms}, GamepadAnalogStick {axis: 128-131, direction?: 'Clockwise'|'CounterClockwise'}, GamepadTrigger {trigger: 132-133, threshold?: 0-255}, OscMessage {address: '/exact/osc/address'}, OscAddressPattern {pattern: '/osc/1.0/glob/* with ? [] {}'}, OscArgRange {arg_index: 0-63, min: f32, max: f32}. OSC triggers fire from OSC listener endpoints and carry a network-origin taint: sensitive actions (Shell/Launch/Keystroke, incl. nested) are refused unless the listener sets allow_sensitive_actions = true (ADR-042 D17)."
                    },
                    "action": {
                        "type": "object",
                        "description": "Action configuration. Valid types: Keystroke {keys, modifiers}, Text {text}, Launch {app}, Shell {command, args?, timeout_ms?}, VolumeControl {operation: Up|Down|Mute|Unmute|Set, value?: 0-100 (only for Set)}, ModeChange {mode}, SendMidi {port, message_type, channel, ...}, MidiForward {target: '<device-alias>' | '<port-name>' | '_source', transform?: {channel?, cc?, note?, velocity_scale?, velocity_offset?, invert_value?, curve?}} — forwards the triggering event's raw MIDI bytes to the target output; '_source' echoes back to the originating device. HidForward {target: '<midi-output-alias>', transform: {type: 'HidToMidi', trigger_to_cc: {<gamepad-trigger>: <cc>}, channel?}} — on a gamepad-triggered mapping ONLY, translates the structured gamepad event to a CC and sends it to the MIDI output target (ADR-039-B; for HID→OSC/Art-Net use a route). OscSend {host, port, address, args}, OscForward {target: '<osc-output-alias>'} — on an OSC-triggered mapping ONLY, re-sends the inbound OSC message verbatim to the target OSC output endpoint (ADR-039-A Slice 3; pass-through, no transform in V1), Sequence {actions[]}, Delay {ms}."
                    },
                    "description": {
                        "type": "string",
                        "description": "Optional human-readable description"
                    }
                },
                "required": ["mode", "index", "trigger", "action"]
            }),
        },
        ToolDefinition {
            name: "conductor_delete_mapping".to_string(),
            description: "Delete a mapping from a mode. Returns a ConfigPlan requiring user approval.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "mode": {
                        "type": "string",
                        "description": "Name of the mode containing the mapping"
                    },
                    "index": {
                        "type": "integer",
                        "description": "Zero-based index of the mapping to delete"
                    }
                },
                "required": ["mode", "index"]
            }),
        },
        // Batch operations (P3-07; ADR-031 P3 slice 12 = gap F — first
        // publication of `conductor_batch_changes` on the daemon's MCP
        // socket. Until this slice the tool existed only in the
        // executor's dispatch (line ~2373 of this file) + the GUI-side
        // duplicate at `conductor-gui/src-tauri/src/llm_commands.rs`
        // (issue #1138 tracks deduplicating the two schemas). External
        // MCP clients calling `tools/list` couldn't see this tool
        // before slice 12 — so they could `conductor_list_routes` but
        // had no way to author route mutations.
        //
        // Schema mirrors the GUI-side duplicate (issue #1138). ADR-035
        // Phase 2 #1748 removed the connector ops (create/update/delete_connector)
        // — endpoints are authored via the singleton conductor_create_endpoint.
        ToolDefinition {
            name: "conductor_batch_changes".to_string(),
            description: "Execute multiple configuration changes atomically. Supports 8 operation types: create_mapping, update_mapping, delete_mapping, create_mode (name + optional color), delete_mode (name), create_route, update_route, delete_route. Route mutations go through batch_changes per ADR-031 § 5.4 (no singleton tools by design). I/O endpoints are authored with the singleton conductor_create_endpoint tool (ADR-035). Use this to create new modes, delete modes, or perform multiple changes in one transaction. All changes succeed or none are applied. Returns a ConfigPlan requiring user approval.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "operations": {
                        "type": "array",
                        "description": "Array of operations to execute atomically",
                        "items": {
                            "type": "object",
                            "description": "A single operation. Mapping/mode ops: create_mapping, update_mapping, delete_mapping, create_mode, delete_mode. Route ops (ADR-031 § 5.4): create_route {from, to, filter?, transform?, enabled?, description?}; update_route {index, from, to, filter?, transform?, enabled?, description?} — total-replaces the entry at the 0-based index; delete_route {index} — 0-based.",
                            "properties": {
                                "type": {
                                    "type": "string",
                                    "enum": ["create_mapping", "update_mapping", "delete_mapping", "create_mode", "delete_mode", "create_route", "update_route", "delete_route"],
                                    "description": "Type of operation"
                                },
                                "mode": { "type": "string", "description": "Mode name (for mapping operations)" },
                                "name": { "type": "string", "description": "Name (for mode operations)" },
                                "index": { "type": "integer", "description": "0-based index (for update/delete mapping; also for update_route/delete_route into the [[routes]] array)" },
                                "trigger": { "type": "object", "description": "Trigger configuration (for create/update mapping). All triggers accept optional 'device' (string). Valid types: Note {note: 0-127, velocity_min?: 0-127}, CC {cc: 0-127, value_min?: 0-127}, VelocityRange {note: 0-127, soft_max?: 0-127, medium_max?: 0-127}, LongPress {note: 0-127, duration_ms?: ms}, DoubleTap {note: 0-127, timeout_ms?: ms}, NoteChord {notes: [0-127, ...], timeout_ms?: ms}, EncoderTurn {cc: 0-127, direction?: 'Clockwise'|'CounterClockwise'}, Aftertouch {pressure_min?: 0-127}, PitchBend {value_min?: 0-16383, value_max?: 0-16383}, GamepadButton {button: 128-144, velocity_min?: 0-127}, GamepadButtonChord {buttons: [...], timeout_ms?: ms}, GamepadAnalogStick {axis: 128-131}, GamepadTrigger {trigger: 132-133, threshold?: 0-255}, OscMessage {address: '/exact/osc/address'}, OscAddressPattern {pattern: '/osc/1.0/glob/* with ? [] {}'}, OscArgRange {arg_index: 0-63, min: f32, max: f32}. OSC triggers fire from OSC listener endpoints and carry a network-origin taint: sensitive actions (Shell/Launch/Keystroke, incl. nested) are refused unless the listener sets allow_sensitive_actions = true (ADR-042 D17)." },
                                "action": { "type": "object", "description": "Action configuration (for create/update mapping). Valid types: Keystroke, Text, Launch, Shell, VolumeControl, ModeChange, SendMidi, Sequence, Delay. SendMidi message_type must be one of: NoteOn, NoteOff, CC, ProgramChange, PitchBend, Aftertouch." },
                                "description": { "type": "string", "description": "Optional description" },
                                "color": { "type": "string", "description": "Mode color (for create_mode)" },
                                "from": { "type": "string", "description": "Source endpoint alias (for create_route/update_route)" },
                                "to": { "type": "string", "description": "Destination endpoint alias (for create_route/update_route)" },
                                "filter": { "type": "object", "description": "Optional SignalFilter for create_route/update_route — fields: note_range [lo, hi], cc_range [lo, hi], channels [0-15, ...], message_types ['NoteOn', 'NoteOff', 'CC', ...]. Filters AND-combine." },
                                "transform": { "type": "object", "description": "Optional SignalTransform for create_route/update_route — same-protocol channel/CC/note remap via Midi(MidiTransform), or cross-protocol MidiToOsc/OscToMidi (latter wired in ADR-031 P5)." },
                                "enabled": { "type": "boolean", "description": "Whether the route is enabled (default true)" }
                            },
                            "required": ["type"]
                        }
                    }
                },
                "required": ["operations"]
            }),
        },
        // ADR-025 Phase 2.H: context-switch authoring (ConfigChange tier)
        ToolDefinition {
            name: "conductor_set_context_mapping".to_string(),
            description: "Create a new mapping whose action routes based on prior MIDI state (ADR-025). The `action` MUST be PcContextSwitch or CcContextSwitch — other action shapes are rejected; use conductor_create_mapping for those. Returns a ConfigPlan requiring user approval. PcContextSwitch routes by the most-recent Program Change on (device, channel); shape is `{ type: 'PcContextSwitch', channel, device, mappings: { '<pc>': <ActionConfig>, ... }, default?: <ActionConfig> }`. CcContextSwitch routes by the most-recent CC value on (device, channel, cc); shape is `{ type: 'CcContextSwitch', cc, channel, device, ranges: [{ min, max, action }, ...], default?: <ActionConfig> }`. The `device` field must match a declared `[[endpoints]]` alias (ADR-035 — `[[bindings]]`/`[[connectors]]` were removed in Phase 2). Typical use: one-pedal-many-functions foot controllers (FCB1010) or per-scene CC authoring.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "mode": {
                        "type": "string",
                        "description": "Name of the mode to add the mapping to"
                    },
                    "trigger": {
                        "type": "object",
                        "description": "Trigger configuration (same shape as conductor_create_mapping). Typically a CC or Note trigger from the pedal/key that should be routed."
                    },
                    "action": {
                        "type": "object",
                        "description": "Must be a PcContextSwitch or CcContextSwitch ActionConfig. See the tool description for the exact shape."
                    },
                    "description": {
                        "type": "string",
                        "description": "Optional human-readable description"
                    }
                },
                "required": ["mode", "trigger", "action"]
            }),
        },
        // Stateful tools (Phase 2) - Execute with logging
        ToolDefinition {
            name: "conductor_start_learn".to_string(),
            description: "Start Learn mode to capture controller input (MIDI or HID). Returns immediately and logs execution.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "timeout_seconds": {
                        "type": "integer",
                        "description": "Auto-stop timeout in seconds (default: 30)"
                    }
                },
                "required": []
            }),
        },
        ToolDefinition {
            name: "conductor_stop_learn".to_string(),
            description: "Stop Learn mode and return captured events with suggested trigger.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        // LLM Editor tools (ADR-017 Phase 2C)
        ToolDefinition {
            name: "conductor_set_mapping_editor".to_string(),
            description: "Open the MappingEditor with pre-filled trigger, action, and description. The GUI will switch to the editor view with the provided data populated.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "trigger": { "type": "object", "description": "Trigger config (type, note, cc, etc.)" },
                    "action": { "type": "object", "description": "Action config (type, keys, command, etc.)" },
                    "description": { "type": "string", "description": "Mapping description" },
                    "mode": { "type": "string", "description": "Target mode name" }
                },
                "required": []
            }),
        },
        ToolDefinition {
            name: "conductor_update_mapping_editor".to_string(),
            description: "Update one or more fields in the currently open MappingEditor. Use after conductor_set_mapping_editor to modify fields.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "fields": {
                        "type": "object",
                        "description": "Map of field names to new values. Valid keys: trigger (object), action (object), description (string), mode (string). Only specified fields are updated.",
                        "properties": {
                            "trigger": { "type": "object", "description": "Trigger config" },
                            "action": { "type": "object", "description": "Action config" },
                            "description": { "type": "string", "description": "Mapping description" },
                            "mode": { "type": "string", "description": "Target mode name" }
                        },
                        "additionalProperties": false
                    }
                },
                "required": ["fields"]
            }),
        },
        // HardwareIO tools (Phase 4) - require multi-step confirmation
        ToolDefinition {
            name: "conductor_send_sysex".to_string(),
            description: "Send a System Exclusive (SysEx) message to a MIDI device. DANGEROUS: Requires multi-step confirmation. Some SysEx messages can modify device firmware or settings.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "device": {
                        "type": "string",
                        "description": "Name or index of the MIDI output device"
                    },
                    "data": {
                        "type": "array",
                        "items": { "type": "integer", "minimum": 0, "maximum": 255 },
                        "description": "SysEx data bytes (excluding F0 start and F7 end bytes)"
                    },
                    "confirmation_token": {
                        "type": "string",
                        "description": "Confirmation token from previous request (required for execution)"
                    }
                },
                "required": ["device", "data"]
            }),
        },
        ToolDefinition {
            name: "conductor_device_reset".to_string(),
            description: "Send a device reset command to restore factory/default state. DANGEROUS: Requires multi-step confirmation.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "device": {
                        "type": "string",
                        "description": "Name or index of the MIDI device to reset"
                    },
                    "reset_type": {
                        "type": "string",
                        "enum": ["soft", "hard", "factory"],
                        "description": "Type of reset: soft (controller reset), hard (full reset), factory (restore factory settings)"
                    },
                    "confirmation_token": {
                        "type": "string",
                        "description": "Confirmation token from previous request (required for execution)"
                    }
                },
                "required": ["device", "reset_type"]
            }),
        },
        // Stateful: Switch mode (v4.26.69)
        ToolDefinition {
            name: "conductor_switch_mode".to_string(),
            description: "DEPRECATED (ADR-040): prefer conductor_set_mode. Switches the active mapping mode by name WITHOUT acquiring or releasing a manual lock; if a mode is locked, use conductor_unlock_mode first or conductor_set_mode to relock.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "mode": {
                        "type": "string",
                        "description": "Name of the mode to switch to"
                    }
                },
                "required": ["mode"]
            }),
        },
        // ADR-040 D4 §4.2 (Slice 4c): mode set + lock / unlock / status.
        ToolDefinition {
            name: "conductor_set_mode".to_string(),
            description: "Set the active mapping mode by name and (by default) lock it against automatic per-app/window switching. Pass lock=false to switch without locking. Prefer this over conductor_switch_mode.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "mode": { "type": "string", "description": "Name of the mode to activate" },
                    "lock": { "type": "boolean", "description": "Lock the mode against auto-switching (default true)" }
                },
                "required": ["mode"]
            }),
        },
        ToolDefinition {
            name: "conductor_unlock_mode".to_string(),
            description: "Release the manual mode lock, resuming automatic per-app/window mode switching.".to_string(),
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        // Stateful: Switch profile (Phase 1 - Issue #323)
        ToolDefinition {
            name: "conductor_switch_profile".to_string(),
            description: "Switch the active profile by name and config path. The profile's config will be hot-loaded into the daemon.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "profile_name": {
                        "type": "string",
                        "description": "Display name of the profile to switch to"
                    },
                    "config_path": {
                        "type": "string",
                        "description": "Absolute path to the profile's TOML config file"
                    }
                },
                "required": ["profile_name", "config_path"]
            }),
        },
        // HardwareIO: Send MIDI (v4.26.67)
        ToolDefinition {
            name: "conductor_send_midi".to_string(),
            description: "Send MIDI messages to a connected output port. Low risk: standard MIDI messages (note, CC, program change) auto-confirm.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "port": {
                        "type": "string",
                        "description": "MIDI output port name"
                    },
                    "messages": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "type": { "type": "string", "enum": ["note_on", "note_off", "cc", "program_change"], "description": "Message type" },
                                "channel": { "type": "integer", "minimum": 1, "maximum": 16, "description": "MIDI channel (1-16)" },
                                "note": { "type": "integer", "minimum": 0, "maximum": 127, "description": "Note number (for note_on/note_off)" },
                                "velocity": { "type": "integer", "minimum": 0, "maximum": 127, "description": "Velocity (for note_on/note_off, default 100)" },
                                "controller": { "type": "integer", "minimum": 0, "maximum": 127, "description": "CC number (for cc)" },
                                "value": { "type": "integer", "minimum": 0, "maximum": 127, "description": "CC value (for cc)" },
                                "program": { "type": "integer", "minimum": 0, "maximum": 127, "description": "Program number (for program_change)" }
                            },
                            "required": ["type", "channel"]
                        },
                        "description": "Array of MIDI messages to send"
                    }
                },
                "required": ["port", "messages"]
            }),
        },
        // Artifact tools (ADR-013 Phase 1C)
        ToolDefinition {
            name: "conductor_render_artifact".to_string(),
            description: "Render an artifact in the workspace canvas. Creates a visual overlay with the specified type and data.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "artifact_type": { "type": "string", "description": "Registered artifact type (e.g., 'mapping-editor', 'config-diff')" },
                    "title": { "type": "string", "description": "Display title" },
                    "data": { "type": "object", "description": "Type-specific data payload" }
                },
                "required": ["artifact_type", "title"]
            }),
        },
        ToolDefinition {
            name: "conductor_dismiss_artifact".to_string(),
            description: "Dismiss (close) an artifact by its ID.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "artifact_id": { "type": "string", "description": "ID of the artifact to dismiss" }
                },
                "required": ["artifact_id"]
            }),
        },
        ToolDefinition {
            name: "conductor_set_device_enabled".to_string(),
            description: "Enable or disable (mute/unmute) a specific device by its device_id".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "device_id": {
                        "type": "string",
                        "description": "The device_id to enable or disable (e.g. 'pads', 'keys')"
                    },
                    "enabled": {
                        "type": "boolean",
                        "description": "true to enable (unmute), false to disable (mute)"
                    }
                },
                "required": ["device_id", "enabled"]
            }),
        },
        ToolDefinition {
            name: "conductor_scan_ports".to_string(),
            description: "Trigger an immediate port rescan to detect newly connected or disconnected MIDI devices".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        ToolDefinition {
            name: "conductor_enable_plugin".to_string(),
            description: "Enable a plugin by name".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Plugin name to enable"
                    }
                },
                "required": ["name"]
            }),
        },
        ToolDefinition {
            name: "conductor_disable_plugin".to_string(),
            description: "Disable a plugin by name".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Plugin name to disable"
                    }
                },
                "required": ["name"]
            }),
        },
        // Deprecated aliases for Learn tools — kept so older LLM prompts still resolve
        ToolDefinition {
            name: "conductor_start_midi_learn".to_string(),
            description: "DEPRECATED: Use conductor_start_learn instead. Start Learn mode to capture controller input.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "timeout_seconds": {
                        "type": "integer",
                        "description": "Auto-stop timeout in seconds (default: 30)"
                    }
                },
                "required": []
            }),
        },
        ToolDefinition {
            name: "conductor_stop_midi_learn".to_string(),
            description: "DEPRECATED: Use conductor_stop_learn instead. Stop Learn mode and return captured events.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        // SysEx identity probing (ADR-026 Phase 2)
        ToolDefinition {
            name: "conductor_probe_device_identity".to_string(),
            description: "Send a Universal SysEx Identity Request (F0 7E 7F 06 01 F7) to a port's paired output and return the device's parsed identity. Reply-wait window is 1 s; total wall-clock time may exceed that under contention because Phase 1 serialises probes globally (queue_wait + 1 s reply ≈ ≤2 s typical, plus runtime scheduling). Returns a structured ProbeOutcome (Identified / NoReply / NoPairedOutput / SendFailed / RateLimited / SysExDisabled) serialised as JSON. The MCP socket path returns it as the body of `content[0].text` on a normal ToolCallResult; the GUI/LLM executor wraps the same JSON in the HardwareIO Confirmed-result envelope (auto-confirmed because the Identity Request is benign and universally standardised). HardwareIO tier; per-port 60 s rate limit. The port_name argument must be the MIDI input port name as reported by the daemon (use conductor_list_device_bindings to find the right value); the daemon resolves the paired output internally.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "port_name": {
                        "type": "string",
                        "description": "MIDI input port name (NOT a device alias). Must be one of the port_name values returned by conductor_list_device_bindings, and the corresponding device must have a configured paired output (or the result is NoPairedOutput)."
                    }
                },
                "required": ["port_name"]
            }),
        },
        ToolDefinition {
            name: "conductor_reset_control_state".to_string(),
            description: "Clear tracked control state for a device or a single channel. STORE-ONLY: does NOT send any MIDI (no All-Notes-Off, no panic). Use conductor_send_midi / conductor_send_sysex separately if you also need to reset the hardware. Scopes: all_sound_off (CC120), reset_all_controllers (CC121), all_notes_off (CC123), or 'all' for full channel clear. Accepts either `device` or `device_id` (alias) for the target identifier.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "device": {
                        "type": "string",
                        "description": "Device alias or port name to reset. Required (or supply `device_id`)."
                    },
                    "device_id": {
                        "type": "string",
                        "description": "Alias for `device`. Accepted for consistency with conductor_list_device_bindings and other tools that return device_id."
                    },
                    "channel": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": 15,
                        "description": "Optional MIDI channel (0-15). Omit to drop ALL state for the device."
                    },
                    "scope": {
                        "type": "string",
                        "enum": ["all_sound_off", "reset_all_controllers", "all_notes_off", "all"],
                        "description": "Channel-scoped reset type. Default: 'all'. Ignored if 'channel' is omitted."
                    }
                }
            }),
        },
    ]
}
