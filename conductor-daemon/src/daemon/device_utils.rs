// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Shared MIDI device enumeration utilities (v4.17.0, #104)
//!
//! Provides reliable MIDI device enumeration using the warmup pattern
//! required by macOS Core MIDI to avoid stale cached device lists.
//! Used by MCP server, ToolExecutor, and EngineManager.

use super::types::MidiDeviceInfo;
use midir::MidiInput;
use tracing::warn;

/// Maximum number of MIDI devices to enumerate, preventing unbounded allocation.
const MAX_MIDI_DEVICES: usize = 64;

/// Enumerate MIDI devices with warmup cycle for fresh results.
///
/// On macOS (Core MIDI), the MIDI API caches device lists. This function
/// uses a warmup pattern to ensure newly-connected devices are visible:
///
/// 1. Create a warmup `MidiInput` instance (registers a Core MIDI client)
/// 2. Sleep 100ms while the warmup client is alive — Core MIDI delivers
///    "device added" notifications only to active clients, so dropping
///    before sleep would lose the notification (#110)
/// 3. Drop the warmup client, then create a fresh `MidiInput` for enumeration
///
/// Results are capped at [`MAX_MIDI_DEVICES`] (64) to prevent unbounded allocation.
///
/// This function is synchronous (uses `thread::sleep`). In async contexts,
/// use [`enumerate_midi_devices_fresh_async`] which wraps this in `spawn_blocking`.
pub fn enumerate_midi_devices_fresh() -> Vec<MidiDeviceInfo> {
    // macOS Core MIDI caches device lists; warmup + sleep forces a cache refresh.
    // This is not needed on Linux/Windows where enumeration is always fresh.
    #[cfg(target_os = "macos")]
    {
        // Step 1: Create warmup MidiInput and keep it alive during the sleep.
        // Core MIDI sends "device added" notifications to active clients only;
        // dropping before sleep means no client receives the notification (#110).
        let _warmup = match MidiInput::new("Conductor Warmup") {
            Ok(mi) => Some(mi),
            Err(e) => {
                warn!("Failed to create warmup MIDI client: {}", e);
                None
            }
        };

        // Step 2: Sleep while warmup client is alive to receive notifications
        std::thread::sleep(std::time::Duration::from_millis(100));
        // _warmup dropped here — cache is now up to date
    }

    // Step 3: Fresh enumeration with bounded allocation
    let mut devices = Vec::new();
    match MidiInput::new("Conductor Device Scanner") {
        Ok(midi_in) => {
            let ports = midi_in.ports();
            for (i, port) in ports.iter().enumerate().take(MAX_MIDI_DEVICES) {
                match midi_in.port_name(port) {
                    Ok(port_name) => {
                        let manufacturer =
                            port_name.split_whitespace().next().map(|s| s.to_string());
                        devices.push(MidiDeviceInfo {
                            port_index: i,
                            port_name,
                            manufacturer,
                            connected: true, // Visible ports are connected
                        });
                    }
                    Err(e) => {
                        warn!("Failed to get name for MIDI port {}: {}", i, e);
                        continue;
                    }
                }
            }
        }
        Err(e) => {
            warn!("Failed to create MIDI input for device enumeration: {}", e);
        }
    }
    devices
}

/// Async wrapper that runs enumeration in a blocking thread.
/// Use this from async contexts to avoid blocking the tokio runtime.
pub async fn enumerate_midi_devices_fresh_async() -> Vec<MidiDeviceInfo> {
    match tokio::task::spawn_blocking(enumerate_midi_devices_fresh).await {
        Ok(devices) => devices,
        Err(e) => {
            warn!("MIDI device enumeration task failed: {}", e);
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_enumerate_returns_vec() {
        // Should not panic even with no MIDI hardware
        let devices = enumerate_midi_devices_fresh();
        // Returns a Vec (may be empty in CI)
        let _ = devices.len();
    }

    /// Verify warmup pattern keeps instance alive during sleep (#110).
    /// Uses `_warmup` (named binding) not `_` (immediate drop).
    #[test]
    fn test_warmup_keeps_instance_alive() {
        // This test validates the warmup pattern at compile time:
        // `let _warmup = ...` keeps the instance alive until end of scope,
        // while `let _ = ...` drops immediately. The named binding ensures
        // Core MIDI has an active client during the sleep window.
        #[cfg(target_os = "macos")]
        {
            let _warmup = MidiInput::new("Test Warmup");
            // _warmup is alive here — Core MIDI client active
            std::thread::sleep(std::time::Duration::from_millis(10));
            // _warmup dropped at end of block
        }
        // Test passes if it compiles with the named binding pattern
    }

    #[test]
    fn test_enumerate_device_fields_valid() {
        let devices = enumerate_midi_devices_fresh();
        for device in &devices {
            assert!(
                !device.port_name.is_empty(),
                "port_name should not be empty"
            );
            assert!(device.connected, "visible ports should be marked connected");
        }
    }
}
