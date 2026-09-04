// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Surface-sync test for ADR-031 § 5.3.
//!
//! Pins the routing-concept fragments that the markdown LLM reference
//! (`docs/llm-reference.md`) must surface. The `REQUIRED_ROUTING_FRAGMENTS`
//! list and the unimplemented-tools canary list evolve as new MCP tools
//! land; see each list's doc comment.
//!
//! These tests pin substrings on the prompt surface so a future edit that
//! drops one fails CI rather than silently drifting the LLM's mental model
//! from the actual config grammar.
//!
//! Why a separate test (not an extension of `signal_routing_skill_test`):
//! the skill test asserts the SKILL.md file structure; this one asserts
//! the *references* to the skill from elsewhere. They're checking
//! different surfaces and a regression in either should be diagnosable
//! independently.

// ADR-045 D1: cross-checks the MCP tool catalog against skill prompts; the catalog needs mcp (or llm-executor).
#![cfg(any(feature = "mcp", feature = "llm-executor"))]

use std::path::PathBuf;

/// Routing concepts the markdown LLM reference must surface.
///
/// Add entries here as new routing concepts / tools land. Removing an
/// entry requires removing it from the reference in lockstep — the
/// failure message on the surface-sync test spells out the correct
/// resolution paths.
const REQUIRED_ROUTING_FRAGMENTS: &[&str] = &[
    "[[routes]]",
    "Signal Routing Graph (ADR-031)",
    "conductor-signal-routing",
    // Evaluation priority is the load-bearing rule for how routes
    // interact with per-event mappings; pin it. (ADR-036 Phase 2 removed
    // the Raw layer, so this is "mappings > routes".)
    "mappings > routes",
    // The first route-side MCP tool. Once the LLM sees this in its
    // system prompt it'll start calling the tool; forgetting to land
    // the mention silently degrades routing discoverability.
    "conductor_list_routes",
    // Combined-topology view. When the LLM wants the full graph in one
    // call instead of stitching `conductor_get_routing_graph` +
    // `conductor_list_routes`, this is the tool.
    "conductor_get_routing_graph",
    // Live runtime metrics (ADR-031 P4 § 6.2). When the LLM is asked
    // "is connector X busy / idle?" or "how many messages has connector
    // Y forwarded?", this tool reads the runtime `connector_registry`
    // for the answer. Distinct from `conductor_get_routing_graph`
    // (static config view).
    "conductor_get_connector_metrics",
    // The daemon-canonical resolved view. While
    // `conductor_get_routing_graph` returns the static config shape,
    // this tool returns the resolver's verdict: every connector with
    // its `bound_port` (the physical port the resolver actually
    // matched, or null if unbound), and every route with
    // `from_missing`/`to_missing` booleans flagging unresolved
    // endpoints. Use this when the LLM needs to debug "why isn't my
    // route firing?" or render an honest routing graph.
    "conductor_get_resolved_routing_graph",
    // ADR-036 D5 — route-match introspection. When the LLM is asked
    // "why didn't my route fire?", this tool evaluates a hypothetical
    // event against the live RouteEngine and explains each candidate
    // route's fired/skipped verdict.
    "conductor_explain_route_match",
    // ADR-036 §8 — recent dispatch decisions from the bounded trace
    // ring. Answers "what did the router just do?".
    "conductor_get_dispatch_trace",
    // Note: `create_route` (a batch_changes op name) is NOT pinned here.
    // It would couple the positive assertion to the canary's negative
    // assertion on `conductor_create_route` — bare-substring match would
    // false-hit the forbidden string. Its assertion lives in the test
    // body below, in markdown-codespan form.
];

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("conductor-daemon must have a parent (workspace root)")
        .to_path_buf()
}

fn read(path: &str) -> String {
    let full = workspace_root().join(path);
    std::fs::read_to_string(&full).unwrap_or_else(|e| panic!("Failed to read {full:?}: {e}"))
}

fn assert_all_present(label: &str, content: &str, fragments: &[&str]) {
    for fragment in fragments {
        assert!(
            content.contains(fragment),
            "{label} missing required fragment {fragment:?}.\n\
             `REQUIRED_ROUTING_FRAGMENTS` pins what the markdown LLM \
             reference must surface.\n\
             To resolve:\n\
             - Re-add the fragment to {label} so the LLM keeps a \
               consistent picture, OR\n\
             - If the routing concept itself is genuinely being removed/\
               renamed, update the fragment list in this test as well so \
               the assertion checks the new name."
        );
    }
}

#[test]
fn llm_reference_md_documents_routes_and_signal_routing_skill() {
    // The markdown reference (`docs/llm-reference.md`) is the
    // canonical, file-on-disk version of the L1 prompt. It must
    // surface every fragment in `REQUIRED_ROUTING_FRAGMENTS` so any
    // agent reading the reference picks up the routing concepts.
    let content = read("docs/llm-reference.md");
    assert_all_present(
        "docs/llm-reference.md",
        &content,
        REQUIRED_ROUTING_FRAGMENTS,
    );
    // Per-surface: the batch op names in their markdown-codespan form.
    // See `REQUIRED_ROUTING_FRAGMENTS` doc comment for why these are
    // per-surface rather than shared.
    assert!(
        content.contains("`create_route`"),
        "docs/llm-reference.md must mention the `create_route` batch op \
         in its markdown-codespan form."
    );
    assert!(
        content.contains("`delete_route`"),
        "docs/llm-reference.md must mention the `delete_route` batch op \
         in its markdown-codespan form."
    );
    assert!(
        content.contains("`update_route`"),
        "docs/llm-reference.md must mention the `update_route` batch op \
         in its markdown-codespan form."
    );
    // NOTE: the connector batch ops (create_connector / update_connector /
    // delete_connector) were removed in ADR-035 Phase 2. They are no
    // longer asserted here; endpoints are authored via conductor_create_endpoint.
    // (Schema-level absence is pinned by
    // `daemon::mcp_tools::tests::test_batch_changes_schema_documents_route_ops`.)
}

/// Names that must NOT appear on the prompt surface — promising them to
/// the LLM would make it call a tool that doesn't exist and get an
/// "unknown tool" rejection from the daemon.
///
/// The three batch-only singletons (`conductor_create_route` /
/// `conductor_delete_route` / `conductor_update_route`) remain
/// forbidden indefinitely (ADR-031 § 5.4 design rule: route mutations
/// go through `conductor_batch_changes`, never singleton tools).
const FORBIDDEN_SINGLETON_TOOL_NAMES: &[&str] = &[
    "conductor_create_route",
    "conductor_delete_route",
    "conductor_update_route",
];

/// Run the canary against one prompt surface. Factored out so every
/// surface is checked with identical logic — a single-surface canary
/// breaks the synchronization guarantee, since a future dev could add
/// a forbidden name to an unchecked surface and the LLM would silently
/// start calling a non-existent tool.
fn assert_no_forbidden_singleton_tools(label: &str, content: &str) {
    for premature in FORBIDDEN_SINGLETON_TOOL_NAMES {
        assert!(
            !content.contains(premature),
            "{label} mentions {premature:?} but it isn't a tool. \
             Per ADR-031 § 5.4 route mutations go through \
             `conductor_batch_changes` operations (`create_route` / \
             `delete_route` / `update_route`), not singleton tools."
        );
    }
}

#[test]
fn required_and_forbidden_lists_are_disjoint() {
    // Meta-test: the two surface-sync lists (`REQUIRED_ROUTING_FRAGMENTS`
    // + `FORBIDDEN_SINGLETON_TOOL_NAMES`) must never share a string. A
    // future contributor adding the same name to both — e.g. adding
    // `conductor_create_route` to REQUIRED (wrong — singletons are
    // non-tools per § 5.4) — would create an unresolvable state where
    // the surface either fails the canary OR fails the required
    // assertion. Catch that at the test layer rather than letting it
    // bite during a real prompt-sync edit.
    //
    // Substring overlap matters too, not just exact equality —
    // `assert_no_forbidden_singleton_tools` uses `.contains()`, so
    // a REQUIRED fragment containing a forbidden name as substring
    // would couple the two assertions.
    for required in REQUIRED_ROUTING_FRAGMENTS {
        for forbidden in FORBIDDEN_SINGLETON_TOOL_NAMES {
            assert!(
                !required.contains(forbidden) && !forbidden.contains(required),
                "REQUIRED fragment {required:?} and FORBIDDEN name {forbidden:?} \
                 overlap (substring match). One must be reworded so the two \
                 surface-sync checks stay independent."
            );
        }
    }
}

#[test]
fn llm_reference_md_does_not_promise_unimplemented_route_mcp_tools() {
    let content = read("docs/llm-reference.md");
    assert_no_forbidden_singleton_tools("docs/llm-reference.md", &content);
}

// The daemon's own tool registry (`conductor-daemon/src/daemon/mcp_tools.rs`)
// is checked at the API level, not by text-scanning the source. The external
// MCP contract is the set of names returned by `get_tool_definitions()` — not
// the source bytes — and a whole-file text scan has two flaws: it raises a
// false failure for any harmless mention of a forbidden name in a comment,
// negative test, or error string, and it never actually proves the
// *published* tool list excludes those names (it only proves the bytes are
// absent). Asserting over the real builder output fixes both. (The
// positive-presence guarantee continues to come from the in-crate unit test
// `daemon::mcp_tools::tests::test_batch_changes_schema_documents_route_and_connector_ops`.)

#[test]
fn mcp_tools_rs_does_not_publish_unimplemented_route_mcp_tools() {
    // API-level canary: assert the daemon's *published* tool registry —
    // the real `get_tool_definitions()` output — advertises none of the
    // forbidden route singletons. Route mutations go through
    // `conductor_batch_changes` (`create_route` / `delete_route` /
    // `update_route`), never singleton tools (ADR-031 § 5.4). Unlike a
    // source text-scan, this proves the actual external contract and lets
    // implementation comments / error strings mention a forbidden name.
    let published: std::collections::BTreeSet<String> =
        conductor_daemon::daemon::get_tool_definitions()
            .into_iter()
            .map(|t| t.name)
            .collect();
    for forbidden in FORBIDDEN_SINGLETON_TOOL_NAMES {
        assert!(
            !published.contains(*forbidden),
            "the daemon's published tool registry (get_tool_definitions()) advertises \
             {forbidden:?}, but route mutations must go through \
             `conductor_batch_changes`, never singleton tools (ADR-031 § 5.4). \
             Published tools: {published:?}"
        );
    }
}
