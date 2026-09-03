// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-042 Phase A — async UDP listener runtime.
//!
//! [`spawn_listener`] binds a loopback UDP socket for a [`ListenerEdge`] and
//! runs a receive loop that applies the edge to every datagram's source IP in
//! the mandated order (ACL → rate-limit, via [`ListenerEdge::admit`]):
//!
//! - **Accept** → forward the raw `(source, bytes)` on `packet_tx`. The protocol
//!   parser is a Phase A placeholder; ADR-039-A/039-C consume this channel to
//!   parse OSC / Art-Net.
//! - **RejectAcl** / **RejectRateLimit** → emit an audit event through the
//!   injected [`EdgeAuditSink`], rate-limited by the edge's own
//!   [`crate::listeners::AuditRateLimiter`] (one line per 60s per
//!   `(listener, source, kind)`).
//!
//! The loop exits on a shutdown broadcast (or when the packet sink closes). The
//! [`EdgeAuditSink`] seam decouples this runtime from the daemon's `AuditLogger`
//! so it is unit-testable; the engine wiring supplies the logger-backed sink (which
//! also emits `NetworkListenerActivity` on the accept path) and wires
//! `spawn_listener` into `EngineManager`.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use tokio::net::UdpSocket;
use tokio::sync::{broadcast, mpsc};
use tokio::task::{JoinError, JoinHandle};
use tracing::{debug, info, warn};

use crate::listeners::{AuditEventKind, EdgeDecision, ListenerEdge};

/// Largest UDP datagram read in one `recv_from`. OSC bundles and Art-Net frames
/// are comfortably under this; oversized datagrams are truncated (the parser
/// rejects malformed input downstream).
const MAX_DATAGRAM: usize = 2048;

/// A datagram that passed the listener edge, handed to the protocol parser.
/// In Phase A the parser is a placeholder; the raw bytes + verified source IP
/// are forwarded for ADR-039 to parse.
#[derive(Debug, Clone)]
pub struct AcceptedPacket {
    /// Connector alias of the listener that admitted this packet.
    pub listener: String,
    /// Verified (on-ACL, within-rate) source IP.
    pub source: IpAddr,
    /// Raw datagram bytes.
    pub data: Vec<u8>,
}

/// Sink for edge audit events. Decouples the runtime from the daemon's
/// `AuditLogger` so the receive loop is unit-testable. `emit` is called only
/// after the edge's dedup window allows it, so implementations may log directly.
pub trait EdgeAuditSink: Send + Sync + 'static {
    /// Record an edge event for `(listener, source, kind)`.
    fn emit(&self, listener: &str, source: IpAddr, kind: AuditEventKind);
}

/// Handle to a running network listener: its bound address and task.
pub struct NetworkListener {
    alias: String,
    local_addr: SocketAddr,
    handle: JoinHandle<()>,
}

impl NetworkListener {
    /// Connector alias.
    pub fn alias(&self) -> &str {
        &self.alias
    }

    /// The concrete bound address (port is resolved even when bound to `:0`).
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Abort the receive task immediately (used on reload when a listener is
    /// removed or rebound).
    pub fn abort(&self) {
        self.handle.abort();
    }

    /// Await the receive task's completion (after a shutdown signal).
    pub async fn join(self) -> Result<(), JoinError> {
        self.handle.await
    }
}

/// Bind a loopback UDP socket for `edge` and spawn its receive loop.
///
/// Returns once the socket is bound (so the caller can read `local_addr`), with
/// the loop running on a background task. A bind failure surfaces here as an
/// `io::Error` (the caller emits `NetworkListenerBindFailed`).
pub async fn spawn_listener(
    edge: Arc<ListenerEdge>,
    packet_tx: mpsc::Sender<AcceptedPacket>,
    audit: Arc<dyn EdgeAuditSink>,
    mut shutdown_rx: broadcast::Receiver<()>,
) -> std::io::Result<NetworkListener> {
    let bind_addr = SocketAddr::new(edge.host(), edge.port());
    let socket = UdpSocket::bind(bind_addr).await?;
    let local_addr = socket.local_addr()?;
    let alias = edge.alias().to_string();
    info!(listener = %alias, addr = %local_addr, "network listener bound (loopback)");

    let task_alias = alias.clone();
    let handle = tokio::spawn(async move {
        let mut buf = vec![0u8; MAX_DATAGRAM];
        loop {
            tokio::select! {
                // Shutdown signalled, or the sender was dropped → exit cleanly.
                _ = shutdown_rx.recv() => {
                    debug!(listener = %task_alias, "listener shutting down");
                    break;
                }
                recv = socket.recv_from(&mut buf) => {
                    match recv {
                        Ok((len, peer)) => {
                            let source = peer.ip();
                            match edge.admit(source) {
                                EdgeDecision::Accept => {
                                    let pkt = AcceptedPacket {
                                        listener: task_alias.clone(),
                                        source,
                                        data: buf[..len].to_vec(),
                                    };
                                    if packet_tx.send(pkt).await.is_err() {
                                        debug!(
                                            listener = %task_alias,
                                            "packet sink closed; stopping listener"
                                        );
                                        break;
                                    }
                                }
                                EdgeDecision::RejectAcl => {
                                    if edge.should_audit(source, AuditEventKind::AclRejected) {
                                        audit.emit(&task_alias, source, AuditEventKind::AclRejected);
                                    }
                                }
                                EdgeDecision::RejectRateLimit => {
                                    if edge.should_audit(source, AuditEventKind::RateLimited) {
                                        audit.emit(&task_alias, source, AuditEventKind::RateLimited);
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            // A transient recv error (e.g. ICMP port-unreachable
                            // surfaced on the socket) must not kill the listener.
                            warn!(listener = %task_alias, error = %e, "listener recv error");
                        }
                    }
                }
            }
        }
    });

    Ok(NetworkListener {
        alias,
        local_addr,
        handle,
    })
}
