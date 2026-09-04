# Changelog

All notable changes to Conductor will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

This repository begins at 0.1.0: the open-source Conductor engine
(`conductor-core`, `conductor-daemon`, `conductor-capture`), rebaselined from
several years of private development. History prior to 0.1.0 lives in the
private predecessor repository and is intentionally not replayed here.

## [Unreleased]

### Added

- Initial open-source release of the Conductor engine:
  - `conductor-core` — the mapping/routing engine: config compilation and
    validation, event processing, device intelligence, plugin runtime
    (native and WASM), feedback, OSC.
  - `conductor-daemon` — the background service: IPC, MCP server
    (read-only tier by default), action execution, security gates,
    LaunchAgent packaging.
  - `conductor-capture` — privacy-aware input capture tooling.
