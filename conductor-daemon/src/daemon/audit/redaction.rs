// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-027 D13c — PII / secret redaction for audit-log entries.
//!
//! The audit log records every tool execution including its
//! `arguments` and `result` JSON. Both are user-controlled
//! payloads: a tool call to (say) a future `conductor_send_http`
//! tool could legitimately carry an `Authorization: Bearer ...`
//! header, an OAuth `client_secret`, or a webhook URL with a
//! signing token in the query string. Persisting those verbatim
//! in `~/Library/Application Support/conductor/audit.db` (or
//! the equivalent on Linux/Windows) is a credential-disclosure
//! risk: the audit DB lives on disk for 90 days by default, is
//! readable by anything running as the daemon's UID, and can
//! end up in user backups or support bundles.
//!
//! D13c does field-level redaction at INSERT time: parse the
//! JSON, walk for secret-shaped keys, replace those values
//! with `"<redacted:N>"` (preserving length), then persist the
//! redacted JSON. Non-secret fields, the tool name, risk tier,
//! success/failure, and timing all stay intact — that's the
//! audit trail value the operator needs.
//!
//! ## Relationship to D20
//!
//! D20 (`conductor-gui/src-tauri/src/llm/redaction.rs`)
//! redacts secrets headed TO the LLM. D13c redacts secrets
//! headed TO the audit log. Same shape of problem, different
//! output stream — both pre-process JSON values destined for a
//! persistent / external surface that operators don't
//! necessarily trust to keep secrets confidential.
//!
//! Why duplicate the logic instead of sharing? D20 lives in
//! conductor-gui (Tauri-only). conductor-daemon would have to
//! depend on conductor-gui's Tauri-bound code, or both would
//! have to depend on a new shared crate. Both options carry
//! more cost than maintaining ~80 lines of secret-detection
//! patterns in two places. If a third consumer arrives the
//! shared crate becomes worth it; for now, this module is
//! self-contained.
//!
//! Closes part of audit Finding F-10 (audit log content
//! exposure).

use serde_json::Value;

/// Case-insensitive substrings that mark a key as secret-shaped.
/// Match anywhere in the key name.
///
/// **D20 (`conductor-gui/src-tauri/src/llm/redaction.rs`)
/// shares most of this set but is currently MISSING
/// `authorization` / `proxy-authorization`** — those were
/// added here in PR #1032 round-2 to cover the documented
/// motivating example (HTTP Authorization headers in tool
/// arguments). D20 should be brought into sync in a follow-up
/// PR; until then the two redactors diverge on those entries
/// only. New entries added to either side should be added to
/// both (PR #1032 round-3 review, 2026-05-02 — previous
/// "Same set as D20" claim was outdated as of round-2).
const SECRET_KEY_SUBSTRINGS: &[&str] = &[
    "api_key",
    "apikey",
    "api-key",
    "secret",
    "password",
    "passwd",
    "auth_token",
    "authtoken",
    "auth-token",
    "access_token",
    "refresh_token",
    "bearer_token",
    "session_token",
    "private_key",
    "privatekey",
    "private-key",
    "credentials",
    "client_secret",
    "webhook_secret",
    "signing_key",
    // PR #1032 review (2026-05-02): the module docs cite
    // `Authorization: Bearer ...` as a motivating example, but
    // pre-fix `is_secret_key("Authorization")` returned false
    // (no substring/suffix match), so the named header would
    // persist verbatim — defeating the example. Adding both
    // standard HTTP variants.
    "authorization",
    "proxy-authorization",
];

/// Suffix patterns that mark a key as secret-shaped. Anchored
/// to the end of the key so we don't redact e.g. `token_count`
/// or `key_label`.
const SECRET_KEY_SUFFIXES: &[&str] = &["_token", "_secret", "_key", "_password", "_credential"];

/// Top-level public entry point. Takes the JSON-string form
/// the audit logger holds (e.g. `entry.arguments` or
/// `entry.result`) and returns the redacted version. If parsing
/// fails (the field isn't valid JSON), returns the input
/// unchanged — we don't want to drop audit content because of a
/// formatting edge case, and a non-JSON string is unlikely to
/// be carrying a recognisable structured secret anyway.
///
/// Returns the input unchanged for `None` inputs so callers can
/// pipe `entry.arguments` straight through.
pub fn redact_audit_field(field: Option<&str>) -> Option<String> {
    let raw = field?;
    let parsed: Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => return Some(raw.to_string()),
    };
    let mut modified = false;
    let redacted = redact_inner(parsed, false, &mut modified);
    // PR #1032 review (2026-05-03): when no secret-shaped keys
    // were touched, skip the serde re-serialise and return the
    // input verbatim. Pre-fix the fast path always parsed +
    // re-serialised, which doubled memory peak for large
    // unchanged payloads (the parsed `Value` tree + the
    // re-serialised string both held the full content). This
    // check costs one bool comparison on the no-change path
    // and saves one input-sized allocation.
    if !modified {
        return Some(raw.to_string());
    }
    Some(serde_json::to_string(&redacted).unwrap_or_else(|_| raw.to_string()))
}

fn redact_inner(value: Value, parent_key_is_secret: bool, modified: &mut bool) -> Value {
    if parent_key_is_secret {
        // `redact_value` replaces every shape EXCEPT Null (which
        // passes through unchanged — see redact_value docs). Only
        // mark `modified` when the byte output actually differs;
        // marking on Null would defeat the no-roundtrip fast
        // path for `{"api_key": null}` and similar.
        let was_null = matches!(value, Value::Null);
        let new_value = redact_value(value);
        if !was_null {
            *modified = true;
        }
        return new_value;
    }
    match value {
        Value::Object(map) => {
            let redacted: serde_json::Map<String, Value> = map
                .into_iter()
                .map(|(k, v)| {
                    let key_is_secret = is_secret_key(&k);
                    (k, redact_inner(v, key_is_secret, modified))
                })
                .collect();
            Value::Object(redacted)
        }
        Value::Array(arr) => {
            // Reached only when the parent key is NOT secret —
            // the early `parent_key_is_secret` return above
            // replaces a secret-keyed array wholesale. Walk
            // each element with no inherited secret context.
            Value::Array(
                arr.into_iter()
                    .map(|v| redact_inner(v, false, modified))
                    .collect(),
            )
        }
        // A string value under a NON-secret key (#2119): the field name doesn't
        // mark it as a secret, but the VALUE may be a webhook URL carrying a
        // signing token in its query string (`...?token=...`). Key-based
        // redaction alone preserves it verbatim. Redact secret-shaped query
        // parameter values; non-URL strings and URLs without secret params are
        // returned unchanged.
        Value::String(s) => match redact_url_query_secrets(&s) {
            Some(redacted) => {
                *modified = true;
                Value::String(redacted)
            }
            None => Value::String(s),
        },
        other => other,
    }
}

/// Query-parameter names whose VALUES should be treated as secret-shaped (e.g.
/// a webhook signing token in `?token=...`). Combines the key-based secret
/// detector with a few URL-specific short parameter names (#2119).
fn is_secret_query_param(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if is_secret_key(&lower) {
        return true;
    }
    matches!(
        lower.as_str(),
        "token"
            | "access_token"
            | "sig"
            | "signature"
            | "key"
            | "secret"
            | "password"
            | "passwd"
            | "auth"
            | "apikey"
            | "api_key"
            | "sas"
            | "code"
    )
}

/// If `s` is an http(s) URL carrying secret-shaped parameters in its query
/// string OR its fragment, return a copy with those parameter VALUES redacted
/// (#2119), preserving the base URL and non-secret params. The FRAGMENT is
/// covered as well: OAuth implicit-flow redirects deliver the token as
/// `#access_token=...`, so a fragment is just as sensitive as the query string.
/// Returns `None` when there is nothing to redact, so the caller keeps the
/// original string (and the no-change fast path). Only http(s) URLs are
/// considered, so arbitrary text containing `?`/`=` is left untouched.
fn redact_url_query_secrets(s: &str) -> Option<String> {
    // Allocation-free scheme gate: `redact_inner` calls this for EVERY non-secret
    // string, so the common (non-URL) case must exit without allocating. Match
    // the `http://` / `https://` prefix case-insensitively on bytes.
    let b = s.as_bytes();
    let is_http = b.len() >= 7 && b[..7].eq_ignore_ascii_case(b"http://");
    let is_https = b.len() >= 8 && b[..8].eq_ignore_ascii_case(b"https://");
    if !is_http && !is_https {
        return None;
    }
    // Decompose `base [?query] [#fragment]`. The fragment starts at the FIRST
    // '#'; everything before it may carry a `?query`.
    let (before_fragment, fragment) = match s.split_once('#') {
        Some((b, f)) => (b, Some(f)),
        None => (s, None),
    };
    let (base, query) = match before_fragment.split_once('?') {
        Some((b, q)) => (b, Some(q)),
        None => (before_fragment, None),
    };

    let mut redacted_any = false;
    let new_query = query.map(|q| redact_param_pairs(q, &mut redacted_any));
    let new_fragment = fragment.map(|f| redact_param_pairs(f, &mut redacted_any));

    if !redacted_any {
        return None;
    }

    let mut out = String::from(base);
    if let Some(q) = new_query {
        out.push('?');
        out.push_str(&q);
    }
    if let Some(f) = new_fragment {
        out.push('#');
        out.push_str(&f);
    }
    Some(out)
}

/// Redact the values of secret-shaped parameters in an `&`-separated
/// `name=value` string — a URL query string or fragment. Sets `redacted_any`
/// when at least one value was replaced; non-secret pairs are preserved.
fn redact_param_pairs(pairs: &str, redacted_any: &mut bool) -> String {
    pairs
        .split('&')
        .map(|pair| match pair.split_once('=') {
            Some((name, value)) if !value.is_empty() && is_secret_query_param(name) => {
                *redacted_any = true;
                format!("{name}=<redacted:{}>", value.chars().count())
            }
            _ => pair.to_string(),
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn redact_value(value: Value) -> Value {
    match value {
        Value::String(s) => {
            // Length leakage acceptable — gives operators a hint
            // about field shape without exposing the secret.
            Value::String(format!("<redacted:{}>", s.chars().count()))
        }
        Value::Object(_) | Value::Array(_) | Value::Number(_) | Value::Bool(_) => {
            // Structured / numeric / boolean values under a
            // secret key are wholesale replaced.
            Value::String("<redacted>".to_string())
        }
        // `null` passes through unchanged — JSON null carries
        // no information about the secret's value or shape, and
        // treating it as `"<redacted>"` would create false-
        // positive "this field had a secret" signals in audit
        // entries where the field was simply unset.
        Value::Null => Value::Null,
    }
}

fn is_secret_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    if SECRET_KEY_SUBSTRINGS
        .iter()
        .any(|needle| lower.contains(needle))
    {
        return true;
    }
    if SECRET_KEY_SUFFIXES
        .iter()
        .any(|suffix| lower.ends_with(suffix))
    {
        return true;
    }
    false
}

/// Truncate a (post-redaction) result string to a safe size for
/// storage in the audit DB. Truncates at a UTF-8 character
/// boundary (NOT a raw byte slice — that could split a
/// multi-byte character and produce invalid UTF-8).
///
/// **`RESULT_MAX_BYTES` is the hard cap on the FINAL stored
/// string** (PR #1032 round-3 review, 2026-05-02). Pre-fix this
/// truncated the input at `RESULT_MAX_BYTES` then APPENDED
/// `"...[truncated]"`, so the stored string could exceed the
/// cap by the marker length. Now the cut budget is reduced by
/// the marker's byte length so `out.len() <= RESULT_MAX_BYTES`
/// is invariant regardless of marker.
///
/// 10KB is the historical limit from the pre-D13c version of
/// `AuditEntry::with_result`. Kept as the working budget; if
/// the audit DB starts holding tool calls with much larger
/// useful results, raise it deliberately.
pub(crate) const RESULT_MAX_BYTES: usize = 10 * 1024;
pub(crate) const TRUNCATION_MARKER: &str = "...[truncated]";
pub(crate) fn truncate_for_storage(result: String) -> String {
    if result.len() <= RESULT_MAX_BYTES {
        return result;
    }
    // Reserve room for the marker so the final string fits
    // within RESULT_MAX_BYTES.
    let budget = RESULT_MAX_BYTES.saturating_sub(TRUNCATION_MARKER.len());
    // Find the largest char-boundary <= budget so we never
    // split a multi-byte UTF-8 character.
    let mut cut = budget;
    while cut > 0 && !result.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut out = result[..cut].to_string();
    out.push_str(TRUNCATION_MARKER);
    debug_assert!(
        out.len() <= RESULT_MAX_BYTES,
        "truncation overflow: {} > {}",
        out.len(),
        RESULT_MAX_BYTES,
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn none_input_returns_none() {
        assert_eq!(redact_audit_field(None), None);
    }

    #[test]
    fn non_json_input_passes_through_unchanged() {
        // Some tool args might be plain strings rather than
        // JSON objects (e.g. a single shell-command argument).
        // We don't try to parse them; they go in unchanged.
        let input = "plain string with no json";
        assert_eq!(
            redact_audit_field(Some(input)).as_deref(),
            Some(input),
            "non-JSON inputs must not be munged",
        );
    }

    #[test]
    fn redacts_http_authorization_header_key() {
        // PR #1032 review (2026-05-02): the module docs cite
        // `Authorization: Bearer ...` as a motivating example,
        // but pre-fix the matcher didn't have an `authorization`
        // pattern entry. Regression: verify it's caught now.
        let input = json!({
            "method": "POST",
            "Authorization": "Bearer ghp_real_token",
            "Proxy-Authorization": "Basic xyz"
        })
        .to_string();
        let out = redact_audit_field(Some(&input)).expect("Some");
        let parsed: Value = serde_json::from_str(&out).unwrap();
        // method passes through; Authorization + Proxy-Authorization redacted.
        assert_eq!(
            parsed.pointer("/method").and_then(Value::as_str),
            Some("POST"),
        );
        for header in ["/Authorization", "/Proxy-Authorization"] {
            let v = parsed
                .pointer(header)
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("{header} must still be present"));
            assert!(
                v.starts_with("<redacted:"),
                "{header} must be redacted; got {v}",
            );
        }
    }

    #[test]
    fn redacts_top_level_api_key() {
        let secret = "sk-deadbeef12345678"; // 19 chars
        let input = json!({"api_key": secret}).to_string();
        let out = redact_audit_field(Some(&input)).expect("Some");
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            parsed,
            json!({"api_key": format!("<redacted:{}>", secret.chars().count())}),
        );
    }

    #[test]
    fn redacts_keys_with_secret_suffix() {
        // Audit-log-relevant cases: a tool that writes to a
        // database might log `db_password`; an HTTP tool might
        // log `bearer_token`.
        let input = json!({
            "db_password": "hunter2",
            "auth_token":  "ghp_abc",
            "private_key": "-----BEGIN PRIVATE KEY-----...",
        })
        .to_string();
        let out = redact_audit_field(Some(&input)).expect("Some");
        let parsed: Value = serde_json::from_str(&out).unwrap();
        match parsed {
            Value::Object(map) => {
                for (k, v) in &map {
                    assert!(
                        v.as_str().is_some_and(|s| s.starts_with("<redacted:")),
                        "audit field key {k} should be redacted; got {v:?}",
                    );
                }
            }
            other => panic!("expected object after redaction, got {other:?}"),
        }
    }

    #[test]
    fn preserves_non_secret_keys() {
        // Sanity check: ordinary tool args round-trip
        // (modulo serde_json's whitespace / key ordering, which
        // is canonicalised by both sides of the pipe).
        let input = json!({
            "tool_name": "conductor_status",
            "limit": 42,
            "include_stale": true,
        })
        .to_string();
        let out = redact_audit_field(Some(&input)).expect("Some");
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            parsed,
            json!({
                "tool_name": "conductor_status",
                "limit": 42,
                "include_stale": true,
            }),
            "non-secret tool arguments must round-trip unchanged",
        );
    }

    #[test]
    fn redacts_nested_secret() {
        // HTTP-tool style: secret nested in an `auth` object.
        let input = json!({
            "url": "https://api.example.com/notify",
            "headers": {
                "Authorization": "Bearer abc.def.ghi",
                "Content-Type": "application/json"
            },
            "auth": {
                "client_id":     "abc123",
                "client_secret": "shhh-this-is-private"
            }
        })
        .to_string();
        let out = redact_audit_field(Some(&input)).expect("Some");
        let parsed: Value = serde_json::from_str(&out).unwrap();
        // url + headers preserved; auth.client_secret redacted.
        assert_eq!(
            parsed.pointer("/url").and_then(Value::as_str),
            Some("https://api.example.com/notify"),
        );
        assert_eq!(
            parsed.pointer("/auth/client_id").and_then(Value::as_str),
            Some("abc123"),
        );
        let redacted_secret = parsed
            .pointer("/auth/client_secret")
            .and_then(Value::as_str)
            .expect("client_secret str");
        assert!(
            redacted_secret.starts_with("<redacted:"),
            "nested client_secret must be redacted; got {redacted_secret}",
        );
    }

    #[test]
    fn redacts_array_under_secret_key_wholesale() {
        // `secrets` contains the substring "secret", so the
        // matcher flags it as secret-shaped. The whole array is
        // wholesale-replaced — we don't try to keep any of the
        // structure under a secret-named key, because the
        // assumption is the operator chose that name precisely
        // because the contents are sensitive.
        let input = json!({
            "secrets": [
                {"name": "GITHUB_TOKEN", "value": "ghp_real"},
                {"name": "SSH_KEY",       "value": "rsa..."}
            ]
        })
        .to_string();
        let out = redact_audit_field(Some(&input)).expect("Some");
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed, json!({"secrets": "<redacted>"}));
    }

    #[test]
    fn redacts_inside_array_elements_under_non_secret_parent_key() {
        // Tools that take batches: e.g. an HTTP-call array
        // where each element has its own secret-shaped fields.
        // Outer key is benign so the array is walked; each
        // element's `bearer_token` is redacted individually.
        let input = json!({
            "calls": [
                {"url": "https://a", "bearer_token": "tok_1"},
                {"url": "https://b", "bearer_token": "tok_2"}
            ]
        })
        .to_string();
        let out = redact_audit_field(Some(&input)).expect("Some");
        let parsed: Value = serde_json::from_str(&out).unwrap();
        let urls: Vec<&str> = (0..2)
            .map(|i| {
                parsed
                    .pointer(&format!("/calls/{i}/url"))
                    .and_then(Value::as_str)
                    .unwrap()
            })
            .collect();
        assert_eq!(urls, vec!["https://a", "https://b"], "URLs preserved");
        for i in 0..2 {
            let tok = parsed
                .pointer(&format!("/calls/{i}/bearer_token"))
                .and_then(Value::as_str)
                .unwrap();
            assert!(
                tok.starts_with("<redacted:"),
                "calls[{i}].bearer_token must be redacted; got {tok}",
            );
        }
    }

    #[test]
    fn null_under_secret_key_passes_through() {
        // Same convention as D20: null carries no information,
        // so we don't false-positive "this field had a secret"
        // for fields that were genuinely unset.
        let input = json!({"api_key": null}).to_string();
        let out = redact_audit_field(Some(&input)).expect("Some");
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed, json!({"api_key": null}));
    }

    #[test]
    fn redacted_string_length_matches_original_chars() {
        // The `<redacted:N>` length leakage hint must match the
        // original UTF-8 character count (not byte count) so
        // CJK/emoji secrets don't surprise an operator counting
        // by-eye.
        let input = json!({"secret": "😀😀😀"}).to_string();
        let out = redact_audit_field(Some(&input)).expect("Some");
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed, json!({"secret": "<redacted:3>"}));
    }

    #[test]
    fn no_secrets_returns_input_verbatim_no_serde_roundtrip() {
        // PR #1032 review (2026-05-03): when nothing is redacted,
        // the function used to parse + re-serialize through
        // serde_json — doubling memory peak for large unchanged
        // payloads. Now the fast path skips the re-serialize and
        // returns the input string unchanged, so unusual whitespace
        // and key ordering are preserved byte-for-byte.
        //
        // Use a JSON shape that serde_json::to_string would
        // write differently from the input (extra whitespace +
        // explicit key order) so a re-serialize would mutate the
        // bytes and this assertion would fail.
        let input = "{ \"alpha\" : 1 ,\n  \"beta\":\"x\"  }";
        let out = redact_audit_field(Some(input)).expect("Some");
        assert_eq!(
            out, input,
            "no-secret JSON must be returned byte-identical (no serde re-serialise)",
        );
    }

    #[test]
    fn redaction_still_round_trips_on_secret_key_path() {
        // Sanity: the fast-path optimisation MUST NOT bypass the
        // actual redaction when a secret key is present. Verify
        // the output bytes differ from the input (re-serialised
        // tree carries the redacted value).
        let input = "{\"api_key\":\"sk-live-real\",\"keep\":1}";
        let out = redact_audit_field(Some(input)).expect("Some");
        assert_ne!(out, input, "secret-bearing input MUST be modified");
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert!(
            parsed
                .pointer("/api_key")
                .and_then(Value::as_str)
                .unwrap()
                .starts_with("<redacted:"),
            "api_key value must be redacted, not the fast-path passthrough",
        );
        assert_eq!(parsed.pointer("/keep").and_then(Value::as_i64), Some(1));
    }

    #[test]
    fn url_query_string_secret_is_redacted() {
        // #2119 (clawpatch #2103): a webhook URL carrying a signing token in its
        // query string must NOT be persisted verbatim, even though the field
        // key (`url`) is not itself secret-shaped.
        let input = json!({
            "url": "https://hooks.example.com/notify?token=whsec_SUPERSECRET&channel=ops",
            "method": "POST"
        })
        .to_string();
        let out = redact_audit_field(Some(&input)).expect("Some");
        let parsed: Value = serde_json::from_str(&out).unwrap();
        let url = parsed.pointer("/url").and_then(Value::as_str).unwrap();

        assert!(
            !url.contains("whsec_SUPERSECRET"),
            "the token VALUE must be redacted; got {url}"
        );
        assert!(
            url.contains("token=<redacted:"),
            "the redaction marker should replace the token value; got {url}"
        );
        assert!(
            url.contains("channel=ops"),
            "non-secret query params must be preserved; got {url}"
        );
        assert!(
            url.starts_with("https://hooks.example.com/notify?"),
            "the base URL must be preserved; got {url}"
        );
        // Unrelated fields are untouched.
        assert_eq!(
            parsed.pointer("/method").and_then(Value::as_str),
            Some("POST")
        );
    }

    #[test]
    fn url_without_secret_query_params_is_preserved() {
        // A URL with no secret-shaped query params must pass through verbatim
        // (no false-positive redaction, and the no-change fast path holds).
        let input =
            json!({ "url": "https://api.example.com/notify?channel=ops&page=2" }).to_string();
        let out = redact_audit_field(Some(&input)).expect("Some");
        assert!(
            out.contains("https://api.example.com/notify?channel=ops&page=2"),
            "a URL without secret query params must be preserved; got {out}"
        );
    }

    #[test]
    fn non_url_string_with_question_mark_is_untouched() {
        // Guard against over-matching: a plain (non-http) string containing '?'
        // and '=' must not be treated as a URL.
        let input = json!({ "note": "is this a token=value? maybe" }).to_string();
        let out = redact_audit_field(Some(&input)).expect("Some");
        assert!(
            out.contains("is this a token=value? maybe"),
            "non-URL text must be untouched; got {out}"
        );
    }

    #[test]
    fn url_fragment_secret_is_redacted() {
        // #2119 (Council): OAuth implicit-flow redirects carry the token in the
        // URL FRAGMENT (`#access_token=...`), which must be redacted too — even
        // when there is no query string.
        let input = json!({
            "url": "https://app.example.com/callback#access_token=ya29_SECRET&expires_in=3600"
        })
        .to_string();
        let out = redact_audit_field(Some(&input)).expect("Some");
        let parsed: Value = serde_json::from_str(&out).unwrap();
        let url = parsed.pointer("/url").and_then(Value::as_str).unwrap();

        assert!(
            !url.contains("ya29_SECRET"),
            "the fragment access_token must be redacted; got {url}"
        );
        assert!(
            url.contains("access_token=<redacted:"),
            "redaction marker should replace the fragment token; got {url}"
        );
        assert!(
            url.contains("expires_in=3600"),
            "non-secret fragment params preserved; got {url}"
        );
        assert!(
            url.starts_with("https://app.example.com/callback#"),
            "base URL + fragment delimiter preserved; got {url}"
        );
    }
}
