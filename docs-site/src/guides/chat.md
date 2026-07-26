# Chat Interface

Conductor v4.11+ includes an AI-powered Chat interface that lets you configure MIDI mappings using natural language. Instead of manually editing configuration files, you can describe what you want and the assistant will help create the mappings.

## Overview

The Chat feature uses large language models (LLMs) to understand your intent and translate it into proper Conductor configuration. It supports:

- **Creating mappings**: "Map pad 36 to Command+C"
- **Modifying mappings**: "Change the volume knob to control system volume"
- **Querying configuration**: "What does pad 40 do?"
- **MIDI Learn integration**: "Start learning and assign the next pad I press to play/pause"

## Opening the Chat

1. Launch the Conductor GUI
2. Click the **Chat** icon in the sidebar (or press `Cmd+K` / `Ctrl+K`)
3. The chat panel opens on the right side of the window

## Your First Chat

Try these example prompts to get started:

```
Map note 36 to Command+Space
```

```
Create a mapping for pad 44 that types "Hello World"
```

```
When I press note 60, open Safari
```

## How It Works

1. **You describe** what you want in plain English
2. **The assistant proposes** a configuration change
3. **You review** the proposed changes in a diff view
4. **You approve** and the change is applied to your config

The Chat never modifies your configuration without your explicit approval.

## Plan/Apply Workflow

When you request a configuration change, the assistant creates a **Plan** shown inline in the chat:

- **Description**: What the change will do
- **Changes list**: Each change with type icon (+, ~, -)
- **Diff preview**: Expandable preview showing exactly what will change
- **Expiration**: Plans expire after 5 minutes for security

You can then:
- Click **Apply Changes** to accept the plan
- Click **Cancel** to reject it
- Use the **Cancel Plan** button in the chat header to dismiss at any time

After applying, a confirmation message appears in the chat.

### Edit in Dialog (v4.26.68)

After reviewing a plan, CreateMapping and UpdateMapping changes include an **"Edit"** button that navigates directly to the Mapping Editor with the relevant mode and mapping pre-selected. This lets you fine-tune trigger or action details in the full editor instead of through chat alone.

## Example Conversations

### Creating a Simple Mapping

**You**: Map pad 36 to copy
**Assistant**: I'll create a mapping for note 36 to trigger Command+C (copy).

*[Plan appears inline with changes list and Apply/Cancel buttons]*

**You**: *[Clicks Apply Changes]*

### Creating a Mode-Specific Mapping

**You**: In DJ mode, map the encoder to scroll
**Assistant**: I'll add a mapping in the "DJ" mode for the encoder to trigger scroll actions.

### Creating a New Mode

**You**: Create a "Gaming" mode with green color
**Assistant**: I'll create a new mode called "Gaming" with a green color.

*[Plan appears with changes preview]*

**You**: *[Clicks Apply]*

You can also create a mode and add mappings to it in a single request:

**You**: Create a "DJ" mode and map pad 36 to play/pause
**Assistant**: I'll create the "DJ" mode and add a play/pause mapping for note 36.

### Querying Your Configuration

**You**: What mappings are in Default mode?
**Assistant**: Default mode has 8 mappings:
- Note 36: Keystroke Cmd+C (copy)
- Note 37: Keystroke Cmd+V (paste)
- ...

## Provider Configuration

The Chat feature requires an LLM provider. See [LLM Providers](llm-providers.md) for setup instructions.

## Session Reference

The current session ID is displayed in the conversation history sidebar. You can:
- **View**: See the truncated ID (first 8 characters) in the header
- **Copy**: Click the clipboard icon to copy the full ID
- **Hover**: See the full ID in a tooltip

This is useful for debugging or sharing conversation references with support.

## MIDI Learn Integration (v4.26.55+)

The assistant automatically recommends MIDI Learn when you want to create a mapping but haven't specified the exact note or CC number. The workflow:

1. **You describe the action**: "Map a pad to copy"
2. **The assistant suggests MIDI Learn**: "I can capture the exact control. Press the pad you want to map while I listen."
3. **Capture starts**: The assistant calls `conductor_start_midi_learn`
4. **You press the control**: Press the pad, turn the knob, or pull the trigger
5. **The assistant reads the result**: Captured note/CC details are used to create the mapping

If you already know the exact note or CC number, the assistant will create the mapping directly without MIDI Learn.

## Config Preview (v4.26.62+)

When reviewing a proposed plan, you can inspect your current configuration using the **View Current Config** toggle that appears below the plan details. This expandable section loads your config on demand and provides three views:

- **Raw TOML**: The exact contents of your `config.toml` file
- **JSON**: Your config formatted as pretty-printed JSON
- **Visual**: A tree view of your config structure (modes, mappings, devices)

This helps you understand what will change relative to your current configuration before approving.

## Export Chat (v4.26.58+)

Click the **Copy** button in the chat header to export the entire conversation as structured markdown to your clipboard. The export includes:
- Session ID, provider, and model metadata
- All user, assistant, and system messages
- Tool call names with JSON arguments and results
- Plan proposals with change lists and diffs

The copy formats directly from the current chat messages without requiring a backend round-trip.

## Timestamps

The chat interface shows two types of timestamps:

- **Relative time**: "2h ago", "Yesterday", "Mon" shown by default
- **Full timestamp**: Hover over any time to see the full date and time (e.g., "Feb 4, 2026 3:45 PM")

This applies to both the conversation list and individual messages.

## Tips

- **Be specific**: "Map note 60 to Cmd+Space" works better than "map a pad to something"
- **Use MIDI Learn**: Say "start learning" to capture the exact note/CC from your controller
- **Review carefully**: Always check the diff preview before applying changes
- **Ask questions**: The assistant can explain your current configuration

## Limitations

- Changes require your approval before being applied
- Complex multi-step changes may need multiple prompts
- The assistant cannot directly control your MIDI hardware

## Next Steps

- [LLM Providers](llm-providers.md) - Configure OpenAI or Anthropic
- [MIDI Learn](../getting-started/midi-learn.md) - Capture notes from your controller
- [Troubleshooting Chat](../troubleshooting/chat-issues.md) - Common issues and solutions
