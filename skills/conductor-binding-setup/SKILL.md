---
name: conductor-binding-setup
description: >
  Help users declare and configure [[endpoints]] for MIDI controllers and game
  controllers (HID) with Conductor. Use when the user wants to connect a
  device, give a port a stable alias, troubleshoot connection issues, or
  rewrite legacy [device]/[[bindings]] config into the [[endpoints]] format.
license: MIT
compatibility: Requires Conductor daemon running
metadata:
  author: Monstrous Media
  version: "4.28.0"
  category: binding
allowed-tools: Bash(conductor:*) Read Write
---

# Endpoint Setup

Help users connect and configure input devices using Conductor's three-layer
model: **Discovery** (ports) → **Endpoints** (config aliases) → **Mapping Rules**.

## Three-Layer Model

1. **Discovery**: The OS exposes ports (MIDI input/output, HID). Conductor
   enumerates them automatically. Use `conductor_list_discovered_ports` to see
   all ports and their binding status.
2. **Endpoints**: An `[[endpoints]]` entry gives a stable alias to one or more
   ports via matchers (ADR-035 — the single way to declare I/O). Endpoints
   survive port renumbering and reconnection.
3. **Mapping Rules**: Triggers and actions reference endpoints by alias. A
   mapping rule like `device = "pads"` targets whichever port the "pads"
   endpoint resolves to.

## Scope & Non-Goals

**This skill covers:**
- Listing discovered ports and their binding status
- Declaring `[[endpoints]]` entries with matchers
- Configuring input mode (MidiOnly, GamepadOnly, Both) in `[advanced_settings]`
- Rewriting legacy `[device]` / `[[bindings]]` config as `[[endpoints]]`
- Endpoint health diagnosis
- Channel-scoped endpoints and triggers
- Direction configuration (`Input`, `Output`, `Bidirectional`)

**This skill does NOT cover:**
- Creating mappings (delegate to conductor-midi-mapping skill)
- Routes between endpoints and network/virtual endpoint types — OSC, Art-Net,
  `MidiVirtualPort` (delegate to conductor-signal-routing skill)
- Learn mode capture (delegate to conductor-learn skill)
- OS-level MIDI/HID driver installation

## IMPORTANT: Config Format

**`[[endpoints]]` is the only I/O declaration format.** The legacy `[device]`,
`[[devices]]`, `[[bindings]]`, and `[[connectors]]` blocks were **removed**
(ADR-035) — a config containing any of them is a hard load error, not a
deprecation warning. There is no automated migration for them
(`conductorctl migrate-config` only handles `--routing`); rewrite the legacy
block as an `[[endpoints]]` entry as shown below.

Deserialization is strict: an unknown or misspelled field on an
`[[endpoints]]` entry is also a hard config-load error.

## Discovering Ports

Use `conductor_list_discovered_ports` to see all ports across all protocols:

```
Agent: Let me check what ports are available.
       [Uses conductor_list_discovered_ports]

       I found these ports:

       **MIDI Receive Ports:**
       - Maschine Mikro MK3 MIDI (bound → "pads")
       - nanoKONTROL2 MIDI (unbound)

       **MIDI Send Ports:**
       - IAC Driver Bus 1 (unbound)

       **HID Devices:**
       - Xbox Wireless Controller (unbound)

       You have 1 bound port and 3 unbound ports.
```

## Declaring Endpoints

Each endpoint needs an `alias`, an explicit `direction` (no default), a
`type`, and — for `type = "Matcher"` — at least one matcher:

```toml
[[endpoints]]
alias = "pads"
direction = "Input"
type = "Matcher"
description = "Maschine Mikro MK3 pad controller"
matchers = [{ type = "NameContains", value = "Mikro" }]
```

For a HID device (gamepad), add an explicit protocol:

```toml
[[endpoints]]
alias = "gamepad"
direction = "Input"
type = "Matcher"
protocol = "Hid"
matchers = [{ type = "ControllerGuid", value = "030000005e040000fd02000003090000" }]
```

### Matcher Types

Ordered by specificity (highest first — the resolver prefers more specific
matches when several endpoints could claim a port):

| Matcher | Description | Example |
|---------|-------------|---------|
| `CoreMidiUniqueId` | macOS CoreMIDI unique ID | `{ type = "CoreMidiUniqueId", value = 12345 }` |
| `SysExIdentity` | SysEx identity reply (requires probing) | `{ type = "SysExIdentity", manufacturer_id = [0x00, 0x21, 0x09] }` |
| `UsbIdentifier` | USB vendor/product ID | `{ type = "UsbIdentifier", vendor_id = 0x17CC, product_id = 0x1600 }` |
| `UsbTopology` | USB topology path | `{ type = "UsbTopology", value = "1-2.3" }` |
| `ExactName` | Exact port name match | `{ type = "ExactName", value = "Maschine Mikro MK3" }` |
| `PlatformId` | Platform-specific device ID | `{ type = "PlatformId", value = "..." }` |
| `NameContains` | Substring match (most common) | `{ type = "NameContains", value = "Mikro" }` |
| `NameRegex` | Regex pattern match (max 256 chars) | `{ type = "NameRegex", value = "Mikro.*MK[23]" }` |
| `ControllerGuid` | Gamepad model identity (SDL GUID) | `{ type = "ControllerGuid", value = "0300...0000" }` |

Use `NameContains` for most setups. Use `ExactName` or `CoreMidiUniqueId`
when you have multiple similar devices.

### Direction Configuration

`direction` is **required** — there is no default:

| `direction` | Use Case |
|-------------|----------|
| `"Input"` | Controllers sending events |
| `"Output"` | Synths/lights receiving MIDI |
| `"Bidirectional"` | Controllers with LED feedback |

```toml
# Bidirectional endpoint (controller with LEDs), symmetric matchers
[[endpoints]]
alias = "mikro"
direction = "Bidirectional"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Mikro" }]
```

When the input and output port names differ, use asymmetric matchers instead
of `matchers`:

```toml
[[endpoints]]
alias = "mikro"
direction = "Bidirectional"
type = "Matcher"
input_matchers = [{ type = "NameContains", value = "Mikro In" }]
output_matchers = [{ type = "NameContains", value = "Mikro Out" }]
```

Setting `output_matchers` on a `direction = "Input"` endpoint (or
`input_matchers` on `"Output"`) is a hard config error.

### Channel Scoping

Scope a whole endpoint to specific MIDI channels with `channels` (0-indexed,
empty = all channels):

```toml
[[endpoints]]
alias = "pads"
direction = "Input"
type = "Matcher"
channels = [9]  # Channel 10 only
matchers = [{ type = "NameContains", value = "Mikro" }]
```

Or filter per-trigger:

```toml
[[modes.mappings]]
[modes.mappings.trigger]
type = "Note"
note = 36
channel = 9  # Channel 10 only (0-indexed)
device = "pads"

[modes.mappings.action]
type = "Keystroke"
keys = "space"
```

## Rewriting Legacy Config

Legacy blocks fail to load — rewrite them by hand:

**Before (removed — hard load error):**
```toml
[device]
name = "Maschine Mikro MK3"
```
```toml
[[bindings]]
alias = "mikro"
[bindings.input]
matchers = [{ type = "NameContains", value = "Mikro MK3" }]
```

**After:**
```toml
[[endpoints]]
alias = "mikro"
direction = "Input"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Mikro MK3" }]
```

The mapping is mechanical: `[bindings.input]`-only → `direction = "Input"`,
`[bindings.output]`-only → `"Output"`, both → `"Bidirectional"` (with
`input_matchers`/`output_matchers` if the two matcher lists differed);
matcher tables carry over unchanged. Validate with
`conductorctl validate` (running daemon) or load-check the file before
reloading.

(`conductorctl migrate-config --routing` still exists, but it only rewrites
legacy `Trigger::Raw` + `MidiForward` mappings into `[[routes]]` — it does
not touch I/O declarations.)

## Endpoint Health Diagnosis

Use `conductor_list_device_bindings` to check endpoint health:

- **connected = true**: Port found, events flowing
- **connected = false**: No matching port — check physical connection
- **enabled = false**: Endpoint is muted (`enabled = false` in config) — events ignored
- **is_configured = false**: Port discovered but no endpoint declared for it

Common issues:
1. **Endpoint disconnected**: Port name changed. Check `conductor_list_discovered_ports`.
2. **Multiple endpoints match same port**: Use more specific matchers (the
   specificity order above decides ties, but overlapping `NameContains`
   patterns are fragile).
3. **Endpoint matches wrong port**: Tighten the matcher pattern.

See [DEVICES.md](references/DEVICES.md) for supported devices.

## UI Mode awareness (Conductor Studio GUI)

When a Conductor Studio GUI is attached, device status dots appear in its title bar in both modes; refer to "the device dots in the title bar" regardless of `ui_mode`. A headless daemon has no such surface.
