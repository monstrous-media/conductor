// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Device/port status, MIDI device info, and daemon statistics.

use super::*;

/// Device status information
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeviceStatus {
    pub connected: bool,
    pub name: Option<String>,
    pub port: Option<usize>,
    pub last_event_at: Option<u64>, // Unix timestamp in seconds
    /// Multi-device port statuses (ADR-009 Phase 2)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub devices: Vec<DevicePortStatus>,
}

/// Per-device port status for multi-device mode (ADR-009 Phase 2)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevicePortStatus {
    pub device_id: String,
    pub port_name: String,
    pub port_index: usize,
    pub connected: bool,
    /// Whether the device is enabled (not muted) (D8)
    pub enabled: bool,
    pub last_event_at: Option<u64>,
    /// Whether this device is bound to a configured `[[devices]]` identity (ADR-009 D19)
    ///
    /// `true` when the device was resolved via `BindingResult::Bound`.
    /// `false` when the device was opened as an unconfigured port (e.g. in `ListenMode::All`).
    #[serde(default)]
    pub is_configured: bool,
    /// Device direction: Input, Output, or Bidirectional (ADR-021)
    #[serde(default)]
    pub direction: conductor_core::config::DeviceDirection,
    /// Resolved output port name, if an output port was successfully matched for this device.
    /// May be `None` even if the device has output configured, when no matching port is
    /// currently available in the MIDI output enumeration. (ADR-021)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_port_name: Option<String>,
    /// Whether an output port was resolved and is currently available for this device.
    /// Set to `true` when `build_output_map` successfully matches the endpoint's output
    /// matchers (or auto-pairs) to a port in the current MIDI output enumeration. Conductor uses
    /// on-demand output connections, so this reflects resolution + availability, not an open
    /// connection. (ADR-021)
    #[serde(default)]
    pub output_connected: bool,
    /// Whether the output port was auto-paired (ADR-021)
    #[serde(default)]
    pub output_auto_paired: bool,
    /// Device protocol: "midi", "hid", "osc", or "artnet"
    #[serde(default = "default_protocol_midi")]
    pub protocol: String,
}

fn default_protocol_midi() -> String {
    "midi".to_string()
}

/// MIDI device information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MidiDeviceInfo {
    pub port_index: usize,
    pub port_name: String,
    pub manufacturer: Option<String>,
    pub connected: bool,
}

/// Daemon statistics
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DaemonStatistics {
    pub events_processed: u64,
    pub actions_executed: u64,
    pub errors_since_start: u64,
    pub config_reloads: u64,
    pub uptime_secs: u64,
    pub last_reload_duration_ms: Option<u64>,
    pub fastest_reload_ms: Option<u64>,
    pub slowest_reload_ms: Option<u64>,
    pub avg_reload_ms: Option<u64>,
}

impl DaemonStatistics {
    /// Update reload statistics with new metrics
    pub fn update_reload_metrics(&mut self, metrics: &ReloadMetrics) {
        self.config_reloads += 1;
        self.last_reload_duration_ms = Some(metrics.duration_ms);

        // Update fastest
        self.fastest_reload_ms = Some(match self.fastest_reload_ms {
            None => metrics.duration_ms,
            Some(fastest) => fastest.min(metrics.duration_ms),
        });

        // Update slowest
        self.slowest_reload_ms = Some(match self.slowest_reload_ms {
            None => metrics.duration_ms,
            Some(slowest) => slowest.max(metrics.duration_ms),
        });

        // Update average using cumulative average formula
        // new_avg = ((count - 1) * old_avg + new_value) / count
        // Use u128 for intermediate calculations to prevent overflow
        self.avg_reload_ms = Some(match self.avg_reload_ms {
            None => metrics.duration_ms,
            Some(avg) => {
                let count = self.config_reloads as u128;
                let old_avg = avg as u128;
                let new_value = metrics.duration_ms as u128;
                // ((count - 1) * old_avg + new_value) / count
                (((count - 1) * old_avg + new_value) / count) as u64
            }
        });
    }
}

/// Error entry for error log
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorEntry {
    pub timestamp: u64, // Unix timestamp in seconds
    pub kind: String,
    pub message: String,
}

impl ErrorEntry {
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            kind: kind.into(),
            message: message.into(),
        }
    }
}

/// Config reload metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReloadMetrics {
    pub duration_ms: u64,
    pub modes_loaded: usize,
    pub mappings_loaded: usize,
    pub config_load_ms: u64,
    pub mapping_compile_ms: u64,
    pub swap_ms: u64,
}

impl ReloadMetrics {
    /// Check if reload met performance targets
    pub fn met_target(&self) -> bool {
        self.duration_ms < 50 // Target: <50ms
    }

    /// Get performance grade (A/B/C/D/F)
    pub fn performance_grade(&self) -> char {
        match self.duration_ms {
            0..=20 => 'A',    // Excellent
            21..=50 => 'B',   // Good (target)
            51..=100 => 'C',  // Acceptable
            101..=200 => 'D', // Poor
            _ => 'F',         // Unacceptable
        }
    }
}
