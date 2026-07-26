//! ADR-027 §D10b — macOS Seatbelt confinement via the `sandbox_init(3)` C API.
//!
//! We generate an SBPL profile from the [`SandboxPolicy`] and apply it in the
//! child between `fork` and `exec` (a `pre_exec` hook). `sandbox_init` with
//! `flags == 0` compiles and applies the supplied SBPL string. We deliberately
//! bypass the deprecated `sandbox-exec` CLI per the Council D10b review — the
//! Seatbelt profile language is stable even though the CLI is not.

use super::{SandboxOutcome, SandboxPolicy, SandboxRefused};
use std::ffi::CString;
use std::os::unix::process::CommandExt;
use std::process::Command;

// `libSystem` exports these (declared in `<sandbox.h>`). `sandbox_init`
// returns 0 on success; on failure it returns non-zero and writes a
// heap-allocated error string into `*errorbuf` (freed via
// `sandbox_free_error`).
unsafe extern "C" {
    fn sandbox_init(
        profile: *const libc::c_char,
        flags: u64,
        errorbuf: *mut *mut libc::c_char,
    ) -> libc::c_int;
    fn sandbox_free_error(errorbuf: *mut libc::c_char);
}

pub(super) fn apply(
    command: &mut Command,
    policy: &SandboxPolicy,
) -> Result<SandboxOutcome, SandboxRefused> {
    let profile = build_profile(policy);
    // A NUL in the profile would truncate it; reject rather than silently
    // weaken confinement.
    let profile_c = CString::new(profile).map_err(|_| SandboxRefused {
        reason: "sandbox profile contained an interior NUL byte".to_string(),
    })?;

    // SAFETY: the closure runs in the child after `fork`, before `exec`. It
    // must be async-signal-safe: it makes a single FFI call into
    // `sandbox_init` using a `CString` that was fully built in the parent
    // (no allocation in the child) and performs no other work on success.
    unsafe {
        command.pre_exec(move || {
            let mut errbuf: *mut libc::c_char = std::ptr::null_mut();
            // flags == 0 ⇒ `profile_c` is an SBPL profile string to compile
            // and apply (not a named built-in profile).
            let rc = sandbox_init(profile_c.as_ptr(), 0, &mut errbuf);
            if rc != 0 {
                if !errbuf.is_null() {
                    sandbox_free_error(errbuf);
                }
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "sandbox_init failed for shell action",
                ));
            }
            Ok(())
        });
    }
    Ok(SandboxOutcome::Sandboxed)
}

/// Build the SBPL profile for `policy`.
///
/// Strategy: `(allow default)` then carve back the two confinement
/// dimensions — deny all writes except the declared subtrees plus the
/// always-safe temp / `/dev/null` paths, and deny network unless opted in.
pub(super) fn build_profile(policy: &SandboxPolicy) -> String {
    let mut p = String::from("(version 1)\n(allow default)\n");

    // Filesystem writes: deny, then re-allow the safe + declared subtrees.
    p.push_str("(deny file-write*)\n");
    p.push_str("(allow file-write*\n");
    // Always-safe write targets so ordinary tools keep working.
    p.push_str("  (literal \"/dev/null\")\n");
    p.push_str("  (subpath \"/private/tmp\")\n");
    p.push_str("  (subpath \"/private/var/folders\")\n"); // macOS per-user temp
    for path in &policy.fs_write_allow {
        let raw = path.to_string_lossy();
        // (Council R1, finding 2) Defence-in-depth against SBPL injection: a
        // path containing control characters (newline, NUL, …) can't appear in
        // a legitimate filesystem path and is the only way the `\`/`"` escaping
        // below could be subverted — drop it rather than emit it into the
        // profile. A dropped path is a STRICTER profile, never a weaker one.
        if raw.chars().any(|c| c.is_control()) {
            continue;
        }
        p.push_str("  (subpath ");
        p.push_str(&sbpl_quote(&raw));
        p.push_str(")\n");
    }
    p.push_str(")\n");

    // Network egress.
    if !policy.network {
        p.push_str("(deny network*)\n");
    }

    p
}

/// Quote a string as an SBPL literal, escaping `\` and `"`.
fn sbpl_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '\\' || c == '"' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}
