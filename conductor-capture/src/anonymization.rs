// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Anonymization pipeline for captured patterns
//!
//! Removes personally identifiable information (PII) before upload:
//! - Device serial numbers
//! - Absolute timestamps (capture creation time dropped; events keep relative offsets)
//! - User IDs and file paths
//! - System information
//!
//! ⚠ PII redaction of free-text fields is heuristic ([`check_for_pii`]) and
//! can produce false positives on common words — e.g. "home", "address",
//! "token" — which cause the whole field to be replaced with `[redacted]`.
//! Always review anonymized output before public upload.

// Allow clippy warnings during development phase
#![allow(clippy::collapsible_if)]

use sha2::{Digest, Sha256};

use crate::capture::{CaptureMetadata, CapturedEvent};
use crate::storage::StoredCapture;

/// Anonymize a captured pattern
pub fn anonymize_capture(capture: &StoredCapture) -> AnonymizedCapture {
    AnonymizedCapture {
        // Generate anonymous ID from original ID
        id_hash: hash_id(&capture.id.to_string()),

        // Name is free user text — redact heuristically if it looks like PII.
        name: redact_if_pii(&capture.name),
        privacy: capture.privacy,

        // Capture creation time is intentionally dropped: a hash of it cannot
        // preserve ordering and a low-entropy timestamp hash is reversible.
        // Events retain only relative offsets.
        metadata: anonymize_metadata(&capture.metadata),
        events: anonymize_events(&capture.events),
    }
}

/// Anonymized capture — PII heuristically redacted; review before public upload
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AnonymizedCapture {
    /// Hash of original ID (for deduplication)
    pub id_hash: String,

    /// Name — user-provided free text, heuristically redacted if PII is detected
    pub name: String,

    /// Privacy level
    pub privacy: crate::privacy::PrivacyLevel,

    /// Anonymized metadata
    pub metadata: AnonymizedMetadata,

    /// Events with relative timestamps only
    pub events: Vec<CapturedEvent>,
}

/// Anonymized metadata
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AnonymizedMetadata {
    /// Tags — user-provided free text, heuristically redacted if PII is detected
    pub tags: Vec<String>,

    /// Description — user-provided free text, heuristically redacted if PII is detected
    pub description: Option<String>,

    /// Protocol
    pub protocol: String,

    /// Duration
    pub duration_ms: Option<u64>,

    /// Event count
    pub event_count: usize,

    /// Conductor version (safe to share)
    pub conductor_version: String,
}

/// Anonymize metadata
fn anonymize_metadata(metadata: &CaptureMetadata) -> AnonymizedMetadata {
    AnonymizedMetadata {
        // Tags and description are free user text — redact PII heuristically.
        tags: metadata.tags.iter().map(|t| redact_if_pii(t)).collect(),
        description: redact_opt_if_pii(&metadata.description),
        protocol: metadata.protocol.clone(),
        duration_ms: metadata.duration_ms,
        event_count: metadata.event_count,
        conductor_version: metadata.conductor_version.clone(),
    }
}

/// Anonymize events (relative timestamps; redacts PII in the comment field)
fn anonymize_events(events: &[CapturedEvent]) -> Vec<CapturedEvent> {
    // Timestamps are already relative, but the optional `comment` field is
    // free user text and can carry PII — redact it heuristically.
    //
    // MAINTENANCE: if `CapturedEvent` gains another free-text field, it MUST
    // be redacted here too. The struct-update `..e.clone()` below copies any
    // new field verbatim and would silently bypass anonymization.
    events
        .iter()
        .map(|e| CapturedEvent {
            comment: redact_opt_if_pii(&e.comment),
            ..e.clone()
        })
        .collect()
}

/// Placeholder substituted for any user-provided text flagged as PII.
const REDACTED: &str = "[redacted]";

/// Redact `text` if the heuristic PII detector flags it.
///
/// Returns the original text when no PII is detected. [`check_for_pii`] is a
/// heuristic — callers must still treat free-text fields as best-effort
/// sanitised, not provably PII-free.
fn redact_if_pii(text: &str) -> String {
    if check_for_pii(text).is_empty() {
        text.to_string()
    } else {
        REDACTED.to_string()
    }
}

/// [`redact_if_pii`] for optional text fields.
fn redact_opt_if_pii(text: &Option<String>) -> Option<String> {
    text.as_deref().map(redact_if_pii)
}

/// Hash an ID for anonymization
fn hash_id(id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(id.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Check if text contains potential PII
pub fn check_for_pii(text: &str) -> Vec<PiiWarning> {
    let mut warnings = Vec::new();

    // Check for email addresses
    if text.contains('@') && text.contains('.') {
        warnings.push(PiiWarning::EmailAddress);
    }

    // Check for file paths
    if text.contains('/') || text.contains('\\') {
        if text.contains("Users") || text.contains("home") {
            warnings.push(PiiWarning::FilePath);
        }
    }

    // Check for common PII patterns
    let lower = text.to_lowercase();
    if lower.contains("password") || lower.contains("secret") || lower.contains("token") {
        warnings.push(PiiWarning::Credentials);
    }

    if lower.contains("phone") || lower.contains("address") {
        warnings.push(PiiWarning::ContactInfo);
    }

    warnings
}

/// Potential PII warning
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PiiWarning {
    EmailAddress,
    FilePath,
    Credentials,
    ContactInfo,
    Other(String),
}

impl PiiWarning {
    pub fn message(&self) -> &str {
        match self {
            PiiWarning::EmailAddress => "Email address detected",
            PiiWarning::FilePath => "File path detected",
            PiiWarning::Credentials => "Credentials detected",
            PiiWarning::ContactInfo => "Contact information detected",
            PiiWarning::Other(msg) => msg,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    #[test]
    fn test_hash_id() {
        let id = "test-id";
        let hash1 = hash_id(id);
        let hash2 = hash_id(id);

        // Same input should produce same hash
        assert_eq!(hash1, hash2);

        // Hash should be 64 characters (SHA256 hex)
        assert_eq!(hash1.len(), 64);
    }

    #[test]
    fn test_check_for_pii() {
        // Email
        let warnings = check_for_pii("Contact me at user@example.com");
        assert!(warnings.contains(&PiiWarning::EmailAddress));

        // File path
        let warnings = check_for_pii("/Users/john/Documents/file.txt");
        assert!(warnings.contains(&PiiWarning::FilePath));

        // Credentials
        let warnings = check_for_pii("My password is secret123");
        assert!(warnings.contains(&PiiWarning::Credentials));

        // Clean text
        let warnings = check_for_pii("Fighting game combo sequence");
        assert!(warnings.is_empty());
    }

    #[test]
    fn test_anonymize_capture() {
        let capture = StoredCapture {
            version: "3.1".to_string(),
            id: Uuid::new_v4(),
            name: "test-capture".to_string(),
            privacy: crate::privacy::PrivacyLevel::Public,
            created_at: Utc::now(),
            metadata: CaptureMetadata {
                tags: vec!["test".to_string()],
                description: Some("Test capture".to_string()),
                protocol: "midi".to_string(),
                duration_ms: Some(5000),
                event_count: 10,
                conductor_version: "3.1.0".to_string(),
            },
            events: Vec::new(),
        };

        let anonymized = anonymize_capture(&capture);

        // Check that ID is hashed
        assert_ne!(anonymized.id_hash, capture.id.to_string());
        assert_eq!(anonymized.id_hash.len(), 64); // SHA256 hex length

        // Check that metadata is preserved
        assert_eq!(anonymized.metadata.protocol, "midi");
        assert_eq!(anonymized.metadata.event_count, 10);
    }

    #[test]
    fn test_anonymize_redacts_pii() {
        use crate::capture::SerializableInputEvent;

        let capture = StoredCapture {
            version: "3.1".to_string(),
            id: Uuid::new_v4(),
            name: "alice@example.com".to_string(),
            privacy: crate::privacy::PrivacyLevel::Public,
            created_at: Utc::now(),
            metadata: CaptureMetadata {
                tags: vec!["/Users/alice/secrets".to_string(), "clean-tag".to_string()],
                description: Some("see token in file".to_string()),
                protocol: "midi".to_string(),
                duration_ms: Some(5000),
                event_count: 1,
                conductor_version: "3.1.0".to_string(),
            },
            events: vec![CapturedEvent {
                timestamp_ms: 0,
                event: SerializableInputEvent::PadPressed {
                    pad: 1,
                    velocity: 100,
                },
                comment: Some("my password is hunter2".to_string()),
            }],
        };

        let anon = anonymize_capture(&capture);

        // A name containing an email address must not survive verbatim.
        assert!(
            !anon.name.contains("alice@example.com"),
            "PII name leaked into AnonymizedCapture"
        );
        // A tag containing a file path must be redacted; the clean tag stays.
        assert!(
            !anon
                .metadata
                .tags
                .iter()
                .any(|t| t.contains("/Users/alice")),
            "PII tag leaked into AnonymizedCapture"
        );
        assert!(
            anon.metadata.tags.iter().any(|t| t == "clean-tag"),
            "clean tag should be preserved, not dropped"
        );
        // A description with a credential keyword must be redacted.
        assert!(
            !anon
                .metadata
                .description
                .as_deref()
                .unwrap_or("")
                .contains("token"),
            "PII description leaked into AnonymizedCapture"
        );
        // An event comment with a credential keyword must be redacted.
        assert!(
            !anon.events[0]
                .comment
                .as_deref()
                .unwrap_or("")
                .contains("password"),
            "PII event comment leaked into AnonymizedCapture"
        );
    }

    #[test]
    fn test_anonymize_preserves_absent_optional_text() {
        use crate::capture::SerializableInputEvent;

        // Exercises the `None` arm of redact_opt_if_pii for both
        // description and event comment.
        let capture = StoredCapture {
            version: "3.1".to_string(),
            id: Uuid::new_v4(),
            name: "clean-name".to_string(),
            privacy: crate::privacy::PrivacyLevel::Public,
            created_at: Utc::now(),
            metadata: CaptureMetadata {
                tags: vec![],
                description: None,
                protocol: "midi".to_string(),
                duration_ms: Some(5000),
                event_count: 1,
                conductor_version: "3.1.0".to_string(),
            },
            events: vec![CapturedEvent {
                timestamp_ms: 0,
                event: SerializableInputEvent::PadPressed {
                    pad: 1,
                    velocity: 100,
                },
                comment: None,
            }],
        };

        let anon = anonymize_capture(&capture);

        assert_eq!(anon.metadata.description, None);
        assert_eq!(anon.events[0].comment, None);
    }

    #[test]
    fn test_anonymized_capture_carries_no_creation_timestamp() {
        // A SHA-256 of the capture timestamp neither preserves ordering
        // (hashes do not preserve input order) nor resists brute-force reversal
        // (timestamps are low-entropy). The anonymized output must therefore
        // carry no creation-time-derived field at all.
        let capture = StoredCapture {
            version: "3.1".to_string(),
            id: Uuid::new_v4(),
            name: "clean-name".to_string(),
            privacy: crate::privacy::PrivacyLevel::Public,
            created_at: Utc::now(),
            metadata: CaptureMetadata {
                tags: vec![],
                description: None,
                protocol: "midi".to_string(),
                duration_ms: Some(5000),
                event_count: 0,
                conductor_version: "3.1.0".to_string(),
            },
            events: Vec::new(),
        };

        let anon = anonymize_capture(&capture);
        let json = serde_json::to_string(&anon).expect("AnonymizedCapture serializes");

        assert!(
            !json.contains("created_at"),
            "anonymized output must not carry a creation-time field (got: {json})"
        );
    }
}
