// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

// ADR-034 §D5 — canonical TOML serialiser tests.
//
// Verifies the behavioural invariants of canonical form (see module
// docs for `conductor_core::config::canonical`) and locks in
// cross-`toml`-version stability via a stored golden hash.

use conductor_core::Config;
use conductor_core::config::{ConfigRevision, canonical};
use sha2::{Digest, Sha256};

const MINIMAL_TOML: &str = include_str!("data/canonical/minimal.toml");

/// Locked SHA-256 of the canonical bytes of `minimal.toml`. If a
/// `toml` crate upgrade changes the byte output, this test fails and
/// the canonical serialiser needs a compensating change to preserve
/// the contract — that's the whole point of pinning a canonical form.
const GOLDEN_MINIMAL_SHA256: &str = include_str!("data/canonical/minimal.sha256").trim_ascii();

fn parse_minimal() -> Config {
    toml::from_str(MINIMAL_TOML).expect("minimal fixture parses")
}

#[test]
fn serialise_produces_valid_utf8_with_no_bom_or_crlf() {
    let config = parse_minimal();
    let bytes = canonical::serialise(&config).expect("serialise");

    // UTF-8.
    let s = std::str::from_utf8(&bytes).expect("output is UTF-8");
    // No BOM (U+FEFF).
    assert!(
        !s.starts_with('\u{FEFF}'),
        "canonical output must not start with a BOM"
    );
    // No CRLF.
    assert!(
        !s.contains('\r'),
        "canonical output must use LF endings, not CRLF"
    );
}

#[test]
fn serialise_strips_comments() {
    let config = parse_minimal();
    let bytes = canonical::serialise(&config).expect("serialise");
    let s = std::str::from_utf8(&bytes).unwrap();
    // Comment characters appear only inside string values, never as
    // line-leading comments. The minimal fixture's `#` comments must
    // be gone after canonicalisation.
    for line in s.lines() {
        assert!(
            !line.trim_start().starts_with('#'),
            "canonical output must not contain comment lines, got: {line:?}"
        );
    }
}

#[test]
fn serialise_is_round_trip_idempotent() {
    let config = parse_minimal();
    let first = canonical::serialise(&config).expect("first serialise");

    // Parse the canonical output back, then re-serialise; bytes must
    // be byte-identical. (Validates that canonical form is a true
    // fixed point of parse+serialise.)
    let first_str = std::str::from_utf8(&first).expect("output is utf-8");
    let reparsed: Config = toml::from_str(first_str).expect("canonical output reparses");
    let second = canonical::serialise(&reparsed).expect("second serialise");
    assert_eq!(
        first, second,
        "canonical form must be idempotent under parse+serialise"
    );
}

#[test]
fn serialise_sorts_keys_lexicographically() {
    // Build a Config that, when serialised non-canonically, would
    // emit `[advanced_settings]` after `[[modes]]` (alphabetic order
    // of the `Config` struct fields differs from declaration order).
    // The canonical form must produce keys in lex order at every
    // table level.
    let config = parse_minimal();
    let bytes = canonical::serialise(&config).expect("serialise");
    let s = std::str::from_utf8(&bytes).unwrap();

    // Find the top-level table headers (`[name]` lines) and verify
    // they appear in sorted order.
    let headers: Vec<&str> = s
        .lines()
        .filter(|l| l.starts_with('[') && !l.starts_with("[["))
        .collect();
    let mut sorted_headers = headers.clone();
    sorted_headers.sort();
    assert_eq!(
        headers, sorted_headers,
        "top-level table headers must appear in lex order"
    );
}

#[test]
fn serialise_preserves_array_of_tables_insertion_order() {
    let two_modes = r#"
[[modes]]
name = "Alpha"
color = "red"
mappings = []

[[modes]]
name = "Beta"
color = "blue"
mappings = []
"#;
    let config: Config = toml::from_str(two_modes).expect("two-mode fixture parses");
    let bytes = canonical::serialise(&config).expect("serialise");
    let s = std::str::from_utf8(&bytes).unwrap();

    // Alpha must appear before Beta in the output — array order is
    // positionally significant in TOML.
    let alpha = s.find("Alpha").expect("Alpha appears in output");
    let beta = s.find("Beta").expect("Beta appears in output");
    assert!(
        alpha < beta,
        "array-of-tables order must be preserved (Alpha at {alpha}, Beta at {beta})"
    );
}

#[test]
fn nested_table_keys_are_sorted() {
    // Verify lex sort applies inside `[advanced_settings]`, not just
    // at the top level (Council R1: Opus §3.1 / GPT-5.4 Issue 3).
    let config = parse_minimal();
    let bytes = canonical::serialise(&config).expect("serialise");
    let s = std::str::from_utf8(&bytes).unwrap();

    // Extract the body of `[advanced_settings]` — all lines from the
    // header to the next blank line / next header.
    let start = s.find("[advanced_settings]").expect("section present");
    let body = &s[start..];
    let scalar_lines: Vec<&str> = body
        .lines()
        .skip(1) // skip the `[advanced_settings]` header line
        .take_while(|l| !l.is_empty() && !l.starts_with('['))
        .collect();

    // Collect keys; assert lex order.
    let keys: Vec<&str> = scalar_lines
        .iter()
        .filter_map(|l| l.split_once(" = ").map(|(k, _)| k.trim()))
        .collect();
    assert!(
        keys.len() >= 2,
        "expected multiple scalar keys, got {keys:?}"
    );
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(
        keys, sorted,
        "nested scalar keys in [advanced_settings] must be lex-sorted"
    );
}

#[test]
fn keys_inside_array_of_tables_are_sorted() {
    // Verify sort applies to scalar keys inside `[[modes]]` entries.
    let multi = r#"
[[modes]]
name = "M"
color = "red"
mappings = []
"#;
    let config: Config = toml::from_str(multi).expect("fixture parses");
    let bytes = canonical::serialise(&config).expect("serialise");
    let s = std::str::from_utf8(&bytes).unwrap();

    // Find the `[[modes]]` block.
    let start = s.find("[[modes]]").expect("array-of-tables present");
    let block = &s[start..];

    let keys: Vec<&str> = block
        .lines()
        .skip(1)
        .take_while(|l| !l.is_empty() && !l.starts_with('['))
        .filter_map(|l| l.split_once(" = ").map(|(k, _)| k.trim()))
        .collect();

    assert!(
        keys.len() >= 2,
        "expected multiple scalar keys in modes block, got {keys:?}"
    );
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(
        keys, sorted,
        "scalar keys inside [[modes]] entry must be lex-sorted"
    );
}

#[test]
fn cross_run_hash_matches_golden() {
    let config = parse_minimal();
    let bytes = canonical::serialise(&config).expect("serialise");

    let actual = hex_digest(&bytes);
    assert_eq!(
        actual.as_str(),
        GOLDEN_MINIMAL_SHA256,
        "canonical hash of minimal.toml drifted — `toml` crate upgrade \
         or canonical-serialiser change broke the contract. If \
         intentional, regenerate `tests/data/canonical/minimal.sha256` \
         after manual review"
    );
}

#[test]
fn config_revision_from_canonical_bytes_matches_direct_sha256() {
    let config = parse_minimal();
    let bytes = canonical::serialise(&config).expect("serialise");

    let rev = ConfigRevision::from_canonical_bytes(&bytes);
    let direct = hex_digest(&bytes);
    assert_eq!(rev.to_string(), direct);
}

// #1356 — `MidiToArtNet { cc_to_dmx: HashMap<u8, u16>, note_to_dmx:
// HashMap<u8, u16> }` failed canonical serialise on any populated map
// because TOML requires string keys. Daemon startup panicked at
// `live_config` init for any real Art-Net config. Pure transform tests
// in slice 5 / slice 6 missed this because they bypassed canonical;
// the existing exclusion tests used empty maps which serialise as `{}`
// and pass.
//
// These tests pin the fix: populated u8-keyed maps must serialise to
// TOML and round-trip through parse.

#[test]
fn serialise_handles_populated_midi_to_artnet_cc_to_dmx() {
    use conductor_core::config::types::{RouteConfig, SignalTransform};
    let mut cc_to_dmx = std::collections::HashMap::new();
    cc_to_dmx.insert(7u8, 50u16); // CC 7 → DMX channel 50
    cc_to_dmx.insert(11u8, 51u16); // CC 11 → DMX channel 51
    let mut config = parse_minimal();
    config.routes = vec![RouteConfig {
        from: "pads".into(),
        to: "lights".into(),
        transform: Some(SignalTransform::MidiToArtNet {
            cc_to_dmx,
            note_to_dmx: std::collections::HashMap::new(),
        }),
        filter: None,
        enabled: true,
        description: None,
        modes: Vec::new(),
    }];

    let bytes = canonical::serialise(&config).expect(
        "populated MidiToArtNet cc_to_dmx must serialise to TOML; \
         #1356 was that u8 keys broke canonical (TOML requires string keys)",
    );
    let s = std::str::from_utf8(&bytes).expect("UTF-8");
    // Spot-check: the CC numbers must appear as string keys in the output.
    // Exact key formatting (`"7"` vs `7 = ...`) is the toml emitter's call,
    // but the digits must be present somewhere.
    assert!(s.contains("7") && s.contains("11") && s.contains("50") && s.contains("51"));
}

#[test]
fn serialise_populated_midi_to_artnet_round_trips_through_parse() {
    use conductor_core::config::types::{RouteConfig, SignalTransform};
    let mut cc_to_dmx = std::collections::HashMap::new();
    cc_to_dmx.insert(7u8, 50u16);
    let mut note_to_dmx = std::collections::HashMap::new();
    note_to_dmx.insert(60u8, 100u16);
    note_to_dmx.insert(127u8, 200u16); // edge: max 7-bit u8 value
    let mut config = parse_minimal();
    config.routes = vec![RouteConfig {
        from: "pads".into(),
        to: "lights".into(),
        transform: Some(SignalTransform::MidiToArtNet {
            cc_to_dmx: cc_to_dmx.clone(),
            note_to_dmx: note_to_dmx.clone(),
        }),
        filter: None,
        enabled: true,
        description: None,
        modes: Vec::new(),
    }];

    let bytes = canonical::serialise(&config).expect("serialise");
    let s = std::str::from_utf8(&bytes).expect("UTF-8");
    let reparsed: Config = toml::from_str(s).expect("canonical output must reparse");

    // The u8 keys must survive a round-trip through TOML string keys
    // — same values, same channels. Validates the serializer
    // correctly parses back via FromStr.
    let route = reparsed.routes.first().expect("route survives round-trip");
    let Some(SignalTransform::MidiToArtNet {
        cc_to_dmx: cc_back,
        note_to_dmx: note_back,
    }) = route.transform.as_ref()
    else {
        panic!(
            "transform shape changed under round-trip: {:?}",
            route.transform
        );
    };
    assert_eq!(cc_back, &cc_to_dmx, "cc_to_dmx mismatch after round-trip");
    assert_eq!(
        note_back, &note_to_dmx,
        "note_to_dmx mismatch after round-trip"
    );
}

#[test]
fn serialise_rejects_or_normalises_out_of_range_u8_keys_via_typed_round_trip() {
    use conductor_core::config::types::RouteConfig;

    // Defensive: the `cc_to_dmx` map is keyed by `u8`, so the deserialiser must
    // reject keys that fail FromStr (non-numeric) or parse outside 0-255 —
    // otherwise a config could serialize one way and parse another, or silently
    // drop the entry.
    //
    // #1519: the fixture must be a *RouteConfig* (root `from`/`to` + a
    // `[transform]` table), NOT a `[[routes]]` array. The old fixture used
    // `[[routes]]` and deserialised as `RouteConfig`, so it errored on the
    // shape mismatch BEFORE the u8 key was ever parsed — a false positive that
    // would pass even if bad keys were accepted. We also assert the error names
    // the offending key so an unrelated missing-field error can't satisfy it.

    // Non-numeric key.
    let non_numeric = r#"
from = "pads"
to = "lights"
enabled = true
[transform]
type = "MidiToArtNet"
cc_to_dmx = { "not_a_number" = 50 }
"#;
    let err = toml::from_str::<RouteConfig>(non_numeric.trim())
        .expect_err("non-numeric u8 key must error, not silently drop");
    assert!(
        err.to_string().contains("not_a_number"),
        "error should name the offending key; got: {err}"
    );

    // Out-of-range key (256 > u8::MAX) — covers the "out-of-range" half of the
    // test name.
    let out_of_range = r#"
from = "pads"
to = "lights"
enabled = true
[transform]
type = "MidiToArtNet"
cc_to_dmx = { "256" = 50 }
"#;
    let err = toml::from_str::<RouteConfig>(out_of_range.trim())
        .expect_err("out-of-range u8 key (256) must error");
    assert!(
        err.to_string().contains("256"),
        "error should name the offending key; got: {err}"
    );

    // Sanity: a VALID numeric key parses cleanly, so the cases above fail for
    // the right reason (the key), not an unrelated shape problem.
    let valid = r#"
from = "pads"
to = "lights"
enabled = true
[transform]
type = "MidiToArtNet"
cc_to_dmx = { "5" = 50 }
"#;
    toml::from_str::<RouteConfig>(valid.trim()).expect("a valid numeric u8 key must parse");
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}
