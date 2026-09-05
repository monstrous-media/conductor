// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Conductor daemon control CLI
//!
//! Command-line interface for controlling the Conductor daemon.

// CLI binary: legitimate println!/eprintln! to stdout/stderr.
// See docs/epic-loop/rust-coverage.md Path A.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use colored::Colorize;
use conductor_core::Config;
// ADR-042 Phase B-early listener-approval CLI is Unix-only (the approval
// registry / keychain hardened-file APIs are `#[cfg(unix)]`); conductorctl
// itself talks over a Unix domain socket, so this is consistent.
#[cfg(unix)]
use conductor_core::security::keychain::select_keychain;
#[cfg(unix)]
use conductor_daemon::security::ApprovingSurface;
#[cfg(unix)]
use conductor_daemon::security::approval_admin;
use conductor_daemon::{IpcClient, IpcCommand, get_socket_path};
use serde_json::Value;
use std::collections::HashSet;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "conductorctl")]
#[command(about = "Control the Conductor daemon", long_about = None)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Enable verbose output
    #[arg(short, long, global = true)]
    verbose: bool,

    /// JSON output format
    #[arg(short, long, global = true)]
    json: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Check daemon status
    Status,

    /// Reload configuration
    Reload,

    /// Stop the daemon gracefully via IPC
    Shutdown,

    /// Validate configuration file
    Validate {
        /// Path to config file (defaults to daemon's current config)
        #[arg(short, long)]
        config: Option<PathBuf>,
    },

    /// Ping the daemon
    Ping,

    /// List available MIDI devices
    #[command(name = "list-devices")]
    ListDevices,

    /// Show resolved binding state from the daemon
    ///
    /// Reuses the existing IpcCommand::Status payload. Useful for headless
    /// operators who can't open the GUI Bindings panel and need to see
    /// which `[[bindings]]` aliases the daemon has actually resolved versus
    /// which ports are running opportunistically.
    Bindings {
        /// Filter to a single binding by device_id. For configured ports the
        /// device_id IS the `[[bindings]]` alias; for opportunistic ports the
        /// daemon prefixes the port name with `raw:`, so e.g. use
        /// `--alias "raw:IAC Driver Bus 1"` to filter to that port.
        #[arg(short, long)]
        alias: Option<String>,
        /// Show only opportunistic / unconfigured ports.
        #[arg(long)]
        unbound_only: bool,
    },

    /// Switch to a different MIDI device
    #[command(name = "set-device")]
    SetDevice {
        /// Port index of the device to switch to
        port: usize,
    },

    /// Get current MIDI device information
    #[command(name = "get-device")]
    GetDevice,

    // ============================================================================
    // Service Management Commands
    // ============================================================================
    /// Install Conductor as a system service (LaunchAgent)
    Install {
        /// Install daemon binary to /usr/local/bin
        #[arg(long)]
        install_binary: bool,

        /// Force reinstall even if already installed
        #[arg(short, long)]
        force: bool,
    },

    /// Uninstall Conductor service
    Uninstall {
        /// Also remove daemon binary from /usr/local/bin
        #[arg(long)]
        remove_binary: bool,

        /// Remove log files
        #[arg(long)]
        remove_logs: bool,
    },

    /// Start the daemon service
    Start {
        /// Wait for daemon to be ready (seconds)
        #[arg(short, long, default_value = "5")]
        wait: u64,
    },

    /// Stop the daemon service
    Stop {
        /// Force stop without graceful shutdown
        #[arg(short, long)]
        force: bool,
    },

    /// Restart the daemon service
    Restart {
        /// Wait for daemon to be ready (seconds)
        #[arg(short, long, default_value = "5")]
        wait: u64,
    },

    /// Enable auto-start on login
    Enable,

    /// Disable auto-start on login
    Disable,

    /// Show service installation status
    ServiceStatus,

    /// Migrate legacy [device] config to [[devices]] format (ADR-009),
    /// or legacy Trigger::Raw + MidiForward mappings to [[routes]]
    /// (ADR-036, with --routing).
    #[command(name = "migrate-config")]
    MigrateConfig {
        /// Path to config file (defaults to the OS config dir)
        #[arg(short, long)]
        config: Option<PathBuf>,
        /// Skip creating .bak backup file when writing
        #[arg(long)]
        no_backup: bool,
        /// Migrate legacy Trigger::Raw + MidiForward mappings to [[routes]]
        /// (ADR-036), preserving TOML comments.
        #[arg(long)]
        routing: bool,
        /// Preview the result without writing or creating a backup.
        #[arg(long)]
        dry_run: bool,
    },

    /// Validate config against protocol schemas with coverage report
    #[command(name = "validate-schema")]
    ValidateSchema {
        /// Path to config file (defaults to ~/.conductor/config.toml)
        #[arg(short, long)]
        config: Option<PathBuf>,
    },

    /// Rollback config to last known-good version
    #[command(name = "rollback-config")]
    RollbackConfig,

    /// Break-glass non-CAS rollback to last known-good version
    /// (ADR-034 §D6 / D4.B.4). Bypasses CAS at both
    /// step 2 and step 11 of the LiveConfig mutate seam. CLI-only:
    /// the daemon rejects this command from Gui / Llm peers.
    /// `--reason` is required and must be non-empty (it is audited
    /// and shown in the daemon log alongside the rollback).
    #[command(name = "rollback-config-force")]
    RollbackConfigForce {
        /// Operator justification (required, non-empty). Audited.
        #[arg(long, required = true)]
        reason: String,
    },

    /// Config IPC surface (ADR-034 §D4.C / §D9): operate the daemon's
    /// new config commands. Wraps the SaveConfig / ReloadFromDisk / ImportConfig
    /// / ConfigDriftStatus / MarkKnownGood IPC surface.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },

    /// Profile management
    Profile {
        #[command(subcommand)]
        action: ProfileAction,
    },

    /// Mode control + manual-override lock (ADR-040 D4)
    Mode {
        #[command(subcommand)]
        action: ModeAction,
    },

    /// LED feedback control
    Led {
        #[command(subcommand)]
        action: LedAction,
    },

    /// Plugin management
    Plugin {
        #[command(subcommand)]
        action: PluginAction,
    },

    /// Monitor input events in real-time
    Events {
        /// Follow mode — continuous live tail
        #[arg(short, long)]
        follow: bool,

        /// Filter by event type (note_on, note_off, cc, encoder, gamepad_button, etc.)
        #[arg(long, name = "type")]
        event_type: Option<String>,

        /// Filter by MIDI channel (1-16, displayed as human-friendly)
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..=16))]
        channel: Option<u8>,

        /// Filter by minimum note number (inclusive, 0-127)
        #[arg(long, value_parser = clap::value_parser!(u8).range(0..=127))]
        note_min: Option<u8>,

        /// Filter by maximum note number (inclusive, 0-127)
        #[arg(long, value_parser = clap::value_parser!(u8).range(0..=127))]
        note_max: Option<u8>,

        /// Filter by device ID
        #[arg(long)]
        device: Option<String>,

        /// Only show events newer than this duration ago (e.g., "5m", "30s", "1h")
        #[arg(long)]
        since: Option<String>,

        /// Debounce rapid events — minimum milliseconds between displayed events
        #[arg(long)]
        debounce: Option<u64>,

        /// Use a named filter from [event_console.filters] config section (R914)
        #[arg(long, short = 'F')]
        filter: Option<String>,

        /// Output format (text or json)
        #[arg(long, default_value = "text")]
        format: String,

        /// Max events to display (non-follow mode); does not cap --output file exports
        #[arg(long, default_value = "50")]
        limit: usize,

        /// Export events to file (JSON or CSV based on extension, snapshot mode only)
        #[arg(short, long, conflicts_with = "follow")]
        output: Option<PathBuf>,

        /// Duration to capture before exporting (e.g., "10s", "1m")
        #[arg(long, conflicts_with = "follow")]
        duration: Option<String>,

        /// Show profiling data (processing time, memory) when available (R921)
        #[arg(long)]
        profiling: bool,
    },

    /// Replay recorded events from a JSON or CSV file (R910)
    PlaybackEvents {
        /// Path to the recorded events file (JSON or CSV)
        file: PathBuf,

        /// Playback speed multiplier (e.g., 2.0 = double speed, 0.5 = half speed)
        #[arg(long, default_value = "1.0")]
        speed: f64,

        /// Output format (text or json)
        #[arg(long, default_value = "text")]
        format: String,

        /// Skip timing — display all events immediately without delays
        #[arg(long)]
        no_delay: bool,
    },

    /// Inspect or manage macOS TCC permission grants (ADR-029 §D3)
    ///
    /// On macOS, daemon binaries need explicit Input Monitoring grants
    /// to enumerate gamepads / joysticks. This command surfaces the
    /// current grant state and provides a deep-link to System Settings.
    /// On Linux / Windows, prints a "no consent gate required" message.
    Permissions {
        /// Probe the daemon's TCC grants and report the result.
        #[arg(long)]
        check: bool,

        /// Open System Settings → Privacy & Security → Input Monitoring
        /// (macOS only). On other platforms, prints equivalent guidance.
        #[arg(long = "open-input-monitoring")]
        open_input_monitoring: bool,
    },

    /// Observe the security audit log (ADR-027 §D13a)
    ///
    /// Surfaces what the daemon's security gates have logged —
    /// tool executions, plan decisions, and (most importantly)
    /// denials. "Security controls that are invisible are security
    /// theatre": this is the read-side that makes refusals visible.
    Audit {
        #[command(subcommand)]
        action: AuditAction,
    },

    /// Manage the MCP client registration table (ADR-027 §D18)
    ///
    /// MCP clients (Claude Desktop, Cursor, etc.) connect to the
    /// daemon via Unix domain sockets. With peer-credential auth
    /// (D1, Phase 1A) the daemon knows the connecting binary path;
    /// these subcommands let you explicitly grant per-client tier
    /// ceilings rather than treating every same-user binary as
    /// implicitly trusted.
    Mcp {
        #[command(subcommand)]
        action: McpAction,
    },

    /// Inspect the LLM agent budget (ADR-027 §D6)
    ///
    /// The multi-dimensional budget bounds the agentic loop —
    /// iterations, tool calls, tokens, wall-clock, and capability-
    /// specific quotas. It is configured file-only via the
    /// `[security.llm]` block (like the D17 egress allowlist) so a
    /// compromised LLM cannot widen its own limits.
    Llm {
        #[command(subcommand)]
        action: LlmAction,
    },

    /// Manage network-listener approvals (ADR-042 Phase B-early)
    ///
    /// Non-loopback OSC/Art-Net listeners only bind once manually approved.
    /// These commands inspect and edit the HMAC-signed approval registry at
    /// `~/.conductor/network_approvals.json`; the daemon honours an approval on
    /// its next (re)bind. Unix-only (the approval registry uses hardened-file APIs).
    #[cfg(unix)]
    Listener {
        #[command(subcommand)]
        action: ListenerAction,
    },

    /// Security key management (ADR-042 Phase B-early)
    #[cfg(unix)]
    Security {
        #[command(subcommand)]
        action: SecurityAction,
    },
}

/// `conductorctl listener` subcommands (ADR-042 §B.4).
#[cfg(unix)]
#[derive(Subcommand)]
enum ListenerAction {
    /// List network listeners and their approval status
    List {
        /// Path to config file (defaults to ~/.config/conductor/config.toml)
        #[arg(short, long)]
        config: Option<PathBuf>,
    },
    /// Detailed approval status (alias) for each network listener
    Status {
        /// Path to config file
        #[arg(short, long)]
        config: Option<PathBuf>,
    },
    /// Approve a non-loopback listener by alias
    Approve {
        /// Listener alias (from `[[endpoints]]`)
        alias: String,
        /// Path to config file
        #[arg(short, long)]
        config: Option<PathBuf>,
    },
    /// Revoke a listener's approval by alias
    Deny {
        /// Listener alias
        alias: String,
        /// Path to config file
        #[arg(short, long)]
        config: Option<PathBuf>,
    },
}

/// `conductorctl security` subcommands (ADR-042 §B.4 / D12).
#[cfg(unix)]
#[derive(Subcommand)]
enum SecurityAction {
    /// Show the network-approval HMAC key status (fingerprint, age, rotation
    /// warning)
    Status,
    /// Rotate the network-approval HMAC key (existing approvals are re-signed
    /// under the new key)
    RotateHmac,
}

/// `conductorctl llm` subcommands (ADR-027 §D6).
#[derive(Subcommand)]
enum LlmAction {
    /// Inspect the multi-dimensional LLM budget
    Budgets {
        #[command(subcommand)]
        action: BudgetsAction,
    },
}

/// `conductorctl llm budgets` subcommands (ADR-027 §D6).
#[derive(Subcommand)]
enum BudgetsAction {
    /// Show the effective `[security.llm]` budget for the session
    Show {
        /// Path to config file (defaults to ~/.conductor/config.toml)
        #[arg(short, long)]
        config: Option<PathBuf>,
        /// Emit the budget as JSON
        #[arg(long)]
        json: bool,
    },
}

/// `conductorctl mcp` subcommands (ADR-027 §D18). Storage is
/// a local JSON file (`~/.local/share/conductor/mcp_registry.json`
/// on macOS / Linux); these subcommands are file ops with no IPC
/// round-trip.
#[derive(Subcommand, Debug)]
enum McpAction {
    /// Register an MCP client (or update an existing registration).
    Register {
        /// Human-readable label.
        #[arg(long)]
        name: String,

        /// Absolute path to the client binary. Must match the
        /// connect-time `peer.initial_exe` exactly — the daemon
        /// looks up by exact equality, not glob or prefix.
        #[arg(long = "exe-path")]
        exe_path: PathBuf,

        /// Per-client tier ceiling. The client is allowed to invoke
        /// tools UP TO this tier; tool requests above it are denied.
        #[arg(long, value_parser = parse_audit_tier)]
        tier: conductor_daemon::daemon::audit::AuditRiskTier,
    },

    /// List all registered MCP clients.
    List,

    /// Revoke a registration by exe path.
    Revoke {
        /// Absolute path to the client binary. Idempotent — revoking
        /// an unregistered exe is a no-op (exit 0).
        #[arg(long = "exe-path")]
        exe_path: PathBuf,
    },
}

/// Parse an `AuditRiskTier` from a CLI string. Accepts BOTH:
///
/// - PascalCase ADR-027 names: `ReadOnly`, `Stateful`,
///   `ConfigChange`, `HardwareIO` — what humans type from the docs.
/// - snake_case serde wire names: `read_only`, `stateful`,
///   `config_change`, `hardware_io` — what `conductorctl mcp list
///   --json` emits via `AuditRiskTier::as_str()`, so piping that
///   output back into `register` works without explicit conversion.
///
/// `Internal` is intentionally NOT accepted from the CLI — it's a
/// system-only tier for daemon-originated operations.
fn parse_audit_tier(s: &str) -> Result<conductor_daemon::daemon::audit::AuditRiskTier, String> {
    use conductor_daemon::daemon::audit::AuditRiskTier;
    match s {
        "ReadOnly" | "read_only" => Ok(AuditRiskTier::ReadOnly),
        "Stateful" | "stateful" => Ok(AuditRiskTier::Stateful),
        "ConfigChange" | "config_change" => Ok(AuditRiskTier::ConfigChange),
        "HardwareIO" | "hardware_io" => Ok(AuditRiskTier::HardwareIO),
        _ => Err(format!(
            "invalid tier '{s}' — expected one of: ReadOnly, Stateful, ConfigChange, HardwareIO \
             (snake_case forms also accepted)"
        )),
    }
}

/// `conductorctl audit` subcommands (ADR-027 §D13a).
#[derive(Subcommand, Debug)]
enum AuditAction {
    /// Tail the audit log — recent entries, optionally followed live.
    Tail {
        /// Follow mode: stream new entries as they're logged
        /// (Ctrl+C to stop). Without this, prints the recent
        /// backlog and exits.
        #[arg(short, long)]
        follow: bool,

        /// How many recent entries to show before following
        /// (or to print and exit, in non-follow mode).
        #[arg(long, default_value = "50")]
        last: u32,
    },

    /// Show denial events only — gate refusals, deny-list
    /// rejections, plan-validation failures. Same flags as `tail`.
    Denied {
        /// Follow mode: stream new denials as they're logged.
        #[arg(short, long)]
        follow: bool,

        /// How many recent denials to show before following
        /// (or to print and exit, in non-follow mode).
        #[arg(long, default_value = "50")]
        last: u32,
    },

    /// Recover the daemon from the fail-closed audit-unavailable state
    /// (ADR-034 §D8). When the audit outbox failed to open (corrupt
    /// chain / I/O), config mutations are refused (`AuditDegraded`). This
    /// reopens it — rotating a corrupt file aside to
    /// `audit-outbox.log.corrupt-<ms>` and starting a fresh chain whose first
    /// record attests the reset — then leaves `AuditDegraded`. The rotated file
    /// is preserved for forensics.
    Resume,
}

/// `conductorctl config <action>` — the ADR-034 §D4.C / §D9 config IPC surface.
/// The remaining `save` action lands in a follow-up slice.
#[derive(Subcommand, Debug)]
enum ConfigAction {
    /// Show config-drift status: whether the on-disk user config has diverged
    /// from the daemon's live config (ADR-034 §D9 notify-only watcher). Read-only.
    Drift,
    /// Mark the daemon's current live config as the known-good snapshot
    /// (ADR-034 §D6) — the target a subsequent `rollback` returns to.
    #[command(name = "mark-known-good")]
    MarkKnownGood,
    /// Re-read the daemon's config file from disk and republish it (ADR-034
    /// §D2.2). The "I hand-edited `user.toml`, please pick it up" flow. With
    /// `--path`, loads that file instead (must be an allowlisted `.toml`).
    Reload {
        /// Optional path to load instead of the daemon's own config file.
        #[arg(long)]
        path: Option<PathBuf>,
    },
    /// Import a config from an explicit allowlisted `.toml` path (e.g. promoting
    /// a stash file or applying a profile snapshot). Same path-validation + CAS
    /// semantics as `reload`, but the path is required (ADR-034 §D2.2).
    Import {
        /// Path to the `.toml` config to import.
        path: PathBuf,
    },
    /// Commit a config read from **stdin** via `SaveConfig` (ADR-034 §D4.C). For
    /// pipelined / generated configs (CI, `yq`, `sed`). To load a FILE on disk,
    /// use `config import PATH` instead — that applies the daemon's path
    /// allowlist, whereas `save` sends a wholly client-constructed body, so a
    /// positional path is rejected (it would bypass the allowlist).
    Save {
        /// Rejected — `save` reads the config from stdin. Captured only so the
        /// error can redirect you to `config import` for files.
        #[arg(value_name = "PATH")]
        path: Option<PathBuf>,
        /// Pin the CAS base generation explicitly. Default: fetch the daemon's
        /// current generation via `GetConfigSnapshot`.
        #[arg(long)]
        base_generation: Option<u64>,
    },
}

#[derive(Subcommand, Debug)]
enum ProfileAction {
    /// Show active profile
    Status,
    /// Switch to a different profile config
    Switch {
        /// Profile name or path to config file (.toml)
        name_or_path: String,
    },
    /// Validate a profile config file (no daemon needed)
    Validate {
        /// Path to the profile config file (.toml)
        path: PathBuf,
    },
    /// List .toml profile files in a directory
    List {
        /// Directory to scan (default: ~/.config/conductor/profiles/)
        dir: Option<PathBuf>,
    },
    /// Create a new profile config
    Create {
        /// Profile name
        name: String,
        /// Bundle ID(s) to associate with this profile
        #[arg(long, num_args = 1..)]
        app: Vec<String>,
    },
    /// Delete a profile config
    Delete {
        /// Profile name
        name: String,
        /// Skip confirmation
        #[arg(long)]
        force: bool,
    },
}

/// `conductorctl mode …` — active-mode control and the manual-override lock
/// (ADR-040 D4 §4.2).
#[derive(Subcommand)]
enum ModeAction {
    /// Set the active mode. Locks it against auto-switching unless `--no-lock`.
    Set {
        /// Mode name
        name: String,
        /// Switch without locking (auto-switching stays active)
        #[arg(long)]
        no_lock: bool,
    },
    /// Release the manual mode lock, resuming auto-switching
    Unlock,
    /// Show the active mode and lock state
    Status,
}

#[derive(Subcommand)]
enum LedAction {
    /// Show current LED configuration and status
    Status,
    /// Set the lighting scheme
    Scheme {
        /// Scheme name (off, static, breathing, pulse, rainbow, wave, sparkle, reactive, vumeter, spiral)
        name: String,
    },
    /// Set LED brightness
    Brightness {
        /// Brightness level (0-127)
        level: u8,
    },
    /// Turn LEDs off (shortcut for scheme off)
    Off,
}

#[derive(Subcommand)]
enum PluginAction {
    /// List available and loaded plugins
    List,
    /// Show plugin details
    Info {
        /// Plugin name
        name: String,
    },
    /// Enable a plugin
    Enable {
        /// Plugin name
        name: String,
    },
    /// Disable a plugin
    Disable {
        /// Plugin name
        name: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize logging if verbose
    if cli.verbose {
        tracing_subscriber::fmt()
            .with_env_filter("conductor_daemon=debug")
            .init();
    }

    match execute_command(&cli).await {
        Ok(()) => Ok(()),
        Err(e) => {
            if !cli.json {
                eprintln!("{} {}", "Error:".red().bold(), e);
            } else {
                let error_json = serde_json::json!({
                    "status": "error",
                    "error": e.to_string()
                });
                println!("{}", serde_json::to_string_pretty(&error_json)?);
            }
            std::process::exit(1);
        }
    }
}

async fn execute_command(cli: &Cli) -> Result<()> {
    // Service management commands don't need IPC client
    match &cli.command {
        Commands::Install {
            install_binary,
            force,
        } => {
            return handle_install(*install_binary, *force, cli.json);
        }
        Commands::Uninstall {
            remove_binary,
            remove_logs,
        } => {
            return handle_uninstall(*remove_binary, *remove_logs, cli.json);
        }
        Commands::Start { wait } => {
            return handle_start(*wait, cli.json).await;
        }
        Commands::Stop { force } => {
            return handle_stop_service(*force, cli.json).await;
        }
        Commands::Restart { wait } => {
            return handle_restart(*wait, cli.json).await;
        }
        Commands::Enable => {
            return handle_enable(cli.json);
        }
        Commands::Disable => {
            return handle_disable(cli.json);
        }
        Commands::ServiceStatus => {
            return handle_service_status(cli.json);
        }
        Commands::MigrateConfig {
            config,
            no_backup,
            routing,
            dry_run,
        } => {
            if *routing {
                return handle_migrate_routing(config, *dry_run, *no_backup, cli.json);
            }
            // ADR-035: the legacy [[bindings]]/[[connectors]] and [device]
            // formats were removed entirely, so the identity/[device] migration
            // paths are gone. Only the ADR-036 routing migration remains.
            bail!(
                "migrate-config now supports only --routing (legacy [[bindings]]/[[connectors]]/[device] \
                 have been removed; author [[endpoints]] directly)."
            );
        }
        Commands::ValidateSchema { config } => {
            return handle_validate_schema(config, cli.json);
        }
        Commands::Permissions {
            check,
            open_input_monitoring,
        } => {
            // The handler is async because it tries
            // IPC first (to ask the running daemon for its real grant
            // state) before falling back to a local probe. The
            // existing dispatch above handles non-IPC commands
            // synchronously, so we await here directly and return.
            return handle_permissions(*check, *open_input_monitoring, cli.json).await;
        }
        Commands::Profile { action } => match action {
            ProfileAction::Validate { path } => {
                return handle_profile_validate(path, cli.json);
            }
            ProfileAction::List { dir } => {
                return handle_profile_list(dir, cli.json);
            }
            ProfileAction::Create { name, app } => {
                return handle_profile_create(name, app, cli.json);
            }
            ProfileAction::Delete { name, force } => {
                return handle_profile_delete(name, *force, cli.json);
            }
            _ => {} // Status and Switch need IPC
        },
        Commands::Mcp { action } => match action {
            McpAction::Register {
                name,
                exe_path,
                tier,
            } => {
                return handle_mcp_register(name, exe_path, *tier, cli.json);
            }
            McpAction::List => {
                return handle_mcp_list(cli.json);
            }
            McpAction::Revoke { exe_path } => {
                return handle_mcp_revoke(exe_path, cli.json);
            }
        },
        Commands::Llm { action } => match action {
            LlmAction::Budgets { action } => match action {
                BudgetsAction::Show { config, json } => {
                    return handle_llm_budgets_show(config, *json || cli.json);
                }
            },
        },
        #[cfg(unix)]
        Commands::Listener { action } => match action {
            ListenerAction::List { config } => {
                return handle_listener_list(config, cli.json, false);
            }
            ListenerAction::Status { config } => {
                return handle_listener_list(config, cli.json, true);
            }
            ListenerAction::Approve { alias, config } => {
                return handle_listener_approve(alias, config, cli.json);
            }
            ListenerAction::Deny { alias, config } => {
                return handle_listener_deny(alias, config, cli.json);
            }
        },
        #[cfg(unix)]
        Commands::Security { action } => match action {
            SecurityAction::Status => {
                return handle_security_status(cli.json);
            }
            SecurityAction::RotateHmac => {
                return handle_security_rotate_hmac(cli.json);
            }
        },
        _ => {} // Fall through to IPC commands
    }

    // IPC commands require connection to daemon
    let socket_path = get_socket_path()
        .context("Failed to determine IPC socket path")?
        .to_string_lossy()
        .to_string();

    let mut client = IpcClient::new(socket_path)
        .await
        .context("Failed to connect to daemon. Is the daemon running?")?;

    // Execute IPC command
    match &cli.command {
        Commands::Status => handle_status(&mut client, cli.json).await?,
        Commands::Reload => handle_reload(&mut client, cli.json).await?,
        Commands::Shutdown => handle_shutdown(&mut client, cli.json).await?,
        Commands::Validate { config } => handle_validate(&mut client, config, cli.json).await?,
        Commands::Ping => handle_ping(&mut client, cli.json).await?,
        Commands::ListDevices => handle_list_devices(&mut client, cli.json).await?,
        Commands::Bindings {
            alias,
            unbound_only,
        } => handle_bindings(&mut client, cli.json, alias.clone(), *unbound_only).await?,
        Commands::SetDevice { port } => handle_set_device(&mut client, *port, cli.json).await?,
        Commands::GetDevice => handle_get_device(&mut client, cli.json).await?,
        Commands::RollbackConfig => handle_rollback_config(&mut client, cli.json).await?,
        Commands::RollbackConfigForce { reason } => {
            handle_rollback_config_force(&mut client, reason, cli.json).await?
        }
        Commands::Config { action } => match action {
            ConfigAction::Drift => handle_config_drift(&mut client, cli.json).await?,
            ConfigAction::MarkKnownGood => {
                handle_config_mark_known_good(&mut client, cli.json).await?
            }
            ConfigAction::Reload { path } => {
                handle_config_reload(&mut client, path.as_deref(), cli.json).await?
            }
            ConfigAction::Import { path } => {
                handle_config_import(&mut client, path, cli.json).await?
            }
            ConfigAction::Save {
                path,
                base_generation,
            } => {
                handle_config_save(&mut client, path.as_deref(), *base_generation, cli.json).await?
            }
        },
        Commands::Profile { action } => match action {
            ProfileAction::Status => handle_profile_status(&mut client, cli.json).await?,
            ProfileAction::Switch { name_or_path } => {
                let path = resolve_profile_path(name_or_path)?;
                handle_profile_switch(&mut client, &path, cli.json).await?
            }
            ProfileAction::Validate { .. }
            | ProfileAction::List { .. }
            | ProfileAction::Create { .. }
            | ProfileAction::Delete { .. } => {
                // Already handled before IPC connection — should not reach here
                bail!("Internal error: local profile command reached IPC handler")
            }
        },
        Commands::Mode { action } => match action {
            ModeAction::Set { name, no_lock } => {
                handle_mode_set(&mut client, name, *no_lock, cli.json).await?
            }
            ModeAction::Unlock => handle_mode_unlock(&mut client, cli.json).await?,
            ModeAction::Status => handle_mode_status(&mut client, cli.json).await?,
        },
        Commands::Led { action } => handle_led(&mut client, action, cli.json).await?,
        Commands::Plugin { action } => handle_plugin(&mut client, action, cli.json).await?,
        Commands::Events {
            follow,
            event_type,
            channel,
            note_min,
            note_max,
            device,
            since,
            debounce,
            filter: named_filter,
            format,
            limit,
            output,
            duration,
            profiling,
        } => {
            // Build filter from CLI args
            // CLI uses 1-16 for channels (human-friendly), internal uses 0-15
            let since_ms = match since.as_deref() {
                Some(s) => {
                    let secs = parse_duration_str(Some(s)).ok_or_else(|| {
                        anyhow!("Invalid --since value '{}'. Use e.g. 30s, 5m, 1h", s)
                    })?;
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)?
                        .as_millis() as u64;
                    Some(now.saturating_sub(secs.saturating_mul(1000)))
                }
                None => None,
            };

            // Named filter from config (R914) — merges with any CLI flags
            let mut filter = conductor_daemon::EventFilter {
                event_type: event_type.clone(),
                channel: channel.map(|ch| ch.saturating_sub(1)),
                note_min: *note_min,
                note_max: *note_max,
                device_id: device.clone(),
                since_ms,
            };
            if let Some(name) = named_filter {
                let config = load_config_for_filter()?;
                let named = config
                    .event_console
                    .as_ref()
                    .and_then(|ec| ec.filters.get(name.as_str()))
                    .ok_or_else(|| {
                        anyhow!(
                            "Named filter '{}' not found in [event_console.filters] config",
                            name
                        )
                    })?;
                // Named filter values fill in any fields not already set by CLI flags
                if filter.event_type.is_none() {
                    filter.event_type = named.event_type.clone();
                }
                if filter.channel.is_none() {
                    filter.channel = named.channel;
                }
                if filter.note_min.is_none() {
                    filter.note_min = named.note_min;
                }
                if filter.note_max.is_none() {
                    filter.note_max = named.note_max;
                }
                if filter.device_id.is_none() {
                    filter.device_id = named.device_id.clone();
                }
            }
            handle_events(
                &mut client,
                *follow,
                filter,
                format,
                *limit,
                cli.json,
                output.clone(),
                duration.clone(),
                *debounce,
                *profiling,
            )
            .await?
        }
        Commands::PlaybackEvents {
            file,
            speed,
            format,
            no_delay,
        } => {
            handle_playback_events(file, *speed, format, *no_delay).await?;
        }
        Commands::Audit { action } => match action {
            AuditAction::Tail { follow, last } => {
                handle_audit(&mut client, false, *follow, *last, cli.json).await?
            }
            AuditAction::Denied { follow, last } => {
                handle_audit(&mut client, true, *follow, *last, cli.json).await?
            }
            AuditAction::Resume => handle_audit_resume(&mut client, cli.json).await?,
        },
        _ => unreachable!("Service commands handled above"),
    }

    Ok(())
}

async fn handle_status(client: &mut IpcClient, json: bool) -> Result<()> {
    let response = client
        .send_command(IpcCommand::Status, Value::Null)
        .await
        .context("Failed to get daemon status")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else {
        // Pretty print status
        if let Some(data) = response.data {
            println!("{}", "Conductor Daemon Status".bold().cyan());
            println!("{}", "─".repeat(50));

            if let Some(state) = data.get("state").and_then(|v| v.as_str()) {
                let state_colored = match state {
                    "Running" => state.green(),
                    "Reloading" => state.yellow(),
                    "Degraded" => state.red(),
                    _ => state.normal(),
                };
                println!("State:           {}", state_colored);
            }

            if let Some(mode) = data.get("current_mode").and_then(|v| v.as_str()) {
                println!("Current Mode:    {}", mode.cyan());
            }

            if let Some(config_path) = data.get("config_path").and_then(|v| v.as_str()) {
                println!("Config:          {}", config_path);
            }

            if let Some(uptime) = data.get("uptime_secs").and_then(|v| v.as_u64()) {
                println!("Uptime:          {}", format_duration(uptime));
            }

            if let Some(events) = data.get("events_processed").and_then(|v| v.as_u64()) {
                println!("Events:          {}", format_number(events));
            }

            if let Some(reloads) = data.get("config_reloads").and_then(|v| v.as_u64()) {
                println!("Config Reloads:  {}", reloads);
            }

            // Reload statistics
            if let Some(reload_stats) = data.get("reload_stats") {
                println!("\n{}", "Reload Performance".bold());
                println!("{}", "─".repeat(50));

                if let Some(last) = reload_stats.get("last_reload_ms").and_then(|v| v.as_u64()) {
                    println!("Last Reload:     {} ms", last);
                }
                if let Some(avg) = reload_stats.get("avg_reload_ms").and_then(|v| v.as_u64()) {
                    let grade = if avg < 20 {
                        "A".green()
                    } else if avg < 50 {
                        "B".yellow()
                    } else {
                        "C".red()
                    };
                    println!("Average:         {} ms (grade: {})", avg, grade);
                }
                if let Some(fastest) = reload_stats
                    .get("fastest_reload_ms")
                    .and_then(|v| v.as_u64())
                {
                    println!("Fastest:         {} ms", fastest);
                }
                if let Some(slowest) = reload_stats
                    .get("slowest_reload_ms")
                    .and_then(|v| v.as_u64())
                {
                    println!("Slowest:         {} ms", slowest);
                }
            }

            println!();
        }
    }

    Ok(())
}

async fn handle_reload(client: &mut IpcClient, json: bool) -> Result<()> {
    if !json {
        println!("Reloading configuration...");
    }

    let response = client
        .send_command(IpcCommand::Reload, Value::Null)
        .await
        .context("Failed to reload configuration")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else if let Some(data) = response.data {
        println!("{}", "✓ Configuration reloaded successfully".green());

        if let Some(duration) = data.get("reload_duration_ms").and_then(|v| v.as_u64()) {
            let grade = data
                .get("performance_grade")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let grade_colored = match grade {
                "A" => grade.green(),
                "B" => grade.yellow(),
                _ => grade.red(),
            };
            println!("Duration:  {} ms (grade: {})", duration, grade_colored);
        }

        if let Some(modes) = data.get("modes_loaded").and_then(|v| v.as_u64()) {
            println!("Modes:     {}", modes);
        }

        if let Some(mappings) = data.get("mappings_loaded").and_then(|v| v.as_u64()) {
            println!("Mappings:  {}", mappings);
        }
    }

    Ok(())
}

async fn handle_shutdown(client: &mut IpcClient, json: bool) -> Result<()> {
    if !json {
        println!("Stopping daemon...");
    }

    let response = client
        .send_command(IpcCommand::Stop, Value::Null)
        .await
        .context("Failed to stop daemon")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else {
        println!("{}", "✓ Daemon stopped successfully".green());
    }

    Ok(())
}

async fn handle_validate(
    client: &mut IpcClient,
    config_path: &Option<PathBuf>,
    json: bool,
) -> Result<()> {
    let args = if let Some(path) = config_path {
        serde_json::json!({ "path": path })
    } else {
        Value::Null
    };

    let response = client
        .send_command(IpcCommand::ValidateConfig, args)
        .await
        .context("Failed to validate configuration")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else if let Some(data) = response.data
        && let Some(valid) = data.get("valid").and_then(|v| v.as_bool())
    {
        if valid {
            println!("{}", "✓ Configuration is valid".green());

            if let Some(modes) = data.get("modes").and_then(|v| v.as_u64()) {
                println!("Modes:    {}", modes);
            }

            if let Some(mappings) = data.get("mappings").and_then(|v| v.as_u64()) {
                println!("Mappings: {}", mappings);
            }
        } else {
            println!("{}", "✗ Configuration is invalid".red());
        }
    }

    Ok(())
}

async fn handle_ping(client: &mut IpcClient, json: bool) -> Result<()> {
    let start = std::time::Instant::now();
    let response = client
        .send_command(IpcCommand::Ping, Value::Null)
        .await
        .context("Failed to ping daemon")?;

    let latency = start.elapsed();

    if json {
        let mut resp_json = serde_json::to_value(&response)?;
        if let Some(obj) = resp_json.as_object_mut() {
            obj.insert(
                "latency_ms".to_string(),
                serde_json::json!(latency.as_millis()),
            );
        }
        println!("{}", serde_json::to_string_pretty(&resp_json)?);
    } else {
        println!(
            "{} ({:.2} ms)",
            "✓ Daemon is responding".green(),
            latency.as_secs_f64() * 1000.0
        );
    }

    Ok(())
}

/// Print resolved bindings derived from the daemon's Status payload.
///
/// The data is already exposed via `IpcCommand::Status` →
/// `device_status.devices` — each entry's `is_configured` flag distinguishes
/// `[[bindings]]`-resolved ports from opportunistic listen-mode ports. This
/// handler just filters and pretty-prints; no daemon-side change required.
async fn handle_bindings(
    client: &mut IpcClient,
    json: bool,
    alias_filter: Option<String>,
    unbound_only: bool,
) -> Result<()> {
    let response = client
        .send_command(IpcCommand::Status, Value::Null)
        .await
        .context("Failed to get daemon status for bindings")?;

    let data = match response.data {
        Some(d) => d,
        None => {
            anyhow::bail!("Daemon Status returned no payload");
        }
    };

    let devices = data
        .get("device_status")
        .and_then(|d| d.get("devices"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let filtered: Vec<&Value> = filter_bindings(&devices, alias_filter.as_deref(), unbound_only);

    if json {
        // Keep both counts so consumers using `--alias` /
        // `--unbound-only` can validate against either the response set
        // (`bindings_count`) or the daemon's full enumeration
        // (`total_ports`). Misleading consumers with a single ambiguous
        // count was the original bug Copilot caught.
        let out = serde_json::json!({
            "listen_mode": data.get("listen_mode"),
            "total_ports": devices.len(),
            "bindings_count": filtered.len(),
            "bindings": filtered,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    println!("{}", "Conductor Daemon Bindings".bold().cyan());
    println!("{}", "─".repeat(50));

    if let Some(mode) = data.get("listen_mode").and_then(|v| v.as_str()) {
        println!("Listen mode:    {}", mode);
    }

    // Single-pass partition replaces the two-filter idiom.
    let (configured, opportunistic): (Vec<&Value>, Vec<&Value>) =
        filtered.iter().copied().partition(|d| {
            d.get("is_configured")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        });

    println!(
        "Total ports:    {} ({} configured, {} opportunistic)",
        filtered.len(),
        configured.len(),
        opportunistic.len()
    );

    if !configured.is_empty() && !unbound_only {
        println!("\n{}", "Configured (resolved via [[bindings]])".bold());
        for dev in &configured {
            print_binding_row(dev);
        }
    }

    if !opportunistic.is_empty() {
        println!(
            "\n{}",
            "Opportunistic (listen_mode=All, no [[bindings]] match)".bold()
        );
        for dev in &opportunistic {
            print_binding_row(dev);
        }
    }

    if filtered.is_empty() {
        if alias_filter.is_some() {
            println!("\nNo binding matches the given --alias filter.");
        } else if unbound_only {
            println!("\nNo opportunistic ports.");
        } else {
            println!("\nNo bindings reported by the daemon.");
        }
    }

    println!();
    Ok(())
}

/// A pure filter — separated from `handle_bindings` so it's exercisable
/// by unit tests without spinning up an IPC client. `alias_filter` matches
/// the device's `device_id` field **exactly** — for configured ports this is
/// the `[[bindings]]` alias, but for opportunistic ports the daemon prefixes
/// the port name with `raw:` (e.g. `raw:IAC Driver Bus 1`), so the caller
/// needs the full string. `unbound_only` keeps only devices where
/// `is_configured` is `false`.
fn filter_bindings<'a>(
    devices: &'a [Value],
    alias_filter: Option<&str>,
    unbound_only: bool,
) -> Vec<&'a Value> {
    devices
        .iter()
        .filter(|dev| {
            if let Some(a) = alias_filter {
                let id = dev.get("device_id").and_then(|v| v.as_str()).unwrap_or("");
                if id != a {
                    return false;
                }
            }
            if unbound_only {
                let is_configured = dev
                    .get("is_configured")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if is_configured {
                    return false;
                }
            }
            true
        })
        .collect()
}

/// Pretty-print one row of the bindings table.
///
/// Output handles missing optional fields (output_port_name / direction)
/// gracefully so callers don't panic on daemon payload changes.
fn print_binding_row(dev: &Value) {
    let id = dev.get("device_id").and_then(|v| v.as_str()).unwrap_or("?");
    let port = dev.get("port_name").and_then(|v| v.as_str()).unwrap_or("?");
    let connected = dev
        .get("connected")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let direction = dev
        .get("direction")
        .and_then(|v| v.as_str())
        .unwrap_or("Input");
    let output_port = dev.get("output_port_name").and_then(|v| v.as_str());
    let auto_paired = dev
        .get("output_auto_paired")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let dot = if connected {
        "●".green()
    } else {
        "○".dimmed()
    };
    let mut suffix = direction.to_string();
    if let Some(out) = output_port {
        suffix.push_str(&format!(", output={}", out));
        if auto_paired {
            suffix.push_str(" [auto-paired]");
        }
    }
    // Widened device_id column from 12 to 24 chars to fit
    // `raw:<port_name>` IDs without breaking the port-column alignment.
    // Real-world examples like `raw:IAC Driver Bus 1` are 20+ chars.
    println!("  {} {:<24} {:<32} ← {}", dot, id, port, suffix);
}

async fn handle_list_devices(client: &mut IpcClient, json: bool) -> Result<()> {
    let response = client
        .send_command(IpcCommand::ListDevices, Value::Null)
        .await
        .context("Failed to list input devices")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else if let Some(data) = response.data {
        println!("{}", "Available Input Devices".bold().cyan());
        println!("{}", "─".repeat(60));

        // Display MIDI devices
        if let Some(midi_devices) = data.get("midi_devices").and_then(|v| v.as_array()) {
            println!("\n{}", "MIDI Devices:".green().bold());
            if midi_devices.is_empty() {
                println!("  No MIDI devices found");
            } else {
                for device in midi_devices {
                    let port = device
                        .get("port_index")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let name = device
                        .get("port_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Unknown");
                    let connected = device
                        .get("connected")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);

                    let status = if connected {
                        " (connected)".green()
                    } else {
                        "".normal()
                    };

                    println!("  [{}] {}{}", port, name, status);
                }
            }
        }

        // Display HID/gamepad devices
        if let Some(hid_devices) = data.get("hid_devices").and_then(|v| v.as_array()) {
            println!(
                "\n{}",
                "HID Devices (Gamepads/Controllers):".yellow().bold()
            );
            if hid_devices.is_empty() {
                println!("  No HID devices found");
            } else {
                for device in hid_devices {
                    let index = device.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                    let name = device
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Unknown");
                    let uuid = device.get("uuid").and_then(|v| v.as_str()).unwrap_or("");

                    println!("  [{}] {} (UUID: {})", index, name, uuid);
                }
            }
        }

        println!();
    }

    Ok(())
}

async fn handle_set_device(client: &mut IpcClient, port: usize, json: bool) -> Result<()> {
    let args = serde_json::json!({ "port": port });

    let response = client
        .send_command(IpcCommand::SetDevice, args)
        .await
        .context("Failed to set MIDI device")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else if let Some(data) = response.data {
        if let Some(message) = data.get("message").and_then(|v| v.as_str()) {
            println!("{}", message.green());
        } else {
            println!(
                "{}",
                format!("✓ Switched to device at port {}", port).green()
            );
        }
    }

    Ok(())
}

async fn handle_get_device(client: &mut IpcClient, json: bool) -> Result<()> {
    let response = client
        .send_command(IpcCommand::GetDevice, Value::Null)
        .await
        .context("Failed to get current MIDI device")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else if let Some(data) = response.data
        && let Some(device) = data.get("device")
    {
        println!("{}", "Current MIDI Device".bold().cyan());
        println!("{}", "─".repeat(50));

        let connected = device
            .get("connected")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let status = if connected {
            "Connected".green()
        } else {
            "Disconnected".red()
        };

        println!("Status:     {}", status);

        if let Some(name) = device.get("name").and_then(|v| v.as_str()) {
            println!("Name:       {}", name);
        }

        if let Some(port) = device.get("port").and_then(|v| v.as_u64()) {
            println!("Port:       {}", port);
        }

        if let Some(last_event) = device.get("last_event_at").and_then(|v| v.as_u64()) {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let secs_ago = now.saturating_sub(last_event);
            println!("Last Event: {} ago", format_duration(secs_ago));
        }

        println!();
    }

    Ok(())
}

/// Format duration in seconds to human-readable string
fn format_duration(secs: u64) -> String {
    let days = secs / 86400;
    let hours = (secs % 86400) / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;

    if days > 0 {
        format!("{}d {}h {}m {}s", days, hours, minutes, seconds)
    } else if hours > 0 {
        format!("{}h {}m {}s", hours, minutes, seconds)
    } else if minutes > 0 {
        format!("{}m {}s", minutes, seconds)
    } else {
        format!("{}s", seconds)
    }
}

/// Format large numbers with comma separators
fn format_number(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    let mut count = 0;

    for c in s.chars().rev() {
        if count == 3 {
            result.push(',');
            count = 0;
        }
        result.push(c);
        count += 1;
    }

    result.chars().rev().collect()
}

// ============================================================================
// Service Management Implementation
// ============================================================================

/// Service configuration constants
mod service {
    use std::path::PathBuf;

    pub const SERVICE_LABEL: &str = "media.monstrous.conductor";
    pub const DAEMON_BINARY_NAME: &str = "conductor";

    pub fn get_plist_path() -> PathBuf {
        dirs::home_dir()
            .expect("Could not determine home directory")
            .join("Library/LaunchAgents")
            .join(format!("{}.plist", SERVICE_LABEL))
    }

    pub fn get_binary_install_path() -> PathBuf {
        PathBuf::from("/usr/local/bin").join(DAEMON_BINARY_NAME)
    }

    pub fn get_log_dir() -> PathBuf {
        dirs::home_dir()
            .expect("Could not determine home directory")
            .join("Library/Logs")
    }

    pub fn get_template_plist_path() -> Option<PathBuf> {
        // Try to find the template plist in the source tree
        let cargo_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
        let template_path = PathBuf::from(cargo_manifest_dir)
            .join("launchd")
            .join(format!("{}.plist", SERVICE_LABEL));

        if template_path.exists() {
            Some(template_path)
        } else {
            None
        }
    }
}

/// Check if running on macOS
fn is_macos() -> bool {
    cfg!(target_os = "macos")
}

/// Check if service is installed
fn is_service_installed() -> bool {
    service::get_plist_path().exists()
}

/// Check if daemon is running via IPC ping
async fn is_daemon_running() -> bool {
    match get_socket_path() {
        Ok(socket_path) => {
            if let Ok(mut client) = IpcClient::new(socket_path.to_string_lossy().to_string()).await
            {
                client
                    .send_command(IpcCommand::Ping, Value::Null)
                    .await
                    .is_ok()
            } else {
                false
            }
        }
        Err(_) => false,
    }
}

/// Install Conductor as a LaunchAgent service
fn handle_install(install_binary: bool, force: bool, json: bool) -> Result<()> {
    if !is_macos() {
        bail!("Service installation is currently only supported on macOS");
    }

    let plist_path = service::get_plist_path();

    // Check if already installed
    if is_service_installed() && !force {
        if json {
            let result = serde_json::json!({
                "status": "error",
                "error": "Service already installed. Use --force to reinstall."
            });
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else {
            eprintln!(
                "{}",
                "Service already installed. Use --force to reinstall.".yellow()
            );
        }
        return Ok(());
    }

    if !json {
        println!("{}", "Installing Conductor service...".bold());
    }

    // Step 1: Install binary if requested
    let binary_path = if install_binary {
        install_daemon_binary(json)?
    } else {
        // Try to find the binary in common locations
        find_daemon_binary()?
    };

    // Step 2: Create LaunchAgents directory
    let launch_agents_dir = plist_path
        .parent()
        .ok_or_else(|| anyhow!("Invalid plist path"))?;

    if !launch_agents_dir.exists() {
        std::fs::create_dir_all(launch_agents_dir)
            .context("Failed to create LaunchAgents directory")?;

        if !json {
            println!("  {} Created LaunchAgents directory", "✓".green());
        }
    }

    // Step 3: Create log directory
    let log_dir = service::get_log_dir();
    if !log_dir.exists() {
        std::fs::create_dir_all(&log_dir).context("Failed to create log directory")?;
    }

    if !json {
        println!(
            "  {} Created log directory: {}",
            "✓".green(),
            log_dir.display()
        );
    }

    // Step 4: Generate plist from template
    let plist_content = generate_plist(&binary_path)?;

    // Step 5: Write plist file
    std::fs::write(&plist_path, plist_content).context("Failed to write plist file")?;

    if !json {
        println!(
            "  {} Installed service plist: {}",
            "✓".green(),
            plist_path.display()
        );
    }

    // Step 6: Load the service (this enables it)
    let output = std::process::Command::new("launchctl")
        .args(["load", "-w", &plist_path.to_string_lossy()])
        .output()
        .context("Failed to execute launchctl")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Failed to load service: {}", stderr);
    }

    if json {
        let result = serde_json::json!({
            "status": "success",
            "plist_path": plist_path,
            "binary_path": binary_path,
            "enabled": true
        });
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!(
            "\n{}",
            "✓ Conductor service installed successfully".green().bold()
        );
        println!("  Binary:  {}", binary_path.display());
        println!("  Plist:   {}", plist_path.display());
        println!("  Status:  {}", "Enabled (will start on login)".green());
        println!("\nUse 'conductorctl start' to start the service now.");
    }

    Ok(())
}

/// Install daemon binary to /usr/local/bin
fn install_daemon_binary(json: bool) -> Result<PathBuf> {
    // Find the source binary (prefer release, fallback to debug)
    let cargo_target_dir =
        std::env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| "target".to_string());

    let source_binary = [
        PathBuf::from(&cargo_target_dir).join("release").join(service::DAEMON_BINARY_NAME),
        PathBuf::from(&cargo_target_dir).join("debug").join(service::DAEMON_BINARY_NAME),
    ]
    .into_iter()
    .find(|p| p.exists())
    .ok_or_else(|| anyhow!(
        "Could not find daemon binary. Build it first with 'cargo build --release --bin conductor'"
    ))?;

    let dest_binary = service::get_binary_install_path();

    // Copy binary
    std::fs::copy(&source_binary, &dest_binary)
        .context("Failed to copy binary. You may need sudo permissions.")?;

    // Make executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&dest_binary, perms)
            .context("Failed to set binary permissions")?;
    }

    if !json {
        println!(
            "  {} Installed binary: {}",
            "✓".green(),
            dest_binary.display()
        );
    }

    Ok(dest_binary)
}

/// Find daemon binary in common locations.
///
/// Returns an absolute path (canonicalized) so it works correctly in the
/// launchd plist regardless of WorkingDirectory setting.
fn find_daemon_binary() -> Result<PathBuf> {
    let candidates = vec![
        service::get_binary_install_path(),
        PathBuf::from("target/release").join(service::DAEMON_BINARY_NAME),
        PathBuf::from("target/debug").join(service::DAEMON_BINARY_NAME),
    ];

    candidates.into_iter()
        .find(|p| p.exists())
        .map(|p| p.canonicalize().unwrap_or(p))
        .ok_or_else(|| anyhow!(
            "Could not find daemon binary. Use --install-binary to install it, or build it with 'cargo build --release --bin conductor'"
        ))
}

/// Generate plist content from template
fn generate_plist(binary_path: &Path) -> Result<String> {
    let username = std::env::var("USER").context("Could not determine current username")?;

    // Load template or use embedded default
    let template = if let Some(template_path) = service::get_template_plist_path() {
        std::fs::read_to_string(&template_path).context("Failed to read plist template")?
    } else {
        // Embedded template
        include_str!("../../../conductor-daemon/launchd/media.monstrous.conductor.plist")
            .to_string()
    };

    // Replace placeholders
    let plist = template
        .replace("/usr/local/bin/conductor", &binary_path.to_string_lossy())
        .replace("/Users/USERNAME", &format!("/Users/{}", username));

    Ok(plist)
}

/// Uninstall Conductor service
fn handle_uninstall(remove_binary: bool, remove_logs: bool, json: bool) -> Result<()> {
    if !is_macos() {
        bail!("Service management is currently only supported on macOS");
    }

    if !is_service_installed() {
        if json {
            let result = serde_json::json!({
                "status": "error",
                "error": "Service not installed"
            });
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else {
            eprintln!("{}", "Service not installed".yellow());
        }
        return Ok(());
    }

    if !json {
        println!("{}", "Uninstalling Conductor service...".bold());
    }

    let plist_path = service::get_plist_path();

    // Step 1: Unload service
    let output = std::process::Command::new("launchctl")
        .args(["unload", "-w", &plist_path.to_string_lossy()])
        .output()
        .context("Failed to execute launchctl")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Don't fail if already unloaded
        if !stderr.contains("Could not find specified service") {
            eprintln!("Warning: {}", stderr);
        }
    }

    if !json {
        println!("  {} Stopped service", "✓".green());
    }

    // Step 2: Remove plist
    std::fs::remove_file(&plist_path).context("Failed to remove plist file")?;

    if !json {
        println!("  {} Removed plist: {}", "✓".green(), plist_path.display());
    }

    // Step 3: Remove binary if requested
    if remove_binary {
        let binary_path = service::get_binary_install_path();
        if binary_path.exists() {
            std::fs::remove_file(&binary_path)
                .context("Failed to remove binary. You may need sudo permissions.")?;

            if !json {
                println!(
                    "  {} Removed binary: {}",
                    "✓".green(),
                    binary_path.display()
                );
            }
        }
    }

    // Step 4: Remove logs if requested
    if remove_logs {
        let log_dir = service::get_log_dir();
        for log_file in ["conductor.log", "conductor.error.log"] {
            let log_path = log_dir.join(log_file);
            if log_path.exists() {
                std::fs::remove_file(&log_path).context("Failed to remove log file")?;
            }
        }

        if !json {
            println!("  {} Removed log files", "✓".green());
        }
    }

    if json {
        let result = serde_json::json!({
            "status": "success",
            "removed_plist": true,
            "removed_binary": remove_binary,
            "removed_logs": remove_logs
        });
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!(
            "\n{}",
            "✓ Conductor service uninstalled successfully"
                .green()
                .bold()
        );
    }

    Ok(())
}

/// Start the daemon service
async fn handle_start(wait_secs: u64, json: bool) -> Result<()> {
    if !is_macos() {
        bail!("Service management is currently only supported on macOS");
    }

    if !is_service_installed() {
        bail!("Service not installed. Run 'conductorctl install' first.");
    }

    // Check if already running
    if is_daemon_running().await {
        if json {
            let result = serde_json::json!({
                "status": "already_running"
            });
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else {
            println!("{}", "Daemon is already running".yellow());
        }
        return Ok(());
    }

    if !json {
        println!("Starting Conductor service...");
    }

    let plist_path = service::get_plist_path();

    // Load the service
    let output = std::process::Command::new("launchctl")
        .args(["load", &plist_path.to_string_lossy()])
        .output()
        .context("Failed to execute launchctl")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Ignore "already loaded" errors
        if !stderr.contains("Already loaded") {
            bail!("Failed to start service: {}", stderr);
        }
    }

    // Wait for daemon to be ready
    if wait_secs > 0 {
        if !json {
            print!("Waiting for daemon to be ready");
            std::io::Write::flush(&mut std::io::stdout())?;
        }

        let start = std::time::Instant::now();
        let timeout = Duration::from_secs(wait_secs);

        while start.elapsed() < timeout {
            tokio::time::sleep(Duration::from_millis(500)).await;

            if is_daemon_running().await {
                if !json {
                    println!(" {}", "✓".green());
                }
                break;
            }

            if !json {
                print!(".");
                std::io::Write::flush(&mut std::io::stdout())?;
            }
        }

        if !is_daemon_running().await {
            if !json {
                println!(" {}", "✗".red());
            }
            bail!(
                "Daemon did not start within {} seconds. Check logs for errors.",
                wait_secs
            );
        }
    }

    if json {
        let result = serde_json::json!({
            "status": "started",
            "ready": is_daemon_running().await
        });
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("{}", "✓ Service started successfully".green().bold());
    }

    Ok(())
}

/// Stop the daemon service
async fn handle_stop_service(force: bool, json: bool) -> Result<()> {
    if !is_macos() {
        bail!("Service management is currently only supported on macOS");
    }

    if !is_service_installed() {
        bail!("Service not installed");
    }

    if !json {
        println!("Stopping Conductor service...");
    }

    // Try graceful shutdown first unless force is specified
    if !force && is_daemon_running().await {
        if !json {
            println!("  Attempting graceful shutdown via IPC...");
        }

        if let Ok(socket_path) = get_socket_path()
            && let Ok(mut client) = IpcClient::new(socket_path.to_string_lossy().to_string()).await
        {
            let _ = client.send_command(IpcCommand::Stop, Value::Null).await;

            // Wait briefly for graceful shutdown
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    // Unload the service
    let plist_path = service::get_plist_path();
    let output = std::process::Command::new("launchctl")
        .args(["unload", &plist_path.to_string_lossy()])
        .output()
        .context("Failed to execute launchctl")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Ignore "not loaded" errors
        if !stderr.contains("Could not find specified service") {
            eprintln!("Warning: {}", stderr);
        }
    }

    // Verify stopped
    if is_daemon_running().await {
        bail!("Service may still be running. Check 'ps aux | grep conductor'");
    }

    if json {
        let result = serde_json::json!({
            "status": "stopped"
        });
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("{}", "✓ Service stopped successfully".green().bold());
    }

    Ok(())
}

/// Restart the daemon service
async fn handle_restart(wait_secs: u64, json: bool) -> Result<()> {
    if !json {
        println!("{}", "Restarting Conductor service...".bold());
    }

    // Stop
    handle_stop_service(false, json).await?;

    // Wait a moment
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Start
    handle_start(wait_secs, json).await?;

    Ok(())
}

/// Enable auto-start on login
fn handle_enable(json: bool) -> Result<()> {
    if !is_macos() {
        bail!("Service management is currently only supported on macOS");
    }

    if !is_service_installed() {
        bail!("Service not installed. Run 'conductorctl install' first.");
    }

    let plist_path = service::get_plist_path();

    // Load with -w flag enables auto-start
    let output = std::process::Command::new("launchctl")
        .args(["load", "-w", &plist_path.to_string_lossy()])
        .output()
        .context("Failed to execute launchctl")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Ignore "already loaded" errors
        if !stderr.contains("Already loaded") {
            bail!("Failed to enable service: {}", stderr);
        }
    }

    if json {
        let result = serde_json::json!({
            "status": "enabled"
        });
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!(
            "{}",
            "✓ Service enabled (will start on login)".green().bold()
        );
    }

    Ok(())
}

/// Disable auto-start on login
fn handle_disable(json: bool) -> Result<()> {
    if !is_macos() {
        bail!("Service management is currently only supported on macOS");
    }

    if !is_service_installed() {
        bail!("Service not installed");
    }

    let plist_path = service::get_plist_path();

    // Unload with -w flag disables auto-start
    let output = std::process::Command::new("launchctl")
        .args(["unload", "-w", &plist_path.to_string_lossy()])
        .output()
        .context("Failed to execute launchctl")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Ignore "not loaded" errors
        if !stderr.contains("Could not find specified service") {
            eprintln!("Warning: {}", stderr);
        }
    }

    if json {
        let result = serde_json::json!({
            "status": "disabled"
        });
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!(
            "{}",
            "✓ Service disabled (will not start on login)"
                .green()
                .bold()
        );
    }

    Ok(())
}

// ============================================================================
// Config Migration Implementation (ADR-009 Phase 6)
// ============================================================================

/// Resolve the config path for `migrate-config`, defaulting to the SAME location
/// the daemon and GUI use (`dirs::config_dir()/conductor/config.toml`) rather
/// than `~/.conductor/config.toml`. On macOS those differ (`config_dir()` is
/// `~/Library/Application Support/conductor`), so the old home-based default
/// pointed `migrate-config` at a file that doesn't exist — it errored/no-op'd
/// and the user's real config (the one the daemon loaded) was never migrated,
/// breaking the migration path ADR-035's deprecation warning advertises.
fn resolve_migrate_config_path(config_path: &Option<PathBuf>) -> Result<PathBuf> {
    match config_path {
        Some(p) => Ok(p.clone()),
        None => Ok(dirs::config_dir()
            .context("Could not determine config directory")?
            .join("conductor")
            .join("config.toml")),
    }
}

/// Handle the `migrate-config --routing` subcommand (ADR-036).
///
/// Rewrites legacy `Trigger::Raw` + `MidiForward` mappings into top-level
/// `[[routes]]` entries, preserving TOML comments via `toml_edit`. (ADR-036
/// Phase 3 removed the inverse `--reverse` direction — all routes are
/// post-mapping.)
fn handle_migrate_routing(
    config_path: &Option<PathBuf>,
    dry_run: bool,
    no_backup: bool,
    json: bool,
) -> Result<()> {
    use conductor_daemon::migration::migrate_raw_to_routes;

    let path = resolve_migrate_config_path(config_path)?;

    if !path.exists() {
        bail!("Config file not found: {}", path.display());
    }

    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read config file: {}", path.display()))?;

    let mut doc = content
        .parse::<toml_edit::DocumentMut>()
        .with_context(|| format!("Failed to parse config file: {}", path.display()))?;

    let report = migrate_raw_to_routes(&mut doc).map_err(|e| anyhow!("{}", e))?;

    let new_toml = doc.to_string();

    if json {
        let r = serde_json::json!({
            "status": if dry_run { "dry_run" } else { "migrated" },
            "direction": "forward",
            "path": path.display().to_string(),
            "rewrites": report.rewrites,
            "errors": report.errors,
            "migrated_toml": if dry_run { Some(new_toml.clone()) } else { None },
        });
        println!("{}", serde_json::to_string_pretty(&r)?);
    } else {
        for rewrite in &report.rewrites {
            println!("{} {}", "✓".green(), rewrite);
        }
        for err in &report.errors {
            println!("{} {}", "⚠".yellow(), err);
        }
        if report.rewrites.is_empty() && report.errors.is_empty() {
            println!("{}", "No routing migration needed.".green());
        }
    }

    if dry_run {
        if !json {
            println!("\n{}", "(dry-run) Resulting TOML:".cyan());
            println!("{}", new_toml);
            println!("Use without --dry-run to apply.");
        }
        return Ok(());
    }

    // True no-op (no rewrites, no errors): leave the file untouched and skip
    // the backup — there's nothing to migrate.
    if report.rewrites.is_empty() && report.errors.is_empty() {
        return Ok(());
    }

    // Write the migrated TOML, backing up the original first.
    if !no_backup {
        let backup_path = path.with_extension("toml.bak");
        std::fs::copy(&path, &backup_path)
            .with_context(|| format!("Failed to create backup at {}", backup_path.display()))?;
        if !json {
            println!("Backup: {}", backup_path.display());
        }
    }

    std::fs::write(&path, &new_toml)
        .with_context(|| format!("Failed to write migrated config: {}", path.display()))?;
    if !json {
        println!("Written: {}", path.display());
    }

    Ok(())
}

/// Show service installation and running status
fn handle_service_status(json: bool) -> Result<()> {
    if !is_macos() {
        bail!("Service management is currently only supported on macOS");
    }

    let installed = is_service_installed();
    let plist_path = service::get_plist_path();
    let binary_path = service::get_binary_install_path();
    let binary_exists = binary_path.exists();

    // Check if service is loaded with launchctl
    let list_output = std::process::Command::new("launchctl")
        .args(["list", service::SERVICE_LABEL])
        .output()
        .context("Failed to execute launchctl")?;

    let loaded = list_output.status.success();

    if json {
        let result = serde_json::json!({
            "installed": installed,
            "plist_path": plist_path,
            "binary_exists": binary_exists,
            "binary_path": binary_path,
            "loaded": loaded,
            "service_label": service::SERVICE_LABEL
        });
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("{}", "Conductor Service Status".bold().cyan());
        println!("{}", "─".repeat(50));

        let status = if installed && loaded {
            "Installed and Loaded".green()
        } else if installed {
            "Installed but Not Loaded".yellow()
        } else {
            "Not Installed".red()
        };

        println!("Status:          {}", status);
        println!("Service Label:   {}", service::SERVICE_LABEL);
        println!(
            "Plist:           {} {}",
            plist_path.display(),
            if installed {
                "✓".green()
            } else {
                "✗".red()
            }
        );
        println!(
            "Binary:          {} {}",
            binary_path.display(),
            if binary_exists {
                "✓".green()
            } else {
                "✗".red()
            }
        );

        if loaded {
            println!("\n{}", "Service is loaded (enabled)".green());
        } else if installed {
            println!(
                "\n{}",
                "Service is not loaded. Use 'conductorctl enable' or 'conductorctl start'."
                    .yellow()
            );
        } else {
            println!(
                "\n{}",
                "Service is not installed. Use 'conductorctl install'.".yellow()
            );
        }
    }

    Ok(())
}

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
async fn handle_permissions(check: bool, open: bool, json: bool) -> Result<()> {
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
async fn force_daemon_probe_invalidation() -> Option<()> {
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
async fn query_daemon_permission() -> Option<conductor_daemon::permissions::PermissionStatus> {
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
fn permission_status_json_fields(
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

fn print_human_check_output(
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
fn resolve_sibling_daemon_path() -> Option<String> {
    let me = std::env::current_exe().ok()?;
    let dir = me.parent()?;
    let candidate = dir.join("conductor");
    if candidate.exists() {
        Some(candidate.display().to_string())
    } else {
        None
    }
}

fn print_grant_instructions() {
    use conductor_daemon::permissions::INPUT_MONITORING_DEEPLINK;
    println!();
    println!("Open System Settings to grant:");
    println!("    open \"{}\"", INPUT_MONITORING_DEEPLINK);
    println!("Or run:");
    println!("    conductorctl permissions --open-input-monitoring");
}

/// Handle validate-schema subcommand — local validation, no daemon needed
fn handle_validate_schema(config_path: &Option<PathBuf>, json: bool) -> Result<()> {
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
fn resolve_llm_budget_from_text(
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
fn handle_llm_budgets_show(config_path: &Option<PathBuf>, json: bool) -> Result<()> {
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

fn print_coverage(name: &str, metric: &conductor_core::config::validator::CoverageMetric) {
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

/// Handle `conductorctl events` — real-time event monitoring
#[allow(clippy::too_many_arguments)]
async fn handle_events(
    client: &mut IpcClient,
    follow: bool,
    filter: conductor_daemon::EventFilter,
    format: &str,
    limit: usize,
    json_output: bool,
    output: Option<PathBuf>,
    duration: Option<String>,
    debounce_ms: Option<u64>,
    _profiling: bool,
) -> Result<()> {
    // Validate --duration BEFORE starting the monitor, so a typo like
    // `--duration nope` fails immediately instead of silently capturing the 2s
    // default. Omitted → 2s default (used only by the snapshot/export branch).
    let capture_secs = resolve_capture_secs(duration.as_deref())?;

    // Start event monitoring
    client
        .send_command(IpcCommand::StartEventMonitor, Value::Null)
        .await
        .context("Failed to start event monitor")?;

    // Set up Ctrl+C handler to stop monitoring
    let stop_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_flag_clone = Arc::clone(&stop_flag);
    let _ = ctrlc::set_handler(move || {
        stop_flag_clone.store(true, std::sync::atomic::Ordering::SeqCst);
    });

    if !json_output && !follow {
        let unit = if capture_secs == 1 {
            "second"
        } else {
            "seconds"
        };
        println!(
            "{}",
            format!("Monitoring events for {capture_secs} {unit}...").bold()
        );
    } else if !json_output {
        println!("{}", "Monitoring events (Ctrl+C to stop)...".bold().cyan());
        println!("{}", "─".repeat(70));
    }

    let mut all_events: Vec<serde_json::Value> = Vec::new();

    // Wrap event collection in a block so StopEventMonitor is always sent
    let result: Result<()> = async {
        if follow {
            // Follow mode: poll continuously
            let mut last_displayed_ms: u64 = 0;
            loop {
                if stop_flag.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }

                let response = client
                    .send_command(IpcCommand::GetMonitorEvents, Value::Null)
                    .await
                    .context("Failed to get monitor events")?;

                if let Some(events) = response
                    .data
                    .as_ref()
                    .and_then(|d| d.get("events"))
                    .and_then(|v| v.as_array())
                {
                    for event in events {
                        if !event_passes_filter(event, &filter) {
                            continue;
                        }
                        // Debounce: skip events too close together
                        if let Some(db) = debounce_ms {
                            let ts = event
                                .get("timestamp_ms")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0);
                            if ts > 0
                                && last_displayed_ms > 0
                                && ts.saturating_sub(last_displayed_ms) < db
                            {
                                continue;
                            }
                            if ts > 0 {
                                last_displayed_ms = ts;
                            }
                        }
                        if format == "json" {
                            println!("{}", serde_json::to_string(event).unwrap_or_default());
                        } else {
                            print_event_text(event);
                        }
                    }
                }

                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        } else {
            // Snapshot/export mode — capture window validated up front.
            tokio::time::sleep(Duration::from_secs(capture_secs)).await;

            let response = client
                .send_command(IpcCommand::GetMonitorEvents, Value::Null)
                .await
                .context("Failed to get monitor events")?;

            if let Some(events) = response
                .data
                .as_ref()
                .and_then(|d| d.get("events"))
                .and_then(|v| v.as_array())
            {
                let mut count = 0;
                for event in events {
                    // --limit bounds terminal display only; a file export (--output)
                    // must capture the whole window, otherwise a high-rate burst is
                    // silently truncated to the default 50 events.
                    if output.is_none() && count >= limit {
                        break;
                    }
                    if !event_passes_filter(event, &filter) {
                        continue;
                    }
                    if json_output || output.is_some() {
                        all_events.push(event.clone());
                    } else if format == "json" {
                        println!("{}", serde_json::to_string(event).unwrap_or_default());
                    } else {
                        print_event_text(event);
                    }
                    count += 1;
                }

                // Export to file if --output specified
                if let Some(ref output_path) = output {
                    export_events(&all_events, output_path)?;
                    println!(
                        "{} {} event(s) exported to {}",
                        "✓".green(),
                        all_events.len(),
                        output_path.display()
                    );
                } else if json_output {
                    println!("{}", serde_json::to_string_pretty(&all_events)?);
                } else if count == 0 {
                    println!("{}", "No events captured.".yellow());
                } else {
                    println!("\n{} event(s) captured.", count);
                }
            }
        }
        Ok(())
    }
    .await;

    // Always stop event monitoring, even on error
    let _ = client
        .send_command(IpcCommand::StopEventMonitor, Value::Null)
        .await;

    result
}

/// Check if a JSON event passes the filter. Returns false (skip) if:
/// - The event can't be deserialized and filters are active
/// - The event doesn't match the filter criteria
fn event_passes_filter(event: &serde_json::Value, filter: &conductor_daemon::EventFilter) -> bool {
    let has_active_filter = filter.event_type.is_some()
        || filter.channel.is_some()
        || filter.note_min.is_some()
        || filter.note_max.is_some()
        || filter.device_id.is_some()
        || filter.since_ms.is_some();

    match serde_json::from_value::<conductor_daemon::MonitorEvent>(event.clone()) {
        Ok(me) => filter.matches(&me),
        Err(_) => !has_active_filter, // Skip undeserializable events when filters are active
    }
}

/// Load config from default path for named filter lookup (R914)
fn load_config_for_filter() -> Result<Config> {
    let config_path = dirs::config_dir()
        .map(|d| d.join("conductor").join("config.toml"))
        .ok_or_else(|| anyhow!("Could not determine config directory"))?;
    let content = std::fs::read_to_string(&config_path)
        .with_context(|| format!("Failed to read config from {}", config_path.display()))?;
    let config: Config =
        toml::from_str(&content).context("Failed to parse config for named filter lookup")?;
    Ok(config)
}

// ── ADR-042 Phase B-early: network-listener approval CLI ──────────────────

/// `~/.conductor/network_approvals.json` — the HMAC-signed approval registry.
#[cfg(unix)]
fn approval_registry_path() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .context("Could not determine home directory")?
        .join(".conductor")
        .join("network_approvals.json"))
}

/// Load the daemon config used to resolve a listener's host/port/ACL by alias.
#[cfg(unix)]
fn load_listener_config(config: &Option<PathBuf>) -> Result<Config> {
    let path = match config {
        Some(p) => p.clone(),
        None => dirs::config_dir()
            .map(|d| d.join("conductor").join("config.toml"))
            .ok_or_else(|| anyhow!("Could not determine config directory"))?,
    };
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read config from {}", path.display()))?;
    toml::from_str(&content).with_context(|| format!("Failed to parse config {}", path.display()))
}

/// Resolve the network-approval HMAC key from the OS keychain.
#[cfg(unix)]
fn approval_key() -> Result<conductor_core::security::keychain::HmacKey> {
    let kc = select_keychain().map_err(|e| anyhow!("keychain unavailable: {e}"))?;
    kc.get_or_create_hmac_key()
        .map_err(|e| anyhow!("keychain key: {e}"))
}

#[cfg(unix)]
fn handle_listener_list(config: &Option<PathBuf>, json: bool, detailed: bool) -> Result<()> {
    let cfg = load_listener_config(config)?;
    let key = approval_key()?;
    let path = approval_registry_path()?;
    let statuses = approval_admin::statuses(&cfg, &path, &key);

    if json {
        let arr: Vec<Value> = statuses
            .iter()
            .map(|s| {
                serde_json::json!({
                    "alias": s.listener.alias,
                    "host": s.listener.host,
                    "port": s.listener.port,
                    "loopback": s.listener.is_loopback,
                    "approved": s.approved,
                    "registry_tampered": s.registry_tampered,
                    "requires_amplification_ack": s.listener.requires_amplification_ack,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "listeners": arr }))?
        );
        return Ok(());
    }

    if statuses.is_empty() {
        println!("No network listeners configured.");
        return Ok(());
    }
    for s in &statuses {
        let status = if s.listener.is_loopback {
            "loopback (auto-approved)".dimmed().to_string()
        } else if s.registry_tampered {
            "REGISTRY TAMPERED — re-approve".red().bold().to_string()
        } else if s.approved {
            "approved".green().to_string()
        } else {
            "PROMPT REQUIRED".yellow().to_string()
        };
        if detailed {
            let amp = if s.listener.requires_amplification_ack {
                "  (amplification ack required)"
            } else {
                ""
            };
            println!(
                "{:<20} {}:{:<6} [{}]{}",
                s.listener.alias.bold(),
                s.listener.host,
                s.listener.port,
                status,
                amp
            );
        } else {
            println!("{:<20} {}", s.listener.alias, status);
        }
    }
    Ok(())
}

#[cfg(unix)]
fn handle_listener_approve(alias: &str, config: &Option<PathBuf>, json: bool) -> Result<()> {
    let cfg = load_listener_config(config)?;
    let key = approval_key()?;
    let path = approval_registry_path()?;
    let info = approval_admin::approve(&cfg, &path, &key, alias, ApprovingSurface::Cli)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "status": "ok", "alias": alias, "loopback": info.is_loopback,
            }))?
        );
    } else if info.is_loopback {
        println!(
            "{} '{}' is loopback — already auto-approved (no record needed).",
            "✓".green(),
            alias
        );
    } else {
        println!(
            "{} approved listener '{}' ({}:{}). The daemon applies it on next (re)bind.",
            "✓".green(),
            alias,
            info.host,
            info.port
        );
    }
    Ok(())
}

#[cfg(unix)]
fn handle_listener_deny(alias: &str, config: &Option<PathBuf>, json: bool) -> Result<()> {
    let cfg = load_listener_config(config)?;
    let key = approval_key()?;
    let path = approval_registry_path()?;
    let removed = approval_admin::deny(&cfg, &path, &key, alias)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({ "status": "ok", "alias": alias, "removed": removed })
            )?
        );
    } else if removed {
        println!("{} revoked approval for listener '{}'.", "✓".green(), alias);
    } else {
        println!("No approval on file for listener '{}'.", alias);
    }
    Ok(())
}

#[cfg(unix)]
fn handle_security_status(json: bool) -> Result<()> {
    let kc = select_keychain().map_err(|e| anyhow!("keychain unavailable: {e}"))?;
    // Report-only: never the init hard-fail; show the level even if hard-expired.
    match conductor_daemon::security::key_rotation_status(kc.as_ref()) {
        Ok(status) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "hmac_key_fingerprint": status.fingerprint,
                        "hmac_key_age_days": status.age_days,
                        // Stable schema: a healthy key reports "ok", not null.
                        "hmac_key_warning": status.warning_tag().unwrap_or("ok"),
                    }))?
                );
            } else {
                println!("Network-approval HMAC key:");
                println!("  fingerprint:  {}", status.fingerprint);
                println!("  age:          {} days", status.age_days);
                match status.warning_tag() {
                    None => println!("  rotation:     {}", "ok".green()),
                    Some(tag) => {
                        let painted = if tag == "hard_expired" {
                            tag.red().bold().to_string()
                        } else {
                            tag.yellow().to_string()
                        };
                        println!("  rotation:     {painted}");
                        if let Some(msg) = status.level.message(status.age_days) {
                            println!("  {msg}");
                        }
                    }
                }
            }
        }
        Err(e) => {
            if json {
                println!(
                    "{}",
                    // Full schema even when unavailable: null fingerprint/age,
                    // warning "unavailable", plus a detail string.
                    serde_json::to_string_pretty(&serde_json::json!({
                        "hmac_key_fingerprint": serde_json::Value::Null,
                        "hmac_key_age_days": serde_json::Value::Null,
                        "hmac_key_warning": "unavailable",
                        "detail": e.to_string(),
                    }))?
                );
            } else {
                println!(
                    "Network-approval HMAC key: {} (no key initialized yet, or backend \
                     unavailable: {e})",
                    "unavailable".dimmed()
                );
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn handle_security_rotate_hmac(json: bool) -> Result<()> {
    let kc = select_keychain().map_err(|e| anyhow!("keychain unavailable: {e}"))?;
    let path = approval_registry_path()?;
    let fingerprint = approval_admin::rotate_hmac(kc.as_ref(), &path)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({ "status": "ok", "new_fingerprint": fingerprint })
            )?
        );
    } else {
        println!(
            "{} rotated the network-approval HMAC key (new fingerprint {}). \
             Existing approvals were re-signed under the new key.",
            "✓".green(),
            fingerprint
        );
    }
    Ok(())
}

/// Parse a duration string like "10s", "1m", "30" into seconds
fn parse_duration_str(s: Option<&str>) -> Option<u64> {
    let s = s?;
    let s = s.trim();
    if let Some(num) = s.strip_suffix('s') {
        num.parse().ok()
    } else if let Some(num) = s.strip_suffix('m') {
        num.parse::<u64>().ok().map(|m| m * 60)
    } else if let Some(num) = s.strip_suffix('h') {
        num.parse::<u64>().ok().map(|h| h * 3600)
    } else {
        s.parse().ok()
    }
}

/// Resolve the snapshot/export capture window (seconds) from `--duration`.
///
/// An OMITTED duration uses the 2-second default, but an explicit
/// unparsable value (e.g. `--duration 10seconds`) is an ERROR — matching the
/// `--since` behaviour — rather than silently falling back to 2 seconds and
/// producing an incomplete export without warning.
fn resolve_capture_secs(duration: Option<&str>) -> Result<u64> {
    match duration {
        None => Ok(2),
        Some(s) => parse_duration_str(Some(s))
            .ok_or_else(|| anyhow!("Invalid --duration value '{}'. Use e.g. 30s, 5m, 1h", s)),
    }
}

/// Export events to file (JSON or CSV based on extension)
/// CSV-safe string escaping: wraps in quotes if value contains comma, quote, or newline
fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Standard CSV column order for event export (shared between CLI and GUI)
const CSV_HEADER: &str =
    "timestamp_ms,event_type,device_id,channel,note,velocity,cc,value,button,axis";

fn format_event_csv_row(event: &serde_json::Value) -> String {
    let str_field =
        |key: &str| -> String { csv_escape(event.get(key).and_then(|v| v.as_str()).unwrap_or("")) };
    let num_field = |key: &str| -> String {
        event
            .get(key)
            .and_then(|v| v.as_u64())
            .map_or(String::new(), |v| v.to_string())
    };
    format!(
        "{},{},{},{},{},{},{},{},{},{}",
        num_field("timestamp_ms"),
        str_field("event_type"),
        str_field("device_id"),
        num_field("channel"),
        num_field("note"),
        num_field("velocity"),
        num_field("cc"),
        num_field("value"),
        num_field("button"),
        num_field("axis"),
    )
}

/// Export events to file (JSON or CSV based on extension)
fn export_events(events: &[serde_json::Value], path: &Path) -> Result<()> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("json");

    match ext {
        "csv" => {
            let mut wtr = std::io::BufWriter::new(
                std::fs::File::create(path)
                    .with_context(|| format!("Failed to create {}", path.display()))?,
            );
            use std::io::Write;
            writeln!(wtr, "{}", CSV_HEADER)?;
            for event in events {
                writeln!(wtr, "{}", format_event_csv_row(event))?;
            }
            Ok(())
        }
        _ => {
            // Default to JSON
            let json = serde_json::to_string_pretty(events)?;
            std::fs::write(path, json)
                .with_context(|| format!("Failed to write {}", path.display()))?;
            Ok(())
        }
    }
}

/// Format a monitor event as human-readable text
fn print_event_text(event: &serde_json::Value) {
    let ts = event
        .get("timestamp_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    // Convert timestamp to HH:MM:SS.mmm
    let secs = (ts / 1000) % 86400;
    let ms = ts % 1000;
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;

    let et = event
        .get("event_type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let dev = event
        .get("device_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let et_upper = et.to_uppercase();
    let et_display = format!("{:<20}", et_upper);

    let mut details = String::new();

    if let Some(note) = event.get("note").and_then(|v| v.as_u64()) {
        details.push_str(&format!("note:{:<4}", note));
    }
    if let Some(vel) = event.get("velocity").and_then(|v| v.as_u64()) {
        details.push_str(&format!("vel:{:<4}", vel));
    }
    if let Some(cc) = event.get("cc").and_then(|v| v.as_u64()) {
        details.push_str(&format!("cc:{:<4}", cc));
    }
    if let Some(val) = event.get("value").and_then(|v| v.as_u64()) {
        details.push_str(&format!("val:{:<4}", val));
    }
    if let Some(btn) = event.get("button").and_then(|v| v.as_u64()) {
        details.push_str(&format!("btn:{:<4}", btn));
    }
    if let Some(axis) = event.get("axis").and_then(|v| v.as_u64()) {
        details.push_str(&format!("axis:{:<4}", axis));
    }
    if let Some(ch) = event.get("channel").and_then(|v| v.as_u64())
        && ch > 0
    {
        details.push_str(&format!("ch:{:<3}", ch));
    }

    let dev_str = if dev.is_empty() {
        String::new()
    } else {
        format!("dev:{}", dev)
    };

    // Profiling data (R921)
    let mut prof_str = String::new();
    if let Some(proc_us) = event.get("processing_us").and_then(|v| v.as_u64()) {
        prof_str.push_str(&format!("proc:{}us ", proc_us));
    }
    if let Some(mem) = event.get("memory_bytes").and_then(|v| v.as_u64()) {
        let mb = mem as f64 / (1024.0 * 1024.0);
        prof_str.push_str(&format!("mem:{:.1}MB ", mb));
    }

    let prof_display = if prof_str.is_empty() {
        String::new()
    } else {
        format!(" {}", prof_str.trim())
    };

    println!(
        "[{:02}:{:02}:{:02}.{:03}] {} {} {}{}",
        h, m, s, ms, et_display, details, dev_str, prof_display
    );
}

/// Replay recorded events from a JSON or CSV file (R910)
async fn handle_playback_events(
    file: &Path,
    speed: f64,
    format: &str,
    no_delay: bool,
) -> Result<()> {
    use std::io::BufRead;

    let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("json");
    let content = std::fs::read_to_string(file)
        .with_context(|| format!("Failed to read {}", file.display()))?;

    let events: Vec<serde_json::Value> = match ext {
        "csv" => {
            let reader = std::io::BufReader::new(content.as_bytes());
            let mut events = Vec::new();
            let mut lines = reader.lines();
            // Skip header
            let header = lines.next().transpose()?.unwrap_or_default();
            let fields: Vec<&str> = header.split(',').collect();

            for line in lines {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                let values: Vec<&str> = line.split(',').collect();
                let mut event = serde_json::Map::new();
                for (i, field) in fields.iter().enumerate() {
                    if let Some(val) = values.get(i) {
                        let val = val.trim();
                        if val.is_empty() {
                            continue;
                        }
                        // Try parsing as number first
                        if let Ok(n) = val.parse::<u64>() {
                            event.insert(field.to_string(), serde_json::Value::from(n));
                        } else {
                            event.insert(
                                field.to_string(),
                                serde_json::Value::from(val.to_string()),
                            );
                        }
                    }
                }
                events.push(serde_json::Value::Object(event));
            }
            events
        }
        _ => {
            // JSON — could be array or newline-delimited
            if content.trim_start().starts_with('[') {
                serde_json::from_str(&content)
                    .with_context(|| "Failed to parse JSON events array")?
            } else {
                content
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .map(serde_json::from_str)
                    .collect::<Result<Vec<_>, _>>()
                    .with_context(|| "Failed to parse JSON lines")?
            }
        }
    };

    if events.is_empty() {
        println!("No events found in {}", file.display());
        return Ok(());
    }

    let speed = if speed <= 0.0 { 1.0 } else { speed };

    println!(
        "{}",
        format!(
            "Replaying {} events from {} (speed: {:.1}x)...",
            events.len(),
            file.display(),
            speed
        )
        .bold()
        .cyan()
    );

    let mut prev_ts: Option<u64> = None;
    for event in &events {
        // Timing delay between events
        if !no_delay && let Some(prev) = prev_ts {
            let ts = event
                .get("timestamp_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(prev);
            let delta = ts.saturating_sub(prev);
            if delta > 0 {
                let delay_ms = (delta as f64 / speed) as u64;
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
        }

        // Only update prev_ts if this event has a timestamp, to avoid
        // erasing the time gap for subsequent events.
        if let Some(ts) = event.get("timestamp_ms").and_then(|v| v.as_u64()) {
            prev_ts = Some(ts);
        }

        match format {
            "json" => println!("{}", serde_json::to_string(event)?),
            _ => print_event_text(event),
        }
    }

    println!(
        "{}",
        format!("Playback complete — {} events replayed.", events.len())
            .bold()
            .green()
    );

    Ok(())
}

/// ADR-027 D13a — observe the security audit log.
///
/// Always prints the recent backlog (`QueryAudit`). In `--follow`
/// mode, the live subscription is opened FIRST and the backlog is
/// queried second so entries logged in the hand-off window are
/// buffered on the stream rather than silently missed. The small
/// overlap is de-duplicated client-side by audit-entry id.
///
/// An explicit `{"lagged": n}` marker from the daemon is surfaced as
/// a stderr warning telling the operator to backfill from the
/// persistent log.
/// `conductorctl audit resume` — recover from the fail-closed
/// audit-unavailable brick (ADR-034 §D8).
///
/// Sends `ResumeAudit`, which asks the daemon to reopen the audit outbox:
/// a corrupt chain is rotated aside to `audit-outbox.log.corrupt-<ms>` and a
/// fresh chain is started whose first record attests the operator-driven
/// reset, after which the daemon leaves `AuditDegraded` and resumes accepting
/// config mutations. A healthy outbox is a no-op.
async fn handle_audit_resume(client: &mut IpcClient, json_output: bool) -> Result<()> {
    let response = client
        .send_command(IpcCommand::ResumeAudit, Value::Null)
        .await
        .context("Failed to send audit resume command")?;

    if let Some(err) = &response.error {
        bail!("Audit resume failed: {}", err.message);
    }

    if json_output {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }

    let recovered = response
        .data
        .as_ref()
        .and_then(|d| d.get("recovered"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let rotated_path = response
        .data
        .as_ref()
        .and_then(|d| d.get("rotated_path"))
        .and_then(|v| v.as_str());

    if recovered {
        match rotated_path {
            Some(path) => println!(
                "{} Audit resumed — rotated corrupt outbox to {}",
                "✓".green(),
                path.bold()
            ),
            None => println!("{} Audit resumed.", "✓".green()),
        }
    } else {
        println!(
            "{} Audit outbox already healthy — nothing to recover.",
            "✓".green()
        );
    }

    Ok(())
}

async fn handle_audit(
    client: &mut IpcClient,
    denied_only: bool,
    follow: bool,
    last: u32,
    json_output: bool,
) -> Result<()> {
    use tokio::io::AsyncBufReadExt;

    // --- Follow setup: subscribe BEFORE querying backlog so entries
    // logged in the hand-off window are buffered on the stream rather
    // than disappearing between the backlog snapshot and the
    // subscription becoming live.
    let mut stream_client = if follow {
        let socket_path = get_socket_path()
            .context("Failed to determine IPC socket path")?
            .to_string_lossy()
            .to_string();
        let mut stream_client = IpcClient::new(socket_path)
            .await
            .context("Failed to open audit stream connection")?;
        let ack = stream_client
            .send_command(
                IpcCommand::SubscribeAudit,
                serde_json::json!({ "denied_only": denied_only }),
            )
            .await
            .context("Failed to subscribe to audit stream")?;
        if let Some(err) = &ack.error {
            bail!("Audit subscription refused: {}", err.message);
        }
        Some(stream_client)
    } else {
        None
    };

    // --- Backlog: one-shot query of the persistent log ---
    let response = client
        .send_command(
            IpcCommand::QueryAudit,
            serde_json::json!({ "denied_only": denied_only, "limit": last }),
        )
        .await
        .context("Failed to query audit log")?;

    if let Some(err) = &response.error {
        bail!("Audit query failed: {}", err.message);
    }

    let mut backlog_ids = HashSet::new();
    if let Some(entries) = response
        .data
        .as_ref()
        .and_then(|d| d.get("entries"))
        .and_then(|e| e.as_array())
    {
        if entries.is_empty() && !json_output {
            let what = if denied_only { "denial" } else { "audit" };
            println!("(no {what} entries in the audit log yet)");
        }
        // The daemon returns most-recent-first; print oldest-first so
        // a follow stream reads naturally as a continuation.
        for entry in entries.iter().rev() {
            if follow && let Some(id) = entry.get("id").and_then(|v| v.as_str()) {
                backlog_ids.insert(id.to_string());
            }
            print_audit_entry(entry, json_output);
        }
    }

    if !follow {
        return Ok(());
    }

    if !json_output {
        eprintln!(
            "{}",
            "Following audit log (Ctrl+C to stop)...".bold().cyan()
        );
    }

    let stream_client = stream_client
        .take()
        .expect("follow mode initialized stream client");
    let mut reader = stream_client.into_reader();
    let mut line = String::new();
    loop {
        line.clear();
        tokio::select! {
            read = reader.read_line(&mut line) => {
                let n = read.context("Audit stream read error")?;
                if n == 0 {
                    // EOF — daemon closed the connection.
                    if !json_output {
                        eprintln!("{}", "Audit stream closed by daemon.".dimmed());
                    }
                    break;
                }
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let value: Value = serde_json::from_str(trimmed)
                    .context("Malformed audit stream line")?;
                // Explicit lag marker (impl-spec case 1J): tell the
                // operator to backfill from the persistent log.
                if let Some(lagged) = value.get("lagged").and_then(|v| v.as_u64()) {
                    eprintln!(
                        "{}",
                        format!(
                            "⚠ audit stream lagged by {lagged} events — \
                             re-run `conductorctl audit tail --last {}` to backfill",
                            lagged.max(last as u64)
                        )
                        .yellow()
                    );
                    continue;
                }
                if let Some(id) = value.get("id").and_then(|v| v.as_str())
                    && backlog_ids.remove(id)
                {
                    continue;
                }
                print_audit_entry(&value, json_output);
            }
            _ = tokio::signal::ctrl_c() => {
                if !json_output {
                    eprintln!("\n{}", "Stopped following audit log.".dimmed());
                }
                break;
            }
        }
    }

    Ok(())
}

/// Render a single audit entry. JSON mode prints the entry verbatim
/// (one JSON object per line — pipe-friendly). Text mode formats a
/// compact human line, highlighting denials.
fn print_audit_entry(entry: &Value, json_output: bool) {
    if json_output {
        println!("{}", entry);
        return;
    }

    let event_type = entry
        .get("event_type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let tool = entry
        .get("tool_name")
        .and_then(|v| v.as_str())
        .unwrap_or("-");
    let created_at = entry
        .get("created_at")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    // created_at is Unix milliseconds.
    let ts = chrono::DateTime::from_timestamp_millis(created_at)
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| created_at.to_string());

    let is_denial = event_type == "tool_denied";
    let label = if is_denial {
        event_type.red().bold()
    } else {
        event_type.normal()
    };

    let mut line = format!("{}  {}  {}", ts.dimmed(), label, tool);
    // For denials the reason lives in error_message — the gate
    // decision string (Deny(...) / RequirePlan / RequireConfirmation).
    if let Some(reason) = entry.get("error_message").and_then(|v| v.as_str()) {
        line.push_str(&format!("  {}", reason.yellow()));
    }
    // Caller identity, when present.
    if let Some(uc) = entry.get("user_context") {
        if let Some(client_id) = uc.get("client_id").and_then(|v| v.as_str()) {
            line.push_str(&format!("  {}", format!("[{client_id}]").cyan()));
        } else if let Some(uid) = uc.get("uid").and_then(|v| v.as_u64()) {
            line.push_str(&format!("  {}", format!("[uid={uid}]").cyan()));
        }
    }
    println!("{line}");
}

/// ADR-027 §D18 — `conductorctl mcp register`. Adds or
/// updates a registration in the on-disk table. Idempotent: same
/// `exe-path` overwrites name + tier rather than duplicating.
fn handle_mcp_register(
    name: &str,
    exe_path: &Path,
    tier: conductor_daemon::daemon::audit::AuditRiskTier,
    json_output: bool,
) -> Result<()> {
    use conductor_daemon::daemon::mcp_registry::{
        McpRegistration, McpRegistry, default_registry_path,
    };

    // Canonicalize
    // the user-supplied `--exe-path` (resolve symlinks, normalise
    // `..`/`.`) so the stored key matches what the kernel will
    // report at connect time. `PinnedPeer::initial_exe` is always
    // canonical; storing a non-canonical user input here would
    // silently never match.
    //
    // Canonicalization requires the file to EXIST — registering an
    // uninstalled binary is unsupported by design. The error
    // surface ("file not found") is the right UX for that case.
    if exe_path.is_relative() {
        anyhow::bail!(
            "--exe-path must be an absolute path (got: {})",
            exe_path.display()
        );
    }
    let canonical_exe = std::fs::canonicalize(exe_path).with_context(|| {
        format!(
            "Failed to canonicalize --exe-path {}. Does the file exist?",
            exe_path.display()
        )
    })?;
    let exe_path: &Path = &canonical_exe;

    let path = default_registry_path()
        .context("Failed to resolve default registry path (no data_local_dir)")?;
    let mut registry = McpRegistry::load(&path).context("Failed to load MCP registry")?;
    let was_existing = registry.lookup_tier(exe_path).is_some();
    registry.register(McpRegistration {
        name: name.to_string(),
        exe_path: exe_path.to_path_buf(),
        tier,
    });
    registry
        .save(&path)
        .context("Failed to save MCP registry")?;

    if json_output {
        println!(
            "{}",
            serde_json::json!({
                "status": "ok",
                "action": if was_existing { "updated" } else { "created" },
                "name": name,
                "exe_path": exe_path,
                "tier": tier.as_str(),
                "registry_path": path,
            })
        );
    } else {
        let verb = if was_existing {
            "Updated"
        } else {
            "Registered"
        };
        println!(
            "{} MCP client {} ({}) at tier {}",
            verb.green().bold(),
            name.bold(),
            exe_path.display(),
            tier.as_str().yellow(),
        );
        println!("  {}: {}", "Registry".dimmed(), path.display());
    }
    Ok(())
}

/// ADR-027 §D18 — `conductorctl mcp list`.
fn handle_mcp_list(json_output: bool) -> Result<()> {
    use conductor_daemon::daemon::mcp_registry::{McpRegistry, default_registry_path};

    let path = default_registry_path()
        .context("Failed to resolve default registry path (no data_local_dir)")?;
    let registry = McpRegistry::load(&path).context("Failed to load MCP registry")?;

    if json_output {
        println!(
            "{}",
            serde_json::json!({
                "registry_path": path,
                "count": registry.entries.len(),
                "entries": registry.entries,
            })
        );
        return Ok(());
    }

    if registry.entries.is_empty() {
        println!("(no MCP clients registered)");
        println!("  {}: {}", "Registry".dimmed(), path.display());
        return Ok(());
    }
    println!("{}", "Registered MCP clients:".bold());
    for entry in &registry.entries {
        println!(
            "  {}  {}  {}",
            entry.tier.as_str().yellow(),
            entry.name.bold(),
            entry.exe_path.display().to_string().dimmed(),
        );
    }
    println!("\n  {}: {}", "Registry".dimmed(), path.display());
    Ok(())
}

/// ADR-027 §D18 — `conductorctl mcp revoke`. Idempotent:
/// revoking a non-registered exe is a no-op (exit 0).
///
/// Canonicalize `--exe-path` the same way `handle_mcp_register`
/// does, so revoking through a symlink / `..`-bearing path that
/// resolves to the registered canonical entry actually removes it.
/// Pre-fix the literal user path was looked up against the canonical
/// stored entry and silently no-op'd — a permission-grant gap when
/// the operator believed they had revoked.
///
/// That canonicalize-or-die approach introduced a *second* gap: a
/// deleted/moved binary can't be canonicalized, so revocation
/// failed outright and the stale grant survived. Canonicalization is
/// now best-effort — see the body — so revocation by exe path works
/// whether or not the binary still exists on disk.
fn handle_mcp_revoke(exe_path: &Path, json_output: bool) -> Result<()> {
    use conductor_daemon::daemon::mcp_registry::{McpRegistry, default_registry_path};

    // Symmetry with `handle_mcp_register` — same absolute-path
    // requirement. The relative-path contract is unchanged.
    if exe_path.is_relative() {
        anyhow::bail!(
            "--exe-path must be an absolute path (got: {})",
            exe_path.display()
        );
    }

    // Canonicalize ONLY when the binary still exists. Pre-fix this
    // handler canonicalized unconditionally and bailed when the registered
    // client binary had been deleted/moved/was temporarily absent — so a
    // stale grant could *never* be revoked, and a future binary dropped at
    // the same path would inherit the stale tier ceiling. Revocation is "by
    // exe path" and must survive an absent binary.
    //
    //   - If the path resolves, revoke the canonical key — a symlinked /
    //     `..`-bearing path still matches the canonical stored entry.
    //   - If it does not resolve (deleted/moved), fall back to the literal
    //     absolute path, which matches an entry registered at an
    //     already-canonical path (the common case).
    //
    // `revoke()` removes only EXACT matches, so attempting both keys can
    // never remove an unintended entry.
    let literal = exe_path.to_path_buf();
    let canonical = std::fs::canonicalize(exe_path).ok();
    let revoke_key: &Path = canonical.as_deref().unwrap_or(&literal);

    let path = default_registry_path()
        .context("Failed to resolve default registry path (no data_local_dir)")?;
    let mut registry = McpRegistry::load(&path).context("Failed to load MCP registry")?;
    let removed = registry.revoke(revoke_key)
        // Belt-and-suspenders for a resolvable-but-symlinked path whose entry
        // was somehow stored non-canonically: also try the literal path.
        || (revoke_key != literal.as_path() && registry.revoke(&literal));
    let exe_path: &Path = revoke_key;
    // Skip the write when nothing
    // changed. A no-op revoke shouldn't touch the file's mtime,
    // shouldn't trigger config-watchers, and shouldn't cause an
    // atomic-rename round-trip for zero semantic effect.
    if removed {
        registry
            .save(&path)
            .context("Failed to save MCP registry")?;
    }

    if json_output {
        println!(
            "{}",
            serde_json::json!({
                "status": "ok",
                "removed": removed,
                "exe_path": exe_path,
                "registry_path": path,
            })
        );
    } else if removed {
        println!(
            "{} {} from MCP registry",
            "Revoked".green().bold(),
            exe_path.display()
        );
    } else {
        println!(
            "{} {} was not registered (no-op)",
            "Note:".yellow(),
            exe_path.display()
        );
    }
    Ok(())
}

/// Convert an IPC response into a Result so the caller can
/// propagate non-zero exit on daemon-reported errors. Pre-fix, both
/// rollback handlers printed `response.error.message` to stderr and
/// then returned `Ok(())` — automation invoking `conductorctl
/// rollback-config` in CI would treat a failed rollback as
/// successful (exit 0).
///
/// Returns `Err` if `response.status` is `Error` OR `response.error`
/// is Some. The error message includes the daemon's reported code
/// and message so callers can grep / dispatch on it.
///
/// Caller passes a `ctx` string (e.g. `"rollback"`) prepended to the
/// error for readability in shell scrollback.
fn check_ipc_response(
    response: &conductor_daemon::daemon::types::IpcResponse,
    ctx: &str,
) -> Result<()> {
    if matches!(response.status, conductor_daemon::ResponseStatus::Error)
        || response.error.is_some()
    {
        let msg = response
            .error
            .as_ref()
            .map(|e| format!("{} (code {})", e.message, e.code))
            .unwrap_or_else(|| "unknown error".to_string());
        bail!("{ctx} failed: {msg}");
    }
    Ok(())
}

async fn handle_rollback_config(client: &mut IpcClient, json: bool) -> Result<()> {
    let response = client
        .send_command(IpcCommand::RollbackConfig, Value::Null)
        .await
        .context("Failed to rollback config")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else if let Some(data) = &response.data {
        // D4.B.4: handler returns state_generation +
        // previous_generation + revision now (not modes/mappings).
        println!(
            "Config rolled back to known-good snapshot: gen {} → {} (revision: {})",
            data["previous_generation"], data["state_generation"], data["revision"]
        );
    } else if let Some(error) = &response.error {
        eprintln!("Rollback failed: {}", error.message);
    }

    // Propagate daemon-reported errors as non-zero exit, in
    // both JSON and non-JSON modes. Pre-fix, both modes returned
    // Ok(()) regardless — automation couldn't distinguish a failed
    // rollback from a successful one.
    check_ipc_response(&response, "rollback")
}

/// Break-glass non-CAS rollback (ADR-034 §D6 / D4.B.4).
/// Daemon-side enforces CLI-only + non-empty reason; we still echo
/// the operator's stated reason locally so a copy lands in the
/// shell scrollback alongside the daemon-log entry.
async fn handle_rollback_config_force(
    client: &mut IpcClient,
    reason: &str,
    json: bool,
) -> Result<()> {
    let response = client
        .send_command(
            IpcCommand::RollbackConfigForce,
            serde_json::json!({ "reason": reason }),
        )
        .await
        .context("Failed to force-rollback config")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else if let Some(data) = &response.data {
        println!(
            "BREAK-GLASS rollback: gen {} → {} (revision: {})",
            data["previous_generation"], data["state_generation"], data["revision"]
        );
        println!("Reason: {reason}");
    } else if let Some(error) = &response.error {
        eprintln!("Force-rollback failed: {}", error.message);
    }

    // Same fix as plain rollback — propagate daemon errors.
    check_ipc_response(&response, "force-rollback")
}

// ============================================================================
// Config IPC surface (ADR-034 §D4.C / §D9)
// ============================================================================

/// `conductorctl config drift` — query whether the on-disk user config has
/// drifted from the daemon's live config (ADR-034 §D9). Read-only.
async fn handle_config_drift(client: &mut IpcClient, json: bool) -> Result<()> {
    let response = client
        .send_command(IpcCommand::ConfigDriftStatus, Value::Null)
        .await
        .context("Failed to query config-drift status")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else if let Some(data) = &response.data {
        println!("Config drift status:");
        match data.as_object() {
            Some(fields) => {
                for (key, value) in fields {
                    println!("  {key}: {value}");
                }
            }
            None => println!("  {data}"),
        }
    } else if let Some(error) = &response.error {
        eprintln!("Drift query failed: {}", error.message);
    }

    check_ipc_response(&response, "config drift")
}

/// `conductorctl config mark-known-good` — mark the daemon's current live config
/// as the known-good snapshot (ADR-034 §D6) that a subsequent rollback targets.
async fn handle_config_mark_known_good(client: &mut IpcClient, json: bool) -> Result<()> {
    let response = client
        .send_command(IpcCommand::MarkKnownGood, Value::Null)
        .await
        .context("Failed to mark config known-good")?;

    // Decide success/failure FIRST: `check_ipc_response` also fails on an Error
    // status with no `error` field, so gate the success line on the real verdict
    // — never print "marked …" and then exit non-zero (Copilot review).
    let outcome = check_ipc_response(&response, "config mark-known-good");
    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else if outcome.is_ok() {
        println!("Marked the current live config as the known-good snapshot.");
    } else if let Some(error) = &response.error {
        eprintln!("mark-known-good failed: {}", error.message);
    }

    outcome
}

/// Fetch the daemon's current config generation — the CAS base a reload / import
/// pins its mutation on top of (ADR-034 §D2.1 optimistic concurrency). A
/// concurrent change bumps the generation and the daemon then returns
/// `StaleBaseGeneration`, so the operator knows to re-run against fresh state.
async fn fetch_base_generation(client: &mut IpcClient) -> Result<u64> {
    let response = client
        .send_command(IpcCommand::GetConfigSnapshot, Value::Null)
        .await
        .context("Failed to fetch the current config generation")?;
    check_ipc_response(&response, "config snapshot")?;
    response
        .data
        .as_ref()
        .and_then(|d| d.get("state_generation"))
        .and_then(serde_json::Value::as_u64)
        .context("daemon config snapshot did not include a u64 state_generation")
}

/// Render a reload/import success line + propagate the daemon verdict. Shared by
/// `config reload` and `config import` (their only difference is the message).
fn report_reload_outcome(
    response: &conductor_daemon::daemon::types::IpcResponse,
    json: bool,
    ctx: &str,
    success_msg: impl FnOnce(&str),
) -> Result<()> {
    let outcome = check_ipc_response(response, ctx);
    if json {
        println!("{}", serde_json::to_string_pretty(response)?);
    } else if outcome.is_ok() {
        let generation = response
            .data
            .as_ref()
            .and_then(|d| d.get("state_generation"))
            .map(|v| v.to_string())
            .unwrap_or_else(|| "?".to_string());
        success_msg(&generation);
    } else if let Some(error) = &response.error {
        eprintln!("{ctx} failed: {}", error.message);
    }
    outcome
}

/// `conductorctl config reload [--path PATH]` — re-read the daemon's config file
/// (or `--path`) from disk and republish it (ADR-034 §D2.2).
async fn handle_config_reload(
    client: &mut IpcClient,
    path: Option<&std::path::Path>,
    json: bool,
) -> Result<()> {
    let base_generation = fetch_base_generation(client).await?;
    let mut args = serde_json::json!({ "base_generation": base_generation });
    if let Some(p) = path {
        args["path"] = serde_json::Value::String(p.display().to_string());
    }
    let response = client
        .send_command(IpcCommand::ReloadFromDisk, args)
        .await
        .context("Failed to reload config from disk")?;

    report_reload_outcome(&response, json, "config reload", |generation| match path {
        Some(p) => println!(
            "Reloaded config from {} (now at generation {generation}).",
            p.display()
        ),
        None => {
            println!("Reloaded the daemon's config from disk (now at generation {generation}).")
        }
    })
}

/// `conductorctl config import PATH` — import a config from an explicit
/// allowlisted `.toml` path (ADR-034 §D2.2).
async fn handle_config_import(
    client: &mut IpcClient,
    path: &std::path::Path,
    json: bool,
) -> Result<()> {
    let base_generation = fetch_base_generation(client).await?;
    let args = serde_json::json!({
        "base_generation": base_generation,
        "path": path.display().to_string(),
    });
    let response = client
        .send_command(IpcCommand::ImportConfig, args)
        .await
        .context("Failed to import config")?;

    report_reload_outcome(&response, json, "config import", |generation| {
        println!(
            "Imported config from {} (now at generation {generation}).",
            path.display()
        );
    })
}

/// `conductorctl config save [--base-generation N]` — commit a config read from
/// **stdin** via `SaveConfig` (ADR-034 §D4.C).
/// "save = bodies, import = paths": `save` sends a wholly
/// client-constructed config body, so it never touches the daemon's §D2.2 path
/// allowlist — a positional path is therefore rejected with a redirect to
/// `config import`, which DOES apply the allowlist (no path-shaped bypass).
async fn handle_config_save(
    client: &mut IpcClient,
    path: Option<&std::path::Path>,
    base_generation: Option<u64>,
    json: bool,
) -> Result<()> {
    // Reject a positional path — `save` is stdin-only by design.
    if let Some(p) = path {
        bail!(
            "`config save` commits a config body read from stdin; it does not take a path. \
             To load a config FILE (under the daemon's path allowlist), use: \
             `conductorctl config import {}`",
            p.display()
        );
    }
    // Don't hang on an interactive terminal — require piped input.
    if std::io::stdin().is_terminal() {
        bail!(
            "`config save` reads the config from stdin — pipe one in \
             (e.g. `cat config.toml | conductorctl config save`), or use \
             `conductorctl config import PATH` to load a file"
        );
    }

    let mut toml_text = String::new();
    // Lock stdin into a local (consistent with the other handlers; avoids a
    // `&mut` borrow of the temporary `std::io::stdin()` returns).
    let mut stdin = std::io::stdin().lock();
    std::io::Read::read_to_string(&mut stdin, &mut toml_text)
        .context("Failed to read config from stdin")?;
    if toml_text.trim().is_empty() {
        bail!("`config save` read an empty config from stdin");
    }
    let config: Config = toml::from_str(&toml_text).context("stdin is not valid config TOML")?;
    let config_json =
        serde_json::to_value(&config).context("failed to serialize the parsed config")?;

    // Use the explicit base generation, else pin the daemon's current one (CAS).
    let base_generation = match base_generation {
        Some(g) => g,
        None => fetch_base_generation(client).await?,
    };
    let args = serde_json::json!({
        "config": config_json,
        "base_generation": base_generation,
    });
    let response = client
        .send_command(IpcCommand::SaveConfig, args)
        .await
        .context("Failed to save config")?;

    report_reload_outcome(&response, json, "config save", |generation| {
        println!("Saved config from stdin (now at generation {generation}).");
    })
}

// ============================================================================
// Profile Management
// ============================================================================

/// `conductorctl mode set <name> [--no-lock]` (ADR-040 D4 §4.2).
async fn handle_mode_set(
    client: &mut IpcClient,
    name: &str,
    no_lock: bool,
    json: bool,
) -> Result<()> {
    let lock = !no_lock;
    let response = client
        .send_command(
            IpcCommand::SetMode,
            serde_json::json!({ "mode": name, "lock": lock }),
        )
        .await
        .context("Failed to set mode")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    }
    // Validate even in --json mode so daemon errors exit non-zero.
    check_ipc_response(&response, "set mode")?;
    if json {
        return Ok(());
    }
    if lock {
        println!("Mode set to {} {}", name.green(), "(locked)".dimmed());
    } else {
        println!(
            "Mode set to {} {}",
            name.green(),
            "(auto-switch active)".dimmed()
        );
    }
    Ok(())
}

/// `conductorctl mode unlock` (ADR-040 D4 §4.2).
async fn handle_mode_unlock(client: &mut IpcClient, json: bool) -> Result<()> {
    let response = client
        .send_command(IpcCommand::UnlockMode, Value::Null)
        .await
        .context("Failed to unlock mode")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    }
    check_ipc_response(&response, "unlock mode")?;
    if json {
        return Ok(());
    }
    let was_locked = response
        .data
        .as_ref()
        .and_then(|d| d.get("unlocked"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if was_locked {
        println!("{}", "Mode unlocked — auto-switching resumed".green());
    } else {
        println!("{}", "No mode lock was held".yellow());
    }
    Ok(())
}

/// `conductorctl mode status` (ADR-040 D4 §4.2).
async fn handle_mode_status(client: &mut IpcClient, json: bool) -> Result<()> {
    let response = client
        .send_command(IpcCommand::ModeStatus, Value::Null)
        .await
        .context("Failed to get mode status")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    }
    check_ipc_response(&response, "get mode status")?;
    if json {
        return Ok(());
    }
    if let Some(data) = &response.data {
        let mode = data.get("mode").and_then(|v| v.as_str()).unwrap_or("?");
        let locked = data
            .get("locked")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        println!("{}", "Mode Status".bold().cyan());
        println!("{}", "─".repeat(50));
        println!("Active: {}", mode.green());
        if locked {
            let origin = data
                .get("lock_origin")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            println!("Lock:   {} (origin: {})", "locked".yellow(), origin);
        } else {
            println!("Lock:   {}", "unlocked (auto-switch active)".dimmed());
        }
    }
    Ok(())
}

async fn handle_profile_status(client: &mut IpcClient, json: bool) -> Result<()> {
    let response = client
        .send_command(IpcCommand::GetActiveProfile, Value::Null)
        .await
        .context("Failed to get active profile")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }

    // Check for daemon errors first
    if matches!(response.status, conductor_daemon::ResponseStatus::Error) {
        let msg = response
            .error
            .map(|e| e.message)
            .unwrap_or_else(|| "Unknown error".to_string());
        bail!("Failed to get profile status: {}", msg);
    }

    if let Some(data) = &response.data {
        if let Some(name) = data.get("name").and_then(|v| v.as_str()) {
            println!("{}", "Active Profile".bold().cyan());
            println!("{}", "─".repeat(50));
            println!("Name:   {}", name.green());
            if let Some(path) = data.get("config_path").and_then(|v| v.as_str()) {
                println!("Config: {}", path);
            }
        } else {
            println!("{}", "No active profile".yellow());
        }
    } else {
        println!("{}", "No active profile".yellow());
    }

    Ok(())
}

async fn handle_profile_switch(client: &mut IpcClient, path: &Path, json: bool) -> Result<()> {
    // Validate path is a .toml file
    if !path.is_file() {
        bail!("Profile config not found or not a file: {}", path.display());
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase());
    if ext.as_deref() != Some("toml") {
        bail!("Profile config must be a .toml file: {}", path.display());
    }

    let canonical = path
        .canonicalize()
        .with_context(|| format!("Failed to resolve path: {}", path.display()))?;

    let args = serde_json::json!({ "config_path": canonical.to_string_lossy() });

    let response = client
        .send_command(IpcCommand::SwitchProfile, args)
        .await
        .context("Failed to switch profile")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else {
        match response.status {
            conductor_daemon::ResponseStatus::Success => {
                println!(
                    "{} Switched to profile: {}",
                    "✓".green(),
                    canonical.display()
                );
            }
            conductor_daemon::ResponseStatus::Error => {
                let msg = response
                    .error
                    .map(|e| e.message)
                    .unwrap_or_else(|| "Unknown error".to_string());
                bail!("Failed to switch profile: {}", msg);
            }
        }
    }

    Ok(())
}

fn handle_profile_validate(path: &Path, json: bool) -> Result<()> {
    if !path.exists() {
        bail!("Profile config not found: {}", path.display());
    }

    let path_str = path
        .to_str()
        .context("Profile config path must be valid UTF-8")?;
    let config =
        Config::load(path_str).map_err(|e| anyhow::anyhow!("Failed to load config: {}", e))?;

    let report = conductor_core::config::validator::validate_config(&config);

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        if report.errors.is_empty() {
            println!("{}", "✓ Profile config is valid".green().bold());
        } else {
            println!(
                "{} ({} error(s)):",
                "✗ Profile config has errors".red().bold(),
                report.errors.len()
            );
            for f in &report.errors {
                println!("  {} {}: {}", "✗".red(), f.path, f.message);
            }
        }

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
    }

    if !report.is_valid() {
        bail!("Validation failed with {} error(s)", report.errors.len());
    }

    Ok(())
}

fn handle_profile_list(dir: &Option<PathBuf>, json: bool) -> Result<()> {
    let profile_dir = match dir {
        Some(d) => d.clone(),
        None => dirs::config_dir()
            .context("Could not determine config directory")?
            .join("conductor")
            .join("profiles"),
    };

    if !profile_dir.exists() {
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "directory": profile_dir.display().to_string(),
                    "profiles": []
                }))?
            );
        } else {
            println!(
                "{} Directory not found: {}",
                "!".yellow(),
                profile_dir.display()
            );
        }
        return Ok(());
    }

    let mut profiles: Vec<(String, u64)> = Vec::new();

    for entry in std::fs::read_dir(&profile_dir)
        .with_context(|| format!("Failed to read directory: {}", profile_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("toml") {
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            profiles.push((name, size));
        }
    }

    profiles.sort_by(|a, b| a.0.cmp(&b.0));

    if json {
        let items: Vec<serde_json::Value> = profiles
            .iter()
            .map(|(name, size)| serde_json::json!({ "name": name, "size": size }))
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "directory": profile_dir.display().to_string(),
                "profiles": items
            }))?
        );
    } else {
        println!("{}", "Profile Configs".bold().cyan());
        println!("Directory: {}", profile_dir.display());
        println!("{}", "─".repeat(50));

        if profiles.is_empty() {
            println!("{}", "  No .toml files found".yellow());
        } else {
            for (name, size) in &profiles {
                println!("  {} ({} bytes)", name, size);
            }
            println!("\n{} profile(s) found.", profiles.len());
        }
    }

    Ok(())
}

/// Resolve a profile name or path to a concrete path.
///
/// If the input contains `/` or ends with `.toml`, treat as a path.
/// Otherwise, resolve as `~/.config/conductor/profiles/<name>.toml`.
/// Validate a profile name to prevent path traversal and invalid filenames.
fn validate_profile_name(name: &str) -> Result<()> {
    if name.is_empty() || name.trim().is_empty() {
        bail!("Profile name cannot be empty");
    }
    if name.len() > 64 {
        bail!("Profile name too long (max 64 characters)");
    }
    if name.contains(['/', '\\', '\0']) || name.contains("..") {
        bail!("Profile name contains invalid characters");
    }
    // Only allow alphanumeric, hyphens, underscores, spaces
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == ' ')
    {
        bail!(
            "Profile name may only contain alphanumeric characters, hyphens, underscores, and spaces"
        );
    }
    Ok(())
}

fn resolve_profile_path(name_or_path: &str) -> Result<PathBuf> {
    if name_or_path.contains('/') || name_or_path.ends_with(".toml") {
        let path = PathBuf::from(name_or_path);
        // For explicit paths, validate the file exists or parent dir exists
        // The caller (switch/delete) will do further validation
        Ok(path)
    } else {
        validate_profile_name(name_or_path)?;
        let profile_dir = dirs::config_dir()
            .context("Could not determine config directory")?
            .join("conductor")
            .join("profiles");
        Ok(profile_dir.join(format!("{}.toml", name_or_path)))
    }
}

fn handle_profile_create(name: &str, apps: &[String], json: bool) -> Result<()> {
    validate_profile_name(name)?;

    let profile_dir = dirs::config_dir()
        .context("Could not determine config directory")?
        .join("conductor")
        .join("profiles");

    std::fs::create_dir_all(&profile_dir).with_context(|| {
        format!(
            "Failed to create profiles directory: {}",
            profile_dir.display()
        )
    })?;

    let profile_path = profile_dir.join(format!("{}.toml", name));

    // Build a minimal config — one mode per app, or a single Default mode
    let modes: Vec<conductor_core::Mode> = if apps.is_empty() {
        vec![conductor_core::Mode {
            name: "Default".to_string(),
            color: Some("blue".to_string()),
            mappings: vec![],
        }]
    } else {
        apps.iter()
            .map(|app| conductor_core::Mode {
                name: app.clone(),
                color: None,
                mappings: vec![],
            })
            .collect()
    };

    let mut config = Config::default_config();
    config.modes = modes;
    config.global_mappings = vec![];

    let toml_str = toml::to_string_pretty(&config).context("Failed to serialize profile config")?;

    // Add a header comment
    let content = format!(
        "# Conductor profile: {}\n# Created by conductorctl\n\n{}",
        name, toml_str
    );

    // Atomic create — fails if file already exists (no TOCTOU race)
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&profile_path)
        .with_context(|| {
            if profile_path.exists() {
                format!(
                    "Profile '{}' already exists: {}",
                    name,
                    profile_path.display()
                )
            } else {
                format!("Failed to create profile: {}", profile_path.display())
            }
        })?;
    file.write_all(content.as_bytes())
        .with_context(|| format!("Failed to write profile: {}", profile_path.display()))?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "status": "created",
                "name": name,
                "path": profile_path.display().to_string(),
                "apps": apps,
            }))?
        );
    } else {
        println!(
            "{} Created profile '{}': {}",
            "✓".green(),
            name.bold(),
            profile_path.display()
        );
        if !apps.is_empty() {
            println!("  Modes: {}", apps.join(", "));
        }
    }

    Ok(())
}

fn handle_profile_delete(name: &str, force: bool, json: bool) -> Result<()> {
    // resolve_profile_path validates names internally
    let profile_path = resolve_profile_path(name)?;

    if !profile_path.exists() {
        bail!("Profile '{}' not found: {}", name, profile_path.display());
    }

    if !force {
        // In non-interactive / CI contexts, require --force
        if !std::io::stdin().is_terminal() {
            bail!(
                "Refusing to delete '{}' without --force in non-interactive mode",
                name
            );
        }
        eprint!("Delete profile '{}'? [y/N] ", name);
        use std::io::BufRead;
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        if !line.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled.");
            return Ok(());
        }
    }

    std::fs::remove_file(&profile_path)
        .with_context(|| format!("Failed to delete profile: {}", profile_path.display()))?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "status": "deleted",
                "name": name,
                "path": profile_path.display().to_string(),
            }))?
        );
    } else {
        println!(
            "{} Deleted profile '{}': {}",
            "✓".green(),
            name.bold(),
            profile_path.display()
        );
    }

    Ok(())
}

async fn handle_led(client: &mut IpcClient, action: &LedAction, json: bool) -> Result<()> {
    match action {
        LedAction::Status => {
            let response = client
                .send_command(IpcCommand::GetLedStatus, serde_json::json!({}))
                .await?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&response.data.unwrap_or(serde_json::json!({})))?
                );
            } else if let Some(data) = &response.data {
                println!("{}", "LED Status".cyan().bold());
                println!("  Enabled:      {}", data["enabled"]);
                println!("  Brightness:   {}", data["brightness"]);
                println!("  Scheme:       {}", data["scheme"]);
                println!("  Idle timeout: {}s", data["idle_timeout_secs"]);
            } else {
                println!("{}", "No LED configuration found".yellow());
            }
        }
        LedAction::Scheme { name } => {
            let response = client
                .send_command(
                    IpcCommand::SetLedScheme,
                    serde_json::json!({ "scheme": name }),
                )
                .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&response)?);
            }
            // JSON mode must change output shape, not success
            // semantics — print the response (above), then return Err on a
            // daemon-error response so automation gets a non-zero exit.
            check_ipc_response(&response, "Set LED scheme")?;
            if !json {
                println!("{} LED scheme set to '{}'", "✓".green(), name);
            }
        }
        LedAction::Brightness { level } => {
            if *level > 127 {
                bail!("Brightness must be 0-127, got {}", level);
            }
            let response = client
                .send_command(
                    IpcCommand::SetLedBrightness,
                    serde_json::json!({ "brightness": level }),
                )
                .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&response)?);
            }
            check_ipc_response(&response, "Set LED brightness")?;
            if !json {
                println!("{} LED brightness set to {}", "✓".green(), level);
            }
        }
        LedAction::Off => {
            let response = client
                .send_command(
                    IpcCommand::SetLedScheme,
                    serde_json::json!({ "scheme": "off" }),
                )
                .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&response)?);
            }
            check_ipc_response(&response, "Turn off LEDs")?;
            if !json {
                println!("{} LEDs turned off", "✓".green());
            }
        }
    }
    Ok(())
}

async fn handle_plugin(client: &mut IpcClient, action: &PluginAction, json: bool) -> Result<()> {
    match action {
        PluginAction::List => {
            let response = client
                .send_command(IpcCommand::ListPlugins, serde_json::json!({}))
                .await?;
            if matches!(response.status, conductor_daemon::ResponseStatus::Error) {
                let msg = response
                    .error
                    .map(|e| e.message)
                    .unwrap_or_else(|| "Unknown error".to_string());
                bail!("Failed to list plugins: {}", msg);
            }
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&response.data.unwrap_or(serde_json::json!({})))?
                );
            } else if let Some(data) = &response.data {
                println!("{}", "Plugins".cyan().bold());
                if let Some(available) = data["available"].as_array() {
                    println!("  Available: {}", available.len());
                    for p in available {
                        println!("    - {}", p.as_str().unwrap_or("?"));
                    }
                }
                if let Some(loaded) = data["loaded"].as_array() {
                    println!("  Loaded: {}", loaded.len());
                    for p in loaded {
                        println!("    - {}", p.as_str().unwrap_or("?"));
                    }
                }
            } else {
                println!("{}", "No plugin data available".yellow());
            }
        }
        PluginAction::Info { name } => {
            let response = client
                .send_command(
                    IpcCommand::GetPluginInfo,
                    serde_json::json!({ "name": name }),
                )
                .await?;
            if matches!(response.status, conductor_daemon::ResponseStatus::Error) {
                let msg = response
                    .error
                    .map(|e| e.message)
                    .unwrap_or_else(|| "Unknown error".to_string());
                bail!("Plugin '{}': {}", name, msg);
            }
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&response.data.unwrap_or(serde_json::json!({})))?
                );
            } else if let Some(data) = &response.data {
                println!("{} {}", "Plugin:".cyan().bold(), name);
                if let Some(v) = data.get("version") {
                    println!("  Version:     {}", v);
                }
                if let Some(d) = data.get("description") {
                    println!("  Description: {}", d);
                }
                if let Some(a) = data.get("author") {
                    println!("  Author:      {}", a);
                }
                if let Some(t) = data.get("type") {
                    println!("  Type:        {}", t);
                }
            } else {
                bail!("Plugin '{}' not found", name);
            }
        }
        PluginAction::Enable { name } => {
            let response = client
                .send_command(
                    IpcCommand::EnablePlugin,
                    serde_json::json!({ "name": name }),
                )
                .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&response)?);
            }
            check_ipc_response(&response, "Enable plugin")?;
            if !json {
                println!("{} Plugin '{}' enabled", "✓".green(), name);
            }
        }
        PluginAction::Disable { name } => {
            let response = client
                .send_command(
                    IpcCommand::DisablePlugin,
                    serde_json::json!({ "name": name }),
                )
                .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&response)?);
            }
            check_ipc_response(&response, "Disable plugin")?;
            if !json {
                println!("{} Plugin '{}' disabled", "✓".green(), name);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ============================================================================
    // Config IPC-surface CLI tests (ADR-034 §D4.C / §D9)
    // ============================================================================

    #[test]
    fn config_drift_cli_parsing() {
        let cli = Cli::try_parse_from(["conductorctl", "config", "drift"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Config {
                action: ConfigAction::Drift
            }
        ));
    }

    #[test]
    fn config_mark_known_good_cli_parsing() {
        // The kebab-case `mark-known-good` maps to `ConfigAction::MarkKnownGood`.
        let cli = Cli::try_parse_from(["conductorctl", "config", "mark-known-good"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Config {
                action: ConfigAction::MarkKnownGood
            }
        ));
    }

    #[test]
    fn config_rejects_unknown_action() {
        // A typo'd subcommand must be a parse error, not silently accepted.
        assert!(Cli::try_parse_from(["conductorctl", "config", "bogus"]).is_err());
    }

    #[test]
    fn config_reload_no_path_cli_parsing() {
        let cli = Cli::try_parse_from(["conductorctl", "config", "reload"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Config {
                action: ConfigAction::Reload { path: None }
            }
        ));
    }

    #[test]
    fn config_reload_with_path_cli_parsing() {
        let cli =
            Cli::try_parse_from(["conductorctl", "config", "reload", "--path", "/tmp/x.toml"])
                .unwrap();
        match cli.command {
            Commands::Config {
                action: ConfigAction::Reload { path: Some(p) },
            } => assert_eq!(p, PathBuf::from("/tmp/x.toml")),
            _ => panic!("expected Reload with --path"),
        }
    }

    #[test]
    fn config_import_cli_parsing() {
        let cli = Cli::try_parse_from(["conductorctl", "config", "import", "/tmp/x.toml"]).unwrap();
        match cli.command {
            Commands::Config {
                action: ConfigAction::Import { path },
            } => assert_eq!(path, PathBuf::from("/tmp/x.toml")),
            _ => panic!("expected Import with path"),
        }
    }

    #[test]
    fn config_import_requires_path() {
        // ImportConfig's path is required (unlike reload's optional --path).
        assert!(Cli::try_parse_from(["conductorctl", "config", "import"]).is_err());
    }

    #[test]
    fn config_save_stdin_cli_parsing() {
        // Bare `config save` reads stdin (no path, no explicit base generation).
        let cli = Cli::try_parse_from(["conductorctl", "config", "save"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Config {
                action: ConfigAction::Save {
                    path: None,
                    base_generation: None
                }
            }
        ));
    }

    #[test]
    fn config_save_base_generation_cli_parsing() {
        let cli = Cli::try_parse_from(["conductorctl", "config", "save", "--base-generation", "7"])
            .unwrap();
        assert!(matches!(
            cli.command,
            Commands::Config {
                action: ConfigAction::Save {
                    path: None,
                    base_generation: Some(7)
                }
            }
        ));
    }

    #[test]
    fn config_save_captures_positional_path_for_redirect() {
        // A positional path PARSES (captured) so the handler can hard-fail with a
        // redirect to `config import` rather than clap's terse "unexpected arg".
        let cli = Cli::try_parse_from(["conductorctl", "config", "save", "x.toml"]).unwrap();
        match cli.command {
            Commands::Config {
                action: ConfigAction::Save { path: Some(p), .. },
            } => assert_eq!(p, PathBuf::from("x.toml")),
            _ => panic!("expected Save with a captured path"),
        }
    }

    // ============================================================================
    // Profile CLI tests
    // ============================================================================

    #[test]
    fn test_profile_status_cli_parsing() {
        let cli = Cli::try_parse_from(["conductorctl", "profile", "status"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Profile {
                action: ProfileAction::Status
            }
        ));
    }

    #[test]
    fn test_profile_switch_cli_parsing() {
        let cli =
            Cli::try_parse_from(["conductorctl", "profile", "switch", "/tmp/test.toml"]).unwrap();
        match cli.command {
            Commands::Profile {
                action: ProfileAction::Switch { name_or_path },
            } => {
                assert_eq!(name_or_path, "/tmp/test.toml");
            }
            _ => panic!("Expected Profile Switch"),
        }
    }

    #[test]
    fn test_profile_validate_cli_parsing() {
        let cli = Cli::try_parse_from(["conductorctl", "profile", "validate", "/tmp/config.toml"])
            .unwrap();
        match cli.command {
            Commands::Profile {
                action: ProfileAction::Validate { path },
            } => {
                assert_eq!(path, PathBuf::from("/tmp/config.toml"));
            }
            _ => panic!("Expected Profile Validate"),
        }
    }

    #[test]
    fn test_profile_list_cli_parsing_default() {
        let cli = Cli::try_parse_from(["conductorctl", "profile", "list"]).unwrap();
        match cli.command {
            Commands::Profile {
                action: ProfileAction::List { dir },
            } => {
                assert!(dir.is_none());
            }
            _ => panic!("Expected Profile List"),
        }
    }

    #[test]
    fn test_profile_list_cli_parsing_with_dir() {
        let cli =
            Cli::try_parse_from(["conductorctl", "profile", "list", "/tmp/profiles"]).unwrap();
        match cli.command {
            Commands::Profile {
                action: ProfileAction::List { dir },
            } => {
                assert_eq!(dir, Some(PathBuf::from("/tmp/profiles")));
            }
            _ => panic!("Expected Profile List"),
        }
    }

    #[test]
    fn test_plugin_list_cli_parsing() {
        let cli = Cli::try_parse_from(["conductorctl", "plugin", "list"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Plugin {
                action: PluginAction::List
            }
        ));
    }

    #[test]
    fn test_plugin_info_cli_parsing() {
        let cli =
            Cli::try_parse_from(["conductorctl", "plugin", "info", "spotify-control"]).unwrap();
        match cli.command {
            Commands::Plugin {
                action: PluginAction::Info { name },
            } => assert_eq!(name, "spotify-control"),
            _ => panic!("Expected Plugin Info"),
        }
    }

    #[test]
    fn test_plugin_enable_cli_parsing() {
        let cli = Cli::try_parse_from(["conductorctl", "plugin", "enable", "my-plugin"]).unwrap();
        match cli.command {
            Commands::Plugin {
                action: PluginAction::Enable { name },
            } => assert_eq!(name, "my-plugin"),
            _ => panic!("Expected Plugin Enable"),
        }
    }

    #[test]
    fn test_plugin_disable_cli_parsing() {
        let cli = Cli::try_parse_from(["conductorctl", "plugin", "disable", "my-plugin"]).unwrap();
        match cli.command {
            Commands::Plugin {
                action: PluginAction::Disable { name },
            } => assert_eq!(name, "my-plugin"),
            _ => panic!("Expected Plugin Disable"),
        }
    }

    // =========================================================================
    // Event monitoring tests
    // =========================================================================

    #[test]
    fn test_csv_escape() {
        assert_eq!(csv_escape("hello"), "hello");
        assert_eq!(csv_escape("hello,world"), "\"hello,world\"");
        assert_eq!(csv_escape("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_escape("line1\nline2"), "\"line1\nline2\"");
        assert_eq!(csv_escape(""), "");
    }

    #[test]
    fn test_event_passes_filter_no_filter() {
        let filter = conductor_daemon::EventFilter::default();
        let event = serde_json::json!({"timestamp_ms": 1000, "event_type": "note_on"});
        assert!(event_passes_filter(&event, &filter));
    }

    #[test]
    fn test_event_passes_filter_with_filter() {
        let filter = conductor_daemon::EventFilter {
            event_type: Some("cc".to_string()),
            ..Default::default()
        };
        let note = serde_json::json!({"timestamp_ms": 1000, "event_type": "note_on", "note": 60});
        let cc =
            serde_json::json!({"timestamp_ms": 1000, "event_type": "cc", "cc": 1, "value": 64});
        assert!(!event_passes_filter(&note, &filter));
        assert!(event_passes_filter(&cc, &filter));
    }

    #[test]
    fn test_event_passes_filter_bad_json_with_filter() {
        let filter = conductor_daemon::EventFilter {
            event_type: Some("note_on".to_string()),
            ..Default::default()
        };
        // Malformed event that can't deserialize — should be skipped when filters active
        let bad = serde_json::json!({"garbage": true});
        assert!(!event_passes_filter(&bad, &filter));
    }

    #[test]
    fn test_event_passes_filter_bad_json_no_filter() {
        let filter = conductor_daemon::EventFilter::default();
        // No filters active — pass through even if deserialization fails
        let bad = serde_json::json!({"garbage": true});
        assert!(event_passes_filter(&bad, &filter));
    }

    #[test]
    fn test_parse_duration_str() {
        assert_eq!(parse_duration_str(None), None);
        assert_eq!(parse_duration_str(Some("10s")), Some(10));
        assert_eq!(parse_duration_str(Some("1m")), Some(60));
        assert_eq!(parse_duration_str(Some("30")), Some(30));
        assert_eq!(parse_duration_str(Some("5m")), Some(300));
        assert_eq!(parse_duration_str(Some("1h")), Some(3600));
        assert_eq!(parse_duration_str(Some("2h")), Some(7200));
        assert_eq!(parse_duration_str(Some("abc")), None);
    }

    /// An omitted `--duration` defaults to 2s, but an explicit invalid
    /// value must error (not silently fall back to 2s).
    #[test]
    fn test_resolve_capture_secs() {
        // Omitted → 2-second default.
        assert_eq!(resolve_capture_secs(None).unwrap(), 2);
        // Valid explicit values pass through.
        assert_eq!(resolve_capture_secs(Some("10s")).unwrap(), 10);
        assert_eq!(resolve_capture_secs(Some("5m")).unwrap(), 300);
        // Explicit INVALID value → error (the bug: previously captured 2s).
        let err = resolve_capture_secs(Some("10seconds")).unwrap_err();
        assert!(
            err.to_string()
                .contains("Invalid --duration value '10seconds'"),
            "expected a clear invalid-duration error, got: {err}"
        );
        assert!(resolve_capture_secs(Some("nope")).is_err());
    }

    #[test]
    fn test_export_events_json() {
        let events = vec![
            serde_json::json!({"timestamp_ms": 1000, "event_type": "note_on", "note": 60, "velocity": 100}),
        ];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.json");
        export_events(&events, &path).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["event_type"], "note_on");
    }

    #[test]
    fn test_export_events_csv() {
        let events = vec![
            serde_json::json!({"timestamp_ms": 1000, "event_type": "note_on", "note": 60, "velocity": 100}),
            serde_json::json!({"timestamp_ms": 2000, "event_type": "cc", "cc": 1, "value": 64}),
        ];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.csv");
        export_events(&events, &path).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 3); // header + 2 events
        assert!(lines[0].starts_with("timestamp_ms,"));
        assert!(lines[1].starts_with("1000,note_on,"));
        assert!(lines[2].starts_with("2000,cc,"));
    }

    #[test]
    fn test_export_events_csv_with_special_chars() {
        let events = vec![
            serde_json::json!({"timestamp_ms": 1000, "event_type": "note_on", "device_id": "my,device"}),
        ];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.csv");
        export_events(&events, &path).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        // device_id with comma should be quoted
        assert!(content.contains("\"my,device\""));
    }

    // =========================================================================
    // Profile create/delete/name-switch tests
    // =========================================================================

    #[test]
    fn test_profile_create_cli_parsing() {
        let cli = Cli::try_parse_from([
            "conductorctl",
            "profile",
            "create",
            "gaming",
            "--app",
            "com.game.one",
            "--app",
            "com.game.two",
        ])
        .unwrap();
        match cli.command {
            Commands::Profile {
                action: ProfileAction::Create { name, app },
            } => {
                assert_eq!(name, "gaming");
                assert_eq!(app, vec!["com.game.one", "com.game.two"]);
            }
            _ => panic!("Expected Profile Create"),
        }
    }

    #[test]
    fn test_profile_create_cli_no_apps() {
        let cli = Cli::try_parse_from(["conductorctl", "profile", "create", "minimal"]).unwrap();
        match cli.command {
            Commands::Profile {
                action: ProfileAction::Create { name, app },
            } => {
                assert_eq!(name, "minimal");
                assert!(app.is_empty());
            }
            _ => panic!("Expected Profile Create"),
        }
    }

    #[test]
    fn test_profile_delete_cli_parsing() {
        let cli =
            Cli::try_parse_from(["conductorctl", "profile", "delete", "old-profile"]).unwrap();
        match cli.command {
            Commands::Profile {
                action: ProfileAction::Delete { name, force },
            } => {
                assert_eq!(name, "old-profile");
                assert!(!force);
            }
            _ => panic!("Expected Profile Delete"),
        }
    }

    #[test]
    fn test_profile_delete_cli_force() {
        let cli = Cli::try_parse_from([
            "conductorctl",
            "profile",
            "delete",
            "old-profile",
            "--force",
        ])
        .unwrap();
        match cli.command {
            Commands::Profile {
                action: ProfileAction::Delete { name, force },
            } => {
                assert_eq!(name, "old-profile");
                assert!(force);
            }
            _ => panic!("Expected Profile Delete"),
        }
    }

    #[test]
    fn test_profile_switch_by_name() {
        let cli = Cli::try_parse_from(["conductorctl", "profile", "switch", "gaming"]).unwrap();
        match cli.command {
            Commands::Profile {
                action: ProfileAction::Switch { name_or_path },
            } => {
                assert_eq!(name_or_path, "gaming");
            }
            _ => panic!("Expected Profile Switch"),
        }
    }

    #[test]
    fn test_resolve_profile_path_name() {
        let path = resolve_profile_path("gaming").unwrap();
        let expected = dirs::config_dir()
            .unwrap()
            .join("conductor")
            .join("profiles")
            .join("gaming.toml");
        assert_eq!(path, expected);
    }

    #[test]
    fn test_resolve_profile_path_absolute() {
        let path = resolve_profile_path("/tmp/my-profile.toml").unwrap();
        assert_eq!(path, PathBuf::from("/tmp/my-profile.toml"));
    }

    #[test]
    fn test_resolve_profile_path_relative_toml() {
        let path = resolve_profile_path("my-profile.toml").unwrap();
        assert_eq!(path, PathBuf::from("my-profile.toml"));
    }

    #[test]
    fn test_resolve_profile_path_with_slash() {
        let path = resolve_profile_path("./profiles/test").unwrap();
        assert_eq!(path, PathBuf::from("./profiles/test"));
    }

    #[test]
    fn migrate_config_default_path_uses_config_dir_not_dot_conductor() {
        // `migrate-config --routing`'s default config path must match the
        // daemon/GUI resolution (`dirs::config_dir()/conductor/config.toml`), NOT
        // `~/.conductor/config.toml`. On macOS those differ (config_dir =
        // ~/Library/Application Support/conductor), so the old home-based default
        // pointed migrate-config at a non-existent file and the user's real
        // config was never migrated.
        let resolved = resolve_migrate_config_path(&None).unwrap();
        let expected = dirs::config_dir()
            .unwrap()
            .join("conductor")
            .join("config.toml");
        assert_eq!(resolved, expected);

        // And it must NOT be the old home-based default (differs on every
        // platform: macOS config_dir is Application Support, Linux is ~/.config).
        if let Some(home) = dirs::home_dir() {
            assert_ne!(resolved, home.join(".conductor").join("config.toml"));
        }
    }

    #[test]
    fn migrate_config_explicit_path_is_passed_through() {
        let resolved =
            resolve_migrate_config_path(&Some(PathBuf::from("/tmp/custom.toml"))).unwrap();
        assert_eq!(resolved, PathBuf::from("/tmp/custom.toml"));
    }

    #[test]
    fn test_profile_create_generates_valid_toml() {
        let dir = tempfile::tempdir().unwrap();
        let profile_dir = dir.path().join("conductor").join("profiles");
        std::fs::create_dir_all(&profile_dir).unwrap();

        // We test the generated config directly rather than calling the handler
        // (which uses dirs::config_dir)
        let mut config = Config::default_config();
        config.modes = vec![conductor_core::Mode {
            name: "Default".to_string(),
            color: Some("blue".to_string()),
            mappings: vec![],
        }];
        config.global_mappings = vec![];

        let toml_str = toml::to_string_pretty(&config).unwrap();
        let parsed: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.modes.len(), 1);
        assert_eq!(parsed.modes[0].name, "Default");
        // ADR-035: the `device` binding field is gone; a freshly exported
        // profile carries no endpoint bindings (only modes/mappings).
        assert!(parsed.endpoints.is_empty());
    }

    #[test]
    fn test_profile_create_with_apps_generates_modes() {
        let apps = [
            "com.apple.Logic".to_string(),
            "com.ableton.Live".to_string(),
        ];
        let modes: Vec<conductor_core::Mode> = apps
            .iter()
            .map(|app| conductor_core::Mode {
                name: app.clone(),
                color: None,
                mappings: vec![],
            })
            .collect();

        let mut config = Config::default_config();
        config.modes = modes;
        config.global_mappings = vec![];

        let toml_str = toml::to_string_pretty(&config).unwrap();
        let parsed: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.modes.len(), 2);
        assert_eq!(parsed.modes[0].name, "com.apple.Logic");
        assert_eq!(parsed.modes[1].name, "com.ableton.Live");
    }

    #[test]
    fn test_validate_profile_name_rejects_empty() {
        assert!(validate_profile_name("").is_err());
        assert!(validate_profile_name("   ").is_err());
    }

    #[test]
    fn test_validate_profile_name_rejects_path_traversal() {
        assert!(validate_profile_name("../evil").is_err());
        assert!(validate_profile_name("foo/bar").is_err());
        assert!(validate_profile_name("foo\\bar").is_err());
        assert!(validate_profile_name("..").is_err());
    }

    #[test]
    fn test_validate_profile_name_rejects_long_names() {
        let long_name = "a".repeat(65);
        assert!(validate_profile_name(&long_name).is_err());
    }

    #[test]
    fn test_validate_profile_name_rejects_special_chars() {
        assert!(validate_profile_name("foo@bar").is_err());
        assert!(validate_profile_name("foo!bar").is_err());
        assert!(validate_profile_name("foo$bar").is_err());
    }

    #[test]
    fn test_validate_profile_name_accepts_valid() {
        assert!(validate_profile_name("my-profile").is_ok());
        assert!(validate_profile_name("My Profile").is_ok());
        assert!(validate_profile_name("profile_123").is_ok());
        assert!(validate_profile_name("DAW").is_ok());
    }

    // ── bindings filter ──────────────────────────────────────────────────
    //
    // The handler reuses `IpcCommand::Status` and just filters/pretty-prints.
    // The filter is the only non-trivial logic; pin it with focused tests so
    // a future refactor of the IPC payload shape (e.g. renaming
    // `is_configured` → `bound_to_alias`) trips the tests instead of silently
    // breaking the CLI.

    fn devices_fixture() -> Vec<Value> {
        vec![
            serde_json::json!({
                "device_id": "fcb",
                "port_name": "Komplete Audio 6 MK2",
                "connected": true,
                "is_configured": true,
            }),
            serde_json::json!({
                "device_id": "raw:IAC Driver Bus 1",
                "port_name": "IAC Driver Bus 1",
                "connected": true,
                "is_configured": false,
            }),
            serde_json::json!({
                "device_id": "pads",
                "port_name": "Maschine Mikro MK3 MIDI",
                "connected": true,
                "is_configured": true,
            }),
        ]
    }

    #[test]
    fn test_bindings_filter_no_filter_returns_all() {
        let devs = devices_fixture();
        let out = filter_bindings(&devs, None, false);
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn test_bindings_filter_alias_matches_exactly_one() {
        let devs = devices_fixture();
        let out = filter_bindings(&devs, Some("fcb"), false);
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].get("device_id").and_then(|v| v.as_str()),
            Some("fcb")
        );
    }

    #[test]
    fn test_bindings_filter_alias_no_match_returns_empty() {
        let devs = devices_fixture();
        let out = filter_bindings(&devs, Some("nonexistent"), false);
        assert!(out.is_empty());
    }

    #[test]
    fn test_bindings_filter_unbound_only_drops_configured() {
        let devs = devices_fixture();
        let out = filter_bindings(&devs, None, true);
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].get("device_id").and_then(|v| v.as_str()),
            Some("raw:IAC Driver Bus 1")
        );
        assert_eq!(
            out[0].get("is_configured").and_then(|v| v.as_bool()),
            Some(false)
        );
    }

    #[test]
    fn test_bindings_filter_alias_and_unbound_compose() {
        // alias matches a configured row; with unbound_only, both predicates
        // apply (AND), so the result is empty.
        let devs = devices_fixture();
        let out = filter_bindings(&devs, Some("fcb"), true);
        assert!(out.is_empty());
    }

    #[test]
    fn test_bindings_filter_treats_missing_is_configured_as_false() {
        // Forward-compat: if a future daemon payload omits the field, the
        // device must be treated as opportunistic rather than panicking.
        let devs = vec![serde_json::json!({
            "device_id": "legacy",
            "port_name": "Some Port",
            "connected": true,
        })];
        let out = filter_bindings(&devs, None, true);
        assert_eq!(
            out.len(),
            1,
            "missing is_configured should default to opportunistic"
        );
    }

    // ─── Rollback CLI exit-code regression tests ───────────────────

    /// Helper test fixture: build an IpcResponse with a synthetic
    /// daemon-side error. Used to exercise `check_ipc_response`'s
    /// error path without a live daemon.
    fn error_response(code: u16, msg: &str) -> conductor_daemon::daemon::types::IpcResponse {
        conductor_daemon::daemon::types::IpcResponse {
            id: "test".to_string(),
            status: conductor_daemon::ResponseStatus::Error,
            data: None,
            error: Some(conductor_daemon::daemon::types::ErrorDetails {
                code,
                message: msg.to_string(),
                details: None,
            }),
        }
    }

    fn success_response() -> conductor_daemon::daemon::types::IpcResponse {
        conductor_daemon::daemon::types::IpcResponse {
            id: "test".to_string(),
            status: conductor_daemon::ResponseStatus::Success,
            data: Some(serde_json::json!({"state_generation": 5})),
            error: None,
        }
    }

    /// Regression test: an error response MUST yield
    /// `Err` from the helper so the caller can propagate non-zero
    /// exit. Pre-fix, both rollback handlers swallowed the error
    /// and returned Ok(()) → process exit 0.
    #[test]
    fn check_ipc_response_returns_err_on_daemon_error() {
        let resp = error_response(1004, "rollback unsupported");
        let result = check_ipc_response(&resp, "rollback");
        assert!(
            result.is_err(),
            "daemon error response MUST yield Err so process exits non-zero; got Ok"
        );
        let err_msg = format!("{:#}", result.unwrap_err());
        assert!(
            err_msg.contains("rollback failed"),
            "ctx should be in error message; got: {err_msg}"
        );
        assert!(
            err_msg.contains("rollback unsupported"),
            "daemon message should propagate; got: {err_msg}"
        );
        assert!(
            err_msg.contains("1004"),
            "daemon error code should propagate; got: {err_msg}"
        );
    }

    /// Success response passes through with no error.
    #[test]
    fn check_ipc_response_returns_ok_on_success() {
        let resp = success_response();
        assert!(check_ipc_response(&resp, "rollback").is_ok());
    }

    /// Defensive: even if `status` is somehow `Success` but `error`
    /// is also populated (protocol bug — shouldn't happen but
    /// shouldn't pass silently if it does), still treat as error.
    /// Catches a class of daemon-side regression where the status
    /// field drifts out of sync with the error field.
    #[test]
    fn check_ipc_response_returns_err_when_error_field_set_despite_success_status() {
        let mut resp = success_response();
        resp.error = Some(conductor_daemon::daemon::types::ErrorDetails {
            code: 5005,
            message: "something is wrong".to_string(),
            details: None,
        });
        // status is still Success but error is Some — should still bail.
        assert!(
            check_ipc_response(&resp, "rollback").is_err(),
            "defensive: error field set MUST yield Err regardless of status"
        );
    }

    /// The LED (scheme/brightness/off) and plugin (enable/disable)
    /// handlers now route daemon-error responses through
    /// `check_ipc_response` AFTER printing JSON, so `--json led scheme
    /// <invalid>` / `--json plugin enable <missing>` return `Err` (non-zero
    /// process exit) instead of printing the error and exiting 0. This pins
    /// the shared contract those handlers rely on for each wired context.
    #[test]
    fn check_ipc_response_propagates_led_and_plugin_errors() {
        for ctx in [
            "Set LED scheme",
            "Set LED brightness",
            "Turn off LEDs",
            "Enable plugin",
            "Disable plugin",
        ] {
            let resp = error_response(5005, "daemon rejected request");
            let result = check_ipc_response(&resp, ctx);
            assert!(
                result.is_err(),
                "{ctx}: a daemon error response MUST yield Err so the process \
                 exits non-zero in JSON mode too"
            );
            let msg = format!("{:#}", result.unwrap_err());
            assert!(
                msg.contains(ctx) && msg.contains("daemon rejected request"),
                "{ctx}: error must carry the context and daemon message; got: {msg}"
            );
        }
    }

    #[test]
    fn test_bindings_filter_alias_matches_raw_prefix_opportunistic() {
        // The clap help and filter_bindings doc both state that opportunistic
        // ports have device_id = "raw:<port_name>". This test pins the exact
        // documented usage: `--alias "raw:IAC Driver Bus 1"` must return the
        // opportunistic entry and nothing else.
        let devs = devices_fixture();
        let out = filter_bindings(&devs, Some("raw:IAC Driver Bus 1"), false);
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].get("device_id").and_then(|v| v.as_str()),
            Some("raw:IAC Driver Bus 1")
        );
        assert_eq!(
            out[0].get("is_configured").and_then(|v| v.as_bool()),
            Some(false)
        );
    }

    // ─── ADR-027 D6: `conductorctl llm budgets show` resolver ─────────

    #[test]
    fn llm_budget_defaults_when_no_config_file() {
        let b = resolve_llm_budget_from_text(None).unwrap();
        assert_eq!(b, conductor_core::security::LlmBudgetConfig::default());
    }

    #[test]
    fn llm_budget_reads_security_llm_block() {
        // Tightens only what the block names; the rest stay at ADR defaults.
        let text = "[security.llm]\nmax_tool_calls_per_session = 9\n";
        let b = resolve_llm_budget_from_text(Some(text)).unwrap();
        assert_eq!(b.max_tool_calls_per_session, 9);
        assert_eq!(b.max_iterations_per_turn, 10); // untouched default
    }

    #[test]
    fn llm_budget_ignores_unrelated_tables() {
        // A config with no [security] table yields the defaults, not an error.
        let text = "[device]\nname = \"Mikro\"\n";
        let b = resolve_llm_budget_from_text(Some(text)).unwrap();
        assert_eq!(b, conductor_core::security::LlmBudgetConfig::default());
    }
}
