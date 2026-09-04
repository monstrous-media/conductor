# Testing Guide

This guide covers how Conductor's test suites are organized and run,
hardware-independent testing via the MIDI device simulator, and the
conventions used when writing new tests.

- [Running Tests](#running-tests)
- [Test Organization](#test-organization)
- [MIDI Device Simulator](#midi-device-simulator)
- [Fixtures](#fixtures)
- [Writing Tests](#writing-tests)
- [Gamepad / HID Testing](#gamepad--hid-testing)
- [Benchmarks](#benchmarks)
- [Continuous Integration](#continuous-integration)

## Running Tests

```bash
# Everything, every feature (what `just ci` runs as its test step)
just test
# == cargo test --workspace --all-features
```

`--workspace` matters: without it, `cargo test` only exercises the root
`conductor` package and silently skips `conductor-core`, `conductor-daemon`,
and `conductor-capture`.

Other `just` recipes relevant to testing:

```bash
just test-compositions   # ADR-045 feature-composition matrix (mirrors CI):
                          #   cargo test -p conductor-daemon --no-default-features
                          #   cargo test -p conductor-daemon
                          #   cargo test -p conductor-daemon --features llm-executor
                          #   cargo test -p conductor-daemon --features mcp-write

just test-watch          # cargo watch -x "test --all-features" (requires cargo-watch)

just ci                  # fmt-check + spdx-check + lint + test, in that order
```

To run one suite or crate directly:

```bash
cargo test -p conductor-core
cargo test -p conductor-daemon
cargo test --test e2e_tests           # a single integration-test binary
cargo test some_test_name -- --nocapture
```

## Test Organization

Tests live in three places:

- **`tests/`** (workspace root, exercised through the `conductor` package) —
  the MIDI simulator itself (`midi_simulator.rs`), pipeline integration
  tests (`integration_tests.rs`, `event_processing_tests.rs`,
  `action_tests.rs`, `action_orchestration_tests.rs`,
  `actions_unit_tests.rs`, `mappings_unit_tests.rs`,
  `chord_mapping_test.rs`, `e2e_tests.rs`), config-compatibility tests
  (`config_compatibility_test.rs`, `backward_compatibility_test.rs`),
  plugin integration (`plugin_integration_test.rs`), and a handful of
  documentation/repo-policy guards (`readme_examples_test.rs` — keeps
  README/docs config examples valid against the current schema;
  `security_supported_versions_test.rs` — keeps `SECURITY.md`'s table in
  sync; `release_secrets_policy.rs`, `ci_runner_policy.rs` — workflow
  security policy checks).
- **`conductor-core/tests/`** — engine-level integration tests: config
  schema and validation (`config_validation_test.rs`, `canonical_serialise.rs`,
  `config_revision.rs`, `endpoint_schema_test.rs`, `endpoint_validation_test.rs`,
  `route_config_test.rs`, `route_validation_test.rs`, context-switch lowering,
  legacy-form rejection tests), device identity and multi-device routing
  (`identity_test.rs`, `resolver_test.rs`, `multi_device_config_test.rs`,
  `multi_device_rule_routing_test.rs`, `e2e_multidevice_test.rs`), event
  processing and trigger matching (`event_processor_chord_test.rs`,
  `trigger_matching_test.rs`, `gamepad_input_test.rs`, `input_event_tests.rs`,
  `malformed_midi_test.rs`, `rule_set_test.rs`), security/capability
  (`capability_declared_actions_test.rs`, `filesystem_capability_test.rs`,
  `resource_limiting_test.rs`, `keychain_test.rs`, `plugin_signing_test.rs`),
  and plugin/WASM integration (`wasm_plugin_integration_test.rs` and the
  `obs_wasm_test.rs` / `spotify_wasm_test.rs` example-plugin tests).
- **`conductor-daemon/tests/`** — daemon-level integration tests: IPC and
  lifecycle (`ipc_security_test.rs`, `ipc_bounded_read_test.rs`,
  `singleton_lock.rs`, `startup_cleanup_test.rs`), the ADR-045 tier boundary
  (`adr045_mcp_toggle_test.rs`, `adr045_tool_tier_split_test.rs`,
  `mcp_registry_tier_ceiling_test.rs`), routing and hot-plug
  (`route_engine_test.rs`, `connector_registry_test.rs`, `hot_plug_test.rs`,
  `multi_device_test.rs`, `midi_integration_test.rs`), the network-listener
  edge (`acl_filter_test.rs`, `rate_limit_edge_test.rs`, `audit_edge_test.rs`,
  `network_approvals_test.rs`), and `conductor-sign`/`conductorctl` CLI
  behavior (`conductor_sign_trust_verify_test.rs`,
  `conductorctl_permissions_test.rs`, `cli_command_name_test.rs`, and others).

This list is illustrative, not exhaustive — run `ls tests/`,
`ls conductor-core/tests/`, and `ls conductor-daemon/tests/` for the current
full set.

## MIDI Device Simulator

`tests/midi_simulator.rs` simulates MIDI input without physical hardware —
Note On/Off, Control Change, Aftertouch, Pitch Bend, Program Change, plus
higher-level gestures (`Gesture::SimpleTap`, `LongPress`, `DoubleTap`,
`Chord`, `EncoderTurn`, `VelocityRamp`) and a `ScenarioBuilder` for
composing event sequences.

```rust
use midi_simulator::{MidiSimulator, Gesture};

#[test]
fn test_my_feature() {
    let sim = MidiSimulator::new(0); // MIDI channel 0

    sim.note_on(60, 100);
    sim.note_off(60);

    let events = sim.get_events(); // drains the queue
    assert_eq!(events.len(), 2);
    assert_eq!(events[0], vec![0x90, 60, 100]); // Note On
    assert_eq!(events[1], vec![0x80, 60, 0x40]); // Note Off
}
```

Gestures:

```rust
sim.perform_gesture(Gesture::LongPress { note: 60, velocity: 80, hold_ms: 2500 });
sim.perform_gesture(Gesture::DoubleTap { note: 60, velocity: 80, tap_duration_ms: 50, gap_ms: 200 });
sim.perform_gesture(Gesture::Chord { notes: vec![60, 64, 67], velocity: 80, stagger_ms: 10, hold_ms: 500 });
sim.perform_gesture(Gesture::EncoderTurn { cc: 1, direction: EncoderDirection::Clockwise, steps: 5, step_delay_ms: 0 });
sim.perform_gesture(Gesture::VelocityRamp { note: 60, min_velocity: 20, max_velocity: 120, steps: 5 });
```

`ScenarioBuilder`:

```rust
use midi_simulator::ScenarioBuilder;

let scenario = ScenarioBuilder::new()
    .note_on(60, 100)
    .wait(100)
    .control_change(1, 64)
    .wait(100)
    .aftertouch(80)
    .note_off(60)
    .build();

sim.execute_sequence(scenario);
```

Other useful methods: `sim.peek_last_event()` (inspect without draining the
queue), `sim.clear_events()`, `sim.set_debug(true)` (prints every simulated
message).

An interactive CLI wraps the same simulator for manual exploration:

```bash
cargo run -p conductor-daemon --bin midi_simulator
```

It supports `note`, `velocity`, `long`, `double`, `chord`, `encoder`,
`aftertouch`, `pitch`, `cc`, `events`, `clear`, `demo`, and `scenario
<name>` (`velocity`, `timing`, `doubletap`, `chord`, `encoder`, `complex`)
— run `help` inside the tool for the full list.

## Fixtures

`tests/fixtures/v0.1.0/baseline.toml` is a legacy-syntax config (bare
`[device]` block, no `[[endpoints]]`) consumed by
`tests/config_compatibility_test.rs` to guard that old-format configs keep
loading. When adding a new legacy-format regression case, add a fixture
under `tests/fixtures/` rather than inlining large TOML strings in test
source.

## Writing Tests

Conventions used across the existing suites:

- **Prefer the MIDI simulator** over hand-built raw byte sequences for MIDI
  event generation — it centralizes the message encoding.
- **Clear the event queue between assertions** — `get_events()` drains it,
  so a second call on the same simulator returns nothing.
- **Give timing-sensitive assertions tolerance.** CI schedulers are less
  precise than a dev machine; the existing timing tests generally allow
  tens of milliseconds of slack around thresholds like long-press and
  double-tap windows rather than asserting exact durations.
- **Test velocity boundaries explicitly** (0, 40, 41, 80, 81, 127) when a
  change touches velocity-level classification.
- **Route action execution through the production `ActionExecutor`** where
  possible (see `tests/action_orchestration_tests.rs`) rather than
  reimplementing dispatch logic in the test.

Minimal template:

```rust
#[test]
fn test_feature_name() {
    let sim = MidiSimulator::new(0);

    // Setup
    sim.note_on(60, 80);

    // Execute / observe
    let events = sim.get_events();

    // Verify
    assert_eq!(events.len(), 1);
    assert_eq!(events[0][0] & 0xF0, 0x90); // Note On
}
```

## Gamepad / HID Testing

Gamepad support is tested at two levels:

- **Unit/conversion tests** — `conductor-core/tests/gamepad_input_test.rs`
  exercises HID → `InputEvent` conversion and the non-overlapping ID-range
  invariant (MIDI notes/CCs use 0-127; gamepad buttons/axes use 128+). The
  exact button/axis ID assignments and the dead-zone/threshold constants
  live in `conductor-core/src/gamepad_events.rs`; see
  [`input-manager-architecture.md`](input-manager-architecture.md#id-range-separation)
  for the documented ID table rather than duplicating it here.
- **InputManager-level tests** — `conductor-daemon/src/input_manager/`
  and `conductor-daemon/tests/multi_device_test.rs` /
  `hot_plug_test.rs` cover `InputMode` selection (MidiOnly / GamepadOnly /
  Both), hybrid event-stream merging, and hot-plug rescan behavior. Run:

```bash
cargo test -p conductor-daemon input_manager
cargo test -p conductor-daemon --test multi_device_test
cargo test -p conductor-daemon --test hot_plug_test
```

Physical-hardware testing (real gamepads, cross-platform permission
prompts, dead-zone feel) is manual and not automated in this repo — there
is no CI job or bundled tooling for virtual/emulated gamepad input. If you
need to validate against real hardware, connect a controller and run the
daemon directly:

```bash
cargo run -p conductor-daemon --bin conductor --release
```

and watch the daemon's rotating log (`conductor_core::logging::log_dir()`,
typically under `~/.conductor` on macOS/Linux) — or set
`RUST_LOG=conductor_daemon=debug` for more detail.

## Benchmarks

`conductor-core` and `conductor-daemon` each carry Criterion-style
`harness = false` benchmarks under `benches/` (e.g.
`conductor-core/benches/event_processing.rs`,
`conductor-daemon/benches/unified_routing_bench.rs`). The routing
benchmark's output is gated by `scripts/check_bench_thresholds.py`, which
parses a machine-readable metrics block
(`median_us` / `p99_us` / `stddev_us`) the benchmark prints and fails if
the passthrough latency regresses past its threshold — this is the
ADR-036 unified-routing-passthrough latency gate.

Code coverage tooling (`cargo-llvm-cov`) is configured via `.llvm-cov.toml`
(a 0.35% baseline floor) for ad hoc local use —
`cargo llvm-cov --workspace --all-features` — but coverage is not
currently wired into `.github/workflows/ci.yml`.

## Continuous Integration

`.github/workflows/ci.yml` runs on every push to `main` and every pull
request:

- **`fmt`** — `cargo fmt --all --check` plus `scripts/check-spdx.sh`
  (every tracked `.rs` file must carry the SPDX license header).
- **`clippy`** — `cargo clippy --workspace --all-targets -- -D warnings`.
- **`test`** — `cargo build --workspace` then `cargo test --workspace`, on
  both `ubuntu-latest` and `macos-latest`.
- **`compositions`** — the ADR-045 feature matrix, package-scoped
  (`-p conductor-daemon`, never `--workspace`, so feature unification can't
  leak paid features into the OSS build): `--no-default-features`, the OSS
  default, `--features llm-executor`, and `--features mcp-write`.

A `gate` job at the front short-circuits everything if `Cargo.toml` is
missing (a scaffold-repo safeguard); it's a no-op once the workspace
exists, which it does.

Related scripts invoked by CI or `just`: `scripts/check-spdx.sh` (license
headers), `scripts/check-oss-binary.sh` (ADR-045 D3 — asserts the built OSS
daemon binary contains no SQLite symbols, no gated MCP tool-name strings,
and no telemetry SDK markers), `scripts/gen-third-party-licenses.sh`
(regenerates `THIRD_PARTY_LICENSES.md`), and `scripts/dev-codesign.sh`
(ad-hoc codesigns dev binaries on macOS so Input Monitoring/TCC grants
survive rebuilds).

## Related Documentation

- [InputManager Architecture](input-manager-architecture.md)
- [Architecture Deep Dive](architecture.md)
- [Action Types Reference](../reference/action-types.md)
- [Contributing Guide](../../CONTRIBUTING.md)
