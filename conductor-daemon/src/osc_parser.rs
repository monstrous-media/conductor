// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! OSC datagram parser (ADR-039-A).
//!
//! Pure decode of an accepted OSC datagram into zero-or-more [`OscInbound`].
//! NO socket, NO I/O — the ADR-042 listener edge (`crate::listeners`) already
//! binds the loopback socket, applies the ACL + rate limit + audit, and hands
//! us raw bytes as an `AcceptedPacket`. This module fills the documented
//! "parser placeholder" gap: bytes → `OscInbound`.
//!
//! ## Amplification bounds (ADR-039-A D2)
//!
//! The edge rate-limits per *datagram* but cannot see *inside* a bundle, so a
//! single small datagram could expand into many messages. Three independent
//! caps bound the work:
//! - [`OSC_MAX_BUNDLE_DEPTH`] — nesting depth,
//! - [`OSC_MAX_MSGS_PER_DATAGRAM`] — emitted messages,
//! - [`OSC_MAX_BUNDLE_NODES`] — **total visited nodes** (messages *and*
//!   bundles). The node cap is what stops a datagram built entirely of nested
//!   *empty* bundles from spinning CPU without ever tripping the message cap.
//!
//! On a cap trip the already-decoded messages are still returned (forwarded)
//! and [`ParsedDatagram::amplification_capped`] is set so the caller emits an
//! audit row — graceful degradation, not drop-all.

use conductor_core::actions::OscArg;
use conductor_core::events::OscInbound;
use std::time::Instant;

/// Maximum OSC bundle nesting depth.
pub const OSC_MAX_BUNDLE_DEPTH: usize = 8;
/// Maximum messages emitted from a single datagram.
pub const OSC_MAX_MSGS_PER_DATAGRAM: usize = 64;
/// Maximum total nodes (messages + bundles) visited while flattening one
/// datagram — bounds CPU for the nested-empty-bundle case.
pub const OSC_MAX_BUNDLE_NODES: usize = 256;

/// Decode failure (the datagram is dropped). Amplification is *not* an error —
/// see [`ParsedDatagram`].
#[derive(Debug, PartialEq, Eq)]
pub enum OscParseError {
    /// `rosc` could not decode the bytes.
    Decode(String),
    /// Decoded successfully but produced no usable message.
    Empty,
}

/// Result of decoding one datagram.
#[derive(Debug)]
pub struct ParsedDatagram {
    /// Flattened messages (already bounded by the caps).
    pub messages: Vec<OscInbound>,
    /// `true` if any amplification cap tripped — the caller should audit it.
    /// The `messages` already decoded are still valid and forwarded.
    pub amplification_capped: bool,
}

/// Decode one UDP datagram into zero-or-more [`OscInbound`].
///
/// `now` is injected (no `Instant::now()` inside) so tests are deterministic.
/// Bundle timetags are ignored in Phase A (immediate dispatch).
///
/// Returns `Err(Empty)` if decoding yielded no messages and nothing was capped;
/// `Err(Decode)` if `rosc` rejected the bytes; otherwise `Ok` with the messages
/// and the amplification flag.
pub fn parse_osc_datagram(bytes: &[u8], now: Instant) -> Result<ParsedDatagram, OscParseError> {
    let (_rest, packet) =
        rosc::decoder::decode_udp(bytes).map_err(|e| OscParseError::Decode(format!("{e:?}")))?;

    let mut messages = Vec::new();
    let mut nodes = 0usize;
    let mut capped = false;
    flatten(&packet, now, 0, &mut messages, &mut nodes, &mut capped);

    if messages.is_empty() && !capped {
        return Err(OscParseError::Empty);
    }
    Ok(ParsedDatagram {
        messages,
        amplification_capped: capped,
    })
}

/// Recursively flatten a packet under the three caps. Sets `*capped` on any trip.
fn flatten(
    packet: &rosc::OscPacket,
    now: Instant,
    depth: usize,
    out: &mut Vec<OscInbound>,
    nodes: &mut usize,
    capped: &mut bool,
) {
    // Visited-node budget: counted for EVERY node (message or bundle), so a
    // tree of empty bundles still terminates.
    if *nodes >= OSC_MAX_BUNDLE_NODES {
        *capped = true;
        return;
    }
    *nodes += 1;

    match packet {
        rosc::OscPacket::Message(m) => {
            if out.len() >= OSC_MAX_MSGS_PER_DATAGRAM {
                *capped = true;
                return;
            }
            out.push(OscInbound {
                address: m.addr.clone(),
                args: map_args(&m.args),
                time: now,
            });
        }
        rosc::OscPacket::Bundle(b) => {
            if depth >= OSC_MAX_BUNDLE_DEPTH {
                *capped = true;
                return;
            }
            for child in &b.content {
                if *nodes >= OSC_MAX_BUNDLE_NODES || out.len() >= OSC_MAX_MSGS_PER_DATAGRAM {
                    *capped = true;
                    break;
                }
                flatten(child, now, depth + 1, out, nodes, capped);
            }
        }
    }
}

/// Map `rosc` argument types into the daemon's [`OscArg`] vocabulary
/// (`Int`/`Float`/`String`). Total and panic-free; uncoercible OSC types
/// (blob, time, color, midi, nil, inf, array) are **omitted** — the transform
/// layer decides what to do with the surviving args. `Long`/`Double` narrow;
/// `Bool`→`Int(0|1)`; `Char`→`Int`.
fn map_args(args: &[rosc::OscType]) -> Vec<OscArg> {
    args.iter()
        .filter_map(|a| match a {
            rosc::OscType::Int(i) => Some(OscArg::Int(*i)),
            rosc::OscType::Float(f) => Some(OscArg::Float(*f)),
            rosc::OscType::String(s) => Some(OscArg::String(s.clone())),
            rosc::OscType::Long(l) => Some(OscArg::Int(
                (*l).clamp(i32::MIN as i64, i32::MAX as i64) as i32,
            )),
            rosc::OscType::Double(d) => Some(OscArg::Float(*d as f32)),
            rosc::OscType::Bool(b) => Some(OscArg::Int(if *b { 1 } else { 0 })),
            rosc::OscType::Char(c) => Some(OscArg::Int(*c as i32)),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rosc::{OscBundle, OscMessage, OscPacket, OscType};

    fn now() -> Instant {
        Instant::now()
    }

    fn encode(packet: &OscPacket) -> Vec<u8> {
        rosc::encoder::encode(packet).expect("encode")
    }

    fn msg(addr: &str, args: Vec<OscType>) -> OscPacket {
        OscPacket::Message(OscMessage {
            addr: addr.to_string(),
            args,
        })
    }

    fn bundle(content: Vec<OscPacket>) -> OscPacket {
        OscPacket::Bundle(OscBundle {
            timetag: (0, 1).into(),
            content,
        })
    }

    #[test]
    fn single_message_decodes_with_args() {
        let bytes = encode(&msg("/eos/fader/7", vec![OscType::Float(0.5)]));
        let out = parse_osc_datagram(&bytes, now()).expect("ok");
        assert_eq!(out.messages.len(), 1);
        assert_eq!(out.messages[0].address, "/eos/fader/7");
        assert_eq!(out.messages[0].args, vec![OscArg::Float(0.5)]);
        assert!(!out.amplification_capped);
    }

    #[test]
    fn flat_bundle_yields_all_messages() {
        let b = bundle(vec![
            msg("/a", vec![OscType::Int(1)]),
            msg("/b", vec![OscType::Int(2)]),
            msg("/c", vec![OscType::Int(3)]),
        ]);
        let out = parse_osc_datagram(&encode(&b), now()).expect("ok");
        assert_eq!(out.messages.len(), 3);
        assert!(!out.amplification_capped);
    }

    #[test]
    fn message_count_cap_trips_and_forwards_emitted() {
        let many: Vec<OscPacket> = (0..OSC_MAX_MSGS_PER_DATAGRAM + 10)
            .map(|i| msg(&format!("/m/{i}"), vec![OscType::Int(0)]))
            .collect();
        let out = parse_osc_datagram(&encode(&bundle(many)), now()).expect("ok");
        assert_eq!(out.messages.len(), OSC_MAX_MSGS_PER_DATAGRAM);
        assert!(out.amplification_capped, "message cap must trip");
    }

    #[test]
    fn nested_bundle_depth_cap_trips() {
        // Build a bundle nested deeper than the depth cap, with one message at
        // the bottom — it must be unreachable past the cap and trip `capped`.
        let mut inner = msg("/deep", vec![OscType::Int(1)]);
        for _ in 0..OSC_MAX_BUNDLE_DEPTH + 2 {
            inner = bundle(vec![inner]);
        }
        let out = parse_osc_datagram(&encode(&inner), now()).expect("ok");
        assert!(out.amplification_capped, "depth cap must trip");
    }

    #[test]
    fn empty_bundle_bomb_trips_node_cap_not_message_cap() {
        // A tree of empty bundles emits ZERO messages but visits many nodes.
        // Without the node cap this would spin without tripping the message cap.
        // Build a wide-and-shallow tree of empty bundles within depth.
        let empties: Vec<OscPacket> = (0..300).map(|_| bundle(vec![])).collect();
        let out = parse_osc_datagram(&encode(&bundle(empties)), now()).expect("ok");
        assert!(out.messages.is_empty());
        assert!(
            out.amplification_capped,
            "node cap must trip on empty-bundle bomb"
        );
    }

    #[test]
    fn malformed_bytes_error_no_panic() {
        let err = parse_osc_datagram(&[0xff, 0x00, 0x13, 0x37], now()).unwrap_err();
        assert!(matches!(err, OscParseError::Decode(_)));
    }

    #[test]
    fn empty_input_is_error() {
        let err = parse_osc_datagram(&[], now()).unwrap_err();
        // rosc rejects a zero-length datagram as a decode error; either Decode
        // or Empty is acceptable, just never a panic.
        assert!(matches!(
            err,
            OscParseError::Decode(_) | OscParseError::Empty
        ));
    }

    #[test]
    fn message_with_empty_args_is_fine() {
        let out = parse_osc_datagram(&encode(&msg("/bang", vec![])), now()).expect("ok");
        assert_eq!(out.messages.len(), 1);
        assert!(out.messages[0].args.is_empty());
    }

    #[test]
    fn arg_types_mapped_and_uncoercible_omitted() {
        let bytes = encode(&msg(
            "/mix",
            vec![
                OscType::Int(5),
                OscType::Float(0.25),
                OscType::String("hi".into()),
                OscType::Bool(true),
                OscType::Long(9),
                OscType::Double(0.5),
                OscType::Blob(vec![1, 2, 3]), // omitted
                OscType::Nil,                 // omitted
            ],
        ));
        let out = parse_osc_datagram(&bytes, now()).expect("ok");
        assert_eq!(
            out.messages[0].args,
            vec![
                OscArg::Int(5),
                OscArg::Float(0.25),
                OscArg::String("hi".into()),
                OscArg::Int(1),     // Bool(true)
                OscArg::Int(9),     // Long
                OscArg::Float(0.5), // Double
            ],
            "blob + nil omitted; long/double narrowed; bool→int"
        );
    }

    /// Highest-value security test: never panic over arbitrary bytes.
    #[test]
    fn never_panics_over_arbitrary_bytes() {
        let t = now();
        // Deterministic pseudo-random-ish byte strings of varied lengths.
        for seed in 0u32..2000 {
            let len = (seed % 64) as usize;
            let bytes: Vec<u8> = (0..len)
                .map(|i| (seed.wrapping_mul(2654435761).wrapping_add(i as u32) & 0xff) as u8)
                .collect();
            // Must return Ok or Err — never panic.
            let _ = parse_osc_datagram(&bytes, t);
        }
    }

    #[test]
    fn truncated_long_datagram_fails_clean() {
        // A valid message truncated mid-payload must error, not panic.
        let bytes = encode(&msg("/eos/fader/7", vec![OscType::Float(0.5)]));
        for cut in 1..bytes.len() {
            let _ = parse_osc_datagram(&bytes[..cut], now()); // no panic
        }
    }
}
