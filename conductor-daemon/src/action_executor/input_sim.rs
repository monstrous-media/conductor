// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Keyboard / mouse input simulation via enigo (#1684 split from
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

impl ActionExecutor {
    /// Lazily initialize and return a mutable reference to Enigo
    ///
    /// Enigo requires accessibility permissions on macOS. By deferring
    /// initialization until first use, we allow constructing an ActionExecutor
    /// without those permissions (useful for tests that only exercise MIDI/OSC).
    pub(crate) fn get_enigo(&mut self) -> Result<&mut Enigo, DispatchError> {
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
            // PR #1030 review (2026-05-02): pre-fix this logged
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
    fn execute_keystroke_with_unrestricted_policy_skips_denylist() {
        // Power-user opt-out path. We can't actually dispatch in
        // a CI environment without Accessibility, so we just
        // verify the policy check itself accepts the combo —
        // the subsequent enigo failure is OK because it's a
        // platform/permission issue, not a policy violation.
        let mut e =
            empty_executor().with_keystroke_policy(keystroke_policy::KeystrokePolicyEnforcer::new(
                keystroke_policy::KeystrokePolicy::Unrestricted,
            ));
        let result = e.execute_keystroke(vec![KeyCode::Unicode('q')], vec![ModifierKey::Command]);
        match result {
            // Either the whole thing succeeded (Accessibility OK) or
            // enigo barfed AFTER the policy passed. Both are
            // acceptable — what we care about is that the error
            // (if any) is NOT a D8 policy denial.
            Ok(()) => {}
            Err(DispatchError::OsAutomation(msg)) => {
                assert!(
                    !msg.contains("ADR-027 D8"),
                    "Unrestricted policy must NOT produce a D8 denial; \
                     enigo failures from the host are OK. Got {msg:?}",
                );
            }
            Err(other) => {
                panic!("Unrestricted Cmd+Q should not produce a D8 denial; got {other:?}",)
            }
        }
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
