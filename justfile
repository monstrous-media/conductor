# Conductor Development Tasks
# Install just: cargo install just
# Run: just <command>

# Default recipe to display help information
default:
    @just --list

# Run all tests
# --workspace is required: without it, cargo only tests the root `conductor`
# package and silently skips conductor-core / -daemon / -capture / -knowledge,
# so `just ci` could pass while a member's tests fail (#1441). --exclude
# conductor-gui mirrors CI (ci.yml): the Tauri GUI's generate_context! needs a
# built frontend and is covered by the separate macOS tauri-build lane.
test:
    cargo test --workspace --exclude conductor-gui --all-features

# Run tests with nextest (improved output)
test-nextest:
    ./scripts/test-nextest.sh

# Run tests in watch mode
test-watch:
    cargo watch -x "test --all-features"

# Generate code coverage report (terminal summary)
coverage:
    ./scripts/coverage.sh

# Generate HTML coverage report
coverage-html:
    ./scripts/coverage.sh --html

# Generate HTML coverage report and open in browser
coverage-open:
    ./scripts/coverage.sh --open

# Generate lcov.info for CI
coverage-lcov:
    ./scripts/coverage.sh --lcov

# Run linter (clippy)
# Mirrors ci.yml's clippy invocation exactly (#1441): --workspace so members
# are linted (not just the root package); --exclude conductor-gui (Tauri uses
# macOS-only APIs, linted in the macOS tauri-lint lane); -A unexpected_cfgs to
# silence objc 0.2.7's cfg(cargo-clippy) under Rust 1.95+.
lint:
    cargo clippy --workspace --exclude conductor-gui --all-targets --all-features -- -D warnings -A unexpected_cfgs

# Format code
fmt:
    cargo fmt --all

# Check formatting without modifying files
fmt-check:
    cargo fmt --all -- --check

# Build in debug mode
build:
    cargo build

# Build in release mode
build-release:
    cargo build --release

# Clean build artifacts
clean:
    cargo clean

# Run the main application (requires port argument)
run PORT:
    cargo run --release {{PORT}}

# Run with LED lighting scheme
run-led PORT SCHEME:
    cargo run --release {{PORT}} --led {{SCHEME}}

# Run MIDI diagnostic tool
diagnostic PORT:
    cargo run --bin midi_diagnostic {{PORT}}

# List available MIDI ports
ports:
    cargo run --bin test_midi

# Run all CI checks locally (lint, format, ADR currency, test, coverage)
ci: fmt-check lint adr-check test coverage
    @echo "All CI checks passed!"

# Regenerate the ADR currency index from frontmatter + deprecations registry + code
adr-index:
    python3 scripts/gen_adr_index.py

# ADR currency gate: frontmatter/supersession lint + spec staleness, and verify
# the generated index is not stale (regenerate-and-diff, like the license gate).
adr-check:
    python3 scripts/check_adr_currency.py
    python3 scripts/gen_adr_index.py
    @git diff --exit-code -- docs/adr-currency-index.md || (echo "::error:: docs/adr-currency-index.md is stale — run 'just adr-index' and commit" && exit 1)
    @echo "ADR currency OK"

# Install development dependencies
dev-setup:
    ./scripts/dev-setup.sh

# Run security audit
audit:
    cargo audit

# Update dependencies
update:
    cargo update

# Generate documentation
docs:
    cargo doc --all-features --no-deps

# Open documentation in browser
docs-open:
    cargo doc --all-features --no-deps --open
