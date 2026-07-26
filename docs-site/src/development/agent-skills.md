# Agent Skills Development

Agent Skills are reusable capabilities that LLM agents can use when interacting with Conductor. This guide covers how skills work and how to create new ones.

## Overview

Skills provide structured instructions that help LLMs understand how to accomplish specific tasks with Conductor. Each skill includes:

- **Metadata**: Name, description, version
- **Instructions**: Step-by-step guidance for the LLM
- **Tool references**: Which MCP tools to use
- **Examples**: Sample interactions

## Built-in Skills

Conductor includes several bundled skills:

| Skill | Description |
|-------|-------------|
| `create-mapping` | Create a new MIDI/gamepad mapping |
| `midi-learn` | Use MIDI Learn to capture input |
| `configure-mode` | Create or modify modes |
| `device-setup` | Configure MIDI devices |
| `troubleshoot` | Diagnose common issues |

## Skill Structure

Skills are defined in TOML files:

```toml
[metadata]
name = "create-mapping"
version = "1.0.0"
description = "Create a new MIDI mapping"
author = "Conductor"

[instructions]
summary = "Guide the user through creating a MIDI mapping"

steps = [
    "Ask which mode to add the mapping to (or use Default)",
    "Ask what trigger to use (note number, CC, etc.)",
    "Ask what action to perform",
    "Call conductor_create_mapping with the parameters",
    "Present the ConfigPlan for user approval"
]

[tools]
required = ["conductor_create_mapping"]
optional = ["conductor_list_modes", "conductor_start_midi_learn"]

[[examples]]
user = "Map pad 36 to copy"
assistant = """
I'll create a mapping for note 36 to trigger the copy action (Cmd+C).

Let me set that up for you in the Default mode.
"""
tool_call = """
conductor_create_mapping({
  "mode": "Default",
  "trigger": { "type": "Note", "note": 36 },
  "action": { "type": "Keystroke", "keys": ["cmd", "c"] }
})
"""

[[examples]]
user = "In DJ mode, map the encoder to scroll"
assistant = """
I'll create an encoder mapping in DJ mode for scrolling.
"""
```

## Skill Discovery

Skills are loaded from:

1. **Built-in**: `conductor-daemon/skills/` (bundled with daemon)
2. **User**: `~/.conductor/skills/` (custom skills)
3. **Plugins**: `~/.conductor/plugins/*/skills/` (plugin-provided)

The skill loader:
1. Scans all skill directories
2. Parses TOML files
3. Validates structure
4. Makes skills available to LLM context

## Creating a Custom Skill

### 1. Create the Skill File

Create `~/.conductor/skills/my-skill.toml`:

```toml
[metadata]
name = "my-skill"
version = "1.0.0"
description = "My custom skill"
author = "Your Name"

[instructions]
summary = "What this skill does"

steps = [
    "Step 1: ...",
    "Step 2: ...",
]

[tools]
required = ["conductor_get_config"]

[[examples]]
user = "Example user message"
assistant = "Example response"
```

### 2. Validate the Skill

```bash
conductor-skills validate ~/.conductor/skills/my-skill.toml
```

### 3. Test the Skill

```bash
conductor-skills test my-skill "Sample user message"
```

### 4. Use the Skill

The skill is automatically available in the Chat interface.

## Skill Best Practices

### Clear Instructions

- Use imperative language ("Ask the user...", "Call the tool...")
- Be specific about parameters and validation
- Include error handling guidance

### Useful Examples

- Cover common use cases
- Show both simple and complex scenarios
- Include tool call examples

### Appropriate Tools

- Only require tools that are essential
- List optional tools for enhanced functionality
- Prefer ReadOnly tools when possible

### Versioning

- Increment version when making changes
- Use semantic versioning (MAJOR.MINOR.PATCH)
- Document breaking changes

## Skill API

### SkillLoader

```rust
pub struct SkillLoader {
    skill_dirs: Vec<PathBuf>,
    cache: HashMap<String, Skill>,
}

impl SkillLoader {
    pub fn load_all(&mut self) -> Result<Vec<Skill>, SkillError>;
    pub fn get(&self, name: &str) -> Option<&Skill>;
    pub fn reload(&mut self) -> Result<(), SkillError>;
}
```

### Skill Struct

```rust
pub struct Skill {
    pub metadata: SkillMetadata,
    pub instructions: SkillInstructions,
    pub tools: SkillTools,
    pub examples: Vec<SkillExample>,
}

pub struct SkillMetadata {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: Option<String>,
}
```

## CLI Tool

The `conductor-skills` CLI validates and tests skills:

```bash
# List available skills
conductor-skills list

# Validate a skill file
conductor-skills validate path/to/skill.toml

# Test a skill with a sample message
conductor-skills test skill-name "user message"

# Show skill details
conductor-skills show skill-name
```

## Integration with LLM

When the LLM receives a user message:

1. Relevant skills are identified based on intent
2. Skill instructions are injected into the system prompt
3. Tool definitions from required tools are included
4. Examples provide few-shot learning context

Example system prompt injection:

```
You have access to the following skill: create-mapping

Summary: Guide the user through creating a MIDI mapping

Steps:
1. Ask which mode to add the mapping to (or use Default)
2. Ask what trigger to use (note number, CC, etc.)
3. Ask what action to perform
4. Call conductor_create_mapping with the parameters
5. Present the ConfigPlan for user approval

Available tools: conductor_create_mapping, conductor_list_modes
```

## Testing Skills

### Unit Tests

```rust
#[test]
fn test_skill_validation() {
    let skill = Skill::from_file("skills/create-mapping.toml").unwrap();
    assert!(skill.validate().is_ok());
}
```

### Integration Tests

```rust
#[tokio::test]
async fn test_skill_execution() {
    let loader = SkillLoader::new();
    let skill = loader.get("create-mapping").unwrap();

    // Verify required tools exist
    for tool in &skill.tools.required {
        assert!(tool_exists(tool));
    }
}
```

## See Also

- [LLM Integration Architecture](llm-integration.md)
- [MCP Server](mcp-server.md)
- [MCP Tools Reference](../reference/mcp-tools.md)
