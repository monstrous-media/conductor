// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use super::*;

// ============================================================================
// Config IPC-surface CLI tests (ADR-034 §D4.C / §D9)
// ============================================================================

#[test]
fn config_drift_cli_parsing() {
    let cli = Cli::try_parse_from(["conductorctl", "config", "drift"]).unwrap();
    assert!(matches!(
        cli.command,
        Commands::Config {
            action: ConfigAction::Drift
        }
    ));
}

#[test]
fn config_mark_known_good_cli_parsing() {
    // The kebab-case `mark-known-good` maps to `ConfigAction::MarkKnownGood`.
    let cli = Cli::try_parse_from(["conductorctl", "config", "mark-known-good"]).unwrap();
    assert!(matches!(
        cli.command,
        Commands::Config {
            action: ConfigAction::MarkKnownGood
        }
    ));
}

#[test]
fn config_rejects_unknown_action() {
    // A typo'd subcommand must be a parse error, not silently accepted.
    assert!(Cli::try_parse_from(["conductorctl", "config", "bogus"]).is_err());
}

#[test]
fn config_reload_no_path_cli_parsing() {
    let cli = Cli::try_parse_from(["conductorctl", "config", "reload"]).unwrap();
    assert!(matches!(
        cli.command,
        Commands::Config {
            action: ConfigAction::Reload { path: None }
        }
    ));
}

#[test]
fn config_reload_with_path_cli_parsing() {
    let cli =
        Cli::try_parse_from(["conductorctl", "config", "reload", "--path", "/tmp/x.toml"]).unwrap();
    match cli.command {
        Commands::Config {
            action: ConfigAction::Reload { path: Some(p) },
        } => assert_eq!(p, PathBuf::from("/tmp/x.toml")),
        _ => panic!("expected Reload with --path"),
    }
}

#[test]
fn config_import_cli_parsing() {
    let cli = Cli::try_parse_from(["conductorctl", "config", "import", "/tmp/x.toml"]).unwrap();
    match cli.command {
        Commands::Config {
            action: ConfigAction::Import { path },
        } => assert_eq!(path, PathBuf::from("/tmp/x.toml")),
        _ => panic!("expected Import with path"),
    }
}

#[test]
fn config_import_requires_path() {
    // ImportConfig's path is required (unlike reload's optional --path).
    assert!(Cli::try_parse_from(["conductorctl", "config", "import"]).is_err());
}

#[test]
fn config_save_stdin_cli_parsing() {
    // Bare `config save` reads stdin (no path, no explicit base generation).
    let cli = Cli::try_parse_from(["conductorctl", "config", "save"]).unwrap();
    assert!(matches!(
        cli.command,
        Commands::Config {
            action: ConfigAction::Save {
                path: None,
                base_generation: None
            }
        }
    ));
}

#[test]
fn config_save_base_generation_cli_parsing() {
    let cli =
        Cli::try_parse_from(["conductorctl", "config", "save", "--base-generation", "7"]).unwrap();
    assert!(matches!(
        cli.command,
        Commands::Config {
            action: ConfigAction::Save {
                path: None,
                base_generation: Some(7)
            }
        }
    ));
}

#[test]
fn config_save_captures_positional_path_for_redirect() {
    // A positional path PARSES (captured) so the handler can hard-fail with a
    // redirect to `config import` rather than clap's terse "unexpected arg".
    let cli = Cli::try_parse_from(["conductorctl", "config", "save", "x.toml"]).unwrap();
    match cli.command {
        Commands::Config {
            action: ConfigAction::Save { path: Some(p), .. },
        } => assert_eq!(p, PathBuf::from("x.toml")),
        _ => panic!("expected Save with a captured path"),
    }
}

// ============================================================================
// Profile CLI tests
// ============================================================================

#[test]
fn test_profile_status_cli_parsing() {
    let cli = Cli::try_parse_from(["conductorctl", "profile", "status"]).unwrap();
    assert!(matches!(
        cli.command,
        Commands::Profile {
            action: ProfileAction::Status
        }
    ));
}

#[test]
fn test_profile_switch_cli_parsing() {
    let cli = Cli::try_parse_from(["conductorctl", "profile", "switch", "/tmp/test.toml"]).unwrap();
    match cli.command {
        Commands::Profile {
            action: ProfileAction::Switch { name_or_path },
        } => {
            assert_eq!(name_or_path, "/tmp/test.toml");
        }
        _ => panic!("Expected Profile Switch"),
    }
}

#[test]
fn test_profile_validate_cli_parsing() {
    let cli =
        Cli::try_parse_from(["conductorctl", "profile", "validate", "/tmp/config.toml"]).unwrap();
    match cli.command {
        Commands::Profile {
            action: ProfileAction::Validate { path },
        } => {
            assert_eq!(path, PathBuf::from("/tmp/config.toml"));
        }
        _ => panic!("Expected Profile Validate"),
    }
}

#[test]
fn test_profile_list_cli_parsing_default() {
    let cli = Cli::try_parse_from(["conductorctl", "profile", "list"]).unwrap();
    match cli.command {
        Commands::Profile {
            action: ProfileAction::List { dir },
        } => {
            assert!(dir.is_none());
        }
        _ => panic!("Expected Profile List"),
    }
}

#[test]
fn test_profile_list_cli_parsing_with_dir() {
    let cli = Cli::try_parse_from(["conductorctl", "profile", "list", "/tmp/profiles"]).unwrap();
    match cli.command {
        Commands::Profile {
            action: ProfileAction::List { dir },
        } => {
            assert_eq!(dir, Some(PathBuf::from("/tmp/profiles")));
        }
        _ => panic!("Expected Profile List"),
    }
}

#[test]
fn test_plugin_list_cli_parsing() {
    let cli = Cli::try_parse_from(["conductorctl", "plugin", "list"]).unwrap();
    assert!(matches!(
        cli.command,
        Commands::Plugin {
            action: PluginAction::List
        }
    ));
}

#[test]
fn test_plugin_info_cli_parsing() {
    let cli = Cli::try_parse_from(["conductorctl", "plugin", "info", "spotify-control"]).unwrap();
    match cli.command {
        Commands::Plugin {
            action: PluginAction::Info { name },
        } => assert_eq!(name, "spotify-control"),
        _ => panic!("Expected Plugin Info"),
    }
}

#[test]
fn test_plugin_enable_cli_parsing() {
    let cli = Cli::try_parse_from(["conductorctl", "plugin", "enable", "my-plugin"]).unwrap();
    match cli.command {
        Commands::Plugin {
            action: PluginAction::Enable { name },
        } => assert_eq!(name, "my-plugin"),
        _ => panic!("Expected Plugin Enable"),
    }
}

#[test]
fn test_plugin_disable_cli_parsing() {
    let cli = Cli::try_parse_from(["conductorctl", "plugin", "disable", "my-plugin"]).unwrap();
    match cli.command {
        Commands::Plugin {
            action: PluginAction::Disable { name },
        } => assert_eq!(name, "my-plugin"),
        _ => panic!("Expected Plugin Disable"),
    }
}

// =========================================================================
// Event monitoring tests
// =========================================================================

#[test]
fn test_csv_escape() {
    assert_eq!(csv_escape("hello"), "hello");
    assert_eq!(csv_escape("hello,world"), "\"hello,world\"");
    assert_eq!(csv_escape("say \"hi\""), "\"say \"\"hi\"\"\"");
    assert_eq!(csv_escape("line1\nline2"), "\"line1\nline2\"");
    assert_eq!(csv_escape(""), "");
}

#[test]
fn test_event_passes_filter_no_filter() {
    let filter = conductor_daemon::EventFilter::default();
    let event = serde_json::json!({"timestamp_ms": 1000, "event_type": "note_on"});
    assert!(event_passes_filter(&event, &filter));
}

#[test]
fn test_event_passes_filter_with_filter() {
    let filter = conductor_daemon::EventFilter {
        event_type: Some("cc".to_string()),
        ..Default::default()
    };
    let note = serde_json::json!({"timestamp_ms": 1000, "event_type": "note_on", "note": 60});
    let cc = serde_json::json!({"timestamp_ms": 1000, "event_type": "cc", "cc": 1, "value": 64});
    assert!(!event_passes_filter(&note, &filter));
    assert!(event_passes_filter(&cc, &filter));
}

#[test]
fn test_event_passes_filter_bad_json_with_filter() {
    let filter = conductor_daemon::EventFilter {
        event_type: Some("note_on".to_string()),
        ..Default::default()
    };
    // Malformed event that can't deserialize — should be skipped when filters active
    let bad = serde_json::json!({"garbage": true});
    assert!(!event_passes_filter(&bad, &filter));
}

#[test]
fn test_event_passes_filter_bad_json_no_filter() {
    let filter = conductor_daemon::EventFilter::default();
    // No filters active — pass through even if deserialization fails
    let bad = serde_json::json!({"garbage": true});
    assert!(event_passes_filter(&bad, &filter));
}

#[test]
fn test_parse_duration_str() {
    assert_eq!(parse_duration_str(None), None);
    assert_eq!(parse_duration_str(Some("10s")), Some(10));
    assert_eq!(parse_duration_str(Some("1m")), Some(60));
    assert_eq!(parse_duration_str(Some("30")), Some(30));
    assert_eq!(parse_duration_str(Some("5m")), Some(300));
    assert_eq!(parse_duration_str(Some("1h")), Some(3600));
    assert_eq!(parse_duration_str(Some("2h")), Some(7200));
    assert_eq!(parse_duration_str(Some("abc")), None);
}

/// An omitted `--duration` defaults to 2s, but an explicit invalid
/// value must error (not silently fall back to 2s).
#[test]
fn test_resolve_capture_secs() {
    // Omitted → 2-second default.
    assert_eq!(resolve_capture_secs(None).unwrap(), 2);
    // Valid explicit values pass through.
    assert_eq!(resolve_capture_secs(Some("10s")).unwrap(), 10);
    assert_eq!(resolve_capture_secs(Some("5m")).unwrap(), 300);
    // Explicit INVALID value → error (the bug: previously captured 2s).
    let err = resolve_capture_secs(Some("10seconds")).unwrap_err();
    assert!(
        err.to_string()
            .contains("Invalid --duration value '10seconds'"),
        "expected a clear invalid-duration error, got: {err}"
    );
    assert!(resolve_capture_secs(Some("nope")).is_err());
}

#[test]
fn test_export_events_json() {
    let events = vec![
        serde_json::json!({"timestamp_ms": 1000, "event_type": "note_on", "note": 60, "velocity": 100}),
    ];
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.json");
    export_events(&events, &path).unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0]["event_type"], "note_on");
}

#[test]
fn test_export_events_csv() {
    let events = vec![
        serde_json::json!({"timestamp_ms": 1000, "event_type": "note_on", "note": 60, "velocity": 100}),
        serde_json::json!({"timestamp_ms": 2000, "event_type": "cc", "cc": 1, "value": 64}),
    ];
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.csv");
    export_events(&events, &path).unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 3); // header + 2 events
    assert!(lines[0].starts_with("timestamp_ms,"));
    assert!(lines[1].starts_with("1000,note_on,"));
    assert!(lines[2].starts_with("2000,cc,"));
}

#[test]
fn test_export_events_csv_with_special_chars() {
    let events = vec![
        serde_json::json!({"timestamp_ms": 1000, "event_type": "note_on", "device_id": "my,device"}),
    ];
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.csv");
    export_events(&events, &path).unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    // device_id with comma should be quoted
    assert!(content.contains("\"my,device\""));
}

// =========================================================================
// Profile create/delete/name-switch tests
// =========================================================================

#[test]
fn test_profile_create_cli_parsing() {
    let cli = Cli::try_parse_from([
        "conductorctl",
        "profile",
        "create",
        "gaming",
        "--app",
        "com.game.one",
        "--app",
        "com.game.two",
    ])
    .unwrap();
    match cli.command {
        Commands::Profile {
            action: ProfileAction::Create { name, app },
        } => {
            assert_eq!(name, "gaming");
            assert_eq!(app, vec!["com.game.one", "com.game.two"]);
        }
        _ => panic!("Expected Profile Create"),
    }
}

#[test]
fn test_profile_create_cli_no_apps() {
    let cli = Cli::try_parse_from(["conductorctl", "profile", "create", "minimal"]).unwrap();
    match cli.command {
        Commands::Profile {
            action: ProfileAction::Create { name, app },
        } => {
            assert_eq!(name, "minimal");
            assert!(app.is_empty());
        }
        _ => panic!("Expected Profile Create"),
    }
}

#[test]
fn test_profile_delete_cli_parsing() {
    let cli = Cli::try_parse_from(["conductorctl", "profile", "delete", "old-profile"]).unwrap();
    match cli.command {
        Commands::Profile {
            action: ProfileAction::Delete { name, force },
        } => {
            assert_eq!(name, "old-profile");
            assert!(!force);
        }
        _ => panic!("Expected Profile Delete"),
    }
}

#[test]
fn test_profile_delete_cli_force() {
    let cli = Cli::try_parse_from([
        "conductorctl",
        "profile",
        "delete",
        "old-profile",
        "--force",
    ])
    .unwrap();
    match cli.command {
        Commands::Profile {
            action: ProfileAction::Delete { name, force },
        } => {
            assert_eq!(name, "old-profile");
            assert!(force);
        }
        _ => panic!("Expected Profile Delete"),
    }
}

#[test]
fn test_profile_switch_by_name() {
    let cli = Cli::try_parse_from(["conductorctl", "profile", "switch", "gaming"]).unwrap();
    match cli.command {
        Commands::Profile {
            action: ProfileAction::Switch { name_or_path },
        } => {
            assert_eq!(name_or_path, "gaming");
        }
        _ => panic!("Expected Profile Switch"),
    }
}

#[test]
fn test_resolve_profile_path_name() {
    let path = resolve_profile_path("gaming").unwrap();
    let expected = dirs::config_dir()
        .unwrap()
        .join("conductor")
        .join("profiles")
        .join("gaming.toml");
    assert_eq!(path, expected);
}

#[test]
fn test_resolve_profile_path_absolute() {
    let path = resolve_profile_path("/tmp/my-profile.toml").unwrap();
    assert_eq!(path, PathBuf::from("/tmp/my-profile.toml"));
}

#[test]
fn test_resolve_profile_path_relative_toml() {
    let path = resolve_profile_path("my-profile.toml").unwrap();
    assert_eq!(path, PathBuf::from("my-profile.toml"));
}

#[test]
fn test_resolve_profile_path_with_slash() {
    let path = resolve_profile_path("./profiles/test").unwrap();
    assert_eq!(path, PathBuf::from("./profiles/test"));
}

#[test]
fn migrate_config_default_path_uses_config_dir_not_dot_conductor() {
    // `migrate-config --routing`'s default config path must match the
    // daemon/GUI resolution (`dirs::config_dir()/conductor/config.toml`), NOT
    // `~/.conductor/config.toml`. On macOS those differ (config_dir =
    // ~/Library/Application Support/conductor), so the old home-based default
    // pointed migrate-config at a non-existent file and the user's real
    // config was never migrated.
    let resolved = resolve_migrate_config_path(&None).unwrap();
    let expected = dirs::config_dir()
        .unwrap()
        .join("conductor")
        .join("config.toml");
    assert_eq!(resolved, expected);

    // And it must NOT be the old home-based default (differs on every
    // platform: macOS config_dir is Application Support, Linux is ~/.config).
    if let Some(home) = dirs::home_dir() {
        assert_ne!(resolved, home.join(".conductor").join("config.toml"));
    }
}

#[test]
fn migrate_config_explicit_path_is_passed_through() {
    let resolved = resolve_migrate_config_path(&Some(PathBuf::from("/tmp/custom.toml"))).unwrap();
    assert_eq!(resolved, PathBuf::from("/tmp/custom.toml"));
}

#[test]
fn test_profile_create_generates_valid_toml() {
    let dir = tempfile::tempdir().unwrap();
    let profile_dir = dir.path().join("conductor").join("profiles");
    std::fs::create_dir_all(&profile_dir).unwrap();

    // We test the generated config directly rather than calling the handler
    // (which uses dirs::config_dir)
    let mut config = Config::default_config();
    config.modes = vec![conductor_core::Mode {
        name: "Default".to_string(),
        color: Some("blue".to_string()),
        mappings: vec![],
    }];
    config.global_mappings = vec![];

    let toml_str = toml::to_string_pretty(&config).unwrap();
    let parsed: Config = toml::from_str(&toml_str).unwrap();
    assert_eq!(parsed.modes.len(), 1);
    assert_eq!(parsed.modes[0].name, "Default");
    // ADR-035: the `device` binding field is gone; a freshly exported
    // profile carries no endpoint bindings (only modes/mappings).
    assert!(parsed.endpoints.is_empty());
}

#[test]
fn test_profile_create_with_apps_generates_modes() {
    let apps = [
        "com.apple.Logic".to_string(),
        "com.ableton.Live".to_string(),
    ];
    let modes: Vec<conductor_core::Mode> = apps
        .iter()
        .map(|app| conductor_core::Mode {
            name: app.clone(),
            color: None,
            mappings: vec![],
        })
        .collect();

    let mut config = Config::default_config();
    config.modes = modes;
    config.global_mappings = vec![];

    let toml_str = toml::to_string_pretty(&config).unwrap();
    let parsed: Config = toml::from_str(&toml_str).unwrap();
    assert_eq!(parsed.modes.len(), 2);
    assert_eq!(parsed.modes[0].name, "com.apple.Logic");
    assert_eq!(parsed.modes[1].name, "com.ableton.Live");
}

#[test]
fn test_validate_profile_name_rejects_empty() {
    assert!(validate_profile_name("").is_err());
    assert!(validate_profile_name("   ").is_err());
}

#[test]
fn test_validate_profile_name_rejects_path_traversal() {
    assert!(validate_profile_name("../evil").is_err());
    assert!(validate_profile_name("foo/bar").is_err());
    assert!(validate_profile_name("foo\\bar").is_err());
    assert!(validate_profile_name("..").is_err());
}

#[test]
fn test_validate_profile_name_rejects_long_names() {
    let long_name = "a".repeat(65);
    assert!(validate_profile_name(&long_name).is_err());
}

#[test]
fn test_validate_profile_name_rejects_special_chars() {
    assert!(validate_profile_name("foo@bar").is_err());
    assert!(validate_profile_name("foo!bar").is_err());
    assert!(validate_profile_name("foo$bar").is_err());
}

#[test]
fn test_validate_profile_name_accepts_valid() {
    assert!(validate_profile_name("my-profile").is_ok());
    assert!(validate_profile_name("My Profile").is_ok());
    assert!(validate_profile_name("profile_123").is_ok());
    assert!(validate_profile_name("DAW").is_ok());
}

// ── bindings filter ──────────────────────────────────────────────────
//
// The handler reuses `IpcCommand::Status` and just filters/pretty-prints.
// The filter is the only non-trivial logic; pin it with focused tests so
// a future refactor of the IPC payload shape (e.g. renaming
// `is_configured` → `bound_to_alias`) trips the tests instead of silently
// breaking the CLI.

fn devices_fixture() -> Vec<Value> {
    vec![
        serde_json::json!({
            "device_id": "fcb",
            "port_name": "Komplete Audio 6 MK2",
            "connected": true,
            "is_configured": true,
        }),
        serde_json::json!({
            "device_id": "raw:IAC Driver Bus 1",
            "port_name": "IAC Driver Bus 1",
            "connected": true,
            "is_configured": false,
        }),
        serde_json::json!({
            "device_id": "pads",
            "port_name": "Maschine Mikro MK3 MIDI",
            "connected": true,
            "is_configured": true,
        }),
    ]
}

#[test]
fn test_bindings_filter_no_filter_returns_all() {
    let devs = devices_fixture();
    let out = filter_bindings(&devs, None, false);
    assert_eq!(out.len(), 3);
}

#[test]
fn test_bindings_filter_alias_matches_exactly_one() {
    let devs = devices_fixture();
    let out = filter_bindings(&devs, Some("fcb"), false);
    assert_eq!(out.len(), 1);
    assert_eq!(
        out[0].get("device_id").and_then(|v| v.as_str()),
        Some("fcb")
    );
}

#[test]
fn test_bindings_filter_alias_no_match_returns_empty() {
    let devs = devices_fixture();
    let out = filter_bindings(&devs, Some("nonexistent"), false);
    assert!(out.is_empty());
}

#[test]
fn test_bindings_filter_unbound_only_drops_configured() {
    let devs = devices_fixture();
    let out = filter_bindings(&devs, None, true);
    assert_eq!(out.len(), 1);
    assert_eq!(
        out[0].get("device_id").and_then(|v| v.as_str()),
        Some("raw:IAC Driver Bus 1")
    );
    assert_eq!(
        out[0].get("is_configured").and_then(|v| v.as_bool()),
        Some(false)
    );
}

#[test]
fn test_bindings_filter_alias_and_unbound_compose() {
    // alias matches a configured row; with unbound_only, both predicates
    // apply (AND), so the result is empty.
    let devs = devices_fixture();
    let out = filter_bindings(&devs, Some("fcb"), true);
    assert!(out.is_empty());
}

#[test]
fn test_bindings_filter_treats_missing_is_configured_as_false() {
    // Forward-compat: if a future daemon payload omits the field, the
    // device must be treated as opportunistic rather than panicking.
    let devs = vec![serde_json::json!({
        "device_id": "legacy",
        "port_name": "Some Port",
        "connected": true,
    })];
    let out = filter_bindings(&devs, None, true);
    assert_eq!(
        out.len(),
        1,
        "missing is_configured should default to opportunistic"
    );
}

// ─── Rollback CLI exit-code regression tests ───────────────────

/// Helper test fixture: build an IpcResponse with a synthetic
/// daemon-side error. Used to exercise `check_ipc_response`'s
/// error path without a live daemon.
fn error_response(code: u16, msg: &str) -> conductor_daemon::daemon::types::IpcResponse {
    conductor_daemon::daemon::types::IpcResponse {
        id: "test".to_string(),
        status: conductor_daemon::ResponseStatus::Error,
        data: None,
        error: Some(conductor_daemon::daemon::types::ErrorDetails {
            code,
            message: msg.to_string(),
            details: None,
        }),
    }
}

fn success_response() -> conductor_daemon::daemon::types::IpcResponse {
    conductor_daemon::daemon::types::IpcResponse {
        id: "test".to_string(),
        status: conductor_daemon::ResponseStatus::Success,
        data: Some(serde_json::json!({"state_generation": 5})),
        error: None,
    }
}

/// Regression test: an error response MUST yield
/// `Err` from the helper so the caller can propagate non-zero
/// exit. Pre-fix, both rollback handlers swallowed the error
/// and returned Ok(()) → process exit 0.
#[test]
fn check_ipc_response_returns_err_on_daemon_error() {
    let resp = error_response(1004, "rollback unsupported");
    let result = check_ipc_response(&resp, "rollback");
    assert!(
        result.is_err(),
        "daemon error response MUST yield Err so process exits non-zero; got Ok"
    );
    let err_msg = format!("{:#}", result.unwrap_err());
    assert!(
        err_msg.contains("rollback failed"),
        "ctx should be in error message; got: {err_msg}"
    );
    assert!(
        err_msg.contains("rollback unsupported"),
        "daemon message should propagate; got: {err_msg}"
    );
    assert!(
        err_msg.contains("1004"),
        "daemon error code should propagate; got: {err_msg}"
    );
}

/// Success response passes through with no error.
#[test]
fn check_ipc_response_returns_ok_on_success() {
    let resp = success_response();
    assert!(check_ipc_response(&resp, "rollback").is_ok());
}

/// Defensive: even if `status` is somehow `Success` but `error`
/// is also populated (protocol bug — shouldn't happen but
/// shouldn't pass silently if it does), still treat as error.
/// Catches a class of daemon-side regression where the status
/// field drifts out of sync with the error field.
#[test]
fn check_ipc_response_returns_err_when_error_field_set_despite_success_status() {
    let mut resp = success_response();
    resp.error = Some(conductor_daemon::daemon::types::ErrorDetails {
        code: 5005,
        message: "something is wrong".to_string(),
        details: None,
    });
    // status is still Success but error is Some — should still bail.
    assert!(
        check_ipc_response(&resp, "rollback").is_err(),
        "defensive: error field set MUST yield Err regardless of status"
    );
}

/// The LED (scheme/brightness/off) and plugin (enable/disable)
/// handlers now route daemon-error responses through
/// `check_ipc_response` AFTER printing JSON, so `--json led scheme
/// <invalid>` / `--json plugin enable <missing>` return `Err` (non-zero
/// process exit) instead of printing the error and exiting 0. This pins
/// the shared contract those handlers rely on for each wired context.
#[test]
fn check_ipc_response_propagates_led_and_plugin_errors() {
    for ctx in [
        "Set LED scheme",
        "Set LED brightness",
        "Turn off LEDs",
        "Enable plugin",
        "Disable plugin",
    ] {
        let resp = error_response(5005, "daemon rejected request");
        let result = check_ipc_response(&resp, ctx);
        assert!(
            result.is_err(),
            "{ctx}: a daemon error response MUST yield Err so the process \
             exits non-zero in JSON mode too"
        );
        let msg = format!("{:#}", result.unwrap_err());
        assert!(
            msg.contains(ctx) && msg.contains("daemon rejected request"),
            "{ctx}: error must carry the context and daemon message; got: {msg}"
        );
    }
}

#[test]
fn test_bindings_filter_alias_matches_raw_prefix_opportunistic() {
    // The clap help and filter_bindings doc both state that opportunistic
    // ports have device_id = "raw:<port_name>". This test pins the exact
    // documented usage: `--alias "raw:IAC Driver Bus 1"` must return the
    // opportunistic entry and nothing else.
    let devs = devices_fixture();
    let out = filter_bindings(&devs, Some("raw:IAC Driver Bus 1"), false);
    assert_eq!(out.len(), 1);
    assert_eq!(
        out[0].get("device_id").and_then(|v| v.as_str()),
        Some("raw:IAC Driver Bus 1")
    );
    assert_eq!(
        out[0].get("is_configured").and_then(|v| v.as_bool()),
        Some(false)
    );
}

// ─── ADR-027 D6: `conductorctl llm budgets show` resolver ─────────

#[test]
fn llm_budget_defaults_when_no_config_file() {
    let b = resolve_llm_budget_from_text(None).unwrap();
    assert_eq!(b, conductor_core::security::LlmBudgetConfig::default());
}

#[test]
fn llm_budget_reads_security_llm_block() {
    // Tightens only what the block names; the rest stay at ADR defaults.
    let text = "[security.llm]\nmax_tool_calls_per_session = 9\n";
    let b = resolve_llm_budget_from_text(Some(text)).unwrap();
    assert_eq!(b.max_tool_calls_per_session, 9);
    assert_eq!(b.max_iterations_per_turn, 10); // untouched default
}

#[test]
fn llm_budget_ignores_unrelated_tables() {
    // A config with no [security] table yields the defaults, not an error.
    let text = "[device]\nname = \"Mikro\"\n";
    let b = resolve_llm_budget_from_text(Some(text)).unwrap();
    assert_eq!(b, conductor_core::security::LlmBudgetConfig::default());
}
