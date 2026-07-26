//! ADR-027 §D10b — sandbox module tests.
//!
//! Policy/path tests run on every platform. The Seatbelt enforcement tests
//! run only on macOS (they exercise the real `sandbox_init` profile).

use super::*;
use conductor_core::config::types::ShellSandboxConfig;

#[test]
fn policy_from_none_is_default_deny() {
    let p = SandboxPolicy::from_config(None);
    assert!(p.fs_write_allow.is_empty());
    assert!(!p.network, "default denies network");
}

#[test]
fn policy_from_config_carries_network_and_absolute_writes() {
    let cfg = ShellSandboxConfig {
        fs_write: vec!["/var/work".to_string(), "relative/dropped".to_string()],
        network: true,
    };
    let p = SandboxPolicy::from_config(Some(&cfg));
    assert!(p.network);
    // Absolute path kept; relative path dropped (meaningless for an OS profile).
    assert_eq!(
        p.fs_write_allow,
        vec![std::path::PathBuf::from("/var/work")]
    );
}

#[test]
fn expand_path_expands_tilde_and_drops_relative() {
    // Read $HOME rather than mutating it — process-wide env mutation races with
    // parallel tests (Copilot review). Assert against whatever HOME is set to.
    if let Some(home) = std::env::var_os("HOME") {
        let home = std::path::PathBuf::from(home);
        if home.is_absolute() {
            assert_eq!(expand_path("~/work"), Some(home.join("work")));
            assert_eq!(expand_path("~"), Some(home));
        }
    }
    assert_eq!(
        expand_path("/abs/path"),
        Some(std::path::PathBuf::from("/abs/path"))
    );
    assert_eq!(expand_path("relative/path"), None);
}

// ── macOS Seatbelt profile + enforcement ──────────────────────────────────

#[cfg(target_os = "macos")]
mod macos_profile {
    use super::*;

    #[test]
    fn default_profile_denies_writes_and_network() {
        let profile = super::super::macos::build_profile(&SandboxPolicy::default());
        assert!(profile.contains("(deny file-write*)"));
        assert!(profile.contains("(deny network*)"));
        // The always-safe write targets are still allowed.
        assert!(profile.contains("/private/tmp"));
    }

    #[test]
    fn network_true_omits_network_deny() {
        let p = SandboxPolicy {
            fs_write_allow: vec![],
            network: true,
        };
        let profile = super::super::macos::build_profile(&p);
        assert!(!profile.contains("(deny network*)"));
    }

    #[test]
    fn declared_write_path_appears_as_subpath() {
        let p = SandboxPolicy {
            fs_write_allow: vec![std::path::PathBuf::from("/Users/x/work")],
            network: false,
        };
        let profile = super::super::macos::build_profile(&p);
        assert!(profile.contains("(subpath \"/Users/x/work\")"));
    }

    #[test]
    fn profile_drops_control_char_paths() {
        // (Council R1, finding 2) A path with a newline trying to inject an
        // `(allow default)` directive must be dropped, not emitted.
        let p = SandboxPolicy {
            fs_write_allow: vec![
                std::path::PathBuf::from("/x/ok"),
                std::path::PathBuf::from("/evil\n(allow default)"),
            ],
            network: false,
        };
        let profile = super::super::macos::build_profile(&p);
        assert!(profile.contains("(subpath \"/x/ok\")"), "safe path kept");
        // The only `(allow default)` is the legitimate header — the injected
        // one was dropped with its control-char-bearing path.
        assert_eq!(
            profile.matches("(allow default)").count(),
            1,
            "control-char path must be dropped, not injected as a directive"
        );
    }

    #[test]
    fn apply_to_command_installs_sandbox() {
        let mut cmd = std::process::Command::new("/usr/bin/true");
        let out = apply_to_command(&mut cmd, &SandboxPolicy::default(), true)
            .expect("macOS always sandboxes");
        assert_eq!(out, SandboxOutcome::Sandboxed);
    }

    /// Real enforcement: a write OUTSIDE the allowed set must fail, while a
    /// write to an allowed subtree (a temp dir) must succeed.
    #[test]
    fn seatbelt_blocks_disallowed_write_allows_allowed_write() {
        let allowed_dir = tempfile::tempdir().unwrap();
        let allowed = allowed_dir.path().to_path_buf();
        let policy = SandboxPolicy {
            fs_write_allow: vec![allowed.clone()],
            network: false,
        };

        // Allowed: write inside the granted subtree → success.
        let ok_target = allowed.join("ok.txt");
        let mut ok = std::process::Command::new("/usr/bin/touch");
        ok.arg(&ok_target);
        apply_to_command(&mut ok, &policy, true).unwrap();
        let status = ok.status().expect("spawn touch (allowed)");
        assert!(status.success(), "write to allowed subtree should succeed");
        assert!(ok_target.exists());

        // Denied: a normally-writable path under $HOME that is NOT in the
        // allow-list and NOT one of the always-safe temp paths (/private/tmp,
        // /private/var/folders — where `tempfile` lives, so a temp dir would be
        // wrongly allowed). $HOME is writable for the test user, so a failure
        // here proves Seatbelt confinement, not ambient permissions (Copilot:
        // /etc would fail for non-root even unsandboxed).
        if let Some(home) = std::env::var_os("HOME") {
            // Unique per-run name so concurrent runs / crashed-run litter can't
            // collide (Copilot review).
            let denied_target = std::path::PathBuf::from(home).join(format!(
                "conductor_d10b_denied_probe_{}",
                std::process::id()
            ));
            let _ = std::fs::remove_file(&denied_target); // pre-clean
            let mut denied = std::process::Command::new("/usr/bin/touch");
            denied.arg(&denied_target);
            apply_to_command(&mut denied, &policy, true).unwrap();
            let status = denied.status().expect("spawn touch (denied)");
            let created = denied_target.exists();
            let _ = std::fs::remove_file(&denied_target); // clean up if sandbox failed to block
            assert!(
                !status.success(),
                "write under $HOME (outside the allow set) must be blocked by Seatbelt"
            );
            assert!(!created, "blocked write must not have created the file");
        }
    }
}

// ── Unsupported-platform fail-closed semantics ────────────────────────────
//
// On platforms with no OS sandbox (Windows, etc.), allow_unsandboxed drives
// refuse-vs-allow. We can only exercise this branch where it is compiled.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod unsupported {
    use super::*;

    #[test]
    fn refuses_when_unsandboxed_disallowed() {
        let mut cmd = std::process::Command::new("true");
        let res = apply_to_command(&mut cmd, &SandboxPolicy::default(), false);
        assert!(res.is_err());
    }

    #[test]
    fn allows_when_unsandboxed_permitted() {
        let mut cmd = std::process::Command::new("true");
        let res = apply_to_command(&mut cmd, &SandboxPolicy::default(), true);
        assert!(matches!(res, Ok(SandboxOutcome::Unsandboxed { .. })));
    }
}
