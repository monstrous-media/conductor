---
name: conductor-troubleshooting
description: >
  Diagnose and resolve common Conductor issues including port detection
  failures, events not triggering, wrong device routing, and LED feedback
  problems. Use when the user reports something isn't working.
license: MIT
compatibility: Requires Conductor daemon running
metadata:
  author: Monstrous Media
  version: "4.28.0"
  category: troubleshooting
allowed-tools: Bash(conductor:*) Read Write
---

# Troubleshooting

Diagnose and resolve common issues with Conductor's device binding, event
processing, and LED feedback systems.

## Scope

**This skill covers:**
- Port not found
- Events not triggering mappings
- Events from wrong device
- LED feedback not working
- Binding health diagnosis

**This skill does NOT cover:**
- OS-level driver installation
- Hardware repairs or firmware updates
- Network/MIDI-over-network issues

## Problem 1: Port Not Found

**Symptom**: `conductor_list_discovered_ports` shows no ports, or the expected
port is missing.

### Diagnosis Steps

```
Agent: Let me check what ports Conductor can see.
       [Uses conductor_list_discovered_ports]

       [If no ports found:]
       I don't see any ports. Let's troubleshoot:

       1. **Check physical connection**
          - Is the controller powered on?
          - Is the USB cable connected? (Try a different port/cable)
          - For Bluetooth: Is the controller paired in OS settings?

       2. **Check OS MIDI settings**
          - macOS: Open "Audio MIDI Setup" (search in Spotlight)
          - Windows: Device Manager > Sound controllers
          - Linux: Run `aconnect -l` to list ALSA MIDI ports

       3. **Check permissions**
          - macOS: System Settings > Privacy & Security > Input Monitoring
          - Grant Conductor access if prompted

       4. **Restart Conductor daemon**
          - conductorctl stop && conductorctl start
```

### HID-Specific

```
Agent: For game controllers:

       1. Try USB connection first (more reliable than Bluetooth)
       2. Check input_mode in [advanced_settings]:
          input_mode = "Both"  # or "GamepadOnly"
       3. Verify controller is recognized by the OS
       4. Some uncommon controllers need SDL2 mapping files
```

## Problem 2: Events Not Triggering

**Symptom**: Device is connected, events appear in the event stream, but
mappings don't fire.

### Diagnosis Steps

```
Agent: Let me check your setup.
       [Uses conductor_get_status]
       [Uses conductor_list_device_bindings]
       [Uses conductor_list_mappings]

       Things to check:

       1. **Binding muted?**
          - Check if the binding shows enabled = false
          - Unmute: conductor_set_device_enabled with enabled = true

       2. **Wrong mode?**
          - Mappings are mode-specific
          - Check which mode is active vs which mode has the mapping

       3. **Trigger mismatch?**
          - Note number doesn't match (use Learn mode to verify)
          - Velocity below threshold
          - Channel filter excluding events
          - Device filter on trigger doesn't match binding alias

       4. **Binding not matching port?**
          - Binding shows connected = false
          - Matcher pattern doesn't match current port name
```

### Quick Fix Checklist

| Check | MCP Tool | What to Look For |
|-------|----------|-----------------|
| Binding connected? | `conductor_list_device_bindings` | `connected: true` |
| Binding enabled? | `conductor_list_device_bindings` | `enabled: true` |
| Correct mode active? | `conductor_get_status` | `selected_mode` |
| Mapping exists? | `conductor_list_mappings` | Trigger matches events |
| Config valid? | `conductor_validate_config` | No errors |

## Problem 3: Wrong Device / Events from Wrong Port

**Symptom**: Pressing controller A triggers mappings meant for controller B.

### Diagnosis Steps

```
Agent: This usually means bindings are matching the wrong ports.
       [Uses conductor_list_discovered_ports]
       [Uses conductor_list_device_bindings]

       Check:

       1. **Matcher too broad?**
          - NameContains "MIDI" would match every MIDI port
          - Use more specific patterns: NameContains "Mikro" or ExactName

       2. **Multiple ports match same binding?**
          - Two similar devices (e.g., two Launchpads)
          - Use ExactName or CoreMidiUniqueId for disambiguation

       3. **Missing device filter on triggers?**
          - Add device = "pads" to triggers that should only fire from one binding

       Fix: Tighten matcher patterns or add device filters to triggers.
```

## Problem 4: LEDs Not Working

**Symptom**: Controller LEDs don't respond to Conductor's feedback.

### Diagnosis Steps

```
Agent: Let me check your LED and output configuration.
       [Uses conductor_get_config]
       [Uses conductor_list_device_bindings]

       Check:

       1. **Output binding configured?**
          - The binding needs a [bindings.output] section
          - Direction should be "Receive & Send"

       2. **Output port connected?**
          - Check output_connected in binding status
          - If false, check output matchers match the output port name

       3. **LED config enabled?**
          - Check [led] section: enabled = true
          - Check LED scheme is set (reactive, rainbow, etc.)

       4. **Controller supports LED feedback?**
          - Not all controllers support LED control via MIDI
          - Check controller documentation for SysEx/CC LED commands
```

## General Diagnosis Flow

For any issue, follow this sequence:

1. `conductor_get_status` — Is the daemon running? Is a device connected?
2. `conductor_list_discovered_ports` — What ports does the OS expose?
3. `conductor_list_device_bindings` — Which ports have bindings? Are they connected?
4. `conductor_validate_config` — Is the config valid?
5. `conductor_list_mappings` — Do mappings exist for the active mode?

If the issue persists after checking all five, ask the user to:
- Restart the daemon: `conductorctl stop && conductorctl start`
- Check for OS-level issues (driver updates, permission changes)
- Try a different USB port or cable

## UI Mode awareness (Conductor Studio GUI)

This guidance applies when a Conductor Studio GUI is attached. A headless daemon (CLI/MCP-only) may report no `ui_mode` — skip this section in that case.

`conductor_get_status` includes `ui_mode` ("llm" or "studio"). When asking the user to verify state visually:

- `ui_mode: "llm"`: events drawer (right slide-in), context chips above input, conversation header above messages.
- `ui_mode: "studio"`: workspace panel (center), event stream panel (right), chat panel (left).
