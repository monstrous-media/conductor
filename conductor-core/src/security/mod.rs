// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Security primitives shared across `conductor-core`, `conductor-daemon`,
//! and `conductor-gui` (`tauri`).
//!
//! ADR-027 Phase 1A bundle scaffolding lands here:
//!
//! - **D5** — global risk-tier classification ([`RiskTier`]).
//! - **D3 (1/N + 2/N + 3/N)** — capability vocabulary ([`Capability`],
//!   [`CapabilitySet`], [`InterpreterFamily`], [`PathScope`]) plus the
//!   per-`ActionConfig`-variant mapping function
//!   ([`capabilities_for_action`]) per spec §3.1, plus runtime wrapper
//!   resolution + interpreter classification ([`resolve_effective_executable`],
//!   [`classify_interpreter`]) per spec §3.2. The wrapper-resolution
//!   path defeats the `/usr/bin/env python3 -c '…'` bypass class by
//!   walking the wrapper chain (env, xargs, sudo, nice, timeout,
//!   nohup, …) to find the *effective* binary before classifying.
//! - **D1** — peer-credential IPC auth (planned: `peer` submodule, daemon-side
//!   only because the auth layer can only run where the kernel surfaces a peer
//!   PID; conductor-core just declares the trait shape).
//!
//! Phase 1A ships atomically per spec §19. This module exposes only the
//! types — the gate (`enforce`), the IPC verifier, and the action wiring all
//! land in `conductor-daemon::security` so `conductor-core` stays free of
//! daemon-only dependencies.

pub mod capabilities;
pub mod egress;
pub mod interpreters;
pub mod keychain;
pub mod llm_budget;
pub mod network_acl;
pub mod tiers;

pub use network_acl::{AclError, AclWarning, NetworkAcl};

pub use keychain::{
    HmacKey, KeyMetadata, KeychainError, KeychainStore, KeyringKeychain, select_keychain,
};

pub use capabilities::{
    Capability, CapabilitySet, InterpreterFamily, PathScope, capabilities_for_action,
    resolved_interpreter_family,
};
pub use egress::{
    EgressConfig, EgressDecision, EgressMode, EgressPolicy, SecurityConfig, ToolEgressConfig,
    is_internal_ip,
};
pub use interpreters::{
    DEFAULT_MAX_WRAPPER_DEPTH, InterpreterClassification, ResolveError, ResolvedExecutable,
    classify_interpreter, resolve_effective_executable,
};
pub use llm_budget::{
    BudgetDimension, BudgetExceeded, BudgetExceededBehaviour, LlmBudgetConfig, LlmBudgetState,
};
pub use tiers::RiskTier;
