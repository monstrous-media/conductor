// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

// ADR-034 §D4.A.3.3.A — structural acceptance for the legacy
// `self.config: Arc<RwLock<Config>>` removal in engine_manager.rs.
//
// Source-text greps capture the textual acceptance criteria:
// once D4.A.3.3.A lands, the legacy field, the legacy reader pattern,
// the bridge helper, and the dead public accessor are all gone.
//
// These tests are deliberately string-based because the migration is
// mechanical and the contract being asserted IS textual (the acceptance
// criteria literally specify `grep` invariants). When sub-piece B lands
// and the executor.rs Arc<RwLock<Config>> path follows, similar greps
// will go there.

/// Concatenation of every `.rs` under `src/daemon/engine_manager/`. The
/// engine_manager module was split from one file into a directory of
/// submodules, so these structural greps must scan the whole
/// module — not just `mod.rs`. Read at test runtime from
/// `CARGO_MANIFEST_DIR` so newly-added submodules are covered without
/// editing this test. Scope matches the pre-split `include_str!` of the
/// single file, which also covered the inline test module.
fn read_engine_manager_src() -> String {
    // Recurse so the scan covers the whole module — including the
    // `engine_manager/tests/` subdirectory — matching the pre-split
    // `include_str!` scope (the single file included its inline test
    // module). (Copilot review.)
    fn collect(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir)
            .expect("read engine_manager dir")
            .flatten()
        {
            let p = entry.path();
            if p.is_dir() {
                collect(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/daemon/engine_manager");
    let mut paths: Vec<std::path::PathBuf> = Vec::new();
    collect(&dir, &mut paths);
    paths.sort();
    let mut out = String::new();
    for p in paths {
        out.push_str(&std::fs::read_to_string(&p).expect("read engine_manager source file"));
        out.push('\n');
    }
    out
}

static ENGINE_MANAGER_SRC: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(read_engine_manager_src);

#[test]
fn d4a3_3a_no_legacy_self_config_read_await() {
    // Acceptance: `grep -nE "self\.config\.read\(\)\.await"` returns 0
    let matches: Vec<_> = ENGINE_MANAGER_SRC
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains("self.config.read().await"))
        .collect();
    assert!(
        matches.is_empty(),
        "legacy self.config.read().await pattern still present at: {:?}",
        matches.iter().map(|(i, _)| i + 1).collect::<Vec<_>>()
    );
}

#[test]
fn d4a3_3a_no_legacy_self_config_write_await() {
    // The bridge helper's `*self.config.write().await = ...` write must
    // go with the helper.
    let matches: Vec<_> = ENGINE_MANAGER_SRC
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains("self.config.write().await"))
        .collect();
    assert!(
        matches.is_empty(),
        "legacy self.config.write().await pattern still present at: {:?}",
        matches.iter().map(|(i, _)| i + 1).collect::<Vec<_>>()
    );
}

#[test]
fn d4a3_3a_sync_legacy_helper_removed() {
    // Acceptance: `sync_legacy_from_live_config` symbol removed.
    assert!(
        !ENGINE_MANAGER_SRC.contains("sync_legacy_from_live_config"),
        "sync_legacy_from_live_config bridge helper / call site still \
         referenced in engine_manager.rs"
    );
}

#[test]
fn d4a3_3a_get_config_arc_removed() {
    // `get_config_arc()` had zero callers across the whole workspace
    // (confirmed during D4.A.3.3.A planning). Dead-code removal is
    // part of this sub-piece, not the sub-piece B mcp/executor work.
    assert!(
        !ENGINE_MANAGER_SRC.contains("get_config_arc"),
        "get_config_arc() dead accessor still present in engine_manager.rs"
    );
}

#[test]
fn d4a3_3a_legacy_config_field_removed() {
    // The field declaration line `config: Arc<RwLock<Config>>` must
    // be gone from the struct body. Match the exact whitespace +
    // type pattern to avoid false-positives on `live_config:` or on
    // method-local `Arc<RwLock<Config>>` shapes elsewhere in the file
    // (legitimately retained until sub-piece B).
    let has_field = ENGINE_MANAGER_SRC
        .lines()
        .any(|line| line.trim_start() == "config: Arc<RwLock<Config>>,");
    assert!(
        !has_field,
        "legacy `config: Arc<RwLock<Config>>` field declaration still \
         present in EngineManager struct"
    );
}

#[test]
fn d4a3_3a_legacy_rule_set_field_removed() {
    // `self.rule_set: Arc<ArcSwap<CompiledRuleSet>>` had zero external
    // consumers (confirmed during D4.A.3.3.A planning) — every read
    // is now sourced from `live_config.load().rules`. The field +
    // its sole write (in the bridge helper) go together.
    let has_field = ENGINE_MANAGER_SRC
        .lines()
        .any(|line| line.trim_start() == "rule_set: Arc<ArcSwap<CompiledRuleSet>>,");
    assert!(
        !has_field,
        "legacy `rule_set: Arc<ArcSwap<CompiledRuleSet>>` field \
         declaration still present in EngineManager struct"
    );
}
