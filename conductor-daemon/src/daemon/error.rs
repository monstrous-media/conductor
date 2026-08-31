// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Error types for daemon operations

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DaemonError {
    #[error("Configuration error: {0}")]
    Config(#[from] conductor_core::ConfigError),

    #[error("Engine error: {0}")]
    Engine(#[from] conductor_core::EngineError),

    #[error("IPC error: {0}")]
    Ipc(String),

    #[error("File watcher error: {0}")]
    FileWatcher(String),

    #[error("State persistence error: {0}")]
    StatePersistence(String),

    #[error("Service registration error: {0}")]
    ServiceRegistration(String),

    #[error("Menu bar error: {0}")]
    MenuBar(String),

    #[error("MCP server error: {0}")]
    Mcp(String),

    #[error("Invalid state transition: {from:?} → {to:?}")]
    InvalidStateTransition { from: String, to: String },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("TOML error: {0}")]
    Toml(#[from] toml::de::Error),

    #[error("Channel send error")]
    ChannelSend,

    #[error("Channel receive error")]
    ChannelRecv,

    #[error("Device not connected")]
    DeviceNotConnected,

    #[error("Timeout: {0}")]
    Timeout(String),

    #[error("Fatal error: {0}")]
    Fatal(String),

    /// Invalid path (non-UTF8 or otherwise unrepresentable)
    /// Added v4.14.0 (LLM Council feedback v4.13.3)
    #[error("Invalid path: {context} - path contains non-UTF8 characters")]
    InvalidPath { context: String },
}

pub type Result<T> = std::result::Result<T, DaemonError>;

/// IPC error codes for structured error responses
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[repr(u16)]
pub enum IpcErrorCode {
    // Protocol errors (1xxx)
    InvalidJson = 1001,
    MissingField = 1002,
    UnknownCommand = 1003,
    InvalidRequest = 1004,

    // Config errors (2xxx)
    ConfigNotFound = 2001,
    ConfigValidationFailed = 2002,
    ConfigParseFailed = 2003,

    // State errors (3xxx)
    InvalidState = 3001,
    DeviceNotConnected = 3002,

    // System errors (4xxx)
    InternalError = 4001,
    Timeout = 4002,

    // Security errors (5xxx) — ADR-027
    /// ADR-027 §D16 (#1168): per-peer IPC message rate limit
    /// exceeded. Returned to a peer that flooded past the
    /// 100-messages-per-second budget; the request is dropped
    /// but the connection stays open.
    RateLimitExceeded = 5001,

    /// ADR-034 §D2 / D4.C.1 (#1308): `base_generation` supplied by
    /// the caller is stale — another mutator advanced the live
    /// snapshot in the meantime. The caller should re-fetch via
    /// `GetConfigSnapshot`, re-validate their intent, and retry.
    /// Returned by `SaveConfig`, `ApplyPlan`, `ReloadFromDisk`,
    /// `ImportConfig`, `MarkKnownGood`, and `RollbackConfig` (not
    /// `RollbackConfigForce` which bypasses CAS per KI-A3).
    StaleBaseGeneration = 5002,

    /// ADR-034 §D2.3 / D4.C.1 (#1308): IPC payload exceeds the
    /// 256 KiB cap. Enforced **pre-deserialisation** at the IPC
    /// framer (before `serde_json::from_slice`) on `SaveConfig`
    /// and `ImportConfig` so a malformed-or-huge payload can't
    /// burn CPU on parse. The connection stays open.
    PayloadTooLarge = 5003,

    /// ADR-034 §D8.2 / D4.C.1 (#1308): the audit outbox is in
    /// `AuditDegraded` lifecycle state — 8+ consecutive flush
    /// failures broke the chain. ConfigChange mutations are
    /// rejected with this code until the operator runs
    /// `conductorctl audit resume` (after fixing the underlying
    /// I/O / quota / permission issue). ReadOnly IPCs still
    /// succeed.
    AuditUnavailable = 5004,

    /// ADR-034 §D4.2 / D4.B.3 — **RESERVED, not currently returned (#1323).**
    ///
    /// Was the rejection code for the `AwaitingConfig` idle mode (no live
    /// config loaded). That mode was never wired and is downgraded to
    /// reserved, so the daemon never emits this code today; the value is
    /// retained so the wire contract is stable if the mode is reinstated.
    DaemonAwaitingConfig = 5005,

    /// ADR-034 §D2.2 (#1902): a caller-supplied `ReloadFromDisk` /
    /// `ImportConfig` path failed the safe-walk path validation —
    /// it escaped the config-directory allowlist root, traversed a
    /// symlink in some component, was not a regular file, or was not
    /// owned by the daemon UID. The path is rejected **before** any
    /// content is read. A `PathValidationFailed` audit event records
    /// the attempted path and the coarse reason.
    PathValidationFailed = 5006,

    /// ADR-034 §D4 / ADR-043 (#2417, B2 completion): the caller-supplied
    /// `base_revision` (content hash of the snapshot the client displayed) no
    /// longer matches the daemon's current snapshot revision — a real content
    /// change landed between the client's read and its `SaveConfig`. The save is
    /// rejected **before** commit so it cannot clobber the intervening change;
    /// the client should re-fetch via `GetConfigBody` and re-apply (the §D4.D
    /// conflict banner). Distinct from `StaleBaseGeneration` — content-hash, not
    /// generation, so a daemon self-write that bumps the generation without
    /// changing content does **not** trip it.
    StaleBaseContent = 5007,
}

impl IpcErrorCode {
    pub fn as_u16(self) -> u16 {
        self as u16
    }

    pub fn message(&self) -> &'static str {
        match self {
            Self::InvalidJson => "Invalid JSON in request",
            Self::MissingField => "Required field missing in request",
            Self::UnknownCommand => "Unknown command",
            Self::InvalidRequest => "Invalid request (exceeds size limits or malformed)",
            Self::ConfigNotFound => "Configuration file not found",
            Self::ConfigValidationFailed => "Configuration validation failed",
            Self::ConfigParseFailed => "Failed to parse configuration",
            Self::InvalidState => "Invalid daemon state for this operation",
            Self::DeviceNotConnected => "MIDI device not connected",
            Self::InternalError => "Internal server error",
            Self::RateLimitExceeded => "Per-peer IPC message rate limit exceeded",
            Self::StaleBaseGeneration => {
                "base_generation is stale — re-fetch via GetConfigSnapshot and retry"
            }
            Self::PayloadTooLarge => "IPC payload exceeds 256 KiB cap",
            Self::AuditUnavailable => {
                "Audit outbox in AuditDegraded state — \
                 run `conductorctl audit resume` after fixing the \
                 underlying flush failure"
            }
            Self::DaemonAwaitingConfig => {
                // RESERVED (#1323): the AwaitingConfig idle mode is unwired,
                // so this code is never returned today.
                "Daemon is in AwaitingConfig idle mode (reserved)"
            }
            Self::PathValidationFailed => {
                "Config path failed safe-walk validation — it must be a \
                 regular .toml file beneath the config directory, owned by \
                 the daemon user, reached without traversing any symlink"
            }
            Self::StaleBaseContent => {
                "base_revision is stale — the config changed since you loaded it; \
                 re-fetch via GetConfigBody and re-apply your change"
            }
            Self::Timeout => "Operation timed out",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR-034 §D2 / D4.C.1 (#1308): the three new 5xxx error codes
    /// pin their numeric values so wire-format consumers (GUI plan
    /// re-apply, CLI exit codes, audit log) can match by `code`
    /// without ambiguity. 5005 was added in D4.B.3 (#1290) and is
    /// re-asserted here as the regression guard.
    #[test]
    fn d4c1_security_error_codes_have_stable_numeric_values() {
        assert_eq!(IpcErrorCode::RateLimitExceeded as u16, 5001);
        assert_eq!(IpcErrorCode::StaleBaseGeneration as u16, 5002);
        assert_eq!(IpcErrorCode::PayloadTooLarge as u16, 5003);
        assert_eq!(IpcErrorCode::AuditUnavailable as u16, 5004);
        assert_eq!(IpcErrorCode::DaemonAwaitingConfig as u16, 5005);
        assert_eq!(IpcErrorCode::PathValidationFailed as u16, 5006);
        assert_eq!(IpcErrorCode::StaleBaseContent as u16, 5007);
    }

    /// The 5xxx range is reserved for ADR-027 security errors;
    /// `as_u16()` is what the wire format carries, and it must
    /// match the discriminant. (`#[repr(u16)]` makes this true
    /// by definition, but the test pins it so a future refactor
    /// to a string-tagged enum doesn't silently change wire shape.)
    #[test]
    fn as_u16_matches_repr_for_d4c1_codes() {
        for code in [
            IpcErrorCode::StaleBaseGeneration,
            IpcErrorCode::PayloadTooLarge,
            IpcErrorCode::AuditUnavailable,
        ] {
            assert_eq!(code.as_u16(), code as u16);
        }
    }

    /// Messages must be non-empty and mention the recovery action.
    /// Operator-facing surface that callers route into IPC error
    /// responses; an empty or "TODO" string is a regression.
    #[test]
    fn d4c1_error_messages_explain_recovery() {
        let stale = IpcErrorCode::StaleBaseGeneration.message();
        assert!(stale.contains("GetConfigSnapshot"), "got: {stale}");

        let payload = IpcErrorCode::PayloadTooLarge.message();
        assert!(payload.contains("256 KiB"), "got: {payload}");

        let audit = IpcErrorCode::AuditUnavailable.message();
        assert!(audit.contains("conductorctl audit resume"), "got: {audit}");
    }

    /// v4.14.0: Test InvalidPath error message format
    /// (LLM Council feedback v4.13.3)
    #[test]
    fn test_invalid_path_error_message_format() {
        let err = DaemonError::InvalidPath {
            context: "config_path in reload_config".to_string(),
        };

        let msg = err.to_string();
        assert!(
            msg.contains("config_path in reload_config"),
            "Error message should contain the context: {}",
            msg
        );
        assert!(
            msg.contains("non-UTF8"),
            "Error message should mention non-UTF8: {}",
            msg
        );
    }

    #[test]
    fn test_invalid_path_error_debug() {
        let err = DaemonError::InvalidPath {
            context: "test context".to_string(),
        };

        let debug_str = format!("{:?}", err);
        assert!(
            debug_str.contains("InvalidPath"),
            "Debug should show variant name"
        );
        assert!(
            debug_str.contains("test context"),
            "Debug should show context"
        );
    }
}
