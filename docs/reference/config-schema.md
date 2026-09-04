# Configuration Schema Reference

Complete reference for Conductor's TOML configuration file format, verified against
`conductor-core/src/config/types.rs` and `conductor-core/src/config/validation.rs`.

> **ADR-035 note:** the legacy `[device]`, `[[devices]]`, `[[bindings]]`, and
> `[[connectors]]` blocks were removed with no migration path. All I/O — inputs,
> outputs, and bidirectional devices — is declared under a single `[[endpoints]]`
> array. If you have an old config using any of those blocks, it will fail to load;
> rewrite it using `[[endpoints]]` as shown below.

## Table of Contents

- [File Structure](#file-structure)
- [Modes](#modes)
- [Mappings](#mappings)
- [Trigger Types](#trigger-types)
- [Action Types](#action-types)
- [Advanced Settings](#advanced-settings-advanced_settings)
- [Logging](#logging-logging)
- [Endpoints](#endpoints-endpoints)
- [Routes](#routes-routes)
- [MCP Socket](#mcp-socket-mcp)
- [Security](#security-security)
- [Other Top-Level Blocks](#other-top-level-blocks)
- [Validation Rules](#validation-rules)
- [Complete Examples](#complete-examples)

## File Structure

A Conductor config file is a single TOML document. Every field below is a field
of the top-level `Config` struct; nothing else is recognized at the root.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `modes` | Array of tables (`[[modes]]`) | **Yes** | — | Mode definitions. No `#[serde(default)]` in the struct, so the key must be present — either one or more `[[modes]]` tables, or an explicit `modes = []`. An empty list is valid: the daemon then runs on `global_mappings` only. |
| `global_mappings` | Array of tables (`[[global_mappings]]`) | No | `[]` | Mappings active in every mode, checked before mode-specific mappings. |
| `logging` | Table (`[logging]`) | No | `None` | Log level and optional file path. |
| `advanced_settings` | Table (`[advanced_settings]`) | No | struct defaults | Timing thresholds, listen mode, rate limits, shell/interpreter policy, route-engine tuning. |
| `last_selected_mode` | String | No | `None` | Last mode selected at runtime; persisted across restarts. Takes priority over `default_mode` on startup. |
| `default_mode` | String | No | `None` | Startup mode name, used when `last_selected_mode` is absent or stale. |
| `led` | Table (`[led]`) | No | `None` | LED feedback configuration. See [LED System Reference](led-system.md). |
| `event_console` | Table (`[event_console]`) | No | struct defaults | Event monitoring buffer size, capture toggles, named filters, event-based triggers. |
| `per_app_modes` | Table (`[per_app_modes]`) | No | `None` | Mode auto-switching by frontmost app / window title (ADR-040). |
| `endpoints` | Array of tables (`[[endpoints]]`) | No | `[]` | Unified I/O endpoints — see [Endpoints](#endpoints-endpoints). |
| `routes` | Array of tables (`[[routes]]`) | No | `[]` | Signal routes between endpoints — see [Routes](#routes-routes). |
| `security` | Table (`[security]`) | No | struct defaults | Shell-action sandbox policy. Omitted entirely from a saved config when it holds only default values. |
| `config` | Table (`[config]`) | No | struct defaults | Config-source mode (`managed`/`file`) and the external-write policy for `user.toml` (ADR-034). Internal field name is `config_meta`; the TOML key is `config`. |
| `mcp` | Table (`[mcp]`) | No | `{ enabled = true }` | Toggles the daemon's read-only MCP Unix socket. |

Minimal skeleton:

```toml
[[endpoints]]
alias = "pads"
direction = "Input"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Mikro" }]

[advanced_settings]
chord_timeout_ms = 50

[[modes]]
name = "Default"

[[modes.mappings]]
[modes.mappings.trigger]
type = "Note"
note = 60
[modes.mappings.action]
type = "Keystroke"
keys = "space"

[[global_mappings]]
[global_mappings.trigger]
type = "EncoderTurn"
cc = 1
[global_mappings.action]
type = "VolumeControl"
operation = "Up"
```

## Modes

Modes group mappings that are active together and can be switched between at
runtime (e.g. via a `ModeChange` action).

```toml
[[modes]]
name = "Default"     # String, required, unique across all modes
color = "blue"        # String, optional — free-form badge color for UI display
                       # (no fixed enum in the schema; any string is accepted)
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | String | Yes | Unique mode name. Empty string and duplicate names are both hard config-load errors. |
| `color` | String | No | Optional color hint for UI badges. Not validated against a fixed palette. |
| `mappings` | Array of tables (`[[modes.mappings]]`) | No | Mappings active only while this mode is selected. Defaults to `[]`. |

## Mappings

A mapping connects one `trigger` to one `action`. Mappings appear either nested
inside a mode (`[[modes.mappings]]`) or at the top level as `[[global_mappings]]`
(active in every mode, evaluated before the active mode's own mappings).

```toml
[[modes.mappings]]                 # or [[global_mappings]]
description = "Spotlight search"   # Optional, human-readable
[modes.mappings.trigger]
type = "Note"
note = 60
device = "pads"                    # Optional: matches an [[endpoints]] alias
[modes.mappings.action]
type = "Keystroke"
keys = "space"
modifiers = ["cmd"]
```

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `trigger` | Table | Yes | — | See [Trigger Types](trigger-types.md). |
| `action` | Table | Yes | — | See [Action Types](action-types.md). |
| `description` | String | No | `None` | Human-readable label. |
| `let_through` | Boolean | No | `false` | ADR-038: when `true`, fire the action **and** still let the event continue to the route stage (`[[routes]]`), instead of being swallowed once a mapping matches it. Does not affect first-match-wins among mappings — only the winning mapping's post-match route disposition. |

Two mappings with a **structurally identical trigger** in the same scope (the
same mode, or `global_mappings`) are a hard config-load error — the second one
could never fire (first-match-wins).

## Trigger Types

Full field-by-field trigger documentation (including the gamepad ID ranges and
matcher-specificity notes) lives in [Trigger Types Reference](trigger-types.md).
In brief: MIDI triggers (`Note`, `VelocityRange`, `LongPress`, `DoubleTap`,
`NoteChord`, `EncoderTurn`, `CC`, `Aftertouch`, `PolyAftertouch`, `PitchBend`,
`ProgramChange`) use the 0-127 MIDI ID space; gamepad triggers (`GamepadButton`,
`GamepadButtonChord`, `GamepadAnalogStick`, `GamepadTrigger`) use IDs 128 and up.
Every trigger type accepts an optional `channel` (0-15, MIDI triggers only) and
an optional `device` filter — `device` matches the `alias` of an `[[endpoints]]`
entry, not a legacy `[[devices]]` binding.

## Action Types

Full field-by-field action documentation lives in
[Action Types Reference](action-types.md). Common actions: `Keystroke`, `Text`,
`Launch`, `Shell`, `MouseClick`, `VolumeControl`, `ModeChange`, `Delay`,
`Sequence`, `Repeat`, `Conditional`, `PcContextSwitch`, `CcContextSwitch`,
`SendMidi`, `MidiForward`, `HidForward`, `OscSend`. `MidiForward`/`SendMidi`
targets should reference an `[[endpoints]]` alias (a raw port name still works
but loses hot-plug status tracking).

```toml
[action]
type = "Keystroke"
keys = "c"
modifiers = ["cmd"]
```

## Advanced Settings (`[advanced_settings]`)

Timing thresholds, listen behavior, rate limits, and security/routing tuning
knobs. Every field has a default, so the whole section may be omitted.

```toml
[advanced_settings]
chord_timeout_ms = 50
chord_learn_timeout_ms = 150
double_tap_timeout_ms = 300
hold_threshold_ms = 2000
short_press_ms = 200
listen_mode = "All"          # "All" | "Configured"
ignore_ports = []
max_midi_ports = 32
max_events_per_sec = 10000
input_mode = "Both"          # "MidiOnly" | "GamepadOnly" | "Both"
stick_deadzone = 0.1
trigger_deadzone = 0.1
sysex_identity_probing = true
probe_on_connect = true
allow_interpreters = "warn"  # "allow" | "warn" | "deny"
allow_cascade = false
cascade_ttl_ms = 100
max_route_depth = 8
trace_buffer_size = 1000
window_title_poll_ms = 500
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `chord_timeout_ms` | Integer | 50 | Time window for chord (`NoteChord`) detection. |
| `chord_learn_timeout_ms` | Integer | 150 | Wider chord window used only while MIDI Learn is active. |
| `double_tap_timeout_ms` | Integer | 300 | Max gap between taps for `DoubleTap`. |
| `hold_threshold_ms` | Integer | 2000 | Minimum hold duration for `LongPress` / the "long press" event class. |
| `short_press_ms` | Integer | 200 | Short→medium press classification boundary. |
| `listen_mode` | String | `"All"` | `"All"` opens every available MIDI port; `"Configured"` listens only on ports matching an `[[endpoints]]` entry. (There is no `"Single"` variant.) |
| `ignore_ports` | Array of strings | `[]` | Port names excluded from listening. |
| `max_midi_ports` | Integer | 32 | Maximum simultaneous open MIDI ports. |
| `max_events_per_sec` | Integer | 10000 | Default per-device event rate limit; excess events are dropped with a warning. |
| `input_mode` | String | `"Both"` | `"MidiOnly"`, `"GamepadOnly"`, or `"Both"`. |
| `stick_deadzone` | Float | 0.1 | Analog-stick dead zone, as a 0.0-1.0 fraction. |
| `trigger_deadzone` | Float | 0.1 | Analog-trigger dead zone, as a 0.0-1.0 fraction. |
| `sysex_identity_probing` | Boolean | `true` | Global on/off switch for SysEx Universal Device Identity probing. |
| `probe_on_connect` | Boolean | `true` | Auto-probe each newly-bound MIDI port on connect. Ignored (probing stays off) when `sysex_identity_probing = false`. |
| `allow_interpreters` | String | `"warn"` | Policy for `Shell` actions whose resolved binary is a known interpreter (sh, bash, python, ruby, perl, node, awk, lua, tclsh, php), including via `env`/`sudo`/`nice`/`nohup` wrappers. `"allow"` is silent, `"warn"` logs at load, `"deny"` rejects the config. |
| `allow_cascade` | Boolean | `false` | When `false`, MIDI input on a port is suppressed for `cascade_ttl_ms` after a `SendMidi`/`MidiForward` action writes to that port (broader than the per-message echo guard). Set `true` to allow deliberate MIDI-routed cascades. |
| `cascade_ttl_ms` | Integer | 100 | TTL for the cascade suppression window. Runtime-clamped to at most 60000. |
| `max_route_depth` | Integer | 8 | Maximum route-dispatch chain depth before the re-entrancy guard drops output (catches multi-hop route cycles). |
| `trace_buffer_size` | Integer | 1000 | Capacity of the in-memory dispatch-trace ring buffer. **Validated**: must be in `[1, 1_000_000]`; `0` and values above the max are hard config-load errors. |
| `window_title_poll_ms` | Integer | 500 | Poll interval for window-title detection, used only when `[per_app_modes].window_rules` are present. Runtime-clamped to a floor of 100ms. |

## Logging (`[logging]`)

```toml
[logging]
level = "info"   # "off" | "error" | "warn" | "info" | "debug" | "trace"
file = "/path/to/conductor.log"   # Optional; omit to log to stdout/stderr only
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `level` | String | `"info"` | Log verbosity. |
| `file` | String | `None` | Optional file path for log output. |

## Endpoints (`[[endpoints]]`)

`[[endpoints]]` is the single way to declare I/O in a Conductor config (ADR-035).
Each entry gives a stable `alias` to a physical or virtual I/O port; triggers and
actions reference that alias (`device = "pads"`, `target = "synth"`) instead of a
raw, renumbering-prone port name.

**Deserialization is strict**: an unknown or misspelled field on an
`[[endpoints]]` entry is a hard config-load error, not a silently-ignored key.

### Common fields

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `alias` | String | Yes | — | Unique across all endpoints. Duplicate aliases are a hard config-load error. |
| `direction` | String | **Yes** | — | `"Input"`, `"Output"`, or `"Bidirectional"`. No default — must be stated explicitly. |
| `type` | String | Yes | — | `"Matcher"`, `"OscEndpoint"`, `"ArtNetEndpoint"`, or `"MidiVirtualPort"`. Selects the fields below. |
| `protocol` | String | No | inferred from `type` | Override: `"Midi"`, `"Osc"`, `"ArtNet"`, or `"Hid"`. `OscEndpoint`→Osc, `ArtNetEndpoint`→ArtNet, `Matcher`/`MidiVirtualPort`→Midi by inference; set explicitly for a HID `Matcher`. |
| `description` | String | No | `None` | Human-readable note. |
| `enabled` | Boolean | No | `true` | `false` mutes the endpoint. |
| `channels` | Array of integers | No | `[]` | MIDI channel scope (0-15, 0-indexed). Empty = all channels. Only meaningful for `protocol = "Midi"`; the validator warns if set on a non-MIDI endpoint. Values above 15 are a hard error. |

### `type = "Matcher"` (MIDI / HID ports)

```toml
[[endpoints]]
alias = "pads"
direction = "Bidirectional"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Mikro" }]
# Or asymmetric input/output ports on a bidirectional device:
# input_matchers = [{ type = "NameContains", value = "Mikro In" }]
# output_matchers = [{ type = "NameContains", value = "Mikro Out" }]
no_probe = false
```

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `matchers` | Array of matcher tables | conditionally | `[]` | Symmetric matchers, used for both directions (or the sole direction). |
| `input_matchers` | Array of matcher tables | conditionally | `[]` | Overrides `matchers` for the input side of a `Bidirectional` endpoint whose input port differs from its output. |
| `output_matchers` | Array of matcher tables | conditionally | `[]` | Overrides `matchers` for the output side. |
| `no_probe` | Boolean | No | `false` | Skip SysEx identity probing for this endpoint. Silently overridden (probing still happens) if the endpoint also declares a `SysExIdentity` matcher, since that matcher can never resolve without a probe. |

At least one of `matchers` / `input_matchers` / `output_matchers` must be
non-empty, or the config fails to load. Setting `output_matchers` on a
`direction = "Input"` endpoint (or `input_matchers` on `direction = "Output"`)
is also a hard error — those fields would silently never be consulted.

**Matcher types** (ordered by specificity, highest first):

| Type | Fields | Description |
|------|--------|-------------|
| `CoreMidiUniqueId` | `value` (integer) | macOS CoreMIDI unique device ID |
| `SysExIdentity` | `manufacturer_id` (bytes), `family?`, `model?` | SysEx Universal Device Identity Reply; requires probing |
| `UsbIdentifier` | `vendor_id`, `product_id` (integers) | USB vendor/product ID pair |
| `UsbTopology` | `value` (string) | USB topology path, e.g. `"1-2.3"` |
| `ExactName` | `value` (string) | Exact port name match |
| `PlatformId` | `value` (string) | Platform-specific device ID |
| `NameContains` | `value` (string) | Substring match on port name |
| `NameRegex` | `value` (string) | Regex match (max 256 chars) |
| `ControllerGuid` | `value` (32-hex-char string) | Gamepad model identity (SDL GUID) |

### `type = "OscEndpoint"`

```toml
[[endpoints]]
alias = "osc_lights"
direction = "Output"
type = "OscEndpoint"
host = "127.0.0.1"
port = 9000
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `host` | String | Yes | Target/listen host. |
| `port` | Integer | Yes | UDP port. |

An `Input`/`Bidirectional` OSC endpoint (a *listener*) also accepts the shared
network-security fields below. A loopback `host` (`127.0.0.1`, `::1`, or
`localhost`) always loads. A non-loopback `host` requires `allow_network = true`
plus a concrete IP literal (DNS names are never resolved) and a non-empty
`network_acl`; the actual bind is then additionally gated at runtime behind an
HMAC-verified approval (ADR-042). A non-loopback `host` without `allow_network`
is a config-load error:

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `allow_network` | Boolean | `false` | Opt-in to bind a non-loopback listener host (with a required `network_acl`). The bind is still gated at runtime by an HMAC-verified approval. |
| `network_acl` | Array of CIDR strings | `[]` | Allow-listed source ranges. `0.0.0.0/0` / `::/0` are rejected. |
| `sender_acl` | Array of IP strings | `[]` | Narrower allow-list of individual sender IPs. |
| `rate_limit_total` | Integer | `None` | Total inbound packet budget. |
| `rate_limit_per_sender` | Integer | `None` | Per-sender inbound packet budget. |
| `i_understand_amplification_risk` | Boolean | `false` | Required acknowledgement for a broad broadcast ACL. |
| `allow_sensitive_actions` | Boolean | `false` | When `false`, events from this listener cannot dispatch `Shell`/`Launch`/`Keystroke` actions — applies even to loopback listeners. |
| `strict_mode` | Table | `None` | `{ type = "SessionToken", arg_index, window_sec = 30, replay_window = 1000 }` — replay-defence nonce checking (forward-compat). |

### `type = "ArtNetEndpoint"`

```toml
[[endpoints]]
alias = "dmx"
direction = "Output"
type = "ArtNetEndpoint"
universe = 1
# host and port both optional — default to Art-Net broadcast
```

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `universe` | Integer | Yes | — | Art-Net universe number. |
| `host` | String | No | `"255.255.255.255"` | Target/listen host. |
| `port` | Integer | No | `6454` | UDP port. |
| `allow_broadcast` | Boolean | No | `false` | Enables Art-Net broadcast; when set on a listener, the ACL amplification budget is enforced. |

Also accepts the same network-security fields as `OscEndpoint` (listed above).

### `type = "MidiVirtualPort"`

Creates a system-visible virtual MIDI port (CoreMIDI on macOS, ALSA on Linux)
that DAWs and other apps can connect to — the "DAW proxy" pattern. Virtual
ports are created lazily, only when referenced by an enabled route.

```toml
[[endpoints]]
alias = "daw_proxy"
direction = "Bidirectional"
type = "MidiVirtualPort"
port_name = "Conductor: Logic"
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `port_name` | String | Yes | Name of the virtual port as it appears to the OS and other apps. |

`direction = "Input"` on a `MidiVirtualPort` is legal but triggers a warning — a
port Conductor itself creates is inherently an output/bidirectional concept.

## Routes (`[[routes]]`)

Routes are mode-independent (by default), fan-out-by-default signal paths
between endpoints, evaluated below the mapping engine — an event flows through
any matching route only if no mapping already claimed it (or if the claiming
mapping sets `let_through = true`). See ADR-031 and ADR-036.

```toml
[[routes]]
from = "pads"          # Source: an [[endpoints]] alias
to = "synth"            # Destination: an [[endpoints]] alias
enabled = true          # Default true
description = "..."     # Optional
modes = ["Mix"]         # Optional; empty/omitted = active in all modes

[routes.filter]         # Optional. All populated criteria AND-combine.
message_types = ["NoteOn", "NoteOff"]
channels = [0]
cc_range = [0, 32]
note_range = [60, 72]

[routes.transform]      # Optional for same-protocol routes; REQUIRED for
type = "Midi"            # most cross-protocol pairs (see below).
channel = 5
```

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `from` | String | Yes | — | Source endpoint alias. Must reference a declared `[[endpoints]]` entry. |
| `to` | String | Yes | — | Destination endpoint alias. Must reference a declared `[[endpoints]]` entry. |
| `filter` | Table | No | `None` | See below. |
| `transform` | Table | No | `None` | See below. |
| `enabled` | Boolean | No | `true` | |
| `description` | String | No | `None` | |
| `modes` | Array of strings | No | `[]` | Mode-name scope (ADR-036). Empty = fires in every mode. Each name must match a declared `[[modes]]` block. |

### `[routes.filter]`

Only signals matching **all** populated fields pass through. Absent/empty
fields are unconstrained.

| Field | Type | Description |
|-------|------|-------------|
| `message_types` | Array of strings | Subset of `NoteOn`, `NoteOff`, `CC`, `ProgramChange`, `Aftertouch`, `PitchBend`, `PolyAftertouch`. `ChannelPressure` and `SysEx` are rejected — the input pipeline doesn't emit them yet. |
| `channels` | Array of integers | 0-indexed (0-15). Values above 15 are a hard error. |
| `cc_range` | `[min, max]` | Inclusive. `min > max` is a hard error (would silently match nothing). |
| `note_range` | `[min, max]` | Inclusive. Same `min > max` rejection. |
| `osc_address_prefix` | String | OSC address prefix match. |

A route whose source protocol is HID or OSC must currently be catch-all — any
active filter on a HID- or OSC-sourced route is a hard config-load error (a
gamepad event serializes to MIDI bytes lossily, and OSC carries no MIDI bytes
at all, so a byte-level filter is meaningless there).

### `[routes.transform]`

Tagged by `type`. Whether one is required depends on the `(from, to)` protocol
pair: same-protocol routes accept an optional transform (or none); most
cross-protocol pairs *require* the matching variant below, and a few
cross-protocol pairs (e.g. HID→OSC, Art-Net→MIDI) have no defined variant and
are rejected outright.

| `type` | Direction | Fields |
|--------|-----------|--------|
| `Midi` | MIDI→MIDI | Same shape as `MidiForward`'s transform: `channel?`, `cc?`, `note?`, `velocity_scale?`, `velocity_offset?`, `invert_value?`, `curve?` (`"Linear"` \| `"Logarithmic"` \| `"Exponential"` \| `{ Lut = [...128 entries...] }`). |
| `MidiToOsc` | MIDI→OSC | `cc_to_address?`, `note_to_address?`, `value_to_float` (default `false`) |
| `OscToMidi` | OSC→MIDI | `address_to_cc?`, `address_to_note?`, `channel?` |
| `MidiToArtNet` | MIDI→Art-Net | `cc_to_dmx` (map of CC number → DMX channel, required), `note_to_dmx?` (same shape) |
| `HidToArtNet` | HID→Art-Net | `trigger_to_channel` (map of gamepad trigger name → DMX channel) |
| `OscToArtNet` | OSC→Art-Net | `address_to_dmx` (address template with one `{dmx}` placeholder, e.g. `"/dmx/{dmx}"`) |
| `HidToMidi` | HID→MIDI | `trigger_to_cc` (map of gamepad trigger name → CC number), `channel` (default `0`) |
| `HidToOsc` | HID→OSC | `trigger_to_address` (map of gamepad trigger name → OSC address), `value_to_float` (default `false`) |

Other route-level validation: `from == to` (self-loop) is rejected; a direct
`A→B` + `B→A` pair of routes is rejected as a guaranteed feedback cycle
(multi-hop cycles, e.g. `A→B→C→A`, are only caught at runtime by
`max_route_depth`); an exact duplicate route (same `from`/`to`/`filter`/
`transform`/mode-scope-overlap) is a warning.

## MCP Socket (`[mcp]`)

```toml
[mcp]
enabled = true   # Default true
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | Boolean | `true` | Binds the daemon's read-only MCP Unix socket at startup. Setting `false` leaves the socket entirely unbound (not merely refused). Takes effect on daemon restart. |

## Security (`[security]`)

```toml
[security.shell]
allow_unsandboxed = true   # Default true
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `security.shell.allow_unsandboxed` | Boolean | `true` | On platforms without an OS sandbox (Windows; Linux kernels < 5.13 without Landlock), `true` allows `Shell` actions to run unsandboxed with a spawn-time warning; `false` fails closed and refuses to spawn a `Shell` action it cannot sandbox. |

A `[security]` block that only holds default values is omitted from a
canonically-saved config, so its absence in a file you're reading does not
imply anything unusual.

Individual `Shell` actions can widen the sandbox further with a per-action
`sandbox` table (`fs_write`, `network`) — see
[Action Types Reference](action-types.md).

## Other Top-Level Blocks

These `Config` fields exist and are documented elsewhere or are narrow enough
not to need a full section here:

- **`[led]`** — LED feedback (schemes, brightness, velocity-color mapping,
  HID/MIDI LED protocols). See [LED System Reference](led-system.md).
- **`[event_console]`** — event-monitoring buffer size, capture toggles
  (`capture_midi`, `capture_processed`, `capture_actions`), named filters, and
  simple event-count/error-rate triggers (`condition`, `action` = `Log` or
  `Notification`).
- **`[per_app_modes]`** — mode auto-switching by frontmost app name or window
  title (ADR-040): `default` mode name, `rules` (app name → mode), and
  `window_rules` (`{ app, title_pattern? | title_regex?, mode }`, higher
  specificity than `rules`).
- **`[config]`** — `source` (`"managed"` default, or `"file"` for the
  deprecated auto-reload-on-external-edit behavior) and `user_file_policy`
  (`"notify"` default, or `"ignore"`) for how the daemon reacts to external
  edits of `user.toml` while running (ADR-034).

## Validation Rules

Conductor validates configs at load time. Selected hard errors (config refuses
to load) and warnings (config loads, diagnostic surfaced):

**Hard errors**

- Duplicate `[[modes]]` names, or an empty mode name.
- Two mappings in the same scope (mode, or `global_mappings`) with a
  structurally identical trigger.
- Duplicate `[[endpoints]]` aliases.
- An `[[endpoints]]` entry with an unknown/misspelled field.
- A `Matcher` endpoint with no matchers set across `matchers` /
  `input_matchers` / `output_matchers`.
- `output_matchers` set with `direction = "Input"`, or `input_matchers` set
  with `direction = "Output"`.
- A HID endpoint with `direction` other than `"Input"` (HID output was
  dropped).
- An endpoint `channels` value above 15.
- A `[[routes]]` `from`/`to` that doesn't match a declared endpoint alias, or
  that names the same endpoint on both sides (self-loop).
- A direct `A→B` + `B→A` route pair.
- A cross-protocol route missing its required `transform` variant, or
  declaring the wrong one.
- An active filter on a HID- or OSC-sourced route.
- `routes[].filter.cc_range` / `note_range` with `min > max`.
- `routes[].filter.channels` values above 15.
- `routes[].filter.message_types` containing `ChannelPressure` or `SysEx`.
- A `routes[].modes` entry that doesn't match a declared mode.
- `advanced_settings.trace_buffer_size` of `0` or above 1,000,000.
- A `ModeChange` action referencing a mode name that doesn't exist.

**Warnings** (config still loads)

- A trigger's `device` filter referencing an alias with no matching
  `[[endpoints]]` entry — the mapping can never fire until the alias is added
  or the filter removed.
- `MidiForward`/`SendMidi` targeting a raw port name instead of an
  `[[endpoints]]` alias (still forwards; loses hot-plug status tracking).
- `channels` set on a non-MIDI endpoint.
- `direction = "Input"` on a `MidiVirtualPort`.
- An exact-duplicate route.
- `allow_interpreters = "warn"` (the default) firing when a `Shell` action
  resolves to a known interpreter binary.

## Complete Examples

### Minimal: single MIDI controller

```toml
[[endpoints]]
alias = "pads"
direction = "Input"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Mikro" }]

[[modes]]
name = "Default"
color = "blue"

[[modes.mappings]]
description = "Pad 1: Play/Pause"
[modes.mappings.trigger]
type = "Note"
note = 36
device = "pads"
[modes.mappings.action]
type = "Keystroke"
keys = "PlayPause"

[[global_mappings]]
description = "Encoder: Volume Up"
[global_mappings.trigger]
type = "EncoderTurn"
cc = 2
direction = "Clockwise"
device = "pads"
[global_mappings.action]
type = "VolumeControl"
operation = "Up"
```

### Controller + hardware synth output + DAW virtual port, with routing

```toml
[[endpoints]]
alias = "pads"
direction = "Input"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Mikro" }]

[[endpoints]]
alias = "synth"
direction = "Output"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Absynth" }]

[[endpoints]]
alias = "daw_proxy"
direction = "Bidirectional"
type = "MidiVirtualPort"
port_name = "Conductor: Logic"

[advanced_settings]
chord_timeout_ms = 50
hold_threshold_ms = 2000
listen_mode = "All"

[logging]
level = "info"

[[modes]]
name = "Default"
color = "blue"

[[modes.mappings]]
description = "Pad 1: Play/Pause"
[modes.mappings.trigger]
type = "Note"
note = 36
device = "pads"
[modes.mappings.action]
type = "Keystroke"
keys = "PlayPause"

# All pad note-on/note-off events also flow straight to the synth, unless a
# mapping above matched first (or a matching mapping set let_through = true).
[[routes]]
from = "pads"
to = "synth"
description = "Pad input to hardware synth"
[routes.filter]
message_types = ["NoteOn", "NoteOff"]

# Mirror pad input into a virtual DAW port at the same time.
[[routes]]
from = "pads"
to = "daw_proxy"
description = "Mirror pad input into the DAW"
```

## See Also

- [Trigger Types Reference](trigger-types.md) — Full trigger field documentation
- [Action Types Reference](action-types.md) — Full action field documentation
- [LED System Reference](led-system.md) — `[led]` block and LED behavior
- [MCP Tools Reference](mcp-tools.md) — MCP tools exposed over the `[mcp]` socket
- [CLI Commands](cli-commands.md) — Command-line tools and debugging
- `docs/llm-reference.md` — Token-budgeted schema summary used in LLM system prompts
