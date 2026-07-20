# Conductor

**Turn any input controller — MIDI or game controller — into an advanced, context-aware
macro surface.** Velocity-sensitive triggers, long-press and double-tap detection, chords,
mode-based mappings, RGB LED feedback, and a sandboxed WASM plugin system — driven by a
fast, minimal Rust daemon.

> **🚧 Repository scaffold.** The Conductor engine is migrating here from its original
> development repository. The code, releases, and full documentation will land shortly.
> Watch this repo or check [getconductor.dev](https://getconductor.dev) for updates.

## What's coming to this repository

- **`conductor-core`** — the pure mapping/routing engine library (event processing,
  trigger detection, velocity curves, plugin traits, device profiles)
- **`conductor-daemon`** — the background service and CLI: MIDI + game controller (HID)
  input, action execution (keystrokes, shell, MIDI out, OSC), config hot-reload,
  read-only MCP server for LLM-client inspection
- **`conductor-capture`** — input pattern capture tool
- **Plugin SDK** — WASM plugin template, examples, and Ed25519 signing tools
- **Device templates and examples**

Everything in this repository is and will remain **MIT-licensed**. Conductor follows an
open-core model: the engine, daemon, plugin system, and device-profile layer are free and
open forever. A commercial desktop GUI (**Conductor Studio / Pro**) with AI-assisted
configuration is built on top of the same published crates — see
[getconductor.app](https://getconductor.app).

## Links

- **Docs**: [getconductor.dev](https://getconductor.dev)
- **Contributing**: [CONTRIBUTING.md](CONTRIBUTING.md) (DCO sign-off required)
- **Security policy**: [SECURITY.md](SECURITY.md)
- **Governance**: [GOVERNANCE.md](GOVERNANCE.md) · [Code of Conduct](CODE_OF_CONDUCT.md)
- **Trademarks**: [TRADEMARKS.md](TRADEMARKS.md)

## License

MIT — see [LICENSE](LICENSE).
