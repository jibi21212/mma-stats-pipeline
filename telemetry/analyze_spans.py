#!/usr/bin/env python3
"""Compute p50/p95/counts from MMA pipeline JSONL span dumps.

Reads one or more files produced by the Go FileExporter and the Python
JSONLSpanExporter (same schema). Prints exact nanosecond-derived seconds
(no rounding beyond Python float formatting of the raw ns/1e9 value).

Usage:
    python telemetry/analyze_spans.py telemetry/runs/e2e/spans.jsonl
    python telemetry/analyze_spans.py telemetry/runs/e2e/*.jsonl
"""
from __future__ import annotations

import argparse
import json
import math
import sys
from collections import defaultdict
from typing import Any


def percentile_ns(sorted_ns: list[int], p: float) -> int | None:
    """Linear-interpolation percentile on integer nanoseconds.

    Matches numpy.percentile(..., method='linear') / the default in numpy 1.22+:
    rank = (n-1) * p/100, then interpolate between adjacent samples.
    The result is rounded to the nearest nanosecond after interpolation so
    the reported value is still an integer count of ns actually computed
    from the samples, not a hand-waved millisecond.
    """
    if not sorted_ns:
        return None
    n = len(sorted_ns)
    if n == 1:
        return sorted_ns[0]
    rank = (n - 1) * (p / 100.0)
    lo = math.floor(rank)
    hi = math.ceil(rank)
    if lo == hi:
        return sorted_ns[int(rank)]
    frac = rank - lo
    val = sorted_ns[lo] * (1.0 - frac) + sorted_ns[hi] * frac
    return int(round(val))


def ns_to_s(ns: int | None) -> str:
    if ns is None:
        return "n/a"
    return f"{ns / 1e9:.9f}"


def load_spans(paths: list[str]) -> list[dict[str, Any]]:
    spans: list[dict[str, Any]] = []
    for path in paths:
        with open(path, encoding="utf-8") as fh:
            for lineno, line in enumerate(fh, 1):
                line = line.strip()
                if not line:
                    continue
                try:
                    rec = json.loads(line)
                except json.JSONDecodeError as exc:
                    print(f"WARN: skip {path}:{lineno}: {exc}", file=sys.stderr)
                    continue
                if "name" not in rec or "duration_ns" not in rec:
                    print(f"WARN: skip {path}:{lineno}: missing name/duration_ns",
                          file=sys.stderr)
                    continue
                rec["_source"] = path
                spans.append(rec)
    return spans


def group_key(span: dict[str, Any], how: str) -> str:
    attrs = span.get("attrs") or {}
    if how == "name":
        return span["name"]
    if how == "stage":
        return str(attrs.get("etl.stage") or span["name"])
    if how == "stage_unit":
        stage = attrs.get("etl.stage")
        unit = attrs.get("etl.unit")
        if stage:
            return f"{stage}/{unit or '?'}"
        return span["name"]
    raise ValueError(how)


def summarize(spans: list[dict[str, Any]], how: str) -> list[tuple[str, dict[str, Any]]]:
    buckets: dict[str, list[int]] = defaultdict(list)
    errors: dict[str, int] = defaultdict(int)
    for s in spans:
        key = group_key(s, how)
        buckets[key].append(int(s["duration_ns"]))
        if s.get("status_code") == "ERROR":
            errors[key] += 1
    rows = []
    for key, vals in sorted(buckets.items()):
        vals.sort()
        total = sum(vals)
        rows.append((key, {
            "n": len(vals),
            "errors": errors.get(key, 0),
            "p50_ns": percentile_ns(vals, 50),
            "p95_ns": percentile_ns(vals, 95),
            "min_ns": vals[0],
            "max_ns": vals[-1],
            "sum_ns": total,
        }))
    return rows


def print_table(title: str, rows: list[tuple[str, dict[str, Any]]]) -> None:
    print()
    print(title)
    print("-" * len(title))
    hdr = f"{'key':<36} {'n':>8} {'err':>6} {'p50_s':>16} {'p95_s':>16} {'min_s':>16} {'max_s':>16}"
    print(hdr)
    for key, d in rows:
        print(f"{key:<36} {d['n']:>8} {d['errors']:>6} "
              f"{ns_to_s(d['p50_ns']):>16} {ns_to_s(d['p95_ns']):>16} "
              f"{ns_to_s(d['min_ns']):>16} {ns_to_s(d['max_ns']):>16}")


def etl_counts(spans: list[dict[str, Any]]) -> dict[str, int]:
    fighters = events = fights = rounds = 0
    for s in spans:
        if s.get("name") != "etl.load":
            continue
        attrs = s.get("attrs") or {}
        unit = attrs.get("etl.unit")
        if unit == "fighter":
            fighters += 1
        elif unit == "event":
            events += 1
            fights += int(attrs.get("etl.fight_count") or 0)
            rounds += int(attrs.get("etl.round_count") or 0)
    return {
        "fighters_loaded": fighters,
        "events_loaded": events,
        "fights_loaded": fights,
        "round_stats_loaded": rounds,
    }


def wall_span(spans: list[dict[str, Any]], name: str) -> dict[str, Any] | None:
    matches = [s for s in spans if s.get("name") == name]
    if not matches:
        return None
    # If multiple (e.g. ML + scraper in one file), take the longest.
    return max(matches, key=lambda s: int(s["duration_ns"]))


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("files", nargs="+", help="JSONL span dump(s)")
    args = p.parse_args(argv)

    spans = load_spans(args.files)
    print(f"spans loaded: {len(spans)}")
    print(f"files: {args.files}")
    print("percentile method: linear interpolation on duration_ns "
          "(numpy.percentile default); p50/p95 reported as ns/1e9 seconds")

    print_table("By span name", summarize(spans, "name"))
    print_table("By etl.stage (extract/transform/load) or span name",
                summarize(spans, "stage"))
    print_table("By etl.stage/etl.unit", summarize(spans, "stage_unit"))

    counts = etl_counts(spans)
    print()
    print("ETL record counts (from etl.load span attributes)")
    print("-------------------------------------------------")
    for k, v in counts.items():
        print(f"  {k}: {v}")

    run = wall_span(spans, "scraper.run")
    if run:
        wall_ns = int(run["duration_ns"])
        wall_s = wall_ns / 1e9
        print()
        print("scraper.run wall")
        print("----------------")
        print(f"  duration_ns: {wall_ns}")
        print(f"  duration_s:  {wall_s:.9f}")
        print(f"  trace_id:    {run.get('trace_id')}")
        all_rows = (counts["fighters_loaded"] + counts["events_loaded"]
                    + counts["fights_loaded"] + counts["round_stats_loaded"])
        if wall_s > 0:
            print(f"  throughput fighters/s:     {counts['fighters_loaded'] / wall_s:.9f}")
            print(f"  throughput events/s:       {counts['events_loaded'] / wall_s:.9f}")
            print(f"  throughput fights/s:       {counts['fights_loaded'] / wall_s:.9f}")
            print(f"  throughput round_stats/s:  {counts['round_stats_loaded'] / wall_s:.9f}")
            print(f"  throughput all-rows/s:     {all_rows / wall_s:.9f}")
            print("  (all-rows = fighters + events + fights + round_stats loaded)")

    for ml_name in ("ml.run_all", "ml.cluster", "ml.train", "ml.predict",
                    "ml.features", "ml.relationships"):
        s = wall_span(spans, ml_name)
        if s:
            print(f"{ml_name}: duration_ns={s['duration_ns']} "
                  f"duration_s={int(s['duration_ns'])/1e9:.9f} n=1+"
                  f" (showing longest)")

    extract_n = sum(1 for s in spans if s.get("name") == "etl.extract")
    print()
    print(f"etl.extract spans (HTTP page fetches): {extract_n}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
