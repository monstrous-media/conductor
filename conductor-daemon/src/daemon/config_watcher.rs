// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Config file watcher with debouncing

use crate::daemon::error::{DaemonError, Result};
use crate::daemon::types::DaemonCommand;
use notify_debouncer_full::notify::event::{EventKind, ModifyKind, RenameMode};
use notify_debouncer_full::notify::{Event, RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{DebounceEventResult, Debouncer, new_debouncer};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, error, info, warn};

// Platform-specific cache types for file watching
#[cfg(target_os = "macos")]
use notify_debouncer_full::FileIdMap as CacheType;
#[cfg(not(target_os = "macos"))]
use notify_debouncer_full::NoCache as CacheType;

/// Config file watcher with debouncing
pub struct ConfigWatcher {
    /// Shared config path — updated on retarget, read by debouncer closure
    config_path: Arc<std::sync::Mutex<PathBuf>>,
    debouncer: Option<Debouncer<RecommendedWatcher, CacheType>>,
    event_rx: mpsc::Receiver<PathBuf>,
    command_tx: mpsc::Sender<DaemonCommand>,
    shutdown_rx: broadcast::Receiver<()>,
    /// Channel to receive retarget requests
    retarget_rx: mpsc::Receiver<PathBuf>,
}

impl ConfigWatcher {
    /// Create a new config watcher
    ///
    /// Returns `(ConfigWatcher, retarget_tx)` where `retarget_tx` can be used to
    /// change the watched config path at runtime.
    pub fn new(
        config_path: impl Into<PathBuf>,
        command_tx: mpsc::Sender<DaemonCommand>,
        shutdown_rx: broadcast::Receiver<()>,
    ) -> Result<(Self, mpsc::Sender<PathBuf>)> {
        let config_path = config_path.into();
        let config_path_shared = Arc::new(std::sync::Mutex::new(config_path.clone()));

        // Create channel for debounced events
        let (event_tx, event_rx) = mpsc::channel(10);

        // Share path with closure so retargets update the filter
        let config_path_for_closure = Arc::clone(&config_path_shared);

        // Create debouncer with 500ms delay
        let debouncer = new_debouncer(
            Duration::from_millis(500),
            None,
            move |result: DebounceEventResult| {
                match result {
                    Ok(events) => {
                        let current_path = match config_path_for_closure.lock() {
                            Ok(guard) => guard.clone(),
                            Err(poisoned) => {
                                error!("config_path mutex poisoned; recovering inner value");
                                poisoned.into_inner().clone()
                            }
                        };
                        for event in events {
                            // Check if this is a modification event for our config file
                            if should_reload(&event.event, &current_path) {
                                debug!("Config file changed: {:?}", current_path);
                                // Hand off WITHOUT blocking the notify callback
                                // thread (a full/slow channel must never stall the OS
                                // file-event thread).
                                forward_reload_path(&event_tx, current_path.clone());
                                break; // Only send one event per batch
                            }
                        }
                    }
                    Err(errors) => {
                        for error in errors {
                            error!("Watch error: {:?}", error);
                        }
                    }
                }
            },
        )
        .map_err(|e| DaemonError::FileWatcher(format!("Failed to create debouncer: {}", e)))?;

        let (retarget_tx, retarget_rx) = mpsc::channel(4);

        Ok((
            Self {
                config_path: config_path_shared,
                debouncer: Some(debouncer),
                event_rx,
                command_tx,
                shutdown_rx,
                retarget_rx,
            },
            retarget_tx,
        ))
    }

    /// Start watching the config file
    pub async fn watch(&mut self) -> Result<()> {
        let current_path = lock_config_path(&self.config_path);

        // Get the parent directory to watch (watching the file directly may not work on all systems)
        let watch_path = current_path
            .parent()
            .ok_or_else(|| DaemonError::FileWatcher("No parent directory".to_string()))?
            .to_path_buf();

        info!("Starting config watcher for {:?}", current_path);
        info!("Watching directory: {:?}", watch_path);

        // Start watching the directory
        if let Some(ref mut debouncer) = self.debouncer {
            debouncer
                .watch(&watch_path, RecursiveMode::NonRecursive)
                .map_err(|e| {
                    DaemonError::FileWatcher(format!("Failed to watch directory: {}", e))
                })?;
        }

        // Event loop
        loop {
            tokio::select! {
                // Handle debounced file change events
                Some(path) = self.event_rx.recv() => {
                    info!("Config file changed, triggering reload: {:?}", path);

                    // Send reload command to engine manager
                    if let Err(e) = self.command_tx.send(DaemonCommand::ConfigFileChanged(path)).await {
                        error!("Failed to send config reload command: {}", e);
                    }
                }

                // Re-target to a different config file
                Some(new_path) = self.retarget_rx.recv() => {
                    let old_path = lock_config_path(&self.config_path);
                    info!("Re-targeting config watcher: {:?} -> {:?}", old_path, new_path);

                    // Unwatch old directory
                    if let Some(ref mut debouncer) = self.debouncer
                        && let Some(old_dir) = old_path.parent()
                    {
                        let _ = debouncer.unwatch(old_dir);
                    }

                    // Update shared path (also updates the debouncer closure's filter)
                    set_config_path(&self.config_path, new_path.clone());

                    // Watch new directory
                    let new_watch_path = new_path.parent()
                        .ok_or_else(|| DaemonError::FileWatcher("No parent directory for new config path".to_string()))?;

                    // A re-watch failure must NOT terminate the watch loop —
                    // propagating here previously killed `watch()` entirely, silently
                    // stopping ALL hot-reloading. Log + surface and keep the loop alive;
                    // hot-reload for the new path resumes on a later successful retarget.
                    // (The old directory was already unwatched and the shared path
                    // already updated above.) Only log the success message when the
                    // re-watch actually succeeded — otherwise the failure
                    // `error!` would be contradicted by an "re-targeted" success line.
                    match self.debouncer {
                        Some(ref mut debouncer) => {
                            match debouncer.watch(new_watch_path, RecursiveMode::NonRecursive) {
                                Ok(()) => info!("Config watcher re-targeted to {:?}", new_path),
                                Err(e) => error!(
                                    "Config watcher failed to watch new directory {:?} after \
                                     retarget: {}; hot-reload is disabled for this path until a \
                                     successful re-target — the watch loop continues",
                                    new_watch_path, e
                                ),
                            }
                        }
                        // No active debouncer (e.g. already stopped): the shared path was
                        // updated, but there is nothing to (re-)watch.
                        None => info!(
                            "Config watcher path updated to {:?} (no active debouncer to re-watch)",
                            new_path
                        ),
                    }
                }

                // Handle shutdown signal
                _ = self.shutdown_rx.recv() => {
                    info!("Config watcher shutting down");
                    break;
                }
            }
        }

        Ok(())
    }

    /// Stop watching (cleanup)
    pub fn stop(&mut self) {
        if let Some(mut debouncer) = self.debouncer.take() {
            // Unwatch all paths
            let current_path = lock_config_path(&self.config_path);
            if let Some(path) = current_path.parent() {
                let _ = debouncer.unwatch(path);
            }
        }
        debug!("Config watcher stopped");
    }
}

impl Drop for ConfigWatcher {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Lock the config path mutex, recovering from poison if needed.
fn lock_config_path(mutex: &std::sync::Mutex<PathBuf>) -> PathBuf {
    match mutex.lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => {
            error!("config_path mutex poisoned; recovering inner value");
            poisoned.into_inner().clone()
        }
    }
}

/// Update the config path mutex, recovering from poison if needed.
fn set_config_path(mutex: &std::sync::Mutex<PathBuf>, new_path: PathBuf) {
    match mutex.lock() {
        Ok(mut guard) => *guard = new_path,
        Err(poisoned) => {
            error!("config_path mutex poisoned; recovering and updating");
            *poisoned.into_inner() = new_path;
        }
    }
}

/// Forward a detected config-file change to the watch loop WITHOUT blocking the
/// notify callback thread.
///
/// The debouncer invokes its callback on the OS file-event thread; a
/// `blocking_send` there could stall that thread if the channel is full or its
/// receiver is slow. `try_send` never blocks: on a full channel we drop the
/// notification with a warning (a reload is already queued, and reloads are
/// idempotent — they re-read the *current* file, so coalescing is safe), and a
/// closed channel just means the watcher is shutting down.
fn forward_reload_path(event_tx: &mpsc::Sender<PathBuf>, path: PathBuf) {
    use tokio::sync::mpsc::error::TrySendError;
    match event_tx.try_send(path) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => {
            warn!(
                "config watcher: reload-event channel full; dropping notification \
                 (a reload is already queued — reloads are idempotent)"
            );
        }
        Err(TrySendError::Closed(_)) => {
            debug!("config watcher: reload-event channel closed (watcher shutting down)");
        }
    }
}

/// Check if an event should trigger a config reload
fn should_reload(event: &Event, config_path: &Path) -> bool {
    // Check if the event is a modification
    match event.kind {
        EventKind::Modify(ModifyKind::Data(_)) | EventKind::Modify(ModifyKind::Any) => {
            // Check if the event is for our config file
            event.paths.iter().any(|p| p == config_path)
        }
        // Atomic-save editors (vim, VS Code, …) — and our own atomic
        // config writes — write a temp file then RENAME it over the target.
        // Replacing an EXISTING config surfaces as `Modify(Name(To|Both))` on
        // the destination path, NOT `Modify(Data)` or `Create` (the `Create`
        // arm below only fires when the target didn't previously exist).
        //
        // Exclude `RenameMode::From`: a `From` event naming the
        // config means the config was renamed AWAY (e.g. an editor moving the
        // existing config to a backup before writing the new one). The file is
        // gone at that instant, so reloading would read a missing file — the
        // subsequent write/rename of the NEW config fires its own event. `To`,
        // `Both`, and the ambiguous `Any`/`Other` modes still reload (the path
        // check confirms the config is involved).
        EventKind::Modify(ModifyKind::Name(mode)) if mode != RenameMode::From => {
            event.paths.iter().any(|p| p == config_path)
        }
        EventKind::Create(_) => {
            // Also reload on create (e.g., atomic save that creates a new file)
            event.paths.iter().any(|p| p == config_path)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use tokio::time::sleep;

    #[tokio::test]
    async fn test_config_watcher_creation() {
        let temp_dir = tempdir().unwrap();
        let config_path = temp_dir.path().join("config.toml");

        // Create a dummy config file
        std::fs::write(&config_path, "# test config").unwrap();

        let (cmd_tx, _cmd_rx) = mpsc::channel(10);
        let (shutdown_tx, shutdown_rx) = broadcast::channel(1);

        let result = ConfigWatcher::new(config_path, cmd_tx, shutdown_rx);
        assert!(result.is_ok());

        // Cleanup
        drop(shutdown_tx);
    }

    #[tokio::test]
    #[ignore] // File watching can be flaky in CI/test environments
    async fn test_config_watcher_detects_changes() {
        let temp_dir = tempdir().unwrap();
        let config_path = temp_dir.path().join("config.toml");

        // Create initial config file
        std::fs::write(&config_path, "# initial config").unwrap();

        let (cmd_tx, mut cmd_rx) = mpsc::channel(10);
        let (_shutdown_tx, shutdown_rx) = broadcast::channel(1);

        let (mut watcher, _retarget_tx) =
            ConfigWatcher::new(&config_path, cmd_tx, shutdown_rx).unwrap();

        // Start watching in background
        let watcher_handle = tokio::spawn(async move {
            let _ = watcher.watch().await;
        });

        // Wait for watcher to initialize
        sleep(Duration::from_millis(200)).await;

        // Modify the config file multiple times to ensure detection
        for i in 0..3 {
            std::fs::write(&config_path, format!("# modified config {}", i)).unwrap();
            sleep(Duration::from_millis(100)).await;
        }

        // Wait for debounce + processing (longer timeout for CI)
        let result = tokio::time::timeout(Duration::from_secs(3), cmd_rx.recv()).await;

        // Should receive a config changed command
        assert!(result.is_ok(), "Timeout waiting for config change event");
        if let Ok(Some(DaemonCommand::ConfigFileChanged(path))) = result {
            assert_eq!(path, config_path);
        } else {
            panic!("Expected ConfigFileChanged command");
        }

        // Cleanup
        watcher_handle.abort();
    }

    #[tokio::test]
    async fn forward_reload_path_drops_on_full_channel_without_blocking() {
        // The notify-callback hand-off must never block the OS file-event
        // thread. On a full channel, `forward_reload_path` drops the notification
        // (a reload is already queued + idempotent) rather than blocking.
        let (tx, mut rx) = mpsc::channel::<PathBuf>(1);
        let a = PathBuf::from("/cfg/a.toml");
        let b = PathBuf::from("/cfg/b.toml");

        // Fill the single slot, then forward a second path: must return
        // immediately (test completing IS the no-block proof) and drop `b`.
        tx.try_send(a.clone()).unwrap();
        forward_reload_path(&tx, b.clone());

        assert_eq!(rx.recv().await, Some(a), "the queued reload survives");
        assert!(
            rx.try_recv().is_err(),
            "the second notification was dropped on a full channel, not blocked/queued"
        );

        // A closed channel (receiver dropped) is a no-op, not a panic/block.
        drop(rx);
        forward_reload_path(&tx, PathBuf::from("/cfg/c.toml"));
    }

    #[tokio::test]
    async fn retarget_to_unwatchable_dir_does_not_kill_watch_loop() {
        // A re-watch failure on retarget must be logged + recoverable, NOT
        // fatal to the watch loop (propagating `?` previously terminated `watch()`,
        // silently stopping all hot-reloading). Retarget to a path under a
        // non-existent directory so `debouncer.watch()` fails, then shut down and
        // assert the loop exited cleanly (Ok) rather than erroring out.
        let temp_dir = tempdir().unwrap();
        let config_path = temp_dir.path().join("config.toml");
        std::fs::write(&config_path, "# initial config").unwrap();

        let (cmd_tx, _cmd_rx) = mpsc::channel(10);
        let (shutdown_tx, shutdown_rx) = broadcast::channel(1);
        let (mut watcher, retarget_tx) =
            ConfigWatcher::new(&config_path, cmd_tx, shutdown_rx).unwrap();

        let handle = tokio::spawn(async move { watcher.watch().await });

        // Let the loop start + watch the initial (valid) directory.
        sleep(Duration::from_millis(150)).await;

        // Retarget to a config whose parent directory does not exist → the
        // debouncer's `watch()` of that directory fails.
        retarget_tx
            .send(
                temp_dir
                    .path()
                    .join("no-such-subdir-2197")
                    .join("config.toml"),
            )
            .await
            .unwrap();

        // Give the loop time to process the (failing) retarget before shutdown.
        sleep(Duration::from_millis(150)).await;
        shutdown_tx.send(()).unwrap();

        let res = tokio::time::timeout(Duration::from_secs(3), handle)
            .await
            .expect("watch loop should terminate on shutdown, not hang")
            .expect("watch task should not panic");
        assert!(
            res.is_ok(),
            "a retarget re-watch failure must not terminate the watch loop; got {res:?}"
        );
    }

    #[test]
    fn test_should_reload_on_modify() {
        let config_path = PathBuf::from("/tmp/config.toml");

        let event = Event {
            kind: EventKind::Modify(ModifyKind::Data(
                notify_debouncer_full::notify::event::DataChange::Any,
            )),
            paths: vec![config_path.clone()],
            attrs: Default::default(),
        };

        assert!(should_reload(&event, &config_path));
    }

    #[test]
    fn test_should_not_reload_on_other_file() {
        let config_path = PathBuf::from("/tmp/config.toml");
        let other_path = PathBuf::from("/tmp/other.toml");

        let event = Event {
            kind: EventKind::Modify(ModifyKind::Data(
                notify_debouncer_full::notify::event::DataChange::Any,
            )),
            paths: vec![other_path],
            attrs: Default::default(),
        };

        assert!(!should_reload(&event, &config_path));
    }

    #[test]
    fn test_should_reload_on_create() {
        let config_path = PathBuf::from("/tmp/config.toml");

        let event = Event {
            kind: EventKind::Create(notify_debouncer_full::notify::event::CreateKind::File),
            paths: vec![config_path.clone()],
            attrs: Default::default(),
        };

        assert!(should_reload(&event, &config_path));
    }

    /// ConfigWatcher::new returns retarget channel
    #[tokio::test]
    async fn test_config_watcher_returns_retarget_channel() {
        let temp_dir = tempdir().unwrap();
        let config_path = temp_dir.path().join("config.toml");
        std::fs::write(&config_path, "# test config").unwrap();

        let (cmd_tx, _cmd_rx) = mpsc::channel(10);
        let (shutdown_tx, shutdown_rx) = broadcast::channel(1);

        let (watcher, retarget_tx) = ConfigWatcher::new(&config_path, cmd_tx, shutdown_rx).unwrap();

        // Retarget channel should be usable
        assert!(!retarget_tx.is_closed());

        drop(watcher);
        drop(shutdown_tx);
    }

    /// Shared config path updated on retarget
    #[tokio::test]
    async fn test_config_watcher_shared_path_update() {
        let temp_dir = tempdir().unwrap();
        let config_path = temp_dir.path().join("config.toml");
        let new_config_path = temp_dir.path().join("profile.toml");
        std::fs::write(&config_path, "# original").unwrap();
        std::fs::write(&new_config_path, "# profile").unwrap();

        let (cmd_tx, _cmd_rx) = mpsc::channel(10);
        let (_shutdown_tx, shutdown_rx) = broadcast::channel(1);

        let (watcher, _retarget_tx) =
            ConfigWatcher::new(&config_path, cmd_tx, shutdown_rx).unwrap();

        // Verify initial path
        let initial = watcher.config_path.lock().unwrap().clone();
        assert_eq!(initial, config_path);

        // Simulate retarget by directly updating shared path
        *watcher.config_path.lock().unwrap() = new_config_path.clone();

        let updated = watcher.config_path.lock().unwrap().clone();
        assert_eq!(updated, new_config_path);
    }

    // ── Atomic-rename-save detection ─────────────────────────────

    /// An atomic save replaces the config by renaming a temp file over
    /// it. Replacing an existing file surfaces as `Modify(Name(To))` on the
    /// destination path — which must trigger a reload. Pre-fix, `should_reload`
    /// only matched `Modify(Data/Any)` + `Create`, so this was silently missed.
    #[test]
    fn atomic_rename_to_config_path_triggers_reload() {
        let config_path = Path::new("/tmp/conductor-test/config.toml");
        let event = Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::To)))
            .add_path(config_path.to_path_buf());
        assert!(
            should_reload(&event, config_path),
            "atomic rename-over-target must trigger a config reload"
        );
    }

    /// A `RenameMode::Both` event carries both the temp source and the config
    /// destination; the config-path match must still fire.
    #[test]
    fn atomic_rename_both_paths_triggers_reload() {
        let config_path = Path::new("/tmp/conductor-test/config.toml");
        let event = Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
            .add_path(PathBuf::from("/tmp/conductor-test/.config.toml.tmp"))
            .add_path(config_path.to_path_buf());
        assert!(
            should_reload(&event, config_path),
            "rename event listing the config as destination must reload"
        );
    }

    /// A rename whose paths don't include the config file must NOT reload
    /// (e.g. the temp source being moved away, or an unrelated file).
    #[test]
    fn rename_not_targeting_config_does_not_reload() {
        let config_path = Path::new("/tmp/conductor-test/config.toml");
        let event = Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::To)))
            .add_path(PathBuf::from("/tmp/conductor-test/unrelated.toml"));
        assert!(
            !should_reload(&event, config_path),
            "a rename not targeting the config must not trigger a reload"
        );
    }

    /// A `RenameMode::From` event naming the config
    /// means the config was renamed AWAY (moved to a backup) — the file is gone
    /// at that instant, so it must NOT trigger a reload.
    #[test]
    fn rename_from_config_path_does_not_reload() {
        let config_path = Path::new("/tmp/conductor-test/config.toml");
        let event = Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::From)))
            .add_path(config_path.to_path_buf());
        assert!(
            !should_reload(&event, config_path),
            "config renamed AWAY (From) must not reload — the file is gone"
        );
    }

    /// Regression guard: the existing Modify(Data) / Create paths still work.
    #[test]
    fn data_modify_and_create_still_reload() {
        let config_path = Path::new("/tmp/conductor-test/config.toml");
        let data = Event::new(EventKind::Modify(ModifyKind::Data(
            notify_debouncer_full::notify::event::DataChange::Content,
        )))
        .add_path(config_path.to_path_buf());
        assert!(should_reload(&data, config_path));

        let create = Event::new(EventKind::Create(
            notify_debouncer_full::notify::event::CreateKind::File,
        ))
        .add_path(config_path.to_path_buf());
        assert!(should_reload(&create, config_path));
    }
}
