# CLI Commands Reference

## Overview

The `conductor` workspace builds several binaries. This reference covers all of
them, grounded in their actual `clap` argument definitions (or, for the
hardware-probe tools that predate `clap`, their actual `main()` behavior).

| Binary | Crate | Purpose |
|--------|-------|---------|
| `conductor` | `conductor-daemon` | The background daemon — loads config, connects to devices, processes events |
| `conductorctl` | `conductor-daemon` | Control and inspect a running daemon over its Unix-domain-socket IPC, plus install it as a service |
| `conductor-sign` | `conductor-daemon` | Generate/rotate Ed25519 keys and sign/verify WASM plugins |
| `conductor-skills` | `conductor-daemon` | Validate, list, and install Agent Skills (see [Agent Skills](../development/agent-skills.md)) |
| `conductor-state` | `conductor-daemon` | Dump the daemon's live physical-control-state store |
| `midi_diagnostic` | `conductor-daemon` | Print incoming MIDI events in real time |
| `led_diagnostic` | `conductor-daemon` | Interactively correlate pad presses with LED addresses (Maschine Mikro MK3 only) |
| `led_tester` | `conductor-daemon` | Probe HID report offsets to find a pad's LED address (Maschine Mikro MK3 only) |
| `pad_mapper` | `conductor-daemon` | Capture MIDI-note ↔ HID-pad-index pairs (Maschine Mikro MK3 only) |
| `test_midi` | `conductor-daemon` | List MIDI input/output ports and exit |
| `midi_simulator` | `conductor-daemon` | Interactive REPL that fabricates MIDI events without hardware |
| `conductor-capture` | `conductor-capture` | Input-pattern recording tool (early development — most subcommands are stubs) |

`conductor_menubar` (a system-tray helper spawned by the desktop app) also
lives in `conductor-daemon`'s `[[bin]]` list, but it takes no CLI arguments —
it's not covered here. The **closed-source GUI application is not part of
this repository.**

All examples below assume an installed binary on `PATH` (e.g. `conductorctl
status`). From a source checkout, run any binary with `cargo run -p
conductor-daemon --bin <name> --` (note the `--` separator before the
binary's own flags), or `cargo run -p conductor-capture --` for
`conductor-capture`.

---

## `conductor` — the daemon

```bash
conductor [OPTIONS]
```

The daemon binary. It has no subcommands — just flags — and always runs in
the foreground; use systemd/launchd directly, or `conductorctl install` (see
below) for macOS LaunchAgent management.

### Flags

| Flag | Description |
|------|-------------|
| `-c, --config <FILE>` | Path to the config file. If given, this path is **adopted**: the daemon overwrites its live config with it and boots from that. Without `--config`, the daemon resolves a default path (see below) and also restores the last-active profile identity. |
| `-v, --verbose` | Debug-level logging for all Conductor modules. |
| `-T, --trace` | Trace-level logging (very verbose). Outranks `--verbose` and `DEBUG=1`. |
| `-f, --foreground` | Accepted for compatibility; the daemon already runs in the foreground by default. |
| `--ignore-user-mappings` | Ignore `~/.conductor/gamecontrollerdb.txt` (the user gamepad-mapping override file) for this run — use this to recover when a bad override file prevents a controller from being recognized. |
| `-h, --help` / `-V, --version` | Standard clap help/version. |

There is no port argument, no `--led`, no `--profile`, and no `--pad-page`
flag on this binary — LED schemes, device selection, and per-app profiles are
all driven by the config file and by `conductorctl` (see below), not by
daemon CLI flags.

### Default config path

Without `--config`, the daemon looks in an OS-specific directory:

| OS | Default path |
|----|---------------|
| macOS | `~/Library/Application Support/conductor/config.toml` |
| Linux | `$XDG_CONFIG_HOME/conductor/config.toml`, falling back to `~/.config/conductor/config.toml` |
| Windows | `%APPDATA%\conductor\config.toml` |

If no config exists at that path, the daemon prints a minimal example
`config.toml` and exits non-zero rather than starting with nothing.

### Environment variables

| Variable | Effect |
|----------|--------|
| `DEBUG=1` | Enables debug-level logging (same effect as `--verbose`, unless overridden by `--trace` or `RUST_LOG`). |
| `RUST_LOG` | Standard `tracing-subscriber` env filter; takes precedence over `DEBUG` and the CLI flags, e.g. `RUST_LOG=conductor_daemon=trace`. |
| `CONDUCTOR_LOG_CONSOLE` | Force console log output on/off (console logging is otherwise auto-detected from whether stdout is a terminal, to avoid double-logging when a GUI spawns the daemon). |

Logs also always go to a rotating file under the platform log directory,
independent of whether console logging is active.

### Examples

```bash
# Run with the default config path, debug logging
conductor --verbose

# Run against an explicit config (adopted as the live config)
conductor --config ~/conductor-configs/dev.toml

# Trace-level logging for deep debugging
conductor --trace

# From a source checkout
cargo run -p conductor-daemon --bin conductor -- --verbose
```

---

## `conductorctl` — daemon control and service management

```bash
conductorctl [GLOBAL OPTIONS] <COMMAND> [ARGS]
```

Most subcommands talk to a **running** daemon over its IPC socket and fail
with "Failed to connect to daemon. Is the daemon running?" if it isn't up.
A few (`install`, `uninstall`, `start`, `stop`, `restart`, `enable`,
`disable`, `service-status`, the local `profile` subcommands, `migrate-config`,
`validate-schema`, `permissions`, `mcp`, `llm budgets show`, and — on Unix —
`listener` / `security`) work without a running daemon.

### Global options

| Flag | Description |
|------|-------------|
| `-v, --verbose` | Enable debug logging in `conductorctl` itself. |
| `-j, --json` | Emit machine-readable JSON instead of formatted text, for every subcommand. |

### Daemon lifecycle (via IPC)

| Command | Args | Description |
|---------|------|--------------|
| `status` | — | Daemon state, current mode, config path, uptime, event count, and hot-reload performance stats. |
| `reload` | — | Trigger a hot config reload (no restart, zero-downtime). |
| `validate` | `[-c, --config <PATH>]` | Ask the daemon to validate a config file (defaults to the daemon's own config if omitted). |
| `ping` | — | Round-trip health check; prints latency. |
| `shutdown` | — | Gracefully stop the **running daemon process** via IPC (state is saved first). Only stops the process — if it's also installed as a service, the service will restart it on the next login unless you also run `disable`. |

```bash
conductorctl status
conductorctl status --json | jq '.data.reload_stats.avg_reload_ms'
conductorctl reload
conductorctl ping
```

### Device management (via IPC)

| Command | Args | Description |
|---------|------|--------------|
| `list-devices` | — | List available MIDI and HID/gamepad input devices. |
| `bindings` | `[-a, --alias <ID>]` `[--unbound-only]` | Show which `[[bindings]]`/`[[endpoints]]` aliases the daemon has resolved to real ports, versus opportunistic (unconfigured) ports it's listening to. `--alias` filters by exact `device_id` — for opportunistic ports that's `raw:<port name>`. |
| `set-device` | `<PORT>` | Switch the daemon to a different MIDI input port index, without restart. |
| `get-device` | — | Show the currently connected MIDI device, connection status, and time since the last event. |

```bash
conductorctl list-devices
conductorctl bindings --unbound-only
conductorctl bindings --alias "raw:IAC Driver Bus 1"
conductorctl set-device 2
conductorctl get-device
```

### Mode control (ADR-040)

| Command | Args | Description |
|---------|------|--------------|
| `mode set` | `<NAME>` `[--no-lock]` | Switch the active mode. **Locks it against per-app auto-switching by default** — pass `--no-lock` to switch without locking, leaving auto-switch active. |
| `mode unlock` | — | Release a manual mode lock, resuming per-app auto-switching. |
| `mode status` | — | Show the active mode, whether it's locked, and the lock's origin. |

```bash
conductorctl mode set DJ            # switches to DJ and locks it
conductorctl mode set DJ --no-lock  # switches to DJ, auto-switch stays live
conductorctl mode unlock
conductorctl mode status
```

### LED control

| Command | Args | Description |
|---------|------|--------------|
| `led status` | — | Show whether LEDs are enabled, current brightness, active scheme, and idle timeout. |
| `led scheme` | `<NAME>` | Set the lighting scheme (`off`, `static`, `breathing`, `pulse`, `rainbow`, `wave`, `sparkle`, `reactive`, `vumeter`, `spiral`). |
| `led brightness` | `<LEVEL>` | Set brightness, `0`–`127`. |
| `led off` | — | Shortcut for `led scheme off`. |

```bash
conductorctl led scheme rainbow
conductorctl led brightness 64
conductorctl led off
```

### Plugin management

| Command | Args | Description |
|---------|------|--------------|
| `plugin list` | — | List available and currently-loaded plugins. |
| `plugin info` | `<NAME>` | Show a plugin's version, description, and author. |
| `plugin enable` | `<NAME>` | Enable a plugin. |
| `plugin disable` | `<NAME>` | Disable a plugin. |

### Live event monitoring

| Command | Args | Description |
|---------|------|--------------|
| `events` | see below | Tail or snapshot input events flowing through the daemon. |
| `playback-events` | `<FILE>` `[--speed <N>]` `[--format text\|json]` `[--no-delay]` | Replay a previously-exported JSON/CSV event file, with timing preserved (or `--no-delay` to dump instantly). |

`events` flags:

| Flag | Description |
|------|-------------|
| `-f, --follow` | Continuously tail live events instead of a bounded snapshot. |
| `--type <TYPE>` | Filter by event type (`note_on`, `note_off`, `cc`, `encoder`, `gamepad_button`, etc). |
| `--channel <1-16>` | Filter by MIDI channel (1-based on the CLI; stored 0-based internally). |
| `--note-min <0-127>` / `--note-max <0-127>` | Restrict to a note range. |
| `--device <ID>` | Filter by device ID. |
| `--since <DUR>` | Only events newer than this (e.g. `30s`, `5m`, `1h`). |
| `--debounce <MS>` | Minimum milliseconds between displayed events. |
| `-F, --filter <NAME>` | Use a named filter from `[event_console.filters]` in the config; CLI flags override it field-by-field. |
| `--format text\|json` | Output format (default `text`). |
| `--limit <N>` | Max events to show in non-follow mode (default `50`); does not cap `--output` exports. |
| `-o, --output <FILE>` | Export a snapshot to JSON or CSV (by extension). Conflicts with `--follow`. |
| `--duration <DUR>` | How long to capture before exporting. Conflicts with `--follow`. |
| `--profiling` | Include per-event processing-time/memory data, when available. |

```bash
conductorctl events --follow
conductorctl events --type note_on --channel 10 --limit 20
conductorctl events --since 5m --output events.json --duration 30s
conductorctl playback-events events.json --speed 2.0
```

### Security audit log (ADR-027 §D13a)

| Command | Args | Description |
|---------|------|--------------|
| `audit tail` | `[-f, --follow]` `[--last <N>]` | Show recent audit-log entries (tool executions, plan decisions, denials); `--follow` streams new ones live. Default `--last 50`. |
| `audit denied` | `[-f, --follow]` `[--last <N>]` | Same as `tail`, filtered to denial events only. |
| `audit resume` | — | Recover the daemon from the fail-closed `AuditDegraded` state after a corrupt/unreadable audit outbox — rotates the corrupt file aside and starts a fresh chain. |

```bash
conductorctl audit tail --last 20
conductorctl audit denied --follow
```

### MCP client registry (ADR-027 §D18)

Per-client tier ceilings for MCP clients (Claude Desktop, Cursor, etc.)
connecting over the daemon's Unix socket. Storage is a local JSON file; these
are plain file operations, no running daemon required.

| Command | Args | Description |
|---------|------|--------------|
| `mcp register` | `--name <NAME>` `--exe-path <PATH>` `--tier <TIER>` | Register (or update) a client. `--exe-path` must match the client's `initial_exe` exactly. `--tier` accepts `ReadOnly`, `Stateful`, `ConfigChange`, `HardwareIO` (or their `snake_case` wire forms — `read_only`, etc.); `Internal` is not accepted from the CLI. |
| `mcp list` | — | List all registered clients. |
| `mcp revoke` | `--exe-path <PATH>` | Revoke a registration by exe path. Idempotent — revoking an unregistered path is a no-op. |

```bash
conductorctl mcp register --name "Claude Desktop" --exe-path /Applications/Claude.app/Contents/MacOS/Claude --tier Stateful
conductorctl mcp list
conductorctl mcp revoke --exe-path /Applications/Claude.app/Contents/MacOS/Claude
```

### LLM agent budget (ADR-027 §D6)

| Command | Args | Description |
|---------|------|--------------|
| `llm budgets show` | `[-c, --config <PATH>]` `[--json]` | Show the effective `[security.llm]` budget (iterations, tool calls, tokens, wall-clock, capability quotas). File-only and offline — never touches a running daemon. |

### Network listener approvals (Unix only, ADR-042)

Non-loopback OSC/Art-Net listeners only bind once explicitly approved. These
edit the HMAC-signed approval registry at `~/.conductor/network_approvals.json`
directly; the daemon honours a change on its next (re)bind.

| Command | Args | Description |
|---------|------|--------------|
| `listener list` | `[-c, --config <PATH>]` | List configured network listeners and approval status. |
| `listener status` | `[-c, --config <PATH>]` | Same as `list`, with per-listener detail (host:port, amplification-ack requirement). |
| `listener approve` | `<ALIAS>` `[-c, --config <PATH>]` | Approve a non-loopback listener by its `[[endpoints]]` alias. Loopback listeners are auto-approved and don't need this. |
| `listener deny` | `<ALIAS>` `[-c, --config <PATH>]` | Revoke a listener's approval. |

### Security key management (Unix only, ADR-042)

| Command | Args | Description |
|---------|------|--------------|
| `security status` | — | Show the network-approval HMAC key's fingerprint, age, and rotation warnings. |
| `security rotate-hmac` | — | Rotate the HMAC key; existing approvals are re-signed under the new key. |

### macOS permissions (ADR-029 §D3)

| Command | Args | Description |
|---------|------|--------------|
| `permissions` | `[--check]` `[--open-input-monitoring]` | Inspect or open the macOS Input Monitoring grant needed for gamepad enumeration. Defaults to `--check` if neither flag is given. Tries the running daemon's own IPC-reported grant first (authoritative), falling back to a local probe if the daemon isn't reachable. On Linux/Windows, prints platform-appropriate guidance instead. |

```bash
conductorctl permissions --check
conductorctl permissions --open-input-monitoring
```

### Config rollback and drift (ADR-034)

| Command | Args | Description |
|---------|------|--------------|
| `rollback-config` | — | Roll the live config back to the last known-good snapshot (CAS-protected). |
| `rollback-config-force` | `--reason <TEXT>` (required) | Break-glass rollback that bypasses the CAS check. **CLI-only** — the daemon rejects this from GUI/LLM peers. `--reason` is audited and must be non-empty. |
| `config drift` | — | Read-only: report whether the on-disk config has diverged from the daemon's live config. |
| `config mark-known-good` | — | Mark the daemon's current live config as the known-good snapshot a later `rollback-config` returns to. |
| `config reload` | `[--path <PATH>]` | Re-read the daemon's config file from disk and republish it (the "I hand-edited the file, pick it up" flow). With `--path`, loads that file instead (must be allowlisted). |
| `config import` | `<PATH>` | Import a config from an explicit allowlisted `.toml` path (e.g. promoting a stash file). |
| `config save` | `[--base-generation <N>]` | Commit a config read from **stdin**. To load a file from disk, use `config import` instead — `save` rejects a positional path, since that would bypass the daemon's path allowlist. |

```bash
conductorctl config drift
conductorctl config mark-known-good
conductorctl rollback-config
conductorctl rollback-config-force --reason "bad hand-edit, need last-known-good immediately"
cat generated-config.toml | conductorctl config save
```

### Config migration

| Command | Args | Description |
|---------|------|--------------|
| `migrate-config` | `--routing` (required) `[-c, --config <PATH>]` `[--no-backup]` `[--dry-run]` | Rewrite legacy `Trigger::Raw` + `Action::MidiForward` mappings into top-level `[[routes]]` entries (ADR-036), preserving comments/formatting. |

The legacy `[device]` → `[[devices]]` migration and the `[[bindings]]` /
`[[connectors]]` formats it targeted **no longer exist** — ADR-035 removed
them entirely, so `migrate-config` without `--routing` now fails with an
explicit error telling you to author `[[endpoints]]` directly (see the
[Configuration Schema Reference](config-schema.md)).
`--routing` is the only supported migration today, and it aborts the whole
migration (no partial rewrite) if it finds a `Raw` trigger paired with
anything other than `MidiForward`.

```bash
# Preview the routing migration without writing
conductorctl migrate-config --routing --dry-run

# Apply it (writes a .bak backup unless --no-backup)
conductorctl migrate-config --routing

# Without --routing: fails on purpose
conductorctl migrate-config
# Error: migrate-config now supports only --routing (legacy
# [[bindings]]/[[connectors]]/[device] have been removed; author
# [[endpoints]] directly).
```

### Schema validation

| Command | Args | Description |
|---------|------|--------------|
| `validate-schema` | `[-c, --config <PATH>]` | Validate a config against MIDI/HID/OSC protocol constraints (note/CC ranges, etc.) with a per-protocol coverage report. Defaults to `~/.conductor/config.toml` if `--config` is omitted — note this is a different default than the daemon's own OS-specific config path. |

### Profile management

"Profile" here means a named, standalone `config.toml` under the profiles
directory — not the removed `.ncmm3` device-editor profile concept. `status`
and `switch` talk to a running daemon; `list`, `create`, `delete`, and
`validate` are local file operations.

| Command | Args | Description |
|---------|------|--------------|
| `profile status` | — | Show the daemon's currently active profile name and config path. |
| `profile switch` | `<NAME_OR_PATH>` | Switch the daemon to a different profile — by name (resolved under the profiles directory) or by an explicit `.toml` path. |
| `profile list` | `[DIR]` | List `.toml` files in a profiles directory (default: the OS config dir's `conductor/profiles/`). |
| `profile create` | `<NAME>` `[--app <BUNDLE_ID>]...` | Create a new profile `.toml` with a starter config. Repeat `--app` for multiple bundle IDs (creates one mode per app instead of a single `Default` mode). |
| `profile delete` | `<NAME>` `[--force]` | Delete a profile file. Refuses to delete the currently active one. `--force` skips the confirmation prompt. |
| `profile validate` | `<PATH>` | Validate a profile `.toml` without loading it into the daemon. |

```bash
conductorctl profile list
conductorctl profile create streaming --app com.obsproject.obs-studio
conductorctl profile switch streaming
conductorctl profile status
conductorctl profile validate ~/.config/conductor/profiles/streaming.toml
conductorctl profile delete streaming --force
```

### Service management (macOS LaunchAgent only)

These commands manage Conductor as a LaunchAgent. The plist has
`RunAtLoad=true`, so `install`/`start`/`enable` all start the daemon
immediately — they differ in whether auto-start-on-login is also set.

| Command | Args | `launchctl` op | Starts daemon? | Auto-start on login? |
|---------|------|-----------------|-----------------|------------------------|
| `install` | `[--install-binary]` `[-f, --force]` | `load` | Yes | No |
| `start` | `[-w, --wait <SECS>]` (default `5`) | `load` | Yes | No |
| `enable` | — | `load -w` | Yes | Yes |
| `stop` | `[-f, --force]` | `unload` | Stops | No |
| `disable` | — | `unload -w` | Stops | No |
| `restart` | `[-w, --wait <SECS>]` | stop, wait 500ms, start | Yes | (unchanged) |
| `uninstall` | `[--remove-binary]` `[--remove-logs]` | — | Stops, removes plist | — |
| `service-status` | — | — | Reports install/load state | — |

`--install-binary` (on `install`) copies the daemon binary to
`/usr/local/bin/conductor`, typically requiring `sudo`. `stop --force` skips
attempting a graceful IPC shutdown before unloading. Service management is
currently macOS-only.

```bash
cargo build --release --bin conductor
conductorctl install --install-binary   # installs + starts
conductorctl enable                     # + auto-start on login
conductorctl service-status
conductorctl restart --wait 10
conductorctl disable
conductorctl uninstall --remove-binary --remove-logs
```

---

## `conductor-sign` — plugin signing

```bash
conductor-sign <COMMAND> [ARGS]
```

Signs and verifies WASM plugins with Ed25519 keys. Key-generation and signing
require the `plugin-signing` feature (`cargo build -p conductor-daemon
--features plugin-signing`); without it, `generate-key` exits with an error
telling you to rebuild.

| Command | Args | Description |
|---------|------|--------------|
| `generate-key` | `<output-path>` | Generate a new Ed25519 keypair (writes `<path>.private`, mode `0600`, and `<path>.public`). Refuses to overwrite an existing key. |
| `sign` | `<plugin> <key> --name <NAME> --email <EMAIL>` | Sign a plugin. `--name`/`--email` are required. |
| `verify` | `<plugin>` | Verify a plugin's signature. |
| `migrate-keys` | `<plugin>` | Migrate a legacy `.sig` file to a root-only manifest. |
| `rotate-key` | `<old> <new> <manifest>` | Append a key-rotation record to a manifest. |
| `sign-registry` | `<reg.json> <key> <out>` | Sign a plugin registry JSON file. |
| `trust add` | `<public-key> <name>` | Add a trusted signer key. |
| `trust list` | — | List trusted keys. |
| `trust remove` | `<public-key>` | Remove a trusted key. |
| `trust verify` | `<manifest.json>` | Validate a key-rotation chain. |

```bash
conductor-sign generate-key ~/.conductor/my-key
conductor-sign sign plugin.wasm ~/.conductor/my-key --name "Jane Doe" --email jane@example.com
conductor-sign verify plugin.wasm
conductor-sign trust add abcd1234... "Official Conductor"
```

See [Plugin Security](../development/plugin-security.md) for the trust model
these commands implement.

---

## `conductor-skills` — Agent Skills management

```bash
conductor-skills <COMMAND> [ARGS]
```

| Command | Args | Description |
|---------|------|--------------|
| `validate` | `<PATH>` `[-v, --verbose]` | Validate a single skill directory (if `<PATH>/SKILL.md` exists) or every skill subdirectory under `<PATH>`. Exits non-zero on any failure. |
| `list` | `[-p, --path <DIR>]` `[-v, --verbose]` | List installed skills (default: `~/.conductor/skills`). |
| `install` | `<SOURCE>` `[-t, --target <DIR>]` | Validate, then copy a skill directory into the target skills dir (default: `~/.conductor/skills`). Fails if a skill of that name is already installed. |

```bash
conductor-skills validate ./skills/conductor-midi-mapping
conductor-skills validate ./skills --verbose
conductor-skills list
conductor-skills install ./my-skill
```

There is no `test` or `show` subcommand. See [Agent Skills
Development](../development/agent-skills.md) for the skill file format and
the five skills shipped with this repo.

---

## `conductor-state` — control-state inspection

```bash
conductor-state [-d, --device <ALIAS>] [-j, --json]
```

Read-only diagnostic that connects to a **running** daemon and dumps its
`PhysicalControlStateStore` snapshot (what the daemon currently believes
about every physical control's state), via the `conductor_get_control_state`
MCP tool. `--device` filters to one device alias; `--json` emits raw JSON.

```bash
conductor-state
conductor-state --device mikro --json
```

---

## Hardware diagnostic tools

These are standalone binaries with no subcommands. Four of the five
(`led_diagnostic`, `led_tester`, `pad_mapper`, and implicitly `midi_diagnostic`'s
auto-select) are hardcoded to look for a **Native Instruments Maschine Mikro
MK3** (HID vendor `0x17CC`, product `0x1700`) — they were built for that
device during early development and haven't been generalized. `test_midi`
and `midi_simulator` are device-agnostic.

### `midi_diagnostic`

```bash
midi_diagnostic [PORT]
```

Lists all MIDI input ports, then connects to `[PORT]` (an index into that
list) and prints every incoming MIDI event live (note on/off with a velocity
bar, CC, pitch bend, aftertouch, program change) until Ctrl+C. Without
`[PORT]`, it auto-selects the first port whose name contains "Mikro". A
present-but-non-numeric argument is rejected outright — it does not silently
fall back to auto-select.

```bash
midi_diagnostic 2
```

### `led_diagnostic`

```bash
led_diagnostic
```

No arguments. Opens the Mikro MK3's HID interface and MIDI port, then loops:
prompts you to press a pad (60s timeout), reports the captured MIDI
note/velocity, and asks you to confirm whether the pad's LED lit up — an
interactive correlator, not an automatic all-pads cycle.

### `led_tester`

```bash
led_tester
```

No arguments. An "LED address finder": captures a pad press, then walks a
series of candidate byte offsets into the Mikro MK3's HID LED-report buffer,
lighting each one red in turn and asking "did you see RED light? (y/n)" until
you confirm the right offset for that pad.

### `pad_mapper`

```bash
pad_mapper
```

No arguments. Opens the Mikro MK3's HID and MIDI interfaces and, as you press
pads, prints the MIDI note number captured for each — for filling in
`config.toml` note numbers.

### `test_midi`

```bash
test_midi
```

No arguments and no interactivity. Prints every MIDI input port, every MIDI
output port, and the OS/architecture, then exits. It does not open a
connection or wait for a test event.

### `midi_simulator`

```bash
midi_simulator
```

No arguments. An interactive REPL that fabricates MIDI events (note,
velocity, long-press, double-tap, chord, encoder, aftertouch, pitch bend, CC)
without any physical hardware — useful for testing mappings and the event
console. Type `help` at the prompt for the full command list, or `demo` to
run a scripted walkthrough.

---

## `conductor-capture` — input pattern recording (early development)

```bash
conductor-capture <COMMAND> [ARGS]
```

Part of an in-progress crowdsourced pattern-recording feature. The full
subcommand surface is defined, but **most subcommands are stubs that exit
non-zero** rather than doing anything — this is deliberate (the crate fails
loudly rather than pretending to succeed) rather than a bug to work around.

| Command | Args | Status |
|---------|------|--------|
| `start` | `--privacy <LEVEL>` `--protocol <midi\|gamepad\|both>` `--tag <TAG>`... `--description <TEXT>` | **Not implemented** — errors. |
| `stop` | `--name <NAME>` | **Not implemented** — errors. |
| `pause` | — | **Not implemented** — errors. |
| `resume` | — | **Not implemented** — errors. |
| `list` | `[--privacy <LEVEL>]` `[--tag <TAG>]` | Prints a header and "No captures found." (storage layer not wired up yet). |
| `info` | `<NAME>` | **Not implemented** — errors. |
| `delete` | `<NAME>` `[--force]` | **Not implemented** — errors. |
| `import` | `<FILE>` `[--privacy <LEVEL>]` | **Not implemented** — errors. |
| `export` | `<NAME>` `--output <FILE>` | **Not implemented** — errors. |
| `upload` | `<NAME>` `[--privacy <LEVEL>]` | **Not implemented** — errors. |
| `status` | — | Prints a header and "No active capture session." |

`--privacy` accepts `public`, `private`, `friends`, or `premium`. Only
`list` and `status` currently produce any real output; everything else is
scaffolding for a feature that hasn't landed yet.

---

## Quick reference

| Task | Command |
|------|---------|
| Start the daemon | `conductor` |
| Check daemon status | `conductorctl status` |
| Hot-reload config | `conductorctl reload` |
| Validate a config file | `conductorctl validate --config path.toml` |
| List MIDI/HID devices | `conductorctl list-devices` |
| Switch MIDI device | `conductorctl set-device 2` |
| Show resolved bindings | `conductorctl bindings` |
| Set active mode (locked) | `conductorctl mode set DJ` |
| Set active mode (no lock) | `conductorctl mode set DJ --no-lock` |
| Release mode lock | `conductorctl mode unlock` |
| Set LED scheme | `conductorctl led scheme rainbow` |
| Tail live events | `conductorctl events --follow` |
| Tail audit denials | `conductorctl audit denied --follow` |
| Roll back to known-good config | `conductorctl rollback-config` |
| Migrate legacy Raw+MidiForward mappings | `conductorctl migrate-config --routing` |
| Validate config against protocol schemas | `conductorctl validate-schema` |
| Switch profile | `conductorctl profile switch music` |
| Install as a login service (macOS) | `conductorctl install --install-binary && conductorctl enable` |
| Check macOS Input Monitoring grant | `conductorctl permissions --check` |
| Register an MCP client | `conductorctl mcp register --name X --exe-path /path --tier Stateful` |
| Validate a skill | `conductor-skills validate ./skills/my-skill` |
| Sign a plugin | `conductor-sign sign plugin.wasm ~/.conductor/my-key --name N --email E` |
| Dump live control state | `conductor-state` |
| View raw MIDI events | `midi_diagnostic 2` |
| List MIDI ports | `test_midi` |
| Simulate MIDI without hardware | `midi_simulator` |

## See Also

- [Configuration Schema Reference](config-schema.md) — the `config.toml` structure these commands read, migrate, and validate
- [MCP Tools Reference](mcp-tools.md) — the tools an MCP/LLM client calls through the daemon
- [Agent Skills Development](../development/agent-skills.md) — the skill format `conductor-skills` validates
- [Plugin Security](../development/plugin-security.md) — the signing model `conductor-sign` implements
