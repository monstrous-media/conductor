# MCP Server Implementation

This document describes the implementation of Conductor's Model Context
Protocol (MCP) server, which lives entirely in `conductor-daemon`.

## Overview

The MCP server provides a JSON-RPC 2.0 interface over a Unix domain socket.
It lets MCP clients (Claude Desktop, Cursor, any MCP-speaking agent) query —
and, on source builds with `mcp-write` enabled, control — a running daemon.

This is a **separate surface** from the daemon's IPC protocol
(`daemon::{IpcClient, IpcCommand, ...}`) that downstream GUI clients use. The
IPC protocol carries the full plan/apply and hardware-confirmation machinery
via `llm::executor::ToolExecutor`; the MCP socket is a thinner, MCP-spec-compliant
front end that reuses the same tool catalog and risk-tier table. See
[LLM Integration](llm-integration.md) for the IPC-side executor.

## Module layout

```
conductor-daemon/src/daemon/
├── mcp/
│   ├── mod.rs           # McpServer: socket lifecycle, JSON-RPC dispatch,
│   │                     # peer tier-ceiling enforcement (check_peer_tier_ceiling)
│   ├── tools_call.rs    # tools/call handling: shared_state special cases,
│   │                     # mcp-write compiled-tool gating, McpToolExecutor dispatch
│   └── tests.rs
├── mcp_tools/
│   ├── mod.rs             # get_tool_definitions, get_tool_risk_tier,
│   │                       # is_compiled_tool, tool_unavailable_error
│   ├── definitions_readonly.rs  # ReadOnly tool catalog (always compiled)
│   ├── definitions_write.rs     # Write-tier tool catalog (cfg(feature = "mcp-write"))
│   ├── write_tiers.rs             # Write-tier risk classification (cfg(feature = "llm-executor"))
│   ├── executor.rs         # McpToolExecutor — stateless dispatch for ReadOnly tools
│   ├── executor_queries.rs # Query helpers used by executor.rs
│   └── tests.rs
├── mcp_types.rs         # Wire types: McpRequest/Response, ToolDefinition,
│                         # ToolCallResult, and the `ToolRiskTier` re-export
└── llm/
    ├── plan.rs           # ConfigChange, ConfigPlan (TOCTOU)
    └── executor.rs       # ToolExecutor — full risk-tier handling for the IPC path
```

`mcp_tools.rs` and `mcp.rs` as single files no longer exist — both were split
into directory modules once they exceeded a comfortable code-review window.
`definitions_readonly.rs` / `definitions_write.rs` hold the tool *catalog*
(name, description, JSON schema); `executor.rs` / `executor_queries.rs` hold
the `McpToolExecutor` that actually answers ReadOnly `tools/call` requests on
the MCP socket.

## Cargo feature gates (ADR-045)

```toml
default = ["mcp"]
mcp = []                          # ReadOnly tool catalog + MCP socket
llm-executor = ["audit-db"]       # ToolExecutor, ConfigPlan, undo/redo (IPC path)
mcp-write = ["mcp", "llm-executor"] # Write-tier tools ALSO advertised on the MCP socket
audit-db = ["dep:rusqlite"]        # SQLite-backed audit log
```

These feature names are load-bearing for CI's composition matrix — don't
rename them. The practical effect:

- A plain `cargo build` (default features) gives you `mcp` only:
  `mcp_tools::get_tool_definitions()` returns just
  `definitions_readonly::readonly_tool_definitions()`, and `get_tool_risk_tier`
  falls through to the `ReadOnly`/`Stateful` classifications in `mcp_tools/mod.rs`
  (the `write_tiers` module doesn't even compile in).
- `--features mcp-write` additionally compiles `definitions_write.rs` into the
  catalog (appended in `get_tool_definitions`) and `write_tiers.rs` into the
  risk-tier lookup. `tools_call.rs` also stops short-circuiting non-catalog
  tool names with the "not available in this build" error.
- `--features llm-executor` alone (without `mcp-write`) compiles the write-tier
  *risk classification* (`write_tiers.rs`) so the IPC plan/apply path can
  still classify write tools correctly, but does **not** advertise them on the
  MCP socket — that's the point of the split (ADR-045 D2): the write machinery
  and its MCP exposure are independently gated.

At the module level (`daemon/mod.rs`), the gating is explicit:
`#[cfg(feature = "llm-executor")] pub mod llm;`, `#[cfg(feature = "mcp")] pub
mod mcp;`, and `#[cfg(any(feature = "mcp", feature = "llm-executor"))] pub mod
mcp_tools;` — the entire `llm` module (both `plan.rs` and `executor.rs`) is
absent from the binary unless `llm-executor` is on, and `mcp_tools` compiles
whenever either side needs the shared catalog.

## Protocol

### JSON-RPC 2.0 over a Unix socket

**Request**:
```json
{ "jsonrpc": "2.0", "method": "tools/call", "params": { "name": "conductor_get_status", "arguments": {} }, "id": 1 }
```

**Response**:
```json
{ "jsonrpc": "2.0", "result": { "content": [{ "type": "text", "text": "{...}" }] }, "id": 1 }
```

**Error**:
```json
{ "jsonrpc": "2.0", "error": { "code": -32601, "message": "Method not found: foo" }, "id": 1 }
```

Supported methods: `initialize`, `initialized` (notification), `tools/list`,
`tools/call`, `ping`.

### Message framing

Messages are newline-delimited JSON; each message is a single line terminated
by `\n`. Requests are capped at 1MB.

## Socket location

`McpServer::get_mcp_socket_path()` resolves to `<runtime_dir>/conductor/conductor-mcp.sock`,
falling back through `dirs::runtime_dir()` → `dirs::cache_dir()` →
`~/.cache`. On startup the daemon removes any stale socket file, creates the
parent directory, binds, and sets `0600` permissions (owner-only).

## Peer registration and tier ceilings

An MCP client isn't automatically trusted just because it connected to the
socket. `McpServer` pins each accepted peer's credentials
(`resolve_peer_tier_ceiling`) and looks up a registered tier ceiling in the
`McpRegistry` (`conductorctl mcp register`). `check_peer_tier_ceiling` then
enforces, per `tools/call`:

- An **unregistered** peer is clamped to `ReadOnly` only.
- A peer registered at a given `AuditRiskTier` (`ReadOnly` / `Stateful` /
  `ConfigChange` / `HardwareIO`) may call tools at that tier or below.
- `Privileged` is **never** reachable from the MCP socket, regardless of
  registration — it's reserved for daemon-internal callers.
- A pin failure (older Linux kernel without `pidfd_open`, sandboxed macOS,
  etc.) is treated as "unregistered" rather than failing closed entirely — the
  peer can still read.

This is independent of the `mcp` / `mcp-write` compile-time gate: the feature
gate controls what's *compiled and advertised*; the registry controls what a
*specific connected peer* is allowed to invoke among what's advertised.

## Adding a new tool

The exact steps depend on the tool's risk tier.

### 1. Adding a ReadOnly tool

**Define it** in `mcp_tools/definitions_readonly.rs`, appending to the `vec![]`
returned by `readonly_tool_definitions()`:

```rust
ToolDefinition {
    name: "conductor_my_tool".to_string(),
    description: "One sentence a caller needs to pick this tool over a similar one.".to_string(),
    input_schema: json!({
        "type": "object",
        "properties": {
            "param1": { "type": "string", "description": "..." }
        },
        "required": ["param1"]
    }),
},
```

**Classify it** in `mcp_tools/mod.rs`'s `get_tool_risk_tier` match:

```rust
"conductor_my_tool" => ToolRiskTier::ReadOnly,
```

**Implement the handler.** ReadOnly tools without daemon-state dependencies
are dispatched inside `McpToolExecutor` (`mcp_tools/executor.rs` /
`executor_queries.rs`) — add a match arm there. If your tool needs live
`SharedDaemonStateRefs` (like `conductor_get_connector_metrics` or the
identity-cache tools), it needs a special-case arm in
`mcp/tools_call.rs::handle_tools_call` instead — `McpToolExecutor` is
deliberately stateless and can't reach the live registry.

### 2. Adding a write-tier tool (Stateful / ArtifactRender / ConfigChange / HardwareIO)

**Define it** in `mcp_tools/definitions_write.rs`, in
`write_tier_tool_definitions()` — same `ToolDefinition` shape as above.

**Classify it** in `mcp_tools/write_tiers.rs`'s `write_tool_risk_tier` match:

```rust
"conductor_my_write_tool" => ToolRiskTier::Stateful, // or ArtifactRender / ConfigChange / HardwareIO
```

**Implement the handler** in `llm/executor.rs`'s `ToolExecutor::execute` (or
the per-tier helper it calls). `ConfigChange` tools should build a
`ConfigChange` variant in `llm/plan.rs` and return it wrapped in a
`ConfigPlan`; `HardwareIO` tools route through the confirmation machinery in
`daemon::hardware_io`.

Because `definitions_write.rs` only compiles under `mcp-write` and
`write_tiers.rs` only under `llm-executor`, a tool that's meant to be
reachable over the MCP socket needs both features enabled to build and test
end to end — `cargo build --features mcp-write` pulls in `llm-executor`
transitively (see the feature table above).

### 3. Write a test

`mcp_tools/tests.rs` and `mcp/tests.rs` cover the catalog and dispatch paths
respectively. A useful pattern instead of hardcoding a tool count (which will
drift every time a tool is added or removed) is to assert on the *shape*:

```rust
#[test]
fn every_readonly_tool_has_a_risk_tier() {
    for tool in mcp_tools::get_tool_definitions() {
        // get_tool_risk_tier never panics and never returns an
        // unexpected default for a name that's actually in the catalog.
        let tier = mcp_tools::get_tool_risk_tier(&tool.name);
        assert_ne!(tier, ToolRiskTier::Privileged, "{} defaulted to fail-closed", tool.name);
    }
}
```

## Rate limiting

Rate limiting is implemented, not a TODO:

- **Per-tier tool rate limiting** (`daemon/ratelimit/`, P4-05): a sliding-window
  `RateLimiter` used by the IPC-side `ToolExecutor`, with a default budget per
  tier per 60-second window (`ArtifactRender` shares the `Stateful` budget):
  ReadOnly 100, Stateful/ArtifactRender 30, ConfigChange 10, HardwareIO 5,
  Privileged 3 — plus a global cap of 200 requests/window across all tiers.
  Exceeding a tier's budget returns `ExecutionResult::RateLimited` rather than
  executing.
- **Per-peer IPC message rate limiting** (`daemon/ipc_rate_limit.rs`, ADR-027
  §D16/§D12): a separate, orthogonal limiter keyed by `(peer_pid, peer_exe_path)`
  capping raw message throughput per connection at 100 messages/second,
  independent of tool tier — this is the defense against a single connection
  flooding the dispatch loop with any mix of requests.
- **Concurrent-connection cap** (`ConnectionLimiter` in `mcp/mod.rs`): the MCP
  socket itself caps concurrent client connections (16) and drops new
  connections at capacity rather than queueing them.

## Security considerations

### Socket permissions

`0600` — owner read/write only, no group or world access.

### Input validation

Tool arguments are validated against each tool's JSON schema plus
tool-specific range/type checks in the handler before use.

### The `mcp-write` boundary

Even on a build with `mcp-write` compiled in, exposing write tools on the MCP
socket is a deliberate, source-builder-only opt-in (ADR-045 D3/D8) — it is
not part of any official Conductor artifact. If you're building your own
daemon and want an external MCP client to be able to mutate config or send
MIDI, compile with `--features mcp-write` and register the peer's tier via
`conductorctl mcp register`.

## Error codes

Standard JSON-RPC:

| Code | Meaning |
|------|---------|
| -32700 | Parse error |
| -32600 | Invalid request |
| -32601 | Method not found |
| -32602 | Invalid params |
| -32603 | Internal error |

## See also

- [MCP Tools Reference](../reference/mcp-tools.md) — the full tool catalog by tier
- [LLM Integration](llm-integration.md) — the IPC-side plan/apply executor
- [Agent Skills](agent-skills.md)
