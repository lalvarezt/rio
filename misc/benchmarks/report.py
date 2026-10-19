#!/usr/bin/env python3
"""Summarize retained upstream/branch samples beside the input artifacts."""
import argparse
import collections
import csv
import json
from pathlib import Path
import statistics

OUT = Path(__file__).resolve().parents[2]/"target/branch-performance"

# Upstream controls do less work for these features; their ratios are not speedups.
FEATURE_PREFIXES = (
    "icons/", "animation/", "status/", "bash/command_loop",
    "application/tabs", "application/tab_repeat", "application/idle",
)
SHARED_CASES = (
    "input/mixed", "input/ascii", "input/unicode", "input/ansi", "input/csi",
    "snapshot/plain/", "snapshot/plain/full", "snapshot/styled/full", "row/plain/", "background/plain/",
    "application/dense", "application/sparse", "application/scroll", "application/search",
    "application/palette", "application/tabs", "application/tab_repeat", "application/idle",
    "row/plain/cached", "row/plain/changing", "row/styled/changing", "row/unicode/",
    "row/unicode/changing", "row/combining/", "row/preedit/", "row/plain/rasterize",
    "row/styled/rasterize", "row/unicode/rasterize", "wgpu/grid_construct",
    "wgpu/frame/full", "wgpu/frame/unchanged", "wgpu/frame/one_row", "wgpu/row_storage/",
    "wgpu/frame/cursor_blink", "wgpu/frame/uniform_change", "wgpu/second_window_context",
    "wgsl/background/", "selection/three_rows_snapshot", "search/unchanged_query",
    "search/changing_query", "palette/score_ascii", "palette/score_unicode",
    "palette/edit_query", "palette/stable_results", "palette/navigation", "palette/append_query",
    "ui_text/cached_short", "ui_text/cached_long", "ui_text/cached_unicode", "ui_text/title_churn",
    "icons/svg_raster/", "icons/atlas_hit", "icons/failed_upload_hit", "icons/catalog/",
    "animation/", "status/oversized_sequence", "status/revision_hit",
    "status/report_and_sync", "status/idle_deadline",
)
LINUX_CASES = (
    "gpu/row_storage/", "gpu/background/transparent", "gpu/background/cursor",
    "gpu/background/sparse", "gpu/background/opaque", "gpu/record/1_atlas/",
    "gpu/record/2_atlas/", "bash/command_loop",
)
WINDOWS_CASES = (
    "windows/startup_exit", "windows/minimize_restore",
    "windows/motion/none", "windows/motion/medium", "windows/motion/full",
)


def main(out=OUT, partial=False):
    build = json.loads((out/"build.json").read_text())
    run = json.loads((out/"run.json").read_text())
    windows = run["platform"] == "win32"
    label = "Windows" if windows else "Linux"
    scope = "kernel" if partial else "branch"
    # Generated reports belong with their samples, never in the source tree.
    doc = out/f"{label.lower()}-{scope}-performance-report.md"
    motion_path = out/"windows/motion.json"
    motion = json.loads(motion_path.read_text()) if motion_path.exists() else []
    rows = [json.loads(line) for line in (out/"results.jsonl").read_text().splitlines()]
    timings = collections.defaultdict(lambda: collections.defaultdict(list))
    allocations = collections.defaultdict(list)
    checks = collections.defaultdict(lambda: collections.defaultdict(set))
    for row in rows:
        key = (row.get("case"), row["revision"])
        if row["kind"] == "bench":
            timings[key][row["repeat"]].append(row["ns"])
        elif row["kind"] == "alloc":
            allocations[key].append(row)
        elif row["kind"] == "check":
            checks[row["case"]][row["revision"]].add(row["value"])
    cases = sorted({case for case, _ in timings})
    errors = []
    results = []
    for case in cases:
        before, after = (
            {repeat: statistics.median(values) for repeat, values in timings[(case, rev)].items()}
            for rev in ("upstream", "branch")
        )
        pairs = sorted(before.keys() & after.keys())
        positive = all(before[repeat] > 0 and after[repeat] > 0 for repeat in pairs)
        ratios = [after[repeat]/before[repeat] for repeat in pairs] if positive else []
        classification = "zero recorded time" if pairs and not positive else "incomplete"
        if ratios:
            classification = ("faster" if max(ratios) < 0.98 else "slower"
                              if min(ratios) > 1.02 else "mixed or within noise")
        if case.startswith(FEATURE_PREFIXES):
            classification = "new feature cost"
        result = {"case": case,
                  "upstream_ns": statistics.median(before.values()) if before else None,
                  "branch_ns": statistics.median(after.values()) if after else None,
                  "time_change_percent": 100*(statistics.median(ratios)-1) if ratios else None,
                  "pair_min_percent": 100*(min(ratios)-1) if ratios else None,
                  "pair_max_percent": 100*(max(ratios)-1) if ratios else None,
                  "pairs": len(pairs), "classification": classification}
        for revision in ("upstream", "branch"):
            for field in ("allocations", "bytes"):
                values = [row[field] for row in allocations[(case, revision)]]
                result[f"{revision}_{field}"] = statistics.median(values) if values else None
        results.append(result)
        if not after or (classification != "new feature cost" and len(pairs) != run["runs"]):
            errors.append(f"{case}: incomplete comparison")
    expected_repeats = set(range(run["runs"]))
    for (case, revision), repeats in timings.items():
        if repeats and set(repeats) != expected_repeats:
            errors.append(f"{case}/{revision}: incomplete run coverage")
    if windows:
        for level in ("none", "medium", "full"):
            if sum(row["case"] == f"windows/motion/{level}" for row in motion) != run["runs"]:
                errors.append(f"windows/motion/{level}: incomplete native captures")
    required = SHARED_CASES + (WINDOWS_CASES if windows else LINUX_CASES)
    measured = cases + [row["case"] for row in motion]
    missing = [prefix for prefix in required if not any(case.startswith(prefix) for case in measured)]
    check_status = []
    for case, revisions in sorted(checks.items()):
        stable = len(revisions) == 2 and all(len(values) == 1 for values in revisions.values())
        if stable and revisions["upstream"] == revisions["branch"]:
            state = "Identical hashes"
        elif stable and case.startswith("ui_text/"):
            state = "Different UI raster hashes: bitmap-origin alignment differs"
        else:
            state = "MISMATCH or unstable check"
            errors.append(case)
        check_status.append((case, state))
    csv_path = doc.with_suffix(".csv")
    if results:
        with csv_path.open("w", newline="") as output:
            writer = csv.DictWriter(output, fieldnames=list(results[0]), lineterminator="\n")
            writer.writeheader()
            writer.writerows(results)
    devices = sorted({row["adapter"] for row in rows if row["kind"] == "device"})
    lines = [f"# {label} {scope} performance report", "",
             f"Upstream: `{build['baseline']}`. Branch: `{build['branch']}`.",
             f"Driver: `{run['driver_revision']}`. Compiled fixtures: `{build['fixtures_sha256']}`.",
             f"Toolchain: {build['rustc'].splitlines()[0]}; {build['cargo']}.",
             f"System: {run.get('system', run['platform'])}; CPU: {run.get('cpu_model', 'not recorded')}.",
             f"Runs: {run['runs']}; samples: {run['samples']}; sample duration: {run['sample_ms']} ms; CPU affinity: {run['cpu']}.",
             f"Adapters: {', '.join(devices) or 'not recorded'}.", "",
             "Times are paired medians. Faster/slower requires every pair to clear a 2% noise guard.",
             "New feature controls perform different work and do not establish speedups.",
             "Application times are process CPU per replay; GPU recording times exclude execution.",
             "Allocation counts exclude native driver and GPU storage; bytes are allocation traffic, not retained memory.", "",
             "| Case | Upstream µs | Branch µs | Paired change | Classification |",
             "|---|---:|---:|---:|---|"]
    for row in results:
        values = [f"{row[key]/1000:.3f}" if row[key] is not None else "Missing"
                  for key in ("upstream_ns", "branch_ns")]
        change = f"{row['time_change_percent']:+.1f}%" if row["time_change_percent"] is not None else "No ratio"
        lines.append(f"| `{row['case']}` | {' | '.join(values)} | {change} | {row['classification']} |")
    if motion:
        lines += ["", "## Native Windows captures", "",
                  "Branch-only instrumentation; frame intervals include pacing and are not input latency.", "",
                  "| Mode | Frames | Process CPU ms | Interval median ms | Interval p95 ms |",
                  "|---|---:|---:|---:|---:|"]
        for level in ("none", "medium", "full"):
            captures = [row["capture"] for row in motion if row["case"] == f"windows/motion/{level}"]
            if captures:
                values = [statistics.median(c["frames"] for c in captures),
                          statistics.median(c["process_cpu_ms"] for c in captures)]
                values += [statistics.median(c["metrics"]["frame.interval"][key] for c in captures)
                           for key in ("median_ms", "p95_ms")]
                lines.append(f"| {level} | " + " | ".join(f"{v:.3f}" for v in values) + " |")
    lines += ["", "## Output checks", "", "| Case | Result |", "|---|---|"]
    lines += [f"| `{case}` | {state} |" for case, state in check_status]
    lines += ["", "## Work counters", ""]
    for row in rows:
        if row["kind"] == "counter":
            lines.append(f"- `{json.dumps(row, sort_keys=True)}`")
    lines += ["", "## Missing cases", ""] + [f"- `{prefix}`" for prefix in missing]
    lines += ["", "## Validation errors", ""] + [f"- {error}" for error in errors]
    doc.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(json.dumps({"report": str(doc), "csv": str(csv_path) if results else None,
                      "cases": len(results), "missing": missing, "check_errors": errors}, indent=2))
    if missing or errors:
        raise SystemExit("Coverage or output checks need investigation; report is incomplete")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", type=Path, default=OUT)
    parser.add_argument("--partial", action="store_true", help="name a kernel report without relaxing coverage checks")
    args = parser.parse_args()
    main(args.input.resolve(), args.partial)
