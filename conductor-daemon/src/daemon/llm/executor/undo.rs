// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Undo/redo methods.

use super::*;

impl ToolExecutor {
    // ==================== Undo/Redo Methods (P4-06) ====================

    /// Check if undo is available
    pub async fn can_undo(&self) -> bool {
        let stack = self.undo_stack.read().await;
        stack.can_undo()
    }

    /// Check if redo is available
    pub async fn can_redo(&self) -> bool {
        let stack = self.undo_stack.read().await;
        stack.can_redo()
    }

    /// Get summary of changes that can be undone
    pub async fn undo_summary(&self, limit: usize) -> Vec<HistorySummary> {
        let stack = self.undo_stack.read().await;
        stack.undo_summary(limit)
    }

    /// Get summary of changes that can be redone
    pub async fn redo_summary(&self, limit: usize) -> Vec<HistorySummary> {
        let stack = self.undo_stack.read().await;
        stack.redo_summary(limit)
    }

    /// Undo the last configuration change
    ///
    /// Returns the description of the undone change on success.
    pub async fn undo(&self) -> Result<String, HistoryError> {
        // Get the inverse changes from the undo stack
        let (description, inverse_changes) = {
            let mut stack = self.undo_stack.write().await;
            let entry = stack.undo()?;
            (entry.description.clone(), entry.inverse_changes.clone())
        };

        // D4.A.3.3.B.1: apply undo via LiveConfig. Same Provenance pattern as
        // apply_plan — Initiator::Llm with placeholder provider/model until
        // B.2 wires the live values.
        let mut apply_err: Option<HistoryError> = None;
        let mutate_result = self
            .live_config
            .mutate_replace_whole(
                conductor_core::config::Provenance {
                    initiator: conductor_core::config::Initiator::Llm {
                        provider: "tbd".to_string(),
                        model: "tbd".to_string(),
                        // D4.A.3.3.B.1 stub: deliberate non-UUID sentinel —
                        // B.2 routes undo/redo through a proper
                        // session-scoped plan id once audit consumes the
                        // value. Free-text descriptions could contain
                        // user-supplied content and shouldn't leak into
                        // a UUID-typed field downstream.
                        plan_id: "undo-placeholder".to_string(),
                    },
                    source: conductor_core::config::Source::InMemoryEdit,
                    peer: None,
                },
                |cfg| {
                    for change in inverse_changes {
                        if let Err(e) = crate::daemon::llm::plan::apply_change(cfg, change) {
                            apply_err = Some(HistoryError::ApplyFailed(e.to_string()));
                            return;
                        }
                    }
                },
            )
            .await;
        if let Err(e) = mutate_result {
            return Err(HistoryError::ApplyFailed(format!(
                "live_config mutate failed: {e}"
            )));
        }
        if let Some(e) = apply_err {
            return Err(e);
        }

        info!("Undid change: {}", description);

        // Audit log the undo
        if let Some(ref logger) = self.audit_logger {
            logger.log_tool_complete(
                "undo",
                AuditRiskTier::ConfigChange,
                Some(&format!(r#"{{"description": "{}"}}"#, description)),
                Some(&json!({"action": "undo", "description": description}).to_string()),
                std::time::Duration::from_millis(0),
                Some(UserContext::local_user()),
            );
        }

        Ok(description)
    }

    /// Redo a previously undone configuration change
    ///
    /// Returns the description of the redone change on success.
    pub async fn redo(&self) -> Result<String, HistoryError> {
        // Get the forward changes from the undo stack
        let (description, forward_changes) = {
            let mut stack = self.undo_stack.write().await;
            let entry = stack.redo()?;
            (entry.description.clone(), entry.forward_changes.clone())
        };

        // D4.A.3.3.B.1: apply redo via LiveConfig — same pattern as undo above.
        let mut apply_err: Option<HistoryError> = None;
        let mutate_result = self
            .live_config
            .mutate_replace_whole(
                conductor_core::config::Provenance {
                    initiator: conductor_core::config::Initiator::Llm {
                        provider: "tbd".to_string(),
                        model: "tbd".to_string(),
                        // See undo() above for the sentinel rationale.
                        plan_id: "redo-placeholder".to_string(),
                    },
                    source: conductor_core::config::Source::InMemoryEdit,
                    peer: None,
                },
                |cfg| {
                    for change in forward_changes {
                        if let Err(e) = crate::daemon::llm::plan::apply_change(cfg, change) {
                            apply_err = Some(HistoryError::ApplyFailed(e.to_string()));
                            return;
                        }
                    }
                },
            )
            .await;
        if let Err(e) = mutate_result {
            return Err(HistoryError::ApplyFailed(format!(
                "live_config mutate failed: {e}"
            )));
        }
        if let Some(e) = apply_err {
            return Err(e);
        }

        info!("Redid change: {}", description);

        // Audit log the redo
        if let Some(ref logger) = self.audit_logger {
            logger.log_tool_complete(
                "redo",
                AuditRiskTier::ConfigChange,
                Some(&format!(r#"{{"description": "{}"}}"#, description)),
                Some(&json!({"action": "redo", "description": description}).to_string()),
                std::time::Duration::from_millis(0),
                Some(UserContext::local_user()),
            );
        }

        Ok(description)
    }

    /// Clear undo history
    pub async fn clear_undo_history(&self) {
        let mut stack = self.undo_stack.write().await;
        stack.clear();
    }

    /// Get number of changes that can be undone
    pub async fn undo_count(&self) -> usize {
        let stack = self.undo_stack.read().await;
        stack.undo_count()
    }

    /// Get number of changes that can be redone
    pub async fn redo_count(&self) -> usize {
        let stack = self.undo_stack.read().await;
        stack.redo_count()
    }
}
