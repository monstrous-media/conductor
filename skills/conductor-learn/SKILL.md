---
name: conductor-learn
description: >
  Guide users through Learn mode to capture controller inputs and create
  mappings. Use when the user wants to "learn" or "capture" what their
  controller does, or when they don't know the note numbers, button IDs,
  or CC values for their controls. Supports MIDI controllers and game
  controllers (HID). Detects patterns like LongPress, DoubleTap, and
  Chords automatically.
license: Apache-2.0
compatibility: Requires Conductor daemon with a device connected
metadata:
  author: Monstrous Media
  version: "4.28.0"
  category: learn
allowed-tools: Bash(conductor:*) Read Write
---

# Learn Mode

Help users discover their controller's messages and create mappings from
captured events. Learn mode eliminates guesswork about note numbers, CC values,
button IDs, and controller behavior.

## Scope & Non-Goals

**This skill covers:**
- Starting and stopping Learn capture sessions
- Analyzing captured events to determine optimal trigger types
- Detecting patterns (LongPress, DoubleTap, Chord, VelocityRange)
- Source identification: selecting which binding/port to capture from
- Multi-protocol support (MIDI and HID)

**This skill does NOT cover:**
- Creating mappings directly (delegate to conductor-midi-mapping skill)
- Troubleshooting connection issues (delegate to conductor-troubleshooting)
- Configuring bindings (delegate to conductor-binding-setup)

## Source Identification

Before starting Learn mode, identify which binding to capture from:

```
Agent: Let me check your connected devices.
       [Uses conductor_list_discovered_ports]

       I see 2 connected ports:
       - "pads" (Maschine Mikro MK3 MIDI) — MIDI, Receive
       - Xbox Wireless Controller — HID, Receive

       Which controller do you want to learn from?
```

For MIDI controllers, Learn captures Note, CC, Aftertouch, PitchBend, and
Encoder events. For HID controllers, it captures button presses and analog
stick/trigger movements.

## Workflow

> **You CANNOT monitor MIDI events in real time.** Once `conductor_start_midi_learn` is called, you have no visibility into incoming events. You will know Learn has captured something only when (a) the user clicks "Stop Learn" in the Events panel — Learn is already stopped by the GUI, you receive a `system_event` describing the outcome, and you must NOT call `conductor_stop_midi_learn` again — or (b) the user tells you in chat that they're done pressing and you call `conductor_stop_midi_learn` yourself, or (c) the daemon-enforced session timeout fires and you receive a `system_event` like "MIDI Learn mode auto-stopped after Ns" — at which point you call `conductor_stop_midi_learn` to drain whatever the daemon's buffer captured before the timeout.
>
> **How to learn the capture outcome differs per path:**
>
> - **Path (a)** — the `system_event` *itself* tells you the outcome: either "Note trigger captured … MappingEditor is open with the trigger pre-filled" (continue with action-fill) or "No trigger was detected" (nothing captured; tell the user, offer retry or manual). Do NOT call `conductor_stop_midi_learn` — Learn is already stopped.
> - **Path (b)** — the response from your `conductor_stop_midi_learn` call has the same two shapes: `suggested_trigger` present (action-fill) or absent (nothing captured).
> - **Path (c)** — the timeout `system_event` only says Learn auto-stopped; it deliberately does NOT tell you whether anything was captured (the GUI countdown can't see the daemon's buffer). You MUST then call `conductor_stop_midi_learn` to drain and inspect — branch on `suggested_trigger` present/absent as in path (b).
>
> **NEVER tell the user that you will detect their press and stop Learn automatically — you cannot do this.** Always direct them to click "Stop Learn" in the Events panel OR tell you in chat when they're done.

1. **Identify Source**: Check which binding/port to capture from
2. **Start Capture**: Use `conductor_start_midi_learn` tool
3. **Guide the User**: Ask them to press/turn the controls AND to either click "Stop Learn" or tell you when done
4. **Analyze Events**: Path (b) or (c) — use `conductor_stop_midi_learn` to get captured events. Path (a) — Learn is already stopped by the GUI (do NOT call `conductor_stop_midi_learn` again); read the `system_event` for the outcome.
5. **Detect Patterns**: Identify LongPress, DoubleTap, Chord, or VelocityRange
6. **Suggest Mappings**: Propose mappings based on captured events
7. **Apply with Approval**: Use Plan/Apply pattern for user confirmation

## Starting a Capture Session

```
Agent: Let me start Learn mode so we can capture your controller's messages.
       [Uses conductor_start_midi_learn]

       Learn mode is now active. Press or turn the controls you want to
       map, then either click "Stop Learn" in the Events panel OR tell me
       when you're done — I can't see the events arriving in real time, so
       I need you to signal when you're finished.
```

## Pattern Detection

| Pattern | What to Look For | Suggested Trigger |
|---------|-----------------|-------------------|
| LongPress | NoteOn held > 500ms | `LongPress` with detected duration |
| DoubleTap | Same note twice within 400ms | `DoubleTap` with detected interval |
| Chord | Multiple notes within 50ms | `NoteChord` with detected notes |
| Velocity Layers | Same note at very different velocities | `VelocityRange` mappings |
| Encoder | CC with incrementing/decrementing values | `EncoderTurn` |
| Gamepad Button | HID button press (IDs 128-255) | `GamepadButton` |
| Analog Stick | HID stick movement | `GamepadAnalogStick` |

## Example Session

```
User: "I want to set up my Launchpad"

Agent: Let me start Learn mode. Press the pads you want to configure, and try
       the gestures you want to use (tap, hold, double-tap, etc.).
       [Uses conductor_start_midi_learn]

User: [Presses pads, holds one, double-taps another]

Agent: [Uses conductor_stop_midi_learn]

       I detected several events:

       1. **Pad at note 36** — Simple tap, velocity range: 45-112
       2. **Pad at note 37** — Long press (~1.5 seconds)
       3. **Pad at note 40** — Double-tap (~200ms interval)
       4. **Pads 44, 45, 46** — Chord (pressed together within 30ms)

       What would you like each of these to do?
```

## Error Recovery

**"Learn mode is already active"**
- Stop the current session first
- Ask if user wants to discard captured events

**"No device connected"**
- Check connection with `conductor_list_discovered_ports`
- Guide user to create a binding first (delegate to conductor-binding-setup)

**"No events captured"**
- Check if controller is sending messages
- Verify the correct port/binding is being monitored
- Some controllers need "local control" enabled

See [PATTERNS.md](references/PATTERNS.md) for advanced pattern detection algorithms.

## UI Mode awareness

Call `conductor_status` first; check the `ui_mode` field.

- `ui_mode: "llm"` — when you call `start_midi_learn`, a 320px events drawer slides in from the right. Tell the user: "Watch the events drawer for your input."
- `ui_mode: "studio"` — the events panel is already visible on the right. Tell the user: "Watch the event stream panel for your input."
