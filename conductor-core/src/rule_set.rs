// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Lock-free compiled rule set for zero-contention event matching
//! (v4.21.0 - ADR-009 Phase 3, decisions D16/D17)
//!
//! CompiledRuleSet is an immutable data structure created by [`rule_compiler::compile()`]
//! and swapped atomically via `ArcSwap`. Reads are wait-free (~1ns), and config reloads
//! never block in-flight event processing.

use crate::actions::Action;
use crate::dispatch::ActionEnvelope;
use crate::event_processor::ProcessedEvent;
use crate::execution_context::ModeId;
use crate::mapping::{self, CompiledTrigger};
use std::collections::HashMap;
use std::sync::Arc;

/// Immutable compiled rule set — atomic swap via ArcSwap for lock-free reads.
/// Created by `rule_compiler::compile()`, never mutated after construction.
#[derive(Debug, Clone)]
pub struct CompiledRuleSet {
    /// Rules indexed by mode index
    mode_rules: Vec<ModeRuleSet>,
    /// Global rules (match in any mode)
    global_rules: GlobalRuleSet,
    /// Monotonic version for debugging/logging
    version: u64,
    /// Channel scopes per device alias (#751).
    /// Missing key = no restriction (all channels). Only non-empty scopes are stored.
    channel_scopes: HashMap<String, Vec<u8>>,
}

/// Rules for a single mode, split only by device filter (ADR-037 D1).
/// Device-filtered rules are checked before any-device rules within the
/// mode. The Raw sub-buckets were removed in ADR-037 Slice 4: `Trigger::Raw`
/// is lowered to a route at config load (ADR-036 Slice 3), so it never
/// reaches the compiled rule set and the matcher no longer needs a
/// specific-vs-Raw axis.
#[derive(Debug, Clone)]
pub struct ModeRuleSet {
    /// Mode name (for logging)
    pub name: String,
    /// Rules with a device filter, indexed by alias.
    pub specific_device_rules: HashMap<String, Vec<CompiledRule>>,
    /// Rules with no device filter.
    pub specific_any_device_rules: Vec<CompiledRule>,
}

/// Global rules, same device/any-device split as `ModeRuleSet`.
#[derive(Debug, Clone)]
struct GlobalRuleSet {
    specific_device_rules: HashMap<String, Vec<CompiledRule>>,
    specific_any_device_rules: Vec<CompiledRule>,
}

/// The mode-scope of a compiled rule (ADR-040 D2, §4.1).
///
/// `[[global_mappings]]` is the only all-modes sugar; internally every
/// `CompiledRule` carries a `ModeScope` so global and mode rules share one
/// uniform model (the same shape ADR-036 routes already have via their
/// `modes` field). Mode-block mappings are `Named([their mode])`; globals
/// lower to `All`.
///
/// **Behaviour note (R3 BLOCKER):** the matcher walks mode buckets before
/// global buckets ([`CompiledRuleSet::match_event`]), so `Named` outranks
/// `All` *structurally* — this tag is metadata the match algorithm never
/// consults. [`ModeScope::weight`] encodes the same precedence intent
/// (`All → 0`, `Named → 1`) for any future consumer that scores scope in a
/// single merged bucket rather than relying on walk order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModeScope {
    /// Fires in every mode. Compiled from `[[global_mappings]]`.
    All,
    /// Scoped to the named mode(s). A mode block's mappings compile to
    /// `Named([that mode's name])`. The list is shared (`Arc`) so every rule
    /// in a mode points at one allocation — per-rule `scope.clone()` is a
    /// refcount bump, not a deep clone (Copilot review on #2269).
    Named(Arc<[ModeId]>),
}

impl ModeScope {
    /// Mode-specificity weight: `All → 0`, `Named → 1`. A `Named` scope is
    /// strictly more specific than `All`, so a global rule can never tie or
    /// shadow a mode-specific rule when scope is scored directly (ADR-040
    /// §4.1 R3 BLOCKER). The separate-bucket matcher enforces this by walk
    /// order today; the weight exists so the guarantee is also expressible
    /// for a merged-bucket consumer.
    pub fn weight(&self) -> u8 {
        match self {
            ModeScope::All => 0,
            ModeScope::Named(_) => 1,
        }
    }
}

/// A single compiled rule (immutable)
#[derive(Debug, Clone)]
pub struct CompiledRule {
    pub(crate) trigger: CompiledTrigger,
    pub action: Action,
    /// ADR-040 D2: mode-scope of this rule. `[[global_mappings]]` ⇒ `All`;
    /// a mode block's mappings ⇒ `Named([mode])`. Uniform IR for later
    /// slices / ADR-033; not consulted by the matcher (scope precedence is
    /// enforced by bucket-walk order — see [`ModeScope`]).
    pub scope: ModeScope,
    pub description: Option<String>,
    /// ADR-038: fire the action AND let the event continue to the route stage.
    /// Metadata on the winning rule only — never consulted by the match
    /// algorithm; copied onto the [`ActionEnvelope`] and consumed at the
    /// event pump's route-disposition gate (Slice 3).
    pub let_through: bool,
    /// ADR-038: positional index of the source mapping within its list
    /// (a mode's `mappings` or `global_mappings`). Identifies which mapping
    /// consented/consumed for the dispatch trace (Slice 4).
    ///
    /// Trace mapping IDs are positionally bound and valid only within the
    /// config generation that produced them: a hot-reload that inserts or
    /// deletes config nodes can leave an in-flight envelope's `mapping_id`
    /// pointing at whatever now sits at that index. This is a write-only
    /// diagnostic (trace labels only) and never affects dispatch.
    pub mapping_id: usize,
}

/// The set of constraint *dimensions* a trigger fixes (ADR-037 D2).
///
/// Used for set-theory specificity ordering within a sub-bucket: a trigger
/// whose constraint set is a strict superset of another's is strictly more
/// specific and must be evaluated first. This intentionally captures only
/// *which* dimensions are constrained, not their values — two `Note`
/// triggers (note 36 vs note 37) have the same dimensions and so the same
/// specificity; their relative order is decided by config order (stable).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TriggerConstraints {
    pub has_device: bool,
    pub has_channel: bool,
    pub has_note: bool,
    pub has_cc: bool,
    pub has_velocity_range: bool,
    pub has_message_types: bool,
    pub has_direction: bool,
    pub has_pressure_range: bool,
    /// True when a `ProgramChange` trigger fixes a specific program number
    /// (`pc = Some(n)`); false for the wildcard `pc = None`. Without this
    /// dimension a specific-program rule and a wildcard rule had identical
    /// constraint sets, so a wildcard declared first could shadow the specific
    /// rule (#2132). Mirrors how `has_velocity_range` distinguishes a
    /// value-constrained CC/Note from an unconstrained one.
    pub has_program: bool,
}

impl TriggerConstraints {
    /// Pack the constraint dimensions into a bitmask for cheap set comparison.
    /// `u16` (not `u8`) because there are now nine dimensions (#2132 added
    /// `has_program`).
    fn bits(&self) -> u16 {
        (self.has_device as u16)
            | (self.has_channel as u16) << 1
            | (self.has_note as u16) << 2
            | (self.has_cc as u16) << 3
            | (self.has_velocity_range as u16) << 4
            | (self.has_message_types as u16) << 5
            | (self.has_direction as u16) << 6
            | (self.has_pressure_range as u16) << 7
            | (self.has_program as u16) << 8
    }

    /// True iff `self`'s constraint set is a strict superset of `other`'s —
    /// every dimension `other` constrains, `self` also constrains, plus at
    /// least one more. A strict superset is strictly *more* specific.
    pub fn strictly_more_specific_than(&self, other: &Self) -> bool {
        let s = self.bits();
        let o = other.bits();
        (o & s) == o && s != o
    }

    /// True iff neither set is a superset of the other and they differ —
    /// incomparable under the specificity partial order (relative order is
    /// then decided by config order).
    pub fn is_disjoint_with(&self, other: &Self) -> bool {
        !self.strictly_more_specific_than(other)
            && !other.strictly_more_specific_than(self)
            && self != other
    }

    /// Extract the constrained dimensions from a compiled trigger.
    /// `has_device` is supplied by the caller because device-filtering is a
    /// bucket property (the `CompiledTrigger` itself carries no device).
    pub(crate) fn from_compiled(trigger: &CompiledTrigger, has_device: bool) -> Self {
        use CompiledTrigger as T;
        let mut c = TriggerConstraints {
            has_device,
            has_channel: false,
            has_note: false,
            has_cc: false,
            has_velocity_range: false,
            has_message_types: false,
            has_direction: false,
            has_pressure_range: false,
            has_program: false,
        };
        match trigger {
            T::Note {
                channel,
                velocity_min,
                ..
            } => {
                c.has_note = true;
                c.has_channel = channel.is_some();
                c.has_velocity_range = *velocity_min > 0;
            }
            T::VelocityRange { channel, .. } => {
                c.has_note = true;
                c.has_velocity_range = true;
                c.has_channel = channel.is_some();
            }
            T::NoteChord { channel, .. }
            | T::DoubleTap { channel, .. }
            | T::LongPress { channel, .. } => {
                c.has_note = true;
                c.has_channel = channel.is_some();
            }
            T::CC {
                channel, value_min, ..
            } => {
                c.has_cc = true;
                c.has_channel = channel.is_some();
                c.has_velocity_range = *value_min > 0;
            }
            T::EncoderTurn {
                direction, channel, ..
            } => {
                c.has_cc = true;
                c.has_direction = direction.is_some();
                c.has_channel = channel.is_some();
            }
            T::Aftertouch {
                pressure_min,
                channel,
            } => {
                c.has_pressure_range = *pressure_min > 0;
                c.has_channel = channel.is_some();
            }
            T::PolyAftertouch {
                pressure_min,
                channel,
                ..
            } => {
                c.has_note = true;
                c.has_pressure_range = *pressure_min > 0;
                c.has_channel = channel.is_some();
            }
            T::PitchBend {
                value_min,
                value_max,
                channel,
            } => {
                c.has_channel = channel.is_some();
                c.has_velocity_range = value_min.is_some() || value_max.is_some();
            }
            T::ProgramChange { channel, pc } => {
                c.has_channel = channel.is_some();
                // #2132: a specific program (`pc = Some(n)`) is a strict
                // superset of the wildcard (`pc = None`), so it must sort first
                // and win regardless of declaration order.
                c.has_program = pc.is_some();
            }
            T::GamepadAnalogStick { direction, .. } => {
                c.has_direction = direction.is_some();
            }
            // Gamepad button/chord/trigger constrain only their own id/axis,
            // which isn't one of the MIDI-oriented dimensions; their device
            // bucket already separates them from MIDI rules.
            T::GamepadButton { .. } | T::GamepadButtonChord { .. } | T::GamepadTrigger { .. } => {}
            // OSC triggers (ADR-039-A Slice 2, #2325) constrain only the OSC
            // address/args — not the MIDI-oriented specificity dimensions.
            // An exact address (OscMessage) is strictly more specific than a
            // pattern or arg-range, expressed through has_note as the generic
            // "exact id" dimension so first-match ordering prefers it.
            T::OscMessage { .. } => {
                c.has_note = true;
            }
            T::OscAddressPattern { .. } | T::OscArgRange { .. } => {}
        }
        c
    }
}

/// Current mode snapshot — swapped atomically alongside rules
#[derive(Debug, Clone, Default)]
pub struct ModeState {
    pub index: usize,
    pub name: String,
}

impl CompiledRuleSet {
    /// Create a new CompiledRuleSet (used by rule_compiler).
    /// Takes the two global buckets (device-filtered, any-device) directly.
    pub(crate) fn new(
        mode_rules: Vec<ModeRuleSet>,
        global_specific_device_rules: HashMap<String, Vec<CompiledRule>>,
        global_specific_any_device_rules: Vec<CompiledRule>,
        version: u64,
        channel_scopes: HashMap<String, Vec<u8>>,
    ) -> Self {
        Self {
            mode_rules,
            global_rules: GlobalRuleSet {
                specific_device_rules: global_specific_device_rules,
                specific_any_device_rules: global_specific_any_device_rules,
            },
            version,
            channel_scopes,
        }
    }

    /// Check if an event's channel is in scope for a device (#751)
    fn is_channel_in_scope(&self, device_id: &str, event: &ProcessedEvent) -> bool {
        match self.channel_scopes.get(device_id) {
            Some(channels) => channel_in_scope(channels, event.channel()),
            None => true, // Missing key = no restriction (all channels)
        }
    }

    /// Match an event against rules for the given mode and device.
    ///
    /// Returns the first matching action. Priority is split by **scope**
    /// (mode-first, then global) and within each scope by **device filter**
    /// (device-bucket before any-device) — ADR-037 D1's 2-stage model:
    ///
    /// 1. mode.specific_device_rules\[device\]
    /// 2. mode.specific_any_device_rules
    /// 3. global.specific_device_rules\[device\]
    /// 4. global.specific_any_device_rules
    ///
    /// The Raw sub-buckets (previously stages 3,4,7,8) were removed in
    /// Slice 4: `Trigger::Raw` is lowered to a route at load (Slice 3) and
    /// never reaches the compiled rule set.
    pub fn match_event(
        &self,
        event: &ProcessedEvent,
        mode_index: usize,
        device_id: Option<&str>,
    ) -> Option<Action> {
        // Channel scope check (#751) — mirrored in match_event_with_provenance().
        // If either changes, update both.
        //
        // NOTE (pre-#751 behaviour, unchanged by ADR-037 Slice 4):
        // `device_in_scope` gates only the device-filtered buckets. The
        // any-device buckets are intentionally NOT channel-gated — an
        // any-device rule is device-agnostic, so a per-device channel scope
        // doesn't apply to it. This was the behaviour before the 8→4 bucket
        // simplification and is preserved verbatim. Whether per-device
        // channel scope should also suppress any-device matches is a
        // separate #751 question, deliberately out of scope for this
        // refactor.
        let device_in_scope =
            device_id.is_none_or(|device| self.is_channel_in_scope(device, event));

        // ── Mode scope ─────────────────────────────────────────────
        if let Some(mode) = self.mode_rules.get(mode_index) {
            // 1. mode, device-bucket
            if device_in_scope
                && let Some(device) = device_id
                && let Some(rules) = mode.specific_device_rules.get(device)
                && let Some(action) = find_matching_action(rules, event)
            {
                return Some(action);
            }
            // 2. mode, any-device (not channel-gated — see note above)
            if let Some(action) = find_matching_action(&mode.specific_any_device_rules, event) {
                return Some(action);
            }
        }

        // ── Global scope ───────────────────────────────────────────
        // 3. global, device-bucket
        if device_in_scope
            && let Some(device) = device_id
            && let Some(rules) = self.global_rules.specific_device_rules.get(device)
            && let Some(action) = find_matching_action(rules, event)
        {
            return Some(action);
        }
        // 4. global, any-device
        find_matching_action(&self.global_rules.specific_any_device_rules, event)
    }

    /// Returns the number of modes in this rule set
    pub fn mode_count(&self) -> usize {
        self.mode_rules.len()
    }

    /// Returns the version number of this rule set
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Returns the mode name for the given index, if valid
    pub fn mode_name(&self, index: usize) -> Option<&str> {
        self.mode_rules.get(index).map(|m| m.name.as_str())
    }

    /// Find the mode index by name (v4.25.0 - ADR-009 Gap 1)
    pub fn find_mode_index(&self, name: &str) -> Option<usize> {
        self.mode_rules.iter().position(|m| m.name == name)
    }

    /// Borrow the compiled rule set for a given mode index. Used by tests
    /// and diagnostics to inspect the two buckets the matcher walks per
    /// scope: `specific_device_rules` is checked before
    /// `specific_any_device_rules` (ADR-037 D1).
    pub fn mode_rules(&self, index: usize) -> Option<&ModeRuleSet> {
        self.mode_rules.get(index)
    }

    /// Borrow the global (all-modes) any-device rules. For tests and
    /// diagnostics, mirroring [`mode_rules`](Self::mode_rules). ADR-040
    /// Slice 1: every rule here carries [`ModeScope::All`].
    pub fn global_any_device_rules(&self) -> &[CompiledRule] {
        &self.global_rules.specific_any_device_rules
    }

    /// Match an event and return an ActionEnvelope with provenance metadata.
    ///
    /// Same 4-stage priority order as `match_event()`. Mirror this and
    /// `match_event` together — diverging would silently desync runtime
    /// dispatch from event-monitor `mapping_matched` payloads.
    pub fn match_event_with_provenance(
        &self,
        event: &ProcessedEvent,
        mode_index: usize,
        device_id: Option<&str>,
    ) -> Option<ActionEnvelope> {
        let mode_name = self.mode_name(mode_index).map(String::from);

        // Channel scope check (#751) — mirrored in match_event().
        // If either changes, update both.
        //
        // NOTE (pre-#751 behaviour, unchanged by ADR-037 Slice 4):
        // `device_in_scope` gates only the device-filtered buckets. The
        // any-device buckets are intentionally NOT channel-gated — an
        // any-device rule is device-agnostic, so a per-device channel scope
        // doesn't apply to it. This was the behaviour before the 8→4 bucket
        // simplification and is preserved verbatim. Whether per-device
        // channel scope should also suppress any-device matches is a
        // separate #751 question, deliberately out of scope for this
        // refactor.
        let device_in_scope =
            device_id.is_none_or(|device| self.is_channel_in_scope(device, event));

        // ── Mode scope ─────────────────────────────────────────────
        if let Some(mode) = self.mode_rules.get(mode_index) {
            // 1. mode, device-bucket
            if device_in_scope
                && let Some(device) = device_id
                && let Some(rules) = mode.specific_device_rules.get(device)
                && let Some(envelope) = find_matching_envelope(rules, event, device_id, &mode_name)
            {
                return Some(envelope);
            }
            // 2. mode, any-device (not channel-gated — see note above)
            if let Some(envelope) = find_matching_envelope(
                &mode.specific_any_device_rules,
                event,
                device_id,
                &mode_name,
            ) {
                return Some(envelope);
            }
        }

        // ── Global scope ───────────────────────────────────────────
        // 3. global, device-bucket
        if device_in_scope
            && let Some(device) = device_id
            && let Some(rules) = self.global_rules.specific_device_rules.get(device)
            && let Some(envelope) = find_matching_envelope(rules, event, device_id, &mode_name)
        {
            return Some(envelope);
        }
        // 4. global, any-device
        find_matching_envelope(
            &self.global_rules.specific_any_device_rules,
            event,
            device_id,
            &mode_name,
        )
    }
}

/// Find the first matching action in a list of rules
fn find_matching_action(rules: &[CompiledRule], event: &ProcessedEvent) -> Option<Action> {
    for rule in rules {
        if mapping::trigger_matches_processed(&rule.trigger, event) {
            return Some(rule.action.clone());
        }
    }
    None
}

/// Find the first matching rule and wrap in an ActionEnvelope with provenance
fn find_matching_envelope(
    rules: &[CompiledRule],
    event: &ProcessedEvent,
    device_id: Option<&str>,
    mode_name: &Option<String>,
) -> Option<ActionEnvelope> {
    for rule in rules {
        if mapping::trigger_matches_processed(&rule.trigger, event) {
            return Some(ActionEnvelope {
                action: rule.action.clone(),
                device_id: device_id.map(String::from),
                matched_rule: rule.description.clone(),
                mode_name: mode_name.clone(),
                let_through: rule.let_through,
                mapping_id: Some(rule.mapping_id),
                // ADR-042 D17: MIDI/gamepad rule matches are not network-tainted.
                // ADR-039 sets this when it routes network-listener events here.
                network_origin: None,
            });
        }
    }
    None
}

/// Check if a MIDI channel is within a channel scope (#751).
/// Shared logic used by both CompiledRuleSet and DeviceIdentityConfig.
/// Empty scope = all channels. None channel (non-MIDI) = pass through.
pub(crate) fn channel_in_scope(channels: &[u8], channel: Option<u8>) -> bool {
    if channels.is_empty() {
        return true;
    }
    match channel {
        Some(ch) => channels.contains(&ch),
        None => true, // Non-MIDI events bypass channel filtering
    }
}
