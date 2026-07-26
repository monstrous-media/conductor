// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Global capability/tier enforcement gate (ADR-027 D5).
//!
//! This is the **(3/N)** sub-piece of D5: the decision-table that maps each
//! `(RiskTier, TrustLevel)` pair to a [`GateDecision`] per spec §2.2 + §2.5.
//! Earlier sub-pieces:
//!
//! - **(1/N)** PR #1047 — moved `RiskTier` to `conductor-core::security`,
//!   eliminating the historical `ToolRiskTier` divergence.
//! - **(2/N)** PR #1049 — landed the gate skeleton (`GateDecision`,
//!   `CallerContext`, `SecurityPolicy`, `TrustLevel`, `enforce`) in shadow
//!   mode, returning `Allow` for everything.
//!
//! ## Behaviour
//!
//! [`enforce`] now consults a real decision table:
//!
//! | Tier               | Decision                                        |
//! |--------------------|--------------------------------------------------|
//! | `ReadOnly`         | `Allow`                                         |
//! | `Stateful`         | `Allow` *(D13a will swap to `AllowWithAudit`)*  |
//! | `ArtifactRender`   | `Allow`                                         |
//! | `ConfigChange`     | `RequirePlan` (any caller — Plan/Apply per D2)  |
//! | `HardwareIO`       | `RequireConfirmation` (Gui/Cli) / `Deny` (Untrusted) |
//! | `Privileged`       | `Deny` (any caller — fallback bucket)           |
//!
//! ## Shadow mode (still on by default)
//!
//! Before the table is consulted, [`enforce`] checks
//! [`SecurityPolicy::shadow_mode`]. While `true` (the default), every tier
//! resolves to `Allow` — exactly the (2/N) behaviour. The flag flips to
//! `false` only after D3 (capability declarations) and D1 (peer-credential
//! IPC auth) also land. This preserves the spec §2 atomic-bundle invariant:
//! shipping the decision table without capability checks and authenticated
//! peers would let a same-user adversary game `TrustLevel::CliTrusted` to
//! reach `HardwareIO` without confirmation.
//!
//! ## Why TrustLevel matters now without D3
//!
//! Spec §2.5 calls out one case where `TrustLevel` alone — without
//! capability declarations — already determines the outcome:
//! `HardwareIO` from an `Untrusted` caller is `Deny`, because the
//! confirmation prompt that would otherwise gate the call has nowhere
//! to land (the Untrusted band covers LLM-initiated calls and
//! unverified IPC peers; neither has a UI confirmation surface).
//!
//! Post-D3, `TrustLevel` becomes one input into a richer
//! `(tier, trust, capability_set)` evaluation; today it shapes the
//! `HardwareIO` row only.

use conductor_core::security::RiskTier;

/// Outcome of a single `enforce` call.
///
/// Distinct from `Result` / `Err` on purpose: per spec §2.2, denial and
/// suspend-and-resume (confirmation/plan) are **outcomes**, not errors. Errors
/// are reserved for I/O, system, or programming failures. Treating denials as
/// errors invites the "log-and-continue after denial" class of bugs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateDecision {
    /// The action may proceed unchanged.
    Allow,
    /// The action may proceed; the executor must emit an audit entry first.
    /// Used for [`RiskTier::Stateful`] LLM-initiated calls in the full gate.
    /// Not yet emitted by (3/N) — D13a will wire the audit-stream sink.
    AllowWithAudit,
    /// The caller must materialise a [`GatePlanRequest`] and re-issue the
    /// call with an Apply token before the action runs. Used for
    /// [`RiskTier::ConfigChange`].
    RequirePlan(GatePlanRequest),
    /// The caller must surface a confirmation prompt and re-issue the call
    /// with the user's approval. Used for [`RiskTier::HardwareIO`] from
    /// trusted callers (GUI / CLI). Untrusted callers cannot reach this
    /// decision — they are denied outright.
    RequireConfirmation(GateConfirmationRequest),
    /// The action is denied terminally. Caller logs the denial and returns
    /// without invoking the executor.
    Deny(DenialReason),
}

/// Hand-off payload for `RequirePlan`. Carries the originating tier so the
/// downstream plan-creation site can record the right risk classification on
/// the resulting [`ConfigPlan`].
///
/// `#[non_exhaustive]` so D3 (capability declarations) and D1 (peer-cred
/// auth) can add fields without breaking downstream destructuring or
/// struct-literal construction. Use [`Self::new`] to construct.
///
/// **Naming:** prefixed `Gate` to distinguish from any future plan-related
/// types in `daemon::llm::plan` (where `ConfigPlan` already lives).
///
/// [`ConfigPlan`]: crate::daemon::ConfigPlan
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct GatePlanRequest {
    /// The originating tier. Today always `RiskTier::ConfigChange` — but
    /// kept as a field so future tiers that route through Plan/Apply
    /// (rather than gaining their own decision row) don't lose provenance.
    pub tier: RiskTier,
}

impl GatePlanRequest {
    /// Construct a new [`GatePlanRequest`] for the given originating tier.
    pub fn new(tier: RiskTier) -> Self {
        Self { tier }
    }
}

/// Hand-off payload for `RequireConfirmation`. Carries the originating tier
/// and the caller's `TrustLevel` so the downstream confirmation flow can
/// render an appropriate prompt — e.g. the GUI's modal vs the CLI's
/// terminal y/N — without re-deriving context from the call site.
///
/// `#[non_exhaustive]` so D3/D1 can add fields without breaking downstream
/// destructuring or struct-literal construction. Use [`Self::new`].
///
/// **Naming:** prefixed `Gate` because there is already a distinct
/// [`crate::daemon::hardware_io::ConfirmationRequest`] (re-exported from
/// `daemon::hardware_io::confirmation` — a private module) — that one is
/// a SysEx pending-confirmation token holding the operation data,
/// validation result, and creation time. *This* type is the gate's
/// **decision-output** signalling that a confirmation surface is required;
/// it does not yet hold a token. The two coexist along the eventual
/// hardware-IO wiring (gate emits `RequireConfirmation(GateConfirmationRequest)`
/// → daemon creates a `hardware_io::ConfirmationRequest` token → user
/// approves → executor runs).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct GateConfirmationRequest {
    /// The originating tier (today always `RiskTier::HardwareIO`).
    pub tier: RiskTier,
    /// Trust band of the caller; used by the prompt-renderer to pick the
    /// right surface (GUI modal vs CLI tty).
    pub trust_level: TrustLevel,
}

impl GateConfirmationRequest {
    /// Construct a new [`GateConfirmationRequest`] for the given tier and
    /// caller trust band.
    pub fn new(tier: RiskTier, trust_level: TrustLevel) -> Self {
        Self { tier, trust_level }
    }
}

/// Why an action was denied. Each variant carries enough context for D13a's
/// structured denial stream to render a meaningful event without re-deriving
/// from the call site. The enum is `#[non_exhaustive]` so D3/D1 can add
/// new variants (`MissingCapability { … }`, `UnauthenticatedPeer { … }`)
/// without breaking downstream `match` arms; each struct-like variant is
/// also `#[non_exhaustive]` so adding fields *within* a variant — e.g.
/// `denied_at: Instant`, `caller_name: Option<String>` — stays additive
/// for downstream destructuring.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DenialReason {
    /// The action's tier is `Privileged`. This is the fallback bucket for
    /// un-classified actions and tools (per spec §2.5 "Unknown tool name →
    /// Privileged → denied") — no caller, however trusted, should reach
    /// this without an explicit tier reclassification on the action /
    /// tool definition.
    #[non_exhaustive]
    PrivilegedTier {
        /// Trust band of the denied caller (purely informational — the
        /// outcome would be `Deny` regardless of trust level).
        trust_level: TrustLevel,
    },
    /// The action's tier requires a UI confirmation surface, but the caller
    /// is `Untrusted` and has nowhere to receive the prompt. Spec §2.5:
    /// "HardwareIO tool with Untrusted caller is denied."
    #[non_exhaustive]
    UntrustedCallerForElevatedTier {
        /// The tier the caller attempted (today: only `HardwareIO`).
        tier: RiskTier,
    },
}

impl std::fmt::Display for DenialReason {
    /// Human-readable rendering for audit-stream messages (D13a) and
    /// `ExecutionResult::Error` payloads. The `Debug` impl is debug-
    /// oriented (struct-literal layout); operators reading audit logs
    /// or end users seeing a denial error need natural-language
    /// reasoning. `Display` is the canonical place for that.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PrivilegedTier { trust_level } => write!(
                f,
                "tool tier is Privileged — no caller may execute it (trust level was {:?})",
                trust_level
            ),
            Self::UntrustedCallerForElevatedTier { tier } => write!(
                f,
                "caller is Untrusted; tier {:?} requires Trusted (Gui or Cli) to grant",
                tier
            ),
        }
    }
}

/// Trust band attached to a caller. The full gate (after D1 lands) replaces
/// this with a kernel-pinned `PinnedPeer` identity derived from peer
/// credentials over the Unix socket. Today it is a hand-rolled enum so
/// callers can be wired into the gate without waiting for D1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrustLevel {
    /// The Tauri GUI process. Verified post-D1 by code-signature pinning.
    GuiTrusted,
    /// `conductorctl` and other CLI peers running as the daemon user.
    CliTrusted,
    /// Anyone else (LLM-initiated calls, third-party MCP clients,
    /// unverified peers).
    Untrusted,
}

/// Identity and authority of the caller invoking an action or MCP tool.
///
/// Current shape (D1 wiring PR-A):
///
/// - `trust_level: TrustLevel` — the band consumed by the decision table.
/// - `peer: Option<Arc<PinnedPeer>>` — `Some` for IPC flows that build
///   via [`Self::from_peer`], `None` for unit-test constructions via
///   [`Self::new`] that drive the decision table with a synthetic
///   trust-level. Carried through for audit logging and for future
///   re-classification on each enforce call (mid-session execve
///   detection, deferred to a follow-up).
///
/// Future Phase 1A bundle additions (still pending):
///
/// - `granted_capabilities: CapabilitySet` — closed-vocabulary capability
///   bag (D3)
/// - `session_budgets: Option<Arc<LlmBudgetTracker>>` — per-session LLM
///   spend (D6)
///
/// `#[non_exhaustive]` so those fields can land without breaking downstream
/// destructuring or struct-literal construction. Use [`Self::new`] or
/// [`Self::from_peer`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CallerContext {
    /// Caller trust band consumed by the decision table. Derived from
    /// `peer` via [`crate::security::classify_peer`] when constructed via
    /// [`Self::from_peer`]; passed in directly by gate unit tests via
    /// [`Self::new`].
    pub trust_level: TrustLevel,

    /// The pinned + classified peer that produced this context. `None`
    /// for unit-test constructions via [`Self::new`] that drive the
    /// decision table with synthetic `trust_level` values; `Some` for
    /// real IPC flows that build via [`Self::from_peer`]. Carrying the
    /// `Arc<PinnedPeer>` here means downstream audit logging can record
    /// the peer's uid/pid/exe alongside every gate decision without an
    /// out-of-band lookup.
    pub peer: Option<std::sync::Arc<crate::security::PinnedPeer>>,
}

impl CallerContext {
    /// Construct a new [`CallerContext`] from a caller trust band only.
    /// `peer` is left `None` — used by unit tests of the gate's
    /// decision table where a real `PinnedPeer` is unavailable.
    pub fn new(trust_level: TrustLevel) -> Self {
        Self {
            trust_level,
            peer: None,
        }
    }

    /// Construct a [`CallerContext`] from a pinned + classified peer.
    /// The trust level is derived by calling
    /// [`crate::security::classify_peer`] on the pinned peer, so the
    /// gate's decision table sees the same band a same-binary live
    /// re-classification would produce.
    ///
    /// This is the canonical constructor for the IPC accept loop: pin
    /// the peer, build the `CallerContext`, carry it through the
    /// per-connection request envelope.
    pub fn from_peer(peer: std::sync::Arc<crate::security::PinnedPeer>) -> Self {
        let trust_level = crate::security::classify_peer(&peer);
        Self {
            trust_level,
            peer: Some(peer),
        }
    }

    /// Construct a [`CallerContext`] for a daemon-internal dispatch
    /// — i.e., an `IpcRequest` synthesised by daemon code itself
    /// on behalf of an outer call that has already passed the gate.
    /// The canonical example today is the `conductor_*_plugin`
    /// match arms inside `ToolExecutor`'s tool dispatch that send a
    /// `DaemonCommand::IpcRequest` over the daemon command
    /// channel for plugin management. Internal-origin dispatches
    /// have no external peer to pin, but they ARE trusted by
    /// construction — the daemon code that issues them has been
    /// gate-checked at the outer entry point.
    ///
    /// Uses `TrustLevel::GuiTrusted` so the gate's decision table
    /// treats the inner dispatch the same as a verified GUI peer
    /// — `Allow` for ReadOnly / Stateful, `RequirePlan` for
    /// ConfigChange (so even internal-origin mutations get the
    /// Plan/Apply flow), `RequireConfirmation` for HardwareIO,
    /// `Deny` for Privileged. This is the pragmatic choice: it
    /// preserves the gate's safety invariants without introducing
    /// a "bypass" trust level that would later need its own audit
    /// posture.
    ///
    /// PR-D adds this constructor to close the
    /// `TODO(PR-D, gate-bypass on None)` comment from PR-B —
    /// distinguishing "anonymous external peer that failed to
    /// pin" (`None` caller_ctx → synthetic Untrusted at the gate)
    /// from "daemon-internal dispatch" (`Some(internal_trusted)`
    /// at the call site).
    pub fn internal_trusted() -> Self {
        Self {
            trust_level: TrustLevel::GuiTrusted,
            peer: None,
        }
    }

    /// Construct a synthetic [`CallerContext`] for a connection
    /// whose peer pinning failed at IPC accept (Linux < 5.3 with
    /// no `pidfd_open`, same-uid TCC anomaly on macOS, etc.). The
    /// trust level is `Untrusted` — without a verified peer
    /// identity we have no basis to grant any trust beyond
    /// `Allow` for read-only operations.
    ///
    /// PR-D's gate-call sites use this fallback when
    /// `caller_ctx.is_none()` so unpinned IPC peers always reach
    /// the gate's decision table; this is what closes the
    /// `TODO(PR-D, gate-bypass on None)` from PR-B. With
    /// `shadow_mode = false` (the new default after PR-D), an
    /// unpinned peer attempting `ConfigChange` / `HardwareIO` /
    /// `Privileged` will be `Deny`'d.
    pub fn synthetic_unpinned() -> Self {
        Self {
            trust_level: TrustLevel::Untrusted,
            peer: None,
        }
    }
}

/// Daemon security policy. Carries the shadow-mode flag and (post-D3) the
/// loaded capability table. Today only `shadow_mode` exists.
///
/// **PR-D activated Phase 1A**: production builds default to
/// `shadow_mode: false`, so the gate's decision table is consulted on
/// every IPC tool call. The unit-test builds (`cfg(test)`) keep
/// `shadow_mode: true` so the existing ToolExecutor tests don't have
/// to be rewritten to thread a real `CallerContext` through every
/// fixture — the production code path is exercised by the daemon
/// itself at startup and by the security-gate integration tests.
///
/// `#[non_exhaustive]` so the post-D3 capability-table field — and any
/// other policy knobs (rate-limit pools, budget caps, audit-stream
/// handles) — can land without breaking downstream destructuring or
/// struct-literal construction. Use [`Self::default`] for the
/// canonical policy, or [`Self::with_shadow_mode`] to override the
/// flag.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SecurityPolicy {
    /// While `true`, [`enforce`] returns [`GateDecision::Allow`]
    /// regardless of tier or caller — the decision table is
    /// bypassed. Production builds default to `false` (PR-D).
    /// Test builds default to `true` so the existing ToolExecutor
    /// tests continue to exercise the dispatch pipeline without
    /// gate interference.
    pub shadow_mode: bool,
}

// `#[derive(Default)]` would produce `shadow_mode: false` (the
// `bool::default()` value) — which matches the `cfg(not(test))`
// branch but cannot express the `cfg(test)` override. The
// hand-rolled impl below keeps unit-test builds at
// `shadow_mode = true` (so 50+ existing ToolExecutor fixtures
// don't need synthetic CallerContext threading) while production
// gets `false`. clippy::derivable_impls fires because it sees
// only the `cfg(not(test))` variant during its lint pass and
// thinks the impl is derivable; suppressing it here with the
// rationale explicit.
#[allow(clippy::derivable_impls)]
impl Default for SecurityPolicy {
    fn default() -> Self {
        Self {
            // PR-D: Phase 1A activation. Production defaults to
            // enforcement on; `cfg(test)` keeps shadow mode true
            // so unit-test fixtures (50+ ToolExecutor tests
            // calling `executor.execute("...", None, None)`) don't
            // need to be rewritten to thread a synthetic trusted
            // CallerContext through every site. The gate decision
            // table is exercised by `tests/security_gate_test.rs`
            // (11 cases pinning every decision-table cell) and by
            // the ToolExecutor unit tests that explicitly pass a
            // real `CallerContext` to verify Allow / Deny flows.
            #[cfg(test)]
            shadow_mode: true,
            #[cfg(not(test))]
            shadow_mode: false,
        }
    }
}

impl SecurityPolicy {
    /// Convenience builder: returns a policy with the supplied
    /// `shadow_mode` flag and defaults for every other field. Preferred
    /// over a struct literal because [`SecurityPolicy`] is
    /// `#[non_exhaustive]` — struct-literal construction is forbidden
    /// from outside this crate (including integration tests under
    /// `tests/`), so a builder keeps test code working as the policy
    /// gains fields in D3/D1.
    pub fn with_shadow_mode(shadow_mode: bool) -> Self {
        Self { shadow_mode }
    }
}

/// Evaluate whether the supplied action/tool may run.
///
/// In shadow mode (the default), returns [`GateDecision::Allow`] for every
/// tier. Otherwise consults the per-tier decision table described in the
/// module-level docs.
///
/// The signature deliberately takes `RiskTier` by value rather than the
/// `dyn HasTierAndCapabilities` trait object the spec sketches. The trait
/// requires `Capability`/`CapabilitySet` from D3, which is not in tree yet.
/// When D3 lands, the signature gains a `CapabilitySet` parameter (or moves
/// to the trait) — this is an additive change for every existing caller.
pub fn enforce(tier: RiskTier, caller: &CallerContext, policy: &SecurityPolicy) -> GateDecision {
    if policy.shadow_mode {
        tracing::trace!(
            ?tier,
            trust = ?caller.trust_level,
            "security_gate.enforce — shadow mode, returning Allow"
        );
        return GateDecision::Allow;
    }

    let decision = decide(tier, caller.trust_level);

    tracing::trace!(
        ?tier,
        trust = ?caller.trust_level,
        ?decision,
        "security_gate.enforce — decision"
    );

    decision
}

/// Pure decision-table lookup. Extracted from [`enforce`] so the table is
/// trivially testable without constructing a [`SecurityPolicy`], and so D3
/// can extend the signature with `granted_capabilities` without churning
/// the call sites in MCP dispatch and `ActionExecutor`.
fn decide(tier: RiskTier, trust: TrustLevel) -> GateDecision {
    match tier {
        RiskTier::ReadOnly | RiskTier::Stateful | RiskTier::ArtifactRender => GateDecision::Allow,

        RiskTier::ConfigChange => GateDecision::RequirePlan(GatePlanRequest::new(tier)),

        RiskTier::HardwareIO => match trust {
            TrustLevel::GuiTrusted | TrustLevel::CliTrusted => {
                GateDecision::RequireConfirmation(GateConfirmationRequest::new(tier, trust))
            }
            TrustLevel::Untrusted => {
                GateDecision::Deny(DenialReason::UntrustedCallerForElevatedTier { tier })
            }
        },

        RiskTier::Privileged => {
            GateDecision::Deny(DenialReason::PrivilegedTier { trust_level: trust })
        }
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for the pure decision table.
    //!
    //! Integration tests in `tests/security_gate_test.rs` exercise the public
    //! `enforce` API end-to-end (including the shadow-mode envelope). These
    //! unit tests pin the shape of [`decide`] so a future refactor that splits
    //! it across multiple files / signatures can pivot without losing the
    //! per-row regression coverage.

    use super::*;

    #[test]
    fn read_only_allows_for_every_trust_level() {
        for trust in [
            TrustLevel::GuiTrusted,
            TrustLevel::CliTrusted,
            TrustLevel::Untrusted,
        ] {
            assert_eq!(decide(RiskTier::ReadOnly, trust), GateDecision::Allow);
        }
    }

    #[test]
    fn config_change_carries_tier_in_plan_request() {
        let GateDecision::RequirePlan(req) = decide(RiskTier::ConfigChange, TrustLevel::GuiTrusted)
        else {
            panic!("ConfigChange must RequirePlan");
        };
        assert_eq!(req.tier, RiskTier::ConfigChange);
    }

    #[test]
    fn hardware_io_carries_trust_in_confirmation_request() {
        let GateDecision::RequireConfirmation(req) =
            decide(RiskTier::HardwareIO, TrustLevel::CliTrusted)
        else {
            panic!("HardwareIO from CliTrusted must RequireConfirmation");
        };
        assert_eq!(req.tier, RiskTier::HardwareIO);
        assert_eq!(req.trust_level, TrustLevel::CliTrusted);
    }

    #[test]
    fn untrusted_hardware_io_denial_carries_tier() {
        let GateDecision::Deny(DenialReason::UntrustedCallerForElevatedTier { tier }) =
            decide(RiskTier::HardwareIO, TrustLevel::Untrusted)
        else {
            panic!("HardwareIO from Untrusted must Deny");
        };
        assert_eq!(tier, RiskTier::HardwareIO);
    }

    #[test]
    fn privileged_denial_records_caller_trust() {
        let GateDecision::Deny(DenialReason::PrivilegedTier { trust_level }) =
            decide(RiskTier::Privileged, TrustLevel::GuiTrusted)
        else {
            panic!("Privileged must Deny");
        };
        assert_eq!(trust_level, TrustLevel::GuiTrusted);
    }
}
