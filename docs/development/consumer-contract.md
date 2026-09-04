# Downstream consumer contract

This repository's crates are consumed by closed-source downstream products
(a desktop GUI, and potentially other tools) that link them as libraries and
talk to the daemon over IPC. The surfaces below are **load-bearing**: renaming,
removing, or changing their semantics is a breaking change for those consumers
even when nothing inside this repository uses them. Anything *not* listed here
is fair game for tightening (`pub` → `pub(crate)`), refactoring, or removal.

This document is a snapshot of what downstream actually consumes; regenerate
it by auditing the downstream imports when planning a breaking change.

## Cargo features

- `conductor-core`: **`plugin`** and **`plugin-registry`** are enabled by the
  GUI. All other core features are used only inside this workspace.
- `conductor-daemon`: linked with **default features** (`mcp`). The feature
  names `mcp` / `llm-executor` / `mcp-write` / `audit-db` are additionally
  load-bearing for CI (ADR-045 composition matrix) — never rename.
- `conductor-capture`: **no external consumers.** Its API, CLI, and storage
  layout can change freely.

## Binary contract

- The daemon binary must be named **`conductor`** (`conductor.exe` on
  Windows). The GUI refuses to launch a daemon whose canonicalized basename
  differs, and launches it **bare — no CLI flags**. Its stdout/stderr are
  redirected to `<log_dir>/daemon-stdout.log`.
- `conductorctl` and `conductor-capture` are not executed by downstream.

## conductor-core public API used downstream

- Root re-exports: `Config`, `EventType` (`NoteOn`/`NoteOff`/`GamepadButton`),
  `PatternType` (`Chord`/`GamepadChord`/`LongPress`)
- `config::types`: `Config`, `ConnectorDirection`, `ConnectorProtocol`,
  `EndpointConfig`, `EndpointKind`, `RouteConfig`
- `config::validation`: `validate_config`, `ValidationFinding`, `Severity`
- `config::preferences`: `GuiPreferences`, `GuiSettings`, `DaemonSettings`,
  `TelemetrySettings`, `load_preferences`, `load_daemon_settings`,
  `atomic_write_toml`
- `events::InputEvent`; `gamepad_events::encoder_ids` (ID scheme)
- `identity::DeviceMatcher`
- `logging`: `log_dir`, `debug_env_enabled`, `component_directive`,
  `component_appender`
- `midi_output::MidiOutputManager` (`new`, `list_output_ports`)
- `plugin`: `PluginDiscovery` (`new`, `scan`), `Capability`, `PluginMetadata`
- `plugin_registry`: `PluginRegistry`, `PluginRegistryClient`
  (`new(url, cache_dir)`, `fetch_registry`, `install_plugin`),
  `enrich_registry_capabilities`, `validate_plugin_id`

## conductor-daemon public API used downstream

The GUI links the daemon crate **only for the IPC client + wire types and a
few utility surfaces**; all daemon operations go over the socket.

- `daemon::{IpcClient, IpcCommand, IpcRequest, IpcResponse, ResponseStatus,
  ErrorDetails, IpcErrorCode}` (both the `daemon::` re-exports and
  `daemon::types::` / `daemon::ipc::` module paths)
  - `IpcClient` methods: `connect`, `new(String)`, `send_request`,
    `send_command`, `into_reader` (streaming)
  - `IpcErrorCode` variants relied on: `ConfigNotFound`, `InternalError`,
    `StaleBaseGeneration`, `StaleBaseContent`
- `daemon::state::get_state_dir`
- `permissions::INPUT_MONITORING_DEEPLINK`
- `plugin_manager::PluginManager` (`default`, `default_plugins_dir`,
  `plugins_dir`, `discover_plugins`, `load_plugin`, `unload_plugin`,
  `enable_plugin`, `disable_plugin`, `grant_capability`, `revoke_capability`)
  and `PluginStats`
- `InputManager::list_gamepads`

## IPC wire contract

Newline-delimited JSON over a Unix socket;
`IpcRequest { id, command, args }` with `IpcCommand` serialized
SCREAMING_SNAKE_CASE. Socket path algorithm (mirrored downstream — a
change here breaks connection):

- macOS: `~/Library/Application Support/conductor/run/conductor.sock`
- Linux: `$XDG_RUNTIME_DIR/conductor/conductor.sock`, else
  `~/.conductor/run/conductor.sock`
- `/tmp`-rooted paths are rejected.

IPC commands sent by downstream (29 of the current variants — args shapes are
part of the contract):

`Ping`, `Status`, `Stop`, `Reload`, `ValidateConfig`, `GetConfigSnapshot`,
`GetConfigBody`, `GetConfigDiff`, `OverwriteConfigFile`, `SaveConfig`
(`{base_generation, config, base_revision?}`), `SwitchProfile`
(`{profile_name, config_path, profile_id?}`), `GetActiveProfile`,
`RefreshAppMappings`, `SwitchMode` (`{mode}`), `SetUiMode` (`{mode}`),
`SetLogLevel` (`{level}`), `SetDeviceEnabled` (`{device_id, enabled}`),
`CheckPermissions` (`{force}`), `GetProbeHistory` (`{port_name}`),
`StartMidiLearn`, `StopMidiLearn`, `GetMidiLearnEvents`, `SubscribeEvents`
(then NDJSON stream), `StopEventMonitor`, `SimulateMapping`
(`{mode_name, mapping_index, execute, value}`), `ApplyPlan` (`{plan_id}`),
`RejectPlan` (`{plan_id}`), `ListPendingPlans`, `ExecuteMcpTool`
(`{tool_name, arguments}`).

The remaining variants are currently unsent by downstream, but treat wire
renames of any variant as breaking.

## MCP tool names (sent via `ExecuteMcpTool`)

The daemon-implemented tool names downstream invokes are part of the
contract, notably: `conductor_probe_device_identity`,
`conductor_get_resolved_routing_graph`, `conductor_get_connector_metrics`,
plus the full catalogue forwarded by the GUI's LLM layer
(`conductor_batch_changes`, `conductor_create_endpoint`,
`conductor_create_mapping`, `conductor_create_profile`,
`conductor_delete_mapping`, `conductor_delete_profile`,
`conductor_get_active_profile`, `conductor_get_binding_health`,
`conductor_get_config`, `conductor_get_device_identity`,
`conductor_get_mapping`, `conductor_get_status`,
`conductor_get_topology_summary`, `conductor_list_device_bindings`,
`conductor_list_device_identities`, `conductor_list_devices`,
`conductor_list_mappings`, `conductor_list_profiles`,
`conductor_list_routes`, `conductor_get_routing_graph`,
`conductor_security_status`, `conductor_send_midi`,
`conductor_set_device_enabled`, `conductor_start_midi_learn`,
`conductor_stop_midi_learn`, `conductor_suggest_binding`,
`conductor_switch_mode`, `conductor_switch_profile`,
`conductor_update_mapping`, `conductor_update_mode`,
`conductor_validate_config`).

## Serialized types consumed wire-only (no Rust link)

Downstream frontend code models these serde shapes directly:

- `daemon::llm::executor::ExecutionResult` — `#[serde(tag = "type")]`,
  variants `Success | Logged | PlanCreated | HardwareIoConfirmation |
  RateLimited | Error`
- The LLM plan wire shape (`daemon::llm::plan`)
- `mcp_tools` arg naming (e.g. `mode_name`)
- The probe-on-connect payload shape

## File-layout contract

Downstream constructs these paths directly; they are part of the contract:

- State dir: `~/.conductor` (macOS/Linux), `%APPDATA%/conductor` (Windows) —
  files `crash-buffer.log`, `state.json`, `active_profile.json`,
  `preferences.toml`, `daemon.toml`
- Config dir: `<config_dir>/conductor/` — `config.toml`, `live.toml`,
  `profiles/`
- Log dir (`conductor_core::logging::log_dir()`): `daemon-stdout.log`,
  rotating `daemon.<date>.log`
- Plugins: `~/.conductor/plugins`
- Plugin-registry cache: `<cache_dir>/conductor/plugin-registry/`
