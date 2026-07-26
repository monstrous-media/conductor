// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Conductor Daemon - Background service for MIDI controller mapping
//!
//! This crate provides the daemon infrastructure for running Conductor as a background service
//! with config hot-reload, IPC control, state persistence, and lifecycle management.
//!
//! # Architecture
//!
//! The daemon follows a modular architecture with clear separation of concerns:
//!
//! ```text
//! ┌──────────────────────────────────────────────────┐
//! │  conductorctl (CLI)                                │
//! │  - status, reload, stop, validate, ping          │
//! └────────────┬─────────────────────────────────────┘
//!              │ IPC (JSON over Unix socket)
//!              ▼
//! ┌──────────────────────────────────────────────────┐
//! │  conductor-daemon Service                          │
//! │  ┌────────────────────────────────────────────┐ │
//! │  │  IPC Server                                │ │
//! │  │  - Accept connections                      │ │
//! │  │  - Route commands                          │ │
//! │  └──────────┬─────────────────────────────────┘ │
//! │             ▼                                    │
//! │  ┌────────────────────────────────────────────┐ │
//! │  │  Engine Manager                            │ │
//! │  │  - Lifecycle management                    │ │
//! │  │  - Atomic config swaps (Arc<RwLock<>>)     │ │
//! │  │  - Performance metrics                     │ │
//! │  └──────────┬─────────────────────────────────┘ │
//! │             ▼                                    │
//! │  ┌────────────────────────────────────────────┐ │
//! │  │  Config Watcher                            │ │
//! │  │  - File system monitoring                  │ │
//! │  │  - 500ms debounce                          │ │
//! │  └────────────────────────────────────────────┘ │
//! │                                                  │
//! │  ┌────────────────────────────────────────────┐ │
//! │  │  State Manager                             │ │
//! │  │  - Atomic persistence                      │ │
//! │  │  - Emergency save handler                  │ │
//! │  └────────────────────────────────────────────┘ │
//! └──────────────────────────────────────────────────┘
//!              │
//!              ▼
//! ┌──────────────────────────────────────────────────┐
//! │  conductor-core Engine                             │
//! │  - Event processing                              │
//! │  - Mapping execution                             │
//! │  - Action dispatch                               │
//! └──────────────────────────────────────────────────┘
//! ```
//!
//! # Key Features
//!
//! ## Config Hot-Reload
//!
//! - **Zero Downtime**: Reload configuration without restarting
//! - **Fast**: 0-8ms reload latency (production configs: <3ms)
//! - **Atomic**: All-or-nothing config swaps via Arc<RwLock<>>
//! - **Validated**: Configuration checked before applying
//!
//! ## IPC Control
//!
//! - **Unix Domain Sockets**: Low-latency inter-process communication
//! - **JSON Protocol**: Structured request/response format
//! - **Commands**: status, reload, validate, ping, stop
//! - **Round-Trip Latency**: <1ms
//!
//! ## State Persistence
//!
//! - **Atomic Writes**: Uses tempfile + rename for crash safety
//! - **Checksums**: SHA256 validation for integrity
//! - **Emergency Saves**: Panic handler for graceful failures
//! - **Recovery**: Automatic state restoration on startup
//!
//! ## Lifecycle Management
//!
//! - **8-State Machine**: Init, Starting, Running, Reloading, Degraded, Reconnecting, Stopping, Stopped
//! - **Graceful Shutdown**: Proper resource cleanup
//! - **Health Monitoring**: Device connection tracking
//! - **Performance Metrics**: Reload latency, uptime, event counts
//!
//! # Performance Characteristics
//!
//! - **Config Reload**: 0-8ms (production configs: <3ms)
//! - **IPC Round-Trip**: <1ms
//! - **Build Time**: 26s clean, 4s incremental
//! - **Binary Size**: 3-5MB (release)
//! - **Memory Usage**: 5-10MB resident
//! - **Test Suite**: 0.24s execution time
//!
//! # Usage
//!
//! ## Starting the Daemon
//!
//! ```bash
//! # Foreground mode (development)
//! cargo run --release --bin conductor 2
//!
//! # Background service (production)
//! systemctl --user start conductor  # Linux
//! launchctl load ~/Library/LaunchAgents/media.monstrous.conductor.plist  # macOS
//! ```
//!
//! ## Controlling the Daemon
//!
//! ```bash
//! # Check status
//! conductorctl status
//!
//! # Hot-reload configuration
//! conductorctl reload
//!
//! # Validate config without reloading
//! conductorctl validate
//!
//! # Health check
//! conductorctl ping
//!
//! # Graceful shutdown
//! conductorctl stop
//! ```
//!
//! ## Programmatic Usage
//!
//! ```no_run
//! use conductor_daemon::{run_daemon, get_socket_path, IpcClient};
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // Start daemon in another task
//! tokio::spawn(async {
//!     run_daemon().await.expect("Daemon failed");
//! });
//!
//! // Wait for daemon to start
//! tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
//!
//! // Connect and send commands
//! let socket_path = get_socket_path().expect("Failed to get socket path");
//! let mut client = IpcClient::new(socket_path.to_string_lossy().to_string()).await?;
//!
//! // Get status
//! let response = client.status().await?;
//! println!("Daemon state: {:?}", response);
//!
//! // Reload config
//! let response = client.reload().await?;
//! println!("Reload result: {:?}", response);
//! # Ok(())
//! # }
//! ```
//!
//! # Modules
//!
//! - [`daemon::ipc`] - IPC server and client implementation
//! - [`daemon::config_watcher`] - File system monitoring for config hot-reload
//! - [`daemon::state`] - State persistence and recovery
//! - [`daemon::engine_manager`] - Core engine lifecycle management
//! - [`daemon::service`] - Main daemon service coordination
//! - [`daemon::types`] - Shared types and data structures
//! - [`daemon::error`] - Error types and handling

pub mod action_executor;
pub mod conditions;
pub mod connector_registry; // ADR-031 § 3.4 — signal routing graph runtime
pub mod daemon;
pub mod gamepad_device; // HID device management - Game Controllers (v3.0)
pub mod input_manager; // Unified MIDI + Gamepad input (v3.0)
pub mod input_source; // ADR-039 §4.1 — uniform inbound extension point (#1758)
pub mod listeners; // ADR-042 Phase A — network-listener edge (ACL/rate-limit/audit)
pub mod midi_bytes; // Wire-format MIDI reconstruction for MidiForward (#1119)
pub mod midi_device;
pub mod midi_template; // Shared {cc}/{value}/{note}/{velocity} substitution (ADR-038 R2 P5)
pub mod migration; // Comment-preserving config migrations (ADR-036 Slice 8)
pub mod osc_parser; // ADR-039-A Slice 1 — OSC datagram → OscInbound (#1361)
pub mod permissions; // TCC permission detection (ADR-029 §D3)
pub mod plugin_manager;
pub mod route_engine; // ADR-031 § 4.4 — signal routing graph runtime (Phase 2B)
pub mod security; // Capability/tier enforcement gate (ADR-027 D5)
pub mod shell_timeout; // ADR-027 D7 — shell action timeout enforcement (#1166)
pub mod skills; // Agent Skills validation (v4.11 - ADR-007)
pub mod transforms; // ADR-031 § 7 — cross-protocol transforms (Phase 5)

// Re-export core types for convenience
pub use daemon::{
    ConfigWatcher, DaemonCommand, DaemonError, DaemonInfo, DaemonService, DaemonStatistics,
    DeviceStatus, EngineInfo, EngineManager, ErrorDetails, ErrorEntry, EventFilter, EventStats,
    IpcClient, IpcCommand, IpcErrorCode, IpcRequest, IpcResponse, IpcServer, LifecycleState,
    MenuBarAction, MidiDeviceInfo, MonitorEvent, PersistedState, ReloadMetrics, ResponseStatus,
    Result, StateManager, calculate_checksum, create_success_response, get_socket_path,
    get_state_dir, run_daemon, run_daemon_with_config, run_daemon_with_identity,
};

// Re-export ActionExecutor, TriggerContext, and helpers for daemon use
pub use action_executor::{ActionExecutor, TriggerContext, derive_shell_argv, parse_command_line};

// Re-export condition evaluation for daemon use
pub use conditions::{ConditionContext, evaluate_condition};

// Re-export device managers for daemon use
pub use gamepad_device::GamepadDeviceManager; // Alias for backward compat (v3.0)
pub use gamepad_device::HidDeviceManager; // HID device manager (v3.0)
pub use input_manager::{InputManager, InputMode}; // Unified input (v3.0)
pub use input_source::{InputSource, InputSourceMetrics, InputSourceMetricsHandle}; // ADR-039 (#1758)
pub use midi_device::MidiDeviceManager;

// Re-export PluginManager for daemon use (v2.3)
pub use plugin_manager::{PluginManager, PluginManagerError, PluginManagerResult};

// Re-export skills module for Agent Skills (v4.11 - ADR-007)
pub use skills::{
    // P5-05: Sandbox types for user-provided skills
    SandboxConfig,
    SandboxError,
    SandboxResult,
    SkillMetadata,
    SkillSandbox,
    SkillValidationError,
    ToolAccessResult,
    ToolPattern,
    ValidatedSkill,
    get_skills_dir,
    install_skill,
    list_skills,
    validate_skill,
};
