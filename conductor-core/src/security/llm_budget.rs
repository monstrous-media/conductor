// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-027 **D6** — multi-dimensional LLM budget (the "agent containment
//! shell", paired with D17 egress).
//!
//! A single iteration counter is insufficient against a compromised LLM: the
//! agentic loop has many independent abuse dimensions — iterations, total tool
//! calls, tokens, wall-clock time, and *capability-specific* counts. Per-
//! capability budgets defeat the "one iteration with 500 mutations" bypass.
//!
//! This module is the **pure budget core**: it holds the configured limits
//! ([`LlmBudgetConfig`]) and the mutable per-session counters
//! ([`LlmBudgetState`]), and decides — for each chargeable unit of work —
//! whether the charge stays within budget. It has no I/O and no clock of its
//! own: every time-windowed dimension takes a caller-supplied monotonic
//! `now_ms` so the whole engine is deterministic and exhaustively
//! unit-testable. Call sites (the GUI agentic loop in `llm_commands.rs`; the
//! daemon MCP [`ToolExecutor`](../../../conductor_daemon) dispatch) are
//! responsible for sourcing the clock and emitting the `LlmBudgetExceeded`
//! audit event on a returned [`BudgetExceeded`].
//!
//! ## Config is file-only (ADR-027 D3)
//!
//! Like the D17 egress allowlist, the budget is security-elevating: lowering a
//! limit must not be possible through the GUI / MCP surfaces a compromised LLM
//! can reach. [`LlmBudgetConfig`] therefore lives on the file-only
//! [`SecurityConfig`](crate::security::egress::SecurityConfig) (parsed from the
//! `[security.llm]` table), never on the round-trippable
//! [`Config`](crate::config::Config).
//!
//! ## Limit semantics
//!
//! - A limit of `0` means **disabled** (unlimited) for that dimension. This is
//!   what lets a call site that cannot observe a dimension (e.g. the daemon MCP
//!   executor has no token count) leave it unenforced, and lets operators opt a
//!   dimension out.
//! - A charge is rejected when applying it *would exceed* the limit: with
//!   `max_iterations_per_turn = 10`, the 11th iteration is refused; 10 are
//!   allowed. The counter is **not** advanced on a rejected charge.
//!
//! ## Council R1 additions
//!
//! - **Token burst rate** ([`LlmBudgetConfig::max_tokens_per_60sec`]) — a
//!   sliding-60s window that catches rapid-fire token exhaustion which evades
//!   the per-session token cap.
//!
//! Two further Council R1 asks — a process-level wall-clock *watchdog* (a
//! dead-man-switch independent of the cooperative `max_wall_clock_*` checks)
//! and *hierarchical daily* limits nested above the session — are deliberately
//! out of scope for this module: both need cross-session / process-supervisor
//! state that does not belong in a pure, single-session engine. They are
//! tracked as follow-ups (see the PR description / ADR-027 §D6).

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

/// Number of milliseconds in the rolling rate-limit window (60 s).
const WINDOW_MS: u64 = 60_000;

/// Termination behaviour when a budget dimension is exhausted
/// (`on_budget_exceeded` in `[security.llm]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetExceededBehaviour {
    /// Freeze the agentic loop and surface the violation to the user for an
    /// explicit decision. The safe, recoverable default.
    #[default]
    FreezeAndPrompt,
    /// Abort the loop immediately with no prompt.
    HardStop,
}

/// Which budget dimension a charge tripped. Rendered into the
/// `LlmBudgetExceeded` audit event by the call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetDimension {
    /// `max_iterations_per_turn`
    IterationsPerTurn,
    /// `max_iterations_per_session`
    IterationsPerSession,
    /// `max_tool_calls_per_turn`
    ToolCallsPerTurn,
    /// `max_tool_calls_per_session`
    ToolCallsPerSession,
    /// `max_tokens_in_per_session`
    TokensInPerSession,
    /// `max_tokens_out_per_session`
    TokensOutPerSession,
    /// `max_wall_clock_seconds_per_turn`
    WallClockPerTurn,
    /// `max_wall_clock_seconds_per_session`
    WallClockPerSession,
    /// `max_config_changes_per_session`
    ConfigChangesPerSession,
    /// `max_shell_exec_per_session`
    ShellExecPerSession,
    /// `max_network_tool_calls_per_session`
    NetworkToolCallsPerSession,
    /// `max_midi_out_per_session`
    MidiOutPerSession,
    /// `max_confirmations_requested_per_minute`
    ConfirmationsPerMinute,
    /// `max_tokens_per_60sec` (Council burst-rate addition)
    TokenBurst,
}

impl BudgetDimension {
    /// Stable snake_case identifier, suitable for the audit log and the CLI.
    pub fn as_str(&self) -> &'static str {
        match self {
            BudgetDimension::IterationsPerTurn => "max_iterations_per_turn",
            BudgetDimension::IterationsPerSession => "max_iterations_per_session",
            BudgetDimension::ToolCallsPerTurn => "max_tool_calls_per_turn",
            BudgetDimension::ToolCallsPerSession => "max_tool_calls_per_session",
            BudgetDimension::TokensInPerSession => "max_tokens_in_per_session",
            BudgetDimension::TokensOutPerSession => "max_tokens_out_per_session",
            BudgetDimension::WallClockPerTurn => "max_wall_clock_seconds_per_turn",
            BudgetDimension::WallClockPerSession => "max_wall_clock_seconds_per_session",
            BudgetDimension::ConfigChangesPerSession => "max_config_changes_per_session",
            BudgetDimension::ShellExecPerSession => "max_shell_exec_per_session",
            BudgetDimension::NetworkToolCallsPerSession => "max_network_tool_calls_per_session",
            BudgetDimension::MidiOutPerSession => "max_midi_out_per_session",
            BudgetDimension::ConfirmationsPerMinute => "max_confirmations_requested_per_minute",
            BudgetDimension::TokenBurst => "max_tokens_per_60sec",
        }
    }
}

impl std::fmt::Display for BudgetDimension {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A charge that would exceed its dimension's configured limit. The counter is
/// left unchanged when this is returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetExceeded {
    /// The dimension that tripped.
    pub dimension: BudgetDimension,
    /// The configured limit for that dimension.
    pub limit: u64,
    /// The value the charge would have reached (`> limit`).
    pub observed: u64,
}

impl std::fmt::Display for BudgetExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "LLM budget exceeded: {} would reach {} (limit {})",
            self.dimension, self.observed, self.limit
        )
    }
}

impl std::error::Error for BudgetExceeded {}

/// `[security.llm]` budget configuration block. File-only by construction (a
/// field on [`SecurityConfig`](crate::security::egress::SecurityConfig), never
/// on the round-trippable [`Config`](crate::config::Config)).
///
/// `#[serde(default)]` on the struct means any field absent from the TOML is
/// filled from [`LlmBudgetConfig::default`] — i.e. the ADR-027 §D6 defaults —
/// so a partial `[security.llm]` block tightens only the limits it names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LlmBudgetConfig {
    /// Max agentic-loop iterations within a single user turn.
    pub max_iterations_per_turn: u32,
    /// Max agentic-loop iterations across the whole session.
    pub max_iterations_per_session: u32,
    /// Max tool calls within a single user turn.
    pub max_tool_calls_per_turn: u32,
    /// Max tool calls across the whole session.
    pub max_tool_calls_per_session: u32,
    /// Max input (prompt) tokens across the whole session.
    pub max_tokens_in_per_session: u64,
    /// Max output (completion) tokens across the whole session.
    pub max_tokens_out_per_session: u64,
    /// Max wall-clock seconds for a single user turn.
    pub max_wall_clock_seconds_per_turn: u64,
    /// Max wall-clock seconds for the whole session.
    pub max_wall_clock_seconds_per_session: u64,
    /// Max ConfigChange-tier tool calls per session.
    pub max_config_changes_per_session: u32,
    /// Max shell-executing tool calls per session.
    pub max_shell_exec_per_session: u32,
    /// Max network-capable tool calls per session.
    pub max_network_tool_calls_per_session: u32,
    /// Max MIDI / HardwareIO output messages per session.
    pub max_midi_out_per_session: u32,
    /// Max user confirmations requested per rolling minute (anti-fatigue).
    pub max_confirmations_requested_per_minute: u32,
    /// Council burst-rate addition: max tokens (in + out) per rolling 60 s.
    pub max_tokens_per_60sec: u64,
    /// What to do when any dimension is exhausted.
    pub on_budget_exceeded: BudgetExceededBehaviour,
}

impl Default for LlmBudgetConfig {
    /// ADR-027 §D6 default budget. Every dimension is non-zero (enforced); set
    /// any to `0` in the config file to disable that dimension.
    fn default() -> Self {
        Self {
            max_iterations_per_turn: 10,
            max_iterations_per_session: 50,
            max_tool_calls_per_turn: 20,
            max_tool_calls_per_session: 200,
            max_tokens_in_per_session: 500_000,
            max_tokens_out_per_session: 100_000,
            max_wall_clock_seconds_per_turn: 120,
            max_wall_clock_seconds_per_session: 900,
            max_config_changes_per_session: 5,
            max_shell_exec_per_session: 10,
            max_network_tool_calls_per_session: 3,
            max_midi_out_per_session: 100,
            max_confirmations_requested_per_minute: 3,
            max_tokens_per_60sec: 100_000,
            on_budget_exceeded: BudgetExceededBehaviour::FreezeAndPrompt,
        }
    }
}

/// Mutable per-session budget counters bound to a [`LlmBudgetConfig`].
///
/// One instance per LLM session. Per-turn dimensions are reset by
/// [`LlmBudgetState::begin_turn`]; per-session dimensions accumulate for the
/// instance's lifetime. Time-windowed dimensions (wall-clock, confirmations,
/// token burst) are driven by a caller-supplied monotonic `now_ms`.
#[derive(Debug, Clone)]
pub struct LlmBudgetState {
    config: LlmBudgetConfig,

    session_start_ms: u64,
    turn_start_ms: u64,

    iterations_turn: u32,
    iterations_session: u32,
    tool_calls_turn: u32,
    tool_calls_session: u32,
    tokens_in_session: u64,
    tokens_out_session: u64,
    config_changes_session: u32,
    shell_exec_session: u32,
    network_calls_session: u32,
    midi_out_session: u32,

    /// Timestamps (ms) of confirmation requests within the rolling window.
    confirmations: VecDeque<u64>,
    /// `(timestamp_ms, tokens)` charges within the rolling burst window.
    token_burst: VecDeque<(u64, u64)>,
    /// Running sum of `token_burst`'s token values, maintained incrementally
    /// (add on push, subtract on prune) so a burst check is O(1) rather than
    /// O(N) over the deque — otherwise granular 1-token charges against the
    /// 100k-default window are an algorithmic CPU-DoS vector.
    token_burst_sum: u64,
}

impl LlmBudgetState {
    /// Start a fresh session at `now_ms`. The first turn is implicitly open
    /// (no `begin_turn` needed before the first iteration).
    pub fn new(config: LlmBudgetConfig, now_ms: u64) -> Self {
        Self {
            config,
            session_start_ms: now_ms,
            turn_start_ms: now_ms,
            iterations_turn: 0,
            iterations_session: 0,
            tool_calls_turn: 0,
            tool_calls_session: 0,
            tokens_in_session: 0,
            tokens_out_session: 0,
            config_changes_session: 0,
            shell_exec_session: 0,
            network_calls_session: 0,
            midi_out_session: 0,
            confirmations: VecDeque::new(),
            token_burst: VecDeque::new(),
            token_burst_sum: 0,
        }
    }

    /// The configured budget.
    pub fn config(&self) -> &LlmBudgetConfig {
        &self.config
    }

    /// Behaviour to apply when a charge is rejected.
    pub fn on_exceeded(&self) -> BudgetExceededBehaviour {
        self.config.on_budget_exceeded
    }

    /// Begin a new user turn at `now_ms`: reset per-turn counters and the
    /// per-turn wall-clock anchor. Per-session state is untouched.
    pub fn begin_turn(&mut self, now_ms: u64) {
        self.iterations_turn = 0;
        self.tool_calls_turn = 0;
        self.turn_start_ms = now_ms;
    }

    /// Charge one agentic-loop iteration (per-turn and per-session). Both caps
    /// are checked before either counter advances, so a rejected charge leaves
    /// the state untouched.
    pub fn charge_iteration(&mut self) -> Result<(), BudgetExceeded> {
        let turn = u64::from(self.iterations_turn) + 1;
        let session = u64::from(self.iterations_session) + 1;
        check(
            u64::from(self.config.max_iterations_per_turn),
            turn,
            BudgetDimension::IterationsPerTurn,
        )?;
        check(
            u64::from(self.config.max_iterations_per_session),
            session,
            BudgetDimension::IterationsPerSession,
        )?;
        self.iterations_turn = self.iterations_turn.saturating_add(1);
        self.iterations_session = self.iterations_session.saturating_add(1);
        Ok(())
    }

    /// Charge one tool call (per-turn and per-session).
    pub fn charge_tool_call(&mut self) -> Result<(), BudgetExceeded> {
        let turn = u64::from(self.tool_calls_turn) + 1;
        let session = u64::from(self.tool_calls_session) + 1;
        check(
            u64::from(self.config.max_tool_calls_per_turn),
            turn,
            BudgetDimension::ToolCallsPerTurn,
        )?;
        check(
            u64::from(self.config.max_tool_calls_per_session),
            session,
            BudgetDimension::ToolCallsPerSession,
        )?;
        self.tool_calls_turn = self.tool_calls_turn.saturating_add(1);
        self.tool_calls_session = self.tool_calls_session.saturating_add(1);
        Ok(())
    }

    /// Charge one ConfigChange-tier tool call.
    pub fn charge_config_change(&mut self) -> Result<(), BudgetExceeded> {
        let next = u64::from(self.config_changes_session) + 1;
        check(
            u64::from(self.config.max_config_changes_per_session),
            next,
            BudgetDimension::ConfigChangesPerSession,
        )?;
        self.config_changes_session = self.config_changes_session.saturating_add(1);
        Ok(())
    }

    /// Charge one shell-executing tool call.
    pub fn charge_shell_exec(&mut self) -> Result<(), BudgetExceeded> {
        let next = u64::from(self.shell_exec_session) + 1;
        check(
            u64::from(self.config.max_shell_exec_per_session),
            next,
            BudgetDimension::ShellExecPerSession,
        )?;
        self.shell_exec_session = self.shell_exec_session.saturating_add(1);
        Ok(())
    }

    /// Charge one network-capable tool call.
    pub fn charge_network_call(&mut self) -> Result<(), BudgetExceeded> {
        let next = u64::from(self.network_calls_session) + 1;
        check(
            u64::from(self.config.max_network_tool_calls_per_session),
            next,
            BudgetDimension::NetworkToolCallsPerSession,
        )?;
        self.network_calls_session = self.network_calls_session.saturating_add(1);
        Ok(())
    }

    /// Charge `n` MIDI / HardwareIO output messages.
    pub fn charge_midi_out(&mut self, n: u32) -> Result<(), BudgetExceeded> {
        let next = u64::from(self.midi_out_session) + u64::from(n);
        check(
            u64::from(self.config.max_midi_out_per_session),
            next,
            BudgetDimension::MidiOutPerSession,
        )?;
        self.midi_out_session = self.midi_out_session.saturating_add(n);
        Ok(())
    }

    /// Charge token usage: the per-session input/output caps plus the rolling
    /// burst window (`now_ms` drives the window). Checked before any counter
    /// advances.
    pub fn charge_tokens(
        &mut self,
        now_ms: u64,
        tokens_in: u64,
        tokens_out: u64,
    ) -> Result<(), BudgetExceeded> {
        // Saturating arithmetic throughout: an overflowing charge must
        // *saturate* (→ `> limit` → blocked = fail-closed), never wrap to a
        // small value that slips under a cap (fail-open) — and never panic in
        // debug (a DoS). All sums below are attacker-influenced (token counts).
        check(
            self.config.max_tokens_in_per_session,
            self.tokens_in_session.saturating_add(tokens_in),
            BudgetDimension::TokensInPerSession,
        )?;
        check(
            self.config.max_tokens_out_per_session,
            self.tokens_out_session.saturating_add(tokens_out),
            BudgetDimension::TokensOutPerSession,
        )?;
        let total = tokens_in.saturating_add(tokens_out);
        // Burst window: maintained ONLY when the dimension is enabled AND the
        // charge is non-zero. Zero-token charges can't change the running sum,
        // and a disabled (`0`) limit never blocks — buffering either would let
        // an attacker grow the deque without bound (memory DoS). When enabled,
        // a charge that would exceed the cap is rejected *before* the push, so
        // the live deque is bounded by `max_tokens_per_60sec` entries.
        if self.config.max_tokens_per_60sec != 0 && total > 0 {
            // Prune expired charges, decrementing the running sum as we go, so
            // the in-window total is O(1) to read (each entry is pushed and
            // popped at most once → amortised O(1) per charge). Re-folding the
            // deque here would be O(N) per charge — an algorithmic DoS.
            let cutoff = now_ms.saturating_sub(WINDOW_MS);
            while let Some(&(t, n)) = self.token_burst.front() {
                if t >= cutoff {
                    break;
                }
                self.token_burst.pop_front();
                self.token_burst_sum = self.token_burst_sum.saturating_sub(n);
            }
            check(
                self.config.max_tokens_per_60sec,
                self.token_burst_sum.saturating_add(total),
                BudgetDimension::TokenBurst,
            )?;
            self.token_burst.push_back((now_ms, total));
            self.token_burst_sum = self.token_burst_sum.saturating_add(total);
        }
        self.tokens_in_session = self.tokens_in_session.saturating_add(tokens_in);
        self.tokens_out_session = self.tokens_out_session.saturating_add(tokens_out);
        Ok(())
    }

    /// Charge one user-confirmation request against the rolling-minute cap.
    pub fn charge_confirmation(&mut self, now_ms: u64) -> Result<(), BudgetExceeded> {
        // Disabled (`0`) → never blocks, so don't buffer the window (the live
        // deque would grow without bound; an enabled limit caps it because a
        // rejected charge is not pushed).
        if self.config.max_confirmations_requested_per_minute == 0 {
            return Ok(());
        }
        Self::prune_window(&mut self.confirmations, now_ms);
        let next = self.confirmations.len() as u64 + 1;
        check(
            u64::from(self.config.max_confirmations_requested_per_minute),
            next,
            BudgetDimension::ConfirmationsPerMinute,
        )?;
        self.confirmations.push_back(now_ms);
        Ok(())
    }

    /// Check the wall-clock caps for the current turn and the session. Pure
    /// read — call before dispatching more work. The turn cap is checked first.
    pub fn check_wall_clock(&self, now_ms: u64) -> Result<(), BudgetExceeded> {
        let turn_secs = now_ms.saturating_sub(self.turn_start_ms).div_ceil(1000);
        check(
            self.config.max_wall_clock_seconds_per_turn,
            turn_secs,
            BudgetDimension::WallClockPerTurn,
        )?;
        let session_secs = now_ms.saturating_sub(self.session_start_ms).div_ceil(1000);
        check(
            self.config.max_wall_clock_seconds_per_session,
            session_secs,
            BudgetDimension::WallClockPerSession,
        )?;
        Ok(())
    }

    fn prune_window(window: &mut VecDeque<u64>, now_ms: u64) {
        let cutoff = now_ms.saturating_sub(WINDOW_MS);
        while window.front().is_some_and(|&t| t < cutoff) {
            window.pop_front();
        }
    }
}

/// Reject the charge when `would_be` overshoots a non-zero `limit`. A `limit`
/// of `0` disables the dimension (always `Ok`). The counter advance is the
/// caller's responsibility and happens only after this returns `Ok`.
fn check(limit: u64, would_be: u64, dimension: BudgetDimension) -> Result<(), BudgetExceeded> {
    if limit != 0 && would_be > limit {
        return Err(BudgetExceeded {
            dimension,
            limit,
            observed: would_be,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unlimited() -> LlmBudgetConfig {
        // Every dimension disabled — a baseline to enable one at a time.
        LlmBudgetConfig {
            max_iterations_per_turn: 0,
            max_iterations_per_session: 0,
            max_tool_calls_per_turn: 0,
            max_tool_calls_per_session: 0,
            max_tokens_in_per_session: 0,
            max_tokens_out_per_session: 0,
            max_wall_clock_seconds_per_turn: 0,
            max_wall_clock_seconds_per_session: 0,
            max_config_changes_per_session: 0,
            max_shell_exec_per_session: 0,
            max_network_tool_calls_per_session: 0,
            max_midi_out_per_session: 0,
            max_confirmations_requested_per_minute: 0,
            max_tokens_per_60sec: 0,
            on_budget_exceeded: BudgetExceededBehaviour::HardStop,
        }
    }

    #[test]
    fn defaults_match_adr() {
        let c = LlmBudgetConfig::default();
        assert_eq!(c.max_iterations_per_turn, 10);
        assert_eq!(c.max_iterations_per_session, 50);
        assert_eq!(c.max_tool_calls_per_session, 200);
        assert_eq!(c.max_tokens_in_per_session, 500_000);
        assert_eq!(c.max_config_changes_per_session, 5);
        assert_eq!(c.max_network_tool_calls_per_session, 3);
        assert_eq!(
            c.on_budget_exceeded,
            BudgetExceededBehaviour::FreezeAndPrompt
        );
    }

    #[test]
    fn iterations_per_turn_blocks_after_limit() {
        let mut cfg = unlimited();
        cfg.max_iterations_per_turn = 3;
        let mut st = LlmBudgetState::new(cfg, 0);
        assert!(st.charge_iteration().is_ok());
        assert!(st.charge_iteration().is_ok());
        assert!(st.charge_iteration().is_ok());
        let err = st.charge_iteration().unwrap_err();
        assert_eq!(err.dimension, BudgetDimension::IterationsPerTurn);
        assert_eq!(err.limit, 3);
        assert_eq!(err.observed, 4);
    }

    #[test]
    fn rejected_charge_does_not_advance_counter() {
        let mut cfg = unlimited();
        cfg.max_tool_calls_per_session = 1;
        let mut st = LlmBudgetState::new(cfg, 0);
        assert!(st.charge_tool_call().is_ok());
        assert!(st.charge_tool_call().is_err());
        // A second rejected charge must report the SAME observed value (2),
        // proving the counter did not advance on the first rejection.
        let err = st.charge_tool_call().unwrap_err();
        assert_eq!(err.observed, 2);
    }

    #[test]
    fn begin_turn_resets_per_turn_but_not_session() {
        let mut cfg = unlimited();
        cfg.max_iterations_per_turn = 2;
        cfg.max_iterations_per_session = 3;
        let mut st = LlmBudgetState::new(cfg, 0);
        assert!(st.charge_iteration().is_ok());
        assert!(st.charge_iteration().is_ok());
        assert!(st.charge_iteration().is_err()); // per-turn cap (2)
        st.begin_turn(1000);
        // Per-turn counter reset → allowed again, but per-session (3) is next.
        assert!(st.charge_iteration().is_ok()); // session now at 3
        let err = st.charge_iteration().unwrap_err();
        assert_eq!(err.dimension, BudgetDimension::IterationsPerSession);
    }

    #[test]
    fn zero_limit_means_unlimited() {
        let mut st = LlmBudgetState::new(unlimited(), 0);
        for _ in 0..10_000 {
            assert!(st.charge_iteration().is_ok());
            assert!(st.charge_tool_call().is_ok());
        }
    }

    #[test]
    fn config_change_and_network_and_shell_caps() {
        let mut cfg = unlimited();
        cfg.max_config_changes_per_session = 1;
        cfg.max_network_tool_calls_per_session = 2;
        cfg.max_shell_exec_per_session = 0;
        let mut st = LlmBudgetState::new(cfg, 0);
        assert!(st.charge_config_change().is_ok());
        assert_eq!(
            st.charge_config_change().unwrap_err().dimension,
            BudgetDimension::ConfigChangesPerSession
        );
        assert!(st.charge_network_call().is_ok());
        assert!(st.charge_network_call().is_ok());
        assert!(st.charge_network_call().is_err());
        // shell disabled → never blocks
        for _ in 0..100 {
            assert!(st.charge_shell_exec().is_ok());
        }
    }

    #[test]
    fn midi_out_charges_in_bulk() {
        let mut cfg = unlimited();
        cfg.max_midi_out_per_session = 100;
        let mut st = LlmBudgetState::new(cfg, 0);
        assert!(st.charge_midi_out(100).is_ok());
        let err = st.charge_midi_out(1).unwrap_err();
        assert_eq!(err.dimension, BudgetDimension::MidiOutPerSession);
        assert_eq!(err.observed, 101);
    }

    #[test]
    fn tokens_in_and_out_capped_separately() {
        let mut cfg = unlimited();
        cfg.max_tokens_in_per_session = 1000;
        cfg.max_tokens_out_per_session = 500;
        let mut st = LlmBudgetState::new(cfg, 0);
        assert!(st.charge_tokens(0, 1000, 500).is_ok());
        // Next input token over budget.
        assert_eq!(
            st.charge_tokens(1, 1, 0).unwrap_err().dimension,
            BudgetDimension::TokensInPerSession
        );
        // Output over budget independently.
        assert_eq!(
            st.charge_tokens(1, 0, 1).unwrap_err().dimension,
            BudgetDimension::TokensOutPerSession
        );
    }

    #[test]
    fn token_burst_window_slides() {
        let mut cfg = unlimited();
        cfg.max_tokens_per_60sec = 1000;
        let mut st = LlmBudgetState::new(cfg, 0);
        assert!(st.charge_tokens(0, 600, 0).is_ok()); // 600 in window
        assert!(st.charge_tokens(10_000, 400, 0).is_ok()); // 1000 in window
        // Still inside the 60s window → burst trips.
        assert_eq!(
            st.charge_tokens(20_000, 1, 0).unwrap_err().dimension,
            BudgetDimension::TokenBurst
        );
        // Advance past 60s from the first two charges → window empties.
        assert!(st.charge_tokens(75_000, 900, 0).is_ok());
    }

    #[test]
    fn token_charge_saturates_instead_of_overflowing() {
        // Council R-high blocker: per-session token caps disabled, burst
        // enabled. An attacker-supplied near-u64::MAX charge must SATURATE
        // (block), not wrap under the burst cap (fail-open) — and must not
        // panic (debug-mode DoS).
        let mut cfg = unlimited();
        cfg.max_tokens_per_60sec = 1000;
        let mut st = LlmBudgetState::new(cfg, 0);
        let err = st.charge_tokens(0, u64::MAX, 1).unwrap_err();
        assert_eq!(err.dimension, BudgetDimension::TokenBurst);
    }

    #[test]
    fn session_token_counter_saturates() {
        // With the per-session input cap disabled, repeated max charges must
        // saturate the accumulator, never wrap to a small (fail-open) value.
        let mut cfg = unlimited();
        cfg.max_tokens_in_per_session = 0; // disabled
        let mut st = LlmBudgetState::new(cfg, 0);
        assert!(st.charge_tokens(0, u64::MAX, 0).is_ok());
        assert!(st.charge_tokens(1, u64::MAX, 0).is_ok());
        assert_eq!(st.tokens_in_session, u64::MAX);
    }

    #[test]
    fn disabled_burst_window_does_not_grow() {
        // Council R-high blocker: with the burst dimension disabled (0), the
        // window must not buffer at all (unbounded-memory DoS otherwise).
        let mut st = LlmBudgetState::new(unlimited(), 0); // burst = 0
        for i in 0..10_000 {
            assert!(st.charge_tokens(i, 5, 5).is_ok());
        }
        assert!(st.token_burst.is_empty(), "disabled burst must not buffer");
    }

    #[test]
    fn zero_token_charges_do_not_grow_window() {
        // Zero-sized charges can't change the running sum, so they must not be
        // buffered — otherwise they grow the deque unboundedly within 60 s.
        let mut cfg = unlimited();
        cfg.max_tokens_per_60sec = 1000;
        let mut st = LlmBudgetState::new(cfg, 0);
        for i in 0..10_000 {
            assert!(st.charge_tokens(i, 0, 0).is_ok());
        }
        assert!(st.token_burst.is_empty(), "zero charges must not buffer");
    }

    #[test]
    fn burst_running_sum_tracks_window_across_slides() {
        // The O(1) running sum must subtract entries as they age out, so it
        // always equals the in-window total — not the cumulative total. A
        // missing subtract-on-prune would wrongly keep blocking after a slide.
        let mut cfg = unlimited();
        cfg.max_tokens_per_60sec = 100;
        let mut st = LlmBudgetState::new(cfg, 0);
        assert!(st.charge_tokens(0, 100, 0).is_ok()); // window full at 100
        assert!(st.charge_tokens(1, 1, 0).is_err()); // 100 + 1 > 100
        assert_eq!(st.token_burst_sum, 100);
        // Slide past 60 s: the t=0 entry ages out, sum must drop to 0.
        assert!(st.charge_tokens(60_001, 100, 0).is_ok());
        assert_eq!(st.token_burst_sum, 100); // only the new in-window entry
        assert_eq!(st.token_burst.len(), 1);
    }

    #[test]
    fn enabled_burst_window_bounded_by_limit() {
        // When enabled, once the running sum hits the cap the charge is
        // rejected BEFORE pushing — so the deque can never exceed `limit`.
        let mut cfg = unlimited();
        cfg.max_tokens_per_60sec = 10;
        let mut st = LlmBudgetState::new(cfg, 0);
        for i in 0..10 {
            assert!(st.charge_tokens(i, 1, 0).is_ok());
        }
        assert!(st.charge_tokens(11, 1, 0).is_err());
        assert!(st.token_burst.len() <= 10);
    }

    #[test]
    fn disabled_confirmations_window_does_not_grow() {
        let mut st = LlmBudgetState::new(unlimited(), 0); // confirmations = 0
        for i in 0..10_000 {
            assert!(st.charge_confirmation(i).is_ok());
        }
        assert!(st.confirmations.is_empty());
    }

    #[test]
    fn capability_counters_saturate() {
        // Disabled capability dims must saturate their counters, not wrap.
        let mut st = LlmBudgetState::new(unlimited(), 0);
        assert!(st.charge_midi_out(u32::MAX).is_ok());
        assert!(st.charge_midi_out(u32::MAX).is_ok());
        assert_eq!(st.midi_out_session, u32::MAX);
    }

    #[test]
    fn confirmations_per_minute_sliding() {
        let mut cfg = unlimited();
        cfg.max_confirmations_requested_per_minute = 2;
        let mut st = LlmBudgetState::new(cfg, 0);
        assert!(st.charge_confirmation(0).is_ok());
        assert!(st.charge_confirmation(1000).is_ok());
        assert_eq!(
            st.charge_confirmation(2000).unwrap_err().dimension,
            BudgetDimension::ConfirmationsPerMinute
        );
        // After the first two age out (> 60s), a new one is allowed.
        assert!(st.charge_confirmation(61_001).is_ok());
    }

    #[test]
    fn wall_clock_turn_and_session() {
        let mut cfg = unlimited();
        cfg.max_wall_clock_seconds_per_turn = 120;
        cfg.max_wall_clock_seconds_per_session = 900;
        let mut st = LlmBudgetState::new(cfg, 1_000);
        assert!(st.check_wall_clock(1_000 + 120_000).is_ok()); // exactly 120s turn
        assert_eq!(
            st.check_wall_clock(1_000 + 120_001).unwrap_err().dimension,
            BudgetDimension::WallClockPerTurn
        );
        // New turn resets the turn anchor; session cap still measured from start.
        st.begin_turn(1_000 + 800_000);
        assert!(st.check_wall_clock(1_000 + 850_000).is_ok());
        assert_eq!(
            st.check_wall_clock(1_000 + 900_001).unwrap_err().dimension,
            BudgetDimension::WallClockPerSession
        );
    }

    #[test]
    fn security_table_parses_llm_block_file_only() {
        // The `[security.llm]` table flows through the same file-only parser as
        // `[security.egress]` (D17) — never via the round-trippable Config.
        let toml = r#"
            [security.egress]
            allowlist_mode = "strict"

            [security.llm]
            max_iterations_per_session = 7
            max_tokens_per_60sec = 0
        "#;
        let sec = crate::security::egress::SecurityConfig::from_toml_str(toml).unwrap();
        assert_eq!(sec.llm.max_iterations_per_session, 7);
        assert_eq!(sec.llm.max_tokens_per_60sec, 0); // burst disabled
        assert_eq!(sec.llm.max_iterations_per_turn, 10); // default retained
    }

    #[test]
    fn missing_security_llm_block_yields_adr_defaults() {
        let sec = crate::security::egress::SecurityConfig::from_toml_str("").unwrap();
        assert_eq!(sec.llm, LlmBudgetConfig::default());
    }

    #[test]
    fn parses_partial_security_llm_block_with_defaults() {
        // A partial block tightens only what it names; the rest stay at ADR
        // defaults via `#[serde(default)]`.
        let toml = r#"
            max_iterations_per_turn = 4
            on_budget_exceeded = "hard_stop"
        "#;
        let cfg: LlmBudgetConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.max_iterations_per_turn, 4);
        assert_eq!(cfg.on_budget_exceeded, BudgetExceededBehaviour::HardStop);
        // Untouched → default.
        assert_eq!(cfg.max_tool_calls_per_session, 200);
    }
}
