// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! ADR-045 D1/D2 (#2492) — MCP tool-registry tier split.
//!
//! The free `mcp` surface advertises ONLY ReadOnly inspection tools
//! (capability rule, ADR-045 D2): a tool is free iff it only inspects
//! daemon state and does not exist solely to serve a paid feature.
//! Stateful / ConfigChange / HardwareIO tools (and the ArtifactRender
//! GUI-chat tools, via clause b) compile in only under `mcp-write`.
//!
//! These tests run in every composition that has an MCP tool catalog;
//! the assertions flip on `mcp-write`.

#![cfg(any(feature = "mcp", feature = "llm-executor"))]

use conductor_daemon::daemon::mcp_tools::{
    get_tool_definitions, get_tool_risk_tier, is_compiled_tool,
};
use conductor_daemon::daemon::mcp_types::ToolRiskTier;

/// Tools that must NEVER be advertised outside an `mcp-write` build.
/// (Names live in test code only — the OSS release binary must not
/// contain them. The ADR-045 D3 negative BINARY assertions that grep the
/// release artifact for these names land in CI with story A3, #2494.)
const GATED_TOOLS: &[&str] = &[
    // ConfigChange
    "conductor_create_endpoint",
    "conductor_create_mapping",
    "conductor_update_mapping",
    "conductor_delete_mapping",
    "conductor_batch_changes",
    "conductor_set_context_mapping",
    // Stateful — MIDI Learn (paid feature, D2 clause b)
    "conductor_start_learn",
    "conductor_start_midi_learn",
    "conductor_stop_learn",
    "conductor_stop_midi_learn",
    "conductor_set_mapping_editor",
    "conductor_update_mapping_editor",
    // Stateful — mode / profile / device / plugin control
    "conductor_switch_mode",
    "conductor_set_mode",
    "conductor_unlock_mode",
    "conductor_switch_profile",
    "conductor_set_device_enabled",
    "conductor_scan_ports",
    "conductor_enable_plugin",
    "conductor_disable_plugin",
    "conductor_reset_control_state",
    // ArtifactRender (GUI chat surface — clause b)
    "conductor_render_artifact",
    "conductor_dismiss_artifact",
    // HardwareIO
    "conductor_send_midi",
    "conductor_send_sysex",
    "conductor_device_reset",
    "conductor_probe_device_identity",
];

/// Write-tier tools that are risk-classified but NEVER advertised in any
/// composition: GUI-only profile tools, frontend-intercepted (pre-ADR-045
/// behaviour, unchanged by the split).
const GATED_UNADVERTISED: &[&str] = &["conductor_create_profile", "conductor_delete_profile"];

/// A sample of ReadOnly inspection tools that must be present in EVERY
/// composition that compiles the catalog (the free MCP value story).
const FREE_TOOLS: &[&str] = &[
    "conductor_get_status",
    "conductor_list_devices",
    "conductor_get_config",
    "conductor_list_mappings",
    "conductor_validate_config",
    "conductor_get_routing_graph",
    "conductor_mode_status",
    "conductor_get_control_state",
    "conductor_security_status",
];

#[test]
fn free_inspection_tools_always_advertised() {
    let names: Vec<String> = get_tool_definitions().into_iter().map(|d| d.name).collect();
    for free in FREE_TOOLS {
        assert!(
            names.iter().any(|n| n == free),
            "ReadOnly inspection tool {free} missing from tools/list"
        );
    }
}

#[cfg(not(feature = "mcp-write"))]
mod without_mcp_write {
    use super::*;
    use conductor_daemon::daemon::mcp_tools::tool_unavailable_error;

    /// D2: every advertised tool must be ReadOnly-tier — nothing that can
    /// mutate state, config, or hardware is visible on the free surface.
    #[test]
    fn advertised_tools_are_readonly_only() {
        for def in get_tool_definitions() {
            let tier = get_tool_risk_tier(&def.name);
            assert_eq!(
                tier,
                ToolRiskTier::ReadOnly,
                "tool `{}` advertised in a non-mcp-write build with tier {tier:?}",
                def.name
            );
        }
    }

    /// D2: no gated tool name appears in tools/list.
    #[test]
    fn gated_tools_not_advertised() {
        let names: Vec<String> = get_tool_definitions().into_iter().map(|d| d.name).collect();
        for gated in GATED_TOOLS.iter().chain(GATED_UNADVERTISED) {
            assert!(
                !names.iter().any(|n| n == gated),
                "gated tool `{gated}` advertised in a non-mcp-write build"
            );
        }
        assert!(!names.iter().any(|n| GATED_TOOLS.contains(&n.as_str())));
    }

    /// D1: gated tools are not compiled in at all.
    #[test]
    fn gated_tools_not_compiled() {
        for gated in GATED_TOOLS {
            assert!(
                !is_compiled_tool(gated),
                "gated tool `{gated}` reported as compiled in a non-mcp-write build"
            );
        }
    }

    /// D2: the standard error for absent tools names the Studio tier and
    /// says "not available in this build" — without embedding gated tool
    /// names in the binary (the caller passes the name through).
    #[test]
    fn unavailable_error_names_studio() {
        let result = tool_unavailable_error("conductor_create_mapping");
        let text = serde_json::to_string(&result).expect("serializable ToolCallResult");
        assert!(
            text.contains("not available in this build"),
            "error text missing 'not available in this build': {text}"
        );
        assert!(
            text.contains("Conductor Studio"),
            "error text does not name Conductor Studio: {text}"
        );
    }
}

/// ADR-045 D1 / Council R1 #2 — the BUNDLE profile (`llm-executor` ON,
/// `mcp-write` OFF: what Studio ships). Write-tool risk tiers ARE compiled
/// (the IPC plan/apply path needs them), yet none of those tools may be
/// advertised or compiled into the MCP catalog: the socket stays read-only.
#[cfg(all(feature = "llm-executor", not(feature = "mcp-write")))]
mod bundle_profile {
    use super::*;

    #[test]
    fn write_tiers_classified_but_tools_absent_from_catalog() {
        // Tier classification works (NOT the Privileged fallback)…
        assert_eq!(
            get_tool_risk_tier("conductor_create_mapping"),
            ToolRiskTier::ConfigChange
        );
        assert_eq!(
            get_tool_risk_tier("conductor_send_midi"),
            ToolRiskTier::HardwareIO
        );
        assert_eq!(
            get_tool_risk_tier("conductor_start_midi_learn"),
            ToolRiskTier::Stateful
        );
        // …but the catalog still excludes every write tool.
        for gated in GATED_TOOLS {
            assert!(
                !is_compiled_tool(gated),
                "`{gated}` compiled into the bundle profile's MCP catalog"
            );
        }
    }
}

#[cfg(feature = "mcp-write")]
mod with_mcp_write {
    use super::*;

    /// The full-composition build advertises the complete catalog.
    #[test]
    fn gated_tools_advertised_with_correct_tiers() {
        let names: Vec<String> = get_tool_definitions().into_iter().map(|d| d.name).collect();
        for gated in GATED_TOOLS {
            assert!(
                names.iter().any(|n| n == gated),
                "tool `{gated}` missing from tools/list in an mcp-write build"
            );
            assert!(is_compiled_tool(gated), "`{gated}` not compiled");
        }
        // GUI-only profile tools stay unadvertised even here (they are
        // frontend-intercepted), but keep their write-tier classification.
        for gated in GATED_UNADVERTISED {
            assert!(
                !names.iter().any(|n| n == gated),
                "`{gated}` must never be advertised (frontend-intercepted)"
            );
            assert_eq!(get_tool_risk_tier(gated), ToolRiskTier::Stateful);
        }
        // Spot-check the risk-tier taxonomy is untouched by the split
        // (Council R1 #1: placement by capability, never by re-tiering).
        assert_eq!(
            get_tool_risk_tier("conductor_create_mapping"),
            ToolRiskTier::ConfigChange
        );
        assert_eq!(
            get_tool_risk_tier("conductor_send_midi"),
            ToolRiskTier::HardwareIO
        );
        assert_eq!(
            get_tool_risk_tier("conductor_start_midi_learn"),
            ToolRiskTier::Stateful
        );
        assert_eq!(
            get_tool_risk_tier("conductor_get_status"),
            ToolRiskTier::ReadOnly
        );
    }
}
