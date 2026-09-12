// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! DaemonState and its status-JSON projections.

use super::*;

/// Daemon state for MCP tools (ADR-007 Phase 2)
///
/// This struct provides a snapshot of daemon state for MCP tool execution.
/// It's created on demand and passed to MCP tool handlers.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DaemonState {
    /// Current lifecycle state
    pub lifecycle_state: Option<LifecycleState>,
    /// Device connection status
    pub device_status: Option<DeviceStatus>,
    /// Daemon statistics
    pub statistics: Option<DaemonStatistics>,
    /// Input mode (MidiOnly, GamepadOnly, Both)
    pub input_mode: Option<String>,
    /// Connected HID/gamepad devices
    pub hid_devices: Vec<Value>,
    /// Uptime in seconds
    pub uptime_secs: u64,
    /// Config path
    pub config_path: Option<String>,
    /// Active profile info (Phase 1)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_profile: Option<ActiveProfileInfo>,
}

impl DaemonState {
    /// Create a new empty daemon state
    pub fn new() -> Self {
        Self::default()
    }

    /// Convert to JSON for MCP status tool
    pub fn to_status_json(&self) -> Value {
        let device_connected = self
            .device_status
            .as_ref()
            .map(|d| d.connected)
            .unwrap_or(false);

        // Include device_bindings from multi-device status
        // Use dps.is_configured field instead of broken starts_with("raw:") check (D19)
        let device_bindings: Vec<Value> = self
            .device_status
            .as_ref()
            .map(|d| {
                d.devices
                    .iter()
                    .map(|dps| {
                        json!({
                            "device_id": dps.device_id,
                            "port_name": dps.port_name,
                            "connected": dps.connected,
                            "enabled": dps.enabled,
                            "is_configured": dps.is_configured,
                            "direction": dps.direction,
                            "output_port_name": dps.output_port_name,
                            "output_connected": dps.output_connected,
                            "output_auto_paired": dps.output_auto_paired,
                            "protocol": dps.protocol
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        json!({
            "daemon_running": true, // Always true when daemon is responding
            "lifecycle_state": self.lifecycle_state.map(|s| format!("{}", s)).unwrap_or_else(|| "Unknown".to_string()),
            "connected": device_connected, // Backward compat: device connection state
            "device_connected": device_connected, // Explicit alias
            "device": self.device_status.as_ref().map(|d| json!({
                "name": d.name,
                "port": d.port,
                "last_event_at": d.last_event_at
            })).unwrap_or(json!(null)),
            "uptime_secs": self.uptime_secs,
            "config_path": self.config_path,
            "input_mode": self.input_mode,
            "statistics": self.statistics.as_ref().map(|s| json!({
                "events_processed": s.events_processed,
                "actions_executed": s.actions_executed,
                "errors_since_start": s.errors_since_start,
                "config_reloads": s.config_reloads
            })).unwrap_or(json!(null)),
            "device_bindings": device_bindings,
            "active_profile": self.active_profile.as_ref().map(|p| json!({
                "name": p.name,
                "config_path": p.config_path
            }))
        })
    }

    /// Convert to JSON for MCP devices tool
    pub fn to_devices_json(&self, midi_devices: Vec<MidiDeviceInfo>) -> Value {
        // Include device_bindings from multi-device status
        // Use dps.is_configured field instead of broken starts_with("raw:") check (D19)
        let device_bindings: Vec<Value> = self
            .device_status
            .as_ref()
            .map(|d| {
                d.devices
                    .iter()
                    .map(|dps| {
                        json!({
                            "device_id": dps.device_id,
                            "port_name": dps.port_name,
                            "connected": dps.connected,
                            "enabled": dps.enabled,
                            "is_configured": dps.is_configured,
                            "direction": dps.direction,
                            "output_port_name": dps.output_port_name,
                            "output_connected": dps.output_connected,
                            "output_auto_paired": dps.output_auto_paired,
                            "protocol": dps.protocol
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        json!({
            "midi_devices": midi_devices.iter().map(|d| json!({
                "port_index": d.port_index,
                "port_name": d.port_name,
                "manufacturer": d.manufacturer,
                "connected": d.connected
            })).collect::<Vec<_>>(),
            "hid_devices": self.hid_devices,
            "device_bindings": device_bindings
        })
    }
}
