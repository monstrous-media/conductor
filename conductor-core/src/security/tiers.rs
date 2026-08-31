// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-027 D5 — risk-tier classification for any dispatchable action,
//! MCP tool, or IPC command.
//!
//! Originally lived as `ToolRiskTier` in
//! `conductor-daemon/src/daemon/mcp_types.rs`. Moved here as the canonical
//! home so `conductor-gui` (Tauri) can reference the same enum without
//! reaching across the crate split, and so the upcoming D5 gate
//! (`conductor-daemon::security::gate`) can take a `RiskTier` directly.
//! `mcp_types.rs` retains a `pub use … as ToolRiskTier` alias for the
//! ~7 daemon files that already import the old name.
//!
//! # `#[non_exhaustive]` — deliberately omitted
//!
//! ADR-027 §2.1 of the implementation spec recommends `#[non_exhaustive]`
//! on this enum. We deviate: leaving it off means adding a new variant
//! produces a compile error at every `match` site, forcing each
//! security-relevant decision (rate-limit weights, audit-tier mapping,
//! gate policy, MCP tool dispatch) to be re-reviewed. For a security
//! tier enum that's the *desired* failure mode — silently defaulting a
//! new tier into an existing branch via a `_` arm is exactly what we
//! want to make impossible. If a future external consumer needs the
//! attribute, we revisit; for now the strictness is load-bearing.

use serde::{Deserialize, Serialize};

/// Risk tier for any dispatchable surface — MCP tools, IPC commands,
/// `[[modes.mappings]]` action variants. Used by the D5 gate to classify
/// a request and select the applicable policy (auto-execute, plan-and-apply,
/// hardware-confirmation, deny).
///
/// ## Tiers
///
/// - [`ReadOnly`](Self::ReadOnly) — pure inspection. No state mutation, no
///   side effects, no hardware emission. Auto-execute.
/// - [`Stateful`](Self::Stateful) — session-scoped, in-memory mutation.
///   Reversible without persistence (e.g. mode switching, MIDI Learn
///   start/stop, mapping-editor staging). Auto-execute.
/// - [`ArtifactRender`](Self::ArtifactRender) — projects an artifact into
///   the workspace UI (#612, #621). Stateful in spirit but distinct
///   audit / undo semantics; carved out so the GUI can treat it
///   uniformly.
/// - [`ConfigChange`](Self::ConfigChange) — persistent config mutation.
///   Requires Plan/Apply cycle so the user sees a diff before commit.
/// - [`HardwareIO`](Self::HardwareIO) — emits MIDI / OSC / SysEx to a
///   physical device. Requires confirmation step (vendor-specific reset
///   commands can hard-brick devices).
/// - [`Privileged`](Self::Privileged) — explicit confirmation required;
///   reserved for operations whose blast radius extends beyond the
///   daemon process or its devices (e.g. shell exec under D3, system
///   keystroke macros that the keystroke deny-list doesn't cover).
///
/// ## Wire format and stability
///
/// Variants serialise as their default-serde names (`"ReadOnly"`,
/// `"Stateful"`, …) when this enum reaches a wire format directly —
/// today that's IPC messages and any JSON the daemon emits to MCP
/// clients. The variants do NOT appear verbatim in the audit-log
/// SQLite store: rows there persist `AuditRiskTier`, a separate
/// companion enum with snake-case serde naming. The conversion
/// happens at the audit-log boundary in
/// `conductor-daemon::daemon::llm::executor::tool_risk_to_audit_risk`,
/// which collapses some variants:
///
/// - [`Stateful`](Self::Stateful) and [`ArtifactRender`](Self::ArtifactRender)
///   both become `AuditRiskTier::Stateful`.
/// - [`Privileged`](Self::Privileged) becomes `AuditRiskTier::Internal`
///   (the audit enum doesn't carry a `Privileged` variant; `Internal`
///   captures "operations beyond a normal tool surface").
/// - The other four variants map 1:1.
///
/// Renaming a `RiskTier` variant is a wire-format break for IPC /
/// MCP and may also require touching the conversion above. The
/// audit-DB schema doesn't notice unless the conversion target name
/// also changes — that's a separate migration concern owned by
/// `AuditRiskTier`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RiskTier {
    ReadOnly,
    Stateful,
    ArtifactRender,
    ConfigChange,
    HardwareIO,
    Privileged,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Variants must serialise as their PascalCase names — this is what
    /// `ToolRiskTier` did before the move and what the audit / IPC
    /// wire formats already persist. Changing the encoding here is a
    /// silent break for any caller that has stored or transmitted these
    /// strings, so we pin every variant explicitly.
    #[test]
    fn serialises_as_pascal_case_variant_names() {
        let cases = [
            (RiskTier::ReadOnly, "\"ReadOnly\""),
            (RiskTier::Stateful, "\"Stateful\""),
            (RiskTier::ArtifactRender, "\"ArtifactRender\""),
            (RiskTier::ConfigChange, "\"ConfigChange\""),
            (RiskTier::HardwareIO, "\"HardwareIO\""),
            (RiskTier::Privileged, "\"Privileged\""),
        ];
        for (tier, expected) in cases {
            let json = serde_json::to_string(&tier).expect("serialise");
            assert_eq!(
                json, expected,
                "RiskTier::{tier:?} must serialise as {expected}; got {json}",
            );
        }
    }

    #[test]
    fn round_trips_through_json() {
        for tier in [
            RiskTier::ReadOnly,
            RiskTier::Stateful,
            RiskTier::ArtifactRender,
            RiskTier::ConfigChange,
            RiskTier::HardwareIO,
            RiskTier::Privileged,
        ] {
            let json = serde_json::to_string(&tier).unwrap();
            let parsed: RiskTier = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, tier);
        }
    }

    #[test]
    fn variants_are_copy_and_hash_for_use_as_map_key() {
        // The D5 gate keeps per-tier rate-limiter buckets and per-tier
        // metric counters keyed on `RiskTier`. Pin the trait derives so
        // a refactor that drops `Copy` or `Hash` fails compilation
        // before it reaches the gate's data structures.
        fn assert_traits<T: Copy + std::hash::Hash + Eq>() {}
        assert_traits::<RiskTier>();
    }
}
