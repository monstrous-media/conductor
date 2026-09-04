// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Frontmost Application Detection for Daemon (Phase 2)
//!
//! Monitors the frontmost macOS application and triggers automatic profile
//! switches via DaemonCommand when the app changes and a matching profile exists.

#![allow(deprecated)] // cocoa deprecation warnings
#![allow(unexpected_cfgs)]

use crate::daemon::platform::window_title;
use crate::daemon::types::DaemonCommand;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{RwLock, mpsc};
use tracing::{debug, info, warn};

/// Load profile-to-app mappings from the profiles manifest file.
///
/// The GUI writes `profiles.json` in the profiles directory on every mutation.
/// Returns an empty map if the file doesn't exist or can't be parsed.
pub fn load_mappings_from_manifest(profiles_dir: &Path) -> HashMap<String, ProfileMapping> {
    let manifest_path = profiles_dir.join("profiles.json");
    if !manifest_path.exists() {
        debug!("No profiles manifest found at {:?}", manifest_path);
        return HashMap::new();
    }

    let content = match std::fs::read_to_string(&manifest_path) {
        Ok(c) => c,
        Err(e) => {
            warn!("Failed to read profiles manifest: {}", e);
            return HashMap::new();
        }
    };

    // Support both legacy format (bare array) and new format ({ profiles: [...] })
    let profiles: Vec<serde_json::Value> = match serde_json::from_str::<serde_json::Value>(&content)
    {
        Ok(serde_json::Value::Array(arr)) => arr,
        Ok(obj) if obj.is_object() => obj
            .get("profiles")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default(),
        Ok(_) => {
            warn!("Profiles manifest has unexpected top-level type");
            return HashMap::new();
        }
        Err(e) => {
            warn!("Failed to parse profiles manifest: {}", e);
            return HashMap::new();
        }
    };

    let mut mappings = HashMap::new();
    for profile in &profiles {
        let name = profile["name"].as_str().unwrap_or_default();
        let config_path = profile["config_path"].as_str().unwrap_or_default();
        if let Some(bundle_ids) = profile["bundle_ids"].as_array() {
            for bid in bundle_ids {
                if let Some(bundle_id) = bid.as_str()
                    && !bundle_id.is_empty()
                    && !name.is_empty()
                    && !config_path.is_empty()
                {
                    mappings.insert(
                        bundle_id.to_string(),
                        ProfileMapping {
                            profile_name: name.to_string(),
                            config_path: PathBuf::from(config_path),
                        },
                    );
                }
            }
        }
    }

    info!(
        "Loaded {} app-profile mappings from manifest",
        mappings.len()
    );
    mappings
}

/// Information about the frontmost application
#[derive(Debug, Clone, PartialEq)]
pub struct AppInfo {
    pub bundle_id: String,
    pub name: String,
}

/// Bundle ID to profile mapping entry
#[derive(Debug, Clone)]
pub struct ProfileMapping {
    pub profile_name: String,
    pub config_path: PathBuf,
}

/// Daemon-side app detector that auto-switches profiles
///
/// # Implementation Note (S5)
/// Currently uses polling (configurable interval, default 500ms). An event-driven
/// approach via `NSWorkspace.didActivateApplicationNotification` would be more
/// power-efficient but requires an NSRunLoop integration with tokio, which is
/// non-trivial. The polling cost is negligible for a daemon already processing
/// real-time MIDI events. Consider migrating if/when `objc2-foundation` adoption
/// makes notification observers easier to integrate.
pub struct DaemonAppDetector {
    /// Bundle ID → profile mapping
    mappings: Arc<RwLock<HashMap<String, ProfileMapping>>>,

    /// Channel to send commands to engine manager
    command_tx: mpsc::Sender<DaemonCommand>,

    /// Polling interval
    poll_interval_ms: u64,

    /// Whether detection is active
    is_active: Arc<RwLock<bool>>,

    /// Currently tracked frontmost app (to detect changes)
    current_bundle_id: Arc<RwLock<Option<String>>>,

    /// Cached focused-window title — the §4.5 snapshot-reconciler seam.
    ///
    /// An app change invalidates this to `None` *before* any mode/profile
    /// decision, so the resolver can never match a stale (old-app) title (§4.5).
    /// The Slice-6 title poller (when enabled) fills it on the next tick after a
    /// change; when title polling is disabled it stays `None` and window rules
    /// requiring a title simply don't match.
    current_window_title: Arc<RwLock<Option<String>>>,

    /// Focused-window-title source (ADR-040 §4.3, Slice 6) — `Some` **only when
    /// `[per_app_modes].window_rules` are present** (lazy: title reads are the
    /// only thing that invokes the macOS Accessibility APIs, so a config without
    /// window rules never touches Accessibility). No system permission dialog is
    /// shown; an ungranted permission surfaces via observable degradation. `None`
    /// ⇒ no title polling. `Arc` so the loop task can clone it (see the
    /// precondition note at the clone site in `start`).
    title_source: Option<Arc<dyn window_title::WindowTitleSource>>,

    /// Title poll interval (ms), already clamped to the safe floor. Only used
    /// when `title_source` is `Some`.
    window_title_poll_ms: u64,

    /// Observable-degradation tracker for title reads (warn-once + shared status
    /// flag). Always present; only exercised when title polling is enabled.
    degradation: window_title::DegradationTracker,

    /// Task handle for the detection loop; retained so stop() can abort it
    task_handle: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

impl DaemonAppDetector {
    /// Create a new daemon app detector
    pub fn new(command_tx: mpsc::Sender<DaemonCommand>, poll_interval_ms: u64) -> Self {
        Self {
            mappings: Arc::new(RwLock::new(HashMap::new())),
            command_tx,
            poll_interval_ms,
            is_active: Arc::new(RwLock::new(false)),
            current_bundle_id: Arc::new(RwLock::new(None)),
            current_window_title: Arc::new(RwLock::new(None)),
            title_source: None,
            window_title_poll_ms: 0,
            degradation: window_title::DegradationTracker::new(),
            task_handle: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    /// Enable focused-window-title polling (ADR-040 §4.3, Slice 6).
    ///
    /// Call this **only when `[per_app_modes].window_rules` are present** — that
    /// is the lazy gate: a config without window rules never invokes the macOS
    /// Accessibility APIs at all (no dialog is shown; permission must be granted
    /// out-of-band, else reads degrade observably). `poll_ms` is clamped to the
    /// safe floor. **Precondition:** must be called before [`start`](Self::start)
    /// — `start` clones `title_source` into the spawned loop, so a later call
    /// won't be picked up.
    pub fn enable_title_polling(
        &mut self,
        source: Arc<dyn window_title::WindowTitleSource>,
        poll_ms: u64,
    ) {
        self.title_source = Some(source);
        self.window_title_poll_ms = window_title::clamp_poll_ms(poll_ms);
        info!(
            "Window-title polling enabled (poll {}ms)",
            self.window_title_poll_ms
        );
    }

    /// Whether the title subsystem is currently degraded (window rules
    /// configured but titles unreadable — permission ungranted/unsupported).
    /// Surfaced as `window_permission_degraded` in `conductor_mode_status`.
    pub fn window_permission_degraded(&self) -> bool {
        self.degradation.is_degraded()
    }

    /// Whether title polling is enabled (window rules present). When `false`,
    /// `window_permission_degraded` is not meaningful (no titles are read).
    pub fn title_polling_active(&self) -> bool {
        self.title_source.is_some()
    }

    /// Update the bundle ID → profile mappings
    pub async fn update_mappings(&self, mappings: HashMap<String, ProfileMapping>) {
        let count = mappings.len();
        *self.mappings.write().await = mappings;
        info!("App detector updated with {} profile mappings", count);
    }

    /// Detect the frontmost macOS application
    #[cfg(target_os = "macos")]
    fn detect_frontmost_app() -> Option<AppInfo> {
        use cocoa::base::{id, nil};
        use cocoa::foundation::NSAutoreleasePool;
        use objc::{class, msg_send, sel, sel_impl};

        unsafe {
            let pool = NSAutoreleasePool::new(nil);

            // Build result inside a block so pool is always drained (prevents
            // autorelease leaks on early-exit paths — review comment #6).
            let result = {
                let workspace: id = msg_send![class!(NSWorkspace), sharedWorkspace];
                let frontmost_app: id = msg_send![workspace, frontmostApplication];

                if frontmost_app == nil {
                    None
                } else {
                    let bundle_id_ns: id = msg_send![frontmost_app, bundleIdentifier];
                    let bundle_id = if bundle_id_ns != nil {
                        let c_str: *const i8 = msg_send![bundle_id_ns, UTF8String];
                        if !c_str.is_null() {
                            Some(
                                std::ffi::CStr::from_ptr(c_str)
                                    .to_string_lossy()
                                    .into_owned(),
                            )
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    if let Some(bundle_id) = bundle_id {
                        let name_ns: id = msg_send![frontmost_app, localizedName];
                        let name = if name_ns != nil {
                            let c_str: *const i8 = msg_send![name_ns, UTF8String];
                            if !c_str.is_null() {
                                std::ffi::CStr::from_ptr(c_str)
                                    .to_string_lossy()
                                    .into_owned()
                            } else {
                                "Unknown".to_string()
                            }
                        } else {
                            "Unknown".to_string()
                        };

                        Some(AppInfo { bundle_id, name })
                    } else {
                        None
                    }
                }
            };

            let _: () = msg_send![pool, drain];
            result
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn detect_frontmost_app() -> Option<AppInfo> {
        None // Platform-specific implementation needed
    }

    /// Start the detection loop
    pub async fn start(&self) {
        let mut is_active = self.is_active.write().await;
        if *is_active {
            return;
        }
        *is_active = true;
        drop(is_active);

        let mappings = Arc::clone(&self.mappings);
        let command_tx = self.command_tx.clone();
        let is_active = Arc::clone(&self.is_active);
        let current_bundle_id = Arc::clone(&self.current_bundle_id);
        let current_window_title = Arc::clone(&self.current_window_title);
        // Precondition (enforced by `enable_title_polling`'s doc): the source is
        // set BEFORE `start`, so this clone captures the configured `Some`/`None`.
        // A later `enable_title_polling` would not affect the already-spawned loop.
        let title_source = self.title_source.clone();
        let degradation = self.degradation.clone();
        // §4.3: enabling window rules changes the loop cadence itself — when title
        // polling is on, the whole loop ticks at the (clamped) title cadence (not
        // just adding a title poll on top of the app cadence). Reading the app
        // every tick keeps each app/title pair coherent (§4.5). NB: on an app
        // *change* the title is invalidated and re-read on the NEXT tick (see the
        // change/no-change branches below), not synchronously — the §4.3 accepted
        // one-tick latency. With no title polling the app cadence governs as before.
        let tick_ms = if title_source.is_some() {
            self.window_title_poll_ms
        } else {
            self.poll_interval_ms
        };

        let handle = tokio::spawn(async move {
            info!(
                "Daemon app detector started (poll interval: {}ms, title polling: {})",
                tick_ms,
                title_source.is_some()
            );

            loop {
                if !*is_active.read().await {
                    break;
                }

                // catch_unwind guards against panics in unsafe FFI
                let app_result = std::panic::catch_unwind(Self::detect_frontmost_app);
                let app = match app_result {
                    Ok(app) => app,
                    Err(e) => {
                        warn!("App detection panicked: {:?}", e);
                        None
                    }
                };

                if let Some(app) = app {
                    let mut current = current_bundle_id.write().await;
                    let changed = match &*current {
                        Some(prev) => prev != &app.bundle_id,
                        None => true,
                    };

                    if changed {
                        debug!("Frontmost app changed to: {} ({})", app.name, app.bundle_id);
                        *current = Some(app.bundle_id.clone());
                        drop(current);

                        // Resolve the matching profile (if any) under the lock, then
                        // release it before the async sends (don't hold a read lock
                        // across `.await`). §4.5: emit_context_switch invalidates the
                        // (now-stale) title to None; the title poll below re-resolves
                        // on a later tick once the new app's title is read.
                        let mapping = mappings.read().await.get(&app.bundle_id).cloned();
                        emit_context_switch(mapping, &current_window_title, &command_tx, &app)
                            .await;
                    } else if let Some(ref source) = title_source {
                        // App unchanged: poll the focused-window title (§4.3). A
                        // title change without an app change (e.g. switching
                        // documents in VS Code) re-resolves so title-scoped
                        // window_rules flip — with up to one-tick latency (§4.3 R3).
                        drop(current);
                        let read = read_title_guarded(source.as_ref());
                        // record() yields the title for RESOLUTION (None on a failed
                        // read, so app-name rules apply) while independently setting
                        // the shared degraded flag for STATUS — the two concerns are
                        // deliberately split: resolution must proceed (with no title)
                        // even while degraded, and `mode_status_json` reads the flag
                        // separately to surface `window_permission_degraded`.
                        let new_title = degradation.record(read);
                        emit_title_resolve(
                            &app.name,
                            new_title,
                            &current_window_title,
                            &command_tx,
                        )
                        .await;
                    }
                }

                tokio::time::sleep(Duration::from_millis(tick_ms)).await;
            }

            info!("Daemon app detector stopped");
        });

        // Retain handle so we can abort on stop
        *self.task_handle.lock().await = Some(handle);
    }

    /// Stop detection
    pub async fn stop(&self) {
        *self.is_active.write().await = false;
        if let Some(handle) = self.task_handle.lock().await.take() {
            handle.abort();
        }
    }

    /// Check if detection is active
    #[allow(dead_code)]
    pub async fn is_active(&self) -> bool {
        *self.is_active.read().await
    }
}

/// Emit the context-switch commands for a detected frontmost-app change, in the
/// ADR-040 §4.7 order: **profile switch first** (if the app maps to a profile),
/// **then mode resolve**.
///
/// §4.5 (snapshot reconciler): the cached window title is invalidated to `None`
/// *before* the snapshot is built, so a stale (old-app) title can never drive
/// the mode resolve. The title that the run loop ultimately resolves against is
/// the post-invalidation value (`None` until the Slice-6 poller fills it).
///
/// Both commands go on the same channel, so the run loop's FIFO consumer
/// processes `ProfileSwitch` (heavyweight reload) before `ResolveContextMode` —
/// that is what makes the mode resolve against the *new* profile's
/// `[per_app_modes]` (§4.7). `ProfileSwitch` is fire-and-forget (`result_tx:
/// None`) as before; ordering does not depend on awaiting its result, only on
/// channel order.
///
/// Kept a free fn (not a `&self` method) so the loop's spawned task — which owns
/// only `Arc` clones, not `&self` — can call it, and so the command-emission
/// logic is unit-testable without constructing a detector or touching FFI.
async fn emit_context_switch(
    profile_mapping: Option<ProfileMapping>,
    window_title_cache: &RwLock<Option<String>>,
    command_tx: &mpsc::Sender<DaemonCommand>,
    app: &AppInfo,
) {
    // §4.5 — invalidate the cached title before any decision; read back the
    // reconciled value for the snapshot.
    let window_title = {
        let mut title = window_title_cache.write().await;
        *title = None;
        title.clone()
    };

    // §4.7 — profile switch FIRST (heavyweight full reload).
    if let Some(mapping) = profile_mapping {
        info!(
            "Auto-switching profile to '{}' for app {} ({})",
            mapping.profile_name, app.name, app.bundle_id
        );
        if let Err(e) = command_tx
            .send(DaemonCommand::ProfileSwitch {
                profile_name: mapping.profile_name.clone(),
                config_path: mapping.config_path.display().to_string(),
                // ProfileMapping doesn't carry the GUI profile id yet;
                // auto-switches persist identity without it (id is additive).
                profile_id: None,
                result_tx: None,
            })
            .await
        {
            warn!("Failed to send auto profile switch: {}", e);
        }
    }

    // …THEN mode resolve within the (possibly reloaded) profile. The resolver
    // keys on the app *name* (not the bundle id profiles use).
    if let Err(e) = command_tx
        .send(DaemonCommand::ResolveContextMode {
            app: app.name.clone(),
            window_title,
        })
        .await
    {
        warn!("Failed to send context mode resolve: {}", e);
    }
}

/// Read the focused-window title, guarding against a panic in the unsafe
/// platform FFI (mirrors the app-detection `catch_unwind`). A panic is treated
/// as a denied read (observable degradation) rather than tearing down the
/// detector task.
fn read_title_guarded(source: &dyn window_title::WindowTitleSource) -> window_title::TitleRead {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        source.focused_window_title()
    })) {
        Ok(read) => read,
        Err(e) => {
            warn!("Window-title read panicked: {:?}", e);
            window_title::TitleRead::PermissionDenied
        }
    }
}

/// On a title-only change (frontmost app unchanged), update the title cache and
/// emit a `ResolveContextMode` so title-scoped `window_rules` re-evaluate
/// against the new `(app, title)` snapshot (§4.3). A no-op (no emit) when the
/// title is unchanged — so a steady title doesn't re-resolve every tick. Returns
/// whether it emitted.
///
/// Free fn for the same testability reason as [`emit_context_switch`]: the
/// title-change decision is unit-tested with a bare channel + cache.
async fn emit_title_resolve(
    app_name: &str,
    new_title: Option<String>,
    window_title_cache: &RwLock<Option<String>>,
    command_tx: &mpsc::Sender<DaemonCommand>,
) -> bool {
    // Unchanged title → nothing to do (cheap read-lock fast path).
    if *window_title_cache.read().await == new_title {
        return false;
    }
    *window_title_cache.write().await = new_title.clone();
    if let Err(e) = command_tx
        .send(DaemonCommand::ResolveContextMode {
            app: app_name.to_string(),
            window_title: new_title,
        })
        .await
    {
        warn!("Failed to send title-change mode resolve: {}", e);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_detector_creation() {
        let (tx, _rx) = mpsc::channel(10);
        let detector = DaemonAppDetector::new(tx, 500);
        assert!(!detector.is_active().await);
    }

    #[tokio::test]
    async fn test_update_mappings() {
        let (tx, _rx) = mpsc::channel(10);
        let detector = DaemonAppDetector::new(tx, 500);

        let mut mappings = HashMap::new();
        mappings.insert(
            "com.apple.logic10".to_string(),
            ProfileMapping {
                profile_name: "Logic Pro".to_string(),
                config_path: PathBuf::from("/profiles/logic.toml"),
            },
        );

        detector.update_mappings(mappings).await;
        let stored = detector.mappings.read().await;
        assert_eq!(stored.len(), 1);
        assert!(stored.contains_key("com.apple.logic10"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[allow(clippy::print_stderr)] // intentional: a test-skip notice on headless runners
    fn test_detect_frontmost_app() {
        // NSWorkspace frontmost-app detection only returns `Some` inside a GUI
        // login session. A headless self-hosted runner (a LaunchDaemon with no
        // Aqua session) has no frontmost app and returns `None` — skip there
        // rather than fail. With a GUI session (dev machine or
        // GitHub-hosted runner) the result is validated for real.
        let Some(info) = DaemonAppDetector::detect_frontmost_app() else {
            eprintln!(
                "skipping test_detect_frontmost_app: no GUI session \
                 (headless runner) — FU-9 #1888"
            );
            return;
        };
        assert!(!info.bundle_id.is_empty());
        assert!(!info.name.is_empty());
    }

    #[tokio::test]
    async fn test_start_stop() {
        let (tx, _rx) = mpsc::channel(10);
        let detector = DaemonAppDetector::new(tx, 500);

        detector.start().await;
        assert!(detector.is_active().await);

        detector.stop().await;
        // Give the task a moment to notice the flag
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!detector.is_active().await);
    }

    #[test]
    fn test_load_mappings_from_manifest() {
        let temp_dir = tempfile::tempdir().unwrap();
        let manifest = serde_json::json!([
            {
                "id": "logic",
                "name": "Logic Pro",
                "bundle_ids": ["com.apple.logic10"],
                "config_path": "/profiles/logic.toml",
                "is_default": false
            },
            {
                "id": "ableton",
                "name": "Ableton Live",
                "bundle_ids": ["com.ableton.live", "com.ableton.live.lite"],
                "config_path": "/profiles/ableton.toml",
                "is_default": false
            },
            {
                "id": "default",
                "name": "Default",
                "bundle_ids": [],
                "config_path": "/profiles/default.toml",
                "is_default": true
            }
        ]);
        std::fs::write(
            temp_dir.path().join("profiles.json"),
            serde_json::to_string(&manifest).unwrap(),
        )
        .unwrap();

        let mappings = load_mappings_from_manifest(temp_dir.path());
        assert_eq!(mappings.len(), 3); // logic + ableton + ableton.lite
        assert_eq!(mappings["com.apple.logic10"].profile_name, "Logic Pro");
        assert_eq!(mappings["com.ableton.live"].profile_name, "Ableton Live");
        assert_eq!(
            mappings["com.ableton.live.lite"].profile_name,
            "Ableton Live"
        );
    }

    #[test]
    fn test_load_mappings_missing_manifest() {
        let temp_dir = tempfile::tempdir().unwrap();
        let mappings = load_mappings_from_manifest(temp_dir.path());
        assert!(mappings.is_empty());
    }

    #[test]
    fn test_load_mappings_invalid_json() {
        let temp_dir = tempfile::tempdir().unwrap();
        std::fs::write(temp_dir.path().join("profiles.json"), "not valid json").unwrap();
        let mappings = load_mappings_from_manifest(temp_dir.path());
        assert!(mappings.is_empty());
    }

    // ── ADR-040: emit_context_switch ───────────────────────
    //
    // These drive the command-emission logic directly with an injected
    // `AppInfo`, so they need no NSWorkspace/FFI and run on every platform.

    fn app(bundle_id: &str, name: &str) -> AppInfo {
        AppInfo {
            bundle_id: bundle_id.to_string(),
            name: name.to_string(),
        }
    }

    #[tokio::test]
    async fn emit_mapped_app_sends_profile_switch_then_mode_resolve() {
        // §4.7 ordering: a mapped app emits ProfileSwitch FIRST, then
        // ResolveContextMode — and the resolve keys on the app *name*.
        let (tx, mut rx) = mpsc::channel(8);
        let title = RwLock::new(None);
        let mapping = ProfileMapping {
            profile_name: "Logic Pro".to_string(),
            config_path: PathBuf::from("/profiles/logic.toml"),
        };

        emit_context_switch(
            Some(mapping),
            &title,
            &tx,
            &app("com.apple.logic10", "Logic Pro"),
        )
        .await;

        match rx.try_recv().expect("first command") {
            DaemonCommand::ProfileSwitch { profile_name, .. } => {
                assert_eq!(profile_name, "Logic Pro");
            }
            other => panic!("expected ProfileSwitch first, got {other:?}"),
        }
        match rx.try_recv().expect("second command") {
            DaemonCommand::ResolveContextMode { app, window_title } => {
                assert_eq!(app, "Logic Pro", "resolver keys on app name, not bundle id");
                assert_eq!(window_title, None);
            }
            other => panic!("expected ResolveContextMode second, got {other:?}"),
        }
        assert!(rx.try_recv().is_err(), "exactly two commands emitted");
    }

    #[tokio::test]
    async fn emit_unmapped_app_sends_only_mode_resolve() {
        // No profile mapping → no ProfileSwitch, but the mode path still runs:
        // an app with no profile can still match a [per_app_modes] rule.
        let (tx, mut rx) = mpsc::channel(8);
        let title = RwLock::new(None);

        emit_context_switch(None, &title, &tx, &app("com.unmapped.app", "Unmapped")).await;

        match rx.try_recv().expect("only command") {
            DaemonCommand::ResolveContextMode { app, .. } => assert_eq!(app, "Unmapped"),
            other => panic!("expected ResolveContextMode, got {other:?}"),
        }
        assert!(
            rx.try_recv().is_err(),
            "no ProfileSwitch for an unmapped app"
        );
    }

    #[tokio::test]
    async fn emit_invalidates_stale_window_title() {
        // §4.5 snapshot reconciler: the app change must invalidate the cached
        // title BEFORE the snapshot is built, so the emitted ResolveContextMode
        // never carries a stale (old-app) title.
        let (tx, mut rx) = mpsc::channel(8);
        // Seed a stale title as if the previous app's poller had cached it.
        let title = RwLock::new(Some("Untitled — TextEdit".to_string()));

        emit_context_switch(None, &title, &tx, &app("com.ableton.live", "Ableton Live")).await;

        // Cache cleared…
        assert_eq!(
            *title.read().await,
            None,
            "stale title invalidated on app change"
        );
        // …and the emitted command carries the reconciled (None) title, not the
        // stale one.
        match rx.try_recv().expect("command") {
            DaemonCommand::ResolveContextMode { window_title, .. } => {
                assert_eq!(
                    window_title, None,
                    "snapshot must not carry the stale title"
                );
            }
            other => panic!("expected ResolveContextMode, got {other:?}"),
        }
    }

    // ── ADR-040: emit_title_resolve ────────────────────────

    #[tokio::test]
    async fn title_change_emits_resolve_with_new_title() {
        // §4.3: a title change with no app change re-resolves so title-scoped
        // window_rules flip. The cache updates and the command carries the new
        // title against the current app name.
        let (tx, mut rx) = mpsc::channel(8);
        let title = RwLock::new(None);

        let emitted =
            emit_title_resolve("Code", Some("main.rs — myproj".into()), &title, &tx).await;

        assert!(emitted, "a title change emits a re-resolve");
        assert_eq!(*title.read().await, Some("main.rs — myproj".to_string()));
        match rx.try_recv().expect("command") {
            DaemonCommand::ResolveContextMode { app, window_title } => {
                assert_eq!(app, "Code");
                assert_eq!(window_title, Some("main.rs — myproj".to_string()));
            }
            other => panic!("expected ResolveContextMode, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unchanged_title_does_not_re_resolve() {
        // A steady title must not re-resolve every poll tick (no command churn).
        let (tx, mut rx) = mpsc::channel(8);
        let title = RwLock::new(Some("main.rs".to_string()));

        let emitted = emit_title_resolve("Code", Some("main.rs".into()), &title, &tx).await;

        assert!(!emitted, "unchanged title is a no-op");
        assert!(
            rx.try_recv().is_err(),
            "no command emitted for a steady title"
        );
    }

    #[tokio::test]
    async fn title_cleared_emits_resolve_with_none() {
        // A window losing its title (Some → None) re-resolves so title rules
        // stop matching and app-name rules take over.
        let (tx, mut rx) = mpsc::channel(8);
        let title = RwLock::new(Some("was-titled".to_string()));

        let emitted = emit_title_resolve("Code", None, &title, &tx).await;

        assert!(emitted);
        assert_eq!(*title.read().await, None);
        match rx.try_recv().expect("command") {
            DaemonCommand::ResolveContextMode { window_title, .. } => {
                assert_eq!(window_title, None);
            }
            other => panic!("expected ResolveContextMode, got {other:?}"),
        }
    }
}
