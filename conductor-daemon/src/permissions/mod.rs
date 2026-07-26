// Copyright 2026 Amiable
// SPDX-License-Identifier: MIT

//! ADR-029 Phase 3 (D3) — runtime TCC permission detection.
//!
//! macOS keys Input Monitoring (and other TCC categories) on a binary's
//! code identity. This module provides:
//!
//! - [`PermissionStatus`] — uniform result type so the CLI doesn't have
//!   to special-case each platform.
//! - [`check_input_monitoring`] — best-effort detection of the daemon's
//!   Input Monitoring grant. On macOS, attempts a quick gilrs-style
//!   probe and classifies the result. On other platforms, returns
//!   [`PermissionStatus::NotApplicable`] because there's no equivalent
//!   consent gate.
//!
//! Detection on macOS is best-effort: macOS does not expose a public
//! API to query "does process X hold permission Y" without elevated
//! privileges. We infer the grant by attempting the operation that
//! would be gated and observing the outcome.

use std::fmt;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// PR #997 round-12 review: Apple's `x-apple.systempreferences:`
/// URL for the Input Monitoring pane, exported here so both the
/// daemon's `open` invocation and the GUI's `commands.rs` deep-link
/// reference one source of truth. If macOS ever renames this anchor
/// across versions, this is the only string to update.
pub const INPUT_MONITORING_DEEPLINK: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent";

/// Outcome of a permission check.
///
/// `Granted` and `NotGranted` are reliable on macOS when the probe
/// succeeds. `Unknown` covers transient errors (probe failed for a
/// reason unrelated to permissions). `NotApplicable` is the response
/// on platforms with no equivalent consent gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionStatus {
    Granted,
    NotGranted,
    Unknown(String),
    NotApplicable,
}

impl fmt::Display for PermissionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Granted => write!(f, "GRANTED"),
            Self::NotGranted => write!(f, "NOT GRANTED"),
            Self::Unknown(reason) => write!(f, "UNKNOWN ({})", reason),
            Self::NotApplicable => write!(f, "n/a — no consent gate on this platform"),
        }
    }
}

/// In-process cache TTL for the gilrs probe. Each probe shells out
/// to `IOHIDManagerCreate` via `gilrs::Gilrs::new()`, which costs
/// ~50–100 ms on a warm machine. Caching for 30 s means back-to-back
/// `conductorctl permissions --check` calls (and the GUI's repeated
/// IPC probes) don't re-init the OS-side enumerator. ADR-029 spec
/// §4.2 documents this as a probe-cache requirement.
const PROBE_CACHE_TTL: Duration = Duration::from_secs(30);

static PROBE_CACHE: Mutex<Option<(Instant, PermissionStatus)>> = Mutex::new(None);

/// Probe the daemon binary's Input Monitoring TCC grant.
///
/// On macOS, attempts a gilrs-style enumeration and infers grant from
/// the outcome. On Linux / Windows, returns [`PermissionStatus::NotApplicable`].
///
/// Results are cached for 30 s in process memory so back-to-back
/// callers (e.g. the GUI polling + a concurrent `conductorctl
/// permissions --check`) don't pay the gilrs-init cost repeatedly.
/// Mutation paths (e.g. user grants permission and reopens the GUI)
/// can call [`invalidate_input_monitoring_cache`] to force the next
/// call to re-probe.
///
/// Best-effort by design. Callers should treat the result as
/// diagnostic output, not as authoritative auth state — the only
/// authoritative answer is what System Settings displays.
pub fn check_input_monitoring() -> PermissionStatus {
    if let Ok(guard) = PROBE_CACHE.lock()
        && let Some((when, status)) = guard.as_ref()
        && when.elapsed() < PROBE_CACHE_TTL
    {
        return status.clone();
    }

    let status = probe_input_monitoring_uncached();

    if let Ok(mut guard) = PROBE_CACHE.lock() {
        *guard = Some((Instant::now(), status.clone()));
    }
    status
}

/// Invalidate the probe cache so the next [`check_input_monitoring`]
/// call re-runs the gilrs probe. Useful after the user is likely to
/// have changed their TCC grant (e.g. clicked "Open System
/// Settings" — the next tick should reflect their new state without
/// waiting for the 30 s TTL).
pub fn invalidate_input_monitoring_cache() {
    if let Ok(mut guard) = PROBE_CACHE.lock() {
        *guard = None;
    }
}

fn probe_input_monitoring_uncached() -> PermissionStatus {
    #[cfg(target_os = "macos")]
    {
        macos::probe_input_monitoring()
    }
    #[cfg(not(target_os = "macos"))]
    {
        PermissionStatus::NotApplicable
    }
}

/// Outcome of an attempt to open the platform-specific Input Monitoring
/// settings UI.
///
/// Library returns this as a structured value rather than printing
/// directly, so the CLI layer can choose between human-readable and
/// `--json` output without the library leaking text to stderr.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenSettingsOutcome {
    /// macOS: System Settings was successfully spawned and exited 0.
    Opened,
    /// No equivalent consent-gate UI exists on this platform.
    NotApplicable,
}

/// Open System Settings to the Input Monitoring pane on macOS, or
/// report that no equivalent action is available on the current
/// platform.
///
/// Returns `Ok(Opened)` only if the macOS `open` command exits
/// successfully. On Linux / Windows returns `Ok(NotApplicable)`
/// without producing any output — printing is the caller's job.
pub fn open_input_monitoring_settings() -> std::io::Result<OpenSettingsOutcome> {
    #[cfg(target_os = "macos")]
    {
        macos::open_input_monitoring_pane()?;
        Ok(OpenSettingsOutcome::Opened)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(OpenSettingsOutcome::NotApplicable)
    }
}

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(test)]
mod tests {
    use super::*;

    /// PR #997 round-10 review: both tests below mutate the global
    /// `PROBE_CACHE` static, so they MUST run serially. cargo test
    /// (and nextest) parallelises by default, so without this lock
    /// one test's `invalidate` could run between another's
    /// "check_input_monitoring populated the cache" assertion and
    /// the populate, causing a flaky failure that's painful to
    /// reproduce. A test-local Mutex<()> held across each test body
    /// serialises only this module's tests.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn lock_for_test() -> std::sync::MutexGuard<'static, ()> {
        // Recover from poisoning so a previous panicking test
        // doesn't leak a poisoned guard to the next one.
        match TEST_LOCK.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    #[test]
    fn invalidate_input_monitoring_cache_clears_stored_value() {
        let _g = lock_for_test();
        // Seed the cache with a known value and confirm invalidate
        // wipes it. We can't easily test the elapsed-30s path without
        // controlling time, but we can prove invalidate works (which
        // is the path the GUI's "Open System Settings" click hits).
        {
            let mut guard = PROBE_CACHE
                .lock()
                .expect("PROBE_CACHE mutex poisoned by another test");
            *guard = Some((Instant::now(), PermissionStatus::Granted));
        }

        invalidate_input_monitoring_cache();

        let guard = PROBE_CACHE
            .lock()
            .expect("PROBE_CACHE mutex poisoned by another test");
        assert!(
            guard.is_none(),
            "expected cache to be empty after invalidate, got {:?}",
            *guard,
        );
    }

    #[test]
    fn check_input_monitoring_populates_cache() {
        let _g = lock_for_test();
        // Reset to a known empty state, then call check and confirm
        // the cache gained an entry. The actual probe outcome varies
        // by platform (NotApplicable on Linux, real probe on macOS),
        // so we only assert the cache was populated, not its value.
        invalidate_input_monitoring_cache();
        let _ = check_input_monitoring();
        let guard = PROBE_CACHE
            .lock()
            .expect("PROBE_CACHE mutex poisoned by another test");
        assert!(
            guard.is_some(),
            "check_input_monitoring should have populated the cache",
        );
    }
}
