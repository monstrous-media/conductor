# Architecture Deep Dive

[`../architecture/overview.md`](../architecture/overview.md) gives the
high-level picture (three crates, one pipeline). This document goes one
level deeper: the real module layout inside `conductor-core` and
`conductor-daemon`, the event flow from input device to executed action,
the config compile pipeline, the plugin runtime, the IPC/MCP layer, and the
ADR-045 open-core feature boundary. The API surface downstream products are
allowed to depend on is pinned separately in
[`consumer-contract.md`](consumer-contract.md) — this document describes
internals that are free to change.

## Workspace layout

```
conductor/                     # root package — re-export-only compat layer
├── conductor-core/             # pure library, zero UI deps
├── conductor-daemon/           # background service + CLI + diagnostics
└── conductor-capture/          # standalone capture tool (early development)
```

- **`conductor` (root)** re-exports `conductor_core::*` and nothing else —
  its `src/lib.rs` is the entire source, with no sibling implementation
  files. It exists for backward-compatible import paths
  (`conductor::config`, `conductor::mappings`, `conductor::feedback`,
  `conductor::device_profile`, `conductor::event_processor`,
  `conductor::actions`); the crate's own doc comment says new code should
  use `conductor_core` directly.
- **`conductor-core`** (lib name `conductor_core`) is the engine: config
  schema, event processing, rule compilation, the plugin runtime (native
  and WASM), and security primitives shared with the daemon (and, per its
  `security` module's own doc comment, with the closed-source `conductor-gui`
  downstream). No daemon state, no sockets.
- **`conductor-daemon`** hosts the engine as a background service. Its
  `[[bin]]` targets: the `conductor` daemon binary itself, `conductorctl`
  (CLI client), `conductor-menubar`, `conductor-sign` (plugin signing,
  behind the `plugin-signing` feature), `conductor-skills`,
  `conductor-state`, and a set of hardware-free diagnostic tools
  (`midi_diagnostic`, `led_diagnostic`, `led_tester`, `pad_mapper`,
  `test_midi`, `midi_simulator`).
- **`conductor-capture`** is a separate, early-stage tool ("many features
  are stubbed out", per its own module doc) that records MIDI/gamepad input
  patterns under configurable privacy levels. It has no external consumers
  (see the consumer contract) and can change freely.

## Module layout

### `conductor-core/src/`

Top-level modules implementing the pipeline itself:

| Module | Responsibility |
|---|---|
| `event_processor.rs`, `event_types.rs`, `events.rs` | `MidiEvent`/`InputEvent`/`ProtocolEvent` types and the state machine that turns raw input into `ProcessedEvent`s (velocity, long-press, double-tap, chords, encoder direction) |
| `mapping.rs` | Backward-compatible `MappingEngine` (used by MCP tooling) |
| `rule_compiler.rs`, `rule_set.rs` | Compiles a `Config` into an immutable `CompiledRuleSet`, swapped into an `ArcSwap` for lock-free, wait-free (~1ns) reads on the hot path |
| `dispatch.rs` | `DispatchResult`/`DispatchOutcome` — structured return types from action execution |
| `actions.rs`, `transform.rs` | Action type definitions (platform-independent) and the MIDI value-transform pipeline (curve → scale → offset → invert → clamp) |
| `feedback.rs`, `midi_feedback.rs`, `mikro_leds.rs` | LED feedback trait abstraction, generic MIDI-note feedback, and HID-direct control for the Maschine Mikro MK3 |
| `identity.rs`, `resolver.rs`, `device.rs` | `DeviceId`/`DeviceMatcher`/`DeviceEvent<T>` and the `PortResolver` that binds OS ports to configured device identities |
| `gamepad_events.rs`, `gamepad_filters.rs`, `velocity.rs` | Gamepad → event conversion, dead-zone/threshold filtering, velocity-level classification |
| `plugin_id.rs`, `plugin_registry.rs` | Plugin identifier validation and the registry client (`plugin-registry` feature) |
| `osc_pattern.rs`, `control_state.rs`, `execution_context.rs`, `error.rs`, `logging.rs`, `midi_output.rs` | OSC address-pattern matching, program-change/CC state tracking, per-execution context, error types, log-directory resolution, virtual MIDI output |
| `engine.rs` | A minimal stub retained for API compatibility; per its own doc comment, "the full engine implementation is in conductor-daemon's `EngineManager`" |

Subdirectories:

- **`config/`** — the config schema and pipeline (see below): `types.rs`,
  `loader.rs`, `validation.rs`, `canonical.rs`, `revision.rs`,
  `provenance.rs`, `compile.rs`, plus narrower helpers
  (`control_state_analyzer.rs`, `feedback_loops.rs`, `midi_channel.rs`,
  `port_binding.rs`, `preferences.rs`, `protocol.rs`, `u8_string_map.rs`).
- **`device_intelligence/`** — SysEx identity probing (`probe.rs`),
  device fingerprinting (`fingerprint.rs`), and identity parsing
  (`sysex_identity.rs`).
- **`plugin/`** — the plugin runtime (see below).
- **`security/`** — capability vocabulary (`capabilities.rs`), risk tiers
  (`tiers.rs`), egress/network-ACL primitives (`egress.rs`,
  `network_acl.rs`), interpreter classification (`interpreters.rs`), OS
  keychain access (`keychain.rs`), and LLM spend budgeting
  (`llm_budget.rs`) — shared across `conductor-core`, `conductor-daemon`,
  and the downstream GUI.

### `conductor-daemon/src/`

| Module | Responsibility |
|---|---|
| `input_manager/` | `mod.rs`, `listen.rs`, `rescan.rs`, `rekey.rs`, `devices.rs` — unified MIDI + gamepad input, hot-plug rescan, device mute/enable. Deep dive: [`input-manager-architecture.md`](input-manager-architecture.md) |
| `daemon/engine_manager/` | The engine's lifecycle, config reload, per-device event dispatch, and every `IpcCommand` handler (split across `ipc_config.rs`, `ipc_devices.rs`, `ipc_dispatch.rs`, `ipc_learn.rs`, `ipc_llm.rs`, `ipc_plugins.rs`, `ipc_profile.rs`, `ipc_status.rs`) |
| `daemon/live_config/` | `LiveConfig` — the single mutation seam (ADR-034 §D1) every config-change operation flows through (LLM `ApplyPlan`, GUI `SaveConfig`, CLI import, rollback), publishing an immutable snapshot |
| `daemon/ipc.rs`, `ipc_framing.rs`, `ipc_rate_limit.rs` | The daemon control socket (see IPC/MCP layer below) |
| `daemon/mcp/`, `daemon/mcp_tools/`, `daemon/mcp_registry.rs`, `daemon/mode_mcp.rs` | The MCP server and its tool catalogue |
| `daemon/llm/` | `ToolExecutor`, `ConfigPlan` (TOCTOU-protected plan/apply), undo/redo, and the LLM plan wire types — gated behind the `llm-executor` feature |
| `daemon/audit/` | Structured audit logging of tool executions (hash-chained JSONL by default; SQLite via `rusqlite` behind the `audit-db` feature) |
| `daemon/security/` (daemon) + `security/` (crate root) | The capability/risk-tier enforcement gate (ADR-027), peer trust classification (macOS Team ID / Linux `SO_PEERCRED`), network-approval envelopes, singleton lock |
| `listeners/` | The network-listener edge for OSC/Art-Net input: ACL filter → rate limiter → audit (ADR-042 Phase A) |
| `transforms/` | Cross-protocol signal transforms (`hid_to_midi`, `midi_to_osc`, `osc_to_artnet`, etc.); same-protocol MIDI transform lives in `conductor-core::transform` |
| `route_engine.rs` | Evaluates the `[[routes]]` signal-routing graph as an explicit stage after the rule-engine matcher (see event flow below) |
| `connector_registry.rs` | Runtime state of the unified endpoint set (bound ports, activity metrics) |
| `action_executor/` | `mod.rs` plus `input_sim.rs`, `shell.rs`, `midi.rs`, `launch.rs`, `osc.rs`, `volume.rs`, `sandbox/` — executes actions against the host system |
| `plugin_manager.rs` | Plugin lifecycle: discovery, loading, permission checks, enable/disable/reload |
| `skills/` | Agent Skills validation, installation, and sandboxing |
| `migration/` | One-shot config migration helpers (e.g. legacy raw-forward → route lowering) |
| `bin/` | The diagnostic and CLI binaries listed above |

## Event flow

```
   MIDI port / gamepad / OSC / Art-Net
                  │
                  ▼
┌───────────────────────────────────────────┐
│ InputManager (conductor-daemon)           │
│  - hot-plug rescan (5s), port binding      │
│  - tags each event: DeviceEvent<ProtocolEvent> │
└───────────────────┬───────────────────────┘
                    │
                    ▼
┌───────────────────────────────────────────┐
│ EventProcessor (conductor-core, per-device)│
│  - velocity level, long-press, double-tap, │
│    chord buffering, encoder direction      │
└───────────────────┬───────────────────────┘
                    │ ProcessedEvent
                    ▼
┌───────────────────────────────────────────┐
│ CompiledRuleSet (conductor-core)          │
│  - Arc<ArcSwap<CompiledRuleSet>>: wait-free│
│    reads; device > any-device > global     │
│  - built by rule_compiler off the hot path │
└───────────────────┬───────────────────────┘
           matched? │        │ unmatched
           yes ▼    │        ▼
┌────────────────┐  │  ┌───────────────────────────┐
│ ActionExecutor │  │  │ RouteEngine (daemon)      │
│ (daemon)       │  │  │ - [[routes]] graph, ADR-031│
└────────────────┘  │  │ - explicit 9th stage after │
                     │  │   the rule-engine matcher  │
                     │  └───────────────────────────┘
```

`CompiledRuleSet` and `ArcSwap<ModeState>` give wait-free reads on the
hot path (per `rule_set.rs`'s own doc comment: "Reads are wait-free (~1ns),
and config reloads never block in-flight event processing"). The route
engine's evaluation-priority rule — mappings match before routes — is
ADR-036.

## Config compile pipeline

`conductor-core::config` is organized as a pipeline rather than a single
parse step:

1. **`loader.rs`** — loads, saves, and validates a config file.
2. **`types.rs`** — the `Config`/`Mode`/`Mapping`/`Trigger`/`ActionConfig`
   data model.
3. **`validation.rs`** — the unified validator: structural + security
   checks (formerly split across `loader.rs` and a separate validator) plus
   protocol-coverage checks, all flowing through one module.
4. **`compile.rs`** — parse-time lowering (ADR-025 Phase 2.E): expands
   sugar variants (`PcContextSwitch`, `CcContextSwitch`) into the canonical
   `Action::Conditional` chains the runtime evaluator actually sees, so the
   executor only has to handle one shape.
5. **`canonical.rs`** — deterministic TOML serialisation (ADR-034 §D5):
   lexicographic key ordering, preserved array order, no comments — used
   as the hashing input for revisions.
6. **`revision.rs`** — `ConfigRevision` = SHA-256 of the canonical bytes;
   used for audit display and on-disk tamper detection (the CAS protocol
   itself keys on a monotonic `state_generation`, not the revision, to
   avoid hash collisions between byte-identical mutations).
7. **`provenance.rs`** — tags every mutation with who initiated it and
   what was applied (ADR-034 §D6).

On the daemon side, `daemon/live_config/` is the single seam
(`LiveConfig`, ADR-034 §D1) every config-change operation — LLM
`ApplyPlan`, GUI `SaveConfig`, CLI import, rollback — flows through, on top
of this pipeline, publishing an immutable snapshot that the engine manager
reloads via `rule_compiler` off the async runtime (`spawn_blocking`) before
an atomic `ArcSwap::store()`.

## Plugin runtime

Two execution models share the same discovery/capability/signing
machinery in `conductor-core::plugin`:

- **Native**: `loader.rs` dynamically loads `.so`/`.dylib`/`.dll` plugins
  via `libloading`; `discovery.rs` scans the plugins directory and its
  manifests; `action_plugin.rs`/`trigger_plugin.rs` define the `ActionPlugin`
  trait plugins implement.
- **WASM**: `wasm_runtime.rs` runs plugins compiled to WebAssembly under
  `wasmtime`/`wasmtime-wasi` (the `plugin-wasm` feature) — sandboxed memory,
  resource limits, and WASI capability-based permissions, so the same
  `.wasm` runs unmodified on macOS/Linux/Windows.

Shared machinery: `capability.rs` (the permission system plugins request
against), `metadata.rs` (manifest parsing), and the signing/trust chain —
`signing.rs`, `key_rotation.rs`, `revocation.rs`, `registry_trust.rs`,
`registry_escrow.rs` — Ed25519-signed plugins verified against a registry.
`conductor-daemon::plugin_manager` owns the runtime lifecycle: discovery,
loading, permission checks, enable/disable/reload.

Full guides: [`plugin-development.md`](plugin-development.md),
[`wasm-plugin-development.md`](wasm-plugin-development.md),
[`plugin-security.md`](plugin-security.md).

## IPC / MCP layer

Two distinct sockets, both newline/JSON-based Unix domain sockets:

- **Daemon control socket** (`daemon/ipc.rs`) — newline-delimited JSON,
  `IpcRequest { id, command, args }` / `IpcResponse`. Hardening per its own
  doc comment: 1MB request-size cap, 10-second operation timeout, socket
  directory `0700` / socket file `0600` with ownership validation. Every
  `IpcCommand` variant is dispatched from `daemon/engine_manager/ipc_*.rs`.
  This is the socket downstream GUIs and `conductorctl` actually talk to;
  its wire contract is pinned in [`consumer-contract.md`](consumer-contract.md).
- **MCP socket** (`daemon/mcp/mod.rs`, socket name `conductor-mcp.sock`) —
  JSON-RPC 2.0 (`initialize`, `tools/list`, `tools/call`) for external LLM
  agents (Claude Code, Cursor, etc.), independent of the control socket.
  Tool definitions live in `daemon/mcp_tools/`. The control socket also
  exposes an `ExecuteMcpTool` command that runs the same tool executor
  without a second connection — this is how the downstream GUI's LLM layer
  invokes MCP tools today.

Full protocol walkthrough: [`mcp-server.md`](mcp-server.md). Tool catalogue
and wire shapes: [`../reference/mcp-tools.md`](../reference/mcp-tools.md),
[`../reference/api-mcp.md`](../reference/api-mcp.md).

## ADR-045 feature boundary

`conductor-daemon`'s Cargo features draw the open-core line, and per the
crate's own `Cargo.toml` comment the feature *names* are load-bearing —
the public CI composition matrix builds exactly these and they must never
be renamed:

| Feature | Default? | What it adds |
|---|---|---|
| `mcp` | Yes (`default = ["mcp"]`) | Read-only MCP inspection tools (status, device enumeration, config read, event streams, diagnostics) |
| `llm-executor` | No | Write machinery — `ToolExecutor`, `ConfigPlan` (TOCTOU plan/apply), undo/redo — reachable only over the GUI's IPC protocol, **not** the MCP socket. Pulls in `audit-db`. This is what the Studio-bundled daemon ships. |
| `mcp-write` | No | `mcp` + `llm-executor`, and additionally exposes the Stateful/ConfigChange/HardwareIO tool tiers **over the MCP socket itself**. Not enabled in any official artifact — source-builders only. |
| `audit-db` | No (implied by `llm-executor`) | SQLite-backed audit log (`rusqlite`); without it the audit subsystem uses a lightweight append-only sink |

CI (`.github/workflows/ci.yml`, the `compositions` job) builds all four
official points in this matrix — `--no-default-features`, the OSS default,
`--features llm-executor`, and `--features mcp-write` — package-scoped
(`-p conductor-daemon`, never `--workspace`) so feature unification can't
leak paid features into the OSS artifact. The full Cargo-feature contract,
including which `conductor-core` features the downstream GUI links, is in
[`consumer-contract.md`](consumer-contract.md).

## Related documentation

- [Architecture overview](../architecture/overview.md)
- [InputManager deep dive](input-manager-architecture.md)
- [MCP server implementation](mcp-server.md)
- [LLM integration](llm-integration.md)
- [Plugin development](plugin-development.md) ·
  [WASM plugins](wasm-plugin-development.md) ·
  [Plugin security](plugin-security.md)
- [Downstream consumer contract](consumer-contract.md)
- [Decision-record index](../architecture/adrs.md)
- [Testing guide](testing.md)
