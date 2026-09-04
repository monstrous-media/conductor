// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Shared MIDI placeholder substitution (ADR-038 R2 P5).
//!
//! A single string-substitution primitive used by both the `MidiToOsc`
//! transform (OSC address templates) and the ADR-038 `Tap` action message, so
//! the placeholder vocabulary (`{cc}` / `{value}` / `{note}` / `{velocity}`)
//! never drifts between the two surfaces.
//!
//! Deliberately simple: plain `str::replace`, no regex, no escaping. A template
//! that omits a placeholder is returned with that placeholder untouched; a
//! template with no placeholders is returned as-is.

/// Replace each `(placeholder, value)` pair in `template`, in order.
///
/// `template`'s placeholders are literal substrings (e.g. `"{cc}"`); each
/// occurrence is replaced with the decimal string of `value`. Callers pass only
/// the placeholders meaningful for the event type, so e.g. a CC template never
/// has `{note}` substituted (it is left literal, matching the prior
/// `MidiToOsc` behaviour).
pub fn substitute(template: &str, pairs: &[(&str, u8)]) -> String {
    let Some((first_placeholder, first_value)) = pairs.first() else {
        return template.to_string();
    };

    let mut out = template.replace(first_placeholder, &first_value.to_string());
    for (placeholder, value) in pairs.iter().skip(1) {
        out = out.replace(placeholder, &value.to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutes_each_pair() {
        assert_eq!(
            substitute("/eos/fader/{cc}/{value}", &[("{cc}", 7), ("{value}", 100)]),
            "/eos/fader/7/100"
        );
    }

    #[test]
    fn leaves_unlisted_placeholders_literal() {
        // A CC-only pair set must not touch {note}.
        assert_eq!(
            substitute("note {note} cc {cc}", &[("{cc}", 7)]),
            "note {note} cc 7"
        );
    }

    #[test]
    fn no_placeholders_returned_as_is() {
        assert_eq!(substitute("/eos/cue/fire", &[("{cc}", 7)]), "/eos/cue/fire");
    }

    #[test]
    fn repeated_placeholder_all_replaced() {
        assert_eq!(substitute("{value}-{value}", &[("{value}", 42)]), "42-42");
    }
}
