// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Regression tests for #1311 — MCP `ConfigChange` over Unix socket
//! must consult the `McpRegistry` for per-client tier ceilings
//! rather than fall through to `CallerContext::internal_trusted()`.
//!
//! The bug: `handle_config_change_over_mcp` synthesised
//! `CallerContext::internal_trusted()` for every connection, so any
//! same-UID process that could reach the 0600 socket got full
//! `ConfigChange` capability — bypassing the registry's documented
//! "unregistered = ReadOnly" contract (ADR-027 §D18).
//!
//! The fix: introduce a pure tier-ceiling check
//! `check_peer_tier_ceiling(registered_tier, requested_tool_tier)`
//! that returns `Ok(())` only if the registered ceiling subsumes
//! the requested tool tier. Unregistered peers (= `None`) get
//! ReadOnly only.

// ADR-045 D1 (#2494): exercises the MCP server's peer tier ceiling; the mcp module only exists in mcp compositions.
#![cfg(feature = "mcp")]

use conductor_daemon::daemon::audit::AuditRiskTier;
use conductor_daemon::daemon::mcp::check_peer_tier_ceiling;
use conductor_daemon::daemon::mcp_types::ToolRiskTier;

/// Unregistered peer (None ceiling) MUST be allowed ReadOnly tools.
/// This is the safe default per ADR-027 §D18.
#[test]
fn unregistered_peer_allowed_read_only() {
    let result = check_peer_tier_ceiling(None, ToolRiskTier::ReadOnly);
    assert!(
        result.is_ok(),
        "unregistered peer must be allowed ReadOnly; got {result:?}"
    );
}

/// THE REGRESSION TEST for #1311. Unregistered peer requesting any
/// ConfigChange tool MUST be denied. Without the fix, the daemon
/// invoked these tools under `CallerContext::internal_trusted()` and
/// they ran successfully.
#[test]
fn unregistered_peer_denied_config_change() {
    let result = check_peer_tier_ceiling(None, ToolRiskTier::ConfigChange);
    assert!(
        result.is_err(),
        "unregistered peer MUST be denied ConfigChange (#1311); got {result:?}"
    );
}

/// Unregistered peer attempting Stateful tools must also be denied
/// — the registry's "unregistered = ReadOnly" contract is strict.
#[test]
fn unregistered_peer_denied_stateful() {
    let result = check_peer_tier_ceiling(None, ToolRiskTier::Stateful);
    assert!(
        result.is_err(),
        "unregistered peer MUST be denied Stateful tools; got {result:?}"
    );
}

/// Unregistered peer attempting HardwareIO tools must be denied.
#[test]
fn unregistered_peer_denied_hardware_io() {
    let result = check_peer_tier_ceiling(None, ToolRiskTier::HardwareIO);
    assert!(
        result.is_err(),
        "unregistered peer MUST be denied HardwareIO tools; got {result:?}"
    );
}

/// Registered ReadOnly peer can invoke ReadOnly tools.
#[test]
fn registered_read_only_allowed_read_only() {
    let result = check_peer_tier_ceiling(Some(AuditRiskTier::ReadOnly), ToolRiskTier::ReadOnly);
    assert!(result.is_ok(), "got {result:?}");
}

/// Registered ReadOnly peer attempting Stateful MUST be denied —
/// the registered tier is the ceiling, not the floor.
#[test]
fn registered_read_only_denied_stateful() {
    let result = check_peer_tier_ceiling(Some(AuditRiskTier::ReadOnly), ToolRiskTier::Stateful);
    assert!(
        result.is_err(),
        "ReadOnly-registered peer MUST be denied Stateful; got {result:?}"
    );
}

/// Registered ConfigChange peer can invoke ReadOnly, Stateful, AND
/// ConfigChange — the registered tier is the ceiling, all lower
/// tiers are subsumed.
#[test]
fn registered_config_change_allows_lower_and_equal_tiers() {
    for tier in [
        ToolRiskTier::ReadOnly,
        ToolRiskTier::Stateful,
        ToolRiskTier::ConfigChange,
    ] {
        let result = check_peer_tier_ceiling(Some(AuditRiskTier::ConfigChange), tier);
        assert!(
            result.is_ok(),
            "ConfigChange-registered peer must be allowed {tier:?}; got {result:?}"
        );
    }
}

/// Registered ConfigChange peer is STILL denied HardwareIO — that's
/// a strictly higher tier requiring explicit grant.
#[test]
fn registered_config_change_denied_hardware_io() {
    let result =
        check_peer_tier_ceiling(Some(AuditRiskTier::ConfigChange), ToolRiskTier::HardwareIO);
    assert!(
        result.is_err(),
        "ConfigChange-registered peer MUST be denied HardwareIO; got {result:?}"
    );
}

/// Registered HardwareIO peer can invoke everything up to and
/// including HardwareIO. This is the top operator-grantable tier.
#[test]
fn registered_hardware_io_allows_all_external_tiers() {
    for tier in [
        ToolRiskTier::ReadOnly,
        ToolRiskTier::Stateful,
        ToolRiskTier::ConfigChange,
        ToolRiskTier::HardwareIO,
    ] {
        let result = check_peer_tier_ceiling(Some(AuditRiskTier::HardwareIO), tier);
        assert!(
            result.is_ok(),
            "HardwareIO-registered peer must be allowed {tier:?}; got {result:?}"
        );
    }
}

/// Privileged tools must NEVER be reachable from an MCP socket peer
/// regardless of registered tier. Privileged is reserved for
/// in-process daemon-internal callers.
#[test]
fn registered_hardware_io_denied_privileged() {
    let result = check_peer_tier_ceiling(Some(AuditRiskTier::HardwareIO), ToolRiskTier::Privileged);
    assert!(
        result.is_err(),
        "Privileged is never grantable via registry; got {result:?}"
    );
}

// ─── Council R1: Stateful tier ceiling coverage ────────────────────

/// Registered Stateful peer can invoke ReadOnly + Stateful tools.
/// Mirrors the ConfigChange-allows-lower test for the middle tier.
#[test]
fn registered_stateful_allows_read_only_and_stateful() {
    for tier in [ToolRiskTier::ReadOnly, ToolRiskTier::Stateful] {
        let result = check_peer_tier_ceiling(Some(AuditRiskTier::Stateful), tier);
        assert!(
            result.is_ok(),
            "Stateful-registered peer must be allowed {tier:?}; got {result:?}"
        );
    }
}

/// Registered Stateful peer is denied ConfigChange and HardwareIO —
/// the registered tier is the inclusive ceiling.
#[test]
fn registered_stateful_denied_higher_tiers() {
    for tier in [ToolRiskTier::ConfigChange, ToolRiskTier::HardwareIO] {
        let result = check_peer_tier_ceiling(Some(AuditRiskTier::Stateful), tier);
        assert!(
            result.is_err(),
            "Stateful-registered peer MUST be denied {tier:?}; got {result:?}"
        );
    }
}

// ─── Council R1: ArtifactRender tool tier coverage ─────────────────

/// ArtifactRender tools (rendering content into MCP responses) are
/// mapped to ConfigChange-equivalent ceiling. A ConfigChange peer
/// can invoke them.
#[test]
fn registered_config_change_allows_artifact_render() {
    let result = check_peer_tier_ceiling(
        Some(AuditRiskTier::ConfigChange),
        ToolRiskTier::ArtifactRender,
    );
    assert!(
        result.is_ok(),
        "ConfigChange-registered peer must be allowed ArtifactRender; got {result:?}"
    );
}

/// A Stateful peer is denied ArtifactRender — the helper treats it
/// as requiring ConfigChange-or-higher ceiling.
#[test]
fn registered_stateful_denied_artifact_render() {
    let result =
        check_peer_tier_ceiling(Some(AuditRiskTier::Stateful), ToolRiskTier::ArtifactRender);
    assert!(
        result.is_err(),
        "Stateful-registered peer MUST be denied ArtifactRender (it maps to ConfigChange-equivalent ceiling); got {result:?}"
    );
}

/// Unregistered peer is denied ArtifactRender — same as any non-
/// ReadOnly tool for unregistered peers.
#[test]
fn unregistered_peer_denied_artifact_render() {
    let result = check_peer_tier_ceiling(None, ToolRiskTier::ArtifactRender);
    assert!(
        result.is_err(),
        "unregistered peer MUST be denied ArtifactRender; got {result:?}"
    );
}

// ─── Council R1: Internal tier ceiling coverage ────────────────────

/// Council R3 (rejected verdict on 14659c17): `AuditRiskTier::Internal`
/// is in the registry vocabulary, but if an operator mistakenly runs
/// `conductorctl mcp register --tier Internal ...` it would (under
/// the prior implementation) escalate the external peer to
/// allow-all-except-Privileged. That defeats the purpose of #1311.
///
/// New defensive behaviour: `Internal` as the REGISTRY-OBSERVED
/// ceiling is treated as if the peer were unregistered — ReadOnly
/// only, everything else denied. `Internal` is reserved for
/// daemon-internal callers (which never go through this code path),
/// so observing it via the external registry is either a
/// misconfiguration or registry tampering. Either way: fail-safe.
#[test]
fn registered_internal_treated_as_unregistered_for_security() {
    // ReadOnly: allowed (same as unregistered).
    assert!(check_peer_tier_ceiling(Some(AuditRiskTier::Internal), ToolRiskTier::ReadOnly).is_ok());
    // Everything else: denied (Internal is not a grantable external tier).
    for tier in [
        ToolRiskTier::Stateful,
        ToolRiskTier::ArtifactRender,
        ToolRiskTier::ConfigChange,
        ToolRiskTier::HardwareIO,
        ToolRiskTier::Privileged,
    ] {
        let result = check_peer_tier_ceiling(Some(AuditRiskTier::Internal), tier);
        assert!(
            result.is_err(),
            "Internal-registered peer MUST NOT be allowed {tier:?} \
             — Internal is daemon-only; treat external registry entries as unregistered (Council R3)"
        );
    }
}

// ─── Council R2: fail-open posture pin (documentation contract) ────

/// Documents the chosen fail-open posture on pin / registry-load
/// failures: `resolve_peer_tier_ceiling` returns `None` (not Err),
/// which means the peer is treated as unregistered → ReadOnly only.
///
/// This is the same posture as the per-peer rate limiter (ADR-027
/// §D16): pin failures (older Linux kernels without `pidfd_open`,
/// sandboxed macOS environments) should not break legitimate
/// ReadOnly clients on systems where pinning is fragile. The
/// security cost is bounded — same-UID processes already have
/// kernel-level access to plenty of state; ReadOnly access to MIDI
/// device enumeration isn't a meaningful escalation.
///
/// If a future threat-model review demands fail-closed, that's a
/// separate change to `resolve_peer_tier_ceiling` — but the
/// `check_peer_tier_ceiling` contract for `None` must NOT change
/// (clients depend on it for the documented unregistered semantics).
#[test]
fn pin_failure_posture_none_means_read_only_only() {
    // None represents "pin failed or unregistered" — the helper
    // treats both identically: ReadOnly OK, everything else denied.
    assert!(check_peer_tier_ceiling(None, ToolRiskTier::ReadOnly).is_ok());
    for non_ro in [
        ToolRiskTier::Stateful,
        ToolRiskTier::ArtifactRender,
        ToolRiskTier::ConfigChange,
        ToolRiskTier::HardwareIO,
        ToolRiskTier::Privileged,
    ] {
        assert!(
            check_peer_tier_ceiling(None, non_ro).is_err(),
            "pin failure (None ceiling) MUST deny {non_ro:?} \
             — even though we're fail-OPEN for ReadOnly per ADR-027 §D16"
        );
    }
}
