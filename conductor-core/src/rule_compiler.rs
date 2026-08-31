// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Compiles a Config into an immutable CompiledRuleSet.
//! (v4.21.0 - ADR-009 Phase 3, decisions D16/D17)
//!
//! Called during startup and config reload (off hot path).
//! The resulting CompiledRuleSet is swapped into an ArcSwap for lock-free reads.

use crate::config::Config;
use crate::mapping;
use crate::rule_set::{CompiledRule, CompiledRuleSet, ModeRuleSet, ModeScope, TriggerConstraints};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::warn;

/// Stable-sort a sub-bucket so strict-superset (more specific) rules are
/// evaluated before their subsets (ADR-037 D2). Rules with incomparable or
/// identical constraint sets keep config order (the sort is stable).
///
/// `has_device` is constant across a single bucket, so it doesn't affect
/// the *relative* order within it; it's passed through for correct
/// constraint extraction. Buckets are tiny, so recomputing constraints per
/// comparison is cheap (no allocation).
///
/// Note: the superset relation is a partial order. For three or more rules
/// with mutually-conflicting superset/disjoint relations the requirements
/// ("all supersets first" AND "disjoint keep config order") can't all hold;
/// the resulting order is then deterministic but unspecified. Real
/// sub-buckets don't hit this, and pairwise ordering is always correct.
fn sort_bucket_by_specificity(rules: &mut [CompiledRule], has_device: bool) {
    rules.sort_by(|a, b| {
        let ca = TriggerConstraints::from_compiled(&a.trigger, has_device);
        let cb = TriggerConstraints::from_compiled(&b.trigger, has_device);
        if ca.strictly_more_specific_than(&cb) {
            Ordering::Less
        } else if cb.strictly_more_specific_than(&ca) {
            Ordering::Greater
        } else {
            Ordering::Equal
        }
    });
}

/// Compile a Config into an immutable CompiledRuleSet.
///
/// This is called during startup and on config reload. The compilation
/// happens off the hot path, so the cost of building HashMaps is acceptable.
pub fn compile(config: &Config, version: u64) -> CompiledRuleSet {
    if config.modes.len() > 256 {
        warn!(
            mode_count = config.modes.len(),
            "Config has more than 256 modes; only first 256 will be compiled"
        );
    }

    // Compile mode-specific rules
    let mode_rules: Vec<ModeRuleSet> = config.modes.iter().take(256).map(compile_mode).collect();

    // Compile global rules: 2-way split (device-filtered / any-device).
    // ADR-040 D2: global mappings are the all-modes case → `ModeScope::All`.
    let global = partition_mappings(&config.global_mappings, &ModeScope::All);

    // Build channel scopes from the unified endpoints (#751, #2046 — ADR-035).
    // Channel scoping is a MIDI-input concept, so restrict to endpoints whose
    // effective protocol is MIDI (the validator already treats `channels` on a
    // non-MIDI endpoint as a warning — Copilot review on #2048). Only endpoints
    // with non-empty channels are included; a missing key = all channels
    // (is_channel_in_scope() treats a missing key as "no restriction").
    use crate::config::types::ConnectorProtocol;
    let channel_scopes: HashMap<String, Vec<u8>> = config
        .endpoints
        .iter()
        .filter(|e| !e.channels.is_empty() && e.effective_protocol() == ConnectorProtocol::Midi)
        .map(|e| (e.alias.clone(), e.channels.clone()))
        .collect();

    CompiledRuleSet::new(
        mode_rules,
        global.specific_device,
        global.specific_any,
        version,
        channel_scopes,
    )
}

/// Compile a single mode's mappings into a ModeRuleSet.
fn compile_mode(mode: &crate::config::Mode) -> ModeRuleSet {
    // ADR-040 D2: a mode block's mappings are scoped to that one mode. Build
    // the shared mode list ONCE; `partition_mappings` refcount-bumps it onto
    // each rule rather than deep-cloning (Copilot review on #2269).
    let scope = ModeScope::Named(Arc::from(vec![mode.name.clone()]));
    let p = partition_mappings(&mode.mappings, &scope);
    ModeRuleSet {
        name: mode.name.clone(),
        specific_device_rules: p.specific_device,
        specific_any_device_rules: p.specific_any,
    }
}

/// 2-way partition output: device-filtered / any-device (ADR-037 D1).
struct Partitioned {
    specific_device: HashMap<String, Vec<CompiledRule>>,
    specific_any: Vec<CompiledRule>,
}

/// Split mappings into the two buckets the matcher walks per scope:
/// device-filtered (indexed by alias) and any-device. Source order is
/// preserved within each bucket — that controls first-match-wins.
fn partition_mappings(mappings: &[crate::config::Mapping], scope: &ModeScope) -> Partitioned {
    let mut p = Partitioned {
        specific_device: HashMap::new(),
        specific_any: Vec::new(),
    };
    for (idx, m) in mappings.iter().enumerate() {
        let rule = CompiledRule {
            trigger: mapping::compile_trigger(&m.trigger),
            action: mapping::compile_action(&m.action),
            description: m.description.clone(),
            let_through: m.let_through,
            // ADR-038: positional index in the source list, captured before the
            // specificity sort reorders the bucket so the id stays bound to the
            // authored position (mode `mappings`/`global_mappings`).
            mapping_id: idx,
            // ADR-040 D2: uniform mode-scope tag (global ⇒ All, mode ⇒ Named).
            scope: scope.clone(),
        };
        match m.trigger.device() {
            Some(d) => p.specific_device.entry(d.clone()).or_default().push(rule),
            None => p.specific_any.push(rule),
        }
    }

    // ADR-037 D2: order each sub-bucket by specificity (strict supersets
    // first) so a more-specific rule wins over a more-general one that
    // matches the same event, regardless of config order.
    for bucket in p.specific_device.values_mut() {
        sort_bucket_by_specificity(bucket, true);
    }
    sort_bucket_by_specificity(&mut p.specific_any, false);
    p
}
