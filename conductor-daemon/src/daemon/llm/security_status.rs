// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! `conductor_security_status` MCP tool payload (ADR-042 Phase B-early
//! visibility).
//!
//! Builds the JSON the ReadOnly `conductor_security_status` tool returns: the
//! network-approval HMAC key's rotation status. Mirrors the
//! `conductorctl security status --json` schema so the CLI and MCP surfaces
//! agree:
//!
//! ```json
//! { "hmac_key_fingerprint": "…", "hmac_key_age_days": 42, "hmac_key_warning": "ok" }
//! ```
//!
//! `hmac_key_warning` is one of `ok` / `consider_rotation` / `should_rotate` /
//! `approaching_expiry` / `deprecated` / `hard_expired` / `unavailable`.
//!
//! **Report-only.** The tool never refuses, even for a hard-expired key — an
//! operator (or the LLM) must be able to *see* why a key is blocking daemon
//! startup. A missing key, backend error, read timeout, or non-Unix platform
//! all degrade to a structured `"unavailable"` payload (null fingerprint/age +
//! a `detail` string), never an executor error.
//!
//! **Bounded.** The keychain read is a synchronous, potentially blocking OS
//! call — on macOS the apple-native keyring read can surface a keychain-access
//! prompt — so [`payload`] runs it on a blocking thread under
//! [`READ_TIMEOUT`]. A wedged/prompting backend degrades to `"unavailable"`
//! rather than hanging the async executor.

use serde_json::{Value, json};
#[cfg(unix)]
use std::sync::{Arc, LazyLock};
#[cfg(unix)]
use std::time::Duration;
#[cfg(unix)]
use tokio::sync::Semaphore;

/// Upper bound for the blocking keychain read. A report-only status probe must
/// return promptly; the timeout converts a wedged or access-prompting backend
/// into an `"unavailable"` result instead of a hang.
#[cfg(unix)]
const READ_TIMEOUT: Duration = Duration::from_secs(2);

/// Single-flight guard for the keychain read. The read is a synchronous OS call
/// that [`payload`] bounds with a timeout, but a timeout can only *abandon* the
/// blocking task — it cannot cancel the underlying FFI call, so the worker
/// thread lingers until the OS returns. Without a guard, a backend that hangs
/// (e.g. an unanswered macOS keychain-access prompt) plus repeated calls would
/// leak one `spawn_blocking` thread per call and could exhaust the pool. The
/// permit is **moved into the blocking task** so it is released only when that
/// task actually finishes; a concurrent call then finds no permit and returns a
/// "busy" payload instead of spawning another read. Net effect: at most one
/// in-flight (and at most one potentially-leaked) keychain read at a time.
#[cfg(unix)]
static READ_GUARD: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(1)));

/// Build the `"unavailable"` payload — used when no key exists, the backend
/// errors, the read times out, or the platform is non-Unix. Null
/// fingerprint/age plus a `detail` string, mirroring the unavailable branch of
/// `conductorctl security status --json`.
///
/// `detail` is operator-/LLM-facing and MUST be a fixed, non-sensitive string.
/// The `&'static str` bound enforces this at the type level: a raw backend error
/// (which can carry filesystem paths or keychain internals) is `String`, not
/// `&'static str`, so it cannot be passed here — it must be logged server-side
/// instead. This makes the no-leak invariant compiler-checked, not conventional.
pub fn unavailable(detail: &'static str) -> Value {
    json!({
        "hmac_key_fingerprint": Value::Null,
        "hmac_key_age_days": Value::Null,
        "hmac_key_warning": "unavailable",
        "detail": detail,
    })
}

/// Build the populated payload from a rotation status.
///
/// `hmac_key_warning` is mapped **exhaustively** from the rotation level — no
/// catch-all fallback — so `"ok"` can only ever mean [`RotationLevel::Ok`]
/// (never an unclassified state: no fail-open) and every emitted value is a
/// documented schema member (no surprise value to break downstream parsing). A
/// future `RotationLevel` variant makes this a compile error, forcing a
/// deliberate wire-string choice rather than silently degrading.
///
/// The fingerprint is the deliberately non-secret 16-char hex key fingerprint
/// (`SHA-256` digest prefix, not attacker-controlled free text) and the age is
/// non-sensitive, so both are safe to expose over the LLM surface.
#[cfg(unix)]
pub fn ok_json(status: &crate::security::KeyRotationStatus) -> Value {
    use crate::security::RotationLevel;
    let warning = match status.level {
        RotationLevel::Ok => "ok",
        RotationLevel::ConsiderRotation => "consider_rotation",
        RotationLevel::ShouldRotate => "should_rotate",
        RotationLevel::ApproachingExpiry => "approaching_expiry",
        RotationLevel::Deprecated => "deprecated",
        RotationLevel::HardExpired => "hard_expired",
    };
    json!({
        "hmac_key_fingerprint": status.fingerprint,
        "hmac_key_age_days": status.age_days,
        "hmac_key_warning": warning,
    })
}

/// Read the network-approval HMAC key's rotation status and render the tool
/// payload. Runs the blocking keychain read off the async worker under
/// [`READ_GUARD`] (single-flight) and bounded by [`READ_TIMEOUT`]; any failure
/// mode degrades to [`unavailable`] with a fixed, non-sensitive detail string
/// (the real error is logged server-side, never returned to the LLM).
#[cfg(unix)]
pub async fn payload() -> Value {
    // Single-flight: if a read is already in progress (possibly a leaked,
    // hung one), short-circuit rather than spawning another blocking task.
    let Ok(permit) = READ_GUARD.clone().try_acquire_owned() else {
        return unavailable("a keychain read is already in progress; try again shortly");
    };
    let read = tokio::task::spawn_blocking(move || {
        // Hold the permit until the (possibly slow) OS read actually returns,
        // so a hung read keeps the slot occupied and bounds leaked threads to 1.
        let _permit = permit;
        crate::security::key_rotation_status_default()
    });
    match tokio::time::timeout(READ_TIMEOUT, read).await {
        Ok(Ok(Ok(status))) => ok_json(&status),
        Ok(Ok(Err(e))) => {
            tracing::warn!(error = %e, "conductor_security_status: keychain read failed");
            unavailable(
                "no network-approval HMAC key is initialised, or the keychain backend is unavailable",
            )
        }
        Ok(Err(join_err)) => {
            tracing::error!(error = %join_err, "conductor_security_status: keychain read task panicked");
            unavailable("the keychain status read failed unexpectedly")
        }
        Err(_elapsed) => {
            tracing::warn!("conductor_security_status: keychain read timed out");
            unavailable("keychain read timed out — backend unavailable or awaiting access approval")
        }
    }
}

/// Non-Unix fallback: the network-approval keychain is Unix-only (it relies on
/// hardened `O_NOFOLLOW`/`fstat` file APIs), so the status is always
/// `"unavailable"`. Keeps the tool callable on every platform.
#[cfg(not(unix))]
pub async fn payload() -> Value {
    unavailable("network-approval keychain is only available on Unix platforms")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn ok_json_reports_warning_tag() {
        use crate::security::{KeyRotationStatus, RotationLevel};
        let status = KeyRotationStatus {
            fingerprint: "abcd1234".to_string(),
            age_days: 400,
            level: RotationLevel::Deprecated,
        };
        let json = ok_json(&status);
        assert_eq!(json["hmac_key_fingerprint"], "abcd1234");
        assert_eq!(json["hmac_key_age_days"], 400);
        assert_eq!(json["hmac_key_warning"], "deprecated");
    }

    #[cfg(unix)]
    #[test]
    fn ok_json_healthy_reports_ok_not_null() {
        use crate::security::{KeyRotationStatus, RotationLevel};
        let status = KeyRotationStatus {
            fingerprint: "feedface".to_string(),
            age_days: 5,
            level: RotationLevel::Ok,
        };
        let json = ok_json(&status);
        // Stable schema: a healthy key reports "ok", never null.
        assert_eq!(json["hmac_key_warning"], "ok");
        assert!(!json["hmac_key_warning"].is_null());
    }

    #[test]
    fn unavailable_shape() {
        // No key / backend error / timeout / non-Unix all share this shape:
        // null fingerprint + age, "unavailable" warning, and a detail string.
        let json = unavailable("backend down");
        assert!(json["hmac_key_fingerprint"].is_null());
        assert!(json["hmac_key_age_days"].is_null());
        assert_eq!(json["hmac_key_warning"], "unavailable");
        assert_eq!(json["detail"], "backend down");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn payload_is_single_flight_when_a_read_holds_the_permit() {
        // Hold the sole permit; a concurrent call must short-circuit to a
        // "busy" unavailable payload WITHOUT spawning a keychain read (this is
        // what bounds leaked blocking threads to one). Deterministic: the guard
        // check precedes any keychain access.
        let _held = READ_GUARD
            .clone()
            .try_acquire_owned()
            .expect("first permit available");
        let json = payload().await;
        assert_eq!(json["hmac_key_warning"], "unavailable");
        assert!(
            json["detail"].as_str().unwrap().contains("in progress"),
            "expected a busy detail, got {}",
            json["detail"]
        );
    }

    // NOTE: no test exercises the live `payload()` against the real keychain.
    // The read can block on a macOS keychain-access prompt; even bounded by
    // `READ_TIMEOUT`, the leaked `spawn_blocking` thread would stall the
    // `#[tokio::test]` runtime at drop. The schema is fully covered by the pure
    // builders above; the timeout/error degradation is simple and branch-obvious.
}
