# ADR-039 Protocol Lifecycle Coverage

<!-- GENERATED from conductor-daemon/tests/protocol_lifecycle_test.rs — do not hand-edit. Run `LIFECYCLE_REGEN=1 cargo test -p conductor-daemon --test protocol_lifecycle_test` to regenerate. -->

Source of truth: the `lifecycle()` matrix (ADR-039 §4.2). Each `Done`/`baseline`
cell is compile-proven against the Rust symbol listed below; a removed or
renamed implementation fails the build.

| Stage | MIDI | HID | OSC | Art-Net |
|---|---|---|---|---|
| 1. Input Listener | Done | Done | Done | 039-C |
| 2. Typed Triggers | Done | Done | Done | 039-C |
| 3. Catch-All (route) | Done | Done | Done | 039-C |
| 4. Forward Action | Done | Done | Done | 039-C |
| 5. Output Connector | Done | n/a (HID output dropped, ADR D7) | Done | Done |
| 6. Cross-Protocol Transform | baseline | Done | Done | 039-C |

## Backing symbols (compile-proven)

- MIDI · 1. Input Listener → `conductor_daemon::midi_device::MidiInputSource`
- MIDI · 2. Typed Triggers → `conductor_core::config::types::Trigger`
- MIDI · 3. Catch-All (route) → `conductor_daemon::route_engine::RouteEngine`
- MIDI · 4. Forward Action → `conductor_core::actions::Action`
- MIDI · 5. Output Connector → `conductor_core::midi_output::MidiOutputManager`
- MIDI · 6. Cross-Protocol Transform → `conductor_core::transform::MidiTransform`
- HID · 1. Input Listener → `conductor_daemon::gamepad_device::HidInputSource`
- HID · 2. Typed Triggers → `conductor_core::config::types::Trigger`
- HID · 3. Catch-All (route) → `conductor_daemon::route_engine::RouteEngine`
- HID · 4. Forward Action → `conductor_core::actions::Action`
- HID · 6. Cross-Protocol Transform → `conductor_core::config::types::SignalTransform`
- OSC · 1. Input Listener → `conductor_daemon::osc_parser::ParsedDatagram`
- OSC · 2. Typed Triggers → `conductor_core::osc_pattern::OscPattern`
- OSC · 3. Catch-All (route) → `conductor_daemon::route_engine::RouteEngine`
- OSC · 4. Forward Action → `conductor_core::actions::Action`
- OSC · 5. Output Connector → `conductor_daemon::connector_registry::EndpointRegistry`
- OSC · 6. Cross-Protocol Transform → `conductor_core::config::types::SignalTransform`
- Art-Net · 5. Output Connector → `conductor_daemon::connector_registry::EndpointRegistry`
