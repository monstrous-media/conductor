// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Keyboard / mouse input simulation via enigo (split from
//! `action_executor.rs`). Houses the domain→enigo conversion helpers
//! (`to_enigo_key`, `to_enigo_modifier`, `to_enigo_button`), the lazy
//! Enigo accessor (`get_enigo`), and the keystroke executor
//! (`execute_keystroke`) with its ADR-027 D8 policy enforcement.

use super::ActionExecutor;
use conductor_core::dispatch::DispatchError;
use conductor_core::{KeyCode, ModifierKey, MouseButton};
use enigo::{Button, Direction, Enigo, Key, Keyboard, Settings};

/// Convert domain KeyCode to enigo Key for execution
///
/// This conversion layer enables conductor-core to remain UI-independent while
/// the daemon can execute actions using platform-specific libraries.
pub(crate) fn to_enigo_key(key_code: KeyCode) -> Key {
    match key_code {
        // Unicode characters (alphanumeric and punctuation)
        KeyCode::Unicode(c) => Key::Unicode(c),

        // Special keys
        KeyCode::Space => Key::Unicode(' '),
        KeyCode::Return => Key::Return,
        KeyCode::Tab => Key::Tab,
        KeyCode::Escape => Key::Escape,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Delete => Key::Delete,

        // Arrow keys
        KeyCode::UpArrow => Key::UpArrow,
        KeyCode::DownArrow => Key::DownArrow,
        KeyCode::LeftArrow => Key::LeftArrow,
        KeyCode::RightArrow => Key::RightArrow,

        // Navigation keys
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,

        // Function keys
        KeyCode::F1 => Key::F1,
        KeyCode::F2 => Key::F2,
        KeyCode::F3 => Key::F3,
        KeyCode::F4 => Key::F4,
        KeyCode::F5 => Key::F5,
        KeyCode::F6 => Key::F6,
        KeyCode::F7 => Key::F7,
        KeyCode::F8 => Key::F8,
        KeyCode::F9 => Key::F9,
        KeyCode::F10 => Key::F10,
        KeyCode::F11 => Key::F11,
        KeyCode::F12 => Key::F12,
        KeyCode::F13 => Key::F13,
        KeyCode::F14 => Key::F14,
        KeyCode::F15 => Key::F15,
        KeyCode::F16 => Key::F16,
        KeyCode::F17 => Key::F17,
        KeyCode::F18 => Key::F18,
        KeyCode::F19 => Key::F19,
        KeyCode::F20 => Key::F20,

        // Media keys
        KeyCode::VolumeUp => Key::VolumeUp,
        KeyCode::VolumeDown => Key::VolumeDown,
        KeyCode::Mute => Key::VolumeMute,
        KeyCode::PlayPause => Key::MediaPlayPause,
        #[cfg(any(target_os = "windows", all(unix, not(target_os = "macos"))))]
        KeyCode::Stop => Key::MediaStop,
        #[cfg(target_os = "macos")]
        KeyCode::Stop => Key::Unicode('\0'), // MediaStop not available on macOS
        KeyCode::NextTrack => Key::MediaNextTrack,
        KeyCode::PreviousTrack => Key::MediaPrevTrack,

        // Editing keys
        #[cfg(any(target_os = "windows", all(unix, not(target_os = "macos"))))]
        KeyCode::Insert => Key::Insert,
        #[cfg(target_os = "macos")]
        KeyCode::Insert => Key::Unicode('\0'), // Insert not available on macOS
        #[cfg(any(target_os = "windows", all(unix, not(target_os = "macos"))))]
        KeyCode::PrintScreen => Key::PrintScr,
        #[cfg(target_os = "macos")]
        KeyCode::PrintScreen => Key::Unicode('\0'), // PrintScreen not available on macOS
        #[cfg(all(unix, not(target_os = "macos")))]
        KeyCode::ScrollLock => Key::ScrollLock,
        #[cfg(not(all(unix, not(target_os = "macos"))))]
        KeyCode::ScrollLock => Key::Unicode('\0'), // ScrollLock not available on macOS/Windows
        #[cfg(any(target_os = "windows", all(unix, not(target_os = "macos"))))]
        KeyCode::Pause => Key::Pause,
        #[cfg(target_os = "macos")]
        KeyCode::Pause => Key::Unicode('\0'), // Pause not available on macOS
        KeyCode::CapsLock => Key::CapsLock,
        #[cfg(any(target_os = "windows", all(unix, not(target_os = "macos"))))]
        KeyCode::NumLock => Key::Numlock,
        #[cfg(target_os = "macos")]
        KeyCode::NumLock => Key::Unicode('\0'), // NumLock not available on macOS
    }
}

/// Convert domain ModifierKey to enigo Key for execution
pub(crate) fn to_enigo_modifier(modifier: ModifierKey) -> Key {
    match modifier {
        ModifierKey::Command => Key::Meta,
        ModifierKey::Control => Key::Control,
        ModifierKey::Option => Key::Alt,
        ModifierKey::Shift => Key::Shift,
    }
}

/// Convert domain MouseButton to enigo Button for execution
pub(crate) fn to_enigo_button(mouse_button: MouseButton) -> Button {
    match mouse_button {
        MouseButton::Left => Button::Left,
        MouseButton::Right => Button::Right,
        MouseButton::Middle => Button::Middle,
    }
}

/// True when this crate was compiled as its own unit-test harness.
#[cfg(test)]
const UNDER_UNIT_TEST: bool = true;
#[cfg(not(test))]
const UNDER_UNIT_TEST: bool = false;

/// Env var integration tests and CI set to declare "this is a test harness".
/// Needed because `tests/` binaries link the library compiled WITHOUT
/// `cfg(test)`, so [`UNDER_UNIT_TEST`] is false for them.
pub const TEST_HARNESS_ENV: &str = "CONDUCTOR_TEST_HARNESS";

/// Deliberate opt-out, for the rare test that genuinely means to inject.
pub const ALLOW_TEST_INJECTION_ENV: &str = "CONDUCTOR_ALLOW_TEST_INPUT_INJECTION";

/// Refuse to build the OS input injector when running under a test harness.
///
/// enigo injects into whatever window currently has focus. macOS attributes
/// Accessibility to the *responsible* process, which for a test binary is the
/// developer's terminal or editor — and those commonly hold the grant. So any
/// test that reaches the injector types into the developer's screen, clicks in
/// it, or quits their application. CI has no display server, enigo errors
/// there, and the hazard is invisible in review: it only bites locally.
///
/// That is not hypothetical. Two tests shipped exactly this bug — one typed a
/// literal `x` into the focused window, another pressed a real Cmd+Q and quit
/// the focused app — and both read as passing. This interlock makes the class
/// unreachable by construction rather than by review habit.
///
/// Opt a deliberate injection test back in with
/// `CONDUCTOR_ALLOW_TEST_INPUT_INJECTION=1`.
fn refuse_injection_under_test() -> Result<(), DispatchError> {
    injection_decision(
        UNDER_UNIT_TEST || std::env::var_os(TEST_HARNESS_ENV).is_some(),
        std::env::var_os(ALLOW_TEST_INJECTION_ENV).is_some(),
    )
}

/// Pure decision half of [`refuse_injection_under_test`], split out so the
/// policy can be tested exhaustively without mutating process environment
/// (`set_var` is `unsafe` in edition 2024 and races other threads).
fn injection_decision(under_test: bool, explicitly_allowed: bool) -> Result<(), DispatchError> {
    if !under_test || explicitly_allowed {
        return Ok(());
    }
    Err(DispatchError::OsAutomation(format!(
        "input injection refused: running under a test harness. enigo would \
         type/click into the focused window on the developer's machine. If this \
         test genuinely means to inject, set {ALLOW_TEST_INJECTION_ENV}=1 and \
         mark it #[ignore]."
    )))
}

impl ActionExecutor {
    /// Lazily initialize and return a mutable reference to Enigo
    ///
    /// Enigo requires accessibility permissions on macOS. By deferring
    /// initialization until first use, we allow constructing an ActionExecutor
    /// without those permissions (useful for tests that only exercise MIDI/OSC).
    ///
    /// Refuses outright under a test harness — see
    /// [`refuse_injection_under_test`].
    pub(crate) fn get_enigo(&mut self) -> Result<&mut Enigo, DispatchError> {
        refuse_injection_under_test()?;
        if self.enigo.is_none() {
            self.enigo = Some(
                Enigo::new(&Settings::default())
                    .map_err(|e| DispatchError::OsAutomation(e.to_string()))?,
            );
        }
        Ok(self.enigo.as_mut().unwrap())
    }

    /// Execute a keystroke with modifiers
    ///
    /// Converts domain types (KeyCode, ModifierKey) to platform-specific enigo types.
    pub(crate) fn execute_keystroke(
        &mut self,
        keys: Vec<KeyCode>,
        modifiers: Vec<ModifierKey>,
    ) -> Result<(), DispatchError> {
        // ADR-027 D8: enforce the keystroke policy (deny-list +
        // rate limit) before any keys reach enigo. Denials must
        // happen BEFORE Enigo is initialised so a denied first-
        // use doesn't trigger the macOS Accessibility prompt.
        if let Err(policy_err) = self.keystroke_policy.check(&keys, &modifiers) {
            // Pre-fix this logged
            // `?keys` (the full KeyCode vector). For a long
            // `Action::Keystroke` sequence — say, a macro that
            // types an API token character-by-character via
            // chained Unicode KeyCodes — being rate-limited
            // would dump the typed text into daemon logs
            // verbatim. (`Action::Text` itself goes through
            // `enigo.text()` and never reaches this code path,
            // but `Action::Keystroke` with many `Unicode(...)`
            // entries can carry the same secrets.)
            // Now we log only the metadata: counts, modifier
            // set (small, fixed-cardinality enum, not
            // sensitive), and the policy error itself (whose
            // Display impl prints the matched-letter-only via
            // KeyCode's Debug, which is fine for combo
            // identification). The literal pressed-text never
            // reaches logs.
            tracing::warn!(
                error = %policy_err,
                key_count = keys.len(),
                ?modifiers,
                "keystroke action refused by policy",
            );
            return Err(DispatchError::OsAutomation(format!(
                "ADR-027 D8: {policy_err}"
            )));
        }

        let enigo = self.get_enigo()?;

        // Convert and press modifiers
        let enigo_modifiers: Vec<Key> = modifiers.iter().map(|&m| to_enigo_modifier(m)).collect();
        for modifier in &enigo_modifiers {
            enigo
                .key(*modifier, Direction::Press)
                .map_err(|e| DispatchError::OsAutomation(e.to_string()))?;
        }

        // Convert and press keys
        for key_code in &keys {
            let enigo_key = to_enigo_key(*key_code);
            enigo
                .key(enigo_key, Direction::Click)
                .map_err(|e| DispatchError::OsAutomation(e.to_string()))?;
        }

        // Release modifiers
        for modifier in enigo_modifiers.iter().rev() {
            enigo
                .key(*modifier, Direction::Release)
                .map_err(|e| DispatchError::OsAutomation(e.to_string()))?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::keystroke_policy;
    use arc_swap::ArcSwap;
    use conductor_core::dispatch::{DispatchError, DispatchOutcome};
    use std::collections::HashMap;
    use std::sync::Arc;

    // ========== ADR-027 D8: keystroke policy integration ==========

    fn empty_executor() -> ActionExecutor {
        ActionExecutor::new(Arc::new(ArcSwap::from_pointee(HashMap::new())))
    }

    #[test]
    fn injection_decision_truth_table() {
        // Production (not under any harness) must be unaffected — the interlock
        // must never change real behaviour for real users.
        assert!(
            injection_decision(false, false).is_ok(),
            "production must inject"
        );
        assert!(
            injection_decision(false, true).is_ok(),
            "production must inject"
        );
        // Under a harness: refused unless explicitly opted in. This arm is what
        // covers integration tests in tests/, which link the library without
        // cfg(test) and so rely on CONDUCTOR_TEST_HARNESS.
        assert!(
            injection_decision(true, false).is_err(),
            "a test harness must NOT be able to inject by default",
        );
        assert!(
            injection_decision(true, true).is_ok(),
            "explicit opt-in must still work for deliberate injection tests",
        );
    }

    #[test]
    fn injector_is_unreachable_under_test_harness() {
        // The interlock is the only thing standing between a careless test and
        // the developer's focused window. Assert it actually holds, so the
        // guarantee cannot rot silently.
        let mut e = empty_executor();
        match e.get_enigo() {
            Err(DispatchError::OsAutomation(msg)) => assert!(
                msg.contains("input injection refused"),
                "expected the interlock refusal; got {msg:?}",
            ),
            Ok(_) => panic!(
                "get_enigo MUST refuse under a test harness — without this, a \
                 test that reaches it types into whatever window has focus on \
                 the machine running the suite",
            ),
            Err(other) => panic!("expected the interlock refusal, got {other:?}"),
        }
    }

    #[test]
    fn execute_keystroke_refuses_denylisted_combo_before_enigo() {
        // The policy check fires BEFORE the lazy Enigo init, so
        // a CI environment without Accessibility doesn't crash —
        // and a malicious caller can't bypass the deny-list by
        // making the first-ever keystroke a denied combo.
        let mut e = empty_executor();
        let result = e.execute_keystroke(vec![KeyCode::Unicode('q')], vec![ModifierKey::Command]);
        match result {
            Err(DispatchError::OsAutomation(msg)) => {
                assert!(
                    msg.contains("ADR-027 D8") && msg.contains("Cmd+Q"),
                    "denial message should reference D8 and the offending \
                     combo so operators reading logs can trace the policy \
                     hit; got {msg:?}",
                );
            }
            other => panic!(
                "Cmd+Q should be refused by D8 policy with an OsAutomation \
                 error; got {other:?}",
            ),
        }
    }

    #[test]
    fn unrestricted_policy_skips_denylist() {
        // Power-user opt-out path: Unrestricted bypasses the D8 deny-list.
        //
        // Assert on the ENFORCER, never via `execute_keystroke`. Once the
        // policy passes there is nothing between the call and `enigo.key(...)`,
        // so dispatching here really presses Cmd+Q and QUITS THE FOCUSED APP.
        // Not hypothetical: macOS attributes Accessibility to the responsible
        // process, which for a test binary is the developer's terminal or
        // editor -- and those commonly hold the grant, so the keystroke lands.
        // CI has no display server, enigo errors, and the hazard stays hidden.
        //
        // The invariant under test is purely the policy decision, so check it
        // directly. See `execute_keystroke_refuses_denylisted_combo_before_enigo`
        // for the deny path, safe precisely because it never reaches the injector.
        let enforcer = keystroke_policy::KeystrokePolicyEnforcer::new(
            keystroke_policy::KeystrokePolicy::Unrestricted,
        );
        assert!(
            enforcer
                .check(&[KeyCode::Unicode('q')], &[ModifierKey::Command])
                .is_ok(),
            "Unrestricted policy must NOT deny Cmd+Q — the deny-list is \
             the thing being opted out of",
        );
    }

    #[test]
    #[ignore] // Sends real keystrokes via enigo — types into active window
    fn test_text_returns_completed() {
        let mut executor = ActionExecutor::default();

        let action = conductor_core::Action::Text("hello".to_string());
        let result = executor.execute(action, None);
        // Text may fail if no display (CI), but if it succeeds it should be Completed
        if let Ok(outcome) = result {
            assert_eq!(outcome, DispatchOutcome::Completed);
        }
    }
}
