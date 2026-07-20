# Contributing to Conductor

Thank you for your interest in contributing to Conductor! We welcome contributions of all
kinds — from bug reports and documentation improvements to new features and hardware
support.

## Table of Contents

- [Code of Conduct](#code-of-conduct)
- [Developer Certificate of Origin (DCO)](#developer-certificate-of-origin-dco)
- [Ways to Contribute](#ways-to-contribute)
- [Development Setup](#development-setup)
- [Coding Standards](#coding-standards)
- [Pull Request Process](#pull-request-process)
- [Testing Guidelines](#testing-guidelines)
- [Communication](#communication)

## Code of Conduct

This project adheres to the [Contributor Covenant Code of Conduct](CODE_OF_CONDUCT.md).
By participating, you are expected to uphold this code. Please report unacceptable
behavior to conduct@amiable.dev.

## Developer Certificate of Origin (DCO)

All contributions must be signed off under the
[Developer Certificate of Origin v1.1](https://developercertificate.org/). This certifies
that you wrote the contribution (or have the right to submit it) under the project's MIT
license. No CLA is required.

Add a sign-off to every commit:

```bash
git commit -s -m "feat: add support for X"
```

which appends a line like:

```
Signed-off-by: Your Name <your.email@example.com>
```

The DCO check runs on every pull request; unsigned commits will fail CI. If you forget,
amend and force-push: `git commit --amend -s && git push -f`.

## Ways to Contribute

### 🐛 Bug Reports

Found a bug? Please open a [GitHub issue](https://github.com/monstrous-media/conductor/issues/new/choose) with:
- Clear description of the issue
- Steps to reproduce
- Expected vs actual behavior
- System information (OS, Rust version, MIDI device or game controller)
- Relevant log output (run with `DEBUG=1` for verbose logs)

### 💡 Feature Requests

Have an idea? We'd love to hear it! Open a
[GitHub Discussion](https://github.com/monstrous-media/conductor/discussions) with:
- Clear description of the proposed feature
- Use cases and benefits
- Potential implementation approaches (optional)

### 📖 Documentation

Documentation improvements are always welcome: fix typos, add examples, improve API docs,
write tutorials or guides.

### 🔧 Code Contributions

Ready to write code? Check out
[Good First Issues](https://github.com/monstrous-media/conductor/labels/good-first-issue)
and [Help Wanted](https://github.com/monstrous-media/conductor/labels/help-wanted).

### 🎹 Device Support

Help us support more controllers:
- Test Conductor with your MIDI device or game controller
- Create device profiles and config templates
- Document device-specific quirks
- Implement LED feedback for new devices

### 🔌 WASM Plugins

Extend Conductor with sandboxed WASM plugins — media control, system utilities, DAW and
streaming integrations. See the plugin template in `plugins/wasm-template/` and the
[Plugin Development Guide](https://getconductor.dev/docs/plugins).

**Plugin requirements**: solve a real problem, request only necessary capabilities,
include tests and documentation, use an MIT-compatible license.

## Development Setup

### Prerequisites

- **Rust** (install via [rustup](https://rustup.rs/)) — `rust-toolchain.toml` pins the
  exact version, honored automatically by rustup
- **macOS** 10.15+ (Linux/Windows support planned)
- **MIDI controller or gamepad** (optional for most development)
- **Git**

### Setup Steps

1. **Fork and clone**
   ```bash
   git clone https://github.com/YOUR_USERNAME/conductor.git
   cd conductor
   ```

2. **Build and test**
   ```bash
   cargo build --workspace
   cargo test --workspace
   ```

3. **Run the daemon**
   ```bash
   # List available MIDI ports
   cargo run -p conductor-daemon --bin conductor --release

   # Connect to a specific port, with debug logging
   DEBUG=1 cargo run -p conductor-daemon --bin conductor --release 2
   ```

4. **Create a feature branch**
   ```bash
   git checkout -b feature/your-feature-name
   ```

## Coding Standards

### Rust Style

- Follow the [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/)
- `cargo fmt` for formatting; `cargo clippy -- -D warnings` must pass
- Maximum line length 100; meaningful names; comments for complex logic

### Project Structure

Conductor is a **Cargo workspace**:

- `conductor-core/` — pure engine library (UI-independent): event processing, mapping
  engine, config, velocity curves, plugin traits, device profiles
- `conductor-daemon/` — background service: action executor, input managers (MIDI + HID),
  plugin manager, read-only MCP server, CLI/diagnostic binaries
- `conductor-capture/` — standalone input-pattern capture tool
- `plugins/` — WASM plugin SDK, template, and open-source example plugins

### Commit Messages

Follow [Conventional Commits](https://www.conventionalcommits.org/), plus the DCO
sign-off (`-s`):

```
feat: add support for MIDI Learn mode
fix: resolve double-tap detection timing issue
docs: update configuration examples
test: add integration tests for velocity detection
refactor: extract LED control to trait
chore: update dependencies
```

## Pull Request Process

### Before Submitting

1. `cargo test --workspace` passes
2. `cargo fmt --check` clean
3. `cargo clippy -- -D warnings` clean
4. Docs updated for user-facing changes (README, config reference, rustdoc)
5. All commits signed off (`git commit -s`)

### Submitting

- Clear title in conventional-commit format; description explaining what and why
- Link related issues; add screenshots/videos if applicable
- Check "Allow edits from maintainers"
- Respond to review feedback by pushing to the same branch

### PR Requirements

- ✅ CI green (tests, fmt, clippy, DCO)
- ✅ Conventional commit format, signed off
- ✅ Documentation updated
- ✅ No unrelated changes

## Testing Guidelines

- Unit tests alongside the code (`#[cfg(test)] mod tests`)
- Integration tests in each crate's `tests/` directory
- Cover edge cases and error conditions

```bash
cargo test --workspace              # all tests
cargo test test_velocity_detection  # one test
cargo test -- --nocapture           # with output
```

## Communication

- **[GitHub Discussions](https://github.com/monstrous-media/conductor/discussions)** —
  questions, ideas, show & tell, device profiles
- **[GitHub Issues](https://github.com/monstrous-media/conductor/issues)** — bugs,
  approved feature requests, documentation issues
- **Email** — security: security@amiable.dev · conduct: conduct@amiable.dev

## First-Time Contributors

New to open source? Start with a
[good-first-issue](https://github.com/monstrous-media/conductor/labels/good-first-issue),
read the docs, ask questions in Discussions, and start small — every contribution counts.

## License

By contributing to Conductor, you agree that your contributions will be licensed under
the [MIT License](LICENSE), as certified by your DCO sign-off.

---

Thank you for contributing to Conductor! 🎹🎮
