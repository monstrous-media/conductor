# Conductor development tasks
# Install just: cargo install just
# Run: just <command>

# Default recipe to display help information
default:
    @just --list

# Run all tests across the workspace.
# --workspace is required: without it, cargo only tests the root `conductor`
# package and silently skips conductor-core / -daemon / -capture.
test:
    cargo test --workspace --all-features

# ADR-045 open-core composition matrix, as CI runs it (ci.yml `compositions`):
# every feature composition of conductor-daemon must stay green.
test-compositions:
    cargo test -p conductor-daemon --no-default-features
    cargo test -p conductor-daemon
    cargo test -p conductor-daemon --features llm-executor
    cargo test -p conductor-daemon --features mcp-write

# Run tests in watch mode
test-watch:
    cargo watch -x "test --all-features"

# Run linter (clippy). Mirrors ci.yml's clippy invocation: --workspace so all
# members are linted, not just the root package.
lint:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

# Format code
fmt:
    cargo fmt --all

# Check formatting without modifying files
fmt-check:
    cargo fmt --all -- --check

# Build in debug mode
build:
    cargo build --workspace

# Build in release mode
build-release:
    cargo build --release --workspace

# Build the workspace and ad-hoc-codesign the dev binaries on macOS so
# Input Monitoring (TCC) grants survive across rebuilds.
dev-build:
    cargo build --workspace
    ./scripts/dev-codesign.sh

# Clean build artifacts
clean:
    cargo clean

# Check every tracked .rs file carries the license header
spdx-check:
    ./scripts/check-spdx.sh

# Run all CI checks locally (format, headers, lint, test)
ci: fmt-check spdx-check lint test
    @echo "All CI checks passed!"

# Run security audit
audit:
    cargo audit

# Regenerate THIRD_PARTY_LICENSES.md from the dependency graph
licenses:
    ./scripts/gen-third-party-licenses.sh

# Verify a built daemon binary contains only OSS-tier features
# e.g. just check-oss-binary target/release/conductor
check-oss-binary BIN="target/release/conductor":
    ./scripts/check-oss-binary.sh {{BIN}}

# Update dependencies
update:
    cargo update

# Generate documentation
docs:
    cargo doc --all-features --no-deps

# Open documentation in browser
docs-open:
    cargo doc --all-features --no-deps --open
