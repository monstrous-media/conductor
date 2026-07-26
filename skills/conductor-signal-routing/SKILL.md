---
name: conductor-signal-routing
description: >
  Help users route, forward, pass through, split, or merge signals between
  devices and software endpoints in Conductor. Use when the user wants
  steady-state signal flow (controller → DAW, keyboard split across two
  synths, OSC bridge, LED return path) rather than per-event mappings.
  Covers `[[endpoints]]` (named I/O endpoints — ADR-035) and `[[routes]]`
  (signal paths between endpoints) introduced by ADR-031.
license: Apache-2.0
compatibility: Requires Conductor daemon running with MCP server enabled
metadata:
  author: amiable
  version: "5.7.0-alpha"
  category: routing
allowed-tools: Bash(conductor:*) Read Write
---

# Signal Routing

Help users compose **signal flow** between devices and software in Conductor.
Signal routing is **mode-independent** and **fan-out by default** — it sits
underneath per-event mappings and complements (does not replace) them.

## Three-Layer Evaluation Priority

When an input event arrives, Conductor evaluates in priority order:

1. **Mappings** (highest priority) — specific triggers like Note 36, Velocity
   range, LongPress. Per-event, mode-dependent. Use for "do something specific
   when this event happens" (e.g., Cmd+C on pad press).
2. **Routes** (this skill, ADR-031/036) — signal paths between connectors.
   A route is mode-independent by default (`modes` omitted), or scoped to
   specific modes via `modes = ["Bass"]` (ADR-036). Use for "always send
   keyboard channel 1 to Absynth and channel 10 to the drum sampler" — and,
   with `modes`, for "while in this mode, treat the controller as a
   passthrough to a specific destination."

A more specific layer ALWAYS wins over a broader one: per-event mappings
shadow routes. This means you can keep a permanent (mode-independent) route
in place AND override one specific event in one specific mode with a mapping
— without breaking the steady-state flow.

(Note: `Trigger::Raw` — the pre-ADR-036 catch-all passthrough — has been
removed. Express mode-dependent passthrough as a mode-scoped route.)

## Scope & Non-Goals

**This skill covers:**
- Listing connectors (`conductor_get_routing_graph`) and declared routes (`conductor_list_routes`)
- Inspecting live per-connector runtime metrics — `total_messages`, `throughput_msgs_per_sec` (10-second sliding window), `last_activity_ago_ms`, `error_count` — via `conductor_get_connector_metrics` (ReadOnly, ADR-031 § 6.2). Use this to answer "is connector X dropping messages?" or "is connector Y idle?" — distinct from `conductor_get_routing_graph` which is the static config view.
- Diagnosing unresolved connectors / dangling route endpoints via `conductor_get_resolved_routing_graph` (ReadOnly, no arguments — ADR-031 line 855). Returns every connector with its `bound_port` (the physical port the resolver actually matched, or `null` if unbound) and every route with `from_missing` / `to_missing` booleans flagging unresolved aliases. Use this when the user asks "why isn't my route firing?" or "what port did connector X actually bind to?" — distinct from `conductor_get_routing_graph` which returns the static config shape without resolver verdicts.
- Explaining a route-match outcome via `conductor_explain_route_match` (ReadOnly, ADR-036 D5 — args `event` { device, type, channel, data1, data2? } + `active_mode`). Evaluates a hypothetical MIDI event against the live RouteEngine across both phases and returns, per candidate route, `{ to_alias, phase, modes, fired, skip_reason }` where `skip_reason` is `mode_ineligible` / `filter_mismatch` (with the failing `dimension`) / `transform_produced_no_output`. This is the precise answer to "why didn't my route fire?" — it reports per-route filter/mode/transform verdicts that the resolved-graph tool (which only checks endpoint resolution) cannot.
- Reviewing recent dispatch decisions via `conductor_get_dispatch_trace` (ReadOnly, ADR-036 §8 — optional `last`, 1-256, default 32). Returns the most recent events that actually routed somewhere: `{ timestamp_ms, device_id, active_mode, phase, event, destinations }`, newest last. Use it to confirm a route fired after the user sent MIDI ("what did the router just do?"); the ring holds up to 1000 entries and is cleared on daemon restart.
- Creating output endpoints (`conductor_create_endpoint` — ADR-035, the single
  I/O-authoring tool; `direction = "Output"` / `"Bidirectional"`, `type` =
  Matcher / OscEndpoint / ArtNetEndpoint / MidiVirtualPort). The legacy
  `conductor_create_connector` tool and the `update_connector` / `delete_connector`
  batch ops were removed in ADR-035 Phase 2 (#1748); updating/deleting an existing
  endpoint via MCP is not yet supported (edit `[[endpoints]]` TOML or use the GUI)
- Creating, updating, and deleting routes via `conductor_batch_changes`
  with `create_route` / `update_route` / `delete_route` operations
  (per spec § 5.4 — route mutations always go through batch_changes,
  never singleton tools, so the LLM can author connector + routes +
  mappings — or evolve a routing graph — in a single Plan/Apply
  round-trip; `update_route` total-replaces the entry at the given
  0-based index)
- Designing the `[[endpoints]]` and `[[routes]]` config sections
- Decision framework for when to use a route vs. a mapping
- Common patterns: keyboard split, controller passthrough, cross-protocol
  bridge, LED return path
- DAW proxy model (`MidiVirtualPort` connectors visible to DAWs)

**This skill does NOT cover:**
- Per-event triggers (delegate to `conductor-midi-mapping`)
- MIDI Learn capture (delegate to `conductor-learn`)
- Binding setup or input-side discovery (delegate to `conductor-binding-setup`)
- OS-level driver installation (out of scope)

## Decision Framework

Walk through these six questions in order. The answers map directly onto a
`[[routes]]` block.

1. **Source identification** — which input port sends the signal?
   - Use `conductor_list_discovered_ports` + `conductor_get_routing_graph`.
   - The source must be an existing input binding (alias) or a connector
     with `direction = "Input"` / `"InputOutput"`.

2. **Destination identification** — where does the signal land?
   - Use `conductor_get_routing_graph`.
   - If the destination endpoint doesn't exist yet, create it with
     `conductor_create_endpoint` (ConfigChange tier — needs user approval;
     `direction = "Output"`).
   - For DAWs that need a virtual port: use `type = "MidiVirtualPort"`
     so Conductor exposes the port to the OS for the DAW to connect to.

3. **Scope** — full passthrough or filtered subset?
   - Full passthrough → route with no filter.
   - Filtered → specify `note_range`, `cc_range`, `channels`, or
     `message_types` so only matching messages flow.
   - Filters are AND-combined: `channels = [1]` + `note_range = [60, 72]`
     means "channel 1 notes between 60 and 72."

4. **Transform** — does the signal need conversion?
   - Same protocol, no remap → omit `transform`.
   - Same protocol, channel/CC/note remap → `Midi(MidiTransform { … })`
     (the same transform pipeline used by `MidiForward`).
   - Cross-protocol → `MidiToOsc`, `OscToMidi`, `MidiToArtNet` (ADR-031
     P5; not all transforms are wired yet — verify availability before
     promising).

5. **Intercepts** — does the user want to act on specific events too?
   - Yes → create the route AND a more specific mapping. The mapping wins
     for matching events; the route handles everything else.
   - No → route only.

6. **Mode-dependence** — should routing change with active mode?
   - Steady-state (always on) → route with no `modes` (ADR-031).
   - Mode-dependent behaviour → mode-scoped route, `modes = [...]` (ADR-036).
   - Per-mode destinations → one mode-scoped route per mode (disjoint `modes`).

## Common Patterns

### Pattern 1 — Keyboard split across two synths

The user says: "I want lower keys to play Absynth and upper keys to play the
drum sampler."

```toml
[[endpoints]]
alias = "absynth"
direction = "Output"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Absynth" }]

[[endpoints]]
alias = "drums"
direction = "Output"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Drum Sampler" }]

[[routes]]
from = "keyboard"
to = "absynth"
filter = { note_range = [21, 59] }   # A0–B3

[[routes]]
from = "keyboard"
to = "drums"
filter = { note_range = [60, 108] }  # C4–C8
```

Why two routes (not one with a transform)? The note ranges don't overlap, so
fan-out with filters is the cleanest expression. The user gets a clean
mental model: "lower keys go to Absynth, upper keys go to drums."

### Pattern 2 — Controller passthrough to DAW

```toml
[[endpoints]]
alias = "ableton"
direction = "Bidirectional"          # bidirectional for LED feedback
type = "MidiVirtualPort"
port_name = "Conductor: Ableton"

[[routes]]
from = "mikro"
to = "ableton"

[[routes]]
from = "ableton"
to = "mikro"
filter = { message_types = ["NoteOn", "NoteOff"] }   # only LEDs back
```

The `MidiVirtualPort` makes Conductor visible to Ableton as a regular MIDI
port. Forward route sends controller events into the DAW; return route sends
LED feedback messages back. The filter on the return route avoids echoing
the DAW's clock/transport into the controller.

### Pattern 3 — Cross-protocol bridge (MIDI → OSC)

```toml
[[endpoints]]
alias = "lighting"
direction = "Output"
type = "OscEndpoint"
host = "192.168.1.50"
port = 7700

[[routes]]
from = "lighting-controller"
to = "lighting"
filter = { cc_range = [0, 32] }
transform = { type = "MidiToOsc", address = "/light/{cc}/value", scale = "0_to_1" }
```

ADR-031 P5 covers cross-protocol transforms in detail. Verify the transform
type is implemented before recommending — `MidiToOsc` may land in a later
phase.

### Pattern 4 — Per-mode routing destination

The user wants the keyboard to drive Absynth normally, but in "Bass" mode
drive a synth bass instead. Express this as two **mode-scoped routes**
(ADR-036) with disjoint `modes` — each fires only in its own mode(s), so
there's no overlap to resolve.

```toml
# Default modes → Absynth
[[routes]]
from = "keyboard"
to = "absynth"
modes = ["Default", "Lead"]

# Bass mode → bass synth instead
[[routes]]
from = "keyboard"
to = "bass-synth"
modes = ["Bass"]
```

Each route's `modes` scopes it to the listed modes only. In "Bass" mode the
`bass-synth` route is active and the `absynth` route is not; switching modes
swaps which route applies. (Pre-ADR-036 this was a `Trigger::Raw` override;
Raw has been removed — use mode-scoped routes.)

## Safety Policy

- **Always require user approval for mutations.** `conductor_create_endpoint`
  is ConfigChange tier — it must surface a Plan/Apply review, never bypass
  it. Routes are config-level changes too (ADR-031 P3); treat them the same.
- **Verify endpoints before committing.** Before creating a route, confirm
  both the source binding/connector and the destination connector exist
  (`conductor_get_routing_graph`). A route to a non-existent alias is a
  validation error, not a runtime fallback.
- **Verify note/CC ranges before committing.** Use the L2 device-knowledge
  lookups (e.g., `device-note-ranges` chunk) or MIDI Learn rather than
  guessing. A wrong note range silently routes the wrong notes — the user
  may not notice until a live performance.
- **Warn about fan-out cost above 3 outputs.** A single source routed to
  many destinations multiplies per-event work. If the user is creating
  more than 3 fan-out routes from one source, surface the cost so they
  can confirm it's intentional.
- **Warn about cross-protocol transform requirements.** OSC needs an
  endpoint reachable on the network; ArtNet needs DMX hardware or a
  software receiver. Don't quietly promise something the user's setup
  can't deliver.
- **Don't hide overlap.** If two routes from the same source can match the
  same event (e.g., overlapping note ranges), surface the warning rather
  than silently letting the engine pick one. Route validation should
  emit overlap warnings modeled on `warn_raw_overlaps_specific` (ADR-030
  P3a precedent, PR #1127).

## Discoverability

This skill is the agent-side counterpart to four LLM-discoverability surfaces
that all reference signal routing (issue #1138 tracks unification):

1. `conductor-gui/ui/src/lib/stores/chat.js` — system prompt template
2. `docs/llm-reference.md` — markdown reference
3. `conductor-daemon/src/daemon/mcp_tools.rs` — daemon-side tool schemas
4. `conductor-gui/src-tauri/src/llm_commands.rs::get_mcp_tool_definitions` —
   GUI-side tool schemas

If you find a routing-related concept documented here that contradicts those
surfaces, the surfaces are stale; flag it for the user rather than working
from the contradictory copy.
