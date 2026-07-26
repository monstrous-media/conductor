# Trigger Types Reference

Complete documentation for all Conductor trigger types. Use this reference when the
quick reference table in SKILL.md doesn't provide enough detail.

## Channel Filtering (All MIDI Triggers)

All MIDI trigger types support an optional `channel` field for filtering by MIDI channel.

```toml
channel = 5  # Optional: MIDI channel (0-15, 0-indexed). Omit to match any channel.
```

- **0-indexed in config** (0-15), **1-indexed in display** (1-16). Channel 0 in config = Channel 1 in DAW.
- Omitting `channel` in TOML configs matches events on any channel (backward compatible). In JSON/schema representations, `channel: null` has the same meaning.
- Gamepad triggers do not support `channel` (not a MIDI concept).

---

## MIDI Triggers

### Note

Basic note trigger with optional velocity threshold. Use for simple pad presses.

**Schema:**
```toml
[trigger]
type = "Note"
note = 60          # MIDI note number (0-127)
velocity_min = 1   # Optional: minimum velocity to trigger (0-127)
channel = 0        # Optional: MIDI channel (0-15). Omit for any channel.
```

**When to use:**
- Simple button/pad presses
- Any velocity triggers the action
- Need basic on/off behavior

**Notes:**
- `velocity_min` defaults to 1 (any non-zero velocity)
- MIDI note 0-127 range, but check controller documentation for actual notes used
- `channel` is optional; omit to match any channel

---

### VelocityRange

Velocity-sensitive trigger with different actions per velocity level. Use when user
wants different behaviors for soft vs hard presses.

**Schema:**
```toml
[trigger]
type = "VelocityRange"
note = 60          # MIDI note number (0-127)
soft_max = 40      # Optional: velocities 1-40 are "soft" (default: 40)
medium_max = 80    # Optional: velocities 41-80 are "medium" (default: 80)
                   # Velocities 81-127 are "hard"
channel = 0        # Optional: MIDI channel (0-15). Omit for any channel.
```

**When to use:**
- User says "hit hard" vs "tap lightly"
- Need velocity-sensitive actions
- Musical expression (soft=piano, hard=forte)

**Notes:**
- Pair with a `VelocityRange` action that specifies `soft_action`, `medium_action`, `hard_action`
- Thresholds are inclusive (velocity 40 is "soft", 41 is "medium")

---

### LongPress

Hold detection. Triggers when a note is held for longer than the specified duration.

**Schema:**
```toml
[trigger]
type = "LongPress"
note = 60            # MIDI note number (0-127)
duration_ms = 2000   # Optional: hold time in milliseconds (default: 2000)
channel = 0          # Optional: MIDI channel (0-15). Omit for any channel.
```

**When to use:**
- "Hold to X" behavior
- Secondary action on same button
- Shift-like modifiers

**Notes:**
- Default duration is 2000ms (2 seconds) from advanced_settings
- Can be overridden per-mapping with `duration_ms`
- NoteOff must occur AFTER duration for trigger to fire

---

### DoubleTap

Quick double-press detection. Triggers when a note is pressed and released quickly twice.

**Schema:**
```toml
[trigger]
type = "DoubleTap"
note = 60           # MIDI note number (0-127)
timeout_ms = 300    # Optional: time window for double-tap (default: 300)
channel = 0         # Optional: MIDI channel (0-15). Omit for any channel.
```

**When to use:**
- Quick confirmation actions
- Alternative to shift+press
- "Double-click" equivalent

**Notes:**
- Default timeout is 300ms from advanced_settings
- Both presses must complete within the timeout window
- Velocity of either press can vary

---

### NoteChord

Multiple notes pressed simultaneously. Use for combo triggers.

**Schema:**
```toml
[trigger]
type = "NoteChord"
notes = [36, 37, 38]  # List of MIDI note numbers
timeout_ms = 50       # Optional: window for simultaneous detection (default: 50)
channel = 0           # Optional: MIDI channel (0-15). Omit for any channel.
```

**When to use:**
- "Press these together" behavior
- Shift+button patterns
- Complex macro triggers

**Notes:**
- All notes must be pressed within `timeout_ms` of each other
- Default timeout is 50ms from advanced_settings
- Order doesn't matter, only simultaneity

---

### EncoderTurn

Encoder rotation with direction. Use for knobs that spin continuously (no endpoints).

**Schema:**
```toml
[trigger]
type = "EncoderTurn"
cc = 16                      # Control Change number (0-127)
direction = "Clockwise"      # Optional: "Clockwise", "CounterClockwise", or null for both
channel = 0                  # Optional: MIDI channel (0-15). Omit for any channel.
```

**When to use:**
- Rotary encoders (endless knobs)
- Jog wheels
- Scroll-like behavior

**Notes:**
- Encoders send CC messages with relative values
- Direction can be null to trigger on any rotation
- Common CC values: 16-31 for encoders on many controllers

---

### Aftertouch

Channel pressure sensitivity. Triggers based on aftertouch values.

**Schema:**
```toml
[trigger]
type = "Aftertouch"
pressure_min = 64    # Optional: minimum pressure to trigger (0-127)
channel = 0          # Optional: MIDI channel (0-15). Omit for any channel.
```

**When to use:**
- Pressure-sensitive pads
- Expressive control
- Secondary modulation

**Notes:**
- Channel aftertouch (affects all notes)
- Not all controllers support aftertouch
- Polyphonic aftertouch not currently supported

---

### PitchBend

Touch strip or pitch bend wheel control.

**Schema:**
```toml
[trigger]
type = "PitchBend"
value_min = 0       # Optional: minimum value (0-16383)
value_max = 16383   # Optional: maximum value (0-16383)
channel = 0         # Optional: MIDI channel (0-15). Omit for any channel.
```

**When to use:**
- Touch strips
- Pitch bend wheels
- Expression pedals mapped to pitch

**Notes:**
- 14-bit resolution (0-16383)
- Center value is 8192
- Use ranges for "zones" on touch strips

---

### CC

Generic Control Change message. Use for faders, buttons that send CC, etc.

**Schema:**
```toml
[trigger]
type = "CC"
cc = 1              # Control Change number (0-127)
value_min = 1       # Optional: minimum value to trigger (0-127)
channel = 0         # Optional: MIDI channel (0-15). Omit for any channel.
```

**When to use:**
- Faders/sliders (absolute position)
- CC buttons
- Expression pedals
- Any CC message

**Notes:**
- For encoders, prefer `EncoderTurn` for direction detection
- `value_min` useful for "above threshold" triggers

---

## Gamepad Triggers (v3.0+)

Gamepad triggers use button/axis IDs in the 128-255 range to avoid conflicts with MIDI.

### GamepadButton

Button press on a game controller.

**Schema:**
```toml
[trigger]
type = "GamepadButton"
button = 128          # Button ID (128-255)
velocity_min = 1      # Optional: minimum velocity (0-127)
```

**Button ID Reference:**
| ID | Button | Xbox | PlayStation | Switch |
|----|--------|------|-------------|--------|
| 128 | South | A | Cross | B |
| 129 | East | B | Circle | A |
| 130 | West | X | Square | Y |
| 131 | North | Y | Triangle | X |
| 132 | DPadUp | D-Pad Up | D-Pad Up | D-Pad Up |
| 133 | DPadDown | D-Pad Down | D-Pad Down | D-Pad Down |
| 134 | DPadLeft | D-Pad Left | D-Pad Left | D-Pad Left |
| 135 | DPadRight | D-Pad Right | D-Pad Right | D-Pad Right |
| 136 | LeftShoulder | LB | L1 | L |
| 137 | RightShoulder | RB | R1 | R |
| 138 | LeftThumb | LS | L3 | LS |
| 139 | RightThumb | RS | R3 | RS |
| 140 | Start | Menu | Options | + |
| 141 | Select | View | Share | - |
| 142 | Guide | Xbox | PS | Home |
| 143 | LeftTrigger | LT (digital) | L2 (digital) | ZL |
| 144 | RightTrigger | RT (digital) | R2 (digital) | ZR |

---

### GamepadButtonChord

Multiple gamepad buttons pressed simultaneously.

**Schema:**
```toml
[trigger]
type = "GamepadButtonChord"
buttons = [128, 129]   # List of button IDs (128-255)
timeout_ms = 50        # Optional: window for simultaneous detection
```

**When to use:**
- Button combos (A+B)
- Shift patterns (LB+A)
- Complex macro triggers

---

### GamepadAnalogStick

Analog stick axis movement.

**Schema:**
```toml
[trigger]
type = "GamepadAnalogStick"
axis = 128                   # Axis ID (128-131)
direction = "Clockwise"      # Optional: "Clockwise" (right/up), "CounterClockwise" (left/down)
```

**Axis ID Reference:**
| ID | Axis |
|----|------|
| 128 | Left Stick X (left/right) |
| 129 | Left Stick Y (up/down) |
| 130 | Right Stick X (left/right) |
| 131 | Right Stick Y (up/down) |

---

### GamepadTrigger

Analog trigger pull (L2/R2).

**Schema:**
```toml
[trigger]
type = "GamepadTrigger"
trigger = 132      # Trigger ID (132-133)
threshold = 64     # Optional: minimum pull value (0-127)
```

**Trigger ID Reference:**
| ID | Trigger |
|----|---------|
| 132 | Left Trigger (L2/LT) |
| 133 | Right Trigger (R2/RT) |

**When to use:**
- Variable pressure actions (throttle, brake)
- "Partial pull" vs "full pull" distinctions
- Expression control

**Notes:**
- Use `threshold` to set dead zone
- Value 0-127 represents trigger pull depth
