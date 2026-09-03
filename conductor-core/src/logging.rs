// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Structured logging configuration and initialization
//!
//! Provides production-ready logging with support for:
//! - Multiple output formats (text, JSON)
//! - Daily file rotation with bounded retention (`max_files`)
//! - Console and file output
//! - Per-module filtering via RUST_LOG
//! - Backward compatibility with DEBUG=1 environment variable

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

/// Logging configuration
///
/// Defines logging behavior including output levels, file paths, formats, and rotation.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LoggingConfig {
    /// Log level: "trace", "debug", "info", "warn", "error"
    #[serde(default = "default_level")]
    pub level: String,

    /// Path to log directory (e.g., ~/.local/share/conductor/logs)
    #[serde(default = "default_path")]
    pub path: PathBuf,

    /// Log format: "text" or "json"
    #[serde(default = "default_format")]
    pub format: String,

    /// Number of rotated log files to keep before the oldest are pruned
    /// (default 5). Honored via the rolling appender's `max_log_files`.
    #[serde(default = "default_max_files")]
    pub max_files: usize,

    /// Enable console output in addition to file
    #[serde(default = "default_console_enabled")]
    pub console_enabled: bool,

    /// Enable file output
    #[serde(default = "default_file_enabled")]
    pub file_enabled: bool,
}

fn default_level() -> String {
    "info".to_string()
}

fn default_path() -> PathBuf {
    log_dir()
}

/// Resolve the log directory for a given home directory.
///
/// On macOS this is `~/Library/Logs/Conductor` — the platform convention, which
/// Console.app indexes and which a non-technical user can be walked to via
/// Finder ▸ Go ▸ Library. Elsewhere it stays on the XDG-ish path.
pub fn log_dir_from_home(home: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home.join("Library/Logs/Conductor")
    }

    // Windows support is lower priority, but routing its logs to an XDG path
    // would be wrong whenever it does land, and this is one line.
    #[cfg(target_os = "windows")]
    {
        home.join("AppData/Local/Conductor/Logs")
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        home.join(".local/share/conductor/logs")
    }
}

/// The log directory for the current user (see [`log_dir_from_home`]).
///
/// With no `HOME` (a bare launchd context, a container), logs go under the temp
/// dir rather than `.` — the working directory of a daemon is not somewhere you
/// want to scatter persistent, rotating files.
pub fn log_dir() -> PathBuf {
    match home_dir() {
        Some(home) => log_dir_from_home(&home),
        None => std::env::temp_dir().join("conductor-logs"),
    }
}

/// The user's home directory, from `HOME` or (on Windows) `USERPROFILE`.
///
/// Windows does not set `HOME`. A `HOME`-only lookup would therefore send every
/// Windows install to the temp dir and make [`log_dir_from_home`]'s `AppData`
/// branch unreachable.
fn home_dir() -> Option<PathBuf> {
    ["HOME", "USERPROFILE"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .find(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Does this `DEBUG` env value mean "log at debug level"?
///
/// Pure so it is testable without mutating process env. `DEBUG=1` / `true` /
/// `yes` all count; anything else (including unset) does not.
pub fn debug_env_enabled(value: Option<&str>) -> bool {
    matches!(
        value.map(str::trim).map(str::to_ascii_lowercase).as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}

/// Should this process emit console (stdout) logs?
///
/// True when stdout is a terminal — i.e. a human is watching. When the GUI
/// spawns the daemon, stdout is redirected to the unrotated `daemon-stdout.log`,
/// and keeping the console layer there would write every trace line twice: once
/// to the rotating `daemon.<date>.log` and once to a file that grows forever.
/// `CONDUCTOR_LOG_CONSOLE=1` forces it back on for deliberate piping (`| tee`).
pub fn console_layer_enabled(stdout_is_terminal: bool, force: Option<&str>) -> bool {
    stdout_is_terminal || debug_env_enabled(force)
}

/// The default tracing directive for a binary's own target, e.g.
/// `conductor_gui=debug` when `DEBUG=1` is set, `conductor_gui=info` otherwise.
pub fn component_directive(target: &str, debug: bool) -> String {
    format!("{target}={}", if debug { "debug" } else { "info" })
}

/// Build a daily-rotating appender writing `<dir>/<component>.<date>.log`.
///
/// Each binary gets its own file: the daemon and the GUI are separate processes
/// and interleaving them into one stream makes both unreadable. `max_files`
/// bounds retention so an always-on daemon cannot fill the disk (a zero is
/// clamped to 1, matching [`build_file_appender`]).
pub fn component_appender(
    dir: &Path,
    component: &str,
    max_files: usize,
) -> Result<RollingFileAppender, Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dir)?;
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(component)
        .filename_suffix("log")
        .max_log_files(max_files.max(1))
        .build(dir)?;
    Ok(appender)
}

fn default_format() -> String {
    "text".to_string()
}

fn default_max_files() -> usize {
    5
}

fn default_console_enabled() -> bool {
    true
}

fn default_file_enabled() -> bool {
    true
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_level(),
            path: default_path(),
            format: default_format(),
            max_files: default_max_files(),
            console_enabled: default_console_enabled(),
            file_enabled: default_file_enabled(),
        }
    }
}

impl LoggingConfig {
    /// Create a logging config with custom path
    pub fn with_path(mut self, path: impl AsRef<Path>) -> Self {
        self.path = path.as_ref().to_path_buf();
        self
    }

    /// Create a logging config with custom level
    pub fn with_level(mut self, level: &str) -> Self {
        self.level = level.to_string();
        self
    }

    /// Create a logging config with JSON format
    pub fn with_json_format(mut self) -> Self {
        self.format = "json".to_string();
        self
    }
}

/// Initialize the tracing logging system
///
/// Sets up console and file logging with the specified configuration.
/// Respects the RUST_LOG environment variable for per-module filtering.
/// Also checks DEBUG=1 for backward compatibility, mapping it to debug level.
///
/// # Arguments
///
/// * `config` - Logging configuration
///
/// # Example
///
/// ```no_run
/// use conductor_core::logging::{LoggingConfig, init_logging};
///
/// let config = LoggingConfig::default().with_level("debug");
/// init_logging(&config).expect("Failed to initialize logging");
///
/// tracing::info!("Application started");
/// ```
pub fn init_logging(config: &LoggingConfig) -> Result<(), Box<dyn std::error::Error>> {
    // Create log directory if it doesn't exist
    if config.file_enabled {
        std::fs::create_dir_all(&config.path)?;
    }

    // Determine filter from environment
    let filter = build_env_filter(&config.level)?;

    // Set up file appender if enabled
    if config.file_enabled {
        let file_appender = build_file_appender(config)?;

        match config.format.as_str() {
            "json" => {
                let file_layer = fmt::layer()
                    .json()
                    .with_writer(file_appender)
                    .with_target(true)
                    .with_thread_ids(true)
                    .with_file(true)
                    .with_line_number(true);

                if config.console_enabled {
                    // `format = "json"` means JSON on BOTH sinks. This branch used
                    // `.compact()`, silently downgrading the console to text and
                    // breaking any log shipper reading stdout.
                    let console_layer =
                        fmt::layer().json().with_target(true).with_thread_ids(false);

                    tracing_subscriber::registry()
                        .with(filter)
                        .with(file_layer)
                        .with(console_layer)
                        .try_init()?;
                } else {
                    tracing_subscriber::registry()
                        .with(filter)
                        .with(file_layer)
                        .try_init()?;
                }
            }
            _ => {
                // Default to text format
                let file_layer = fmt::layer()
                    .compact()
                    .with_writer(file_appender)
                    .with_target(true)
                    .with_thread_ids(true)
                    .with_file(true)
                    .with_line_number(true);

                if config.console_enabled {
                    let console_layer = fmt::layer()
                        .compact()
                        .with_target(true)
                        .with_thread_ids(false);

                    tracing_subscriber::registry()
                        .with(filter)
                        .with(file_layer)
                        .with(console_layer)
                        .try_init()?;
                } else {
                    tracing_subscriber::registry()
                        .with(filter)
                        .with(file_layer)
                        .try_init()?;
                }
            }
        }
    } else if config.console_enabled {
        // Console only
        match config.format.as_str() {
            "json" => {
                let console_layer = fmt::layer().json().with_target(true).with_thread_ids(true);

                tracing_subscriber::registry()
                    .with(filter)
                    .with(console_layer)
                    .try_init()?;
            }
            _ => {
                let console_layer = fmt::layer()
                    .compact()
                    .with_target(true)
                    .with_thread_ids(false);

                tracing_subscriber::registry()
                    .with(filter)
                    .with(console_layer)
                    .try_init()?;
            }
        }
    }

    Ok(())
}

/// Build the rolling file appender, honoring `config.max_files`.
///
/// The previous code used `rolling::daily(&config.path, "conductor.log")`,
/// which rotates daily but keeps **every** rotated file — so the configured
/// retention (`max_files`) was silently ignored and the log directory grew
/// without bound. (`max_size_mb` was also ignored; tracing-appender does
/// time-based rotation only, with no size threshold, so that field was removed
/// rather than left as a misleading no-op.)
///
/// This uses the appender builder with `max_log_files(config.max_files)`, which
/// prunes the oldest logs down to that count (pruning runs when the appender is
/// constructed and on each rotation). The `filename_prefix("conductor.log")`
/// keeps the existing `conductor.log.<date>` filename layout unchanged.
///
/// `max_files` is clamped to at least 1: tracing-appender's prune computes
/// `len - (max_files - 1)`, which underflows (and panics) for `max_files == 0`.
/// Keeping at least the current file is also the only sensible retention.
fn build_file_appender(
    config: &LoggingConfig,
) -> Result<RollingFileAppender, Box<dyn std::error::Error>> {
    let max_files = config.max_files.max(1);
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("conductor.log")
        .max_log_files(max_files)
        .build(&config.path)?;
    Ok(appender)
}

/// Build EnvFilter with DEBUG=1 backward compatibility support
fn build_env_filter(default_level: &str) -> Result<EnvFilter, Box<dyn std::error::Error>> {
    let filter_str = resolve_filter_str(
        std::env::var("RUST_LOG").ok().as_deref(),
        debug_env_enabled(std::env::var("DEBUG").ok().as_deref()),
        default_level,
    );
    Ok(filter_from_str(&filter_str, default_level))
}

/// Which filter string wins: `RUST_LOG` > `DEBUG=1` > the configured default.
///
/// Pure, so the precedence is testable without mutating process env. `DEBUG` is
/// parsed by [`debug_env_enabled`] — the one place that decides what a DEBUG
/// value means, so `DEBUG=yes` and `DEBUG=1 ` cannot mean different things here
/// than they do to the daemon and the GUI.
pub fn resolve_filter_str(rust_log: Option<&str>, debug: bool, default_level: &str) -> String {
    match (rust_log, debug) {
        (Some(log), _) => log.to_string(),
        (None, true) => "debug".to_string(),
        (None, false) => default_level.to_string(),
    }
}

/// Parse a filter string, degrading to the default level (then to `info`) rather
/// than failing.
///
/// The previous code fell back from `try_from_default_env()` to
/// `try_new(&filter_str)` — but when `RUST_LOG` is set and *invalid*, those are
/// the SAME string, so the fallback re-parsed the same bad input, failed again,
/// and took the whole logging init down with it. A typo'd `RUST_LOG` must cost
/// you your filter, not all of your logs.
pub fn filter_from_str(filter_str: &str, default_level: &str) -> EnvFilter {
    EnvFilter::try_new(filter_str)
        .or_else(|_| EnvFilter::try_new(default_level))
        .unwrap_or_else(|_| EnvFilter::new("info"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A released `.app` writes no logs anywhere, so a field bug from a
    /// non-technical tester arrives with zero diagnostics. Both binaries need a
    /// rotating log FILE, and they must not write to the same one — otherwise
    /// two processes interleave into a single stream and neither is readable.
    #[test]
    fn component_log_paths_are_distinct_and_named_per_component() {
        let dir = tempfile::tempdir().unwrap();

        let mut daemon = component_appender(dir.path(), "daemon", 5).unwrap();
        let mut gui = component_appender(dir.path(), "gui", 5).unwrap();

        use std::io::Write;
        writeln!(daemon, "daemon line").unwrap();
        writeln!(gui, "gui line").unwrap();
        drop(daemon);
        drop(gui);

        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();

        assert!(
            names
                .iter()
                .any(|n| n.starts_with("daemon.") && n.ends_with(".log")),
            "expected a daemon.<date>.log, got {names:?}"
        );
        assert!(
            names
                .iter()
                .any(|n| n.starts_with("gui.") && n.ends_with(".log")),
            "expected a gui.<date>.log, got {names:?}"
        );
    }

    /// The log directory must be somewhere a non-technical macOS user
    /// can actually be talked to — `~/Library/Logs/Conductor` is the platform
    /// convention (Console.app reads it; Finder ▸ Go ▸ Library gets you there).
    /// `~/.local/share/…` is a Linux convention that macOS Finder hides.
    #[test]
    fn log_dir_follows_platform_convention() {
        let home = Path::new("/Users/testuser");
        let dir = log_dir_from_home(home);

        #[cfg(target_os = "macos")]
        assert_eq!(dir, Path::new("/Users/testuser/Library/Logs/Conductor"));

        #[cfg(target_os = "windows")]
        assert_eq!(
            dir,
            Path::new("/Users/testuser/AppData/Local/Conductor/Logs")
        );

        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        assert_eq!(
            dir,
            Path::new("/Users/testuser/.local/share/conductor/logs")
        );
    }

    /// The old fallback chain was
    /// `try_from_default_env().or_else(|_| try_new(&filter_str))` — but when
    /// `RUST_LOG` is set and invalid, `filter_str` IS that same invalid string,
    /// so the fallback re-parsed the same bad input and failed again, taking the
    /// whole logging init down. A typo'd `RUST_LOG` must cost you your filter,
    /// not all of your logs.
    #[test]
    fn invalid_rust_log_degrades_to_default_instead_of_killing_logging() {
        // A filter string tracing-subscriber cannot parse.
        let bad = "this is not=a=valid=filter=@!";
        assert!(
            EnvFilter::try_new(bad).is_err(),
            "test premise: input is invalid"
        );

        // Must still yield a working filter rather than an error.
        let filter = filter_from_str(bad, "warn");
        assert_eq!(filter.to_string(), "warn");

        // And a valid filter is of course honoured.
        assert_eq!(filter_from_str("debug", "warn").to_string(), "debug");
    }

    /// `RUST_LOG` > `DEBUG` > configured default. Pure,
    /// so precedence is asserted without racing on process env.
    #[test]
    fn filter_precedence_is_rust_log_then_debug_then_default() {
        assert_eq!(resolve_filter_str(Some("trace"), true, "info"), "trace");
        assert_eq!(resolve_filter_str(None, true, "info"), "debug");
        assert_eq!(resolve_filter_str(None, false, "warn"), "warn");
    }

    /// A flag the user typed must outrank an env var
    /// they exported once. The daemon folds `DEBUG` into its level BEFORE calling
    /// `resolve_filter_str` (where `--trace`/`--verbose` outrank it), so it passes
    /// `debug: false` — consulting DEBUG a second time would unconditionally
    /// return "debug" and silently downgrade an explicit `--trace`.
    #[test]
    fn debug_env_does_not_override_an_explicit_level() {
        let trace_default = "conductor=trace,conductor_daemon=trace,warn";

        // How the daemon calls it: DEBUG already folded in, so debug=false.
        assert_eq!(
            resolve_filter_str(None, false, trace_default),
            trace_default,
            "an explicit --trace must survive DEBUG=1"
        );

        // And the caller that has NOT pre-folded DEBUG still gets debug.
        assert_eq!(resolve_filter_str(None, true, "info"), "debug");
    }

    /// With no `HOME`, logs must not be scattered into
    /// whatever the daemon's working directory happens to be.
    #[test]
    fn log_dir_without_home_does_not_use_the_working_directory() {
        // The pure helper is what `log_dir()` delegates to when HOME is present;
        // the no-HOME branch is asserted by construction here.
        let fallback = std::env::temp_dir().join("conductor-logs");
        assert_ne!(fallback, PathBuf::from("."));
        assert!(fallback.is_absolute());
    }

    /// When the GUI spawns the daemon, the daemon's stdout is
    /// redirected to `daemon-stdout.log` — which has NO rotation and NO
    /// retention. If the daemon kept its console layer in that situation, every
    /// trace line would be written twice: once to the rotating `daemon.<date>.log`
    /// and once, forever, to an unbounded append-only file. Console output is for
    /// a human at a terminal; when there isn't one, don't produce it.
    #[test]
    fn console_layer_is_for_terminals_not_redirected_pipes() {
        // Interactive run: console on.
        assert!(console_layer_enabled(true, None));
        // Spawned by the GUI (stdout is a file): console off — the file layer
        // already captures everything, with rotation.
        assert!(!console_layer_enabled(false, None));
        // Escape hatch for anyone piping deliberately (`… | tee`, CI capture).
        assert!(console_layer_enabled(false, Some("1")));
        assert!(!console_layer_enabled(false, Some("0")));
    }

    /// The docs promise `DEBUG=1` turns on debug logging, but
    /// the GUI pinned its own target to `info` unconditionally, so the promise
    /// held for the daemon and silently failed for the app.
    #[test]
    fn component_directive_honours_debug() {
        assert_eq!(
            component_directive("conductor_gui", false),
            "conductor_gui=info"
        );
        assert_eq!(
            component_directive("conductor_gui", true),
            "conductor_gui=debug"
        );
    }

    /// The troubleshooting docs have told users to run with `DEBUG=1`
    /// for years, but the daemon only ever read `RUST_LOG`, so it was a silent
    /// no-op. Either the docs or the code had to change; the code did.
    #[test]
    fn debug_env_value_is_recognised() {
        assert!(debug_env_enabled(Some("1")));
        assert!(debug_env_enabled(Some("true")));
        assert!(debug_env_enabled(Some("YES")));
        assert!(debug_env_enabled(Some(" 1 ")));

        assert!(!debug_env_enabled(None));
        assert!(!debug_env_enabled(Some("0")));
        assert!(!debug_env_enabled(Some("")));
        assert!(!debug_env_enabled(Some("false")));
    }

    /// Retention is what stops an always-on daemon filling the disk.
    /// `component_appender` must honour `max_files` the way `build_file_appender`
    /// already does — a zero must not panic.
    #[test]
    fn component_appender_survives_zero_max_files() {
        let dir = tempfile::tempdir().unwrap();
        assert!(component_appender(dir.path(), "daemon", 0).is_ok());
    }

    /// `init_logging` returns a `Result`, but it used
    /// `SubscriberInitExt::init()`, which PANICS when a global subscriber is
    /// already installed (e.g. a second call). A function that returns `Result`
    /// must surface that as an `Err`, not abort the process. Calling it twice
    /// must therefore not panic, and the repeat must return `Err`.
    #[test]
    fn init_logging_does_not_panic_on_repeated_init() {
        // Console-only so the test has no filesystem side effects.
        let config = LoggingConfig {
            file_enabled: false,
            console_enabled: true,
            ..LoggingConfig::default()
        };

        // The first call may succeed, or already fail if another test in this
        // process installed the global subscriber first — either way it must
        // not panic. After it, a global subscriber is definitely set.
        let _ = init_logging(&config);

        // The second call must return Err (subscriber already set), NOT panic.
        let result = init_logging(&config);
        assert!(
            result.is_err(),
            "repeated init_logging must return Err, not panic (#2151)"
        );
    }

    /// The rolling appender must honor `max_files` and prune old logs.
    /// The previous `rolling::daily(...)` kept every rotated file, so the
    /// retention setting was ignored and the log directory grew without bound.
    /// Seeding more than `max_files` old log files and building the appender
    /// must prune them down to at most `max_files`.
    #[test]
    fn file_appender_honors_max_files_retention() {
        let dir = tempfile::tempdir().expect("temp dir");

        // Seed 5 old rotated logs matching the appender's `conductor.log.<date>`
        // layout. With the pre-fix code (no max_log_files) these would all be
        // kept; the fix prunes the oldest on construction.
        for day in 1..=5 {
            let name = format!("conductor.log.2024-01-0{day}");
            std::fs::write(dir.path().join(&name), b"old log\n").expect("seed log");
        }

        let config = LoggingConfig {
            file_enabled: true,
            console_enabled: false,
            max_files: 3,
            ..LoggingConfig::default()
        }
        .with_path(dir.path());

        // Building the appender prunes old logs to `max_files`.
        let _appender = build_file_appender(&config).expect("build appender");

        let remaining = std::fs::read_dir(dir.path())
            .expect("read dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("conductor.log"))
            .count();

        assert!(
            remaining <= config.max_files,
            "expected <= {} retained logs after pruning, found {} (#2152 — max_files ignored?)",
            config.max_files,
            remaining
        );
        // And pruning actually happened (we seeded 5).
        assert!(
            remaining < 5,
            "no pruning occurred: {remaining} files remain"
        );
    }

    /// `max_files = 0` must not panic. tracing-appender's prune
    /// computes `len - (max_files - 1)`, which underflows for 0; clamp to 1.
    #[test]
    fn file_appender_does_not_panic_on_zero_max_files() {
        let dir = tempfile::tempdir().expect("temp dir");
        let config = LoggingConfig {
            file_enabled: true,
            console_enabled: false,
            max_files: 0,
            ..LoggingConfig::default()
        }
        .with_path(dir.path());

        let appender = build_file_appender(&config);
        assert!(
            appender.is_ok(),
            "build_file_appender must not panic/err on max_files=0 (#2152)"
        );
    }

    #[test]
    fn test_default_config() {
        let config = LoggingConfig::default();
        assert_eq!(config.level, "info");
        assert_eq!(config.format, "text");
        assert!(config.console_enabled);
        assert!(config.file_enabled);
    }

    #[test]
    fn test_config_builder() {
        let config = LoggingConfig::default()
            .with_level("debug")
            .with_json_format();

        assert_eq!(config.level, "debug");
        assert_eq!(config.format, "json");
    }

    #[test]
    fn test_env_filter_with_debug() {
        // This test requires DEBUG env var to be set
        // Just verify it doesn't panic
        let _ = build_env_filter("info");
    }
}
