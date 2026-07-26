// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! ADR-031 Phase 2A § 4.3 — `[[routes]]` reject-rule validation.
//!
//! Per spec § 4.3 (4 reject rules + the cc/note range correctness fix
//! from spec § 4.1 post-#1139):
//!   1. `from` / `to` MUST reference an existing `[[endpoints]]` alias.
//!   2. Self-referencing route (`from == to`) is rejected.
//!   3. A→B + B→A direct cycle is rejected.
//!   4. Cross-protocol route MUST have a `transform` (otherwise the
//!      route would forward incompatible payloads).
//!   5. `cc_range` / `note_range` with `min > max` is rejected (would
//!      silently match nothing — same failure mode as #1118 cross-bucket
//!      shadowing but harder to diagnose).
//!
//! Overlap warnings (route shadowed by Raw, etc.) land in slice 3.
//!
//! ADR-035: the legacy `[[bindings]]` / `[[connectors]]` blocks are gone;
//! every route source/target resolves against the unified `[[endpoints]]`
//! set. Endpoints used as a route source declare `direction = "Input"`
//! (or `"Bidirectional"`), targets `direction = "Output"` (or
//! `"Bidirectional"`); a `Matcher` endpoint must carry at least one matcher.
//!
//! Spec reference: `docs/signal-routing/ADR-031-implementation-spec.md` § 4.3.

use conductor_core::Config;
use conductor_core::config::validation::validate_config;

fn parse_or_panic(toml: &str) -> Config {
    toml::from_str(toml).expect("config parses")
}

/// Assert that an error exists whose path STARTS WITH `path_prefix`
/// and whose message contains `fragment` (case-insensitive).
///
/// Slice 8 hardening (Council finding on slice 5): the previous
/// `assert_error_about` only matched on message substring, which is
/// brittle — a message mentioning "ghost" could match an error about
/// an entirely different alias. Pinning by `path` ties each test to
/// the specific rule that produced the finding.
fn assert_error_at_path(
    report: &conductor_core::config::validation::ValidationReport,
    path_prefix: &str,
    fragment: &str,
) {
    let matches: Vec<_> = report
        .errors
        .iter()
        .filter(|e| {
            e.path.starts_with(path_prefix)
                && e.message.to_lowercase().contains(&fragment.to_lowercase())
        })
        .collect();
    assert!(
        !matches.is_empty(),
        "expected an error at path starting '{}' mentioning '{}'; got errors: {:#?}",
        path_prefix,
        fragment,
        report.errors
    );
}

fn assert_error_about(
    report: &conductor_core::config::validation::ValidationReport,
    fragment: &str,
) {
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.to_lowercase().contains(&fragment.to_lowercase())),
        "expected an error mentioning '{}'; got: {:#?}",
        fragment,
        report.errors
    );
}

// ── Rule 1: nonexistent endpoint alias ──

#[test]
fn route_from_nonexistent_alias_is_rejected() {
    let cfg = parse_or_panic(
        r#"
[[modes]]
name = "Default"

[[routes]]
from = "ghost"
to = "absynth"

[[endpoints]]
alias = "absynth"
direction = "Output"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Absynth" }]
"#,
    );
    let report = validate_config(&cfg);
    // Slice 8 hardening: assert by path so a stray error mentioning
    // "ghost" elsewhere can't satisfy the test.
    assert_error_at_path(&report, "routes[0].from", "ghost");
}

#[test]
fn route_to_nonexistent_alias_is_rejected() {
    let cfg = parse_or_panic(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "pads"
direction = "Input"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Mikro" }]

[[routes]]
from = "pads"
to = "no-such-thing"
"#,
    );
    let report = validate_config(&cfg);
    // Slice 8 hardening: pin to the specific rule's path.
    assert_error_at_path(&report, "routes[0].to", "no-such-thing");
}

#[test]
fn route_from_and_to_resolving_to_endpoint_validates() {
    // Sanity — legal config must validate cleanly.
    let cfg = parse_or_panic(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "pads"
direction = "Input"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Mikro" }]

[[endpoints]]
alias = "absynth"
direction = "Output"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Absynth" }]

[[routes]]
from = "pads"
to = "absynth"
"#,
    );
    let report = validate_config(&cfg);
    let route_errors: Vec<_> = report
        .errors
        .iter()
        .filter(|e| e.path.starts_with("routes["))
        .collect();
    assert!(
        route_errors.is_empty(),
        "no route errors expected; got: {:#?}",
        route_errors
    );
}

// ── Rule 2: self-referencing route ──

#[test]
fn self_referencing_route_is_rejected() {
    let cfg = parse_or_panic(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "loopy"
direction = "Bidirectional"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Loopy" }]

[[routes]]
from = "loopy"
to = "loopy"
"#,
    );
    let report = validate_config(&cfg);
    // Slice 9 hardening: precise path so a stray "loopy" in some
    // unrelated future error can't satisfy the test.
    assert_error_at_path(&report, "routes[0]", "self-loop");
    assert_error_at_path(&report, "routes[0]", "loopy");
}

// ── Rule 3: A→B + B→A direct cycle ──

#[test]
fn direct_cycle_between_two_routes_is_rejected() {
    // A→B and B→A together form a feedback loop that would emit each
    // event back at the source forever. Spec § 4.3 calls this out
    // explicitly as REJECT.
    let cfg = parse_or_panic(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "a"
direction = "Bidirectional"
type = "Matcher"
matchers = [{ type = "NameContains", value = "DeviceA" }]

[[endpoints]]
alias = "b"
direction = "Bidirectional"
type = "Matcher"
matchers = [{ type = "NameContains", value = "DeviceB" }]

[[routes]]
from = "a"
to = "b"

[[routes]]
from = "b"
to = "a"
"#,
    );
    let report = validate_config(&cfg);
    // Slice 9 hardening: cycle error fires at the SECOND route
    // (`routes[1]`), not the first.
    assert_error_at_path(&report, "routes[1]", "cycle");
}

// ── Rule 4: cross-protocol without transform ──

#[test]
fn cross_protocol_route_without_transform_is_rejected() {
    // MIDI → OSC requires a `transform: SignalTransform::MidiToOsc { ... }`.
    // Without one, we'd be forwarding raw MIDI bytes to a UDP socket
    // that expects OSC framing — guaranteed garbage on the wire.
    let cfg = parse_or_panic(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "pads-midi"
direction = "Input"
type = "Matcher"
protocol = "Midi"
matchers = [{ type = "NameContains", value = "Mikro" }]

[[endpoints]]
alias = "lights-osc"
direction = "Output"
type = "OscEndpoint"
protocol = "Osc"
host = "127.0.0.1"
port = 9000

[[routes]]
from = "pads-midi"
to = "lights-osc"
"#,
    );
    let report = validate_config(&cfg);
    // Slice 9 hardening: precise path. The cross-protocol error
    // fires at `routes[N]` (route-level), not `routes[N].transform`
    // (no field-level path).
    assert_error_at_path(&report, "routes[0]", "protocol");
    assert_error_at_path(&report, "routes[0]", "transform");
}

#[test]
fn cross_protocol_route_with_transform_validates() {
    // Same shape as above but with a transform — should pass.
    let cfg = parse_or_panic(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "pads-midi"
direction = "Input"
type = "Matcher"
protocol = "Midi"
matchers = [{ type = "NameContains", value = "Mikro" }]

[[endpoints]]
alias = "lights-osc"
direction = "Output"
type = "OscEndpoint"
protocol = "Osc"
host = "127.0.0.1"
port = 9000

[[routes]]
from = "pads-midi"
to = "lights-osc"
[routes.transform]
type = "MidiToOsc"
cc_to_address = "/lights/cc"
"#,
    );
    let report = validate_config(&cfg);
    let cross_proto_errors: Vec<_> = report
        .errors
        .iter()
        .filter(|e| {
            e.message.to_lowercase().contains("protocol")
                && e.message.to_lowercase().contains("transform")
        })
        .collect();
    assert!(
        cross_proto_errors.is_empty(),
        "cross-protocol-with-transform must validate; got: {:#?}",
        cross_proto_errors
    );
}

// ── Rule 5: cc_range / note_range min > max ──

#[test]
fn cc_range_min_greater_than_max_is_rejected() {
    // Per spec § 4.1 post-#1139: silent-no-match would be exactly the
    // failure mode #1118 caught (cross-bucket shadowing) but harder
    // to diagnose because no trigger is involved.
    let cfg = parse_or_panic(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "from-c"
direction = "Input"
type = "Matcher"
matchers = [{ type = "NameContains", value = "FromC" }]

[[endpoints]]
alias = "to-c"
direction = "Output"
type = "Matcher"
matchers = [{ type = "NameContains", value = "ToC" }]

[[routes]]
from = "from-c"
to = "to-c"
[routes.filter]
cc_range = [64, 1]
"#,
    );
    let report = validate_config(&cfg);
    assert_error_at_path(&report, "routes[0].filter.cc_range", "cc_range");
}

#[test]
fn note_range_min_greater_than_max_is_rejected() {
    let cfg = parse_or_panic(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "from-n"
direction = "Input"
type = "Matcher"
matchers = [{ type = "NameContains", value = "FromN" }]

[[endpoints]]
alias = "to-n"
direction = "Output"
type = "Matcher"
matchers = [{ type = "NameContains", value = "ToN" }]

[[routes]]
from = "from-n"
to = "to-n"
[routes.filter]
note_range = [96, 36]
"#,
    );
    let report = validate_config(&cfg);
    assert_error_at_path(&report, "routes[0].filter.note_range", "note_range");
}

// ── Slice 4: Copilot review on PR #1161 (drift / hardening) ──

#[test]
fn unknown_endpoint_does_not_also_emit_cycle_error() {
    // ADR-031 #1142 — Copilot finding #2: cycle detection runs on
    // unknown aliases and cascades into a confusing extra "cycle"
    // error. Gate cycle detection on both endpoints being known.
    let cfg = parse_or_panic(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "real"
direction = "Bidirectional"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Real" }]

[[routes]]
from = "ghost-a"
to = "real"

[[routes]]
from = "real"
to = "ghost-a"
"#,
    );
    let report = validate_config(&cfg);
    let cycle_errors: Vec<_> = report
        .errors
        .iter()
        .filter(|e| e.message.to_lowercase().contains("cycle"))
        .collect();
    assert!(
        cycle_errors.is_empty(),
        "cycle check must not run when an endpoint is unknown — only the unknown-alias errors should fire; got cycle errors: {:#?}",
        cycle_errors
    );
}

#[test]
fn filter_channel_out_of_0_15_range_is_rejected() {
    // ADR-031 #1142 — Copilot finding #3: SignalFilter.channels values
    // must be 0-15 (parity with `endpoints[*].channels` per ADR-022 / #751).
    let cfg = parse_or_panic(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "from-bad"
direction = "Input"
type = "Matcher"
matchers = [{ type = "NameContains", value = "FromBad" }]

[[endpoints]]
alias = "to-bad"
direction = "Output"
type = "Matcher"
matchers = [{ type = "NameContains", value = "ToBad" }]

[[routes]]
from = "from-bad"
to = "to-bad"
[routes.filter]
channels = [0, 16]
"#,
    );
    let report = validate_config(&cfg);
    assert_error_at_path(&report, "routes[0].filter.channels", "channel");
    assert_error_at_path(&report, "routes[0].filter.channels", "16");
}

#[test]
fn filter_message_type_sysex_is_rejected() {
    // ADR-031 #1142 — Copilot finding #3: the input pipeline doesn't
    // emit SysEx events yet (mirrors the Raw trigger validator at PR
    // #1105 follow-up). A route with such a filter would silently
    // never match.
    let cfg = parse_or_panic(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "from-mt"
direction = "Input"
type = "Matcher"
matchers = [{ type = "NameContains", value = "FromMt" }]

[[endpoints]]
alias = "to-mt"
direction = "Output"
type = "Matcher"
matchers = [{ type = "NameContains", value = "ToMt" }]

[[routes]]
from = "from-mt"
to = "to-mt"
[routes.filter]
message_types = ["SysEx"]
"#,
    );
    let report = validate_config(&cfg);
    assert_error_at_path(&report, "routes[0].filter.message_types", "sysex");
}

#[test]
fn filter_message_type_channelpressure_is_rejected() {
    // Slice 9 (Council finding): the prior single test was named
    // `..._sysex_or_channelpressure_..._is_rejected` but only exercised
    // SysEx. Split into two tests so each rejected variant has its
    // own pinning. ChannelPressure has the same "input pipeline
    // doesn't emit it yet" rationale as SysEx.
    let cfg = parse_or_panic(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "from-mt"
direction = "Input"
type = "Matcher"
matchers = [{ type = "NameContains", value = "FromMt" }]

[[endpoints]]
alias = "to-mt"
direction = "Output"
type = "Matcher"
matchers = [{ type = "NameContains", value = "ToMt" }]

[[routes]]
from = "from-mt"
to = "to-mt"
[routes.filter]
message_types = ["ChannelPressure"]
"#,
    );
    let report = validate_config(&cfg);
    assert_error_at_path(&report, "routes[0].filter.message_types", "channelpressure");
}

// ── Slice 9: Council finding (transitive-cycle scope) ──

#[test]
fn transitive_cycle_does_not_emit_static_validation_error() {
    // SCOPE PIN per spec § 4.3 + slice-6 doc comment: only direct
    // (depth-1) cycles are detected at config load. A 3-route
    // transitive cycle (A→B→C→A) is intentionally NOT flagged
    // here — the runtime route engine (Phase 2B § 4.5) is the
    // second line of defence (per-event recursion guard, mirrors
    // `MidiRecursionGuard` from ADR-015 D8).
    //
    // This test is a negative control: if a future change adds
    // multi-hop cycle detection, this test will fail and the
    // author must update both the spec and this test deliberately.
    let cfg = parse_or_panic(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "a"
direction = "Bidirectional"
type = "Matcher"
matchers = [{ type = "NameContains", value = "DeviceA" }]

[[endpoints]]
alias = "b"
direction = "Bidirectional"
type = "Matcher"
matchers = [{ type = "NameContains", value = "DeviceB" }]

[[endpoints]]
alias = "c"
direction = "Bidirectional"
type = "Matcher"
matchers = [{ type = "NameContains", value = "DeviceC" }]

[[routes]]
from = "a"
to = "b"

[[routes]]
from = "b"
to = "c"

[[routes]]
from = "c"
to = "a"
"#,
    );
    let report = validate_config(&cfg);
    let cycle_errors: Vec<_> = report
        .errors
        .iter()
        .filter(|e| e.message.to_lowercase().contains("cycle"))
        .collect();
    assert!(
        cycle_errors.is_empty(),
        "Phase 2A scope: transitive 3-route cycles must NOT emit a \
         static cycle error (runtime route engine is the catch-net per \
         spec § 4.3). If you're adding multi-hop detection, update \
         both this test and the spec; got: {:#?}",
        cycle_errors
    );
}

// ── Slice 8: Council finding on slice-5 ──

#[test]
fn disabled_route_still_validates_against_all_rules() {
    // Council finding (slice 5 review): no test pinned what happens
    // when a route has `enabled = false`. Pin the contract: validation
    // is independent of `enabled` — disabled routes still fail
    // validation if they have errors. Skipping validation for disabled
    // routes would let bad config silently accumulate (user toggles
    // the route on later, gets surprise runtime failure).
    let cfg = parse_or_panic(
        r#"
[[modes]]
name = "Default"

[[routes]]
from = "ghost"
to = "absynth"
enabled = false

[[endpoints]]
alias = "absynth"
direction = "Output"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Absynth" }]
"#,
    );
    let report = validate_config(&cfg);
    // Even though the route is disabled, the unknown-alias error
    // for `from = "ghost"` MUST still emit.
    assert_error_at_path(&report, "routes[0].from", "ghost");
}

#[test]
fn disabled_duplicate_route_still_warns() {
    // Same logic for warnings — disabled route shouldn't suppress
    // the duplicate-shape detection. Author needs to know about the
    // duplicate so toggling enabled later doesn't surprise them.
    let cfg = parse_or_panic(
        r#"
[[endpoints]]
alias = "a"
direction = "Bidirectional"
type = "Matcher"
matchers = [{ type = "NameContains", value = "DeviceA" }]

[[endpoints]]
alias = "b"
direction = "Bidirectional"
type = "Matcher"
matchers = [{ type = "NameContains", value = "DeviceB" }]

[[modes]]
name = "Default"

[[routes]]
from = "a"
to = "b"

[[routes]]
from = "a"
to = "b"
enabled = false
"#,
    );
    let report = validate_config(&cfg);
    let dup_warnings: Vec<_> = report
        .warnings
        .iter()
        .filter(|w| {
            w.path.starts_with("routes[1]") && w.message.to_lowercase().contains("duplicate")
        })
        .collect();
    assert!(
        !dup_warnings.is_empty(),
        "disabled route must still emit duplicate warning; got: {:#?}",
        report.warnings
    );
}

// ── Slice 7: Copilot review on slice-5 ──
//
// `expected_transform_variant` returned `Some("MidiToOsc")` as a
// fallback for ANY unsupported cross-protocol pair. That made any
// cross-protocol route + `transform.type = "MidiToOsc"` incorrectly
// validate. Fix: return Unsupported for pairs with no defined variant
// and reject as "unsupported protocol pair" regardless of transform.
//
// NOTE (ADR-039-B #1762 step 3): HID→OSC became a SUPPORTED pair
// (requires `HidToOsc`), so these tests rotated to OSC→Art-Net; then
// ADR-039-A Slice 1b (#2324) made THAT pair supported too (requires
// `OscToArtNet`), so they rotated again to Art-Net→MIDI — Art-Net has
// no input transform variants until ADR-039-C lands. When 039-C makes
// this pair supported, rotate to the next still-unsupported pair.
// HID→OSC's own validation is covered by the two tests further below.

#[test]
fn unsupported_cross_protocol_pair_is_rejected_with_clear_message() {
    // Art-Net → MIDI has no defined `SignalTransform` variant. Even with
    // a transform present, the route must be rejected with an
    // "unsupported protocol pair" error.
    let cfg = parse_or_panic(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "dmx-in"
direction = "Input"
type = "ArtNetEndpoint"
protocol = "ArtNet"
universe = 0

[[endpoints]]
alias = "synth"
direction = "Output"
type = "Matcher"
protocol = "Midi"
matchers = [{ type = "NameContains", value = "Synth" }]

[[routes]]
from = "dmx-in"
to = "synth"
[routes.transform]
type = "MidiToOsc"
cc_to_address = "/lights/cc"
"#,
    );
    let report = validate_config(&cfg);
    assert_error_about(&report, "unsupported");
    assert_error_about(&report, "protocol");
}

#[test]
fn unsupported_cross_protocol_pair_without_transform_also_rejected() {
    // Same shape, no transform — must still reject (not error
    // suggesting a transform that doesn't exist).
    let cfg = parse_or_panic(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "dmx-in"
direction = "Input"
type = "ArtNetEndpoint"
protocol = "ArtNet"
universe = 0

[[endpoints]]
alias = "synth"
direction = "Output"
type = "Matcher"
protocol = "Midi"
matchers = [{ type = "NameContains", value = "Synth" }]

[[routes]]
from = "dmx-in"
to = "synth"
"#,
    );
    let report = validate_config(&cfg);
    assert_error_about(&report, "unsupported");
}

// ── ADR-039-B #1762 step 3: HID→OSC is supported, requires HidToOsc ──

#[test]
fn hid_to_osc_with_wrong_transform_variant_is_rejected_mentioning_hidtoosc() {
    // HID→OSC is now a supported pair, but only with a `HidToOsc`
    // transform. A `MidiToOsc` transform on this pair must be rejected,
    // and the error must name the expected variant so the fix is obvious.
    let cfg = parse_or_panic(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "xbox"
direction = "Input"
type = "Matcher"
protocol = "Hid"
matchers = [{ type = "NameContains", value = "Xbox" }]

[[endpoints]]
alias = "lights-osc"
direction = "Output"
type = "OscEndpoint"
protocol = "Osc"
host = "127.0.0.1"
port = 9000

[[routes]]
from = "xbox"
to = "lights-osc"
[routes.transform]
type = "MidiToOsc"
cc_to_address = "/lights/cc"
"#,
    );
    let report = validate_config(&cfg);
    assert_error_about(&report, "HidToOsc");
}

#[test]
fn hid_to_osc_without_transform_is_rejected_mentioning_hidtoosc() {
    // No transform on a supported cross-protocol pair must error with
    // the required variant name, not "unsupported protocol pair".
    let cfg = parse_or_panic(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "xbox"
direction = "Input"
type = "Matcher"
protocol = "Hid"
matchers = [{ type = "NameContains", value = "Xbox" }]

[[endpoints]]
alias = "lights-osc"
direction = "Output"
type = "OscEndpoint"
protocol = "Osc"
host = "127.0.0.1"
port = 9000

[[routes]]
from = "xbox"
to = "lights-osc"
"#,
    );
    let report = validate_config(&cfg);
    assert_error_about(&report, "HidToOsc");
}

// ── Slice 5: Copilot review on slice-4 finding #1 ──

#[test]
fn cross_protocol_route_with_wrong_transform_variant_is_rejected() {
    // Copilot finding on PR #1161: cross-protocol route with the
    // wrong transform variant currently slips through. A MIDI→OSC
    // route with `transform.type = "Midi"` is forwarding raw MIDI
    // bytes through what's nominally an OSC translation step but
    // the variant is a no-op for the protocol gap.
    let cfg = parse_or_panic(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "pads-midi"
direction = "Input"
type = "Matcher"
protocol = "Midi"
matchers = [{ type = "NameContains", value = "Mikro" }]

[[endpoints]]
alias = "lights-osc"
direction = "Output"
type = "OscEndpoint"
protocol = "Osc"
host = "127.0.0.1"
port = 9000

[[routes]]
from = "pads-midi"
to = "lights-osc"
[routes.transform]
type = "Midi"
channel = 5
"#,
    );
    let report = validate_config(&cfg);
    assert_error_about(&report, "transform");
    assert_error_about(&report, "MidiToOsc");
}

#[test]
fn same_protocol_route_with_midi_transform_validates() {
    // Negative control — Midi→Midi with `Midi` transform is the
    // canonical case, must pass.
    let cfg = parse_or_panic(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "pads"
direction = "Input"
type = "Matcher"
protocol = "Midi"
matchers = [{ type = "NameContains", value = "Mikro" }]

[[endpoints]]
alias = "absynth"
direction = "Output"
type = "Matcher"
protocol = "Midi"
matchers = [{ type = "NameContains", value = "Absynth" }]

[[routes]]
from = "pads"
to = "absynth"
[routes.transform]
type = "Midi"
channel = 9
"#,
    );
    let report = validate_config(&cfg);
    let xform_errors: Vec<_> = report
        .errors
        .iter()
        .filter(|e| e.message.contains("transform"))
        .collect();
    assert!(
        xform_errors.is_empty(),
        "Midi→Midi with Midi transform must validate; got: {:#?}",
        xform_errors
    );
}

#[test]
fn equal_min_max_in_ranges_validates() {
    // Boundary case — `[N, N]` is a valid one-element range.
    let cfg = parse_or_panic(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "from-eq"
direction = "Input"
type = "Matcher"
matchers = [{ type = "NameContains", value = "FromEq" }]

[[endpoints]]
alias = "to-eq"
direction = "Output"
type = "Matcher"
matchers = [{ type = "NameContains", value = "ToEq" }]

[[routes]]
from = "from-eq"
to = "to-eq"
[routes.filter]
cc_range = [10, 10]
note_range = [60, 60]
"#,
    );
    let report = validate_config(&cfg);
    let range_errors: Vec<_> = report
        .errors
        .iter()
        .filter(|e| e.message.contains("cc_range") || e.message.contains("note_range"))
        .collect();
    assert!(
        range_errors.is_empty(),
        "equal min/max must validate; got: {:#?}",
        range_errors
    );
}

// ── Slice 2 (ADR-036 D1): route `modes` references + `phase` rules ──

#[test]
fn route_referencing_nonexistent_mode_is_rejected() {
    // A route scoped to a mode that no [[modes]] block declares can
    // never fire — surface it at config load with a path that points
    // at the exact offending entry.
    let cfg = parse_or_panic(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "pads"
direction = "Input"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Mikro" }]

[[endpoints]]
alias = "absynth"
direction = "Output"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Absynth" }]

[[routes]]
from = "pads"
to = "absynth"
modes = ["Ghost"]
"#,
    );
    let report = validate_config(&cfg);
    assert_error_at_path(&report, "routes[0].modes[0]", "Ghost");
}

#[test]
fn route_with_declared_modes_validates_cleanly() {
    // Negative control — a route scoped to a declared mode must not
    // emit a mode-reference error.
    let cfg = parse_or_panic(
        r#"
[[modes]]
name = "Drums"

[[modes]]
name = "Keys"

[[endpoints]]
alias = "pads"
direction = "Input"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Mikro" }]

[[endpoints]]
alias = "absynth"
direction = "Output"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Absynth" }]

[[routes]]
from = "pads"
to = "absynth"
modes = ["Drums", "Keys"]
"#,
    );
    let report = validate_config(&cfg);
    let mode_errors: Vec<_> = report
        .errors
        .iter()
        .filter(|e| e.path.contains(".modes["))
        .collect();
    assert!(
        mode_errors.is_empty(),
        "routes scoped to declared modes must validate; got: {:#?}",
        mode_errors
    );
}

// ADR-036 Phase 3: the route `phase` field was removed. `pre_mapping` is no
// longer a deprecation warning — it is rejected at config load
// (`route_phase_removed_test.rs`). The former warning/negative-control tests
// for `phase` were deleted with the field.

#[test]
fn duplicate_routes_with_same_mode_scope_warn() {
    // Two routes with identical from/to/filter/transform AND overlapping
    // mode scope at the same phase double-send within that scope —
    // warn, parity with the existing exact-duplicate check.
    let cfg = parse_or_panic(
        r#"
[[endpoints]]
alias = "a"
direction = "Bidirectional"
type = "Matcher"
matchers = [{ type = "NameContains", value = "DeviceA" }]

[[endpoints]]
alias = "b"
direction = "Bidirectional"
type = "Matcher"
matchers = [{ type = "NameContains", value = "DeviceB" }]

[[modes]]
name = "Drums"

[[routes]]
from = "a"
to = "b"
modes = ["Drums"]

[[routes]]
from = "a"
to = "b"
modes = ["Drums"]
"#,
    );
    let report = validate_config(&cfg);
    let dup_warnings: Vec<_> = report
        .warnings
        .iter()
        .filter(|w| {
            w.path.starts_with("routes[1]") && w.message.to_lowercase().contains("duplicate")
        })
        .collect();
    assert!(
        !dup_warnings.is_empty(),
        "same-mode-scope duplicate routes must warn; got: {:#?}",
        report.warnings
    );
}

#[test]
fn routes_with_disjoint_modes_do_not_warn_as_duplicate() {
    // Same from/to/filter/transform but disjoint mode scopes never
    // both fire for the same event — legitimate, must not warn.
    let cfg = parse_or_panic(
        r#"
[[endpoints]]
alias = "a"
direction = "Bidirectional"
type = "Matcher"
matchers = [{ type = "NameContains", value = "DeviceA" }]

[[endpoints]]
alias = "b"
direction = "Bidirectional"
type = "Matcher"
matchers = [{ type = "NameContains", value = "DeviceB" }]

[[modes]]
name = "Drums"

[[modes]]
name = "Keys"

[[routes]]
from = "a"
to = "b"
modes = ["Drums"]

[[routes]]
from = "a"
to = "b"
modes = ["Keys"]
"#,
    );
    let report = validate_config(&cfg);
    let dup_warnings: Vec<_> = report
        .warnings
        .iter()
        .filter(|w| w.message.to_lowercase().contains("duplicate"))
        .collect();
    assert!(
        dup_warnings.is_empty(),
        "disjoint mode scopes are not duplicates; got: {:#?}",
        dup_warnings
    );
}
