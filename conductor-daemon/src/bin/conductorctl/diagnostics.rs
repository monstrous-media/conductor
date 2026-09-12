// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Permissions probes, schema validation, and LLM budget display.

use super::*;

/// ADR-029 §D3 — `conductorctl permissions` handler.
///
/// Reports macOS Input Monitoring TCC grant state and optionally
/// deep-links to System Settings. On non-macOS, prints a short
/// "no consent gate required" message.
///
/// The daemon's grant is what users actually
/// care about, but a local gilrs probe in conductorctl's process
/// only tells us about *conductorctl's* identity (which TCC may
/// attribute to the parent terminal app, not the daemon). To get
/// the daemon's real grant we ask it via IPC. If the daemon isn't
/// running, fall back to a clearly-labelled local probe and tell
/// the user what they're seeing.
///
/// We also list both Conductor binary paths users need to grant
/// (GUI app + daemon) — TCC keys grants per-binary, so each one
/// needs its own.
///
/// `--check` and `--open-input-monitoring` can be combined; if
/// neither is passed, defaults to `--check`.
pub(crate) async fn handle_permissions(check: bool, open: bool, json: bool) -> Result<()> {
    use conductor_daemon::permissions::{
        OpenSettingsOutcome, check_input_monitoring, open_input_monitoring_settings,
    };

    // Default to --check if no flag is set.
    let do_check = check || !open;

    if do_check {
        // Try IPC to the running daemon first — that gives us the
        // daemon's authoritative grant, not conductorctl's.
        let daemon_status = query_daemon_permission().await;

        // Fall back to a local probe (in this conductorctl process)
        // when the daemon is unreachable. Clearly labelled so the
        // user knows what they're seeing.
        let local_status = if daemon_status.is_none() {
            Some(check_input_monitoring())
        } else {
            None
        };

        if json {
            let mut payload = serde_json::json!({
                "platform": std::env::consts::OS,
            });
            if let Some(ref s) = daemon_status {
                payload["source"] = serde_json::json!("daemon-ipc");
                let (label, detail) = permission_status_json_fields(s);
                payload["input_monitoring"] = serde_json::json!(label);
                if let Some(d) = detail {
                    payload["detail"] = serde_json::json!(d);
                }
            } else if let Some(ref s) = local_status {
                payload["source"] = serde_json::json!("local-probe");
                payload["daemon_running"] = serde_json::json!(false);
                let (label, detail) = permission_status_json_fields(s);
                payload["input_monitoring"] = serde_json::json!(label);
                if let Some(d) = detail {
                    payload["detail"] = serde_json::json!(d);
                }
            }
            println!("{}", serde_json::to_string_pretty(&payload)?);
        } else {
            print_human_check_output(daemon_status.as_ref(), local_status.as_ref());
        }
    }

    if open {
        let outcome = open_input_monitoring_settings()
            .map_err(|e| anyhow::anyhow!("Failed to open System Settings: {}", e))?;

        // The daemon caches its probe
        // result for 30 s. Without telling the daemon to bypass
        // that cache, a `conductorctl permissions --check`
        // immediately after the user grants permission would still
        // return the stale pre-grant value. Best-effort: ping the
        // daemon with `force: true` so its cache is invalidated.
        // If the daemon isn't running, there's nothing to
        // invalidate — silently swallow that case.
        let _ = force_daemon_probe_invalidation().await;

        if !json {
            match outcome {
                OpenSettingsOutcome::Opened => {
                    println!("Opening System Settings → Privacy & Security → Input Monitoring...");
                }
                OpenSettingsOutcome::NotApplicable => {
                    println!(
                        "--open-input-monitoring is a macOS-only operation. \
                         On Linux, Conductor uses udev rules + the `input` group. \
                         On Windows, no per-app consent gate applies."
                    );
                }
            }
        }
    }

    Ok(())
}

/// Best-effort: ask the running daemon to invalidate its probe
/// cache by sending a CheckPermissions request with `force: true`.
/// Used after `--open-input-monitoring` so a follow-up `--check`
/// sees the freshly-granted permission without waiting for the 30s
/// TTL. Errors are swallowed because they typically just mean
/// "daemon not running" — in which case there's no cache to clear.
pub(crate) async fn force_daemon_probe_invalidation() -> Option<()> {
    let socket_path = get_socket_path().ok()?.to_string_lossy().to_string();
    let mut client = IpcClient::new(socket_path).await.ok()?;
    let _ = client
        .send_command(
            IpcCommand::CheckPermissions,
            serde_json::json!({ "force": true }),
        )
        .await;
    Some(())
}

/// Try to connect to the running daemon and ask for its real
/// Input Monitoring grant state via the IpcCommand::CheckPermissions
/// handler. Returns None on connection or
/// IPC failure — caller falls back to a local probe.
pub(crate) async fn query_daemon_permission()
-> Option<conductor_daemon::permissions::PermissionStatus> {
    use conductor_daemon::permissions::PermissionStatus;
    use serde_json::Value;

    let socket_path = get_socket_path().ok()?.to_string_lossy().to_string();
    let mut client = IpcClient::new(socket_path).await.ok()?;
    let response = client
        .send_command(IpcCommand::CheckPermissions, Value::Null)
        .await
        .ok()?;
    let data = response.data?;

    // Daemon's payload: { platform, input_monitoring,
    //                     input_monitoring_granted: bool|null,
    //                     detail: string|null }
    let granted = data.get("input_monitoring_granted");
    let label = data
        .get("input_monitoring")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    Some(match (granted.and_then(|v| v.as_bool()), label) {
        (Some(true), _) => PermissionStatus::Granted,
        (Some(false), _) => PermissionStatus::NotGranted,
        (None, "not_applicable") => PermissionStatus::NotApplicable,
        (None, _) => {
            let detail = data
                .get("detail")
                .and_then(|v| v.as_str())
                .unwrap_or("daemon reported unknown")
                .to_string();
            PermissionStatus::Unknown(detail)
        }
    })
}

/// `--json` output keeps `input_monitoring`
/// to a stable enum (`granted | not_granted | unknown |
/// not_applicable`) and emits any human-readable detail in a
/// separate `detail` field. The previous shape (`"unknown: <reason>"`)
/// forced machine consumers to parse a string prefix to recover the
/// discriminant — fragile and likely to break if the reason text
/// ever contained a colon.
pub(crate) fn permission_status_json_fields(
    s: &conductor_daemon::permissions::PermissionStatus,
) -> (&'static str, Option<String>) {
    use conductor_daemon::permissions::PermissionStatus;
    match s {
        PermissionStatus::Granted => ("granted", None),
        PermissionStatus::NotGranted => ("not_granted", None),
        PermissionStatus::Unknown(reason) => ("unknown", Some(reason.clone())),
        PermissionStatus::NotApplicable => ("not_applicable", None),
    }
}

pub(crate) fn print_human_check_output(
    daemon_status: Option<&conductor_daemon::permissions::PermissionStatus>,
    local_status: Option<&conductor_daemon::permissions::PermissionStatus>,
) {
    use conductor_daemon::permissions::PermissionStatus;

    if cfg!(target_os = "macos") {
        println!("[macOS] Input Monitoring grants are required for both Conductor binaries:");
        println!();
        // Previously hard-coded paths
        // misled users on non-default installs (custom Homebrew
        // prefix, dev builds, non-`/Applications` GUI). Resolve the
        // daemon's actual location from conductorctl's own path —
        // the daemon binary lives next to conductorctl in every
        // shipped layout (Homebrew bin/, dev target/{debug,release}/,
        // tarball staging/). The GUI's location isn't discoverable
        // from the CLI, so we soften that path with "typically".
        let daemon_path = resolve_sibling_daemon_path();
        println!("  GUI app:    /Applications/Conductor.app (typical install path)");
        if let Some(p) = daemon_path {
            println!("  Daemon:     {} (resolved from this conductorctl)", p);
        } else {
            println!(
                "  Daemon:     /usr/local/bin/conductor (typical install path; \
                 couldn't resolve a sibling binary next to conductorctl)"
            );
        }
        println!();
    }

    if let Some(status) = daemon_status {
        match status {
            PermissionStatus::Granted => {
                println!("Daemon (running, via IPC): {}", "GRANTED".green());
            }
            PermissionStatus::NotGranted => {
                println!("Daemon (running, via IPC): {}", "NOT GRANTED".red());
                print_grant_instructions();
            }
            PermissionStatus::Unknown(reason) => {
                println!(
                    "Daemon (running, via IPC): {} ({})",
                    "UNKNOWN".yellow(),
                    reason
                );
                println!("Verify in System Settings → Privacy & Security → Input Monitoring.");
            }
            PermissionStatus::NotApplicable => {
                println!("{}", "No consent gate required on this platform.".dimmed());
                if std::env::consts::OS == "linux" {
                    println!(
                        "Linux uses udev rules + the `input` group. \
                         See SUPPORT.md for the install instructions."
                    );
                }
            }
        }
        if cfg!(target_os = "macos") {
            println!();
            println!(
                "{}",
                "(GUI app's grant can't be checked from the CLI — open the GUI to verify, \
                 or check System Settings directly.)"
                    .dimmed()
            );
        }
    } else if let Some(status) = local_status {
        match status {
            PermissionStatus::NotApplicable => {
                println!(
                    "Input Monitoring: {}",
                    "no consent gate required on this platform".dimmed()
                );
                if std::env::consts::OS == "linux" {
                    println!(
                        "Linux uses udev rules + the `input` group. \
                         See SUPPORT.md for the install instructions."
                    );
                }
            }
            _ => {
                println!(
                    "Daemon: {} (couldn't reach the running daemon)",
                    "UNKNOWN".yellow()
                );
                println!();
                println!(
                    "{}",
                    "Local probe (this conductorctl process — NOT the daemon's grant):".dimmed()
                );
                match status {
                    PermissionStatus::Granted => {
                        println!("  conductorctl probe: {}", "GRANTED".green());
                    }
                    PermissionStatus::NotGranted => {
                        println!("  conductorctl probe: {}", "NOT GRANTED".red());
                    }
                    PermissionStatus::Unknown(reason) => {
                        println!("  conductorctl probe: {} ({})", "UNKNOWN".yellow(), reason);
                    }
                    PermissionStatus::NotApplicable => unreachable!(),
                }
                println!();
                println!(
                    "Start the daemon for an authoritative answer, or grant via \
                     System Settings:"
                );
                print_grant_instructions();
            }
        }
    }
}

/// Try to find the daemon binary that lives
/// next to this conductorctl. Works for any install layout where the
/// two binaries ship in the same directory:
///   - Homebrew (any prefix): `<prefix>/bin/{conductor,conductorctl}`
///   - Tarball install: `staging/{conductor,conductorctl}`
///   - Dev: `target/{debug,release}/{conductor,conductorctl}`
///
/// Returns None if `current_exe()` fails (very rare — typically
/// only on platforms where /proc/self/exe-style introspection is
/// blocked) or if no sibling `conductor` binary exists. Caller
/// falls back to the "typical install path" string.
pub(crate) fn resolve_sibling_daemon_path() -> Option<String> {
    let me = std::env::current_exe().ok()?;
    let dir = me.parent()?;
    let candidate = dir.join("conductor");
    if candidate.exists() {
        Some(candidate.display().to_string())
    } else {
        None
    }
}

pub(crate) fn print_grant_instructions() {
    use conductor_daemon::permissions::INPUT_MONITORING_DEEPLINK;
    println!();
    println!("Open System Settings to grant:");
    println!("    open \"{}\"", INPUT_MONITORING_DEEPLINK);
    println!("Or run:");
    println!("    conductorctl permissions --open-input-monitoring");
}

/// Handle validate-schema subcommand — local validation, no daemon needed
pub(crate) fn handle_validate_schema(config_path: &Option<PathBuf>, json: bool) -> Result<()> {
    let path = config_path.clone().unwrap_or_else(|| {
        dirs::home_dir()
            .unwrap_or_default()
            .join(".conductor")
            .join("config.toml")
    });

    let config = Config::load(path.to_str().unwrap_or("config.toml"))
        .map_err(|e| anyhow::anyhow!("Failed to load config: {}", e))?;

    let report = conductor_core::config::validator::validate_config(&config);

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        // Errors
        if report.errors.is_empty() {
            println!("{}", "  No errors".green());
        } else {
            println!("{} ({}):", "Errors".red().bold(), report.errors.len());
            for f in &report.errors {
                println!("  {} {}: {}", "x".red(), f.path, f.message);
            }
        }

        // Warnings
        if !report.warnings.is_empty() {
            println!(
                "\n{} ({}):",
                "Warnings".yellow().bold(),
                report.warnings.len()
            );
            for f in &report.warnings {
                println!("  {} {}: {}", "!".yellow(), f.path, f.message);
            }
        }

        // Coverage
        println!("\n{}", "Protocol Coverage:".bold());
        print_coverage("MIDI", &report.coverage.midi);
        print_coverage("HID", &report.coverage.hid);
        print_coverage("OSC", &report.coverage.osc);

        if report.is_valid() {
            println!("\n{}", "Config is valid".green().bold());
        } else {
            println!(
                "\n{} ({} errors)",
                "Config has errors".red().bold(),
                report.errors.len()
            );
        }
    }

    Ok(())
}

/// Resolve the effective `[security.llm]` budget from raw config text.
/// `None` (no config file present) yields the ADR-027 §D6 defaults. Pure and
/// I/O-free so it is unit-testable; [`handle_llm_budgets_show`] supplies the
/// file read.
pub(crate) fn resolve_llm_budget_from_text(
    text: Option<&str>,
) -> Result<conductor_core::security::LlmBudgetConfig> {
    match text {
        Some(t) => Ok(conductor_core::security::SecurityConfig::from_toml_str(t)
            .map_err(|e| anyhow::anyhow!("Failed to parse [security.llm]: {}", e))?
            .llm),
        None => Ok(conductor_core::security::LlmBudgetConfig::default()),
    }
}

/// `conductorctl llm budgets show` — display the effective LLM agent budget
/// (ADR-027 §D6). Reads the file-only `[security.llm]` block; never touches the
/// daemon, so it works headless and offline.
pub(crate) fn handle_llm_budgets_show(config_path: &Option<PathBuf>, json: bool) -> Result<()> {
    let path = config_path.clone().unwrap_or_else(|| {
        dirs::home_dir()
            .unwrap_or_default()
            .join(".conductor")
            .join("config.toml")
    });

    let (budget, source) = match std::fs::read_to_string(&path) {
        Ok(text) => (
            resolve_llm_budget_from_text(Some(&text))?,
            path.display().to_string(),
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // No config file → the ADR defaults are in effect.
            (
                resolve_llm_budget_from_text(None)?,
                "ADR-027 §D6 defaults (no config file)".to_string(),
            )
        }
        Err(e) => {
            return Err(anyhow::anyhow!(
                "Failed to read config {}: {}",
                path.display(),
                e
            ));
        }
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&budget)?);
        return Ok(());
    }

    println!("{}", "LLM agent budget (ADR-027 D6)".bold());
    println!("  source: {}", source);
    println!("  termination: {:?}", budget.on_budget_exceeded);
    println!("  {}\n", "(a limit of 0 disables that dimension)".dimmed());

    let rows: [(&str, u64); 14] = [
        (
            "max_iterations_per_turn",
            budget.max_iterations_per_turn.into(),
        ),
        (
            "max_iterations_per_session",
            budget.max_iterations_per_session.into(),
        ),
        (
            "max_tool_calls_per_turn",
            budget.max_tool_calls_per_turn.into(),
        ),
        (
            "max_tool_calls_per_session",
            budget.max_tool_calls_per_session.into(),
        ),
        (
            "max_tokens_in_per_session",
            budget.max_tokens_in_per_session,
        ),
        (
            "max_tokens_out_per_session",
            budget.max_tokens_out_per_session,
        ),
        (
            "max_wall_clock_seconds_per_turn",
            budget.max_wall_clock_seconds_per_turn,
        ),
        (
            "max_wall_clock_seconds_per_session",
            budget.max_wall_clock_seconds_per_session,
        ),
        (
            "max_config_changes_per_session",
            budget.max_config_changes_per_session.into(),
        ),
        (
            "max_shell_exec_per_session",
            budget.max_shell_exec_per_session.into(),
        ),
        (
            "max_network_tool_calls_per_session",
            budget.max_network_tool_calls_per_session.into(),
        ),
        (
            "max_midi_out_per_session",
            budget.max_midi_out_per_session.into(),
        ),
        (
            "max_confirmations_requested_per_minute",
            budget.max_confirmations_requested_per_minute.into(),
        ),
        ("max_tokens_per_60sec", budget.max_tokens_per_60sec),
    ];
    for (name, value) in rows {
        let shown = if value == 0 {
            "disabled".dimmed().to_string()
        } else {
            value.to_string()
        };
        println!("  {:<40} {}", name, shown);
    }

    Ok(())
}

pub(crate) fn print_coverage(
    name: &str,
    metric: &conductor_core::config::validator::CoverageMetric,
) {
    println!(
        "  {}: {:.0}% ({}/{})",
        name,
        metric.percentage,
        metric.used.len(),
        metric.available.len()
    );
    if !metric.used.is_empty() {
        println!("    Used: {}", metric.used.join(", "));
    }
}
