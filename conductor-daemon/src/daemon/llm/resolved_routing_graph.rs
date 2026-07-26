// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Pure-function builder for the `conductor_get_resolved_routing_graph`
//! MCP tool response (ADR-031 P6 / #1598 Phase 2 Step C).
//!
//! ## Why extracted
//!
//! The first Step C implementation read `bound_port` / `connected`
//! directly off `connector_registry`'s `LiveConnector` runtime fields
//! — which `connector_registry::from_config` initialises to
//! `None`/`false` and nothing in the daemon ever populates. Result:
//! the Routing Graph showed every connector as "unbound" while the
//! Bindings panel showed them all connected. The per-layer tests all
//! passed (registry test asserted the empty init; adapter test fed
//! synthesised payloads with `bound_port` set; pill test fed
//! synthesised connectors with `bound_port` set).
//!
//! The gap was no test exercising the actual production data path
//! end-to-end. See `[[tdd-must-exercise-production-data-path]]` in
//! the project memory for the lesson.
//!
//! ## Architecture (Option B from the design discussion)
//!
//! Instead of duplicating live state onto `LiveConnector` (Option A
//! — risk of drift on hot-plug), the resolver tool reads from the
//! SAME authoritative sources the Bindings panel + ActionExecutor
//! already use:
//!
//! - **Input side**: `InputManager::get_device_bindings()` — drives
//!   `connected` + `bound_port` for binding-derived Input connectors.
//! - **Output side**: `device_output_map` (alias → port_name) —
//!   drives `connected` + `bound_port` for any alias that resolved
//!   to a physical output port.
//!
//! Single source of truth = no drift. The registry retains its
//! routing-time role (config-derived shape + `resolve_output` for
//! the ActionExecutor hot path); the resolver tool just doesn't
//! rely on its runtime fields.
//!
//! ## Caveats acknowledged in this PR
//!
//! - **`auto_paired` is always `false`** in this response. The flat
//!   `HashMap<String, String>` shared with ActionExecutor lost the
//!   `OutputResolution.auto_paired` flag at the store sites. Tracked
//!   as a follow-up — a `device_output_map` type refactor across
//!   ActionExecutor restores it.
//! - **`port_index` is always `0`** — never tracked on the flat map.
//!   The wire field is preserved as a sentinel for back-compat with
//!   pre-Step-C consumers; consumers that care should look up the
//!   port name in the live enumeration.

use conductor_core::config::types::{ConnectorConfig, RouteConfig};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

/// One connector entry as the tool's caller will see it.
///
/// Borrows a `ConnectorConfig` from the caller — typically a registry
/// `LiveConnector.config` — for the lifetime `'a`. **The caller must
/// keep the underlying registry guard (or other config owner) alive
/// for as long as any `ConnectorView` is in use.** The helper
/// (`build_resolved_routing_graph_response`) consumes the slice
/// synchronously and returns, so the typical pattern is:
///
/// 1. Acquire all non-registry data first (`device_output_map` load,
///    `input_manager` lock + snapshot, `live_config.load()`).
/// 2. Acquire `connector_registry.read()`.
/// 3. Map `(_, live) → ConnectorView { config: &live.config }`.
/// 4. Call the helper. The registry guard stays alive across the
///    synchronous build.
/// 5. Guard drops at end of scope. No `.await` between (2) and (5)
///    or the lock-ordering invariant breaks (Copilot finding on PR
///    #1633).
///
/// Alias is read via `alias()` — `config.alias.as_str()` — rather
/// than carried as a separate field, so the two can't drift (Council
/// finding on PR #1633: an earlier shape with both
/// `alias: &'a str` and `config: &'a ConnectorConfig` let callers
/// construct a view where the two disagreed, silently misrendering
/// the response).
pub struct ConnectorView<'a> {
    pub config: &'a ConnectorConfig,
}

impl<'a> ConnectorView<'a> {
    /// Canonical alias for this view. Always `config.alias` —
    /// the single source of truth.
    pub fn alias(&self) -> &'a str {
        self.config.alias.as_str()
    }
}

/// One input-binding entry the resolver helper needs. Constructed by
/// the caller from `InputManager::get_device_bindings()` after
/// filtering to `is_configured: true`.
pub struct InputBindingEntry {
    pub alias: String,
    pub port_name: String,
    pub connected: bool,
    /// Runtime mute state (ADR-009 Phase 4b — `InputManager::is_device_enabled`,
    /// inverted). In-memory only (not persisted, unlike config `enabled=false`);
    /// surfaced so the routing canvas can render muted connectors (#1626).
    pub muted: bool,
}

/// Build the `conductor_get_resolved_routing_graph` response payload.
///
/// `connectors` MUST already be sorted by alias for stable diffs in
/// downstream consumers (the GUI canvas keys connector pills by
/// alias; reordering would cause spurious re-renders).
///
/// Pure: no I/O, no locks, no `await`. The caller acquires any
/// necessary read guards on `connector_registry` / `input_manager` /
/// `device_output_map` and hands the snapshots in.
pub fn build_resolved_routing_graph_response(
    connectors: &[ConnectorView<'_>],
    input_bindings: &[InputBindingEntry],
    output_map: &HashMap<String, String>,
    available_outputs: &HashSet<String>,
    routes: &[RouteConfig],
) -> Value {
    let known_aliases: HashSet<&str> = connectors.iter().map(|c| c.alias()).collect();

    let connector_entries: Vec<Value> = connectors
        .iter()
        .map(|c| {
            let alias = c.alias();
            // Output side wins on bound_port — that's what
            // MidiForward / SendMidi / resolve_output read at
            // dispatch time, so it's the "load-bearing" binding.
            let output_port = output_map.get(alias);
            let input_match = input_bindings.iter().find(|b| b.alias == alias);

            let bound_port = if let Some(port_name) = output_port {
                Some(json!({
                    "port_name": port_name,
                    "port_index": 0,
                    "auto_paired": false,
                }))
            } else {
                input_match.map(|b| {
                    json!({
                        "port_name": b.port_name,
                        "port_index": 0,
                        "auto_paired": false,
                    })
                })
            };

            // `connected` is OR — either side counts as connected.
            // Output-side (#2203): presence in `device_output_map` is NOT
            // sufficient — a `MidiVirtualPort` output is inserted into the
            // map unconditionally (output_resolver.rs), so a virtual port
            // that was never created (or a target that's input-only) would
            // otherwise report connected=true while every dispatch fails
            // `connect_by_name`. Gate on the LIVE output enumeration: the
            // resolved port_name must actually be present as an output port.
            // (Matcher outputs are already availability-checked at map build,
            // so this only tightens the virtual-port case; `bound_port` still
            // shows the configured target regardless.) Input-side: the live
            // `connected` flag from InputManager.
            let output_connected = output_port
                .map(|p| available_outputs.contains(p.as_str()))
                .unwrap_or(false);
            let connected = output_connected || input_match.map(|b| b.connected).unwrap_or(false);

            json!({
                "alias": alias,
                "direction": c.config.direction,
                "protocol": c.config.protocol,
                "enabled": c.config.enabled,
                "connected": connected,
                // #1626: runtime mute (in-memory, ADR-009 Phase 4b) — distinct
                // from persisted `enabled=false`. Only input/binding-derived
                // connectors carry it; absent from `input_bindings` ⇒ false.
                "muted": input_match.map(|b| b.muted).unwrap_or(false),
                "bound_port": bound_port,
                "description": c.config.description,
                "channels": c.config.channels,
            })
        })
        .collect();

    // #1634: emit a STABLE key derived from the route's shape + mode scope
    // rather than its array index. Index-based keys (`route-{idx}`) shift when a
    // mid-list route is deleted, so Svelte's keyed `#each` remounts the wrong
    // line and selection/hover/styling jumps to a neighbouring route. A
    // content-derived key survives reorders and deletes. The occurrence counter
    // disambiguates the rare exact-duplicate case — the config validator only
    // *warns* on identical routes, it does not reject them, so two routes can
    // hash equal; keep their keys distinct (and stable as long as their relative
    // order holds).
    let mut route_key_occurrences: HashMap<u64, u32> = HashMap::new();
    let route_entries: Vec<Value> = routes
        .iter()
        .map(|route| {
            let id = route_identity_hash(route);
            let occurrence = {
                let count = route_key_occurrences.entry(id).or_insert(0);
                let occ = *count;
                *count += 1;
                occ
            };
            json!({
                "key": format!("route-{:016x}-{}", id, occurrence),
                "from_alias": route.from,
                "to_alias": route.to,
                "from_missing": !known_aliases.contains(route.from.as_str()),
                "to_missing": !known_aliases.contains(route.to.as_str()),
                "enabled": route.enabled,
                "filter": route.filter,
                "transform": route.transform,
                "description": route.description,
                // ADR-036 D1: `modes` is the route's mode scope (empty =
                // global / all-modes); drives the GUI mode chips. (Phase 3
                // removed `phase` — all routes are post-mapping.)
                "modes": route.modes,
            })
        })
        .collect();

    json!({
        "connectors": connector_entries,
        "routes": route_entries,
    })
}

/// Stable identity hash for a route's *shape + mode scope* — the fields that
/// make two routes genuinely distinct, matching the duplicate-route validator's
/// `route_shapes_equal` (`from`/`to`/`filter`/`transform`) plus the ADR-036
/// `modes` scope. `enabled` and `description` are deliberately EXCLUDED so that
/// toggling a route on/off or editing its label does not change its key (no
/// spurious remount of the SVG line). `filter`/`transform`/`modes` are folded in
/// via their canonical JSON because the signal enums don't derive `Hash`.
///
/// Uses FNV-1a — an explicitly-specified, fixed algorithm — rather than
/// `DefaultHasher`, whose hashing algorithm is unspecified and may change
/// between Rust releases. The key only needs stability within a daemon process
/// run (it is recomputed on every `get_resolved_routing_graph` fetch and never
/// persisted), but a spec'd hash removes any version/rebuild ambiguity. Each
/// field is length-prefixed so concatenation can't alias across field
/// boundaries (e.g. `from="ab",to="c"` vs `from="a",to="bc"`).
///
/// `filter`/`transform`/`modes` are folded in via `canonical_json` (sorted
/// keys), NOT plain `serde_json::to_string` — several `SignalTransform` variants
/// (`MidiToArtNet`, `HidToArtNet`, `HidToMidi`, `HidToOsc`) hold `HashMap`
/// fields whose serialization order is otherwise nondeterministic, which would
/// make the *same* route hash differently on each fetch (Copilot finding on
/// #2330).
fn route_identity_hash(route: &RouteConfig) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    fn eat(hash: &mut u64, bytes: &[u8]) {
        for &b in bytes {
            *hash ^= b as u64;
            *hash = hash.wrapping_mul(FNV_PRIME);
        }
    }

    let filter_json = canonical_json(&route.filter);
    let transform_json = canonical_json(&route.transform);
    let modes_json = canonical_json(&route.modes);

    let mut hash = FNV_OFFSET;
    for field in [
        route.from.as_str(),
        route.to.as_str(),
        filter_json.as_str(),
        transform_json.as_str(),
        modes_json.as_str(),
    ] {
        // Length-prefix so field boundaries are unambiguous.
        eat(&mut hash, &(field.len() as u64).to_le_bytes());
        eat(&mut hash, field.as_bytes());
    }
    hash
}

/// Serialize `value` to JSON with object keys sorted at every level, so the
/// output is independent of `HashMap` iteration order (and of serde_json's
/// `preserve_order` feature). Array order is preserved (semantically
/// meaningful). Used to make `route_identity_hash` deterministic for transforms
/// carrying `HashMap` fields.
fn canonical_json<T: serde::Serialize>(value: &T) -> String {
    fn write(v: &Value, out: &mut String) {
        match v {
            Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                out.push('{');
                for (i, k) in keys.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::to_string(k).unwrap_or_default());
                    out.push(':');
                    write(&map[*k], out);
                }
                out.push('}');
            }
            Value::Array(arr) => {
                out.push('[');
                for (i, e) in arr.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(e, out);
                }
                out.push(']');
            }
            scalar => out.push_str(&scalar.to_string()),
        }
    }

    let value = serde_json::to_value(value).unwrap_or(Value::Null);
    let mut out = String::new();
    write(&value, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use conductor_core::config::types::{
        ConnectorConfig, ConnectorDirection, ConnectorProtocol, EndpointKind,
    };
    use conductor_core::identity::DeviceMatcher;

    fn binding_connector(alias: &str) -> ConnectorConfig {
        ConnectorConfig {
            alias: alias.to_string(),
            direction: ConnectorDirection::Input,
            protocol: ConnectorProtocol::Midi,
            endpoint: EndpointKind::Matcher {
                input_matchers: Vec::new(),
                output_matchers: Vec::new(),
                matchers: vec![DeviceMatcher::name_contains(alias)],
                no_probe: false,
            },
            description: None,
            enabled: true,
            channels: vec![],
        }
    }

    fn output_connector(alias: &str) -> ConnectorConfig {
        ConnectorConfig {
            alias: alias.to_string(),
            direction: ConnectorDirection::Output,
            protocol: ConnectorProtocol::Midi,
            endpoint: EndpointKind::Matcher {
                input_matchers: Vec::new(),
                output_matchers: Vec::new(),
                matchers: vec![DeviceMatcher::name_contains(alias)],
                no_probe: false,
            },
            description: None,
            enabled: true,
            channels: vec![],
        }
    }

    // ─── The bug-pin test ────────────────────────────────────────
    //
    // This is the vertical-slice test that would have caught the
    // Step C round-1 bug if it had existed: given a connector with
    // a matching input-binding (the production reality the user
    // sees in the Bindings panel), the response MUST report
    // `connected: true` and `bound_port: { port_name: ... }`.
    //
    // The round-1 code returned `connected: false, bound_port: null`
    // for everything because it read `LiveConnector.bound_port`
    // directly, which is initialised to `None` and never populated.

    #[test]
    fn muted_input_binding_reports_muted_true_on_its_connector() {
        // #1626: runtime mute (in-memory) must surface per connector so the
        // routing canvas can render it, distinct from persisted enabled=false.
        let mpk = binding_connector("mpk_input");
        let other = binding_connector("other_input");
        let connectors = vec![
            ConnectorView { config: &mpk },
            ConnectorView { config: &other },
        ];
        let input_bindings = vec![InputBindingEntry {
            alias: "mpk_input".to_string(),
            port_name: "MPK mini Mk II".to_string(),
            connected: true,
            muted: true,
        }];

        let response = build_resolved_routing_graph_response(
            &connectors,
            &input_bindings,
            &HashMap::new(),
            &HashSet::new(),
            &[],
        );

        let by_alias = |alias: &str| {
            response["connectors"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["alias"] == alias)
                .cloned()
                .unwrap()
        };
        assert_eq!(
            by_alias("mpk_input")["muted"],
            true,
            "muted input binding must report muted=true"
        );
        // A connector with no matching input binding defaults to muted=false.
        assert_eq!(
            by_alias("other_input")["muted"],
            false,
            "connector without an input binding must default muted=false"
        );
    }

    #[test]
    fn binding_derived_connector_with_matching_input_binding_reports_connected_and_bound_port() {
        let mpk = binding_connector("mpk_input");
        let connectors = vec![ConnectorView { config: &mpk }];
        let input_bindings = vec![InputBindingEntry {
            alias: "mpk_input".to_string(),
            port_name: "MPK mini Mk II".to_string(),
            connected: true,
            muted: false,
        }];
        let output_map = HashMap::new();
        let routes = vec![];

        let response = build_resolved_routing_graph_response(
            &connectors,
            &input_bindings,
            &output_map,
            &HashSet::new(),
            &routes,
        );

        let entry = &response["connectors"][0];
        assert_eq!(entry["alias"], "mpk_input");
        assert_eq!(
            entry["connected"], true,
            "connector with matching input binding must report connected=true"
        );
        assert_eq!(
            entry["bound_port"]["port_name"], "MPK mini Mk II",
            "bound_port.port_name must come from the input binding's port_name"
        );
    }

    #[test]
    fn output_connector_with_matching_device_output_map_entry_reports_connected_and_bound_port() {
        let absynth = output_connector("absynth_output");
        let connectors = vec![ConnectorView { config: &absynth }];
        let mut output_map = HashMap::new();
        output_map.insert(
            "absynth_output".to_string(),
            "Absynth 5 Virtual Output".to_string(),
        );

        // The resolved output port is present in the live enumeration → connected.
        let available_outputs: HashSet<String> = ["Absynth 5 Virtual Output".to_string()]
            .into_iter()
            .collect();
        let response = build_resolved_routing_graph_response(
            &connectors,
            &[],
            &output_map,
            &available_outputs,
            &[],
        );

        let entry = &response["connectors"][0];
        assert_eq!(entry["connected"], true);
        assert_eq!(entry["bound_port"]["port_name"], "Absynth 5 Virtual Output");
    }

    #[test]
    fn output_in_map_but_absent_from_live_enumeration_reports_disconnected_with_bound_port() {
        // #2203: a MidiVirtualPort output is inserted into device_output_map
        // unconditionally, so a virtual port that was never created (or an
        // input-only / nonexistent target) used to report connected=true while
        // every dispatch failed `connect_by_name`. The resolved port must be
        // present in the LIVE output enumeration to count as connected; the
        // configured target is still surfaced via bound_port.
        let virt = output_connector("virtual_test_out");
        let connectors = vec![ConnectorView { config: &virt }];
        let mut output_map = HashMap::new();
        output_map.insert(
            "virtual_test_out".to_string(),
            "Virtual Test Port".to_string(),
        );
        // "Virtual Test Port" is NOT among the live output ports.
        let available_outputs: HashSet<String> =
            ["IAC Driver Bus 1".to_string(), "TouchOSC".to_string()]
                .into_iter()
                .collect();

        let response = build_resolved_routing_graph_response(
            &connectors,
            &[],
            &output_map,
            &available_outputs,
            &[],
        );

        let entry = &response["connectors"][0];
        assert_eq!(
            entry["connected"], false,
            "output whose resolved port is absent from the live enumeration must be disconnected"
        );
        assert_eq!(
            entry["bound_port"]["port_name"], "Virtual Test Port",
            "the configured target port is still surfaced even when disconnected"
        );
    }

    #[test]
    fn connector_absent_from_both_sources_reports_unbound() {
        // The "unbound" state the GUI badge surfaces — daemon
        // explicitly says we know about this connector but it didn't
        // match any port.
        let ghost = binding_connector("ghost");
        let connectors = vec![ConnectorView { config: &ghost }];

        let response = build_resolved_routing_graph_response(
            &connectors,
            &[],
            &HashMap::new(),
            &HashSet::new(),
            &[],
        );

        let entry = &response["connectors"][0];
        assert_eq!(entry["connected"], false);
        assert_eq!(entry["bound_port"], Value::Null);
    }

    #[test]
    fn input_binding_with_connected_false_reports_disconnected_but_with_bound_port() {
        // Device is configured (binding exists) but currently
        // unplugged. `connected: false` per InputManager, but the
        // port_name from `device_info` is still in the binding
        // (we know what port it WOULD bind to). GUI distinguishes
        // "unbound" (no port) from "disconnected" (port known but
        // not live).
        let mpk = binding_connector("mpk_input");
        let connectors = vec![ConnectorView { config: &mpk }];
        let input_bindings = vec![InputBindingEntry {
            alias: "mpk_input".to_string(),
            port_name: "MPK mini Mk II".to_string(),
            connected: false,
            muted: false,
        }];

        let response = build_resolved_routing_graph_response(
            &connectors,
            &input_bindings,
            &HashMap::new(),
            &HashSet::new(),
            &[],
        );

        let entry = &response["connectors"][0];
        assert_eq!(entry["connected"], false);
        assert_eq!(entry["bound_port"]["port_name"], "MPK mini Mk II");
    }

    #[test]
    fn output_side_wins_on_bound_port_when_both_sides_have_an_entry() {
        // Bidirectional connectors can match BOTH input and output —
        // the output port is what consumers care about for
        // MidiForward / dispatch, so it takes precedence.
        let bridge = ConnectorConfig {
            alias: "bridge".to_string(),
            direction: ConnectorDirection::Bidirectional,
            ..output_connector("bridge")
        };
        let connectors = vec![ConnectorView { config: &bridge }];
        let input_bindings = vec![InputBindingEntry {
            alias: "bridge".to_string(),
            port_name: "Bridge IN".to_string(),
            connected: true,
            muted: false,
        }];
        let mut output_map = HashMap::new();
        output_map.insert("bridge".to_string(), "Bridge OUT".to_string());
        let available_outputs: HashSet<String> = ["Bridge OUT".to_string()].into_iter().collect();

        let response = build_resolved_routing_graph_response(
            &connectors,
            &input_bindings,
            &output_map,
            &available_outputs,
            &[],
        );

        let entry = &response["connectors"][0];
        assert_eq!(
            entry["bound_port"]["port_name"], "Bridge OUT",
            "output side must win on bound_port when both are present"
        );
        assert_eq!(entry["connected"], true);
    }

    // ─── Routes / from_missing / to_missing ──────────────────────

    fn route(from: &str, to: &str) -> RouteConfig {
        RouteConfig {
            from: from.to_string(),
            to: to.to_string(),
            filter: None,
            transform: None,
            enabled: true,
            description: None,
            modes: Vec::new(),
        }
    }

    #[test]
    fn route_with_known_endpoints_reports_neither_missing() {
        let mpk = binding_connector("mpk_input");
        let absynth = output_connector("absynth_output");
        let connectors = vec![
            ConnectorView { config: &mpk },
            ConnectorView { config: &absynth },
        ];
        let routes = vec![route("mpk_input", "absynth_output")];

        let response = build_resolved_routing_graph_response(
            &connectors,
            &[],
            &HashMap::new(),
            &HashSet::new(),
            &routes,
        );

        let r = &response["routes"][0];
        assert_eq!(r["from_missing"], false);
        assert_eq!(r["to_missing"], false);
        // #1634: key is now a stable content hash, not the array index.
        assert!(
            r["key"].as_str().unwrap().starts_with("route-"),
            "route key should be prefixed 'route-': {:?}",
            r["key"]
        );
    }

    // ─── #1634: route keys are stable across mid-list deletes ──────────
    // Index-based `route-{idx}` keys shifted on every delete, remounting the
    // wrong Svelte `#each` node. Content keys survive.
    fn route_keys(response: &Value) -> Vec<String> {
        response["routes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["key"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn route_keys_are_stable_across_mid_list_delete() {
        let a = binding_connector("a");
        let b = output_connector("b");
        let c = output_connector("c");
        let connectors = vec![
            ConnectorView { config: &a },
            ConnectorView { config: &b },
            ConnectorView { config: &c },
        ];

        let routes = vec![route("a", "b"), route("a", "c"), route("b", "c")];
        let before = route_keys(&build_resolved_routing_graph_response(
            &connectors,
            &[],
            &HashMap::new(),
            &HashSet::new(),
            &routes,
        ));

        // Delete the MIDDLE route — with index keys, the third route's key
        // would shift from route-2 to route-1; with content keys it must not.
        let routes_after = vec![route("a", "b"), route("b", "c")];
        let after = route_keys(&build_resolved_routing_graph_response(
            &connectors,
            &[],
            &HashMap::new(),
            &HashSet::new(),
            &routes_after,
        ));

        assert_eq!(after[0], before[0], "surviving first route key changed");
        assert_eq!(after[1], before[2], "surviving last route key changed");
    }

    #[test]
    fn distinct_routes_get_distinct_keys() {
        let a = binding_connector("a");
        let b = output_connector("b");
        let connectors = vec![ConnectorView { config: &a }, ConnectorView { config: &b }];
        let routes = vec![
            route("a", "b"),
            route_with("a", "b", vec!["Drums".to_string()]),
        ];
        let keys = route_keys(&build_resolved_routing_graph_response(
            &connectors,
            &[],
            &HashMap::new(),
            &HashSet::new(),
            &routes,
        ));
        assert_ne!(
            keys[0], keys[1],
            "differing mode scope must yield distinct keys"
        );
    }

    #[test]
    fn exact_duplicate_routes_get_distinct_keys_via_occurrence() {
        let a = binding_connector("a");
        let b = output_connector("b");
        let connectors = vec![ConnectorView { config: &a }, ConnectorView { config: &b }];
        // Two byte-identical routes (validator warns but allows them).
        let routes = vec![route("a", "b"), route("a", "b")];
        let keys = route_keys(&build_resolved_routing_graph_response(
            &connectors,
            &[],
            &HashMap::new(),
            &HashSet::new(),
            &routes,
        ));
        assert_ne!(keys[0], keys[1], "duplicate routes must not collide on key");
    }

    #[test]
    fn route_key_is_stable_for_hashmap_transforms_regardless_of_map_order() {
        // Several SignalTransform variants carry HashMap fields whose
        // serialization order is nondeterministic; the key must still be stable
        // (Copilot finding on #2330). Build the SAME logical transform with its
        // map entries inserted in different orders and assert equal keys.
        use conductor_core::config::types::SignalTransform;
        let a = binding_connector("a");
        let b = output_connector("b");
        let connectors = vec![ConnectorView { config: &a }, ConnectorView { config: &b }];

        let mk = |pairs: &[(&str, u8)]| {
            let mut m = std::collections::HashMap::new();
            for (k, v) in pairs {
                m.insert(k.to_string(), *v);
            }
            let mut r = route("a", "b");
            r.transform = Some(SignalTransform::HidToMidi {
                trigger_to_cc: m,
                channel: 0,
            });
            r
        };

        let r1 = mk(&[("south", 1), ("east", 2), ("west", 3)]);
        let r2 = mk(&[("west", 3), ("south", 1), ("east", 2)]);

        let k1 = route_keys(&build_resolved_routing_graph_response(
            &connectors,
            &[],
            &HashMap::new(),
            &HashSet::new(),
            &[r1],
        ));
        let k2 = route_keys(&build_resolved_routing_graph_response(
            &connectors,
            &[],
            &HashMap::new(),
            &HashSet::new(),
            &[r2],
        ));
        assert_eq!(
            k1[0], k2[0],
            "HashMap-transform route key must be independent of map order"
        );
    }

    #[test]
    fn route_with_unknown_endpoints_reports_missing() {
        let mpk = binding_connector("mpk_input");
        let connectors = vec![ConnectorView { config: &mpk }];
        let routes = vec![route("mpk_input", "ghost_output")];

        let response = build_resolved_routing_graph_response(
            &connectors,
            &[],
            &HashMap::new(),
            &HashSet::new(),
            &routes,
        );

        let r = &response["routes"][0];
        assert_eq!(r["from_missing"], false);
        assert_eq!(r["to_missing"], true);
    }

    // ─── modes on route entries (Slice 10 / #1668; ADR-036 Phase 3) ──
    //
    // ADR-036 D1 made `modes` first-class on RouteConfig. The
    // resolved-routing-graph response must surface it so the GUI can chip
    // mode-scoped routes (RoutingGraph.svelte / RouteInspector.svelte).
    // `modes` is a JSON string array (empty = global / all-modes). Phase 3
    // removed `phase` — all routes are post-mapping, so the response no
    // longer carries a `phase` field.

    fn route_with(from: &str, to: &str, modes: Vec<String>) -> RouteConfig {
        RouteConfig {
            from: from.to_string(),
            to: to.to_string(),
            filter: None,
            transform: None,
            enabled: true,
            description: None,
            modes,
        }
    }

    #[test]
    fn route_entry_includes_scoped_modes() {
        let mpk = binding_connector("mpk_input");
        let absynth = output_connector("absynth_output");
        let connectors = vec![
            ConnectorView { config: &mpk },
            ConnectorView { config: &absynth },
        ];
        let routes = vec![route_with(
            "mpk_input",
            "absynth_output",
            vec!["Mix".to_string(), "Edit".to_string()],
        )];

        let response = build_resolved_routing_graph_response(
            &connectors,
            &[],
            &HashMap::new(),
            &HashSet::new(),
            &routes,
        );

        let r = &response["routes"][0];
        assert!(
            r.get("phase").is_none(),
            "Phase 3 removed `phase` from the routing-graph response; got: {r}"
        );
        assert_eq!(
            r["modes"],
            json!(["Mix", "Edit"]),
            "scoped modes must round-trip as a JSON string array"
        );
    }

    #[test]
    fn route_entry_with_no_modes_emits_empty_array() {
        // A bare route (legacy, mode-independent) must emit `modes: []`
        // — NOT null and NOT an absent key — so the GUI's
        // `modes.length` chip check is uniform across all routes.
        let mpk = binding_connector("mpk_input");
        let connectors = vec![ConnectorView { config: &mpk }];
        let routes = vec![route_with("mpk_input", "mpk_input", Vec::new())];

        let response = build_resolved_routing_graph_response(
            &connectors,
            &[],
            &HashMap::new(),
            &HashSet::new(),
            &routes,
        );

        assert_eq!(response["routes"][0]["modes"], json!([]));
    }

    #[test]
    fn empty_inputs_produce_empty_arrays() {
        let response =
            build_resolved_routing_graph_response(&[], &[], &HashMap::new(), &HashSet::new(), &[]);
        assert_eq!(response["connectors"].as_array().unwrap().len(), 0);
        assert_eq!(response["routes"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn connector_view_alias_always_matches_config_alias() {
        // Council finding on PR #1633 — an earlier shape carried
        // `alias` and `config` independently, so a caller could
        // construct a view where the two disagreed and silently
        // misrender. The struct now reads `alias()` from
        // `config.alias` so they can't drift. This test pins the
        // invariant so a future shape change that reintroduces a
        // duplicate field is caught.
        let mpk = binding_connector("mpk_input");
        let view = ConnectorView { config: &mpk };
        assert_eq!(view.alias(), "mpk_input");
        assert_eq!(view.alias(), view.config.alias.as_str());
    }
}
