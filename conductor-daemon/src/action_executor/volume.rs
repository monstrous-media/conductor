// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! System volume control for the `VolumeControl` action (#1684 split from
//! `action_executor.rs`). Platform-specific: AppleScript on macOS,
//! PulseAudio on Linux, unimplemented on Windows.

use conductor_core::VolumeOperation;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::process::Command;

pub(crate) fn execute_volume_control(operation: &VolumeOperation, value: &Option<u8>) {
    #[cfg(target_os = "macos")]
    {
        let script = match operation {
            VolumeOperation::Up => {
                "set volume output volume ((output volume of (get volume settings)) + 10)"
            }
            VolumeOperation::Down => {
                "set volume output volume ((output volume of (get volume settings)) - 10)"
            }
            VolumeOperation::Mute => "set volume output muted true",
            VolumeOperation::Unmute => "set volume output muted false",
            VolumeOperation::Set => {
                if let Some(vol) = value {
                    &format!("set volume output volume {}", vol)
                } else {
                    "set volume output volume 50"
                }
            }
        };

        Command::new("osascript").arg("-e").arg(script).spawn().ok();
    }

    #[cfg(target_os = "linux")]
    {
        // Try PulseAudio first, fall back to ALSA
        match operation {
            VolumeOperation::Up => {
                Command::new("pactl")
                    .args(["set-sink-volume", "@DEFAULT_SINK@", "+10%"])
                    .spawn()
                    .ok();
            }
            VolumeOperation::Down => {
                Command::new("pactl")
                    .args(["set-sink-volume", "@DEFAULT_SINK@", "-10%"])
                    .spawn()
                    .ok();
            }
            VolumeOperation::Mute => {
                Command::new("pactl")
                    .args(["set-sink-mute", "@DEFAULT_SINK@", "1"])
                    .spawn()
                    .ok();
            }
            VolumeOperation::Unmute => {
                Command::new("pactl")
                    .args(["set-sink-mute", "@DEFAULT_SINK@", "0"])
                    .spawn()
                    .ok();
            }
            VolumeOperation::Set => {
                let volume_str = if let Some(vol) = value {
                    format!("{}%", vol)
                } else {
                    "50%".to_string()
                };
                Command::new("pactl")
                    .args(["set-sink-volume", "@DEFAULT_SINK@", &volume_str])
                    .spawn()
                    .ok();
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        let _ = (operation, value);
        tracing::warn!("Volume control not implemented for Windows");
    }
}
