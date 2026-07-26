// ADR-034 §D4.A.3.3.B.1 — structural acceptance for the legacy
// `Arc<RwLock<Option<Config>>>` (mcp.rs) and `Arc<RwLock<Config>>`
// (executor.rs) removal.
//
// Source-text greps capture the textual acceptance criteria from
// #1281: once B.1 lands, every reference to the legacy types in
// the daemon source tree is gone. Audit-provenance plumbing is
// a separate sub-piece (#1282) and does NOT gate these tests.
//
// These tests deliberately mirror the style introduced by D4.A.3.3.A
// (engine_manager_d4a3_3a.rs) — the migration is mechanical and the
// contract being asserted IS textual (the issue body literally specifies
// `grep` invariants).

const EXECUTOR_SRC: &str = include_str!("../src/daemon/llm/executor.rs");
/// Concatenation of every `.rs` under `src/daemon/engine_manager/` — the
/// module was split into submodules in #2073, so the per-file scan below
/// must read the whole module. Runtime read from `CARGO_MANIFEST_DIR`
/// covers newly-added submodules automatically; scope matches the
/// pre-split single-file `include_str!` (which included its test module).
fn read_engine_manager_src() -> String {
    // Recurse so the scan covers the whole module — including the
    // `engine_manager/tests/` subdirectory — matching the pre-split
    // `include_str!` scope (the single file included its inline test
    // module). (Copilot review on #2075.)
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

/// Concatenation of every `.rs` under `src/daemon/mcp/` — the module was
/// split into submodules in #2601 (same treatment as engine_manager/#2073),
/// so the scan reads the whole directory, inline tests included.
fn read_mcp_src() -> String {
    fn collect(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("read mcp dir").flatten() {
            let p = entry.path();
            if p.is_dir() {
                collect(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/daemon/mcp");
    let mut paths: Vec<std::path::PathBuf> = Vec::new();
    collect(&dir, &mut paths);
    paths.sort();
    let mut out = String::new();
    for p in paths {
        out.push_str(&std::fs::read_to_string(&p).expect("read mcp source file"));
        out.push('\n');
    }
    out
}

static MCP_SRC: std::sync::LazyLock<String> = std::sync::LazyLock::new(read_mcp_src);

/// Enumerate `src` lines as `(original 1-based line number, line text)`,
/// dropping comment lines so structural-pattern asserts don't false-positive
/// on doc comments that mention the retired types for historical context.
///
/// Diagnostic correctness (Council #1283 round 2): the prior implementation
/// stripped comments BEFORE enumeration, so a failing assertion would
/// report a line index within the stripped source — not the real line in
/// the original file. Engineers then chased ghost line numbers. We now
/// enumerate FIRST (preserving the real index), then filter, so panic
/// messages always cite a real source line.
///
/// Comment forms recognised: `///`, `//!`, and `//` (plus space). Block
/// comments (`/* */`) and `//` without trailing space are intentionally
/// NOT recognised — historical mentions of the retired types use the
/// `// `-prefixed form, and the false-negative case (someone introduces a
/// type use inside a block comment) is preferable to false positives.
fn non_comment_lines(src: &str) -> Vec<(usize, &str)> {
    src.lines()
        .enumerate()
        .map(|(i, line)| (i + 1, line))
        .filter(|(_, line)| {
            let trimmed = line.trim_start();
            !(trimmed.starts_with("///")
                || trimmed.starts_with("//!")
                || trimmed.starts_with("// "))
        })
        .collect()
}

#[test]
fn non_comment_lines_preserves_original_line_numbers() {
    // Regression test for Council #1283 round 2 finding: line numbers in
    // panic messages must match the *original* source, not the post-strip
    // index.
    let src = "// comment 1\n// comment 2\nbad code here\n// comment 4\nmore bad code\n";
    let lines = non_comment_lines(src);
    assert_eq!(
        lines,
        vec![(3, "bad code here"), (5, "more bad code")],
        "non_comment_lines should report original 1-based line numbers"
    );
}

#[test]
fn d4a3_3b1_mcp_no_arc_rwlock_option_config() {
    // Acceptance: `grep -nE "Arc<RwLock<Option<Config>>>"` returns zero in mcp.rs
    let matches: Vec<usize> = non_comment_lines(&MCP_SRC)
        .into_iter()
        .filter(|(_, line)| line.contains("Arc<RwLock<Option<Config>>>"))
        .map(|(lineno, _)| lineno)
        .collect();
    assert!(
        matches.is_empty(),
        "legacy Arc<RwLock<Option<Config>>> in mcp/*.rs at lines: {:?}",
        matches
    );
}

#[test]
fn d4a3_3b1_executor_no_arc_rwlock_option_config() {
    let matches: Vec<usize> = non_comment_lines(EXECUTOR_SRC)
        .into_iter()
        .filter(|(_, line)| line.contains("Arc<RwLock<Option<Config>>>"))
        .map(|(lineno, _)| lineno)
        .collect();
    assert!(
        matches.is_empty(),
        "legacy Arc<RwLock<Option<Config>>> in executor.rs at lines: {:?}",
        matches
    );
}

#[test]
fn d4a3_3b1_executor_no_arc_rwlock_config() {
    // `Arc<RwLock<Config>>` (no Option) lived only in `new_with_config`
    // — also retired with the live_config migration.
    let matches: Vec<usize> = non_comment_lines(EXECUTOR_SRC)
        .into_iter()
        .filter(|(_, line)| line.contains("Arc<RwLock<Config>>"))
        .map(|(lineno, _)| lineno)
        .collect();
    assert!(
        matches.is_empty(),
        "legacy Arc<RwLock<Config>> in executor.rs at lines: {:?}",
        matches
    );
}

#[test]
fn d4a3_3b1_executor_no_self_config_lock_access() {
    // Every `self.config.read().await` / `self.config.write().await`
    // should be gone — readers via `live_config.load()`, writers via
    // `live_config.mutate_replace_whole()`.
    let matches: Vec<usize> = non_comment_lines(EXECUTOR_SRC)
        .into_iter()
        .filter(|(_, line)| {
            line.contains("self.config.read().await") || line.contains("self.config.write().await")
        })
        .map(|(lineno, _)| lineno)
        .collect();
    assert!(
        matches.is_empty(),
        "executor.rs still has self.config.{{read,write}}().await at lines: {:?}",
        matches
    );
}

#[test]
fn d4a3_3b1_no_arc_rwlock_config_in_three_target_files() {
    // Per-file check across the three biggest-offender daemon source files
    // (mcp.rs, executor.rs, engine_manager.rs) — these are the files the
    // B.1 migration touches. Other daemon source files are caught at
    // compile time when callers can't pass the old type signature; this
    // explicit scan exists to give friendly diagnostics for the three
    // files where the migration is most visible.
    //
    // Diagnostic correctness (Council #1283 round 2): each file is
    // scanned separately so panic messages cite (file, line) rather
    // than a synthetic combined-source offset that wouldn't help
    // anyone find the violation.
    let bad_patterns = ["Arc<RwLock<Option<Config>>>", "Arc<RwLock<Config>>"];
    for (label, src) in [
        ("mcp/*.rs", MCP_SRC.as_str()),
        ("executor.rs", EXECUTOR_SRC),
        ("engine_manager/*.rs", ENGINE_MANAGER_SRC.as_str()),
    ] {
        let scanned = non_comment_lines(src);
        for pat in &bad_patterns {
            let hits: Vec<usize> = scanned
                .iter()
                .filter(|(_, line)| line.contains(pat))
                .map(|(lineno, _)| *lineno)
                .collect();
            assert!(
                hits.is_empty(),
                "pattern `{}` still present in {} at lines: {:?}",
                pat,
                label,
                hits
            );
        }
    }
}

/// Recursively collect every `.rs` file under `dir`.
fn rust_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {dir:?}: {e}")) {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn d4a3_3b1_no_arc_rwlock_config_in_any_daemon_source() {
    // #1502: the legacy-type invariant is repo-wide for the daemon source
    // tree, not just the three migration-visible files. The hard-coded
    // include_str! tests above can't catch a regression introduced in any
    // OTHER module (including subdirectories such as `daemon/llm`). Walk
    // every `.rs` file under `src/daemon` and report (relative path, line)
    // for each violation.
    let daemon_src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/daemon");
    let mut files = Vec::new();
    rust_files(&daemon_src, &mut files);
    // `read_dir` order is unspecified; sort so scan order (and any failure
    // diagnostics) are deterministic across platforms/runs.
    files.sort();
    assert!(
        !files.is_empty(),
        "expected to find .rs files under {daemon_src:?}"
    );

    let bad_patterns = ["Arc<RwLock<Option<Config>>>", "Arc<RwLock<Config>>"];
    let mut violations: Vec<String> = Vec::new();
    for path in &files {
        let src = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
        let rel = path.strip_prefix(&daemon_src).unwrap_or(path).display();
        for (lineno, line) in non_comment_lines(&src) {
            for pat in &bad_patterns {
                if line.contains(pat) {
                    violations.push(format!("{rel}:{lineno}: {pat}"));
                }
            }
        }
    }

    // Deterministic diagnostics regardless of filesystem iteration order.
    violations.sort();
    assert!(
        violations.is_empty(),
        "retired Arc<RwLock<…Config…>> pattern(s) present in daemon source \
         (#1502 — invariant is repo-wide):\n{}",
        violations.join("\n")
    );
}
