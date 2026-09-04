# Plugin Examples

This repository ships two WASM plugin crates and one native plugin example.
They're starting points, not finished products — there are no official
"Spotify" or "OBS" plugins in this repo. If you're building your own plugin,
copy one of these out and adapt it.

| Example | Type | Purpose |
|---------|------|---------|
| [`plugins/wasm-template/`](../../plugins/wasm-template/) | WASM | Full-featured starting point (metadata, actions, tests) |
| [`plugins/wasm-minimal/`](../../plugins/wasm-minimal/) | WASM | Smallest possible plugin — no allocator, no `std` |
| [`examples/http-plugin/`](../../examples/http-plugin/) | Native (`ActionPlugin`) | HTTP requests via the dylib plugin API |

See [Plugin Development](plugin-development.md) for the native `ActionPlugin`
API and [WASM Plugins](wasm-plugins.md) / [Plugin Security](plugin-security.md)
for the WASM ABI and signing model.

## `plugins/wasm-template/` — the starting point

`plugins/wasm-template/src/lib.rs` implements the full WASM plugin ABI:

- `init() -> u64` returns a pointer+length packed into a single `u64` (JSON
  metadata: `name`, `version`, `description`, `author`, optional `homepage`,
  `license`, `type`, `capabilities`).
- `execute(ptr: u32, len: u32) -> i32` reads a JSON `ActionRequest` from WASM
  memory and dispatches on `action`.
- `alloc`/`dealloc` let the host copy request bytes into the plugin's linear
  memory.

The template ships three example actions (`hello`, `greet`, `goodbye`) and
unit tests for metadata/request serialization and the action dispatch logic.
Its `plugin.toml` declares the manifest shape used for the marketplace/registry
(`[plugin]` name/version/description/author/license, `entry_point`,
`capabilities`, `[[actions]]` entries) — note this is a different shape from
the native `[plugin.capabilities]` table used by `examples/http-plugin/`.

Because it lives under `plugins/` but is excluded from the root Cargo
workspace, its `Cargo.toml` declares its own empty `[workspace]` table so
`cargo build` works standalone once you copy it out.

### Building it

The template's `Makefile` wraps the real build commands:

```bash
cd plugins/wasm-template

make install-deps    # rustup target add wasm32-wasip1; brew/apt install binaryen
make build           # debug build -> target/wasm32-wasip1/debug/*.wasm
make build-release   # release build (opt-level "z", LTO, stripped)
make optimize         # build-release, then wasm-opt -Oz
make inspect          # list WASM exports via wasm-objdump
make test             # cargo test (native, not WASM)
```

Install the resulting `.wasm` file the same way any WASM plugin is installed
— copy it into `~/.conductor/plugins/` (see
[Plugin Development](plugin-development.md) for the directory layout and the
`[plugins.<name>] granted_capabilities` config used to grant it capabilities).

## `plugins/wasm-minimal/` — the smallest working plugin

`plugins/wasm-minimal/src/lib.rs` is `#![no_std]` with no allocator:

- `init()` returns a pointer to a static, pre-serialized metadata byte string
  — no heap allocation, no `serde_json` at runtime.
- `execute()` is a stub that always returns `0` (success) without inspecting
  its arguments.
- `alloc()` returns a null pointer — it does not support dynamic allocation
  at all, since the host never needs to write request data. `dealloc()` is a
  no-op.
- A `#[panic_handler]` that loops forever, required for `no_std`.

Use this as a reference for the *minimum* ABI surface Conductor's WASM
runtime requires (`init`, `execute`, `alloc`, `dealloc`), stripped of every
convenience the template adds. Its `Cargo.toml` also declares its own empty
`[workspace]` and builds with `panic = "abort"` in the release profile.

## `examples/http-plugin/` — a native plugin

Unlike the two WASM crates above, `examples/http-plugin/` is a **native**
plugin: it implements `conductor_core::plugin::ActionPlugin` directly and
compiles to a `cdylib` (`.dylib`/`.so`/`.dll`), loaded via `libloading`
instead of `wasmtime`.

`HttpRequestPlugin` in `src/lib.rs`:

- Declares `capabilities() -> vec![Capability::Network]`.
- `execute()` builds a `reqwest::blocking::Client` with a configurable
  per-request timeout (`timeout_secs` param, default 30s, capped at 300s) so
  a stalled endpoint can't block the daemon's synchronous action-dispatch
  path indefinitely.
- Supports GET/POST/PUT/DELETE, custom headers, a JSON body, and a
  `{velocity}` placeholder substituted with the triggering MIDI velocity
  (works in nested objects and arrays).
- Its test suite includes a regression test that spins up a TCP listener
  which accepts a connection but never responds, confirming `execute()`
  returns an error near the configured timeout instead of hanging.

Build it like any native plugin:

```bash
cd examples/http-plugin
cargo build --release
```

## WASM integration test fixtures

`conductor-core/tests/spotify_wasm_test.rs` and `obs_wasm_test.rs` exist in
the test suite, but both are `#[ignore]`d: they exercise
`conductor_core::plugin::wasm_runtime::WasmPlugin` against pre-built `.wasm`
artifacts from plugin source trees (`plugins/wasm-spotify`,
`plugins/wasm-obs-control`) that are **not present in this repository**. They
document the intended shape of a real-world plugin (multi-action dispatch,
parameterized actions, metadata checks) but you cannot run them — or find
the plugins they test — without that external source. Treat them as design
references for `execute()` dispatch patterns, not as shipped examples.

## Signing a plugin

Both WASM and native plugins can be signed with `conductor-sign`; see
[Plugin Security](plugin-security.md) for the full workflow
(`generate-key`, `sign`, `verify`, `trust`).
