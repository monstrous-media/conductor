# Agent Skills Development

Agent Skills are self-contained Markdown documents that teach an LLM agent
(Claude Code, Claude Desktop, or any other Skills-aware client) how to
operate Conductor for a specific job — creating MIDI mappings, walking a
user through Learn mode, routing signals between endpoints, and so on. This
guide covers the real, on-disk skill format, how `conductor-daemon` validates
and installs skills, and how a skill actually reaches an LLM.

## Overview

A skill is a **directory**, not a config file. The daemon's `skills` module
(`conductor-daemon/src/skills/`) implements the [agentskills.io][agentskills]
progressive-disclosure convention:

```text
skill-name/
├── SKILL.md              # Required: YAML frontmatter + Markdown instructions
└── references/           # Optional: deeper reference docs, linked lazily
    ├── TRIGGERS.md
    └── ACTIONS.md
```

- **`SKILL.md`'s frontmatter** (Level 1) is small and cheap — an agent can
  read every installed skill's frontmatter to decide which skill is relevant
  before loading anything else.
- **`SKILL.md`'s body** (Level 2) is the actual instructions, capped at
  ~6000 estimated tokens by the validator.
- **`references/*.md`** (Level 3) are pulled in only when the body links to
  them (`[TRIGGERS.md](references/TRIGGERS.md)`) — the agent decides whether
  it needs that detail.

[agentskills]: https://agentskills.io

## Frontmatter fields

`SkillMetadata` (`conductor-daemon/src/skills/validator.rs`) defines what the
YAML frontmatter block can contain:

| Field | Required | Notes |
|-------|----------|-------|
| `name` | Yes | Must exactly match the skill's directory name, or validation fails with a `NameMismatch` error. |
| `description` | Yes | What the skill does and when an agent should reach for it. |
| `license` | No (defaults to `Apache-2.0`) | Must be one of: `MIT`, `Apache-2.0`, `GPL-3.0`, `BSD-3-Clause`, `BSD-2-Clause`, `ISC`, `MPL-2.0`, `LGPL-3.0`, `Unlicense`, `CC0-1.0`. |
| `compatibility` | No | Free-text note on runtime requirements (e.g. "Requires Conductor daemon running with MCP server enabled"). |
| `allowed-tools` | No | Restricts which tools the skill may invoke — see below. |
| `trust-level` | No (defaults to `user`) | `bundled`, `user`, or `remote`; consumed by the (currently unwired — see below) sandbox library. |
| `metadata` | No | Free-form string map for anything else (`author`, `version`, `category`, ...). There is no dedicated top-level `version` field — the shipped skills put it under `metadata.version`. |

### `allowed-tools` syntax

The validator (`validate_tool_patterns`) accepts **two grammars**, chosen
structurally by whether the value contains a comma:

- **Legacy `namespace:pattern, …`** (comma-separated) — e.g.
  `"conductor:get_*, conductor:list_*"`. Each pattern is
  `<namespace>:<alphanumeric-and-underscore-pattern>`, with an optional
  trailing `*` wildcard; a bare `*` means unrestricted.
- **Claude Code space-separated** (no comma) — e.g. `Bash(conductor:*) Read
  Write` or `Read mcp:llm-council/verify`. Tokens are bare identifiers
  (`Bash`, `Read`), `Name(args)` permission scopes, or `mcp:<server>[/<tool>]`
  references. **Every skill shipped in this repo uses this grammar.**

Both grammars share the global `*` wildcard. A malformed pattern in either
grammar fails validation with `InvalidToolPattern`.

## The shipped skills

Five skills ship under [`skills/`](../../skills) at the repo root, all using
`allowed-tools: Bash(conductor:*) Read Write`:

| Skill | Description |
|-------|-------------|
| `conductor-binding-setup` | Set up and configure MIDI/HID device bindings; troubleshoot connections; migrate legacy `[device]` config to `[[bindings]]`. |
| `conductor-learn` | Guide a user through Learn mode to capture controller inputs (MIDI or HID) and create mappings without knowing note/button/CC numbers up front. |
| `conductor-midi-mapping` | Create and manage MIDI controller mappings — triggers (`Note`, `VelocityRange`, `LongPress`, `DoubleTap`, `NoteChord`, `EncoderTurn`, `CC`) and actions (`Keystroke`, `Launch`, `Shell`, `SendMidi`, `ModeChange`, `Sequence`). |
| `conductor-signal-routing` | Route, forward, split, or merge signals between `[[endpoints]]` via `[[routes]]` — steady-state signal flow rather than per-event mappings. |
| `conductor-troubleshooting` | Diagnose common issues: port detection failures, events not triggering, wrong device routing, LED feedback problems. |

Read [`skills/conductor-midi-mapping/SKILL.md`](../../skills/conductor-midi-mapping/SKILL.md)
for a complete real example — it documents a decision framework, a cookbook
of `let_through` recipes, common pitfalls, and error-recovery guidance, all
in plain Markdown with no special syntax beyond the YAML frontmatter.

## The `conductor-skills` CLI

`conductor-skills` (`conductor-daemon/src/bin/conductor_skills.rs`) has
exactly three subcommands — there is no `test` or `show`:

| Command | Args | Description |
|---------|------|--------------|
| `validate` | `<PATH>` `[-v, --verbose]` | Validates one skill (if `<PATH>/SKILL.md` exists) or every skill directory under `<PATH>`. `--verbose` prints description, license, estimated body tokens, and reference files. Exits non-zero if any skill fails. |
| `list` | `[-p, --path <DIR>]` `[-v, --verbose]` | Lists installed skills (default: `~/.conductor/skills`). |
| `install` | `<SOURCE>` `[-t, --target <DIR>]` | Validates `<SOURCE>`, then copies it into the target skills directory (default: `~/.conductor/skills`). Fails if a skill of that name is already installed there — it does not overwrite. |

```bash
# Validate one skill
conductor-skills validate ./skills/conductor-midi-mapping

# Validate every skill in a directory
conductor-skills validate ./skills --verbose

# List what's installed
conductor-skills list

# Install a skill you've written
conductor-skills install ./my-skill
```

## What validation actually checks

`validate_skill()` (`conductor-daemon/src/skills/validator.rs`) runs, in
order:

1. `SKILL.md` exists in the directory.
2. The file starts with a `---`-delimited YAML frontmatter block.
3. The frontmatter parses and has non-empty `name` and `description`.
4. `allowed-tools`, if present, matches one of the two grammars above.
5. `name` in the frontmatter equals the directory's basename.
6. `license` is a recognized SPDX identifier from the fixed list above.
7. The body's estimated token count (`chars × 0.25`, rounded up) is ≤ 6000.
8. Every `[text](references/FILE.md)`-style link in the body resolves to a
   file that actually exists under `references/`.

Any failure is collected (not short-circuited on the first one, except for a
missing `SKILL.md` or unparseable frontmatter) and returned as a
`Vec<SkillValidationError>`.

## How a skill actually reaches an LLM

There is **no MCP mechanism that serves skill content**. The daemon's MCP
server declares its capabilities at handshake
(`conductor-daemon/src/daemon/mcp/mod.rs`, `handle_initialize`) as
`tools: Some(...)`, `resources: None`, `prompts: None` — it implements only
the MCP *tools* primitive, not *resources* or *prompts*, so there is no
protocol-level channel for it to hand skill files to a client even in
principle. Its tool set (see [MCP Tools Reference](../reference/mcp-tools.md))
is entirely about controlling Conductor (mappings, modes, devices, config),
not about distributing skills. The skills pipeline works like this instead:

1. A skill is just files on disk — under this repo's `skills/` directory, or
   installed to `~/.conductor/skills/<name>/` via `conductor-skills install`.
2. **The LLM client** (Claude Code, Claude Desktop, or any other
   Skills-compatible agent) discovers and loads `SKILL.md` files directly
   from wherever it's configured to look — the same mechanism that client
   uses for any other Agent Skill, with no Conductor-specific transport in
   between.
3. Once loaded, the skill's Markdown instructions steer the LLM to call
   Conductor's regular MCP tools (`conductor_create_mapping`,
   `conductor_get_config`, `conductor_start_midi_learn`, etc.) — the
   `allowed-tools` frontmatter is a hint to that client about which tools
   the skill is expected to need, expressed in *that client's* permission
   syntax (hence the shipped skills using Claude Code's `Bash(...)`/`Read`
   grammar rather than the legacy `namespace:pattern` one).
4. Those MCP tool calls are gated by the daemon's own audit-tier system
   (`ReadOnly` / `Stateful` / `ConfigChange` / `HardwareIO` — ADR-027), which
   is independent of anything in a skill's frontmatter.

`conductor_daemon::skills` also exports a `sandbox` module
(`SkillSandbox`, `ToolPattern`, `SandboxConfig`) implementing tool-access
matching against `allowed-tools` and trust-level rules. It's a real,
unit-tested library — but as of this writing it isn't called from anywhere
in the daemon's MCP request path or from `conductor-skills`; it exists as a
building block for a host application that wants to enforce a skill's
declared tool restrictions itself, not as an active runtime gate today.

## Writing a new skill

1. Create the directory and `SKILL.md`:

   ```bash
   mkdir -p my-skill/references
   ```

   ```markdown
   ---
   name: my-skill
   description: >
     One or two sentences on what this skill does and when an agent
     should use it.
   license: MIT
   compatibility: Requires Conductor daemon running
   metadata:
     author: Your Name
     version: "0.1.0"
   allowed-tools: Bash(conductor:*) Read Write
   ---

   # My Skill

   Instructions for the LLM go here, in plain Markdown. Be specific about
   what tools to call and in what order. Link to `references/*.md` for
   detail the agent only needs occasionally.
   ```

2. Validate it:

   ```bash
   conductor-skills validate ./my-skill --verbose
   ```

3. Install it for local use:

   ```bash
   conductor-skills install ./my-skill
   ```

4. Point your LLM client at the install location (or this repo's `skills/`
   directory) the same way you would for any other Agent Skill — Conductor
   does not have a separate registration step.

### Practical guidance

- **Match the directory name exactly.** A `name` mismatch is one of the most
  common validation failures.
- **Keep the body under the token budget.** If a skill is growing past
  ~6000 estimated tokens, move detail into `references/*.md` and link to it
  — that's what progressive disclosure is for.
- **Write `allowed-tools` in your target client's grammar.** All five
  shipped skills use Claude Code's space-separated form; use the legacy
  `namespace:pattern` form only if you have a specific reason to.
- **Be concrete about Conductor's domain model.** Look at
  `conductor-midi-mapping/SKILL.md` for the level of specificity that
  works well — decision frameworks, a pitfalls table, and worked examples
  beat abstract advice.

## See Also

- [LLM Integration Architecture](llm-integration.md)
- [MCP Server](mcp-server.md)
- [MCP Tools Reference](../reference/mcp-tools.md)
- [CLI Commands Reference](../reference/cli-commands.md)
