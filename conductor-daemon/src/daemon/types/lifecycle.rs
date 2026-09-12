// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Daemon lifecycle state and the active-profile record.

use super::*;

/// Active profile information (Phase 1)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveProfileInfo {
    /// The GUI's profile id (`profile-<timestamp>`), when the switch carried
    /// one (additive). `None` for the built-in Default and for
    /// legacy callers that only send name + path.
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    pub config_path: String,
}

/// Daemon lifecycle states
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LifecycleState {
    /// Initial state, loading configuration and connecting to devices
    Init,

    /// ADR-034 §D4.2 / D4.B.3 — **RESERVED, not reachable today.**
    ///
    /// Was intended as a startup idle mode: when no live config is found
    /// (neither `live.toml` nor `live.toml.known_good` parsed), the daemon
    /// would stay up and accept a small bootstrap IPC accept-list so an
    /// operator could bootstrap one over IPC. That boot path was never
    /// wired — nothing transitions the daemon into this state — so the
    /// contract was unreachable and is downgraded to reserved. A fresh
    /// install with no resolvable config now exits with a descriptive error
    /// (see `main.rs`) instead. The variant, its
    /// transitions, `IpcCommand::allowed_during_awaiting_config`, and
    /// `IpcErrorCode::DaemonAwaitingConfig` are retained as scaffolding so
    /// the mode can be reinstated without a wire-format break if
    /// headless/zero-touch provisioning becomes a real goal.
    AwaitingConfig,

    /// Starting up, initializing all components
    Starting,

    /// Running normally, processing events
    Running,

    /// Reloading configuration
    Reloading,

    /// Device disconnected, attempting to reconnect
    Degraded,

    /// ADR-034 §D8.2 / D4.C.1 — audit outbox has hit 8+
    /// consecutive flush failures and broken its hash chain.
    /// ConfigChange mutations reject with
    /// `IpcErrorCode::AuditUnavailable = 5004` until the operator
    /// runs `conductorctl audit resume`. ReadOnly IPCs still
    /// succeed — the daemon is functionally up, just unable to
    /// durably attest mutations. Distinct from `Degraded` (which
    /// is device-level): a daemon can be `AuditDegraded` while
    /// connected to all devices and vice versa. Transitions:
    /// `Running ↔ AuditDegraded`; either may go to `Stopping`.
    AuditDegraded,

    /// Attempting to reconnect to device
    Reconnecting,

    /// Shutting down gracefully
    Stopping,

    /// Stopped, daemon has exited
    Stopped,
}

impl std::fmt::Display for LifecycleState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Init => write!(f, "Init"),
            Self::AwaitingConfig => write!(f, "AwaitingConfig"),
            Self::Starting => write!(f, "Starting"),
            Self::Running => write!(f, "Running"),
            Self::Reloading => write!(f, "Reloading"),
            Self::Degraded => write!(f, "Degraded"),
            Self::AuditDegraded => write!(f, "AuditDegraded"),
            Self::Reconnecting => write!(f, "Reconnecting"),
            Self::Stopping => write!(f, "Stopping"),
            Self::Stopped => write!(f, "Stopped"),
        }
    }
}

impl LifecycleState {
    /// Check if a state transition is valid
    pub fn can_transition_to(&self, new_state: Self) -> bool {
        matches!(
            (self, new_state),
            (Self::Init, Self::Starting)
                // D4.B.3 — RESERVED: the AwaitingConfig idle mode
                // was never wired, so these edges are unreachable today.
                // Retained so the mode can be reinstated without a
                // transition-table change. (Would-be routing: startup load
                // failure → AwaitingConfig; `Init { source }` →
                // Starting; SIGTERM → Stopping.)
                | (Self::Init, Self::AwaitingConfig)
                | (Self::AwaitingConfig, Self::Starting)
                | (Self::AwaitingConfig, Self::Stopping)
                | (Self::Starting, Self::Running)
                | (Self::Starting, Self::Degraded) // Allow Starting → Degraded when device connection fails
                | (Self::Starting, Self::Stopping) // Allow Starting → Stopping for clean shutdown during startup
                | (Self::Running, Self::Reloading)
                | (Self::Running, Self::Degraded)
                | (Self::Running, Self::Stopping)
                | (Self::Reloading, Self::Running)
                | (Self::Reloading, Self::Degraded)
                | (Self::Reloading, Self::Reconnecting) // Device lost during config reload
                | (Self::Reloading, Self::Stopping) // Clean shutdown during config reload
                | (Self::Degraded, Self::Reconnecting)
                | (Self::Degraded, Self::Stopping)
                // D4.C.1: audit outbox lifecycle. Running
                // demotes to AuditDegraded on 8+ consecutive flush
                // failures; resumes back to Running after operator
                // runs `conductorctl audit resume`. Always allowed
                // to Stopping for clean shutdown from either state.
                | (Self::Running, Self::AuditDegraded)
                | (Self::AuditDegraded, Self::Running)
                | (Self::AuditDegraded, Self::Stopping)
                | (Self::Reconnecting, Self::Running)
                | (Self::Reconnecting, Self::Degraded)
                | (Self::Reconnecting, Self::Stopping) // Clean shutdown during reconnect
                | (Self::Stopping, Self::Stopped)
        )
    }
}
