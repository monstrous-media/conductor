// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! MIDI Device Management
//!
//! This module manages the lifecycle of MIDI device connections with automatic
//! reconnection support and robust error handling.
//!
//! # Features
//!
//! - **Automatic Reconnection**: Exponential backoff with configurable max attempts
//! - **Thread Safety**: Arc/Mutex patterns for safe concurrent access
//! - **Non-blocking Callbacks**: Uses try_send to prevent callback blocking
//! - **Device Discovery**: Find devices by name or use first available
//! - **Connection Lifecycle**: Clean connection/disconnection with proper cleanup
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────┐
//! │  MidiDeviceManager                      │
//! │  ┌───────────────────────────────────┐ │
//! │  │  Connection State                 │ │
//! │  │  - device_name                    │ │
//! │  │  - port_index                     │ │
//! │  │  - is_connected                   │ │
//! │  └───────────────────────────────────┘ │
//! │  ┌───────────────────────────────────┐ │
//! │  │  midir::MidiInputConnection       │ │
//! │  │  - Callback thread                │ │
//! │  │  - Parse with midi-msg            │ │
//! │  │  - Send MidiEvent via mpsc        │ │
//! │  └───────────────────────────────────┘ │
//! │  ┌───────────────────────────────────┐ │
//! │  │  Reconnection Logic               │ │
//! │  │  - Exponential backoff            │ │
//! │  │  - Background thread              │ │
//! │  │  - Send DaemonCommand on result   │ │
//! │  └───────────────────────────────────┘ │
//! └─────────────────────────────────────────┘
//! ```
//!
//! # Example
//!
//! ```no_run
//! use conductor_daemon::midi_device::MidiDeviceManager;
//! use tokio::sync::mpsc;
//! use conductor_core::event_processor::MidiEvent;
//! use conductor_daemon::DaemonCommand;
//!
//! # async fn example() -> Result<(), String> {
//! let (event_tx, mut event_rx) = mpsc::channel::<MidiEvent>(1024);
//! let (command_tx, mut command_rx) = mpsc::channel::<DaemonCommand>(32);
//!
//! let mut manager = MidiDeviceManager::new(
//!     "Maschine Mikro MK3".to_string(),
//!     true  // auto_reconnect
//! );
//!
//! // Connect to device
//! let (port_index, port_name) = manager.connect(
//!     event_tx.clone(),
//!     command_tx.clone()
//! )?;
//!
//! println!("Connected to device: {} (port {})", port_name, port_index);
//!
//! // Process events
//! while let Some(event) = event_rx.recv().await {
//!     println!("Received: {:?}", event);
//! }
//! # Ok(())
//! # }
//! ```

use conductor_core::config::types::ConnectorProtocol;
use conductor_core::device_intelligence::probe::{ProbeCoordinator, SysExObserver};
use conductor_core::event_processor::MidiEvent;
use conductor_core::events::ProtocolEvent;
use conductor_core::identity::{DeviceEvent, DeviceId};
use midir::{Ignore, MidiInput, MidiInputConnection, MidiInputPort};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info, trace, warn};

use crate::daemon::DaemonCommand;
use crate::input_source::{
    InputSource, InputSourceMetrics, InputSourceMetricsHandle, spawn_input_pump,
};

/// Cheap structural check for a SysEx frame (F0 ... F7). Used by the
/// midir callback to decide whether to offer the bytes to the probe
/// coordinator's side-observing hook (ADR-026 Phase 1.B). The full
/// Identity-Reply shape check lives in `probe::is_identity_reply_shape`;
/// we only need the `F0`/`F7` framing gate here so non-SysEx messages
/// don't pay any probe-hook cost.
fn is_sysex(message: &[u8]) -> bool {
    message.len() >= 2 && message[0] == 0xF0 && message.last() == Some(&0xF7)
}

/// Shared callback body — extracted so (a) the three midir-callback
/// sites in this module stay DRY and (b) the dispatch logic is
/// unit-testable without a real MIDI runtime. Side-observing per
/// ADR-026 D9: the probe hook runs before `from_midi_msg` but never
/// short-circuits the parse-and-forward path.
///
/// Takes `Option<&dyn SysExObserver>` rather than `&ProbeCoordinator`
/// so tests can substitute a counting observer and verify the
/// `is_sysex` gate directly, instead of relying on `observe_reply`'s
/// defensive internal re-check to mask a broken gate.
fn handle_midi_message(
    message: &[u8],
    port_name: &str,
    sysex_observer: Option<&dyn SysExObserver>,
    event_tx: &mpsc::Sender<MidiEvent>,
) {
    trace!(
        "MIDI callback received {} bytes: {:02X?}",
        message.len(),
        message
    );

    // ADR-026 Phase 1.B: side-observing SysEx hook. Runs before the
    // regular parse so a pending probe sees the reply first; returns
    // nothing (cannot suppress the event).
    if let Some(observer) = sysex_observer
        && is_sysex(message)
    {
        observer.observe_reply(port_name, message);
    }

    // Parse MIDI message using midi-msg library
    match MidiEvent::from_midi_msg(message) {
        Ok(event) => {
            // Non-blocking send to prevent callback blocking
            if let Err(e) = event_tx.try_send(event.clone()) {
                match e {
                    mpsc::error::TrySendError::Full(_) => {
                        warn!("Event channel full, dropping event: {:?}", event);
                    }
                    mpsc::error::TrySendError::Closed(_) => {
                        error!("Event channel closed, cannot send event");
                    }
                }
            } else {
                trace!("Successfully sent event: {:?}", event);
            }
        }
        Err(e) => {
            // Log parsing errors at debug level (many MIDI messages are unsupported)
            debug!(
                "Failed to parse MIDI message: {} (bytes: {:02X?})",
                e, message
            );
        }
    }
}

/// Reconnection backoff schedule (in seconds)
///
/// Exponential backoff: 1s, 2s, 4s, 8s, 16s, 30s
const RECONNECT_BACKOFF: &[u64] = &[1, 2, 4, 8, 16, 30];

/// Maximum number of reconnection attempts
const MAX_RECONNECT_ATTEMPTS: usize = 6;

/// MIDI device manager with automatic reconnection
///
/// Manages the connection to a MIDI input device with automatic reconnection
/// support, exponential backoff, and thread-safe access patterns.
///
/// # Thread Safety
///
/// The manager uses `Arc<Mutex<>>` for internal state and is safe to share
/// across threads. The MIDI callback runs in a separate thread managed by midir.
///
/// # Connection Lifecycle
///
/// 1. **Connect**: Find device by name (or first available) and establish connection
/// 2. **Active**: Process MIDI events via callback, send to mpsc channel
/// 3. **Disconnect**: Clean up connection, optionally trigger reconnection
/// 4. **Reconnect**: Background thread with exponential backoff
///
/// # Example
///
/// ```no_run
/// use conductor_daemon::midi_device::MidiDeviceManager;
/// use tokio::sync::mpsc;
///
/// # async fn example() -> Result<(), String> {
/// let (event_tx, _) = mpsc::channel(1024);
/// let (command_tx, _) = mpsc::channel(32);
///
/// let mut manager = MidiDeviceManager::new("My Device".to_string(), true);
/// manager.connect(event_tx, command_tx)?;
///
/// assert!(manager.is_connected());
/// # Ok(())
/// # }
/// ```
pub struct MidiDeviceManager {
    /// Name of the device to connect to (or empty for first available)
    device_name: String,

    /// Whether to automatically reconnect on disconnect
    auto_reconnect: bool,

    /// Active MIDI input connection (None if disconnected)
    connection: Arc<Mutex<Option<MidiInputConnection<()>>>>,

    /// Currently connected port index
    port_index: Arc<Mutex<Option<usize>>>,

    /// Currently connected port name
    port_name: Arc<Mutex<Option<String>>>,

    /// Whether currently connected
    is_connected: Arc<Mutex<bool>>,

    /// Device identity for multi-device tagging (ADR-009 Phase 2)
    device_id: Option<DeviceId>,

    /// SysEx probe coordinator for Universal Identity Request dispatch
    /// (ADR-026 Phase 1.B). `None` for tests / contexts that don't need
    /// probing. When set, the callback routes SysEx messages through
    /// `ProbeCoordinator::observe_reply` before normal MIDI dispatch.
    /// The hook is side-observing (ADR-026 D9) — it never suppresses
    /// the event.
    probe_coordinator: Option<Arc<ProbeCoordinator>>,
}

impl MidiDeviceManager {
    /// Create a new MIDI device manager
    ///
    /// # Arguments
    ///
    /// * `device_name` - Name of device to connect to (empty = first available)
    /// * `auto_reconnect` - Whether to automatically reconnect on disconnect
    ///
    /// # Example
    ///
    /// ```
    /// use conductor_daemon::midi_device::MidiDeviceManager;
    ///
    /// // Connect to specific device with auto-reconnect
    /// let manager = MidiDeviceManager::new("Maschine Mikro MK3".to_string(), true);
    ///
    /// // Connect to first available device without auto-reconnect
    /// let manager = MidiDeviceManager::new(String::new(), false);
    /// ```
    pub fn new(device_name: String, auto_reconnect: bool) -> Self {
        Self {
            device_name,
            auto_reconnect,
            connection: Arc::new(Mutex::new(None)),
            port_index: Arc::new(Mutex::new(None)),
            port_name: Arc::new(Mutex::new(None)),
            is_connected: Arc::new(Mutex::new(false)),
            device_id: None,
            probe_coordinator: None,
        }
    }

    /// Set the device identity for multi-device tagging (ADR-009 Phase 2)
    pub fn set_device_id(&mut self, id: DeviceId) {
        self.device_id = Some(id);
    }

    /// Inject the shared `ProbeCoordinator` for SysEx identity probing
    /// (ADR-026 Phase 1.B). Must be called before `connect()` — subsequent
    /// connects pick up the current value by cloning the Arc into the
    /// midir callback. Calling `set_probe_coordinator(...)` after connect
    /// has no effect on the already-running callback.
    pub fn set_probe_coordinator(&mut self, coordinator: Arc<ProbeCoordinator>) {
        self.probe_coordinator = Some(coordinator);
    }

    /// Get the device identity (ADR-009 Phase 2)
    pub fn device_id(&self) -> Option<&DeviceId> {
        self.device_id.as_ref()
    }

    /// Connect to MIDI device and start event processing
    ///
    /// Attempts to find the configured device by name, or uses the first available
    /// device if no name is set. Creates a midir callback that parses MIDI messages
    /// and sends them to the event channel.
    ///
    /// # Arguments
    ///
    /// * `event_tx` - Channel for sending parsed MIDI events
    /// * `command_tx` - Channel for sending reconnection commands
    ///
    /// # Returns
    ///
    /// * `Ok((port_index, port_name))` - Successfully connected
    /// * `Err(String)` - Connection failed with error message
    ///
    /// # Errors
    ///
    /// - No MIDI input ports available
    /// - Named device not found (falls back to first port with warning)
    /// - Failed to create MIDI input
    /// - Failed to establish connection
    ///
    /// # Example
    ///
    /// ```no_run
    /// use conductor_daemon::midi_device::MidiDeviceManager;
    /// use tokio::sync::mpsc;
    ///
    /// # async fn example() -> Result<(), String> {
    /// let (event_tx, _) = mpsc::channel(1024);
    /// let (command_tx, _) = mpsc::channel(32);
    ///
    /// let mut manager = MidiDeviceManager::new("My Device".to_string(), true);
    /// let (port_idx, port_name) = manager.connect(event_tx, command_tx)?;
    ///
    /// println!("Connected to port {}: {}", port_idx, port_name);
    /// # Ok(())
    /// # }
    /// ```
    pub fn connect(
        &mut self,
        event_tx: mpsc::Sender<MidiEvent>,
        _command_tx: mpsc::Sender<DaemonCommand>,
    ) -> Result<(usize, String), String> {
        info!(
            "Connecting to MIDI device: {}",
            if self.device_name.is_empty() {
                "[first available]"
            } else {
                &self.device_name
            }
        );

        // Create MIDI input. Suppress MIDI Timing Clock (F8) and Active
        // Sense (FE) — both high-volume system-realtime messages we
        // never dispatch. SysEx remains enabled (midir default) so the
        // probe-coordinator hook can observe Identity Replies.
        let mut midi_in = MidiInput::new("Conductor Daemon")
            .map_err(|e| format!("Failed to create MIDI input: {}", e))?;
        midi_in.ignore(Ignore::TimeAndActiveSense);

        // Get available ports
        let ports = midi_in.ports();
        if ports.is_empty() {
            return Err("No MIDI input ports available".to_string());
        }

        // Find target port
        let (port, port_index) = self.find_port(&midi_in, &ports)?;
        let port_name = midi_in
            .port_name(&port)
            .unwrap_or_else(|_| format!("Port {}", port_index));

        debug!("Opening MIDI port {} (index {})", port_name, port_index);

        // Clone inputs the midir callback needs. The callback thread
        // is spawned by midir and outlives this scope, so anything it
        // touches must be `Send + 'static`.
        let probe_coordinator = self.probe_coordinator.clone();
        let cb_port_name = port_name.clone();

        // Create callback that delegates to the shared dispatch helper.
        let callback = move |_timestamp: u64, message: &[u8], _: &mut ()| {
            handle_midi_message(
                message,
                &cb_port_name,
                probe_coordinator
                    .as_deref()
                    .map(|c| c as &dyn SysExObserver),
                &event_tx,
            );
        };

        // Establish connection
        let connection = midi_in
            .connect(&port, &format!("conductor-{}", port_index), callback, ())
            .map_err(|e| format!("Failed to connect to port: {}", e))?;

        // Update state
        *self.connection.lock().unwrap() = Some(connection);
        *self.port_index.lock().unwrap() = Some(port_index);
        *self.port_name.lock().unwrap() = Some(port_name.clone());
        *self.is_connected.lock().unwrap() = true;

        info!(
            "Successfully connected to MIDI device: {} (port {})",
            port_name, port_index
        );

        Ok((port_index, port_name))
    }

    /// Connect to a specific MIDI port by index
    ///
    /// Similar to `connect()` but bypasses device name search and connects
    /// directly to the specified port index.
    ///
    /// # Arguments
    ///
    /// * `port_index` - Explicit port index to connect to
    /// * `event_tx` - Channel for sending parsed MIDI events
    /// * `command_tx` - Channel for sending reconnection commands
    ///
    /// # Returns
    ///
    /// * `Ok((port_index, port_name))` - Successfully connected
    /// * `Err(String)` - Connection failed with error message
    ///
    /// # Errors
    ///
    /// - Port index out of range
    /// - Failed to create MIDI input
    /// - Failed to establish connection
    ///
    /// # Example
    ///
    /// ```no_run
    /// use conductor_daemon::midi_device::MidiDeviceManager;
    /// use tokio::sync::mpsc;
    ///
    /// # async fn example() -> Result<(), String> {
    /// let (event_tx, _) = mpsc::channel(1024);
    /// let (command_tx, _) = mpsc::channel(32);
    ///
    /// let mut manager = MidiDeviceManager::new(String::new(), true);
    /// let (port_idx, port_name) = manager.connect_to_port(2, event_tx, command_tx)?;
    ///
    /// println!("Connected to port {}: {}", port_idx, port_name);
    /// # Ok(())
    /// # }
    /// ```
    pub fn connect_to_port(
        &mut self,
        port_index: usize,
        event_tx: mpsc::Sender<MidiEvent>,
        _command_tx: mpsc::Sender<DaemonCommand>,
    ) -> Result<(usize, String), String> {
        info!("Connecting to MIDI port index {}", port_index);

        // Create MIDI input. See `connect()` for the ignore-flag rationale.
        let mut midi_in = MidiInput::new("Conductor Daemon")
            .map_err(|e| format!("Failed to create MIDI input: {}", e))?;
        midi_in.ignore(Ignore::TimeAndActiveSense);

        // Get available ports
        let ports = midi_in.ports();
        if ports.is_empty() {
            return Err("No MIDI input ports available".to_string());
        }

        // Validate port index
        if port_index >= ports.len() {
            return Err(format!(
                "Port index {} out of range (0-{})",
                port_index,
                ports.len() - 1
            ));
        }

        // Get target port
        let port = &ports[port_index];
        let port_name = midi_in
            .port_name(port)
            .unwrap_or_else(|_| format!("Port {}", port_index));

        debug!("Opening MIDI port {} (index {})", port_name, port_index);

        // Capture coordinator + port name into the midir callback (see
        // `connect()` for the side-observing SysEx hook rationale).
        let probe_coordinator = self.probe_coordinator.clone();
        let cb_port_name = port_name.clone();

        // Create callback that delegates to the shared dispatch helper.
        let callback = move |_timestamp: u64, message: &[u8], _: &mut ()| {
            handle_midi_message(
                message,
                &cb_port_name,
                probe_coordinator
                    .as_deref()
                    .map(|c| c as &dyn SysExObserver),
                &event_tx,
            );
        };

        // Establish connection
        let connection = midi_in
            .connect(port, &format!("conductor-{}", port_index), callback, ())
            .map_err(|e| format!("Failed to connect to port: {}", e))?;

        // Update state
        *self.connection.lock().unwrap() = Some(connection);
        *self.port_index.lock().unwrap() = Some(port_index);
        *self.port_name.lock().unwrap() = Some(port_name.clone());
        *self.is_connected.lock().unwrap() = true;

        info!(
            "Successfully connected to MIDI device: {} (port {})",
            port_name, port_index
        );

        Ok((port_index, port_name))
    }

    /// Find the target MIDI port by name or use first available
    ///
    /// # Arguments
    ///
    /// * `midi_in` - MIDI input instance for querying port names
    /// * `ports` - List of available MIDI ports
    ///
    /// # Returns
    ///
    /// * `Ok((port, index))` - Found port and its index
    /// * `Err(String)` - No ports available (should never happen, checked in connect)
    ///
    /// # Behavior
    ///
    /// - If device_name is empty: Use first port
    /// - If device_name is set: Search by exact match
    /// - If named device not found: Warn and fall back to first port
    fn find_port(
        &self,
        midi_in: &MidiInput,
        ports: &[MidiInputPort],
    ) -> Result<(MidiInputPort, usize), String> {
        if self.device_name.is_empty() {
            // Use first available port
            debug!("No device name specified, using first available port");
            return Ok((ports[0].clone(), 0));
        }

        // Search for named device
        for (i, port) in ports.iter().enumerate() {
            if let Ok(name) = midi_in.port_name(port)
                && name.contains(&self.device_name)
            {
                debug!("Found matching port: {} (index {})", name, i);
                return Ok((port.clone(), i));
            }
        }

        // Device not found, fall back to first port with warning
        warn!(
            "Device '{}' not found, falling back to first port",
            self.device_name
        );

        if let Ok(fallback_name) = midi_in.port_name(&ports[0]) {
            warn!("Using fallback device: {}", fallback_name);
        }

        Ok((ports[0].clone(), 0))
    }

    /// Disconnect from MIDI device
    ///
    /// Cleanly closes the MIDI connection and updates internal state.
    /// Does not trigger reconnection logic.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use conductor_daemon::midi_device::MidiDeviceManager;
    /// use tokio::sync::mpsc;
    ///
    /// # async fn example() -> Result<(), String> {
    /// let (event_tx, _) = mpsc::channel(1024);
    /// let (command_tx, _) = mpsc::channel(32);
    ///
    /// let mut manager = MidiDeviceManager::new("My Device".to_string(), false);
    /// manager.connect(event_tx, command_tx)?;
    ///
    /// // Later...
    /// manager.disconnect();
    /// assert!(!manager.is_connected());
    /// # Ok(())
    /// # }
    /// ```
    pub fn disconnect(&mut self) {
        info!("Disconnecting from MIDI device");

        // Close connection
        if let Some(conn) = self.connection.lock().unwrap().take() {
            // Connection is dropped here, which closes it
            drop(conn);
            debug!("MIDI connection closed");
        }

        // Clear state
        *self.port_index.lock().unwrap() = None;
        *self.port_name.lock().unwrap() = None;
        *self.is_connected.lock().unwrap() = false;

        info!("MIDI device disconnected");
    }

    /// Spawn background reconnection thread with exponential backoff
    ///
    /// Creates a background thread that attempts to reconnect to the MIDI device
    /// with exponential backoff. Sends `DaemonCommand::DeviceReconnected` on success
    /// or `DaemonCommand::DeviceReconnectionFailed` after max attempts.
    ///
    /// # Arguments
    ///
    /// * `event_tx` - Channel for sending MIDI events after reconnection
    /// * `command_tx` - Channel for sending reconnection status commands
    ///
    /// # Backoff Schedule
    ///
    /// Attempts reconnection with increasing delays:
    /// - Attempt 1: 1 second
    /// - Attempt 2: 2 seconds
    /// - Attempt 3: 4 seconds
    /// - Attempt 4: 8 seconds
    /// - Attempt 5: 16 seconds
    /// - Attempt 6: 30 seconds
    ///
    /// # Example
    ///
    /// ```no_run
    /// use conductor_daemon::midi_device::MidiDeviceManager;
    /// use tokio::sync::mpsc;
    ///
    /// # async fn example() -> Result<(), String> {
    /// let (event_tx, _) = mpsc::channel(1024);
    /// let (command_tx, mut command_rx) = mpsc::channel(32);
    ///
    /// let mut manager = MidiDeviceManager::new("My Device".to_string(), true);
    ///
    /// // Simulate disconnect, spawn reconnection
    /// manager.spawn_reconnection_thread(event_tx, command_tx);
    ///
    /// // Wait for reconnection result
    /// while let Some(cmd) = command_rx.recv().await {
    ///     match cmd {
    ///         conductor_daemon::DaemonCommand::DeviceReconnected => {
    ///             println!("Device reconnected!");
    ///             break;
    ///         }
    ///         conductor_daemon::DaemonCommand::DeviceReconnectionFailed => {
    ///             println!("Reconnection failed after max attempts");
    ///             break;
    ///         }
    ///         _ => {}
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn spawn_reconnection_thread(
        &self,
        event_tx: mpsc::Sender<MidiEvent>,
        command_tx: mpsc::Sender<DaemonCommand>,
    ) {
        if !self.auto_reconnect {
            debug!("Auto-reconnect disabled, not spawning reconnection thread");
            return;
        }

        info!("Spawning reconnection thread with exponential backoff");

        let device_name = self.device_name.clone();
        let connection = Arc::clone(&self.connection);
        let port_index = Arc::clone(&self.port_index);
        let port_name = Arc::clone(&self.port_name);
        let is_connected = Arc::clone(&self.is_connected);
        let probe_coordinator = self.probe_coordinator.clone();

        thread::spawn(move || {
            for (attempt, &delay_secs) in RECONNECT_BACKOFF.iter().enumerate() {
                let attempt_num = attempt + 1;
                info!(
                    "Reconnection attempt {}/{} (waiting {}s)...",
                    attempt_num, MAX_RECONNECT_ATTEMPTS, delay_secs
                );

                thread::sleep(Duration::from_secs(delay_secs));

                // Attempt reconnection
                match Self::try_reconnect(&device_name, event_tx.clone(), probe_coordinator.clone())
                {
                    Ok((new_port_idx, new_port_name, new_connection)) => {
                        // Update state
                        *connection.lock().unwrap() = Some(new_connection);
                        *port_index.lock().unwrap() = Some(new_port_idx);
                        *port_name.lock().unwrap() = Some(new_port_name.clone());
                        *is_connected.lock().unwrap() = true;

                        info!(
                            "Successfully reconnected to device: {} (port {})",
                            new_port_name, new_port_idx
                        );

                        // Notify daemon of successful reconnection
                        if let Err(e) = command_tx.blocking_send(DaemonCommand::DeviceReconnected) {
                            error!("Failed to send DeviceReconnected command: {}", e);
                        }
                        return;
                    }
                    Err(e) => {
                        warn!(
                            "Reconnection attempt {}/{} failed: {}",
                            attempt_num, MAX_RECONNECT_ATTEMPTS, e
                        );
                    }
                }
            }

            // All attempts exhausted
            error!(
                "Failed to reconnect after {} attempts",
                MAX_RECONNECT_ATTEMPTS
            );

            // Notify daemon of reconnection failure
            if let Err(e) = command_tx.blocking_send(DaemonCommand::DeviceReconnectionFailed) {
                error!("Failed to send DeviceReconnectionFailed command: {}", e);
            }
        });
    }

    /// Attempt a single reconnection to the MIDI device
    ///
    /// Internal helper for reconnection thread. Creates a new MIDI input,
    /// finds the target device, and establishes a connection.
    ///
    /// # Arguments
    ///
    /// * `device_name` - Name of device to connect to (empty = first available)
    /// * `event_tx` - Channel for sending MIDI events
    ///
    /// # Returns
    ///
    /// * `Ok((port_index, port_name, connection))` - Successful reconnection
    /// * `Err(String)` - Connection failed
    fn try_reconnect(
        device_name: &str,
        event_tx: mpsc::Sender<MidiEvent>,
        probe_coordinator: Option<Arc<ProbeCoordinator>>,
    ) -> Result<(usize, String, MidiInputConnection<()>), String> {
        debug!("Attempting to reconnect to MIDI device: {}", device_name);

        // Create MIDI input. See `connect()` for the ignore-flag rationale.
        let mut midi_in = MidiInput::new("Conductor Daemon Reconnect")
            .map_err(|e| format!("Failed to create MIDI input: {}", e))?;
        midi_in.ignore(Ignore::TimeAndActiveSense);

        // Get available ports
        let ports = midi_in.ports();
        if ports.is_empty() {
            return Err("No MIDI input ports available".to_string());
        }

        // Find target port
        let (port, port_index) = if device_name.is_empty() {
            (ports[0].clone(), 0)
        } else {
            // Search for named device
            let mut found = None;
            for (i, p) in ports.iter().enumerate() {
                if let Ok(name) = midi_in.port_name(p)
                    && name.contains(device_name)
                {
                    found = Some((p.clone(), i));
                    break;
                }
            }

            // Fall back to first port if not found
            found.unwrap_or_else(|| {
                debug!(
                    "Device '{}' not found during reconnect, using first port",
                    device_name
                );
                (ports[0].clone(), 0)
            })
        };

        let port_name = midi_in
            .port_name(&port)
            .unwrap_or_else(|_| format!("Port {}", port_index));

        // Capture coordinator + port name into the midir callback.
        let cb_port_name = port_name.clone();

        // Create callback that delegates to the shared dispatch helper.
        let callback = move |_timestamp: u64, message: &[u8], _: &mut ()| {
            handle_midi_message(
                message,
                &cb_port_name,
                probe_coordinator
                    .as_deref()
                    .map(|c| c as &dyn SysExObserver),
                &event_tx,
            );
        };

        // Establish connection
        let connection = midi_in
            .connect(&port, &format!("conductor-{}", port_index), callback, ())
            .map_err(|e| format!("Failed to connect to port: {}", e))?;

        Ok((port_index, port_name, connection))
    }

    /// Check if currently connected to a MIDI device
    ///
    /// # Returns
    ///
    /// `true` if connected, `false` otherwise
    ///
    /// # Example
    ///
    /// ```no_run
    /// use conductor_daemon::midi_device::MidiDeviceManager;
    ///
    /// let manager = MidiDeviceManager::new("My Device".to_string(), false);
    /// assert!(!manager.is_connected());
    /// ```
    pub fn is_connected(&self) -> bool {
        *self.is_connected.lock().unwrap()
    }

    /// Get current device information
    ///
    /// Returns the port index and name of the currently connected device,
    /// or `None` if not connected.
    ///
    /// # Returns
    ///
    /// * `Some((port_index, port_name))` - Currently connected device
    /// * `None` - Not connected
    ///
    /// # Example
    ///
    /// ```no_run
    /// use conductor_daemon::midi_device::MidiDeviceManager;
    /// use tokio::sync::mpsc;
    ///
    /// # async fn example() -> Result<(), String> {
    /// let (event_tx, _) = mpsc::channel(1024);
    /// let (command_tx, _) = mpsc::channel(32);
    ///
    /// let mut manager = MidiDeviceManager::new("My Device".to_string(), false);
    /// manager.connect(event_tx, command_tx)?;
    ///
    /// if let Some((idx, name)) = manager.device_info() {
    ///     println!("Connected to port {}: {}", idx, name);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn device_info(&self) -> Option<(usize, String)> {
        let port_index = self.port_index.lock().unwrap();
        let port_name = self.port_name.lock().unwrap();

        match (*port_index, port_name.as_ref()) {
            (Some(idx), Some(name)) => Some((idx, name.clone())),
            _ => None,
        }
    }

    /// Graceful shutdown
    ///
    /// Disconnects from the device and cleans up all resources.
    /// This is the recommended way to stop the device manager.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use conductor_daemon::midi_device::MidiDeviceManager;
    /// use tokio::sync::mpsc;
    ///
    /// # async fn example() -> Result<(), String> {
    /// let (event_tx, _) = mpsc::channel(1024);
    /// let (command_tx, _) = mpsc::channel(32);
    ///
    /// let mut manager = MidiDeviceManager::new("My Device".to_string(), false);
    /// manager.connect(event_tx, command_tx)?;
    ///
    /// // Later, during shutdown...
    /// manager.shutdown();
    /// # Ok(())
    /// # }
    /// ```
    pub fn shutdown(&mut self) {
        info!("Shutting down MIDI device manager");
        self.disconnect();
        info!("MIDI device manager shutdown complete");
    }
}

impl Drop for MidiDeviceManager {
    /// Ensure clean shutdown on drop
    ///
    /// If the manager is still connected when dropped, this will
    /// disconnect cleanly to prevent resource leaks.
    fn drop(&mut self) {
        if self.is_connected() {
            warn!("MidiDeviceManager dropped while still connected, forcing disconnect");
            self.disconnect();
        }
    }
}

/// ADR-039 §4.1 — the MIDI [`InputSource`].
///
/// Wraps a [`MidiDeviceManager`] (which keeps its existing push-based midir
/// callback delivery) and tags it with the protocol + endpoint alias the
/// unified pump needs, plus a live-ready [`InputSourceMetricsHandle`].
///
/// This type is defined and conformance-tested but is **not yet** the
/// thing the daemon wires into the live event path — the manager is still wired
/// directly (now over a `DeviceEvent<ProtocolEvent>` channel). `start(tx)` was
/// added to [`InputSource`] to route the manager's sends through this
/// source's [`metrics_handle`](MidiInputSource::metrics_handle).
///
/// The symbol path `conductor_daemon::midi_device::MidiInputSource` is what the
/// ADR-039 lifecycle-coverage matrix asserts resolves.
pub struct MidiInputSource {
    /// Endpoint alias — the route `from` key this source serves.
    alias: String,
    /// Identity events from this source are tagged with on the pump. Defaults
    /// to `DeviceId::raw(alias)`; the live multi-device path mints a more
    /// specific id and sets it via [`with_device_id`](Self::with_device_id).
    device_id: DeviceId,
    /// The wrapped device manager (retains its existing delivery path).
    manager: MidiDeviceManager,
    /// Live-ready observability counters (incremented by the push path).
    metrics: InputSourceMetricsHandle,
    /// Intermediate `MidiEvent` receiver retained by [`connect_to_port`](Self::connect_to_port)
    /// so [`start`](InputSource::start) can pump it into the unified channel.
    /// `None` until connected (or after `start` consumes it).
    pending_rx: Option<mpsc::Receiver<MidiEvent>>,
}

impl MidiInputSource {
    /// Wrap a [`MidiDeviceManager`] as a protocol-tagged input source.
    pub fn new(alias: impl Into<String>, manager: MidiDeviceManager) -> Self {
        let alias = alias.into();
        let device_id = DeviceId::raw(&alias);
        Self {
            alias,
            device_id,
            manager,
            metrics: InputSourceMetricsHandle::new(),
            pending_rx: None,
        }
    }

    /// Override the [`DeviceId`] events from this source are tagged with on the
    /// pump (the live multi-device path resolves a specific id per port).
    pub fn with_device_id(mut self, device_id: DeviceId) -> Self {
        self.device_id = device_id;
        self
    }

    /// Connect the wrapped manager to `port_index`, retaining the intermediate
    /// `MidiEvent` receiver so [`start`](InputSource::start) can pump it into
    /// the unified channel. Wraps the manager's existing midir-callback
    /// delivery — no behaviour change to how events are produced.
    pub fn connect_to_port(
        &mut self,
        port_index: usize,
        command_tx: mpsc::Sender<DaemonCommand>,
    ) -> Result<(usize, String), String> {
        let (raw_tx, raw_rx) = mpsc::channel::<MidiEvent>(1024);
        let res = self
            .manager
            .connect_to_port(port_index, raw_tx, command_tx)?;
        self.pending_rx = Some(raw_rx);
        Ok(res)
    }

    /// Borrow the wrapped manager.
    pub fn manager(&self) -> &MidiDeviceManager {
        &self.manager
    }

    /// Mutably borrow the wrapped manager (e.g. to `connect`).
    pub fn manager_mut(&mut self) -> &mut MidiDeviceManager {
        &mut self.manager
    }

    /// Clone the metrics handle so the push path can increment it from
    /// inside the midir callback without locking the hot path.
    pub fn metrics_handle(&self) -> InputSourceMetricsHandle {
        self.metrics.clone()
    }

    /// Test seam: stage an intermediate channel without real hardware, so the
    /// `start(tx)` round-trip can be exercised by feeding the returned sender.
    #[cfg(test)]
    pub(crate) fn attach_test_channel(&mut self) -> mpsc::Sender<MidiEvent> {
        let (tx, rx) = mpsc::channel::<MidiEvent>(1024);
        self.pending_rx = Some(rx);
        tx
    }
}

impl InputSource for MidiInputSource {
    fn protocol(&self) -> ConnectorProtocol {
        ConnectorProtocol::Midi
    }

    fn source_alias(&self) -> &str {
        &self.alias
    }

    fn shutdown(&mut self) {
        self.manager.shutdown();
    }

    fn metrics(&self) -> InputSourceMetrics {
        self.metrics.snapshot()
    }

    fn start(&mut self, tx: mpsc::Sender<DeviceEvent<ProtocolEvent>>) -> Result<(), String> {
        let rx = self.pending_rx.take().ok_or_else(|| {
            format!(
                "MidiInputSource '{}' not connected; call connect_to_port before start",
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

    fn unconnected_source(alias: &str) -> MidiInputSource {
        // Constructing a manager does not open any port (hardware-free).
        MidiInputSource::new(
            alias,
            MidiDeviceManager::new("Test Device".to_string(), false),
        )
    }

    #[test]
    fn protocol_is_midi() {
        assert_eq!(
            unconnected_source("pads").protocol(),
            ConnectorProtocol::Midi
        );
    }

    #[test]
    fn source_alias_is_the_endpoint_alias() {
        assert_eq!(unconnected_source("pads").source_alias(), "pads");
    }

    #[test]
    fn fresh_metrics_are_zeroed() {
        let m = unconnected_source("pads").metrics();
        assert_eq!(m, InputSourceMetrics::default());
    }

    #[test]
    fn metrics_handle_is_shared_with_the_source() {
        let src = unconnected_source("pads");
        src.metrics_handle().record_event();
        assert_eq!(src.metrics().events_in, 1);
    }

    #[test]
    fn shutdown_is_idempotent_when_unconnected() {
        let mut src = unconnected_source("pads");
        src.shutdown();
        src.shutdown();
        assert!(!src.manager().is_connected());
    }

    #[tokio::test]
    async fn start_pumps_source_events_onto_the_unified_channel() {
        // ADR-039: start(tx) wires the source's intermediate stream to the
        // pump via the §4.3 shed-load policy. Exercised hardware-free through the
        // test channel seam (no real midir port).
        use conductor_core::event_processor::MidiEvent;
        use std::time::Instant;

        let mut src = unconnected_source("pads").with_device_id(DeviceId::from_alias("pads"));
        let raw_tx = src.attach_test_channel();
        let (pump_tx, mut pump_rx) = mpsc::channel::<DeviceEvent<ProtocolEvent>>(8);

        src.start(pump_tx).expect("start succeeds once connected");

        raw_tx
            .send(MidiEvent::NoteOn {
                note: 36,
                velocity: 100,
                channel: 0,
                time: Instant::now(),
            })
            .await
            .unwrap();

        let got = pump_rx.recv().await.expect("event reaches the pump");
        assert_eq!(got.device_id(), &DeviceId::from_alias("pads"));
        assert!(matches!(got.event(), ProtocolEvent::Input(_)));
        assert_eq!(src.metrics().events_in, 1, "push path records the event");
    }

    #[tokio::test]
    async fn start_before_connect_is_an_error() {
        let mut src = unconnected_source("pads");
        let (pump_tx, _pump_rx) = mpsc::channel::<DeviceEvent<ProtocolEvent>>(8);
        assert!(
            src.start(pump_tx).is_err(),
            "start must refuse before connect_to_port stages a channel"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_manager() {
        let manager = MidiDeviceManager::new("Test Device".to_string(), true);
        assert!(!manager.is_connected());
        assert_eq!(manager.device_info(), None);
    }

    #[test]
    fn test_new_manager_empty_name() {
        let manager = MidiDeviceManager::new(String::new(), false);
        assert!(!manager.is_connected());
    }

    #[test]
    fn test_reconnect_backoff_schedule() {
        // Verify backoff schedule is correct
        assert_eq!(RECONNECT_BACKOFF, &[1, 2, 4, 8, 16, 30]);
        assert_eq!(MAX_RECONNECT_ATTEMPTS, 6);
    }

    #[tokio::test]
    async fn test_disconnect_when_not_connected() {
        let mut manager = MidiDeviceManager::new("Test".to_string(), false);
        // Should not panic
        manager.disconnect();
        assert!(!manager.is_connected());
    }

    #[tokio::test]
    async fn test_shutdown_when_not_connected() {
        let mut manager = MidiDeviceManager::new("Test".to_string(), false);
        // Should not panic
        manager.shutdown();
        assert!(!manager.is_connected());
    }

    // -------------------------------------------------------------------
    // ADR-026 Phase 1.B — SysEx ingress hook
    // -------------------------------------------------------------------

    #[test]
    fn is_sysex_accepts_valid_frame() {
        assert!(is_sysex(&[0xF0, 0x7E, 0x00, 0x06, 0x01, 0xF7]));
        assert!(is_sysex(&[0xF0, 0xF7]));
    }

    #[test]
    fn is_sysex_rejects_non_sysex() {
        // Note On (0x90)
        assert!(!is_sysex(&[0x90, 0x3C, 0x7F]));
        // Missing F7 terminator
        assert!(!is_sysex(&[0xF0, 0x7E, 0x00]));
        // Starts with F7 (truncated SysEx end-only)
        assert!(!is_sysex(&[0xF7]));
        // Empty
        assert!(!is_sysex(&[]));
        // Single byte
        assert!(!is_sysex(&[0xF0]));
    }

    #[tokio::test]
    async fn handle_midi_message_forwards_sysex_to_probe_coordinator() {
        // Arrange: running probe waiting on "test-port" for an Identity
        // Reply. We simulate the midir callback by calling
        // handle_midi_message directly with a canned reply.
        use conductor_core::device_intelligence::probe::{ProbeCoordinator, ProbeResult};
        use std::sync::Arc as StdArc;

        let coord = StdArc::new(ProbeCoordinator::new().with_timeout(Duration::from_millis(200)));
        let (event_tx, _event_rx) = mpsc::channel::<MidiEvent>(16);

        // Spawn the probe on a blocking thread so this test can call
        // handle_midi_message on the current thread once probe() has
        // registered its pending slot.
        let coord_bg = coord.clone();
        let (registered_tx, registered_rx) = std::sync::mpsc::sync_channel::<()>(1);
        let probe_handle = std::thread::spawn(move || {
            coord_bg.probe("test-port", |_| {
                registered_tx.send(()).unwrap();
                Ok(())
            })
        });

        registered_rx
            .recv_timeout(Duration::from_millis(500))
            .expect("probe should register pending slot promptly");

        // Canned KORG Identity Reply
        let reply = &[
            0xF0, 0x7E, 0x00, 0x06, 0x02, 0x42, 0x34, 0x00, 0x01, 0x00, 0x01, 0x02, 0x03, 0x04,
            0xF7,
        ];

        handle_midi_message(
            reply,
            "test-port",
            Some(&*coord as &dyn SysExObserver),
            &event_tx,
        );

        let outcome = probe_handle.join().unwrap();
        match outcome {
            Ok(ProbeResult::Identified { identity, .. }) => {
                assert_eq!(identity.manufacturer_id, vec![0x42]);
            }
            other => panic!("expected Ok(Identified), got {:?}", other),
        }
    }

    /// Test-only `SysExObserver` impl that records every call. Lets us
    /// directly assert the `is_sysex` gate in `handle_midi_message`
    /// works, rather than relying on `ProbeCoordinator::observe_reply`'s
    /// defensive early-return to mask a broken gate.
    struct RecordingObserver {
        calls: Mutex<Vec<(String, Vec<u8>)>>,
    }

    impl RecordingObserver {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
            }
        }
        fn calls(&self) -> Vec<(String, Vec<u8>)> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl conductor_core::device_intelligence::probe::SysExObserver for RecordingObserver {
        fn observe_reply(&self, port_name: &str, bytes: &[u8]) {
            self.calls
                .lock()
                .unwrap()
                .push((port_name.to_string(), bytes.to_vec()));
        }
    }

    #[tokio::test]
    async fn handle_midi_message_non_sysex_does_not_reach_observer() {
        // The is_sysex gate in handle_midi_message must prevent non-
        // SysEx bytes from reaching the observer at all — a broken
        // gate would be masked by ProbeCoordinator's internal shape
        // check, so we use a RecordingObserver to assert directly.
        let observer = RecordingObserver::new();
        let (event_tx, mut event_rx) = mpsc::channel::<MidiEvent>(16);

        // Note-On, Control-Change, Pitch-Bend, Program-Change — none are SysEx.
        let non_sysex_messages: &[&[u8]] = &[
            &[0x90, 0x3C, 0x7F], // Note-On
            &[0xB0, 0x07, 0x40], // CC
            &[0xE0, 0x00, 0x40], // Pitch-Bend
            &[0xC0, 0x05],       // Program-Change
        ];
        for msg in non_sysex_messages {
            handle_midi_message(msg, "note-port", Some(&observer), &event_tx);
        }

        assert_eq!(
            observer.calls(),
            Vec::<(String, Vec<u8>)>::new(),
            "non-SysEx bytes must not reach the observer",
        );

        // Non-SysEx events are still forwarded to event_tx (side-observing).
        for _ in 0..non_sysex_messages.len() {
            let event = tokio::time::timeout(Duration::from_millis(50), event_rx.recv())
                .await
                .expect("non-SysEx events should dispatch")
                .expect("channel not closed");
            // We don't care which; the existence is the check.
            let _ = event;
        }
    }

    #[tokio::test]
    async fn handle_midi_message_sysex_reaches_observer_with_port_and_bytes() {
        // Positive gate test — the SysEx bytes DO reach the observer
        // with the port name and byte slice intact. Complements the
        // negative-gate `_non_sysex_does_not_reach_observer` test.
        let observer = RecordingObserver::new();
        let (event_tx, _event_rx) = mpsc::channel::<MidiEvent>(16);

        let reply = &[
            0xF0, 0x7E, 0x00, 0x06, 0x02, 0x42, 0x34, 0x00, 0x01, 0x00, 0x01, 0x02, 0x03, 0x04,
            0xF7,
        ];
        handle_midi_message(reply, "ident-port", Some(&observer), &event_tx);

        let calls = observer.calls();
        assert_eq!(calls.len(), 1, "SysEx must reach the observer exactly once");
        assert_eq!(calls[0].0, "ident-port");
        assert_eq!(calls[0].1, reply);
    }

    #[tokio::test]
    async fn handle_midi_message_without_coordinator_still_forwards_events() {
        // probe_coordinator = None is a valid state (tests, gamepad-only
        // contexts). Ensure events still get forwarded normally.
        let (event_tx, mut event_rx) = mpsc::channel::<MidiEvent>(16);
        handle_midi_message(&[0x90, 0x3C, 0x7F], "any-port", None, &event_tx);
        let event = tokio::time::timeout(Duration::from_millis(50), event_rx.recv())
            .await
            .expect("event should dispatch")
            .expect("channel not closed");
        assert!(matches!(event, MidiEvent::NoteOn { note: 0x3C, .. }));
    }

    #[tokio::test]
    async fn handle_midi_message_sysex_still_falls_through_when_unparseable() {
        // SysEx bytes go to observe_reply AND through from_midi_msg.
        // from_midi_msg rejects SysEx (not a MidiMessage variant) — the
        // parse error is logged at debug level and no event is sent,
        // which is the expected side-observing behaviour: the probe
        // hook saw the bytes, the mapping pipeline correctly drops
        // them, and nothing panics.
        use conductor_core::device_intelligence::probe::ProbeCoordinator;
        use std::sync::Arc as StdArc;

        let coord = StdArc::new(ProbeCoordinator::new());
        let (event_tx, mut event_rx) = mpsc::channel::<MidiEvent>(16);

        // No pending probe — observe_reply is a silent no-op; the SysEx
        // bytes pass through to from_midi_msg which also drops them.
        handle_midi_message(
            &[
                0xF0, 0x7E, 0x00, 0x06, 0x02, 0x42, 0x34, 0x00, 0x01, 0x00, 0x01, 0x02, 0x03, 0x04,
                0xF7,
            ],
            "unpaired",
            Some(&*coord as &dyn SysExObserver),
            &event_tx,
        );

        // No MidiEvent should be produced.
        let recv = tokio::time::timeout(Duration::from_millis(20), event_rx.recv()).await;
        assert!(
            recv.is_err(),
            "SysEx must not produce a MidiEvent; got {:?}",
            recv
        );
    }

    #[test]
    fn set_probe_coordinator_stores_the_arc() {
        use conductor_core::device_intelligence::probe::ProbeCoordinator;
        use std::sync::Arc as StdArc;

        let mut manager = MidiDeviceManager::new("Test".to_string(), false);
        assert!(manager.probe_coordinator.is_none());

        let coord = StdArc::new(ProbeCoordinator::new());
        manager.set_probe_coordinator(coord.clone());
        assert!(manager.probe_coordinator.is_some());
        assert!(StdArc::ptr_eq(
            manager.probe_coordinator.as_ref().unwrap(),
            &coord
        ));
    }
}
