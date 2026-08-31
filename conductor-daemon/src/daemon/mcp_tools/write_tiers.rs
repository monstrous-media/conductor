// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-045 D2 (#2492) — risk-tier classification for write-tier MCP tools.
//!
//! Compiled under `llm-executor`: the IPC plan/apply path needs correct
//! tiers for write tools even when the MCP socket never advertises them
//! (`mcp-write` off — every official artifact). The tier values are
//! IDENTICAL to the pre-split classification (Council R1 #1: commercial
//! placement never re-tiers). Definitions live in `definitions_write.rs`
//! (advertised only under `mcp-write`).

use super::super::mcp_types::ToolRiskTier;

/// Risk tier for write-tier tools; `None` when `tool_name` is not write-tier.
pub(super) fn write_tool_risk_tier(tool_name: &str) -> Option<ToolRiskTier> {
    Some(match tool_name {
        "conductor_create_endpoint" => ToolRiskTier::ConfigChange, // ADR-035 Slice 8

        // ConfigChange tools (Phase 2) - require Plan/Apply
        "conductor_create_mapping" => ToolRiskTier::ConfigChange,
        "conductor_update_mapping" => ToolRiskTier::ConfigChange,
        "conductor_delete_mapping" => ToolRiskTier::ConfigChange,
        "conductor_batch_changes" => ToolRiskTier::ConfigChange, // P3-07: Batch operations
        "conductor_set_context_mapping" => ToolRiskTier::ConfigChange, // ADR-025 Phase 2.H

        // Stateful tools (Phase 2) - execute with logging
        "conductor_start_learn" | "conductor_start_midi_learn" => ToolRiskTier::Stateful,
        "conductor_stop_learn" | "conductor_stop_midi_learn" => ToolRiskTier::Stateful,
        "conductor_set_mapping_editor" => ToolRiskTier::Stateful,
        "conductor_update_mapping_editor" => ToolRiskTier::Stateful,
        "conductor_render_artifact" => ToolRiskTier::ArtifactRender, // #612
        "conductor_dismiss_artifact" => ToolRiskTier::ArtifactRender, // #621
        "conductor_switch_mode" => ToolRiskTier::Stateful,           // v4.26.69
        "conductor_set_mode" => ToolRiskTier::Stateful,              // ADR-040 4c
        "conductor_unlock_mode" => ToolRiskTier::Stateful,           // ADR-040 4c
        "conductor_switch_profile" => ToolRiskTier::Stateful,        // Phase 1 - Issue #323

        // HardwareIO tools (Phase 4) - require multi-step confirmation
        "conductor_send_sysex" => ToolRiskTier::HardwareIO,
        "conductor_device_reset" => ToolRiskTier::HardwareIO,
        "conductor_send_midi" => ToolRiskTier::HardwareIO, // v4.26.67
        "conductor_probe_device_identity" => ToolRiskTier::HardwareIO, // ADR-026 Phase 2
        "conductor_set_device_enabled" => ToolRiskTier::Stateful,
        "conductor_scan_ports" => ToolRiskTier::Stateful,
        "conductor_enable_plugin" => ToolRiskTier::Stateful,
        "conductor_disable_plugin" => ToolRiskTier::Stateful,
        "conductor_reset_control_state" => ToolRiskTier::Stateful,
        _ => return None,
    })
}
