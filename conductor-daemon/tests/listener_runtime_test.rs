// ADR-042 Phase A — Slice A.6b-2: async UDP listener runtime.
//
// `spawn_listener` binds a loopback UDP socket for a ListenerEdge and runs a
// receive loop that applies the edge (ACL → rate-limit) to every packet's
// source. Accepted packets are forwarded on an mpsc channel (the protocol
// parser is a Phase A placeholder — ADR-039 fills OSC/Art-Net parsing).
// Rejected packets emit through an injected EdgeAuditSink (A.6b-3 supplies the
// AuditLogger-backed impl). The loop exits on a shutdown broadcast.
//
// These tests bind REAL loopback sockets and send UDP — they run (not just
// compile). They are kept deterministic by awaiting channels with a timeout
// (no sleeps) so they don't become timing flakes.
//
// Acceptance (issue #1898, spec §5 A.6):
//   cargo test --package conductor-daemon --test listener_runtime_test

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::sync::{broadcast, mpsc};
use tokio::time::timeout;

use conductor_core::config::types::ConnectorProtocol;
use conductor_daemon::listeners::runtime::{AcceptedPacket, EdgeAuditSink, spawn_listener};
use conductor_daemon::listeners::{AclFilter, AuditEventKind, ListenerEdge, RateLimitEdge};

fn lo() -> IpAddr {
    "127.0.0.1".parse().unwrap()
}

fn loopback_edge(total: u32, per_sender: u32) -> Arc<ListenerEdge> {
    let acl = AclFilter::from_config(&["127.0.0.0/8".to_string()], &[], false, false).unwrap();
    Arc::new(ListenerEdge::new(
        "osc_in".to_string(),
        ConnectorProtocol::Osc,
        lo(),
        0, // ephemeral port — the OS assigns one; read it back via local_addr()
        false,
        acl,
        RateLimitEdge::for_osc(total, per_sender),
    ))
}

/// Audit sink that forwards edge events on a channel so tests can await them.
struct ChannelAudit(mpsc::UnboundedSender<(String, IpAddr, AuditEventKind)>);
impl EdgeAuditSink for ChannelAudit {
    fn emit(&self, listener: &str, source: IpAddr, kind: AuditEventKind) {
        let _ = self.0.send((listener.to_string(), source, kind));
    }
}

#[tokio::test]
async fn binds_loopback_and_forwards_accepted_packet() {
    let (audit_tx, _audit_rx) = mpsc::unbounded_channel();
    let (pkt_tx, mut pkt_rx) = mpsc::channel(16);
    let (sh_tx, sh_rx) = broadcast::channel(1);

    let listener = spawn_listener(
        loopback_edge(1000, 200),
        pkt_tx,
        Arc::new(ChannelAudit(audit_tx)),
        sh_rx,
    )
    .await
    .expect("binds loopback");
    let addr = listener.local_addr();
    assert_eq!(addr.ip(), lo());
    assert_ne!(addr.port(), 0, "OS assigned a concrete ephemeral port");

    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    client.send_to(b"/hello\0\0", addr).await.unwrap();

    let pkt: AcceptedPacket = timeout(Duration::from_secs(2), pkt_rx.recv())
        .await
        .expect("packet within timeout")
        .expect("channel open");
    assert_eq!(pkt.source, lo());
    assert_eq!(pkt.data, b"/hello\0\0");
    assert_eq!(pkt.listener, "osc_in");

    // Await the receive task's exit so it can't outlive the test (Copilot #1953).
    let _ = sh_tx.send(());
    let _ = timeout(Duration::from_secs(2), listener.join()).await;
}

#[tokio::test]
async fn over_rate_limit_packet_emits_audit() {
    // per-sender budget of 1: the first loopback packet is accepted, the second
    // (same sender, same second) is rate-limited and emits a RateLimited audit.
    let (audit_tx, mut audit_rx) = mpsc::unbounded_channel();
    let (pkt_tx, mut pkt_rx) = mpsc::channel(16);
    let (sh_tx, sh_rx) = broadcast::channel(1);

    let listener = spawn_listener(
        loopback_edge(1000, 1),
        pkt_tx,
        Arc::new(ChannelAudit(audit_tx)),
        sh_rx,
    )
    .await
    .unwrap();
    let addr = listener.local_addr();

    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    client.send_to(b"first", addr).await.unwrap();
    client.send_to(b"second", addr).await.unwrap();

    // first packet accepted
    let _ = timeout(Duration::from_secs(2), pkt_rx.recv())
        .await
        .expect("first accepted")
        .expect("channel open");

    // second packet rate-limited → audit event (await it, no sleep)
    let (listener_name, source, kind) = timeout(Duration::from_secs(2), audit_rx.recv())
        .await
        .expect("audit within timeout")
        .expect("channel open");
    assert_eq!(listener_name, "osc_in");
    assert_eq!(source, lo());
    assert_eq!(kind, AuditEventKind::RateLimited);

    // Await the receive task's exit so it can't outlive the test (Copilot #1953).
    let _ = sh_tx.send(());
    let _ = timeout(Duration::from_secs(2), listener.join()).await;
}

#[tokio::test]
async fn shutdown_stops_the_listener() {
    let (audit_tx, _audit_rx) = mpsc::unbounded_channel();
    let (pkt_tx, _pkt_rx) = mpsc::channel(16);
    let (sh_tx, sh_rx) = broadcast::channel(1);

    let listener = spawn_listener(
        loopback_edge(1000, 200),
        pkt_tx,
        Arc::new(ChannelAudit(audit_tx)),
        sh_rx,
    )
    .await
    .unwrap();

    // Signal shutdown; the spawned task observes it and exits.
    sh_tx.send(()).expect("send shutdown");
    timeout(Duration::from_secs(2), listener.join())
        .await
        .expect("task exits within timeout")
        .expect("task did not panic");
}
