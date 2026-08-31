// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Configuration change plans with TOCTOU protection (ADR-007 Phase 2)
//!
//! ConfigPlan provides safe configuration modifications by:
//! 1. Capturing base state hash at plan creation
//! 2. Validating hash matches before apply
//! 3. Enforcing 5-minute TTL to prevent stale plans

use chrono::{DateTime, Duration, Utc};
use conductor_core::actions::{parse_keys, parse_modifier};
use conductor_core::config::validator::validate_config;
use conductor_core::config::{ActionConfig, Config, Mapping, Trigger};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt::Debug;
use thiserror::Error;
use uuid::Uuid;

use crate::daemon::keystroke_policy::{KeystrokePolicy, check_combo_only};

/// Default for `CreateEndpoint.enabled` serde default
fn default_enabled_true() -> bool {
    true
}

/// Format a serializable value as compact JSON, falling back to Debug format (#271)
fn format_value<T: Serialize + Debug>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| format!("{:?}", v))
}

/// Plan expiration time in minutes
const PLAN_TTL_MINUTES: i64 = 5;

/// Errors that can occur during plan operations
#[derive(Debug, Error, Clone, PartialEq)]
pub enum PlanError {
    /// Plan has expired (older than 5 minutes)
    #[error("Plan expired at {0}")]
    Expired(DateTime<Utc>),

    /// Configuration changed since plan was created (TOCTOU violation)
    #[error(
        "Configuration changed since plan was created (expected hash: {expected}, current: {actual})"
    )]
    StateChanged { expected: String, actual: String },

    /// Mode not found in configuration
    #[error("Mode not found: {0}")]
    ModeNotFound(String),

    /// Mapping index out of range
    #[error("Mapping index {index} out of range for mode '{mode}' (has {count} mappings)")]
    IndexOutOfRange {
        mode: String,
        index: usize,
        count: usize,
    },

    /// Invalid trigger configuration
    #[error("Invalid trigger: {0}")]
    InvalidTrigger(String),

    /// Invalid action configuration
    #[error("Invalid action: {0}")]
    InvalidAction(String),

    /// Plan not found
    #[error("Plan not found: {0}")]
    NotFound(Uuid),

    /// ADR-027 D2 — the resulting configuration would fail
    /// `conductor_core::config::validation::validate_config()`.
    /// The plan is rejected before the caller's config is mutated;
    /// `errors` contains the structural validator's error report
    /// (formatted via `ValidationReport::format_errors()`) so the
    /// LLM (or operator) can correct the plan and resubmit.
    ///
    /// Audit Attack Chain C: previously `apply_plan` modified the
    /// in-memory config without revalidating, so an MCP client could
    /// create plans containing arbitrary shell commands and bypass
    /// every check that runs on config-file load.
    #[error("Plan would produce an invalid configuration: {errors}")]
    ValidationFailed { errors: String },
}

/// A single configuration change operation
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ConfigChange {
    /// Create a new mapping in a mode
    CreateMapping {
        mode: String,
        trigger: Trigger,
        action: ActionConfig,
        description: Option<String>,
        /// ADR-038: fire the action AND let the event continue to routes.
        /// `#[serde(default)]` so plans serialized before Slice 6 (no field)
        /// still deserialize to the pre-ADR-038 swallow behaviour.
        #[serde(default)]
        let_through: bool,
    },

    /// Update an existing mapping
    UpdateMapping {
        mode: String,
        index: usize,
        trigger: Trigger,
        action: ActionConfig,
        description: Option<String>,
    },

    /// Delete a mapping
    DeleteMapping { mode: String, index: usize },

    /// Create a new mode
    CreateMode { name: String, color: Option<String> },

    /// Delete a mode
    DeleteMode { name: String },

    /// Restore a previously-deleted mapping at its ORIGINAL index (#2121).
    ///
    /// This is the inverse of [`ConfigChange::DeleteMapping`]. Unlike
    /// `CreateMapping` — which always appends — `InsertMapping` preserves the
    /// mapping's position, so undoing a delete restores the exact original
    /// configuration rather than moving the mapping to the end.
    InsertMapping {
        mode: String,
        index: usize,
        trigger: Trigger,
        action: ActionConfig,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(default)]
        let_through: bool,
    },

    /// Restore a previously-deleted mode IN FULL at its ORIGINAL index (#2121).
    ///
    /// This is the inverse of [`ConfigChange::DeleteMode`]. `CreateMode` only
    /// recreates an empty mode (name + color), silently dropping the mode's
    /// mappings; `RestoreMode` carries the entire original [`Mode`] so an undo
    /// restores every mapping and field exactly, at the correct position.
    RestoreMode {
        index: usize,
        mode: conductor_core::config::Mode,
    },

    /// Create a unified endpoint in [[endpoints]] (ADR-035 Slice 8). Pushed onto
    /// `config.endpoints` by `apply()`. `direction` is required (no serde default
    /// — R2 P1); `protocol` is optional (inferred from `kind` at load when
    /// omitted). Alias must be unique across endpoints + bindings + connectors
    /// (the shared namespace `normalize_to_endpoints` enforces at load).
    CreateEndpoint {
        alias: String,
        direction: conductor_core::config::types::ConnectorDirection,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        protocol: Option<conductor_core::config::types::ConnectorProtocol>,
        kind: conductor_core::config::types::EndpointKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(default = "default_enabled_true")]
        enabled: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        channels: Vec<u8>,
    },

    /// Create a new route in [[routes]] (ADR-031 P3 § 5.4 — issue #1143).
    ///
    /// Pushed onto `config.routes` by `apply()`. The ConfigPlan-level
    /// `validate_config` call (PR #1025) catches structural problems —
    /// nonexistent endpoint aliases, self-referencing routes,
    /// A→B + B→A cycles, cross-protocol routes without a compatible
    /// transform — before the change is committed; the per-variant
    /// apply path only rejects empty endpoints (the cheapest possible
    /// "didn't fill in the form" check) so the user gets a clear
    /// error rather than a vacuous validator complaint.
    CreateRoute {
        from: String,
        to: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transform: Option<conductor_core::config::types::SignalTransform>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filter: Option<conductor_core::config::types::SignalFilter>,
        #[serde(default = "default_enabled_true")]
        enabled: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },

    /// Delete a route from [[routes]] by 0-based index (ADR-031 P3
    /// § 5.4 — issue #1143). Index is into the route list as it
    /// appears at plan-apply time — the ConfigPlan TOCTOU
    /// machinery already guards against the underlying list mutating
    /// between plan creation and apply (base_state_hash), so the
    /// index is stable across the approval round-trip.
    DeleteRoute { index: usize },

    /// Replace the route at 0-based `index` with a new RouteConfig
    /// composed from the same fields as `CreateRoute` (ADR-031 P3
    /// § 5.4 — issue #1143). Total-update semantics: the LLM
    /// supplies the complete new shape, the apply replaces the
    /// whole entry. Same TOCTOU guarantees as `DeleteRoute`.
    /// `from`/`to` non-empty enforced at apply-time; structural
    /// validity (endpoint existence, cycles, transform compat)
    /// is caught by the ConfigPlan-level `validate_config` call.
    UpdateRoute {
        index: usize,
        from: String,
        to: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transform: Option<conductor_core::config::types::SignalTransform>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filter: Option<conductor_core::config::types::SignalFilter>,
        #[serde(default = "default_enabled_true")]
        enabled: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
}

impl ConfigChange {
    /// Get a human-readable description of this change
    pub fn description(&self) -> String {
        match self {
            ConfigChange::CreateMapping {
                mode, description, ..
            } => {
                let desc = description.as_deref().unwrap_or("new mapping");
                format!("Create mapping '{}' in mode '{}'", desc, mode)
            }
            ConfigChange::UpdateMapping {
                mode,
                index,
                description,
                ..
            } => {
                let desc = description.as_deref().unwrap_or("mapping");
                format!("Update {} at index {} in mode '{}'", desc, index, mode)
            }
            ConfigChange::DeleteMapping { mode, index } => {
                format!("Delete mapping at index {} in mode '{}'", index, mode)
            }
            ConfigChange::CreateMode { name, .. } => {
                format!("Create new mode '{}'", name)
            }
            ConfigChange::DeleteMode { name } => {
                format!("Delete mode '{}'", name)
            }
            ConfigChange::InsertMapping {
                mode,
                index,
                description,
                ..
            } => {
                let desc = description.as_deref().unwrap_or("mapping");
                format!("Restore {} at index {} in mode '{}'", desc, index, mode)
            }
            ConfigChange::RestoreMode { index, mode } => {
                format!(
                    "Restore mode '{}' ({} mapping(s)) at index {}",
                    mode.name,
                    mode.mappings.len(),
                    index
                )
            }
            ConfigChange::CreateEndpoint {
                alias,
                direction,
                kind,
                description,
                ..
            } => {
                let dir = match direction {
                    conductor_core::config::types::ConnectorDirection::Input => "input",
                    conductor_core::config::types::ConnectorDirection::Output => "output",
                    conductor_core::config::types::ConnectorDirection::Bidirectional => {
                        "bidirectional"
                    }
                };
                let ty = match kind {
                    conductor_core::config::types::EndpointKind::Matcher { .. } => "matcher",
                    conductor_core::config::types::EndpointKind::OscEndpoint { .. } => "osc",
                    conductor_core::config::types::EndpointKind::ArtNetEndpoint { .. } => "artnet",
                    conductor_core::config::types::EndpointKind::MidiVirtualPort { .. } => {
                        "midi-virtual-port"
                    }
                };
                let desc = description.as_deref().unwrap_or("endpoint");
                format!("Create {} {} {} '{}'", dir, ty, desc, alias)
            }
            ConfigChange::CreateRoute {
                from,
                to,
                description,
                ..
            } => {
                let desc = description.as_deref().unwrap_or("route");
                format!("Create {} '{}' → '{}'", desc, from, to)
            }
            ConfigChange::DeleteRoute { index } => {
                format!("Delete route at index {}", index)
            }
            ConfigChange::UpdateRoute {
                index,
                from,
                to,
                description,
                ..
            } => {
                let desc = description.as_deref().unwrap_or("route");
                format!("Update {} at index {} ('{}' → '{}')", desc, index, from, to)
            }
        }
    }
}

/// A configuration change plan with TOCTOU protection
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigPlan {
    /// Unique plan identifier
    pub id: Uuid,

    /// Human-readable description of the overall plan
    pub description: String,

    /// List of changes in this plan
    pub changes: Vec<ConfigChange>,

    /// SHA256 hash of configuration when plan was created
    pub base_state_hash: String,

    /// When this plan expires
    pub expires_at: DateTime<Utc>,

    /// When this plan was created
    pub created_at: DateTime<Utc>,

    /// Pre-computed TOML diff preview (populated after creation)
    #[serde(default)]
    pub diff_preview: String,

    /// Pre-computed human-readable descriptions for each change
    #[serde(default)]
    pub change_descriptions: Vec<String>,

    /// Validation warnings for the proposed config (v4.26.77)
    #[serde(default)]
    pub validation_warnings: Vec<String>,

    /// Validation errors for the proposed config (v4.26.77)
    #[serde(default)]
    pub validation_errors: Vec<String>,

    /// Set when this plan was produced via a DEPRECATED tool that delegated to
    /// the canonical codepath (ADR-035 Slice 8). Surfaces the replacement tool +
    /// removal horizon to the caller. `None` for plans from non-deprecated tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deprecation: Option<Deprecation>,
}

/// Structured deprecation notice attached to a [`ConfigPlan`] produced by a
/// deprecated tool (ADR-035 §4.5 R2). Mirrors the documented response shape:
/// `{ since, replacement, removal }`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Deprecation {
    /// Version/ADR that deprecated the tool.
    pub since: String,
    /// The tool callers should use instead.
    pub replacement: String,
    /// When the deprecated tool will be removed.
    pub removal: String,
}

impl ConfigPlan {
    /// Create a new plan from changes
    ///
    /// # Arguments
    /// * `description` - Human-readable description
    /// * `changes` - List of configuration changes
    /// * `current_config` - Current configuration to hash for TOCTOU protection
    pub fn new(
        description: impl Into<String>,
        changes: Vec<ConfigChange>,
        current_config: &Config,
    ) -> Self {
        let now = Utc::now();
        let change_descriptions = changes.iter().map(|c| c.description()).collect();

        // Validate proposed config by applying changes to a clone (v4.26.77)
        let mut proposed = current_config.clone();
        let mut validation_warnings = Vec::new();
        let mut validation_errors = Vec::new();

        // Try applying changes to get the proposed config state
        let mut proposed_changes = changes.clone();
        if Self::try_apply_changes(&mut proposed, &mut proposed_changes) {
            let report = validate_config(&proposed);
            for finding in &report.warnings {
                validation_warnings.push(format!("{}: {}", finding.path, finding.message));
            }
            for finding in &report.errors {
                validation_errors.push(format!("{}: {}", finding.path, finding.message));
            }
        }

        let mut plan = Self {
            id: Uuid::new_v4(),
            description: description.into(),
            changes,
            base_state_hash: hash_config(current_config),
            expires_at: now + Duration::minutes(PLAN_TTL_MINUTES),
            created_at: now,
            diff_preview: String::new(),
            change_descriptions,
            validation_warnings,
            validation_errors,
            deprecation: None,
        };
        plan.diff_preview = plan.preview_diff(current_config);
        plan
    }

    /// Attach a deprecation notice (ADR-035 §4.5) — used by deprecated tools
    /// that delegate to the canonical codepath. Builder-style so the create
    /// path stays untouched and the deprecated arms add the notice in one line.
    pub fn with_deprecation(mut self, deprecation: Deprecation) -> Self {
        self.deprecation = Some(deprecation);
        self
    }

    /// Check if this plan has expired
    pub fn is_expired(&self) -> bool {
        Utc::now() > self.expires_at
    }

    /// Validate that the current config matches the base state
    ///
    /// # Arguments
    /// * `current_config` - Current configuration to validate against
    ///
    /// # Errors
    /// Returns `PlanError::StateChanged` if config has been modified
    pub fn validate_state(&self, current_config: &Config) -> Result<(), PlanError> {
        if self.is_expired() {
            return Err(PlanError::Expired(self.expires_at));
        }

        let current_hash = hash_config(current_config);
        if current_hash != self.base_state_hash {
            return Err(PlanError::StateChanged {
                expected: self.base_state_hash.clone(),
                actual: current_hash,
            });
        }

        Ok(())
    }

    /// Apply this plan to a configuration.
    ///
    /// Thin wrapper over [`ConfigPlan::apply_atomic`] kept for call sites that
    /// don't need the applied-change count. It is **not** a separate, lighter
    /// code path: #2115 (clawpatch #2103) flagged the previous implementation,
    /// which ran only the TOCTOU `validate_state` check and then mutated the
    /// caller's config in place, as a bypass of the ADR-027 D2 post-mutation
    /// `validate_config` gate and the D8 keystroke deny-list. Delegating keeps a
    /// single validated, atomic apply path so an invalid plan can never be
    /// silently committed (and the caller's config is left untouched on error).
    ///
    /// # Arguments
    /// * `config` - Configuration to modify
    ///
    /// # Errors
    /// Returns error if:
    /// - Plan is expired
    /// - Config has changed since plan creation (TOCTOU)
    /// - Any change is invalid (mode not found, index out of range, etc.)
    /// - The post-mutation config fails structural validation or the keystroke
    ///   deny-list
    pub fn apply(self, config: &mut Config) -> Result<(), PlanError> {
        self.apply_atomic(config).map(|_| ())
    }

    /// Apply this plan atomically - all changes succeed or none are applied (P3-07)
    ///
    /// Creates a clone of the configuration, applies all changes to the clone,
    /// and only replaces the original if all changes succeed.
    ///
    /// # Arguments
    /// * `config` - Configuration to modify
    ///
    /// # Errors
    /// Returns error if:
    /// - Plan is expired
    /// - Config has changed since plan creation (TOCTOU)
    /// - Any change is invalid (mode not found, index out of range, etc.)
    ///
    /// If any error occurs, the original config is unchanged.
    pub fn apply_atomic(self, config: &mut Config) -> Result<usize, PlanError> {
        // Validate state before applying
        self.validate_state(config)?;

        // Clone config for atomic operation
        let mut config_clone = config.clone();
        let changes_count = self.changes.len();

        // Pre-process changes to avoid index drift (#259)
        let changes = reorder_for_index_safety(self.changes);

        // Apply all changes to the clone
        for change in changes {
            apply_change(&mut config_clone, change)?;
        }

        // ADR-027 D2 — re-validate the post-mutation config before
        // committing. Audit Attack Chain C: previously this method
        // mutated the caller's config without revalidating, so a
        // plan that produced an invalid configuration (empty
        // Keystroke keys, unknown LED scheme, shell command failing
        // the metacharacter check, etc.) was silently applied. With
        // this gate, the caller's config is never mutated unless
        // the post-state passes the same structural validator that
        // runs on file load.
        //
        // PR #1025 review: call `validate_config` directly rather
        // than `validate_for_loading`. The latter prints all
        // validation warnings via `eprintln!` (fine for the
        // file-load path, noisy on every plan apply) and wraps
        // errors in `ConfigError::ValidationError(...)`, which
        // would double-prefix the plan-error message. Going through
        // the report gives us a clean error string and lets us
        // route warnings through `tracing` instead of stderr.
        let report = conductor_core::config::validation::validate_config(&config_clone);
        for w in &report.warnings {
            // PR #1025 round-2 review: `tracing` auto-binds the
            // event's `message` field to the trailing string
            // literal. A structured field also named `message`
            // would clobber that in JSON sinks and be confusing in
            // text output. Use `finding_message` so the per-warning
            // text is preserved alongside the human label without
            // shadowing.
            tracing::warn!(
                target: "llm.plan",
                path = %w.path,
                finding_message = %w.message,
                "ConfigPlan post-mutation validation warning",
            );
        }
        if !report.is_valid() {
            return Err(PlanError::ValidationFailed {
                errors: report.format_errors(),
            });
        }

        // ADR-027 D8 save-time deny-list (issue #1040). The structural
        // validator above doesn't know about the keystroke deny-list —
        // that list lives in `daemon::keystroke_policy` because it's a
        // runtime enforcement concern. Catching deny-listed combos at
        // plan-apply time turns silent dead mappings into clear,
        // user-visible rejections. Mirrors how Shell metacharacters
        // are rejected at config-load by `validate_shell_command`.
        if let Err(combo_errors) =
            scan_config_for_denylisted_keystrokes(&config_clone, KeystrokePolicy::Standard)
        {
            return Err(PlanError::ValidationFailed {
                errors: combo_errors,
            });
        }

        // All changes succeeded - replace original config
        *config = config_clone;

        Ok(changes_count)
    }

    /// Try to apply changes to a config for validation preview.
    /// Returns true if all changes applied successfully, false if any failed.
    fn try_apply_changes(config: &mut Config, changes: &mut Vec<ConfigChange>) -> bool {
        let ordered = reorder_for_index_safety(std::mem::take(changes));
        for change in ordered {
            if apply_change(config, change).is_err() {
                return false;
            }
        }
        true
    }

    /// Number of changes in this plan
    pub fn changes_count(&self) -> usize {
        self.changes.len()
    }

    /// Get a preview of changes as TOML diff
    pub fn preview_diff(&self, current_config: &Config) -> String {
        let mut diff = String::new();

        for change in &self.changes {
            diff.push_str(&format!("# {}\n", change.description()));
            match change {
                ConfigChange::CreateMapping {
                    mode,
                    trigger,
                    action,
                    description,
                    let_through,
                } => {
                    diff.push_str(&format!("+ [[modes.{}.mappings]]\n", mode));
                    diff.push_str(&format!("+   trigger = {}\n", format_value(trigger)));
                    diff.push_str(&format!("+   action = {}\n", format_value(action)));
                    if let Some(desc) = description {
                        diff.push_str(&format!("+   description = \"{}\"\n", desc));
                    }
                    if *let_through {
                        diff.push_str("+   let_through = true\n");
                    }
                }
                ConfigChange::UpdateMapping {
                    mode,
                    index,
                    trigger,
                    action,
                    description,
                } => {
                    // Show old mapping if available
                    if let Some(m) = current_config.modes.iter().find(|m| m.name == *mode)
                        && let Some(old) = m.mappings.get(*index)
                    {
                        diff.push_str(&format!("- # modes.{}.mappings[{}]\n", mode, index));
                        diff.push_str(&format!("-   trigger = {}\n", format_value(&old.trigger)));
                        diff.push_str(&format!("-   action = {}\n", format_value(&old.action)));
                    }
                    diff.push_str(&format!("+ # modes.{}.mappings[{}]\n", mode, index));
                    diff.push_str(&format!("+   trigger = {}\n", format_value(trigger)));
                    diff.push_str(&format!("+   action = {}\n", format_value(action)));
                    if let Some(desc) = description {
                        diff.push_str(&format!("+   description = \"{}\"\n", desc));
                    }
                }
                ConfigChange::DeleteMapping { mode, index } => {
                    if let Some(m) = current_config.modes.iter().find(|m| m.name == *mode)
                        && let Some(old) = m.mappings.get(*index)
                    {
                        diff.push_str(&format!("- # modes.{}.mappings[{}]\n", mode, index));
                        diff.push_str(&format!("-   trigger = {}\n", format_value(&old.trigger)));
                        diff.push_str(&format!("-   action = {}\n", format_value(&old.action)));
                    }
                }
                ConfigChange::CreateMode { name, color } => {
                    diff.push_str("+ [[modes]]\n");
                    diff.push_str(&format!("+   name = \"{}\"\n", name));
                    if let Some(c) = color {
                        diff.push_str(&format!("+   color = \"{}\"\n", c));
                    }
                }
                ConfigChange::DeleteMode { name } => {
                    diff.push_str("- [[modes]]\n");
                    diff.push_str(&format!("-   name = \"{}\"\n", name));
                }
                ConfigChange::InsertMapping {
                    mode,
                    index,
                    trigger,
                    action,
                    description,
                    let_through,
                } => {
                    diff.push_str(&format!(
                        "+ [[modes.{}.mappings]] @ index {}\n",
                        mode, index
                    ));
                    diff.push_str(&format!("+   trigger = {}\n", format_value(trigger)));
                    diff.push_str(&format!("+   action = {}\n", format_value(action)));
                    if let Some(desc) = description {
                        diff.push_str(&format!("+   description = \"{}\"\n", desc));
                    }
                    if *let_through {
                        diff.push_str("+   let_through = true\n");
                    }
                }
                ConfigChange::RestoreMode { index, mode } => {
                    diff.push_str(&format!("+ [[modes]] @ index {}\n", index));
                    diff.push_str(&format!("+   name = \"{}\"\n", mode.name));
                    if let Some(c) = &mode.color {
                        diff.push_str(&format!("+   color = \"{}\"\n", c));
                    }
                    diff.push_str(&format!(
                        "+   ({} mapping(s) restored)\n",
                        mode.mappings.len()
                    ));
                }
                ConfigChange::CreateEndpoint {
                    alias,
                    direction,
                    protocol,
                    kind,
                    description,
                    enabled,
                    channels,
                } => {
                    diff.push_str("+ [[endpoints]]\n");
                    diff.push_str(&format!("+   alias = \"{}\"\n", alias));
                    diff.push_str(&format!("+   direction = \"{:?}\"\n", direction));
                    if let Some(p) = protocol {
                        diff.push_str(&format!("+   protocol = \"{:?}\"\n", p));
                    }
                    diff.push_str(&format!("+   kind = {}\n", format_value(kind)));
                    if let Some(desc) = description {
                        diff.push_str(&format!("+   description = \"{}\"\n", desc));
                    }
                    if !enabled {
                        diff.push_str("+   enabled = false\n");
                    }
                    if !channels.is_empty() {
                        diff.push_str(&format!("+   channels = {}\n", format_value(channels)));
                    }
                }
                ConfigChange::CreateRoute {
                    from,
                    to,
                    transform,
                    filter,
                    enabled,
                    description,
                } => {
                    diff.push_str("+ [[routes]]\n");
                    diff.push_str(&format!("+   from = \"{}\"\n", from));
                    diff.push_str(&format!("+   to = \"{}\"\n", to));
                    if let Some(t) = transform {
                        diff.push_str(&format!("+   transform = {}\n", format_value(t)));
                    }
                    if let Some(f) = filter {
                        diff.push_str(&format!("+   filter = {}\n", format_value(f)));
                    }
                    if !enabled {
                        diff.push_str("+   enabled = false\n");
                    }
                    if let Some(desc) = description {
                        diff.push_str(&format!("+   description = \"{}\"\n", desc));
                    }
                }
                ConfigChange::DeleteRoute { index } => {
                    if let Some(old) = current_config.routes.get(*index) {
                        diff.push_str(&format!("- # routes[{}]\n", index));
                        diff.push_str(&format!("-   from = \"{}\"\n", old.from));
                        diff.push_str(&format!("-   to = \"{}\"\n", old.to));
                    }
                }
                ConfigChange::UpdateRoute {
                    index,
                    from,
                    to,
                    transform,
                    filter,
                    enabled,
                    description,
                } => {
                    // Show ±-style: old line first (if findable) then
                    // the new fields. Same shape as the diff for
                    // CreateRoute since UpdateRoute totals-replaces.
                    if let Some(old) = current_config.routes.get(*index) {
                        diff.push_str(&format!("- # routes[{}]\n", index));
                        diff.push_str(&format!("-   from = \"{}\"\n", old.from));
                        diff.push_str(&format!("-   to = \"{}\"\n", old.to));
                    }
                    diff.push_str(&format!("+ # routes[{}]\n", index));
                    diff.push_str(&format!("+   from = \"{}\"\n", from));
                    diff.push_str(&format!("+   to = \"{}\"\n", to));
                    if let Some(t) = transform {
                        diff.push_str(&format!("+   transform = {}\n", format_value(t)));
                    }
                    if let Some(f) = filter {
                        diff.push_str(&format!("+   filter = {}\n", format_value(f)));
                    }
                    if !enabled {
                        diff.push_str("+   enabled = false\n");
                    }
                    if let Some(desc) = description {
                        diff.push_str(&format!("+   description = \"{}\"\n", desc));
                    }
                }
            }
            diff.push('\n');
        }

        diff
    }
}

/// Hash a configuration for TOCTOU protection.
///
/// Panics if Config serialization fails — this should never happen since
/// all Config types implement Serialize with standard serde types. A panic
/// is preferable to silently producing an empty-string hash that would
/// make all TOCTOU comparisons pass (#260).
fn hash_config(config: &Config) -> String {
    let serialized = serde_json::to_string(config)
        .expect("Config serialization failed — all Config types implement Serialize");
    let mut hasher = Sha256::new();
    hasher.update(serialized.as_bytes());
    hex::encode(hasher.finalize())
}

/// Adjust index-based operations to account for prior deletes in the same mode (#259).
///
/// All plan indices reference the original config state, but sequential application
/// shifts indices after each delete. This function adjusts DeleteMapping and
/// UpdateMapping indices by subtracting the count of prior DeleteMapping operations
/// in the same mode that target a lower original index. The operation order is
/// preserved to avoid breaking plans that mix DeleteMapping with DeleteMode.
fn reorder_for_index_safety(changes: Vec<ConfigChange>) -> Vec<ConfigChange> {
    // Collect (position, mode, original_index) for all DeleteMapping operations
    let delete_info: Vec<(usize, String, usize)> = changes
        .iter()
        .enumerate()
        .filter_map(|(pos, c)| match c {
            ConfigChange::DeleteMapping { mode, index } => Some((pos, mode.clone(), *index)),
            _ => None,
        })
        .collect();

    changes
        .into_iter()
        .enumerate()
        .map(|(pos, change)| {
            match change {
                ConfigChange::DeleteMapping { ref mode, index } => {
                    let prior_lower_deletes = delete_info
                        .iter()
                        .filter(|(p, m, idx)| *p < pos && m == mode && *idx < index)
                        .count();
                    ConfigChange::DeleteMapping {
                        mode: mode.clone(),
                        index: index.saturating_sub(prior_lower_deletes),
                    }
                }
                ConfigChange::UpdateMapping {
                    ref mode,
                    index,
                    ref trigger,
                    ref action,
                    ref description,
                } => {
                    // UpdateMapping indices also shift when prior deletes remove lower items
                    let prior_lower_deletes = delete_info
                        .iter()
                        .filter(|(p, m, idx)| *p < pos && m == mode && *idx < index)
                        .count();
                    ConfigChange::UpdateMapping {
                        mode: mode.clone(),
                        index: index.saturating_sub(prior_lower_deletes),
                        trigger: trigger.clone(),
                        action: action.clone(),
                        description: description.clone(),
                    }
                }
                other => other,
            }
        })
        .collect()
}

/// Walk every `Action::Keystroke` reachable from `config` and check it
/// against the keystroke deny-list (ADR-027 D8, issue #1040). Returns
/// `Ok(())` when all combos are safe, or `Err(msg)` describing every
/// denied combo found.
///
/// The walk is recursive — context-switch tables, `Sequence`, `Repeat`,
/// and `Conditional` actions all nest other `ActionConfig`s, and a
/// deny-listed Keystroke hidden inside any of them is just as
/// dangerous as a top-level one.
///
/// Parses the schema-level `keys: String` + `modifiers: Vec<String>`
/// via `conductor_core::actions::parse_keys` / `parse_modifier` — the
/// same parsers the runtime uses — so the schema-time and runtime
/// checks see exactly the same tokenised inputs.
fn scan_config_for_denylisted_keystrokes(
    config: &Config,
    policy: KeystrokePolicy,
) -> Result<(), String> {
    let mut errors: Vec<String> = Vec::new();
    for (mode_idx, mode) in config.modes.iter().enumerate() {
        let mode_path = format!("modes[{}] '{}'", mode_idx, mode.name);
        for (m_idx, mapping) in mode.mappings.iter().enumerate() {
            let path = format!("{}.mappings[{}]", mode_path, m_idx);
            walk_action_for_denylist(&mapping.action, &path, policy, &mut errors);
        }
    }
    for (m_idx, mapping) in config.global_mappings.iter().enumerate() {
        let path = format!("global_mappings[{}]", m_idx);
        walk_action_for_denylist(&mapping.action, &path, policy, &mut errors);
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}

/// Recursive helper for [`scan_config_for_denylisted_keystrokes`].
/// Visits every leaf `Keystroke` in the action tree.
fn walk_action_for_denylist(
    action: &ActionConfig,
    path: &str,
    policy: KeystrokePolicy,
    errors: &mut Vec<String>,
) {
    match action {
        ActionConfig::Keystroke { keys, modifiers } => {
            let parsed_keys = parse_keys(keys);
            let parsed_mods: Vec<_> = modifiers.iter().filter_map(|m| parse_modifier(m)).collect();
            if let Err(err) = check_combo_only(policy, &parsed_keys, &parsed_mods) {
                errors.push(format!("{}: {}", path, err));
            }
        }
        ActionConfig::Sequence { actions } => {
            for (i, sub) in actions.iter().enumerate() {
                walk_action_for_denylist(sub, &format!("{}.actions[{}]", path, i), policy, errors);
            }
        }
        ActionConfig::Repeat { action, .. } => {
            walk_action_for_denylist(action, &format!("{}.action", path), policy, errors);
        }
        ActionConfig::Conditional {
            then_action,
            else_action,
            ..
        } => {
            walk_action_for_denylist(then_action, &format!("{}.then", path), policy, errors);
            if let Some(e) = else_action {
                walk_action_for_denylist(e, &format!("{}.else", path), policy, errors);
            }
        }
        ActionConfig::PcContextSwitch {
            mappings, default, ..
        } => {
            for (pc, sub) in mappings {
                walk_action_for_denylist(
                    sub,
                    &format!("{}.mappings[pc={}]", path, pc),
                    policy,
                    errors,
                );
            }
            if let Some(d) = default {
                walk_action_for_denylist(d, &format!("{}.default", path), policy, errors);
            }
        }
        ActionConfig::CcContextSwitch {
            ranges, default, ..
        } => {
            for (i, range) in ranges.iter().enumerate() {
                walk_action_for_denylist(
                    &range.action,
                    &format!("{}.ranges[{}].action", path, i),
                    policy,
                    errors,
                );
            }
            if let Some(d) = default {
                walk_action_for_denylist(d, &format!("{}.default", path), policy, errors);
            }
        }
        // Non-Keystroke / non-recursive variants — no deny-list applies.
        _ => {}
    }
}

/// Apply a single change to a configuration
///
/// This function is public to allow undo/redo operations (P4-06) to apply
/// individual changes when restoring state.
pub fn apply_change(config: &mut Config, change: ConfigChange) -> Result<(), PlanError> {
    match change {
        ConfigChange::CreateMapping {
            mode,
            trigger,
            action,
            description,
            let_through,
        } => {
            let mode_obj = config
                .modes
                .iter_mut()
                .find(|m| m.name == mode)
                .ok_or_else(|| PlanError::ModeNotFound(mode.clone()))?;

            mode_obj.mappings.push(Mapping {
                trigger,
                action,
                description,
                // ADR-038 Slice 6: threaded from the MCP create_mapping arg.
                let_through,
            });
            Ok(())
        }

        ConfigChange::UpdateMapping {
            mode,
            index,
            trigger,
            action,
            description,
        } => {
            let mode_obj = config
                .modes
                .iter_mut()
                .find(|m| m.name == mode)
                .ok_or_else(|| PlanError::ModeNotFound(mode.clone()))?;

            let count = mode_obj.mappings.len();
            let mapping =
                mode_obj
                    .mappings
                    .get_mut(index)
                    .ok_or_else(|| PlanError::IndexOutOfRange {
                        mode: mode.clone(),
                        index,
                        count,
                    })?;

            mapping.trigger = trigger;
            mapping.action = action;
            mapping.description = description;
            Ok(())
        }

        ConfigChange::DeleteMapping { mode, index } => {
            let mode_obj = config
                .modes
                .iter_mut()
                .find(|m| m.name == mode)
                .ok_or_else(|| PlanError::ModeNotFound(mode.clone()))?;

            let count = mode_obj.mappings.len();
            if index >= count {
                return Err(PlanError::IndexOutOfRange {
                    mode: mode.clone(),
                    index,
                    count,
                });
            }

            mode_obj.mappings.remove(index);
            Ok(())
        }

        ConfigChange::CreateMode { name, color } => {
            // Check if mode already exists
            if config.modes.iter().any(|m| m.name == name) {
                return Err(PlanError::InvalidAction(format!(
                    "Mode '{}' already exists",
                    name
                )));
            }

            config.modes.push(conductor_core::config::Mode {
                name,
                color,
                mappings: vec![],
            });
            Ok(())
        }

        ConfigChange::DeleteMode { name } => {
            let idx = config
                .modes
                .iter()
                .position(|m| m.name == name)
                .ok_or_else(|| PlanError::ModeNotFound(name.clone()))?;

            config.modes.remove(idx);
            Ok(())
        }

        ConfigChange::InsertMapping {
            mode,
            index,
            trigger,
            action,
            description,
            let_through,
        } => {
            let mode_obj = config
                .modes
                .iter_mut()
                .find(|m| m.name == mode)
                .ok_or_else(|| PlanError::ModeNotFound(mode.clone()))?;

            // Clamp to len so a restore against a (legitimately) shorter list
            // appends rather than panicking; for a true undo the index is in
            // range and position is preserved exactly (#2121).
            let at = index.min(mode_obj.mappings.len());
            mode_obj.mappings.insert(
                at,
                Mapping {
                    trigger,
                    action,
                    description,
                    let_through,
                },
            );
            Ok(())
        }

        ConfigChange::RestoreMode { index, mode } => {
            // Restoring a deleted mode: the name must not already be present
            // (same uniqueness rule as CreateMode).
            if config.modes.iter().any(|m| m.name == mode.name) {
                return Err(PlanError::InvalidAction(format!(
                    "Mode '{}' already exists",
                    mode.name
                )));
            }
            let at = index.min(config.modes.len());
            config.modes.insert(at, mode);
            Ok(())
        }

        ConfigChange::CreateEndpoint {
            alias,
            direction,
            protocol,
            kind,
            description,
            enabled,
            channels,
        } => {
            // ADR-035 — alias must be unique across the endpoint namespace
            // (the same rule `normalize_to_endpoints` hard-fails on at load).
            if config.endpoints.iter().any(|e| e.alias == alias) {
                return Err(PlanError::InvalidAction(format!(
                    "Endpoint alias '{}' already exists",
                    alias
                )));
            }
            config
                .endpoints
                .push(conductor_core::config::types::EndpointConfig {
                    alias,
                    direction,
                    protocol,
                    description,
                    enabled,
                    channels,
                    kind,
                });
            Ok(())
        }

        ConfigChange::CreateRoute {
            from,
            to,
            transform,
            filter,
            enabled,
            description,
        } => {
            // Cheap "did you fill in the form" guards. The endpoint-
            // existence / cycle / cross-protocol-transform checks live
            // in `validate_config`, which the ConfigPlan invokes
            // before committing the change (PR #1025), so we don't
            // duplicate them here.
            if from.trim().is_empty() {
                return Err(PlanError::InvalidAction(
                    "Route 'from' alias cannot be empty".to_string(),
                ));
            }
            if to.trim().is_empty() {
                return Err(PlanError::InvalidAction(
                    "Route 'to' alias cannot be empty".to_string(),
                ));
            }

            config
                .routes
                .push(conductor_core::config::types::RouteConfig {
                    from,
                    to,
                    transform,
                    filter,
                    enabled,
                    description,
                    modes: Vec::new(),
                });
            Ok(())
        }

        ConfigChange::DeleteRoute { index } => {
            let count = config.routes.len();
            if index >= count {
                return Err(PlanError::InvalidAction(format!(
                    "Route index {} out of range (have {} routes)",
                    index, count
                )));
            }
            config.routes.remove(index);
            Ok(())
        }

        ConfigChange::UpdateRoute {
            index,
            from,
            to,
            transform,
            filter,
            enabled,
            description,
        } => {
            if from.trim().is_empty() {
                return Err(PlanError::InvalidAction(
                    "Route 'from' alias cannot be empty".to_string(),
                ));
            }
            if to.trim().is_empty() {
                return Err(PlanError::InvalidAction(
                    "Route 'to' alias cannot be empty".to_string(),
                ));
            }
            let count = config.routes.len();
            if index >= count {
                return Err(PlanError::InvalidAction(format!(
                    "Route index {} out of range (have {} routes)",
                    index, count
                )));
            }
            // UpdateRoute's payload was designed before ADR-036 added
            // `modes` to RouteConfig. Since the change-type has no field
            // for it, preserve whatever the existing route already has —
            // otherwise an update silently strips mode-scoping (PR #1673
            // Copilot finding). (Phase 3 removed `phase`; all routes are
            // post-mapping.)
            let existing_modes = config.routes[index].modes.clone();
            config.routes[index] = conductor_core::config::types::RouteConfig {
                from,
                to,
                transform,
                filter,
                enabled,
                description,
                modes: existing_modes,
            };
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conductor_core::config::Mode;

    fn create_test_config() -> Config {
        Config {
            mcp: Default::default(),
            per_app_modes: None,
            config_meta: Default::default(),
            security: Default::default(),
            endpoints: vec![],
            modes: vec![
                Mode {
                    name: "Default".to_string(),
                    color: Some("blue".to_string()),
                    mappings: vec![Mapping {
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
                    }],
                },
                Mode {
                    name: "DJ".to_string(),
                    color: Some("purple".to_string()),
                    mappings: vec![],
                },
            ],
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
    fn test_config_plan_validates_base_state_hash() {
        let config = create_test_config();
        let plan = ConfigPlan::new(
            "Test plan",
            vec![ConfigChange::CreateMapping {
                mode: "Default".to_string(),
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
            }],
            &config,
        );

        // Same config should validate
        assert!(plan.validate_state(&config).is_ok());
    }

    #[test]
    fn test_config_plan_toctou_protection() {
        let config = create_test_config();
        let plan = ConfigPlan::new(
            "Test plan",
            vec![ConfigChange::CreateMapping {
                mode: "Default".to_string(),
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
                description: None,
                let_through: false,
            }],
            &config,
        );

        // Modify the config — ADR-035 removed `[device]`; mutate a
        // serializing field (the mode name) so the state hash changes.
        let mut modified_config = config.clone();
        modified_config.modes[0].name = "Modified Mode".to_string();

        // Validation should fail
        let result = plan.validate_state(&modified_config);
        assert!(matches!(result, Err(PlanError::StateChanged { .. })));
    }

    #[test]
    fn test_config_plan_expires_after_5_minutes() {
        let config = create_test_config();
        let mut plan = ConfigPlan::new("Test plan", vec![], &config);

        // Plan should not be expired initially
        assert!(!plan.is_expired());

        // Manually set expiration to the past
        plan.expires_at = Utc::now() - Duration::minutes(1);

        // Plan should now be expired
        assert!(plan.is_expired());

        // Validation should fail
        let result = plan.validate_state(&config);
        assert!(matches!(result, Err(PlanError::Expired(_))));
    }

    #[test]
    fn test_config_plan_serialization() {
        let config = create_test_config();
        let plan = ConfigPlan::new(
            "Test plan",
            vec![ConfigChange::CreateMapping {
                mode: "Default".to_string(),
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
            }],
            &config,
        );

        // Should serialize and deserialize correctly
        let json = serde_json::to_string(&plan).expect("Failed to serialize");
        let deserialized: ConfigPlan = serde_json::from_str(&json).expect("Failed to deserialize");

        assert_eq!(plan.id, deserialized.id);
        assert_eq!(plan.description, deserialized.description);
        assert_eq!(plan.base_state_hash, deserialized.base_state_hash);
        assert_eq!(plan.changes.len(), deserialized.changes.len());
        assert_eq!(plan.diff_preview, deserialized.diff_preview);
        assert_eq!(plan.change_descriptions, deserialized.change_descriptions);
    }

    #[test]
    fn test_config_plan_serializes_diff_preview() {
        let config = create_test_config();
        let plan = ConfigPlan::new(
            "Add paste mapping",
            vec![ConfigChange::CreateMapping {
                mode: "Default".to_string(),
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
            }],
            &config,
        );

        // diff_preview should be populated by new()
        assert!(!plan.diff_preview.is_empty());
        assert!(plan.diff_preview.contains("+"));
        assert!(plan.diff_preview.contains("Default"));

        // Should round-trip through JSON
        let json = serde_json::to_string(&plan).expect("serialize");
        assert!(json.contains("diff_preview"));
        let deser: ConfigPlan = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(plan.diff_preview, deser.diff_preview);
    }

    #[test]
    fn test_config_plan_populates_change_descriptions() {
        let config = create_test_config();
        let plan = ConfigPlan::new(
            "Multi-change",
            vec![
                ConfigChange::CreateMapping {
                    mode: "Default".to_string(),
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
                ConfigChange::DeleteMapping {
                    mode: "Default".to_string(),
                    index: 0,
                },
                ConfigChange::CreateMode {
                    name: "DJ".to_string(),
                    color: Some("purple".to_string()),
                },
                ConfigChange::DeleteMode {
                    name: "Old".to_string(),
                },
            ],
            &config,
        );
        assert_eq!(plan.changes.len(), 4);
        for c in &plan.changes {
            assert!(
                !c.description().is_empty(),
                "every change must have a non-empty description"
            );
        }
    }

    // ─── ADR-027 D2 — apply_atomic enforces validation ─────────────
    //
    // Audit Attack Chain C: when mappings are created via MCP tools,
    // `apply_plan()` (and `apply_atomic`) modified the in-memory
    // configuration without calling `validate_for_loading()`. The
    // plan's `validation_errors` was informational only — the daemon
    // did not enforce it. With D2, the resulting config is validated
    // INSIDE apply_atomic before the swap; failures abort the apply
    // with `PlanError::ValidationFailed` and the original config is
    // left untouched.

    #[test]
    fn test_apply_atomic_rejects_plan_that_produces_invalid_config() {
        // A plan that adds a Keystroke mapping with empty `keys`.
        // The structural validator rejects this with
        // "Keystroke requires keys" — exactly the kind of
        // mistake D2 prevents from being applied via MCP.
        let mut config = create_test_config();
        let plan = ConfigPlan::new(
            "Add invalid keystroke mapping",
            vec![ConfigChange::CreateMapping {
                mode: "Default".to_string(),
                trigger: Trigger::Note {
                    note: 40,
                    velocity_min: Some(1),
                    channel: None,
                    device: None,
                },
                action: ActionConfig::Keystroke {
                    keys: String::new(), // ← invalid; validator rejects
                    modifiers: vec![],
                },
                description: None,
                let_through: false,
            }],
            &config,
        );

        let original_mapping_count = config.modes[0].mappings.len();
        let result = plan.apply_atomic(&mut config);

        match result {
            Err(PlanError::ValidationFailed { ref errors }) => {
                assert!(
                    errors.contains("Keystroke requires keys")
                        || errors.to_lowercase().contains("keystroke"),
                    "PlanError::ValidationFailed should mention the failed check; got: {}",
                    errors,
                );
            }
            other => panic!(
                "expected PlanError::ValidationFailed for an invalid plan, got: {:?}",
                other,
            ),
        }

        // CRUCIAL atomicity invariant: a rejected apply must NOT
        // leave any partial mutation behind on the caller's config.
        assert_eq!(
            config.modes[0].mappings.len(),
            original_mapping_count,
            "apply_atomic must not mutate caller's config when validation fails",
        );
    }

    #[test]
    fn test_apply_atomic_still_accepts_valid_plans_after_d2() {
        // Regression guard: D2 must not break the happy path.
        // A plan with a fully-valid Keystroke must still apply.
        let mut config = create_test_config();
        let original_mapping_count = config.modes[0].mappings.len();
        let plan = ConfigPlan::new(
            "Add valid keystroke",
            vec![ConfigChange::CreateMapping {
                mode: "Default".to_string(),
                trigger: Trigger::Note {
                    note: 40,
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
            }],
            &config,
        );
        let applied = plan.apply_atomic(&mut config).expect("valid plan applies");
        assert_eq!(applied, 1);
        assert_eq!(config.modes[0].mappings.len(), original_mapping_count + 1);
    }

    // ────────────────────────────────────────────────────────────────
    // ADR-027 D8 save-time deny-list validation (issue #1040)
    //
    // Pre-fix, deny-listed Keystroke combos (Cmd+Q force-quit,
    // Cmd+Ctrl+Q screen-lock, etc.) saved silently and only failed at
    // action-execute time with a generic `OsAutomation` error string.
    // Plan apply now runs `check_combo_only` over every Action::Keystroke
    // in the post-mutation config so users see a clear rejection at
    // save time, symmetric with how Shell command-string validation
    // already worked.
    // ────────────────────────────────────────────────────────────────

    #[test]
    fn test_apply_atomic_rejects_denylisted_cmd_q() {
        // Cmd+Q force-quit foreground app — entry from DENYLIST in
        // keystroke_policy.rs. Should be rejected at apply_atomic time
        // with a clear PlanError::ValidationFailed citing the combo
        // label, not silently saved.
        let mut config = create_test_config();
        let original_mapping_count = config.modes[0].mappings.len();

        let plan = ConfigPlan::new(
            "Mistakenly map a pad to Cmd+Q",
            vec![ConfigChange::CreateMapping {
                mode: "Default".to_string(),
                trigger: Trigger::Note {
                    note: 41,
                    velocity_min: Some(1),
                    channel: None,
                    device: None,
                },
                action: ActionConfig::Keystroke {
                    keys: "q".to_string(),
                    modifiers: vec!["cmd".to_string()],
                },
                description: Some("Quit (denied)".to_string()),
                let_through: false,
            }],
            &config,
        );

        let result = plan.apply_atomic(&mut config);
        match result {
            Err(PlanError::ValidationFailed { ref errors }) => {
                assert!(
                    errors.to_lowercase().contains("deny")
                        || errors.contains("Cmd+Q")
                        || errors.contains("force-quit"),
                    "expected validation error to identify the denied combo; got: {}",
                    errors,
                );
            }
            other => panic!(
                "expected PlanError::ValidationFailed for deny-listed Cmd+Q, got: {:?}",
                other,
            ),
        }

        // Atomicity: rejected apply must leave config untouched.
        assert_eq!(
            config.modes[0].mappings.len(),
            original_mapping_count,
            "apply_atomic must not mutate caller's config when keystroke deny-list fires",
        );
    }

    #[test]
    fn test_apply_atomic_rejects_denylisted_cmd_ctrl_q_screen_lock() {
        // Cmd+Ctrl+Q macOS screen-lock — the exact combo cited in
        // the issue repro. Multiple modifiers, must still trip.
        let mut config = create_test_config();
        let plan = ConfigPlan::new(
            "Map a pad to screen-lock",
            vec![ConfigChange::CreateMapping {
                mode: "Default".to_string(),
                trigger: Trigger::Note {
                    note: 42,
                    velocity_min: Some(1),
                    channel: None,
                    device: None,
                },
                action: ActionConfig::Keystroke {
                    keys: "q".to_string(),
                    modifiers: vec!["cmd".to_string(), "ctrl".to_string()],
                },
                description: None,
                let_through: false,
            }],
            &config,
        );
        match plan.apply_atomic(&mut config) {
            Err(PlanError::ValidationFailed { .. }) => { /* expected */ }
            other => panic!(
                "expected PlanError::ValidationFailed for Cmd+Ctrl+Q screen-lock, got: {:?}",
                other,
            ),
        }
    }

    #[test]
    fn test_apply_atomic_rejects_denylisted_combo_inside_sequence() {
        // The deny-list must be checked recursively — a Sequence (or
        // Conditional, etc.) wrapping a denied Keystroke is just as
        // dangerous as a top-level Keystroke action.
        let mut config = create_test_config();
        let plan = ConfigPlan::new(
            "Sequence that hides a deny-listed keystroke",
            vec![ConfigChange::CreateMapping {
                mode: "Default".to_string(),
                trigger: Trigger::Note {
                    note: 43,
                    velocity_min: Some(1),
                    channel: None,
                    device: None,
                },
                action: ActionConfig::Sequence {
                    actions: vec![
                        ActionConfig::Delay { ms: 50 },
                        ActionConfig::Keystroke {
                            keys: "q".to_string(),
                            modifiers: vec!["cmd".to_string(), "ctrl".to_string()],
                        },
                    ],
                },
                description: None,
                let_through: false,
            }],
            &config,
        );
        match plan.apply_atomic(&mut config) {
            Err(PlanError::ValidationFailed { .. }) => { /* expected */ }
            other => panic!(
                "expected PlanError::ValidationFailed for sequence-wrapped Cmd+Ctrl+Q, got: {:?}",
                other,
            ),
        }
    }

    #[test]
    fn test_apply_atomic_accepts_safe_keystroke_combos() {
        // Regression guard: legitimate combos (Cmd+C copy, Cmd+V paste,
        // Cmd+S save, etc.) must still apply cleanly.
        let mut config = create_test_config();
        let original_mapping_count = config.modes[0].mappings.len();

        let plan = ConfigPlan::new(
            "Add safe keystroke combos",
            vec![
                ConfigChange::CreateMapping {
                    mode: "Default".to_string(),
                    trigger: Trigger::Note {
                        note: 50,
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
                ConfigChange::CreateMapping {
                    mode: "Default".to_string(),
                    trigger: Trigger::Note {
                        note: 51,
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
            &config,
        );

        let applied = plan
            .apply_atomic(&mut config)
            .expect("safe combos must still apply");
        assert_eq!(applied, 2);
        assert_eq!(config.modes[0].mappings.len(), original_mapping_count + 2,);
    }

    /// Regression test for #2115 (clawpatch #2103): the non-atomic
    /// `ConfigPlan::apply` path must enforce the SAME post-mutation gates as
    /// `apply_atomic` (ADR-027 D2 structural validation + D8 keystroke
    /// deny-list), not bypass them. A plan whose post-state is rejected by the
    /// deny-list (Cmd+Q force-quit) must fail through `apply` too, and — like
    /// `apply_atomic` — must leave the caller's config untouched. Before the
    /// fix, `apply` ran only the TOCTOU `validate_state` check and then mutated
    /// the config in place, silently applying the invalid mapping.
    #[test]
    fn test_apply_enforces_post_validation_like_apply_atomic() {
        let mut config = create_test_config();
        let original_mapping_count = config.modes[0].mappings.len();

        let plan = ConfigPlan::new(
            "Mistakenly map a pad to Cmd+Q via the non-atomic apply path",
            vec![ConfigChange::CreateMapping {
                mode: "Default".to_string(),
                trigger: Trigger::Note {
                    note: 41,
                    velocity_min: Some(1),
                    channel: None,
                    device: None,
                },
                action: ActionConfig::Keystroke {
                    keys: "q".to_string(),
                    modifiers: vec!["cmd".to_string()],
                },
                description: Some("Quit (denied)".to_string()),
                let_through: false,
            }],
            &config,
        );

        match plan.apply(&mut config) {
            Err(PlanError::ValidationFailed { .. }) => { /* expected — gate enforced */ }
            other => panic!(
                "ConfigPlan::apply must reject a deny-listed post-state (not bypass \
                 post-apply validation), got: {:?}",
                other,
            ),
        }

        // Atomicity: a rejected apply must leave the caller's config untouched.
        assert_eq!(
            config.modes[0].mappings.len(),
            original_mapping_count,
            "ConfigPlan::apply must not mutate the config when post-apply validation fails",
        );
    }
}
