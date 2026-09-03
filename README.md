# Conductor

**Turn any input controller — MIDI or game controller — into an advanced, context-aware
macro surface.** Velocity-sensitive triggers, long-press and double-tap detection, chords,
mode-based mappings, signal routing, RGB LED feedback, and a sandboxed WASM plugin
system — driven by a fast, minimal Rust daemon.

## What's in this repository

- **`conductor-core`** — the pure mapping/routing engine library: config compilation and
  validation, event processing, trigger detection, velocity curves, device intelligence,
  plugin runtime (native and WASM), feedback, OSC
- **`conductor-daemon`** — the background service and CLI (`conductor`, `conductorctl`):
  MIDI + game controller (HID) input, action execution (keystrokes, shell, MIDI out,
  OSC), config hot-reload, and a read-only MCP server for LLM-client inspection
- **`conductor-capture`** — privacy-aware input pattern capture tool
- **Plugin SDK** — WASM plugin template and minimal example (`plugins/`), Ed25519
  signing tools, and a native-plugin example (`examples/http-plugin`)
- **Configuration examples** — `config.toml`, `config_enhanced.toml`, `config/examples/`

## Quick start

```sh
# Build everything (Linux needs libasound2-dev and libudev-dev)
cargo build --workspace

# Run the test suite
cargo test --workspace

# Start the daemon with the example config
cargo run -p conductor-daemon --bin conductor
```

Development tasks are wrapped in a [justfile](justfile) — `just ci` runs the same
format/lint/test gates as CI. See [docs/](docs/README.md) for the reference
documentation (config schema, trigger and action types, CLI commands, MCP tools) and
development guides.

## Open core

Everything in this repository is and will remain **MIT-licensed**. Conductor follows an
open-core model: the engine, daemon, plugin system, and device-profile layer are free and
open forever. A commercial desktop GUI (**Conductor Studio / Pro**) with AI-assisted
configuration is built on top of the same published crates — see
[getconductor.app](https://getconductor.app).

The daemon's cargo features implement the tier boundary: the default build ships the
full engine with a read-only MCP server; write-tier MCP and the LLM executor are
opt-in features (see `conductor-daemon/Cargo.toml`).

## Links

- **Docs**: [docs/](docs/README.md) · [getconductor.dev](https://getconductor.dev)
- **Contributing**: [CONTRIBUTING.md](CONTRIBUTING.md) (DCO sign-off required)
- **Security policy**: [SECURITY.md](SECURITY.md)
- **Governance**: [GOVERNANCE.md](GOVERNANCE.md) · [Code of Conduct](CODE_OF_CONDUCT.md)
- **Trademarks**: [TRADEMARKS.md](TRADEMARKS.md)

## License

MIT — see [LICENSE](LICENSE).
