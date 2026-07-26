# Conductor Roadmap

This document describes where Conductor is, where it's going, and how the open-source
and commercial tiers fit together. Detailed history lives in [CHANGELOG.md](CHANGELOG.md);
strategy detail in [docs/conductor-go-to-market-strategy.md](docs/conductor-go-to-market-strategy.md);
architectural decisions in [docs/adrs/](docs/adrs/).

## Vision

**Transform any input controller — MIDI or gamepad — into an advanced, context-aware
macro surface with professional-grade feedback, timing-based triggers, and natural-language
configuration.**

- Musicians control DAWs and effects with velocity-sensitive, multi-layer mappings
- Developers streamline workflows with mode-based hotkey systems
- Streamers and creators drive OBS, scenes, and audio routing from physical controls
- Power users replace dedicated macro pads with hardware they already own

## Where we are: v5.7.1-alpha (pre-launch)

Conductor is a mature multi-protocol input mapping system. The major architectural
phases — engine extraction, daemon infrastructure, Tauri GUI, plugin system (native +
WASM + Ed25519 signing), Game Controller (HID) support, LLM integration (MCP server,
plan/apply, 5 chat providers), and multi-device architecture (ADR-009, all 6 phases) —
are complete. See [CLAUDE.md](CLAUDE.md) and [CHANGELOG.md](CHANGELOG.md) for the full
feature inventory and version history.

**Current focus**: pre-launch hardening (security-first; see ADR-027/ADR-042), the
open-core tier boundary (ADR-045), and the closed-alpha program.

## Product structure (three tiers)

| Tier | Price | What it is |
|------|-------|------------|
| **Conductor Open Source** | Free (MIT) | CLI daemon: full mapping/routing engine, all triggers, plugins, config hot-reload, read-only MCP (inspect/diagnose from any LLM client) |
| **Conductor Studio** | $49 perpetual + optional $29/yr updates | Visual GUI, AI configuration via integrated chat (BYOK, 5 providers, plan/approval workflow), MIDI Learn, profiles |
| **Conductor Pro** | $79 perpetual | Studio + commercial-use license, priority support, early access, unlimited device configs |

The daemon and core stay MIT forever; the plugin system and device-profile library are
permanently free and open — they are the community ecosystem. The free/paid boundary is
build composition along MCP risk tiers (ADR-045): the OSS daemon cannot mutate config via
MCP; AI-applied configuration is the paid differentiator. Contributions are accepted
under DCO (no CLA).

## Launch phases (2026)

| Phase | Timing | Goal | Status |
|-------|--------|------|--------|
| **Closed Alpha** | Apr–May 2026 | 20–50 hand-picked macOS testers; device compatibility data | In progress |
| **Open Beta** | Jun–Jul 2026 | 200–500 users; beta pricing ($19/$29); Homebrew + crates.io; Show HN | Next |
| **Public Launch** | Aug–Sep 2026 | Direct download, launch pricing, Product Hunt, press | Planned |
| **Growth** | Q4 2026+ | Windows release, plugin/profile directory, enterprise tier, Linux, MIDI 2.0 | Planned |

Gating work for beta (tracked as issues/ADRs):

- **ADR-045**: feature-gated daemon composition — OSS artifact = mapping/routing +
  read-only MCP, no SQLite; the MCP socket is read-only in every official artifact, and
  config mutation flows only through the licensed GUI (IPC)
- **ADR-046**: repository decomposition — public `monstrous-media/conductor` (engine,
  MIT, canonical) + private `monstrous-media/conductor-studio` (GUI, knowledge,
  licensing); target repos are provisioned and configured first, code migration follows
  as a separate step
- **LicenseState**: license-key validation in the GUI (Lemon Squeezy), first-run flow
- **GUI v2**: three-zone workspace rebuild (see `docs/gui-v2/`)
- Signed/notarized release artifacts for both daemon and GUI (release.yml)

## Engineering priorities

### P0 — beta blockers

- ADR-045 tier-boundary implementation (cargo features, audit-sink seam, CI feature matrix)
- LicenseState + payment integration
- Onboarding polish: first-run wizard, device detection, template selection
- Crash reporting / opt-in telemetry

### P1 — launch quality

- macOS Bluetooth gamepad input backend (GCController bridge, #2229)
- GUI v2 remaining phases
- Device-profile library growth + contribution tooling (template, submission CI)
- Documentation site (getconductor.dev)

### P2 — post-launch

- Windows support, then Linux
- Free curated plugin & device-profile directory (rev-share marketplace deferred — GTM §5.4)
- Enterprise tier (multi-seat, broadcast/production)
- MIDI 2.0

## Community goals

- An open plugin/integration layer with low-friction contribution is the ecosystem
  strategy (template repo, docs, CI-validated community submissions)
- GitHub Discussions for long-form technical conversation; Discord for community
  (structure in GTM §6)
- Build in public: release notes every release, monthly roadmap updates
- Short term: first external contributors, community device profiles, 1,000 GitHub stars
  within 6 months of launch

## Release cadence

- Minor releases every 4–6 weeks; patches as needed
- Every release: signed artifacts, CHANGELOG entry, release notes
- v5.x-alpha line until alpha exit criteria are met (GTM §4.3)

---

**Roadmap version**: 2.0
**Last updated**: 2026-06-10
**Supersedes**: v1.0 (2025-11-11), which described the pre-workspace v0.1.0 monolith
**Next review**: at beta open (July 2026)
