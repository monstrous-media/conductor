# LLM Integration Architecture

This document describes the **daemon-side** LLM integration: the tool
catalog, the risk-tier execution model, and the plan/apply flow. It does not
cover any chat UI — this repository doesn't contain one. A closed-source
downstream product (a desktop GUI) implements its own chat surface and LLM
provider wiring on top of the contract described here; that code lives
outside this repository.

## Two ways an LLM reaches Conductor

1. **The MCP socket** (`conductor-mcp.sock`) — any MCP-speaking client
   (Claude Desktop, Cursor, a script) connects directly and calls
   `tools/list` / `tools/call`. What's available depends on how the daemon
   was compiled (see below). This is the surface documented in
   [MCP Tools Reference](../reference/mcp-tools.md) and
   [MCP Server Implementation](mcp-server.md).
2. **The daemon's IPC protocol** — a GUI client connects over the IPC socket
   and issues an `ExecuteMcpTool` command (alongside sibling commands
   `ApplyPlan` / `RejectPlan` / `ListPendingPlans`). This is what a chat-style
   product wires an LLM's tool calls through, because it gets the full
   risk-tier handling — plan/apply, hardware confirmation, undo/redo, audit
   logging — that the thinner MCP socket path doesn't need for ReadOnly-only
   builds. See [Downstream consumer contract](consumer-contract.md) for the
   exact wire shapes.

Both paths dispatch against the **same** tool catalog and risk-tier table
(`mcp_tools::get_tool_definitions`, `mcp_tools::get_tool_risk_tier`) — a tool
named `conductor_create_mapping` means the same thing and carries the same
tier whichever way it's invoked.

## The `llm-executor` feature (ADR-045)

```toml
llm-executor = ["audit-db"]
mcp-write = ["mcp", "llm-executor"]
```

`llm-executor` compiles in `llm::executor::ToolExecutor` — the full
plan/apply/confirmation/undo machinery — plus the write-tier risk
classification (`mcp_tools::write_tiers`). It does **not** by itself expose
write tools on the MCP socket; that additionally requires `mcp-write`. A
daemon built without `llm-executor` returns a fixed error for
`ExecuteMcpTool` on the IPC path: *"LLM tool execution is not available in
this build ... source builds can enable the `llm-executor` cargo feature."*

## Plan/apply flow

### `ExecutionResult` (`daemon/llm/executor.rs`)

`ToolExecutor::execute(tool_name, arguments, caller_ctx)` looks up the tool's
risk tier, runs it through the ADR-027 security gate and the per-tier rate
limiter, then dispatches to one of:

```rust
pub enum ExecutionResult {
    Success { result: ToolCallResult },                 // ReadOnly
    Logged { result: ToolCallResult, log_entry: LogEntry }, // Stateful
    PlanCreated { plan: ConfigPlan },                    // ConfigChange
    HardwareIoConfirmation { status: ConfirmationStatus, tool_name: String }, // HardwareIO
    RateLimited { tier: ToolRiskTier, current: u32, limit: u32, retry_after_secs: u64 },
    Error { message: String },                           // gate denial, validation failure, etc.
}
```

A caller (the IPC dispatch layer) matches on this enum to decide what to send
back over the wire: a `Success`/`Logged` result is returned immediately; a
`PlanCreated` plan is handed back to the client for review before a follow-up
`ApplyPlan`/`RejectPlan` call; a `HardwareIoConfirmation` requires a
confirmation round-trip; `RateLimited` and `Error` are surfaced as-is.

### `ConfigPlan` and `ConfigChange` (`daemon/llm/plan.rs`)

`ConfigChange` is the set of persistable mutations a `ConfigChange`-tier tool
can request — `CreateMapping`, `UpdateMapping`, `DeleteMapping`, `CreateMode`,
`DeleteMode`, `InsertMapping`/`RestoreMode` (undo inverses),
`CreateEndpoint`, `CreateRoute`, `UpdateRoute`, `DeleteRoute`.

`ConfigPlan` wraps one or more `ConfigChange`s with TOCTOU (time-of-check to
time-of-use) protection:

1. `base_state_hash` — a hash of the config at plan-creation time.
2. On apply, the current config hash is recomputed and compared; a mismatch
   fails with `PlanError::StateChanged`.
3. Plans expire 5 minutes after creation (`PlanError::Expired`).
4. Before commit, the resulting config is revalidated against
   `conductor_core::config::validation::validate_config()` — a plan that
   would produce an invalid config is rejected (`PlanError::ValidationFailed`)
   rather than silently corrupting the live config.

Other `PlanError` variants: `ModeNotFound`, `IndexOutOfRange`,
`InvalidTrigger`, `InvalidAction`, `NotFound` (unknown plan ID).

### Security gate and rate limiting

Every `ToolExecutor::execute` call goes through `security::gate::enforce`
first — a caller's pinned trust level (from `CallerContext`, ADR-027 D1)
determines whether the requested tier is `Allow`ed, `AllowWithAudit`ed, or
`Deny`d outright, before any rate-limit budget is consumed. Tools that pass
the gate are then checked against the per-tier `RateLimiter`
(`daemon/ratelimit/`) — exceeding a tier's budget short-circuits to
`ExecutionResult::RateLimited` without executing.

## How MCP tools are exposed to LLM clients

- **`tools/list`** on the MCP socket returns `mcp_tools::get_tool_definitions()`
  — ReadOnly tools always, write-tier tools only under `mcp-write`. Each
  `ToolDefinition` carries `name`, `description`, and a JSON Schema
  `input_schema` an LLM's function-calling layer can consume directly.
- **`tools/call`** dispatches by risk tier: ReadOnly calls go through the
  stateless `McpToolExecutor`; everything else is either rejected (if
  `mcp-write` isn't compiled in) or routed to the same tier-aware handling
  described above.
- A registered MCP peer's tier ceiling (`conductorctl mcp register`) further
  restricts what that specific client may invoke, independent of what the
  binary compiled in — see [MCP Server Implementation](mcp-server.md#peer-registration-and-tier-ceilings).

## Downstream GUI clients

A closed-source desktop GUI (and potentially other downstream products) links
this daemon's IPC client types and drives a chat-style interface: it sends
user intent to an LLM provider of its choosing, receives tool calls back, and
issues them as `ExecuteMcpTool` IPC commands against the daemon — reviewing
`ConfigPlan`s with the user before calling `ApplyPlan`. None of that provider
integration, UI, or API-key handling lives in this repository; what's
documented here is the daemon-side contract those products build against.

## Where to go next

- [llm-reference.md](../llm-reference.md) — the canonical, token-budgeted
  reference an LLM system prompt should actually load: config schema,
  triggers, actions, routing graph tools, common patterns.
- [Downstream consumer contract](consumer-contract.md) — the exact IPC wire
  types and socket-path contract a client must match.
- [MCP Tools Reference](../reference/mcp-tools.md) — full tool catalog by tier.
- [MCP Server Implementation](mcp-server.md) — module layout, adding new
  tools, peer registration.
