// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Plan creation and the plan lifecycle.

use super::*;

impl ToolExecutor {
    /// Create a ConfigPlan for a ConfigChange tool
    pub(super) fn create_plan_for_tool(
        &self,
        tool_name: &str,
        arguments: Option<Value>,
        config: &Config,
    ) -> Result<ConfigPlan, PlanError> {
        let args = arguments.unwrap_or(json!({}));

        match tool_name {
            "conductor_create_mapping" => plan_create_mapping(&args, config),

            // ADR-025 Phase 2.H — focused tool for authoring context-
            // switch mappings. Reuses the CreateMapping change but
            // enforces that `action` is PcContextSwitch / CcContextSwitch
            // so the LLM gets a clear error instead of silently
            // authoring a non-routing mapping.
            "conductor_set_context_mapping" => plan_set_context_mapping(&args, config),

            "conductor_update_mapping" => plan_update_mapping(&args, config),

            "conductor_delete_mapping" => plan_delete_mapping(&args, config),

            "conductor_batch_changes" => plan_batch_changes(&args, config),

            "conductor_create_endpoint" => plan_create_endpoint(&args, config),

            _ => Err(PlanError::InvalidAction(format!(
                "Unknown ConfigChange tool: {}",
                tool_name
            ))),
        }
    }

    /// Get a pending plan by ID
    pub async fn get_plan(&self, plan_id: &Uuid) -> Option<ConfigPlan> {
        let plans = self.pending_plans.read().await;
        plans.get(plan_id).cloned()
    }

    /// Apply a pending plan atomically (P3-07)
    ///
    /// # Arguments
    /// * `plan_id` - ID of the plan to apply
    ///
    /// # Returns
    /// - Ok(changes_count) if plan was applied successfully
    /// - Err(PlanError) if plan not found, expired, or config changed
    ///
    /// Uses atomic apply: all changes succeed or none are applied.
    /// Records the change in undo history (P4-06).
    pub async fn apply_plan(&self, plan_id: &Uuid) -> Result<usize, PlanError> {
        let start_time = Instant::now();

        // Remove plan from pending
        let plan = {
            let mut plans = self.pending_plans.write().await;
            plans.remove(plan_id).ok_or(PlanError::NotFound(*plan_id))?
        };

        // D4.A.3.3.B.1: apply plan via LiveConfig.
        //
        // Route the mutation through `live_config.mutate_replace_whole` so
        // both engine_manager and any future LiveConfig subscriber see the
        // change through the same atomic publication. Undo history is
        // recorded AFTER a successful publish: if `mutate_replace_whole`
        // errors (CAS conflict, compile failure),
        // a pre-recorded undo entry would describe an apply that never
        // happened, and a subsequent `undo()` would silently fast-forward
        // the config to a phantom inverse.
        //
        // Provenance: `Initiator::Llm { provider, model, plan_id }`.
        // D4.A.3.3.B.2 records the SAME Provenance value into
        // the audit log via `log_plan_applied(..., Some(provenance))`,
        // so both sinks (LiveConfig publish + audit row) agree on who
        // initiated the apply. `provider`/`model` remain placeholders
        // until the calling LLM session can thread its identity here
        // — that wiring is the open follow-up; the pipeline already
        // carries the value once it's set.
        let pre_config = (*self.live_config.load().config).clone();
        let plan_description = plan.description.clone();
        let plan_changes = plan.changes.clone();
        let plan_id_string = plan_id.to_string();
        let provenance = conductor_core::config::Provenance {
            initiator: conductor_core::config::Initiator::Llm {
                provider: "tbd".to_string(),
                model: "tbd".to_string(),
                plan_id: plan_id_string,
            },
            source: conductor_core::config::Source::InMemoryEdit,
            peer: None,
        };
        // Use `try_mutate_replace_whole` so a failed
        // `apply_atomic` aborts the publish — pre-fix, the old
        // `mutate_replace_whole` would publish the candidate
        // regardless of apply success, bumping `state_generation`
        // for a no-op mutation.
        //
        // The closure captures `apply_outcome` (Ok(count) or
        // Err(PlanError)) for the caller to re-derive after the
        // helper returns. The helper's `Ok(())` / `Err(msg)` return
        // is just the abort signal — the rich error stays in
        // `apply_outcome`.
        let mut apply_outcome: Option<Result<usize, super::PlanError>> = None;
        let mutate_result = self
            .live_config
            .try_mutate_replace_whole(provenance.clone(), |cfg| {
                let outcome = plan.apply_atomic(cfg);
                let signal = match &outcome {
                    Ok(_) => Ok(()),
                    Err(e) => Err(e.to_string()),
                };
                apply_outcome = Some(outcome);
                signal
            })
            .await;
        let result: Result<usize, super::PlanError> = match mutate_result {
            Ok(_) => apply_outcome.unwrap_or(Ok(0)),
            Err(crate::daemon::live_config::MutateError::MutatorAborted(_)) => {
                // The closure signalled abort because `apply_atomic`
                // failed; the rich error is in apply_outcome.
                apply_outcome.unwrap_or_else(|| {
                    Err(super::PlanError::InvalidAction(
                        "apply aborted but closure did not capture the error".into(),
                    ))
                })
            }
            Err(other) => {
                return Err(super::PlanError::InvalidAction(format!(
                    "live_config mutate failed: {other}"
                )));
            }
        };
        let execution_time = start_time.elapsed();

        // Record undo only on a successful publish (see contract note above).
        if result.is_ok() {
            let mut undo_stack = self.undo_stack.write().await;
            if let Err(e) = undo_stack.record(
                *plan_id,
                plan_description.clone(),
                plan_changes,
                &pre_config,
            ) {
                warn!("Failed to record undo history: {}", e);
                // Continue — undo not being recorded is not fatal.
            }
        }

        match result {
            Ok(changes_count) => {
                info!("Applied plan {} with {} changes", plan_id, changes_count);

                // Audit log plan application (P4-04 + D4.A.3.3.B.2:
                // pass the same Provenance used for the LiveConfig
                // mutation — both audit + LiveConfig agree on the
                // initiator).
                if let Some(ref logger) = self.audit_logger {
                    logger.log_plan_applied(
                        &plan_id.to_string(),
                        changes_count,
                        execution_time,
                        Some(UserContext::local_user()),
                        Some(provenance.clone()),
                    );
                }

                Ok(changes_count)
            }
            Err(e) => {
                // Audit log the failure
                if let Some(ref logger) = self.audit_logger {
                    logger.log_tool_error(
                        "apply_plan",
                        AuditRiskTier::ConfigChange,
                        Some(&format!(r#"{{"plan_id": "{}"}}"#, plan_id)),
                        &e.to_string(),
                        execution_time,
                        Some(UserContext::local_user()),
                    );
                }
                Err(e)
            }
        }
    }

    /// Reject a pending plan
    pub async fn reject_plan(&self, plan_id: &Uuid) -> Result<(), PlanError> {
        let mut plans = self.pending_plans.write().await;
        plans.remove(plan_id).ok_or(PlanError::NotFound(*plan_id))?;
        info!("Rejected plan {}", plan_id);

        // Audit log plan rejection (P4-04)
        if let Some(ref logger) = self.audit_logger {
            logger.log_plan_rejected(&plan_id.to_string(), Some(UserContext::local_user()));
        }

        Ok(())
    }

    /// Get all pending plans
    pub async fn list_pending_plans(&self) -> Vec<ConfigPlan> {
        let plans = self.pending_plans.read().await;
        plans.values().cloned().collect()
    }

    /// Clean up expired plans
    pub async fn cleanup_expired_plans(&self) {
        let mut plans = self.pending_plans.write().await;
        let expired: Vec<Uuid> = plans
            .iter()
            .filter(|(_, p)| p.is_expired())
            .map(|(id, _)| *id)
            .collect();

        for id in expired {
            plans.remove(&id);
            warn!("Cleaned up expired plan {}", id);
        }
    }

    /// Get execution log
    pub async fn get_execution_log(&self) -> Vec<LogEntry> {
        let log = self.execution_log.read().await;
        log.clone()
    }

    /// Clear execution log
    pub async fn clear_execution_log(&self) {
        let mut log = self.execution_log.write().await;
        log.clear();
    }

    /// Summarize a tool result for logging
    pub(super) fn summarize_result(&self, result: &ToolCallResult) -> String {
        if result.is_error == Some(true) {
            "Error".to_string()
        } else {
            "Success".to_string()
        }
    }
}

fn plan_create_mapping(args: &Value, config: &Config) -> Result<ConfigPlan, PlanError> {
    let mode = args
        .get("mode")
        .and_then(|m| m.as_str())
        .ok_or_else(|| PlanError::InvalidAction("Missing 'mode' argument".to_string()))?
        .to_string();

    let trigger: Trigger = serde_json::from_value(
        args.get("trigger")
            .cloned()
            .ok_or_else(|| PlanError::InvalidTrigger("Missing 'trigger' argument".to_string()))?,
    )
    .map_err(|e| PlanError::InvalidTrigger(e.to_string()))?;

    let action: ActionConfig = serde_json::from_value(
        args.get("action")
            .cloned()
            .ok_or_else(|| PlanError::InvalidAction("Missing 'action' argument".to_string()))?,
    )
    .map_err(|e| PlanError::InvalidAction(e.to_string()))?;

    let description = args
        .get("description")
        .and_then(|d| d.as_str())
        .map(|s| s.to_string());

    // ADR-038: optional let_through (default false = swallow).
    let let_through = args
        .get("let_through")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // Validate mode exists
    if !config.modes.iter().any(|m| m.name == mode) {
        return Err(PlanError::ModeNotFound(mode));
    }

    Ok(ConfigPlan::new(
        format!("Create new mapping in mode '{}'", mode),
        vec![ConfigChange::CreateMapping {
            mode,
            trigger,
            action,
            description,
            let_through,
        }],
        config,
    ))
}

fn plan_set_context_mapping(args: &Value, config: &Config) -> Result<ConfigPlan, PlanError> {
    let mode = args
        .get("mode")
        .and_then(|m| m.as_str())
        .ok_or_else(|| PlanError::InvalidAction("Missing 'mode' argument".to_string()))?
        .to_string();

    let trigger: Trigger = serde_json::from_value(
        args.get("trigger")
            .cloned()
            .ok_or_else(|| PlanError::InvalidTrigger("Missing 'trigger' argument".to_string()))?,
    )
    .map_err(|e| PlanError::InvalidTrigger(e.to_string()))?;

    let action: ActionConfig = serde_json::from_value(
        args.get("action")
            .cloned()
            .ok_or_else(|| PlanError::InvalidAction("Missing 'action' argument".to_string()))?,
    )
    .map_err(|e| PlanError::InvalidAction(e.to_string()))?;

    if !matches!(
        action,
        ActionConfig::PcContextSwitch { .. } | ActionConfig::CcContextSwitch { .. }
    ) {
        return Err(PlanError::InvalidAction(
            "conductor_set_context_mapping expects action.type = 'PcContextSwitch' or 'CcContextSwitch'; use conductor_create_mapping for other action shapes".to_string(),
        ));
    }

    let description = args
        .get("description")
        .and_then(|d| d.as_str())
        .map(|s| s.to_string());

    if !config.modes.iter().any(|m| m.name == mode) {
        return Err(PlanError::ModeNotFound(mode));
    }

    Ok(ConfigPlan::new(
        format!("Create context-switch mapping in mode '{}'", mode),
        vec![ConfigChange::CreateMapping {
            mode,
            trigger,
            action,
            description,
            // Context-switch mappings consume the event (route by
            // prior state); let-through doesn't apply.
            let_through: false,
        }],
        config,
    ))
}

fn plan_update_mapping(args: &Value, config: &Config) -> Result<ConfigPlan, PlanError> {
    let mode = args
        .get("mode")
        .and_then(|m| m.as_str())
        .ok_or_else(|| PlanError::InvalidAction("Missing 'mode' argument".to_string()))?
        .to_string();

    let index = args
        .get("index")
        .and_then(|i| i.as_u64())
        .ok_or_else(|| PlanError::InvalidAction("Missing 'index' argument".to_string()))?
        as usize;

    let trigger: Trigger = serde_json::from_value(
        args.get("trigger")
            .cloned()
            .ok_or_else(|| PlanError::InvalidTrigger("Missing 'trigger' argument".to_string()))?,
    )
    .map_err(|e| PlanError::InvalidTrigger(e.to_string()))?;

    let action: ActionConfig = serde_json::from_value(
        args.get("action")
            .cloned()
            .ok_or_else(|| PlanError::InvalidAction("Missing 'action' argument".to_string()))?,
    )
    .map_err(|e| PlanError::InvalidAction(e.to_string()))?;

    let description = args
        .get("description")
        .and_then(|d| d.as_str())
        .map(|s| s.to_string());

    // Validate mode and index
    let mode_obj = config
        .modes
        .iter()
        .find(|m| m.name == mode)
        .ok_or_else(|| PlanError::ModeNotFound(mode.clone()))?;

    if index >= mode_obj.mappings.len() {
        return Err(PlanError::IndexOutOfRange {
            mode: mode.clone(),
            index,
            count: mode_obj.mappings.len(),
        });
    }

    Ok(ConfigPlan::new(
        format!("Update mapping {} in mode '{}'", index, mode),
        vec![ConfigChange::UpdateMapping {
            mode,
            index,
            trigger,
            action,
            description,
        }],
        config,
    ))
}

fn plan_delete_mapping(args: &Value, config: &Config) -> Result<ConfigPlan, PlanError> {
    let mode = args
        .get("mode")
        .and_then(|m| m.as_str())
        .ok_or_else(|| PlanError::InvalidAction("Missing 'mode' argument".to_string()))?
        .to_string();

    let index = args
        .get("index")
        .and_then(|i| i.as_u64())
        .ok_or_else(|| PlanError::InvalidAction("Missing 'index' argument".to_string()))?
        as usize;

    // Validate mode and index
    let mode_obj = config
        .modes
        .iter()
        .find(|m| m.name == mode)
        .ok_or_else(|| PlanError::ModeNotFound(mode.clone()))?;

    if index >= mode_obj.mappings.len() {
        return Err(PlanError::IndexOutOfRange {
            mode: mode.clone(),
            index,
            count: mode_obj.mappings.len(),
        });
    }

    Ok(ConfigPlan::new(
        format!("Delete mapping {} in mode '{}'", index, mode),
        vec![ConfigChange::DeleteMapping { mode, index }],
        config,
    ))
}

fn plan_batch_changes(args: &Value, config: &Config) -> Result<ConfigPlan, PlanError> {
    // P3-07: Batch operations support
    let operations = args
        .get("operations")
        .and_then(|o| o.as_array())
        .ok_or_else(|| PlanError::InvalidAction("Missing 'operations' array".to_string()))?;

    if operations.is_empty() {
        return Err(PlanError::InvalidAction(
            "Operations array is empty".to_string(),
        ));
    }

    let mut changes = Vec::new();
    let mut descriptions = Vec::new();

    for (idx, op) in operations.iter().enumerate() {
        let op_type = op.get("type").and_then(|t| t.as_str()).ok_or_else(|| {
            PlanError::InvalidAction(format!("Operation {} missing 'type' field", idx))
        })?;

        let change = match op_type {
            "create_mapping" | "CreateMapping" => {
                batch_create_mapping(op, idx, config, &mut descriptions)?
            }

            "update_mapping" | "UpdateMapping" => {
                batch_update_mapping(op, idx, config, &mut descriptions)?
            }

            "delete_mapping" | "DeleteMapping" => {
                batch_delete_mapping(op, idx, config, &mut descriptions)?
            }

            "create_mode" | "CreateMode" => batch_create_mode(op, idx, config, &mut descriptions)?,

            "delete_mode" | "DeleteMode" => batch_delete_mode(op, idx, config, &mut descriptions)?,

            // ADR-031 P3 § 5.4 —
            // `update_route` completes the route-mutation
            // trio (create/delete/update). Total-replace
            // semantics: the LLM supplies the full new
            // shape, and `apply()` swaps the whole
            // RouteConfig at `index`. Required args:
            // `index`, `from`, `to`. Optional:
            // `transform`, `filter`, `enabled`,
            // `description`. Same TOCTOU stability
            // story as `delete_route`.
            "update_route" | "UpdateRoute" => {
                batch_update_route(op, idx, config, &mut descriptions)?
            }

            // ADR-031 P3 § 5.4 — paired
            // with `create_route` per spec; `delete_route`
            // also goes through batch_changes by design
            // (no singleton tool). Takes a 0-based `index`
            // into `config.routes` as it stands at apply
            // time. The plan's TOCTOU base_state_hash
            // guards against the underlying list mutating
            // between plan creation and approval.
            "delete_route" | "DeleteRoute" => {
                batch_delete_route(op, idx, config, &mut descriptions)?
            }

            // ADR-031 P3 § 5.4 — accept
            // `create_route` inside a batch so the LLM can
            // build a routing-setup plan with several
            // routes in one approval round-trip.
            // Singleton-tool form (`conductor_create_route`)
            // is deliberately NOT planned per spec § 5.4 —
            // route mutations always go through batch_changes.
            "create_route" | "CreateRoute" => {
                batch_create_route(op, idx, config, &mut descriptions)?
            }

            _ => {
                return Err(PlanError::InvalidAction(format!(
                    "Operation {} has unknown type: {}",
                    idx, op_type
                )));
            }
        };

        changes.push(change);
    }

    let description = format!(
        "Batch operation ({} changes): {}",
        changes.len(),
        descriptions.join(", ")
    );

    Ok(ConfigPlan::new(description, changes, config))
}

fn plan_create_endpoint(args: &Value, config: &Config) -> Result<ConfigPlan, PlanError> {
    use conductor_core::config::types::{ConnectorDirection, ConnectorProtocol, EndpointKind};

    let alias = args
        .get("alias")
        .and_then(|a| a.as_str())
        .ok_or_else(|| PlanError::InvalidAction("Missing 'alias' argument".to_string()))?
        .to_string();

    // `direction` is REQUIRED for endpoints (ADR-035 §4.1 R2 P1 — no
    // default; forcing it avoids binding a network listener as
    // implicitly Bidirectional).
    let direction: ConnectorDirection = args
        .get("direction")
        .ok_or_else(|| {
            PlanError::InvalidAction(
                "Missing 'direction' argument (required for endpoints — ADR-035 §4.1)".to_string(),
            )
        })
        .and_then(|v| {
            serde_json::from_value(v.clone())
                .map_err(|e| PlanError::InvalidAction(format!("Invalid direction: {}", e)))
        })?;

    // `protocol` is optional — inferred from `kind` at load when omitted.
    let protocol: Option<ConnectorProtocol> = args
        .get("protocol")
        .map(|v| serde_json::from_value(v.clone()))
        .transpose()
        .map_err(|e| PlanError::InvalidAction(format!("Invalid protocol: {}", e)))?;

    // `kind` is the internally-tagged `type` + its variant fields,
    // which sit at the top level of the args (mirroring EndpointConfig).
    // EndpointKind ignores the common fields it doesn't recognize.
    let kind: EndpointKind = serde_json::from_value(args.clone()).map_err(|e| {
        PlanError::InvalidAction(format!(
            "Invalid endpoint `type`/fields (expected a `type` of Matcher/OscEndpoint/ArtNetEndpoint/MidiVirtualPort plus its fields): {}",
            e
        ))
    })?;

    let description = args
        .get("description")
        .and_then(|d| d.as_str())
        .map(|s| s.to_string());

    let enabled = args
        .get("enabled")
        .and_then(|e| e.as_bool())
        .unwrap_or(true);

    let channels: Vec<u8> = args
        .get("channels")
        .map(|v| serde_json::from_value(v.clone()))
        .transpose()
        .map_err(|e| PlanError::InvalidAction(format!("Invalid channels: {}", e)))?
        .unwrap_or_default();

    build_create_endpoint_plan(
        alias,
        direction,
        protocol,
        kind,
        description,
        enabled,
        channels,
        config,
    )
}

fn batch_create_route(
    op: &Value,
    idx: usize,
    _config: &Config,
    descriptions: &mut Vec<String>,
) -> Result<ConfigChange, PlanError> {
    Ok({
        let from = op
            .get("from")
            .and_then(|f| f.as_str())
            .ok_or_else(|| PlanError::InvalidAction(format!("Operation {} missing 'from'", idx)))?
            .to_string();
        if from.trim().is_empty() {
            return Err(PlanError::InvalidAction(format!(
                "Operation {} 'from' cannot be empty",
                idx
            )));
        }

        let to = op
            .get("to")
            .and_then(|t| t.as_str())
            .ok_or_else(|| PlanError::InvalidAction(format!("Operation {} missing 'to'", idx)))?
            .to_string();
        if to.trim().is_empty() {
            return Err(PlanError::InvalidAction(format!(
                "Operation {} 'to' cannot be empty",
                idx
            )));
        }

        let transform = op
            .get("transform")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| {
                PlanError::InvalidAction(format!("Operation {} invalid transform: {}", idx, e))
            })?;

        let filter = op
            .get("filter")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| {
                PlanError::InvalidAction(format!("Operation {} invalid filter: {}", idx, e))
            })?;

        let enabled = op.get("enabled").and_then(|e| e.as_bool()).unwrap_or(true);

        let description = op
            .get("description")
            .and_then(|d| d.as_str())
            .map(|s| s.to_string());

        descriptions.push(format!("Create route '{}' → '{}'", from, to));

        ConfigChange::CreateRoute {
            from,
            to,
            transform,
            filter,
            enabled,
            description,
        }
    })
}

fn batch_delete_route(
    op: &Value,
    idx: usize,
    _config: &Config,
    descriptions: &mut Vec<String>,
) -> Result<ConfigChange, PlanError> {
    Ok({
        let index = op.get("index").and_then(|i| i.as_u64()).ok_or_else(|| {
            PlanError::InvalidAction(format!(
                "Operation {} missing 'index' (must be a non-negative integer)",
                idx
            ))
        })? as usize;

        descriptions.push(format!("Delete route at index {}", index));

        ConfigChange::DeleteRoute { index }
    })
}

fn batch_update_route(
    op: &Value,
    idx: usize,
    _config: &Config,
    descriptions: &mut Vec<String>,
) -> Result<ConfigChange, PlanError> {
    Ok({
        let index = op.get("index").and_then(|i| i.as_u64()).ok_or_else(|| {
            PlanError::InvalidAction(format!(
                "Operation {} missing 'index' (must be a non-negative integer)",
                idx
            ))
        })? as usize;

        let from = op
            .get("from")
            .and_then(|f| f.as_str())
            .ok_or_else(|| PlanError::InvalidAction(format!("Operation {} missing 'from'", idx)))?
            .to_string();
        if from.trim().is_empty() {
            return Err(PlanError::InvalidAction(format!(
                "Operation {} 'from' cannot be empty",
                idx
            )));
        }

        let to = op
            .get("to")
            .and_then(|t| t.as_str())
            .ok_or_else(|| PlanError::InvalidAction(format!("Operation {} missing 'to'", idx)))?
            .to_string();
        if to.trim().is_empty() {
            return Err(PlanError::InvalidAction(format!(
                "Operation {} 'to' cannot be empty",
                idx
            )));
        }

        let transform = op
            .get("transform")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| {
                PlanError::InvalidAction(format!("Operation {} invalid transform: {}", idx, e))
            })?;

        let filter = op
            .get("filter")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| {
                PlanError::InvalidAction(format!("Operation {} invalid filter: {}", idx, e))
            })?;

        let enabled = op.get("enabled").and_then(|e| e.as_bool()).unwrap_or(true);

        let description = op
            .get("description")
            .and_then(|d| d.as_str())
            .map(|s| s.to_string());

        descriptions.push(format!(
            "Update route at index {} ('{}' → '{}')",
            index, from, to
        ));

        ConfigChange::UpdateRoute {
            index,
            from,
            to,
            transform,
            filter,
            enabled,
            description,
        }
    })
}

fn batch_delete_mode(
    op: &Value,
    idx: usize,
    _config: &Config,
    descriptions: &mut Vec<String>,
) -> Result<ConfigChange, PlanError> {
    Ok({
        let name = op
            .get("name")
            .and_then(|n| n.as_str())
            .ok_or_else(|| PlanError::InvalidAction(format!("Operation {} missing 'name'", idx)))?
            .to_string();

        descriptions.push(format!("Delete mode '{}'", name));

        ConfigChange::DeleteMode { name }
    })
}

fn batch_create_mode(
    op: &Value,
    idx: usize,
    _config: &Config,
    descriptions: &mut Vec<String>,
) -> Result<ConfigChange, PlanError> {
    Ok({
        let name = op
            .get("name")
            .and_then(|n| n.as_str())
            .ok_or_else(|| PlanError::InvalidAction(format!("Operation {} missing 'name'", idx)))?
            .to_string();

        let color = op
            .get("color")
            .and_then(|c| c.as_str())
            .map(|s| s.to_string());

        descriptions.push(format!("Create mode '{}'", name));

        ConfigChange::CreateMode { name, color }
    })
}

fn batch_delete_mapping(
    op: &Value,
    idx: usize,
    config: &Config,
    descriptions: &mut Vec<String>,
) -> Result<ConfigChange, PlanError> {
    Ok({
        let mode = op
            .get("mode")
            .and_then(|m| m.as_str())
            .ok_or_else(|| PlanError::InvalidAction(format!("Operation {} missing 'mode'", idx)))?
            .to_string();

        let index =
            op.get("index").and_then(|i| i.as_u64()).ok_or_else(|| {
                PlanError::InvalidAction(format!("Operation {} missing 'index'", idx))
            })? as usize;

        // Validate mode and index
        let mode_obj = config
            .modes
            .iter()
            .find(|m| m.name == mode)
            .ok_or_else(|| PlanError::ModeNotFound(mode.clone()))?;

        if index >= mode_obj.mappings.len() {
            return Err(PlanError::IndexOutOfRange {
                mode: mode.clone(),
                index,
                count: mode_obj.mappings.len(),
            });
        }

        descriptions.push(format!("Delete mapping {} in '{}'", index, mode));

        ConfigChange::DeleteMapping { mode, index }
    })
}

fn batch_update_mapping(
    op: &Value,
    idx: usize,
    config: &Config,
    descriptions: &mut Vec<String>,
) -> Result<ConfigChange, PlanError> {
    Ok({
        let mode = op
            .get("mode")
            .and_then(|m| m.as_str())
            .ok_or_else(|| PlanError::InvalidAction(format!("Operation {} missing 'mode'", idx)))?
            .to_string();

        let index =
            op.get("index").and_then(|i| i.as_u64()).ok_or_else(|| {
                PlanError::InvalidAction(format!("Operation {} missing 'index'", idx))
            })? as usize;

        // Validate mode and index
        let mode_obj = config
            .modes
            .iter()
            .find(|m| m.name == mode)
            .ok_or_else(|| PlanError::ModeNotFound(mode.clone()))?;

        if index >= mode_obj.mappings.len() {
            return Err(PlanError::IndexOutOfRange {
                mode: mode.clone(),
                index,
                count: mode_obj.mappings.len(),
            });
        }

        let trigger: Trigger =
            serde_json::from_value(op.get("trigger").cloned().ok_or_else(|| {
                PlanError::InvalidTrigger(format!("Operation {} missing 'trigger'", idx))
            })?)
            .map_err(|e| PlanError::InvalidTrigger(e.to_string()))?;

        let action: ActionConfig =
            serde_json::from_value(op.get("action").cloned().ok_or_else(|| {
                PlanError::InvalidAction(format!("Operation {} missing 'action'", idx))
            })?)
            .map_err(|e| PlanError::InvalidAction(e.to_string()))?;

        let description = op
            .get("description")
            .and_then(|d| d.as_str())
            .map(|s| s.to_string());

        descriptions.push(format!("Update mapping {} in '{}'", index, mode));

        ConfigChange::UpdateMapping {
            mode,
            index,
            trigger,
            action,
            description,
        }
    })
}

fn batch_create_mapping(
    op: &Value,
    idx: usize,
    config: &Config,
    descriptions: &mut Vec<String>,
) -> Result<ConfigChange, PlanError> {
    Ok({
        let mode = op
            .get("mode")
            .and_then(|m| m.as_str())
            .ok_or_else(|| PlanError::InvalidAction(format!("Operation {} missing 'mode'", idx)))?
            .to_string();

        // Validate mode exists
        if !config.modes.iter().any(|m| m.name == mode) {
            return Err(PlanError::ModeNotFound(mode));
        }

        let trigger: Trigger =
            serde_json::from_value(op.get("trigger").cloned().ok_or_else(|| {
                PlanError::InvalidTrigger(format!("Operation {} missing 'trigger'", idx))
            })?)
            .map_err(|e| PlanError::InvalidTrigger(e.to_string()))?;

        let action: ActionConfig =
            serde_json::from_value(op.get("action").cloned().ok_or_else(|| {
                PlanError::InvalidAction(format!("Operation {} missing 'action'", idx))
            })?)
            .map_err(|e| PlanError::InvalidAction(e.to_string()))?;

        let description = op
            .get("description")
            .and_then(|d| d.as_str())
            .map(|s| s.to_string());

        // ADR-038: optional let_through in batch ops too.
        let let_through = op
            .get("let_through")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let desc_str = description.as_deref().unwrap_or("mapping");
        descriptions.push(format!("Create '{}' in '{}'", desc_str, mode));

        ConfigChange::CreateMapping {
            mode,
            trigger,
            action,
            description,
            let_through,
        }
    })
}
