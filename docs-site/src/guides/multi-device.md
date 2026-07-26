# Multi-Device Setup

Conductor supports connecting and mapping multiple MIDI devices simultaneously (v4.19.0+, ADR-009). Each device can have independent mappings, and events are routed to the correct device-specific rules automatically.

## Defining Devices

Use `[[bindings]]` sections in your `config.toml` to define device identities:

```toml
[[bindings]]
alias = "pads"
matchers = [{ type = "NameContains", value = "Mikro" }]

[[bindings]]
alias = "keys"
matchers = [{ type = "NameContains", value = "Launchpad" }]
```

Each binding needs:
- **alias**: A short name used in trigger mappings (e.g., `"pads"`, `"keys"`)
- **matchers**: One or more rules for matching physical MIDI ports

> **Legacy key:** `[[devices]]` is still accepted as a backward-compatible
> alias for `[[bindings]]` (serde `alias = "devices"`). New configs should
> use `[[bindings]]`; `conductorctl migrate-config` emits the canonical key.

## Matcher Types

| Matcher | Description | Example |
|---------|-------------|---------|
| `ExactName` | Exact port name match | `{ type = "ExactName", value = "Maschine Mikro MK3" }` |
| `NameContains` | Substring match | `{ type = "NameContains", value = "Mikro" }` |
| `NameRegex` | Regex pattern match (patterns > 256 chars rejected) | `{ type = "NameRegex", value = "Mikro.*MK[23]" }` |
| `UsbIdentifier` | USB vendor/product ID pair | `{ type = "UsbIdentifier", vendor_id = 0x17CC, product_id = 0x1600 }` |
| `UsbTopology` | USB topology path | `{ type = "UsbTopology", value = "1-2.3" }` |
| `PlatformId` | Platform-specific device ID string | `{ type = "PlatformId", value = "..." }` |
| `CoreMidiUniqueId` | macOS CoreMIDI unique ID | `{ type = "CoreMidiUniqueId", value = 12345 }` |
| `SysExIdentity` | SysEx Identity Reply data (requires probing the device; ADR-022 D6) | `{ type = "SysExIdentity", manufacturer_id = [0x00, 0x21, 0x09] }` |

Matchers have a **specificity ordering** for priority (least → most specific):
`NameRegex` (10) < `NameContains` (20) < `PlatformId` (30) < `ExactName` (40) <
`UsbTopology` (50) < `UsbIdentifier` (60) < `SysExIdentity` (65) <
`CoreMidiUniqueId` (70). When multiple identities could match a port, the most
specific matcher wins.

## Avoiding ambiguous matchers

A single `NameContains` matcher can silently match **multiple physical ports**.
For example, `{ type = "NameContains", value = "TouchOSC" }` matches **both**
`TouchOSC` and `TouchOSC Bridge` — the substring is in both port names.

When one binding matches several ports, only the first port enumerated wins
(claims the alias via first-come-first-served, ADR-009 D7). The other matching
ports are marked **ambiguous** and **never opened** — their events never reach
the engine. The daemon `warn!`s, and the GUI surfaces an **error toast**
("Ambiguous port: … — alias … already bound"), but the dropped port is
otherwise invisible.

**The ambiguous config** — one binding whose `NameContains` matches two ports:

```toml
[[bindings]]
alias = "touchosc"
matchers = [{ type = "NameContains", value = "TouchOSC" }]
```

**Fix:** use a more specific matcher so each binding claims exactly one port.
When you need both ports, declare **two bindings** with **distinct aliases**,
each with its own `ExactName` matcher:

```toml
[[bindings]]
alias = "touchosc"
matchers = [{ type = "ExactName", value = "TouchOSC" }]

[[bindings]]
alias = "touchosc_bridge"
matchers = [{ type = "ExactName", value = "TouchOSC Bridge" }]
```

If you prefer patterns, an **anchored `NameRegex`** disambiguates just as well —
`{ type = "NameRegex", value = "^TouchOSC$" }` matches `TouchOSC` but not
`TouchOSC Bridge`. (Each `[[bindings]]` `alias` must still be unique — the
config validator rejects duplicates.)

## Device-Specific Mappings

Add a `device` field to any trigger to restrict it to a specific device:

```toml
[[modes]]
name = "Default"

# Only triggers from the "pads" device
[[modes.mappings]]
trigger = { type = "Note", note = 36, device = "pads" }
action = { type = "Launch", app = "Finder" }

# Only triggers from the "keys" device
[[modes.mappings]]
trigger = { type = "Note", note = 36, device = "keys" }
action = { type = "Shell", command = "echo hello" }

# Triggers from any device (no device filter)
[[modes.mappings]]
trigger = { type = "Note", note = 60 }
action = { type = "Text", text = "any device" }
```

## Priority Order

When an event arrives, Conductor checks rules in this order:

1. **Device-specific rules** for the current mode (O(1) HashMap lookup)
2. **Any-device rules** for the current mode (linear scan)
3. **Global device-specific rules**
4. **Global any-device rules**

The first match wins.

## Hot-Plug Detection

Conductor automatically detects when devices are connected or disconnected (v4.22.0+). New ports are scanned every 5 seconds and matched against your `[[bindings]]` configuration.

## GUI Multi-Device Status

The GUI shows all connected devices with status indicators (v4.22.0+):
- **Green dot**: Active and receiving events
- **Yellow dot**: Muted (events ignored)
- **Red dot**: Disconnected

You can mute/unmute individual devices from the GUI.

## MCP Multi-Device Tools

Three MCP tools are available for LLM-assisted multi-device management (v4.23.0+):

- `conductor_list_device_bindings`: List all device bindings and their status
- `conductor_set_device_enabled`: Mute/unmute a specific device
- `conductor_scan_ports`: Trigger a port rescan for hot-plug detection

## Migrating from Legacy Config

If you have an existing `[device]` section, use the migration CLI:

```bash
# Preview what would change (dry-run)
conductorctl migrate-config

# Apply the migration (creates .bak backup)
conductorctl migrate-config --write
```

This converts:
```toml
[device]
name = "Mikro MK3"
auto_connect = true
```

Into:
```toml
[[bindings]]
alias = "mikro-mk3"
matchers = [{ type = "NameContains", value = "Mikro MK3" }]
```

`migrate-config` emits a `NameContains` matcher as a convenient starting
point — it's the closest automatic translation of a legacy `[device]`
`name`. If you run multiple devices whose port names share that substring,
tighten it to `ExactName` (or an anchored `NameRegex`) per
[Avoiding ambiguous matchers](#avoiding-ambiguous-matchers) so each binding
claims exactly one port.

## Config Validation

Conductor validates multi-device configs (v4.24.0+):
- Device aliases must be unique and non-empty
- Trigger `device` fields must reference a defined device alias
- Invalid references produce clear error messages during config load

## Complete Example

```toml
[[bindings]]
alias = "pads"
matchers = [{ type = "NameContains", value = "Mikro" }]

[[bindings]]
alias = "faders"
matchers = [{ type = "NameContains", value = "nanoKONTROL" }]

[[modes]]
name = "Production"

[[modes.mappings]]
trigger = { type = "Note", note = 36, device = "pads" }
action = { type = "Keystroke", keys = "space", modifiers = ["cmd"] }

[[modes.mappings]]
trigger = { type = "CC", cc = 0, device = "faders" }
action = { type = "VolumeControl", operation = "Set" }

[[modes]]
name = "Mixing"

[[modes.mappings]]
trigger = { type = "Note", note = 36, device = "pads" }
action = { type = "Keystroke", keys = "m" }

[[global_mappings]]
trigger = { type = "Note", note = 127 }
action = { type = "ModeChange", mode = "Production" }
```
