// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Integration tests for executor thread (ADR-015)
//!
//! Tests 8 of the 11 scenarios from the ADR (3 tested in engine_manager):
//! 1. Sequence doesn't block event loop (test_sequence_doesnt_block_event_loop)
//! 2. ModeChange applied via completion (test_mode_change_via_completion)
//! 3. Channel backpressure (test_channel_backpressure)
//! 4. Graceful shutdown mid-sequence (test_graceful_shutdown_mid_sequence)
//! 5. mapping_matched before mapping_fired ordering — tested in engine_manager
//! 6. invocation_id correlation (test_invocation_id_correlation)
//! 7. Dual latency fields populated (test_dual_latency_populated)
//! 8. MIDI recursion guard blocks echo (test_midi_recursion_guard_*)
//! 9. Config update between actions — tested in engine_manager unit tests
//! 10. simulate_mapping works (test_dispatch_and_wait)
//! 11. Terminal event invariant (test_terminal_event_invariant)

use arc_swap::ArcSwap;
use conductor_core::actions::Action;
use conductor_core::dispatch::DispatchOutcome;
use conductor_core::event_types::FiredTriggerInfo;
use conductor_daemon::daemon::executor_thread::{
    ActionDispatch, ActionDispatcher, ActionProvenance, DISPATCH_CHANNEL_CAPACITY,
};
use conductor_daemon::daemon::recursion_guard::MidiRecursionGuard;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

fn test_provenance(action_type: &str, summary: &str) -> ActionProvenance {
    ActionProvenance {
        device_id: Some("test-device".to_string()),
        matched_rule: Some("Note 36 → action".to_string()),
        mode_name: Some("Default".to_string()),
        action_type: action_type.to_string(),
        action_summary: summary.to_string(),
        trigger_info: FiredTriggerInfo {
            trigger_type: "note".to_string(),
            device: Some("test-device".to_string()),
            channel: Some(0),
            number: Some(36),
            value: Some(100),
        },
        mapping_label: Some("Test mapping".to_string()),
        let_through: false,
    }
}

/// Test 1: Sequence execution doesn't block the event loop (ADR-015 D1).
///
/// #1546: the previous version only asserted that a second `try_dispatch`
/// returned `Ok` while the channel wasn't full. That proves nothing about the
/// ADR-015 guarantee: an implementation that executed actions *inline on the
/// dispatch path* (blocking the producer for the full sequence duration) would
/// still let the second enqueue succeed once the first finally returned.
///
/// The real guarantee is that the producer (the MIDI/IPC event loop calling
/// `try_dispatch`) is decoupled from execution by the dedicated executor
/// thread, so dispatching never waits for an in-flight action to finish. This
/// is asserted by *timing*: dispatching a ~510ms sequence and then a simple
/// action must cost the producer far less than that sequence's execution time.
///
/// Note: the executor is intentionally single-threaded/serial, so the simple
/// action does NOT complete before the long sequence — the non-blocking
/// property is about DISPATCH, not about reordering or interleaving execution.
/// (That's why a literal "simple completes before the sequence" assertion would
/// be wrong for this architecture.)
#[tokio::test]
async fn test_sequence_doesnt_block_event_loop() {
    let mut dispatcher = ActionDispatcher::spawn(Arc::new(ArcSwap::from_pointee(HashMap::new())));

    // A sequence whose EXECUTION is genuinely long: 10 steps, each
    // Delay(1ms) + a 50ms interruptible inter-step delay ≈ 510ms on the
    // executor thread.
    let seq_id = dispatcher.next_invocation_id();
    let seq_actions: Vec<Action> = (0..10).map(|_| Action::Delay(1)).collect();
    let seq_dispatch = ActionDispatch {
        invocation_id: seq_id,
        action: Action::Sequence(seq_actions),
        context: None,
        provenance: test_provenance("sequence", "Sequence (10)"),
        dispatch_time: Instant::now(),
        network_origin: None,
    };

    // Measure the producer-side cost of dispatching the long sequence AND a
    // simple action immediately after it. If dispatch were synchronous/inline
    // (the regression ADR-015 D1 guards against), dispatching the sequence
    // alone would block here for its full ~510ms execution.
    let dispatch_start = Instant::now();
    dispatcher.try_dispatch(seq_dispatch).unwrap();

    let simple_id = dispatcher.next_invocation_id();
    let simple_dispatch = ActionDispatch {
        invocation_id: simple_id,
        action: Action::ModeChange {
            mode: "Test".to_string(),
        },
        context: None,
        provenance: test_provenance("mode_change", "Switch to Test"),
        dispatch_time: Instant::now(),
        network_origin: None,
    };
    dispatcher
        .try_dispatch(simple_dispatch)
        .expect("dispatch while the sequence is executing must not be blocked");
    let dispatch_elapsed = dispatch_start.elapsed();

    // THE non-blocking assertion: the producer dispatched both actions in a
    // small fraction of the sequence's ~510ms execution time. A ceiling of
    // 150ms sits comfortably above two channel sends (sub-millisecond) yet far
    // below the ~510ms an inline-executing dispatch path would cost.
    assert!(
        dispatch_elapsed < std::time::Duration::from_millis(150),
        "dispatching while a ~510ms sequence executes must not block the event \
         loop; both dispatches took {dispatch_elapsed:?} (inline execution on the \
         dispatch path would be >500ms) — ADR-015 D1 regression"
    );

    // Drain both completions (sequence ~510ms, then the simple action).
    let mut completions = Vec::new();
    while completions.len() < 2 {
        match tokio::time::timeout(
            std::time::Duration::from_secs(5),
            dispatcher.completion_rx.recv(),
        )
        .await
        {
            Ok(Some(c)) => completions.push(c),
            _ => break,
        }
    }

    assert_eq!(completions.len(), 2, "Should have received 2 completions");

    // Serial executor: the sequence (dispatched first) completes before the
    // simple action.
    assert_eq!(
        completions[0].invocation_id, seq_id,
        "sequence (dispatched first) completes first on the serial executor"
    );
    assert_eq!(completions[1].invocation_id, simple_id);

    // Guard against the timing assertion above being vacuous: the sequence must
    // really have spent a substantial time EXECUTING, so "dispatch was fast" is
    // meaningful relative to a genuinely slow execution rather than a no-op.
    assert!(
        completions[0].execution_time_us >= 300_000,
        "the sequence should take >= 300ms to execute (10 × ~51ms); got {}us — \
         if execution were trivially fast the non-blocking timing assertion \
         would prove nothing",
        completions[0].execution_time_us
    );

    dispatcher.shutdown();
}

/// Test 2: ModeChange propagated through completion
#[tokio::test]
async fn test_mode_change_via_completion() {
    let mut dispatcher = ActionDispatcher::spawn(Arc::new(ArcSwap::from_pointee(HashMap::new())));
    let id = dispatcher.next_invocation_id();

    let dispatch = ActionDispatch {
        invocation_id: id,
        action: Action::ModeChange {
            mode: "Live".to_string(),
        },
        context: None,
        provenance: test_provenance("mode_change", "Switch to Live"),
        dispatch_time: Instant::now(),
        network_origin: None,
    };
    dispatcher.try_dispatch(dispatch).unwrap();

    let completion = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        dispatcher.completion_rx.recv(),
    )
    .await
    .unwrap()
    .unwrap();

    assert_eq!(completion.invocation_id, id);
    assert!(matches!(
        completion.result,
        Ok(DispatchOutcome::ModeChangeRequested { ref mode }) if mode == "Live"
    ));

    dispatcher.shutdown();
}

/// Test 3: Channel backpressure (bounded capacity) — exercised through the
/// real `ActionDispatcher`, not a standalone crossbeam channel.
///
/// #1544: the previous version built its own `crossbeam_channel::bounded` and
/// filled it directly, so it only verified crossbeam's behaviour and the value
/// of `DISPATCH_CHANNEL_CAPACITY` — never that `ActionDispatcher` actually uses
/// a bounded channel of that capacity. A regression switching the dispatcher to
/// an unbounded channel, a different capacity, or a blocking send path would
/// not have been caught. This version drives the public `try_dispatch` API:
/// it pins the single executor worker on a long-running action so the channel
/// cannot drain, fills it to capacity, and asserts the next dispatch is
/// rejected without blocking — proving the dispatcher's own channel is bounded
/// at `DISPATCH_CHANNEL_CAPACITY`.
#[test]
fn test_channel_backpressure() {
    let mut dispatcher = ActionDispatcher::spawn(Arc::new(ArcSwap::from_pointee(HashMap::new())));

    // Pin the single executor worker on a long-running action so it stops
    // draining the dispatch channel. The delay is cancelled by `shutdown()`
    // at the end, so the full 60s never actually elapses.
    let blocker = ActionDispatch {
        invocation_id: dispatcher.next_invocation_id(),
        action: Action::Delay(60_000),
        context: None,
        provenance: test_provenance("delay", "Delay 60s (worker blocker)"),
        dispatch_time: Instant::now(),
        network_origin: None,
    };
    dispatcher
        .try_dispatch(blocker)
        .expect("first dispatch must be accepted");

    // Fill until we've observed exactly DISPATCH_CHANNEL_CAPACITY accepts.
    // If the worker hasn't pulled the blocker yet, we may briefly see Full
    // one slot early; retry until the blocker is consumed and the final slot
    // opens, then continue to full.
    let mut accepted = 0usize;
    let wait_deadline = Instant::now() + std::time::Duration::from_secs(2);
    while accepted < DISPATCH_CHANNEL_CAPACITY {
        let dispatch = ActionDispatch {
            invocation_id: dispatcher.next_invocation_id(),
            action: Action::Delay(1),
            context: None,
            provenance: test_provenance("delay", "Delay 1ms"),
            dispatch_time: Instant::now(),
            network_origin: None,
        };
        match dispatcher.try_dispatch(dispatch) {
            Ok(_) => accepted += 1,
            Err(_) => {
                // The channel is momentarily full only because the worker
                // hasn't consumed the blocker yet (a slot is still occupied by
                // it). Bounded-wait for the worker to pull the blocker and free
                // the final slot, then retry. The deadline (not a per-call
                // timing assertion) is what prevents an infinite loop if the
                // worker never makes progress — see the review note below on why
                // we don't time individual `try_dispatch` calls.
                assert!(
                    Instant::now() < wait_deadline,
                    "timed out waiting for worker to consume blocker and free a slot"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }

    // While the worker is busy executing the blocker, the dispatch queue must
    // hold exactly DISPATCH_CHANNEL_CAPACITY items before applying backpressure.
    // (The blocker was accepted too, but the worker consumed it off the queue,
    // so it does not occupy a queue slot — the bound we're pinning is the
    // queue's capacity, not the total number of accepted dispatches.)
    assert_eq!(
        accepted, DISPATCH_CHANNEL_CAPACITY,
        "the dispatch queue must hold exactly DISPATCH_CHANNEL_CAPACITY ({}) items while the \
         worker is busy — proving the dispatcher's channel is bounded at that capacity, \
         not unbounded or a different size",
        DISPATCH_CHANNEL_CAPACITY
    );

    // One more dispatch, with the queue full, must be rejected with Err
    // (back-pressure). We assert only on the Err outcome, not on timing:
    // `try_dispatch` uses crossbeam's non-blocking `try_send` (returns `Full`
    // rather than blocking), so the bounded-capacity + Err result is the
    // meaningful, deterministic guarantee. A timing assertion can't prove
    // non-blocking anyway — a regression to a blocking send would hang here
    // rather than fail an `elapsed()` check, and `elapsed()` is itself flaky
    // under CI scheduler jitter (#1544 review).
    let overflow = ActionDispatch {
        invocation_id: dispatcher.next_invocation_id(),
        action: Action::Delay(1),
        context: None,
        provenance: test_provenance("delay", "Delay 1ms"),
        dispatch_time: Instant::now(),
        network_origin: None,
    };
    assert!(
        dispatcher.try_dispatch(overflow).is_err(),
        "the dispatch past capacity must return Err (backpressure), not be accepted"
    );

    dispatcher.shutdown();
}

/// Test 4: Graceful shutdown cancels in-flight Delay
#[tokio::test]
async fn test_graceful_shutdown_mid_sequence() {
    let mut dispatcher = ActionDispatcher::spawn(Arc::new(ArcSwap::from_pointee(HashMap::new())));
    let id = dispatcher.next_invocation_id();

    // Long sequence
    let actions: Vec<Action> = (0..20).map(|_| Action::Delay(200)).collect();
    let dispatch = ActionDispatch {
        invocation_id: id,
        action: Action::Sequence(actions),
        context: None,
        provenance: test_provenance("sequence", "Sequence (20)"),
        dispatch_time: Instant::now(),
        network_origin: None,
    };
    dispatcher.try_dispatch(dispatch).unwrap();

    // Wait for execution to start
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // Shutdown should be fast (not wait for remaining steps)
    let start = Instant::now();
    let cancelled = dispatcher.shutdown();
    let shutdown_duration = start.elapsed();

    assert!(
        shutdown_duration < std::time::Duration::from_secs(5),
        "Shutdown should complete within 5s, took {:?}",
        shutdown_duration
    );

    // The in-flight sequence should have been cancelled or completed.
    // Check completion_rx for any completions received before shutdown drained them.
    let mut all_completions = cancelled;
    while let Ok(c) = dispatcher.completion_rx.try_recv() {
        all_completions.push(c);
    }

    // We should see at least one completion for the dispatched sequence
    let seq_completion = all_completions.iter().find(|c| c.invocation_id == id);
    assert!(
        seq_completion.is_some(),
        "Expected completion for in-flight sequence invocation {}",
        id
    );

    // The sequence should be cancelled (shutdown interrupted it mid-execution)
    if let Some(c) = seq_completion {
        assert!(
            matches!(c.result, Ok(DispatchOutcome::Cancelled)),
            "Expected Cancelled outcome for interrupted sequence, got {:?}",
            c.result
        );
    }
}

/// Test 6: invocation_id correlation
#[tokio::test]
async fn test_invocation_id_correlation() {
    let mut dispatcher = ActionDispatcher::spawn(Arc::new(ArcSwap::from_pointee(HashMap::new())));

    let mut expected_ids = Vec::new();
    for _ in 0..5 {
        let id = dispatcher.next_invocation_id();
        expected_ids.push(id);
        let dispatch = ActionDispatch {
            invocation_id: id,
            action: Action::Delay(1),
            context: None,
            provenance: test_provenance("delay", "Delay 1ms"),
            dispatch_time: Instant::now(),
            network_origin: None,
        };
        dispatcher.try_dispatch(dispatch).unwrap();
    }

    let mut received_ids = Vec::new();
    for _ in 0..5 {
        let c = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            dispatcher.completion_rx.recv(),
        )
        .await
        .unwrap()
        .unwrap();
        received_ids.push(c.invocation_id);
    }

    // All dispatched IDs should appear in completions
    for expected in &expected_ids {
        assert!(
            received_ids.contains(expected),
            "Missing invocation_id {} in completions",
            expected
        );
    }

    dispatcher.shutdown();
}

/// Test 7: Dual latency fields populated
#[tokio::test]
async fn test_dual_latency_populated() {
    let mut dispatcher = ActionDispatcher::spawn(Arc::new(ArcSwap::from_pointee(HashMap::new())));
    let id = dispatcher.next_invocation_id();

    let dispatch = ActionDispatch {
        invocation_id: id,
        action: Action::Delay(10), // 10ms
        context: None,
        provenance: test_provenance("delay", "Delay 10ms"),
        dispatch_time: Instant::now(),
        network_origin: None,
    };
    dispatcher.try_dispatch(dispatch).unwrap();

    let completion = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        dispatcher.completion_rx.recv(),
    )
    .await
    .unwrap()
    .unwrap();

    // execution_time_us should be at least 10ms (10000us)
    assert!(
        completion.execution_time_us >= 5000,
        "execution_time_us should be >= 5000, got {}",
        completion.execution_time_us
    );

    // dispatch_time → now should be >= execution_time (includes queue + overhead)
    let e2e_us = completion.dispatch_time.elapsed().as_micros() as u64;
    assert!(
        e2e_us >= completion.execution_time_us,
        "e2e latency ({}) should be >= execution time ({})",
        e2e_us,
        completion.execution_time_us
    );

    dispatcher.shutdown();
}

/// Test 8: MIDI recursion guard blocks echo
#[test]
fn test_midi_recursion_guard_blocks_echo() {
    let mut guard = MidiRecursionGuard::new();

    let note_on = [0x90, 60, 100]; // Note On C4 vel 100

    // Before recording, should NOT be detected
    assert!(!guard.is_echo(&note_on, None));

    // Record the sent message
    guard.record(&note_on, None);

    // Now it SHOULD be detected as echo
    assert!(guard.is_echo(&note_on, None));

    // Different message should NOT be detected
    assert!(!guard.is_echo(&[0x90, 62, 100], None));
}

/// Test 8b: MIDI recursion guard — different messages don't collide
#[test]
fn test_midi_recursion_guard_different_messages() {
    let mut guard = MidiRecursionGuard::new();

    // Record Note On C4
    guard.record(&[0x90, 60, 100], None);

    // Note On D4 should NOT be detected as echo
    assert!(!guard.is_echo(&[0x90, 62, 100], None));

    // Note Off C4 should NOT be detected as echo (different status byte)
    assert!(!guard.is_echo(&[0x80, 60, 64], None));

    // CC message should NOT be detected
    assert!(!guard.is_echo(&[0xB0, 7, 100], None));
}

/// Test 11: Terminal event invariant — every dispatch gets exactly one completion
#[tokio::test]
async fn test_terminal_event_invariant() {
    let mut dispatcher = ActionDispatcher::spawn(Arc::new(ArcSwap::from_pointee(HashMap::new())));

    // Dispatch a mix of actions, recording each invocation ID.
    let actions = vec![
        Action::Delay(1),
        Action::ModeChange {
            mode: "A".to_string(),
        },
        Action::Delay(1),
        Action::Sequence(vec![Action::Delay(1), Action::Delay(1)]),
        Action::Delay(1),
    ];

    let mut dispatched_ids = Vec::new();
    for action in actions {
        let id = dispatcher.next_invocation_id();
        let dispatch = ActionDispatch {
            invocation_id: id,
            action,
            context: None,
            provenance: test_provenance("test", "test action"),
            dispatch_time: Instant::now(),
            network_origin: None,
        };
        dispatcher.try_dispatch(dispatch).unwrap();
        dispatched_ids.push(id);
    }

    // Each dispatch — including a Sequence — emits exactly one terminal
    // completion, so we expect exactly `dispatched_ids.len()` completions.
    // Collect that many into a frequency map keyed by invocation ID (generous
    // per-recv timeout for slow CI). Collecting a known count avoids paying a
    // full multi-second drain timeout on every run (#1545 review).
    let mut completion_counts: HashMap<u64, usize> = HashMap::new();
    for _ in 0..dispatched_ids.len() {
        let completion = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            dispatcher.completion_rx.recv(),
        )
        .await
        .expect("timed out waiting for a completion")
        .expect("completion channel closed before all completions arrived");
        *completion_counts
            .entry(completion.invocation_id)
            .or_insert(0) += 1;
    }

    // Then assert NO further completion arrives. This still catches a trailing
    // duplicate (a second completion for an already-seen ID) — duplicates from
    // a single execution arrive near-simultaneously, so a short bounded wait
    // suffices instead of a multi-second drain (#1545 review). Distinguish the
    // three outcomes so a failure is unambiguous: a timeout is the success
    // path, an actual extra completion is the regression we guard, and a
    // closed channel (Ok(None)) is a distinct, separately-reported fault.
    match tokio::time::timeout(
        std::time::Duration::from_millis(250),
        dispatcher.completion_rx.recv(),
    )
    .await
    {
        Err(_) => { /* timed out — good: no further completion */ }
        Ok(Some(extra)) => panic!(
            "unexpected extra completion for invocation {} — every dispatch must emit exactly one",
            extra.invocation_id
        ),
        Ok(None) => {
            panic!("completion channel closed unexpectedly while checking for extra completions")
        }
    }

    // #1545: the terminal-event invariant ("every dispatch gets exactly one
    // completion") is PER invocation ID, not aggregate. Asserting only
    // `dispatch_count == completion_count` would let a doubled completion for
    // one ID mask a missing completion for another (or a spurious completion
    // for an ID we never dispatched). Assert per-ID cardinality instead.
    for id in &dispatched_ids {
        assert_eq!(
            completion_counts.get(id).copied().unwrap_or(0),
            1,
            "invocation {id} must have exactly one completion; full completion map: \
             {completion_counts:?}"
        );
    }

    // ...and there must be no completions for IDs that were never dispatched.
    assert_eq!(
        completion_counts.len(),
        dispatched_ids.len(),
        "unexpected completion(s) for non-dispatched invocation IDs: dispatched {dispatched_ids:?}, \
         completion map {completion_counts:?}"
    );

    dispatcher.shutdown();
}

/// Test: dispatch_and_wait works for simulate_mapping pattern
#[tokio::test]
async fn test_dispatch_and_wait() {
    let mut dispatcher = ActionDispatcher::spawn(Arc::new(ArcSwap::from_pointee(HashMap::new())));
    let id = dispatcher.next_invocation_id();

    let dispatch = ActionDispatch {
        invocation_id: id,
        action: Action::ModeChange {
            mode: "Simulate".to_string(),
        },
        context: None,
        provenance: test_provenance("mode_change", "Switch to Simulate"),
        dispatch_time: Instant::now(),
        network_origin: None,
    };

    let (completion, others) = dispatcher.dispatch_and_wait(dispatch).await.unwrap();
    assert_eq!(completion.invocation_id, id);
    assert!(others.is_empty());
    assert!(matches!(
        completion.result,
        Ok(DispatchOutcome::ModeChangeRequested { ref mode }) if mode == "Simulate"
    ));

    dispatcher.shutdown();
}
