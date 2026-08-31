// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Action dispatch result types (v4.25.0 - ADR-009 Gap 1)
//!
//! Provides structured return types for action execution, enabling the
//! engine manager to handle mode changes and errors appropriately instead
//! of relying on side effects (eprintln) in the executor.

use crate::actions::Action;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Result type for action dispatch
pub type DispatchResult = Result<DispatchOutcome, DispatchError>;

/// Envelope wrapping an action with provenance metadata (v4.26.0 - ADR-009 Gap K)
///
/// Tracks which rule matched, the device that triggered it, and the active mode,
/// enabling richer debug logging and diagnostics in the engine manager.
#[derive(Debug, Clone)]
pub struct ActionEnvelope {
    /// The action to execute
    pub action: Action,
    /// Device ID that triggered the event (if multi-device)
    pub device_id: Option<String>,
    /// Description from the matched CompiledRule
    pub matched_rule: Option<String>,
    /// Name of the active mode when the rule matched
    pub mode_name: Option<String>,
    /// ADR-038: whether the matched rule asked to let the event continue to the
    /// route stage after firing the action. Metadata copied from the winning
    /// `CompiledRule`; consumed at the event pump's route-disposition gate
    /// (Slice 3). Defaults to `false` (swallow) for envelopes not built from a
    /// rule match.
    pub let_through: bool,
    /// ADR-038: positional index of the winning mapping within its source list,
    /// for the dispatch trace (Slice 4). `None` when the envelope was not built
    /// from a rule match (e.g. constructed directly in tests). Positionally
    /// bound — see [`crate::rule_set::CompiledRule::mapping_id`].
    pub mapping_id: Option<usize>,
    /// ADR-042 D17 (Slice A.6.6) — network-origin taint. `Some(listener_alias)`
    /// when this action originates from a network listener (OSC/Art-Net,
    /// **including loopback**); `None` for MIDI/gamepad origins. **Copied, never
    /// cleared**, into envelopes derived from this one (sequence/plugin/
    /// mode-change/timer children) so the action-class gate sees the effective
    /// taint, closing the confused-deputy bypass. The dispatch-time gate refuses
    /// a sensitive action class (`Shell`/`Launch`/`Keystroke`) from a tainted
    /// origin unless that listener set `allow_sensitive_actions = true`.
    pub network_origin: Option<String>,
}

/// Successful outcome of dispatching an action
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchOutcome {
    /// Action completed normally
    Completed,
    /// Action requests a mode change (caller must update ArcSwap<ModeState>)
    ModeChangeRequested { mode: String },
    /// Action was cancelled (shutdown requested during execution) (ADR-015 D11/D13)
    Cancelled,
}

/// Error during action dispatch
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchError {
    /// MIDI output error (port not found, send failed, etc.)
    MidiOutput(String),
    /// Plugin execution error
    Plugin(String),
    /// OS automation error (keyboard/mouse simulation)
    OsAutomation(String),
    /// Target device/port not bound
    TargetNotBound(String),
    /// OSC send error (network, encoding, etc.)
    OscSend(String),
    /// `Launch` action failed (app not found, sandbox denied spawn,
    /// binary not on PATH, `open` exited non-zero, etc.). Distinct from
    /// `OsAutomation` because the failure path goes through
    /// `Command::spawn`/`status` rather than enigo, and the diagnostic
    /// information is different (#938).
    Launch(String),
    /// `Shell` action failed before a process started: missing binary
    /// (ENOENT), permission denied, unparseable/empty command, or
    /// resource exhaustion at spawn. Parallels `Launch` (#1479) — the
    /// failure path is `Command::spawn` and previously a spawn error was
    /// only logged, so the action falsely reported as completed.
    Shell(String),
    /// ADR-042 D17 (Slice A.6.6): a network-origin action was refused by the
    /// action-class gate — a sensitive class (`Shell`/`Launch`/`Keystroke`) from
    /// the named network listener that did not set `allow_sensitive_actions`.
    /// `listener` is the origin alias (recorded as `NetworkActionClassBlocked`).
    NetworkActionClassBlocked { listener: String },
    /// ADR-027 §D10a — a `Shell` action was refused by the persistence write
    /// veto: its resolved argv attempted to write one of the daemon's own
    /// protected state directories (`~/.conductor/`, the macOS
    /// Application-Support dir, or the XDG data dir). The action is rejected
    /// before spawn, so it never reaches `execve`. `matched_pattern` names the
    /// heuristic that fired (e.g. `tee`, `redirect >`), recorded as the
    /// `ShellVetoedByPersistenceCheck` audit event.
    ShellVetoedByPersistenceCheck { matched_pattern: String },
    /// ADR-027 §D10b — a `Shell` action could not be OS-sandboxed (platform
    /// lacks a sandbox, or the kernel lacks Landlock) and
    /// `security.shell.allow_unsandboxed = false`, so the daemon failed closed
    /// and refused to spawn it. `reason` explains why confinement was
    /// unavailable. Recorded as the `ShellSandboxUnavailable` audit event.
    ShellSandboxUnavailable { reason: String },
}

impl fmt::Display for DispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DispatchError::MidiOutput(msg) => write!(f, "MIDI output error: {}", msg),
            DispatchError::Plugin(msg) => write!(f, "Plugin error: {}", msg),
            DispatchError::OsAutomation(msg) => write!(f, "OS automation error: {}", msg),
            DispatchError::TargetNotBound(msg) => write!(f, "Target not bound: {}", msg),
            DispatchError::OscSend(msg) => write!(f, "OSC send error: {}", msg),
            DispatchError::Launch(msg) => write!(f, "App launch error: {}", msg),
            DispatchError::Shell(msg) => write!(f, "Shell execution error: {}", msg),
            DispatchError::NetworkActionClassBlocked { listener } => write!(
                f,
                "network listener '{}' refused a sensitive action (set allow_sensitive_actions to permit)",
                listener
            ),
            DispatchError::ShellVetoedByPersistenceCheck { matched_pattern } => write!(
                f,
                "shell action refused: it would write a protected daemon state directory \
                 (matched `{}`). To change Conductor's own config/state, use the IPC API \
                 (e.g. `conductorctl`) instead of a shell action.",
                matched_pattern
            ),
            DispatchError::ShellSandboxUnavailable { reason } => write!(
                f,
                "shell action refused: could not OS-sandbox it ({}) and \
                 security.shell.allow_unsandboxed = false. Set \
                 security.shell.allow_unsandboxed = true to permit unsandboxed \
                 execution on this platform.",
                reason
            ),
        }
    }
}

impl std::error::Error for DispatchError {}

/// Options for simulating a mapping execution (ADR-014 Phase 5A)
///
/// Specifies which mapping to simulate and whether to actually execute the action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulateOptions {
    /// Mode name (use `"__global__"` for global mappings)
    pub mode: String,
    /// Zero-based mapping index within the mode or global_mappings
    pub index: usize,
    /// Whether to actually execute the action (true) or dry-run (false)
    #[serde(default)]
    pub execute: bool,
    /// Optional velocity/value override (0-127). If None, uses sensible defaults
    /// (100 for notes, 64 for CC/encoder).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<u8>,
}

/// Sentinel value indicating global mappings
pub const GLOBAL_MODE_SENTINEL: &str = "__global__";

/// Result of a simulate_mapping call (ADR-014 Phase 5A)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulateResult {
    /// The mode name
    pub mode: String,
    /// The mapping index
    pub index: usize,
    /// Human-readable action summary (from summarize_action)
    pub action_summary: String,
    /// Whether the action was actually executed
    pub executed: bool,
    /// Dispatch outcome (if executed)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// Dispatch error (if executed and failed)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Processing latency in microseconds
    #[serde(default)]
    pub latency_us: u64,
}

/// Errors from simulate_mapping (ADR-014 Phase 5A)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SimulateError {
    /// The requested mode does not exist in the config
    ModeNotFound(String),
    /// The mapping index is out of bounds
    MappingIndexOutOfBounds {
        mode: String,
        index: usize,
        count: usize,
    },
}

impl fmt::Display for SimulateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SimulateError::ModeNotFound(mode) => write!(f, "Mode not found: {}", mode),
            SimulateError::MappingIndexOutOfBounds { mode, index, count } => {
                write!(
                    f,
                    "Mapping index {} out of bounds for mode '{}' (has {} mappings)",
                    index, mode, count
                )
            }
        }
    }
}

impl std::error::Error for SimulateError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dispatch_outcome_completed() {
        let outcome = DispatchOutcome::Completed;
        assert_eq!(outcome, DispatchOutcome::Completed);
    }

    #[test]
    fn test_dispatch_outcome_mode_change() {
        let outcome = DispatchOutcome::ModeChangeRequested {
            mode: "DJ".to_string(),
        };
        assert!(matches!(outcome, DispatchOutcome::ModeChangeRequested { mode } if mode == "DJ"));
    }

    #[test]
    fn test_dispatch_error_display() {
        let err = DispatchError::MidiOutput("port not found".to_string());
        assert_eq!(format!("{}", err), "MIDI output error: port not found");

        let err = DispatchError::Plugin("load failed".to_string());
        assert_eq!(format!("{}", err), "Plugin error: load failed");

        let err = DispatchError::OsAutomation("key sim failed".to_string());
        assert_eq!(format!("{}", err), "OS automation error: key sim failed");

        let err = DispatchError::TargetNotBound("synth".to_string());
        assert_eq!(format!("{}", err), "Target not bound: synth");

        let err = DispatchError::OscSend("connection refused".to_string());
        assert_eq!(format!("{}", err), "OSC send error: connection refused");

        // #938 / #982 review 3169357558: cover the new Launch variant
        // in the Display test matrix so future renames/changes can't
        // silently regress the user-visible error wording.
        let err = DispatchError::Launch("'open -a Calculator' exited with status 1".to_string());
        assert_eq!(
            format!("{}", err),
            "App launch error: 'open -a Calculator' exited with status 1"
        );
    }

    #[test]
    fn test_dispatch_result_ok() {
        let result: DispatchResult = Ok(DispatchOutcome::Completed);
        assert!(result.is_ok());
    }

    #[test]
    fn test_dispatch_result_err() {
        let result: DispatchResult = Err(DispatchError::MidiOutput("fail".to_string()));
        assert!(result.is_err());
    }

    #[test]
    fn test_action_envelope_full_provenance() {
        let envelope = ActionEnvelope {
            action: Action::ModeChange {
                mode: "DJ".to_string(),
            },
            device_id: Some("mikro".to_string()),
            matched_rule: Some("Note 36 -> ModeChange DJ".to_string()),
            mode_name: Some("Default".to_string()),
            let_through: false,
            mapping_id: None,
            network_origin: None,
        };
        assert_eq!(envelope.device_id.as_deref(), Some("mikro"));
        assert_eq!(
            envelope.matched_rule.as_deref(),
            Some("Note 36 -> ModeChange DJ")
        );
        assert_eq!(envelope.mode_name.as_deref(), Some("Default"));
        assert!(matches!(envelope.action, Action::ModeChange { mode } if mode == "DJ"));
    }

    #[test]
    fn test_action_envelope_minimal_provenance() {
        let envelope = ActionEnvelope {
            action: Action::Text("hello".to_string()),
            device_id: None,
            matched_rule: None,
            mode_name: None,
            let_through: false,
            mapping_id: None,
            network_origin: None,
        };
        assert!(envelope.device_id.is_none());
        assert!(envelope.matched_rule.is_none());
        assert!(envelope.mode_name.is_none());
    }

    #[test]
    fn test_osc_send_dispatch_error_display() {
        let err = DispatchError::OscSend("bind error".to_string());
        assert_eq!(format!("{}", err), "OSC send error: bind error");
    }

    // =============================================================================
    // SimulateOptions / SimulateResult / SimulateError Tests (ADR-014 Phase 5A)
    // =============================================================================

    #[test]
    fn test_simulate_options_serialization() {
        let opts = SimulateOptions {
            mode: "Mix".to_string(),
            index: 2,
            execute: true,
            value: Some(127),
        };
        let json = serde_json::to_value(&opts).unwrap();
        assert_eq!(json["mode"], "Mix");
        assert_eq!(json["index"], 2);
        assert_eq!(json["execute"], true);
        assert_eq!(json["value"], 127);
    }

    #[test]
    fn test_simulate_options_deserialization_defaults() {
        // execute and value default when omitted
        let json = r#"{"mode": "Mix", "index": 0}"#;
        let opts: SimulateOptions = serde_json::from_str(json).unwrap();
        assert_eq!(opts.mode, "Mix");
        assert_eq!(opts.index, 0);
        assert!(!opts.execute);
        assert!(opts.value.is_none());
    }

    #[test]
    fn test_simulate_result_serialization() {
        let result = SimulateResult {
            mode: "Mix".to_string(),
            index: 0,
            action_summary: "Cmd+C".to_string(),
            executed: true,
            outcome: Some("completed".to_string()),
            error: None,
            latency_us: 250,
        };
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["action_summary"], "Cmd+C");
        assert_eq!(json["executed"], true);
        assert_eq!(json["outcome"], "completed");
        assert_eq!(json["latency_us"], 250);
        // error should be absent (skip_serializing_if = None)
        assert!(json.get("error").is_none() || json["error"].is_null());
    }

    #[test]
    fn test_simulate_error_display() {
        let err = SimulateError::ModeNotFound("DJ".to_string());
        assert_eq!(format!("{}", err), "Mode not found: DJ");

        let err = SimulateError::MappingIndexOutOfBounds {
            mode: "Mix".to_string(),
            index: 5,
            count: 3,
        };
        assert_eq!(
            format!("{}", err),
            "Mapping index 5 out of bounds for mode 'Mix' (has 3 mappings)"
        );
    }

    #[test]
    fn test_global_mode_sentinel() {
        assert_eq!(GLOBAL_MODE_SENTINEL, "__global__");
    }
}
