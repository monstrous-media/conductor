// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Daemon-side security gate (ADR-027 D5 onwards).
//!
//! This module hosts the global capability/tier enforcement gate that the
//! Phase 1A bundle (D5 + D3 + D1, per spec §2) rallies behind. The bundle is
//! atomic — partial-phase intermediate states are explicitly unsafe per
//! spec §2 line 100–104 — so the work lands in sub-pieces that each preserve
//! today's behaviour:
//!
//! - **D5 (1/N) — `RiskTier` canonicalisation** ([PR #1047]): moved
//!   `RiskTier` from `conductor-daemon::daemon::mcp_types` to
//!   `conductor-core::security::tiers`, eliminating the historical
//!   `ToolRiskTier` divergence so the gate has a single vocabulary.
//! - **D5 (2/N) — Gate scaffold** ([PR #1049]): introduced the
//!   `enforce` function, [`GateDecision`], [`CallerContext`],
//!   [`SecurityPolicy`], and [`TrustLevel`] types. In shadow mode (the
//!   default) the gate returned [`GateDecision::Allow`] for every tier
//!   so callers could be wired without any behavioural change.
//! - **D5 (3/N) — Decision table** (this module's current state):
//!   [`enforce`] now consults a real per-tier decision table per spec
//!   §2.2 + §2.5 — `ConfigChange` → `RequirePlan`, `HardwareIO` →
//!   `RequireConfirmation` (or `Deny` for `Untrusted`), `Privileged`
//!   → `Deny`, others → `Allow`. The default `shadow_mode = true`
//!   bypasses the table entirely so the runtime behaviour matches
//!   pre-gate dispatch.
//! - **Future sub-pieces**: D3 capability declarations (the table
//!   gains a `CapabilitySet` axis), D1 peer-credential IPC auth (the
//!   `TrustLevel` enum is replaced by a kernel-pinned `PinnedPeer`),
//!   the call-site wiring through `mcp.rs` + `action_executor.rs`,
//!   then the atomic flag flip that turns `shadow_mode = false` on.
//!
//! [PR #1047]: https://github.com/monstrous-media/conductor/pull/1047
//! [PR #1049]: https://github.com/monstrous-media/conductor/pull/1049

pub mod gate;
// network_approvals uses Unix-only hardened-file APIs (libc / std::os::unix);
// gate it (and its re-exports) so non-Unix daemon builds still compile.
#[cfg(unix)]
pub mod approval_admin;
#[cfg(unix)]
pub mod keychain_init;
#[cfg(unix)]
pub mod network_approvals;
#[cfg(unix)]
pub mod network_bind_gate;
pub mod peer_pin;

#[cfg(unix)]
pub use approval_admin::{AdminError, ListenerInfo, ListenerStatus};
pub use gate::{
    CallerContext, DenialReason, GateConfirmationRequest, GateDecision, GatePlanRequest,
    SecurityPolicy, TrustLevel, enforce,
};
#[cfg(unix)]
pub use keychain_init::{
    KeyRotationStatus, KeychainInit, KeychainInitError, RotationLevel, init_keychain,
    init_keychain_with, key_rotation_status, key_rotation_status_default,
};
#[cfg(unix)]
pub use network_approvals::{
    ApprovalDecision, ApprovalKey, ApprovalRecord, ApprovalRegistry, ApprovingSurface,
    RegistryError, TrustedNetworkApproval, compute_acl_hash,
};
#[cfg(unix)]
pub use network_bind_gate::{
    BindVerdict, GateEdge, KeychainProvider, NetworkBindGate, PlatformKeychainProvider,
    WithholdReason,
};
pub use peer_pin::{PeerAuthError, PinnedPeer, classify_peer};
