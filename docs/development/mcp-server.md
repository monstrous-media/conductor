# MCP Server Implementation

This document describes the implementation of Conductor's Model Context Protocol (MCP) server, which enables LLM integration.

## Overview

The MCP server provides a JSON-RPC 2.0 interface over Unix domain sockets. It allows LLMs and external clients to query and control the Conductor daemon.

## Server Architecture

```
┌─────────────────────────────────────────────────────────┐
│                    MCP Server                           │
│  ┌─────────────────┐    ┌─────────────────────────┐    │
│  │  Unix Socket    │───▶│  JSON-RPC Handler       │    │
│  │  ~/.conductor/  │    │  (method dispatch)      │    │
│  │  mcp.sock       │◀───│                         │    │
│  └─────────────────┘    └───────────┬─────────────┘    │
│                                     │                   │
│                         ┌───────────▼─────────────┐    │
│                         │  Tool Definitions       │    │
│                         │  (mcp_tools.rs)         │    │
│                         └───────────┬─────────────┘    │
│                                     │                   │
│                         ┌───────────▼─────────────┐    │
│                         │  ToolExecutor           │    │
│                         │  (risk tier handling)   │    │
│                         └─────────────────────────┘    │
└─────────────────────────────────────────────────────────┘
```

## Socket Location

The MCP socket is created at:

```
~/.conductor/mcp.sock
```

On startup, the daemon:
1. Removes any stale socket file
2. Creates the directory if needed
3. Binds to the socket path
4. Sets permissions (owner-only access)

## Protocol

### JSON-RPC 2.0

All communication uses JSON-RPC 2.0:

**Request**:
```json
{
  "jsonrpc": "2.0",
  "method": "conductor_get_status",
  "params": {},
  "id": 1
}
```

**Response**:
```json
{
  "jsonrpc": "2.0",
  "result": {
    "running": true,
    "current_mode": "Default"
  },
  "id": 1
}
```

**Error**:
```json
{
  "jsonrpc": "2.0",
  "error": {
    "code": -32601,
    "message": "Method not found"
  },
  "id": 1
}
```

### Message Framing

Messages are newline-delimited JSON. Each message is a single line terminated by `\n`.

## Implementation Details

### Server Startup

Located in `conductor-daemon/src/daemon/mcp.rs`:

```rust
pub struct McpServer {
    socket_path: PathBuf,
    config_manager: Arc<RwLock<ConfigManager>>,
    // ...
}

impl McpServer {
    pub async fn start(&self) -> Result<(), McpError> {
        // Remove stale socket
        let _ = std::fs::remove_file(&self.socket_path);

        // Bind to socket
        let listener = UnixListener::bind(&self.socket_path)?;

        // Accept connections
        loop {
            let (stream, _) = listener.accept().await?;
            tokio::spawn(self.handle_connection(stream));
        }
    }
}
```

### Tool Definitions

Located in `conductor-daemon/src/daemon/mcp_tools.rs`:

```rust
pub fn get_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "conductor_get_config".to_string(),
            description: "Get the current configuration".to_string(),
            parameters: json!({}),
            risk_tier: ToolRiskTier::ReadOnly,
        },
        // ... more tools
    ]
}
```

### Risk Tier Handling

Located in `conductor-daemon/src/daemon/llm/executor.rs`:

```rust
impl ToolExecutor {
    pub async fn execute(&self, tool_name: &str, params: Value) -> ExecutionResult {
        let risk_tier = get_tool_risk_tier(tool_name);

        match risk_tier {
            ToolRiskTier::ReadOnly => {
                // Execute immediately
                self.execute_tool(tool_name, params).await
            }
            ToolRiskTier::Stateful => {
                // Log and execute
                self.log_execution(tool_name, &params);
                self.execute_tool(tool_name, params).await
            }
            ToolRiskTier::ConfigChange => {
                // Create plan for user approval
                self.create_plan(tool_name, params).await
            }
        }
    }
}
```

## Adding New Tools

### 1. Define the Tool

Add to `mcp_tools.rs`:

```rust
ToolDefinition {
    name: "conductor_my_tool".to_string(),
    description: "Description of what the tool does".to_string(),
    parameters: json!({
        "type": "object",
        "properties": {
            "param1": { "type": "string", "description": "..." }
        },
        "required": ["param1"]
    }),
    risk_tier: ToolRiskTier::ReadOnly,  // Choose appropriate tier
}
```

### 2. Implement the Handler

Add handler in `mcp.rs`:

```rust
async fn handle_my_tool(&self, params: Value) -> Result<Value, McpError> {
    let param1 = params.get("param1")
        .and_then(|v| v.as_str())
        .ok_or(McpError::InvalidParams)?;

    // Implementation
    Ok(json!({ "result": "..." }))
}
```

### 3. Register in Dispatcher

Add to the method dispatcher:

```rust
match method {
    "conductor_my_tool" => self.handle_my_tool(params).await,
    // ...
}
```

## Testing

### Unit Tests

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn test_tool_definitions() {
        let tools = get_tool_definitions();
        assert_eq!(tools.len(), 10);
    }

    #[tokio::test]
    async fn test_get_status() {
        let server = create_test_server();
        let result = server.handle_get_status(json!({})).await;
        assert!(result.is_ok());
    }
}
```

### Integration Tests

```bash
# Test MCP server manually
echo '{"jsonrpc":"2.0","method":"conductor_get_status","params":{},"id":1}' | \
  nc -U ~/.conductor/mcp.sock
```

## Shared Device Enumeration

> **v4.17.0**: MIDI device enumeration is centralized in `conductor-daemon/src/daemon/device_utils.rs`.

All MCP tools, LLM executor, and engine manager use `device_utils::enumerate_midi_devices_fresh()` for consistent device enumeration. This module implements the macOS Core MIDI warmup pattern:

1. Create and immediately drop a warmup `MidiInput` (busts OS driver cache)
2. Sleep 100ms for the OS/driver to recognize hardware changes
3. Create a fresh `MidiInput` and enumerate ports

An async wrapper `enumerate_midi_devices_fresh_async()` is provided for use in async contexts via `tokio::task::spawn_blocking`.

Previously, three separate implementations existed in `mcp.rs`, `llm/executor.rs`, and `engine_manager.rs`, none of which included the warmup step — causing stale device lists on macOS.

## Security Considerations

### Socket Permissions

The socket is created with restrictive permissions:
- Owner read/write only (mode 0600)
- No group or world access

### Input Validation

All parameters are validated before use:
- Type checking
- Range validation
- Path sanitization (for file operations)

### Rate Limiting

Consider implementing rate limiting for production:
- Per-connection limits
- Global request limits
- Timeout for long-running operations

## Error Codes

| Code | Meaning |
|------|---------|
| -32700 | Parse error |
| -32600 | Invalid request |
| -32601 | Method not found |
| -32602 | Invalid params |
| -32603 | Internal error |

Custom error codes (application-specific):
| Code | Meaning |
|------|---------|
| -32000 | Config not found |
| -32001 | Mode not found |
| -32002 | Device not found |
| -32003 | Plan expired |

## See Also

- [LLM Integration Architecture](llm-integration.md)
- [MCP Tools Reference](../reference/mcp-tools.md)
- [Agent Skills](agent-skills.md)
