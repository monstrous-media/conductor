// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Plugin id validation — the single source of truth for the
//! character set AND length cap of plugin identifiers used
//! across the codebase.
//!
//! Extracted from `plugin_registry::validate_plugin_id` so the WASM runtime
//! (gated behind `plugin-wasm`, NOT behind `plugin-registry`) can apply the
//! same rules. Without this extraction, the install/discovery
//! flow and the runtime sandbox flow could accept different ids —
//! a divergence risk.
//!
//! The 128-byte length
//! cap moved from `plugin::wasm_runtime::is_safe_plugin_id` into
//! this shared validator. Pre-fix install/discovery accepted
//! 129+ char ids that the runtime then rejected — same install/
//! runtime divergence the earlier extraction was meant to close,
//! but for length rather than character set.
//!
//! Plugin ids are used as filesystem subdirectory names (D10c
//! per-plugin sandbox), as registry directory names, and as the
//! key in trust stores. The character set is intentionally
//! conservative: ASCII alphanumerics + `_` + `-`. No `.` (those
//! belong in `version` fields), no whitespace, no path
//! separators, no shell metacharacters.

/// Maximum permitted plugin id length, in bytes (== chars since
/// the validator only allows ASCII). Conservative; chosen to
/// keep filesystem subdirectory names well below typical
/// per-component limits across all platforms (NTFS / ext4 / APFS
/// all permit 255+, but shorter is friendlier for path handling
/// and log readability).
pub const MAX_PLUGIN_ID_LEN: usize = 128;

/// Validate that `plugin_id` is acceptable as a plugin
/// identifier. Returns `Ok(())` for valid ids, `Err` (with
/// `ErrorKind::InvalidInput`) for empty ids, ids longer than
/// [`MAX_PLUGIN_ID_LEN`], or any character outside `[A-Za-z0-9_-]`.
///
/// **Used by:**
/// - `plugin_registry::install_*` (install-time validation; via
///   re-export)
/// - `plugin::wasm_runtime::is_safe_plugin_id` (runtime
///   sandbox-id validation)
///
/// Both call sites delegate to this function so install-time and
/// runtime can never diverge on either character set or length.
pub fn validate_plugin_id(plugin_id: &str) -> std::io::Result<()> {
    if plugin_id.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "plugin id must not be empty",
        ));
    }
    if plugin_id.len() > MAX_PLUGIN_ID_LEN {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "plugin id is {} bytes; max permitted is {} \
                 (truncate or rename — long ids hurt path \
                 readability and risk per-component filesystem \
                 limits)",
                plugin_id.len(),
                MAX_PLUGIN_ID_LEN,
            ),
        ));
    }
    if !plugin_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "plugin id {plugin_id:?} contains invalid characters; \
                 only ASCII letters, digits, '_' and '-' are allowed"
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_simple_alphanumerics() {
        assert!(validate_plugin_id("spotify").is_ok());
        assert!(validate_plugin_id("Plugin42").is_ok());
        assert!(validate_plugin_id("a").is_ok());
    }

    #[test]
    fn accepts_underscore_and_dash() {
        assert!(validate_plugin_id("obs-control").is_ok());
        assert!(validate_plugin_id("system_utils").is_ok());
        assert!(validate_plugin_id("a-b-c_d").is_ok());
    }

    #[test]
    fn rejects_empty_and_dotted() {
        assert!(validate_plugin_id("").is_err());
        assert!(validate_plugin_id(".").is_err());
        assert!(validate_plugin_id("..").is_err());
        assert!(validate_plugin_id("name.with.dots").is_err());
        assert!(validate_plugin_id(".hidden").is_err());
    }

    #[test]
    fn rejects_overly_long_ids() {
        // Install/discovery and runtime
        // need to agree on the length cap, not just the character
        // set. 128 bytes is the boundary — exactly 128 is fine,
        // 129 isn't. Both call sites delegate to this function
        // so they can't drift.
        let exactly_max: String = "a".repeat(MAX_PLUGIN_ID_LEN);
        let too_long: String = "a".repeat(MAX_PLUGIN_ID_LEN + 1);
        assert!(
            validate_plugin_id(&exactly_max).is_ok(),
            "exactly MAX_PLUGIN_ID_LEN ({MAX_PLUGIN_ID_LEN}) bytes \
             must be accepted",
        );
        assert!(
            validate_plugin_id(&too_long).is_err(),
            "MAX_PLUGIN_ID_LEN+1 ({}) bytes must be rejected — \
             otherwise install accepts an id the WASM sandbox \
             will then refuse, defeating the round-2 unification",
            MAX_PLUGIN_ID_LEN + 1,
        );
    }

    #[test]
    fn rejects_path_separators_and_metachars() {
        for bad in [
            "a/b",
            "a\\b",
            "a b",
            "a\tb",
            "a\nb",
            "a;b",
            "a|b",
            "a`b",
            "a$b",
            "../escape",
            "/abs/path",
        ] {
            assert!(
                validate_plugin_id(bad).is_err(),
                "{bad:?} must be rejected as a plugin id",
            );
        }
    }
}
