// Copyright 2026 Amiable
// SPDX-License-Identifier: MIT

//! ADR-027 D8 — keystroke policy mode for the daemon's
//! `Action::Keystroke` execution path.
//!
//! Closes part of audit Finding F-08. Keystroke simulation is
//! **more dangerous than shell exec** because keystrokes can:
//!
//! - confirm OS-level dialogs (including the very D1 confirmation
//!   prompts the user might be reading),
//! - type into password fields,
//! - drive any application UI without restriction.
//!
//! ADR-027 §D8 specifies a default `standard` policy that blocks
//! a small static deny-list of universally-dangerous combos
//! (force-quit, screen-lock, secure-delete, etc.) and a rate
//! limit of 60 keystrokes/sec. An `unrestricted` opt-out exists
//! for power users; it logs a startup warning to make the
//! reduced-safety stance visible to operators.
//!
//! ## Scope of this slice
//!
//! This first ADR-027 D8 slice intentionally covers:
//!
//! - Cross-platform static deny-list of dangerous modifier/key
//!   combos. Combos dangerous on *any* supported platform are
//!   denied universally — typing `Ctrl+Alt+Delete` on macOS is
//!   harmless, but on Linux/Windows it triggers screen-lock,
//!   and a "platform-aware" deny-list complicates the threat
//!   model without measurable benefit.
//! - 60 keys/sec sliding-window rate limit. Actions exceeding
//!   the cap are **rejected wholesale** (no deferral, no
//!   partial dispatch) — the whole `Action::Keystroke` call
//!   returns an error and zero keys are sent. Partial dispatch
//!   would leave the keyboard in an undefined state for the
//!   calling application; deferred dispatch would queue
//!   keystrokes for an arbitrary delay, also undesirable for
//!   user-driven macros.
//! - `unrestricted` mode that skips the deny-list and the rate
//!   limit. Caller is expected to log a startup warning.
//!
//! Deferred to follow-up PRs:
//!
//! - macOS AX framework password-field detection. Requires
//!   accessibility-API integration, separate platform code path,
//!   and a permission grant under ADR-029. The deny-list is the
//!   right protection for the PUBLIC threat model (A1 same-user
//!   malware confirming dialogs); password-field denial is a
//!   different mitigation for a separate threat (A1 typing into
//!   the focused app's password input).
//! - Per-keystroke capability gating (`keystroke.modifier_combo`
//!   as a separate capability). Depends on D5/D3 capability
//!   schema work that hasn't merged yet.
//! - Audit logging of the focused application bundle ID for
//!   every keystroke. Depends on the audit-logging plumbing
//!   already in place but adds frontmost-app detection latency
//!   to every dispatch — design consideration not yet settled.

use conductor_core::actions::{KeyCode, ModifierKey};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Maximum keystrokes the daemon will dispatch per second under
/// the `standard` policy. Drawn from ADR-027 §D8. High enough to
/// support legitimate macros (a fast typist hits ~6–8 keys/sec;
/// chord stems and repeated-action loops can hit 20–30/sec) but
/// low enough that a malicious flood is forced through a
/// throttle that's noticeable to the user.
pub const STANDARD_RATE_LIMIT_KEYS_PER_SEC: u32 = 60;

/// Length of the rate-limit sliding window. Combined with
/// [`STANDARD_RATE_LIMIT_KEYS_PER_SEC`] this gives the effective
/// per-second cap.
pub const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(1);

/// Daemon-wide policy mode for keystroke simulation.
///
/// **Currently set programmatically only.** PR #1030 review
/// (2026-05-02) flagged that earlier doc comments here implied
/// a `[security.keystroke] policy` config field that doesn't
/// exist in the repo. The config-file plumbing for this is a
/// follow-up — when D5's capability/tier surface lands, that
/// PR will wire `[security.keystroke]` through to construction
/// of [`KeystrokePolicyEnforcer`] and emit the startup warning
/// for `Unrestricted`. Until then, callers construct directly
/// via [`KeystrokePolicyEnforcer::new`] / [`Self::Standard`]
/// (the default) / [`Self::Unrestricted`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KeystrokePolicy {
    /// Default. Static deny-list of dangerous combos +
    /// 60-keys/sec rate limit. Non-bypassable; the daemon refuses
    /// to dispatch denied keystrokes regardless of caller.
    ///
    /// Default-deny posture: require an explicit opt-in to
    /// disable the deny-list. Same posture as ADR-027 D5's
    /// tier gate.
    #[default]
    Standard,

    /// Fully bypassable. Used only when the operator has
    /// explicitly opted out and accepts the risk. **The
    /// startup warning the spec asks for is part of D5's
    /// config-wiring follow-up; this PR doesn't emit one
    /// because it doesn't read config.** When D5 lands the
    /// startup-warning emit goes there.
    Unrestricted,
}

/// Reason a keystroke action was denied. Carries enough
/// structured information to surface useful audit log entries
/// and a clear error message to the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeystrokePolicyError {
    /// The combo matches an entry in the static deny-list.
    /// `combo_label` identifies the rule that fired (e.g.
    /// `"Cmd+Q force-quit"`). `modifiers` is the concrete
    /// modifier set the caller requested. `key` is the
    /// concrete *requested* key that matched (PR #1030 review,
    /// 2026-05-02): pre-fix this was the rule's key (e.g.
    /// always `'q'` even when the caller asked for `'Q'`),
    /// which lost case information for audit/debug logs.
    /// `combo_label` still identifies which rule matched.
    DenylistedCombo {
        combo_label: &'static str,
        modifiers: Vec<ModifierKey>,
        key: KeyCode,
    },

    /// Rate limit fired. `recent_count` is the number of
    /// keystrokes already dispatched in the current window;
    /// `requested` is how many this action would add.
    RateLimitExceeded {
        recent_count: u32,
        requested: u32,
        max_per_window: u32,
    },

    /// PR #1030 round-3 review (2026-05-02): the rate-limit
    /// `Mutex` was poisoned (a previous panic on a different
    /// thread left the `RateState` in an unknown shape). We
    /// fail closed — keystrokes are denied — but surface this
    /// as a dedicated variant rather than a synthetic
    /// `RateLimitExceeded`. That keeps audit logs and operator
    /// alerts honest: an at-cap denial means traffic, an
    /// `InternalStateCorrupt` denial means "restart the daemon
    /// to clear the poisoned state".
    InternalStateCorrupt,
}

impl std::fmt::Display for KeystrokePolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DenylistedCombo {
                combo_label,
                modifiers,
                key,
            } => write!(
                f,
                "keystroke policy: denied {combo_label} (modifiers={modifiers:?}, key={key:?})",
            ),
            Self::RateLimitExceeded {
                recent_count,
                requested,
                max_per_window,
            } => write!(
                f,
                "keystroke policy: rate limit exceeded \
                 ({recent_count} recent + {requested} requested > \
                 {max_per_window}/sec)",
            ),
            Self::InternalStateCorrupt => write!(
                f,
                "keystroke policy: internal rate-limit state \
                 was poisoned by a previous panic; daemon \
                 restart required to clear",
            ),
        }
    }
}

impl std::error::Error for KeystrokePolicyError {}

/// A single deny-list entry. A keystroke request matches if the
/// requested modifiers contain ALL of `required_modifiers`
/// (subset match — adding extra modifiers doesn't make a
/// dangerous combo safe) AND the requested keys contain `key`.
struct DenylistEntry {
    label: &'static str,
    required_modifiers: &'static [ModifierKey],
    key: KeyCode,
}

/// Static cross-platform deny-list. Each entry is a combo that's
/// dangerous on at least one supported platform; we deny it
/// universally so the policy doesn't depend on runtime OS
/// detection (which would itself become an attack surface — a
/// platform-spoof could disable a deny).
///
/// Coverage:
/// - Cmd+Q / Ctrl+Q          → force-quit foreground app
/// - Cmd+Option+Esc          → macOS force-quit menu
/// - Cmd+Shift+Delete        → macOS empty trash (no confirmation)
/// - Cmd+Ctrl+Q              → macOS screen-lock
/// - Ctrl+Option+Delete      → Linux/Windows Ctrl-Alt-Del
///   (Option = Alt in our domain model, see ModifierKey doc)
/// - Cmd+L (rare on macOS, but Win+L is screen-lock; our
///   ModifierKey::Command = Win on Windows)
const DENYLIST: &[DenylistEntry] = &[
    DenylistEntry {
        label: "Cmd+Q force-quit",
        required_modifiers: &[ModifierKey::Command],
        key: KeyCode::Unicode('q'),
    },
    DenylistEntry {
        label: "Ctrl+Q force-quit",
        required_modifiers: &[ModifierKey::Control],
        key: KeyCode::Unicode('q'),
    },
    DenylistEntry {
        label: "Cmd+Option+Esc force-quit-menu",
        required_modifiers: &[ModifierKey::Command, ModifierKey::Option],
        key: KeyCode::Escape,
    },
    DenylistEntry {
        label: "Cmd+Shift+Delete empty-trash",
        required_modifiers: &[ModifierKey::Command, ModifierKey::Shift],
        key: KeyCode::Delete,
    },
    DenylistEntry {
        label: "Cmd+Ctrl+Q screen-lock",
        required_modifiers: &[ModifierKey::Command, ModifierKey::Control],
        key: KeyCode::Unicode('q'),
    },
    DenylistEntry {
        label: "Ctrl+Alt+Delete system-interrupt",
        required_modifiers: &[ModifierKey::Control, ModifierKey::Option],
        key: KeyCode::Delete,
    },
    DenylistEntry {
        label: "Cmd+L screen-lock-or-address-bar",
        required_modifiers: &[ModifierKey::Command],
        key: KeyCode::Unicode('l'),
    },
];

/// Enforces the keystroke policy. Owns the rate-limit state so
/// callers don't need to coordinate timing themselves.
///
/// Cheap to clone via `Arc` if the daemon needs to share it
/// between executor tasks (the action executor today is single-
/// owner so no Arc wrapper here).
pub struct KeystrokePolicyEnforcer {
    policy: KeystrokePolicy,
    rate_state: Mutex<RateState>,
}

#[derive(Default)]
struct RateState {
    /// Timestamps of the last N dispatched keys; entries older
    /// than [`RATE_LIMIT_WINDOW`] are pruned on each check. Cap
    /// the buffer at the rate limit so worst-case memory is O(1)
    /// even under sustained at-cap traffic.
    timestamps: std::collections::VecDeque<Instant>,
}

impl KeystrokePolicyEnforcer {
    pub fn new(policy: KeystrokePolicy) -> Self {
        Self {
            policy,
            rate_state: Mutex::new(RateState::default()),
        }
    }

    /// Convenience constructor — default policy is `Standard`.
    pub fn standard() -> Self {
        Self::new(KeystrokePolicy::Standard)
    }

    pub fn policy(&self) -> KeystrokePolicy {
        self.policy
    }

    /// Check + register a keystroke action. Returns `Ok(())`
    /// when the action is permitted (rate-limit slot consumed),
    /// or a [`KeystrokePolicyError`] describing why it's denied.
    ///
    /// Production callers use this; tests use [`check_at`] for
    /// deterministic time control.
    pub fn check(
        &self,
        keys: &[KeyCode],
        modifiers: &[ModifierKey],
    ) -> Result<(), KeystrokePolicyError> {
        self.check_at(keys, modifiers, Instant::now())
    }

    /// Check + register at the given wall-clock time. Same
    /// semantics as [`check`] but the clock is injected for
    /// deterministic tests.
    pub fn check_at(
        &self,
        keys: &[KeyCode],
        modifiers: &[ModifierKey],
        now: Instant,
    ) -> Result<(), KeystrokePolicyError> {
        if matches!(self.policy, KeystrokePolicy::Unrestricted) {
            return Ok(());
        }

        // Deny-list check first. Each entry requires its
        // modifiers as a subset of the requested modifiers AND
        // its key to be present (case-insensitively for ASCII
        // letters) in the requested keys list.
        //
        // PR #1030 review (2026-05-02): linear `contains` over
        // the small `modifiers` slice (max 4 ModifierKey
        // variants) is faster and allocation-free vs. the
        // previous per-call `HashSet`.
        for entry in DENYLIST {
            let modifiers_ok = entry
                .required_modifiers
                .iter()
                .all(|m| modifiers.contains(m));
            if !modifiers_ok {
                continue;
            }
            // Find the actual REQUESTED key that matched the
            // rule (not the rule's key). Captures case
            // correctly so the error/audit log can report
            // "user pressed 'Q'" rather than "rule fired with
            // 'q'" (PR #1030 review).
            let matched_requested = keys
                .iter()
                .copied()
                .find(|k| keys_equivalent(*k, entry.key));
            if let Some(matched) = matched_requested {
                return Err(KeystrokePolicyError::DenylistedCombo {
                    combo_label: entry.label,
                    modifiers: modifiers.to_vec(),
                    key: matched,
                });
            }
        }

        // Rate-limit check. Prune old timestamps, count what's
        // left in the window, decide whether `keys.len()` more
        // would push us over.
        let requested = keys.len() as u32;
        let mut state = match self.rate_state.lock() {
            Ok(g) => g,
            // PR #1030 round-3 review (2026-05-02): poisoning
            // is a genuine safety problem for a security
            // limiter — we don't know whether the prior panic
            // left the timestamps deque in a consistent state.
            // Pre-fix this used `into_inner()` which proceeded
            // with possibly-corrupt state; that could
            // undercount and let through more keystrokes than
            // the cap. Round-2 returned a synthetic
            // RateLimitExceeded with maxed counters, but that
            // mis-attributed the cause. Round-3: dedicated
            // `InternalStateCorrupt` variant. Callers fail
            // closed and operator audit logs / metrics see the
            // root cause cleanly.
            Err(_) => return Err(KeystrokePolicyError::InternalStateCorrupt),
        };

        let cutoff = now.checked_sub(RATE_LIMIT_WINDOW);
        if let Some(cutoff) = cutoff {
            while let Some(&front) = state.timestamps.front() {
                if front < cutoff {
                    state.timestamps.pop_front();
                } else {
                    break;
                }
            }
        }
        let recent_count = state.timestamps.len() as u32;
        if recent_count + requested > STANDARD_RATE_LIMIT_KEYS_PER_SEC {
            return Err(KeystrokePolicyError::RateLimitExceeded {
                recent_count,
                requested,
                max_per_window: STANDARD_RATE_LIMIT_KEYS_PER_SEC,
            });
        }

        // Register `requested` keystrokes at this instant.
        for _ in 0..requested {
            state.timestamps.push_back(now);
        }
        Ok(())
    }
}

impl Default for KeystrokePolicyEnforcer {
    fn default() -> Self {
        Self::standard()
    }
}

/// Stateless deny-list check (issue #1040, ADR-027 D8 save-time
/// surfacing).
///
/// Runs only the deny-list portion of [`KeystrokePolicyEnforcer::check_at`],
/// without consulting or mutating any rate-limit state. Suitable for
/// call sites that want to surface a deny *before* execution — e.g.
/// plan-apply validation in `conductor-daemon::daemon::llm::plan`, or
/// editor-time visual warnings — without coupling to the daemon's
/// rate-limit machinery.
///
/// Returns:
/// - `Ok(())` when the policy is [`KeystrokePolicy::Unrestricted`]
///   (explicit user opt-out) OR the combo doesn't match any
///   deny-list entry.
/// - `Err(KeystrokePolicyError::DenylistedCombo { … })` when the
///   combo matches a deny-list entry under the `Standard` policy.
///
/// **Never returns** the rate-limit or internal-corruption variants —
/// those are stateful concerns of the enforcer.
pub fn check_combo_only(
    policy: KeystrokePolicy,
    keys: &[KeyCode],
    modifiers: &[ModifierKey],
) -> Result<(), KeystrokePolicyError> {
    if matches!(policy, KeystrokePolicy::Unrestricted) {
        return Ok(());
    }
    for entry in DENYLIST {
        let modifiers_ok = entry
            .required_modifiers
            .iter()
            .all(|m| modifiers.contains(m));
        if !modifiers_ok {
            continue;
        }
        if let Some(matched) = keys
            .iter()
            .copied()
            .find(|k| keys_equivalent(*k, entry.key))
        {
            return Err(KeystrokePolicyError::DenylistedCombo {
                combo_label: entry.label,
                modifiers: modifiers.to_vec(),
                key: matched,
            });
        }
    }
    Ok(())
}

/// Compare two KeyCode values for deny-list matching. Treats
/// `Unicode('q')` and `Unicode('Q')` as the same key — adding
/// Shift to a deny-listed letter mustn't bypass the deny.
///
/// Case folding is **ASCII-only** (`eq_ignore_ascii_case`); PR
/// #1030 review (2026-05-02). The deny-list keys are all ASCII
/// letters, so non-ASCII inputs (e.g. `'Ä'`) are deliberately
/// outside the deny scope and compare via plain `==`. Implementing
/// full Unicode case folding here would expand the matcher's
/// surface without any deny-list entry to anchor it to.
fn keys_equivalent(a: KeyCode, b: KeyCode) -> bool {
    match (a, b) {
        (KeyCode::Unicode(x), KeyCode::Unicode(y)) => x.eq_ignore_ascii_case(&y),
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enforcer() -> KeystrokePolicyEnforcer {
        KeystrokePolicyEnforcer::standard()
    }

    // ─── policy mode ─────────────────────────────────────────

    #[test]
    fn default_policy_is_standard() {
        let e = KeystrokePolicyEnforcer::default();
        assert_eq!(e.policy(), KeystrokePolicy::Standard);
    }

    #[test]
    fn unrestricted_policy_bypasses_denylist_and_rate_limit() {
        // Unrestricted is a deliberate opt-out for power users.
        // Must allow combos the standard policy denies AND must
        // not consume rate-limit budget.
        let e = KeystrokePolicyEnforcer::new(KeystrokePolicy::Unrestricted);
        // A combo the standard policy refuses:
        e.check(&[KeyCode::Unicode('q')], &[ModifierKey::Command])
            .expect("unrestricted must allow Cmd+Q");
        // 1000 keys at once — way over the 60/sec limit:
        let many = vec![KeyCode::Unicode('a'); 1000];
        e.check(&many, &[])
            .expect("unrestricted must skip rate limiting");
    }

    // ─── deny-list ───────────────────────────────────────────

    #[test]
    fn denies_cmd_q_force_quit() {
        let err = enforcer()
            .check(&[KeyCode::Unicode('q')], &[ModifierKey::Command])
            .unwrap_err();
        assert!(
            matches!(
                err,
                KeystrokePolicyError::DenylistedCombo { combo_label, .. }
                    if combo_label.contains("Cmd+Q")
            ),
            "Cmd+Q must be denied — keystrokes can confirm system \
             dialogs that the user is reading. Got {err:?}",
        );
    }

    #[test]
    fn denies_cmd_q_even_with_extra_modifier() {
        // Adding Shift to a dangerous combo doesn't make it
        // safer — it would still force-quit if the OS is
        // forgiving about extra mods, and even if not it's a
        // suspicious action shape.
        let err = enforcer()
            .check(
                &[KeyCode::Unicode('q')],
                &[ModifierKey::Command, ModifierKey::Shift],
            )
            .unwrap_err();
        assert!(
            matches!(err, KeystrokePolicyError::DenylistedCombo { .. }),
            "Cmd+Shift+Q must still be denied (Cmd+Q is the rule \
             that matches; extra modifiers don't bypass)",
        );
    }

    #[test]
    fn denies_uppercase_q_via_case_insensitive_match() {
        // A trivial bypass attempt: "I'll send 'Q' instead of
        // 'q'." Letters are matched case-insensitively in the
        // deny-list.
        let err = enforcer()
            .check(&[KeyCode::Unicode('Q')], &[ModifierKey::Command])
            .unwrap_err();
        assert!(
            matches!(err, KeystrokePolicyError::DenylistedCombo { .. }),
            "Cmd+Q deny must also catch Cmd+'Q' — case-insensitive \
             match is required to defeat the trivial bypass.",
        );
    }

    #[test]
    fn denies_ctrl_alt_delete() {
        let err = enforcer()
            .check(
                &[KeyCode::Delete],
                &[ModifierKey::Control, ModifierKey::Option],
            )
            .unwrap_err();
        assert!(matches!(err, KeystrokePolicyError::DenylistedCombo { .. }));
    }

    #[test]
    fn denies_cmd_option_escape() {
        let err = enforcer()
            .check(
                &[KeyCode::Escape],
                &[ModifierKey::Command, ModifierKey::Option],
            )
            .unwrap_err();
        assert!(matches!(err, KeystrokePolicyError::DenylistedCombo { .. }));
    }

    #[test]
    fn denies_cmd_shift_delete_empty_trash() {
        let err = enforcer()
            .check(
                &[KeyCode::Delete],
                &[ModifierKey::Command, ModifierKey::Shift],
            )
            .unwrap_err();
        assert!(matches!(err, KeystrokePolicyError::DenylistedCombo { .. }));
    }

    #[test]
    fn denies_cmd_ctrl_q_screen_lock() {
        let err = enforcer()
            .check(
                &[KeyCode::Unicode('q')],
                &[ModifierKey::Command, ModifierKey::Control],
            )
            .unwrap_err();
        assert!(matches!(err, KeystrokePolicyError::DenylistedCombo { .. }));
    }

    #[test]
    fn denies_cmd_l_screen_lock() {
        // Win+L is the Windows screen-lock; our ModifierKey::
        // Command maps to Win on Windows. Cross-platform-deny
        // approach: deny on every platform.
        let err = enforcer()
            .check(&[KeyCode::Unicode('l')], &[ModifierKey::Command])
            .unwrap_err();
        assert!(matches!(err, KeystrokePolicyError::DenylistedCombo { .. }));
    }

    #[test]
    fn allows_benign_cmd_c_copy() {
        // Smoke test: the standard policy must NOT block normal
        // user macros. If this ever fails, ergonomics broken.
        enforcer()
            .check(&[KeyCode::Unicode('c')], &[ModifierKey::Command])
            .expect("Cmd+C is benign; must be permitted");
    }

    #[test]
    fn allows_function_keys_without_modifiers() {
        enforcer()
            .check(&[KeyCode::F1], &[])
            .expect("F1 alone is benign");
    }

    #[test]
    fn allows_text_typing_without_modifiers() {
        // Typing "hello" character-by-character.
        enforcer()
            .check(
                &[
                    KeyCode::Unicode('h'),
                    KeyCode::Unicode('e'),
                    KeyCode::Unicode('l'),
                    KeyCode::Unicode('l'),
                    KeyCode::Unicode('o'),
                ],
                &[],
            )
            .expect("plain text typing is benign");
    }

    #[test]
    fn denies_when_any_key_in_sequence_matches_denylist() {
        // A single keystroke action with a sequence: if even
        // ONE entry hits the deny-list, the whole action is
        // denied — partial dispatch would leave the deny-listed
        // key in the keyboard's executed history, defeating the
        // policy.
        let err = enforcer()
            .check(
                &[
                    KeyCode::Unicode('a'), // benign
                    KeyCode::Unicode('q'), // deny-listed under Cmd
                    KeyCode::Unicode('b'), // benign
                ],
                &[ModifierKey::Command],
            )
            .unwrap_err();
        assert!(
            matches!(err, KeystrokePolicyError::DenylistedCombo { .. }),
            "any deny-listed key in the sequence must deny the whole \
             action; got {err:?}",
        );
    }

    // ─── rate limit ──────────────────────────────────────────

    #[test]
    fn allows_burst_just_under_rate_limit() {
        let e = enforcer();
        let t0 = Instant::now();
        // Cmd+C is benign — use as the burst payload.
        for _ in 0..STANDARD_RATE_LIMIT_KEYS_PER_SEC {
            e.check_at(&[KeyCode::Unicode('c')], &[ModifierKey::Command], t0)
                .expect("under limit");
        }
        // Now at exactly the cap; one more must fail.
        let err = e
            .check_at(&[KeyCode::Unicode('c')], &[ModifierKey::Command], t0)
            .unwrap_err();
        assert!(
            matches!(err, KeystrokePolicyError::RateLimitExceeded { .. }),
            "60 keys then 1 more should exceed the 60/sec cap; got {err:?}",
        );
    }

    #[test]
    fn rate_limit_window_slides() {
        // Send 60 keys at t0, fill the bucket. At t0 + 1s + 1ms,
        // the original timestamps fall out of the window and a
        // fresh keystroke must succeed.
        let e = enforcer();
        let t0 = Instant::now();
        for _ in 0..STANDARD_RATE_LIMIT_KEYS_PER_SEC {
            e.check_at(&[KeyCode::Unicode('c')], &[ModifierKey::Command], t0)
                .expect("fill bucket");
        }
        let t1 = t0 + RATE_LIMIT_WINDOW + Duration::from_millis(1);
        e.check_at(&[KeyCode::Unicode('c')], &[ModifierKey::Command], t1)
            .expect(
                "after the sliding window slides, a fresh keystroke \
                 must succeed — otherwise legit users are throttled \
                 forever after a burst",
            );
    }

    #[test]
    fn rate_limit_rejects_oversized_single_action() {
        // A single keystroke action with > 60 keys exceeds the
        // window's full budget by itself. Must be rejected.
        let e = enforcer();
        let many = vec![KeyCode::Unicode('a'); (STANDARD_RATE_LIMIT_KEYS_PER_SEC + 1) as usize];
        let err = e.check(&many, &[]).unwrap_err();
        assert!(
            matches!(err, KeystrokePolicyError::RateLimitExceeded { .. }),
            "a single Keystroke with > 60 keys must be rejected by the \
             rate limiter, not partially dispatched; got {err:?}",
        );
    }

    #[test]
    fn rate_limit_does_not_consume_budget_when_denylisted() {
        // If a request is rejected by the deny-list, the rate-
        // limit budget must be unchanged — otherwise an attacker
        // can flood deny-listed combos to throttle legit users.
        let e = enforcer();
        let t0 = Instant::now();
        for _ in 0..70 {
            // Denied — Cmd+Q. Budget unchanged.
            let _ = e.check_at(&[KeyCode::Unicode('q')], &[ModifierKey::Command], t0);
        }
        // Now a benign 60 keystrokes must still all succeed —
        // budget should be intact.
        for _ in 0..STANDARD_RATE_LIMIT_KEYS_PER_SEC {
            e.check_at(&[KeyCode::Unicode('c')], &[ModifierKey::Command], t0)
                .expect(
                    "deny-list rejection must not consume rate-limit \
                     budget — otherwise an attacker can throttle legit \
                     users by spamming denied combos",
                );
        }
    }

    #[test]
    fn rate_limit_error_carries_useful_counters() {
        let e = enforcer();
        let t0 = Instant::now();
        // Partially fill bucket: 30 keystrokes so far.
        for _ in 0..30 {
            e.check_at(&[KeyCode::Unicode('c')], &[], t0)
                .expect("under limit");
        }
        // Try to add 50 more. 30 + 50 = 80 > 60 → reject.
        let err = e
            .check_at(&[KeyCode::Unicode('c'); 50], &[], t0)
            .unwrap_err();
        match err {
            KeystrokePolicyError::RateLimitExceeded {
                recent_count,
                requested,
                max_per_window,
            } => {
                assert_eq!(recent_count, 30, "recent count");
                assert_eq!(requested, 50, "requested count");
                assert_eq!(max_per_window, STANDARD_RATE_LIMIT_KEYS_PER_SEC, "max",);
            }
            other => panic!("expected RateLimitExceeded, got {other:?}"),
        }
    }

    #[test]
    fn enforcer_is_send_sync_for_use_in_action_executor() {
        // Static assert: the action executor holds an enforcer
        // and may dispatch from multiple tokio tasks. Send+Sync
        // is required.
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<KeystrokePolicyEnforcer>();
    }
}
