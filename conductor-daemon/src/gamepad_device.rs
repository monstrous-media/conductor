// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! HID Device Management (Game Controllers)
//!
//! This module manages the lifecycle of HID input device connections (gamepads, joysticks,
//! racing wheels, flight sticks, HOTAS controllers) with automatic reconnection support
//! and robust error handling.
//!
//! # Features
//!
//! - **Automatic Reconnection**: Detects disconnections and attempts reconnection
//! - **Thread Safety**: Arc/Mutex patterns for safe concurrent access
//! - **Event Polling**: Continuous polling loop (1ms intervals)
//! - **Device Discovery**: List all connected game controllers
//! - **Connection Lifecycle**: Clean connection/disconnection with proper cleanup
//! - **Multi-Device Support**: Gamepads, joysticks, racing wheels, flight sticks, HOTAS
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────┐
//! │  HidDeviceManager                       │
//! │  ┌───────────────────────────────────┐ │
//! │  │  Connection State                 │ │
//! │  │  - gamepad_id                     │ │
//! │  │  - gamepad_name                   │ │
//! │  │  - is_connected                   │ │
//! │  └───────────────────────────────────┘ │
//! │  ┌───────────────────────────────────┐ │
//! │  │  gilrs::Gilrs                     │ │
//! │  │  - Event polling thread           │ │
//! │  │  - Convert to InputEvent          │ │
//! │  │  - Send via mpsc channel          │ │
//! │  └───────────────────────────────────┘ │
//! │  ┌───────────────────────────────────┐ │
//! │  │  Reconnection Logic               │ │
//! │  │  - Detect disconnects             │ │
//! │  │  - Background polling             │ │
//! │  │  - Send DaemonCommand on result   │ │
//! │  └───────────────────────────────────┘ │
//! └─────────────────────────────────────────┘
//! ```
//!
//! # Example
//!
//! ```no_run
//! use conductor_daemon::gamepad_device::HidDeviceManager;
//! use tokio::sync::mpsc;
//! use conductor_core::events::InputEvent;
//! use conductor_daemon::DaemonCommand;
//!
//! # async fn example() -> Result<(), String> {
//! let (event_tx, mut event_rx) = mpsc::channel::<InputEvent>(1024);
//! let (command_tx, mut command_rx) = mpsc::channel::<DaemonCommand>(32);
//!
//! let mut manager = HidDeviceManager::new(true); // auto_reconnect
//!
//! // Connect to first available game controller
//! let (gamepad_id, gamepad_name) = manager.connect(
//!     event_tx.clone(),
//!     command_tx.clone()
//! )?;
//!
//! println!("Connected to game controller: {} (ID {})", gamepad_name, gamepad_id);
//!
//! // Process events
//! while let Some(event) = event_rx.recv().await {
//!     println!("Received: {:?}", event);
//! }
//! # Ok(())
//! # }
//! ```

use conductor_core::events::{InputEvent, ProtocolEvent};
use conductor_core::gamepad_events::{
    AxisZeroFilter, DEFAULT_STICK_DEADZONE, DEFAULT_TRIGGER_DEADZONE, TriggerNoiseGate,
    axis_changed_to_input_with_deadzone, button_pressed_to_input, button_released_to_input,
    trigger_button_changed_to_input,
};
use conductor_core::identity::{DeviceEvent, DeviceId};
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{debug, error, info, trace, warn};

use crate::daemon::DaemonCommand;
use crate::input_source::{
    InputSource, InputSourceMetrics, InputSourceMetricsHandle, spawn_input_pump,
};
use conductor_core::config::types::ConnectorProtocol;

/// ADR-029 §D3 — surface a permission-grant breadcrumb when gilrs
/// initialisation fails on macOS due to TCC denying Input Monitoring.
///
/// Without this hint, the daemon log just says "Failed to initialize
/// gilrs: ..." and the user has no way to map that back to a fix.
/// On macOS, an `IOHIDManagerCreate` failure typically means TCC
/// hasn't been granted; we tell the user how to check and grant.
fn init_gilrs_error(e: gilrs::Error) -> String {
    let detail = format!("{}", e);
    #[cfg(target_os = "macos")]
    if detail.contains("IOHIDManager") {
        error!(
            "Gamepad subsystem init failed — likely missing macOS \
             Input Monitoring permission. Run `conductorctl permissions \
             --check` for diagnostics, or grant via System Settings → \
             Privacy & Security → Input Monitoring."
        );
    }
    format!("Failed to initialize gilrs: {}", detail)
}

/// ADR-047 §D1: maximum size of the user SDL override file. A larger file is
/// skipped with a warning (never loaded) so gamepad init can't be wedged by a
/// huge/garbage file.
const USER_GAMECONTROLLERDB_MAX_BYTES: u64 = 2 * 1024 * 1024; // 2 MiB

/// ADR-047 §D1: process-wide `--ignore-user-mappings` toggle. Set once by the
/// daemon's `main()` before the gamepad subsystem starts; read when building
/// `Gilrs`. Defaults to `false` (load the user override file if present).
static IGNORE_USER_MAPPINGS: AtomicBool = AtomicBool::new(false);

/// Set the process-wide `--ignore-user-mappings` toggle (ADR-047 §D1).
///
/// Call once at daemon startup, before any gamepad enumeration/connection.
pub fn set_ignore_user_mappings(ignore: bool) {
    IGNORE_USER_MAPPINGS.store(ignore, Ordering::Relaxed);
}

/// Path to the user SDL mapping override file, `~/.conductor/gamecontrollerdb.txt`
/// (ADR-047 §D1). Matches the `~/.conductor` convention used for plugins and
/// security data.
fn user_gamecontrollerdb_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".conductor")
        .join("gamecontrollerdb.txt")
}

/// Load the user SDL mapping override file for `add_mappings`, or `None` when it
/// should not be applied (ADR-047 §D1).
///
/// Returns `None` when `ignore` is set (`--ignore-user-mappings`), the file is
/// absent, exceeds [`USER_GAMECONTROLLERDB_MAX_BYTES`], is unreadable, or has no
/// usable mapping line. gilrs silently skips malformed / foreign-platform lines,
/// so this also scans the file to log each loaded GUID and warn on malformed
/// lines. Never panics on a bad or oversized file.
fn load_user_mappings(path: &Path, ignore: bool) -> Option<String> {
    if ignore {
        info!(
            "ADR-047 §D1: ignoring user gamepad mappings ({}) — --ignore-user-mappings set",
            path.display()
        );
        return None;
    }
    // Bounded read: cap the allocation at the limit + 1 byte so a file that
    // grows between a stat and the read (TOCTOU) still can't blow memory
    // (Copilot review). Absent file is the normal case — silent.
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return None,
    };
    let mut content = String::new();
    if let Err(e) = file
        .take(USER_GAMECONTROLLERDB_MAX_BYTES + 1)
        .read_to_string(&mut content)
    {
        warn!(
            "Failed to read user gamepad mappings {}: {} — skipping",
            path.display(),
            e
        );
        return None;
    }
    if content.len() as u64 > USER_GAMECONTROLLERDB_MAX_BYTES {
        warn!(
            "User gamepad mapping file {} exceeds the {} byte cap — skipping it \
             (ADR-047 §D1); trim the file to load it",
            path.display(),
            USER_GAMECONTROLLERDB_MAX_BYTES
        );
        return None;
    }
    // gilrs's `add_mappings` silently drops malformed lines (no error/log — see
    // spike #2424), so scan ourselves for user-facing diagnostics.
    let (mut loaded, mut skipped) = (0usize, 0usize);
    for (i, raw) in content.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let guid = line.split(',').next().unwrap_or("");
        if guid.len() == 32 && guid.bytes().all(|b| b.is_ascii_hexdigit()) {
            loaded += 1;
            debug!(
                "ADR-047 §D1: loaded user gamepad mapping for GUID {} ({}:{})",
                guid,
                path.display(),
                i + 1
            );
        } else {
            skipped += 1;
            warn!(
                "Skipping malformed user gamepad mapping at {}:{} (no 32-hex GUID prefix) — ADR-047 §D1",
                path.display(),
                i + 1
            );
        }
    }
    if loaded == 0 {
        if skipped > 0 {
            warn!(
                "No usable mappings in {} ({} malformed line(s)) — not applying",
                path.display(),
                skipped
            );
        }
        return None;
    }
    info!(
        "ADR-047 §D1: applying {} user gamepad mapping(s) from {} ({} malformed line(s) skipped). \
         Restart the daemon after editing this file to apply changes (mappings are construction-time only).",
        loaded,
        path.display(),
        skipped
    );
    Some(content)
}

/// Build `Gilrs` with conductor's SDL mapping layering (ADR-047 §D1 / spike #2424).
///
/// Precedence, lowest → highest: **user override file < gilrs bundled SDL DB <
/// `SDL_GAMECONTROLLERCONFIG`**. We keep gilrs's bundled DB *and* its native env
/// handling, so Steam's `SDL_GAMECONTROLLERCONFIG` is inserted last in `build()`
/// and wins (Steam-safe). The user file is added via `add_mappings`, which gilrs
/// inserts *before* the bundled DB — making it an **additive** layer: it maps
/// controllers gilrs does not recognize, but does NOT override a controller
/// already in the bundled DB (use `SDL_GAMECONTROLLERCONFIG` for that). gilrs's
/// mapping DB is construction-time only, so changes require a daemon restart.
///
/// Public so tools like `gamepad_diagnostic` initialise gilrs through the *same*
/// mapping layer as the daemon (otherwise the diagnostic's button ids wouldn't
/// reflect a user override file — Copilot review, PR #2440).
pub fn build_gilrs() -> Result<gilrs::Gilrs, String> {
    let ignore = IGNORE_USER_MAPPINGS.load(Ordering::Relaxed);
    // Defaults: included (bundled) + env mappings both ON.
    let mut builder = gilrs::GilrsBuilder::new();
    if let Some(user) = load_user_mappings(&user_gamecontrollerdb_path(), ignore) {
        builder = builder.add_mappings(&user);
    }
    builder.build().map_err(init_gilrs_error)
}

/// Polling interval for gamepad events (1ms)
const POLL_INTERVAL_MS: u64 = 1;

/// ADR-039-B #2229: how long `connect()` pumps gilrs events while waiting for
/// an already-connected controller to register before giving up.
///
/// macOS registers Bluetooth-LE HID gamepads (e.g. the Xbox Wireless
/// Controller) with gilrs's IOKit backend via an **asynchronous** run-loop
/// match callback that can fire a beat *after* `Gilrs::new()` returns. A single
/// immediate `gilrs.gamepads()` check therefore reports "No game controllers
/// connected" even though the controller is paired and HID-visible. Pumping
/// `next_event()` for this window lets that callback land so the controller is
/// detected. Only the *worst case* (no controller present at all) waits the
/// full window; a present controller returns as soon as it registers.
const GAMEPAD_DISCOVERY_WINDOW_MS: u64 = 2000;

/// Poll cadence while waiting inside [`GAMEPAD_DISCOVERY_WINDOW_MS`].
const GAMEPAD_DISCOVERY_POLL_MS: u64 = 50;

/// Shorter discovery window for the *enumeration* path ([`HidDeviceManager::list_gamepads`]),
/// vs the 2 s *connection* window above.
///
/// `list_gamepads` is called by `conductorctl list-devices`, the MCP/LLM device
/// tools, and `reconnect_loop` — places where a quick answer matters and the
/// controller, if present, is almost always already registered. Measured BLE
/// registration is ~50 ms, so 500 ms keeps a 10× margin for detection while
/// bounding the no-controller case (incl. CI's `test_list_gamepads`) to 500 ms
/// instead of 2 s (Copilot review, PR #2289). The connection path keeps the
/// full 2 s window where reliably catching a just-connected pad matters most.
const GAMEPAD_ENUM_WINDOW_MS: u64 = 500;

/// Reconnection backoff schedule (in seconds)
///
/// Exponential backoff: 1s, 2s, 4s, 8s, 16s, 30s
const RECONNECT_BACKOFF: &[u64] = &[1, 2, 4, 8, 16, 30];

/// Maximum number of reconnection attempts
const MAX_RECONNECT_ATTEMPTS: usize = 6;

/// HID device manager with automatic reconnection
///
/// Manages the connection to HID input devices (gamepads, joysticks, racing wheels,
/// flight sticks, HOTAS controllers) with automatic reconnection support, continuous
/// event polling, and thread-safe access patterns.
///
/// # Thread Safety
///
/// The manager uses `Arc<Mutex<>>` and `Arc<AtomicBool>` for internal state and
/// is safe to share across threads. The polling loop runs in a separate background thread.
///
/// # Connection Lifecycle
///
/// 1. **Connect**: Find first available game controller and start polling thread
/// 2. **Active**: Poll events at 1ms intervals, convert to InputEvent, send to channel
/// 3. **Disconnect**: Detect disconnection, stop polling, optionally trigger reconnection
/// 4. **Reconnect**: Background thread with exponential backoff
///
/// # Example
///
/// ```no_run
/// use conductor_daemon::gamepad_device::HidDeviceManager;
/// use tokio::sync::mpsc;
///
/// # async fn example() -> Result<(), String> {
/// let (event_tx, _) = mpsc::channel(1024);
/// let (command_tx, _) = mpsc::channel(32);
///
/// let mut manager = HidDeviceManager::new(true);
/// manager.connect(event_tx, command_tx)?;
///
/// assert!(manager.is_connected());
/// # Ok(())
/// # }
/// ```
pub struct HidDeviceManager {
    /// Whether to automatically reconnect on disconnect
    auto_reconnect: bool,
    /// Dead zone for analog sticks (0.0-1.0)
    stick_deadzone: f32,
    /// Dead zone for analog triggers (0.0-1.0)
    trigger_deadzone: f32,

    /// ADR-047 §D2: SDL GUIDs of controllers a configured `[[endpoints]]`
    /// `ControllerGuid` matcher prefers. When non-empty, `connect()` selects a
    /// connected controller whose GUID is listed over first-available; empty =
    /// historical first-available behaviour. Set by the daemon from the resolved
    /// HID input endpoint before connecting.
    preferred_guids: Vec<[u8; 16]>,

    /// Currently connected gamepad ID
    gamepad_id: Arc<Mutex<Option<gilrs::GamepadId>>>,

    /// Currently connected gamepad name
    gamepad_name: Arc<Mutex<Option<String>>>,

    /// Whether currently connected
    is_connected: Arc<AtomicBool>,

    /// Flag to signal polling thread to stop
    stop_polling: Arc<AtomicBool>,

    /// Handle to polling thread (if active)
    polling_thread: Arc<Mutex<Option<thread::JoinHandle<()>>>>,
}

/// Type alias for backward compatibility (v3.0)
///
/// Use `HidDeviceManager` for new code. This alias ensures existing code
/// using `GamepadDeviceManager` continues to work without modification.
pub type GamepadDeviceManager = HidDeviceManager;

impl HidDeviceManager {
    /// Create a new HID device manager
    ///
    /// # Arguments
    ///
    /// * `auto_reconnect` - Whether to automatically reconnect on disconnect
    ///
    /// # Returns
    ///
    /// A new HidDeviceManager instance
    ///
    /// # Example
    ///
    /// ```
    /// use conductor_daemon::gamepad_device::HidDeviceManager;
    ///
    /// let manager = HidDeviceManager::new(true);
    /// assert!(!manager.is_connected());
    /// ```
    pub fn new(auto_reconnect: bool) -> Self {
        Self::with_deadzone(
            auto_reconnect,
            DEFAULT_STICK_DEADZONE,
            DEFAULT_TRIGGER_DEADZONE,
        )
    }

    /// Create a new HidDeviceManager with configurable dead zones
    pub fn with_deadzone(auto_reconnect: bool, stick_deadzone: f32, trigger_deadzone: f32) -> Self {
        Self {
            auto_reconnect,
            stick_deadzone,
            trigger_deadzone,
            preferred_guids: Vec::new(),
            gamepad_id: Arc::new(Mutex::new(None)),
            gamepad_name: Arc::new(Mutex::new(None)),
            is_connected: Arc::new(AtomicBool::new(false)),
            stop_polling: Arc::new(AtomicBool::new(false)),
            polling_thread: Arc::new(Mutex::new(None)),
        }
    }

    /// Set the preferred controller GUIDs for connect selection (ADR-047 §D2).
    ///
    /// When non-empty, [`connect`](Self::connect) prefers a connected controller
    /// whose SDL GUID is in this list over first-available. The daemon supplies
    /// these from a configured `[[endpoints]]` `ControllerGuid` matcher before
    /// connecting. Empty (the default) keeps the historical first-available
    /// behaviour.
    pub fn set_preferred_guids(&mut self, guids: Vec<[u8; 16]>) {
        self.preferred_guids = guids;
    }

    /// List all connected game controllers
    ///
    /// Returns a list of (ID, Name, UUID) tuples for all connected HID game controllers.
    ///
    /// # Returns
    ///
    /// Vector of (GamepadId, Name, UUID) for each connected game controller
    ///
    /// # Errors
    ///
    /// Returns error string if gilrs initialization fails
    ///
    /// # Example
    ///
    /// ```no_run
    /// use conductor_daemon::gamepad_device::HidDeviceManager;
    ///
    /// # fn example() -> Result<(), String> {
    /// let gamepads = HidDeviceManager::list_gamepads()?;
    /// for (id, name, uuid) in gamepads {
    ///     println!("Game controller: {} (ID: {:?}, UUID: {})", name, id, uuid);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn list_gamepads() -> Result<Vec<(gilrs::GamepadId, String, String)>, String> {
        let mut gilrs = build_gilrs()?;

        // ADR-039-B #2229: pump events briefly so an async-registering
        // Bluetooth-LE HID gamepad (Xbox Wireless Controller) appears in the
        // enumeration, instead of a single immediate check that races the macOS
        // IOKit match callback (same fix as `connect`). Without this, a
        // controller that is connected and streaming events still reports
        // "No HID devices found" from this one-shot enumeration path (used by
        // `conductorctl list-devices` and the MCP/LLM device tools). Uses the
        // shorter enumeration window so the no-controller case returns quickly
        // (Copilot review, PR #2289); the connection path keeps the full 2 s.
        let deadline = Instant::now() + Duration::from_millis(GAMEPAD_ENUM_WINDOW_MS);
        loop {
            while gilrs.next_event().is_some() {}
            if gilrs.gamepads().next().is_some() || Instant::now() >= deadline {
                break;
            }
            thread::sleep(Duration::from_millis(GAMEPAD_DISCOVERY_POLL_MS));
        }

        let mut gamepads = Vec::new();
        for (id, gamepad) in gilrs.gamepads() {
            let name = gamepad.name().to_string();
            let uuid = format!("{:?}", gamepad.uuid());
            gamepads.push((id, name, uuid));
        }

        Ok(gamepads)
    }

    /// Connect to the first available game controller
    ///
    /// Finds the first connected HID game controller and starts the polling loop.
    ///
    /// # Arguments
    ///
    /// * `event_tx` - Channel sender for InputEvent messages
    /// * `command_tx` - Channel sender for DaemonCommand messages (reconnection, etc.)
    ///
    /// # Returns
    ///
    /// Tuple of (GamepadId, Name) for the connected game controller
    ///
    /// # Errors
    ///
    /// Returns error string if:
    /// - gilrs initialization fails
    /// - No game controllers are connected
    /// - Already connected to a game controller
    ///
    /// # Example
    ///
    /// ```no_run
    /// use conductor_daemon::gamepad_device::HidDeviceManager;
    /// use tokio::sync::mpsc;
    ///
    /// # async fn example() -> Result<(), String> {
    /// let (event_tx, _) = mpsc::channel(1024);
    /// let (command_tx, _) = mpsc::channel(32);
    ///
    /// let mut manager = HidDeviceManager::new(true);
    /// let (id, name) = manager.connect(event_tx, command_tx)?;
    /// println!("Connected to: {}", name);
    /// # Ok(())
    /// # }
    /// ```
    pub fn connect(
        &mut self,
        event_tx: mpsc::Sender<InputEvent>,
        command_tx: mpsc::Sender<DaemonCommand>,
    ) -> Result<(gilrs::GamepadId, String), String> {
        if self.is_connected.load(Ordering::Relaxed) {
            return Err("Already connected to a game controller".to_string());
        }

        // Initialize gilrs
        let mut gilrs = build_gilrs()?;

        // Find first connected game controller. ADR-039-B #2229: pump events
        // for a short window so a controller that registers asynchronously
        // (Bluetooth-LE HID gamepads on macOS) is detected, instead of a single
        // immediate check that races the IOKit match callback.
        let (gamepad_id, gamepad_name) = Self::discover_gamepad(
            &mut gilrs,
            Duration::from_millis(GAMEPAD_DISCOVERY_WINDOW_MS),
            &self.preferred_guids,
        )
        .ok_or_else(|| "No game controllers connected".to_string())?;

        info!(
            "Connecting to game controller: {} (ID: {:?})",
            gamepad_name, gamepad_id
        );

        // Store connection info
        *self.gamepad_id.lock().unwrap() = Some(gamepad_id);
        *self.gamepad_name.lock().unwrap() = Some(gamepad_name.clone());
        self.is_connected.store(true, Ordering::Relaxed);
        self.stop_polling.store(false, Ordering::Relaxed);

        // Start polling thread
        let stop_polling = Arc::clone(&self.stop_polling);
        let is_connected = Arc::clone(&self.is_connected);
        let gamepad_id_clone = Arc::clone(&self.gamepad_id);
        let auto_reconnect = self.auto_reconnect;

        let stick_deadzone = self.stick_deadzone;
        let trigger_deadzone = self.trigger_deadzone;

        let polling_handle = thread::spawn(move || {
            Self::polling_loop(
                gilrs,
                gamepad_id,
                event_tx,
                command_tx,
                stop_polling,
                is_connected,
                gamepad_id_clone,
                auto_reconnect,
                stick_deadzone,
                trigger_deadzone,
            );
        });

        *self.polling_thread.lock().unwrap() = Some(polling_handle);

        info!("Game controller polling thread started");

        Ok((gamepad_id, gamepad_name))
    }

    /// Discover a controller to connect to, preferring one whose SDL GUID is in
    /// `preferred_guids` (ADR-047 §D2), else first-available.
    ///
    /// Pumps gilrs events across `window` (draining `next_event()` to advance
    /// the macOS IOKit run-loop) so an async-arriving controller (Bluetooth-LE
    /// HID on macOS, #2229) registers. Returns immediately on the first scan
    /// when a controller is already present (e.g. a USB pad enumerated
    /// synchronously by `Gilrs::new()`).
    ///
    /// With a non-empty preference set: keeps pumping until a preferred GUID
    /// matches or the window expires, then falls back to first-available — a
    /// configured pad that pairs late still wins, but a non-matching pad is
    /// never rejected outright. With an empty preference set: returns as soon
    /// as any pad registers (historical first-available behaviour).
    fn discover_gamepad(
        gilrs: &mut gilrs::Gilrs,
        window: Duration,
        preferred_guids: &[[u8; 16]],
    ) -> Option<(gilrs::GamepadId, String)> {
        let start = Instant::now();
        let deadline = start + window;
        let mut attempts: u32 = 0;

        loop {
            // Drain pending events so an async-arriving `Connected` registers
            // the device in gilrs's gamepad list. (Pre-connection input events
            // drained here are intentionally discarded.)
            while gilrs.next_event().is_some() {}

            // Scan registered gamepads: a preferred-GUID match wins immediately;
            // otherwise remember the first as the fallback.
            let mut first: Option<(gilrs::GamepadId, String)> = None;
            for (id, gamepad) in gilrs.gamepads() {
                if !preferred_guids.is_empty() && preferred_guids.contains(&gamepad.uuid()) {
                    debug!(
                        elapsed_ms = start.elapsed().as_millis(),
                        "Selected gamepad by configured ControllerGuid (ADR-047 §D2)"
                    );
                    return Some((id, gamepad.name().to_string()));
                }
                if first.is_none() {
                    first = Some((id, gamepad.name().to_string()));
                }
            }

            if let Some(found) = first.clone() {
                if attempts > 0 {
                    debug!(
                        attempts,
                        elapsed_ms = start.elapsed().as_millis(),
                        "Gamepad registered after pumping gilrs events (async BLE-HID discovery, #2229)"
                    );
                }
                // No preference → take first immediately (historical behaviour).
                // Preference set but unmatched → keep waiting for a preferred pad
                // until the window expires, then accept this fallback.
                if preferred_guids.is_empty() || Instant::now() >= deadline {
                    if !preferred_guids.is_empty() {
                        debug!(
                            elapsed_ms = start.elapsed().as_millis(),
                            "Configured ControllerGuid not matched within discovery window; \
                             falling back to first-available controller (ADR-047 §D2)"
                        );
                    }
                    return Some(found);
                }
            }

            if Instant::now() >= deadline {
                // `first` may be Some here if the deadline crossed between the
                // two Instant::now() checks above (preferred_guids non-empty,
                // pad present but wrong GUID). Log the right message for each case.
                if first.is_some() {
                    debug!(
                        window_ms = window.as_millis(),
                        "Configured ControllerGuid not matched; using first-available fallback \
                         (ADR-047 §D2)"
                    );
                } else if attempts > 0 {
                    debug!(
                        attempts,
                        window_ms = window.as_millis(),
                        "No gamepad registered within discovery window (#2229)"
                    );
                }
                return first;
            }

            attempts += 1;
            thread::sleep(Duration::from_millis(GAMEPAD_DISCOVERY_POLL_MS));
        }
    }

    /// Log an unmapped gamepad button once per raw code (ADR-047 §D3a).
    ///
    /// Returns silently after the first sighting so a chattering control does
    /// not flood the log. ButtonPressed and ButtonReleased share key kind `0`,
    /// so each unmapped button is reported at most once per connection.
    fn log_unmapped_button(
        logged: &mut HashSet<(u8, gilrs::ev::Code)>,
        button: gilrs::Button,
        code: gilrs::ev::Code,
    ) {
        if logged.insert((0, code)) {
            warn!(
                "Dropping unmapped gamepad button {:?} (raw code {}) — no Conductor id \
                 (ADR-047 §D3a); not emitted as legacy id 255. Logged once per control.",
                button, code
            );
        }
    }

    /// Log an unmapped gamepad axis once per raw code (ADR-047 §D3a).
    fn log_unmapped_axis(
        logged: &mut HashSet<(u8, gilrs::ev::Code)>,
        axis: gilrs::Axis,
        code: gilrs::ev::Code,
    ) {
        if logged.insert((1, code)) {
            warn!(
                "Dropping unmapped gamepad axis {:?} (raw code {}) — no Conductor encoder id \
                 (ADR-047 §D3a); not emitted as legacy id 255. Logged once per control.",
                axis, code
            );
        }
    }

    /// Polling loop for HID game controller events (runs in background thread)
    ///
    /// Continuously polls gilrs for events, converts them to InputEvent,
    /// and sends them to the event channel. Handles disconnection and
    /// triggers reconnection if enabled.
    #[allow(clippy::too_many_arguments)]
    fn polling_loop(
        mut gilrs: gilrs::Gilrs,
        initial_gamepad_id: gilrs::GamepadId,
        event_tx: mpsc::Sender<InputEvent>,
        command_tx: mpsc::Sender<DaemonCommand>,
        stop_polling: Arc<AtomicBool>,
        is_connected: Arc<AtomicBool>,
        gamepad_id: Arc<Mutex<Option<gilrs::GamepadId>>>,
        auto_reconnect: bool,
        stick_deadzone: f32,
        trigger_deadzone: f32,
    ) {
        let current_gamepad_id = initial_gamepad_id;
        let mut trigger_gate = TriggerNoiseGate::new();
        let mut axis_filter = AxisZeroFilter::new();
        // ADR-047 §D3a: gilrs controls with no Conductor id are dropped (never
        // emitted as the old id-255 collision sentinel). An unmapped axis can
        // chatter every poll, so log each once per (control_kind, raw_code) for
        // the lifetime of this connection. `current_gamepad_id` is fixed per
        // polling loop, so the device dimension is implicit. kind: 0 = button
        // (pressed/released share a key), 1 = axis.
        let mut unmapped_logged: HashSet<(u8, gilrs::ev::Code)> = HashSet::new();

        loop {
            // Check stop signal
            if stop_polling.load(Ordering::Relaxed) {
                debug!("HID device polling loop stopped");
                break;
            }

            // Poll for events
            while let Some(event) = gilrs.next_event() {
                trace!("HID device event: {:?}", event);

                // Check if this event is from our connected game controller
                if event.id != current_gamepad_id {
                    continue;
                }

                match event.event {
                    gilrs::EventType::ButtonPressed(button, code) => {
                        match button_pressed_to_input(button) {
                            Some(input_event) => {
                                if let Err(e) = event_tx.try_send(input_event) {
                                    warn!("Failed to send button press event: {}", e);
                                }
                            }
                            None => Self::log_unmapped_button(&mut unmapped_logged, button, code),
                        }
                    }
                    gilrs::EventType::ButtonReleased(button, code) => {
                        match button_released_to_input(button) {
                            Some(input_event) => {
                                if let Err(e) = event_tx.try_send(input_event) {
                                    warn!("Failed to send button release event: {}", e);
                                }
                            }
                            None => Self::log_unmapped_button(&mut unmapped_logged, button, code),
                        }
                    }
                    gilrs::EventType::AxisChanged(axis, value, code) => {
                        // #599: suppress spurious 0.0 readings from duplicate
                        // HID elements (macOS) — see AxisZeroFilter (inline
                        // keep/hold, no buffering of non-zero signal). Held
                        // zeros that turn out to be genuine recenters are
                        // released by the due_recenters() flush below.
                        if !axis_filter.should_keep(axis, value, Instant::now()) {
                            continue;
                        }
                        match axis_changed_to_input_with_deadzone(
                            axis,
                            value,
                            stick_deadzone,
                            trigger_deadzone,
                        ) {
                            Some(input_event) => {
                                if let Err(e) = event_tx.try_send(input_event) {
                                    warn!("Failed to send axis change event: {}", e);
                                }
                            }
                            // ADR-047 §D3a: unmapped axis (e.g. d-pad-as-axis) —
                            // dropped, logged once per raw code, never id 255.
                            None => Self::log_unmapped_axis(&mut unmapped_logged, axis, code),
                        }
                    }
                    // #599: on macOS (and some other backends) gilrs delivers
                    // analog trigger travel as ButtonChanged(LeftTrigger2 /
                    // RightTrigger2, value), not AxisChanged(LeftZ / RightZ).
                    // Convert to the analog trigger encoder stream (132-133);
                    // non-trigger buttons return None and stay digital-only.
                    // Idle triggers chatter below the deadzone — the noise
                    // gate suppresses repeated quantised zeros.
                    gilrs::EventType::ButtonChanged(button, value, _code) => {
                        if let Some(input_event) =
                            trigger_button_changed_to_input(button, value, trigger_deadzone)
                        {
                            let suppressed = matches!(
                                &input_event,
                                InputEvent::EncoderTurned { encoder, value, .. }
                                    if !trigger_gate.should_emit(*encoder, *value)
                            );
                            if !suppressed && let Err(e) = event_tx.try_send(input_event) {
                                warn!("Failed to send trigger change event: {}", e);
                            }
                        }
                    }
                    gilrs::EventType::Disconnected => {
                        warn!("Game controller disconnected: {:?}", current_gamepad_id);
                        is_connected.store(false, Ordering::Relaxed);
                        *gamepad_id.lock().unwrap() = None;

                        if auto_reconnect {
                            info!("Triggering reconnection...");
                            // Spawn reconnection thread
                            let command_tx_clone = command_tx.clone();
                            thread::spawn(move || {
                                Self::reconnect_loop(command_tx_clone);
                            });
                        }

                        // Exit polling loop
                        return;
                    }
                    _ => {
                        // Ignore other events (Connected, etc.)
                    }
                }
            }

            // #599: release any held zero whose suppression window expired
            // with no newer non-zero reading — it was a genuine recenter on
            // hardware that recenters faster than the window, not a
            // duplicate-element twin (cloud-review, PR #2337). Worst-case
            // recenter delay is the 4ms window.
            for axis in axis_filter.due_recenters(Instant::now()) {
                // Recenter is a derived event for an axis already seen above; an
                // unmapped axis maps to None here and is silently skipped (the
                // first sighting already logged it via the AxisChanged arm).
                if let Some(input_event) =
                    axis_changed_to_input_with_deadzone(axis, 0.0, stick_deadzone, trigger_deadzone)
                    && let Err(e) = event_tx.try_send(input_event)
                {
                    warn!("Failed to send recenter event: {}", e);
                }
            }

            // Sleep to avoid busy-waiting
            thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
        }
    }

    /// Reconnection loop with exponential backoff
    ///
    /// Attempts to reconnect to a game controller with exponential backoff.
    /// Sends a DaemonCommand::ReconnectGamepad when a device becomes available.
    fn reconnect_loop(command_tx: mpsc::Sender<DaemonCommand>) {
        for (attempt, &delay) in RECONNECT_BACKOFF.iter().enumerate() {
            info!("Reconnection attempt {} in {}s...", attempt + 1, delay);
            thread::sleep(Duration::from_secs(delay));

            // Try to list game controllers
            match Self::list_gamepads() {
                Ok(gamepads) if !gamepads.is_empty() => {
                    info!("Game controller detected, sending reconnect command");
                    let (id, name, _uuid) = &gamepads[0];
                    debug!("Found game controller: {} (ID: {:?})", name, id);

                    // Send reconnect command
                    if let Err(e) = command_tx.try_send(DaemonCommand::ReconnectGamepad) {
                        error!("Failed to send reconnect command: {}", e);
                    }
                    return;
                }
                Ok(_) => {
                    debug!("No game controllers found, continuing backoff...");
                }
                Err(e) => {
                    error!("Error checking for game controllers: {}", e);
                }
            }
        }

        error!(
            "Failed to reconnect after {} attempts",
            MAX_RECONNECT_ATTEMPTS
        );
    }

    /// Disconnect from the current game controller
    ///
    /// Stops the polling thread and cleans up the connection.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use conductor_daemon::gamepad_device::HidDeviceManager;
    /// use tokio::sync::mpsc;
    ///
    /// # async fn example() -> Result<(), String> {
    /// let (event_tx, _) = mpsc::channel(1024);
    /// let (command_tx, _) = mpsc::channel(32);
    ///
    /// let mut manager = HidDeviceManager::new(true);
    /// manager.connect(event_tx, command_tx)?;
    /// manager.disconnect();
    ///
    /// assert!(!manager.is_connected());
    /// # Ok(())
    /// # }
    /// ```
    pub fn disconnect(&mut self) {
        if !self.is_connected.load(Ordering::Relaxed) {
            return;
        }

        info!("Disconnecting from game controller");

        // Signal polling thread to stop
        self.stop_polling.store(true, Ordering::Relaxed);

        // Wait for polling thread to finish
        if let Some(handle) = self.polling_thread.lock().unwrap().take() {
            let _ = handle.join();
        }

        // Clear connection state
        *self.gamepad_id.lock().unwrap() = None;
        *self.gamepad_name.lock().unwrap() = None;
        self.is_connected.store(false, Ordering::Relaxed);

        info!("Disconnected from game controller");
    }

    /// Check if currently connected to a game controller
    ///
    /// # Returns
    ///
    /// true if connected, false otherwise
    ///
    /// # Example
    ///
    /// ```
    /// use conductor_daemon::gamepad_device::HidDeviceManager;
    ///
    /// let manager = HidDeviceManager::new(true);
    /// assert!(!manager.is_connected());
    /// ```
    pub fn is_connected(&self) -> bool {
        self.is_connected.load(Ordering::Relaxed)
    }

    /// Get the current game controller name
    ///
    /// # Returns
    ///
    /// Some(name) if connected, None otherwise
    ///
    /// # Example
    ///
    /// ```no_run
    /// use conductor_daemon::gamepad_device::HidDeviceManager;
    /// use tokio::sync::mpsc;
    ///
    /// # async fn example() -> Result<(), String> {
    /// let (event_tx, _) = mpsc::channel(1024);
    /// let (command_tx, _) = mpsc::channel(32);
    ///
    /// let mut manager = HidDeviceManager::new(true);
    /// manager.connect(event_tx, command_tx)?;
    ///
    /// if let Some(name) = manager.get_gamepad_name() {
    ///     println!("Connected to: {}", name);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_gamepad_name(&self) -> Option<String> {
        self.gamepad_name.lock().unwrap().clone()
    }

    /// Get the connected game controller info (if connected)
    ///
    /// Unlike `list_gamepads()` which returns all available devices,
    /// this returns only the gamepad that is actually connected and
    /// being polled by this manager.
    ///
    /// # Returns
    ///
    /// Some((gamepad_id_string, name)) if connected, None otherwise
    pub fn get_connected_gamepad_info(&self) -> Option<(String, String)> {
        if !self.is_connected.load(Ordering::Relaxed) {
            return None;
        }

        let id = self.gamepad_id.lock().unwrap();
        let name = self.gamepad_name.lock().unwrap();

        match (&*id, &*name) {
            (Some(gid), Some(gname)) => Some((format!("{:?}", gid), gname.clone())),
            _ => None,
        }
    }
}

impl Drop for HidDeviceManager {
    fn drop(&mut self) {
        self.disconnect();
    }
}

/// ADR-039 §4.1 (#1758) — the HID [`InputSource`].
///
/// Wraps a [`HidDeviceManager`] (which keeps its existing push-based gilrs
/// polling-thread delivery) and tags it with the protocol + endpoint alias the
/// unified pump needs, plus a live-ready [`InputSourceMetricsHandle`].
///
/// As with [`crate::midi_device::MidiInputSource`], #1758 defines and
/// conformance-tests this type; #1760 adds `start(tx)` and routes the polling
/// loop's sends through this source's
/// [`metrics_handle`](HidInputSource::metrics_handle).
///
/// The symbol path `conductor_daemon::gamepad_device::HidInputSource` is what
/// the ADR-039 lifecycle-coverage matrix (#1761) asserts resolves.
pub struct HidInputSource {
    /// Endpoint alias — the route `from` key this source serves.
    alias: String,
    /// Identity events from this source are tagged with on the pump. Defaults
    /// to `DeviceId::raw("gamepad")` — the id the live multi-device gamepad
    /// path already uses.
    device_id: DeviceId,
    /// The wrapped device manager (retains its existing delivery path).
    manager: HidDeviceManager,
    /// Live-ready observability counters (incremented by the #1760 push path).
    metrics: InputSourceMetricsHandle,
    /// Intermediate `InputEvent` receiver retained by [`connect`](Self::connect)
    /// so [`start`](InputSource::start) can pump it into the unified channel.
    /// `None` until connected (or after `start` consumes it).
    pending_rx: Option<mpsc::Receiver<InputEvent>>,
}

impl HidInputSource {
    /// Wrap a [`HidDeviceManager`] as a protocol-tagged input source.
    pub fn new(alias: impl Into<String>, manager: HidDeviceManager) -> Self {
        Self {
            alias: alias.into(),
            device_id: DeviceId::raw("gamepad"),
            manager,
            metrics: InputSourceMetricsHandle::new(),
            pending_rx: None,
        }
    }

    /// Override the [`DeviceId`] events from this source are tagged with.
    pub fn with_device_id(mut self, device_id: DeviceId) -> Self {
        self.device_id = device_id;
        self
    }

    /// Set the [`DeviceId`] events from this source are tagged with, in place.
    ///
    /// The live daemon path (ADR-039-B #1762 step 4c) constructs the source
    /// before the config's HID input endpoint alias is known, then sets the tag
    /// here once `resolve_hid_input_alias` has run — so a catch-all route
    /// `from = "<alias>"` matches the gamepad's events.
    pub fn set_device_id(&mut self, device_id: DeviceId) {
        self.device_id = device_id;
    }

    /// The [`DeviceId`] events from this source are tagged with.
    pub fn device_id(&self) -> &DeviceId {
        &self.device_id
    }

    /// Connect the wrapped manager to the first available gamepad, retaining the
    /// intermediate `InputEvent` receiver so [`start`](InputSource::start) can
    /// pump it into the unified channel (#1760). Wraps the manager's existing
    /// gilrs polling-thread delivery — no behaviour change to how events are
    /// produced.
    pub fn connect(
        &mut self,
        command_tx: mpsc::Sender<DaemonCommand>,
    ) -> Result<(gilrs::GamepadId, String), String> {
        let (raw_tx, raw_rx) = mpsc::channel::<InputEvent>(1024);
        let res = self.manager.connect(raw_tx, command_tx)?;
        self.pending_rx = Some(raw_rx);
        Ok(res)
    }

    /// Borrow the wrapped manager.
    pub fn manager(&self) -> &HidDeviceManager {
        &self.manager
    }

    /// Mutably borrow the wrapped manager (e.g. to `connect`).
    pub fn manager_mut(&mut self) -> &mut HidDeviceManager {
        &mut self.manager
    }

    /// Clone the metrics handle so the #1760 push path can increment it from
    /// inside the polling loop without locking the hot path.
    pub fn metrics_handle(&self) -> InputSourceMetricsHandle {
        self.metrics.clone()
    }

    /// Test seam: stage an intermediate channel without real hardware, so the
    /// `start(tx)` round-trip can be exercised by feeding the returned sender.
    #[cfg(test)]
    pub(crate) fn attach_test_channel(&mut self) -> mpsc::Sender<InputEvent> {
        let (tx, rx) = mpsc::channel::<InputEvent>(1024);
        self.pending_rx = Some(rx);
        tx
    }
}

impl InputSource for HidInputSource {
    fn protocol(&self) -> ConnectorProtocol {
        ConnectorProtocol::Hid
    }

    fn source_alias(&self) -> &str {
        &self.alias
    }

    fn shutdown(&mut self) {
        self.manager.disconnect();
    }

    fn metrics(&self) -> InputSourceMetrics {
        self.metrics.snapshot()
    }

    fn start(&mut self, tx: mpsc::Sender<DeviceEvent<ProtocolEvent>>) -> Result<(), String> {
        let rx = self.pending_rx.take().ok_or_else(|| {
            format!(
                "HidInputSource '{}' not connected; call connect before start",
                self.alias
            )
        })?;
        spawn_input_pump(self.device_id.clone(), rx, tx, self.metrics.clone());
        Ok(())
    }
}

#[cfg(test)]
mod input_source_tests {
    use super::*;

    fn unconnected_source(alias: &str) -> HidInputSource {
        // Constructing a manager does not initialise gilrs (hardware-free).
        HidInputSource::new(alias, HidDeviceManager::new(false))
    }

    #[test]
    fn protocol_is_hid() {
        assert_eq!(unconnected_source("pad").protocol(), ConnectorProtocol::Hid);
    }

    #[test]
    fn source_alias_is_the_endpoint_alias() {
        assert_eq!(unconnected_source("pad").source_alias(), "pad");
    }

    #[test]
    fn fresh_metrics_are_zeroed() {
        assert_eq!(
            unconnected_source("pad").metrics(),
            InputSourceMetrics::default()
        );
    }

    #[test]
    fn metrics_handle_is_shared_with_the_source() {
        let src = unconnected_source("pad");
        src.metrics_handle().record_dropped();
        assert_eq!(src.metrics().dropped, 1);
    }

    #[test]
    fn shutdown_is_idempotent_when_unconnected() {
        let mut src = unconnected_source("pad");
        src.shutdown();
        src.shutdown();
        assert!(!src.manager().is_connected());
    }

    #[tokio::test]
    async fn start_pumps_source_events_onto_the_unified_channel() {
        // ADR-039 #1760: start(tx) wires the gilrs polling stream to the pump via
        // the §4.3 shed-load policy. Exercised hardware-free through the test
        // channel seam (no real gilrs device).
        use std::time::Instant;

        let mut src = unconnected_source("pad");
        let raw_tx = src.attach_test_channel();
        let (pump_tx, mut pump_rx) = mpsc::channel::<DeviceEvent<ProtocolEvent>>(8);

        src.start(pump_tx).expect("start succeeds once connected");

        raw_tx
            .send(InputEvent::PadPressed {
                pad: 128,
                velocity: 127,
                channel: None,
                time: Instant::now(),
            })
            .await
            .unwrap();

        let got = pump_rx.recv().await.expect("event reaches the pump");
        assert_eq!(got.device_id(), &DeviceId::raw("gamepad"));
        assert!(matches!(got.event(), ProtocolEvent::Input(_)));
        assert_eq!(src.metrics().events_in, 1, "push path records the event");
    }

    #[tokio::test]
    async fn start_before_connect_is_an_error() {
        let mut src = unconnected_source("pad");
        let (pump_tx, _pump_rx) = mpsc::channel::<DeviceEvent<ProtocolEvent>>(8);
        assert!(
            src.start(pump_tx).is_err(),
            "start must refuse before connect stages a channel"
        );
    }

    #[tokio::test]
    async fn set_device_id_tags_pumped_events_with_the_endpoint_alias() {
        // ADR-039-B #1762 (step 4c): the live cutover sets the source's
        // DeviceId from the configured `[[endpoints]]` Input/Hid alias before
        // `start`, so a gamepad's events reach the route engine under that
        // alias (`route_destinations_ctx(device_id.as_str(), …)`) and a
        // catch-all route `from = "<alias>"` matches. Exercised hardware-free
        // through the test channel seam.
        use std::time::Instant;

        let mut src = unconnected_source("pad");
        src.set_device_id(DeviceId::raw("MyPad"));
        assert_eq!(src.device_id(), &DeviceId::raw("MyPad"));

        let raw_tx = src.attach_test_channel();
        let (pump_tx, mut pump_rx) = mpsc::channel::<DeviceEvent<ProtocolEvent>>(8);
        src.start(pump_tx).expect("start succeeds once connected");

        raw_tx
            .send(InputEvent::PadPressed {
                pad: 128,
                velocity: 127,
                channel: None,
                time: Instant::now(),
            })
            .await
            .unwrap();

        let got = pump_rx.recv().await.expect("event reaches the pump");
        assert_eq!(
            got.device_id(),
            &DeviceId::raw("MyPad"),
            "pumped event must carry the configured endpoint alias, not the default"
        );
    }
}

#[cfg(test)]
#[allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "test diagnostic output"
)]
mod tests {
    use super::*;

    #[test]
    fn test_new_manager() {
        let manager = GamepadDeviceManager::new(true);
        assert!(!manager.is_connected());
        assert!(manager.get_gamepad_name().is_none());
    }

    // ── ADR-047 §D1: user gamecontrollerdb.txt override loader ──

    /// Unique temp path per case to keep parallel tests from colliding.
    fn tmp_mapping_path(case: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("conductor_adr047_d1_{case}.txt"))
    }

    const VALID_MAPPING_LINE: &str =
        "03000000de280000ff11000001000000,Conductor Test Pad,a:b0,b:b1,platform:Mac OS X,";

    #[test]
    fn test_load_user_mappings_ignore_flag_skips_file() {
        let path = tmp_mapping_path("ignore");
        std::fs::write(&path, VALID_MAPPING_LINE).unwrap();
        // --ignore-user-mappings → file is not loaded even though it is valid.
        assert_eq!(load_user_mappings(&path, true), None);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_load_user_mappings_absent_is_none() {
        let path = tmp_mapping_path("absent_does_not_exist");
        let _ = std::fs::remove_file(&path);
        assert_eq!(load_user_mappings(&path, false), None);
    }

    #[test]
    fn test_load_user_mappings_valid_returns_content() {
        let path = tmp_mapping_path("valid");
        std::fs::write(&path, format!("# header comment\n{VALID_MAPPING_LINE}\n")).unwrap();
        let loaded = load_user_mappings(&path, false).expect("valid mapping should load");
        assert!(loaded.contains("03000000de280000ff11000001000000"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_load_user_mappings_oversized_skipped_no_panic() {
        let path = tmp_mapping_path("oversized");
        // Just over the 2 MiB cap — must be skipped (None), never read/loaded.
        let big = vec![b'#'; (USER_GAMECONTROLLERDB_MAX_BYTES as usize) + 16];
        std::fs::write(&path, &big).unwrap();
        assert_eq!(load_user_mappings(&path, false), None);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_load_user_mappings_all_garbage_is_none() {
        let path = tmp_mapping_path("garbage");
        // No 32-hex GUID prefix on any line → nothing usable, init must not panic.
        std::fs::write(&path, "not a mapping\nzzzz,also bad\n# comment\n").unwrap();
        assert_eq!(load_user_mappings(&path, false), None);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_list_gamepads() {
        // This may pass or fail depending on whether game controllers are connected
        let result = HidDeviceManager::list_gamepads();
        assert!(result.is_ok(), "list_gamepads should not error");

        if let Ok(gamepads) = result {
            println!("Found {} game controllers", gamepads.len());
            for (id, name, uuid) in gamepads {
                println!("  - {} (ID: {:?}, UUID: {})", name, id, uuid);
            }
        }
    }

    #[test]
    fn test_manager_drop() {
        let manager = GamepadDeviceManager::new(false);
        drop(manager); // Should not panic
    }

    #[test]
    fn test_get_connected_gamepad_info_returns_none_when_not_connected() {
        let manager = HidDeviceManager::new(false);
        assert!(manager.get_connected_gamepad_info().is_none());
    }
}
