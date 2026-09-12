// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Clap CLI surface: Cli, Commands, and the per-subcommand action enums.

use super::*;

#[derive(Parser)]
#[command(name = "conductorctl")]
#[command(about = "Control the Conductor daemon", long_about = None)]
#[command(version)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Commands,

    /// Enable verbose output
    #[arg(short, long, global = true)]
    pub(crate) verbose: bool,

    /// JSON output format
    #[arg(short, long, global = true)]
    pub(crate) json: bool,
}

#[derive(Subcommand)]
pub(crate) enum Commands {
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
pub(crate) enum ListenerAction {
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
pub(crate) enum SecurityAction {
    /// Show the network-approval HMAC key status (fingerprint, age, rotation
    /// warning)
    Status,
    /// Rotate the network-approval HMAC key (existing approvals are re-signed
    /// under the new key)
    RotateHmac,
}

/// `conductorctl llm` subcommands (ADR-027 §D6).
#[derive(Subcommand)]
pub(crate) enum LlmAction {
    /// Inspect the multi-dimensional LLM budget
    Budgets {
        #[command(subcommand)]
        action: BudgetsAction,
    },
}

/// `conductorctl llm budgets` subcommands (ADR-027 §D6).
#[derive(Subcommand)]
pub(crate) enum BudgetsAction {
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
pub(crate) enum McpAction {
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
pub(crate) fn parse_audit_tier(
    s: &str,
) -> Result<conductor_daemon::daemon::audit::AuditRiskTier, String> {
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
pub(crate) enum AuditAction {
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
pub(crate) enum ConfigAction {
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
pub(crate) enum ProfileAction {
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
pub(crate) enum ModeAction {
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
pub(crate) enum LedAction {
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
pub(crate) enum PluginAction {
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
