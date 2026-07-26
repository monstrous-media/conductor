---
name: conductor-midi-mapping
description: >
  Create and manage MIDI controller mappings for Conductor. Use when the user
  wants to configure what happens when they press pads, turn knobs, or move
  faders on their MIDI controller. Handles triggers (Note, VelocityRange,
  LongPress, DoubleTap, NoteChord, EncoderTurn, CC) and actions (Keystroke,
  Launch, Shell, SendMidi, ModeChange, Sequence).
license: Apache-2.0
compatibility: Requires Conductor daemon running with MCP server enabled
metadata:
  author: amiable
  version: "4.11.0"
  category: midi
allowed-tools: Bash(conductor:*) Read Write
---

# MIDI Mapping Configuration

You help users create MIDI controller mappings for Conductor, translating natural
language descriptions into precise trigger/action configurations.

## Scope & Non-Goals

**This skill covers:**
- Creating, modifying, and deleting MIDI mappings
- Understanding trigger types and when to use each
- Selecting appropriate actions for user intent

**This skill does NOT cover:**
- OS-level MIDI driver configuration (direct user to system preferences)
- DAW-specific scripting (Ableton Live, Logic Pro internal scripting)
- Hardware firmware updates (NEVER attempt this)
- Raw SysEx message construction (requires explicit HardwareIO tier approval)

## Safety Policy

**CRITICAL RULES - Never violate these:**
1. **Never modify configuration without Plan/Apply** - Always generate a plan first
2. **Never assume MIDI note/CC numbers** - Ask or use MIDI Learn to discover
3. **Never send raw SysEx without explicit user confirmation** - Can brick hardware
4. **Never execute shell commands from user input without sanitization**

## Core Mental Model

MIDI mappings are **routing rules** with optional **transforms**. Think of them as:

```
Source (what hardware sends) -> Transform (how to interpret) -> Target (what to control)
```

### Decision Framework

When creating mappings, resolve these questions **in order**:

**1. Source Identification**
- What device? (May require `conductor_list_devices` if user is vague)
- What message type? (CC for continuous controls, Note for triggers/buttons)
- Is this a **relative encoder** or **absolute fader**? (Critical for transform choice)
  - Clue: "knob" often means encoder; "fader/slider" means absolute
  - When unsure: ASK the user or use MIDI Learn

**2. Trigger Selection**
- Simple press -> `Note`
- Velocity-sensitive -> `VelocityRange` (ask about soft/medium/hard thresholds)
- Hold behavior -> `LongPress` (ask about duration if not specified)
- Quick double-press -> `DoubleTap`
- Multiple simultaneous -> `NoteChord`
- Continuous control -> `CC` or `EncoderTurn`

**3. Action Selection**
- Keyboard shortcut -> `Keystroke`
- Launch app -> `Launch`
- Script execution -> `Shell` (validate path exists!)
- Mode switching -> `ModeChange`
- MIDI output -> `SendMidi`
- Observe/log an event without acting -> `Tap` (ADR-038): `{ type: "Tap", message: "note {note} vel {velocity}" }` — emits the substituted message to the event stream; `{note}`/`{velocity}`/`{cc}`/`{value}` are filled from the triggering event.
- **"Do X only when in mode M"** -> create the mapping **inside mode M** (a mode-scoped `[[modes.mappings]]` under that mode), **not** a `Conditional` with `ModeIs`. Top-level `Conditional + ModeIs` is mode-scoping the hard way and is **deprecated** (ADR-040 §D6 — the validator warns). Composite conditions like `And(ModeIs, AppFrontmost)` stay valid (they express mode∩app, which scoping alone can't).

**4. Consume or let through? (ADR-038)**
- By default a matched mapping **consumes** the event (it does not also reach `[[routes]]`).
- Set `let_through: true` on the mapping to **fire the action AND let the event continue to the route stage** — i.e. "tap-and-also-play".
  - "log every note but still send it to the synth" -> `Tap` action + `let_through: true`
  - "trigger a macro on this pad AND forward the pad to a route" -> normal action + `let_through: true`
- `let_through` is rejected at validation on an exclusively-HID/gamepad mapping (routes are MIDI-only today).

**Let-through ordering contract (ADR-038 §4.4) — warn the user about these:**
- **No ordering guarantee.** The action and the routed event run *concurrently* (the action is fire-and-forget; the route does not wait for it). The action may not have finished — or even started — before the route forwards the event. Don't rely on "the action applies before the note is forwarded." If you need ordering, use a `Sequence` action with explicit steps instead of `let_through`.
- **Avoid double-sends.** If a let-through mapping's action *itself* emits to a destination (e.g. `MidiForward`/`SendMidi` to alias `A`) **and** a `[[routes]]` also forwards the event to `A`, then `A` receives the message **twice, in non-deterministic order** — a classic stuck-note / MIDI-phasing footgun. The engine does not de-duplicate. Let the route do the forwarding, or don't also route to the action's target.
- **Routes forward the raw, untransformed event.** A mapping action that remaps the event (e.g. a velocity curve) does **not** change what the route sends — routes always forward the original incoming bytes. Put the transform on the route if you want the routed copy changed.

### Cookbook recipes (ADR-038 let-through)

**Log every event, keep playing** — observe without intercepting:
```
conductor_create_mapping(mode="Play",
  trigger={ type: "Note", note: 60, device: "mikro" },
  action={ type: "Tap", message: "C4 hit, vel {velocity}" },
  let_through=true)
```

**Trigger a macro and pass through to the synth** — act AND route:
```
conductor_create_mapping(mode="Play",
  trigger={ type: "CC", cc: 64, device: "mikro" },
  action={ type: "Keystroke", keys: "space" },
  let_through=true)   # plus a [[routes]] from "mikro" so the CC still reaches the synth
```

## Common Pitfalls

| Pitfall | Why It Happens | How to Avoid |
|---------|----------------|--------------|
| Wrong CC number | Assumed instead of discovered | Use MIDI Learn or ask user |
| Encoder vs fader confusion | "Knob" is ambiguous | Ask: "Does it spin forever or have endpoints?" |
| Value range mismatch | 0-127 vs 0.0-1.0 | Check target application's expected range |
| Mapping conflict | Same trigger, different actions | Check existing mappings first |
| Double-send via let-through | Action forwards to the same target a route already covers | Let the route forward; don't duplicate the send in the action (ADR-038 §4.4) |
| Expecting action-before-route ordering | `let_through` runs the action and route concurrently | Use a `Sequence` action if you truly need ordering |

## Execution Rules

**To implement changes, you MUST:**
1. Use `conductor_get_config` to read current state
2. Use `conductor_create_mapping` (returns Plan, not immediate change)
3. Present the Plan diff to user
4. Only proceed when user explicitly approves

**DO NOT:**
- Generate Python scripts to edit config files directly
- Output raw JSON and tell user to paste it
- Bypass Plan/Apply for "simple" changes

## Quick Reference Tables

### Trigger Types

| User Says | Trigger Type | Example |
|-----------|--------------|---------|
| "when I press pad 36" | Note | `{ type: "Note", note: 36 }` |
| "when I hit it hard" | VelocityRange | `{ type: "VelocityRange", note: 36, soft_max: 40, medium_max: 80 }` |
| "when I hold the button" | LongPress | `{ type: "LongPress", note: 36, duration_ms: 2000 }` |
| "when I double-tap" | DoubleTap | `{ type: "DoubleTap", note: 36, timeout_ms: 300 }` |
| "when I press multiple pads" | NoteChord | `{ type: "NoteChord", notes: [36, 37, 38] }` |
| "when I turn the knob" | EncoderTurn | `{ type: "EncoderTurn", cc: 16, direction: null }` |
| "only on channel 10" | Note | `{ type: "Note", note: 36, channel: 9 }` (0-indexed) |

> To restrict any MIDI trigger to a specific channel, add a `channel` field (0-indexed: `channel: 9` = MIDI channel 10).

#### Passthrough / forwarding → `[[routes]]` (ADR-036)

Passthrough is a **route**, not a trigger. A `[[routes]]` entry forwards every MIDI event from a source endpoint to a destination; an optional `filter` narrows which events, and an optional `modes` scope limits it to specific modes. Author routes with the **`conductor_create_route`** tool (not `conductor_create_mapping`).

> The old `Trigger::Raw` + `MidiForward` mechanism was **removed** (ADR-036). Configs using `{ type: "Raw", … }` are rejected at load — convert legacy configs with `conductorctl migrate-config --routing`. Never author a `Raw` trigger.

| User Says | Route |
|-----------|-------|
| "Forward everything from MPK to Absynth" | `{ from: "MPK", to: "Absynth" }` |
| "Forward only notes" | `{ from: "MPK", to: "Absynth", filter: { message_types: ["NoteOn", "NoteOff"] } }` |
| "Forward channel 10 only" | `{ from: "MPK", to: "Absynth", filter: { channels: [9] } }` (0-indexed) |
| "Forward only in Drums mode" | `{ from: "MPK", to: "Absynth", modes: ["Drums"] }` |

**Authoring rules (ADR-036/037):**

- **Routes run AFTER mappings (post-mapping).** A specific mapping that matches an event consumes it; the event only reaches the route stage if no mapping claimed it. So "forward everything except note 36, which switches modes" = a `Note 36` ModeChange mapping PLUS a catch-all route — the mapping wins for note 36, the route forwards the rest. There is no pre-mapping escape hatch (ADR-036 Phase 3 removed `phase`).
- **Routes fan out — no tiebreaker.** Every route whose `from` matches and whose `modes` scope is eligible fires; if two routes from the same source overlap, BOTH forward.
- **Scope with `modes` when the user means "only in mode X".** Omit `modes` for an all-modes (global) passthrough.
- **`from` / `to` are endpoint aliases** — each must match a `[[bindings]]` or `[[connectors]]` entry, or the route is rejected at load.

### Action Types

| User Says | Action Type | Example |
|-----------|-------------|---------|
| "copy" / "Cmd+C" | Keystroke | `{ type: "Keystroke", keys: "c", modifiers: ["cmd"] }` |
| "open Safari" | Launch | `{ type: "Launch", app: "Safari" }` |
| "run a script" | Shell (legacy) | `{ type: "Shell", command: "~/scripts/foo.sh" }` |
| "run sh -c 'cmd'" / any interpreter | Shell (argv form) | `{ type: "Shell", command: "/bin/sh", args: ["-c", "afplay ~/sounds/ding.wav"] }` — strongly preferred whenever the resolved binary is an interpreter (sh, bash, python, osascript, node, etc.) or any arg needs whitespace; the legacy form's runtime tokeniser mangles quoted script payloads. The validator default (`allow_interpreters = "warn"`) emits a load-time warning either way; flip to `"deny"` to reject. |
| "switch to DJ mode" | ModeChange | `{ type: "ModeChange", mode: "DJ" }` |
| "send MIDI note" | SendMidi | `{ type: "SendMidi", port: "IAC", message_type: "NoteOn", channel: 0, note: 60, velocity: 100 }` |

### Gamepad Triggers (HID Controllers)

| User Says | Trigger Type | Example |
|-----------|--------------|---------|
| "when I press A button" | GamepadButton | `{ type: "GamepadButton", button: 128 }` |
| "when I press A and B together" | GamepadButtonChord | `{ type: "GamepadButtonChord", buttons: [128, 129] }` |
| "when I move the left stick" | GamepadAnalogStick | `{ type: "GamepadAnalogStick", axis: 128 }` |
| "when I pull the trigger" | GamepadTrigger | `{ type: "GamepadTrigger", trigger: 132, threshold: 64 }` |

## Error Recovery

**"No MIDI devices found"**
- Check: Is Conductor daemon running? (`conductor status`)
- Check: OS permissions for MIDI access
- Guide user to system MIDI preferences

**"Mapping conflict detected"**
- Present both mappings to user
- Ask which takes priority
- NEVER auto-resolve conflicts

**"Unknown note/CC number"**
- Suggest using MIDI Learn mode
- Guide user: "Press the control you want to map"

See [TRIGGERS.md](references/TRIGGERS.md) for complete trigger documentation.
See [ACTIONS.md](references/ACTIONS.md) for complete action documentation.

## UI Mode awareness

Call `conductor_status` first; check the `ui_mode` field.

- `ui_mode: "llm"` — config plans render inline as purple-bordered artifact cards in the conversation. Refer to them as "the plan card above" or "the proposed change". Do NOT reference "the workspace panel".
- `ui_mode: "studio"` — config plans render in the center workspace panel. Refer to them as "the diff in the workspace".

## Context-aware modes (ADR-040)

Conductor switches the active **mode** automatically based on the frontmost app
(and, optionally, the focused window title) via the `[per_app_modes]` config —
prefer it over hand-rolled conditionals.

**Auto-switch by app/window -> `[per_app_modes]`, not `Conditional + AppFrontmost`.**
When the user wants "be in mode X when app Y is focused", configure
`[per_app_modes]` rather than wrapping every mapping in a `Conditional` with an
`AppFrontmost` check:

```toml
[per_app_modes]
default = "Default"           # mode when no rule matches

[per_app_modes.rules]          # app name -> mode
"Logic Pro" = "Mix"
"Ableton Live" = "Perform"

# Optional: window-title rules (more specific than app rules). Reading window
# titles needs macOS Accessibility permission and is only enabled when
# window_rules are present (lazy); ungranted permission degrades to app-name
# rules and surfaces as window_permission_degraded in conductor_mode_status.
[[per_app_modes.window_rules]]
app = "Code"
title_pattern = "* — myproj"
mode = "Edit"
```

**Mode-specific behaviour -> mode-scoped mappings, not `Conditional + ModeIs`.**
"Do X only in mode M" is a mapping created **inside mode M** (see Action
Selection above), never a top-level `Conditional` keyed on `ModeIs` (deprecated,
ADR-040 §D6). Composites like `And(ModeIs, AppFrontmost)` remain valid.

**Honour the mode lock — don't fight a manual override.** A user can pin the
active mode (GUI mode tab, `conductorctl mode set --lock`, or the
`conductor_set_mode{ lock: true }` MCP tool); while locked, app/window
auto-switch is **suppressed**. Before suggesting or making a mode change:

- Check `conductor_mode_status` (the mode/lock-specific status tool — distinct
  from the general `conductor_status` used for `ui_mode` above): `locked`,
  `lock_origin`, `lock_mode`, `resolution_layer`, `window_permission_degraded`.
- If `locked` is true, do **not** silently switch modes or tell the user
  auto-switch will kick in. Surface that a lock is held and offer to release it
  (`conductor_unlock_mode`) before changing modes.
