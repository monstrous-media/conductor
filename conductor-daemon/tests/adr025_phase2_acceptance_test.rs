// Copyright 2026 Amiable
// SPDX-License-Identifier: MIT

//! ADR-025 Phase 2 — end-to-end acceptance tests (#868).
//!
//! Covers the testable subset of the spec §2.7 acceptance checklist.
//! Hardware-dependent items (real FCB1010, GUI authoring flow, LLM
//! chat round-trip) are tracked in
//! `docs/pc-context-routing/acceptance-checklist.md` for manual
//! verification.
//!
//! What this file proves:
//!   1. The FCB1010 reference fixture parses, validates, and lowers
//!      through the real `lower_action` path.
//!   2. A PC-switched expression pedal routes to the correct branch
//!      for each observed Program Change, with the default firing
//!      when no recognised preset has been stomped.
//!   3. `PhysicalControlStateStore` is in-memory only — a "daemon
//!      restart" (fresh store instance) clears all observed state
//!      (ADR-025 D2).
//!   4. Physical state is device-scoped, not mode-scoped — a
//!      `ModeChange` must not affect observed PC / CC values.
//!   5. `CcIsOn` / `CcIsOff` sugar evaluates to the expected ranges.
//!
//! Tests use `ModeChange` actions as the routed leaves so each
//! branch's firing can be asserted via `DispatchOutcome` without
//! shelling out.

use arc_swap::ArcSwap;
use conductor_core::actions::Action;
use conductor_core::config::Config;
use conductor_core::config::compile::lower_action;
use conductor_core::control_state::{PhysicalControlStateStore, StateKey};
use conductor_core::dispatch::DispatchOutcome;
use conductor_core::event_processor::MidiEvent;
use conductor_core::event_types::FiredTriggerInfo;
use conductor_daemon::daemon::executor_thread::{
    ActionDispatch, ActionDispatcher, ActionProvenance,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

fn fixture_path(name: &str) -> PathBuf {
    // `CARGO_MANIFEST_DIR` points at `conductor-daemon/` here.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("conductor-core/tests/fixtures")
        .join(name)
}

fn test_provenance() -> ActionProvenance {
    // `trigger_info.channel` is the raw MIDI channel (0-15);
    // `action_summary` is human-facing and shows the 1-indexed
    // channel that matches the Events panel / logs, so derive the
    // display channel from the same value to keep them aligned.
    let channel: u8 = 0;
    ActionProvenance {
        device_id: Some("fcb1010".to_string()),
        matched_rule: None,
        mode_name: Some("Live".to_string()),
        action_type: "context-switch".to_string(),
        action_summary: format!("PcContextSwitch(fcb1010/ch{})", channel + 1),
        trigger_info: FiredTriggerInfo {
            trigger_type: "cc".to_string(),
            device: Some("fcb1010".to_string()),
            channel: Some(channel),
            number: Some(7),
            value: Some(64),
        },
        mapping_label: None,
        let_through: false,
    }
}

fn mode_change(name: &str) -> conductor_core::config::ActionConfig {
    conductor_core::config::ActionConfig::ModeChange {
        mode: name.to_string(),
    }
}

/// Build a PC-switched expression-pedal config with ModeChange
/// branches — same shape as the FCB1010 fixture, but leaves are
/// testable (each branch's firing is visible via
/// `DispatchOutcome::ModeChangeRequested`).
fn pc_switched_pedal_action() -> Action {
    use conductor_core::config::ActionConfig;
    use indexmap::IndexMap;

    let mut mappings: IndexMap<u8, Box<ActionConfig>> = IndexMap::new();
    mappings.insert(0, Box::new(mode_change("Volume")));
    mappings.insert(1, Box::new(mode_change("Delay")));
    mappings.insert(2, Box::new(mode_change("Modulation")));

    lower_action(ActionConfig::PcContextSwitch {
        channel: 0,
        device: "fcb1010".into(),
        mappings,
        default: Some(Box::new(mode_change("NoPreset"))),
    })
}

async fn dispatch_expect_mode_change(
    dispatcher: &mut ActionDispatcher,
    action: Action,
    expected_mode: &str,
) {
    let id = dispatcher.next_invocation_id();
    let (completion, _) = dispatcher
        .dispatch_and_wait(ActionDispatch {
            invocation_id: id,
            action,
            context: None,
            provenance: test_provenance(),
            dispatch_time: Instant::now(),
            network_origin: None,
        })
        .await
        .unwrap();

    let result = completion.result.as_ref();
    assert!(
        result.is_ok(),
        "expected dispatch to succeed for mode '{expected_mode}', got error: {:?}",
        result.err(),
    );
    assert_eq!(
        result.unwrap(),
        &DispatchOutcome::ModeChangeRequested {
            mode: expected_mode.to_string()
        },
        "expected branch for mode '{expected_mode}' to fire",
    );
}

/// (1) FCB1010 reference fixture parses, validates, and lowers.
///
/// Catches bit-rot: if a future change breaks TOML parsing for
/// `PcContextSwitch`, the string-keyed `mappings` serde helper, or
/// the `[[endpoints]]` alias, this fails loudly. The fixture doubles
/// as user-facing documentation, so its validity is also a
/// contract.
#[test]
fn fcb1010_fixture_parses_validates_and_lowers() {
    // Copy the fixture into the OS temp dir so `Config::load`'s
    // directory allowlist accepts the path. This exercises the
    // real load path (allowlist + canonicalisation + serde +
    // any version-specific toml quirks) rather than only the raw
    // serde surface, so a future regression in the loader surfaces
    // here too.
    let src = fixture_path("fcb1010.toml");
    let contents = std::fs::read_to_string(&src)
        .unwrap_or_else(|e| panic!("read fixture {}: {}", src.display(), e));
    // `std::env::temp_dir()` on macOS is the user TMPDIR (e.g.
    // `/var/folders/...`), not `/tmp` — `Config::load`'s allowlist
    // canonicalises against that, so `NamedTempFile` (which uses
    // the OS temp dir) is the right home here.
    let mut tmp = tempfile::NamedTempFile::with_suffix(".toml").expect("create tempfile");
    use std::io::Write;
    tmp.write_all(contents.as_bytes())
        .expect("write fixture to tmp");
    let cfg = Config::load(tmp.path().to_str().unwrap())
        .unwrap_or_else(|e| panic!("FCB1010 fixture must load via Config::load: {e}"));

    // One endpoint declared: the FCB1010 itself (ADR-035 Phase 2 — the fixture
    // is now authored as [[endpoints]]; legacy [[bindings]] is rejected on load).
    assert_eq!(cfg.endpoints.len(), 1);
    assert_eq!(cfg.endpoints[0].alias, "fcb1010");

    // One mapping with a CC 7 trigger and a PcContextSwitch action.
    assert_eq!(cfg.modes.len(), 1);
    assert_eq!(cfg.modes[0].mappings.len(), 1);
    match &cfg.modes[0].mappings[0].action {
        conductor_core::config::ActionConfig::PcContextSwitch {
            device,
            channel,
            mappings,
            default,
        } => {
            assert_eq!(device, "fcb1010");
            assert_eq!(*channel, 0);
            assert!(mappings.contains_key(&0));
            assert!(mappings.contains_key(&1));
            assert!(mappings.contains_key(&2));
            assert!(default.is_some(), "fixture should set a default branch");
        }
        other => panic!("expected PcContextSwitch action, got: {:?}", other),
    }

    // Validation passes with no errors.
    let report = conductor_core::config::validation::validate_config(&cfg);
    assert!(
        report.errors.is_empty(),
        "FCB1010 fixture must validate cleanly; got errors: {:?}",
        report.errors,
    );

    // And the action lowers through the real `lower_action` path
    // without panicking. Exact shape is pinned by
    // `context_switch_lowering_test.rs`; here we just assert it
    // doesn't blow up on a realistic config.
    let lowered = lower_action(cfg.modes[0].mappings[0].action.clone());
    match lowered {
        Action::Conditional { .. } => {} // expected: ≤ MAX_LINEAR_BRANCHES
        Action::ContextSwitchTable { .. } => {} // also valid (oversize table)
        other => panic!(
            "expected lowered Conditional or ContextSwitchTable, got {:?}",
            other
        ),
    }
}

/// (2) PC-switched expression pedal routes to the correct branch
/// for each observed Program Change.
#[tokio::test]
async fn pc_switch_routes_active_branch_per_preset() {
    let store = Arc::new(PhysicalControlStateStore::default());
    let mut dispatcher = ActionDispatcher::spawn_with_state(
        Arc::new(ArcSwap::from_pointee(HashMap::new())),
        Some(Arc::clone(&store)),
    );

    // Simulate stomping preset 0 → expression pedal fires Volume.
    store.observe_event(
        "fcb1010",
        &MidiEvent::ProgramChange {
            program: 0,
            channel: 0,
            time: Instant::now(),
        },
    );
    dispatch_expect_mode_change(&mut dispatcher, pc_switched_pedal_action(), "Volume").await;

    // Stomp preset 1 → Delay.
    store.observe_event(
        "fcb1010",
        &MidiEvent::ProgramChange {
            program: 1,
            channel: 0,
            time: Instant::now(),
        },
    );
    dispatch_expect_mode_change(&mut dispatcher, pc_switched_pedal_action(), "Delay").await;

    // Stomp preset 2 → Modulation.
    store.observe_event(
        "fcb1010",
        &MidiEvent::ProgramChange {
            program: 2,
            channel: 0,
            time: Instant::now(),
        },
    );
    dispatch_expect_mode_change(&mut dispatcher, pc_switched_pedal_action(), "Modulation").await;

    dispatcher.shutdown();
}

/// (2b) No PC observed since daemon start → default branch fires.
#[tokio::test]
async fn pc_switch_falls_back_to_default_with_no_observed_preset() {
    let store = Arc::new(PhysicalControlStateStore::default()); // empty store
    let mut dispatcher = ActionDispatcher::spawn_with_state(
        Arc::new(ArcSwap::from_pointee(HashMap::new())),
        Some(Arc::clone(&store)),
    );

    dispatch_expect_mode_change(&mut dispatcher, pc_switched_pedal_action(), "NoPreset").await;
    dispatcher.shutdown();
}

/// (2c) Unrecognised PC (not in branch table) → default branch
/// fires. Matches ADR-025 D7 "absence ≠ match" at the branch
/// level.
#[tokio::test]
async fn pc_switch_falls_back_to_default_on_unknown_preset() {
    let store = Arc::new(PhysicalControlStateStore::default());
    store.observe_event(
        "fcb1010",
        &MidiEvent::ProgramChange {
            program: 99, // not in the branch table
            channel: 0,
            time: Instant::now(),
        },
    );
    let mut dispatcher = ActionDispatcher::spawn_with_state(
        Arc::new(ArcSwap::from_pointee(HashMap::new())),
        Some(Arc::clone(&store)),
    );

    dispatch_expect_mode_change(&mut dispatcher, pc_switched_pedal_action(), "NoPreset").await;
    dispatcher.shutdown();
}

/// (3) "Daemon restart" — a fresh `PhysicalControlStateStore`
/// instance sees no prior state. Pins the ADR-025 D2 invariant
/// that the store is in-memory only. (A true daemon restart is
/// process-level and can't be exercised in-process; this models
/// the semantic: new store = clean slate.)
#[test]
fn store_is_in_memory_only_new_instance_is_empty() {
    let first = Arc::new(PhysicalControlStateStore::default());
    first.observe_event(
        "fcb1010",
        &MidiEvent::ProgramChange {
            program: 5,
            channel: 0,
            time: Instant::now(),
        },
    );
    // First store has the observed PC.
    assert!(
        first
            .get(&StateKey::ProgramChange {
                device: "fcb1010".into(),
                channel: 0,
            })
            .is_some(),
    );

    // A new instance (simulating restart) does not.
    let second = Arc::new(PhysicalControlStateStore::default());
    assert!(
        second
            .get(&StateKey::ProgramChange {
                device: "fcb1010".into(),
                channel: 0,
            })
            .is_none(),
        "new store instance must have no observed state",
    );
}

/// (4) Physical state survives a ModeChange. The store is
/// keyed by `(device, channel)` — a `ModeChange` action at the
/// mapping layer has no effect on the store's contents, so the
/// routing resumes with the same observed PC after a mode swap.
#[tokio::test]
async fn physical_state_survives_mode_change() {
    let store = Arc::new(PhysicalControlStateStore::default());
    store.observe_event(
        "fcb1010",
        &MidiEvent::ProgramChange {
            program: 1, // Delay branch
            channel: 0,
            time: Instant::now(),
        },
    );
    let mut dispatcher = ActionDispatcher::spawn_with_state(
        Arc::new(ArcSwap::from_pointee(HashMap::new())),
        Some(Arc::clone(&store)),
    );

    // Before mode change: preset 1 routes to Delay.
    dispatch_expect_mode_change(&mut dispatcher, pc_switched_pedal_action(), "Delay").await;

    // Fire a ModeChange action — this simulates the user switching
    // mode in the GUI. The ModeChange is handled by the executor's
    // ModeChangeRequested outcome, NOT by touching the physical
    // control-state store. Assert both the dispatch succeeded AND
    // produced the expected outcome so a future regression in the
    // mode-change path can't mask the state-preservation claim
    // this test is here to make.
    dispatch_expect_mode_change(
        &mut dispatcher,
        Action::ModeChange {
            mode: "SomeOtherMode".into(),
        },
        "SomeOtherMode",
    )
    .await;

    // After mode change: the same expression-pedal routing still
    // resolves to Delay — the store's PC is untouched.
    dispatch_expect_mode_change(&mut dispatcher, pc_switched_pedal_action(), "Delay").await;

    dispatcher.shutdown();
}

/// (5) `CcIsOn` / `CcIsOff` sugar evaluates to the documented
/// inclusive ranges (64..=127 and 0..=63 respectively). Drives the
/// real `evaluate_condition` path against a live
/// `PhysicalControlStateStore` so a future change to the threshold
/// constants shows up here, not just at the config-surface layer.
#[test]
fn cc_is_on_off_sugar_evaluates_at_range_boundaries() {
    use conductor_core::Condition;
    use conductor_core::event_processor::MidiEvent;
    use conductor_daemon::conditions::{ConditionContext, evaluate_condition};

    let store = Arc::new(PhysicalControlStateStore::default());
    let ctx = ConditionContext::with_state(Arc::clone(&store));

    let on = Condition::CcIsOn {
        cc: 64,
        channel: 0,
        device: "keyboard".into(),
    };
    let off = Condition::CcIsOff {
        cc: 64,
        channel: 0,
        device: "keyboard".into(),
    };

    // Walk the full 0..=127 boundary space — the spec says
    // CcIsOn ≡ [64, 127] and CcIsOff ≡ [0, 63] inclusive.
    for (value, expect_on, expect_off) in [
        (0u8, false, true),   // low end of Off
        (63u8, false, true),  // Off boundary (top)
        (64u8, true, false),  // On boundary (bottom)
        (100u8, true, false), // mid-On
        (127u8, true, false), // top of On
    ] {
        store.observe_event(
            "keyboard",
            &MidiEvent::ControlChange {
                cc: 64,
                value,
                channel: 0,
                time: Instant::now(),
            },
        );
        assert_eq!(
            evaluate_condition(&on, Some(&ctx)),
            expect_on,
            "CcIsOn @ cc=64 value={value} should be {expect_on}"
        );
        assert_eq!(
            evaluate_condition(&off, Some(&ctx)),
            expect_off,
            "CcIsOff @ cc=64 value={value} should be {expect_off}"
        );
    }
}
