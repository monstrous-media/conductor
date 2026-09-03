// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-027 §D10a — per-pattern unit tests for the persistence write veto.
//! Positive cases (must veto) and negative cases (must NOT veto) for every
//! listed write pattern, plus the read-vs-write distinction, path
//! normalisation, and symlink resolution.

use super::*;
use std::path::{Path, PathBuf};

const HOME: &str = "/home/tester";

fn protected() -> ProtectedPaths {
    ProtectedPaths::for_test(Path::new(HOME), None)
}

fn home() -> Option<&'static Path> {
    Some(Path::new(HOME))
}

/// Build `args` from string literals.
fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

/// Assert the (program, args) pair is vetoed and return the match.
fn veto(program: &str, args: &[&str]) -> VetoMatch {
    persistence_write_veto(program, &argv(args), &protected(), home())
        .unwrap_or_else(|| panic!("expected veto for `{program} {}`", args.join(" ")))
}

/// Assert the (program, args) pair is allowed.
fn allow(program: &str, args: &[&str]) {
    let r = persistence_write_veto(program, &argv(args), &protected(), home());
    assert!(
        r.is_none(),
        "expected NO veto for `{program} {}`, got {r:?}",
        args.join(" ")
    );
}

// ───────────────────────── protected paths ─────────────────────────

#[test]
fn protects_dot_conductor() {
    let m = veto("tee", &["~/.conductor/config.toml"]);
    assert!(m.protected_path.ends_with("/.conductor"));
}

#[test]
fn protects_application_support_dir() {
    veto(
        "tee",
        &["~/Library/Application Support/conductor/live.toml"],
    );
}

#[test]
fn protects_xdg_data_home() {
    let xdg = PathBuf::from("/home/tester/xdg-data");
    let prot = ProtectedPaths::for_test(Path::new(HOME), Some(&xdg));
    let r = persistence_write_veto(
        "tee",
        &argv(&["/home/tester/xdg-data/conductor/state.json"]),
        &prot,
        home(),
    );
    assert!(r.is_some(), "XDG_DATA_HOME/conductor must be protected");
}

#[test]
fn xdg_falls_back_to_local_share() {
    // No XDG_DATA_HOME → ~/.local/share/conductor is the data root.
    veto("tee", &["~/.local/share/conductor/state.json"]);
}

#[test]
fn absolute_form_of_home_path_is_protected() {
    veto("tee", &["/home/tester/.conductor/audit.db"]);
}

#[test]
fn unrelated_paths_are_allowed() {
    allow("tee", &["/tmp/out.txt"]);
    allow("tee", &["~/Documents/notes.txt"]);
    allow("cp", &["a", "/var/tmp/b"]);
}

// ───────────────────────── write commands ─────────────────────────

#[test]
fn tee_into_protected_vetoed() {
    assert_eq!(veto("tee", &["~/.conductor/x"]).matched_pattern, "tee");
}

#[test]
fn tee_append_into_protected_vetoed() {
    veto("tee", &["-a", "~/.conductor/x"]);
}

#[test]
fn tee_to_safe_path_allowed() {
    allow("tee", &["/tmp/safe"]);
}

#[test]
fn cp_dest_protected_vetoed() {
    assert_eq!(
        veto("cp", &["/tmp/evil", "~/.conductor/config.toml"]).matched_pattern,
        "cp"
    );
}

#[test]
fn cp_source_protected_allowed() {
    // Reading a protected file as the SOURCE is fine; dest is /tmp.
    allow("cp", &["~/.conductor/config.toml", "/tmp/copy"]);
}

#[test]
fn mv_dest_protected_vetoed() {
    veto("mv", &["/tmp/x", "~/.conductor/x"]);
}

#[test]
fn install_dest_protected_vetoed() {
    veto("install", &["-m", "600", "/tmp/x", "~/.conductor/x"]);
}

#[test]
fn ln_into_protected_vetoed() {
    assert_eq!(
        veto("ln", &["-s", "/tmp/evil", "~/.conductor/hook"]).matched_pattern,
        "ln"
    );
}

#[test]
fn rm_protected_vetoed() {
    assert_eq!(veto("rm", &["-rf", "~/.conductor"]).matched_pattern, "rm");
}

#[test]
fn rm_safe_allowed() {
    allow("rm", &["-rf", "/tmp/scratch"]);
}

#[test]
fn unlink_protected_vetoed() {
    veto("unlink", &["~/.conductor/audit.db"]);
}

#[test]
fn shred_protected_vetoed() {
    veto("shred", &["~/.conductor/trusted_keys.json"]);
}

#[test]
fn dd_of_protected_vetoed() {
    assert_eq!(
        veto("dd", &["if=/dev/zero", "of=/home/tester/.conductor/x"]).matched_pattern,
        "dd of="
    );
}

#[test]
fn dd_if_protected_allowed() {
    // Reading via `if=` is not a write.
    allow("dd", &["if=/home/tester/.conductor/x", "of=/tmp/out"]);
}

#[test]
fn truncate_protected_vetoed() {
    veto("truncate", &["-s", "0", "~/.conductor/live.toml"]);
}

#[test]
fn truncate_size_value_not_treated_as_path() {
    // `-s 0` value must not be a target, and /tmp file is safe.
    allow("truncate", &["-s", "0", "/tmp/x"]);
}

#[test]
fn sed_inplace_protected_vetoed() {
    assert_eq!(
        veto("sed", &["-i", "s/a/b/", "~/.conductor/config.toml"]).matched_pattern,
        "sed -i"
    );
}

#[test]
fn sed_inplace_suffix_protected_vetoed() {
    veto("sed", &["-i.bak", "s/a/b/", "~/.conductor/config.toml"]);
}

#[test]
fn sed_without_inplace_allowed() {
    // No -i → sed reads the file and writes to stdout; not a persistence write.
    allow("sed", &["s/a/b/", "~/.conductor/config.toml"]);
}

#[test]
fn sed_inplace_on_safe_file_allowed() {
    allow("sed", &["-i", "s/a/b/", "/tmp/notes.txt"]);
}

// ───────────────────────── interpreter redirects ─────────────────────────

#[test]
fn sh_c_redirect_into_protected_vetoed() {
    let m = veto("sh", &["-c", "echo pwned > ~/.conductor/config.toml"]);
    assert_eq!(m.matched_pattern, "redirect >");
}

#[test]
fn bash_c_append_redirect_vetoed() {
    veto("bash", &["-c", "echo x >> ~/.conductor/audit.log"]);
}

#[test]
fn sh_c_read_redirect_allowed() {
    // `<` is a READ — an explicit must-not-veto case.
    allow("sh", &["-c", "cat < ~/.conductor/config.toml"]);
}

#[test]
fn sh_c_read_then_write_safe_allowed() {
    // Read protected, write /tmp → allowed.
    allow("sh", &["-c", "cat ~/.conductor/config.toml > /tmp/out"]);
}

#[test]
fn sh_c_fd_redirect_into_protected_vetoed() {
    // `2>` is still a write.
    veto("sh", &["-c", "do_thing 2> ~/.conductor/err.log"]);
}

#[test]
fn sh_c_ampersand_redirect_into_protected_vetoed() {
    veto("sh", &["-c", "do_thing &> ~/.conductor/all.log"]);
}

#[test]
fn sh_c_nested_tee_into_protected_vetoed() {
    // Write-command nested inside an interpreter pipeline.
    veto("sh", &["-c", "echo x | tee ~/.conductor/config.toml"]);
}

#[test]
fn sh_c_quoted_path_vetoed() {
    veto("sh", &["-c", "echo x > \"~/.conductor/config.toml\""]);
}

#[test]
fn sh_c_single_quoted_path_vetoed() {
    veto(
        "sh",
        &["-c", "echo x > '/home/tester/.conductor/config.toml'"],
    );
}

#[test]
fn sh_c_redirect_to_safe_allowed() {
    allow("sh", &["-c", "echo x > /tmp/out.txt"]);
}

#[test]
fn sh_c_clobber_redirect_vetoed() {
    veto("sh", &["-c", "echo x >| ~/.conductor/config.toml"]);
}

#[test]
fn sh_c_second_command_in_sequence_vetoed() {
    // Veto must survive a command separator.
    veto("sh", &["-c", "true; echo x > ~/.conductor/config.toml"]);
}

#[test]
fn sh_script_file_not_vetoed() {
    // `sh script.sh` reads the script; no -c, nothing to inspect/veto.
    allow("sh", &["/tmp/script.sh"]);
}

#[test]
fn zsh_c_redirect_vetoed() {
    veto("zsh", &["-c", "echo x > ~/.conductor/x"]);
}

// ───────────────────────── awk ─────────────────────────

#[test]
fn awk_redirect_into_protected_vetoed() {
    veto("awk", &["{ print $0 > \"/home/tester/.conductor/x\" }"]);
}

#[test]
fn awk_without_protected_redirect_allowed() {
    allow("awk", &["{ print $0 > \"/tmp/out\" }"]);
}

// ───── Regression: prior bypasses ─────

#[test]
fn cp_target_directory_flag_vetoed() {
    // `-t DIR` makes DIR the destination; sources are positional. The naive
    // last-operand check misses it.
    veto("cp", &["-t", "~/.conductor", "/tmp/evil"]);
}

#[test]
fn cp_target_directory_equals_vetoed() {
    veto("cp", &["--target-directory=~/.conductor", "/tmp/evil"]);
}

#[test]
fn cp_target_directory_long_space_vetoed() {
    veto("cp", &["--target-directory", "~/.conductor", "/tmp/evil"]);
}

#[test]
fn mv_target_directory_flag_vetoed() {
    veto("mv", &["-t", "/home/tester/.conductor", "/tmp/a", "/tmp/b"]);
}

#[test]
fn install_target_directory_vetoed() {
    veto("install", &["-t", "~/.conductor", "/tmp/x"]);
}

#[test]
fn cp_target_directory_safe_allowed() {
    allow("cp", &["-t", "/tmp/dest", "/home/tester/.conductor/src"]);
}

#[test]
fn sh_glued_c_flag_redirect_vetoed() {
    // `-c` glued to the script (`-cSCRIPT`) must still be lexed.
    veto("sh", &["-cecho x > ~/.conductor/config.toml"]);
}

#[test]
fn bash_login_c_flag_redirect_vetoed() {
    // `-lc` (login + command) — script is the following arg.
    veto("bash", &["-lc", "echo x > ~/.conductor/config.toml"]);
}

#[test]
fn sh_command_substitution_write_vetoed() {
    // A write-command hidden inside `$(...)`.
    veto("sh", &["-c", "echo $(tee ~/.conductor/config.toml)"]);
}

#[test]
fn sh_backtick_substitution_write_vetoed() {
    veto("sh", &["-c", "echo `tee ~/.conductor/config.toml`"]);
}

#[test]
fn sh_nested_command_substitution_redirect_vetoed() {
    // Redirect inside a command substitution.
    veto("sh", &["-c", "echo $(date > ~/.conductor/x)"]);
}

#[test]
fn sh_command_substitution_safe_allowed() {
    allow("sh", &["-c", "echo $(tee /tmp/out)"]);
}

// ───────────────────────── path normalisation ─────────────────────────

#[test]
fn dotdot_normalisation_still_vetoed() {
    // `~/.conductor/../.conductor/x` cleans to `~/.conductor/x`.
    veto("tee", &["/home/tester/.conductor/../.conductor/x"]);
}

#[test]
fn dot_segments_vetoed() {
    veto("tee", &["/home/tester/./.conductor/./x"]);
}

#[test]
fn tilde_only_expands_to_home() {
    // `~` alone is HOME, which is not itself a protected root.
    allow("tee", &["~"]);
}

#[test]
fn relative_path_not_vetoed() {
    // cwd unknown → relative paths cannot be resolved (documented gap).
    allow("tee", &[".conductor/x"]);
}

#[test]
fn full_program_path_basename_used() {
    // argv[0] may be an absolute path to the binary.
    veto("/usr/bin/tee", &["~/.conductor/x"]);
}

#[test]
fn symlinked_alias_of_protected_dir_vetoed() {
    // A symlink whose target IS the protected dir must still be caught.
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path();
    let conductor = home.join(".conductor");
    std::fs::create_dir_all(&conductor).expect("mkdir .conductor");
    let alias = home.join("alias");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&conductor, &alias).expect("symlink");
    #[cfg(not(unix))]
    return; // symlink semantics differ; covered on unix CI

    let prot = ProtectedPaths::for_test(home, None);
    let target = alias.join("config.toml");
    let r = persistence_write_veto("tee", &argv(&[target.to_str().unwrap()]), &prot, Some(home));
    assert!(r.is_some(), "write through symlink alias must be vetoed");
}

// ───────────────────────── benign commands ─────────────────────────

#[test]
fn benign_read_commands_allowed() {
    allow("cat", &["~/.conductor/config.toml"]);
    allow("ls", &["-la", "~/.conductor"]);
    allow("grep", &["foo", "~/.conductor/config.toml"]);
    allow("git", &["status"]);
}

#[test]
fn echo_with_redirect_token_argv_form_no_shell() {
    // Defensive case (d): a stray `>` token + protected path in argv.
    veto("echo", &["x", ">", "~/.conductor/config.toml"]);
}

// ───────────────────────── tokenizer direct ─────────────────────────

#[test]
fn tokenizer_classifies_read_vs_write() {
    let toks = tokenize("a > b < c >> d");
    let redirs: Vec<bool> = toks
        .iter()
        .filter_map(|t| match t {
            Tok::Redir { write } => Some(*write),
            _ => None,
        })
        .collect();
    assert_eq!(redirs, vec![true, false, true]);
}

#[test]
fn tokenizer_pipe_is_separator() {
    let toks = tokenize("a | b");
    assert!(toks.iter().any(|t| matches!(t, Tok::Sep)));
}
