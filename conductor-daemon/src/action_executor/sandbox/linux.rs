//! ADR-027 §D10b — Linux Landlock confinement (kernel 5.13+).
//!
//! We restrict **filesystem writes** to the declared subtrees plus the
//! always-safe temp paths; reads and exec stay unrestricted (Landlock only
//! denies access rights it is told to *handle*, so leaving read/exec
//! unhandled leaves them allowed). Network egress is restricted on kernels
//! with Landlock ABI ≥ 4 (6.7+); on older kernels the `network` toggle is a
//! no-op and we log that.
//!
//! Availability is probed in the parent (the `landlock_create_ruleset`
//! version query) so the refuse-vs-allow decision for
//! `allow_unsandboxed = false` happens *before* spawn. The ruleset is built in
//! the parent; only `restrict_self()` runs in the child `pre_exec` hook.

use super::{SandboxOutcome, SandboxPolicy, SandboxRefused};
use landlock::{
    ABI, Access, AccessFs, AccessNet, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset,
    RulesetAttr, RulesetCreatedAttr,
};
use std::os::unix::process::CommandExt;
use std::process::Command;

/// `landlock_create_ruleset(NULL, 0, LANDLOCK_CREATE_RULESET_VERSION)` returns
/// the kernel's supported ABI version (≥ 1) or `-1`/`ENOSYS` when Landlock is
/// absent or disabled. This is the canonical availability probe.
const LANDLOCK_CREATE_RULESET_VERSION: libc::c_int = 1;

/// Returns the supported Landlock ABI version, or `None` if Landlock is
/// unavailable (kernel < 5.13, or disabled at boot).
fn supported_abi() -> Option<i32> {
    // SAFETY: a pure query syscall — NULL attr + 0 size + the version flag.
    // It has no side effects and does not restrict the calling thread.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<libc::c_void>(),
            0_usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    (rc >= 1).then_some(rc as i32)
}

/// When the ruleset can't be built, honour the fail-closed policy: refuse if
/// `allow_unsandboxed` is false, otherwise spawn unconfined with a reason.
fn unsandboxed_or_refused(
    reason: String,
    allow_unsandboxed: bool,
) -> Result<SandboxOutcome, SandboxRefused> {
    if allow_unsandboxed {
        Ok(SandboxOutcome::Unsandboxed { reason })
    } else {
        Err(SandboxRefused { reason })
    }
}

pub(super) fn apply(
    command: &mut Command,
    policy: &SandboxPolicy,
    allow_unsandboxed: bool,
) -> Result<SandboxOutcome, SandboxRefused> {
    let Some(abi_version) = supported_abi() else {
        let reason = "Landlock unavailable (kernel < 5.13 or disabled)".to_string();
        return if allow_unsandboxed {
            Ok(SandboxOutcome::Unsandboxed { reason })
        } else {
            Err(SandboxRefused { reason })
        };
    };

    // (Council R1, finding 3) A network-deny the kernel can't enforce must NOT
    // silently fail open. Landlock network rights need ABI ≥ 4 (kernel 6.7+);
    // on older kernels, if the policy denies network and the operator forbade
    // unsandboxed execution, fail closed.
    let restrict_network = !policy.network;
    if restrict_network && abi_version < 4 {
        let reason = "Landlock network restriction needs ABI ≥ 4 (kernel 6.7+); \
                      cannot enforce network=false on this kernel"
            .to_string();
        if !allow_unsandboxed {
            return Err(SandboxRefused { reason });
        }
        tracing::warn!(
            landlock_abi = abi_version,
            "ADR-027 D10b: {reason}; filesystem confinement still applies, network egress is NOT confined"
        );
    }

    // (Council R1, finding 1) Handle the highest filesystem-access set the
    // crate knows for the kernel's ABI — `BestEffort` downgrades to what the
    // kernel actually supports — so newer rights (e.g. V3 file truncation) are
    // restricted, not just the V1 baseline. We handle only write-class access,
    // leaving reads/exec unrestricted.
    let abi = match abi_version {
        1 => ABI::V1,
        2 => ABI::V2,
        3 => ABI::V3,
        4 => ABI::V4,
        _ => ABI::V5,
    };
    // `from_write` is exactly the write-class rights — it deliberately EXCLUDES
    // `EXECUTE` (which Landlock groups with read, not write). Handling EXECUTE
    // here without granting exec rules would block the sandboxed child from
    // exec'ing its own target binary (Copilot review).
    let write_access = AccessFs::from_write(abi);

    let mut builder = match Ruleset::default()
        .set_compatibility(CompatLevel::BestEffort)
        .handle_access(write_access)
    {
        Ok(b) => b,
        Err(e) => {
            return unsandboxed_or_refused(
                format!("Landlock handle_access(fs) failed: {e}"),
                allow_unsandboxed,
            );
        }
    };

    // (finding 3) On ABI ≥ 4, enforce the network deny by HANDLING the network
    // access rights and adding NO network rules — Landlock then denies all TCP
    // bind/connect for the child.
    if restrict_network && abi_version >= 4 {
        builder = match builder.handle_access(AccessNet::from_all(abi)) {
            Ok(b) => b,
            Err(e) => {
                return unsandboxed_or_refused(
                    format!("Landlock handle_access(net) failed: {e}"),
                    allow_unsandboxed,
                );
            }
        };
    }

    let mut created = match builder.create() {
        Ok(c) => c,
        Err(e) => {
            return unsandboxed_or_refused(
                format!("Landlock ruleset creation failed: {e}"),
                allow_unsandboxed,
            );
        }
    };

    // Grant write on the always-safe temp paths plus the declared subtrees.
    // Paths that do not exist are skipped (best-effort) — Landlock cannot
    // grant access on a path it can't open.
    let mut allow: Vec<std::path::PathBuf> = vec![
        "/tmp".into(),
        "/var/tmp".into(),
        "/dev/null".into(),
        "/dev/zero".into(),
    ];
    allow.extend(policy.fs_write_allow.iter().cloned());

    for path in &allow {
        // Unopenable allow paths are skipped — Landlock can't grant access on
        // a path it can't open. Warn so the operator knows the declared path
        // is NOT being write-allowed (a stricter-than-intended confinement,
        // never a weaker one).
        let fd = match PathFd::new(path) {
            Ok(fd) => fd,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "ADR-027 D10b: cannot open declared sandbox write path; not granting write access to it"
                );
                continue;
            }
        };
        // `add_rule` consumes `created` and returns it on success; on failure
        // the ruleset is lost, so we can't keep going. With BestEffort +
        // a valid `PathFd` this shouldn't fail — treat it as "can't sandbox".
        match created.add_rule(PathBeneath::new(fd, write_access)) {
            Ok(c) => created = c,
            Err(e) => {
                let reason = format!("Landlock add_rule failed for {}: {e}", path.display());
                return if allow_unsandboxed {
                    Ok(SandboxOutcome::Unsandboxed { reason })
                } else {
                    Err(SandboxRefused { reason })
                };
            }
        }
    }

    // SAFETY: the closure runs in the child after `fork`, before `exec`.
    // `restrict_self()` performs the `prctl(PR_SET_NO_NEW_PRIVS)` +
    // `landlock_restrict_self` syscalls on the already-built ruleset fd
    // (created in the parent), so the child does no ruleset construction.
    //
    // `pre_exec` takes an `FnMut`, but `restrict_self()` consumes `self`. Park
    // the ruleset in an `Option` and `take()` it on the first (and only —
    // `execve` replaces the image) invocation.
    let mut created = Some(created);
    unsafe {
        command.pre_exec(move || {
            let ruleset = created
                .take()
                .ok_or_else(|| std::io::Error::other("Landlock ruleset already consumed"))?;
            ruleset.restrict_self().map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("Landlock restrict_self failed: {e}"),
                )
            })?;
            Ok(())
        });
    }

    Ok(SandboxOutcome::Sandboxed)
}
