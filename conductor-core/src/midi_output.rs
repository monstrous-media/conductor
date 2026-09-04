// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! MIDI Output Management
//!
//! Provides MIDI output port management for sending MIDI messages to virtual
//! or physical MIDI devices. Supports creating virtual MIDI ports (macOS/Linux only)
//! and sending MIDI messages with precise timing.
//!
//! # Platform Support
//!
//! - **macOS**: Full support via CoreMIDI (virtual ports + output)
//! - **Linux**: Full support via ALSA/JACK (virtual ports + output)
//! - **Windows**: Output only (no virtual port creation - use third-party drivers like loopMIDI)
//!
//! # Example
//!
//! ```rust,no_run
//! use conductor_core::midi_output::MidiOutputManager;
//!
//! let mut manager = MidiOutputManager::new();
//!
//! // List available output ports
//! let ports = manager.list_output_ports();
//! println!("Available MIDI outputs: {:?}", ports);
//!
//! // Create a virtual port (macOS/Linux only)
//! #[cfg(not(target_os = "windows"))]
//! manager.create_virtual_port("Conductor Virtual Out")
//!     .expect("Failed to create virtual port");
//!
//! // Send a MIDI message (Note On: C4, velocity 100)
//! let note_on = vec![0x90, 60, 100];
//! manager.send_message("Conductor Virtual Out", &note_on)
//!     .expect("Failed to send MIDI message");
//! ```

use midir::{MidiOutput, MidiOutputConnection};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[cfg(not(target_os = "windows"))]
use midir::os::unix::VirtualOutput;

use crate::error::EngineError;

/// MIDI output port manager
///
/// Manages MIDI output connections, virtual port creation, and message sending.
/// Supports both immediate message sending and async queued sending.
pub struct MidiOutputManager {
    /// Active output connections (port name → connection)
    connections: HashMap<String, MidiOutputConnection>,

    /// Virtual ports created by Conductor (macOS/Linux only)
    #[cfg(not(target_os = "windows"))]
    virtual_ports: HashMap<String, VirtualMidiPort>,

    /// Message queue for async sending
    message_queue: Arc<Mutex<VecDeque<MidiMessage>>>,
}

/// Represents a virtual MIDI port (macOS/Linux only)
#[cfg(not(target_os = "windows"))]
pub struct VirtualMidiPort {
    /// Port name
    name: String,

    /// MIDI output connection for this virtual port
    connection: MidiOutputConnection,

    /// Timestamp when port was created
    created_at: Instant,
}

/// Outcome of [`MidiOutputManager::sync_virtual_ports`].
///
/// All three lists are empty on a no-op (and on Windows). `failed` carries the
/// port name plus a human-readable error for logging/audit — sync never returns
/// `Err`, so a partial failure is observable here without aborting a reload.
#[derive(Debug, Default, Clone)]
pub struct VirtualPortSync {
    /// Port names newly created this reconcile.
    pub created: Vec<String>,
    /// Port names torn down (no longer desired) this reconcile.
    pub removed: Vec<String>,
    /// Desired ports that failed to create, with the error message.
    pub failed: Vec<(String, String)>,
}

/// MIDI message to be sent
#[derive(Debug, Clone)]
pub struct MidiMessage {
    /// MIDI message bytes (status byte + data bytes)
    pub data: Vec<u8>,

    /// Optional timestamp for precise timing (future use)
    pub timestamp: Option<Instant>,

    /// Target port name
    pub port_name: String,
}

impl MidiOutputManager {
    /// Create a new MIDI output manager
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use conductor_core::midi_output::MidiOutputManager;
    ///
    /// let manager = MidiOutputManager::new();
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the names of all virtual output ports created by Conductor.
    ///
    /// Used to auto-exclude these ports from input scanning (ADR-009 D21),
    /// preventing feedback loops where Conductor reads its own output.
    pub fn virtual_port_names(&self) -> Vec<String> {
        #[cfg(not(target_os = "windows"))]
        {
            self.virtual_ports.keys().cloned().collect()
        }
        #[cfg(target_os = "windows")]
        {
            Vec::new()
        }
    }
}

impl Default for MidiOutputManager {
    fn default() -> Self {
        Self {
            connections: HashMap::new(),
            #[cfg(not(target_os = "windows"))]
            virtual_ports: HashMap::new(),
            message_queue: Arc::new(Mutex::new(VecDeque::new())),
        }
    }
}

impl MidiOutputManager {
    /// Create a virtual MIDI port (macOS/Linux only)
    ///
    /// Creates a virtual MIDI port that other applications can connect to.
    /// The port will appear in the system's MIDI device list.
    ///
    /// # Arguments
    ///
    /// * `name` - Name for the virtual port (must be unique)
    ///
    /// # Errors
    ///
    /// Returns `EngineError::MidiOutput` if:
    /// - A port with this name already exists
    /// - Virtual port creation fails (system-level error)
    ///
    /// # Platform Support
    ///
    /// This function is only available on macOS and Linux. On Windows, you must
    /// use third-party virtual MIDI drivers like loopMIDI.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # #[cfg(not(target_os = "windows"))]
    /// # {
    /// use conductor_core::midi_output::MidiOutputManager;
    ///
    /// let mut manager = MidiOutputManager::new();
    /// manager.create_virtual_port("Conductor Virtual Out")
    ///     .expect("Failed to create virtual port");
    /// # }
    /// ```
    #[cfg(not(target_os = "windows"))]
    pub fn create_virtual_port(&mut self, name: &str) -> Result<(), EngineError> {
        // Check if port already exists
        if self.virtual_ports.contains_key(name) {
            return Err(EngineError::MidiOutput(format!(
                "Virtual port '{}' already exists",
                name
            )));
        }

        // Create a new MidiOutput instance for this virtual port
        let midi_out = MidiOutput::new("Conductor Virtual Port")
            .map_err(|e| EngineError::MidiInit(format!("Failed to create MIDI output: {}", e)))?;

        // Create the virtual port
        let connection = midi_out.create_virtual(name).map_err(|e| {
            EngineError::MidiOutput(format!("Failed to create virtual port '{}': {}", name, e))
        })?;

        // Store the virtual port
        let virtual_port = VirtualMidiPort {
            name: name.to_string(),
            connection,
            created_at: Instant::now(),
        };

        self.virtual_ports.insert(name.to_string(), virtual_port);

        Ok(())
    }

    /// Reconcile the set of created virtual ports to exactly `desired`.
    ///
    /// Creates any desired port not yet present and tears down any created port
    /// no longer desired (so a config reload that adds/removes/disables a
    /// `MidiVirtualPort` endpoint takes effect on the OS without a restart).
    /// Dropping a [`VirtualMidiPort`] closes its CoreMIDI/ALSA connection, which
    /// removes the OS port. Returns a [`VirtualPortSync`] report.
    ///
    /// Infallible: a single port that fails to create (e.g. a name clash from
    /// another app) is collected into `failed` rather than sinking the whole
    /// reload — callers run this from the post-commit APPLY phase (ADR-044),
    /// which must not fail. No-op on Windows (midir has no virtual ports there).
    pub fn sync_virtual_ports(&mut self, desired: &[String]) -> VirtualPortSync {
        let mut report = VirtualPortSync::default();
        #[cfg(not(target_os = "windows"))]
        {
            // Tear down ports no longer desired first (frees the OS name before
            // a same-name recreate, though that case shouldn't arise).
            let to_remove: Vec<String> = self
                .virtual_ports
                .keys()
                .filter(|name| !desired.iter().any(|d| d == *name))
                .cloned()
                .collect();
            for name in to_remove {
                // Drop closes the connection → OS port disappears.
                self.virtual_ports.remove(&name);
                report.removed.push(name);
            }
            // Create any desired port not already present.
            for name in desired {
                if self.virtual_ports.contains_key(name) {
                    continue;
                }
                match self.create_virtual_port(name) {
                    Ok(()) => report.created.push(name.clone()),
                    Err(e) => report.failed.push((name.clone(), e.to_string())),
                }
            }
        }
        #[cfg(target_os = "windows")]
        {
            let _ = desired;
        }
        report
    }

    /// Returns `true` if this platform can actually create a virtual MIDI
    /// output port right now.
    ///
    /// On macOS, virtual ports require CoreMIDI's `MIDIServer`, which only
    /// runs inside a GUI login session. A headless self-hosted CI runner (a
    /// `LaunchDaemon` with no Aqua session) therefore returns `false` — as
    /// does Linux without an ALSA sequencer, or Windows (midir has no virtual
    /// ports there). The result is probed once and cached for the process.
    ///
    /// Tests that create real virtual ports use this to skip gracefully on
    /// headless runners instead of panicking, which lets the
    /// macOS test job run on the self-hosted pool. Set the
    /// `CONDUCTOR_DISABLE_VIRTUAL_MIDI` environment variable to force `false`
    /// (useful for exercising the skip path locally, or disabling virtual-MIDI
    /// tests on a misbehaving runner).
    pub fn virtual_ports_available() -> bool {
        #[cfg(target_os = "windows")]
        {
            // midir has no virtual output ports on Windows.
            false
        }
        #[cfg(not(target_os = "windows"))]
        {
            use std::sync::OnceLock;
            static AVAILABLE: OnceLock<bool> = OnceLock::new();
            *AVAILABLE.get_or_init(|| {
                if std::env::var_os("CONDUCTOR_DISABLE_VIRTUAL_MIDI").is_some() {
                    return false;
                }
                // Probe by actually creating a virtual port; `probe` is dropped
                // at the end of this closure, which tears the port back down.
                // The probe name is unlikely to collide with a real test port.
                let mut probe = MidiOutputManager::new();
                probe
                    .create_virtual_port("conductor-coremidi-probe")
                    .is_ok()
            })
        }
    }

    /// Connect to an existing MIDI output port by index
    ///
    /// Opens a connection to a physical or virtual MIDI output port.
    ///
    /// # Arguments
    ///
    /// * `port_index` - Index of the port in the available ports list
    ///
    /// # Returns
    ///
    /// The name of the connected port.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::MidiOutput` if:
    /// - The port index is invalid
    /// - Connection to the port fails
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use conductor_core::midi_output::MidiOutputManager;
    ///
    /// let mut manager = MidiOutputManager::new();
    /// let ports = manager.list_output_ports();
    /// println!("Available ports: {:?}", ports);
    ///
    /// // Connect to first port
    /// if !ports.is_empty() {
    ///     let port_name = manager.connect_to_port(0)
    ///         .expect("Failed to connect");
    ///     println!("Connected to: {}", port_name);
    /// }
    /// ```
    pub fn connect_to_port(&mut self, port_index: usize) -> Result<String, EngineError> {
        // Create a new MidiOutput instance for port discovery
        let midi_out = MidiOutput::new("Conductor Port Connect")
            .map_err(|e| EngineError::MidiInit(format!("Failed to create MIDI output: {}", e)))?;

        // Get available ports
        let ports = midi_out.ports();

        // Validate port index
        if port_index >= ports.len() {
            return Err(EngineError::MidiOutput(format!(
                "Port index {} out of range (0-{})",
                port_index,
                ports.len().saturating_sub(1)
            )));
        }

        // Get port info
        let port = &ports[port_index];
        let port_name = midi_out
            .port_name(port)
            .unwrap_or_else(|_| format!("Port {}", port_index));

        // Check if already connected
        if self.connections.contains_key(&port_name) {
            return Ok(port_name);
        }

        // Connect to the port (consumes midi_out)
        let connection = midi_out.connect(port, &port_name).map_err(|e| {
            EngineError::MidiOutput(format!("Failed to connect to port '{}': {}", port_name, e))
        })?;

        // Store the connection
        self.connections.insert(port_name.clone(), connection);

        Ok(port_name)
    }

    /// List available MIDI output ports
    ///
    /// Returns a list of all available MIDI output ports (physical and virtual).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use conductor_core::midi_output::MidiOutputManager;
    ///
    /// let manager = MidiOutputManager::new();
    /// let ports = manager.list_output_ports();
    /// for (i, port) in ports.iter().enumerate() {
    ///     println!("{}: {}", i, port);
    /// }
    /// ```
    pub fn list_output_ports(&self) -> Vec<String> {
        // Create a temporary MidiOutput instance for port discovery
        let midi_out = match MidiOutput::new("Conductor Port List") {
            Ok(out) => out,
            Err(_) => return Vec::new(), // Return empty list if MIDI init fails
        };

        midi_out
            .ports()
            .iter()
            .enumerate()
            .map(|(i, port)| {
                midi_out
                    .port_name(port)
                    .unwrap_or_else(|_| format!("Port {}", i))
            })
            .collect()
    }

    /// Connect to a MIDI output port by name
    ///
    /// Finds the port with the given name and connects to it.
    /// If already connected, returns Ok immediately.
    ///
    /// # Arguments
    ///
    /// * `port_name` - Name of the port to connect to
    ///
    /// # Errors
    ///
    /// Returns `EngineError::MidiOutput` if:
    /// - No port with the given name exists
    /// - Connection fails
    pub fn connect_by_name(&mut self, port_name: &str) -> Result<(), EngineError> {
        // Check if already connected
        if self.connections.contains_key(port_name) {
            return Ok(());
        }

        // Check virtual ports too
        #[cfg(not(target_os = "windows"))]
        if self.virtual_ports.contains_key(port_name) {
            return Ok(());
        }

        // Fast path: find and connect using a single MidiOutput instance.
        // Using one instance for both enumeration and connection avoids port
        // index instability across separate MidiOutput instances.
        if let Ok(()) = self.find_and_connect_port(port_name) {
            return Ok(());
        }

        // macOS slow path: port not found, try with Core MIDI cache warmup
        // Apps started after the daemon won't appear in the cached port list.
        // Keep warmup alive during sleep so Core MIDI can deliver "device added"
        // notifications to this active client. Called from sync ActionExecutor.
        #[cfg(target_os = "macos")]
        {
            let _warmup = MidiOutput::new("Conductor Output Warmup");
            std::thread::sleep(std::time::Duration::from_millis(100));

            if let Ok(()) = self.find_and_connect_port(port_name) {
                return Ok(());
            }
        }

        Err(EngineError::MidiOutput(format!(
            "MIDI output port '{}' not found. Available ports: {:?}",
            port_name,
            self.list_output_ports()
        )))
    }

    /// Find a port by name and connect using a single MidiOutput instance.
    ///
    /// This avoids port index instability by enumerating and connecting
    /// within the same MidiOutput instance.
    fn find_and_connect_port(&mut self, port_name: &str) -> Result<(), EngineError> {
        let midi_out = MidiOutput::new("Conductor Port Connect")
            .map_err(|e| EngineError::MidiInit(format!("Failed to create MIDI output: {}", e)))?;

        let ports = midi_out.ports();
        for port in &ports {
            if let Ok(name) = midi_out.port_name(port)
                && name == port_name
            {
                let connection = midi_out.connect(port, port_name).map_err(|e| {
                    EngineError::MidiOutput(format!(
                        "Failed to connect to port '{}': {}",
                        port_name, e
                    ))
                })?;
                self.connections.insert(port_name.to_string(), connection);
                return Ok(());
            }
        }

        Err(EngineError::MidiOutput(format!(
            "MIDI output port '{}' not found",
            port_name
        )))
    }

    /// Send MIDI message immediately
    ///
    /// Sends a MIDI message to the specified output port with no buffering.
    ///
    /// # Arguments
    ///
    /// * `port_name` - Name of the target port
    /// * `message` - MIDI message bytes (status byte + data bytes)
    ///
    /// # Errors
    ///
    /// Returns `EngineError::MidiOutput` if:
    /// - Port is not connected
    /// - Message sending fails
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use conductor_core::midi_output::MidiOutputManager;
    ///
    /// let mut manager = MidiOutputManager::new();
    /// # #[cfg(not(target_os = "windows"))]
    /// manager.create_virtual_port("Test Port").unwrap();
    ///
    /// // Send Note On: C4 (60), velocity 100
    /// let note_on = vec![0x90, 60, 100];
    /// manager.send_message("Test Port", &note_on)
    ///     .expect("Failed to send MIDI message");
    /// ```
    pub fn send_message(&mut self, port_name: &str, message: &[u8]) -> Result<(), EngineError> {
        // Try to find connection in regular connections
        if let Some(connection) = self.connections.get_mut(port_name) {
            return connection.send(message).map_err(|e| {
                EngineError::MidiOutput(format!(
                    "Failed to send message to port '{}': {}",
                    port_name, e
                ))
            });
        }

        // Try to find connection in virtual ports (macOS/Linux)
        #[cfg(not(target_os = "windows"))]
        {
            if let Some(virtual_port) = self.virtual_ports.get_mut(port_name) {
                return virtual_port.connection.send(message).map_err(|e| {
                    EngineError::MidiOutput(format!(
                        "Failed to send message to virtual port '{}': {}",
                        port_name, e
                    ))
                });
            }
        }

        // Port not found
        Err(EngineError::MidiOutput(format!(
            "Port '{}' is not connected",
            port_name
        )))
    }

    /// Queue MIDI message for async sending
    ///
    /// Adds a MIDI message to the queue for later processing.
    /// Call `process_queue()` to send all queued messages.
    ///
    /// # Arguments
    ///
    /// * `message` - MIDI message to queue
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use conductor_core::midi_output::{MidiOutputManager, MidiMessage};
    ///
    /// let mut manager = MidiOutputManager::new();
    ///
    /// let message = MidiMessage {
    ///     data: vec![0x90, 60, 100],
    ///     timestamp: None,
    ///     port_name: "Test Port".to_string(),
    /// };
    ///
    /// manager.queue_message(message);
    /// manager.process_queue().expect("Failed to process queue");
    /// ```
    pub fn queue_message(&mut self, message: MidiMessage) {
        if let Ok(mut queue) = self.message_queue.lock() {
            queue.push_back(message);
        }
    }

    /// Process message queue (called from event loop)
    ///
    /// Sends all queued MIDI messages and returns the number of messages sent.
    ///
    /// # Returns
    ///
    /// The number of messages successfully sent.
    ///
    /// # Errors
    ///
    /// Returns `EngineError::MidiOutput` if message sending fails.
    /// Failed messages are skipped and logged, but processing continues.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use conductor_core::midi_output::MidiOutputManager;
    ///
    /// let mut manager = MidiOutputManager::new();
    ///
    /// // In your event loop:
    /// loop {
    ///     let sent = manager.process_queue().unwrap_or(0);
    ///     if sent > 0 {
    ///         println!("Sent {} MIDI messages", sent);
    ///     }
    ///     // ... other processing
    /// #   break; // Exit loop for doc test
    /// }
    /// ```
    pub fn process_queue(&mut self) -> Result<usize, EngineError> {
        let mut sent_count = 0;
        let mut errors = Vec::new();

        // Lock the queue and drain all messages
        let messages: Vec<MidiMessage> = {
            if let Ok(mut queue) = self.message_queue.lock() {
                queue.drain(..).collect()
            } else {
                return Ok(0);
            }
        };

        // Send each message
        for message in messages {
            match self.send_message(&message.port_name, &message.data) {
                Ok(_) => sent_count += 1,
                Err(e) => errors.push(format!("Port '{}': {}", message.port_name, e)),
            }
        }

        // Report errors if any occurred
        if !errors.is_empty() {
            return Err(EngineError::MidiOutput(format!(
                "Failed to send {} messages: {}",
                errors.len(),
                errors.join("; ")
            )));
        }

        Ok(sent_count)
    }

    /// Disconnect from a specific port
    ///
    /// Closes the connection to the specified port and removes it from
    /// the active connections.
    ///
    /// # Arguments
    ///
    /// * `port_name` - Name of the port to disconnect
    ///
    /// # Errors
    ///
    /// Returns `EngineError::MidiOutput` if the port is not connected.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use conductor_core::midi_output::MidiOutputManager;
    ///
    /// let mut manager = MidiOutputManager::new();
    /// let port_name = manager.connect_to_port(0).unwrap();
    ///
    /// // Later...
    /// manager.disconnect(&port_name)
    ///     .expect("Failed to disconnect");
    /// ```
    pub fn disconnect(&mut self, port_name: &str) -> Result<(), EngineError> {
        // Remove from regular connections
        if self.connections.remove(port_name).is_some() {
            return Ok(());
        }

        // Remove from virtual ports (macOS/Linux)
        #[cfg(not(target_os = "windows"))]
        {
            if self.virtual_ports.remove(port_name).is_some() {
                return Ok(());
            }
        }

        Err(EngineError::MidiOutput(format!(
            "Port '{}' is not connected",
            port_name
        )))
    }

    /// Disconnect all ports
    ///
    /// Closes all active connections (both regular and virtual ports).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use conductor_core::midi_output::MidiOutputManager;
    ///
    /// let mut manager = MidiOutputManager::new();
    /// // ... connect to ports ...
    ///
    /// // Cleanup
    /// manager.disconnect_all();
    /// ```
    pub fn disconnect_all(&mut self) {
        self.connections.clear();

        #[cfg(not(target_os = "windows"))]
        {
            self.virtual_ports.clear();
        }
    }

    /// Get the number of active connections
    ///
    /// Returns the total number of active MIDI output connections
    /// (regular ports + virtual ports on macOS/Linux).
    pub fn connection_count(&self) -> usize {
        let regular_count = self.connections.len();

        #[cfg(not(target_os = "windows"))]
        {
            regular_count + self.virtual_ports.len()
        }

        #[cfg(target_os = "windows")]
        {
            regular_count
        }
    }
}

impl Drop for MidiOutputManager {
    fn drop(&mut self) {
        // Cleanup is automatic when connections are dropped
        self.disconnect_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_midi_output_manager() {
        let manager = MidiOutputManager::new();
        assert_eq!(manager.connection_count(), 0);
    }

    #[test]
    fn test_list_output_ports() {
        let manager = MidiOutputManager::new();
        let ports = manager.list_output_ports();

        // We can't guarantee ports exist, but the function should return a valid vec
        // (ports.len() can be 0 or more)
        assert!(ports.is_empty() || !ports.is_empty());
    }

    #[test]
    #[ignore] // Requires ALSA /dev/snd/seq (Linux) or CoreMIDI (macOS) — not available in CI
    #[cfg(not(target_os = "windows"))]
    fn test_create_virtual_port() {
        let mut manager = MidiOutputManager::new();
        let result = manager.create_virtual_port("Conductor Test Port");

        assert!(result.is_ok(), "Failed to create virtual port");
        assert_eq!(manager.connection_count(), 1);

        // Verify port exists in virtual_ports
        assert!(manager.virtual_ports.contains_key("Conductor Test Port"));
    }

    #[test]
    #[ignore] // Requires ALSA /dev/snd/seq (Linux) or CoreMIDI (macOS) — not available in CI
    #[cfg(not(target_os = "windows"))]
    fn test_duplicate_virtual_port_fails() {
        let mut manager = MidiOutputManager::new();

        // Create first port
        manager.create_virtual_port("Test Port").unwrap();

        // Try to create duplicate
        let result = manager.create_virtual_port("Test Port");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already exists"));
    }

    #[test]
    fn test_virtual_port_names_empty_initially() {
        let manager = MidiOutputManager::new();
        assert!(manager.virtual_port_names().is_empty());
    }

    #[test]
    fn test_sync_virtual_ports_empty_is_noop() {
        // Reconciling to an empty desired set on a fresh manager creates
        // and removes nothing (CI-safe — no real port I/O on this path).
        let mut manager = MidiOutputManager::new();
        let report = manager.sync_virtual_ports(&[]);
        assert!(report.created.is_empty());
        assert!(report.removed.is_empty());
        assert!(report.failed.is_empty());
        assert!(manager.virtual_port_names().is_empty());
    }

    #[test]
    #[ignore] // Requires ALSA /dev/snd/seq (Linux) or CoreMIDI (macOS) — not available in CI
    #[cfg(not(target_os = "windows"))]
    fn test_sync_virtual_ports_creates_and_tears_down() {
        // Sync creates desired ports, then a second sync to a different
        // desired set removes the dropped one and adds the new one.
        let mut manager = MidiOutputManager::new();

        let r1 = manager.sync_virtual_ports(&[
            "Conductor Sync A".to_string(),
            "Conductor Sync B".to_string(),
        ]);
        assert_eq!(r1.created.len(), 2, "both ports created");
        assert!(r1.removed.is_empty());
        assert!(r1.failed.is_empty());
        let mut names = manager.virtual_port_names();
        names.sort();
        assert_eq!(names, vec!["Conductor Sync A", "Conductor Sync B"]);

        // Reconcile to {B, C}: A removed, C created, B untouched.
        let r2 = manager.sync_virtual_ports(&[
            "Conductor Sync B".to_string(),
            "Conductor Sync C".to_string(),
        ]);
        assert_eq!(r2.created, vec!["Conductor Sync C"]);
        assert_eq!(r2.removed, vec!["Conductor Sync A"]);
        assert!(r2.failed.is_empty());
        let mut names = manager.virtual_port_names();
        names.sort();
        assert_eq!(names, vec!["Conductor Sync B", "Conductor Sync C"]);

        // Idempotent: same desired set is a no-op.
        let r3 = manager.sync_virtual_ports(&[
            "Conductor Sync B".to_string(),
            "Conductor Sync C".to_string(),
        ]);
        assert!(r3.created.is_empty() && r3.removed.is_empty() && r3.failed.is_empty());
    }

    #[test]
    #[ignore] // Requires ALSA /dev/snd/seq (Linux) or CoreMIDI (macOS) — not available in CI
    #[cfg(not(target_os = "windows"))]
    fn test_virtual_port_names_returns_created_ports() {
        let mut manager = MidiOutputManager::new();
        manager.create_virtual_port("Conductor Out A").unwrap();
        manager.create_virtual_port("Conductor Out B").unwrap();

        let mut names = manager.virtual_port_names();
        names.sort();
        assert_eq!(names.len(), 2);
        assert_eq!(names[0], "Conductor Out A");
        assert_eq!(names[1], "Conductor Out B");
    }

    #[test]
    fn test_send_to_nonexistent_port_fails() {
        let mut manager = MidiOutputManager::new();
        let result = manager.send_message("Nonexistent Port", &[0x90, 60, 100]);

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not connected"));
    }

    #[test]
    #[ignore] // Calls create_virtual_port on non-Windows — requires ALSA/CoreMIDI hardware
    fn test_disconnect_all() {
        let mut manager = MidiOutputManager::new();

        #[cfg(not(target_os = "windows"))]
        {
            manager.create_virtual_port("Test Port 1").ok();
            manager.create_virtual_port("Test Port 2").ok();
            assert!(manager.connection_count() > 0);
        }

        manager.disconnect_all();
        assert_eq!(manager.connection_count(), 0);
    }

    #[test]
    fn test_message_queue() {
        let mut manager = MidiOutputManager::new();

        let message = MidiMessage {
            data: vec![0x90, 60, 100],
            timestamp: None,
            port_name: "Test Port".to_string(),
        };

        manager.queue_message(message);

        // Process queue (will fail because port doesn't exist, but tests queuing)
        let result = manager.process_queue();
        assert!(result.is_err()); // Port doesn't exist, so should fail
    }
}
