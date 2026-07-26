# LLM Integration Architecture

This document describes the architecture of Conductor's LLM integration, introduced in v4.11.0 as part of ADR-007.

## Overview

The LLM integration enables natural language configuration of MIDI mappings. Users can describe what they want in plain English, and an AI assistant translates their intent into proper configuration changes.

```
┌─────────────────┐      ┌──────────────────┐      ┌─────────────────┐
│   Chat UI       │ ───▶ │  LLM Provider    │ ───▶ │  MCP Server     │
│   (Svelte)      │ ◀─── │  (OpenAI/Claude) │ ◀─── │  (Daemon)       │
└─────────────────┘      └──────────────────┘      └─────────────────┘
         │                                                   │
         │                                                   │
         ▼                                                   ▼
┌─────────────────┐                               ┌─────────────────┐
│  Svelte Store   │                               │  ToolExecutor   │
│  (pendingPlan)  │                               │  (Risk Tiers)   │
└─────────────────┘                               └─────────────────┘
└─────────────────┘                                        │
                                                           ▼
                                                ┌─────────────────┐
                                                │  ConfigPlan     │
                                                │  (TOCTOU)       │
                                                └─────────────────┘
```

## Components

### 1. Chat UI (Svelte)

Located in `conductor-gui/ui/src/lib/components/`:

- **ChatPane.svelte**: Main chat interface with message history
- **PlanReviewModal.svelte**: Modal for reviewing and approving ConfigPlans (mounted in ChatView.svelte, controlled via `chatStore.pendingPlan` props)
- **InlineDiff.svelte**: Diff display with red/green highlighting

The Chat UI communicates with the backend via Tauri commands.

### 2. LLM Providers

Located in `conductor-gui/src-tauri/src/llm/`:

- **mod.rs**: Provider traits and types
- **providers/openai.rs**: OpenAI API integration
- **providers/anthropic.rs**: Anthropic API integration
- **keychain.rs**: Secure API key storage

Providers implement the `LLMProvider` trait:

```rust
#[async_trait]
pub trait LLMProvider: Send + Sync {
    fn name(&self) -> &str;
    fn capabilities(&self) -> ProviderCapabilities;
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, LlmError>;
    async fn health_check(&self) -> Result<ProviderStatus, LlmError>;
}
```

### 3. MCP Server

Located in `conductor-daemon/src/daemon/`:

- **mcp.rs**: JSON-RPC 2.0 server over Unix socket
- **mcp_tools.rs**: Tool definitions and implementations

The MCP server exposes tools that LLMs can call:

| Tool | Risk Tier | Description |
|------|-----------|-------------|
| `conductor_get_config` | ReadOnly | Get current configuration |
| `conductor_get_status` | ReadOnly | Get daemon status |
| `conductor_list_modes` | ReadOnly | List configured modes |
| `conductor_get_mappings` | ReadOnly | Get mappings for a mode |
| `conductor_list_devices` | ReadOnly | List MIDI/gamepad devices |
| `conductor_create_mapping` | ConfigChange | Create a new mapping |
| `conductor_update_mapping` | ConfigChange | Update an existing mapping |
| `conductor_delete_mapping` | ConfigChange | Delete a mapping |
| `conductor_start_midi_learn` | Stateful | Start MIDI Learn mode |
| `conductor_stop_midi_learn` | Stateful | Stop MIDI Learn mode |

### 4. ToolExecutor

Located in `conductor-daemon/src/daemon/llm/executor.rs`:

The ToolExecutor handles tool execution based on risk tier:

```rust
pub enum ExecutionResult {
    Success(serde_json::Value),        // ReadOnly: immediate result
    PlanCreated(ConfigPlan),           // ConfigChange: needs approval
    Logged { result: serde_json::Value }, // Stateful: logged execution
    Error(String),
}
```

### 5. ConfigPlan

Located in `conductor-daemon/src/daemon/llm/plan.rs`:

ConfigPlan provides TOCTOU (Time-of-Check to Time-of-Use) protection:

```rust
pub struct ConfigPlan {
    pub id: Uuid,
    pub description: String,
    pub changes: Vec<ConfigChange>,
    pub diff_preview: String,
    pub base_state_hash: String,  // SHA256 of config at creation
    pub expires_at: DateTime<Utc>, // 5-minute TTL
}
```

Before applying a plan:
1. Current config hash is computed
2. Compared against `base_state_hash`
3. If different, plan is rejected
4. If expired, plan is rejected

### 6. Tauri Events

Located in `conductor-gui/src-tauri/src/events.rs`:

Communication for real-time UI updates:

| Mechanism | Description |
|-----------|-------------|
| `chatStore.pendingPlan` | Svelte store field — triggers PlanReviewModal when set (v4.26.42+) |
| `chatStore.applyPendingPlan()` | Invokes `llm_apply_plan` and clears pendingPlan |
| `chatStore.rejectPendingPlan()` | Invokes `llm_reject_plan` and clears pendingPlan |
| `llm:config-updated` | Tauri event — Configuration was updated |
| `llm:midi-learn-state-changed` | Tauri event — MIDI Learn state changed |

> **Note (v4.26.42):** PlanReviewModal was originally designed to use Tauri events (`llm:plan-ready`, `llm:plan-applied`, `llm:plan-rejected`), but Tauri v2's `emit()` from `@tauri-apps/api/event` does not reliably deliver events to frontend `listen()` handlers. The modal now uses Svelte store props (same pattern as MidiLearnDialog).

## Data Flow

### Creating a Mapping

1. User types "Map note 36 to copy"
2. Chat UI sends message to LLM provider
3. LLM calls `conductor_create_mapping` MCP tool
4. ToolExecutor creates ConfigPlan (not applied yet)
5. `chatStore.setPendingPlan(plan)` — sets `pendingPlan` in Svelte store
6. ChatView passes `pendingPlan` as props to PlanReviewModal
7. PlanReviewModal shows diff preview
8. User clicks "Apply"
9. `chatStore.applyPendingPlan()` calls `llm_apply_plan` Tauri command
10. ConfigPlan validated (hash, expiration)
11. Config file updated, pendingPlan cleared
12. Config hot-reloaded by daemon

### MIDI Learn Flow (v4.26.38+)

1. User says "Start MIDI Learn"
2. LLM calls `conductor_start_midi_learn`
3. MidiLearnDialog opens automatically in the GUI
4. User presses pad/button/encoder on controller
5. Dialog shows captured events with pattern detection (chord, long press, double tap)
6. User clicks **"Use this"** to select the captured trigger
7. Trigger data sent as a user message back to the LLM (via `chatStore.sendMessage`)
8. LLM sees the trigger JSON and continues the conversation (e.g., asks what action to assign)
9. LLM creates mapping with captured values

If the user **cancels** the dialog, a cancellation message is sent to the LLM so it can offer alternatives.

## Security Model

### API Key Storage

API keys are stored in the system keychain, never in plain text:

- macOS: Keychain Access
- Windows: Windows Credential Manager
- Linux: Secret Service (GNOME Keyring)

### Tool Risk Tiers

The risk tier system provides defense-in-depth:

1. **ReadOnly**: No side effects, safe to auto-execute
2. **Stateful**: Affects runtime state, logged for auditing
3. **ConfigChange**: Modifies files, requires user approval
4. **HardwareIO**: Controls hardware (reserved for future)
5. **Privileged**: System-level operations (reserved)

### TOCTOU Protection

ConfigPlans prevent race conditions:

1. Hash of config captured when plan created
2. Hash verified before applying
3. Plans expire after 5 minutes
4. Prevents accidental overwrites from concurrent edits

## Error Handling

### LLM Errors

```rust
pub enum LlmError {
    ApiKeyNotFound(String),
    AuthenticationFailed(String),
    RateLimitExceeded(String),
    ContextLengthExceeded { max: usize, actual: usize },
    Network(String),
    Timeout(u64),
    ContentFiltered(String),
    // ...
}
```

### Plan Errors

```rust
pub enum PlanError {
    NotFound(Uuid),
    Expired(Uuid),
    ConfigChanged { expected: String, actual: String },
    ApplyFailed(String),
}
```

## Testing

Unit tests are in each module:

```bash
# Run all LLM integration tests
cargo test --package conductor-daemon llm
cargo test --package conductor-gui llm
```

Key test files:
- `conductor-daemon/src/daemon/llm/plan.rs` - ConfigPlan tests
- `conductor-daemon/src/daemon/llm/executor.rs` - ToolExecutor tests
- `conductor-daemon/src/daemon/mcp_tools.rs` - Tool definition tests

## See Also

- [ADR-007: LLM Integration Architecture](../../docs/adrs/ADR-007-llm-integration-architecture.md)
- [MCP Server](mcp-server.md) - Server implementation details
- [Agent Skills](agent-skills.md) - Skill development guide
- [MCP Tools Reference](../reference/mcp-tools.md) - Tool documentation
