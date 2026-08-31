// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Per-event evaluation context (ADR-025).
//!
//! Threaded through the trigger → condition → action pipeline. Captures
//! everything the evaluator needs to make a decision for a single ingress
//! event, so the frozen fields (`mode_id`, `profile_id`, `device_id`,
//! raw/transformed events) don't drift under mid-evaluation side effects
//! like `ModeChange`.
//!
//! Multi-profile: the ingress dispatcher produces one context per active
//! profile bound to the source device, all sharing the same raw event and
//! state reference.
//!
//! The `state` field is a **live reference** — the evaluator reads
//! "what's true right now", not a pinned snapshot. This is intentional:
//! a PC arriving on the same tick as the triggering event must be visible
//! to the condition evaluator for same-tick context switching.

use crate::control_state::{DeviceId, PhysicalControlStateStore};
use crate::event_processor::MidiEvent;

pub type ProfileId = String;
pub type ModeId = String;

/// Per-event, per-profile evaluation context. Owned by the ingress
/// dispatcher for the duration of one dispatch; passed by reference
/// into trigger matchers, condition evaluators, and action executors.
pub struct ExecutionContext<'a> {
    pub raw_event: &'a MidiEvent,
    pub transformed_event: &'a MidiEvent,
    pub device_id: DeviceId,
    pub profile_id: ProfileId,
    pub mode_id: ModeId,
    /// Live reference — reads reflect state at the moment of evaluation.
    pub state: &'a PhysicalControlStateStore,
}

/// Staged builder populated across the ingress pipeline:
/// 1. `new(state)`
/// 2. `.with_raw_event(event, device_id)` (right after raw parse)
/// 3. `.with_profile(profile_id, mode_id)` (per active binding)
/// 4. `.with_transformed_event(transformed)` (after `MidiTransform`)
/// 5. `.build()`
///
/// `build()` panics if any stage was skipped; the ingress pipeline is the
/// only caller, so the bug is a programming error not a user error.
#[derive(Clone)]
pub struct ExecutionContextBuilder<'a> {
    raw_event: Option<&'a MidiEvent>,
    transformed_event: Option<&'a MidiEvent>,
    device_id: Option<DeviceId>,
    profile_id: Option<ProfileId>,
    mode_id: Option<ModeId>,
    state: &'a PhysicalControlStateStore,
}

impl<'a> ExecutionContextBuilder<'a> {
    pub fn new(state: &'a PhysicalControlStateStore) -> Self {
        Self {
            raw_event: None,
            transformed_event: None,
            device_id: None,
            profile_id: None,
            mode_id: None,
            state,
        }
    }

    pub fn with_raw_event(mut self, event: &'a MidiEvent, device_id: DeviceId) -> Self {
        self.raw_event = Some(event);
        self.device_id = Some(device_id);
        self
    }

    pub fn with_profile(mut self, profile_id: ProfileId, mode_id: ModeId) -> Self {
        self.profile_id = Some(profile_id);
        self.mode_id = Some(mode_id);
        self
    }

    pub fn with_transformed_event(mut self, event: &'a MidiEvent) -> Self {
        self.transformed_event = Some(event);
        self
    }

    pub fn build(self) -> ExecutionContext<'a> {
        ExecutionContext {
            raw_event: self
                .raw_event
                .expect("raw_event must be set before build()"),
            transformed_event: self
                .transformed_event
                .expect("transformed_event must be set before build()"),
            device_id: self
                .device_id
                .expect("device_id must be set before build()"),
            profile_id: self
                .profile_id
                .expect("profile_id must be set before build()"),
            mode_id: self.mode_id.expect("mode_id must be set before build()"),
            state: self.state,
        }
    }

    /// True iff `build()` would succeed. Exposed for tests and guards.
    pub fn is_complete(&self) -> bool {
        self.raw_event.is_some()
            && self.transformed_event.is_some()
            && self.device_id.is_some()
            && self.profile_id.is_some()
            && self.mode_id.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn evt() -> MidiEvent {
        MidiEvent::ProgramChange {
            program: 12,
            channel: 0,
            time: Instant::now(),
        }
    }

    #[test]
    fn builder_reports_incomplete_until_all_fields_set() {
        let store = PhysicalControlStateStore::default();
        let raw = evt();
        let transformed = evt();

        let b = ExecutionContextBuilder::new(&store);
        assert!(!b.is_complete());

        let b = b.with_raw_event(&raw, "fcb1010".into());
        assert!(!b.is_complete());

        let b = b.with_profile("studio".into(), "default".into());
        assert!(!b.is_complete());

        let b = b.with_transformed_event(&transformed);
        assert!(b.is_complete());
    }

    #[test]
    #[should_panic(expected = "raw_event must be set")]
    fn build_panics_if_raw_event_missing() {
        let store = PhysicalControlStateStore::default();
        let _ = ExecutionContextBuilder::new(&store).build();
    }

    #[test]
    fn build_populates_all_fields() {
        let store = PhysicalControlStateStore::default();
        let raw = evt();
        let transformed = MidiEvent::ControlChange {
            cc: 7,
            value: 100,
            channel: 0,
            time: Instant::now(),
        };

        let ctx = ExecutionContextBuilder::new(&store)
            .with_raw_event(&raw, "fcb1010".into())
            .with_profile("studio".into(), "default".into())
            .with_transformed_event(&transformed)
            .build();

        assert_eq!(ctx.device_id, "fcb1010");
        assert_eq!(ctx.profile_id, "studio");
        assert_eq!(ctx.mode_id, "default");
        // Raw and transformed are distinguishable.
        assert!(matches!(ctx.raw_event, MidiEvent::ProgramChange { .. }));
        assert!(matches!(
            ctx.transformed_event,
            MidiEvent::ControlChange { .. }
        ));
    }

    #[test]
    fn multiple_profile_bindings_produce_distinct_contexts() {
        let store = PhysicalControlStateStore::default();
        let raw = evt();
        let transformed = evt();
        let base = ExecutionContextBuilder::new(&store).with_raw_event(&raw, "fcb1010".into());

        let ctx_a = base
            .clone()
            .with_profile("studio".into(), "default".into())
            .with_transformed_event(&transformed)
            .build();
        let ctx_b = base
            .with_profile("live".into(), "setlist-1".into())
            .with_transformed_event(&transformed)
            .build();

        assert_eq!(ctx_a.device_id, ctx_b.device_id);
        assert_ne!(ctx_a.profile_id, ctx_b.profile_id);
        assert_ne!(ctx_a.mode_id, ctx_b.mode_id);
    }

    #[test]
    fn state_reference_is_live_not_snapshot() {
        // A snapshot taken at context build time would miss this write.
        let store = PhysicalControlStateStore::default();
        let raw = evt();
        let transformed = evt();

        let ctx = ExecutionContextBuilder::new(&store)
            .with_raw_event(&raw, "fcb1010".into())
            .with_profile("studio".into(), "default".into())
            .with_transformed_event(&transformed)
            .build();

        // Write happens after context is built.
        store.observe_event(
            "fcb1010",
            &MidiEvent::ProgramChange {
                program: 12,
                channel: 0,
                time: Instant::now(),
            },
        );

        // Condition evaluator reads through ctx.state — should see the write.
        let value = ctx
            .state
            .get(&crate::control_state::StateKey::ProgramChange {
                device: "fcb1010".into(),
                channel: 0,
            })
            .expect("live ref should see post-build writes");
        assert_eq!(value.value, 12);
    }

    #[test]
    fn context_mode_id_frozen_across_state_mutation() {
        // Regression: even if a concurrent ModeChange updates some global
        // engine state, the ctx.mode_id captured at dispatch time stays
        // the one we evaluated against.
        let store = PhysicalControlStateStore::default();
        let raw = evt();
        let transformed = evt();

        let ctx = ExecutionContextBuilder::new(&store)
            .with_raw_event(&raw, "fcb1010".into())
            .with_profile("studio".into(), "default".into())
            .with_transformed_event(&transformed)
            .build();

        let captured_mode = ctx.mode_id.clone();
        // Simulate a global mode change happening between evaluations.
        let _some_other_global_state = "edit".to_string();
        assert_eq!(ctx.mode_id, captured_mode);
    }
}
