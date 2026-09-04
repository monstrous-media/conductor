// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Smoke test for the Slice 9 unified routing passthrough
//! benchmark (ADR-036 D7). The benchmark itself is a `harness = false`
//! binary that `cargo test` does not execute, so this mirrors the
//! pipeline it measures — rule-engine miss → post-mapping route forward
//! — and asserts it runs without panic and produces the passthrough
//! output. Catches a broken pipeline before the CI `benchmark-gate` job
//! does.

use conductor_core::config::types::{
    ConnectorDirection, EndpointConfig, EndpointKind, RouteConfig,
};
use conductor_core::events::{ProcessedEvent, VelocityLevel};
use conductor_core::identity::DeviceMatcher;
use conductor_core::{Config, Mode};
use conductor_daemon::route_engine::RouteEngine;

#[test]
fn unified_routing_passthrough_runs_without_panic() {
    let config = Config {
        mcp: Default::default(),
        config_meta: Default::default(),
        security: Default::default(),
        // ADR-035: the "pads" device is now authored as an Input endpoint
        // (formerly a `[[devices]]`/`DeviceIdentityConfig` entry).
        endpoints: vec![EndpointConfig {
            alias: "pads".to_string(),
            direction: ConnectorDirection::Input,
            protocol: None,
            description: None,
            enabled: true,
            channels: vec![],
            kind: EndpointKind::Matcher {
                matchers: vec![DeviceMatcher::name_contains("Pads")],
                input_matchers: vec![],
                output_matchers: vec![],
                no_probe: false,
            },
        }],
        modes: vec![Mode {
            name: "Default".to_string(),
            color: None,
            mappings: vec![],
        }],
        global_mappings: vec![],
        logging: None,
        advanced_settings: Default::default(),
        last_selected_mode: None,
        per_app_modes: None,
        default_mode: None,
        led: None,
        event_console: None,
        routes: vec![],
    };
    let rule_set = conductor_core::rule_compiler::compile(&config, 1);
    let route_engine = RouteEngine::compile(&[RouteConfig {
        from: "pads".to_string(),
        to: "out".to_string(),
        transform: None,
        filter: None,
        enabled: true,
        description: None,
        modes: Vec::new(),
    }]);

    let event = ProcessedEvent::PadPressed {
        note: 36,
        velocity: 64,
        velocity_level: VelocityLevel::Medium,
        channel: Some(0),
    };
    let raw_midi = vec![0x90, 36, 64];

    // Rule engine misses (no mappings), then the passthrough route forwards.
    let matched = rule_set.match_event(&event, 0, Some("pads"));
    assert!(
        matched.is_none(),
        "passthrough event should miss the rule engine"
    );
    let outputs = route_engine.route_destinations_midi("pads", &raw_midi, "Default");
    assert_eq!(outputs.len(), 1, "single passthrough route forwards once");
    assert_eq!(outputs[0].to_alias, "out");
    assert_eq!(
        outputs[0].bytes, raw_midi,
        "passthrough preserves raw bytes"
    );
}
