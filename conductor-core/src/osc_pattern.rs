// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! OSC 1.0 address pattern matching (ADR-039-A Slice 2, #2325).
//!
//! Implements the OSC 1.0 "OSC Message Dispatching and Pattern Matching"
//! semantics for `Trigger::OscAddressPattern` — deliberately **not regex**
//! (Council-mandated: no ReDoS surface on attacker-supplied addresses):
//!
//! - `?` matches any single character except `/`
//! - `*` matches any sequence of zero or more characters except `/`
//! - `[abc]` / `[a-z]` matches one character in the set; a leading `!`
//!   negates (`[!0-9]`); `-` first/last is literal
//! - `{foo,bar}` matches any one of the comma-separated alternatives
//! - every other character (including `/`) matches literally
//!
//! ## Security shape
//!
//! The **pattern** comes from config (validated at load via
//! [`OscPattern::compile`], which rejects malformed syntax); the **address**
//! comes off the wire (attacker-controlled, already capped by the OSC parser's
//! datagram limit). Matching is panic-free over arbitrary address bytes and
//! runs in O(pattern_len × address_len) per alternative:
//!
//! - `{}` alternation is expanded at **compile time** into separate simple
//!   globs (no runtime branching). Expansion is bounded by
//!   [`MAX_PATTERN_ALTERNATIVES`] so a config like `{a,b}{c,d}{e,f}…` cannot
//!   amplify; over-limit patterns are a load error.
//! - each simple glob is matched with the classic two-pointer backtracking
//!   algorithm (single star-resume point — linear scan, no exponential
//!   blowup, no recursion, no stack growth on adversarial addresses).

/// Cap on the number of simple globs a single pattern may expand to via
/// `{}` alternation (compile-time bound; exceeding it is a config error).
pub const MAX_PATTERN_ALTERNATIVES: usize = 64;

/// A compiled OSC address pattern: brace alternations expanded into simple
/// globs (`?`, `*`, `[…]`, literals only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OscPattern {
    alternatives: Vec<String>,
}

/// Byte offset of the `}` closing a brace alternation, **skipping any `}` that
/// sits inside a `[...]` character class** in the body (e.g. for `a,[}]}/x`
/// this returns the index of the SECOND `}`, not the in-class first one).
/// `s` begins immediately after the opening `{`. `None` if there is no
/// top-level close (the validation pass guarantees one exists for config
/// patterns; the `expect` at the call site relies on this).
fn find_brace_close(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut in_class = false;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'[' if !in_class => in_class = true,
            b']' if in_class => in_class = false,
            b'}' if !in_class => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

/// Split a brace body on **top-level** commas only — a comma inside a `[...]`
/// class (e.g. `{[a,b],c}` → `["[a,b]", "c"]`) belongs to that class, not the
/// alternation. Operates on byte boundaries; OSC addresses are ASCII so the
/// returned slices are valid UTF-8.
fn split_brace_options(body: &str) -> Vec<&str> {
    let bytes = body.as_bytes();
    let mut options = Vec::new();
    let mut in_class = false;
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'[' if !in_class => in_class = true,
            b']' if in_class => in_class = false,
            b',' if !in_class => {
                options.push(&body[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    options.push(&body[start..]);
    options
}

impl OscPattern {
    /// A pattern that matches nothing — the defensive compile target for a
    /// pattern string that failed [`OscPattern::compile`] after the validator
    /// should have rejected it (belt-and-braces; never matching is the
    /// fail-closed behaviour).
    pub fn never() -> OscPattern {
        OscPattern {
            alternatives: Vec::new(),
        }
    }

    /// Compile an OSC 1.0 address pattern, validating syntax and expanding
    /// `{}` alternations. Errors (for config-load rejection):
    /// - empty pattern or not starting with `/`
    /// - unclosed `[` or `{`, or a nested `{`
    /// - `}`/`]` without an opener are treated as literals per OSC practice,
    ///   but an unclosed opener is always an error
    /// - alternation expansion exceeding [`MAX_PATTERN_ALTERNATIVES`]
    pub fn compile(pattern: &str) -> Result<OscPattern, String> {
        if pattern.is_empty() || !pattern.starts_with('/') {
            return Err("OSC address pattern must start with '/'".to_string());
        }
        // Validate bracket/brace structure first so expansion can assume
        // well-formed input.
        let bytes = pattern.as_bytes();
        let mut i = 0;
        let mut in_class = false;
        let mut in_brace = false;
        while i < bytes.len() {
            match bytes[i] {
                b'[' if !in_class => in_class = true,
                b']' if in_class => in_class = false,
                b'{' if !in_class => {
                    if in_brace {
                        return Err("nested '{' is not valid in an OSC pattern".to_string());
                    }
                    in_brace = true;
                }
                b'}' if !in_class && in_brace => in_brace = false,
                _ => {}
            }
            i += 1;
        }
        if in_class {
            return Err("unclosed '[' in OSC pattern".to_string());
        }
        if in_brace {
            return Err("unclosed '{' in OSC pattern".to_string());
        }

        // Expand `{a,b}` alternations left-to-right, breadth-first, bounded.
        // Track `[...]` class state exactly as the validation pass above does,
        // so a `{` *inside* a character class is a literal — not an
        // alternation opener. Without this, `/f/[{]/x` passes validation (the
        // `{` is in a class) but `rest.find('}')` finds no close and panics
        // (Copilot review, PR #2377).
        let mut alternatives = vec![String::new()];
        let mut in_class = false;
        let mut chars = pattern.char_indices().peekable();
        while let Some((idx, c)) = chars.next() {
            match c {
                '[' if !in_class => {
                    in_class = true;
                    for alt in &mut alternatives {
                        alt.push(c);
                    }
                }
                ']' if in_class => {
                    in_class = false;
                    for alt in &mut alternatives {
                        alt.push(c);
                    }
                }
                '{' if !in_class => {
                    // Find the matching close — class-aware, so a `}` *inside*
                    // a `[...]` within the brace body (e.g. `/{a,[}]}/x`) is a
                    // literal, not the closer. A naive `find('}')` would stop at
                    // the in-class `}` and misparse the alternation (Council
                    // review, PR #2377). Validated above to exist.
                    let rest = &pattern[idx + 1..];
                    let close = find_brace_close(rest).expect("validated");
                    let body = &rest[..close];
                    // Split on TOP-LEVEL commas only — a comma inside a class
                    // (`{[a,b],c}`) belongs to that class, not the alternation.
                    let options = split_brace_options(body);
                    if alternatives.len().saturating_mul(options.len()) > MAX_PATTERN_ALTERNATIVES {
                        return Err(format!(
                            "OSC pattern expands to more than {} alternatives",
                            MAX_PATTERN_ALTERNATIVES
                        ));
                    }
                    alternatives = alternatives
                        .iter()
                        .flat_map(|prefix| {
                            options.iter().map(move |opt| {
                                let mut s = prefix.clone();
                                s.push_str(opt);
                                s
                            })
                        })
                        .collect();
                    // Skip past the brace body and the closing '}'.
                    while let Some(&(j, _)) = chars.peek() {
                        if j <= idx + 1 + close {
                            chars.next();
                        } else {
                            break;
                        }
                    }
                }
                _ => {
                    for alt in &mut alternatives {
                        alt.push(c);
                    }
                }
            }
        }

        Ok(OscPattern { alternatives })
    }

    /// Whether `address` (attacker-controlled wire data) matches this
    /// pattern. Panic-free over arbitrary input; linear two-pointer glob per
    /// alternative.
    pub fn matches(&self, address: &str) -> bool {
        self.alternatives
            .iter()
            .any(|glob| glob_match(glob.as_bytes(), address.as_bytes()))
    }
}

/// Classic iterative glob match with single-star backtracking. `?`/`*` do
/// not cross `/` (OSC part boundaries); `[…]` supports ranges and `!`
/// negation. Operates on bytes — OSC addresses are ASCII per spec, and byte
/// semantics keep the matcher allocation-free and panic-free for any input.
fn glob_match(pattern: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0usize, 0usize);
    // Backtrack point: position after the most recent `*` in the pattern and
    // the text position it was tried at.
    let (mut star_p, mut star_t): (Option<usize>, usize) = (None, 0);

    while t < text.len() {
        if p < pattern.len() {
            match pattern[p] {
                b'?' if text[t] != b'/' => {
                    p += 1;
                    t += 1;
                    continue;
                }
                b'*' => {
                    // Record resume point; `*` initially matches empty.
                    star_p = Some(p + 1);
                    star_t = t;
                    p += 1;
                    continue;
                }
                b'[' => {
                    if let Some((matched, next_p)) = class_match(pattern, p, text[t])
                        && matched
                        && text[t] != b'/'
                    {
                        p = next_p;
                        t += 1;
                        continue;
                    }
                    // fall through to backtrack
                }
                lit => {
                    if lit == text[t] {
                        p += 1;
                        t += 1;
                        continue;
                    }
                }
            }
        }
        // Mismatch: backtrack to the last `*`, consuming one more text byte —
        // but `*` may not cross a part boundary.
        match star_p {
            Some(sp) if text[star_t] != b'/' => {
                star_t += 1;
                t = star_t;
                p = sp;
            }
            _ => return false,
        }
    }
    // Text consumed: remaining pattern must be all `*`.
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

/// Match one byte against the character class starting at `pattern[start]`
/// (which is `[`). Returns `Some((matched, index_after_class))`, or `None`
/// for an unclosed class (compile() prevents this for config patterns;
/// defensive for direct calls).
fn class_match(pattern: &[u8], start: usize, byte: u8) -> Option<(bool, usize)> {
    let mut i = start + 1;
    let negated = pattern.get(i) == Some(&b'!');
    if negated {
        i += 1;
    }
    let mut matched = false;
    let mut first = true;
    while i < pattern.len() {
        match pattern[i] {
            b']' if !first => {
                return Some((matched != negated, i + 1));
            }
            lo => {
                // Range `lo-hi` (a `-` at the start/end is literal).
                if pattern.get(i + 1) == Some(&b'-')
                    && i + 2 < pattern.len()
                    && pattern[i + 2] != b']'
                {
                    let hi = pattern[i + 2];
                    if lo <= byte && byte <= hi {
                        matched = true;
                    }
                    i += 3;
                } else {
                    if byte == lo {
                        matched = true;
                    }
                    i += 1;
                }
            }
        }
        first = false;
    }
    None // unclosed class
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(pattern: &str, address: &str) -> bool {
        OscPattern::compile(pattern)
            .expect("valid pattern")
            .matches(address)
    }

    #[test]
    fn literal_addresses_match_exactly() {
        assert!(m("/eos/fader/1", "/eos/fader/1"));
        assert!(!m("/eos/fader/1", "/eos/fader/2"));
        assert!(!m("/eos/fader/1", "/eos/fader/1/fine"));
        assert!(!m("/eos/fader/1", "/eos/fader"));
    }

    #[test]
    fn question_mark_matches_single_non_slash() {
        assert!(m("/f/?", "/f/1"));
        assert!(m("/f/?", "/f/x"));
        assert!(!m("/f/?", "/f/12"), "? is exactly one char");
        assert!(!m("/f/?", "/f//"), "? must not match '/'");
        assert!(!m("/a?c", "/a/c"), "? must not cross part boundary");
    }

    #[test]
    fn star_matches_within_a_part_only() {
        assert!(m("/f/*", "/f/anything"));
        assert!(m("/f/*", "/f/"), "* matches empty");
        assert!(!m("/f/*", "/f/a/b"), "* must not cross '/'");
        assert!(m("/f/*/level", "/f/x/level"));
        assert!(!m("/f/*/level", "/f/x/y/level"));
        assert!(m("/*/fader", "/eos/fader"));
    }

    #[test]
    fn star_backtracking_is_correct_not_greedy_failure() {
        assert!(m("/a*b*c", "/axxbyyc"));
        assert!(m("/a*b*c", "/abc"));
        assert!(m("/a*bc", "/abxbc"), "star must backtrack past first b");
        assert!(!m("/a*b", "/axc"));
    }

    #[test]
    fn char_class_ranges_and_negation() {
        assert!(m("/f/[0-9]", "/f/5"));
        assert!(!m("/f/[0-9]", "/f/x"));
        assert!(m("/f/[!0-9]", "/f/x"));
        assert!(!m("/f/[!0-9]", "/f/5"));
        assert!(m("/f/[abc]", "/f/b"));
        assert!(!m("/f/[abc]", "/f/d"));
        // '-' literal at edges
        assert!(m("/f/[-x]", "/f/-"));
        // class must not match '/'
        assert!(
            !m("/f[!x]bar", "/f/bar"),
            "negated class must not match '/'"
        );
    }

    #[test]
    fn brace_alternation() {
        assert!(m("/{eos,ion}/fader", "/eos/fader"));
        assert!(m("/{eos,ion}/fader", "/ion/fader"));
        assert!(!m("/{eos,ion}/fader", "/etc/fader"));
        // alternation combined with wildcards
        assert!(m("/{go,stop}/*", "/go/now"));
    }

    #[test]
    fn compile_rejects_malformed_patterns() {
        assert!(OscPattern::compile("").is_err());
        assert!(OscPattern::compile("no-slash").is_err());
        assert!(OscPattern::compile("/f/[0-9").is_err(), "unclosed class");
        assert!(OscPattern::compile("/f/{a,b").is_err(), "unclosed brace");
        assert!(OscPattern::compile("/f/{a,{b,c}}").is_err(), "nested brace");
    }

    #[test]
    fn compile_bounds_alternation_amplification() {
        // 4 groups of 4 = 256 > 64 → rejected at load, no runtime cost.
        let bomb = "/{a,b,c,d}{a,b,c,d}{a,b,c,d}{a,b,c,d}";
        assert!(OscPattern::compile(bomb).is_err());
        // 2×2×2 = 8 ≤ 64 → fine.
        let ok = "/{a,b}{c,d}{e,f}";
        assert!(OscPattern::compile(ok).is_ok());
    }

    #[test]
    fn brace_close_and_comma_are_class_aware() {
        // Council review (PR #2377): the alternation close-finder and the
        // comma-splitter must skip `}` / `,` that sit inside a `[...]` class.
        // `/{a,[}]}/x`: options are "a" and "[}]" (a class matching literal }).
        let p = OscPattern::compile("/{a,[}]}/x").expect("compiles");
        assert!(p.matches("/a/x"));
        assert!(
            p.matches("/}/x"),
            "the in-class option matches a literal brace"
        );
        assert!(!p.matches("/[/x"), "must NOT misparse into a '[' option");
        // `/{[a,b],c}/x`: top-level options are "[a,b]" (class a|,|b) and "c".
        let p2 = OscPattern::compile("/{[a,b],c}/x").expect("compiles");
        assert!(p2.matches("/a/x"));
        assert!(
            p2.matches("/,/x"),
            "comma is inside the class, a valid member"
        );
        assert!(p2.matches("/b/x"));
        assert!(p2.matches("/c/x"));
        assert!(!p2.matches("/d/x"));
    }

    #[test]
    fn brace_inside_char_class_is_literal_not_alternation() {
        // Copilot review (PR #2377): `{` inside `[...]` must be a literal —
        // compile must not panic, and the class must match a literal `{`.
        let p = OscPattern::compile("/f/[{]/x").expect("compiles, no panic");
        assert!(p.matches("/f/{/x"), "class [{{]] matches a literal '{{'");
        assert!(!p.matches("/f/}/x"));
        // A real alternation outside a class still expands.
        let p2 = OscPattern::compile("/[ab]/{go,stop}").expect("compiles");
        assert!(p2.matches("/a/go"));
        assert!(p2.matches("/b/stop"));
        assert!(!p2.matches("/c/go"));
    }

    #[test]
    fn panic_free_and_bounded_on_adversarial_addresses() {
        // Worst-case star backtracking input: long runs of almost-matching
        // text. Must complete (linear backtracking) and not panic.
        let p = OscPattern::compile("/a*a*a*a*a*b").unwrap();
        let evil = format!("/{}", "a".repeat(2048));
        assert!(!p.matches(&evil));

        // Arbitrary bytes (non-ASCII, embedded NUL-ish, empty).
        let p2 = OscPattern::compile("/f/*").unwrap();
        assert!(!p2.matches(""));
        assert!(!p2.matches("\u{0}\u{ffff}"));
        assert!(
            p2.matches("/f/\u{e9}\u{e9}"),
            "non-ASCII bytes in a part are fine"
        );
    }

    #[test]
    fn empty_address_and_root_edge_cases() {
        assert!(!m("/f", ""));
        assert!(m("/*", "/x"));
        assert!(m("/*", "/"), "* matches empty part");
        assert!(!m("/*", "/x/y"));
    }
}
