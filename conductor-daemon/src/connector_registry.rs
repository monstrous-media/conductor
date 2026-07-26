// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Endpoint Registry — runtime state of the signal routing graph (ADR-031 § 3.4,
//! ADR-035 Slice 6).
//!
//! Consumes the already-unified endpoint set produced by
//! [`conductor_core::config::loader::normalize_to_endpoints`] — which lowers the
//! legacy `[[bindings]]` (`DeviceIdentityConfig`) and `[[connectors]]`
//! (`ConnectorConfig`) blocks and folds in authored `[[endpoints]]` — into a
//! single alias-keyed map of `LiveConnector`s. Lowering is NO LONGER performed
//! here: ADR-035 Slice 6 collapsed it into core so there is one source of truth.
//! Each entry tracks its current bound port (if any) and per-connector activity
//! metrics.
//!
//! `ConnectorRegistry` remains as a type alias for `EndpointRegistry` during the
//! ADR-035 transition so existing daemon/MCP call sites compile unchanged.

use conductor_core::config::types::{ConnectorConfig, EndpointConfig, EndpointKind};
use std::collections::HashMap;
use std::net::UdpSocket;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Runtime state of the signal routing graph (ADR-031 D5).
pub struct EndpointRegistry {
    /// All connectors, keyed by alias. Holds authored `[[endpoints]]` plus the
    /// connectors lowered from legacy `[[bindings]]`/`[[connectors]]` upstream.
    connectors: HashMap<String, LiveConnector>,
}

/// Transitional alias (ADR-035 Slice 6). The runtime registry is now built from
/// the unified endpoint set; the old name is kept so call sites across the
/// daemon, MCP, and GUI compile unchanged until they migrate.
pub type ConnectorRegistry = EndpointRegistry;

/// One connector's runtime state.
pub struct LiveConnector {
    pub config: ConnectorConfig,
    pub bound_port: Option<BoundPort>,
    pub connected: bool,
    pub metrics: ConnectorMetrics,
    /// Ephemeral UDP socket for OSC sends (ADR-031 § 7.2 / #1145 P5 slice 2).
    /// Lazy: `None` until the first `send_osc` call on an OSC-protocol
    /// connector creates it bound to `0.0.0.0:0`. Persists across sends
    /// to avoid per-send `bind()` syscall overhead on high-rate streams
    /// (lighting consoles drive multi-kHz CC streams). `Some` only for
    /// connectors whose endpoint resolved to an `OscEndpoint`.
    pub osc_socket: Option<UdpSocket>,
    /// Art-Net DMX frame state + socket (ADR-031 § 7.3 / #1145 P5 slice 6).
    /// Lazy: `None` until the first `send_artnet` call on an Art-Net
    /// connector creates it. Persists across sends so multiple
    /// `DmxUpdate`s accumulate into the same 512-channel frame, and
    /// each call sends ONE full OpDmx packet containing the current
    /// frame state (Art-Net is a "send the whole frame" protocol —
    /// lighting fixtures interpolate between successive frames).
    pub artnet_state: Option<ArtNetState>,
}

/// Per-Art-Net-connector persistent state: the 512-channel DMX frame
/// + the UDP source socket used to send it.
///
/// Why both fields together: the frame and the socket have the same
/// lifecycle (lazy-init on first send, persist for the connector's
/// lifetime). Pairing them avoids two independent `Option`s on
/// `LiveConnector` that could only be `Some`/`Some` or `None`/`None`
/// in practice.
pub struct ArtNetState {
    /// 512-byte DMX universe data — index 0 is DMX channel 1.
    /// All-zero on init (lights off). Each `send_artnet` call mutates
    /// the channel(s) in the `DmxUpdate` and then sends the FULL
    /// frame as one OpDmx packet.
    pub frame: [u8; 512],
    /// Ephemeral UDP source socket. Persists for the connector lifetime
    /// to avoid `bind()` cost per send.
    pub socket: UdpSocket,
}

/// The physical port currently bound to a connector.
#[derive(Debug, Clone)]
pub struct BoundPort {
    pub port_name: String,
    pub port_index: usize,
    /// True if the port was auto-paired (matched by alias on hot-plug)
    /// rather than explicitly opened.
    pub auto_paired: bool,
}

/// Per-connector activity metrics (ADR-031 § 6.1).
///
/// `throughput_msgs_per_sec` is a snapshot field, NOT a live counter —
/// it's recomputed from `window` whenever `current_throughput()` is
/// called. The field stays in the struct for backward-compat with
/// existing serializers/readers that depend on the shape, but its
/// value is only meaningful right after a `current_throughput()` call
/// updates it. New code should call `current_throughput()` directly.
#[derive(Default, Clone)]
pub struct ConnectorMetrics {
    pub throughput_msgs_per_sec: f64,
    pub total_messages: u64,
    pub last_activity: Option<Instant>,
    pub error_count: u64,
    /// Sliding-window event timestamps for throughput calculation.
    /// Private — accessed via `record_activity` / `current_throughput`.
    window: ThroughputWindow,
}

/// 10-second sliding window of per-second event counts.
///
/// Implementation: array of 10 `(unix_second, count)` pairs. Each
/// `record(now)` either bumps the matching second's count or
/// replaces the oldest bucket. `snapshot(now)` sums the counts of
/// buckets within the last 10 seconds and divides by 10 — yields
/// messages-per-second over the rolling window.
///
/// All-zero initial state (`(0, 0)`) — second 0 is unix epoch 1970-01-01,
/// always older than any "last 10 seconds" cutoff, so empty buckets
/// are naturally excluded from snapshot sums without an `Option`
/// indirection.
#[derive(Clone, Default)]
struct ThroughputWindow {
    buckets: [(u64, u32); 10],
}

impl ThroughputWindow {
    /// Record one event observed at `now_unix_second`.
    ///
    /// If a bucket already exists for this second, increment its count.
    /// Otherwise replace the oldest bucket (smallest second). Pure
    /// in-place — no allocation, bounded to 10 cmp + 1 write.
    fn record(&mut self, now_unix_second: u64) {
        if let Some(b) = self.buckets.iter_mut().find(|(s, _)| *s == now_unix_second) {
            b.1 = b.1.saturating_add(1);
            return;
        }
        if let Some(b) = self.buckets.iter_mut().min_by_key(|(s, _)| *s) {
            *b = (now_unix_second, 1);
        }
    }

    /// Compute the messages-per-second rate over the 10-second window
    /// ending at `now_unix_second`. Pure read — does NOT mutate.
    ///
    /// Buckets older than `now_unix_second - 10` are excluded (covers
    /// both naturally-empty epoch-zero buckets AND legitimately stale
    /// buckets from periods of past activity).
    fn snapshot(&self, now_unix_second: u64) -> f64 {
        let cutoff = now_unix_second.saturating_sub(10);
        let sum: u32 = self
            .buckets
            .iter()
            .filter(|(sec, _)| *sec > cutoff)
            .map(|(_, count)| *count)
            .sum();
        f64::from(sum) / 10.0
    }
}

fn now_unix_second() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Project a unified [`EndpointConfig`] onto the runtime [`ConnectorConfig`]
/// stored in each [`LiveConnector`]. Keeping `LiveConnector::config` as
/// `ConnectorConfig` (rather than churning every reader to `EndpointConfig`)
/// preserves byte-for-byte parity with the pre-Slice-6 registry output.
///
/// Protocol comes from [`EndpointConfig::effective_protocol`] — the explicit
/// `protocol` override when authored, else inferred from the endpoint kind
/// (so an authored `OscEndpoint`/`ArtNetEndpoint` doesn't silently fall back
/// to the `ConnectorProtocol::Midi` default; `Matcher`/`MidiVirtualPort` infer
/// `Midi`, byte-for-byte parity with the legacy binding-lowering branch). The
/// output resolver shares this same inference path (ADR-035 Slice 9.5).
fn endpoint_to_connector_config(ep: &EndpointConfig) -> ConnectorConfig {
    ConnectorConfig {
        alias: ep.alias.clone(),
        direction: ep.direction,
        protocol: ep.effective_protocol(),
        endpoint: ep.kind.clone(),
        description: ep.description.clone(),
        enabled: ep.enabled,
        channels: ep.channels.clone(),
    }
}

impl EndpointRegistry {
    /// Build a new registry from the unified endpoint set (ADR-035 Slice 6).
    ///
    /// `endpoints` is the output of
    /// [`conductor_core::config::loader::normalize_to_endpoints`]: authored
    /// `[[endpoints]]` plus the connectors lowered from legacy
    /// `[[bindings]]`/`[[connectors]]`. Lowering and alias-collision detection
    /// already happened upstream (collisions hard-fail at `Config::load`), so
    /// the HashMap-insert "last writer wins" here is only a defensive fallback.
    pub fn from_config(endpoints: &[EndpointConfig]) -> Self {
        let mut map = HashMap::with_capacity(endpoints.len());

        for ep in endpoints {
            map.insert(
                ep.alias.clone(),
                LiveConnector {
                    config: endpoint_to_connector_config(ep),
                    bound_port: None,
                    connected: false,
                    metrics: ConnectorMetrics::default(),
                    osc_socket: None,
                    artnet_state: None,
                },
            );
        }

        Self { connectors: map }
    }

    /// Look up a connector by alias.
    pub fn get(&self, alias: &str) -> Option<&LiveConnector> {
        self.connectors.get(alias)
    }

    /// Check if an alias exists (either binding-derived or explicit connector).
    pub fn contains(&self, alias: &str) -> bool {
        self.connectors.contains_key(alias)
    }

    /// Iterate all `(alias, LiveConnector)` pairs. Order is unspecified
    /// (HashMap iteration). Used by the `conductor_get_resolved_routing_graph`
    /// MCP tool and the GUI's status panel to render the routing graph.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &LiveConnector)> {
        self.connectors.iter()
    }

    /// Get the output port name for a connector alias.
    /// Returns `None` if the connector is unknown, not currently
    /// connected, or has no bound port. The `connected` predicate is
    /// checked explicitly even though `bind_port` always sets both
    /// fields together — a defensive check against future disconnect
    /// paths that might clear only one (e.g. a teardown sequence
    /// cancelled mid-flight).
    pub fn resolve_output(&self, alias: &str) -> Option<&str> {
        self.connectors
            .get(alias)
            .filter(|c| c.connected)
            .and_then(|c| c.bound_port.as_ref())
            .map(|bp| bp.port_name.as_str())
    }

    /// Bind a physical port to a connector. Silent no-op if the alias
    /// is unknown (connector may have been removed by a concurrent reload).
    pub fn bind_port(
        &mut self,
        alias: &str,
        port_name: String,
        port_index: usize,
        auto_paired: bool,
    ) {
        if let Some(connector) = self.connectors.get_mut(alias) {
            connector.bound_port = Some(BoundPort {
                port_name,
                port_index,
                auto_paired,
            });
            connector.connected = true;
        }
    }

    /// Tear down the port binding for a connector — clears BOTH
    /// `bound_port` and `connected`. The `ConnectorConfig` itself
    /// remains in the registry (it is still a valid entry; only its
    /// physical-port state is reset). Used by the daemon on hot-plug
    /// removal and config-reload teardown (spec § 3.4.1 / § 3.4.2).
    /// Silent no-op if the alias is unknown.
    pub fn disconnect(&mut self, alias: &str) {
        if let Some(connector) = self.connectors.get_mut(alias) {
            connector.bound_port = None;
            connector.connected = false;
        }
    }

    /// Record one routed event for metrics. Silent no-op if alias is unknown.
    pub fn record_activity(&mut self, alias: &str) {
        if let Some(connector) = self.connectors.get_mut(alias) {
            connector.metrics.total_messages += 1;
            connector.metrics.last_activity = Some(Instant::now());
            // P4 slice 3 (gap C): also feed the sliding-window
            // throughput tracker so `current_throughput()` reflects
            // real activity instead of always returning 0.
            connector.metrics.window.record(now_unix_second());
        }
    }

    /// Compute the messages-per-second rate for a connector over its
    /// 10-second sliding window. Returns 0.0 for unknown aliases or
    /// connectors with no activity in the window.
    ///
    /// Pure read — does NOT mutate the underlying buckets. Safe to
    /// call from `&self` MCP / IPC paths, but the registry itself
    /// is currently behind `RwLock`, so this method takes `&self` and
    /// callers acquire a read lock.
    pub fn current_throughput(&self, alias: &str) -> f64 {
        self.connectors
            .get(alias)
            .map(|c| c.metrics.window.snapshot(now_unix_second()))
            .unwrap_or(0.0)
    }

    /// Record one routing-side error for metrics (ADR-031 P4 § 6.1 / #1144
    /// slice 2). Increments `ConnectorMetrics::error_count`. Silent
    /// no-op if alias is unknown.
    ///
    /// Distinct from `record_activity`: a route attempt that failed
    /// (e.g. action-executor queue full, downstream send error) should
    /// NOT increment `total_messages` because the message didn't reach
    /// the destination. Both counters are independent — `total_messages`
    /// reflects successful forwards, `error_count` reflects observed
    /// failures.
    ///
    /// `last_activity` is intentionally NOT touched on error — that
    /// field's contract is "last time data flowed", and an errored
    /// forward did not flow.
    pub fn record_error(&mut self, alias: &str) {
        if let Some(connector) = self.connectors.get_mut(alias) {
            connector.metrics.error_count += 1;
        }
    }

    /// Send an OSC packet to the connector's configured endpoint
    /// (ADR-031 § 7.2 / #1145 P5 slice 2).
    ///
    /// Lazy-opens an ephemeral UDP socket on first call per connector
    /// (bound to `0.0.0.0:0` — OS picks a free source port). The
    /// socket persists across sends so high-rate OSC streams don't
    /// pay the `bind()` syscall cost on every packet.
    ///
    /// Returns `Err` (and DOES NOT alter metrics — caller's
    /// responsibility) if:
    /// - the alias is unknown
    /// - the connector's endpoint isn't `OscEndpoint` (caller routed
    ///   the wrong destination — defensive)
    /// - the socket bind fails (port exhaustion, permission)
    /// - the `send_to` syscall fails (DNS, network unreachable)
    ///
    /// The caller (stage-9 dispatch in slice 3) decides whether to
    /// call `record_activity` (Ok) or `record_error` (Err). This
    /// method intentionally does NOT call them itself — separating
    /// the send from the metric write means a future caller can batch
    /// multiple sends per metric update if needed.
    pub fn send_osc(&mut self, alias: &str, packet_bytes: &[u8]) -> Result<(), String> {
        let Some(connector) = self.connectors.get_mut(alias) else {
            return Err(format!("unknown connector alias: '{alias}'"));
        };

        let (host, port) = match &connector.config.endpoint {
            EndpointKind::OscEndpoint { host, port, .. } => (host.clone(), *port),
            other => {
                return Err(format!(
                    "connector '{alias}' is not an OSC endpoint (got: {:?})",
                    other
                ));
            }
        };

        if connector.osc_socket.is_none() {
            let socket = UdpSocket::bind("0.0.0.0:0").map_err(|e| {
                format!("failed to bind ephemeral OSC source socket for '{alias}': {e}")
            })?;
            connector.osc_socket = Some(socket);
        }

        let socket = connector
            .osc_socket
            .as_ref()
            .expect("just set above; unwrap is safe inside the same lock scope");
        let dest = format!("{host}:{port}");
        socket
            .send_to(packet_bytes, &dest)
            .map_err(|e| format!("OSC send to '{dest}' failed: {e}"))?;
        Ok(())
    }

    /// Apply a `DmxUpdate` to the connector's persistent DMX frame and
    /// send the full frame as an Art-Net OpDmx UDP packet to the
    /// connector's configured endpoint (ADR-031 § 7.3 / #1145 P5
    /// slice 6).
    ///
    /// Lazy-initializes the per-connector `ArtNetState` (DMX frame +
    /// UDP source socket) on first call. Subsequent calls accumulate
    /// updates into the persistent frame — lighting fixtures interpolate
    /// between successive Art-Net frames, so the protocol is "send the
    /// whole frame each time, one channel at a time evolves" rather
    /// than "send only the channel that changed".
    ///
    /// Returns `Err` (and DOES NOT alter metrics — caller's
    /// responsibility) if:
    /// - the alias is unknown
    /// - the connector's endpoint isn't `ArtNetEndpoint` (caller routed
    ///   the wrong destination — defensive)
    /// - the channel value in the update is out of range (must be 1-512
    ///   per DMX spec; channel 0 and channels > 512 are rejected
    ///   here — they shouldn't reach this method because the validator
    ///   gates them at config-load, but defense-in-depth)
    /// - the socket bind fails (port exhaustion, permission)
    /// - the `send_to` syscall fails (DNS, network unreachable)
    pub fn send_artnet(
        &mut self,
        alias: &str,
        update: crate::transforms::midi_to_artnet::DmxUpdate,
    ) -> Result<(), String> {
        // Validate channel range FIRST so we don't lazy-init state for
        // a malformed update.
        if update.channel == 0 || update.channel > 512 {
            return Err(format!(
                "Art-Net DMX channel out of range for '{alias}': {} (must be 1-512)",
                update.channel
            ));
        }

        let Some(connector) = self.connectors.get_mut(alias) else {
            return Err(format!("unknown connector alias: '{alias}'"));
        };

        let (universe, host, port) = match &connector.config.endpoint {
            EndpointKind::ArtNetEndpoint {
                universe,
                host,
                port,
                ..
            } => (*universe, host.clone(), *port),
            other => {
                return Err(format!(
                    "connector '{alias}' is not an Art-Net endpoint (got: {:?})",
                    other
                ));
            }
        };

        if connector.artnet_state.is_none() {
            let socket = UdpSocket::bind("0.0.0.0:0").map_err(|e| {
                format!("failed to bind ephemeral Art-Net source socket for '{alias}': {e}")
            })?;
            // Art-Net is typically broadcast (default host
            // 255.255.255.255). Some platforms require explicit
            // SO_BROADCAST. Set it best-effort; ignore failure (most
            // user setups use a direct IP rather than broadcast).
            let _ = socket.set_broadcast(true);
            connector.artnet_state = Some(ArtNetState {
                frame: [0u8; 512],
                socket,
            });
        }

        let state = connector
            .artnet_state
            .as_mut()
            .expect("just set above; unwrap safe inside the same lock scope");

        // Apply update — channel is 1-indexed per DMX spec, array is 0-indexed.
        state.frame[(update.channel - 1) as usize] = update.value;

        let packet = encode_artnet_opdmx(universe, &state.frame);
        let dest = format!("{host}:{port}");
        state
            .socket
            .send_to(&packet, &dest)
            .map_err(|e| format!("Art-Net send to '{dest}' failed: {e}"))?;
        Ok(())
    }
}

/// Encode an Art-Net OpDmx packet (ADR-031 § 7.3 / #1145 P5 slice 6).
///
/// Wire format per <https://art-net.org.uk/structure/streaming-packets/artdmx-packet-definition/>:
///
/// ```text
/// Bytes 0..8    "Art-Net\0"     (8-byte null-terminated string ID)
/// Bytes 8..10   OpCode = 0x5000  (little-endian — OpDmx)
/// Byte  10      ProtVerHi = 0
/// Byte  11      ProtVerLo = 14   (Art-Net protocol version)
/// Byte  12      Sequence = 0     (0 = sequence-disabled per spec; some receivers want monotonic 1-255)
/// Byte  13      Physical = 0     (sender-side physical port info; 0 is acceptable)
/// Byte  14      SubUni           (low byte of universe — low 8 bits)
/// Byte  15      Net              (high byte of universe — bits 8-14)
/// Bytes 16..18  Length = 512     (big-endian — DMX data byte count)
/// Bytes 18..530 frame data       (512 bytes of channel values)
/// ```
///
/// Total packet size = 18 (header) + 512 (data) = 530 bytes.
///
/// **Universe layout (Art-Net 4)**: 15-bit universe = `(net << 8) | sub_uni`.
/// `net` is bits 8-14, `sub_uni` is bits 0-7. The validator gates the
/// 0-32767 range at config-load; this function trusts its input but
/// masks to 15 bits defensively so a value > 32767 doesn't corrupt
/// the Net byte.
fn encode_artnet_opdmx(universe: u16, frame: &[u8; 512]) -> Vec<u8> {
    let universe = universe & 0x7FFF; // 15-bit mask, defensive
    let sub_uni = (universe & 0xFF) as u8;
    let net = ((universe >> 8) & 0xFF) as u8;

    let mut packet = Vec::with_capacity(18 + 512);
    packet.extend_from_slice(b"Art-Net\0");
    packet.extend_from_slice(&0x5000u16.to_le_bytes()); // OpDmx
    packet.push(0); // ProtVerHi
    packet.push(14); // ProtVerLo
    packet.push(0); // Sequence (disabled)
    packet.push(0); // Physical
    packet.push(sub_uni);
    packet.push(net);
    packet.extend_from_slice(&512u16.to_be_bytes()); // Length (big-endian per spec)
    packet.extend_from_slice(frame);
    debug_assert_eq!(packet.len(), 530, "OpDmx packet must be exactly 530 bytes");
    packet
}

// ADR-035 Slice 6: the binding-side `Protocol → ConnectorProtocol` mapping that
// used to live here (`connector_protocol_from_binding_protocol`) is gone. Binding
// lowering — and that mapping — now live solely in
// `conductor_core::config::loader` (`connector_protocol_from_protocol`, used by
// `lower_binding`), so there is one source of truth for the conversion.

#[cfg(test)]
mod tests {
    use super::*;
    use conductor_core::config::types::{
        ConnectorConfig, ConnectorDirection, ConnectorProtocol, EndpointConfig, EndpointKind,
    };
    use conductor_core::identity::DeviceMatcher;

    /// ADR-035 Slice 6: `loader::lower_connector` was removed along with the
    /// legacy lowering path; the registry now consumes `EndpointConfig`
    /// directly. This local helper preserves the test-fixture ergonomics
    /// (build a `ConnectorConfig`, lower it) and mirrors the old conversion:
    /// the connector's `endpoint` field IS the endpoint `kind`, and its
    /// concrete `protocol` becomes the `Some(..)` override.
    fn lower_connector(c: ConnectorConfig) -> EndpointConfig {
        EndpointConfig {
            alias: c.alias,
            direction: c.direction,
            protocol: Some(c.protocol),
            description: c.description,
            enabled: c.enabled,
            channels: c.channels,
            kind: c.endpoint,
        }
    }

    fn make_test_registry() -> ConnectorRegistry {
        // ADR-035 Slice 6: the registry consumes the unified endpoint set, so
        // lower the test connector into an `EndpointConfig` first (the same
        // path `normalize_to_endpoints` uses for `[[connectors]]`).
        ConnectorRegistry::from_config(&[lower_connector(ConnectorConfig {
            alias: "absynth".to_string(),
            direction: ConnectorDirection::Output,
            protocol: ConnectorProtocol::Midi,
            endpoint: EndpointKind::Matcher {
                input_matchers: Vec::new(),
                output_matchers: Vec::new(),
                matchers: vec![DeviceMatcher::ExactName {
                    value: "Absynth".to_string(),
                }],
                no_probe: false,
            },
            description: None,
            enabled: true,
            channels: vec![],
        })])
    }

    /// `record_activity` increments `total_messages` and updates
    /// `last_activity`. Baseline test for symmetry with the new
    /// `record_error` test below; covers the slice-3a wiring path
    /// that previously had no dedicated registry-level unit test.
    #[test]
    fn record_activity_increments_total_and_sets_last_activity() {
        let mut registry = make_test_registry();

        // Fresh registry baseline.
        let baseline = registry.get("absynth").unwrap().metrics.clone();
        assert_eq!(baseline.total_messages, 0);
        assert!(baseline.last_activity.is_none());
        assert_eq!(baseline.error_count, 0);

        registry.record_activity("absynth");
        registry.record_activity("absynth");

        let metrics = &registry.get("absynth").unwrap().metrics;
        assert_eq!(metrics.total_messages, 2);
        assert!(metrics.last_activity.is_some());
        assert_eq!(
            metrics.error_count, 0,
            "record_activity must NOT touch error_count"
        );
    }

    /// `record_error` increments `error_count`. Mirrors `record_activity`
    /// shape, but distinct counters — a failed forward must NOT bump
    /// `total_messages` (the message never reached the destination).
    /// `last_activity` is also left alone because errors are by
    /// definition NOT data flow.
    #[test]
    fn record_error_increments_error_count_only() {
        let mut registry = make_test_registry();

        registry.record_error("absynth");
        registry.record_error("absynth");
        registry.record_error("absynth");

        let metrics = &registry.get("absynth").unwrap().metrics;
        assert_eq!(
            metrics.error_count, 3,
            "record_error must increment error_count once per call"
        );
        assert_eq!(
            metrics.total_messages, 0,
            "record_error must NOT increment total_messages — \
             that counter is reserved for successful forwards"
        );
        assert!(
            metrics.last_activity.is_none(),
            "record_error must NOT set last_activity — \
             that timestamp is for data flow, not error events"
        );
    }

    /// Unknown aliases are silent no-ops (both methods). Defensive
    /// contract — the dispatch path can't always know a fresh hot-plug
    /// removed the destination connector between compile and dispatch,
    /// and crashing on the metric write would mask the real signal.
    #[test]
    fn record_activity_and_record_error_unknown_alias_are_noops() {
        let mut registry = make_test_registry();
        // Neither should panic, neither should mutate the existing
        // entry's metrics (no cross-contamination via wrong-alias write).
        registry.record_activity("does_not_exist");
        registry.record_error("does_not_exist");
        let metrics = &registry.get("absynth").unwrap().metrics;
        assert_eq!(metrics.total_messages, 0);
        assert_eq!(metrics.error_count, 0);
    }

    // ── P4 slice 3 (gap C): ThroughputWindow tests ───────────────────
    //
    // The window is a private struct on ConnectorMetrics; these tests
    // exercise it directly so the bucket arithmetic is locked down
    // before higher-level integration tests rely on it.

    #[test]
    fn throughput_window_empty_snapshot_is_zero() {
        let w = ThroughputWindow::default();
        assert_eq!(
            w.snapshot(1_000_000),
            0.0,
            "fresh window must return 0 rate"
        );
    }

    #[test]
    fn throughput_window_single_event_yields_tenth_rate() {
        // 1 event in a 10-second window → 0.1 msg/s.
        let mut w = ThroughputWindow::default();
        w.record(1_000_000);
        assert_eq!(w.snapshot(1_000_000), 0.1);
    }

    #[test]
    fn throughput_window_events_in_same_second_share_bucket() {
        // 5 events all at the same second; rate is 5/10 = 0.5.
        let mut w = ThroughputWindow::default();
        for _ in 0..5 {
            w.record(1_000_000);
        }
        assert_eq!(w.snapshot(1_000_000), 0.5);
    }

    #[test]
    fn throughput_window_events_across_buckets_aggregate() {
        // 1 event per second for 5 different seconds → 5/10 = 0.5.
        let mut w = ThroughputWindow::default();
        for s in 1_000_000..1_000_005 {
            w.record(s);
        }
        assert_eq!(w.snapshot(1_000_004), 0.5);
    }

    #[test]
    fn throughput_window_decays_past_10_seconds() {
        // 10 buckets at 1-second granularity cover seconds [N-9, N]
        // inclusive (i.e. the 10 distinct seconds ending at `now`).
        // An event at second 1_000_000 is still in the window for
        // snapshots up through second 1_000_009 (10 seconds total:
        // 1_000_000, 1_000_001, ..., 1_000_009), and drops off at
        // 1_000_010.
        let mut w = ThroughputWindow::default();
        w.record(1_000_000);
        assert_eq!(
            w.snapshot(1_000_009),
            0.1,
            "at the inclusive far edge (9s later), the event is still counted \
             (covers 10 distinct seconds: 1_000_000..=1_000_009)"
        );
        assert_eq!(
            w.snapshot(1_000_010),
            0.0,
            "at 10s past, the event has fallen outside the rolling window"
        );
    }

    #[test]
    fn throughput_window_replaces_oldest_when_full() {
        // Fill all 10 buckets with distinct seconds, then add an 11th
        // event with a newer second. The OLDEST bucket (second
        // 1_000_000) should be replaced — the second 1_000_010
        // observation survives.
        let mut w = ThroughputWindow::default();
        for s in 1_000_000..1_000_010 {
            w.record(s);
        }
        // All 10 buckets occupied, sum = 10, rate = 1.0 in window
        // centered on second 1_000_009.
        assert_eq!(w.snapshot(1_000_009), 1.0);

        // Add an 11th event at second 1_000_010. Bucket for
        // 1_000_000 must be replaced (oldest); buckets for
        // 1_000_001..=1_000_010 now occupied. Rate at snapshot
        // time 1_000_010 covers cutoff > 1_000_000, so all 10
        // surviving buckets count, sum=10, rate=1.0.
        w.record(1_000_010);
        assert_eq!(
            w.snapshot(1_000_010),
            1.0,
            "after replacing oldest bucket, 10 events remain in the window"
        );

        // Sanity: bucket for second 1_000_000 is gone — snapshot
        // at cutoff=1_000_000 (i.e., now=1_000_010, cutoff=now-10)
        // must not double-count it.
        let cutoff_bucket_count = w.buckets.iter().filter(|(s, _)| *s == 1_000_000).count();
        assert_eq!(
            cutoff_bucket_count, 0,
            "oldest bucket must be evicted when full window receives new second"
        );
    }

    /// AC: `record_activity` populates the throughput window so
    /// `current_throughput()` reflects real activity instead of
    /// always returning 0 (pre-slice this was unwired).
    #[test]
    fn record_activity_populates_throughput_window() {
        let mut registry = make_test_registry();
        assert_eq!(
            registry.current_throughput("absynth"),
            0.0,
            "fresh registry must report 0 throughput"
        );

        // Fire 3 events. record_activity uses real wall-clock seconds
        // so the snapshot at this moment must include them.
        for _ in 0..3 {
            registry.record_activity("absynth");
        }

        // Rate ≥ 0.3 — 3 events spread across at most a few seconds,
        // worst-case all in one bucket means 3/10 = 0.3.
        let rate = registry.current_throughput("absynth");
        assert!(
            rate >= 0.3,
            "3 record_activity calls must yield rate ≥ 0.3 (got {rate})"
        );
    }

    #[test]
    fn current_throughput_unknown_alias_returns_zero() {
        let registry = make_test_registry();
        assert_eq!(
            registry.current_throughput("ghost"),
            0.0,
            "unknown alias must return 0 — defensive contract for hot-plug races"
        );
    }

    // ── P5 slice 2 (gap B): OSC sender tests ───────────────────────
    //
    // Real UDP via loopback — the receiver socket is bound first to
    // an ephemeral local port, the test config points the connector
    // at that port, and `send_osc` is verified by the receiver
    // observing the bytes. Tests are tagged-by-name so they can be
    // skipped in environments with strict network sandboxing if
    // needed; localhost UDP should work on all developer + CI
    // platforms today.

    /// Build a registry with one OSC connector pointing at `host:port`.
    /// Helper for the slice-2 send tests.
    fn make_osc_registry(alias: &str, host: &str, port: u16) -> ConnectorRegistry {
        ConnectorRegistry::from_config(&[lower_connector(ConnectorConfig {
            alias: alias.to_string(),
            direction: ConnectorDirection::Output,
            protocol: ConnectorProtocol::Osc,
            endpoint: EndpointKind::OscEndpoint {
                host: host.to_string(),
                port,
                security: Default::default(),
            },
            description: None,
            enabled: true,
            channels: vec![],
        })])
    }

    #[test]
    fn send_osc_delivers_bytes_to_destination() {
        // Bind a receiver socket to a free localhost port, point the
        // connector at it, send a fixture packet, assert receipt.
        let receiver = UdpSocket::bind("127.0.0.1:0").expect("receiver bind");
        receiver
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .expect("set read timeout");
        let dest_port = receiver.local_addr().expect("local_addr").port();

        let mut registry = make_osc_registry("eos_console", "127.0.0.1", dest_port);

        // Hand-crafted OSC packet ("/foo" with no args) — 12 bytes.
        // We're not exercising the rosc encoder here; that's slice 1's
        // job. This test just pins that whatever bytes we hand to
        // send_osc land at the receiver intact.
        let packet: &[u8] = b"/foo\0\0\0\0,\0\0\0";

        registry
            .send_osc("eos_console", packet)
            .expect("send_osc must succeed for valid OSC endpoint");

        let mut buf = [0u8; 1024];
        let (n, _src) = receiver.recv_from(&mut buf).expect("receiver got bytes");
        assert_eq!(&buf[..n], packet, "receiver must see the exact bytes sent");
    }

    #[test]
    fn send_osc_persists_socket_across_calls() {
        // First send lazy-opens; second send reuses. Pinning the
        // "no per-send bind" optimization the lazy-init was added
        // for. We can't assert the syscall count directly from
        // user-space, but we CAN assert that two sends in a row both
        // succeed AND that `osc_socket.is_some()` stays true.
        let receiver = UdpSocket::bind("127.0.0.1:0").expect("receiver bind");
        receiver
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .expect("set read timeout");
        let dest_port = receiver.local_addr().expect("local_addr").port();

        let mut registry = make_osc_registry("pad", "127.0.0.1", dest_port);

        assert!(
            registry.get("pad").unwrap().osc_socket.is_none(),
            "socket must not exist before the first send"
        );

        registry
            .send_osc("pad", b"/a\0\0,\0\0\0")
            .expect("first send");
        assert!(
            registry.get("pad").unwrap().osc_socket.is_some(),
            "first send must lazy-init the socket"
        );

        registry
            .send_osc("pad", b"/b\0\0,\0\0\0")
            .expect("second send");
        assert!(
            registry.get("pad").unwrap().osc_socket.is_some(),
            "second send must reuse the socket"
        );

        let mut buf = [0u8; 1024];
        let (n1, _) = receiver.recv_from(&mut buf).expect("first recv");
        assert_eq!(&buf[..n1], b"/a\0\0,\0\0\0");
        let (n2, _) = receiver.recv_from(&mut buf).expect("second recv");
        assert_eq!(&buf[..n2], b"/b\0\0,\0\0\0");
    }

    #[test]
    fn send_osc_unknown_alias_returns_err_with_alias_in_message() {
        let mut registry = make_osc_registry("known", "127.0.0.1", 9000);
        let err = registry
            .send_osc("ghost", b"/x\0\0,\0\0\0")
            .expect_err("unknown alias must return Err");
        assert!(
            err.contains("ghost"),
            "error message must include the offending alias (got: {err})"
        );
    }

    #[test]
    fn send_osc_rejects_non_osc_endpoint() {
        // Defensive: caller routed to a MIDI/HID/ArtNet/MidiVirtualPort
        // connector by mistake. Return Err rather than silently no-op.
        let registry = make_test_registry(); // returns an EndpointKind::Matcher MIDI connector
        let mut registry = registry; // drop immutability
        let err = registry
            .send_osc("absynth", b"/x\0\0,\0\0\0")
            .expect_err("non-OSC endpoint must return Err");
        assert!(
            err.contains("not an OSC endpoint"),
            "error must clearly identify the endpoint-type mismatch (got: {err})"
        );
    }

    // ── P5 slice 6 (gap F): Art-Net sender tests ──────────────────
    //
    // Real UDP via loopback — receiver bound first, the connector
    // points at it, send_artnet is verified via the receiver getting
    // a valid OpDmx packet. Frame state pinned across multiple sends.

    fn make_artnet_registry(
        alias: &str,
        host: &str,
        port: u16,
        universe: u16,
    ) -> ConnectorRegistry {
        ConnectorRegistry::from_config(&[lower_connector(ConnectorConfig {
            alias: alias.to_string(),
            direction: ConnectorDirection::Output,
            protocol: ConnectorProtocol::ArtNet,
            endpoint: EndpointKind::ArtNetEndpoint {
                universe,
                host: host.to_string(),
                port,
                allow_broadcast: false,
                security: Default::default(),
            },
            description: None,
            enabled: true,
            channels: vec![],
        })])
    }

    #[test]
    fn encode_artnet_opdmx_produces_530_byte_packet_with_correct_header() {
        // Pin the wire format byte-by-byte so a future tweak to the
        // encoder surfaces here, not at runtime when a lighting
        // fixture mysteriously stops responding.
        let frame = [0u8; 512];
        let packet = encode_artnet_opdmx(0, &frame);
        assert_eq!(packet.len(), 530, "OpDmx packet must be 530 bytes");
        assert_eq!(&packet[0..8], b"Art-Net\0", "ID prefix");
        // OpCode 0x5000 little-endian = 0x00 0x50
        assert_eq!(&packet[8..10], &[0x00, 0x50], "OpCode = OpDmx");
        assert_eq!(packet[10], 0, "ProtVerHi");
        assert_eq!(packet[11], 14, "ProtVerLo");
        assert_eq!(packet[12], 0, "Sequence = disabled");
        assert_eq!(packet[13], 0, "Physical");
        // Length (bytes 16-17) is big-endian 512 = 0x02 0x00
        assert_eq!(&packet[16..18], &[0x02, 0x00], "Length = 512 (big-endian)");
    }

    #[test]
    fn encode_artnet_opdmx_splits_universe_into_subuni_and_net() {
        // Universe 0x0123 → sub_uni = 0x23, net = 0x01.
        // Pinning the bit-layout because Art-Net's two-byte universe
        // split is easy to invert.
        let frame = [0u8; 512];
        let packet = encode_artnet_opdmx(0x0123, &frame);
        assert_eq!(packet[14], 0x23, "SubUni = low byte of universe");
        assert_eq!(packet[15], 0x01, "Net = high byte (bits 8-14)");
    }

    #[test]
    fn encode_artnet_opdmx_masks_universe_to_15_bits() {
        // Universe > 32767 (0x8000+) would corrupt Net if not masked.
        // Defensive: validator should catch this at config-load, but
        // mask here too so the wire bytes are always spec-valid.
        let frame = [0u8; 512];
        let packet = encode_artnet_opdmx(0xFFFF, &frame); // would be 0x7FFF after mask
        assert_eq!(packet[14], 0xFF, "SubUni = 0xFF (low byte after mask)");
        assert_eq!(
            packet[15], 0x7F,
            "Net = 0x7F (high 7 bits — bit 15 masked off)"
        );
    }

    #[test]
    fn send_artnet_delivers_opdmx_packet_with_updated_channel() {
        let receiver = UdpSocket::bind("127.0.0.1:0").expect("receiver bind");
        receiver
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .expect("set read timeout");
        let dest_port = receiver.local_addr().expect("local_addr").port();

        let mut registry = make_artnet_registry("lights", "127.0.0.1", dest_port, 0);
        registry
            .send_artnet(
                "lights",
                crate::transforms::midi_to_artnet::DmxUpdate {
                    channel: 5,
                    value: 200,
                },
            )
            .expect("send_artnet must succeed");

        let mut buf = [0u8; 1024];
        let (n, _) = receiver.recv_from(&mut buf).expect("recv");
        assert_eq!(n, 530, "must receive a full OpDmx packet");
        // Verify the updated channel: byte index = channel - 1 + 18 (header).
        assert_eq!(buf[18 + 4], 200, "DMX channel 5 must be set to 200");
        // All other channels should be 0 (frame initialized to zeros).
        for i in 0..512 {
            if i == 4 {
                continue;
            }
            assert_eq!(buf[18 + i], 0, "DMX channel {} should be 0", i + 1);
        }
    }

    #[test]
    fn send_artnet_accumulates_multiple_updates_in_persistent_frame() {
        // Lighting fixtures interpolate between successive Art-Net
        // frames. Each send must contain the CURRENT state of all
        // channels, not just the one that changed. Pin the
        // accumulation contract by sending 3 updates and verifying
        // the final packet has all 3 values set.
        let receiver = UdpSocket::bind("127.0.0.1:0").expect("receiver bind");
        receiver
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .expect("set read timeout");
        let dest_port = receiver.local_addr().expect("local_addr").port();

        let mut registry = make_artnet_registry("lights", "127.0.0.1", dest_port, 0);
        for (ch, v) in &[(1u16, 100u8), (50, 200), (512, 50)] {
            registry
                .send_artnet(
                    "lights",
                    crate::transforms::midi_to_artnet::DmxUpdate {
                        channel: *ch,
                        value: *v,
                    },
                )
                .unwrap();
        }

        // Drain receiver — last packet must reflect all three updates.
        let mut buf = [0u8; 1024];
        let mut last_packet = vec![];
        while let Ok((n, _)) = receiver.recv_from(&mut buf) {
            last_packet = buf[..n].to_vec();
        }
        assert_eq!(last_packet.len(), 530);
        assert_eq!(last_packet[18], 100, "channel 1 in final frame");
        assert_eq!(last_packet[18 + 49], 200, "channel 50 in final frame");
        assert_eq!(last_packet[18 + 511], 50, "channel 512 in final frame");
    }

    #[test]
    fn send_artnet_persists_socket_across_calls() {
        let receiver = UdpSocket::bind("127.0.0.1:0").expect("receiver bind");
        let dest_port = receiver.local_addr().expect("local_addr").port();
        let mut registry = make_artnet_registry("lights", "127.0.0.1", dest_port, 0);

        assert!(registry.get("lights").unwrap().artnet_state.is_none());
        registry
            .send_artnet(
                "lights",
                crate::transforms::midi_to_artnet::DmxUpdate {
                    channel: 1,
                    value: 50,
                },
            )
            .unwrap();
        assert!(
            registry.get("lights").unwrap().artnet_state.is_some(),
            "first send must lazy-init"
        );
        // Second send must reuse — frame already has channel 1 = 50,
        // and we add channel 2 = 100, so the next packet should have
        // both set.
        registry
            .send_artnet(
                "lights",
                crate::transforms::midi_to_artnet::DmxUpdate {
                    channel: 2,
                    value: 100,
                },
            )
            .unwrap();
        let state = registry
            .get("lights")
            .unwrap()
            .artnet_state
            .as_ref()
            .unwrap();
        assert_eq!(state.frame[0], 50, "channel 1 (index 0) preserved");
        assert_eq!(state.frame[1], 100, "channel 2 (index 1) added");
    }

    #[test]
    fn send_artnet_unknown_alias_returns_err() {
        let mut registry = make_artnet_registry("lights", "127.0.0.1", 9000, 0);
        let err = registry
            .send_artnet(
                "ghost",
                crate::transforms::midi_to_artnet::DmxUpdate {
                    channel: 1,
                    value: 50,
                },
            )
            .expect_err("unknown alias");
        assert!(err.contains("ghost"), "error must include offending alias");
    }

    #[test]
    fn send_artnet_rejects_non_artnet_endpoint() {
        let mut registry = make_test_registry(); // MIDI/Matcher endpoint
        let err = registry
            .send_artnet(
                "absynth",
                crate::transforms::midi_to_artnet::DmxUpdate {
                    channel: 1,
                    value: 50,
                },
            )
            .expect_err("wrong endpoint type");
        assert!(
            err.contains("not an Art-Net endpoint"),
            "error must clearly identify the endpoint-type mismatch (got: {err})"
        );
    }

    #[test]
    fn send_artnet_rejects_out_of_range_channels() {
        let mut registry = make_artnet_registry("lights", "127.0.0.1", 9000, 0);
        // Channel 0 — DMX is 1-indexed, channel 0 is invalid.
        let err0 = registry
            .send_artnet(
                "lights",
                crate::transforms::midi_to_artnet::DmxUpdate {
                    channel: 0,
                    value: 50,
                },
            )
            .expect_err("channel 0 must be rejected");
        assert!(err0.contains("0"));
        // Channel 513 — DMX universe is 512 channels.
        let err513 = registry
            .send_artnet(
                "lights",
                crate::transforms::midi_to_artnet::DmxUpdate {
                    channel: 513,
                    value: 50,
                },
            )
            .expect_err("channel 513 must be rejected");
        assert!(err513.contains("513"));
    }
}
