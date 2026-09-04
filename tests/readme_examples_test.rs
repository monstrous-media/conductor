// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Guard the documentation's `VolumeControl` examples against the
//! obsolete `action` / `direction` field.
//!
//! The config schema is `VolumeControl { operation: String, value: Option<u8> }`
//! (see `conductor_core::config::types`), but the README and other doc surfaces
//! historically published copy-paste TOML using `action = "Up"` /
//! `direction = "Up"`. A user following them would author an invalid config.
//!
//! This test scans the README plus the `docs/`, `docs-site/`, and `docs-v2/`
//! trees and fails if any `VolumeControl` example reintroduces the stale field
//! (inline `{ type = "VolumeControl", action = … }` or block `type =
//! "VolumeControl"` then `action = …`). `ADR-031-implementation-spec.md` is
//! skipped because it intentionally *documents* the historical drift.

use std::path::{Path, PathBuf};

/// True if `c` can be part of a TOML/identifier key (so we don't match
/// `then_action`/`subdirection` etc. as the `action`/`direction` key).
fn is_key_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Inline form: does this single line contain a `VolumeControl` object whose
/// operation is given via the stale `action` / `direction` key (followed by
/// `=`, with any/no surrounding whitespace)?
fn line_has_stale_inline_volume_field(line: &str) -> bool {
    let Some(pos) = line.find("VolumeControl") else {
        return false;
    };
    let rest = &line[pos..];
    for field in ["action", "direction"] {
        let mut search_from = 0;
        while let Some(idx) = rest[search_from..].find(field) {
            let abs = search_from + idx;
            let preceded_by_key_char = rest[..abs].chars().next_back().is_some_and(is_key_char);
            let after = rest[abs + field.len()..].trim_start();
            if !preceded_by_key_char && after.starts_with('=') {
                return true;
            }
            search_from = abs + field.len();
        }
    }
    false
}

/// Block form: a field line whose key is the stale `action` / `direction`
/// (`action = …` or `action= …`).
fn is_stale_block_field(line: &str) -> bool {
    let t = line.trim_start();
    for field in ["action", "direction"] {
        if let Some(rest) = t.strip_prefix(field)
            && rest.trim_start().starts_with('=')
        {
            return true;
        }
    }
    false
}

/// Is this a block-form `type = "VolumeControl"` opener (no inline comma → the
/// operation is on a following line)?
fn is_block_volume_opener(line: &str) -> bool {
    let t = line.trim();
    t.starts_with("type") && t.contains("VolumeControl") && !t.contains(',')
}

fn check_doc(path: &Path) {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let lines: Vec<&str> = text.lines().collect();

    for (i, line) in lines.iter().enumerate() {
        assert!(
            !line_has_stale_inline_volume_field(line),
            "{}:{} — VolumeControl inline example uses a stale field (use `operation`): {}",
            path.display(),
            i + 1,
            line.trim(),
        );

        if is_block_volume_opener(line) {
            for next in lines.iter().skip(i + 1) {
                let t = next.trim();
                if t.is_empty()
                    || t.starts_with('[')
                    || t.starts_with("```")
                    || t.starts_with("type")
                {
                    break; // end of this action table
                }
                assert!(
                    !is_stale_block_field(next),
                    "{}:{} — VolumeControl block uses a stale field `{}` (use `operation`)",
                    path.display(),
                    i + 1,
                    t,
                );
            }
        }
    }
}

/// Collect `*.md` files under `dir`, skipping the ADR that documents the drift.
fn collect_md(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect_md(&p, out);
        } else if p.extension().is_some_and(|e| e == "md")
            && !p.ends_with("ADR-031-implementation-spec.md")
        {
            out.push(p);
        }
    }
}

#[test]
fn doc_volume_control_examples_use_operation_field() {
    // Root-crate tests run with the repo root as the working directory.
    let mut docs = vec![PathBuf::from("README.md")];
    for dir in ["docs", "docs-site", "docs-v2"] {
        collect_md(Path::new(dir), &mut docs);
    }
    // Sanity: we actually found the doc trees (don't silently pass on a bad CWD).
    assert!(
        docs.len() > 5,
        "expected to scan the README + docs trees, only found {docs:?}"
    );

    for doc in &docs {
        check_doc(doc);
    }
}
