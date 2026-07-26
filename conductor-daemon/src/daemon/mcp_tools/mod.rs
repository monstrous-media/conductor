// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! MCP Tool definitions for Conductor (ADR-007 Phase 1B)
//!
//! This module defines the ReadOnly tools exposed via MCP:
//! - conductor_get_status: Daemon status and lifecycle state
//! - conductor_list_devices: Available MIDI and HID devices
//! - conductor_get_config: Current configuration
//! - conductor_list_mappings: Mappings by mode
//! - conductor_get_mapping: Single mapping by mode/index

//!
//! #2601: decomposed into a directory module so each unit fits a code-review
//! window: `definitions_readonly` / `definitions_write` (catalog),
//! `executor` / `executor_queries` (`McpToolExecutor`), `tests`.

use super::mcp_types::{ToolCallResult, ToolDefinition, ToolRiskTier};

mod definitions_readonly;
// ADR-045 D2 (#2492): write-tier definitions are advertised on the MCP
// socket only under `mcp-write` (never in official artifacts, D3); the
// write-tier RISK CLASSIFICATION compiles under `llm-executor` because the
// IPC plan/apply path needs correct tiers even with a read-only socket.
#[cfg(feature = "mcp-write")]
mod definitions_write;
mod executor;
mod executor_queries;
#[cfg(test)]
mod tests;
#[cfg(feature = "llm-executor")]
mod write_tiers;

pub use executor::McpToolExecutor;

/// Error returned when a GUI-only tool reaches the daemon (ADR-023 boundary).
pub const GUI_ONLY_TOOL_ERROR: &str = "This tool is managed by the GUI and should not reach the daemon. Ensure the GUI frontend is intercepting this tool call locally.";

/// Get all tool definitions for MCP.
///
/// ADR-045 D2 (#2492): the base catalog holds only ReadOnly inspection
/// tools; write-tier tools are appended only under `mcp-write`, so
/// `tools/list` always reflects exactly what is compiled in. Catalog order:
/// ReadOnly first (regrouped by the #2601 split; no client contract).
pub fn get_tool_definitions() -> Vec<ToolDefinition> {
    #[allow(unused_mut)]
    let mut tools = definitions_readonly::readonly_tool_definitions();
    #[cfg(feature = "mcp-write")]
    tools.extend(definitions_write::write_tier_tool_definitions());
    tools
}

/// Get the risk tier for a tool.
///
/// The risk-tier taxonomy is a security classification and is untouched by
/// the ADR-045 tier split (Council R1 #1): write-tier arms live in
/// `write_tiers` purely because those tools only compile in with the write
/// machinery. Unknown tools stay fail-closed (Privileged).
pub fn get_tool_risk_tier(tool_name: &str) -> ToolRiskTier {
    #[cfg(feature = "llm-executor")]
    if let Some(tier) = write_tiers::write_tool_risk_tier(tool_name) {
        return tier;
    }
    match tool_name {
        // ReadOnly tools (Phase 1B)
        "conductor_get_status" => ToolRiskTier::ReadOnly,
        "conductor_list_devices" => ToolRiskTier::ReadOnly,
        "conductor_get_config" => ToolRiskTier::ReadOnly,
        "conductor_list_mappings" => ToolRiskTier::ReadOnly,
        "conductor_get_mapping" => ToolRiskTier::ReadOnly,
        "conductor_validate_config" => ToolRiskTier::ReadOnly,
        "conductor_get_topology_summary" => ToolRiskTier::ReadOnly,
        "conductor_list_routes" => ToolRiskTier::ReadOnly, // ADR-031 P3 slice 3
        "conductor_get_routing_graph" => ToolRiskTier::ReadOnly, // ADR-031 P3 slice 16 (gap A)
        "conductor_get_resolved_routing_graph" => ToolRiskTier::ReadOnly, // ADR-031 #1598 Phase 1
        "conductor_explain_route_match" => ToolRiskTier::ReadOnly, // ADR-036 D5 / Slice 9 #1667
        "conductor_get_dispatch_trace" => ToolRiskTier::ReadOnly, // ADR-036 §8 / Slice 9 #1667
        "conductor_get_connector_metrics" => ToolRiskTier::ReadOnly, // ADR-031 P4 slice 4 (gap D)

        // Stateful tools (Phase 2) - execute with logging
        "conductor_mode_status" => ToolRiskTier::ReadOnly, // ADR-040 4c
        "conductor_get_active_profile" => ToolRiskTier::ReadOnly, // Phase 1 - Issue #323

        // HardwareIO tools (Phase 4) - require multi-step confirmation
        "conductor_get_device_identity" => ToolRiskTier::ReadOnly, // ADR-026 Phase 2
        "conductor_list_device_identities" => ToolRiskTier::ReadOnly, // ADR-026 Phase 2
        "conductor_suggest_binding" => ToolRiskTier::ReadOnly,     // ADR-022 Phase 5D

        // Discovery + workspace + health tools
        "conductor_list_discovered_ports" => ToolRiskTier::ReadOnly,
        "conductor_get_binding_health" => ToolRiskTier::ReadOnly,
        "conductor_get_workspace_state" => ToolRiskTier::ReadOnly,

        // Multi-device tools (v4.23.0 - ADR-009 Phase 5)
        "conductor_list_device_bindings" => ToolRiskTier::ReadOnly,

        // GUI-only profile tools (frontend-intercepted; should not reach daemon).
        // ReadOnly for list, Stateful for mutating ops. All have fallback handlers
        // in both McpToolExecutor and ToolExecutor.
        "conductor_list_profiles" => ToolRiskTier::ReadOnly,
        "conductor_create_profile" => ToolRiskTier::Stateful,
        "conductor_delete_profile" => ToolRiskTier::Stateful,

        // Plugin management (Issue #328)
        "conductor_list_plugins" => ToolRiskTier::ReadOnly,
        "conductor_plugin_info" => ToolRiskTier::ReadOnly,

        // Physical control state (ADR-025 Phase 1)
        "conductor_get_control_state" => ToolRiskTier::ReadOnly,
        "conductor_get_active_pc" => ToolRiskTier::ReadOnly,

        // Network-listener security (ADR-042 Phase B-early, #1899 B.7)
        "conductor_security_status" => ToolRiskTier::ReadOnly,

        // Unknown tools default to most restrictive tier (fail-closed)
        _ => {
            tracing::warn!(
                "Unknown tool '{}' — defaulting to Privileged tier",
                tool_name
            );
            ToolRiskTier::Privileged
        }
    }
}

/// True iff `tool_name` is present in this build's compiled tool catalog.
///
/// ADR-045 D2 (#2492): used by the MCP server to reject calls to tools that
/// exist in richer compositions without embedding their names in this binary
/// (the caller passes the name through). Built once — `get_tool_definitions`
/// allocates the full catalog (JSON schemas included), far too heavy per
/// tools/call (Copilot review on PR #2600); O(1) lookups thereafter.
pub fn is_compiled_tool(tool_name: &str) -> bool {
    use std::collections::HashSet;
    use std::sync::LazyLock;
    static COMPILED_TOOL_NAMES: LazyLock<HashSet<String>> =
        LazyLock::new(|| get_tool_definitions().into_iter().map(|d| d.name).collect());
    COMPILED_TOOL_NAMES.contains(tool_name)
}

/// ADR-045 D2 (#2492): standard error for tool calls absent from this
/// build composition. Names the Studio tier per the ADR.
pub fn tool_unavailable_error(tool_name: &str) -> ToolCallResult {
    ToolCallResult::error(&format!(
        "Tool '{tool_name}' is not available in this build. Write-capable MCP \
         tools ship with Conductor Studio; source builds can enable the \
         `mcp-write` cargo feature."
    ))
}
