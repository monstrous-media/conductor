---
name: conductor-binding-setup
description: >
  Help users set up and configure bindings for MIDI controllers and game
  controllers (HID) with Conductor. Use when the user wants to connect a
  device, create a binding, troubleshoot connection issues, or migrate from
  legacy [device] config to the [[bindings]] format.
license: MIT
compatibility: Requires Conductor daemon running
metadata:
  author: Monstrous Media
  version: "4.28.0"
  category: binding
allowed-tools: Bash(conductor:*) Read Write
---

# Binding Setup

Help users connect and configure input devices using Conductor's three-layer
model: **Discovery** (ports) → **Bindings** (config aliases) → **Mapping Rules**.

## Three-Layer Model

1. **Discovery**: The OS exposes ports (MIDI input/output, HID). Conductor
   enumerates them automatically. Use `conductor_list_discovered_ports` to see
   all ports and their binding status.
2. **Bindings**: A `[[bindings]]` entry gives a stable alias to one or more
   ports via matchers. Bindings survive port renumbering and reconnection.
3. **Mapping Rules**: Triggers and actions reference bindings by alias. A
   mapping rule like `device = "pads"` targets whichever port the "pads"
   binding resolves to.

## Scope & Non-Goals

**This skill covers:**
- Listing discovered ports and their binding status
- Creating bindings with `[[bindings]]` sections and matchers
- Configuring input mode (MidiOnly, GamepadOnly, Both) in `[advanced_settings]`
- Migrating legacy `[device]` config to `[[bindings]]` format
- Binding health diagnosis
- Channel-scoped bindings
- Direction configuration (Receive, Send, Receive & Send)

**This skill does NOT cover:**
- Creating mappings (delegate to conductor-midi-mapping skill)
- Learn mode capture (delegate to conductor-learn skill)
- OS-level MIDI/HID driver installation

## IMPORTANT: Config Format

**`[device]` is deprecated; prefer `[[bindings]]`.** Always use `[[bindings]]` with matchers.

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

## Creating Bindings

Each binding needs a `[[bindings]]` entry with an alias and matchers:

```toml
[[bindings]]
alias = "pads"
description = "Maschine Mikro MK3 pad controller"

[bindings.input]
matchers = [{ type = "NameContains", value = "Mikro" }]
```

### Matcher Types

| Matcher | Description | Example |
|---------|-------------|---------|
| `ExactName` | Exact port name match | `{ type = "ExactName", value = "Maschine Mikro MK3" }` |
| `NameContains` | Substring match (most common) | `{ type = "NameContains", value = "Mikro" }` |
| `NameRegex` | Regex pattern match | `{ type = "NameRegex", value = "Mikro.*MK[23]" }` |
| `UsbIdentifier` | USB vendor/product ID | `{ type = "UsbIdentifier", vendor_id = 0x17CC, product_id = 0x1600 }` |
| `CoreMidiUniqueId` | macOS CoreMIDI unique ID | `{ type = "CoreMidiUniqueId", value = 12345 }` |

Use `NameContains` for most setups. Use `ExactName` or `CoreMidiUniqueId` when you have multiple similar devices.

### Direction Configuration

| Config | Direction | Use Case |
|--------|-----------|----------|
| `input` only | Receive | Controllers sending events |
| `output` only | Send | Synths/lights receiving MIDI |
| Both `input` and `output` | Receive & Send | Controllers with LED feedback |

```toml
# Receive & Send binding (controller with LEDs)
[[bindings]]
alias = "mikro"

[bindings.input]
matchers = [{ type = "NameContains", value = "Mikro" }]

[bindings.output]
matchers = [{ type = "NameContains", value = "Mikro" }]
```

### Channel-Scoped Triggers

Filter events by MIDI channel on individual triggers:

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

## Migrating from Legacy Config

```bash
# Preview migration (dry-run)
conductorctl migrate-config

# Apply (creates .bak backup)
conductorctl migrate-config --write
```

**Before (deprecated):**
```toml
[device]
name = "Maschine Mikro MK3"
```

**After:**
```toml
[[bindings]]
alias = "mikro"
[bindings.input]
matchers = [{ type = "NameContains", value = "Mikro MK3" }]
```

## Binding Health Diagnosis

Use `conductor_list_device_bindings` to check binding health:

- **connected = true**: Port found, events flowing
- **connected = false**: No matching port — check physical connection
- **enabled = false**: Binding is muted — events ignored
- **is_configured = false**: Port discovered but no binding created

Common issues:
1. **Binding disconnected**: Port name changed. Check `conductor_list_discovered_ports`.
2. **Multiple bindings match same port**: Use more specific matchers.
3. **Binding matches wrong port**: Tighten the matcher pattern.

See [DEVICES.md](references/DEVICES.md) for supported devices.

## UI Mode awareness (Conductor Studio GUI)

When a Conductor Studio GUI is attached, device status dots appear in its title bar in both modes; refer to "the device dots in the title bar" regardless of `ui_mode`. A headless daemon has no such surface.
