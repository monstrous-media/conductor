// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Surface-sync test for ADR-031 P3 § 5.3 (#1143).
//!
//! Introduced in slice 2 (#1224) to pin the routing-concept fragments
//! that BOTH the markdown reference (`docs/llm-reference.md`) and the
//! GUI's chat-store template (`conductor-gui/ui/src/lib/stores/chat.js`)
//! must surface — issue #1138 tracks unification of these LLM-
//! discoverability surfaces. The `REQUIRED_ROUTING_FRAGMENTS` list and
//! the unimplemented-tools canary list evolve slice-by-slice as new
//! MCP tools land; see each list's doc comment for the slice history.
//!
//! These tests pin the substrings on both surfaces so a future edit
//! that drops one (or syncs only one) fails CI rather than silently
//! drifting the LLM's mental model from the actual config grammar.
//!
//! Why a separate test (not an extension of `signal_routing_skill_test`):
//! the skill test asserts the SKILL.md file structure; this one asserts
//! the *references* to the skill from elsewhere. They're checking
//! different surfaces and a regression in either should be diagnosable
//! independently.

// ADR-045 D1 (#2494): cross-checks the MCP tool catalog against skill prompts; the catalog needs mcp (or llm-executor).
#![cfg(any(feature = "mcp", feature = "llm-executor"))]

use std::path::PathBuf;

/// Routing concepts that BOTH the markdown reference and the chat.js
/// system-prompt template must surface. Defined once so the two
/// surface-sync tests can never drift out of step (Council review on
/// PR #1224 — duplicated fragment lists undermine the synchronisation
/// guarantee).
///
/// Add entries here as new routing concepts / tools land. Removing an
/// entry requires removing it from BOTH surfaces in lockstep — the
/// failure message on the surface-sync tests spells out the correct
/// resolution paths.
const REQUIRED_ROUTING_FRAGMENTS: &[&str] = &[
    "[[routes]]",
    "Signal Routing Graph (ADR-031)",
    "conductor-signal-routing",
    // Evaluation priority is the load-bearing rule for how routes
    // interact with per-event mappings; pin it. (ADR-036 Phase 2 removed
    // the Raw layer, so this is "mappings > routes" on both surfaces.)
    "mappings > routes",
    // The first route-side MCP tool (slice 3 / PR #1233). Once the LLM
    // sees this in its system prompt it'll start calling the tool;
    // forgetting to land the mention silently degrades routing
    // discoverability.
    "conductor_list_routes",
    // Combined-topology view (slice 16 / PR #1275). When the LLM
    // wants the full graph in one call instead of stitching
    // `conductor_get_routing_graph` + `conductor_list_routes`, this is
    // the tool. Pinning the name on both surfaces so a future edit
    // dropping it (e.g. while restructuring the systemPrompt) fails CI.
    "conductor_get_routing_graph",
    // P4 § 6.2 / #1144 slice 4 (gap D) — live runtime metrics. When
    // the LLM is asked "is connector X busy / idle?" or "how many
    // messages has connector Y forwarded?", this tool reads the
    // runtime `connector_registry` for the answer. Distinct from
    // `conductor_get_routing_graph` (static config view). Pinning the
    // name on both surfaces so the slice-5 discoverability work
    // doesn't silently regress on the next chat.js restructure.
    "conductor_get_connector_metrics",
    // #1612 (P6 follow-up to #1598 Phase 1, landed in #1605) — the
    // daemon-canonical resolved view. While `conductor_get_routing_graph`
    // returns the static config shape, this tool returns the resolver's
    // verdict: every connector with its `bound_port` (the physical port
    // the resolver actually matched, or null if unbound), and every
    // route with `from_missing`/`to_missing` booleans flagging unresolved
    // endpoints. Use this when the LLM needs to debug "why isn't my
    // route firing?" or render an honest routing graph. Pinning the
    // name on both surfaces so the future GUI swap (#1598 Phase 2) and
    // Raw-trigger rendering (#1597) keep finding it.
    "conductor_get_resolved_routing_graph",
    // ADR-036 D5 / Slice 9 (#1667) — route-match introspection. When the
    // LLM is asked "why didn't my route fire?", this tool evaluates a
    // hypothetical event against the live RouteEngine and explains each
    // candidate route's fired/skipped verdict. Pinned on both surfaces so
    // a future prompt restructure can't silently drop it.
    "conductor_explain_route_match",
    // ADR-036 §8 / Slice 9 (#1667) — recent dispatch decisions from the
    // bounded trace ring. Answers "what did the router just do?".
    "conductor_get_dispatch_trace",
    // Note: `create_route` (the batch_changes op-name landed in slice 5
    // / PR #1236) is NOT pinned here. It would couple the positive
    // assertion to the canary's negative assertion on
    // `conductor_create_route` — bare-substring match would false-hit
    // the forbidden string (Council review on PR #1237). And the
    // backtick-delimited form differs per surface (chat.js escapes as
    // `\\\`` inside the template literal; llm-reference.md uses plain
    // `` ` `` in a codespan), so the SHARED-const approach doesn't
    // cleanly cover this op. Per-surface assertions for the op name
    // live in the individual test bodies below.
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
             `REQUIRED_ROUTING_FRAGMENTS` pins what BOTH the markdown \
             reference and chat.js system-prompt template must surface.\n\
             To resolve:\n\
             - Re-add the fragment to {label} and the other surface in \
               lockstep so the LLM keeps a consistent picture, OR\n\
             - If the routing concept itself is genuinely being removed/\
               renamed, update the fragment list in this test as well so \
               the assertion checks the new name on every surface."
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
    // Per-surface: `create_route` op-name in its markdown codespan
    // form. See `REQUIRED_ROUTING_FRAGMENTS` doc comment for why this
    // is per-surface rather than shared.
    assert!(
        content.contains("`create_route`"),
        "docs/llm-reference.md must mention the `create_route` batch op \
         (slice 5 / PR #1236) in its markdown-codespan form."
    );
    // Same for `delete_route` — slice 7 (PR #1238) landed the op;
    // slice 7.5 (this PR) catches the prompt copy up.
    assert!(
        content.contains("`delete_route`"),
        "docs/llm-reference.md must mention the `delete_route` batch op \
         (slice 7 / PR #1238) in its markdown-codespan form."
    );
    // Same for `update_route` — slice 8 (PR #1240) landed the op;
    // slice 8.5 (this PR) catches the prompt copy up.
    assert!(
        content.contains("`update_route`"),
        "docs/llm-reference.md must mention the `update_route` batch op \
         (slice 8 / PR #1240) in its markdown-codespan form."
    );
    // NOTE: the connector batch ops (create_connector / update_connector /
    // delete_connector) were removed in ADR-035 Phase 2 (#1748). They are no
    // longer asserted here; endpoints are authored via conductor_create_endpoint.
    // (Schema-level absence is pinned by
    // `daemon::mcp_tools::tests::test_batch_changes_schema_documents_route_ops`.)
}

#[test]
fn chat_js_system_prompt_documents_routes_and_signal_routing_skill() {
    // The GUI's `chat.js` system-prompt template is the runtime copy
    // the LLM actually sees. Must surface the SAME fragments as the
    // markdown reference (shared `REQUIRED_ROUTING_FRAGMENTS` const)
    // — the two surfaces can never drift out of step on what they
    // promise to mention.
    let content = read("conductor-gui/ui/src/lib/stores/chat.js");
    assert_all_present(
        "conductor-gui/ui/src/lib/stores/chat.js",
        &content,
        REQUIRED_ROUTING_FRAGMENTS,
    );
    // Per-surface: `create_route` op-name in its template-literal
    // backtick-escaped form. The chat.js prompt is a JavaScript
    // template literal so backticks must be escaped in the source as
    // backslash-backtick (otherwise the template literal terminates
    // mid-string and vite fails to build — PR #1130 precedent).
    // The on-disk bytes are `\`create_route\``; in a Rust string
    // literal that's `\\`create_route\\``.
    assert!(
        content.contains("\\`create_route\\`"),
        "chat.js prompt must mention the create_route batch op \
         (slice 5 / PR #1236) inside its systemPrompt template literal. \
         Backticks must be backslash-escaped in the source."
    );
    // Same for `delete_route` — slice 7 (PR #1238) landed the op;
    // slice 7.5 (this PR) catches the prompt copy up.
    assert!(
        content.contains("\\`delete_route\\`"),
        "chat.js prompt must mention the delete_route batch op \
         (slice 7 / PR #1238) inside its systemPrompt template literal. \
         Backticks must be backslash-escaped in the source."
    );
    // Same for `update_route` — slice 8 (PR #1240) landed the op;
    // slice 8.5 (this PR) catches the prompt copy up.
    assert!(
        content.contains("\\`update_route\\`"),
        "chat.js prompt must mention the update_route batch op \
         (slice 8 / PR #1240) inside its systemPrompt template literal. \
         Backticks must be backslash-escaped in the source."
    );
    // NOTE: the connector batch ops (create_connector / update_connector /
    // delete_connector) were removed in ADR-035 Phase 2 (#1748) — endpoints are
    // authored via conductor_create_endpoint, so the prompt no longer advertises
    // them. (Daemon schema absence pinned by
    // `daemon::mcp_tools::tests::test_batch_changes_schema_documents_route_ops`.)
}

/// Names that must NOT appear on EITHER prompt surface — promising
/// them to the LLM would make it call a tool that doesn't exist and
/// get an "unknown tool" rejection from the daemon. The list shrinks
/// as slices land:
///   - slice 2 (#1224): the original three singleton names
///     (`conductor_create_route` / `conductor_list_routes` /
///     `conductor_get_routing_graph`) forbidden, only docs shipped.
///   - slice 3 (#1233): `conductor_list_routes` landed →
///     dropped from the canary.
///   - slice 7.5 (#1239): list extended to include
///     `conductor_delete_route` and `conductor_update_route` —
///     per spec § 5.4 these singletons are deliberate non-tools
///     (route mutations go through batch_changes), so they stay
///     forbidden regardless of which slice lands. Total: 4 names.
///   - slice 16 (#1275): `conductor_get_routing_graph` landed →
///     dropped from the canary. Per-surface positive-presence
///     assertions added below.
///
/// The three batch-only singletons (`conductor_create_route` /
/// `conductor_delete_route` / `conductor_update_route`) remain
/// forbidden indefinitely (spec § 5.4 design rule: route mutations
/// go through `conductor_batch_changes`, never singleton tools).
const FORBIDDEN_SINGLETON_TOOL_NAMES: &[&str] = &[
    "conductor_create_route",
    "conductor_delete_route",
    "conductor_update_route",
];

/// Run the canary against one prompt surface. Factored out so the
/// chat.js + llm-reference.md guards stay in lockstep (Council review
/// on PR #1239: a single-surface canary breaks the synchronization
/// guarantee — a future dev could add a forbidden name to the
/// surface the canary doesn't check and the LLM would silently start
/// calling a non-existent tool).
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
    // Meta-test (Council review on PR #1242): the two surface-sync
    // lists (`REQUIRED_ROUTING_FRAGMENTS` + `FORBIDDEN_SINGLETON_TOOL_NAMES`)
    // must never share a string. A future contributor adding the
    // same name to both — e.g. by adding `conductor_create_route` to
    // REQUIRED when slice 5 lands it (wrong — singletons are
    // non-tools per § 5.4) — would create an unresolvable state
    // where every surface either fails the canary OR fails the
    // required assertion. Catch that at the test layer rather than
    // letting it bite during a real prompt-sync edit.
    //
    // Substring overlap matters too, not just exact equality —
    // `assert_no_forbidden_singleton_tools` uses `.contains()`, so
    // a REQUIRED fragment containing a forbidden name as substring
    // would couple the two assertions. See the slice-6 → slice-3.5
    // canary-coupling history for the same lesson.
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
fn chat_js_does_not_promise_unimplemented_route_mcp_tools() {
    let content = read("conductor-gui/ui/src/lib/stores/chat.js");
    assert_no_forbidden_singleton_tools("conductor-gui/ui/src/lib/stores/chat.js", &content);
}

#[test]
fn llm_reference_md_does_not_promise_unimplemented_route_mcp_tools() {
    let content = read("docs/llm-reference.md");
    assert_no_forbidden_singleton_tools("docs/llm-reference.md", &content);
}

/// Batch-op string-literal fragments that the GUI-side
/// `llm_commands.rs::get_mcp_tool_definitions()` MUST publish in the
/// `conductor_batch_changes` schema (`properties.operations.items.properties.type.enum`).
///
/// Slice 11 / PR-#1257 (this PR) — gap G from the 2026-05-16 mid-flight
/// audit on #1143: surface 5 of the 5-surface LLM-discoverability
/// checklist (chat.js / llm-reference.md / SKILL.md / mcp_tools.rs /
/// llm_commands.rs) was missing every route/connector batch op-type
/// from slices 5-10, so the in-GUI LLM saw a stale schema that listed
/// only the original 5 mapping/mode ops. The prompt copy promised the
/// new ops; the published tool schema didn't deliver them.
///
/// Pattern matches: the form `"<op_name>"` (double-quoted string in
/// Rust source) so the assertion lands on the enum entry, not on
/// incidental mentions of the op name in surrounding code/comments.
/// (`conductor-gui/src-tauri/src/llm_commands.rs` is a Rust file, so
/// the on-disk bytes inside the Rust string literal need no escaping;
/// in this Rust test source we use `\"` for the embedded double quote.)
// ADR-035 Phase 2 #1748: the connector batch ops (create_connector /
// update_connector / delete_connector) were removed; only the route ops remain.
const LLM_COMMANDS_RS_REQUIRED_BATCH_OPS: &[&str] =
    &["\"create_route\"", "\"delete_route\"", "\"update_route\""];

/// Extract the `conductor_batch_changes` operations *type* enum array from the
/// `llm_commands.rs` source text (#1509). That enum is the only one in the file
/// that lists the mapping ops, so we anchor on `"create_mapping"` to
/// disambiguate it from the unrelated `direction` / `protocol` enums. Returns
/// the bracketed array slice, e.g. `["create_mapping", …, "delete_connector"]`.
///
/// This lets the assertion below land on the actual enum array rather than the
/// whole file — a whole-file `contains` would still pass if an op were removed
/// from the enum but its quoted token survived in a comment, the tool
/// description string, or a test assertion elsewhere in the file (the exact
/// regression class this test exists to catch).
fn batch_changes_type_enum(content: &str) -> &str {
    let line = content
        .lines()
        .find(|l| l.contains("\"enum\"") && l.contains("\"create_mapping\""))
        .expect(
            "could not locate the conductor_batch_changes operations type enum in \
             llm_commands.rs (the `\"enum\": [...]` array listing \"create_mapping\"). \
             If the schema was reformatted across multiple lines, update this helper.",
        );
    let start = line.find('[').expect("enum array opening bracket");
    let end = start + line[start..].find(']').expect("enum array closing bracket");
    &line[start..=end]
}

#[test]
fn llm_commands_rs_batch_changes_schema_lists_route_and_connector_ops() {
    // Surface 5 of issue #1138's LLM-discoverability checklist. The
    // GUI-side Tauri command crate publishes its own `ToolDefinition`
    // schemas (separate from the daemon's `mcp_tools.rs` — see issue
    // #1138 for the unification proposal). When the prompt copy in
    // chat.js advertises a new `conductor_batch_changes` op-type, the
    // matching enum entry MUST also land here or the in-GUI LLM hits
    // a schema-rejection on the tool call.
    let content = read("conductor-gui/src-tauri/src/llm_commands.rs");
    // #1509: assert against the actual operations-type enum array, not the
    // whole file, so a removed op can't be masked by an incidental mention
    // elsewhere. Each required op token is already double-quoted, so a
    // substring check against the array slice is exact enum membership.
    let type_enum = batch_changes_type_enum(&content);
    for op in LLM_COMMANDS_RS_REQUIRED_BATCH_OPS {
        assert!(
            type_enum.contains(op),
            "conductor-gui/src-tauri/src/llm_commands.rs `conductor_batch_changes` \
             operations type enum is missing the batch-op entry {op}. \
             `properties.operations.items.properties.type.enum` must include every \
             op-type the daemon's executor accepts (slices 5/7/8/9/10 added all 5 \
             route + connector ops; surface 5 of the LLM-discoverability checklist \
             must mirror them). Found enum array: {type_enum}"
        );
    }
}

#[test]
fn llm_commands_rs_does_not_promise_unimplemented_route_mcp_tools() {
    // Symmetric canary across all prompt surfaces — adding a forbidden
    // singleton name to the GUI tool-definitions list would have the
    // in-GUI LLM call a non-existent tool and get a structured failure.
    // The 3 forbidden route singletons (`conductor_create_route` /
    // `conductor_delete_route` / `conductor_update_route`) must stay off
    // this surface too, for the same § 5.4 design reason: route mutations
    // are batch-only. (`conductor_get_routing_graph` is now a published
    // read-only tool — it is not forbidden.)
    let content = read("conductor-gui/src-tauri/src/llm_commands.rs");
    assert_no_forbidden_singleton_tools("conductor-gui/src-tauri/src/llm_commands.rs", &content);
}

// Surface 4 (`conductor-daemon/src/daemon/mcp_tools.rs`) is checked at the
// API level, not by text-scanning the source (#1510). The external MCP
// contract is the set of names returned by `get_tool_definitions()` — not
// the source bytes — and a whole-file text scan has two flaws: it raises a
// false failure for any harmless mention of a forbidden name in a comment,
// negative test, or error string, and it never actually proves the
// *published* tool list excludes those names (it only proves the bytes are
// absent). Asserting over the real builder output fixes both. (Surface 4's
// positive-presence guarantee continues to come from the in-crate unit test
// `daemon::mcp_tools::tests::test_batch_changes_schema_documents_route_and_connector_ops`.)
//
// The GUI surface (`llm_commands.rs`, surface 5) stays text-based in
// `llm_commands_rs_does_not_promise_unimplemented_route_mcp_tools` above: this
// daemon integration test cannot depend on the GUI crate to call its tool
// builders, so the equivalent API-level check belongs in a GUI-crate test
// (tracked as a follow-up). The forbidden names are full identifier tokens
// (`conductor_create_route`), so the text scan there has a low false-positive
// risk in the meantime.

#[test]
fn mcp_tools_rs_does_not_publish_unimplemented_route_mcp_tools() {
    // API-level canary on surface 4 (#1510): assert the daemon's *published*
    // tool registry — the real `get_tool_definitions()` output — advertises
    // none of the forbidden route singletons. Route mutations go through
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
