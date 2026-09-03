# Conductor — workspace Makefile.
#
# Convenience targets for developers. Cargo remains the canonical build
# tool; these targets wrap common workflows (codesign for macOS dev
# builds, etc.) so contributors don't have to remember the flags.
# The justfile carries the fuller task set; this file keeps the
# make-flavored entry points working.

.PHONY: dev-build dev-codesign test test-hid help

help:
	@echo "Conductor developer targets:"
	@echo "  make dev-build      — cargo build --workspace + macOS dev codesign"
	@echo "  make dev-codesign   — re-sign target/debug/ binaries with stable ad-hoc identity (macOS only)"
	@echo "  make test           — cargo test --workspace"
	@echo "  make test-hid       — build, then codesign, then cargo test --workspace"
	@echo "                        (gives HID-touching tests a stable macOS TCC identity)"

# Build the whole workspace and ad-hoc-codesign the dev binaries on
# macOS so Input Monitoring grants survive across rebuilds. See
# `scripts/dev-codesign.sh` for the detail.
dev-build:
	cargo build --workspace
	./scripts/dev-codesign.sh

# Re-sign without rebuilding (useful after a `cargo run` that bypassed
# the wrapper).
dev-codesign:
	./scripts/dev-codesign.sh

test:
	cargo test --workspace

# Tests that may need real HID access (gilrs / IOHIDManager). On macOS
# the binaries must hold the Input Monitoring TCC grant — `dev-codesign`
# gives them a stable identity so the grant persists across rebuilds.
#
# Order matters: signing used to be a PREREQUISITE, so it ran first and
# then `cargo test` rebuilt the binaries, overwriting the ad-hoc
# signature (and invalidating the TCC grant) before any HID test ran.
# Compile everything first (`--no-run` builds the bins AND the test
# harnesses), THEN codesign, THEN run — the run finds everything up to
# date so it can't rebuild over the signed binaries. `--test-threads=1`
# keeps codesign from racing with itself if a test re-signs binaries.
test-hid:
	cargo test --workspace --no-run
	./scripts/dev-codesign.sh
	cargo test --workspace -- --test-threads=1
