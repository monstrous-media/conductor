# MCP Tools Reference

Conductor exposes a Model Context Protocol (MCP) server that allows LLMs and other clients to interact with the daemon. This page documents the available MCP tools.

## Overview

The MCP server runs on a Unix domain socket at `~/.conductor/mcp.sock` and implements the JSON-RPC 2.0 protocol. Tools are organized by risk tier:

| Risk Tier | Description | Execution |
|-----------|-------------|-----------|
| **ReadOnly** | Query-only, no side effects | Auto-executed |
| **Stateful** | Affects runtime state | Logged, auto-executed |
| **ConfigChange** | Modifies configuration | Requires user approval |

## ReadOnly Tools

These tools query information without making changes.

### conductor_get_config

Returns the current configuration file content.

**Parameters**: None

**Returns**:
```json
{
  "content": "# config.toml content...",
  "path": "/Users/you/.conductor/config.toml",
  "hash": "sha256:abc123..."
}
```

**Example**:
```json
{
  "jsonrpc": "2.0",
  "method": "conductor_get_config",
  "params": {},
  "id": 1
}
```

### conductor_get_status

Returns daemon status and connected devices.

**Parameters**: None

**Returns**:
```json
{
  "daemon_running": true,
  "lifecycle_state": "Running",
  "connected": true,
  "device_connected": true,
  "device": {
    "name": "Maschine Mikro MK3",
    "port": 2,
    "last_event_at": 1706200000
  },
  "uptime_secs": 3600,
  "input_mode": "MidiOnly",
  "statistics": {
    "events_processed": 1500,
    "actions_executed": 750
  }
}
```

> **v4.17.0**: `daemon_running` indicates the daemon process is alive, independent of device connection. `device_connected` is an explicit alias for `connected` (which indicates device connection status).
>
> **v4.17.1**: Status is now reported correctly via both the MCP Unix socket and the IPC path (used by the chat UI). Prior to v4.17.1, the IPC path always returned `connected: false` due to missing daemon state refs in the ToolExecutor.

### conductor_list_modes

Lists all configured modes.

**Parameters**: None

**Returns**:
```json
{
  "modes": [
    {
      "name": "Default",
      "color": "blue",
      "mapping_count": 12
    },
    {
      "name": "DJ",
      "color": "purple",
      "mapping_count": 8
    }
  ]
}
```

### conductor_get_mappings

Returns mappings for a specific mode.

**Parameters**:
| Name | Type | Required | Description |
|------|------|----------|-------------|
| `mode` | string | Yes | Mode name |

**Returns**:
```json
{
  "mode": "Default",
  "mappings": [
    {
      "index": 0,
      "trigger": { "type": "Note", "note": 36 },
      "action": { "type": "Keystroke", "keys": ["cmd", "c"] }
    }
  ]
}
```

### conductor_list_devices

Lists available MIDI devices and gamepads.

**Parameters**: None

**Returns**:
```json
{
  "midi_inputs": ["Maschine Mikro MK3", "Virtual MIDI Bus"],
  "midi_outputs": ["Maschine Mikro MK3"],
  "gamepads": ["Xbox Controller"]
}
```

### conductor_list_device_bindings

Returns multi-device binding status including connection state, mute state, and configuration status.

**Parameters**: None

**Returns**:
```json
{
  "device_bindings": [
    {
      "device_id": "pads",
      "port_name": "Mikro MK3 MIDI",
      "connected": true,
      "enabled": true,
      "is_configured": true
    }
  ],
  "multi_device_active": true,
  "total_devices": 2,
  "connected_count": 2,
  "muted_count": 0
}
```

> **v4.23.0**: Added in ADR-009 Phase 5. `is_configured` is `true` when the device_id is a named alias (not `raw:` prefixed).

### conductor_validate_config

Validates the configuration against MIDI/HID/OSC protocol standards and returns a report with errors, warnings, and coverage metrics.

**Parameters**: None

**Returns**:
```json
{
  "valid": true,
  "errors": [],
  "warnings": ["CC 128 exceeds MIDI range 0-127"],
  "coverage": {
    "midi": { "notes_used": 12, "cc_used": 3 },
    "hid": { "buttons_used": 0 },
    "osc": { "addresses_used": 0 }
  }
}
```

> **v4.26.66**: Added for protocol-aware config validation.

## Stateful Tools

These tools affect runtime state and are logged for auditing.

### conductor_start_midi_learn

Starts MIDI Learn mode to capture input events.

**Parameters**:
| Name | Type | Required | Description |
|------|------|----------|-------------|
| `timeout_ms` | number | No | Timeout in milliseconds (default: 30000) |

**Returns**:
```json
{
  "status": "learning",
  "expires_at": "2025-01-15T10:30:00Z"
}
```

### conductor_stop_midi_learn

Stops MIDI Learn mode and returns captured events.

**Parameters**: None

**Returns**:
```json
{
  "events": [
    {
      "type": "NoteOn",
      "note": 36,
      "velocity": 100,
      "timestamp": "2025-01-15T10:29:55Z"
    }
  ],
  "suggestions": [
    {
      "type": "Note",
      "note": 36,
      "velocity_range": [1, 127]
    }
  ]
}
```

### conductor_set_device_enabled

Enable or disable (mute/unmute) a specific device by its device_id.

**Parameters**:
| Name | Type | Required | Description |
|------|------|----------|-------------|
| `device_id` | string | Yes | The device_id to enable or disable |
| `enabled` | boolean | Yes | `true` to enable (unmute), `false` to disable (mute) |

**Returns**:
```json
{
  "device_id": "pads",
  "enabled": false,
  "message": "Device 'pads' muted"
}
```

> **v4.23.0**: Added in ADR-009 Phase 5.

### conductor_scan_ports

Triggers an immediate port rescan to detect newly connected or disconnected MIDI devices (vs the default 5-second interval).

**Parameters**: None

**Returns**:
```json
{
  "message": "Port rescan triggered"
}
```

> **v4.23.0**: Added in ADR-009 Phase 5.

### conductor_switch_mode

Switches the active mapping mode by name.

**Parameters**:
| Name | Type | Required | Description |
|------|------|----------|-------------|
| `mode` | string | Yes | Mode name to switch to |

**Returns**:
```json
{
  "success": true,
  "mode_name": "Performance",
  "mode_index": 1,
  "total_modes": 3
}
```

> **v4.26.69**: Added for LLM-driven mode switching.

## ConfigChange Tools

These tools modify configuration and require user approval via the Plan/Apply workflow.

### conductor_create_mapping

Creates a new mapping in a mode.

**Parameters**:
| Name | Type | Required | Description |
|------|------|----------|-------------|
| `mode` | string | Yes | Target mode name |
| `trigger` | object | Yes | Trigger configuration (see valid types below) |
| `action` | object | Yes | Action configuration (see valid types below) |

> **Note (v4.18.1):** Tool schemas include enriched descriptions listing all valid trigger and action types inline, preventing LLM hallucination of invalid types.

**Valid trigger types**: `Note`, `CC`, `VelocityRange`, `LongPress`, `DoubleTap`, `NoteChord`, `EncoderTurn`, `Aftertouch`, `PitchBend`, `GamepadButton`, `GamepadButtonChord`, `GamepadAnalogStick`, `GamepadTrigger`

**Valid action types**: `Keystroke`, `Text`, `Launch`, `Shell`, `VolumeControl`, `ModeChange`, `SendMidi`, `MidiForward`, `Sequence`, `Delay`

**Valid SendMidi message_type values**: `NoteOn`, `NoteOff`, `CC`, `ProgramChange`, `PitchBend`, `Aftertouch`

**Returns**: `ConfigPlan` (see below)

**Example**:
```json
{
  "jsonrpc": "2.0",
  "method": "conductor_create_mapping",
  "params": {
    "mode": "Default",
    "trigger": { "type": "Note", "note": 36 },
    "action": { "type": "Keystroke", "keys": ["cmd", "c"] }
  },
  "id": 1
}
```

### conductor_update_mapping

Updates an existing mapping.

**Parameters**:
| Name | Type | Required | Description |
|------|------|----------|-------------|
| `mode` | string | Yes | Mode name |
| `index` | number | Yes | Mapping index (0-based) |
| `trigger` | object | No | New trigger (if changing) |
| `action` | object | No | New action (if changing) |

**Returns**: `ConfigPlan`

### conductor_delete_mapping

Deletes a mapping from a mode.

**Parameters**:
| Name | Type | Required | Description |
|------|------|----------|-------------|
| `mode` | string | Yes | Mode name |
| `index` | number | Yes | Mapping index to delete |

**Returns**: `ConfigPlan`

### conductor_create_endpoint

Creates a unified I/O endpoint in the `[[endpoints]]` configuration (ADR-035). This is the single tool for authoring any device/connector identity; it supersedes the removed `conductor_create_binding` / `conductor_create_connector` / `conductor_create_device_identity` tools (ADR-035 Phase 2).

**Parameters**:
| Name | Type | Required | Description |
|------|------|----------|-------------|
| `alias` | string | Yes | Unique endpoint alias (e.g. "keystep", "synth") |
| `direction` | string | Yes | `Input`, `Output`, or `Bidirectional` |
| `type` | string | Yes | `Matcher`, `OscEndpoint`, `ArtNetEndpoint`, or `MidiVirtualPort` |
| `protocol` | string | No | `Midi` / `Osc` / `ArtNet` / `Hid` (inferred from `type` when omitted) |
| `matchers` | array | For `Matcher` | Device matchers (at least one matcher list non-empty) |
| `description` | string | No | Human-readable description |

**Matcher types**: `ExactName`, `NameContains`, `NameRegex`, `UsbIdentifier`, `CoreMidiUniqueId`

**Returns**: `ConfigPlan`

**Example**:
```json
{
  "jsonrpc": "2.0",
  "method": "conductor_create_endpoint",
  "params": {
    "alias": "keystep",
    "direction": "Input",
    "type": "Matcher",
    "matchers": [{ "type": "NameContains", "value": "KeyStep" }],
    "description": "Arturia KeyStep 37"
  },
  "id": 1
}
```

> **ADR-035**: `conductor_create_endpoint` is the unified replacement for the
> legacy binding/connector authoring tools. Updating or deleting an existing
> endpoint via MCP is not yet supported — edit `[[endpoints]]` TOML or use the
> GUI endpoint editor.

## ConfigPlan Response

ConfigChange tools return a `ConfigPlan` that must be approved:

```json
{
  "plan_id": "550e8400-e29b-41d4-a716-446655440000",
  "description": "Create mapping for note 36 in Default mode",
  "changes": [
    {
      "change_type": "CreateMapping",
      "mode": "Default",
      "description": "Add Note 36 -> Keystroke Cmd+C"
    }
  ],
  "diff_preview": "+ [[modes.mappings]]\n+ trigger = { type = \"Note\", note = 36 }\n+ action = { type = \"Keystroke\", keys = [\"cmd\", \"c\"] }",
  "base_state_hash": "sha256:abc123...",
  "expires_at": "2025-01-15T10:35:00Z"
}
```

### Applying a Plan

To apply a plan, call `llm_apply_plan` via the Tauri command interface:

```javascript
await invoke('llm_apply_plan', { planId: plan.plan_id });
```

### Rejecting a Plan

To reject a plan:

```javascript
await invoke('llm_reject_plan', { planId: plan.plan_id });
```

## HardwareIO Tools

These tools interact with hardware devices and require confirmation for safety.

### conductor_send_sysex

Sends raw SysEx messages to a connected MIDI device.

**Parameters**:
| Name | Type | Required | Description |
|------|------|----------|-------------|
| `port` | string | Yes | Output port name |
| `data` | array | Yes | SysEx bytes (hex) |

**Returns**: Confirmation token or execution result.

### conductor_send_midi

Sends standard MIDI messages (note, CC, program change) to a connected device.

**Parameters**:
| Name | Type | Required | Description |
|------|------|----------|-------------|
| `port` | string | Yes | Output port name |
| `messages` | array | Yes | Array of MIDI messages |

Each message has:
| Field | Type | Description |
|-------|------|-------------|
| `type` | string | `note_on`, `note_off`, `cc`, or `program_change` |
| `channel` | number | MIDI channel (1-16) |
| `note` | number | Note number (0-127, for note_on/note_off) |
| `velocity` | number | Velocity (0-127, for note_on/note_off) |
| `controller` | number | CC number (0-127, for cc) |
| `value` | number | CC value (0-127, for cc) |
| `program` | number | Program number (0-127, for program_change) |

**Returns**:
```json
{
  "success": true,
  "messages_sent": 3,
  "port": "IAC Driver Bus 1"
}
```

> **v4.26.67**: Standard MIDI messages auto-confirm (low risk). SysEx still requires confirmation token.

## Tool Risk Tiers

### Security Model

The risk tier system provides defense-in-depth:

1. **ReadOnly tools** cannot modify state, safe to auto-execute
2. **Stateful tools** are logged for audit trails
3. **ConfigChange tools** require explicit user approval

### TOCTOU Protection

ConfigChange tools use Time-of-Check to Time-of-Use (TOCTOU) protection:

1. When a plan is created, the current config hash is captured
2. Before applying, the current config hash is compared
3. If the config changed, the plan is rejected
4. Plans expire after 5 minutes

This prevents race conditions and accidental overwrites.

## Error Handling

Tools return JSON-RPC error responses for failures:

```json
{
  "jsonrpc": "2.0",
  "error": {
    "code": -32600,
    "message": "Invalid mode: 'Unknown' does not exist"
  },
  "id": 1
}
```

Common error codes:
- `-32600`: Invalid request
- `-32601`: Method not found
- `-32602`: Invalid params
- `-32603`: Internal error

## See Also

- [Chat Interface](../guides/chat.md) - Using Chat with MCP tools
- [Development: MCP Server](../development/mcp-server.md) - Server implementation details
- [Development: LLM Integration](../development/llm-integration.md) - Architecture overview
