#!/usr/bin/env python3
"""Remeasure outliers from a saved run, preserving its exact binaries and inputs.

This audits historical measurements; `just bench` still builds the latest three
heads. Audit reports identify the original run and never overwrite its samples.
"""
import argparse
import copy
import datetime
import json
import math
from pathlib import Path

import compare


def outliers(report, threshold):
    selected = []
    for case in report["results"]:
        medians = case.get("median_ns", {})
        reference = medians.get("river-raptor")
        if case["control"] or not reference or case.get("error"):
            continue
        ratios = [medians[e] / reference for e in ("upstream", "ghostty") if medians.get(e)]
        if any(r >= threshold or r <= 1 / threshold for r in ratios):
            selected.append(case)
    return selected


def validate_inputs(report):
    for engine in report["engines"].values():
        binary = engine["binary"]
        if compare.digest(Path(binary["path"]).read_bytes()) != binary["sha256"]:
            raise ValueError(f"Saved binary hash differs: {binary['path']}")
    checked = set()
    for case in report["results"]:
        for args in (case["rio_args"], case["ghostty_args"]):
            if "--data" in args:
                path = Path(args[args.index("--data") + 1])
            else:
                values = [a.removeprefix("--data=") for a in args if a.startswith("--data=")]
                if not values:
                    continue
                path = Path(values[0])
            if path in checked:
                continue
            checked.add(path)
            group = (report["corpora"]["short"]["files"] if path.parent.name == "corpora"
                     else report["corpora"]["stream"])
            expected = group[path.name]
            data = path.read_bytes()
            if len(data) != expected["bytes"] or compare.digest(data) != expected["sha256"]:
                raise ValueError(f"Saved corpus differs: {path}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("report", type=Path)
    parser.add_argument("--samples", type=int, default=7)
    parser.add_argument("--warmups", type=int, default=2)
    parser.add_argument("--threshold", type=float, default=10)
    parser.add_argument("--filter", help="Replay cases containing this text instead of selecting by ratio")
    parser.add_argument("--timeout", type=float, default=120)
    args = parser.parse_args()
    if (args.samples < 1 or args.warmups < 0 or not math.isfinite(args.threshold) or args.threshold <= 1
            or not math.isfinite(args.timeout) or args.timeout <= 0):
        parser.error("Samples must be positive, warmups nonnegative and threshold > 1")
    original = json.loads(args.report.read_text())
    selected = ([c for c in original["results"] if args.filter in c["case"] and c.get("median_ns")]
                if args.filter is not None else outliers(original, args.threshold))
    if not selected:
        parser.error("No cases meet the outlier threshold")
    validate_inputs(original)
    directory = compare.new_run(args.report.resolve().parent / "audits")
    report = copy.deepcopy(original)
    report.update(id=directory.name, status="running", results=[], failures=[],
                  started_utc=datetime.datetime.now(datetime.timezone.utc).isoformat(),
                  audit_source=str(args.report.resolve()), selected_cases=[c["case"] for c in selected])
    report.pop("finished_utc", None)
    report["options"].update(samples=args.samples, warmups=args.warmups)
    report["audit_original_medians"] = {c["case"]: c["median_ns"] for c in selected}
    compare.save(directory, report)
    try:
        cases = [{k: v for k, v in c.items() if k in (
            "case", "action", "profile", "mode", "operations", "ghostty_args", "rio_args", "notes", "control",
        )} for c in selected]
        compare.measure(args, directory, report, cases)
        report["status"] = "failed" if report["failures"] else "complete"
    except (Exception, KeyboardInterrupt) as error:
        report["status"] = "failed"
        report["error"] = str(error)
    report["finished_utc"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
    compare.save(directory, report)
    print(directory / "report.html")
    return int(report["status"] != "complete")


if __name__ == "__main__":
    raise SystemExit(main())
