// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Integration tests for action orchestration (Sequence, Delay, Repeat,
//! Conditional) — exercised through the PRODUCTION `ActionExecutor`.
//!
//! The previous version of this file only imported chrono + std and
//! tested local loops, structs, `thread::sleep`, and boolean expressions —
//! none of the real conductor action types or the executor. It could pass even
//! if the orchestration implementation was missing or regressed.
//!
//! These tests build real `Action` values and run them through
//! `ActionExecutor::execute`. Each leaf action is a `Shell` that appends a
//! marker line to a temp file (via `sh -c "echo … >> file"` using explicit
//! argv, which needs no display and is fully deterministic), so the file
//! content records the executor's actual execution order, repeat count,
//! conditional gating, and error-propagation behaviour.
//!
//! MouseClick is intentionally not covered here: it goes through enigo (a real
//! display/OS-automation backend with no mock seam) and is exercised by the
//! `#[ignore]`d executor tests in `tests/actions_unit_tests.rs`.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "test diagnostic output"
)]

use conductor::actions::{Action, Condition};
use conductor_daemon::ActionExecutor;
use std::path::Path;
use std::time::{Duration, Instant};

/// A leaf action that appends `marker` (+ newline) to `path`. Uses explicit
/// argv so no command-line parsing or display is involved. The path and marker
/// are passed as positional args (`$1`/`$2`) rather than interpolated into the
/// script, so paths with spaces or shell-special characters are handled safely.
fn append_marker(path: &Path, marker: &str) -> Action {
    Action::Shell {
        sandbox: None,
        command: "sh".to_string(),
        args: Some(vec![
            "-c".to_string(),
            r#"echo "$2" >> "$1""#.to_string(),
            "sh".to_string(),           // $0
            path.display().to_string(), // $1
            marker.to_string(),         // $2
        ]),
        timeout_ms: None,
    }
}

fn read_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// Run `action` through a real `ActionExecutor`, wait for the expected shell
/// appends to land, THEN settle briefly and re-read.
///
/// The executor spawns shell actions fire-and-forget (ADR-027 D7 watchdog), so
/// the writes are asynchronous. After `expect` lines appear we sleep a short
/// settle window and re-read, so a regression that erroneously runs an EXTRA
/// action (a stray conditional branch, an over-count repeat, or an action after
/// an error) lands its write before we return — and the callers' exact
/// `assert_eq!` then catches it. Without the settle, the poll could return on
/// the first line, before a stray later write appeared.
fn run_and_wait(action: Action, path: &Path, expect: usize) -> (Result<(), String>, Vec<String>) {
    let mut executor = ActionExecutor::default();
    let result = executor
        .execute(action, None)
        .map(|_| ())
        .map_err(|e| e.to_string());

    let deadline = Instant::now() + Duration::from_secs(5);
    while read_lines(path).len() < expect && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    // Settle: give any erroneous additional writes time to land before the
    // exact-equality assertions in the callers run.
    std::thread::sleep(Duration::from_millis(300));
    (result, read_lines(path))
}

#[test]
fn sequence_executes_leaf_actions_in_order() {
    let tmp = tempfile::TempDir::new().unwrap();
    let log = tmp.path().join("seq.log");
    let action = Action::Sequence(vec![
        append_marker(&log, "A"),
        append_marker(&log, "B"),
        append_marker(&log, "C"),
    ]);

    let (result, lines) = run_and_wait(action, &log, 3);
    result.expect("sequence of shell appends should succeed");
    assert_eq!(
        lines,
        vec!["A", "B", "C"],
        "executor must run the sequence in order"
    );
}

#[test]
fn repeat_executes_the_action_count_times() {
    let tmp = tempfile::TempDir::new().unwrap();
    let log = tmp.path().join("repeat.log");
    let action = Action::Repeat {
        action: Box::new(append_marker(&log, "X")),
        count: 3,
        delay_ms: None,
    };

    let (result, lines) = run_and_wait(action, &log, 3);
    result.expect("repeat should succeed");
    assert_eq!(
        lines,
        vec!["X", "X", "X"],
        "executor must run the action exactly `count` times"
    );
}

#[test]
fn conditional_always_runs_then_branch() {
    let tmp = tempfile::TempDir::new().unwrap();
    let log = tmp.path().join("cond_then.log");
    let action = Action::Conditional {
        condition: Condition::Always,
        then_action: Box::new(append_marker(&log, "THEN")),
        else_action: Some(Box::new(append_marker(&log, "ELSE"))),
    };

    let (result, lines) = run_and_wait(action, &log, 1);
    result.expect("conditional should succeed");
    assert_eq!(
        lines,
        vec!["THEN"],
        "Always must gate to the then-branch only"
    );
}

#[test]
fn conditional_never_runs_else_branch() {
    let tmp = tempfile::TempDir::new().unwrap();
    let log = tmp.path().join("cond_else.log");
    let action = Action::Conditional {
        condition: Condition::Never,
        then_action: Box::new(append_marker(&log, "THEN")),
        else_action: Some(Box::new(append_marker(&log, "ELSE"))),
    };

    let (result, lines) = run_and_wait(action, &log, 1);
    result.expect("conditional should succeed");
    assert_eq!(
        lines,
        vec!["ELSE"],
        "Never must gate to the else-branch only"
    );
}

#[test]
fn delay_inside_sequence_preserves_order_without_breaking_it() {
    let tmp = tempfile::TempDir::new().unwrap();
    let log = tmp.path().join("delay.log");
    let action = Action::Sequence(vec![
        append_marker(&log, "A"),
        Action::Delay(1), // a real Delay action between the two appends
        append_marker(&log, "B"),
    ]);

    let (result, lines) = run_and_wait(action, &log, 2);
    result.expect("sequence with a delay should succeed");
    assert_eq!(
        lines,
        vec!["A", "B"],
        "a Delay must not drop or reorder the surrounding actions"
    );
}

#[test]
fn sequence_propagates_error_and_halts_remaining_actions() {
    let tmp = tempfile::TempDir::new().unwrap();
    let log = tmp.path().join("err.log");
    // The middle action spawns a non-existent binary → execute returns Err.
    let action = Action::Sequence(vec![
        append_marker(&log, "A"),
        Action::Shell {
            sandbox: None,
            command: "definitely-not-a-real-binary-xyzzy".to_string(),
            args: None,
            timeout_ms: None,
        },
        append_marker(&log, "B"),
    ]);

    let (result, lines) = run_and_wait(action, &log, 1);
    assert!(
        result.is_err(),
        "a failing action inside a sequence must propagate the error"
    );
    assert_eq!(
        lines,
        vec!["A"],
        "the action after the failure must NOT run (executor halts on error)"
    );
}
