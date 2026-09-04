// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Conductor Daemon - Background MIDI controller mapping service
//!
//! This is the main entry point for the Conductor daemon service.
//! It parses command-line arguments and launches the daemon infrastructure
//! with IPC control, config hot-reload, and state persistence.

// CLI binary: legitimate println!/eprintln! to stdout/stderr.
// See docs/epic-loop/rust-coverage.md Path A.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use clap::Parser;
use conductor_daemon::daemon::startup::resolve_startup_identity_and_path;
use conductor_daemon::{get_socket_path, run_daemon_with_identity};
use std::path::PathBuf;
use tracing::{error, info};
use tracing_subscriber::{EnvFilter, fmt, prelude::*, reload};

/// Conductor Daemon - MIDI controller mapping service
///
/// Transform MIDI devices into advanced macro pads with velocity sensitivity,
/// long press detection, double-tap, chord detection, and RGB LED feedback.
///
/// The daemon runs as a background service with:
/// - Config hot-reload (zero downtime)
/// - IPC control via Unix domain socket
/// - State persistence across restarts
/// - Performance metrics and health monitoring
///
/// Control the daemon using `conductorctl`:
///   conductorctl status   - Check daemon state
///   conductorctl reload   - Hot-reload configuration
///   conductorctl validate - Validate config without reloading
///   conductorctl ping     - Health check
///   conductorctl stop     - Graceful shutdown
#[derive(Parser, Debug)]
#[command(name = "conductor")]
#[command(version)]
#[command(about = "Conductor Daemon - MIDI Controller Mapping Service", long_about = None)]
struct Args {
    /// Path to configuration file
    ///
    /// Defaults to ~/Library/Application Support/conductor/config.toml on macOS
    /// or ~/.config/conductor/config.toml on Linux
    #[arg(short, long, value_name = "FILE")]
    config: Option<PathBuf>,

    /// Enable verbose logging (debug level)
    ///
    /// Sets logging level to DEBUG for all Conductor modules.
    /// Can also be controlled via RUST_LOG environment variable.
    #[arg(short, long)]
    verbose: bool,

    /// Enable trace-level logging
    ///
    /// Sets logging level to TRACE (very verbose, includes all events).
    /// Useful for diagnosing event processing issues.
    #[arg(short = 'T', long)]
    trace: bool,

    /// Run in foreground mode (don't detach)
    ///
    /// By default the daemon runs in foreground mode.
    /// Use systemd/launchd for proper background service management.
    #[arg(short, long)]
    foreground: bool,

    /// Ignore the user gamepad mapping override file
    /// (~/.conductor/gamecontrollerdb.txt).
    ///
    /// ADR-047 §D1. Use this to recover when a bad override file prevents a
    /// controller from being recognized: the daemon then uses only gilrs's
    /// bundled SDL mappings plus `SDL_GAMECONTROLLERCONFIG`. The override file
    /// is loaded at startup only (restart to apply edits).
    #[arg(long)]
    ignore_user_mappings: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Parse command line arguments
    let args = Args::parse();

    // Initialize logging. The reload handle is kept alive (required by tracing-subscriber)
    // but not yet wired to IPC — SetLogLevel persists to daemon.toml for next restart.
    // TODO(ADR-017): Pass reload handle into EngineManager for runtime log level changes.
    let _log_reload_handle = setup_logging(args.verbose, args.trace);

    info!("Conductor daemon starting");
    info!("Version: {}", env!("CARGO_PKG_VERSION"));

    // ADR-047 §D1: apply the user gamepad-mapping override toggle before the
    // gamepad subsystem initialises (process-wide; read when building Gilrs).
    conductor_daemon::gamepad_device::set_ignore_user_mappings(args.ignore_user_mappings);

    // Determine config path. Without an explicit `--config`, honour
    // the GUI's active-profile selection from `profiles.json` before
    // falling back to `<config_dir>/config.toml`.
    //
    // Short-circuit on `--config`: only
    // consult `get_default_config_dir()` when no explicit path was
    // given. systemd / launchd often launch services with HOME and
    // XDG_CONFIG_HOME unset; an explicit `--config /etc/conductor.toml`
    // must not fail just because the OS-default lookup can't find
    // `$HOME`.
    // An explicit `--config` is authoritative — the daemon ADOPTS it
    // (overwrites the live config). Track whether the path came from `--config`
    // vs default discovery so adoption happens only in the explicit case.
    // Default discovery also restores the active-profile IDENTITY —
    // primary source is the daemon's own `active_profile.json` (state dir),
    // with a one-time migration from the GUI's `profiles.json`. Explicit
    // `--config` boots are ephemeral: no identity restore, no persist.
    let (config_path, explicit_config, boot_identity) = match args.config {
        Some(path) => (path, true, None),
        None => {
            let config_dir = get_default_config_dir()?;
            let state_dir = conductor_daemon::daemon::state::get_state_dir()?;
            let (path, identity) = resolve_startup_identity_and_path(None, &config_dir, &state_dir);
            (path, false, identity.map(Into::into))
        }
    };
    if explicit_config {
        // An explicit --config is ADOPTED — it overwrites live.toml and
        // the daemon then boots from live.toml. Say so, so operator triage isn't
        // misled into thinking this path is the live runtime source.
        info!(
            "Explicit --config {} will be adopted as the live config (overwrites live.toml) before boot",
            config_path.display()
        );
    } else {
        info!("Using config: {}", config_path.display());
    }

    // Verify config file exists.
    //
    // The `AwaitingConfig` idle mode (stay up and accept bootstrap
    // IPCs when no config is resolvable) was never wired, so a missing
    // config is a clean, descriptive exit rather than a recoverable idle
    // state. Distinguish the explicit-path case (operator typo) from the
    // fresh-install case (nothing to resume) so the guidance is actionable.
    if !config_path.is_file() {
        error!("Config file not found: {}", config_path.display());
        if explicit_config {
            eprintln!(
                "Error: --config path does not exist or is not a regular file: {}",
                config_path.display()
            );
            eprintln!();
            eprintln!("Check the path, or omit --config to use the daemon's live config.");
        } else {
            eprintln!("Error: no configuration found at {}", config_path.display());
            eprintln!();
            eprintln!("Conductor has no config to start from. Either:");
            eprintln!("  • create a config at that path, or");
            eprintln!(
                "  • pass an existing one with `--config <path>` (it is adopted as the live config)."
            );
            eprintln!();
            eprintln!("Example config.toml:");
            eprintln!("{}", get_example_config());
        }
        std::process::exit(1);
    }

    info!("Config file: {}", config_path.display());

    // Show socket path for IPC
    let socket_path = get_socket_path()?;
    info!("IPC socket: {}", socket_path.display());

    // Run in foreground mode (tokio runtime required for async daemon)
    let rt = tokio::runtime::Runtime::new()?;

    info!(
        "Starting daemon service (foreground mode: {})",
        args.foreground
    );

    let result = rt.block_on(async {
        run_daemon_with_identity(config_path, explicit_config, boot_identity).await
    });

    match result {
        Ok(()) => {
            info!("Daemon stopped successfully");
            Ok(())
        }
        Err(e) => {
            error!("Daemon error: {}", e);
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}

/// Build an `EnvFilter` for Conductor modules at the given level.
///
/// Delegates precedence (`RUST_LOG` > `DEBUG` > level) and invalid-filter
/// degradation to `conductor_core::logging`, so the daemon, the GUI and the core
/// API cannot drift apart on what a given `RUST_LOG`/`DEBUG` value means.
fn build_filter(log_level: &str) -> EnvFilter {
    let default = format!(
        "conductor={},conductor_core={},conductor_daemon={},warn",
        log_level, log_level, log_level
    );
    // `debug: false` is deliberate: `setup_logging` has ALREADY folded DEBUG into
    // `log_level` (and `--trace`/`--verbose` outrank it there). Consulting DEBUG
    // again here would unconditionally return "debug" and silently downgrade an
    // explicit `--trace` — DEBUG=1 must not beat a flag the user typed.
    let filter_str = conductor_core::logging::resolve_filter_str(
        std::env::var("RUST_LOG").ok().as_deref(),
        false,
        &default,
    );
    conductor_core::logging::filter_from_str(&filter_str, &default)
}

/// Setup logging with tracing-subscriber and a reload handle for dynamic level changes.
///
/// Priority: RUST_LOG env var > CLI flags (--verbose/--trace) > daemon.toml > default ("info").
/// When RUST_LOG is set, it overrides all other sources via `EnvFilter::try_from_default_env`.
fn setup_logging(
    verbose: bool,
    trace: bool,
) -> reload::Handle<EnvFilter, tracing_subscriber::Registry> {
    // Note: RUST_LOG override is handled by EnvFilter::try_from_default_env() in build_filter().
    // We always compute the fallback level from CLI flags / daemon.toml so that invalid RUST_LOG
    // values still fall through to the correct priority order.
    let log_level = if trace {
        "trace".to_string()
    } else if verbose
        || conductor_core::logging::debug_env_enabled(std::env::var("DEBUG").ok().as_deref())
    {
        // DEBUG=1 has been documented for years but only ever read by the
        // (uncalled) core logging module — a silent no-op until now.
        "debug".to_string()
    } else {
        // Read daemon.toml for persisted log level (ADR-017)
        conductor_daemon::daemon::state::get_state_dir()
            .ok()
            .and_then(
                |dir| match conductor_core::config::preferences::load_daemon_settings(&dir) {
                    Ok(s) => {
                        let valid = ["error", "warn", "info", "debug", "trace"];
                        if valid.contains(&s.logging.level.as_str()) {
                            Some(s.logging.level)
                        } else {
                            eprintln!(
                                "Warning: invalid log level '{}' in daemon.toml, using 'info'",
                                s.logging.level
                            );
                            None
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "Warning: failed to load daemon.toml, using default log level: {}",
                            e
                        );
                        None
                    }
                },
            )
            .unwrap_or_else(|| "info".to_string())
    };

    let filter = build_filter(&log_level);

    // Create a reload layer so the filter can be changed at runtime
    let (filter_layer, reload_handle) = reload::Layer::new(filter);

    // Only log to the console when a human is actually watching one. When
    // the GUI spawns us, stdout is the unrotated `daemon-stdout.log` — keeping the
    // console layer there would duplicate every line into a file that grows
    // forever, alongside the rotating `daemon.<date>.log`. That file is for panics
    // and pre-logger output, not a second copy of the log.
    let console_layer = if conductor_core::logging::console_layer_enabled(
        std::io::IsTerminal::is_terminal(&std::io::stdout()),
        std::env::var("CONDUCTOR_LOG_CONSOLE").ok().as_deref(),
    ) {
        Some(
            fmt::layer()
                .with_target(true)
                .with_level(true)
                .with_thread_ids(false)
                .with_thread_names(false)
                .with_line_number(true),
        )
    } else {
        None
    };

    // Also write to a rotating file. A released `.app` launches the
    // daemon with no terminal attached, so console-only logging means a field
    // bug arrives with zero diagnostics. Console output stays as-is for anyone
    // running the binary by hand.
    let log_dir = conductor_core::logging::log_dir();
    let file_layer = match conductor_core::logging::component_appender(&log_dir, "daemon", 5) {
        Ok(appender) => Some(
            fmt::layer()
                .with_ansi(false)
                .with_writer(appender)
                .with_target(true)
                .with_level(true)
                .with_line_number(true),
        ),
        Err(e) => {
            // Never fatal: losing the log file must not stop the daemon.
            eprintln!(
                "Warning: file logging disabled ({}): {e}",
                log_dir.display()
            );
            None
        }
    };

    tracing_subscriber::registry()
        .with(filter_layer)
        .with(console_layer)
        .with(file_layer)
        .init();

    reload_handle
}

/// Resolve the Linux config base directory (the dir under which `conductor`
/// lives) from the relevant environment values.
///
/// Pure — environment is injected rather than read here — so it is testable
/// on every platform without mutating process-global state. Per the XDG Base
/// Directory spec, `XDG_CONFIG_HOME` takes precedence; `HOME` is only required
/// for the `$HOME/.config` fallback when `XDG_CONFIG_HOME` is unset or empty.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn linux_config_dir(
    xdg_config_home: Option<&str>,
    home: Option<&str>,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let config_home = match xdg_config_home {
        // XDG_CONFIG_HOME wins when set and non-empty — HOME is not consulted,
        // so a service env with XDG set but HOME unset resolves fine.
        Some(x) if !x.is_empty() => x.to_string(),
        // Unset or empty XDG_CONFIG_HOME falls back to $HOME/.config, which is
        // the only branch that actually requires HOME.
        _ => {
            let home = home.ok_or("HOME environment variable not set")?;
            format!("{home}/.config")
        }
    };
    Ok(PathBuf::from(config_home).join("conductor"))
}

/// Get the OS-specific Conductor config directory (without trailing
/// `config.toml`). This is the parent directory under which both
/// `config.toml` and `profiles/profiles.json` live.
fn get_default_config_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").map_err(|_| "HOME environment variable not set")?;
        Ok(PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("conductor"))
    }

    #[cfg(target_os = "linux")]
    {
        let xdg = std::env::var("XDG_CONFIG_HOME").ok();
        let home = std::env::var("HOME").ok();
        linux_config_dir(xdg.as_deref(), home.as_deref())
    }

    #[cfg(target_os = "windows")]
    {
        let appdata =
            std::env::var("APPDATA").map_err(|_| "APPDATA environment variable not set")?;
        Ok(PathBuf::from(appdata).join("conductor"))
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        Err("Unsupported platform".into())
    }
}

/// Get example config for error messages
fn get_example_config() -> &'static str {
    r#"[device]
name = "Mikro"
auto_connect = true

[advanced_settings]
chord_timeout_ms = 50
double_tap_timeout_ms = 300
hold_threshold_ms = 2000

[[modes]]
name = "Default"
color = "blue"

[[modes.mappings]]
description = "Pad 1 triggers Cmd+Space (Spotlight)"
[modes.mappings.trigger]
type = "Note"
note = 60

[modes.mappings.action]
type = "Keystroke"
keys = "space"
modifiers = ["cmd"]

[[global_mappings]]
description = "Emergency exit on Pad 16 (Note 75)"
[global_mappings.trigger]
type = "Note"
note = 75

[global_mappings.action]
type = "Shell"
command = "pkill conductor"
"#
}

#[cfg(test)]
mod tests {
    use super::*;

    // Regression: a service environment that sets XDG_CONFIG_HOME
    // but omits HOME must still resolve, using $XDG_CONFIG_HOME/conductor.
    #[test]
    fn linux_config_dir_prefers_xdg_when_home_unset() {
        let dir = linux_config_dir(Some("/some/config"), None)
            .expect("XDG_CONFIG_HOME alone must be sufficient");
        assert_eq!(dir, PathBuf::from("/some/config/conductor"));
    }

    #[test]
    fn linux_config_dir_falls_back_to_home_config_when_xdg_absent() {
        let dir = linux_config_dir(None, Some("/home/u")).expect("HOME fallback");
        assert_eq!(dir, PathBuf::from("/home/u/.config/conductor"));
    }

    // Per the XDG spec, an empty XDG_CONFIG_HOME is treated as unset.
    #[test]
    fn linux_config_dir_treats_empty_xdg_as_absent() {
        let dir =
            linux_config_dir(Some(""), Some("/home/u")).expect("empty XDG falls back to HOME");
        assert_eq!(dir, PathBuf::from("/home/u/.config/conductor"));
    }

    #[test]
    fn linux_config_dir_errors_when_both_unset() {
        assert!(linux_config_dir(None, None).is_err());
    }
}
