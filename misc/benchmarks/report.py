#!/usr/bin/env python3
"""Write the upstream-to-branch Linux report from retained benchmark samples."""
import collections
import csv
import json
from pathlib import Path
import platform
import statistics

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT/"target/branch-performance"
DOC = ROOT/"misc/profiling/linux-branch-performance-report.md"
CSV = DOC.with_suffix(".csv")


def median(values):
    return statistics.median(values)


def main():
    build = json.loads((OUT/"build.json").read_text())
    run = json.loads((OUT/"run.json").read_text())
    rows = [json.loads(line) for line in (OUT/"results.jsonl").read_text().splitlines()]
    changes = json.loads((ROOT/"misc/benchmarks/changes.json").read_text())["changes"]
    feature_prefixes = [prefix for change in changes if change.get("new_feature") for prefix in change["cases"]]
    timings = collections.defaultdict(lambda:collections.defaultdict(list))
    allocations = collections.defaultdict(list)
    checks = collections.defaultdict(lambda:collections.defaultdict(set))
    counters = collections.defaultdict(lambda:collections.defaultdict(list))
    for row in rows:
        key = (row.get("case"),row["revision"])
        if row["kind"]=="bench": timings[key][row["repeat"]].append(row["ns"])
        elif row["kind"]=="alloc": allocations[key].append(row)
        elif row["kind"]=="check": checks[row["case"]][row["revision"]].add(row["value"])
        elif row["kind"]=="counter": counters[row["case"]][row["revision"]].append(row)
    cases = sorted({case for case,_ in timings})
    results = []
    for case in cases:
        revisions = {revision:{repeat:median(values) for repeat,values in timings[(case,revision)].items()} for revision in ["upstream","branch"]}
        before,after = revisions["upstream"],revisions["branch"]
        pairs = sorted(before.keys() & after.keys())
        ratios = [after[repeat]/before[repeat] for repeat in pairs if before[repeat]>0]
        classification = "new feature cost" if not before else "incomplete"
        if ratios:
            classification = "faster" if max(ratios)<0.98 else "slower" if min(ratios)>1.02 else "mixed or within noise"
        if any(case.startswith(prefix) for prefix in feature_prefixes):
            classification = "new feature cost"
        result = {"case":case,"upstream_ns":median(before.values()) if before else None,
                  "branch_ns":median(after.values()) if after else None,
                  "time_change_percent":100*(median(ratios)-1) if ratios else None,
                  "pair_min_percent":100*(min(ratios)-1) if ratios else None,
                  "pair_max_percent":100*(max(ratios)-1) if ratios else None,
                  "pairs":len(pairs),"classification":classification}
        for revision in ["upstream","branch"]:
            for field in ["allocations","bytes"]:
                values = [row[field] for row in allocations[(case,revision)]]
                result[f"{revision}_{field}"] = median(values) if values else None
        results.append(result)
    with CSV.open("w") as output:
        writer = csv.DictWriter(output,fieldnames=list(results[0]))
        writer.writeheader(); writer.writerows(results)
    index = {row["case"]:row for row in results}
    coverage,missing = [],[]
    for change in changes:
        if change.get("platform")=="windows":
            coverage.append((change["id"],"Windows only; not run"))
            continue
        absent = [prefix for prefix in change["cases"] if not any(case.startswith(prefix) for case in cases)]
        coverage.append((change["id"],"Missing: "+", ".join(absent) if absent else "Measured"))
        if absent: missing.append(change["id"])
    check_status = []
    errors = []
    for case,revisions in sorted(checks.items()):
        stable = all(len(values)==1 for values in revisions.values())
        same = stable and len(revisions)==2 and revisions["upstream"]==revisions["branch"]
        if same: state = "Identical upstream and branch hashes"
        elif case.startswith("ui_text/") and stable: state = "Different UI raster hashes; bitmap-origin alignment changed on the branch"
        else: state = "MISMATCH or unstable check"; errors.append(case)
        check_status.append((case,state))
    cpuinfo = Path("/proc/cpuinfo").read_text()
    model = next((line.split(":",1)[1].strip() for line in cpuinfo.splitlines() if line.startswith("model name")),"unknown")
    devices = sorted({row["adapter"] for row in rows if row["kind"]=="device"})
    lines = ["# Linux branch performance report", "",
        f"Upstream: `{build['baseline']}`. Branch benchmark revision: `{build['branch']}`.",
        "This report compares the final branch with upstream, not with intermediate optimization commits.","",
        "## Measurement environment", "",
        f"- System: {platform.platform()}; {model}.",
        f"- Toolchain: {build['rustc'].splitlines()[0]}; {build['cargo']}.",
        f"- CPU affinity: logical CPU {run['cpu']}. No power policy was changed.",
        f"- GPU adapters reported by the probes: {', '.join(devices) or 'not measured'}.",
        f"- {run['runs']} alternating executable pairs; {run['samples']} CPU samples per pair and case.",
        f"- CPU samples target {run['sample_ms']} ms. Allocation counting uses a separate warmed pass.",
        "- Matching optimized profiles in separate build directories: LTO disabled, 16 codegen units, debug information and incremental compilation disabled, no stripping.",
        "- Native application replays use an isolated KWin virtual display with Xwayland.",
        "- Builds finish before timing. The suite includes the existing VT and poll Criterion benchmarks.","",
        "## Reading the results", "",
        "A negative change means less time. The paired range shows the smallest and largest time change across runs.",
        "A case is labeled faster or slower only when every pair changes in the same direction by more than 2%.",
        "This classification is a descriptive noise guard, not a confidence interval or a statistical significance test.",
        "New features with no equivalent upstream work are cost measurements. Their relative timings do not establish a feature-preserving speedup.",
        "The results cannot be added into a single terminal speedup: the cases measure different operations and overlapping pipeline stages.","",
        "The complete per-case timing and allocation results are in [the CSV](linux-branch-performance-report.csv).", "",
        "## Changes with repeatable timing differences", "",
        "| Case | Upstream µs/op | Branch µs/op | Time change | Paired range |", "|---|---:|---:|---:|---:|"]
    for row in results:
        if row["classification"] not in ("faster","slower"): continue
        lines.append(f"| `{row['case']}` | {row['upstream_ns']/1000:,.3f} | {row['branch_ns']/1000:,.3f} | {row['time_change_percent']:+.1f}% | {row['pair_min_percent']:+.1f}% to {row['pair_max_percent']:+.1f}% |")
    lines += ["", "The full CSV also retains mixed, unchanged, incomplete, and feature-cost cases. A shader measurement is GPU time per draw.",
              "WGPU frame measurements include encoding, submission, and completion. Native Vulkan recording measurements are CPU work without submission.",
              "Application entries are Rio process CPU time for a four-second replay, not wall time per rendered frame. They exclude child-process CPU time.","",
              "## Storage and work counters", ""]
    for case,revisions in sorted(counters.items()):
        values = {}
        for revision,records in revisions.items():
            values[revision] = {key:median([row[key] for row in records]) for key in records[0]
                if key not in ("revision","repeat","suite","kind","case") and isinstance(records[0][key],(int,float))}
        lines.append(f"- `{case}`: `{json.dumps(values,sort_keys=True)}`.")
    lines += ["", "## Output checks", "", "| Check | Result |", "|---|---|"]
    lines += [f"| `{case}` | {state} |" for case,state in check_status]
    lines += ["", "These checks cover the recorded fixtures. They do not prove every terminal, font, or platform combination.",
              "Application UI workloads assert that requested UI changes affect the rendered client image before timing.","",
              "## Coverage of the branch inventory", "", "| Change | Result |", "|---|---|"]
    lines += [f"| {identifier} | {state} |" for identifier,state in coverage]
    lines += ["", "P29–P31 are Windows lifecycle and display-clock cases. Their runner is committed, but Linux cannot validate those native behaviors.",
              "Linux WGPU results concern the shared renderer implementation; they are not Windows measurements.","",
              "## Reproduction and retained data", "", "```sh", "python3 misc/benchmarks/run.py build",
              f"python3 misc/benchmarks/run.py run --runs {run['runs']} --samples {run['samples']} --ms {run['sample_ms']} --cpu {run['cpu']}",
              "python3 misc/benchmarks/report.py", "```", "",
              "The ignored `target/branch-performance` directory retains all samples, allocation probes, output hashes, build logs, isolated-display logs, and executable copies.",
              f"Compiled Rust fixture SHA-256: `{build['fixtures_sha256']}`.", ""]
    DOC.write_text("\n".join(lines))
    print(json.dumps({"report":str(DOC),"csv":str(CSV),"cases":len(results),"changes":len(changes),"missing":missing,"check_errors":errors},indent=2))
    if missing or errors:
        raise SystemExit("Coverage or output checks need investigation; report is incomplete")


if __name__=="__main__": main()
