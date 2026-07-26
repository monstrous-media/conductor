#!/usr/bin/env bash
# ADR-029 Phase 5 (D5) — dev codesign helper.
#
# Sign target/debug/ binaries with a stable ad-hoc identity so macOS
# TCC remembers Input Monitoring / Accessibility grants across rebuilds.
#
# `cargo run` produces unsigned binaries that change identity on every
# rebuild, so the OS forgets any prior TCC grant. Ad-hoc signatures
# don't satisfy notarization or Gatekeeper for distribution, but they
# DO give the binary a stable identity within a single machine, which
# is what TCC needs.
#
# Usage:
#   ./scripts/dev-codesign.sh                  # sign whatever exists
#   make dev-build                             # build then sign
#
# Exits 0 on:
#   - macOS: each binary signed (or absent → SKIPPED line)
#   - non-macOS: short "skipping" message
#
# Exits non-zero on:
#   - codesign command itself failing (rare; usually means missing
#     macOS dev tools or a malformed entitlements file)

set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "dev-codesign.sh: not macOS, skipping"
  exit 0
fi

# Each entry: "binary_path:identifier:entitlements_path_or_empty"
# Identifier suffix `.dev` distinguishes ad-hoc dev signing from the
# release Developer ID identifiers used in CI (`media.monstrous.conductor`,
# `media.monstrous.conductor.daemon`). TCC keys grants on identifier, so a
# dev grant won't accidentally cover a production binary or vice versa.
BINARIES=(
  "target/debug/conductor-gui:media.monstrous.conductor.dev:conductor-gui/src-tauri/entitlements.plist"
  "target/debug/conductor:media.monstrous.conductor.daemon.dev:"
)

for entry in "${BINARIES[@]}"; do
  IFS=':' read -r path identity entitlements <<< "$entry"
  if [[ ! -f "$path" ]]; then
    echo "SKIPPED $path (not built)"
    continue
  fi
  args=("--force" "--sign" "-" "--identifier" "$identity")
  if [[ -n "$entitlements" && -f "$entitlements" ]]; then
    args+=("--entitlements" "$entitlements")
  fi
  if codesign "${args[@]}" "$path" 2>/dev/null; then
    echo "OK      $path  ($identity)"
  else
    # Re-run without 2>/dev/null so the error reaches the user; preserve
    # the non-zero exit so CI / make catches it.
    echo "FAILED  $path"
    codesign "${args[@]}" "$path" >&2
    exit 1
  fi
done
