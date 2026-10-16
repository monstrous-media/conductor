# Conductor Roadmap

Where the open-source Conductor engine is and where it's going. Version
history lives in [CHANGELOG.md](CHANGELOG.md); the commercial products built
on this engine are described at [getconductor.app](https://getconductor.app).

## Vision

**Transform any input controller — MIDI or gamepad — into an advanced,
context-aware macro surface with professional-grade feedback, timing-based
triggers, and natural-language configuration.**

- Musicians control DAWs and effects with velocity-sensitive, multi-layer mappings
- Developers streamline workflows with mode-based hotkey systems
- Streamers and creators drive OBS, scenes, and audio routing from physical controls
- Power users replace dedicated macro pads with hardware they already own

## Where we are

Conductor is a mature multi-protocol input mapping system, pre-launch. The
engine in this repository is feature-complete for its core mission: the full
trigger/action mapping engine, mode system, signal routing between endpoints
(MIDI, OSC, Art-Net, virtual ports), game-controller (HID) support,
multi-device architecture, LED feedback, a plugin system (native + WASM with
Ed25519 signing), config hot-reload, and a read-only MCP server for
inspection and diagnosis from any LLM client.

Current focus is pre-launch hardening: security posture, release artifact
integrity, and documentation quality.

## Open source and commercial

The daemon and core engine in this repository are MIT **forever**, and the
plugin system and device-profile layer are permanently free and open — they
are the community ecosystem. Contributions are accepted under DCO (no CLA).

Commercial products (Conductor Studio — a visual GUI with AI-assisted
configuration — and Conductor Pro) are built on this engine in a separate,
closed-source repository. The boundary is build composition along MCP risk
tiers (ADR-045): every official open-source artifact exposes a **read-only**
MCP socket; configuration mutation flows only through the commercial GUI.
Source builds can opt into the full write tier with the `mcp-write` cargo
feature. See [getconductor.app](https://getconductor.app) for the product
side.

## Near term (engine scope)

- **Documentation growth** — expand [getconductor.dev](https://getconductor.dev)
  (guides, device notes) and keep the in-repo reference synchronized with the
  code it documents.
- **Publish the architecture decision records** — the code cites ADR numbers
  throughout; a sanitized public ADR corpus makes those references resolve
  ([#45](https://github.com/monstrous-media/conductor/issues/45)).
- **Device-profile contributions** — lower the friction for community device
  configs: a contribution template and CI validation for submitted profiles.
- **macOS Bluetooth gamepad backend** — bridge GCController so Bluetooth
  controllers work without a USB fallback.
- **Linux desktop surface** — the Linux daemon is currently headless by
  design; a system-tray backend is under consideration
  ([#41](https://github.com/monstrous-media/conductor/issues/41)).

## Later

- **Windows support**, then broader Linux desktop polish
- **MIDI 2.0**
- **Curated plugin & device-profile directory** — free, community-driven
- Enterprise/broadcast scenarios (multi-seat, production environments)

## Community goals

- An open plugin/integration layer with low-friction contribution: template
  repo, docs, CI-validated community submissions
- GitHub Discussions for long-form technical conversation
- Build in public: release notes every release, roadmap updates as things ship
- Short term: first external contributors and community device profiles

## Release cadence

- Minor releases every 4–6 weeks; patches as needed
- Every release: signed artifacts, CHANGELOG entry, release notes

---

**Last updated**: 2026-09-13
