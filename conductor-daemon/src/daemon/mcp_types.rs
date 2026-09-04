// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! MCP (Model Context Protocol) types for LLM integration (ADR-007)
//!
//! Implements the MCP specification for tool exposure to external LLM agents.
//! See: https://spec.modelcontextprotocol.io/

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// MCP JSON-RPC request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpRequest {
    pub jsonrpc: String,
    pub id: Option<McpRequestId>,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

/// Request ID can be string or number
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum McpRequestId {
    String(String),
    Number(i64),
}

/// MCP JSON-RPC response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<McpRequestId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<McpError>,
}

impl McpResponse {
    /// Create a success response
    pub fn success(id: Option<McpRequestId>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Create an error response
    pub fn error(id: Option<McpRequestId>, error: McpError) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

/// MCP error object
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl McpError {
    /// Standard JSON-RPC error codes
    pub const PARSE_ERROR: i32 = -32700;
    pub const INVALID_REQUEST: i32 = -32600;
    pub const METHOD_NOT_FOUND: i32 = -32601;
    pub const INVALID_PARAMS: i32 = -32602;
    pub const INTERNAL_ERROR: i32 = -32603;

    pub fn parse_error(message: &str) -> Self {
        Self {
            code: Self::PARSE_ERROR,
            message: format!("Parse error: {}", message),
            data: None,
        }
    }

    pub fn invalid_request(message: &str) -> Self {
        Self {
            code: Self::INVALID_REQUEST,
            message: format!("Invalid request: {}", message),
            data: None,
        }
    }

    pub fn method_not_found(method: &str) -> Self {
        Self {
            code: Self::METHOD_NOT_FOUND,
            message: format!("Method not found: {}", method),
            data: None,
        }
    }

    pub fn invalid_params(message: &str) -> Self {
        Self {
            code: Self::INVALID_PARAMS,
            message: format!("Invalid params: {}", message),
            data: None,
        }
    }

    pub fn internal_error(message: &str) -> Self {
        Self {
            code: Self::INTERNAL_ERROR,
            message: format!("Internal error: {}", message),
            data: None,
        }
    }
}

/// MCP Server capabilities
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ServerCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolsCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourcesCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompts: Option<PromptsCapability>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolsCapability {
    #[serde(default)]
    pub list_changed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ResourcesCapability {
    #[serde(default)]
    pub subscribe: bool,
    #[serde(default)]
    pub list_changed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PromptsCapability {
    #[serde(default)]
    pub list_changed: bool,
}

/// MCP Server info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

/// Initialize result
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub protocol_version: String,
    pub capabilities: ServerCapabilities,
    pub server_info: ServerInfo,
}

/// Tool definition
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Tools list result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolsListResult {
    pub tools: Vec<ToolDefinition>,
}

/// Tool call request params
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallParams {
    pub name: String,
    #[serde(default)]
    pub arguments: Option<Value>,
}

/// Tool call result
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallResult {
    pub content: Vec<ToolContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

/// Tool content item
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ToolContent {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image { data: String, mime_type: String },
    #[serde(rename = "resource")]
    Resource {
        uri: String,
        mime_type: Option<String>,
        text: Option<String>,
    },
}

impl ToolContent {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    pub fn json(value: &Value) -> Self {
        Self::Text {
            text: serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_string()),
        }
    }
}

impl ToolCallResult {
    pub fn success(content: Vec<ToolContent>) -> Self {
        Self {
            content,
            is_error: None,
        }
    }

    pub fn error(message: &str) -> Self {
        Self {
            content: vec![ToolContent::text(message)],
            is_error: Some(true),
        }
    }

    pub fn text(text: impl Into<String>) -> Self {
        Self::success(vec![ToolContent::text(text)])
    }

    pub fn json(value: &Value) -> Self {
        Self::success(vec![ToolContent::json(value)])
    }
}

/// Tool risk tier (ADR-007).
///
/// Re-exported from [`conductor_core::security::RiskTier`] (ADR-027 D5,
/// 2026-05-03). The canonical home is `conductor-core` so the GUI's Tauri
/// command surface and the upcoming D5 gate can both reference the same
/// enum without reaching across the crate split. This alias preserves the
/// `ToolRiskTier` name for the ~7 daemon files that already import it; new
/// code should prefer `conductor_core::security::RiskTier` directly.
pub use conductor_core::security::RiskTier as ToolRiskTier;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mcp_request_parsing() {
        let json = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let request: McpRequest = serde_json::from_str(json).unwrap();
        assert_eq!(request.method, "initialize");
        assert!(matches!(request.id, Some(McpRequestId::Number(1))));
    }

    #[test]
    fn test_mcp_request_string_id() {
        let json = r#"{"jsonrpc":"2.0","id":"abc-123","method":"tools/list"}"#;
        let request: McpRequest = serde_json::from_str(json).unwrap();
        assert!(matches!(request.id, Some(McpRequestId::String(_))));
    }

    #[test]
    fn test_mcp_response_success() {
        let response = McpResponse::success(
            Some(McpRequestId::Number(1)),
            serde_json::json!({"status": "ok"}),
        );
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("result"));
        assert!(!json.contains("error"));
    }

    #[test]
    fn test_mcp_response_error() {
        let response = McpResponse::error(
            Some(McpRequestId::Number(1)),
            McpError::method_not_found("unknown"),
        );
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("error"));
        assert!(!json.contains("result"));
    }

    #[test]
    fn test_tool_content_text() {
        let content = ToolContent::text("Hello, world!");
        let json = serde_json::to_string(&content).unwrap();
        assert!(json.contains("text"));
        assert!(json.contains("Hello, world!"));
    }

    #[test]
    fn test_tool_call_result_json() {
        let result = ToolCallResult::json(&serde_json::json!({"key": "value"}));
        assert!(result.is_error.is_none());
        assert_eq!(result.content.len(), 1);
    }
}
