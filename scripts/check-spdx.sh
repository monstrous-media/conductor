#!/usr/bin/env bash
# License-header gate: every tracked Rust source file must start with the
# two-line copyright + SPDX header. Keeps the rebaselined header policy
# from regressing as new files land.
#
# Usage: ./scripts/check-spdx.sh   (exits non-zero listing offenders)

set -euo pipefail

fail=0
while IFS= read -r f; do
  if ! head -2 "$f" | grep -q 'SPDX-License-Identifier: MIT'; then
    echo "missing SPDX header: $f"
    fail=1
  fi
done < <(git ls-files '*.rs')

if [[ "$fail" -ne 0 ]]; then
  echo ""
  echo "Add this header as the first two lines of each file listed above:"
  echo "// Copyright 2025-2026 Monstrous Media"
  echo "// SPDX-License-Identifier: MIT"
  exit 1
fi
echo "SPDX headers OK"
