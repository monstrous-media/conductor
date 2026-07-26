# Create Your First Mapping with Chat

This tutorial walks you through creating your first MIDI mapping using Conductor's AI-powered Chat interface. You'll learn how to describe what you want in plain English and have the assistant generate the configuration for you.

## Prerequisites

Before starting, make sure you have:

1. Conductor installed and running
2. An LLM provider configured (see [LLM Providers](../guides/llm-providers.md))
3. Your MIDI controller connected

## Step 1: Open the Chat Panel

1. Launch Conductor GUI
2. Click the **Chat** icon in the left sidebar (or press `Cmd+K` / `Ctrl+K`)
3. The chat panel opens on the right side

You should see a welcome message and an input field at the bottom.

## Step 2: Describe Your Mapping

Let's create a simple mapping. In the chat input, type:

```
Map note 36 to Command+C
```

Press Enter to send the message.

## Step 3: Review the Proposed Changes

The assistant will respond with a plan showing:

1. **Summary**: "Create a mapping for note 36 to trigger Cmd+C (copy)"
2. **Changes**: A list showing the new mapping
3. **Preview**: A diff view with the new lines highlighted in green

Take a moment to review the changes. The diff preview shows exactly what will be added to your configuration.

## Step 4: Apply the Changes

If the proposed changes look correct:

1. Click the **Apply Changes** button
2. Wait for confirmation that the changes were applied
3. The assistant will confirm: "Changes applied successfully!"

Your mapping is now active. Press note 36 on your controller to verify it triggers copy.

## Step 5: Test Your Mapping

1. Open a text editor with some text selected
2. Press pad/note 36 on your MIDI controller
3. The text should be copied to your clipboard
4. Press `Cmd+V` to paste and verify it worked

## Creating More Mappings

Now try creating additional mappings. Here are some examples to try:

### Paste Action
```
Map note 37 to Command+V
```

### Open an Application
```
Map note 40 to open Spotify
```

### Type Text
```
Map note 44 to type "Hello World"
```

### Use MIDI Learn

If you don't know the note number:

```
Start MIDI Learn for copy
```

Then press the pad you want to use. The assistant will capture the note and create the mapping.

## Using Different Modes

You can create mode-specific mappings:

```
In Production mode, map note 36 to save
```

```
Create a new mode called "Streaming" and map note 36 to toggle mute
```

## Understanding the Diff Preview

The diff preview uses color coding:
- **Green (+)**: Lines being added
- **Red (-)**: Lines being removed (for updates/deletes)
- **Gray**: Context lines (unchanged)

Always review the diff before clicking Apply.

## Canceling a Change

If the proposed changes aren't what you want:

1. Click **Cancel** to reject the plan
2. Rephrase your request more specifically
3. Try again

Plans automatically expire after 5 minutes if you don't respond.

## Tips for Better Results

1. **Be specific about notes**: "Map note 36" is better than "map a pad"
2. **Use standard action names**: "copy", "paste", "save", "undo" are recognized
3. **Specify the mode**: "In DJ mode, map..." when targeting specific modes
4. **Use MIDI Learn**: Let the assistant capture the exact note from your controller

## Next Steps

- [Chat Interface Guide](../guides/chat.md) - Full Chat documentation
- [MIDI Learn](midi-learn.md) - Capture notes from your controller
- [Understanding Modes](modes.md) - Organize mappings by context
- [LLM Providers](../guides/llm-providers.md) - Configure your AI provider

## Troubleshooting

If you encounter issues:

- **"API key not configured"**: See [LLM Providers](../guides/llm-providers.md)
- **Changes not applying**: Check [Chat Issues](../troubleshooting/chat-issues.md)
- **Wrong note captured**: Use `cancel midi learn` and try again
