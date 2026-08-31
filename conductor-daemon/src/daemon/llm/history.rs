// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Undo/redo history for configuration changes (P4-06)
//!
//! Provides undo/redo capability for ConfigPlan operations by tracking
//! applied changes and their inverse operations.
//!
//! # Architecture
//!
//! ```text
//! ┌────────────────────────────────────────────────────────┐
//! │                    UndoStack                           │
//! │  ┌────────────────────────────────────────────────┐    │
//! │  │ History Entries (oldest → newest)              │    │
//! │  │  [Entry1] ← [Entry2] ← [Entry3] ← current_pos  │    │
//! │  └────────────────────────────────────────────────┘    │
//! │                                                        │
//! │  ┌────────────────────────────────────────────────┐    │
//! │  │ HistoryEntry                                   │    │
//! │  │  - id: Uuid (plan ID)                          │    │
//! │  │  - description: String                         │    │
//! │  │  - forward_changes: Vec<ConfigChange>          │    │
//! │  │  - inverse_changes: Vec<ConfigChange>          │    │
//! │  │  - applied_at: DateTime                        │    │
//! │  └────────────────────────────────────────────────┘    │
//! └────────────────────────────────────────────────────────┘
//! ```

use super::plan::ConfigChange;
use chrono::{DateTime, Utc};
use conductor_core::config::Config;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use thiserror::Error;
use uuid::Uuid;

/// Default maximum number of entries in the undo stack
const DEFAULT_MAX_HISTORY_SIZE: usize = 50;

/// Errors that can occur during undo/redo operations
#[derive(Debug, Error, Clone)]
pub enum HistoryError {
    /// Nothing to undo
    #[error("Nothing to undo")]
    NothingToUndo,

    /// Nothing to redo
    #[error("Nothing to redo")]
    NothingToRedo,

    /// Failed to create inverse change
    #[error("Cannot create inverse change: {0}")]
    CannotCreateInverse(String),

    /// Failed to apply undo/redo
    #[error("Failed to apply change: {0}")]
    ApplyFailed(String),

    /// Entry not found
    #[error("History entry not found: {0}")]
    NotFound(Uuid),
}

/// A single entry in the undo history
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// Plan ID this entry corresponds to
    pub id: Uuid,

    /// Human-readable description
    pub description: String,

    /// The forward (original) changes that were applied
    pub forward_changes: Vec<ConfigChange>,

    /// The inverse changes to undo this operation
    pub inverse_changes: Vec<ConfigChange>,

    /// When this change was applied
    pub applied_at: DateTime<Utc>,
}

impl HistoryEntry {
    /// Create a new history entry
    pub fn new(
        id: Uuid,
        description: String,
        forward_changes: Vec<ConfigChange>,
        inverse_changes: Vec<ConfigChange>,
    ) -> Self {
        Self {
            id,
            description,
            forward_changes,
            inverse_changes,
            applied_at: Utc::now(),
        }
    }
}

/// Undo/redo stack for configuration changes
#[derive(Debug)]
pub struct UndoStack {
    /// History entries (oldest to newest)
    entries: VecDeque<HistoryEntry>,

    /// Current position in the history (points to last applied entry)
    /// When `current_pos == entries.len()`, we're at the "present"
    /// When `current_pos < entries.len()`, we have undone entries that can be redone
    current_pos: usize,

    /// Maximum number of entries to keep
    max_size: usize,
}

impl Default for UndoStack {
    fn default() -> Self {
        Self::new()
    }
}

impl UndoStack {
    /// Create a new undo stack with default capacity
    pub fn new() -> Self {
        Self {
            entries: VecDeque::new(),
            current_pos: 0,
            max_size: DEFAULT_MAX_HISTORY_SIZE,
        }
    }

    /// Create an undo stack with custom maximum size
    pub fn with_max_size(max_size: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            current_pos: 0,
            max_size,
        }
    }

    /// Record a new entry after a plan is applied
    ///
    /// This will:
    /// 1. Discard any "future" entries if we've undone and then made a new change
    /// 2. Add the new entry
    /// 3. Trim old entries if over capacity
    pub fn record(
        &mut self,
        id: Uuid,
        description: String,
        forward_changes: Vec<ConfigChange>,
        config_before: &Config,
    ) -> Result<(), HistoryError> {
        // Create inverse changes
        let inverse_changes = create_inverse_changes(&forward_changes, config_before)?;

        // Discard any "future" entries (entries after current_pos)
        while self.entries.len() > self.current_pos {
            self.entries.pop_back();
        }

        // Create and push the new entry
        let entry = HistoryEntry::new(id, description, forward_changes, inverse_changes);
        self.entries.push_back(entry);
        self.current_pos = self.entries.len();

        // Trim old entries if over capacity
        while self.entries.len() > self.max_size {
            self.entries.pop_front();
            self.current_pos = self.current_pos.saturating_sub(1);
        }

        Ok(())
    }

    /// Check if undo is available
    pub fn can_undo(&self) -> bool {
        self.current_pos > 0
    }

    /// Check if redo is available
    pub fn can_redo(&self) -> bool {
        self.current_pos < self.entries.len()
    }

    /// Get the entry that would be undone
    pub fn peek_undo(&self) -> Option<&HistoryEntry> {
        if self.can_undo() {
            self.entries.get(self.current_pos - 1)
        } else {
            None
        }
    }

    /// Get the entry that would be redone
    pub fn peek_redo(&self) -> Option<&HistoryEntry> {
        if self.can_redo() {
            self.entries.get(self.current_pos)
        } else {
            None
        }
    }

    /// Perform an undo operation, returning the inverse changes to apply
    ///
    /// The caller is responsible for actually applying the changes to the config
    pub fn undo(&mut self) -> Result<&HistoryEntry, HistoryError> {
        if !self.can_undo() {
            return Err(HistoryError::NothingToUndo);
        }

        self.current_pos -= 1;
        Ok(&self.entries[self.current_pos])
    }

    /// Perform a redo operation, returning the forward changes to apply
    ///
    /// The caller is responsible for actually applying the changes to the config
    pub fn redo(&mut self) -> Result<&HistoryEntry, HistoryError> {
        if !self.can_redo() {
            return Err(HistoryError::NothingToRedo);
        }

        let entry = &self.entries[self.current_pos];
        self.current_pos += 1;
        Ok(entry)
    }

    /// Get number of entries that can be undone
    pub fn undo_count(&self) -> usize {
        self.current_pos
    }

    /// Get number of entries that can be redone
    pub fn redo_count(&self) -> usize {
        self.entries.len() - self.current_pos
    }

    /// Get all entries (for display/debugging)
    pub fn entries(&self) -> &VecDeque<HistoryEntry> {
        &self.entries
    }

    /// Get current position
    pub fn current_position(&self) -> usize {
        self.current_pos
    }

    /// Clear all history
    pub fn clear(&mut self) {
        self.entries.clear();
        self.current_pos = 0;
    }

    /// Get a summary of undo history for display
    pub fn undo_summary(&self, limit: usize) -> Vec<HistorySummary> {
        let start = self.current_pos.saturating_sub(limit);
        (start..self.current_pos)
            .rev()
            .filter_map(|i| self.entries.get(i))
            .map(|entry| HistorySummary {
                id: entry.id,
                description: entry.description.clone(),
                applied_at: entry.applied_at,
                changes_count: entry.forward_changes.len(),
            })
            .collect()
    }

    /// Get a summary of redo history for display
    pub fn redo_summary(&self, limit: usize) -> Vec<HistorySummary> {
        let end = (self.current_pos + limit).min(self.entries.len());
        (self.current_pos..end)
            .filter_map(|i| self.entries.get(i))
            .map(|entry| HistorySummary {
                id: entry.id,
                description: entry.description.clone(),
                applied_at: entry.applied_at,
                changes_count: entry.forward_changes.len(),
            })
            .collect()
    }
}

/// Summary of a history entry for display
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistorySummary {
    pub id: Uuid,
    pub description: String,
    pub applied_at: DateTime<Utc>,
    pub changes_count: usize,
}

/// Create inverse changes for a list of forward changes
///
/// # Arguments
/// * `forward_changes` - The changes that will be/were applied
/// * `config_before` - The configuration state before the changes are applied
///
/// The inverse changes should be in REVERSE order so when applied they
/// undo the changes in the opposite order they were originally applied.
fn create_inverse_changes(
    forward_changes: &[ConfigChange],
    config_before: &Config,
) -> Result<Vec<ConfigChange>, HistoryError> {
    use std::collections::HashMap;

    let mut inverse = Vec::with_capacity(forward_changes.len());

    // Track how many mappings have been added to each mode
    // so we can calculate correct indices for batch CreateMapping
    let mut mode_additions: HashMap<String, usize> = HashMap::new();
    // Slice 17 / gap B (#1143) — routes are a flat Vec (not
    // mode-scoped) so a single counter suffices for the CreateRoute
    // inverse's post-apply index calculation.
    let mut route_additions: usize = 0;

    for (change_pos, change) in forward_changes.iter().enumerate() {
        let inv = create_single_inverse_with_offset(
            change,
            config_before,
            &mode_additions,
            route_additions,
            forward_changes,
            change_pos,
        )?;
        inverse.push(inv);

        // Update offset tracking for CreateMapping
        if let ConfigChange::CreateMapping { mode, .. } = change {
            *mode_additions.entry(mode.clone()).or_default() += 1;
        }
        // Same shape for CreateRoute — append-only, so the inverse
        // arm uses `route_additions` (pre-increment) to land at
        // `base_len + route_additions` for the current op; then we
        // bump the counter so the next CreateRoute in the batch
        // sees the updated value.
        if matches!(change, ConfigChange::CreateRoute { .. }) {
            route_additions += 1;
        }
        // Note: DeleteMapping / DeleteRoute would decrement, but for
        // simplicity we don't support mixed create/delete in the same
        // batch for now.
    }

    inverse.reverse();
    Ok(inverse)
}

/// Create a single inverse change
///
/// # Arguments
/// * `change` - The forward change to create inverse for
/// * `config_before` - The configuration state before any changes
/// * `mode_additions` - Number of mappings already added to each mode in this batch
/// * `route_additions` - Number of routes already added in this batch (#1143 slice 17)
fn create_single_inverse_with_offset(
    change: &ConfigChange,
    config_before: &Config,
    mode_additions: &std::collections::HashMap<String, usize>,
    route_additions: usize,
    forward_changes: &[ConfigChange],
    change_pos: usize,
) -> Result<ConfigChange, HistoryError> {
    match change {
        ConfigChange::CreateMapping { mode, .. } => {
            // To undo CreateMapping, we delete the mapping that was added
            // The new mapping will be at the end of the mode's mappings
            let mode_obj = config_before
                .modes
                .iter()
                .find(|m| m.name == *mode)
                .ok_or_else(|| {
                    HistoryError::CannotCreateInverse(format!("Mode '{}' not found", mode))
                })?;

            // Account for mappings already added in this batch
            let offset = mode_additions.get(mode).copied().unwrap_or(0);

            Ok(ConfigChange::DeleteMapping {
                mode: mode.clone(),
                index: mode_obj.mappings.len() + offset, // Base + offset for batch creates
            })
        }

        ConfigChange::UpdateMapping { mode, index, .. } => {
            // To undo UpdateMapping, we restore the original mapping
            let mode_obj = config_before
                .modes
                .iter()
                .find(|m| m.name == *mode)
                .ok_or_else(|| {
                    HistoryError::CannotCreateInverse(format!("Mode '{}' not found", mode))
                })?;

            let original_mapping = mode_obj.mappings.get(*index).ok_or_else(|| {
                HistoryError::CannotCreateInverse(format!(
                    "Mapping at index {} not found in mode '{}'",
                    index, mode
                ))
            })?;

            Ok(ConfigChange::UpdateMapping {
                mode: mode.clone(),
                index: *index,
                trigger: original_mapping.trigger.clone(),
                action: original_mapping.action.clone(),
                description: original_mapping.description.clone(),
            })
        }

        ConfigChange::DeleteMapping { mode, index } => {
            // To undo DeleteMapping, we recreate the deleted mapping
            let mode_obj = config_before
                .modes
                .iter()
                .find(|m| m.name == *mode)
                .ok_or_else(|| {
                    HistoryError::CannotCreateInverse(format!("Mode '{}' not found", mode))
                })?;

            let original_mapping = mode_obj.mappings.get(*index).ok_or_else(|| {
                HistoryError::CannotCreateInverse(format!(
                    "Mapping at index {} not found in mode '{}'",
                    index, mode
                ))
            })?;

            let prior_lower_deletes = forward_changes
                .iter()
                .take(change_pos)
                .filter(|prior| matches!(prior, ConfigChange::DeleteMapping { mode: m, index: i } if m == mode && i < index))
                .count();
            let effective_index = index.saturating_sub(prior_lower_deletes);

            // #2121: restore via InsertMapping so undo reproduces the exact
            // configuration. In batched deletes we must target the same
            // effective index that `apply_atomic` deleted from.
            Ok(ConfigChange::InsertMapping {
                mode: mode.clone(),
                // #2121 follow-up: `apply_atomic` adjusts batch DeleteMapping
                // indices for index safety, so undo must restore at the
                // deletion-time effective index to preserve order when inverses
                // run in reverse.
                index: effective_index,
                trigger: original_mapping.trigger.clone(),
                action: original_mapping.action.clone(),
                description: original_mapping.description.clone(),
                // Preserve let-through when reconstructing for undo (ADR-038).
                let_through: original_mapping.let_through,
            })
        }

        ConfigChange::CreateMode { name, .. } => {
            // To undo CreateMode, we delete the mode
            Ok(ConfigChange::DeleteMode { name: name.clone() })
        }

        ConfigChange::DeleteMode { name } => {
            // #2121: restore the mode IN FULL — all mappings and fields, at its
            // original index — via RestoreMode. (Previously this emitted
            // CreateMode, which recreates an empty mode and silently drops every
            // mapping, so an undo did NOT restore the original configuration.)
            let index = config_before
                .modes
                .iter()
                .position(|m| m.name == *name)
                .ok_or_else(|| {
                    HistoryError::CannotCreateInverse(format!("Mode '{}' not found", name))
                })?;

            let prior_lower_mode_deletes = forward_changes
                .iter()
                .take(change_pos)
                .filter_map(|prior| match prior {
                    ConfigChange::DeleteMode { name: prior_name } => config_before
                        .modes
                        .iter()
                        .position(|m| m.name == *prior_name),
                    _ => None,
                })
                .filter(|prior_index| *prior_index < index)
                .count();
            let effective_index = index.saturating_sub(prior_lower_mode_deletes);

            Ok(ConfigChange::RestoreMode {
                index: effective_index,
                mode: config_before.modes[index].clone(),
            })
        }

        ConfigChange::InsertMapping { mode, index, .. } => {
            // InsertMapping is normally produced AS an inverse, but it is fully
            // invertible: undoing an insert removes the mapping at that index.
            Ok(ConfigChange::DeleteMapping {
                mode: mode.clone(),
                index: *index,
            })
        }

        ConfigChange::RestoreMode { mode, .. } => {
            // Symmetric inverse of a restore: delete the mode by name.
            Ok(ConfigChange::DeleteMode {
                name: mode.name.clone(),
            })
        }

        ConfigChange::CreateEndpoint { alias, .. } => {
            // ADR-035 Slice 8 (#1745) — `CreateEndpoint` ships before its
            // `DeleteEndpoint` inverse (a follow-up slice), mirroring how
            // `CreateConnector` was non-invertible until `DeleteConnector`
            // landed (#1243). Until then, an endpoint create is not
            // undoable via history — surface that clearly rather than
            // fabricating an inverse that can't be applied.
            Err(HistoryError::CannotCreateInverse(format!(
                "CreateEndpoint ('{}') has no inverse yet — DeleteEndpoint lands in a follow-up slice (ADR-035)",
                alias
            )))
        }

        ConfigChange::CreateRoute { .. } => {
            // Slice 17 / gap B (#1143) — `CreateRoute` is append-only,
            // so the post-apply index is `config_before.routes.len() +
            // route_additions` (offset accounts for sibling CreateRoute
            // ops earlier in the same batch). Same shape as the
            // CreateMapping → DeleteMapping inverse above.
            Ok(ConfigChange::DeleteRoute {
                index: config_before.routes.len() + route_additions,
            })
        }

        ConfigChange::DeleteRoute { index } => {
            // Slice 17 / gap B (#1143) — look the deleted route up in
            // `config_before` by its index and emit a `CreateRoute`
            // carrying the original fields. The post-undo route lands
            // appended at the END of `config.routes` (not at the
            // original `index`), since `CreateRoute` is append-only.
            // For a strict in-place restore, callers should record
            // the index separately; for now, undoability of the
            // route's existence (with all fields preserved) is the
            // useful guarantee.
            let original_route = config_before.routes.get(*index).ok_or_else(|| {
                HistoryError::CannotCreateInverse(format!(
                    "Route at index {} not found in config_before",
                    index
                ))
            })?;
            Ok(ConfigChange::CreateRoute {
                from: original_route.from.clone(),
                to: original_route.to.clone(),
                transform: original_route.transform.clone(),
                filter: original_route.filter.clone(),
                enabled: original_route.enabled,
                description: original_route.description.clone(),
            })
        }

        ConfigChange::UpdateRoute { index, .. } => {
            // Slice 17 / gap B (#1143) — `UpdateRoute` total-replaces;
            // inverse total-replaces back to the pre-update fields
            // pulled from `config_before` at the same index. Same
            // pattern as `UpdateMapping`'s inverse above.
            let original_route = config_before.routes.get(*index).ok_or_else(|| {
                HistoryError::CannotCreateInverse(format!(
                    "Route at index {} not found in config_before",
                    index
                ))
            })?;
            Ok(ConfigChange::UpdateRoute {
                index: *index,
                from: original_route.from.clone(),
                to: original_route.to.clone(),
                transform: original_route.transform.clone(),
                filter: original_route.filter.clone(),
                enabled: original_route.enabled,
                description: original_route.description.clone(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conductor_core::config::{ActionConfig, Mapping, Mode, Trigger};

    fn create_test_config() -> Config {
        Config {
            mcp: Default::default(),
            per_app_modes: None,
            config_meta: Default::default(),
            security: Default::default(),
            endpoints: vec![],
            modes: vec![Mode {
                name: "Default".to_string(),
                color: Some("blue".to_string()),
                mappings: vec![
                    Mapping {
                        trigger: Trigger::Note {
                            note: 36,
                            velocity_min: Some(1),
                            channel: None,
                            device: None,
                        },
                        action: ActionConfig::Keystroke {
                            keys: "c".to_string(),
                            modifiers: vec!["cmd".to_string()],
                        },
                        description: Some("Copy".to_string()),
                        let_through: false,
                    },
                    Mapping {
                        trigger: Trigger::Note {
                            note: 37,
                            velocity_min: Some(1),
                            channel: None,
                            device: None,
                        },
                        action: ActionConfig::Keystroke {
                            keys: "v".to_string(),
                            modifiers: vec!["cmd".to_string()],
                        },
                        description: Some("Paste".to_string()),
                        let_through: false,
                    },
                ],
            }],
            global_mappings: vec![],
            logging: None,
            advanced_settings: Default::default(),
            last_selected_mode: None,
            default_mode: None,
            led: None,
            event_console: None,
            routes: vec![],
        }
    }

    #[test]
    fn test_undo_stack_new() {
        let stack = UndoStack::new();
        assert_eq!(stack.undo_count(), 0);
        assert_eq!(stack.redo_count(), 0);
        assert!(!stack.can_undo());
        assert!(!stack.can_redo());
    }

    #[test]
    fn test_record_entry() {
        let mut stack = UndoStack::new();
        let config = create_test_config();

        let changes = vec![ConfigChange::CreateMapping {
            mode: "Default".to_string(),
            trigger: Trigger::Note {
                note: 38,
                velocity_min: Some(1),
                channel: None,
                device: None,
            },
            action: ActionConfig::Keystroke {
                keys: "x".to_string(),
                modifiers: vec!["cmd".to_string()],
            },
            description: Some("Cut".to_string()),
            let_through: false,
        }];

        stack
            .record(
                Uuid::new_v4(),
                "Create Cut mapping".to_string(),
                changes,
                &config,
            )
            .unwrap();

        assert_eq!(stack.undo_count(), 1);
        assert_eq!(stack.redo_count(), 0);
        assert!(stack.can_undo());
        assert!(!stack.can_redo());
    }

    #[test]
    fn test_undo_operation() {
        let mut stack = UndoStack::new();
        let config = create_test_config();

        let changes = vec![ConfigChange::CreateMapping {
            mode: "Default".to_string(),
            trigger: Trigger::Note {
                note: 38,
                velocity_min: Some(1),
                channel: None,
                device: None,
            },
            action: ActionConfig::Keystroke {
                keys: "x".to_string(),
                modifiers: vec!["cmd".to_string()],
            },
            description: Some("Cut".to_string()),
            let_through: false,
        }];

        stack
            .record(
                Uuid::new_v4(),
                "Create Cut mapping".to_string(),
                changes,
                &config,
            )
            .unwrap();

        // Undo should return the entry
        let entry = stack.undo().unwrap();
        assert_eq!(entry.description, "Create Cut mapping");

        // Inverse should be a DeleteMapping
        assert_eq!(entry.inverse_changes.len(), 1);
        match &entry.inverse_changes[0] {
            ConfigChange::DeleteMapping { mode, index } => {
                assert_eq!(mode, "Default");
                assert_eq!(*index, 2); // New mapping would be at index 2
            }
            _ => panic!("Expected DeleteMapping inverse"),
        }

        assert_eq!(stack.undo_count(), 0);
        assert_eq!(stack.redo_count(), 1);
    }

    #[test]
    fn test_redo_operation() {
        let mut stack = UndoStack::new();
        let config = create_test_config();

        let id = Uuid::new_v4();
        let changes = vec![ConfigChange::CreateMapping {
            mode: "Default".to_string(),
            trigger: Trigger::Note {
                note: 38,
                velocity_min: Some(1),
                channel: None,
                device: None,
            },
            action: ActionConfig::Keystroke {
                keys: "x".to_string(),
                modifiers: vec!["cmd".to_string()],
            },
            description: Some("Cut".to_string()),
            let_through: false,
        }];

        stack
            .record(
                id,
                "Create Cut mapping".to_string(),
                changes.clone(),
                &config,
            )
            .unwrap();

        // Undo then redo
        stack.undo().unwrap();
        assert!(stack.can_redo());

        let entry = stack.redo().unwrap();
        assert_eq!(entry.id, id);
        assert_eq!(entry.forward_changes.len(), 1);

        assert_eq!(stack.undo_count(), 1);
        assert_eq!(stack.redo_count(), 0);
    }

    #[test]
    fn test_new_change_discards_redo_history() {
        let mut stack = UndoStack::new();
        let config = create_test_config();

        // Record two entries
        stack
            .record(
                Uuid::new_v4(),
                "Change 1".to_string(),
                vec![ConfigChange::CreateMode {
                    name: "Mode1".to_string(),
                    color: None,
                }],
                &config,
            )
            .unwrap();

        stack
            .record(
                Uuid::new_v4(),
                "Change 2".to_string(),
                vec![ConfigChange::CreateMode {
                    name: "Mode2".to_string(),
                    color: None,
                }],
                &config,
            )
            .unwrap();

        assert_eq!(stack.entries.len(), 2);

        // Undo one
        stack.undo().unwrap();
        assert_eq!(stack.redo_count(), 1);

        // Record new change - should discard the redo
        stack
            .record(
                Uuid::new_v4(),
                "Change 3".to_string(),
                vec![ConfigChange::CreateMode {
                    name: "Mode3".to_string(),
                    color: None,
                }],
                &config,
            )
            .unwrap();

        assert_eq!(stack.entries.len(), 2); // "Change 2" was discarded
        assert_eq!(stack.redo_count(), 0);
    }

    #[test]
    fn test_max_size_enforcement() {
        let mut stack = UndoStack::with_max_size(3);
        let config = create_test_config();

        for i in 0..5 {
            stack
                .record(
                    Uuid::new_v4(),
                    format!("Change {}", i),
                    vec![ConfigChange::CreateMode {
                        name: format!("Mode{}", i),
                        color: None,
                    }],
                    &config,
                )
                .unwrap();
        }

        // Should only keep the last 3
        assert_eq!(stack.entries.len(), 3);
        assert_eq!(stack.undo_count(), 3);

        // Oldest entries (0, 1) should have been trimmed
        let summaries = stack.undo_summary(10);
        assert_eq!(summaries.len(), 3);
        assert!(summaries[0].description.contains("4")); // Most recent
        assert!(summaries[2].description.contains("2")); // Oldest remaining
    }

    #[test]
    fn test_inverse_for_update_mapping() {
        let mut stack = UndoStack::new();
        let config = create_test_config();

        // Update the first mapping
        let changes = vec![ConfigChange::UpdateMapping {
            mode: "Default".to_string(),
            index: 0,
            trigger: Trigger::Note {
                note: 50,
                velocity_min: Some(1),
                channel: None,
                device: None,
            },
            action: ActionConfig::Keystroke {
                keys: "z".to_string(),
                modifiers: vec!["cmd".to_string()],
            },
            description: Some("Undo".to_string()),
        }];

        stack
            .record(
                Uuid::new_v4(),
                "Update mapping".to_string(),
                changes,
                &config,
            )
            .unwrap();

        let entry = stack.undo().unwrap();

        // Inverse should restore the original mapping
        match &entry.inverse_changes[0] {
            ConfigChange::UpdateMapping {
                mode,
                index,
                trigger,
                action,
                description,
            } => {
                assert_eq!(mode, "Default");
                assert_eq!(*index, 0);
                // Should restore original trigger (note 36)
                match trigger {
                    Trigger::Note { note, .. } => assert_eq!(*note, 36),
                    _ => panic!("Expected Note trigger"),
                }
                // Should restore original action (Copy)
                match action {
                    ActionConfig::Keystroke { keys, .. } => assert_eq!(keys, "c"),
                    _ => panic!("Expected Keystroke action"),
                }
                assert_eq!(*description, Some("Copy".to_string()));
            }
            _ => panic!("Expected UpdateMapping inverse"),
        }
    }

    #[test]
    fn test_inverse_for_delete_mapping() {
        let mut stack = UndoStack::new();
        let config = create_test_config();

        // Delete the first mapping
        let changes = vec![ConfigChange::DeleteMapping {
            mode: "Default".to_string(),
            index: 0,
        }];

        stack
            .record(
                Uuid::new_v4(),
                "Delete mapping".to_string(),
                changes,
                &config,
            )
            .unwrap();

        let entry = stack.undo().unwrap();

        // #2121: the inverse must restore the mapping at its ORIGINAL index
        // (InsertMapping), not append it (CreateMapping).
        match &entry.inverse_changes[0] {
            ConfigChange::InsertMapping {
                mode,
                index,
                trigger,
                action,
                description,
                ..
            } => {
                assert_eq!(mode, "Default");
                assert_eq!(*index, 0, "must restore at the original index");
                match trigger {
                    Trigger::Note { note, .. } => assert_eq!(*note, 36),
                    _ => panic!("Expected Note trigger"),
                }
                match action {
                    ActionConfig::Keystroke { keys, .. } => assert_eq!(keys, "c"),
                    _ => panic!("Expected Keystroke action"),
                }
                assert_eq!(*description, Some("Copy".to_string()));
            }
            other => panic!("Expected InsertMapping inverse, got {other:?}"),
        }
    }

    #[test]
    fn test_inverse_for_delete_mode() {
        let mut stack = UndoStack::new();
        let config = create_test_config();

        // Delete the mode
        let changes = vec![ConfigChange::DeleteMode {
            name: "Default".to_string(),
        }];

        stack
            .record(Uuid::new_v4(), "Delete mode".to_string(), changes, &config)
            .unwrap();

        let entry = stack.undo().unwrap();

        // #2121: the inverse must restore the mode IN FULL (RestoreMode with all
        // mappings + index), not an empty CreateMode that drops the mappings.
        match &entry.inverse_changes[0] {
            ConfigChange::RestoreMode { index, mode } => {
                assert_eq!(*index, 0, "must restore at the original index");
                assert_eq!(mode.name, "Default");
                assert_eq!(mode.color, Some("blue".to_string()));
                assert_eq!(
                    mode.mappings.len(),
                    2,
                    "all mappings must be carried in the inverse, not dropped"
                );
            }
            other => panic!("Expected RestoreMode inverse, got {other:?}"),
        }
    }

    /// #2121 end-to-end: deleting a mode then applying the recorded inverse
    /// must restore the ORIGINAL configuration exactly — every mapping, color,
    /// and position — not an empty mode.
    #[test]
    fn test_undo_delete_mode_restores_full_config() {
        use crate::daemon::llm::plan::apply_change;

        let original = create_test_config();
        let mut working = original.clone();
        let mut stack = UndoStack::new();

        // Record the delete (inverse derived from the pre-delete config) then
        // actually apply the forward delete to `working`.
        stack
            .record(
                Uuid::new_v4(),
                "Delete mode".to_string(),
                vec![ConfigChange::DeleteMode {
                    name: "Default".to_string(),
                }],
                &working,
            )
            .unwrap();
        apply_change(
            &mut working,
            ConfigChange::DeleteMode {
                name: "Default".to_string(),
            },
        )
        .unwrap();
        assert!(working.modes.is_empty(), "precondition: mode was deleted");

        // Undo: apply every inverse change.
        let inverses = stack.undo().unwrap().inverse_changes.clone();
        for change in inverses {
            apply_change(&mut working, change).unwrap();
        }

        // The mode — with BOTH mappings, color, and position — is fully back.
        assert_eq!(working.modes.len(), 1);
        let m = &working.modes[0];
        assert_eq!(m.name, "Default");
        assert_eq!(m.color, Some("blue".to_string()));
        assert_eq!(
            m.mappings.len(),
            original.modes[0].mappings.len(),
            "undo must restore all mappings, not an empty mode"
        );
        assert_eq!(m.mappings[0].description, Some("Copy".to_string()));
        assert_eq!(m.mappings[1].description, Some("Paste".to_string()));
    }

    /// #2121 end-to-end: deleting a non-last mapping then applying the inverse
    /// must restore it at its ORIGINAL index, not append it at the end.
    #[test]
    fn test_undo_delete_mapping_restores_position() {
        use crate::daemon::llm::plan::apply_change;

        let mut working = create_test_config(); // [Copy@0, Paste@1]
        let mut stack = UndoStack::new();

        stack
            .record(
                Uuid::new_v4(),
                "Delete first mapping".to_string(),
                vec![ConfigChange::DeleteMapping {
                    mode: "Default".to_string(),
                    index: 0,
                }],
                &working,
            )
            .unwrap();
        apply_change(
            &mut working,
            ConfigChange::DeleteMapping {
                mode: "Default".to_string(),
                index: 0,
            },
        )
        .unwrap();
        // Precondition: only "Paste" remains.
        assert_eq!(working.modes[0].mappings.len(), 1);
        assert_eq!(
            working.modes[0].mappings[0].description,
            Some("Paste".to_string())
        );

        let inverses = stack.undo().unwrap().inverse_changes.clone();
        for change in inverses {
            apply_change(&mut working, change).unwrap();
        }

        // "Copy" is back at index 0 (not appended after "Paste").
        let maps = &working.modes[0].mappings;
        assert_eq!(maps.len(), 2);
        assert_eq!(
            maps[0].description,
            Some("Copy".to_string()),
            "deleted mapping must return to its original index 0, not the end"
        );
        assert_eq!(maps[1].description, Some("Paste".to_string()));
    }

    /// #2121 review (batch): deleting MULTIPLE mappings in one entry then
    /// undoing must restore every mapping at its original index — not the
    /// descending order the blanket `reverse()` produced.
    #[test]
    fn test_undo_batch_delete_mappings_restores_order() {
        use crate::daemon::llm::plan::apply_change;

        // [Copy@0, Paste@1, Cut@2]
        let mut working = create_test_config();
        working.modes[0].mappings.push(Mapping {
            trigger: Trigger::Note {
                note: 38,
                velocity_min: Some(1),
                channel: None,
                device: None,
            },
            action: ActionConfig::Keystroke {
                keys: "x".to_string(),
                modifiers: vec!["cmd".to_string()],
            },
            description: Some("Cut".to_string()),
            let_through: false,
        });
        let mut stack = UndoStack::new();

        // One entry that deletes the first two mappings (original indices 0,1).
        let forward = vec![
            ConfigChange::DeleteMapping {
                mode: "Default".to_string(),
                index: 0,
            },
            ConfigChange::DeleteMapping {
                mode: "Default".to_string(),
                index: 1,
            },
        ];
        stack
            .record(
                Uuid::new_v4(),
                "batch delete".to_string(),
                forward,
                &working,
            )
            .unwrap();

        // Apply the deletes in the same effective order `apply_atomic` uses
        // for forward [0, 1]: delete index 0, then index 0 again.
        apply_change(
            &mut working,
            ConfigChange::DeleteMapping {
                mode: "Default".to_string(),
                index: 0,
            },
        )
        .unwrap();
        apply_change(
            &mut working,
            ConfigChange::DeleteMapping {
                mode: "Default".to_string(),
                index: 0,
            },
        )
        .unwrap();
        assert_eq!(working.modes[0].mappings.len(), 1);
        assert_eq!(
            working.modes[0].mappings[0].description,
            Some("Cut".to_string())
        );

        // Undo the whole entry.
        let inverses = stack.undo().unwrap().inverse_changes.clone();
        assert_eq!(inverses.len(), 2);
        assert!(matches!(
            &inverses[0],
            ConfigChange::InsertMapping {
                mode,
                index: 0,
                description: Some(desc),
                ..
            } if mode == "Default" && desc == "Paste"
        ));
        assert!(matches!(
            &inverses[1],
            ConfigChange::InsertMapping {
                mode,
                index: 0,
                description: Some(desc),
                ..
            } if mode == "Default" && desc == "Copy"
        ));
        for c in inverses {
            apply_change(&mut working, c).unwrap();
        }

        let m = &working.modes[0].mappings;
        assert_eq!(m.len(), 3);
        assert_eq!(m[0].description, Some("Copy".to_string()));
        assert_eq!(m[1].description, Some("Paste".to_string()));
        assert_eq!(m[2].description, Some("Cut".to_string()));
    }

    /// #2121 review (batch): deleting MULTIPLE modes in one entry then undoing
    /// must restore them all at their original positions.
    #[test]
    fn test_undo_batch_delete_modes_restores_order() {
        use crate::daemon::llm::plan::apply_change;

        // [Default@0, Second@1, Third@2]
        let mut working = create_test_config();
        working.modes.push(Mode {
            name: "Second".to_string(),
            color: Some("red".to_string()),
            mappings: vec![],
        });
        working.modes.push(Mode {
            name: "Third".to_string(),
            color: Some("green".to_string()),
            mappings: vec![],
        });
        let mut stack = UndoStack::new();

        let forward = vec![
            ConfigChange::DeleteMode {
                name: "Default".to_string(),
            },
            ConfigChange::DeleteMode {
                name: "Second".to_string(),
            },
        ];
        stack
            .record(
                Uuid::new_v4(),
                "batch delete modes".to_string(),
                forward,
                &working,
            )
            .unwrap();

        apply_change(
            &mut working,
            ConfigChange::DeleteMode {
                name: "Default".to_string(),
            },
        )
        .unwrap();
        apply_change(
            &mut working,
            ConfigChange::DeleteMode {
                name: "Second".to_string(),
            },
        )
        .unwrap();
        assert_eq!(working.modes.len(), 1);
        assert_eq!(working.modes[0].name, "Third");

        let inverses = stack.undo().unwrap().inverse_changes.clone();
        assert_eq!(inverses.len(), 2);
        assert!(matches!(
            &inverses[0],
            ConfigChange::RestoreMode {
                index: 0,
                mode
            } if mode.name == "Second"
        ));
        assert!(matches!(
            &inverses[1],
            ConfigChange::RestoreMode {
                index: 0,
                mode
            } if mode.name == "Default"
        ));
        for c in inverses {
            apply_change(&mut working, c).unwrap();
        }

        let names: Vec<&str> = working.modes.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["Default", "Second", "Third"],
            "all modes restored at their original positions"
        );
    }

    #[test]
    fn test_nothing_to_undo() {
        let mut stack = UndoStack::new();
        let result = stack.undo();
        assert!(matches!(result, Err(HistoryError::NothingToUndo)));
    }

    #[test]
    fn test_nothing_to_redo() {
        let mut stack = UndoStack::new();
        let result = stack.redo();
        assert!(matches!(result, Err(HistoryError::NothingToRedo)));
    }

    #[test]
    fn test_clear_history() {
        let mut stack = UndoStack::new();
        let config = create_test_config();

        stack
            .record(
                Uuid::new_v4(),
                "Change".to_string(),
                vec![ConfigChange::CreateMode {
                    name: "Test".to_string(),
                    color: None,
                }],
                &config,
            )
            .unwrap();

        stack.clear();

        assert_eq!(stack.undo_count(), 0);
        assert_eq!(stack.redo_count(), 0);
        assert!(stack.entries.is_empty());
    }

    #[test]
    fn test_undo_summary() {
        let mut stack = UndoStack::new();
        let config = create_test_config();

        for i in 0..5 {
            stack
                .record(
                    Uuid::new_v4(),
                    format!("Change {}", i),
                    vec![ConfigChange::CreateMode {
                        name: format!("Mode{}", i),
                        color: None,
                    }],
                    &config,
                )
                .unwrap();
        }

        let summary = stack.undo_summary(3);
        assert_eq!(summary.len(), 3);
        // Most recent first
        assert!(summary[0].description.contains("4"));
        assert!(summary[1].description.contains("3"));
        assert!(summary[2].description.contains("2"));
    }

    #[test]
    fn test_redo_summary() {
        let mut stack = UndoStack::new();
        let config = create_test_config();

        for i in 0..5 {
            stack
                .record(
                    Uuid::new_v4(),
                    format!("Change {}", i),
                    vec![ConfigChange::CreateMode {
                        name: format!("Mode{}", i),
                        color: None,
                    }],
                    &config,
                )
                .unwrap();
        }

        // Undo 3 times
        stack.undo().unwrap();
        stack.undo().unwrap();
        stack.undo().unwrap();

        let summary = stack.redo_summary(10);
        assert_eq!(summary.len(), 3);
        // Oldest first (in order they would be redone)
        assert!(summary[0].description.contains("2"));
        assert!(summary[1].description.contains("3"));
        assert!(summary[2].description.contains("4"));
    }

    #[test]
    fn test_peek_undo_redo() {
        let mut stack = UndoStack::new();
        let config = create_test_config();

        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();

        stack
            .record(
                id1,
                "Change 1".to_string(),
                vec![ConfigChange::CreateMode {
                    name: "Mode1".to_string(),
                    color: None,
                }],
                &config,
            )
            .unwrap();

        stack
            .record(
                id2,
                "Change 2".to_string(),
                vec![ConfigChange::CreateMode {
                    name: "Mode2".to_string(),
                    color: None,
                }],
                &config,
            )
            .unwrap();

        // Peek undo should show Change 2 (most recent)
        let peek = stack.peek_undo().unwrap();
        assert_eq!(peek.id, id2);

        // No redo available
        assert!(stack.peek_redo().is_none());

        // Undo once
        stack.undo().unwrap();

        // Now peek undo should show Change 1
        let peek = stack.peek_undo().unwrap();
        assert_eq!(peek.id, id1);

        // And peek redo should show Change 2
        let peek = stack.peek_redo().unwrap();
        assert_eq!(peek.id, id2);
    }

    #[test]
    fn test_multiple_changes_in_entry() {
        let mut stack = UndoStack::new();
        let config = create_test_config();

        // Batch with multiple changes
        let changes = vec![
            ConfigChange::CreateMapping {
                mode: "Default".to_string(),
                trigger: Trigger::Note {
                    note: 38,
                    velocity_min: Some(1),
                    channel: None,
                    device: None,
                },
                action: ActionConfig::Keystroke {
                    keys: "x".to_string(),
                    modifiers: vec!["cmd".to_string()],
                },
                description: Some("Cut".to_string()),
                let_through: false,
            },
            ConfigChange::CreateMapping {
                mode: "Default".to_string(),
                trigger: Trigger::Note {
                    note: 39,
                    velocity_min: Some(1),
                    channel: None,
                    device: None,
                },
                action: ActionConfig::Keystroke {
                    keys: "a".to_string(),
                    modifiers: vec!["cmd".to_string()],
                },
                description: Some("Select All".to_string()),
                let_through: false,
            },
        ];

        stack
            .record(Uuid::new_v4(), "Batch create".to_string(), changes, &config)
            .unwrap();

        let entry = stack.undo().unwrap();

        // Should have 2 inverse changes in reverse order
        assert_eq!(entry.inverse_changes.len(), 2);

        // First inverse should be for the second change (Select All)
        match &entry.inverse_changes[0] {
            ConfigChange::DeleteMapping { mode, index } => {
                assert_eq!(mode, "Default");
                assert_eq!(*index, 3); // Would be at index 3
            }
            _ => panic!("Expected DeleteMapping"),
        }

        // Second inverse should be for the first change (Cut)
        match &entry.inverse_changes[1] {
            ConfigChange::DeleteMapping { mode, index } => {
                assert_eq!(mode, "Default");
                assert_eq!(*index, 2); // Would be at index 2
            }
            _ => panic!("Expected DeleteMapping"),
        }
    }
}

// ── ADR-031 P3 slice 17 (gap B from the 2026-05-16 mid-flight audit
// on #1143) — undo/redo for route + connector ops. The 6 arms below
// were `CannotCreateInverse(...)` placeholders until this slice; each
// test pins one inverse-creation contract so a future regression
// dropping the inverse fails CI rather than silently no-op-ing undo.

#[cfg(test)]
mod route_inverse_tests {
    use super::*;
    use conductor_core::config::Mode;
    use conductor_core::config::types::RouteConfig;

    /// Minimal config — no routes, one mode. Each test pushes routes
    /// into `config.routes` as needed. Local to this mod (the parent
    /// `tests::create_test_config` isn't `pub(super)`).
    fn create_test_config() -> Config {
        Config {
            mcp: Default::default(),
            per_app_modes: None,
            config_meta: Default::default(),
            security: Default::default(),
            endpoints: vec![],
            modes: vec![Mode {
                name: "Default".to_string(),
                color: Some("blue".to_string()),
                mappings: vec![],
            }],
            global_mappings: vec![],
            logging: None,
            advanced_settings: Default::default(),
            last_selected_mode: None,
            default_mode: None,
            led: None,
            event_console: None,
            routes: vec![],
        }
    }

    fn sample_route(from: &str, to: &str) -> RouteConfig {
        RouteConfig {
            from: from.to_string(),
            to: to.to_string(),
            transform: None,
            filter: None,
            enabled: true,
            description: Some(format!("{from} → {to}")),
            modes: Vec::new(),
        }
    }

    #[test]
    fn test_inverse_create_route_is_delete_route_at_appended_index() {
        let mut stack = UndoStack::new();
        let mut config = create_test_config();
        // Start with one existing route; the CreateRoute lands at index 1
        config.routes.push(sample_route("mikro", "drums"));
        let forward = vec![ConfigChange::CreateRoute {
            from: "mikro".to_string(),
            to: "absynth".to_string(),
            transform: None,
            filter: None,
            enabled: true,
            description: None,
        }];
        stack
            .record(Uuid::new_v4(), "add route".to_string(), forward, &config)
            .expect("record must succeed (CreateRoute inverse landed in slice 17)");
        let entry = stack.undo().unwrap();
        match &entry.inverse_changes[0] {
            ConfigChange::DeleteRoute { index } => {
                // Post-apply, the new route is at index 1 (after the
                // existing route at index 0).
                assert_eq!(*index, 1);
            }
            other => panic!("expected DeleteRoute inverse; got {other:?}"),
        }
    }

    #[test]
    fn test_inverse_create_route_batch_offset_tracking() {
        // Two CreateRoute ops in one batch — the second one lands at
        // a higher index post-apply because the first already added
        // to config.routes. The inverse-creation code's
        // `route_additions` offset must account for this.
        let mut stack = UndoStack::new();
        let config = create_test_config(); // no routes initially
        let forward = vec![
            ConfigChange::CreateRoute {
                from: "mikro".to_string(),
                to: "absynth".to_string(),
                transform: None,
                filter: None,
                enabled: true,
                description: None,
            },
            ConfigChange::CreateRoute {
                from: "mikro".to_string(),
                to: "drums".to_string(),
                transform: None,
                filter: None,
                enabled: true,
                description: None,
            },
        ];
        stack
            .record(
                Uuid::new_v4(),
                "add two routes".to_string(),
                forward,
                &config,
            )
            .expect("record must succeed for batch CreateRoute");
        let entry = stack.undo().unwrap();
        // Inverses are reversed for undo order — so inverse[0]
        // (applied first on undo) deletes index 1, then inverse[1]
        // deletes index 0. (Higher index first so the lower-index
        // delete still finds its route at the right position.)
        assert_eq!(entry.inverse_changes.len(), 2);
        match (&entry.inverse_changes[0], &entry.inverse_changes[1]) {
            (ConfigChange::DeleteRoute { index: i0 }, ConfigChange::DeleteRoute { index: i1 }) => {
                assert_eq!(*i0, 1, "first undo (reversed) deletes higher index");
                assert_eq!(*i1, 0, "second undo deletes the remaining index");
            }
            other => panic!("expected two DeleteRoute inverses; got {other:?}"),
        }
    }

    #[test]
    fn test_inverse_delete_route_is_create_route_with_originals() {
        let mut stack = UndoStack::new();
        let mut config = create_test_config();
        let original_route = sample_route("mikro", "absynth");
        config.routes.push(original_route.clone());
        let forward = vec![ConfigChange::DeleteRoute { index: 0 }];
        stack
            .record(Uuid::new_v4(), "drop route".to_string(), forward, &config)
            .expect("record must succeed (DeleteRoute inverse landed in slice 17)");
        let entry = stack.undo().unwrap();
        match &entry.inverse_changes[0] {
            ConfigChange::CreateRoute {
                from,
                to,
                enabled,
                description,
                ..
            } => {
                assert_eq!(from, &original_route.from);
                assert_eq!(to, &original_route.to);
                assert_eq!(*enabled, original_route.enabled);
                assert_eq!(description, &original_route.description);
            }
            other => panic!("expected CreateRoute inverse; got {other:?}"),
        }
    }

    #[test]
    fn test_inverse_update_route_is_update_route_with_originals() {
        let mut stack = UndoStack::new();
        let mut config = create_test_config();
        let original_route = sample_route("mikro", "absynth");
        config.routes.push(original_route.clone());
        let forward = vec![ConfigChange::UpdateRoute {
            index: 0,
            from: "mikro".to_string(),
            to: "drums".to_string(), // total-replace to a different target
            transform: None,
            filter: None,
            enabled: false,
            description: Some("new description".to_string()),
        }];
        stack
            .record(Uuid::new_v4(), "update route".to_string(), forward, &config)
            .expect("record must succeed (UpdateRoute inverse landed in slice 17)");
        let entry = stack.undo().unwrap();
        match &entry.inverse_changes[0] {
            ConfigChange::UpdateRoute {
                index,
                from,
                to,
                enabled,
                description,
                ..
            } => {
                assert_eq!(*index, 0);
                // Inverse restores ORIGINAL fields
                assert_eq!(from, &original_route.from);
                assert_eq!(to, &original_route.to);
                assert_eq!(*enabled, original_route.enabled);
                assert_eq!(description, &original_route.description);
            }
            other => panic!("expected UpdateRoute inverse; got {other:?}"),
        }
    }
}
