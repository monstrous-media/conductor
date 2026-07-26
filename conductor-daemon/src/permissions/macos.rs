// Copyright 2026 Amiable
// SPDX-License-Identifier: MIT

//! macOS-specific implementation of the permission probes used by
//! `conductorctl permissions`.
//!
//! ## How the probe works
//!
//! macOS does not expose a public API to query "does process X hold
//! Input Monitoring" without elevated privileges. We infer the grant
//! by attempting the operation that would be gated and observing the
//! outcome:
//!
//! - **Granted**: gilrs successfully creates an `IOHIDManager` and
//!   enumerates zero or more gamepads.
//! - **NotGranted**: gilrs fails to create `IOHIDManager`. The most
//!   common cause is `kIOReturnNotPermitted`, but the gilrs API
//!   doesn't surface that specific code — we treat any failure as
//!   "probably permission missing" and tell the user to check System
//!   Settings.
//!
//! Best-effort by design. The only authoritative answer is what
//! System Settings → Privacy & Security → Input Monitoring shows.
//!
//! ## Why not call IOHIDManagerCreate directly?
//!
//! Linking against IOKit + writing FFI for a single diagnostic CLI
//! command is more code (and a bigger dependency surface) than using
//! gilrs, which is already a workspace dependency. The functional
//! contract is identical.

use super::PermissionStatus;

/// Probe Input Monitoring by attempting a gilrs initialisation.
///
/// `gilrs::Gilrs::new()` returns `Err(gilrs::Error::Other(_))` when
/// `IOHIDManagerCreate` fails on macOS. We don't get a fine-grained
/// IOReturn code from gilrs, so any error from `Gilrs::new()` is
/// treated as "probably permission missing" — the user is steered to
/// System Settings either way.
pub fn probe_input_monitoring() -> PermissionStatus {
    match gilrs::Gilrs::new() {
        Ok(_gilrs) => PermissionStatus::Granted,
        Err(e) => {
            // gilrs::Error::Other typically wraps "Failed to create
            // IOHIDManager object" on macOS without permission. Other
            // variants exist (e.g. PlatformNotSupported) but on macOS
            // any error from Gilrs::new() is overwhelmingly likely
            // to be the TCC denial.
            let detail = format!("{}", e);
            if detail.contains("IOHIDManager") || detail.is_empty() {
                PermissionStatus::NotGranted
            } else {
                PermissionStatus::Unknown(detail)
            }
        }
    }
}

/// Open System Settings to the Input Monitoring pane via the deep-link
/// URL scheme. Returns `Ok(())` only if the `open` command exits
/// successfully — a non-zero exit (bad URL scheme, System Settings
/// unavailable, etc.) becomes an `io::Error` so the caller can
/// surface it instead of silently reporting success.
pub fn open_input_monitoring_pane() -> std::io::Result<()> {
    let status = std::process::Command::new("open")
        .arg(super::INPUT_MONITORING_DEEPLINK)
        .status()?;

    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "failed to open Input Monitoring pane: `open` exited with status {}",
            status
        )))
    }
}
