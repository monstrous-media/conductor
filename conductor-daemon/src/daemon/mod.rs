// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Daemon infrastructure for background service operation

pub mod active_profile_persist; // daemon-owned durable active-profile identity
pub mod app_detector; // Automatic profile switching by app
pub mod audit; // P4-04: Comprehensive audit logging
pub mod config_watcher;
pub mod connection_limiter; // ADR-027 D16: global concurrent-connection cap for IPC
pub mod device_rate_limiter; // D9: Per-device event rate limiting
pub mod device_utils; // Shared MIDI device enumeration
pub mod dispatch_trace; // ADR-036 §8: bounded route-dispatch trace ring buffer
pub mod engine_manager;
pub mod error;
pub mod event_triggers; // R915-R917: Event-based triggers and notifications
pub mod executor_thread; // ADR-015: Dedicated action execution thread
pub mod gui_handshake; // ADR-027 D19: GUI launch handshake nonce registry
pub mod hardware_io; // P4-01: HardwareIO tier with multi-step confirmation
pub mod ipc;
pub mod ipc_framing; // bounded line-read helper for IPC DoS prevention
pub mod ipc_rate_limit; // ADR-027 D16 (full): per-peer IPC message rate limit
pub mod keystroke_policy; // ADR-027 D8: keystroke policy mode
pub mod live_config; // ADR-034 §D1 — daemon-managed live config seam
pub mod mcp_registry; // ADR-027 D18: MCP client registration
// Knowledge Layer migrated to conductor-knowledge crate (ADR-023)
// ADR-045 D1: tier-boundary feature gates. `llm` is the IPC-only
// write machinery (`llm-executor`); `mcp` is the socket server; the tool
// catalog + shared mode helpers compile whenever either side needs them
// (the LLM executor embeds `McpToolExecutor`). `mcp_types` stays ungated:
// pure data types, also used by ratelimit.
#[cfg(feature = "llm-executor")]
pub mod llm; // LLM integration Phase 2 (ADR-007)
#[cfg(feature = "mcp")]
pub mod mcp;
#[cfg(any(feature = "mcp", feature = "llm-executor"))]
pub mod mcp_tools;
pub mod mcp_types;
// macOS-only: the tray stack (tray-icon) is a macOS target dependency —
// the Linux daemon is headless (see conductor_menubar.rs).
#[cfg(target_os = "macos")]
pub mod menu_bar;
pub mod midi_watcher; // Persistent CoreMIDI watcher for daemon hot-plug
#[cfg(any(feature = "mcp", feature = "llm-executor"))]
pub mod mode_mcp; // ADR-040 D4 §4.2: shared exec for mode-lock MCP tools
pub mod mode_resolver; // ADR-040 D4: pure mode precedence stack (lock>window>app>default)
pub mod output_resolver; // ADR-021 Phase 1B: Output port enumeration and auto-pairing
pub mod path_validation; // ADR-034 §D2.2 — safe-walk path validation for ReloadFromDisk/ImportConfig
pub mod platform; // ADR-040 §4.3: OS integration — focused-window-title detection
pub mod probe_on_connect; // ADR-026 Phase 3.C.2: SysEx identity probe-on-connect orchestration
pub mod profile_cache; // Profile cache for fast switching
pub mod ratelimit; // P4-05: Rate limiting per tier
pub mod recursion_guard; // ADR-015 D8: MIDI echo suppression
pub mod service;
#[cfg(unix)]
pub mod singleton_lock; // ADR-034 §D10 / §D4.B.2: flock(2) singleton enforcement
pub mod startup; // honour profiles.json active_profile_id at daemon launch
pub mod state;
pub mod suppression_throttle; // coalesce midi_*_suppressed MonitorEvents
pub mod types;

pub use audit::{AuditEntry, AuditEventType, AuditQuery, AuditRiskTier, AuditSummary, UserContext};
// ADR-045 D1: the SQLite-backed logger only exists under `audit-db`;
// the audit *types* above stay ungated (conductorctl + tier ceiling use them).
#[cfg(feature = "audit-db")]
pub use audit::{AuditLogger, AuditLoggerConfig};
pub use config_watcher::ConfigWatcher;
pub use engine_manager::EngineManager;
pub use error::{DaemonError, IpcErrorCode, Result};
pub use hardware_io::{
    ConfirmationManager, ConfirmationRequest, ConfirmationStatus, ConfirmationToken,
    HardwareIoError, SysExCategory, SysExValidation, SysExValidator,
};
pub use ipc::{IpcClient, IpcServer, create_success_response};
#[cfg(feature = "llm-executor")]
pub use llm::{ConfigChange, ConfigPlan, PlanError, ToolExecutor};
#[cfg(feature = "mcp")]
pub use mcp::{McpServer, get_mcp_socket_path};
#[cfg(any(feature = "mcp", feature = "llm-executor"))]
pub use mcp_tools::{McpToolExecutor, get_tool_definitions, get_tool_risk_tier};
pub use mcp_types::{
    InitializeResult, McpError, McpRequest, McpRequestId, McpResponse, ServerCapabilities,
    ServerInfo, ToolCallParams, ToolCallResult, ToolContent, ToolDefinition, ToolRiskTier,
    ToolsCapability, ToolsListResult,
};
#[cfg(target_os = "macos")]
pub use menu_bar::{IconState, MenuAction, MenuBar, MenuBarError};
pub use midi_watcher::{MidiWatcherHandle, start_midi_watcher};
pub use ratelimit::{RateLimitConfig, RateLimitError, RateLimitResult, RateLimiter, TierLimits};
pub use service::{DaemonService, run_daemon, run_daemon_with_config, run_daemon_with_identity};
pub use state::{
    ConfigInfo, DaemonInfo, EngineInfo, PersistedState, StateManager, calculate_checksum,
    get_socket_path, get_state_dir,
};
pub use types::{
    DaemonCommand, DaemonStatistics, DeviceStatus, ErrorDetails, ErrorEntry, EventFilter,
    EventStats, IpcCommand, IpcRequest, IpcResponse, LifecycleState, MenuBarAction, MidiDeviceInfo,
    MonitorEvent, ReloadMetrics, ResponseStatus,
};
