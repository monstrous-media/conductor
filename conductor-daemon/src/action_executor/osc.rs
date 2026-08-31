// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! OSC message sending for the `OscSend` action (#1684 split from
//! `action_executor.rs`). Encodes args via `rosc` and sends over UDP
//! (v4.26.0 — ADR-009 Gap H).

use super::ActionExecutor;
use conductor_core::dispatch::{DispatchError, DispatchOutcome, DispatchResult};

impl ActionExecutor {
    /// Execute an OscSend action: encode and send an OSC message over UDP
    pub(crate) fn execute_osc_send(
        &self,
        host: &str,
        port: u16,
        address: &str,
        args: &[conductor_core::OscArg],
    ) -> DispatchResult {
        use conductor_core::OscArg;
        use rosc::{OscMessage, OscPacket, OscType};

        let osc_args: Vec<OscType> = args
            .iter()
            .map(|a| match a {
                OscArg::Int(v) => OscType::Int(*v),
                OscArg::Float(v) => OscType::Float(*v),
                OscArg::String(v) => OscType::String(v.clone()),
            })
            .collect();

        let msg = OscPacket::Message(OscMessage {
            addr: address.to_string(),
            args: osc_args,
        });

        let buf = rosc::encoder::encode(&msg)
            .map_err(|e| DispatchError::OscSend(format!("encode error: {}", e)))?;

        let addr = format!("{}:{}", host, port);
        let socket = std::net::UdpSocket::bind("0.0.0.0:0")
            .map_err(|e| DispatchError::OscSend(format!("bind error: {}", e)))?;
        socket
            .send_to(&buf, &addr)
            .map_err(|e| DispatchError::OscSend(format!("send to {} error: {}", addr, e)))?;

        Ok(DispatchOutcome::Completed)
    }
}

// ========== OscSend Tests (v4.26.0 - ADR-009 Gap H) ==========
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
    fn test_osc_send_loopback() {
        use std::net::UdpSocket;

        // Bind a UDP listener on a random port
        let listener = UdpSocket::bind("127.0.0.1:0").unwrap();
        let local_port = listener.local_addr().unwrap().port();
        listener
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();

        let mut executor = ActionExecutor::default();
        let action = conductor_core::Action::OscSend {
            host: "127.0.0.1".to_string(),
            port: local_port,
            address: "/test/ping".to_string(),
            args: vec![
                conductor_core::OscArg::Int(42),
                conductor_core::OscArg::Float(0.75),
                conductor_core::OscArg::String("hello".to_string()),
            ],
        };

        let result = executor.execute(action, None);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), DispatchOutcome::Completed);

        // Read what was sent
        let mut buf = [0u8; 1024];
        let (len, _src) = listener.recv_from(&mut buf).unwrap();
        let packet = rosc::decoder::decode_udp(&buf[..len]).unwrap();
        match packet.1 {
            rosc::OscPacket::Message(msg) => {
                assert_eq!(msg.addr, "/test/ping");
                assert_eq!(msg.args.len(), 3);
                assert_eq!(msg.args[0], rosc::OscType::Int(42));
                assert_eq!(msg.args[1], rosc::OscType::Float(0.75));
                assert_eq!(msg.args[2], rosc::OscType::String("hello".to_string()));
            }
            _ => panic!("Expected OSC Message"),
        }
    }

    #[test]
    #[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
    fn test_osc_send_no_args() {
        use std::net::UdpSocket;

        let listener = UdpSocket::bind("127.0.0.1:0").unwrap();
        let local_port = listener.local_addr().unwrap().port();
        listener
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();

        let mut executor = ActionExecutor::default();
        let action = conductor_core::Action::OscSend {
            host: "127.0.0.1".to_string(),
            port: local_port,
            address: "/heartbeat".to_string(),
            args: vec![],
        };

        let result = executor.execute(action, None);
        assert!(result.is_ok());

        let mut buf = [0u8; 1024];
        let (len, _) = listener.recv_from(&mut buf).unwrap();
        let packet = rosc::decoder::decode_udp(&buf[..len]).unwrap();
        match packet.1 {
            rosc::OscPacket::Message(msg) => {
                assert_eq!(msg.addr, "/heartbeat");
                assert!(msg.args.is_empty());
            }
            _ => panic!("Expected OSC Message"),
        }
    }

    #[test]
    fn test_osc_forward_resends_inbound_to_output_endpoint() {
        // ADR-039-A Slice 3 (#2326): OscForward re-sends the inbound OSC
        // message (from the trigger context) verbatim to the target OSC
        // output endpoint resolved by alias.
        use crate::action_executor::TriggerContext;
        use std::collections::HashMap;
        use std::net::UdpSocket;

        let listener = UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();

        let mut ex = ActionExecutor::default();
        let mut map = HashMap::new();
        map.insert("eos-out".to_string(), ("127.0.0.1".to_string(), port));
        ex.set_osc_output_endpoints(map);

        let ctx = TriggerContext {
            osc_message: Some(conductor_core::events::OscInbound {
                address: "/eos/go".to_string(),
                args: vec![conductor_core::OscArg::Float(1.0)],
                time: std::time::Instant::now(),
            }),
            ..Default::default()
        };
        let action = conductor_core::Action::OscForward {
            target: "eos-out".to_string(),
            transform: None,
        };
        assert_eq!(
            ex.execute(action, Some(ctx)).unwrap(),
            DispatchOutcome::Completed
        );

        let mut buf = [0u8; 1024];
        let (len, _) = listener.recv_from(&mut buf).unwrap();
        match rosc::decoder::decode_udp(&buf[..len]).unwrap().1 {
            rosc::OscPacket::Message(m) => {
                assert_eq!(m.addr, "/eos/go");
                assert_eq!(m.args, vec![rosc::OscType::Float(1.0)]);
            }
            _ => panic!("expected OSC message"),
        }
    }

    #[test]
    fn test_osc_forward_without_inbound_message_is_noop() {
        // A non-OSC-triggered mapping has no osc_message in context → no-op
        // (mirrors HidForward's missing-event skip), never an error.
        use crate::action_executor::TriggerContext;
        let mut ex = ActionExecutor::default();
        let action = conductor_core::Action::OscForward {
            target: "eos-out".to_string(),
            transform: None,
        };
        // No osc_message, and the target isn't even mapped — still Completed
        // because the absent inbound short-circuits before target resolution.
        let ctx = TriggerContext::default();
        assert_eq!(
            ex.execute(action, Some(ctx)).unwrap(),
            DispatchOutcome::Completed
        );
    }

    #[test]
    fn test_osc_forward_unknown_target_errors() {
        use crate::action_executor::TriggerContext;
        let mut ex = ActionExecutor::default();
        let ctx = TriggerContext {
            osc_message: Some(conductor_core::events::OscInbound {
                address: "/x".to_string(),
                args: vec![],
                time: std::time::Instant::now(),
            }),
            ..Default::default()
        };
        let action = conductor_core::Action::OscForward {
            target: "ghost".to_string(),
            transform: None,
        };
        // Inbound present but target not in the output map → dispatch error.
        assert!(ex.execute(action, Some(ctx)).is_err());
    }

    #[test]
    fn test_osc_send_action_conversion() {
        use conductor_core::config::ActionConfig;

        let config = ActionConfig::OscSend {
            host: "192.168.1.10".to_string(),
            port: 8000,
            address: "/mixer/channel/1/fader".to_string(),
            args: vec![conductor_core::OscArg::Float(0.75)],
        };
        let action: conductor_core::Action = config.into();
        match action {
            conductor_core::Action::OscSend {
                host,
                port,
                address,
                args,
            } => {
                assert_eq!(host, "192.168.1.10");
                assert_eq!(port, 8000);
                assert_eq!(address, "/mixer/channel/1/fader");
                assert_eq!(args.len(), 1);
                assert_eq!(args[0], conductor_core::OscArg::Float(0.75));
            }
            _ => panic!("Expected OscSend action"),
        }
    }
}
