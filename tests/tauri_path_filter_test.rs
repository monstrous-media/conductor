// Copyright 2026 Amiable
// SPDX-License-Identifier: MIT

//! Workflow path-filter policy test (#1622).
//!
//! The Tauri GUI Build workflow trigger watches `conductor-daemon/**`
//! so daemon library changes (which the GUI links against via the
//! workspace) rebuild the Tauri bundle. Test-only changes under
//! `conductor-daemon/tests/**` and `conductor-daemon/benches/**` do
//! NOT affect the bundle, but the broad path-filter still triggers
//! the build — which then has a non-trivial probability of hitting
//! the `bundle.global.js: No such file` race documented in #1622
//! (cargo registry corruption from concurrent workspace builds on
//! self-hosted macOS runners).
//!
//! This test pins the GitHub-Actions negation patterns that exclude
//! test/bench paths from the Tauri trigger. The fix is structural:
//! a test-only PR like #1617 simply skips the flaky long-pole job.

use std::path::PathBuf;

fn tauri_workflow() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/tauri-build.yml");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Collects the entries under every `paths:` block in the file.
/// Returns `Vec<Vec<String>>` — one inner Vec per `paths:` block, in
/// source order (push first, then pull_request).
///
/// Uses index-controlled iteration (mirroring `tests/release_secrets_policy.rs`)
/// so the line that ends a block is RE-EXAMINED for any new block it might
/// start, rather than dropped. A naive `for line in lines { continue; }`
/// pattern silently misses adjacent `paths:` blocks at the same indent.
fn paths_blocks(yaml: &str) -> Vec<Vec<String>> {
    let mut blocks: Vec<Vec<String>> = Vec::new();
    let lines: Vec<&str> = yaml.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();

        if trimmed != "paths:" {
            i += 1;
            continue;
        }

        // Collect the body: every line indented STRICTLY more than the
        // `paths:` key, until we hit a line at the same or lower indent
        // (or EOF). Empty lines are tolerated inside the block.
        let block_indent = indent;
        let mut block: Vec<String> = Vec::new();
        let mut j = i + 1;
        while j < lines.len() {
            let l = lines[j];
            let t = l.trim_start();
            let li = l.len() - t.len();
            if !t.is_empty() && li <= block_indent {
                break; // de-indent → next outer scan picks up at `j`
            }
            if let Some(rest) = t.strip_prefix("- ") {
                let pat = rest.trim().trim_matches('\'').trim_matches('"').to_string();
                block.push(pat);
            }
            j += 1;
        }
        blocks.push(block);
        i = j; // resume from the line that terminated the block
    }
    blocks
}

#[test]
fn paths_blocks_are_present_in_push_and_pull_request_triggers() {
    // Sanity: the parser found both blocks (push trigger + pull_request
    // trigger). If this fails the workflow structure has changed and
    // the rest of the assertions below are meaningless.
    let blocks = paths_blocks(&tauri_workflow());
    assert_eq!(
        blocks.len(),
        2,
        "expected two `paths:` blocks (push + pull_request), found {} — \
         the workflow structure has changed; update the parser",
        blocks.len()
    );
}

#[test]
fn tauri_trigger_still_fires_on_daemon_library_changes() {
    // Regression guard for the fix below: narrowing must not remove the
    // positive match on `conductor-daemon/**` — production daemon code
    // changes still need to rebuild Tauri (it links against the daemon
    // via the cargo workspace).
    let yaml = tauri_workflow();
    for (i, block) in paths_blocks(&yaml).into_iter().enumerate() {
        let has_daemon = block.iter().any(|p| p == "conductor-daemon/**");
        assert!(
            has_daemon,
            "paths block {i}: must still include `conductor-daemon/**` so \
             real daemon library changes trigger the Tauri rebuild. \
             Patterns present: {block:?}"
        );
    }
}

#[test]
fn tauri_trigger_excludes_daemon_test_only_changes() {
    // The actual fix: test-only changes under conductor-daemon/tests/
    // do not affect the Tauri bundle (tests aren't linked into the GUI
    // binary). Excluding them via GitHub Actions' `!` negation prevents
    // PRs like #1617 from gratuitously triggering the long-pole macOS
    // Tauri build, where the cargo registry race (#1622) intermittently
    // corrupts tauri-2.9.3/scripts/bundle.global.js on concurrent runs.
    let yaml = tauri_workflow();
    for (i, block) in paths_blocks(&yaml).into_iter().enumerate() {
        let excludes_tests = block.iter().any(|p| p == "!conductor-daemon/tests/**");
        assert!(
            excludes_tests,
            "paths block {i}: must include `!conductor-daemon/tests/**` so \
             test-only PRs don't trigger the flaky long-pole Tauri macOS \
             build (#1622). Per GitHub Actions paths matching: a positive \
             pattern followed by a `!` negation excludes the negated \
             subset. Patterns present: {block:?}"
        );
    }
}

#[test]
fn tauri_trigger_excludes_daemon_bench_only_changes() {
    // Same reasoning as tests: bench code doesn't ship in the bundle.
    let yaml = tauri_workflow();
    for (i, block) in paths_blocks(&yaml).into_iter().enumerate() {
        let excludes_benches = block.iter().any(|p| p == "!conductor-daemon/benches/**");
        assert!(
            excludes_benches,
            "paths block {i}: must include `!conductor-daemon/benches/**` \
             so bench-only PRs don't trigger the flaky long-pole Tauri \
             macOS build (#1622). Patterns present: {block:?}"
        );
    }
}

#[test]
fn negation_patterns_follow_their_positive_match() {
    // GitHub Actions paths-matching is order-sensitive: a `!` pattern
    // takes effect only if it appears AFTER the positive pattern it
    // negates. Pin the relative order so a future cleanup doesn't
    // silently invert the filter.
    let yaml = tauri_workflow();
    for (i, block) in paths_blocks(&yaml).into_iter().enumerate() {
        let daemon_idx = block.iter().position(|p| p == "conductor-daemon/**");
        let neg_tests_idx = block.iter().position(|p| p == "!conductor-daemon/tests/**");
        let neg_benches_idx = block
            .iter()
            .position(|p| p == "!conductor-daemon/benches/**");

        if let (Some(d), Some(t)) = (daemon_idx, neg_tests_idx) {
            assert!(
                t > d,
                "paths block {i}: `!conductor-daemon/tests/**` (idx {t}) \
                 must come AFTER `conductor-daemon/**` (idx {d}) — GitHub \
                 Actions evaluates patterns in order; a leading negation \
                 does not match anything."
            );
        }
        if let (Some(d), Some(b)) = (daemon_idx, neg_benches_idx) {
            assert!(
                b > d,
                "paths block {i}: `!conductor-daemon/benches/**` (idx {b}) \
                 must come AFTER `conductor-daemon/**` (idx {d})."
            );
        }
    }
}
