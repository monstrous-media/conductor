# Modes System

Modes allow you to organize your mappings into switchable groups. Each mode contains its own set of mappings, so you can have different button configurations for different tasks.

## Defining Modes

Modes are defined in `config.toml` as `[[modes]]` sections:

```toml
[[modes]]
name = "Default"
color = "blue"

[[modes.mappings]]
trigger = { type = "Note", note = 36 }
action = { type = "Keystroke", keys = ["cmd", "c"] }

[[modes]]
name = "Music"
color = "purple"

[[modes.mappings]]
trigger = { type = "Note", note = 36 }
action = { type = "Keystroke", keys = ["space"] }
```

## Switching Modes

You can switch modes in several ways:

- **ModeChange action**: Map a trigger to switch modes
- **GUI mode selector**: Use the dropdown in the Mapping Editor
- **MCP tool**: LLM can switch modes via `conductor_switch_mode` (v4.26.69)
- **conductorctl**: Programmatic mode switching via IPC

### ModeChange Action

```toml
[[modes.mappings]]
trigger = { type = "Note", note = 48 }
action = { type = "ModeChange", mode = "Music" }
```

## Default Mode (v4.26.70)

You can designate a mode as the startup default. The daemon will start in this mode instead of the first mode (index 0).

```toml
default_mode = "Music"
```

### Setting Default Mode in the GUI

1. Open the **Mapping Editor** view
2. Select the mode you want as default from the dropdown
3. Click the **"Set as Default"** button next to the mode selector
4. The mode dropdown will show a **(Default)** badge next to the default mode name

## Moving Mappings Between Modes (v4.26.71)

The mapping editor dialog includes a **Target Mode** dropdown that lets you:

- **Move existing mappings** to a different mode (delete from source, add to target)
- **Create new mappings** directly in a non-current mode

When editing an existing mapping and changing the target mode, a warning message confirms the mapping will be moved.

## Global Mappings

Global mappings work in all modes. Define them under `[[global_mappings]]`:

```toml
[[global_mappings]]
trigger = { type = "Note", note = 60 }
action = { type = "VolumeControl", operation = "Mute" }
```

Toggle between global and mode-specific mappings using the checkbox in the Mapping Editor.

## See Also

- [Configuration Overview](overview.md) - Full config file reference
- [Triggers Reference](triggers.md) - All trigger types
- [Actions Reference](actions.md) - All action types
- [MCP Tools](../reference/mcp-tools.md) - `conductor_switch_mode` tool

---

**Last Updated**: February 12, 2026 (v4.26.71)
