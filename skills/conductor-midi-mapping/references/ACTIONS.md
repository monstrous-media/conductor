# Action Types Reference

Complete documentation for all Conductor action types. Use this reference when the
quick reference table in SKILL.md doesn't provide enough detail.

## Keyboard & Mouse Actions

### Keystroke

Simulate keyboard keystroke(s) with optional modifiers.

**Schema:**
```toml
[action]
type = "Keystroke"
keys = "space"           # Key name (e.g., "space", "Return", "Escape", "a", "1")
modifiers = ["cmd"]      # Optional: modifier keys ["cmd", "shift", "alt", "ctrl"]
```

**Common Key Names:**
| Category | Keys |
|----------|------|
| Letters | a-z (lowercase) |
| Numbers | 0-9 |
| Function | F1-F12 |
| Navigation | Return, Escape, Tab, space, Delete, BackSpace |
| Arrows | Up, Down, Left, Right |
| Media | VolumeUp, VolumeDown, Mute, Play, Pause, Next, Previous |

**Modifier Keys:**
| Modifier | macOS | Windows/Linux |
|----------|-------|---------------|
| cmd | Command (⌘) | Windows |
| shift | Shift | Shift |
| alt | Option (⌥) | Alt |
| ctrl | Control (⌃) | Control |

**Examples:**
```toml
# Copy (Cmd+C on macOS)
[action]
type = "Keystroke"
keys = "c"
modifiers = ["cmd"]

# Undo (Cmd+Z)
[action]
type = "Keystroke"
keys = "z"
modifiers = ["cmd"]

# Save (Cmd+Shift+S)
[action]
type = "Keystroke"
keys = "s"
modifiers = ["cmd", "shift"]
```

---

### Text

Type a text string character by character.

**Schema:**
```toml
[action]
type = "Text"
text = "Hello World"    # Text to type
```

**When to use:**
- Typing complete strings
- Inserting snippets
- Auto-fill text fields

**Notes:**
- Types one character at a time (slower than Keystroke)
- Respects current keyboard layout
- Use for longer text sequences

---

### MouseClick

Simulate mouse click at current or specified position.

**Schema:**
```toml
[action]
type = "MouseClick"
button = "left"     # "left", "right", or "middle"
x = 100             # Optional: X coordinate
y = 200             # Optional: Y coordinate
```

**When to use:**
- Click specific UI elements
- Context menus (right-click)
- Automation requiring mouse interaction

**Notes:**
- If x/y not specified, clicks at current cursor position
- Coordinates are absolute screen pixels

---

## Application Actions

### Launch

Open an application by name or path.

**Schema:**
```toml
[action]
type = "Launch"
app = "Safari"    # Application name or full path
```

**Examples:**
```toml
# By name (macOS searches Applications folder)
[action]
type = "Launch"
app = "Safari"

# By full path
[action]
type = "Launch"
app = "/Applications/Visual Studio Code.app"
```

**Notes:**
- On macOS, searches /Applications and ~/Applications
- Use full path for non-standard locations
- If app is already running, typically brings to front

---

### Shell

Execute a shell command. Two schema shapes (ADR-027 D3 §3.1):

**Schema (legacy, single-string):**
```toml
[action]
type = "Shell"
command = "~/scripts/my-script.sh"    # Whitespace-split at runtime
```

**Schema (argv form — prefer when quoting matters):**
```toml
[action]
type = "Shell"
command = "/bin/sh"                    # argv[0]: resolved binary path
args    = ["-c", "echo hi"]            # argv[1..]: passed verbatim
```

**When to use argv form:**
- Whenever the command resolves to an interpreter (`/bin/sh`,
  `/bin/bash`, `python`, `node`, `osascript`, etc.) — the legacy form's
  whitespace splitter is too fragile around quoted scripts.
- Any time an argument contains whitespace that should NOT be split
  into separate argv tokens.
- Any time the user wants per-argument error attribution from the
  validator (errors carry a `args[i]` path).

The executor passes `command` + `args` directly to
`Command::new(command).args(args)` — no whitespace tokenisation, no
shell interpretation. The validator's metacharacter blocklist still
runs on every argv-form `args[i]` so a smuggled `>` redirect inside
argv is rejected the same way as in legacy form.

**`allow_interpreters` policy** (`advanced_settings.allow_interpreters`):
The validator resolves the effective binary after walking wrapper
chains (`env`, `sudo`, `nice`, `nohup`, etc. — 16 wrappers covered)
and applies a per-config policy when it classifies as an interpreter
family:

| Setting | Behaviour |
|---------|-----------|
| `"allow"` | Silent — explicit opt-in for users who want shell scripting |
| `"warn"` (default) | Warning at config load, config still loads |
| `"deny"` | Validation error, config rejected |

So `command = "/usr/bin/env", args = ["python", "-c", "..."]`
resolves to **python** (the wrapper is stripped) and trips the policy
the same way `command = "python"` would. Plan accordingly when
suggesting Shell mappings to the user.

**Security Notes:**
- NEVER use user-provided input directly in commands
- Validate paths exist before execution
- Avoid commands with `rm`, `sudo`, or network operations
- Prefer scripts in known locations over inline commands
- For interpreter invocations, ALWAYS use argv form (one flag per arg)
  so the validator's per-arg blocklist applies cleanly. Legacy
  `command = "/bin/sh -c 'evil'"` is rejected the same way today, but
  argv form gives clearer error paths.

**Examples:**
```toml
# Run a script (single-token command, legacy form is fine)
[action]
type = "Shell"
command = "~/scripts/toggle-dark-mode.sh"

# Open URL in default browser
[action]
type = "Shell"
command = "open https://example.com"

# AppleScript for macOS — argv form because the script payload
# contains quotes that the legacy whitespace-splitter would mangle.
[action]
type = "Shell"
command = "osascript"
args    = ["-e", "tell application \"Finder\" to activate"]

# Shell-interpreted command — argv form is mandatory; `command = "/bin/sh"`
# is the resolved binary, `args = ["-c", "..."]` is the script.
[action]
type = "Shell"
command = "/bin/sh"
args    = ["-c", "afplay ~/sounds/ding.wav"]
```

---

## Control Actions

### ModeChange

Switch to a different mapping mode.

**Schema:**
```toml
[action]
type = "ModeChange"
mode = "DJ"    # Name of the mode to switch to
```

**When to use:**
- Context switching (production mode vs DJ mode)
- Application-specific mappings
- Layered control schemes

**Notes:**
- Mode name must match exactly (case-sensitive)
- Switching modes does not reset held notes
- LED colors typically update to reflect new mode

---

### VolumeControl

Control system volume.

**Schema:**
```toml
[action]
type = "VolumeControl"
operation = "Up"      # "Up", "Down", "Mute", "Unmute", "Set"
value = 50            # Optional: volume level (0-100) for "Set" operation
```

**Operations:**
| Operation | Description |
|-----------|-------------|
| Up | Increase volume by system increment |
| Down | Decrease volume by system increment |
| Mute | Mute system audio |
| Unmute | Unmute system audio |
| Set | Set volume to specific level (requires `value`) |

---

## Sequence & Timing Actions

### Sequence

Execute multiple actions in order.

**Schema:**
```toml
[action]
type = "Sequence"
actions = [
    { type = "Keystroke", keys = "c", modifiers = ["cmd"] },
    { type = "Delay", ms = 100 },
    { type = "Keystroke", keys = "v", modifiers = ["cmd"] }
]
```

**When to use:**
- Complex macros
- Actions requiring timing
- Multi-step operations

**Notes:**
- Actions execute synchronously in order
- Use `Delay` for timing between actions
- Nested sequences are allowed

---

### Delay

Pause execution for a specified duration.

**Schema:**
```toml
[action]
type = "Delay"
ms = 100    # Duration in milliseconds
```

**When to use:**
- Timing between keystrokes
- Waiting for UI to respond
- Pacing rapid actions

**Notes:**
- Typically used inside `Sequence`
- Blocks execution (not async)
- Keep delays short to avoid perceived lag

---

### Repeat

Execute an action multiple times.

**Schema:**
```toml
[action]
type = "Repeat"
count = 5           # Number of repetitions
delay_ms = 50       # Optional: delay between repetitions
action = { type = "Keystroke", keys = "Delete" }
```

**When to use:**
- Delete multiple items
- Repeated increments
- Automation of repetitive tasks

---

## Conditional Actions

### Conditional

Execute different actions based on runtime conditions.

**Schema:**
```toml
[action]
type = "Conditional"

[action.condition]
type = "AppFrontmost"
app = "Safari"

[action.then_action]
type = "Keystroke"
keys = "r"
modifiers = ["cmd"]

[action.else_action]
type = "Keystroke"
keys = "o"
modifiers = ["cmd"]
```

**Condition Types:**

| Type | Description | Parameters |
|------|-------------|------------|
| `Always` | Always true (testing) | None |
| `Never` | Always false (testing) | None |
| `TimeRange` | Time of day | `start: "09:00"`, `end: "17:00"` |
| `DayOfWeek` | Day of week | `days: [1, 2, 3, 4, 5]` (1=Mon, 7=Sun) |
| `AppRunning` | Is app running | `app: "Spotify"` |
| `AppFrontmost` | Is app active | `app: "Safari"` |
| `ModeIs` | Current mode | `mode: "DJ"` |
| `And` | Logical AND | `conditions: [...]` |
| `Or` | Logical OR | `conditions: [...]` |
| `Not` | Logical NOT | `condition: {...}` |

**Examples:**
```toml
# Refresh Safari, open file in other apps
[action]
type = "Conditional"

[action.condition]
type = "AppFrontmost"
app = "Safari"

[action.then_action]
type = "Keystroke"
keys = "r"
modifiers = ["cmd"]

[action.else_action]
type = "Keystroke"
keys = "o"
modifiers = ["cmd"]
```

```toml
# Only during work hours AND in production mode
[action]
type = "Conditional"

[action.condition]
type = "And"
conditions = [
    { type = "TimeRange", start = "09:00", end = "17:00" },
    { type = "ModeIs", mode = "Production" }
]

[action.then_action]
type = "Launch"
app = "Slack"
```

---

## MIDI Output Actions

### SendMidi

Send MIDI messages to output ports.

**Schema:**
```toml
[action]
type = "SendMidi"
port = "IAC Driver Bus 1"    # MIDI output port name
message_type = "NoteOn"      # Message type (see below)
channel = 0                  # MIDI channel (0-15)
note = 60                    # Note number for Note messages
velocity = 100               # Velocity for Note messages
controller = 1               # CC number for CC messages
value = 127                  # CC value for CC messages
program = 1                  # Program number for ProgramChange
pitch = 0                    # Pitch bend value (-8192 to +8191)
pressure = 64                # Aftertouch pressure
```

**Message Types:**
| Type | Required Fields |
|------|-----------------|
| `NoteOn` | channel, note, velocity |
| `NoteOff` | channel, note, velocity |
| `CC` | channel, controller, value |
| `ProgramChange` | channel, program |
| `PitchBend` | channel, pitch |
| `Aftertouch` | channel, pressure |

**When to use:**
- Controlling other MIDI software
- DAW integration
- Lighting systems via MIDI

**Examples:**
```toml
# Send note to virtual instrument
[action]
type = "SendMidi"
port = "IAC Driver Bus 1"
message_type = "NoteOn"
channel = 0
note = 60
velocity = 100

# Send CC for automation
[action]
type = "SendMidi"
port = "IAC Driver Bus 1"
message_type = "CC"
channel = 0
controller = 1
value = 127
```

**Notes:**
- Port name must match available MIDI output ports
- Use virtual MIDI ports (IAC on macOS) for inter-app communication
- Channel is 0-indexed (channel 0 = MIDI channel 1)
