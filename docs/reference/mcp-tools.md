# MCP Tools Reference

Conductor exposes a Model Context Protocol (MCP) server so LLM clients (Claude
Desktop, Cursor, or any MCP-speaking agent) can inspect — and, in richer
builds, control — a running daemon. This page documents the tool catalog as
actually compiled into `conductor-daemon`.

## The feature gate (ADR-045)

The MCP tool catalog is split across daemon cargo features, and **which tools
a running daemon advertises depends entirely on how it was built**:

| Feature | Adds | Notes |
|---|---|---|
| `mcp` (default) | ~27 **ReadOnly** inspection tools | Ships in every official OSS artifact. Read-only: no mutation, no hardware emission. |
| `mcp-write` | ~27 additional **Stateful / ArtifactRender / ConfigChange / HardwareIO** tools, advertised on the MCP socket | **Not enabled in any official artifact** — source builders only. |
| `llm-executor` | The daemon-side plan/apply executor (`ToolExecutor`) that backs the write-tier risk handling | Pulled in automatically by `mcp-write`; also used standalone by the IPC-based LLM executor path (see [LLM Integration](../development/llm-integration.md)) that downstream GUI clients use. |

A daemon built with just `default = ["mcp"]` (i.e. any plain `cargo build` of
this repository) exposes **only the ReadOnly tools below**. If you're
following an LLM's suggestion to call a write-tier tool (`conductor_create_mapping`,
`conductor_send_midi`, etc.) against a stock build, the daemon will reject it —
`tools/list` never advertises a tool the binary didn't compile in, and calling
one anyway returns a "not available in this build" error naming the
`mcp-write` feature.

Regardless of which tools are *advertised*, the MCP socket enforces a
per-peer tier ceiling: an unregistered MCP client is clamped to ReadOnly even
on an `mcp-write` build; higher tiers require `conductorctl mcp register`.
`Privileged` is never reachable from the MCP socket at all — it's reserved
for daemon-internal operations. See [MCP Server Implementation](../development/mcp-server.md)
for the registration and peer-ceiling mechanics.

## Risk tiers

Every tool (and every dispatchable IPC command / mapping action) carries one
of six risk tiers:

| Tier | Meaning | Execution |
|---|---|---|
| **ReadOnly** | Pure inspection — no state mutation, no side effects | Auto-executed |
| **Stateful** | Session-scoped, in-memory mutation (mode switching, MIDI Learn, mapping-editor staging) | Auto-executed, logged |
| **ArtifactRender** | Projects an artifact into a workspace UI | Auto-executed, logged like Stateful |
| **ConfigChange** | Persistent config mutation | Returns a `ConfigPlan` requiring approval (Plan/Apply) |
| **HardwareIO** | Emits MIDI/OSC/SysEx to a physical device | Requires a confirmation step |
| **Privileged** | Blast radius beyond the daemon process/devices | Daemon-internal only; never reachable via MCP or IPC from an external peer |

## ReadOnly tools (`mcp` — always available)

All of these take no required arguments unless noted, and none of them can be
disabled by a feature flag: they're the base catalog.

| Tool | Purpose | Key args |
|---|---|---|
| `conductor_get_status` | Daemon lifecycle state, device connection, uptime | — |
| `conductor_list_devices` | Available MIDI and HID (gamepad) devices | — |
| `conductor_get_config` | Current configuration (endpoints, modes, mappings) | — |
| `conductor_list_mappings` | List mappings, optionally filtered to one mode | `mode?` |
| `conductor_get_mapping` | A single mapping by mode + index | `mode`, `index` |
| `conductor_validate_config` | Validate config against MIDI/HID/OSC schemas; coverage report | — |
| `conductor_list_routes` | List declared `[[routes]]` (ADR-031) | — |
| `conductor_get_routing_graph` | Declared routing graph: `[[endpoints]]` + `[[routes]]` in one call | — |
| `conductor_get_resolved_routing_graph` | Runtime-resolved routing graph from the live `connector_registry` (bound ports, `from_missing`/`to_missing`) | — |
| `conductor_explain_route_match` | Explain why each route fires/is skipped for a hypothetical MIDI event (ADR-036 D5) | `event` (device, type, channel, data1, data2?), `active_mode` |
| `conductor_get_dispatch_trace` | Recent route-dispatch decisions from the in-memory ring buffer (ADR-036 §8) | `last?` (1-256, default 32) |
| `conductor_get_connector_metrics` | Live per-connector throughput/error metrics | — |
| `conductor_mode_status` | Active mode + lock state | — |
| `conductor_get_topology_summary` | Structured signal-routing topology summary (devices, mappings, cross-device routing, feedback loops) | — |
| `conductor_get_active_profile` | Currently active profile name + config path | — |
| `conductor_get_binding_health` | Health diagnostic for a specific binding | `alias` |
| `conductor_list_discovered_ports` | All visible ports across protocols, with binding status | — |
| `conductor_get_workspace_state` | Current workspace view/editing context | — |
| `conductor_list_device_bindings` | Multi-device binding status (connection, mute state) | — |
| `conductor_list_plugins` | Available and loaded plugins | — |
| `conductor_plugin_info` | Plugin metadata (name, version, capabilities, status) | `name` |
| `conductor_suggest_binding` | Suggest a binding config for a port, from live fingerprinting or name heuristics | `port_name` |
| `conductor_get_device_identity` | Cached SysEx identity for a port, if probed this session | `port_name` |
| `conductor_list_device_identities` | Every device with a cached SysEx identity this session | — |
| `conductor_get_control_state` | Current physical control state (last PC, CC values, held notes, aftertouch) | `device?` |
| `conductor_get_active_pc` | Active Program Change per (device, channel) | `device?` |
| `conductor_security_status` | Network-approval HMAC key rotation status | — |

> **Note on profile tools:** `conductor_list_profiles`, `conductor_create_profile`,
> and `conductor_delete_profile` carry ReadOnly/Stateful risk-tier classifications
> in the daemon's tier table, but they are **not** part of the compiled MCP tool
> catalog returned by `tools/list` — they are GUI-intercepted tools (ADR-023)
> that always error with "This tool is managed by the GUI and should not reach
> the daemon" if a client somehow reaches the daemon with them. They're
> mentioned here only because the risk-tier classification is visible in
> source; don't expect them to show up in `tools/list`.

## Write-tier tools (`mcp-write` — source builds only)

These are only advertised on the MCP socket when the daemon is compiled with
`--features mcp-write`. No official Conductor artifact ships this way.

### Stateful

| Tool | Purpose | Key args |
|---|---|---|
| `conductor_start_learn` / `conductor_start_midi_learn`* | Start Learn mode to capture controller input | `timeout_seconds?` |
| `conductor_stop_learn` / `conductor_stop_midi_learn`* | Stop Learn mode; return captured events + suggested trigger | — |
| `conductor_set_mapping_editor` | Open a mapping editor pre-filled with trigger/action/description | `trigger?`, `action?`, `description?`, `mode?` |
| `conductor_update_mapping_editor` | Update fields in the currently open mapping editor | `fields` |
| `conductor_switch_mode` | **Deprecated** — switch active mode without lock semantics; prefer `conductor_set_mode` | `mode` |
| `conductor_set_mode` | Set active mode and (by default) lock it against auto-switching | `mode`, `lock?` |
| `conductor_unlock_mode` | Release the manual mode lock | — |
| `conductor_switch_profile` | Switch active profile by name + config path | `profile_name`, `config_path` |
| `conductor_set_device_enabled` | Enable/disable (mute) a device | `device_id`, `enabled` |
| `conductor_scan_ports` | Trigger an immediate port rescan | — |
| `conductor_enable_plugin` / `conductor_disable_plugin` | Enable/disable a plugin by name | `name` |
| `conductor_reset_control_state` | Clear tracked control state (store-only — sends no MIDI) | `device` (or `device_id`), `channel?`, `scope?` |

\* `_learn` and `_midi_learn` are aliases; the `_midi_learn` forms are deprecated but kept so older prompts still resolve.

### ArtifactRender

| Tool | Purpose | Key args |
|---|---|---|
| `conductor_render_artifact` | Render an artifact (visual overlay) in the workspace canvas | `artifact_type`, `title`, `data?` |
| `conductor_dismiss_artifact` | Dismiss an artifact by ID | `artifact_id` |

### ConfigChange (returns a `ConfigPlan` for approval)

| Tool | Purpose | Key args |
|---|---|---|
| `conductor_create_endpoint` | Create a unified I/O endpoint in `[[endpoints]]` (ADR-035) — the sole I/O-authoring tool | `alias`, `direction`, `type`, plus type-specific fields (matchers / host+port / universe / port_name) |
| `conductor_create_mapping` | Create a new mapping in a mode | `mode`, `trigger`, `action`, `description?`, `let_through?` |
| `conductor_update_mapping` | Update an existing mapping | `mode`, `index`, `trigger`, `action`, `description?` |
| `conductor_delete_mapping` | Delete a mapping | `mode`, `index` |
| `conductor_batch_changes` | Atomically apply multiple operations — mapping/mode CRUD plus route CRUD (`create_route`/`update_route`/`delete_route`, ADR-031 §5.4) | `operations[]` |
| `conductor_set_context_mapping` | Create a mapping whose action routes on prior MIDI state (`PcContextSwitch` / `CcContextSwitch`, ADR-025) | `mode`, `trigger`, `action` |

> `conductor_create_endpoint` is create-only: there is no MCP tool to update
> or delete an existing endpoint (`ConfigChange` has no such variant). Edit
> `[[endpoints]]` in TOML directly, or use whatever endpoint-authoring UI a
> downstream GUI client provides.

### HardwareIO (requires confirmation)

| Tool | Purpose | Key args |
|---|---|---|
| `conductor_send_sysex` | Send a raw SysEx message | `device`, `data[]`, `confirmation_token?` |
| `conductor_device_reset` | Send a device reset (soft/hard/factory) | `device`, `reset_type`, `confirmation_token?` |
| `conductor_send_midi` | Send standard MIDI messages (note/CC/program change auto-confirm) | `port`, `messages[]` |
| `conductor_probe_device_identity` | Send a Universal SysEx Identity Request and return the parsed identity | `port_name` |

## ConfigPlan response shape

`ConfigChange`-tier tools return a `ConfigPlan` requiring approval rather than
mutating config directly:

```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "description": "Create mapping 'new mapping' in mode 'Default'",
  "changes": [
    { "type": "CreateMapping", "mode": "Default", "trigger": { "...": "..." }, "action": { "...": "..." } }
  ],
  "base_state_hash": "sha256:...",
  "expires_at": "2026-09-03T10:35:00Z"
}
```

(Abbreviated — the real `ConfigPlan` also carries `created_at`, a
pre-rendered `diff_preview`, `change_descriptions`, `validation_warnings`/
`validation_errors`, and an optional `deprecation` block when the plan came
from a deprecated tool like `conductor_switch_mode`.)

### TOCTOU protection

1. When a plan is created, the current config hash is captured as
   `base_state_hash`.
2. Before applying, the current config hash is recomputed and compared.
3. If the config changed underneath the plan, the apply is rejected.
4. Plans expire 5 minutes after creation.

Applying or rejecting a plan is not itself an MCP tool call — it goes through
the daemon's IPC protocol (`ExecuteMcpTool`'s siblings `ApplyPlan` /
`RejectPlan`), which is how downstream GUI clients drive the plan/apply
workflow. See [LLM Integration](../development/llm-integration.md).

## Error handling

Tools return JSON-RPC 2.0 error responses for protocol-level failures:

```json
{
  "jsonrpc": "2.0",
  "error": { "code": -32602, "message": "Invalid params: missing field `mode`" },
  "id": 1
}
```

Standard JSON-RPC codes in use: `-32700` (parse error), `-32600` (invalid
request), `-32601` (method not found), `-32602` (invalid params), `-32603`
(internal error). A tool that runs but fails its own validation returns a
successful JSON-RPC envelope with `isError: true` on the `ToolCallResult`
rather than a JSON-RPC error — check `isError` in the response, not just the
outer envelope.

## See also

- [MCP Server Implementation](../development/mcp-server.md) — server internals, peer registration, adding new tools
- [LLM Integration](../development/llm-integration.md) — the daemon-side plan/apply executor and IPC path
- [Architecture Decision Records](../architecture/adrs.md) — ADR-007, ADR-031, ADR-035, ADR-036, ADR-045 anchors referenced above
