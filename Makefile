# Conductor — workspace Makefile.
#
# Convenience targets for developers. Cargo remains the canonical build
# tool; these targets wrap common workflows (codesign for macOS dev
# builds, etc.) so contributors don't have to remember the flags.

.PHONY: dev-build dev-codesign dev-gui test test-hid help

help:
	@echo "Conductor developer targets:"
	@echo "  make dev-build      — cargo build --workspace + macOS dev codesign"
	@echo "  make dev-codesign   — re-sign target/debug/ binaries with stable ad-hoc identity (macOS only)"
	@echo "  make dev-gui [WORKTREE=name]"
	@echo "                      — build UI assets + dev-build, then launch the Tauri dev server."
	@echo "                        WORKTREE=<name> finds a git worktree by name and runs there;"
	@echo "                        defaults to the current worktree."
	@echo "  make test           — cargo test --workspace"
	@echo "  make test-hid       — build, then codesign, then cargo test --workspace"
	@echo "                        (gives HID-touching tests a stable macOS TCC identity)"

# Build the whole workspace and ad-hoc-codesign the dev binaries on
# macOS so Input Monitoring grants survive across rebuilds. See
# ADR-029 §D5 for rationale and `scripts/dev-codesign.sh` for the
# detail.
dev-build:
	cargo build --workspace
	./scripts/dev-codesign.sh

# Re-sign without rebuilding (useful after a `cargo run` that bypassed
# the wrapper).
dev-codesign:
	./scripts/dev-codesign.sh

# One-shot GUI dev loop. Builds the Svelte UI, then build+codesigns the
# workspace binaries (dev-build), then starts the Tauri dev server —
# replacing the manual cd-dance between the worktree root and
# conductor-gui. UI assets are built first so the Tauri build script
# finds the dist dir.
#
# Pass WORKTREE=<name> to run the whole flow inside a different git
# worktree (matched by name against `git worktree list`), e.g.
#   make dev-gui WORKTREE=ticket-master
# With no WORKTREE it runs in the current worktree.
dev-gui:
	@dir="$(CURDIR)"; \
	if [ -n "$(WORKTREE)" ]; then \
	  dir=$$(git worktree list --porcelain | sed -n 's/^worktree //p' | grep -F "$(WORKTREE)" | head -n1); \
	  if [ -z "$$dir" ]; then \
	    echo "dev-gui: no worktree matching '$(WORKTREE)'. Available:"; \
	    git worktree list; \
	    exit 1; \
	  fi; \
	fi; \
	echo "dev-gui: building in $$dir"; \
	cd "$$dir/conductor-gui/ui" && npm install && npm run build && \
	$(MAKE) -C "$$dir" dev-build && \
	cd "$$dir/conductor-gui" && npm --prefix ui exec tauri -- dev

test:
	cargo test --workspace

# Tests that may need real HID access (gilrs / IOHIDManager). On macOS
# the binaries must hold the Input Monitoring TCC grant — `dev-codesign`
# gives them a stable identity so the grant persists across rebuilds.
#
# #2149: order matters. The previous `test-hid: dev-codesign` made signing a
# PREREQUISITE, so it ran first and then `cargo test` rebuilt the binaries,
# overwriting the ad-hoc signature (and invalidating the TCC grant) before any
# HID test ran. Compile everything first (`--no-run` builds the bins AND the
# test harnesses), THEN codesign, THEN run — the run finds everything up to
# date so it can't rebuild over the signed binaries. `--test-threads=1` keeps
# codesign from racing with itself if a test re-signs binaries.
test-hid:
	cargo test --workspace --no-run
	./scripts/dev-codesign.sh
	cargo test --workspace -- --test-threads=1
