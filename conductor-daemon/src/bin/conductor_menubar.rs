// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Menu bar service for Conductor daemon
//!
//! Provides a system tray icon with status display and quick actions.
//! Communicates with the conductor daemon via IPC.

// CLI binary: legitimate println!/eprintln! to stdout/stderr.
// See docs/epic-loop/rust-coverage.md Path A.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use conductor_daemon::daemon::{
    IconState, IpcClient, IpcCommand, IpcRequest, MenuAction, MenuBar, MenuBarError, ResponseStatus,
};
use std::time::Duration;
use tokio::time;
use uuid::Uuid;

#[tokio::main]
async fn main() {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("conductor_menubar=info".parse().unwrap()),
        )
        .init();

    tracing::info!("Starting Conductor menu bar");

    // Create menu bar
    let mut menu_bar = match MenuBar::new(IconState::Stopped) {
        Ok(mb) => mb,
        Err(MenuBarError::HeadlessEnvironment) => {
            tracing::warn!("Cannot create menu bar in headless environment");
            return;
        }
        Err(e) => {
            tracing::error!("Failed to create menu bar: {}", e);
            return;
        }
    };

    tracing::info!("Menu bar created successfully");

    // Main event loop
    let mut last_status_check = std::time::Instant::now();
    let status_interval = Duration::from_secs(3);

    loop {
        // Check for menu events
        if let Some(action) = menu_bar.poll_events() {
            handle_menu_action(action, &mut menu_bar).await;
        }

        // Update status periodically
        if last_status_check.elapsed() >= status_interval {
            update_status(&mut menu_bar).await;
            last_status_check = std::time::Instant::now();
        }

        // Sleep to avoid busy-waiting
        time::sleep(Duration::from_millis(100)).await;
    }
}

/// Handle a menu action from the user
async fn handle_menu_action(action: MenuAction, menu_bar: &mut MenuBar) {
    match action {
        MenuAction::ReloadConfig => {
            tracing::info!("Reload config requested");
            menu_bar.update_status("Conductor: Reloading...");

            match send_ipc_command(IpcCommand::Reload).await {
                Ok(_) => {
                    tracing::info!("Config reloaded successfully");
                    menu_bar.update_status("Conductor: Running");
                    // Icon will be updated on next status check
                }
                Err(e) => {
                    tracing::error!("Failed to reload config: {}", e);
                    menu_bar.update_status(&format!("Conductor: Error - {}", e));
                    let _ = menu_bar.set_state(IconState::Error);
                }
            }
        }
        MenuAction::Quit => {
            tracing::info!("Quit requested");
            menu_bar.update_status("Conductor: Stopping...");

            // Quit is a *best-effort* daemon shutdown (#1428): Stop is sent,
            // but whether or not the daemon acknowledges it, the tray process
            // MUST exit. The pre-fix code only exited inside the Ok arm, so a
            // Quit issued while the daemon was already down/crashed logged an
            // error and kept the tray running — leaving the user no in-menu
            // way to dismiss the icon. `send_ipc_command` is itself bounded by
            // IPC_TIMEOUT (#1429), so a *hung* (as opposed to merely down)
            // daemon surfaces as an Err here too rather than wedging the await
            // and starving the unconditional exit below.
            let plan = plan_quit(send_ipc_command(IpcCommand::Stop).await);
            match &plan {
                QuitAction::DrainThenExit { status, drain } => {
                    tracing::info!("Daemon stop command sent");
                    menu_bar.update_status(status);
                    time::sleep(*drain).await;
                }
                QuitAction::ExitImmediately { status } => {
                    tracing::error!(
                        "Stop request failed during Quit; exiting menubar anyway (#1428)"
                    );
                    menu_bar.update_status(status);
                }
            }
            // Unconditional, outside the match: this is the whole point of
            // #1428. Both QuitAction variants fall through to here, so the
            // tray always exits — whether or not the daemon acknowledged Stop.
            std::process::exit(0);
        }
    }
}

/// How the menubar should wind down after attempting a best-effort daemon
/// Stop in response to a Quit menu action (#1428).
///
/// Both variants ultimately exit the process — the distinction is only
/// whether the daemon acknowledged the Stop (and so deserves a brief drain
/// delay) or the request failed (exit immediately, surfacing the error).
#[derive(Debug, PartialEq, Eq)]
enum QuitAction {
    /// Daemon acknowledged Stop; give it `drain` to wind down, then exit.
    DrainThenExit { status: String, drain: Duration },
    /// Stop failed (daemon unreachable/crashed); surface the error and exit
    /// immediately. Pre-#1428 this path did NOT exit, stranding the tray.
    ExitImmediately { status: String },
}

/// Decide the menubar's Quit wind-down from the daemon Stop response (#1428).
///
/// Pure and synchronous so the best-effort-exit contract is unit-testable
/// without IPC or calling `std::process::exit` (the caller owns the actual
/// exit). The key guarantee under test: an `Err` Stop result still yields an
/// exit-bearing action.
fn plan_quit(stop_result: Result<String, String>) -> QuitAction {
    match stop_result {
        Ok(_) => QuitAction::DrainThenExit {
            status: "Conductor: Stopping...".to_string(),
            drain: Duration::from_millis(500),
        },
        Err(e) => QuitAction::ExitImmediately {
            status: format!("Error: {e} (exiting anyway)"),
        },
    }
}

/// Update the menu bar status by querying the daemon
async fn update_status(menu_bar: &mut MenuBar) {
    match send_ipc_command(IpcCommand::Status).await {
        Ok(response) => {
            // Parse the status response
            // #2122: derive the icon from the exact parsed state; unexpected
            // payloads map to Error, never the healthy Running icon.
            let status = extract_status(&response);
            menu_bar.update_status(&status.text);
            let _ = menu_bar.set_state(status.icon);
        }
        Err(_) => {
            // Daemon not responding
            menu_bar.update_status("Conductor: Not running");
            let _ = menu_bar.set_state(IconState::Stopped);
        }
    }
}

/// UI-appropriate upper bound on a whole IPC round-trip (#1429). The tray
/// runs user actions (Reload/Quit) and the periodic status refresh
/// sequentially on a single async task; an IPC peer that accepts the
/// connection but never replies would otherwise wedge that task and make the
/// menu unresponsive. Bounding every round-trip keeps the event loop live —
/// a stalled daemon surfaces as a normal "not responding" failure.
const IPC_TIMEOUT: Duration = Duration::from_secs(2);

/// Bound an IPC round-trip future with `timeout` (#1429). A future that never
/// resolves yields a "not responding" `Err` instead of blocking the caller
/// forever. The timeout is a parameter (not hardcoded to `IPC_TIMEOUT`) so the
/// behaviour is unit-testable without waiting the production deadline.
async fn with_ipc_timeout<F>(timeout: Duration, fut: F) -> Result<String, String>
where
    F: std::future::Future<Output = Result<String, String>>,
{
    match time::timeout(timeout, fut).await {
        Ok(result) => result,
        Err(_elapsed) => Err(format!(
            "daemon did not respond within {}s (not responding)",
            timeout.as_secs()
        )),
    }
}

/// Send an IPC command to the daemon, bounded by [`IPC_TIMEOUT`] (#1429).
async fn send_ipc_command(command: IpcCommand) -> Result<String, String> {
    with_ipc_timeout(IPC_TIMEOUT, send_ipc_command_inner(command)).await
}

/// The actual connect + request round-trip. Wrapped by [`send_ipc_command`]
/// so the timeout covers both `connect` and `send_request`.
async fn send_ipc_command_inner(command: IpcCommand) -> Result<String, String> {
    let mut client = IpcClient::connect()
        .await
        .map_err(|e| format!("Failed to connect: {}", e))?;

    let request = IpcRequest {
        id: Uuid::new_v4().to_string(),
        command,
        args: serde_json::Value::Null,
    };

    let response = client
        .send_request(request)
        .await
        .map_err(|e| format!("Failed to send request: {}", e))?;

    match response.status {
        ResponseStatus::Success => {
            Ok(serde_json::to_string_pretty(&response.data).unwrap_or_default())
        }
        ResponseStatus::Error => {
            let error_msg = response
                .error
                .map(|e| e.message)
                .unwrap_or_else(|| "Unknown error".to_string());
            Err(error_msg)
        }
    }
}

/// Parsed menu-bar status: the display text plus the tray icon it maps to.
///
/// The icon is derived from the EXACT daemon `lifecycle_state` (see
/// [`icon_for_state`]), not by substring-matching the display text — so a
/// state like "NotRunning" can never accidentally light the healthy icon
/// (#2122, Council review).
struct MenuStatus {
    text: String,
    icon: IconState,
}

/// Parse an IPC status response into display text + tray icon.
///
/// Always succeeds: a response that isn't valid JSON, or that lacks the
/// expected `daemon` object, is an UNEXPECTED payload — reported as an
/// explicit unknown/error state, never as a healthy "Running" daemon (#2122).
/// Previously this fell through to "Conductor: Running" and lit the green
/// icon for any malformed/unexpected (but received) payload.
fn extract_status(response: &str) -> MenuStatus {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(response)
        && let Some(daemon) = value.get("daemon")
    {
        let state = daemon
            .get("lifecycle_state")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown");

        let uptime = daemon
            .get("uptime_seconds")
            .and_then(|v| v.as_f64())
            .map(format_duration)
            .unwrap_or_else(|| "Unknown".to_string());

        return MenuStatus {
            text: format!("Conductor: {}\nUptime: {}", state, uptime),
            icon: icon_for_state(state),
        };
    }

    MenuStatus {
        text: "Conductor: Unknown (unexpected status payload)".to_string(),
        icon: IconState::Error,
    }
}

/// Map an EXACT daemon `lifecycle_state` string to a tray [`IconState`] (#2122).
///
/// The invariant that matters: ONLY the exact state `"Running"` shows the
/// healthy green icon. Stopped/Stopping map to the stopped icon; everything
/// else — Degraded, AuditDegraded, an `Unknown`/unexpected payload, or any
/// unrecognized future state — maps to `Error`, so the tray never shows green
/// for a non-running or uninterpretable state. Exact matching (vs. a substring
/// check) is what guarantees a value like "NotRunning" can't map to Running.
fn icon_for_state(state: &str) -> IconState {
    match state {
        "Running" => IconState::Running,
        "Stopped" | "Stopping" => IconState::Stopped,
        _ => IconState::Error,
    }
}

/// Format duration in human-readable form
fn format_duration(seconds: f64) -> String {
    let secs = seconds as u64;
    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;

    if hours > 0 {
        format!("{}h {}m", hours, minutes)
    } else if minutes > 0 {
        format!("{}m", minutes)
    } else {
        format!("{}s", secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #2122: a malformed (non-JSON) status response must NOT be reported as a
    /// running daemon. Pre-fix, `extract_status` fell through to
    /// "Conductor: Running", lighting the healthy green tray icon.
    #[test]
    fn malformed_status_payload_is_not_reported_as_running() {
        let status = extract_status("this is not valid json {{{");
        assert!(
            !status.text.contains("Running"),
            "a malformed payload must not read as Running; got {:?}",
            status.text
        );
        assert_eq!(
            status.icon,
            IconState::Error,
            "an uninterpretable status must map to the Error icon, not Running"
        );
    }

    /// #2122: a well-formed JSON response that lacks the expected `daemon`
    /// object is an unexpected shape — also must not read as running.
    #[test]
    fn unexpected_status_shape_is_not_reported_as_running() {
        let status = extract_status(r#"{"something_else": {"foo": 1}}"#);
        assert!(
            !status.text.contains("Running"),
            "an unexpected payload shape must not read as Running; got {:?}",
            status.text
        );
        assert_eq!(status.icon, IconState::Error);
    }

    /// A genuine running daemon still maps to the healthy icon.
    #[test]
    fn valid_running_status_maps_to_running_icon() {
        let payload = r#"{"daemon": {"lifecycle_state": "Running", "uptime_seconds": 12.0}}"#;
        let status = extract_status(payload);
        assert!(status.text.contains("Running"), "got {:?}", status.text);
        assert_eq!(status.icon, IconState::Running);
    }

    /// A daemon object that reports a non-running lifecycle state must not show
    /// the Running icon either.
    #[test]
    fn degraded_daemon_does_not_show_running_icon() {
        let payload = r#"{"daemon": {"lifecycle_state": "Degraded", "uptime_seconds": 5.0}}"#;
        let status = extract_status(payload);
        assert_eq!(status.icon, IconState::Error);
    }

    /// #2122 (Council): exact-state matching — a state that merely CONTAINS
    /// "Running" as a substring (e.g. "NotRunning") must NOT light the healthy
    /// icon. This is what the old `contains("Running")` check got wrong.
    #[test]
    fn substring_running_state_does_not_show_running_icon() {
        assert_eq!(icon_for_state("NotRunning"), IconState::Error);
        assert_eq!(icon_for_state("RunningDegraded"), IconState::Error);
        // Sanity: the exact state still works.
        assert_eq!(icon_for_state("Running"), IconState::Running);
        assert_eq!(icon_for_state("Stopped"), IconState::Stopped);
        assert_eq!(icon_for_state("Stopping"), IconState::Stopped);
        // Unrecognized future state → not healthy.
        assert_eq!(icon_for_state("SomeNewState"), IconState::Error);
    }

    /// THE regression test for #1428. When the Stop request fails (daemon
    /// already stopped, crashed, or socket unavailable), Quit must map to
    /// `ExitImmediately` — the variant the caller turns into an unconditional
    /// `process::exit(0)`. Pre-fix, a failed Stop only updated the status text
    /// and fell through WITHOUT exiting, leaving the tray stuck with no in-menu
    /// way to dismiss it. (The actual exit lives in the caller, outside the
    /// match, so it can't be unit-tested without terminating the test process;
    /// this pins the decision that feeds it.)
    #[test]
    fn quit_maps_stop_failure_to_exit_immediately() {
        let plan = plan_quit(Err("Failed to connect: socket missing".to_string()));
        match plan {
            QuitAction::ExitImmediately { status } => {
                assert!(
                    status.contains("socket missing"),
                    "status should surface the underlying error: {status}"
                );
            }
            QuitAction::DrainThenExit { .. } => panic!(
                "a failed Stop must map to ExitImmediately, not DrainThenExit (#1428); got {plan:?}"
            ),
        }
    }

    /// The happy path drains briefly before exiting so the daemon can finish
    /// winding down. This is the only behavioural difference between the two
    /// outcomes — both ultimately exit in the caller.
    #[test]
    fn quit_maps_stop_success_to_drain_then_exit() {
        let plan = plan_quit(Ok("{}".to_string()));
        match plan {
            QuitAction::DrainThenExit { drain, .. } => {
                assert_eq!(
                    drain,
                    Duration::from_millis(500),
                    "successful stop keeps the original 500ms drain delay"
                );
            }
            QuitAction::ExitImmediately { .. } => {
                panic!("a successful Stop must drain-then-exit; got {plan:?}")
            }
        }
    }

    /// THE regression test for #1429. An IPC round-trip whose future never
    /// resolves (peer accepted the connection but never replies) must NOT
    /// block the single menu event loop forever — `with_ipc_timeout` has to
    /// return a "not responding" `Err` once the deadline elapses, and do so
    /// promptly (bounded by the timeout, not the never-resolving future).
    #[tokio::test]
    async fn ipc_timeout_returns_not_responding_when_future_never_resolves() {
        let never = std::future::pending::<Result<String, String>>();
        let start = std::time::Instant::now();
        let result = with_ipc_timeout(Duration::from_millis(30), never).await;

        assert!(
            result.is_err(),
            "a never-resolving IPC future must time out, not hang (#1429); got {result:?}"
        );
        assert!(
            result.unwrap_err().contains("not responding"),
            "timeout should surface as a 'not responding' status"
        );
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "must return shortly after the 30ms deadline, not block on the stalled peer"
        );
    }

    /// The timeout wrapper is transparent to a future that resolves in time:
    /// both Ok and Err pass straight through unchanged.
    #[tokio::test]
    async fn ipc_timeout_passes_through_a_ready_result() {
        let ok = with_ipc_timeout(
            Duration::from_secs(5),
            std::future::ready(Ok::<String, String>("payload".to_string())),
        )
        .await;
        assert_eq!(ok, Ok("payload".to_string()));

        let err = with_ipc_timeout(
            Duration::from_secs(5),
            std::future::ready(Err::<String, String>("daemon error".to_string())),
        )
        .await;
        assert_eq!(
            err,
            Err("daemon error".to_string()),
            "an in-time error must pass through verbatim, not be masked as a timeout"
        );
    }
}
