// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

// ADR-034 §D5 — ConfigRevision newtype tests.

use conductor_core::config::ConfigRevision;

#[test]
fn display_is_lowercase_hex_64_chars() {
    // SHA-256 of the empty input — well-known constant.
    let rev = ConfigRevision::from_canonical_bytes(b"");
    let s = rev.to_string();
    assert_eq!(s.len(), 64);
    assert!(
        s.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    );
    assert_eq!(
        s,
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[test]
fn debug_prefixes_with_typename() {
    let rev = ConfigRevision::from_canonical_bytes(b"");
    let dbg = format!("{rev:?}");
    assert!(dbg.starts_with("ConfigRevision("));
    assert!(dbg.contains("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"));
}

#[test]
fn serde_roundtrip_through_json() {
    let rev = ConfigRevision::from_canonical_bytes(b"hello");
    let json = serde_json::to_string(&rev).expect("serialise");
    // Hex string representation, length-66 including quotes.
    assert_eq!(json.len(), 66);
    assert!(json.starts_with('"') && json.ends_with('"'));

    let back: ConfigRevision = serde_json::from_str(&json).expect("deserialise");
    assert_eq!(back, rev);
    assert_eq!(back.to_string(), rev.to_string());
}

#[test]
fn deserialise_rejects_wrong_length() {
    let too_short = "\"abc\"";
    let result: Result<ConfigRevision, _> = serde_json::from_str(too_short);
    assert!(result.is_err());

    let too_long = format!("\"{}\"", "a".repeat(65));
    let result: Result<ConfigRevision, _> = serde_json::from_str(&too_long);
    assert!(result.is_err());
}

#[test]
fn deserialise_rejects_non_hex_chars() {
    let not_hex = format!("\"{}\"", "z".repeat(64));
    let result: Result<ConfigRevision, _> = serde_json::from_str(&not_hex);
    assert!(result.is_err());
}

#[test]
fn deserialise_rejects_uppercase_hex() {
    // Docstring + error message both claim "lowercase hex". Enforce it
    // — accepting uppercase would create an asymmetric round-trip where
    // input "ABCD..." is accepted but Display emits "abcd...".
    let uppercase = format!("\"{}\"", "A".repeat(64));
    let result: Result<ConfigRevision, _> = serde_json::from_str(&uppercase);
    assert!(
        result.is_err(),
        "uppercase hex must be rejected to preserve lowercase-canonical contract"
    );
}

#[test]
fn deserialise_does_not_panic_on_64_byte_multibyte_string() {
    // Critical DoS regression test:
    // A 64-byte string with multi-byte UTF-8 chars has fewer than 64
    // chars. The old implementation sliced by byte index which panics
    // on non-char boundaries. Must return Err, never panic.
    //
    // "é" = 2 bytes, "a" = 1 byte. "aé" + 61×"a" = 1 + 2 + 61 = 64 bytes
    // but only 63 chars.
    let payload = format!("aé{}", "a".repeat(61));
    assert_eq!(payload.len(), 64, "fixture must be exactly 64 bytes");
    assert_ne!(
        payload.chars().count(),
        64,
        "fixture must have fewer than 64 chars"
    );

    let json = format!("\"{payload}\"");
    let result: Result<ConfigRevision, _> = serde_json::from_str(&json);
    assert!(
        result.is_err(),
        "must reject malformed input without panicking"
    );
}

#[test]
fn deserialise_rejects_plus_sign() {
    // `u8::from_str_radix(_, 16)` accepts `+` prefixes natively. Our
    // contract demands strict hex; the chars().all() check catches it.
    let with_plus = format!("\"+{}\"", "a".repeat(63));
    let result: Result<ConfigRevision, _> = serde_json::from_str(&with_plus);
    assert!(result.is_err(), "must reject `+` sign prefix");
}

#[test]
fn equality_distinguishes_different_bytes() {
    let a = ConfigRevision::from_canonical_bytes(b"foo");
    let b = ConfigRevision::from_canonical_bytes(b"bar");
    assert_ne!(a, b);

    let a2 = ConfigRevision::from_canonical_bytes(b"foo");
    assert_eq!(a, a2);
}
