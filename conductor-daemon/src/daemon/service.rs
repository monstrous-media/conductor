// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Main daemon service orchestrator

use crate::daemon::config_watcher::ConfigWatcher;
use crate::daemon::engine_manager::EngineManager;
use crate::daemon::error::{DaemonError, Result};
use crate::daemon::ipc::IpcServer;
#[cfg(feature = "mcp")]
use crate::daemon::mcp::McpServer;
use crate::daemon::midi_watcher;
use crate::daemon::state::{DaemonInfo, PersistedState, StateManager, get_state_dir};
use crate::daemon::types::DaemonCommand;
use conductor_core::{Config, UserFilePolicy};
use std::path::{Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, mpsc};
use tracing::{error, info, warn};

/// Convert a PathBuf to a UTF-8 string, returning an error with context if conversion fails.
fn pathbuf_to_str_or_err<'a>(path: &'a PathBuf, context: &str) -> Result<&'a str> {
    path.to_str().ok_or_else(|| DaemonError::InvalidPath {
        context: format!("{}: {:?}", context, path),
    })
}

/// ADR-045 D4: should the (read-only) MCP socket be bound at
/// startup? Pure decision seam over `[mcp] enabled` (default true) so the
/// bind/no-bind rule is unit-testable without spinning the service. When
/// false the socket is never bound — not merely refusing calls
/// (ADR-027 minimal-surface). Takes effect at startup; toggling requires
/// a daemon restart.
#[cfg(feature = "mcp")]
pub fn mcp_socket_enabled(config: &Config) -> bool {
    config.mcp.enabled
}

/// Main daemon service coordinating all components
pub struct DaemonService {
    config_path: PathBuf,
    /// The operator-editable **user file** (`config.toml` / active
    /// profile) that the §D9 drift watcher watches — distinct from
    /// `config_path`, which is the daemon's loaded **authority** (`live.toml`
    /// when present). ADR-034:853 says the watcher watches `user.toml`, NOT
    /// `live.toml`; pointing it at the authority made the daemon's own writes
    /// look like external edits (the self-write drift loop). Defaults to
    /// `config_path` when constructed via [`Self::new`].
    user_file_path: PathBuf,
    /// Active-profile identity restored at BOOT (from the daemon's own
    /// `active_profile.json`, or the one-time `profiles.json` migration).
    /// Seeded into the engine's `active_profile` ArcSwap in [`Self::run`] —
    /// store only, never re-persisted. `None` for explicit `--config` boots
    /// (ephemeral) and when nothing resolved.
    boot_identity: Option<crate::daemon::types::ActiveProfileInfo>,
    state_manager: StateManager,
    command_tx: mpsc::Sender<DaemonCommand>,
    command_rx: Option<mpsc::Receiver<DaemonCommand>>,
    shutdown_tx: broadcast::Sender<()>,
    #[allow(dead_code)] // Reserved for future graceful shutdown coordination
    shutdown_rx: Option<broadcast::Receiver<()>>,
}

impl DaemonService {
    /// Create a new daemon service. The §D9 watcher watches `config_path`
    /// itself (back-compat: correct when the path given IS the operator's user
    /// file, e.g. the no-arg / test cases). When the loaded authority
    /// (`live.toml`) differs from the user file, use [`Self::new_with_user_file`]
    /// so the watcher targets the user file.
    pub fn new(config_path: impl Into<PathBuf>) -> Result<Self> {
        let config_path = config_path.into();
        let user_file_path = config_path.clone();
        Self::build(config_path, user_file_path)
    }

    /// Construct with a distinct **authority** path (`config_path`, e.g.
    /// `live.toml`) and operator-editable **user file** (`user_file_path`, e.g.
    /// `config.toml` / active profile). The §D9 watcher watches `user_file_path`
    /// so the daemon's own `live.toml` writes are never seen as external edits
    /// (ADR-034:853); the engine still loads + mutates `config_path`.
    pub fn new_with_user_file(
        config_path: impl Into<PathBuf>,
        user_file_path: impl Into<PathBuf>,
    ) -> Result<Self> {
        Self::build(config_path.into(), user_file_path.into())
    }

    fn build(config_path: PathBuf, user_file_path: PathBuf) -> Result<Self> {
        let state_manager = StateManager::new()?;

        // Create command channel (100 message buffer)
        let (command_tx, command_rx) = mpsc::channel(100);

        // Create shutdown broadcast channel
        let (shutdown_tx, shutdown_rx) = broadcast::channel(10);

        Ok(Self {
            config_path,
            user_file_path,
            boot_identity: None,
            state_manager,
            command_tx,
            command_rx: Some(command_rx),
            shutdown_tx,
            shutdown_rx: Some(shutdown_rx),
        })
    }

    /// The path the §D9 drift watcher watches — the operator-editable
    /// user file, NOT the daemon's `live.toml` authority.
    pub fn user_file_path(&self) -> &std::path::Path {
        &self.user_file_path
    }

    /// Carry the boot-restored active-profile identity into [`Self::run`],
    /// where it seeds the engine's `active_profile` ArcSwap (store only — the
    /// identity came FROM disk, so it is not re-persisted).
    pub fn set_boot_identity(&mut self, identity: crate::daemon::types::ActiveProfileInfo) {
        self.boot_identity = Some(identity);
    }

    /// Run the daemon service
    pub async fn run(&mut self) -> Result<()> {
        info!("Conductor daemon starting");

        // Install panic handler for emergency state save
        self.install_panic_handler();

        // Load initial config
        let config_path_str = pathbuf_to_str_or_err(&self.config_path, "config_path in run")?;
        let config = Config::load(config_path_str)
            .map_err(|e| DaemonError::Ipc(format!("Failed to load config: {}", e)))?;

        info!("Config loaded from {:?}", self.config_path);

        // Create engine manager
        let command_rx = self
            .command_rx
            .take()
            .ok_or_else(|| DaemonError::Fatal("Command receiver already taken".to_string()))?;

        // ADR-045 D4: capture the MCP toggle before `config` moves
        // into the engine manager.
        #[cfg(feature = "mcp")]
        let mcp_enabled = mcp_socket_enabled(&config);
        let mut engine_manager = EngineManager::new(
            config,
            self.config_path.clone(),
            command_rx,
            self.command_tx.clone(),
            self.shutdown_tx.clone(),
        )?;

        // The "Overwrite user.toml" drift action must write the
        // operator-editable USER file (`config.toml`), not the `live.toml`
        // authority (`config_path`). Set this unconditionally — the Overwrite
        // action is available regardless of the watcher's user_file_policy. When
        // constructed via `Self::new` the two paths are equal, so this is a
        // no-op; via `new_with_user_file` they differ.
        engine_manager.set_user_file_path(self.user_file_path.clone());

        // Wire identity persistence unconditionally — profile switches
        // must persist `active_profile.json` regardless of HOW this boot was
        // configured (an explicit `--config` boot is itself ephemeral, but a
        // user switching profiles afterwards is a durable selection).
        match get_state_dir() {
            Ok(dir) => engine_manager.set_active_profile_persist_dir(dir),
            Err(e) => warn!(
                "Could not resolve state dir for active-profile persistence: {e}; switches will not persist identity"
            ),
        }
        // Seed the boot-restored identity (store only — no re-persist; see
        // set_boot_identity). Done before the IPC server starts so the first
        // GetActiveProfile already sees it.
        if let Some(identity) = self.boot_identity.clone() {
            info!(
                "Restoring active profile identity at boot: '{}' ({})",
                identity.name,
                identity.id.as_deref().unwrap_or("no id")
            );
            engine_manager.seed_active_profile_identity(identity);
        }

        // Get the event broadcast sender for push-based monitoring
        let event_broadcast_tx = engine_manager.event_broadcast_tx();
        // Audit sink handle for `SubscribeAudit` streaming (ADR-027 D13a,
        // ADR-045 D5) — the same seam every audit producer
        // writes to (SQLite under `audit-db`, JSONL otherwise), so the live
        // tail works in every composition.
        let audit_sink = engine_manager.audit_sink();

        // ADR-034 §D8.3 — emit an audit event for each config mutation that was
        // pending in the outbox at crash and did not publish (startup
        // reconciliation ran during `EngineManager::new`). Done here, before the
        // IPC server starts, so an operator querying the log immediately sees any
        // in-flight-at-crash mutations. No-op (0) after a clean shutdown.
        let pending_at_crash = engine_manager.emit_pending_at_crash_audit();
        if pending_at_crash > 0 {
            warn!(
                "{pending_at_crash} config mutation(s) were pending in the audit outbox at crash \
                 and did not publish; emitted to the audit log, best-effort (ADR-034 §D8.3)"
            );
        }

        // Create IPC server
        let shutdown_rx_ipc = self.shutdown_tx.subscribe();
        let mut ipc_server = IpcServer::new(
            self.command_tx.clone(),
            shutdown_rx_ipc,
            event_broadcast_tx,
            audit_sink,
        )?;

        // Spawn IPC server task
        let ipc_handle = tokio::spawn(async move {
            if let Err(e) = ipc_server.run().await {
                error!("IPC server error: {}", e);
            }
            info!("IPC server stopped");
        });

        // Create config watcher (returns retarget channel).
        //
        // ADR-034 §D9: the watcher is DISABLED entirely when the live config
        // declares `user_file_policy = "ignore"` (0 inotify slots). In notify
        // mode (the default) and legacy `source = "file"` mode it runs; the
        // notify-only-vs-legacy-reload decision is made per-event in the
        // engine_manager `ConfigFileChanged` handler against the LIVE policy,
        // so this startup gate only governs whether the watcher exists at all.
        //
        // Reading `config_meta` here is safe: `EngineManager::new()` above is
        // synchronous and seeds `live_config` via `LiveConfig::new(config)`
        // before returning (no async config-load step runs between `new()` and
        // this point — `engine_manager.run()` is not awaited until later), so
        // the policy reflects the loaded config, never a default placeholder.
        let user_file_policy = engine_manager
            .get_live_config()
            .load()
            .config
            .config_meta
            .user_file_policy;

        let watcher_handle = if user_file_policy == UserFilePolicy::Ignore {
            info!("Config watcher disabled (user_file_policy = ignore)");
            // No-op task keeps the `tokio::join!` shape uniform below.
            tokio::spawn(async {})
        } else {
            let shutdown_rx_watcher = self.shutdown_tx.subscribe();
            // Watch the operator-editable USER file (config.toml /
            // active profile), NOT `self.config_path` (the loaded authority,
            // which is `live.toml` when present). Watching the authority made
            // the daemon's own writes look like external edits (ADR-034:853).
            let (mut config_watcher, watcher_retarget_tx) = ConfigWatcher::new(
                self.user_file_path.clone(),
                self.command_tx.clone(),
                shutdown_rx_watcher,
            )?;

            // Wire retarget channel to engine manager for profile switch re-targeting
            engine_manager.set_watcher_retarget_tx(watcher_retarget_tx);

            // Spawn config watcher task
            tokio::spawn(async move {
                if let Err(e) = config_watcher.watch().await {
                    error!("Config watcher error: {}", e);
                }
                info!("Config watcher stopped");
            })
        };

        // Create MCP server for LLM integration (ADR-007 Phase 1B -> Phase 2)
        // Get shared state refs from engine_manager for real-time status.
        // D4.A.3.3.B.1: hand MCP the same `Arc<LiveConfig>` engine_manager
        // uses — the legacy `Arc<RwLock<Option<Config>>>` shim (which loaded
        // a separate copy from disk and never synced) retired.
        // ADR-045 D1: the MCP server (and its socket bind, inside
        // `McpServer::run`) only exists in `mcp` compositions.
        #[cfg(feature = "mcp")]
        let mcp_handle = if !mcp_enabled {
            // ADR-045 D4: `[mcp] enabled = false` — never bind the socket.
            // No-op task keeps the `tokio::join!` shape uniform.
            info!(
                "MCP socket disabled by config ([mcp] enabled = false) — not binding (ADR-045 D4)"
            );
            tokio::spawn(async {})
        } else {
            let shared_state = engine_manager.get_shared_state_refs();
            let live_config = engine_manager.get_live_config();
            let shutdown_rx_mcp = self.shutdown_tx.subscribe();
            let mut mcp_server =
                McpServer::new_with_shared_state(shutdown_rx_mcp, live_config, shared_state)?;

            // Spawn MCP server task
            tokio::spawn(async move {
                if let Err(e) = mcp_server.run().await {
                    error!("MCP server error: {}", e);
                }
                info!("MCP server stopped");
            })
        };
        // No-op task keeps the `tokio::join!` shape below uniform (same
        // pattern as the disabled config watcher above).
        #[cfg(not(feature = "mcp"))]
        let mcp_handle = tokio::spawn(async {});

        // Start persistent MIDI watcher thread
        // On macOS, keeps a CoreMIDI client alive with CFRunLoopRun() so that
        // device-added notifications are received. Without this, the daemon's
        // HotPlugCheck rescan never sees newly connected MIDI devices.
        // Binding must stay alive for daemon lifetime; do not change to `let _ =`.
        let _midi_watcher = midi_watcher::start_midi_watcher();

        // Spawn signal handler task. It subscribes to the shutdown broadcast
        // so that an IPC-initiated stop also terminates it — otherwise the
        // final `tokio::join!` below blocks forever and the daemon process
        // never exits after `conductorctl stop` / GUI "Stop & Close".
        let command_tx = self.command_tx.clone();
        let shutdown_rx_signal = self.shutdown_tx.subscribe();
        let signal_handle = tokio::spawn(async move {
            Self::signal_handler(command_tx, shutdown_rx_signal).await;
        });

        // Run engine manager (blocks until shutdown)
        info!("Starting engine manager");
        let engine_result = engine_manager.run().await;

        // Broadcast shutdown to all tasks
        info!("Broadcasting shutdown signal");
        let _ = self.shutdown_tx.send(());

        // Wait for all tasks to complete
        info!("Waiting for tasks to complete");
        let _ = tokio::join!(ipc_handle, watcher_handle, mcp_handle, signal_handle);

        // Final state save
        info!("Saving final daemon state");
        if let Err(e) = self.save_state(&engine_manager).await {
            error!("Failed to save final state: {}", e);
        }

        info!("Conductor daemon stopped");

        engine_result
    }

    /// Signal handler task.
    ///
    /// Exits on either an OS signal OR the daemon's shutdown broadcast.
    /// The broadcast arm is what lets an IPC-initiated stop (no OS signal
    /// involved) terminate this task; without it `DaemonService::run`'s
    /// final `tokio::join!` waits on this task forever.
    async fn signal_handler(
        command_tx: mpsc::Sender<DaemonCommand>,
        mut shutdown_rx: broadcast::Receiver<()>,
    ) {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};

            let mut sigterm =
                signal(SignalKind::terminate()).expect("Failed to install SIGTERM handler");
            let mut sigint =
                signal(SignalKind::interrupt()).expect("Failed to install SIGINT handler");
            let mut sighup =
                signal(SignalKind::hangup()).expect("Failed to install SIGHUP handler");

            tokio::select! {
                _ = sigterm.recv() => {
                    info!("Received SIGTERM, initiating graceful shutdown");
                    let _ = command_tx.send(DaemonCommand::Shutdown).await;
                }
                _ = sigint.recv() => {
                    info!("Received SIGINT, initiating graceful shutdown");
                    let _ = command_tx.send(DaemonCommand::Shutdown).await;
                }
                _ = sighup.recv() => {
                    info!("Received SIGHUP, reloading configuration");
                    // ADR-034 §D9: SIGHUP is an EXPLICIT reload (not the passive
                    // watcher), so it must reload even in managed/notify mode.
                    let _ = command_tx.send(DaemonCommand::SignalReload).await;
                }
                result = shutdown_rx.recv() => {
                    // Shutdown already in progress (IPC Stop or engine exit) —
                    // nothing to send; just let the task complete so the final
                    // join in run() can finish and the state save can run.
                    // Err(Closed) (sender dropped) and Err(Lagged) also mean
                    // shutdown is the only remaining course — exit either way,
                    // but log accurately.
                    match result {
                        Ok(()) => info!("Shutdown broadcast received, signal handler exiting"),
                        Err(e) => info!("Shutdown channel ended ({e}), signal handler exiting"),
                    }
                }
            }
        }

        #[cfg(not(unix))]
        {
            // Windows: Use ctrl_c handler
            tokio::select! {
                result = tokio::signal::ctrl_c() => {
                    if let Err(e) = result {
                        error!("Failed to listen for ctrl-c: {}", e);
                        return;
                    }
                    info!("Received Ctrl-C, initiating graceful shutdown");
                    let _ = command_tx.send(DaemonCommand::Shutdown).await;
                }
                result = shutdown_rx.recv() => {
                    match result {
                        Ok(()) => info!("Shutdown broadcast received, signal handler exiting"),
                        Err(e) => info!("Shutdown channel ended ({e}), signal handler exiting"),
                    }
                }
            }
        }
    }

    // Note: Periodic state persistence can be added later if needed
    // For now, state is saved only at shutdown to avoid lifetime complexity

    /// Save daemon state
    async fn save_state(&self, engine_manager: &EngineManager) -> Result<()> {
        Self::save_state_internal(&self.state_manager, engine_manager, &self.config_path).await
    }

    /// Internal state save implementation
    async fn save_state_internal(
        state_manager: &StateManager,
        engine_manager: &EngineManager,
        _config_path: &PathBuf,
    ) -> Result<()> {
        let daemon_info = DaemonInfo {
            lifecycle_state: engine_manager.get_state().await,
            started_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            pid: process::id(),
        };

        let config_info = engine_manager.get_config_info().await?;
        let engine_info = engine_manager.get_engine_info().await;
        let statistics = engine_manager.get_statistics().await;
        let last_errors = engine_manager.get_recent_errors().await;

        let state = PersistedState::new(
            daemon_info,
            config_info,
            engine_info,
            statistics,
            last_errors,
        );

        state_manager.save(state).await
    }

    /// Install panic handler for emergency state save
    fn install_panic_handler(&self) {
        let state_manager = self.state_manager.clone();

        std::panic::set_hook(Box::new(move |panic_info| {
            error!("PANIC: {}", panic_info);

            // Try to save state before exit
            if let Some(state) = tokio::runtime::Handle::try_current()
                .ok()
                .and_then(|handle| handle.block_on(async { state_manager.get().await }))
                && let Err(e) = state_manager.save_emergency(&state)
            {
                error!("Failed to save emergency state: {}", e);
            }
        }));
    }

    /// Get state manager reference
    pub fn state_manager(&self) -> &StateManager {
        &self.state_manager
    }

    /// Get command sender for external control
    pub fn command_sender(&self) -> mpsc::Sender<DaemonCommand> {
        self.command_tx.clone()
    }
}

/// EX_TEMPFAIL — POSIX exit code for "transient failure, please
/// retry." systemd / launchd treat this as a non-broken restart hint
/// (vs. EX_NOINPUT = 66 which means "this unit is permanently broken,
/// stop restarting"). D4.B.2 uses this for singleton-lock contention.
const EX_TEMPFAIL: i32 = 75;

/// EX_NOINPUT — POSIX exit code for "input file did not exist or was
/// not readable." systemd `RestartPreventExitStatus=66` recognises
/// this as "stop restarting; this unit needs human intervention
/// (e.g. operator hasn't bootstrapped a config yet)".
///
/// ADR-034 §D4.2 / D4.B.3.B: was intended for the `AwaitingConfig` idle
/// mode's SIGTERM exit so a misconfigured systemd unit wouldn't crash-loop
/// while waiting for the operator to bootstrap a config.
///
/// **RESERVED: the AwaitingConfig idle mode was never wired and is
/// downgraded to reserved, so nothing returns this code today.** Kept as the
/// documented "no input config" exit for an eventual reinstatement.
#[allow(dead_code)] // Reserved for the AwaitingConfig idle mode (downgraded)
const EX_NOINPUT: i32 = 66;

/// Acquire the daemon-singleton flock per ADR-034 §D10 / §D4.B.2.
///
/// Called from both `run_daemon` and `run_daemon_with_config` BEFORE
/// IPC bind or config load. On contention exits the process with
/// `EX_TEMPFAIL = 75` so systemd / launchd treat it as a transient
/// "another instance is already running" rather than a unit-broken
/// signal.
///
/// Returns the lock guard for the caller to hold for the daemon's
/// lifetime. Dropping the guard releases the flock; the kernel also
/// releases it on process exit (including SIGKILL).
#[cfg(unix)]
fn acquire_singleton_lock_or_exit() -> super::singleton_lock::SingletonLock {
    use super::live_config::LivePaths;
    use super::singleton_lock::{SingletonLock, SingletonLockError};

    let paths = match LivePaths::from_env() {
        Ok(p) => p,
        Err(e) => {
            error!("Failed to resolve $XDG_STATE_HOME for singleton lock: {e}");
            std::process::exit(EX_TEMPFAIL);
        }
    };
    if let Err(e) = paths.ensure_dir() {
        error!(
            "Failed to create state dir {} for singleton lock: {e}",
            paths.state_dir.display()
        );
        std::process::exit(EX_TEMPFAIL);
    }
    match SingletonLock::acquire(&paths.lockfile) {
        Ok(lock) => {
            info!("Singleton lock acquired at {}", paths.lockfile.display());
            lock
        }
        Err(SingletonLockError::Contention { path }) => {
            error!(
                "Another conductor daemon is already running (singleton lock contention on {}). \
                 Exiting with EX_TEMPFAIL = {EX_TEMPFAIL}.",
                path.display()
            );
            std::process::exit(EX_TEMPFAIL);
        }
        Err(SingletonLockError::Io { path, source }) => {
            error!(
                "Failed to open singleton lockfile {}: {source}",
                path.display()
            );
            std::process::exit(EX_TEMPFAIL);
        }
    }
}

/// Walk the ADR-034 §D4.1 startup precedence chain and surface a
/// concrete config path the daemon should load.
///
/// D4.B.3 partial wire-up:
/// - If `live.toml` or `live.toml.known_good` loads, return that path
///   (the existing `DaemonService::new` / `Config::load` flow then
///   reads it). Warn-log the recovery case so operators notice the
///   fallback.
/// - If neither loads (`AwaitingConfig` outcome), return `None` and
///   let the caller fall through to the legacy `~/.config/conductor`
///   path. The full `LifecycleState::AwaitingConfig` idle mode was never
///   wired and is downgraded to reserved: when no config resolves
///   anywhere, the daemon exits with a descriptive error (see `main.rs`)
///   rather than entering an idle bootstrap state.
fn resolve_canonical_config_path() -> Option<PathBuf> {
    use super::live_config::{LivePaths, LoadedFrom, StartupLoadOutcome, try_load_chain};

    let paths = match LivePaths::from_env() {
        Ok(p) => p,
        Err(e) => {
            // Same posture as the singleton-lock resolver: a path
            // resolution failure is not fatal here — the legacy
            // `~/.config/conductor/config.toml` fallback may still
            // work. Log and return None so the caller falls through.
            warn!("Could not resolve $XDG_STATE_HOME for live config: {e}");
            return None;
        }
    };
    match try_load_chain(&paths) {
        StartupLoadOutcome::Loaded {
            loaded_from: LoadedFrom::Live { path },
            ..
        } => {
            info!("Loaded canonical config from {}", path.display());
            Some(path)
        }
        StartupLoadOutcome::Loaded {
            loaded_from: LoadedFrom::KnownGoodRecovery { path },
            prior_rejections,
            ..
        } => {
            // Surface every prior-rejection diagnostic so the
            // operator can see WHY live.toml was rejected (parse
            // failure, read failure, etc.) alongside the recovery
            // notice — diagnosing corruption shouldn't require
            // grepping two separate log lines.
            for r in &prior_rejections {
                warn!(
                    "Live config source {} rejected: {} (will recover from known-good)",
                    r.path.display(),
                    r.reason
                );
            }
            warn!(
                "Recovered from known-good snapshot at {} — live.toml was \
                 absent or corrupt. Operator should review and re-confirm \
                 via `conductorctl config mark-known-good` after validating.",
                path.display()
            );
            Some(path)
        }
        StartupLoadOutcome::AwaitingConfig { rejections } => {
            if rejections.is_empty() {
                // Silent fresh-install case: both files genuinely
                // absent. Caller falls through to legacy path.
                info!(
                    "No live.toml or live.toml.known_good at {}; \
                     falling through to legacy config path",
                    paths.state_dir.display()
                );
            } else {
                // Files present but invalid — surface every rejection
                // so the operator can diagnose. Falling through to the
                // legacy path is the behaviour; the hard-stop
                // AwaitingConfig idle mode was downgraded to reserved,
                // so an unresolvable config ultimately exits
                // with a descriptive error in `main.rs`.
                for r in rejections {
                    warn!(
                        "Live config source {} rejected: {}",
                        r.path.display(),
                        r.reason
                    );
                }
            }
            None
        }
    }
}

/// Run daemon with default config path
/// Choose the §D9 watcher's user-file target. The watcher
/// must NEVER target the daemon's `live.toml` authority — its own writes would
/// self-trip drift (ADR-034:853). Substitute the conventional
/// `<state_dir>/config.toml` (which the daemon never writes) ONLY when the
/// discovered `user_file` IS the **existing canonical authority** — i.e. the
/// operator ran `--config live.toml`, so `arg_path` points at the live.toml that
/// `resolve_canonical_config_path` already found.
///
/// `canonical_authority` is `Some(path)` only when a `live.toml` / known-good
/// already exists. When it is `None`, `resolved` merely *fell back* to the user
/// file (no live.toml yet) — that file (config.toml / active profile) is a
/// legitimate watch target even though it doubles as the loaded config, so it is
/// kept as-is (the daemon's mutate seam writes `live.toml`, never this file).
/// True when `a` and `b` resolve to the **same file**. Uses `canonicalize`
/// (resolves `.`/`..`/symlinks and relative→absolute) so a non-canonical
/// `--config ./live.toml` still matches the absolute canonical authority;
/// falls back to lexical equality only when a path can't be canonicalized
/// (e.g. it does not exist). Raw `PathBuf` equality alone
/// would miss un-normalized/relative forms and re-target the watcher at
/// `live.toml`.
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => a == b,
    }
}

fn watcher_user_file(
    canonical_authority: Option<&Path>,
    user_file: PathBuf,
    conventional: PathBuf,
) -> PathBuf {
    match canonical_authority {
        Some(auth) if same_file(user_file.as_path(), auth) => conventional,
        _ => user_file,
    }
}

pub async fn run_daemon() -> Result<()> {
    // D4.B.2: acquire singleton lock BEFORE config load + IPC bind.
    // Lock held for the daemon's lifetime via `_singleton_lock`; drop
    // releases the flock, process exit (including SIGKILL) also
    // releases via the kernel.
    #[cfg(unix)]
    let _singleton_lock = acquire_singleton_lock_or_exit();

    // D4.B.3 wire-up: prefer the ADR-034 §D4.1 canonical source
    // (live.toml → known_good). Falls through to the legacy
    // `~/.config/conductor/config.toml` path on AwaitingConfig.
    // The §D9 watcher watches the operator-editable user file, not the
    // `live.toml` authority. The user file is the discovered `config.toml`.
    let canonical = resolve_canonical_config_path();
    let conventional = get_state_dir()?.join("config.toml");
    let config_path = canonical.clone().unwrap_or_else(|| conventional.clone());
    // Guard: never let the watcher target the authority.
    let user_file = watcher_user_file(canonical.as_deref(), conventional.clone(), conventional);

    let mut daemon = DaemonService::new_with_user_file(config_path, user_file)?;
    daemon.run().await
}

/// Run daemon with a custom config path.
///
/// `explicit` distinguishes the two CLI cases:
/// - `explicit == true` — the operator passed `conductor --config X`.
///   The explicit path is **authoritative**. We *adopt* it — validate X and
///   overwrite the live config (`live.toml`) with it — then boot from
///   `live.toml`. Because `live.toml` is the daemon's mutate target / authority
///   (§D11), an explicit override must rewrite it, not merely shadow it for one
///   boot (which would leave the next no-arg boot resuming a stale `live.toml`).
///   This fixes operator recovery (`--config known_good.toml` reliably escapes a
///   corrupt `live.toml`) and test isolation.
/// - `explicit == false` — no `--config`; the path is the discovered default
///   (active profile / `config.toml`). Resume `live.toml` (authority) if present,
///   else seed from the discovered path. (Unchanged behaviour.)
pub async fn run_daemon_with_config(config_path: impl Into<PathBuf>, explicit: bool) -> Result<()> {
    run_daemon_with_identity(config_path, explicit, None).await
}

/// `run_daemon_with_config` plus the boot-restored active-profile
/// IDENTITY (from `startup::resolve_startup_identity_and_path`). `None` for
/// explicit `--config` boots (ephemeral) and when nothing resolved.
pub async fn run_daemon_with_identity(
    config_path: impl Into<PathBuf>,
    explicit: bool,
    boot_identity: Option<crate::daemon::types::ActiveProfileInfo>,
) -> Result<()> {
    // D4.B.2: see `run_daemon` above for the singleton lock contract.
    #[cfg(unix)]
    let _singleton_lock = acquire_singleton_lock_or_exit();

    let arg_path = config_path.into();

    if explicit {
        // Adopt the explicit config as the live authority before boot.
        adopt_explicit_config(&arg_path).await?;
    }

    // After an adopt, `live.toml` holds the explicit config, so the canonical
    // resolver loads it. In the non-explicit case this resumes `live.toml` /
    // known-good and falls back to the discovered path on AwaitingConfig.
    // Keep the user-file path (`arg_path`) for the §D9 watcher; `resolved`
    // is the authority (`live.toml` after an adopt / when present).
    let canonical = resolve_canonical_config_path();
    let resolved = canonical.clone().unwrap_or_else(|| arg_path.clone());
    // Guard: if `--config` pointed at the EXISTING authority itself (e.g.
    // `--config live.toml`, so `arg_path` == the canonical live.toml), the watcher
    // must not target it — fall back to the conventional
    // `config.toml`. When no live.toml exists yet, `arg_path` is a legitimate user
    // file even though `resolved` fell back to it, so it is kept.
    let conventional = get_state_dir()?.join("config.toml");
    let user_file = watcher_user_file(canonical.as_deref(), arg_path, conventional);

    let mut daemon = DaemonService::new_with_user_file(resolved, user_file)?;
    if let Some(identity) = boot_identity {
        daemon.set_boot_identity(identity);
    }
    daemon.run().await
}

/// Make an explicit `--config X` authoritative by overwriting the live
/// config (`live.toml`) with the validated contents of X. Thin wrapper that
/// resolves `LivePaths` from the environment; the testable core is
/// [`adopt_explicit_config_to`].
async fn adopt_explicit_config(arg_path: &std::path::Path) -> Result<()> {
    use super::live_config::LivePaths;
    let paths = LivePaths::from_env()
        .map_err(|e| DaemonError::Ipc(format!("resolve $XDG_STATE_HOME for live config: {e}")))?;
    adopt_explicit_config_to(arg_path, &paths).await
}

/// Validate `arg_path` and overwrite `paths.live` with its canonical content.
/// No-op if `arg_path` already IS the live file. Returns an error (without
/// touching `live.toml`) if the explicit config fails to load/validate.
///
/// Writes via the canonical live-config path: `canonical::serialise` +
/// `persist_atomically` (0600, atomic temp→rename, dir fsync). NOT `Config::save`
/// — that enforces a user-file directory allowlist (config dir / cwd / /tmp) and
/// would reject `$XDG_STATE_HOME/conductor/live.toml` under e.g. a systemd
/// `WorkingDirectory=/`. This is the same write contract the
/// daemon's own commit path uses for `live.toml` (§D11).
async fn adopt_explicit_config_to(
    arg_path: &std::path::Path,
    paths: &super::live_config::LivePaths,
) -> Result<()> {
    use super::live_config::persist_atomically;

    // Validate BEFORE overwriting anything — never clobber live.toml with a
    // config that doesn't load.
    let arg_str = arg_path.to_str().ok_or_else(|| DaemonError::InvalidPath {
        context: format!("--config path: {arg_path:?}"),
    })?;
    let config = Config::load(arg_str)
        .map_err(|e| DaemonError::Ipc(format!("--config {arg_str} failed to load: {e}")))?;

    if arg_path == paths.live.as_path() {
        // The operator pointed --config straight at live.toml — already adopted.
        return Ok(());
    }

    paths
        .ensure_dir()
        .map_err(|e| DaemonError::Ipc(format!("create state dir for live config: {e}")))?;
    let bytes = conductor_core::config::canonical::serialise(&config)
        .map_err(|e| DaemonError::Ipc(format!("serialise --config for live-config adopt: {e}")))?;
    persist_atomically(&paths.live, &bytes)
        .await
        .map_err(|e| DaemonError::Ipc(format!("overwrite live.toml during --config adopt: {e}")))?;
    warn!(
        "Adopted --config {} as the live configuration: overwrote {} \
         (live.toml is the daemon's authority; subsequent boots resume it).",
        arg_path.display(),
        paths.live.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::live_config::LivePaths;
    use tempfile::tempdir;

    /// An explicit `--config X` must be authoritative — it overwrites the
    /// live config so the daemon boots from X (operator recovery + test
    /// isolation), instead of an existing `live.toml` silently winning.
    #[tokio::test]
    async fn explicit_config_adopt_overwrites_live_toml() {
        let state = tempdir().unwrap();
        let paths = LivePaths::from_state_dir(state.path().to_path_buf());
        paths.ensure_dir().unwrap();

        // A pre-existing (stale) live.toml the operator wants to override.
        let mut stale = Config::default_config();
        stale.last_selected_mode = Some("STALE_LIVE".to_string());
        std::fs::write(&paths.live, toml::to_string(&stale).unwrap()).unwrap();

        // The explicit `--config X`, carrying a distinctive marker.
        let xdir = tempdir().unwrap();
        let xpath = xdir.path().join("explicit.toml");
        let mut x = Config::default_config();
        x.last_selected_mode = Some("EXPLICIT_X_1318".to_string());
        std::fs::write(&xpath, toml::to_string(&x).unwrap()).unwrap();

        adopt_explicit_config_to(&xpath, &paths)
            .await
            .expect("adopt should succeed");

        // live.toml now holds X's content, not the stale default.
        let adopted =
            Config::load(paths.live.to_str().unwrap()).expect("adopted live.toml must be loadable");
        assert_eq!(
            adopted.last_selected_mode.as_deref(),
            Some("EXPLICIT_X_1318"),
            "an explicit --config must overwrite live.toml with its content"
        );
    }

    /// A `--config` that fails to load must NOT clobber an existing live.toml.
    #[tokio::test]
    async fn explicit_config_adopt_preserves_live_toml_on_invalid_config() {
        let state = tempdir().unwrap();
        let paths = LivePaths::from_state_dir(state.path().to_path_buf());
        paths.ensure_dir().unwrap();
        let mut good = Config::default_config();
        good.last_selected_mode = Some("KEEP_ME".to_string());
        std::fs::write(&paths.live, toml::to_string(&good).unwrap()).unwrap();

        let xdir = tempdir().unwrap();
        let bad = xdir.path().join("bad.toml");
        std::fs::write(&bad, "this is { not valid toml ===").unwrap();

        assert!(
            adopt_explicit_config_to(&bad, &paths).await.is_err(),
            "an unloadable --config must fail the adopt"
        );
        let kept = Config::load(paths.live.to_str().unwrap()).expect("live.toml still loads");
        assert_eq!(
            kept.last_selected_mode.as_deref(),
            Some("KEEP_ME"),
            "a failed adopt must leave the existing live.toml untouched"
        );
    }

    /// The signal-handler task must exit on the shutdown broadcast,
    /// not only on an OS signal. Without this, an IPC `Stop` leaves
    /// `DaemonService::run` blocked forever in its final `tokio::join!` —
    /// the process never exits and the final state save is skipped.
    #[tokio::test]
    async fn signal_handler_exits_on_shutdown_broadcast() {
        let (command_tx, _command_rx) = mpsc::channel::<DaemonCommand>(8);
        let (shutdown_tx, shutdown_rx) = broadcast::channel::<()>(4);

        let handle = tokio::spawn(DaemonService::signal_handler(command_tx, shutdown_rx));

        // The handler must park in select! and still be running before the
        // broadcast fires — guards against a handler that exits immediately.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            !handle.is_finished(),
            "signal handler exited before any shutdown broadcast"
        );

        shutdown_tx.send(()).expect("broadcast send");

        tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await
            .expect("signal handler did not exit on shutdown broadcast")
            .expect("signal handler task panicked");
    }

    /// The shutdown arm also fires with `Err(Closed)` when the broadcast
    /// sender is dropped (service torn down) — the handler must exit then
    /// too, not only on a clean `Ok(())` broadcast.
    #[tokio::test]
    async fn signal_handler_exits_when_shutdown_sender_dropped() {
        let (command_tx, _command_rx) = mpsc::channel::<DaemonCommand>(8);
        let (shutdown_tx, shutdown_rx) = broadcast::channel::<()>(4);

        let handle = tokio::spawn(DaemonService::signal_handler(command_tx, shutdown_rx));
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            !handle.is_finished(),
            "signal handler exited before shutdown sender was dropped"
        );

        drop(shutdown_tx);

        tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await
            .expect("signal handler did not exit when shutdown sender was dropped")
            .expect("signal handler task panicked");
    }

    #[test]
    fn test_daemon_service_creation() {
        let temp_dir = tempdir().unwrap();
        let config_path = temp_dir.path().join("config.toml");

        // Create a minimal config file
        std::fs::write(
            &config_path,
            r#"
            [[modes]]
            name = "Default"
            color = "blue"
        "#,
        )
        .unwrap();

        let result = DaemonService::new(config_path);
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_state_manager_access() {
        let temp_dir = tempdir().unwrap();
        let config_path = temp_dir.path().join("config.toml");

        std::fs::write(
            &config_path,
            r#"
            [[modes]]
            name = "Default"
            color = "blue"
        "#,
        )
        .unwrap();

        let daemon = DaemonService::new(config_path).unwrap();
        let state_manager = daemon.state_manager();

        // Should be able to save/load state
        let state = state_manager.get().await;
        assert!(state.is_none()); // No state saved yet
    }

    #[test]
    fn test_command_sender() {
        let temp_dir = tempdir().unwrap();
        let config_path = temp_dir.path().join("config.toml");

        std::fs::write(
            &config_path,
            r#"
            [[modes]]
            name = "Default"
            color = "blue"
        "#,
        )
        .unwrap();

        let daemon = DaemonService::new(config_path).unwrap();
        let _sender = daemon.command_sender();
        // Should be able to get command sender for external control
    }

    // ── §D9 watcher must watch the USER file, not the live.toml authority ──

    #[test]
    fn new_with_user_file_targets_user_file_not_authority() {
        // The §D9 drift watcher must watch the operator-editable user file
        // (config.toml), NOT the daemon's loaded authority (live.toml). Watching
        // the authority made the daemon's own live.toml writes look like external
        // edits → false drift banner + Apply loop (ADR-034:853). run() hands
        // `user_file_path()` to ConfigWatcher::new, so this is the watch target.
        let dir = tempdir().unwrap();
        let authority = dir.path().join("live.toml");
        let user_file = dir.path().join("config.toml");
        for p in [&authority, &user_file] {
            std::fs::write(p, "[[modes]]\nname = \"Default\"\ncolor = \"blue\"\n").unwrap();
        }

        let daemon = DaemonService::new_with_user_file(&authority, &user_file).unwrap();

        assert_eq!(
            daemon.user_file_path(),
            user_file.as_path(),
            "watcher must target the user file (config.toml), not the live.toml authority"
        );
        assert_ne!(
            daemon.user_file_path(),
            authority.as_path(),
            "watcher must NOT target live.toml — the daemon's own writes would self-trip drift"
        );
    }

    #[test]
    fn new_defaults_user_file_to_config_path() {
        // Back-compat: the single-path constructor watches the path it is given
        // (correct when that path IS the operator's user file — no-arg / tests).
        let dir = tempdir().unwrap();
        let p = dir.path().join("config.toml");
        std::fs::write(&p, "[[modes]]\nname = \"Default\"\ncolor = \"blue\"\n").unwrap();

        let daemon = DaemonService::new(&p).unwrap();

        assert_eq!(daemon.user_file_path(), p.as_path());
    }

    #[test]
    fn watcher_user_file_never_targets_the_authority() {
        // If `--config` points at the EXISTING authority (e.g.
        // `--config live.toml`), the user file == the canonical authority and the
        // watcher would self-trip drift again. Guard: fall back to the conventional
        // config.toml, which the daemon never writes (ADR-034:853).
        let live = PathBuf::from("/state/live.toml");
        let cfg = PathBuf::from("/state/config.toml");
        let profile = PathBuf::from("/state/profiles/studio.toml");

        // Canonical authority exists + a distinct user file → use it as-is.
        assert_eq!(
            watcher_user_file(Some(live.as_path()), cfg.clone(), cfg.clone()),
            cfg,
            "a distinct user file must be used as-is"
        );

        // Degenerate: user file IS the canonical authority → fall back.
        assert_eq!(
            watcher_user_file(Some(live.as_path()), live.clone(), cfg.clone()),
            cfg,
            "must never hand the live.toml authority to the watcher"
        );

        // No canonical authority yet (no live.toml): the user file (a profile /
        // config.toml that `resolved` fell back to) is legitimate — do NOT
        // over-fire the guard onto the conventional file.
        assert_eq!(
            watcher_user_file(None, profile.clone(), cfg.clone()),
            profile,
            "with no live.toml, the fallback user file must still be watched"
        );
    }

    #[cfg(unix)]
    #[test]
    fn watcher_user_file_detects_authority_via_symlink() {
        // A path that resolves to the authority via a symlink
        // (or a relative form) is NOT lexically equal to the absolute canonical
        // authority. Raw `PathBuf` equality would miss it and re-target the
        // watcher at `live.toml`; `same_file` canonicalizes so it is caught.
        let dir = tempdir().unwrap();
        let live = dir.path().join("live.toml");
        std::fs::write(&live, "x").unwrap();
        let cfg = dir.path().join("config.toml");

        // A symlink pointing at the SAME live.toml — lexically distinct.
        let link = dir.path().join("live-link.toml");
        std::os::unix::fs::symlink(&live, &link).unwrap();
        assert_ne!(
            link, live,
            "precondition: the symlink path differs lexically"
        );

        assert_eq!(
            watcher_user_file(Some(live.as_path()), link, cfg.clone()),
            cfg,
            "must detect the authority through a symlink (canonicalized comparison)"
        );
    }
}
