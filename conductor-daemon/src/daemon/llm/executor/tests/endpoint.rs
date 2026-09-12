// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use super::*;

// ===== ADR-035: conductor_create_endpoint + deprecations =====

#[tokio::test]
async fn test_create_endpoint_matcher_produces_plan() {
    use crate::daemon::llm::ConfigChange;
    use conductor_core::config::types::EndpointKind;
    let executor = ToolExecutor::new(live_config_arc(create_test_config()));
    let args = json!({
        "alias": "pads",
        "direction": "Input",
        "type": "Matcher",
        "matchers": [{ "type": "NameContains", "value": "Mikro" }]
    });
    match executor
        .execute("conductor_create_endpoint", Some(args), None)
        .await
    {
        ExecutionResult::PlanCreated { plan } => {
            assert!(
                plan.validation_errors.is_empty(),
                "valid endpoint must apply+validate cleanly: {:?}",
                plan.validation_errors
            );
            assert!(
                plan.deprecation.is_none(),
                "create_endpoint is not deprecated"
            );
            match &plan.changes[0] {
                ConfigChange::CreateEndpoint {
                    alias,
                    direction,
                    kind,
                    ..
                } => {
                    assert_eq!(alias, "pads");
                    assert_eq!(format!("{direction:?}"), "Input");
                    assert!(matches!(kind, EndpointKind::Matcher { .. }));
                }
                other => panic!("expected CreateEndpoint, got {other:?}"),
            }
        }
        other => panic!("expected PlanCreated, got {other:?}"),
    }
}

#[tokio::test]
async fn test_create_endpoint_osc_roundtrips_kind_and_protocol() {
    use crate::daemon::llm::ConfigChange;
    use conductor_core::config::types::EndpointKind;
    let executor = ToolExecutor::new(live_config_arc(create_test_config()));
    let args = json!({
        "alias": "eos",
        "direction": "Output",
        "protocol": "Osc",
        "type": "OscEndpoint",
        "host": "127.0.0.1",
        "port": 9000
    });
    match executor
        .execute("conductor_create_endpoint", Some(args), None)
        .await
    {
        ExecutionResult::PlanCreated { plan } => match &plan.changes[0] {
            ConfigChange::CreateEndpoint {
                alias,
                protocol,
                kind,
                ..
            } => {
                assert_eq!(alias, "eos");
                assert_eq!(format!("{protocol:?}"), "Some(Osc)");
                assert!(matches!(kind, EndpointKind::OscEndpoint { port: 9000, .. }));
            }
            other => panic!("expected CreateEndpoint, got {other:?}"),
        },
        other => panic!("expected PlanCreated, got {other:?}"),
    }
}

#[tokio::test]
async fn test_create_endpoint_requires_direction() {
    // `direction` is REQUIRED for endpoints (ADR-035 §4.1 R2 P1 — no default).
    let executor = ToolExecutor::new(live_config_arc(create_test_config()));
    let args = json!({
        "alias": "nodir",
        "type": "Matcher",
        "matchers": [{ "type": "NameContains", "value": "X" }]
    });
    match executor
        .execute("conductor_create_endpoint", Some(args), None)
        .await
    {
        ExecutionResult::Error { message } => assert!(
            message.to_lowercase().contains("direction"),
            "error must name the missing field; got: {message}"
        ),
        other => panic!("expected Error for missing direction, got {other:?}"),
    }
}

#[tokio::test]
async fn test_create_endpoint_rejects_duplicate_alias() {
    use conductor_core::config::types::{ConnectorDirection, EndpointConfig, EndpointKind};
    let mut config = create_test_config();
    config.endpoints.push(EndpointConfig {
        alias: "taken".to_string(),
        direction: ConnectorDirection::Input,
        protocol: None,
        description: None,
        enabled: true,
        channels: vec![],
        kind: EndpointKind::Matcher {
            matchers: vec![conductor_core::identity::DeviceMatcher::NameContains {
                value: "A".to_string(),
            }],
            input_matchers: vec![],
            output_matchers: vec![],
            no_probe: false,
        },
    });
    let executor = ToolExecutor::new(live_config_arc(config));
    let args = json!({
        "alias": "taken",
        "direction": "Input",
        "type": "Matcher",
        "matchers": [{ "type": "NameContains", "value": "B" }]
    });
    match executor
        .execute("conductor_create_endpoint", Some(args), None)
        .await
    {
        ExecutionResult::Error { message } => assert!(
            message.contains("taken") && message.to_lowercase().contains("exist"),
            "error must name the colliding alias and that it already exists; got: {message}"
        ),
        other => panic!("expected Error for duplicate alias, got {other:?}"),
    }
}
