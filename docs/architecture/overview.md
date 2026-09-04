# Architecture overview

Conductor is a three-crate Rust workspace:

```
 input devices (MIDI / HID gamepad / OSC / Art-Net)
        │
        ▼
┌─────────────────────────────────────────────┐
│ conductor-daemon  (the `conductor` binary)  │
│  input manager → engine → action executor   │
│  IPC server (Unix socket) · MCP server      │
└──────────────────┬──────────────────────────┘
                   │ links
                   ▼
┌─────────────────────────────────────────────┐
│ conductor-core  (pure library)              │
│  config load/validate/compile · event       │
│  processing · mapping + routing · device    │
│  intelligence · plugin runtime · feedback   │
└─────────────────────────────────────────────┘

 conductor-capture — standalone input-capture tool (privacy-aware)
```

- **`conductor-core`** is a pure library: no daemon state, no sockets. It owns
  the config schema (`[[modes]]`, `[[global_mappings]]`, `[[endpoints]]`,
  `[[routes]]`), trigger/action types, the event processor, and the plugin
  runtime (native dylib and sandboxed WASM, Ed25519-signed).
- **`conductor-daemon`** hosts the engine as a background service. Clients —
  `conductorctl`, GUIs, LLM agents — talk to it over newline-delimited JSON on
  a Unix socket, and over MCP for tool-based inspection. The ADR-045 cargo
  features draw the open-core boundary: the default build (`mcp`) exposes
  read-only MCP tools; `mcp-write` / `llm-executor` / `audit-db` are opt-in.
- **`conductor-capture`** records input patterns to disk with configurable
  privacy levels, independent of the daemon.

Deep dives: [`../development/architecture.md`](../development/architecture.md),
[`../development/input-manager-architecture.md`](../development/input-manager-architecture.md),
[`../development/mcp-server.md`](../development/mcp-server.md), and the
decision-record index in [`adrs.md`](adrs.md). The API surface downstream
products rely on is pinned in
[`../development/consumer-contract.md`](../development/consumer-contract.md).
