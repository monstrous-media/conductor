// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Test error handling across crate boundaries

use conductor_core::{ActionError, Config, ConfigError, EngineError, FeedbackError, ProfileError};
use std::error::Error;

#[test]
fn test_config_error_from_io_error() {
    // Test that ConfigError can be created from std::io::Error
    let io_error = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
    let config_error = ConfigError::from(io_error);

    // Verify it's a proper error
    assert!(config_error.to_string().contains("IO error"));
}

#[test]
fn test_config_error_display() {
    // Test ConfigError display implementations
    let err = ConfigError::ValidationError("test validation".to_string());
    assert_eq!(err.to_string(), "Validation error: test validation");

    let err = ConfigError::InvalidTrigger("bad trigger".to_string());
    assert_eq!(err.to_string(), "Invalid trigger: bad trigger");
}

#[test]
fn test_engine_error_display() {
    // Test EngineError display implementations
    let err = EngineError::DeviceNotFound("test device".to_string());
    assert_eq!(err.to_string(), "Device not found: test device");

    let err = EngineError::InvalidMode(5);
    assert_eq!(err.to_string(), "Invalid mode: 5");
}

#[test]
fn test_action_error_display() {
    // Test ActionError display implementations
    let err = ActionError::InvalidKey("test key".to_string());
    assert_eq!(err.to_string(), "Invalid key: test key");

    let err = ActionError::AppNotFound("TestApp".to_string());
    assert_eq!(err.to_string(), "Application not found: TestApp");
}

#[test]
fn test_feedback_error_display() {
    // Test FeedbackError display implementations
    let err = FeedbackError::NotConnected;
    assert_eq!(err.to_string(), "Device not connected");

    let err = FeedbackError::HidError("test error".to_string());
    assert_eq!(err.to_string(), "HID error: test error");
}

#[test]
fn test_profile_error_display() {
    // Test ProfileError display implementations
    let err = ProfileError::XmlError("parse error".to_string());
    assert_eq!(err.to_string(), "XML parse error: parse error");

    let err = ProfileError::InvalidProfile("bad profile".to_string());
    assert_eq!(err.to_string(), "Invalid profile: bad profile");
}

#[test]
fn test_engine_error_from_config_error() {
    // Test that EngineError can be created from ConfigError
    let config_err = ConfigError::ValidationError("test".to_string());
    let engine_err = EngineError::from(config_err);

    match engine_err {
        EngineError::ConfigError(_) => {}
        _ => panic!("Expected ConfigError variant"),
    }
}

#[test]
fn test_error_source_chain() {
    // Test that error source chains work correctly
    let io_error = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
    let config_error = ConfigError::from(io_error);

    // Verify source chain
    assert!(config_error.source().is_some());
}

#[test]
fn test_config_load_missing_file_creates_defaults() {
    // Contract (loader.rs `Config::load`): a path that does not exist is NOT an
    // error — load creates a default config, saves it to the path, and returns
    // `Ok(default_config())`. The previous test accepted both Ok and Err, so it
    // enforced nothing: a regression that started erroring (or silently dropped
    // defaults) would still pass.
    //
    // Use an isolated temp dir (an allowed save location) so the created file is
    // cleaned up and the test doesn't pollute the working directory.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let path = tmp.path().join("missing_config_12345.toml");
    let path_str = path.to_str().expect("utf-8 temp path");

    assert!(
        !path.exists(),
        "precondition: the config file must not exist yet"
    );

    let config =
        Config::load(path_str).expect("loading a missing path must create defaults, not error");

    // The RETURNED config is exactly the default. Config has no PartialEq, so
    // compare via the project's canonical serializer (ADR-034 — lex-sorted
    // keys, deterministic; not coupled to struct field order or `toml` emitter
    // defaults).
    use conductor_core::config::canonical::serialise as canonical;
    let default_canon = canonical(&Config::default_config()).expect("canonical serialise default");
    assert_eq!(
        canonical(&config).expect("canonical serialise loaded config"),
        default_canon,
        "missing-file load must return the default config"
    );

    // ...and load must have PERSISTED that default to disk. Verify the file's
    // *contents* (not just its existence) by reading it back, parsing it, and
    // confirming it canonicalises to the same default.
    assert!(path.exists(), "load must create the config file on disk");
    let on_disk = std::fs::read_to_string(&path).expect("read the persisted config file");
    let reparsed: Config =
        toml::from_str(&on_disk).expect("the persisted config must be valid, loadable TOML");
    assert_eq!(
        canonical(&reparsed).expect("canonical serialise reparsed config"),
        default_canon,
        "the persisted file must contain the default config (write -> read -> parse round-trip)"
    );
}

#[test]
fn test_all_errors_are_send_sync() {
    // Verify all error types are Send + Sync (required for threading)
    fn assert_send_sync<T: Send + Sync>() {}

    assert_send_sync::<EngineError>();
    assert_send_sync::<ConfigError>();
    assert_send_sync::<ActionError>();
    assert_send_sync::<FeedbackError>();
    assert_send_sync::<ProfileError>();
}
