// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Integration tests for ADR-009 Phase 3b: Lock-free engine hot path
//!
//! Verifies that ArcSwap-based rule_set and current_mode provide
//! wait-free reads and atomic config reloads.

use arc_swap::ArcSwap;
use conductor_core::config::types::Config;
use conductor_core::event_processor::{ProcessedEvent, VelocityLevel};
use conductor_core::rule_compiler;
use conductor_core::rule_set::{CompiledRuleSet, ModeState};
use std::sync::Arc;

#[test]
fn test_arcswap_rule_set_load_is_wait_free() {
    let config_toml = r#"
[[modes]]
name = "Default"

[[modes.mappings]]
[modes.mappings.trigger]
type = "Note"
note = 36

[modes.mappings.action]
type = "Keystroke"
keys = "a"
"#;
    let config: Config = toml::from_str(config_toml).expect("parse");
    let rules = rule_compiler::compile(&config, 1);
    let swappable: Arc<ArcSwap<CompiledRuleSet>> = Arc::new(ArcSwap::from_pointee(rules));

    // Load is wait-free — returns an Arc<CompiledRuleSet>
    let loaded = swappable.load();
    assert_eq!(loaded.mode_count(), 1);
    assert_eq!(loaded.version(), 1);

    // Concurrent loads return the same snapshot
    let loaded2 = swappable.load();
    assert_eq!(loaded2.version(), loaded.version());
}

#[test]
fn test_mode_state_swap_atomic() {
    let mode = Arc::new(ArcSwap::from_pointee(ModeState {
        index: 0,
        name: "Default".to_string(),
    }));

    // Initial state
    let loaded = mode.load();
    assert_eq!(loaded.index, 0);
    assert_eq!(loaded.name, "Default");

    // Atomic swap to new mode
    mode.store(Arc::new(ModeState {
        index: 2,
        name: "DJ Mode".to_string(),
    }));

    // Verify swap
    let loaded = mode.load();
    assert_eq!(loaded.index, 2);
    assert_eq!(loaded.name, "DJ Mode");
}

#[test]
fn test_config_reload_swaps_rule_set() {
    // Simulate config reload: compile new rules, swap atomically
    let config_v1_toml = r#"
[[modes]]
name = "Mode A"

[[modes.mappings]]
[modes.mappings.trigger]
type = "Note"
note = 36

[modes.mappings.action]
type = "Keystroke"
keys = "a"
"#;
    let config_v2_toml = r#"
[[modes]]
name = "Mode B"

[[modes.mappings]]
[modes.mappings.trigger]
type = "Note"
note = 37

[modes.mappings.action]
type = "Keystroke"
keys = "b"
"#;
    let config_v1: Config = toml::from_str(config_v1_toml).expect("parse");
    let config_v2: Config = toml::from_str(config_v2_toml).expect("parse");

    let rules_v1 = rule_compiler::compile(&config_v1, 1);
    let swappable = Arc::new(ArcSwap::from_pointee(rules_v1));

    // Before reload: note 36 matches, note 37 doesn't
    let note36 = ProcessedEvent::PadPressed {
        note: 36,
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    };
    let note37 = ProcessedEvent::PadPressed {
        note: 37,
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    };

    let loaded = swappable.load();
    assert!(loaded.match_event(&note36, 0, None).is_some());
    assert!(loaded.match_event(&note37, 0, None).is_none());

    // Reload: atomic swap
    let rules_v2 = rule_compiler::compile(&config_v2, 2);
    swappable.store(Arc::new(rules_v2));

    // After reload: note 37 matches, note 36 doesn't
    let loaded = swappable.load();
    assert_eq!(loaded.version(), 2);
    assert!(loaded.match_event(&note37, 0, None).is_some());
    assert!(loaded.match_event(&note36, 0, None).is_none());
}

#[test]
fn test_hot_path_uses_rule_set_not_mapping_engine() {
    // Verify that CompiledRuleSet::match_event produces the same results
    // as MappingEngine::get_action_for_processed_with_device
    use conductor_core::MappingEngine;

    let config_toml = r#"
[[modes]]
name = "Default"

[[modes.mappings]]
[modes.mappings.trigger]
type = "Note"
note = 40
device = "mikro"

[modes.mappings.action]
type = "Keystroke"
keys = "a"

[[modes.mappings]]
[modes.mappings.trigger]
type = "Note"
note = 41

[modes.mappings.action]
type = "Keystroke"
keys = "b"

[[global_mappings]]
[global_mappings.trigger]
type = "CC"
cc = 7

[global_mappings.action]
type = "Keystroke"
keys = "c"
"#;
    let config: Config = toml::from_str(config_toml).expect("parse");

    // CompiledRuleSet path
    let rules = rule_compiler::compile(&config, 1);

    // MappingEngine path (backward compat)
    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    // Test 1: Device-specific match
    let note40 = ProcessedEvent::PadPressed {
        note: 40,
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    };
    let rule_result = rules.match_event(&note40, 0, Some("mikro"));
    let engine_result = engine.get_action_for_processed_with_device(&note40, 0, Some("mikro"));
    assert!(rule_result.is_some());
    assert!(engine_result.is_some());

    // Test 2: Wrong device — both should miss device-specific, match any-device if applicable
    let rule_result = rules.match_event(&note40, 0, Some("launchpad"));
    let engine_result = engine.get_action_for_processed_with_device(&note40, 0, Some("launchpad"));
    assert!(rule_result.is_none());
    assert!(engine_result.is_none());

    // Test 3: Any-device match
    let note41 = ProcessedEvent::PadPressed {
        note: 41,
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    };
    let rule_result = rules.match_event(&note41, 0, Some("mikro"));
    let engine_result = engine.get_action_for_processed_with_device(&note41, 0, Some("mikro"));
    assert!(rule_result.is_some());
    assert!(engine_result.is_some());

    // Test 4: Global CC match
    let cc7 = ProcessedEvent::CCReceived {
        cc: 7,
        value: 100,
        channel: Some(0),
    };
    let rule_result = rules.match_event(&cc7, 0, None);
    let engine_result = engine.get_action_for_processed_with_device(&cc7, 0, None);
    assert!(rule_result.is_some());
    assert!(engine_result.is_some());
}

/// A REAL concurrency test. The other tests in this file do
/// single-threaded load/store sequences (one even labels a second sequential
/// load "concurrent"), so they validate ArcSwap API usage, not the wait-free /
/// atomic-reload semantic guarantee.
///
/// Here multiple reader threads repeatedly `load_full()` + `match_event()`
/// while a writer thread swaps the `ArcSwap<CompiledRuleSet>` between distinct
/// compiled rule sets. Each rule set satisfies the invariant
/// `version() == mode_count()`, so a reader that ever observed a *torn* / mixed
/// snapshot (version from one publish, mode_count from another) would see the
/// invariant broken. Readers also hold an old snapshot and re-check it after
/// the writer has swapped many times — proving old Arcs stay usable.
#[test]
fn concurrent_readers_see_consistent_snapshots_during_writer_reloads() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;

    /// Build a CompiledRuleSet with `n` modes, compiled at version `n`, so the
    /// invariant `version() == mode_count()` holds per snapshot.
    fn compile_with_n_modes(n: usize) -> CompiledRuleSet {
        let mut toml = String::from("[device]\nname = \"T\"\nauto_connect = false\n");
        for i in 0..n {
            toml.push_str(&format!(
                "\n[[modes]]\nname = \"M{i}\"\n\
                 [[modes.mappings]]\n[modes.mappings.trigger]\n\
                 type = \"Note\"\nnote = 36\n\
                 [modes.mappings.action]\ntype = \"Keystroke\"\nkeys = \"a\"\n"
            ));
        }
        let config: Config = toml::from_str(&toml).expect("parse");
        rule_compiler::compile(&config, n as u64)
    }

    let rule_sets: Vec<Arc<CompiledRuleSet>> =
        (1..=3).map(|n| Arc::new(compile_with_n_modes(n))).collect();
    // Sanity: the invariant we rely on actually holds for each.
    for rs in &rule_sets {
        assert_eq!(rs.version() as usize, rs.mode_count());
    }

    let swappable = Arc::new(ArcSwap::from(Arc::clone(&rule_sets[0])));
    let stop = Arc::new(AtomicBool::new(false));

    // Writer: cycle through the rule sets as fast as it can.
    let writer = {
        let swappable = Arc::clone(&swappable);
        let stop = Arc::clone(&stop);
        let rule_sets = rule_sets.clone();
        thread::spawn(move || {
            let mut i = 0usize;
            while !stop.load(Ordering::Relaxed) {
                swappable.store(Arc::clone(&rule_sets[i % rule_sets.len()]));
                i += 1;
            }
        })
    };

    let event = ProcessedEvent::PadPressed {
        note: 36,
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    };

    let readers: Vec<_> = (0..4)
        .map(|_| {
            let swappable = Arc::clone(&swappable);
            let event = event.clone();
            thread::spawn(move || {
                // Capture an OLD snapshot UP FRONT and hold it for the whole
                // loop while the writer swaps the source many times underneath
                // us. It must stay internally consistent and usable afterwards.
                let old = swappable.load_full();
                for _ in 0..50_000 {
                    let snap = swappable.load_full();
                    // Wait-free read of a WHOLE, internally-consistent snapshot.
                    assert_eq!(
                        snap.version() as usize,
                        snap.mode_count(),
                        "torn snapshot observed: version != mode_count during concurrent reload"
                    );
                    // Hot path must not panic regardless of concurrent swaps.
                    let _ = snap.match_event(&event, 0, None);
                }
                // The pre-loop snapshot, retained across thousands of reloads,
                // is still consistent and usable.
                assert_eq!(
                    old.version() as usize,
                    old.mode_count(),
                    "a retained old snapshot became inconsistent after reloads"
                );
                let _ = old.match_event(&event, 0, None);
            })
        })
        .collect();

    // Join readers WITHOUT panicking the test thread first, so the writer is
    // always stopped+joined even if a reader assertion failed (no leaked
    // spinning writer). Surface reader panics only after cleanup.
    let reader_results: Vec<_> = readers.into_iter().map(|r| r.join()).collect();
    stop.store(true, Ordering::Relaxed);
    writer.join().expect("the writer thread panicked");
    for res in reader_results {
        res.expect("a reader thread panicked under concurrent reload");
    }
}
