// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! LLM Integration module for Phase 2 (ADR-007)
//!
//! This module provides:
//! - ToolExecutor: Transport-agnostic tool execution with risk tiers
//! - ConfigPlan: TOCTOU-protected configuration change plans
//! - UndoStack: Undo/redo for configuration changes (P4-06)

pub mod control_state_tools;
pub mod executor;
pub mod history;
pub mod plan;
pub mod resolved_routing_graph;
pub mod security_status;

pub use executor::ToolExecutor;
pub use history::{HistoryEntry, HistoryError, HistorySummary, UndoStack};
pub use plan::{ConfigChange, ConfigPlan, PlanError};
