// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! The tier-dispatching execute entry point and device-data fetch.

use super::*;

impl ToolExecutor {
    /// Execute a tool call with risk tier handling
    ///
    /// # Arguments
    /// * `tool_name` - Name of the tool to execute
    /// * `arguments` - Tool arguments as JSON
    /// * `caller_ctx` - ADR-027 D1-pinned peer identity from the IPC
    ///   accept loop. **Every call goes through
    ///   `security::gate::enforce`**. When `Some`,
    ///   the supplied trust band drives the decision table directly.
    ///   When `None`, the method substitutes
    ///   `CallerContext::synthetic_unpinned()` (trust band
    ///   `Untrusted`) so an unpinned IPC peer is denied for
    ///   anything beyond `ReadOnly` / `Stateful` /
    ///   `ArtifactRender`. Daemon-internal callers that have
    ///   already been gate-checked at an outer boundary should
    ///   pass `Some(CallerContext::internal_trusted())` to be
    ///   admitted as `GuiTrusted`. Lib unit-test builds keep
    ///   `SecurityPolicy::default().shadow_mode = true` via
    ///   `cfg(test)` so the existing fixture pattern of passing
    ///   `None` still produces today's behaviour for the
    ///   ToolExecutor unit tests; production builds enforce.
    ///
    /// # Returns
    /// - ReadOnly tools: `ExecutionResult::Success`
    /// - Stateful tools: `ExecutionResult::Logged`
    /// - ConfigChange tools: `ExecutionResult::PlanCreated`
    /// - HardwareIO tools: `ExecutionResult::HardwareIoConfirmation`
    /// - Rate limited: `ExecutionResult::RateLimited`
    /// - Gate denial: `ExecutionResult::Error` with the
    ///   `DenialReason` rendered into the message
    pub async fn execute(
        &self,
        tool_name: &str,
        arguments: Option<Value>,
        caller_ctx: Option<&crate::security::CallerContext>,
    ) -> ExecutionResult {
        let risk_tier = get_tool_risk_tier(tool_name);
        debug!(
            "Executing tool '{}' with risk tier {:?}",
            tool_name, risk_tier
        );

        // ADR-027 D5/D1 wiring: run the admission sequence — security
        // gate → rate limiter → LLM budget, in that order, each with
        // audit-on-deny — before any tool work. The sequence lives in
        // `admit_with_policy` so the engine-manager direct-dispatch arms
        // (which cannot route through this method without deadlocking on
        // the daemon command channel) run the identical checks; see that
        // method for the ordering rationale and the None-ctx semantics.
        // This call site wires `RequirePlan` / `RequireConfirmation` to
        // the existing per-tier handlers, activates enforcement
        // (`SecurityPolicy::default().shadow_mode = false` in
        // production), and closes the gate-bypass-on-`None` gap.
        let gate_route = match self
            .admit_with_policy(
                tool_name,
                caller_ctx,
                &crate::security::SecurityPolicy::default(),
            )
            .await
        {
            Ok(route) => route,
            Err(denial) => return denial,
        };

        // Now route per the gate decision (if any). Gate-routed
        // paths still hit `execute_config_change` /
        // `execute_hardware_io` — same handlers the per-tier
        // dispatch below would have invoked, just selected via
        // gate decision instead of `risk_tier` matching.
        match gate_route {
            AdmitRoute::ToConfigChange => {
                return self.execute_config_change(tool_name, arguments).await;
            }
            AdmitRoute::ToHardwareIo => {
                return self.execute_hardware_io(tool_name, arguments).await;
            }
            AdmitRoute::FallThrough => {
                // Fall through to per-tier dispatch below.
            }
        }

        // Per-tier dispatch (no gate consulted, or gate said
        // Allow / AllowWithAudit).
        match risk_tier {
            ToolRiskTier::ReadOnly => self.execute_readonly(tool_name, arguments).await,
            ToolRiskTier::Stateful | ToolRiskTier::ArtifactRender => {
                self.execute_stateful(tool_name, arguments).await
            }
            ToolRiskTier::ConfigChange => self.execute_config_change(tool_name, arguments).await,
            ToolRiskTier::HardwareIO => self.execute_hardware_io(tool_name, arguments).await,
            ToolRiskTier::Privileged => ExecutionResult::Error {
                message: format!(
                    "Tool '{}' has risk tier {:?} which is not yet supported",
                    tool_name, risk_tier
                ),
            },
        }
    }

    /// ADR-027 D5/D1 — the admission sequence every MCP tool call must
    /// pass before any tool work: security gate, then rate limiter, then
    /// LLM budget, each with audit-on-deny. Factored out of
    /// [`Self::execute`] so the engine-manager direct-dispatch arms
    /// (`conductor_switch_profile`, `conductor_probe_device_identity` —
    /// which bypass `execute` because its `command_tx` round-trip cannot
    /// resolve while `handle_ipc_request` runs inside the command_rx
    /// select arm) run the identical checks instead of skipping them.
    ///
    /// Ordering:
    ///   1. Gate first. `Deny` returns immediately without consuming
    ///      rate-limit quota — denials shouldn't waste throttling budget,
    ///      and the denial reason is more informative than `RateLimited`
    ///      would be.
    ///   2. Rate-limit check (applies to every path that passes the gate).
    ///   3. LLM budget (when attached): charged only once the gate and
    ///      rate-limiter have admitted the call — a denied/throttled call
    ///      shouldn't consume budget.
    ///
    /// Returns the gate's routing choice on admission, or the
    /// `ExecutionResult` to send back on denial.
    pub(crate) async fn admit(
        &self,
        tool_name: &str,
        caller_ctx: Option<&crate::security::CallerContext>,
    ) -> Result<AdmitRoute, ExecutionResult> {
        // `SecurityPolicy::default()` has `shadow_mode = false` outside
        // `cfg(test)` — the gate's decision table is enforced in
        // production. Lib unit tests get shadow mode so fixtures passing
        // `None` keep their behaviour; the enforcing-policy semantics are
        // pinned in `tests/admission.rs` via `admit_with_policy`.
        self.admit_with_policy(
            tool_name,
            caller_ctx,
            &crate::security::SecurityPolicy::default(),
        )
        .await
    }

    /// [`Self::admit`] with an explicit [`SecurityPolicy`] — the testable
    /// core (unit-test builds flip `SecurityPolicy::default()` to shadow
    /// mode, so gate-denial semantics are only observable through this
    /// variant).
    ///
    /// [`SecurityPolicy`]: crate::security::SecurityPolicy
    pub(crate) async fn admit_with_policy(
        &self,
        tool_name: &str,
        caller_ctx: Option<&crate::security::CallerContext>,
        policy: &crate::security::SecurityPolicy,
    ) -> Result<AdmitRoute, ExecutionResult> {
        let risk_tier = get_tool_risk_tier(tool_name);

        // None-handling: a `None` caller_ctx means the IPC accept loop
        // couldn't pin the peer (Linux < 5.3 with no `pidfd_open`,
        // same-uid TCC anomaly, etc.). Such peers go through the gate as
        // a synthetic `Untrusted` — with `shadow_mode = false` they'll be
        // denied for anything beyond ReadOnly / Stateful /
        // ArtifactRender. Daemon-internal callers that have already been
        // gate-checked at an outer boundary pass
        // `Some(CallerContext::internal_trusted())` so they reach the
        // gate as `GuiTrusted` and route through Plan/Apply for
        // ConfigChange just like a verified GUI peer would.
        let ctx_for_gate: std::borrow::Cow<'_, crate::security::CallerContext> = match caller_ctx {
            Some(ctx) => std::borrow::Cow::Borrowed(ctx),
            None => std::borrow::Cow::Owned(crate::security::CallerContext::synthetic_unpinned()),
        };

        let mut gate_route = AdmitRoute::FallThrough;
        {
            let ctx = ctx_for_gate.as_ref();
            match crate::security::enforce(risk_tier, ctx, policy) {
                crate::security::GateDecision::Allow
                | crate::security::GateDecision::AllowWithAudit => {
                    // Fall through to rate-limit + per-tier dispatch.
                    // `AllowWithAudit` is treated as `Allow` — D13a's
                    // audit-stream emission is a follow-up sub-piece; the
                    // audit logger already records every tool execution
                    // via the per-tier handler's own log calls.
                }
                crate::security::GateDecision::Deny(reason) => {
                    // **Return BEFORE rate-limit** so denied requests
                    // don't consume throttling budget. The gate's denial
                    // reason is also more informative than `RateLimited`
                    // would be. Use `{}` (Display) not `{:?}` (Debug) —
                    // the `Display` impl on `DenialReason` renders
                    // natural-language reasoning operators / end users
                    // can act on.
                    warn!(
                        "Gate denied tool '{}' (tier {:?}, trust {:?}): {}",
                        tool_name, risk_tier, ctx.trust_level, reason
                    );
                    if let Some(ref logger) = self.audit_logger {
                        logger.log_tool_denied(
                            tool_name,
                            tool_risk_to_audit_risk(&risk_tier),
                            &format!("Gate denied: {}", reason),
                            Some(UserContext::local_user()),
                        );
                    }
                    return Err(ExecutionResult::Error {
                        message: format!("Gate denied tool '{}': {}", tool_name, reason),
                    });
                }
                crate::security::GateDecision::RequirePlan(_req) => {
                    // ADR-027 D5/D1 wiring: route to the existing
                    // Plan/Apply machinery. The gate's RequirePlan
                    // decision means this tool needs a user-confirmable
                    // plan before its mutation applies (ADR-007 D2).
                    // `execute_config_change` returns
                    // `ExecutionResult::PlanCreated { plan }` and the
                    // GUI / CLI submits the plan_id back via `ApplyPlan`
                    // to confirm. Rate-limit applies between here and
                    // the handler call (see below). Audit attribution
                    // today: the handler calls `log_plan_created` (not
                    // `log_tool_complete`) — the plan event doesn't
                    // include the originating tool_name / args, which is
                    // a known pre-existing gap for a future PR that adds
                    // tool-attributable plan logging.
                    debug!(
                        "Gate routed tool '{}' (tier {:?}) to Plan/Apply via RequirePlan",
                        tool_name, risk_tier
                    );
                    gate_route = AdmitRoute::ToConfigChange;
                }
                crate::security::GateDecision::RequireConfirmation(_req) => {
                    // ADR-027 D5/D1 wiring: route to the existing
                    // hardware-IO confirmation machinery (ADR-027 D7
                    // partial). The handler returns
                    // `ExecutionResult::HardwareIoConfirmation` with a
                    // confirmation token; the GUI prompts the user, and
                    // the token is submitted back via the same tool with
                    // `args.confirmation_token` set. Rate-limit applies
                    // between here and the handler call.
                    debug!(
                        "Gate routed tool '{}' (tier {:?}) to HardwareIO confirmation via RequireConfirmation",
                        tool_name, risk_tier
                    );
                    gate_route = AdmitRoute::ToHardwareIo;
                }
            }
        }

        // Rate-limit check (P4-05). Runs AFTER the gate has had
        // a chance to deny — gate-denied requests skip this and
        // get a clean denial reason. Runs BEFORE both gate-
        // routed handlers (Plan/Apply, HardwareIO confirmation)
        // and per-tier dispatch — every "proceed" path is
        // throttled equally.
        match self
            .rate_limiter
            .check_and_record(&self.client_id, risk_tier)
        {
            Ok(_) => {
                // Rate limit check passed, continue with execution
            }
            Err(RateLimitError::Exceeded {
                tier,
                current,
                limit,
                retry_after_secs,
            }) => {
                warn!(
                    "Rate limit exceeded for tool '{}': {}/{} requests for {:?} tier",
                    tool_name, current, limit, tier
                );

                // Audit log the rate limit denial
                if let Some(ref logger) = self.audit_logger {
                    logger.log_tool_denied(
                        tool_name,
                        tool_risk_to_audit_risk(&risk_tier),
                        &format!(
                            "Rate limit exceeded: {}/{} requests in window. Retry after {}s",
                            current, limit, retry_after_secs
                        ),
                        Some(UserContext::local_user()),
                    );
                }

                return Err(ExecutionResult::RateLimited {
                    tier,
                    current,
                    limit,
                    retry_after_secs,
                });
            }
        }

        // ADR-027 D6 — multi-dimensional LLM budget. Charge AFTER the gate
        // and rate-limiter have admitted the call (a denied/throttled call
        // shouldn't consume budget) and BEFORE any handler runs. This MCP
        // surface can observe three of the D6 dimensions: every tool call,
        // ConfigChange-tier calls, and HardwareIO/MIDI output. The token /
        // iteration / wall-clock dimensions live in the GUI agentic loop.
        // On exhaustion we halt with an `LlmBudgetExceeded` audit event —
        // satisfying "the daemon halts the loop with a clear audit event".
        if let Some(ref budget) = self.budget {
            let mut state = budget.lock().await;
            let charge = state.charge_tool_call().and_then(|()| match risk_tier {
                ToolRiskTier::ConfigChange => state.charge_config_change(),
                ToolRiskTier::HardwareIO => state.charge_midi_out(1),
                _ => Ok(()),
            });
            if let Err(exceeded) = charge {
                // Drop the lock before the (synchronous) audit insert.
                drop(state);
                warn!(
                    "LLM budget exceeded for tool '{}' (tier {:?}): {}",
                    tool_name, risk_tier, exceeded
                );
                if let Some(ref logger) = self.audit_logger {
                    logger.log_llm_budget_exceeded(
                        tool_name,
                        tool_risk_to_audit_risk(&risk_tier),
                        exceeded.dimension.as_str(),
                        exceeded.limit,
                        exceeded.observed,
                        Some(UserContext::local_user()),
                    );
                }
                return Err(ExecutionResult::Error {
                    message: format!("LLM budget exceeded for tool '{}': {}", tool_name, exceeded),
                });
            }
        }

        Ok(gate_route)
    }

    /// ADR-027 D13 — audit a direct-dispatch tool outcome. The
    /// engine-manager arms that bypass [`Self::execute`] for deadlock
    /// reasons record completion/error through the same sink the
    /// per-tier handlers use, so the direct paths leave the same trail.
    pub(crate) fn audit_direct_tool_outcome(
        &self,
        tool_name: &str,
        outcome: Result<Option<&str>, &str>,
        execution_time: std::time::Duration,
    ) {
        if let Some(ref logger) = self.audit_logger {
            let tier = get_tool_risk_tier(tool_name);
            match outcome {
                Ok(result) => logger.log_tool_complete(
                    tool_name,
                    tool_risk_to_audit_risk(&tier),
                    None,
                    result,
                    execution_time,
                    Some(UserContext::local_user()),
                ),
                Err(error_message) => logger.log_tool_error(
                    tool_name,
                    tool_risk_to_audit_risk(&tier),
                    None,
                    error_message,
                    execution_time,
                    Some(UserContext::local_user()),
                ),
            }
        }
    }

    /// Fetch device data for conductor_list_devices tool
    pub(super) async fn fetch_devices_data() -> Option<Value> {
        let mut devices_data = json!({
            "midi_devices": [],
            "hid_devices": []
        });

        // Enumerate MIDI devices with warmup pattern for fresh results
        // Uses spawn_blocking to avoid blocking the tokio runtime
        let midi_devices = crate::daemon::device_utils::enumerate_midi_devices_fresh_async().await;
        let midi_json: Vec<Value> = midi_devices
            .iter()
            .map(|d| {
                json!({
                    "index": d.port_index,
                    "name": d.port_name,
                    "type": "midi_input"
                })
            })
            .collect();
        devices_data["midi_devices"] = json!(midi_json);

        // Enumerate HID/gamepad devices via spawn_blocking to avoid blocking tokio
        let hid_result = tokio::task::spawn_blocking(HidDeviceManager::list_gamepads).await;
        if let Ok(Ok(gamepads)) = hid_result {
            let hid_devices: Vec<Value> = gamepads
                .iter()
                .enumerate()
                .map(|(idx, (_id, name, uuid))| {
                    json!({
                        "index": idx,
                        "name": name,
                        "uuid": uuid,
                        "type": "hid_gamepad"
                    })
                })
                .collect();
            devices_data["hid_devices"] = json!(hid_devices);
        }

        Some(devices_data)
    }
}
