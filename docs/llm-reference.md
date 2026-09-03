# Conductor Reference (L1)

Token-budgeted reference for LLM system prompts. ADR-018 D2.

## Three-Layer Model

1. **Discovery** — OS-exposed ports (MIDI input/output, HID). Enumerated automatically. Use `conductor_list_discovered_ports`.
2. **Endpoints** — `[[endpoints]]` entries give stable aliases to I/O ports via matchers (ADR-035 — supersedes the removed `[[bindings]]`/`[[connectors]]` blocks). Survive reconnection/renumbering. Each declares `direction` = Input / Output / Bidirectional.
3. **Mapping Rules** — Triggers/actions reference endpoints by alias: `device = "pads"`.

## Signal Routing Graph (ADR-031)

Layered on top of the three-layer model when steady-state signal flow is needed (controller → DAW, keyboard split, OSC bridge, LED return path):

- **`[[endpoints]]`** — named I/O endpoints (alias, direction, protocol, type) — ADR-035, supersedes the removed `[[bindings]]`/`[[connectors]]` blocks. MCP: `conductor_get_routing_graph` (ReadOnly view), `conductor_create_endpoint` (the single authoring tool — required `alias` + `direction` + `type`). The legacy `conductor_create_connector` / `conductor_create_binding` tools and the `create_connector` / `update_connector` / `delete_connector` batch ops were removed in ADR-035 Phase 2; update/delete of an existing endpoint via MCP is a tracked follow-up (edit TOML or use the GUI endpoint editor in the meantime). DAW proxy: `type = "MidiVirtualPort"` creates a system-visible virtual port for DAWs to connect to.
- **`[[routes]]`** — signal paths between endpoints (`from`, `to`, optional `filter`, optional `transform`). **Mode-independent**, **fan-out by default** (multiple routes from same source). MCP: `conductor_list_routes` (ReadOnly), `conductor_get_routing_graph` (ReadOnly — combined `{connectors, routes, excluded}` view in one call so the LLM doesn't have to stitch the two list_* tools). Route mutation goes through `conductor_batch_changes` with `create_route` / `update_route` / `delete_route` operations (per spec § 5.4 — no singleton tools by design). The `excluded` runtime detail on routing-graph + list_routes responses is wired in a follow-up that plumbs RouteEngine onto `SharedDaemonStateRefs`.
- **Live runtime metrics** — per-connector activity counters (`total_messages`, `throughput_msgs_per_sec` over a 10-second sliding window, `last_activity_ago_ms`, `error_count`). MCP: `conductor_get_connector_metrics` (ReadOnly — reads the runtime `connector_registry`, NOT the static config). Distinct from `conductor_get_routing_graph` which is the config-derived view. Use this when the user asks "how busy is connector X?" / "is connector Y idle?" / "is connector Z dropping messages?" — the static config tool can't answer those.
- **Resolved routing graph** — daemon-canonical resolver verdict for the routing graph. MCP: `conductor_get_resolved_routing_graph` (ReadOnly — no arguments). Returns every connector with its `bound_port` (the physical port the resolver actually matched, or `null` if unbound) and every route with `from_missing` / `to_missing` booleans flagging unresolved endpoints. Distinct from `conductor_get_routing_graph` which returns the *static* `[[endpoints]]` / `[[routes]]` config view without resolver verdicts. Use this when the user asks "why isn't my route firing?" / "what port did connector X actually bind to?" / "is there a typo in a route's `from` or `to`?" — the static graph can't surface unbound connectors or dangling route endpoints. (Designed for ADR-031 P6 routing-graph rendering, and available to the LLM today.)
- **Route-match introspection** — explain why each route fires or is skipped for a hypothetical event (ADR-036 D5). MCP: `conductor_explain_route_match` (ReadOnly — args `event` { device, type, channel, data1, data2? } + `active_mode`). Evaluates the event against the live compiled RouteEngine across both phases and returns, per candidate route, `{ to_alias, phase, modes, fired, skip_reason }` where `skip_reason` is `mode_ineligible` / `filter_mismatch` (with the failing `dimension`) / `transform_produced_no_output`. The `fired` set equals what the event pump would dispatch. Use this when the user asks "why didn't my route fire?" without sending real MIDI.
- **Dispatch trace** — recent route-dispatch decisions from the daemon's in-memory ring buffer (ADR-036 §8). MCP: `conductor_get_dispatch_trace` (ReadOnly — optional `last`, 1-256, default 32). Each entry is one event that actually routed somewhere: `{ timestamp_ms, device_id, active_mode, event, destinations }`, newest last. (All routes are post-mapping since ADR-036 Phase 3, so there is no longer a `phase` field.) Holds up to `advanced_settings.trace_buffer_size` entries (default 1000, oldest evicted; valid range 1–1,000,000); cleared on daemon restart. Use this to answer "what did the router just do?" or to confirm a route fired after sending MIDI.
  - **stderr trace logging (ADR-037 D4)** — for a live tail without the MCP round-trip, start the daemon with `CONDUCTOR_TRACE_LOG=1` (alias `CONDUCTOR_TRACE=1`). Each routed event is then also written to stderr as a single structured-JSON line — the same `{ timestamp_ms, device_id, active_mode, event, destinations }` shape as the ring entry. Off by default with zero per-event overhead when unset; the gate is read once at first dispatch, so set the env var **before** launching the daemon. Pipe stderr through `jq` to filter, e.g. `CONDUCTOR_TRACE_LOG=1 conductor 2>&1 >/dev/null | jq 'select(.destinations[] == "absynth")'`.
- **Evaluation priority**: per-event mappings > routes. A more specific layer always shadows a broader one — keep a permanent route in place AND override one note in one mode (mode-scoped route, ADR-036) without breaking the steady-state flow.

For routing decisions and patterns (keyboard split, DAW proxy, cross-protocol bridge, mode-dependent override), see the `conductor-signal-routing` agent skill.

## Config Schema

```toml
# Top-level: endpoints, modes, mappings, advanced_settings, led
[[endpoints]]         # ADR-035 — unified I/O endpoint (supersedes [[bindings]]/[[connectors]])
alias = "pads"        # Stable name for triggers/actions
direction = "Bidirectional"  # Input | Output | Bidirectional (REQUIRED)
type = "Matcher"      # Matcher | OscEndpoint | ArtNetEndpoint | MidiVirtualPort
description = "..."   # Optional
enabled = true        # false = muted
input_matchers = [{ type = "NameContains", value = "Mikro" }]   # Receive port
output_matchers = [{ type = "NameContains", value = "Mikro" }]  # Send port (LEDs, MIDI out)
# Symmetric form: use `matchers = [...]` when input and output share one port.

[[modes]]
name = "Mix"
color = "blue"        # Optional badge color

[[modes.mappings]]
[modes.mappings.trigger]
type = "Note"
note = 60
channel = 9           # Optional 0-indexed (0-15); omit = any channel
device = "pads"       # Optional endpoint alias; omit = any device
[modes.mappings.action]
type = "Keystroke"
keys = "space"
modifiers = ["cmd"]

[[global_mappings]]    # Active in all modes

[[endpoints]]          # An output endpoint (e.g. a synth the router sends to)
alias = "absynth"      # Unique across all endpoints
direction = "Output"   # Input | Output | Bidirectional (REQUIRED)
type = "Matcher"       # Matcher | OscEndpoint | ArtNetEndpoint | MidiVirtualPort
protocol = "Midi"      # Optional: Midi | Osc | ArtNet | Hid (inferred from type)
description = "..."    # Optional
enabled = true         # Default true
channels = []          # Optional 0-indexed channel scope (0-15); empty = all
matchers = [{ type = "NameContains", value = "Absynth" }]
# OscEndpoint: type = "OscEndpoint", host = "127.0.0.1", port = 9000
# ArtNetEndpoint: type = "ArtNetEndpoint", universe = 1, host?, port?
# MidiVirtualPort: type = "MidiVirtualPort", port_name = "Conductor: Logic" (DAW proxy)

[[routes]]             # ADR-031 — signal paths between endpoints
from = "mikro"         # Source: endpoint alias
to = "absynth"         # Destination: endpoint alias
enabled = true         # Default true
[routes.filter]        # Optional. Filters AND-combine.
note_range = [60, 72]  # Optional [low, high] (inclusive)
cc_range = [0, 32]     # Optional [low, high] (inclusive)
channels = [0]         # Optional 0-indexed (0-15); empty/omitted = all
message_types = ["NoteOn", "NoteOff"]  # Optional subset
[routes.transform]     # Optional. Same shape as MidiForward.transform (Phase 2 wiring).
type = "Midi"          # Midi (channel/CC/note remap) — cross-protocol variants land in P5
# transform body matches MidiTransform: channel_remap, cc_remap, note_offset, etc.

[advanced_settings]
input_mode = "Both"    # MidiOnly | GamepadOnly | Both
listen_mode = "All"    # All | Configured
chord_timeout_ms = 50
double_tap_timeout_ms = 300
hold_threshold_ms = 2000
```

## Triggers

| Type | Key Params | Range/Notes |
|------|-----------|-------------|
| `Note` | `note`, `velocity_min?`, `channel?`, `device?` | note: 0-127 |
| `VelocityRange` | `note`, `soft_max?`, `medium_max?`, `channel?`, `device?` | Splits velocity into soft/medium/hard zones |
| `LongPress` | `note`, `duration_ms?`, `channel?`, `device?` | Default 2000ms |
| `DoubleTap` | `note`, `timeout_ms?`, `channel?`, `device?` | Default 300ms |
| `NoteChord` | `notes[]`, `timeout_ms?`, `channel?`, `device?` | 2+ notes within timeout |
| `EncoderTurn` | `cc`, `channel?`, `device?` | Relative CC encoder |
| `CC` | `cc`, `channel?`, `device?` | Absolute CC fader/knob |
| `Aftertouch` | `pressure_min?`, `channel?`, `device?` | Channel aftertouch pressure |
| `PitchBend` | `value_min?`, `value_max?`, `channel?`, `device?` | 14-bit pitch wheel |
| `GamepadButton` | `button`, `device?` | IDs 128-255 (common: 128-144) |
| `GamepadButtonChord` | `buttons[]`, `timeout_ms?`, `device?` | Multiple buttons |
| `GamepadAnalogStick` | `axis`, `direction?`, `device?` | IDs 128-131 |
| `GamepadTrigger` | `trigger`, `threshold?`, `device?` | IDs 132-133 |

**Gamepad button map:** 128=South(A), 129=East(B), 130=West(X), 131=North(Y), 132-135=DPad, 136-137=Bumpers, 138-139=Thumbsticks, 140=Start, 141=Select, 142=Guide, 143-144=Triggers

**Matcher types** (highest specificity first): `CoreMidiUniqueId` (70) > `SysExIdentity { manufacturer_id, family?, model? }` (65) > `UsbIdentifier { vendor_id, product_id }` (60) > `UsbTopology` (50) > `ExactName` (40) > `PlatformId` (30) > `NameContains` (20) > `NameRegex` (10). Channel scope: `channels = [9]` restricts to MIDI ch.10 (0-indexed). Protocol: `protocol = "midi" | "hid" | "osc" | "artnet"` (default: `"midi"`).

**Endpoint patterns** (set `direction` + the matching matcher list):
- **Receive-only** (controller): `direction = "Input"` + `input_matchers` (or `matchers`) → events received
- **Send-only** (synth/lights): `direction = "Output"` + `output_matchers` (or `matchers`) → MIDI output target
- **Bidirectional** (controller with LEDs): `direction = "Bidirectional"` + `input_matchers` + `output_matchers` (or one symmetric `matchers`)
- **Bridge** (MIDI router): an Input endpoint + a MidiForward action to an Output endpoint's alias

## Actions

| Type | Key Params | Notes |
|------|-----------|-------|
| `Keystroke` | `keys`, `modifiers[]?` | Platform keycodes; mods: cmd/ctrl/shift/alt/option |
| `Text` | `text` | Type string literally |
| `Launch` | `app` | Open application by name/path |
| `Shell` | `command`, `args?[]` | Execute shell command. Legacy form: `command` is whitespace-split at runtime. Argv form: `command` is argv[0], `args` is argv[1..] — no tokenisation, no shell interpretation. Use argv form for `/bin/sh -c "…"` / `python -c "…"` / any case where quoting matters. ADR-027 D3 §3.1. |
| `MouseClick` | `button?` | left/right/middle (default left) |
| `VolumeControl` | `operation` | Up/Down/Mute/Unmute/Set(level) |
| `ModeChange` | `mode` | Switch to named mode |
| `Delay` | `ms` | Wait N milliseconds |
| `Sequence` | `actions[]` | Run actions in order |
| `Repeat` | `action`, `count` | Repeat action N times |
| `Conditional` | `condition`, `then_action`, `else_action?` | If/then/else |
| `SendMidi` | `port`, `message_type`, `channel`, `note?`, `velocity?`, `controller?`, `value?`, `program?`, `pitch?`, `pressure?` | Output MIDI message |
| `MidiForward` | `target`, `transform?` | Forward + transform MIDI |
| `OscSend` | `host`, `port`, `address`, `args[]?` | UDP OSC message (host+port required) |
| `Plugin` | `plugin`, `params?` | Plugin action call |

**Condition types:** `Always`, `Never`, `TimeRange { start, end }`, `DayOfWeek { days[] }`, `AppRunning { app }`, `AppFrontmost { app }`, `ModeIs { mode }`, `And { conditions[] }`, `Or { conditions[] }`, `Not { condition }`

## Velocity Curves

| Type | Params | Behavior |
|------|--------|----------|
| `Fixed` | `value` (0-127) | Constant output |
| `PassThrough` | — | 1:1 input=output |
| `Linear` | `min`, `max` | Scale to range |
| `Curve` | `curve_type`, `strength?` | Non-linear mapping |

**Curve types:** `Exponential` (emphasize hard), `Logarithmic` (emphasize soft), `SCurve` (emphasize extremes)

## Profiles vs Modes

- **Profile** = top-level container. Each profile has its own config.toml file with separate bindings, modes, and mappings. Switching profiles loads a completely different configuration. Use `conductor_list_profiles` to discover available profiles (returns `{ profile_count, profiles: [{ id, name, config_path, is_default }], active_profile_id }`). Use `conductor_get_active_profile` to check which is active (returns `{ active_profile: { id, name, config_path } }`, or `{ active_profile: null }` when the default profile is active; the daemon owns this identity record and restores it across restarts). Use `conductor_switch_profile` with `{ profile_name, config_path }` to switch.
- **Mode** = mapping group within a profile. Multiple modes per profile (e.g., "Mix", "Edit", "Transport"). Switching modes changes which mappings are active without changing bindings or settings. Use `conductor_switch_mode` to change.
- **Config file** = the TOML backing a profile. Profile selection IS config selection.

**Hierarchy:** Profile → Modes → Mappings. The TitleBar dropdown selects the profile; workspace mode tabs select the mode within that profile.

## Modes

- **Switching:** `ModeChange` action switches active mode; daemon applies immediately
- **Default mode:** `default_mode = "Mix"` in config root sets startup mode
- **Global mappings:** `[[global_mappings]]` active in ALL modes (checked before mode mappings)
- **Mode colors:** Optional `color` field for UI badge display

## Common Patterns

```toml
# 1. Pad → keyboard shortcut
[[modes.mappings]]
trigger = { type = "Note", note = 36 }
action = { type = "Keystroke", keys = "space" }

# 2. Encoder → volume
[[modes.mappings]]
trigger = { type = "EncoderTurn", cc = 16 }
action = { type = "VolumeControl", operation = "Up" }

# 3. Long press → app launch
[[modes.mappings]]
trigger = { type = "LongPress", note = 40, duration_ms = 1000 }
action = { type = "Launch", app = "Safari" }

# 4. Double tap → mode switch
[[modes.mappings]]
trigger = { type = "DoubleTap", note = 44, timeout_ms = 300 }
action = { type = "ModeChange", mode = "Transport" }

# 5. Chord → complex action
[[modes.mappings]]
trigger = { type = "NoteChord", notes = [60, 64, 67] }
action = { type = "Sequence", actions = [
  { type = "Keystroke", keys = "c", modifiers = ["cmd"] },
  { type = "Delay", ms = 100 },
  { type = "Keystroke", keys = "v", modifiers = ["cmd"] },
]}

# 6. Velocity-sensitive (soft/hard)
[[modes.mappings]]
trigger = { type = "VelocityRange", note = 36, soft_max = 60, medium_max = 100 }
action = { type = "Keystroke", keys = "a" }

# 7. Conditional (app-aware)
[[modes.mappings]]
trigger = { type = "Note", note = 48 }
action = { type = "Conditional",
  condition = { type = "AppFrontmost", app = "Safari" },
  then_action = { type = "Keystroke", keys = "r", modifiers = ["cmd"] },
  else_action = { type = "Keystroke", keys = "o", modifiers = ["cmd"] } }

# 8. MIDI forward with transform
[[modes.mappings]]
trigger = { type = "CC", cc = 1, device = "faders" }
action = { type = "MidiForward", target = "synth",
  transform = { cc = 7 } }

# 9. Gamepad button → keystroke
[[modes.mappings]]
trigger = { type = "GamepadButton", button = 128 }
action = { type = "Keystroke", keys = "space" }

# 10. Channel-filtered trigger
[[modes.mappings]]
trigger = { type = "Note", note = 36, channel = 9, device = "drums" }
action = { type = "Shell", command = "play kick.wav" }

# 11. Shell argv form (ADR-027 D3 §3.1) — required whenever quoting matters
# or the command resolves to an interpreter (sh, bash, python, etc.).
# Each arg is passed verbatim as argv; no whitespace tokenisation, no
# shell interpretation. The validator's `allow_interpreters` policy
# (default: warn) surfaces a load-time diagnostic when the resolved
# binary is an interpreter family, even via wrappers like
# `/usr/bin/env python`. Switch the policy to `"deny"` to reject; to
# `"allow"` to silence the warning when you deliberately want shell
# scripting.
[[modes.mappings]]
trigger = { type = "Note", note = 60 }
action = { type = "Shell", command = "/bin/sh", args = ["-c", "afplay ~/sounds/ding.wav"] }
```

## Shell action: legacy vs argv form

Both shapes deserialise. Pick argv form whenever any of these apply:

- Your `command` value contains whitespace that should NOT be
  whitespace-split as separate argv tokens (e.g. a path with a space,
  or you want `argv[1]` to be a multi-word string).
- You're invoking an interpreter (`/bin/sh -c "…"`, `python -c "…"`,
  `osascript -e "…"`) — the legacy form's whitespace splitter is too
  fragile around quoted scripts.
- You want the validator's metacharacter blocklist to apply to each
  argv token individually with clear `path = "…args[i]"` error
  attribution.

Argv form schema:

```toml
[mapping.action]
type    = "Shell"
command = "/bin/sh"             # argv[0] — the resolved binary
args    = ["-c", "echo hi"]     # argv[1..] — passed verbatim
```

Validator behaviour (`advanced_settings.allow_interpreters`):

| Setting | When the resolved binary is an interpreter (sh, bash, python, …) |
|---------|------------------------------------------------------------------|
| `"allow"` | Silent — explicit opt-in for users who want shell scripting |
| `"warn"` (default) | Warning at config load, config still loads |
| `"deny"` | Validation error, config rejected |

Wrapper-chain resolution: `env python -c "…"`, `sudo python …`,
`nice python …`, `nohup python …` all resolve to **python** (16
wrappers covered). The policy applies to the effective binary after
unwinding — `env python` is detected as a Python invocation, not a
benign `env` call.
