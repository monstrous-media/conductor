// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-040 §4.3 — focused-window-title detection.
//!
//! Window TITLE is a **real OS permission escalation** beyond the app-name
//! detection in [`crate::daemon::app_detector`] — on macOS it needs
//! Accessibility (AX) permission. So the whole subsystem is **lazy**: the AX
//! APIs are invoked **only when `[per_app_modes].window_rules` are present** in
//! the loaded config ([`title_polling_enabled`]). A config with no window rules
//! never touches Accessibility at all.
//!
//! Note: the macOS leaf checks `AXIsProcessTrusted` but does **not**
//! pop the system permission dialog (no `AXIsProcessTrustedWithOptions{prompt}`)
//! — a background daemon surprising the user with a dialog is worse UX than the
//! observable-degradation path below. Permission is granted out-of-band (System
//! Settings → Privacy & Security → Accessibility); until then reads fail and the
//! subsystem degrades observably.
//!
//! When window rules *are* configured but the read fails (permission ungranted,
//! or an unsupported platform), the failure is **observable, not silent**: a
//! [`DegradationTracker`] warns **once** and flips a status flag
//! (`window_permission_degraded`), and resolution falls back to app-name rules.
//!
//! Privacy (§4.1/§4.3): a daemon reading every window title is an MDM/privacy
//! risk, so titles are **masked** in logs (`<title:len=N>`) unless the operator
//! opts in with `[per_app_modes].log_titles = true` ([`mask_title`]).
//!
//! Testability: every read goes through the [`WindowTitleSource`] trait, so the
//! lazy/degradation/masking/clamp logic is unit-tested with a mock source; the
//! unsafe macOS AX FFI is a thin `#[cfg(target_os = "macos")]` leaf
//! ([`macos::AxWindowTitleSource`]) mirroring `app_detector::detect_frontmost_app`.

use conductor_core::config::types::{MIN_WINDOW_TITLE_POLL_MS, PerAppModes};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::warn;

/// Outcome of reading the focused window's title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TitleRead {
    /// Read succeeded. `None` = there is a focused window but no title (or no
    /// focused window) — distinct from a permission/platform failure.
    Ok(Option<String>),
    /// The OS denied the read — AX/Accessibility permission is not granted.
    /// Observable degradation applies (warn-once + status flag + app-rule
    /// fallback), NOT a silent skip.
    PermissionDenied,
    /// This platform can't read window titles (no backend wired). Degrades the
    /// same way as `PermissionDenied`.
    Unsupported,
}

/// A source of the focused-window title. Trait-bound so the poller logic is
/// testable with a mock; the real macOS implementation is the AX FFI leaf.
pub trait WindowTitleSource: Send + Sync {
    /// Read the focused window's title (or report why it couldn't).
    fn focused_window_title(&self) -> TitleRead;
}

/// Whether the title poller should run at all (§4.3 lazy evaluation): only when
/// at least one `[per_app_modes].window_rules` entry is declared. Returning
/// `false` here is what prevents the OS permission prompt for users who don't
/// use window rules.
pub fn title_polling_enabled(per_app_modes: Option<&PerAppModes>) -> bool {
    per_app_modes.is_some_and(|pam| !pam.window_rules.is_empty())
}

/// Clamp a configured poll interval up to the safe floor
/// ([`MIN_WINDOW_TITLE_POLL_MS`], 100ms) so a tiny/zero value can't spin the
/// Accessibility API (§4.3 "safe floor 100ms").
pub fn clamp_poll_ms(configured: u64) -> u64 {
    configured.max(MIN_WINDOW_TITLE_POLL_MS)
}

/// Mask a title for logging unless `log_titles` is set (§4.1/§4.3 privacy).
/// `Some("Untitled")` → `<title:len=8>`; `None` → `<no-title>`. With
/// `log_titles = true` the raw title is shown **`Debug`-quoted** (so embedded
/// quotes/newlines are escaped — a window title is attacker-influenced text, and
/// naive quoting would allow log-injection / ambiguous lines), `None` →
/// `<no-title>`.
///
/// Length is the Unicode scalar count (`chars().count()`), not bytes, so the
/// masked form doesn't leak the script/encoding of the title.
pub fn mask_title(title: Option<&str>, log_titles: bool) -> String {
    match title {
        None => "<no-title>".to_string(),
        // `{:?}` on a &str quotes AND escapes (control chars, quotes, newlines).
        Some(t) if log_titles => format!("{t:?}"),
        Some(t) => format!("<title:len={}>", t.chars().count()),
    }
}

/// Tracks observable degradation for the title subsystem (§4.3 P0 #4): warn
/// **once** (not every poll) when reads fail, and expose a shared `degraded`
/// flag for `conductor_mode_status` / GUI. Cleared when a read later succeeds,
/// so a granted-after-prompt permission flips the status back.
///
/// Concurrency: [`record`](Self::record) is **single-writer** — only the app
/// detector's one poll task calls it, so the two-flag update is never racing a
/// concurrent `record`. The only cross-thread access is the status surface
/// reading [`flag`](Self::flag)/[`is_degraded`](Self::is_degraded); a `Relaxed`
/// read of a `bool` status flag is intentionally fine (worst case it observes a
/// one-poll-stale value — eventual consistency, no torn read on a `bool`). No
/// stronger ordering or lock is warranted.
#[derive(Clone)]
pub struct DegradationTracker {
    /// Shared with the status surface (read by `mode_status_json`).
    degraded: Arc<AtomicBool>,
    /// Guards the one-shot warning so a failing read every `poll_ms` doesn't
    /// flood the log; reset on the next success.
    warned: Arc<AtomicBool>,
}

impl DegradationTracker {
    pub fn new() -> Self {
        Self {
            degraded: Arc::new(AtomicBool::new(false)),
            warned: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The shared degraded flag, for the status surface to read.
    pub fn flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.degraded)
    }

    /// Whether the subsystem is currently degraded (window rules configured but
    /// titles unreadable).
    pub fn is_degraded(&self) -> bool {
        self.degraded.load(Ordering::Relaxed)
    }

    /// Record a title read. Returns the title to use for resolution (`Some`/`None`
    /// on success, always `None` on a degraded read so app-name rules apply).
    /// Sets/clears the degraded flag and emits the one-shot warning as needed.
    pub fn record(&self, read: TitleRead) -> Option<String> {
        match read {
            TitleRead::Ok(title) => {
                // Recovered (or never degraded): clear the flag and re-arm the
                // one-shot warning for any future failure.
                self.degraded.store(false, Ordering::Relaxed);
                self.warned.store(false, Ordering::Relaxed);
                title
            }
            TitleRead::PermissionDenied | TitleRead::Unsupported => {
                self.degraded.store(true, Ordering::Relaxed);
                // Warn exactly once per degradation episode.
                if !self.warned.swap(true, Ordering::Relaxed) {
                    warn!(
                        "Window-title detection degraded: focused-window title \
                         unreadable (Accessibility permission ungranted or \
                         unsupported platform). Falling back to app-name rules; \
                         window_rules will not match until resolved. Status: \
                         window_permission_degraded = true."
                    );
                }
                None
            }
        }
    }
}

impl Default for DegradationTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Construct the platform's title source. macOS reads via the Accessibility API;
/// other platforms report `Unsupported` until a backend is wired (§4.3
/// Linux/Windows note).
pub fn default_window_title_source() -> Box<dyn WindowTitleSource> {
    #[cfg(target_os = "macos")]
    {
        Box::new(macos::AxWindowTitleSource)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Box::new(UnsupportedWindowTitleSource)
    }
}

/// A source that always reports `Unsupported` — the non-macOS default and a
/// test fixture. Degrades observably rather than silently doing nothing.
pub struct UnsupportedWindowTitleSource;

impl WindowTitleSource for UnsupportedWindowTitleSource {
    fn focused_window_title(&self) -> TitleRead {
        TitleRead::Unsupported
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conductor_core::config::types::WindowRule;

    fn pam_with_rules(rules: Vec<WindowRule>) -> PerAppModes {
        PerAppModes {
            default: None,
            rules: std::collections::HashMap::new(),
            window_rules: rules,
            log_titles: false,
        }
    }

    fn window_rule(app: &str) -> WindowRule {
        WindowRule {
            app: app.to_string(),
            title_pattern: Some("*".to_string()),
            title_regex: None,
            mode: "Mix".to_string(),
        }
    }

    // ── lazy gating (§4.3) ─────────────────────────────────────────────────

    #[test]
    fn polling_disabled_without_window_rules() {
        // No [per_app_modes] at all → no poller, no permission prompt.
        assert!(!title_polling_enabled(None));
        // [per_app_modes] present but zero window_rules → still no poller.
        assert!(!title_polling_enabled(Some(&pam_with_rules(vec![]))));
    }

    #[test]
    fn polling_enabled_only_with_window_rules() {
        let pam = pam_with_rules(vec![window_rule("Logic Pro")]);
        assert!(title_polling_enabled(Some(&pam)));
    }

    // ── poll-interval floor (§4.3) ─────────────────────────────────────────

    #[test]
    fn advanced_settings_default_and_serde_backfill() {
        use conductor_core::config::types::AdvancedSettings;
        // Default poll is 500ms.
        assert_eq!(AdvancedSettings::default().window_title_poll_ms, 500);
        // A config omitting the field backfills the default — old configs still
        // load (serde default), no breaking change.
        let s: AdvancedSettings = toml::from_str("").unwrap();
        assert_eq!(s.window_title_poll_ms, 500);
    }

    #[test]
    fn poll_ms_clamped_to_safe_floor() {
        assert_eq!(clamp_poll_ms(0), MIN_WINDOW_TITLE_POLL_MS);
        assert_eq!(clamp_poll_ms(1), MIN_WINDOW_TITLE_POLL_MS);
        assert_eq!(clamp_poll_ms(99), MIN_WINDOW_TITLE_POLL_MS);
        // At/above the floor is passed through unchanged.
        assert_eq!(clamp_poll_ms(100), 100);
        assert_eq!(clamp_poll_ms(500), 500);
    }

    // ── privacy masking (§4.1/§4.3) ────────────────────────────────────────

    #[test]
    fn titles_masked_by_default() {
        assert_eq!(mask_title(Some("Untitled"), false), "<title:len=8>");
        // Length is char count, not bytes — doesn't leak script/encoding.
        assert_eq!(mask_title(Some("café"), false), "<title:len=4>");
        assert_eq!(mask_title(None, false), "<no-title>");
    }

    #[test]
    fn titles_shown_only_when_opted_in() {
        assert_eq!(mask_title(Some("Untitled"), true), "\"Untitled\"");
        // Even opted-in, absence is reported structurally (not an empty quote).
        assert_eq!(mask_title(None, true), "<no-title>");
    }

    #[test]
    fn opted_in_titles_are_escaped_against_log_injection() {
        // A title is attacker-influenced text. Debug-quoting escapes embedded
        // quotes and newlines so a crafted title can't forge a second log line
        // or break out of the quoted field (Copilot review).
        assert_eq!(
            mask_title(Some("evil\nINJECTED"), true),
            "\"evil\\nINJECTED\""
        );
        assert_eq!(mask_title(Some("say \"hi\""), true), "\"say \\\"hi\\\"\"");
    }

    // ── observable degradation (§4.3 P0 #4) ────────────────────────────────

    #[test]
    fn ok_read_is_not_degraded_and_passes_title_through() {
        let tracker = DegradationTracker::new();
        assert_eq!(
            tracker.record(TitleRead::Ok(Some("Doc".into()))),
            Some("Doc".into())
        );
        assert!(!tracker.is_degraded());
        assert_eq!(tracker.record(TitleRead::Ok(None)), None);
        assert!(!tracker.is_degraded());
    }

    #[test]
    fn failed_read_degrades_and_falls_back_to_no_title() {
        let tracker = DegradationTracker::new();
        // Permission denied → degraded, title is None so app-name rules apply.
        assert_eq!(tracker.record(TitleRead::PermissionDenied), None);
        assert!(tracker.is_degraded());
        assert!(tracker.flag().load(Ordering::Relaxed));
        // Unsupported degrades the same way.
        assert_eq!(tracker.record(TitleRead::Unsupported), None);
        assert!(tracker.is_degraded());
    }

    #[test]
    fn degradation_recovers_on_later_success() {
        let tracker = DegradationTracker::new();
        tracker.record(TitleRead::PermissionDenied);
        assert!(tracker.is_degraded());
        // Permission later granted → a successful read clears the flag.
        tracker.record(TitleRead::Ok(Some("Now readable".into())));
        assert!(!tracker.is_degraded());
        assert!(!tracker.flag().load(Ordering::Relaxed));
    }

    #[test]
    fn unsupported_source_reports_unsupported() {
        assert_eq!(
            UnsupportedWindowTitleSource.focused_window_title(),
            TitleRead::Unsupported
        );
    }

    /// A scripted source for driving the tracker/poller logic deterministically.
    struct MockSource(std::sync::Mutex<std::collections::VecDeque<TitleRead>>);
    impl WindowTitleSource for MockSource {
        fn focused_window_title(&self) -> TitleRead {
            self.0
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(TitleRead::Ok(None))
        }
    }

    #[test]
    fn mock_source_threads_reads_through_the_tracker() {
        let src = MockSource(std::sync::Mutex::new(
            [
                TitleRead::Ok(Some("A".into())),
                TitleRead::PermissionDenied,
                TitleRead::Ok(Some("B".into())),
            ]
            .into(),
        ));
        let tracker = DegradationTracker::new();
        assert_eq!(tracker.record(src.focused_window_title()), Some("A".into()));
        assert!(!tracker.is_degraded());
        assert_eq!(tracker.record(src.focused_window_title()), None);
        assert!(tracker.is_degraded());
        assert_eq!(tracker.record(src.focused_window_title()), Some("B".into()));
        assert!(!tracker.is_degraded(), "recovered after permission granted");
    }
}

#[cfg(target_os = "macos")]
mod macos {
    //! Thin Accessibility-API leaf: read the system-wide focused application's
    //! focused window's `AXTitle`. Kept minimal and isolated — this is the only
    //! unsafe code in the subsystem; all logic lives in the parent module behind
    //! the [`super::WindowTitleSource`] trait.

    use super::{TitleRead, WindowTitleSource};
    // Reuse core-foundation's own `CFTypeRef` rather than a local `*const c_void`
    // alias, so our extern signatures and `wrap_under_create_rule` agree on one
    // pointer type — no cross-alias `as` casts that could mask a type confusion.
    // An `AXUIElementRef` is a `CFTypeRef` in the AX API.
    use core_foundation::base::{CFType, CFTypeRef, TCFType};
    use core_foundation::string::{CFString, CFStringRef};
    use std::ptr;
    use tracing::debug;

    type AXUIElementRef = CFTypeRef;
    type AXError = i32;

    /// Per-element messaging timeout for AX calls (seconds). Bounds a blocking
    /// `AXUIElementCopyAttributeValue` so an unresponsive target app can't stall
    /// the (background) title poll indefinitely. The poll runs
    /// off the engine hot path, so a short bound is plenty.
    const AX_MESSAGING_TIMEOUT_SECS: f32 = 0.25;

    const KAX_ERROR_SUCCESS: AXError = 0;
    // Accessibility globally disabled mid-session → a genuine permission failure,
    // so degradation (warn-once + status flag + app-rule fallback) fires. This is
    // the ONLY AX error that flips the user-visible degraded flag (the up-front
    // `AXIsProcessTrusted()` check catches the never-granted case).
    const KAX_ERROR_API_DISABLED: AXError = -25211;
    // A focused element with no value for the attribute (e.g. no focused window,
    // or a window with no title) — a successful read of "nothing", not a failure.
    const KAX_ERROR_NO_VALUE: AXError = -25212;
    const KAX_ERROR_ATTRIBUTE_UNSUPPORTED: AXError = -25205;

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        /// Whether this process is trusted for Accessibility. Returns Apple's
        /// `Boolean` (`unsigned char`), NOT a C99 `_Bool` — declaring it as Rust
        /// `bool` would be UB if the byte is neither 0 nor 1. We
        /// take it as `u8` and test `!= 0`. `false` ⇒ title reads fail →
        /// `PermissionDenied`.
        fn AXIsProcessTrusted() -> u8;
        fn AXUIElementCreateSystemWide() -> AXUIElementRef;
        /// Bound how long AX calls on `element` block waiting on the target app.
        fn AXUIElementSetMessagingTimeout(element: AXUIElementRef, timeout: f32) -> AXError;
        fn AXUIElementCopyAttributeValue(
            element: AXUIElementRef,
            attribute: CFStringRef,
            value: *mut CFTypeRef,
        ) -> AXError;
    }

    /// Copy an AX attribute off `element`, returning the value wrapped for
    /// automatic `CFRelease` (create-rule: the Copy fn returns a +1 ref). `Ok(None)`
    /// for the "no value / unsupported attribute" success-of-nothing case; `Err`
    /// carries the raw `AXError` for the caller to classify.
    ///
    /// # Safety
    /// `element` must be a live `AXUIElementRef` (or null, handled as an error).
    unsafe fn copy_attr(
        element: AXUIElementRef,
        attribute: &str,
    ) -> Result<Option<CFType>, AXError> {
        if element.is_null() {
            return Err(KAX_ERROR_NO_VALUE);
        }
        let attr = CFString::new(attribute);
        let mut value: CFTypeRef = ptr::null();
        let err = unsafe {
            AXUIElementCopyAttributeValue(element, attr.as_concrete_TypeRef(), &mut value)
        };
        match err {
            KAX_ERROR_SUCCESS if !value.is_null() => {
                // Wrap under the create rule so Drop calls CFRelease on the +1 ref.
                Ok(Some(unsafe { CFType::wrap_under_create_rule(value as _) }))
            }
            KAX_ERROR_SUCCESS => Ok(None),
            KAX_ERROR_NO_VALUE | KAX_ERROR_ATTRIBUTE_UNSUPPORTED => Ok(None),
            other => Err(other),
        }
    }

    /// Map an `AXError` from a focused-app/window traversal to a `TitleRead`.
    ///
    /// Only [`KAX_ERROR_API_DISABLED`] is a permission failure (→ degrade). Every
    /// other error — no focused app/window, an app that doesn't expose the
    /// attribute, or a **transient** messaging error (e.g. `kAXErrorCannotComplete`
    /// when an app is busy, `kAXErrorInvalidUIElement` on a stale element) — is a
    /// "no title this read": app-name rules apply and we retry next tick, WITHOUT
    /// flipping the user-visible `window_permission_degraded` flag for a transient
    /// or app-specific condition — the prior `_ => PermissionDenied`
    /// over-degraded.
    fn err_to_read(err: AXError) -> TitleRead {
        match err {
            KAX_ERROR_API_DISABLED => TitleRead::PermissionDenied,
            _ => TitleRead::Ok(None),
        }
    }

    /// Reads the focused window title via the Accessibility API.
    pub struct AxWindowTitleSource;

    impl WindowTitleSource for AxWindowTitleSource {
        fn focused_window_title(&self) -> TitleRead {
            // Cheap up-front gate: an untrusted process can't read AX attributes.
            // `Boolean` is `unsigned char` → test `!= 0` (any non-zero is true).
            if unsafe { AXIsProcessTrusted() } == 0 {
                return TitleRead::PermissionDenied;
            }
            let system_wide = unsafe { AXUIElementCreateSystemWide() };
            if system_wide.is_null() {
                return TitleRead::PermissionDenied;
            }
            // Bound how long the AX calls below can block on an unresponsive
            // app — the title poll must not hang. Per Apple's
            // AXUIElement docs, setting the timeout on the **system-wide** element
            // (the one from AXUIElementCreateSystemWide) sets it GLOBALLY for this
            // process — i.e. it applies to the AXFocusedApplication/Window/Title
            // calls below, which is exactly the scope we want. Best-effort: a
            // failure just leaves the OS default timeout in place.
            let set_rc =
                unsafe { AXUIElementSetMessagingTimeout(system_wide, AX_MESSAGING_TIMEOUT_SECS) };
            if set_rc != KAX_ERROR_SUCCESS {
                debug!(
                    "AXUIElementSetMessagingTimeout failed ({set_rc}); using OS default timeout"
                );
            }
            // RAII: wrap under the create rule so the system-wide element is
            // CFRelease'd by `Drop` on EVERY exit — including a panic inside
            // `read_focused_title` — rather than via a manual call that an early
            // return or unwind could skip.
            let system_wide = unsafe { CFType::wrap_under_create_rule(system_wide) };
            unsafe { read_focused_title(system_wide.as_concrete_TypeRef()) }
        }
    }

    /// systemWide → AXFocusedApplication → AXFocusedWindow → AXTitle.
    ///
    /// # Safety
    /// `system_wide` must be a live `AXUIElementRef`.
    unsafe fn read_focused_title(system_wide: AXUIElementRef) -> TitleRead {
        // The intermediate AXUIElements auto-release via `CFType`'s Drop.
        let app = match unsafe { copy_attr(system_wide, "AXFocusedApplication") } {
            Ok(Some(app)) => app,
            Ok(None) => return TitleRead::Ok(None), // no focused app
            Err(e) => return err_to_read(e),
        };
        let window = match unsafe { copy_attr(app.as_concrete_TypeRef() as _, "AXFocusedWindow") } {
            Ok(Some(w)) => w,
            Ok(None) => return TitleRead::Ok(None), // app has no focused window
            Err(e) => return err_to_read(e),
        };
        match unsafe { copy_attr(window.as_concrete_TypeRef() as _, "AXTitle") } {
            Ok(Some(title_cf)) => {
                // The AXTitle value is a CFString; downcast and stringify.
                match title_cf.downcast_into::<CFString>() {
                    Some(s) => TitleRead::Ok(Some(s.to_string())),
                    None => TitleRead::Ok(None), // unexpected type → treat as no title
                }
            }
            Ok(None) => TitleRead::Ok(None), // window with no title
            Err(e) => err_to_read(e),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn only_api_disabled_degrades() {
            // Accessibility globally disabled → permission failure (degrade).
            assert_eq!(
                err_to_read(KAX_ERROR_API_DISABLED),
                TitleRead::PermissionDenied
            );
            // Transient / app-specific errors are "no title this read", NOT a
            // permission failure — they must not flip the degraded flag.
            const KAX_ERROR_CANNOT_COMPLETE: AXError = -25204; // app busy/timeout
            const KAX_ERROR_INVALID_UI_ELEMENT: AXError = -25202; // stale element
            for transient in [
                KAX_ERROR_CANNOT_COMPLETE,
                KAX_ERROR_INVALID_UI_ELEMENT,
                KAX_ERROR_NO_VALUE,
                -25208, // not-implemented (app doesn't support AX titles)
                -1,     // unknown
            ] {
                assert_eq!(
                    err_to_read(transient),
                    TitleRead::Ok(None),
                    "AXError {transient} must not degrade",
                );
            }
        }
    }
}
