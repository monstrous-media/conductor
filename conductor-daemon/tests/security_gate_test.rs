// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Tests for the ADR-027 D5 security gate.
//!
//! Phase 1A is an atomic bundle (D5 + D3 + D1) per spec §2 line 100–104. The
//! gate runs in **shadow mode** by default until D3 (capability declarations)
//! and D1 (peer-credential IPC auth) are also in tree — when shadow mode is
//! on, every decision is overridden to [`GateDecision::Allow`] so callers can
//! be wired through the gate without changing runtime behaviour.
//!
//! These tests exercise both halves of the contract:
//!
//! 1. **Shadow-mode envelope** (default). Every `(RiskTier, TrustLevel)` pair
//!    resolves to `Allow`. Pre-existing tests from D5 (2/N) survive
//!    unchanged — the (3/N) decision-table only takes effect when shadow
//!    mode is explicitly disabled.
//!
//! 2. **Decision table** (D5 3/N). With `shadow_mode = false`, the gate
//!    returns the per-tier outcomes specified in §2.2 + §2.5 of the spec:
//!    `ReadOnly`/`Stateful`/`ArtifactRender` → `Allow`,
//!    `ConfigChange` → `RequirePlan(GatePlanRequest)`,
//!    `HardwareIO` → `RequireConfirmation(GateConfirmationRequest)`
//!    (or `Deny` for `Untrusted`),
//!    `Privileged` → `Deny` (any caller).
//!
//! All gate hand-off types (`GatePlanRequest`, `GateConfirmationRequest`,
//! `CallerContext`, `SecurityPolicy`, `DenialReason`, and the latter's
//! struct-like variants) are `#[non_exhaustive]` so D3/D1 can extend
//! them additively. Tests construct via `Type::new(…)` (or the
//! `with_shadow_mode` builder for `SecurityPolicy`) and destructure with
//! `..` rather than struct literals.

use conductor_core::security::RiskTier;
use conductor_daemon::security::{
    CallerContext, DenialReason, GateConfirmationRequest, GateDecision, GatePlanRequest,
    SecurityPolicy, TrustLevel, enforce,
};

fn cli_caller() -> CallerContext {
    CallerContext::new(TrustLevel::CliTrusted)
}

fn gui_caller() -> CallerContext {
    CallerContext::new(TrustLevel::GuiTrusted)
}

fn untrusted_caller() -> CallerContext {
    CallerContext::new(TrustLevel::Untrusted)
}

fn enforce_mode() -> SecurityPolicy {
    SecurityPolicy::with_shadow_mode(false)
}

// ───────────────────────────────────────────────────────────────────────
// Shadow-mode envelope (default policy — preserved from D5 2/N)
// ───────────────────────────────────────────────────────────────────────

#[test]
fn production_security_policy_default_has_shadow_mode_off() {
    // PR-D activated Phase 1A: production builds default to
    // `shadow_mode = false` so the gate's decision table is
    // consulted on every IPC tool call. The lib's unit-test
    // builds (`cfg(test)`) keep `shadow_mode = true` so the
    // existing ToolExecutor fixtures don't have to thread a
    // synthetic trusted CallerContext through every site —
    // that's an internal optimisation, not part of the public
    // contract. From an integration-test viewpoint (which sees
    // the lib as a normal dependency, NOT compiled with `cfg(test)`),
    // the default is `false`.
    let policy = SecurityPolicy::default();
    assert!(
        !policy.shadow_mode,
        "Phase 1A is active: production default must be shadow_mode = false"
    );
}

#[test]
fn shadow_mode_overrides_every_tier_to_allow() {
    // Test the shadow-mode envelope explicitly. Build a policy
    // with `with_shadow_mode(true)` rather than `default()`
    // because the production default is now `false` (PR-D).
    let policy = SecurityPolicy::with_shadow_mode(true);
    for trust in [
        TrustLevel::GuiTrusted,
        TrustLevel::CliTrusted,
        TrustLevel::Untrusted,
    ] {
        let caller = CallerContext::new(trust);
        for tier in [
            RiskTier::ReadOnly,
            RiskTier::Stateful,
            RiskTier::ArtifactRender,
            RiskTier::ConfigChange,
            RiskTier::HardwareIO,
            RiskTier::Privileged,
        ] {
            assert!(
                matches!(enforce(tier, &caller, &policy), GateDecision::Allow),
                "shadow mode must Allow tier={:?} trust={:?}",
                tier,
                trust
            );
        }
    }
}

// ───────────────────────────────────────────────────────────────────────
// Decision table (D5 3/N — `shadow_mode = false`)
// ───────────────────────────────────────────────────────────────────────

#[test]
fn enforce_allows_read_only_for_every_caller() {
    let policy = enforce_mode();
    for trust in [
        TrustLevel::GuiTrusted,
        TrustLevel::CliTrusted,
        TrustLevel::Untrusted,
    ] {
        let caller = CallerContext::new(trust);
        assert!(
            matches!(
                enforce(RiskTier::ReadOnly, &caller, &policy),
                GateDecision::Allow
            ),
            "ReadOnly must always Allow (trust={:?})",
            trust
        );
    }
}

#[test]
fn enforce_allows_stateful_for_every_caller() {
    let policy = enforce_mode();
    // Per spec §2.2 the *full* gate returns AllowWithAudit for LLM-initiated
    // Stateful calls. D13a wires the audit consumer; until then `Allow` is
    // correct. The Stateful → AllowWithAudit transition lands in (4/N) when
    // the audit-stream sink is plugged in.
    for trust in [
        TrustLevel::GuiTrusted,
        TrustLevel::CliTrusted,
        TrustLevel::Untrusted,
    ] {
        let caller = CallerContext::new(trust);
        assert!(
            matches!(
                enforce(RiskTier::Stateful, &caller, &policy),
                GateDecision::Allow
            ),
            "Stateful must Allow until D13a wires AllowWithAudit (trust={:?})",
            trust
        );
    }
}

#[test]
fn enforce_allows_artifact_render_for_every_caller() {
    let policy = enforce_mode();
    for trust in [
        TrustLevel::GuiTrusted,
        TrustLevel::CliTrusted,
        TrustLevel::Untrusted,
    ] {
        let caller = CallerContext::new(trust);
        assert!(
            matches!(
                enforce(RiskTier::ArtifactRender, &caller, &policy),
                GateDecision::Allow
            ),
            "ArtifactRender must always Allow (trust={:?})",
            trust
        );
    }
}

#[test]
fn enforce_requires_plan_for_config_change_from_every_caller() {
    let policy = enforce_mode();
    // ConfigChange always goes through Plan/Apply per ADR-007 — even GUI
    // callers who already hold the user's UI gesture. The plan provides
    // the visible diff + Apply token that ADR-027 D2 enforces.
    for trust in [
        TrustLevel::GuiTrusted,
        TrustLevel::CliTrusted,
        TrustLevel::Untrusted,
    ] {
        let caller = CallerContext::new(trust);
        assert!(
            matches!(
                enforce(RiskTier::ConfigChange, &caller, &policy),
                GateDecision::RequirePlan(_)
            ),
            "ConfigChange must require Plan/Apply (trust={:?})",
            trust
        );
    }
}

#[test]
fn enforce_requires_confirmation_for_hardware_io_from_gui_or_cli() {
    let policy = enforce_mode();
    for caller in [gui_caller(), cli_caller()] {
        assert!(
            matches!(
                enforce(RiskTier::HardwareIO, &caller, &policy),
                GateDecision::RequireConfirmation(_)
            ),
            "HardwareIO must require confirmation for trusted callers (trust={:?})",
            caller.trust_level
        );
    }
}

#[test]
fn enforce_denies_hardware_io_from_untrusted_caller() {
    // Spec §2.5: "HardwareIO tool with Untrusted caller is denied."
    // Untrusted spans LLM-initiated calls and unverified IPC peers — exactly
    // the surface where a confirmation prompt has nowhere to land.
    let policy = enforce_mode();
    let decision = enforce(RiskTier::HardwareIO, &untrusted_caller(), &policy);
    let GateDecision::Deny(reason) = decision else {
        panic!("HardwareIO from Untrusted must Deny, got {:?}", decision);
    };
    assert!(
        matches!(reason, DenialReason::UntrustedCallerForElevatedTier { .. }),
        "expected UntrustedCallerForElevatedTier, got {:?}",
        reason
    );
}

#[test]
fn enforce_denies_privileged_for_every_caller() {
    // Spec §2.5: "Unknown tool name → Privileged → denied." Privileged is
    // the deliberate fallback bucket for un-classified actions and tools —
    // no caller, however trusted, should be able to invoke it without an
    // explicit tier reclassification.
    let policy = enforce_mode();
    for trust in [
        TrustLevel::GuiTrusted,
        TrustLevel::CliTrusted,
        TrustLevel::Untrusted,
    ] {
        let caller = CallerContext::new(trust);
        let decision = enforce(RiskTier::Privileged, &caller, &policy);
        let GateDecision::Deny(reason) = decision else {
            panic!(
                "Privileged must Deny (trust={:?}), got {:?}",
                trust, decision
            );
        };
        assert!(
            matches!(reason, DenialReason::PrivilegedTier { .. }),
            "expected PrivilegedTier, got {:?} (trust={:?})",
            reason,
            trust
        );
    }
}

#[test]
fn enforce_decision_carries_caller_trust_in_denial_reason() {
    // Denials have to be self-describing for D13a's structured denial
    // stream — the reason must include enough context to render
    // a meaningful event without re-deriving from the call site.
    let policy = enforce_mode();
    let GateDecision::Deny(DenialReason::PrivilegedTier { trust_level, .. }) =
        enforce(RiskTier::Privileged, &untrusted_caller(), &policy)
    else {
        panic!("expected PrivilegedTier denial");
    };
    assert_eq!(trust_level, TrustLevel::Untrusted);
}

// ───────────────────────────────────────────────────────────────────────
// Cross-cutting invariants
// ───────────────────────────────────────────────────────────────────────

#[test]
fn gate_types_are_send_sync_for_cross_thread_use() {
    // Compile-time guard: gate types travel across the IPC server's tokio
    // task boundary, so they must be Send + Sync. If a future field breaks
    // this, the assertion fails at build time, not at runtime under load.
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<GateDecision>();
    assert_send_sync::<CallerContext>();
    assert_send_sync::<SecurityPolicy>();
    assert_send_sync::<TrustLevel>();
    assert_send_sync::<DenialReason>();
    assert_send_sync::<GatePlanRequest>();
    assert_send_sync::<GateConfirmationRequest>();
}
