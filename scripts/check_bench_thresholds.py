#!/usr/bin/env python3
# Copyright 2025 Amiable
# SPDX-License-Identifier: MIT
"""Gate the unified routing passthrough benchmark (ADR-036 D7 / Slice 9).

Parses the machine-parseable metrics block emitted by
`conductor-daemon/benches/unified_routing_bench.rs`:

    === unified_routing_bench metrics ===
    events 10000
    median_us 0.1234
    p99_us 0.4567
    stddev_us 0.0123
    === end metrics ===

and exits non-zero if any metric exceeds its threshold, so CI fails on a
catastrophic routing-latency regression.

Usage:
    cargo bench --package conductor-daemon --bench unified_routing_bench \\
        | tee bench_results.txt
    python3 scripts/check_bench_thresholds.py bench_results.txt \\
        --median-max-us 1000 --p99-max-us 3000 --stddev-max-us 500

Reads from the named file, or stdin if no file is given.
"""

import argparse
import re
import sys

# Default thresholds (microseconds) from ADR-036 D7:
# median < 1.0 ms, P99 < 3.0 ms, jitter (stddev) < 0.5 ms.
DEFAULTS = {
    "median_us": 1000.0,
    "p99_us": 3000.0,
    "stddev_us": 500.0,
}

METRIC_RE = re.compile(r"^(median_us|p99_us|stddev_us)\s+([0-9]+(?:\.[0-9]+)?)\s*$")


def parse_metrics(text):
    """Extract {metric_key: value_us} from the benchmark output."""
    metrics = {}
    for line in text.splitlines():
        m = METRIC_RE.match(line.strip())
        if m:
            metrics[m.group(1)] = float(m.group(2))
    return metrics


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "input",
        nargs="?",
        help="benchmark output file (default: stdin)",
    )
    parser.add_argument("--median-max-us", type=float, default=DEFAULTS["median_us"])
    parser.add_argument("--p99-max-us", type=float, default=DEFAULTS["p99_us"])
    parser.add_argument("--stddev-max-us", type=float, default=DEFAULTS["stddev_us"])
    args = parser.parse_args(argv)

    if args.input:
        with open(args.input, "r", encoding="utf-8") as fh:
            text = fh.read()
    else:
        text = sys.stdin.read()

    metrics = parse_metrics(text)

    thresholds = {
        "median_us": args.median_max_us,
        "p99_us": args.p99_max_us,
        "stddev_us": args.stddev_max_us,
    }

    missing = [k for k in thresholds if k not in metrics]
    if missing:
        print(
            f"ERROR: benchmark output missing metric(s): {', '.join(sorted(missing))}. "
            f"Did the bench print its '=== unified_routing_bench metrics ===' block?",
            file=sys.stderr,
        )
        return 2

    failed = False
    for key, limit in thresholds.items():
        value = metrics[key]
        status = "OK" if value <= limit else "FAIL"
        if value > limit:
            failed = True
        print(f"  {key}: {value:.4f} µs (limit {limit:.1f} µs) [{status}]")

    if failed:
        print(
            "Benchmark gate FAILED — routing passthrough latency exceeded a "
            "threshold (ADR-036 D7). Investigate a regression before merging.",
            file=sys.stderr,
        )
        return 1

    print("Benchmark gate passed — all routing-latency metrics within thresholds.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
