// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Smoke test for the `conductor-signal-routing` agent skill (ADR-031 P3 § 5.1).
//!
//! The TDD plan specifies `grep "## Signal Routing"
//! skills/conductor-signal-routing/SKILL.md` as the bare-minimum acceptance
//! check. This test runs that grep at the workspace level plus parses the
//! frontmatter through the existing skill loader so a future SKILL.md edit
//! that breaks the format (missing `name`, `description`, etc.) fails CI.
//!
//! Why a separate test file (not a unit test in `src/skills/validator.rs`):
//! the validator's own tests use synthesised `tempfile`-backed skills; this
//! one points at the *real* file shipped in the repo so a bad commit can't
//! pass the per-module suite while shipping a broken skill.

use std::path::PathBuf;

/// Resolve the workspace root from `CARGO_MANIFEST_DIR` (which is
/// `conductor-daemon/`) by climbing one directory.
fn skill_path() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .expect("conductor-daemon must have a parent (workspace root)")
        .join("skills")
        .join("conductor-signal-routing")
        .join("SKILL.md")
}

#[test]
fn skill_md_exists_and_is_non_empty() {
    let path = skill_path();
    let content =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("Failed to read {:?}: {e}", path));
    assert!(
        content.len() > 200,
        "SKILL.md is suspiciously small ({} bytes) — frontmatter + decision framework should be at least a few hundred",
        content.len()
    );
}

#[test]
fn skill_md_has_decision_framework_and_safety_policy() {
    // Per spec § 5.1 the skill body must surface a decision framework and a
    // safety policy. The TDD plan specifies the
    // `grep "## Signal Routing"` check; we extend that to the two body
    // sections that the agent actually depends on at inference time.
    let content = std::fs::read_to_string(skill_path()).expect("read SKILL.md");

    for required in [
        "# Signal Routing",
        "## Decision Framework",
        "## Safety Policy",
        "## Common Patterns",
    ] {
        assert!(
            content.contains(required),
            "SKILL.md missing required section heading {required:?}"
        );
    }
}

#[test]
fn skill_md_frontmatter_has_correct_name_and_routing_description() {
    // The validator's `validate_tool_patterns` step now accepts
    // both the legacy `namespace:pattern, …` form and Claude Code's
    // space-separated `Bash(conductor:*) Read Write` form, so the
    // shipped SKILL.md can be run through the full `validate_skill`
    // path — not the `parse_skill_md` workaround the test used before
    // the fix.
    use conductor_daemon::skills::validate_skill;

    let skill_dir = skill_path()
        .parent()
        .expect("SKILL.md sits inside its skill directory")
        .to_path_buf();
    let validated = validate_skill(&skill_dir)
        .expect("validate_skill must accept the shipped signal-routing skill");
    let metadata = &validated.metadata;

    assert_eq!(
        metadata.name, "conductor-signal-routing",
        "name must match directory"
    );

    let lower = metadata.description.to_lowercase();
    assert!(
        lower.contains("rout") || lower.contains("signal"),
        "description should mention routing/signal so the agent picks \
         the skill for routing requests; got: {:?}",
        metadata.description
    );

    let allowed = metadata
        .allowed_tools
        .as_deref()
        .expect("allowed-tools must be present (string form per existing skills)");
    // The skill needs at least these tools at runtime: Bash to invoke
    // `conductor` MCP-tool calls, Read/Write to introspect or scaffold
    // config files. Pin the literal string so a future SKILL.md edit
    // that drops one of them fails CI rather than silently shipping a
    // skill without the access it needs.
    for required in ["Bash", "Read", "Write"] {
        assert!(
            allowed.contains(required),
            "allowed-tools missing {required:?}; got {allowed:?}"
        );
    }
}
