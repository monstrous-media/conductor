// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Per-device mute / enable / status helpers for [`super::InputManager`].

use super::InputManager;
use conductor_core::identity::DeviceId;
use tracing::info;

impl InputManager {
    /// Enable or disable a device (mute/unmute) (v4.20.0 - ADR-009 Phase 2, D8)
    pub fn set_device_enabled(&mut self, device_id: &DeviceId, enabled: bool) {
        if enabled {
            self.muted_devices.remove(device_id);
            info!(device_id = %device_id, "Device enabled (unmuted)");
        } else {
            self.muted_devices.insert(device_id.clone());
            info!(device_id = %device_id, "Device disabled (muted)");
        }
    }

    /// Check if a device is enabled (not muted) (v4.20.0 - ADR-009 Phase 2, D8)
    pub fn is_device_enabled(&self, device_id: &DeviceId) -> bool {
        !self.muted_devices.contains(device_id)
    }

    /// Get device bindings for status reporting (v4.20.0 - ADR-009 Phase 2)
    ///
    /// Returns (device_id, port_name, connected) for each managed device.
    /// Get device bindings with connection and configuration status (v4.26.0 - D19)
    ///
    /// Returns `(device_id, port_name, connected, is_configured)` tuples.
    /// `is_configured` is `true` when the DeviceId was resolved from a `[[devices]]` identity.
    pub fn get_device_bindings(&self) -> Vec<(DeviceId, String, bool, bool)> {
        self.midi_managers
            .iter()
            .map(|(device_id, mgr)| {
                let port_name = mgr
                    .device_info()
                    .map(|(_, name)| name)
                    .unwrap_or_else(|| "unknown".to_string());
                let connected = mgr.is_connected();
                let is_configured = self.configured_devices.contains(device_id);
                (device_id.clone(), port_name, connected, is_configured)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::super::InputMode;
    use super::*;

    #[test]
    fn test_device_mute_unmute() {
        let mut manager = InputManager::new(None, false, InputMode::MidiOnly);
        let device_id = DeviceId::from_alias("pads");

        // Initially enabled
        assert!(manager.is_device_enabled(&device_id));

        // Mute
        manager.set_device_enabled(&device_id, false);
        assert!(!manager.is_device_enabled(&device_id));

        // Unmute
        manager.set_device_enabled(&device_id, true);
        assert!(manager.is_device_enabled(&device_id));
    }
}
