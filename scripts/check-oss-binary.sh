#!/usr/bin/env bash
#
# check-oss-binary.sh (ADR-045 D3, #2494)
# ---------------------------------------
# Negative assertions on the FINAL OSS daemon binary — the verified half of
# the open-core tier boundary. Build flags are a convention; this script
# checks the artifact itself, so a workspace feature-unification accident or
# a mis-wired release job fails the pipeline instead of shipping the paid
# tier's capabilities in the free binary.
#
# Asserts the given binary contains:
#   1. NO SQLite (rusqlite is `audit-db`-gated; the OSS profile must not
#      bundle it) — checked via `sqlite3_*` C symbol names, which survive
#      in the strings/symbol table of any build that links bundled SQLite.
#   2. NO gated MCP tool-name strings (the write-tier catalog is
#      `mcp-write`-gated and never ships in official artifacts).
#   3. NO telemetry SDK markers (ADR-048 D6: Sentry/Aptabase/PostHog live
#      only in the private GUI tier). Complements the dependency-tree guard
#      in check-daemon-no-telemetry.sh with an artifact-level check.
#
# Usage: check-oss-binary.sh <path-to-conductor-binary>
# Exit 0 = clean OSS artifact; exit 1 = assertion tripped (with detail).
#
# NOTE for the public-repo migration (ADR-046): this script is deliberately
# self-contained (bash + strings/grep, `nm` best-effort) so the public
# repo's CI can adopt it unchanged.
set -euo pipefail

BIN="${1:?usage: check-oss-binary.sh <path-to-conductor-binary>}"
[[ -f "$BIN" ]] || { echo "FAIL: no such binary: $BIN"; exit 1; }

fail=0

# Pull printable strings once. `strings` handles Mach-O and ELF alike and
# works on stripped binaries (where `nm` sees nothing).
STRINGS_FILE="$(mktemp "${TMPDIR:-/tmp}/oss-binary-check.XXXXXX")"
trap 'rm -f "$STRINGS_FILE"' EXIT
strings "$BIN" > "$STRINGS_FILE"

# --- 1. SQLite -------------------------------------------------------------
# Bundled SQLite carries its own C identifiers as literal strings (API names
# in error paths, the amalgamation banner) and, unstripped, as symbols.
sqlite_hits="$(grep -ci 'sqlite3_' "$STRINGS_FILE" || true)"
if command -v nm >/dev/null 2>&1; then
  sym_hits="$(nm -g "$BIN" 2>/dev/null | grep -ci ' _\{0,1\}sqlite3_' || true)"
else
  sym_hits=0
fi
if [[ "$sqlite_hits" -gt 0 || "$sym_hits" -gt 0 ]]; then
  echo "FAIL: SQLite present in OSS binary (strings=$sqlite_hits, symbols=$sym_hits) — audit-db leaked into the artifact (ADR-045 D1/D3)"
  fail=1
else
  echo "OK: no SQLite symbols/strings"
fi

# --- 2. Gated MCP tool names ------------------------------------------------
# The canonical gated-name set from ADR-045 D3. These strings exist ONLY in
# mcp-write / llm-executor code paths; their presence means the write tier
# was compiled in.
GATED_TOOLS=(
  conductor_create_mapping
  conductor_send_midi
  conductor_start_midi_learn
  conductor_get_midi_learn_events
)
for name in "${GATED_TOOLS[@]}"; do
  if grep -q -- "$name" "$STRINGS_FILE"; then
    echo "FAIL: gated tool name '$name' present in OSS binary (ADR-045 D3)"
    fail=1
  else
    echo "OK: '$name' absent"
  fi
done

# --- 3. Telemetry SDKs (ADR-048 D6) ------------------------------------------
# Crate/SDK markers that would only appear if a telemetry dependency were
# linked. Case-insensitive SUBSTRING matches — deliberately broad (a false
# positive here is a one-line triage; a false negative ships telemetry).
TELEMETRY_PATTERNS=(
  'sentry::'
  'sentry_core'
  'aptabase'
  'posthog'
)
for pat in "${TELEMETRY_PATTERNS[@]}"; do
  if grep -qi -- "$pat" "$STRINGS_FILE"; then
    echo "FAIL: telemetry marker '$pat' present in OSS binary (ADR-048 D6)"
    fail=1
  else
    echo "OK: telemetry marker '$pat' absent"
  fi
done

# --- 4. test-helpers seams ---------------------------------------------------
# The daemon's `test-helpers` feature exposes CAS-bypass seams for the test
# suite. conductor-daemon/src/lib.rs plants a `#[used]` marker string whenever
# the feature is compiled in; a release artifact must not contain it.
if grep -q -- 'CONDUCTOR_TEST_HELPERS_COMPILED' "$STRINGS_FILE"; then
  echo "FAIL: test-helpers marker present — the test-only CAS-bypass seams were compiled into this artifact"
  fail=1
else
  echo "OK: test-helpers marker absent"
fi

if [[ "$fail" -eq 0 ]]; then
  echo "PASS: $BIN is a clean OSS artifact (no SQLite, no gated tools, no telemetry, no test seams)"
fi
exit "$fail"
