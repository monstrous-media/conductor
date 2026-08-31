// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Daemon-managed live configuration seam per ADR-034 §D1.
//!
//! `LiveConfig` is the single mutation seam through which every
//! config-change operation (LLM `ApplyPlan`, GUI `SaveConfig`, CLI
//! `ImportConfig`, `MarkKnownGood`, `Rollback`, `RollbackForce`)
//! flows. It publishes an immutable `LiveConfigSnapshot` via
//! `Arc<ArcSwap<...>>` — readers are lock-free.
//!
//! **Scope (post-D4.B.1):** mutation seam + atomic file persistence.
//! `live.toml` and `live.toml.known_good` are written via the per-op
//! [`PersistTarget`] matrix during `commit_locked` step 13.
//! `mutate_replace_whole` writes `live.toml`; `MarkKnownGood` writes
//! `live.toml.known_good` only. The two-phase audit outbox (§D8) is wired
//! when [`LiveConfig::with_audit_outbox`] is enabled: `commit_locked` appends
//! a `Pending` row at step 12 and an `Applied`/`Failed` marker at step 16, and
//! refuses the mutation fail-closed with `MutateError::AuditUnavailable` if the
//! outbox is at its §D8.1 cap (§D8.2). The SQLite flusher and the full
//! `AuditDegraded` lifecycle (flush-failure tracking + `conductorctl audit
//! resume`) are follow-up sub-slices.
//!
//! ## Key invariants
//!
//! - **Single CAS token (R5-C1).** `state_generation: u64` advances
//!   on every `mutate()`; `revision: ConfigRevision` is content-hash
//!   for audit only, never a concurrency check. This avoids the ABA
//!   defect where byte-identical mutations (`MarkKnownGood`,
//!   identity-preserving `Rollback`) collide on revision.
//! - **First publish = generation 1 (KI-A2).** Sentinel `0` is
//!   reserved for "uninitialised" (AwaitingConfig idle mode in D4.B).
//! - **Detached commit task (KI-A1).** `mutate()` spawns the
//!   persist+publish phase as `tokio::spawn`; dropping the caller's
//!   future does NOT cancel the commit. IPC framework cancellation
//!   on client disconnect cannot leave the mutex+persist mid-flight.
//! - **Step-2 force bypass (KI-A3).** `RollbackForce` skips the CAS
//!   check at step 2 as well as step 11 — break-glass means break-glass.
//! - **Overflow panics (KI-A4).** `state_generation` uses
//!   `checked_add(1).expect(...)` rather than wrapping; an attacker
//!   sustaining 2³² mutations/sec for 100+ years to wrap is already
//!   broken-system territory.
//! - **Audit-before-persist (R4-C3).** Pending audit enqueued at
//!   step 12, after CAS double-check, before persist at step 13.
//!   A crash between persist and publish leaves a pending audit
//!   entry the next startup can reconcile.
//! - **Re-entrancy ban (KI-C1).** No code reachable from steps 10–16
//!   may call `mutate()` or `current.load_full()` synchronously.
//!   Enforced by convention; documented for `MutationObserver`
//!   implementations.

pub mod paths;
pub mod persist;
pub mod startup;

pub use paths::LivePaths;
pub use persist::persist_atomically;
pub use startup::{
    LoadRejection, LoadRejectionReason, LoadedFrom, StartupLoadOutcome, try_load_chain,
};

use arc_swap::ArcSwap;
use conductor_core::Config;
use conductor_core::config::{ConfigRevision, Provenance};
use conductor_core::rule_set::CompiledRuleSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;
use thiserror::Error;
use tokio::sync::{Mutex, oneshot};

// ────────────────────────────────────────────────────────────────────
// LiveConfigSnapshot — atomic publication unit
// ────────────────────────────────────────────────────────────────────

/// Immutable, atomically-published configuration state.
///
/// Readers acquire a snapshot with `current.load_full()` (cost ≈ 1 ns)
/// and hold it for the duration of one logical operation. Writers
/// produce a new `Arc<LiveConfigSnapshot>` and `store()` it; all
/// fields advance atomically.
#[derive(Clone)]
pub struct LiveConfigSnapshot {
    /// Monotonic CAS token. Sentinel `0` means uninitialised; first
    /// successful `mutate()` publishes `state_generation = 1`.
    pub state_generation: u64,
    /// The config tree.
    pub config: Arc<Config>,
    /// Compiled rule set, kept in lock-step with `config`.
    pub rules: Arc<CompiledRuleSet>,
    /// Content-hash for audit/display only — see §D1 and ADR-034 §D5.
    /// Derived by [`revision_for`] over a NORMALIZED form (stripped-config
    /// canonical bytes ‖ `NORMALIZER_VERSION`-stamped endpoint digest), NOT the
    /// raw persisted bytes (ADR-035 Slice 9), so a semantic-identical
    /// `[[bindings]]` ↔ `[[endpoints]]` rewrite is revision-neutral.
    pub revision: ConfigRevision,
    /// `Some` if the operator has run `MarkKnownGood` since startup,
    /// pinning a known-good revision for `Rollback`.
    pub known_good_revision: Option<ConfigRevision>,
    /// Wall-clock time of publish. Informational; not a monotonic
    /// ordering signal (use `state_generation` for that).
    pub applied_at: SystemTime,
}

// ────────────────────────────────────────────────────────────────────
// RuleCompiler — trait so tests can swap in fakes
// ────────────────────────────────────────────────────────────────────

/// Compiles a `Config` into the dispatch-time
/// [`CompiledRuleSet`](conductor_core::rule_set::CompiledRuleSet).
///
/// Trait-doubled so tests can inject deterministic fakes; production
/// uses [`RealRuleCompiler`] which wraps `conductor_core::rule_compiler::compile`
/// (ADR-009 Phase 3). Version-counter is an implementation detail
/// of each compiler — the trait deliberately doesn't expose it.
pub trait RuleCompiler: Send + Sync + 'static {
    fn compile(&self, config: &Config) -> Result<CompiledRuleSet, CompileError>;
}

#[derive(Debug, Error)]
#[error("rule compilation failed: {0}")]
pub struct CompileError(pub String);

/// Production compiler — wraps `conductor_core::rule_compiler::compile`
/// with an internal monotonic version counter (the existing
/// engine_manager `rule_set_version: AtomicU64` will be retired in
/// D4.A.3.3 once all rule_set writes funnel through here).
pub struct RealRuleCompiler {
    version: AtomicU64,
}

impl RealRuleCompiler {
    pub fn new() -> Self {
        Self {
            version: AtomicU64::new(0),
        }
    }
}

impl Default for RealRuleCompiler {
    fn default() -> Self {
        Self::new()
    }
}

impl RuleCompiler for RealRuleCompiler {
    fn compile(&self, config: &Config) -> Result<CompiledRuleSet, CompileError> {
        let v = self.version.fetch_add(1, Ordering::Relaxed) + 1;
        Ok(conductor_core::rule_compiler::compile(config, v))
    }
}

// ────────────────────────────────────────────────────────────────────
// ConfigOp — typed mutation requests
// ────────────────────────────────────────────────────────────────────

/// One mutation request to `LiveConfig::mutate()`.
///
/// Typed (rather than a closure) so call sites are grep-able, audit
/// entries can carry a discriminator, and the per-op persist matrix
/// (ADR-034 §D1.2.1) is implementable.
#[derive(Debug, Clone)]
pub enum ConfigOp {
    /// Replace the entire config tree (CAS-checked).
    ReplaceWhole { config: Box<Config> },
    /// CAS-checked rollback to `live.toml.known_good`. Loaded by
    /// the daemon at step 3; not transmitted in the op payload.
    ///
    /// D4.B.4 (#1291) implements the file-side reader in
    /// `compute_candidate`. Surfaces InvalidOp distinctly for
    /// "no known-good recorded yet", "io error on known_good",
    /// and "parse error on known_good" so the operator can
    /// diagnose without grepping the daemon log.
    Rollback,
    /// Break-glass non-CAS rollback. Bypasses CAS at steps 2 AND 11
    /// (KI-A3 fix vs R6 which only fixed step 11). `reason` is
    /// required (non-empty, enforced at IPC framer per KI-B3 and
    /// re-validated in `compute_candidate` per Council R4).
    ///
    /// D4.B.4 (#1291) implements the file-side reader. Same posture
    /// as [`Self::Rollback`] for failure modes — empty reason still
    /// rejects before any read attempt.
    RollbackForce { reason: String },
    /// Promote the current snapshot's `revision` to `known_good`.
    /// Bytes don't change; only `known_good_revision` advances.
    MarkKnownGood,
}

impl ConfigOp {
    /// Human-readable discriminator for audit / log messages.
    pub fn kind(&self) -> &'static str {
        match self {
            ConfigOp::ReplaceWhole { .. } => "replace_whole",
            ConfigOp::Rollback => "rollback",
            ConfigOp::RollbackForce { .. } => "rollback_force",
            ConfigOp::MarkKnownGood => "mark_known_good",
        }
    }

    /// True for the break-glass variant — skips CAS at step 2 and 11.
    pub fn is_force(&self) -> bool {
        matches!(self, ConfigOp::RollbackForce { .. })
    }
}

// ────────────────────────────────────────────────────────────────────
// MutationOutcome / MutateError
// ────────────────────────────────────────────────────────────────────

/// Returned to the caller on successful `mutate()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationOutcome {
    pub applied_generation: u64,
    pub applied_revision: ConfigRevision,
    pub previous_generation: u64,
    pub previous_revision: ConfigRevision,
}

#[derive(Debug, Error)]
pub enum MutateError {
    /// Caller's `base_generation` does not match the current snapshot
    /// — another writer committed in the interim. Re-fetch via
    /// `current.load_full()` and retry. `RollbackForce` bypasses this.
    #[error("stale base_generation: current={current}, supplied={supplied}")]
    StaleBaseGeneration { current: u64, supplied: u64 },

    /// `compute_candidate` couldn't produce a valid config from the op.
    /// Most often: `MarkKnownGood` issued before the operator has
    /// committed any mutation (no current snapshot to mark good).
    #[error("could not compute candidate config: {0}")]
    InvalidOp(String),

    /// Rule compilation failed (config didn't lower into a valid
    /// dispatch tree). Should be rare given that the config already
    /// parsed; indicates an internal invariant break.
    #[error("rule compilation failed: {0}")]
    CompileFailed(#[from] CompileError),

    /// `state_generation` would overflow `u64::MAX`. Treated as a
    /// panic-class invariant break (KI-A4); the wrapping alternative
    /// would silently re-enable ABA. Reached only after ~10¹⁹ mutations.
    #[error("state_generation overflow — broken-system territory")]
    Overflow,

    /// Caller's commit task panicked. Reachable only if the spawned
    /// commit task aborts; surfaces the join error.
    #[error("commit task panicked: {0}")]
    Internal(String),

    /// #1320: `try_mutate_replace_whole`'s closure returned `Err(...)`.
    /// The candidate config is dropped — no persist, no publish, no
    /// `state_generation` bump. The string carries the closure's
    /// reason (typically the upstream error message, e.g. from
    /// `plan.apply_atomic`).
    #[error("mutator aborted before publish: {0}")]
    MutatorAborted(String),

    /// ADR-035 §D5 (R2): an in-place change to a LIVE endpoint's `direction`
    /// is restart-required. Routes reference endpoints by alias; a hot-path
    /// destroy+create to flip a direction could dangle a route or misroute an
    /// in-flight event, so the mutate is rejected and the running config (with
    /// its routes) left intact. The operator must restart the daemon to apply
    /// a direction change.
    #[error("restart required: {0}")]
    RestartRequired(String),

    /// ADR-034 §D8.2: the audit outbox is full (cap reached), so this
    /// ConfigChange mutation is refused fail-closed — audit-on-critical-path
    /// makes audit-blinding *obstructive*, not just observable. The operator
    /// drains/clears the outbox (the flusher + `conductorctl audit resume`)
    /// before mutations can resume. Maps to `IpcErrorCode::AuditUnavailable`.
    #[error("audit unavailable: {0}")]
    AuditUnavailable(String),
}

// ────────────────────────────────────────────────────────────────────
// LiveConfig — the mutation seam
// ────────────────────────────────────────────────────────────────────

/// ADR-034 §D8 audit-outbox state — the SINGLE source of truth the commit
/// path consults for the audit gate (#2380). Collapsing the prior split
/// `audit_outbox: Option<…>` + `audit_unavailable: bool` into one enum behind
/// one mutex removes the split-brain risk and lets `resume_audit` heal it at
/// runtime (Council #2380 refinement #2). Held as `Arc<Mutex<AuditState>>`
/// so `clone_for_commit`'s detached task and `resume_audit` share it.
pub(crate) enum AuditState {
    /// Audit recording not enabled (tests / non-audited deployments). No gate —
    /// mutations proceed without recording. (`audit_outbox == None` pre-#2380.)
    Disabled,
    /// Outbox open + recording. `commit_locked` enqueues here; the gate passes.
    Available(Arc<std::sync::Mutex<super::audit::AuditOutbox>>),
    /// Audit was REQUESTED (`with_audit_outbox`) but the outbox failed to open
    /// (corrupt chain / I/O). `commit_locked` refuses every mutation fail-closed
    /// with `MutateError::AuditUnavailable` until `resume_audit` heals it
    /// (§D8.2 audit-on-critical-path). Carries the open-failure reason.
    Unavailable(String),
}

/// Outcome of [`LiveConfig::resume_audit`] (#2380).
#[derive(Debug)]
pub enum ResumeOutcome {
    /// Audit was already healthy (`Available`) or never enabled (`Disabled`) —
    /// nothing to recover.
    AlreadyHealthy,
    /// The audit gate was reopened. `rotated_path` is `Some(...)` when a corrupt
    /// outbox was rotated aside and a fresh `ChainReset` chain was started; `None`
    /// when the existing file simply reopened cleanly (a transient prior error).
    Recovered {
        rotated_path: Option<std::path::PathBuf>,
    },
}

/// Owner of the live config snapshot + mutation serialisation lock.
///
/// Single-instance per daemon. Cloning the `Arc` is cheap; all
/// instances share the same `ArcSwap<LiveConfigSnapshot>` and the
/// same `mutate_lock`.
pub struct LiveConfig {
    current: Arc<ArcSwap<LiveConfigSnapshot>>,
    /// Serialises writers. Held across the persist+publish window of
    /// `mutate()` (the durability commit). Readers do not take it.
    mutate_lock: Arc<Mutex<()>>,
    /// Trait-doubled here in D4.A.2; D4.A.3 wires the real compiler.
    rule_compiler: Arc<dyn RuleCompiler>,
    /// Canonical serialiser source — trait-objected so tests can
    /// inject deterministic stand-ins if the real serialiser becomes
    /// noisy (cross-`toml`-crate-version stability is the contract).
    canonical: Arc<dyn CanonicalSerialiser>,
    /// File-system layout for daemon-managed state (D4.B.1).
    /// `commit_locked` step 13 routes `persist_atomically` against
    /// `paths.live` or `paths.known_good` per [`PersistTarget`].
    paths: LivePaths,
    /// ADR-034 §D8 audit-outbox state (the §D8.2 commit gate) — see
    /// [`AuditState`]. `Arc<Mutex<…>>` so the detached commit task
    /// (`clone_for_commit`) and `resume_audit` share one source of truth.
    /// `std::sync::Mutex` because the outbox append is a blocking `fdatasync`
    /// driven from `spawn_blocking`; held only briefly (never across an await).
    audit_state: Arc<std::sync::Mutex<AuditState>>,
    /// ADR-034 §D8.3 — mutations found `PendingAtCrash` by startup reconciliation
    /// (in flight at the previous crash, never published). Consumed at daemon
    /// startup by `EngineManager::emit_pending_at_crash_audit`, which emits one
    /// `ConfigMutationPendingAtCrash` audit event per entry before the IPC
    /// server starts. `Arc` so `clone_for_commit` shares it without deep-copying.
    pending_at_crash: Arc<Vec<super::audit::ReconciledMutation>>,
    /// **Test-only barrier.** When armed via
    /// [`arm_commit_gate`](Self::arm_commit_gate), the spawned commit task
    /// blocks on this `Notify` before running `commit_locked`, so a test can
    /// deterministically cancel the caller's `mutate` future while the commit
    /// is still pending instead of racing a tight timeout (#1522).
    #[cfg(any(test, feature = "test-helpers"))]
    commit_gate: Arc<std::sync::Mutex<Option<Arc<tokio::sync::Notify>>>>,
}

/// Canonical TOML serialiser hook.
pub trait CanonicalSerialiser: Send + Sync + 'static {
    fn serialise(&self, config: &Config)
    -> Result<Vec<u8>, conductor_core::config::CanonicalError>;
}

struct DefaultCanonicalSerialiser;

impl CanonicalSerialiser for DefaultCanonicalSerialiser {
    fn serialise(
        &self,
        config: &Config,
    ) -> Result<Vec<u8>, conductor_core::config::CanonicalError> {
        conductor_core::config::canonical::serialise(config)
    }
}

/// Derive the CAS [`ConfigRevision`] for a config (ADR-035 Slice 9 / §D5).
///
/// We hash a NORMALIZED form: the config with its `endpoints` block stripped,
/// concatenated with the `NORMALIZER_VERSION`-stamped
/// [`canonical_endpoint_digest`] of the endpoint set. The PERSISTED bytes are
/// unchanged — only this revision derivation normalizes. A deliberate
/// `NORMALIZER_VERSION` bump changes the digest, so the revision bumps on
/// purpose.
///
/// `pub(crate)` so the `ConfigDriftStatus` IPC handler (ADR-034 §D2 / D4.C,
/// #1901) can derive an on-disk config's revision using the SAME normalizer
/// the live snapshot uses — comparing a `canonical::serialise` hash against a
/// `revision_for` hash would spuriously report drift on every endpoint-bearing
/// config.
/// Current Unix-epoch milliseconds for an audit-outbox `created_at` (§D8). On
/// the impossible pre-1970 clock, falls back to 0 rather than erroring an
/// audit write.
fn now_unix_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub(crate) fn revision_for(config: &Config) -> Result<ConfigRevision, String> {
    use conductor_core::config::loader::{canonical_endpoint_digest, normalize_to_endpoints};

    // normalize_to_endpoints only errors on a duplicate endpoint alias, which
    // `validate`/`load` already reject — so this is not expected on a live
    // candidate. Surface it rather than silently mis-hashing.
    let (endpoints, _findings) =
        normalize_to_endpoints(config).map_err(|e| format!("normalize: {e}"))?;
    let digest = canonical_endpoint_digest(&endpoints);

    let mut stripped = config.clone();
    stripped.endpoints = Vec::new();
    let mut bytes = conductor_core::config::canonical::serialise(&stripped)
        .map_err(|e| format!("canonical: {e}"))?;
    // `ConfigRevision`'s Display is a stable lowercase-hex digest (its whole
    // purpose), so hashing the rendered string is a deliberate, durable coupling
    // — the endpoint digest fully participates in the revision this way.
    bytes.extend_from_slice(digest.to_string().as_bytes());
    Ok(ConfigRevision::from_canonical_bytes(&bytes))
}

/// Detect an in-place DIRECTION change on a live endpoint (ADR-035 §D5, R2).
///
/// Compares the normalized endpoint set of `current` vs `candidate` by alias;
/// returns a human-readable reason if any alias present in both has a different
/// `direction`. Such a flip is restart-required (the caller rejects it) — routes
/// reference endpoints by alias and a hot-path destroy+create could dangle a
/// route or misroute an in-flight event. New/removed aliases are NOT flagged
/// (those are ordinary create/delete, handled by the normal reload).
fn detect_live_direction_change(current: &Config, candidate: &Config) -> Option<String> {
    use conductor_core::config::loader::normalize_to_endpoints;
    // Both configs are pre-validated before reaching here, so a normalize error
    // is not expected — but log on the off chance a future path slips one
    // through, so the guard silently disabling itself is visible to operators.
    let cur = normalize_to_endpoints(current)
        .inspect_err(|e| tracing::warn!("direction-change check: normalize(current) failed: {e}"))
        .ok()?
        .0;
    let cand = normalize_to_endpoints(candidate)
        .inspect_err(|e| tracing::warn!("direction-change check: normalize(candidate) failed: {e}"))
        .ok()?
        .0;
    let cur_dirs: std::collections::HashMap<
        &str,
        conductor_core::config::types::ConnectorDirection,
    > = cur
        .iter()
        .map(|e| (e.alias.as_str(), e.direction))
        .collect();
    for e in &cand {
        if let Some(&prev) = cur_dirs.get(e.alias.as_str())
            && prev != e.direction
        {
            return Some(format!(
                "changing the direction of live endpoint '{}' ({:?} → {:?}) requires a daemon restart (ADR-035 §D5)",
                e.alias, prev, e.direction
            ));
        }
    }
    None
}

impl LiveConfig {
    /// Build a `LiveConfig` whose initial snapshot is the **uninitialised
    /// sentinel** — `state_generation = 0` — so the first successful `mutate()`
    /// publishes generation 1 (ADR-034 KI-A2 / R6-A8: gen 0 is the unambiguous
    /// "not yet published" marker). This is the test/fixture constructor and the
    /// base for callers that will load their real config via `mutate()`.
    ///
    /// **The daemon BOOT must NOT use this** — a freshly-loaded `live.toml` is the
    /// *first published snapshot*, not a sentinel, and `handle_get_config_body`
    /// blanks the gen-0 sentinel. The boot uses [`Self::new_published`] so that
    /// config is served at gen 1 (#2533: otherwise a cold-booted daemon's
    /// `GetConfigBody` returns an empty body and the GUI falls back to stale
    /// `config.toml`).
    ///
    /// `LivePaths` resolved from `$XDG_STATE_HOME` via [`LivePaths::from_env`].
    /// To inject a test-friendly path set, use [`Self::new_with_paths`].
    pub fn new(initial_config: Config) -> Result<Self, MutateError> {
        let paths = LivePaths::from_env()
            .map_err(|e| MutateError::InvalidOp(format!("resolve $XDG_STATE_HOME: {e}")))?;
        Self::new_with_paths_at(initial_config, paths, Arc::new(RealRuleCompiler::new()), 0)
    }

    /// #2533: build a `LiveConfig` whose initial snapshot IS the first published
    /// snapshot — `state_generation = 1` (ADR-034 KI-A2/R6-A8 "first published =
    /// 1"). Used by the daemon BOOT: a `live.toml` loaded at startup is already
    /// the canonical, persisted, `DaemonStartup`-audited config — it is the live
    /// config, not the uninitialised sentinel — so it must be published at gen 1,
    /// not parked at the gen-0 sentinel that `handle_get_config_body` blanks. (It
    /// is NOT routed through `mutate()`: the boot-load is not a runtime mutation,
    /// and the seam would redundantly re-persist + re-audit an already-canonical
    /// config. The mutate seam handles subsequent runtime mutations at gen ≥ 2.)
    pub fn new_published(initial_config: Config) -> Result<Self, MutateError> {
        let paths = LivePaths::from_env()
            .map_err(|e| MutateError::InvalidOp(format!("resolve $XDG_STATE_HOME: {e}")))?;
        Self::new_with_paths_at(initial_config, paths, Arc::new(RealRuleCompiler::new()), 1)
    }

    /// Test entry point — injects custom paths + rule compiler. Seeds the gen-0
    /// uninitialised sentinel (like [`Self::new`]); for the gen-1 published-boot
    /// variant with custom paths use [`Self::new_published_with_paths`].
    /// Production code uses [`Self::new`] / [`Self::new_published`].
    pub fn new_with_paths(
        initial_config: Config,
        paths: LivePaths,
        rule_compiler: Arc<dyn RuleCompiler>,
    ) -> Result<Self, MutateError> {
        Self::new_with_paths_at(initial_config, paths, rule_compiler, 0)
    }

    /// Test entry point for the gen-1 published-boot constructor with custom paths
    /// (the [`Self::new_published`] analogue of [`Self::new_with_paths`]).
    pub fn new_published_with_paths(
        initial_config: Config,
        paths: LivePaths,
        rule_compiler: Arc<dyn RuleCompiler>,
    ) -> Result<Self, MutateError> {
        Self::new_with_paths_at(initial_config, paths, rule_compiler, 1)
    }

    /// Shared seed. `initial_generation` is `0` (uninitialised sentinel — `new` /
    /// `new_with_paths`) or `1` (first published snapshot — `new_published` /
    /// `new_published_with_paths`, i.e. the daemon boot). #2533.
    fn new_with_paths_at(
        initial_config: Config,
        paths: LivePaths,
        rule_compiler: Arc<dyn RuleCompiler>,
        initial_generation: u64,
    ) -> Result<Self, MutateError> {
        // Ensure the state directory exists before the first persist.
        // Failure here surfaces as `InvalidOp` so the daemon refuses
        // to start rather than racing the first commit_locked.
        paths
            .ensure_dir()
            .map_err(|e| MutateError::InvalidOp(format!("create state dir: {e}")))?;

        // ADR-035 Slice 9 (§D5): derive the revision over the NORMALIZED endpoint
        // set so a semantic-identical `[[bindings]]`/`[[connectors]]` ↔
        // `[[endpoints]]` rewrite is reload-neutral (same revision ⇒ no CAS bump).
        let revision = revision_for(&initial_config)
            .map_err(|e| MutateError::InvalidOp(format!("initial revision: {e}")))?;
        let rules = rule_compiler.compile(&initial_config)?;

        let initial = LiveConfigSnapshot {
            // 0 = uninitialised sentinel; 1 = first published snapshot (boot).
            // The next `mutate()` publishes `initial_generation + 1` (KI-A2/R6-A8).
            state_generation: initial_generation,
            config: Arc::new(initial_config),
            rules: Arc::new(rules),
            revision,
            known_good_revision: None,
            applied_at: SystemTime::now(),
        };

        Ok(Self {
            current: Arc::new(ArcSwap::from_pointee(initial)),
            mutate_lock: Arc::new(Mutex::new(())),
            rule_compiler,
            canonical: Arc::new(DefaultCanonicalSerialiser),
            paths,
            audit_state: Arc::new(std::sync::Mutex::new(AuditState::Disabled)),
            pending_at_crash: Arc::new(Vec::new()),
            #[cfg(any(test, feature = "test-helpers"))]
            commit_gate: Arc::new(std::sync::Mutex::new(None)),
        })
    }

    /// Enable ADR-034 §D8 audit-outbox recording, opening the outbox at
    /// [`LivePaths::audit_outbox`]. Idempotent across re-opens (the outbox
    /// recovers its chain tail from any existing file). Returns the original
    /// `LiveConfig` unchanged if the outbox can't be opened, after logging —
    /// audit-outbox unavailability must not prevent the daemon from booting
    /// (the degraded-mode gate that makes it *obstructive* is §D8.2 / sub-slice
    /// C). Builder form so call sites read `LiveConfig::new(..)?.with_audit_outbox()`.
    pub fn with_audit_outbox(mut self) -> Self {
        use super::audit::MutationDisposition;
        match super::audit::AuditOutbox::open(self.paths.audit_outbox.clone()) {
            Ok((mut outbox, recovered)) => {
                // ADR-034 §D8.3 startup reconciliation: classify the recovered
                // rows against the revision actually loaded into the initial
                // snapshot.
                let live_revision = self.load().revision.to_string();
                let reconciled = super::audit::reconcile_outbox(&recovered, &live_revision);

                let mut pending = Vec::new();
                let mut inconsistent = 0usize;
                for m in &reconciled {
                    match m.disposition {
                        // Publish completed but the Applied-marker write was
                        // lost — re-attach it so the row is resolved.
                        MutationDisposition::PromoteToApplied => {
                            if let Err(e) = outbox.mark_applied(m.id.clone(), now_unix_ms()) {
                                tracing::warn!(
                                    "ADR-034 §D8.3: failed to promote outbox row {}: {e}",
                                    m.id
                                );
                            }
                        }
                        MutationDisposition::PendingAtCrash => pending.push(m.clone()),
                        MutationDisposition::Inconsistent => inconsistent += 1,
                        MutationDisposition::Completed | MutationDisposition::RolledBack => {}
                    }
                }
                if !pending.is_empty() || inconsistent > 0 {
                    tracing::warn!(
                        "ADR-034 §D8.3: audit outbox reconciliation — {} mutation(s) \
                         pending at crash (never published), {} inconsistent",
                        pending.len(),
                        inconsistent
                    );
                }
                self.pending_at_crash = Arc::new(pending);
                *self.audit_state.lock().unwrap_or_else(|p| p.into_inner()) =
                    AuditState::Available(Arc::new(std::sync::Mutex::new(outbox)));
            }
            Err(e) => {
                // §D8 sub-slice C (#2296): audit recording was requested but the
                // outbox could not be opened. Mark the daemon audit-unavailable so
                // `commit_locked` refuses ConfigChange mutations fail-closed —
                // rather than silently running un-audited (the pre-#2296 bug). The
                // daemon also surfaces this as the `AuditDegraded` lifecycle state
                // at startup (see `EngineManager::run`).
                tracing::error!(
                    "ADR-034 §D8: failed to open audit outbox at {}: {e}; \
                     entering audit-unavailable — config mutations are refused \
                     fail-closed until the outbox is restored",
                    self.paths.audit_outbox.display()
                );
                *self.audit_state.lock().unwrap_or_else(|p| p.into_inner()) =
                    AuditState::Unavailable(format!(
                        "audit outbox failed to open at {}: {e}",
                        self.paths.audit_outbox.display()
                    ));
            }
        }
        self
    }

    /// ADR-034 §D8 sub-slice C (#2296): `true` when audit recording was requested
    /// (`with_audit_outbox`) but the outbox failed to open, so config mutations are
    /// refused fail-closed. The daemon reads this at startup to enter
    /// `AuditDegraded`. (`false` both when audit is healthy and when it was never
    /// enabled — only an open *failure* sets it.)
    pub fn is_audit_unavailable(&self) -> bool {
        matches!(
            &*self.audit_state.lock().unwrap_or_else(|p| p.into_inner()),
            AuditState::Unavailable(_)
        )
    }

    /// `true` only when the outbox is open and recording (`AuditState::Available`).
    /// Used by `commit_locked` to decide whether to serialise provenance for the
    /// §D8 audit row. (`false` for both `Disabled` and `Unavailable` — the latter
    /// is refused at the commit gate before provenance would be used.)
    fn is_audit_available(&self) -> bool {
        matches!(
            &*self.audit_state.lock().unwrap_or_else(|p| p.into_inner()),
            AuditState::Available(_)
        )
    }

    /// ADR-034 §D8 / #2380 — operator recovery from the fail-closed
    /// audit-unavailable state (`conductorctl audit resume`).
    ///
    /// Holds `mutate_lock` for the whole operation so it serialises with
    /// `commit_locked` (no commit can observe a half-healed gate, and the
    /// rotation can't race an in-flight append — Council #2380 #3). Only acts on
    /// `Unavailable`; `Available`/`Disabled` are a no-op `AlreadyHealthy`.
    ///
    /// Recovery (operator-gated, never automatic — Council #2380 #1): reopen the
    /// outbox; if it's still corrupt, rotate the file aside to
    /// `audit-outbox.log.corrupt-<ms>` (preserving it for forensics), fsync the
    /// parent dir so the rename survives power loss, open a FRESH chain, and
    /// write a `ChainReset` attestation (operator, rotated filename, last valid
    /// hash, reason) as its first durable record. The audit gate is cleared
    /// (`Unavailable → Available`) ONLY after that record is fsync'd — a
    /// successful `open()` alone does not prove the new chain is writable. On any
    /// failure the gate is left `Unavailable` and an error naming the rotated
    /// file is returned (fail-closed preserved).
    ///
    /// KNOWN LIMITATION (Slice 1, #2380): a crash in the narrow window between the
    /// `rename` and the `ChainReset` write leaves the canonical path missing, so
    /// the next daemon boot auto-initialises a fresh empty chain WITHOUT an
    /// in-chain `ChainReset` attestation — the integrity break is then evidenced
    /// only by the rotated `.corrupt-<ms>` file on disk, not within the chain. The
    /// parent-dir fsync narrows this window but does not eliminate it. A fully
    /// crash-atomic variant (write the attested fresh chain to a temp file, fsync,
    /// then a single atomic rename over the canonical path while preserving the
    /// corrupt file via a hardlink) is deferred: this is an attended, operator-run
    /// recovery; normal-operation fail-closed protection is unaffected; and a
    /// missing / freshly-init'd chain on the next boot still trips downstream audit
    /// monitoring (Council #2384 — 2/3 GO for Slice 1 with this documented gap).
    pub async fn resume_audit(&self, operator: String) -> Result<ResumeOutcome, MutateError> {
        let _guard = self.mutate_lock.lock().await;

        // Inspect the gate; only `Unavailable` needs recovery. Drop the std guard
        // before any await — `MutexGuard` is not `Send`. Safe to release: we hold
        // `mutate_lock`, so neither a commit nor another resume can change
        // `audit_state` while we work.
        let reason = {
            let state = self.audit_state.lock().unwrap_or_else(|p| p.into_inner());
            match &*state {
                AuditState::Available(_) | AuditState::Disabled => {
                    return Ok(ResumeOutcome::AlreadyHealthy);
                }
                AuditState::Unavailable(r) => r.clone(),
            }
        };

        let path = self.paths.audit_outbox.clone();
        let now_ms = now_unix_ms();

        // All outbox file I/O is blocking (fdatasync) — run it off the executor.
        let opened: Result<(super::audit::AuditOutbox, Option<std::path::PathBuf>), MutateError> =
            tokio::task::spawn_blocking(move || {
                use super::audit::{AuditOutbox, OutboxError, last_valid_entry_hash};
                match AuditOutbox::open(path.clone()) {
                    // Reopened cleanly — the prior failure was transient (e.g. an
                    // I/O blip since startup). No rotation needed.
                    Ok((ob, _recovered)) => Ok((ob, None)),
                    // Corrupt chain — rotate aside and start fresh. The operator
                    // sanctioned the discontinuity by running `audit resume`.
                    Err(OutboxError::Corruption(_)) => {
                        let last_hash = last_valid_entry_hash(&path);
                        let rotated =
                            path.with_file_name(format!("audit-outbox.log.corrupt-{now_ms}"));
                        std::fs::rename(&path, &rotated).map_err(|e| {
                            MutateError::AuditUnavailable(format!(
                                "audit resume: failed to rotate corrupt outbox {} aside: {e}",
                                path.display()
                            ))
                        })?;
                        // Crash-safety: persist the rename before the fresh chain
                        // forms (best-effort fsync of the parent dir).
                        if let Some(parent) = path.parent()
                            && let Ok(dir) = std::fs::File::open(parent)
                        {
                            let _ = dir.sync_all();
                        }
                        // Path is now missing → a fresh, empty outbox.
                        let (mut ob, _r) = AuditOutbox::open(path.clone()).map_err(|e| {
                            MutateError::AuditUnavailable(format!(
                                "audit resume: corrupt outbox rotated to {} but the fresh \
                                 outbox failed to open: {e}",
                                rotated.display()
                            ))
                        })?;
                        let provenance = serde_json::json!({
                            "kind": "chain_reset_by_operator",
                            "operator": operator,
                            "rotated_to": rotated.display().to_string(),
                            "last_valid_hash": last_hash,
                            "reason": reason,
                        })
                        .to_string();
                        // Writing this (fdatasync'd) proves the fresh chain is
                        // durably writable — only then is the gate cleared.
                        ob.record_chain_reset(uuid::Uuid::new_v4().to_string(), provenance, now_ms)
                            .map_err(|e| {
                                MutateError::AuditUnavailable(format!(
                                    "audit resume: corrupt outbox rotated to {}, fresh chain \
                                     opened, but the ChainReset record failed to write: {e}",
                                    rotated.display()
                                ))
                            })?;
                        Ok((ob, Some(rotated)))
                    }
                    // Can't even read the file (I/O error) — stay degraded.
                    Err(e) => Err(MutateError::AuditUnavailable(format!(
                        "audit resume: cannot read outbox {}: {e}",
                        path.display()
                    ))),
                }
            })
            .await
            .map_err(|e| MutateError::Internal(format!("audit resume task panicked: {e}")))?;

        // `?` here leaves `audit_state` as `Unavailable` on failure (fail-closed).
        let (ob, rotated) = opened?;

        // Reopened + (if rotated) attested: NOW clear the gate. `mutate_lock` was
        // held throughout, so no commit observed an intermediate state.
        *self.audit_state.lock().unwrap_or_else(|p| p.into_inner()) =
            AuditState::Available(Arc::new(std::sync::Mutex::new(ob)));
        Ok(ResumeOutcome::Recovered {
            rotated_path: rotated,
        })
    }

    /// ADR-034 §D8.3 — mutations that were in flight at the previous crash and
    /// never published (`PendingAtCrash`), found by startup reconciliation when
    /// the audit outbox was opened. Empty unless [`Self::with_audit_outbox`] was
    /// enabled and the recovered outbox held unresolved pending rows. Consumed at
    /// startup by [`crate::daemon::engine_manager::EngineManager::emit_pending_at_crash_audit`],
    /// which emits one `ConfigMutationPendingAtCrash` audit event per entry.
    pub fn pending_at_crash(&self) -> &[super::audit::ReconciledMutation] {
        &self.pending_at_crash
    }

    /// Run a blocking outbox append (`fdatasync`) on a blocking worker, holding
    /// the outbox's `std::sync::Mutex` only for the duration of the append. A
    /// poisoned lock is recovered (`into_inner`) — a panicked prior append must
    /// not wedge all future audit writes. Maps both the join error and the
    /// `OutboxError` to `MutateError::Internal`.
    async fn outbox_append(
        outbox: &Arc<std::sync::Mutex<super::audit::AuditOutbox>>,
        f: impl FnOnce(
            &mut super::audit::AuditOutbox,
        ) -> Result<super::audit::OutboxEntry, super::audit::OutboxError>
        + Send
        + 'static,
    ) -> Result<(), MutateError> {
        let ob = Arc::clone(outbox);
        tokio::task::spawn_blocking(move || {
            let mut guard = ob.lock().unwrap_or_else(|p| p.into_inner());
            f(&mut guard).map(|_| ())
        })
        .await
        .map_err(|e| MutateError::Internal(format!("audit outbox task join: {e}")))?
        .map_err(|e| match e {
            // §D8.2: at-cap is a fail-closed refusal (atomic with the would-be
            // append inside `enqueue_pending`), not an internal fault — surface
            // it as AuditUnavailable so the caller can drain + retry.
            super::audit::OutboxError::AtCap => MutateError::AuditUnavailable(
                "audit outbox is full (cap reached); drain it (flusher / \
                 `conductorctl audit resume`) before retrying"
                    .to_string(),
            ),
            other => MutateError::Internal(format!("audit outbox append: {other}")),
        })
    }

    /// **Test-only.** Arm a barrier so the NEXT spawned commit task blocks
    /// before running `commit_locked`. Returns the `Notify`; the test releases
    /// the commit by calling `notify_one()`. This lets a test deterministically
    /// drop the caller's `mutate` future while the commit is pending (forcing
    /// the KI-A1 cancellation path) instead of racing a timeout (#1522). Gated
    /// behind the `test-helpers` feature (#1608).
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn arm_commit_gate(&self) -> Arc<tokio::sync::Notify> {
        let notify = Arc::new(tokio::sync::Notify::new());
        *self.commit_gate.lock().unwrap() = Some(Arc::clone(&notify));
        notify
    }

    /// Same as `new` but injects a custom compiler — pre-D4.B.1 test
    /// entry point retained for backward compat with the existing
    /// `live_config_mutate.rs` suite. Resolves paths from env.
    ///
    /// **Test-only.** Gated behind the `test-helpers` feature (#1608)
    /// so a downstream lib consumer can't reach for the
    /// inject-arbitrary-compiler seam by accident. CI and pre-push
    /// gates use `--all-features` which activates the gate; see
    /// `conductor-daemon/Cargo.toml`'s `[features]` block.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn with_compiler(
        initial_config: Config,
        rule_compiler: Arc<dyn RuleCompiler>,
    ) -> Result<Self, MutateError> {
        let paths = LivePaths::from_env()
            .map_err(|e| MutateError::InvalidOp(format!("resolve $XDG_STATE_HOME: {e}")))?;
        Self::new_with_paths(initial_config, paths, rule_compiler)
    }

    /// Lock-free read of the current snapshot. O(1), ~1 ns.
    pub fn load(&self) -> Arc<LiveConfigSnapshot> {
        self.current.load_full()
    }

    /// **Test-only seam — bypasses the CAS protocol, would silently
    /// corrupt LiveConfig if called from production code.**
    ///
    /// Replaces the current snapshot with a synthesised one. Used by
    /// the overflow regression test (#1325) to inject
    /// `state_generation = u64::MAX` so `mutate()` actually exercises
    /// the `checked_add` overflow path (KI-A4). Pre-fix, the
    /// integration test only asserted on the std-lib `checked_add`
    /// behaviour without driving `LiveConfig` at all.
    ///
    /// Gated behind the `test-helpers` feature (#1608) — `cfg(test)`
    /// alone doesn't reach integration tests in `tests/` since those
    /// link the crate as a normal lib. CI and pre-push gates use
    /// `--all-features` which activates the gate.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn install_test_snapshot(&self, snapshot: LiveConfigSnapshot) {
        self.current.store(Arc::new(snapshot));
    }

    /// **Test-only.** Install a specific [`AuditOutbox`] (e.g. forced at-cap via
    /// `set_count_for_test`) so the §D8.2 cap-refusal path can be exercised
    /// without the env-derived outbox or 4096 real appends.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn set_audit_outbox_for_test(&mut self, outbox: super::audit::AuditOutbox) {
        *self.audit_state.lock().unwrap_or_else(|p| p.into_inner()) =
            AuditState::Available(Arc::new(std::sync::Mutex::new(outbox)));
    }

    /// THE mutation entry point. See module docs + ADR-034 §D1.1 for
    /// the full 17-step contract.
    ///
    /// ## Cancellation contract (KI-A1)
    ///
    /// The commit phase runs as a detached `tokio::spawn` task; the
    /// caller awaits the outcome via a `oneshot::channel`. If the
    /// caller drops this future at `.await`:
    /// - The spawned task **continues running and the commit completes
    ///   (or fails) durably.** This is the load-bearing invariant.
    /// - The receiver drops, the task's `send()` fails silently, the
    ///   caller never observes the `MutationOutcome`.
    /// - **Callers MUST NOT retry blindly.** If you intend to retry
    ///   after a dropped/abandoned `mutate()`, first re-read
    ///   `current.load_full()` to see whether your intended state was
    ///   already applied. Blind retry would either get
    ///   `StaleBaseGeneration` (the commit succeeded — your op landed)
    ///   or duplicate-apply if you computed a fresh `base_generation`.
    /// - `MutateError::Internal("commit task channel closed ...")` is
    ///   only emitted when the channel is closed without a send —
    ///   should be unreachable in normal operation; surface as a bug
    ///   if observed.
    pub async fn mutate(
        &self,
        provenance: Provenance,
        base_generation: u64,
        op: ConfigOp,
    ) -> Result<MutationOutcome, MutateError> {
        // ── Steps 1-9: staging, no lock ─────────────────────────
        let snapshot_before = self.current.load_full();
        let force = op.is_force();

        // Step 2: outer CAS pre-check (KI-A3: honour force here too).
        if !force && snapshot_before.state_generation != base_generation {
            return Err(MutateError::StaleBaseGeneration {
                current: snapshot_before.state_generation,
                supplied: base_generation,
            });
        }

        // Step 3: compute candidate per op. `compute_candidate` is
        // async because the Rollback path reads from disk via
        // `spawn_blocking` (#1607) — the await yields back to the
        // runtime so a slow disk doesn't stall the executor.
        let candidate = self.compute_candidate(&op, &snapshot_before).await?;

        // Step 3.5: run core validation on every candidate that mutates
        // bytes. `Config::load` calls `validate_for_loading` (rejecting
        // shell-chaining and other security-sensitive patterns); the live
        // path used to skip it, so a malicious or malformed config could be
        // ReplaceWhole-d or Rollback-ed past the gate (#1475). MarkKnownGood
        // doesn't change config bytes — its candidate is the current
        // snapshot, already validated — so it's exempt.
        if !matches!(op, ConfigOp::MarkKnownGood) {
            conductor_core::config::validation::validate_for_loading(&candidate)
                .map_err(|e| MutateError::InvalidOp(format!("config validation failed: {e}")))?;
        }

        // Step 3.6 (ADR-035 §D5, Slice 9, R2): changing the DIRECTION of a LIVE
        // endpoint is restart-required. Reject the in-place flip here and leave
        // the running config — and the routes referencing the endpoint by alias
        // — untouched; a hot-path destroy+create could dangle a route or
        // misroute an in-flight event. MarkKnownGood changes no bytes, so exempt.
        // Force ops (RollbackForce) bypass too: break-glass means break-glass
        // (KI-A3) — an operator forcing a known-good restore must not be blocked
        // because that snapshot happens to flip a direction.
        if !force
            && !matches!(op, ConfigOp::MarkKnownGood)
            && let Some(reason) = detect_live_direction_change(&snapshot_before.config, &candidate)
        {
            return Err(MutateError::RestartRequired(reason));
        }

        // Steps 4–7: validate, compile, canonical bytes, new revision.
        //
        // MarkKnownGood doesn't change config bytes — `commit_locked`
        // sources publish_config/rules/revision from snapshot_now per
        // R5-C1, so the staged compile + canonical work would be
        // discarded. Skip it for MarkKnownGood (Council R1 D4.A.3.1).
        // For all other ops, run the full staging pipeline.
        let (compiled, canonical_bytes, new_revision) = if matches!(op, ConfigOp::MarkKnownGood) {
            // Defaults; commit_locked uses snapshot_now's values for
            // publish_config/rules/revision and the staged ones are
            // never read. CompiledRuleSet has no public ctor so we
            // borrow snapshot_before's — also unused at commit time.
            (
                (*snapshot_before.rules).clone(),
                Vec::new(),
                snapshot_before.revision,
            )
        } else {
            let compiled = self.rule_compiler.compile(&candidate)?;
            let bytes = self
                .canonical
                .serialise(&candidate)
                .map_err(|e| MutateError::InvalidOp(format!("canonical: {e}")))?;
            // ADR-035 Slice 9 (§D5): the revision normalizes the endpoint set
            // (reload-neutral legacy↔endpoint rewrites); the PERSISTED `bytes`
            // remain the verbatim candidate config.
            let rev = revision_for(&candidate)
                .map_err(|e| MutateError::InvalidOp(format!("revision: {e}")))?;
            (compiled, bytes, rev)
        };

        // Step 8: persist-target planning — known_good_revision is
        // computed INSIDE commit_locked from snapshot_now (TOCTOU
        // fix per Council R1: a concurrent MarkKnownGood between
        // steps 1 and 11 would otherwise see the stale writer
        // publish with the pre-MarkKnownGood marker value, silently
        // clobbering it).
        let op_kind_for_known_good = match op {
            ConfigOp::MarkKnownGood => MarkKnownGoodHint::Mark,
            _ => MarkKnownGoodHint::Inherit,
        };

        // Step 9 (per-op persist target): D4.B.1 wires this to the
        // file-side dispatch. `commit_locked` step 13 uses this to
        // route `persist_atomically` against `paths.live` vs
        // `paths.known_good`.
        let persist_target = PersistTarget::from_op(&op);

        // ── Steps 10-16: commit via fire-and-forget channel (KI-A1) ──
        // The commit task's lifetime is independent of the caller's
        // future. Result is delivered via a oneshot channel: if the
        // caller drops their `mutate()` future, the receiver drops and
        // the channel send fails silently — but the spawn completes
        // the persist+publish regardless. This is the correct shape
        // for cancellation safety; the previous pattern
        // (`tokio::spawn(...).await`) silently lost results on caller
        // cancellation because awaiting the JoinHandle is itself
        // cancellable. Round-2 Council fix.
        let live = self.clone_for_commit();
        let prov_for_audit = provenance;
        let (tx, rx) = oneshot::channel::<Result<MutationOutcome, MutateError>>();
        tokio::spawn(async move {
            // #1522: test-only barrier — when armed, block the detached commit
            // before it runs/publishes so a test can deterministically cancel
            // the caller's `mutate` future while the commit is still pending.
            // No-op (and compiled out) without the `test-helpers` feature.
            #[cfg(any(test, feature = "test-helpers"))]
            {
                // `take()` (not `clone()`) so an armed gate blocks only the
                // NEXT spawned commit, matching `arm_commit_gate`'s contract —
                // a stray arm can't wedge later commits.
                let gate = live.commit_gate.lock().unwrap().take();
                if let Some(notify) = gate {
                    notify.notified().await;
                }
            }
            let outcome = live
                .commit_locked(
                    snapshot_before,
                    candidate,
                    compiled,
                    canonical_bytes,
                    new_revision,
                    op_kind_for_known_good,
                    persist_target,
                    force,
                    base_generation,
                    prov_for_audit,
                )
                .await;
            // Receiver may be gone (caller dropped) — send returns
            // Err which we ignore. Commit already happened.
            let _ = tx.send(outcome);
        });
        rx.await.unwrap_or_else(|_recv_err| {
            // Channel closed without a send. This is only reachable
            // when the spawned commit task ended without executing
            // `tx.send` — in practice, that means it panicked (tokio
            // catches the panic, drops the closure, drops `tx`). The
            // panic trace is logged by tokio's default panic hook.
            // Note: caller-future cancellation does NOT reach here
            // (that drops `rx`, not `tx`). Council R4–R5 fix.
            Err(MutateError::Internal(
                "commit task ended without producing a result — \
                 check daemon logs for a tokio panic trace"
                    .to_string(),
            ))
        })
    }

    /// Ergonomic helper for the common "clone current config →
    /// mutate one field → publish" pattern. Used by D4.A.3.2 call-site
    /// migrations to replace `let mut g = self.config.write().await;
    /// g.field = x;` with a single non-blocking call:
    ///
    /// ```ignore
    /// self.live_config.mutate_replace_whole(
    ///     Provenance { initiator: Cli, source: InMemoryEdit, peer: None },
    ///     |cfg| { cfg.led.get_or_insert_with(Default::default).scheme = s; },
    /// ).await?;
    /// ```
    ///
    /// CAS uses the snapshot loaded at entry. On `StaleBaseGeneration`
    /// the helper does **NOT** retry — the caller MUST handle the
    /// error explicitly. Blind retry is unsafe because:
    ///
    /// - the previous attempt may have already published (your op
    ///   landed; retry would double-apply if you compute a fresh base)
    /// - concurrent writers may have a legitimate reason to win the
    ///   race (e.g. operator MarkKnownGood, LLM ApplyPlan)
    ///
    /// Recommended retry pattern: re-fetch via `load()`, re-render
    /// the user's intent over the new state, prompt for confirmation,
    /// then call again. See ADR-034 §D2.1 for the GUI/LLM/CLI
    /// conventions.
    pub async fn mutate_replace_whole<F>(
        &self,
        provenance: Provenance,
        mutator: F,
    ) -> Result<MutationOutcome, MutateError>
    where
        F: FnOnce(&mut Config),
    {
        let snap = self.current.load_full();
        let mut candidate = (*snap.config).clone();
        mutator(&mut candidate);
        self.mutate(
            provenance,
            snap.state_generation,
            ConfigOp::ReplaceWhole {
                config: Box::new(candidate),
            },
        )
        .await
    }

    /// #1320 fallible variant of [`mutate_replace_whole`]: closure
    /// returns `Result<(), String>`. On `Err(msg)`, the candidate
    /// config is dropped and the helper returns
    /// [`MutateError::MutatorAborted`] — NO persist, NO publish, NO
    /// `state_generation` bump. On `Ok(())`, behaves identically to
    /// the infallible variant: clone snapshot, apply mutator, then
    /// `mutate(ReplaceWhole)`.
    ///
    /// ## Why
    ///
    /// Pre-fix, `apply_plan` captured `plan.apply_atomic`'s error
    /// in an outer variable and called `mutate_replace_whole`, which
    /// has no return signal — so the publish ran regardless. A
    /// failed apply would still advance `state_generation` and write
    /// `live.toml`, spuriously invalidating pending plans and
    /// emitting an audit-log entry for a no-op mutation. Clawpatch
    /// HIGH-severity finding (#1320).
    ///
    /// ## Contract
    ///
    /// The mutator MAY mutate `cfg` even when it later returns
    /// `Err`. The partial mutation is discarded along with the
    /// candidate. Callers that need stronger atomicity within the
    /// closure should clone `cfg` once at the top of the closure
    /// and only commit the staging copy when they're sure the
    /// operation succeeds.
    ///
    /// On `Err(MutateError::MutatorAborted(msg))`, the caller should
    /// re-derive the upstream error from `msg` (typically a serialised
    /// `PlanError` / `ApplyError`); the live snapshot is unchanged.
    pub async fn try_mutate_replace_whole<F>(
        &self,
        provenance: Provenance,
        mutator: F,
    ) -> Result<MutationOutcome, MutateError>
    where
        F: FnOnce(&mut Config) -> std::result::Result<(), String>,
    {
        let snap = self.current.load_full();
        let mut candidate = (*snap.config).clone();
        mutator(&mut candidate).map_err(MutateError::MutatorAborted)?;
        self.mutate(
            provenance,
            snap.state_generation,
            ConfigOp::ReplaceWhole {
                config: Box::new(candidate),
            },
        )
        .await
    }

    /// Cheap clone of the live-config shell for the detached commit
    /// task. All inner state is `Arc`-shared.
    fn clone_for_commit(&self) -> Self {
        Self {
            current: Arc::clone(&self.current),
            mutate_lock: Arc::clone(&self.mutate_lock),
            rule_compiler: Arc::clone(&self.rule_compiler),
            canonical: Arc::clone(&self.canonical),
            paths: self.paths.clone(),
            // Arc-shared: the detached commit task + resume_audit see the SAME
            // audit state / outbox.
            audit_state: Arc::clone(&self.audit_state),
            pending_at_crash: Arc::clone(&self.pending_at_crash),
            #[cfg(any(test, feature = "test-helpers"))]
            commit_gate: Arc::clone(&self.commit_gate),
        }
    }

    /// Inner commit phase. Holds `mutate_lock` across CAS recheck +
    /// (would-be) persist + publish. Always runs to completion once
    /// spawned — the caller's future cancellation does not reach
    /// this task (KI-A1).
    ///
    /// Note: `known_good_revision` for the published snapshot is
    /// derived from `snapshot_now` (loaded inside the lock), NOT
    /// from `snapshot_before` — concurrent MarkKnownGood between
    /// step 1 and step 11 must not be silently clobbered (TOCTOU
    /// fix per Council R1).
    #[allow(clippy::too_many_arguments)]
    async fn commit_locked(
        &self,
        _snapshot_before: Arc<LiveConfigSnapshot>,
        candidate: Config,
        compiled: CompiledRuleSet,
        // D4.B.1 wires this to `persist_atomically` at step 13.
        // For MarkKnownGood the bytes are sourced from `snapshot_now`
        // inside the lock (TOCTOU symmetry with `op_kind_for_known_good`);
        // for all other ops the staged precompute is used.
        canonical_bytes: Vec<u8>,
        new_revision: ConfigRevision,
        op_kind_for_known_good: MarkKnownGoodHint,
        persist_target: PersistTarget,
        force: bool,
        base_generation: u64,
        provenance: Provenance,
    ) -> Result<MutationOutcome, MutateError> {
        let _guard = self.mutate_lock.lock().await;

        // Serialise provenance for the §D8 audit row BEFORE the CAS double-check
        // and any state advance, so an (impossible-in-practice, `Provenance` is
        // plain data) serialization failure can never strand a half-applied
        // mutation. Computed unconditionally but only consumed when the outbox
        // is enabled. (cloud-review.)
        let provenance_json = if self.is_audit_available() {
            Some(serde_json::to_string(&provenance).map_err(|e| {
                MutateError::Internal(format!("serialize provenance for audit outbox: {e}"))
            })?)
        } else {
            None
        };

        // Step 11: CAS double-check inside lock (KI-A3: skip if force).
        let snapshot_now = self.current.load_full();
        if !force && snapshot_now.state_generation != base_generation {
            return Err(MutateError::StaleBaseGeneration {
                current: snapshot_now.state_generation,
                supplied: base_generation,
            });
        }

        // Step 8 (deferred to here): per-op known_good_revision +
        // selection of published config/rules/revision.
        //
        // For MarkKnownGood specifically, we source EVERYTHING from
        // `snapshot_now` (NOT the staged candidate from outside the
        // lock). The staged candidate was a clone of
        // `snapshot_before.config`; if any other writer committed
        // between step 1 and step 11, CAS at step 11 would catch it
        // and reject — so reaching this point means
        // `snapshot_now == snapshot_before` logically. But sourcing
        // from `snapshot_now` makes the invariant "MarkKnownGood
        // publishes a snapshot whose `revision` equals the marker"
        // *structurally* true, not just CAS-implied. Council R2
        // round-3 (Sonnet 4.6) flagged the latter as a latent
        // correctness concern.
        let (publish_config, publish_rules, publish_revision, new_known_good) =
            match op_kind_for_known_good {
                MarkKnownGoodHint::Mark => (
                    Arc::clone(&snapshot_now.config),
                    Arc::clone(&snapshot_now.rules),
                    snapshot_now.revision,
                    Some(snapshot_now.revision),
                ),
                MarkKnownGoodHint::Inherit => (
                    Arc::new(candidate),
                    // Move `compiled` into the Arc rather than clone+wrap.
                    // The `Mark` arm above doesn't reference `compiled`,
                    // so the borrow checker accepts the move from this
                    // arm. CompiledRuleSet's per-rule compile cost is
                    // load-bearing on the hot reconfigure path (#1610).
                    Arc::new(compiled),
                    new_revision,
                    snapshot_now.known_good_revision,
                ),
            };

        // Step 12 (audit enqueue, §D8.1): durably append a `Pending` outbox
        // row AFTER the step-11 CAS and BEFORE persist at step 13 (R5-M1
        // ordering). The append is fsync'd before it returns. If it fails we
        // ABORT the mutation before touching disk — audit-on-critical-path
        // (§D8.2): a ConfigChange that cannot be recorded must not proceed.
        // (The 8-failure → AuditDegraded escalation is sub-slice C.)
        // §D8 sub-slice C (#2296) / #2380: consult the unified audit gate ONCE.
        // `Unavailable` (outbox failed to open) → refuse the mutation fail-closed
        // BEFORE persist (audit-on-critical-path, §D8.2): nothing is published.
        // `Available` → clone the outbox Arc to enqueue against; `Disabled` → no
        // recording. The lock is held only to inspect/clone, never across the
        // append await. Safe to release before the append: `commit_locked` holds
        // `mutate_lock` (above) for the whole window, and `resume_audit` (the only
        // writer of `audit_state`) also takes `mutate_lock`, so the state cannot
        // change between here and the append (Council #2380 #3).
        let recording_outbox: Option<Arc<std::sync::Mutex<super::audit::AuditOutbox>>> = {
            let state = self.audit_state.lock().unwrap_or_else(|p| p.into_inner());
            match &*state {
                AuditState::Unavailable(reason) => {
                    return Err(MutateError::AuditUnavailable(format!(
                        "audit outbox is unavailable ({reason}); config mutations are \
                         refused until it is restored (e.g. repair/remove the corrupt \
                         outbox and restart, or `conductorctl audit resume`)"
                    )));
                }
                AuditState::Available(ob) => Some(Arc::clone(ob)),
                AuditState::Disabled => None,
            }
        };
        let outbox_pending: Option<(Arc<std::sync::Mutex<super::audit::AuditOutbox>>, String)> =
            match recording_outbox {
                Some(outbox) => {
                    let id = uuid::Uuid::new_v4().to_string();
                    // Provenance was serialised at the top of the lock (before
                    // the CAS double-check), fail-closed — reuse it here.
                    let provenance_for_row = provenance_json.clone();
                    // The revision that will actually be PUBLISHED — for
                    // MarkKnownGood this is `snapshot_now`'s revision, not the
                    // staged `new_revision`. §D8.3 startup reconciliation
                    // compares `live.toml`'s hash against this, so it must match
                    // what lands on disk.
                    let intended_revision = publish_revision.to_string();
                    let now_ms = now_unix_ms();
                    let id_for_enqueue = id.clone();
                    Self::outbox_append(&outbox, move |ob| {
                        ob.enqueue_pending(
                            id_for_enqueue,
                            provenance_for_row,
                            Some(intended_revision),
                            now_ms,
                        )
                    })
                    .await?;
                    Some((outbox, id))
                }
                None => None,
            };

        // Step 13 (persist): D4.B.1 wires `persist_atomically` against
        // the per-op `PersistTarget`. For MarkKnownGood, the bytes
        // sourced are `publish_config`'s canonical form computed
        // inside the lock — the staged `canonical_bytes` from outside
        // the lock would describe `snapshot_before`, NOT the
        // freshly-confirmed `snapshot_now`. Symmetry with
        // `op_kind_for_known_good`'s TOCTOU fix (Council R1).
        //
        // Failure surfaces as `MutateError::Internal` — we've already
        // passed CAS at step 11 and updated the in-memory snapshot
        // would be inconsistent with disk. Surfacing the I/O failure
        // makes the operator aware; later phases (D4.C) add the
        // audit-outbox degraded mode.
        let (persist_path, persist_bytes) = match persist_target {
            PersistTarget::Live => (self.paths.live.clone(), canonical_bytes),
            PersistTarget::KnownGood => {
                // Serialise `publish_config` (= snapshot_now.config for
                // the Mark hint) inside the lock for TOCTOU symmetry
                // with `op_kind_for_known_good`.
                let bytes = self.canonical.serialise(&publish_config).map_err(|e| {
                    MutateError::Internal(format!(
                        "canonical serialise for known_good persist: {e}"
                    ))
                })?;
                (self.paths.known_good.clone(), bytes)
            }
        };
        if let Err(e) = persist_atomically(&persist_path, &persist_bytes).await {
            // Persist failed after the Pending row was enqueued: append a
            // `Failed` marker (best-effort — the mutation is already aborting,
            // and startup reconciliation §D8.3 resolves a stranded Pending
            // regardless) and surface the original I/O failure.
            if let Some((outbox, id)) = &outbox_pending {
                let id_for_failed = id.clone();
                let now_ms = now_unix_ms();
                if let Err(mark_err) =
                    Self::outbox_append(outbox, move |ob| ob.mark_failed(id_for_failed, now_ms))
                        .await
                {
                    tracing::warn!(
                        "ADR-034 §D8: mark_failed after persist error also failed: {mark_err}"
                    );
                }
            }
            return Err(MutateError::Internal(format!(
                "persist_atomically({}): {e}",
                persist_path.display()
            )));
        }

        // Step 14: build the new snapshot. `checked_add` per KI-A4:
        // overflow is broken-system territory, panic loudly.
        let next_generation = snapshot_now
            .state_generation
            .checked_add(1)
            .ok_or(MutateError::Overflow)?;

        let new_snapshot = Arc::new(LiveConfigSnapshot {
            state_generation: next_generation,
            config: publish_config,
            rules: publish_rules,
            revision: publish_revision,
            known_good_revision: new_known_good,
            applied_at: SystemTime::now(),
        });

        // Step 15: single atomic publish. After this, all readers see
        // the new generation + revision + known_good + config + rules
        // as one consistent Arc.
        self.current.store(Arc::clone(&new_snapshot));

        // Step 16 (mark audit applied, §D8.1): append an `Applied` marker AFTER
        // the atomic publish. A failure here is RECOVERABLE and does NOT fail
        // the (already-published) mutation: startup reconciliation (§D8.3) sees
        // the Pending row plus a `live.toml` whose hash matches the intended
        // revision and promotes it to applied. Log and continue.
        if let Some((outbox, id)) = &outbox_pending {
            let id_for_applied = id.clone();
            let now_ms = now_unix_ms();
            if let Err(e) =
                Self::outbox_append(outbox, move |ob| ob.mark_applied(id_for_applied, now_ms)).await
            {
                tracing::warn!(
                    "ADR-034 §D8: mark_applied failed post-publish (recoverable via \
                     startup reconciliation): {e}"
                );
            }
        }

        // Step 17: return outcome. Use the *publish* revision rather
        // than the staged one — for MarkKnownGood the publish came
        // from snapshot_now, not the staged candidate. CAS guarantees
        // these are equal (snapshot_now == snapshot_before logically
        // when CAS passes), so they're mathematically the same, but
        // sourcing from the published values makes the contract
        // structurally consistent: "the outcome describes what was
        // published." Council R5 fix.
        Ok(MutationOutcome {
            applied_generation: next_generation,
            applied_revision: publish_revision,
            previous_generation: snapshot_now.state_generation,
            previous_revision: snapshot_now.revision,
        })
    }

    /// Per-op candidate construction (step 3 of the 17-step contract).
    ///
    /// Async because the Rollback / RollbackForce branches read
    /// `live.toml.known_good` from disk via `tokio::task::spawn_blocking`
    /// (#1607) — a slow or hung disk on that path used to stall the
    /// tokio worker thread because the prior implementation called
    /// `std::fs::read` synchronously from inside the surrounding async
    /// `mutate()`. The other op branches are pure compute and resolve
    /// without yielding.
    async fn compute_candidate(
        &self,
        op: &ConfigOp,
        snapshot_before: &LiveConfigSnapshot,
    ) -> Result<Config, MutateError> {
        match op {
            ConfigOp::ReplaceWhole { config } => Ok((**config).clone()),
            ConfigOp::MarkKnownGood => {
                // No content change — just promote known_good_revision.
                // Candidate IS the current config.
                Ok((*snapshot_before.config).clone())
            }
            ConfigOp::RollbackForce { reason } if reason.trim().is_empty() => {
                // Defense in depth: IPC framer enforces non-empty per
                // KI-B3, but we re-validate in the daemon so a buggy
                // or compromised IPC path can't bypass the contract.
                // Council R4 fix per DeepSeek.
                Err(MutateError::InvalidOp(
                    "RollbackForce.reason must be non-empty (defense in depth; IPC framer also enforces)".to_string(),
                ))
            }
            ConfigOp::Rollback | ConfigOp::RollbackForce { .. } => {
                // D4.B.4 (#1291): read `live.toml.known_good` from disk
                // and parse it as `Config`. The bytes are the source of
                // truth for "what to roll back to" — the snapshot's
                // `known_good_revision` is only the *content hash* of
                // those bytes (audit-only), so we can't reconstruct the
                // Config tree from the snapshot alone.
                //
                // Error posture (Council R1 from D4.A.2): InvalidOp
                // rather than a silent identity-mutation. Three failure
                // modes surface distinctly so the operator can diagnose:
                //   - file absent → "no known-good snapshot"
                //   - file unreadable → "io error on known_good"
                //   - file present-but-corrupt → "parse error on known_good"
                //
                // #1607: drive the read via `tokio::task::spawn_blocking`
                // so a slow/hung disk can't stall the executor. The
                // closure owns the path clone; the `await` yields back
                // to the runtime, so other tasks make progress while
                // the blocking pool waits on the fs call.
                let _ = snapshot_before;
                let path = self.paths.known_good.clone();
                let display = self.paths.known_good.display().to_string();
                let read_result = tokio::task::spawn_blocking(move || std::fs::read(&path))
                    .await
                    .map_err(|join_err| {
                        // JoinError surfaces here only on a panic inside
                        // the blocking task (cancellation isn't reachable
                        // — we always `.await`). std::fs::read doesn't
                        // panic on its documented inputs, so this is
                        // defensive against runtime bugs.
                        MutateError::InvalidOp(format!(
                            "spawn_blocking for known-good read panicked: {join_err}"
                        ))
                    })?;
                let bytes = match read_result {
                    Ok(b) => b,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        return Err(MutateError::InvalidOp(
                            "no known-good snapshot recorded yet — \
                             run `conductorctl config mark-known-good` \
                             after the current config is validated"
                                .to_string(),
                        ));
                    }
                    Err(e) => {
                        return Err(MutateError::InvalidOp(format!(
                            "io error reading {display}: {e}"
                        )));
                    }
                };
                let text = std::str::from_utf8(&bytes).map_err(|e| {
                    MutateError::InvalidOp(format!("known-good snapshot is not valid utf-8: {e}"))
                })?;
                toml::from_str::<Config>(text).map_err(|e| {
                    MutateError::InvalidOp(format!("known-good snapshot parse failed: {e}"))
                })
            }
        }
    }
}

/// Per-`ConfigOp` persist target. D4.B uses this to dispatch step 13
/// between `live.toml` and `live.toml.known_good`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistTarget {
    Live,
    KnownGood,
}

/// Sent from `mutate()` to `commit_locked()` so the known_good
/// computation happens INSIDE the lock (from `snapshot_now`, not
/// the staged-outside-lock `snapshot_before`). Closes the TOCTOU
/// window described in `commit_locked` docs.
#[derive(Debug, Clone, Copy)]
enum MarkKnownGoodHint {
    /// `ConfigOp::MarkKnownGood` — set `known_good_revision` to
    /// `snapshot_now.revision` (the currently-published bytes the
    /// operator is confirming).
    Mark,
    /// Any other op — inherit `known_good_revision` from `snapshot_now`
    /// at commit time.
    Inherit,
}

impl PersistTarget {
    pub fn from_op(op: &ConfigOp) -> Self {
        match op {
            ConfigOp::MarkKnownGood => PersistTarget::KnownGood,
            _ => PersistTarget::Live,
        }
    }
}
