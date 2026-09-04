// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Application launching for the `Launch` action (split from
//! `action_executor.rs`). Platform-specific spawn semantics with
//! failure surfacing.

use super::ActionExecutor;
use conductor_core::dispatch::DispatchError;
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
use std::process::Command;

impl ActionExecutor {
    /// Launch an application.
    ///
    /// Returns `DispatchError::Launch` on failure so the caller can
    /// surface the error in the dispatch outcome instead of pretending
    /// the action completed. Pre-fix this used `.spawn().ok()`
    /// which discarded all errors, producing fake-success dispatch
    /// results with no log trail.
    ///
    /// Platform notes:
    /// - **macOS**: `open -a App` exits immediately after instructing
    ///   Launch Services. Uses `.output()` so we capture both the exit
    ///   status and the stderr text — `open` writes the actual reason
    ///   ("Unable to find application named '...'", sandbox denial, etc.)
    ///   to stderr, which is much more useful in the error message than
    ///   a generic exit-code hint. The actual app starts asynchronously
    ///   so this doesn't block on slow-launching apps.
    /// - **Linux**: the spawn target IS the app process; `.spawn()` is
    ///   correct. Spawn errors (`ErrorKind::NotFound`, permission denied)
    ///   surface here.
    /// - **Windows**: `cmd /C start app` returns spawn-success even when
    ///   the target app doesn't exist (Windows shows an error dialog
    ///   rather than failing). Best-effort error surfacing — at minimum
    ///   we report spawn errors from `cmd` itself.
    pub(crate) fn launch_app(&self, app: &str) -> Result<(), DispatchError> {
        #[cfg(target_os = "macos")]
        {
            let output = Command::new("open")
                .arg("-a")
                .arg(app)
                .output()
                .map_err(|e| {
                    let msg = format!("Failed to invoke 'open -a {}': {}", app, e);
                    tracing::error!(app = %app, error = %e, "Failed to invoke 'open' for app launch");
                    DispatchError::Launch(msg)
                })?;
            if !output.status.success() {
                // Capture `open`'s actual error text from stderr — much
                // more useful than a generic exit-code hint.
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                let msg = if stderr.is_empty() {
                    format!("'open -a {}' exited with status {}", app, output.status)
                } else {
                    format!(
                        "'open -a {}' exited with status {}: {}",
                        app, output.status, stderr
                    )
                };
                tracing::error!(
                    app = %app,
                    status = %output.status,
                    stderr = %stderr,
                    "Failed to launch app"
                );
                return Err(DispatchError::Launch(msg));
            }
            tracing::info!(app = %app, "Launched app");
            Ok(())
        }

        #[cfg(target_os = "linux")]
        {
            match Command::new(app).spawn() {
                Ok(child) => {
                    tracing::info!(app = %app, pid = child.id(), "Launched app");
                    Ok(())
                }
                Err(e) => {
                    let msg = format!("Failed to spawn '{}': {}", app, e);
                    tracing::error!(app = %app, error = %e, "Failed to launch app");
                    Err(DispatchError::Launch(msg))
                }
            }
        }

        #[cfg(target_os = "windows")]
        {
            match Command::new("cmd").args(&["/C", "start", app]).spawn() {
                Ok(child) => {
                    tracing::info!(app = %app, pid = child.id(), "Launched app");
                    Ok(())
                }
                Err(e) => {
                    let msg = format!("Failed to spawn 'cmd /C start {}': {}", app, e);
                    tracing::error!(app = %app, error = %e, "Failed to launch app");
                    Err(DispatchError::Launch(msg))
                }
            }
        }
    }
}

// ========================================================================
// Launch action surfaces failures (was silently swallowed)
// ========================================================================
//
// Pre-fix: `launch_app` used `.spawn().ok()` which discarded all errors,
// and the caller always returned `DispatchOutcome::Completed`. So a
// mapping that "launched Calculator" reported success even when:
//  - the app name was wrong / mistyped
//  - macOS sandbox denied the spawn
//  - the binary wasn't on PATH (Linux)
//  - `open` couldn't find the bundle
//
// No log line, no error, no signal. Worst-case debug experience.
//
// Fix: return `Result<(), DispatchError>`; macOS uses `.status()` to
// capture the non-zero exit from `open` (it exits immediately after
// instructing Launch Services); Linux/Windows surface spawn errors.
#[cfg(test)]
mod tests {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    use crate::action_executor::test_support::test_executor;

    #[cfg(target_os = "macos")]
    #[test]
    fn launch_app_returns_err_for_nonexistent_app_macos() {
        // `open -a <invalid-app>` exits with status 1 and writes
        // "Unable to find application named '...'" to stderr. The fix
        // uses .output() (not .spawn() or .status()) so we capture that
        // stderr text and surface it in the error message — gives the
        // user the actual failure reason instead of a generic hint.
        let executor = test_executor();
        let result = executor.launch_app("DefinitelyNonexistentApp_xyzzy_12345");
        assert!(
            result.is_err(),
            "must propagate launch failure; got: {:?}",
            result
        );
        let err = result.unwrap_err();
        match &err {
            conductor_core::dispatch::DispatchError::Launch(msg) => {
                assert!(
                    msg.contains("DefinitelyNonexistentApp_xyzzy_12345"),
                    "error message must include the app name for diagnosability: {}",
                    msg
                );
                // Captured-stderr contract: the actual
                // failure reason from `open` must appear in the error
                // message, not just an inferred guess.
                assert!(
                    msg.contains("Unable to find application named"),
                    "error message must include the captured stderr from `open`: {}",
                    msg
                );
            }
            other => panic!("Expected DispatchError::Launch, got: {:?}", other),
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn launch_app_returns_err_for_nonexistent_binary_linux() {
        // On Linux the spawn target IS the app process; an invalid path
        // causes `Command::spawn` to return `ErrorKind::NotFound`.
        let executor = test_executor();
        let result = executor.launch_app("/nonexistent/path/to/binary_xyzzy_12345");
        assert!(
            result.is_err(),
            "must propagate spawn failure; got: {:?}",
            result
        );
        match result.unwrap_err() {
            conductor_core::dispatch::DispatchError::Launch(msg) => {
                assert!(
                    msg.contains("binary_xyzzy_12345"),
                    "error message must include the binary path for diagnosability: {}",
                    msg
                );
            }
            other => panic!("Expected DispatchError::Launch, got: {:?}", other),
        }
    }
}
