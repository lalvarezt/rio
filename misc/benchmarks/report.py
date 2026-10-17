#!/usr/bin/env python3
"""Write an upstream-to-branch report from retained benchmark samples."""
import argparse
import collections
import csv
import json
from pathlib import Path
import platform
import statistics

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT/"target/branch-performance"


def median(values):
    return statistics.median(values)


def windows_findings(index, results, motion, out):
    counts = collections.Counter(row["classification"] for row in results)
    lines = ["## Findings", "",
        f"The shared suite contains {len(results)} cases: {counts['faster']} faster, {counts['slower']} slower, "
        f"{counts['mixed or within noise']} mixed or within the noise guard, and {counts['new feature cost']} new-feature costs. "
        "These counts are not an overall performance score. The following cases connect the isolated stages to application workloads.", "",
        "| Case | Paired time change | Classification |", "|---|---:|---|"]
    for case in ["input/mixed","input/unicode","snapshot/plain/full","row/plain/cached","ui_text/cached_long",
                 "ui_text/title_churn","wgpu/frame/full","wgpu/frame/unchanged","wgpu/second_window_context",
                 "application/dense","application/scroll","application/search","application/palette","application/tabs",
                 "application/tab_repeat","windows/startup_exit","windows/minimize_restore",
                 "internal/vt_input/unicode_ascii","internal/vt_input/unicode","internal/bench_poll"]:
        row = index[case]
        lines.append(f"| `{case}` | {row['time_change_percent']:+.1f}% | {row['classification']} |")
    lines += ["", "UI cache churn measures `Text::measure` over 4,096 distinct titles. "
        "The branch caps retained shapes at 2,048. This is a bounded-memory tradeoff, not a complete measure-and-draw frame. "
        "Warmed allocation counts and the retained-shape counter are recorded separately.", "",
        "The Windows application replays use the same fixed terminal workloads as Linux and the Vulkan WGPU backend. "
        "They measure Rio process CPU time, excluding child Python interpreters. "
        "The exit probe measures the fixture stop-file signal to Rio process exit, including the child's 10 ms polling interval and shutdown. "
        "It does not isolate event-loop dispatch latency. "
        "Minimize/restore also samples the child terminal's dimensions throughout 20 cycles and fails if the branch changes them.", "",
        "## Native Windows captures", "",
        "These captures instrument the branch only and use `cmd`, rather than PowerShell. "
        "They do not establish a speedup relative to upstream. Frame interval statistics include pacing waits and are not input latency. "
        "The native capture process is not restricted to the shared suite's single CPU.", "",
        "| Motion level | Frames | Process CPU ms | Frame interval median ms | Frame interval p95 ms |", "|---|---:|---:|---:|---:|"]
    for level in ["none","medium","full"]:
        captures = [row["capture"] for row in motion if row["case"]==f"windows/motion/{level}"]
        if not captures: continue
        metric = lambda field:median(c["metrics"]["frame.interval"][field] for c in captures)
        lines.append(f"| {level} | {median(c['frames'] for c in captures):.0f} | "
                     f"{median(c['process_cpu_ms'] for c in captures):.3f} | {metric('median_ms'):.3f} | {metric('p95_ms'):.3f} |")
    lines += ["", "| Branch workload | Shell ready ms | First frame ms | Process CPU ms |", "|---|---:|---:|---:|"]
    for case in ["startup_exit","second_window","many_tabs","dense"]:
        path = out/"windows"/case/"summary.json"
        if path.exists():
            summary = json.loads(path.read_text())
            lines.append(f"| `{case}` | {summary['shell_ready_ms']:.3f} | {summary['first_frame_ms']:.3f} | {summary['process_cpu_ms']:.3f} |")
    return lines+[""]


def main(out=OUT):
    build = json.loads((out/"build.json").read_text())
    run = json.loads((out/"run.json").read_text())
    is_windows = run["platform"]=="win32"
    label = "Windows" if is_windows else "Linux"
    doc = ROOT/f"misc/profiling/{label.lower()}-branch-performance-report.md"
    csv_path = doc.with_suffix(".csv")
    motion_path = out/"windows/motion.json"
    windows_motion = json.loads(motion_path.read_text()) if motion_path.exists() else []
    rows = [json.loads(line) for line in (out/"results.jsonl").read_text().splitlines()]
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
    with csv_path.open("w") as output:
        writer = csv.DictWriter(output,fieldnames=list(results[0]),lineterminator="\n")
        writer.writeheader(); writer.writerows(results)
    index = {row["case"]:row for row in results}
    coverage,missing = [],[]
    for change in changes:
        if change.get("platform")=="windows" and not is_windows:
            coverage.append((change["id"],"Windows only; not run"))
            continue
        if is_windows and change["id"] in ("P14","P28"):
            coverage.append((change["id"],"Not run: Linux native Vulkan" if change["id"]=="P14" else "Not run: Bash integration"))
            continue
        prefixes = [prefix for prefix in change["cases"] if not (is_windows and prefix.startswith("gpu/"))]
        measured = cases + [row["case"] for row in windows_motion]
        absent = [prefix for prefix in prefixes if not any(case.startswith(prefix) for case in measured)]
        note = "Measured shared WGPU/WGSL work; Linux native Vulkan excluded" if is_windows and change["id"] in ("P12","P13") else "Measured"
        coverage.append((change["id"],"Missing: "+", ".join(absent) if absent else note))
        if absent: missing.append(change["id"])
    check_status = []
    errors = []
    if is_windows:
        for level in ["none","medium","full"]:
            if sum(row["case"]==f"windows/motion/{level}" for row in windows_motion)!=run["runs"]:
                errors.append(f"windows/motion/{level}: incomplete native captures")
    expected_repeats = set(range(run["runs"]))
    for (case,revision),repeats in timings.items():
        if repeats and set(repeats)!=expected_repeats:
            errors.append(f"{case}/{revision}: incomplete run coverage")
    for row in results:
        if row["branch_ns"] is None or (row["classification"]!="new feature cost" and row["pairs"]!=run["runs"]):
            errors.append(f"{row['case']}: incomplete comparison")
    for case,revisions in sorted(checks.items()):
        stable = all(len(values)==1 for values in revisions.values())
        same = stable and len(revisions)==2 and revisions["upstream"]==revisions["branch"]
        if same: state = "Identical upstream and branch hashes"
        elif case.startswith("ui_text/") and stable and len(revisions)==2: state = "Different UI raster hashes; bitmap-origin alignment changed on the branch"
        else: state = "MISMATCH or unstable check"; errors.append(case)
        check_status.append((case,state))
    model = run.get("cpu_model")
    if not model:
        cpuinfo = Path("/proc/cpuinfo").read_text()
        model = next((line.split(":",1)[1].strip() for line in cpuinfo.splitlines() if line.startswith("model name")),"unknown")
    devices = sorted({row["adapter"] for row in rows if row["kind"]=="device"})
    lines = [f"# {label} branch performance report", "",
        f"Upstream: `{build['baseline']}`. Branch executable revision: `{build['branch']}`.",
        f"Benchmark driver revision: `{run['driver_revision']}`. The runner permits only Python-driver and report changes after the executable revision; compiled sources and fixture hashes must match.",
        "This report compares the final branch with upstream, not with intermediate optimization commits.","",
        "## Measurement environment", "",
        f"- System: {run.get('system',platform.platform())}; {model}.",
        f"- Toolchain: {build['rustc'].splitlines()[0]}; {build['cargo']}.",
        f"- CPU affinity: logical CPU {run['cpu']}. No power policy was changed.",
        f"- GPU adapters reported by the probes: {', '.join(devices) or 'not measured'}.",
        f"- {run['runs']} alternating executable pairs; {run['samples']} CPU samples per pair and case.",
        f"- CPU samples target {run['sample_ms']} ms. Allocation counting uses a separate warmed pass.",
        "- Matching optimized profiles in separate build directories: LTO disabled, 16 codegen units, debug information and incremental compilation disabled, no stripping.",
        (f"- Native application replays use the {run['display']}." if is_windows else "- Native application replays use an isolated KWin virtual display with Xwayland."),
        "- Builds finish before timing. The suite includes the existing VT and poll Criterion benchmarks.","",
        "## Reading the results", "",
        "A negative change means less time. The paired range shows the smallest and largest time change across runs.",
        "A case is labeled faster or slower only when every pair changes in the same direction by more than 2%.",
        "This classification is a descriptive noise guard, not a confidence interval or a statistical significance test.",
        "New features with no equivalent upstream work are cost measurements. Their relative timings do not establish a feature-preserving speedup.",
        "The results cannot be added into a single terminal speedup: the cases measure different operations and overlapping pipeline stages.","",
        f"The complete per-case timing and allocation results are in [the CSV]({csv_path.name}).", ""]
    if is_windows:
        lines += windows_findings(index,results,windows_motion,out)
    else:
        counts = collections.Counter(row["classification"] for row in results)
        lines += ["## Findings", "",
            f"The suite contains {len(results)} cases: {counts['faster']} faster, {counts['slower']} slower, "
            f"{counts['mixed or within noise']} mixed or within the noise guard, and {counts['new feature cost']} new-feature costs. "
            "These counts give equal weight to small helpers and whole-workload probes; they are not an overall performance score.", "",
            "### Terminal input and CPU rendering", "",
            f"Mixed Unicode/ASCII input time changes by {index['input/mixed']['time_change_percent']:+.1f}% relative to upstream. "
            f"The existing `unicode_ascii` VT benchmark agrees ({index['internal/vt_input/unicode_ascii']['time_change_percent']:+.1f}%). "
            "Plain ASCII input is within the noise guard. Pure Unicode is a repeatable regression: "
            f"{index['input/unicode']['time_change_percent']:+.1f}% in the new fixture and "
            f"{index['internal/vt_input/unicode']['time_change_percent']:+.1f}% in the existing VT benchmark. "
            "The branch therefore does not improve every input distribution.", "",
            f"Full plain snapshot time changes by {index['snapshot/plain/full']['time_change_percent']:+.1f}%; "
            f"styled snapshot time changes by {index['snapshot/styled/full']['time_change_percent']:+.1f}%. "
            "Both remove 60 warmed allocations and 76,800 allocated bytes per operation. "
            "Cached row processing improves across all five fixtures, with smaller gains when row content changes. "
            "Glyph bitmap reuse reduces warmed allocations, but the rasterization fixtures do not show a consistent timing win; "
            "the Unicode rasterization case is slower. Narrow selection invalidation reduces the fixture's dirty rows from 60 to 4.", "",
            "### GPU and application workloads", "",
            "Native Vulkan CPU packing, upload, and command-recording cases improve with both one and two atlases. "
            "The shared WGPU frame fixtures also improve, as does creating a second WGPU window context. "
            "These are focused controls: zero-area glyphs isolate storage and upload work rather than dense text execution.", "",
            "The native Vulkan background timestamps are mixed: the canonical transparent sentinel skips the draw, "
            "and the cursor case improves slightly, but sparse and opaque cases regress substantially "
            f"({index['gpu/background/sparse']['time_change_percent']:+.1f}% and "
            f"{index['gpu/background/opaque']['time_change_percent']:+.1f}%). "
            "The separate WGSL probe shows lower GPU time for zero-alpha-heavy backgrounds, including nonzero-RGB/zero-alpha cells, "
            "while opaque and faint backgrounds stay within the noise guard. These probes use different pipelines and execution patterns. "
            "The WGSL result does not explain away the native Vulkan regressions; their cause needs a separate investigation.", "",
            "Four-second application replays consume less Rio CPU time for cursor updates, scrolling, and search. "
            "Dense text, sparse text, Unicode, and palette replays are mixed or within the noise guard. "
            "The tab replay uses more CPU with the branch's animated tab UI; the upstream control lacks equivalent work. "
            "None of these process-CPU measurements establishes improved frame latency or fewer missed frames.", "",
            "### UI, search, and feature costs", "",
            "Palette scoring removes most allocations, and stable results/navigation reuse cached matches. "
            "The large helper-level gains do not become a measurable palette application-replay gain in this run. "
            "Changing a search query costs about 2 ns more in its isolated setter probe; unchanged queries are cheaper. "
            f"The existing poll benchmark is within the noise guard ({index['internal/bench_poll']['time_change_percent']:+.1f}%).", "",
            "Cached UI text is a repeatable regression despite removing its warmed allocation: short and Unicode labels are about 50% slower, "
            "and long labels about 74% slower. The 4,096-title churn fixture exposes a larger time/memory tradeoff: "
            "the branch retains 2,048 shapes instead of upstream's 4,096, so repeated sequential measurement evicts and reshapes titles. "
            "This fixture measures `Text::measure` only; its roughly 160-fold cost increase does not describe a complete measure-and-draw frame. "
            "It still identifies a workload where the bounded cache is substantially worse.", "",
            "Icon catalog lookups, cached upload checks, animation steps, and status revision hits have small isolated costs. "
            "SVG rasterization is a miss-only cost. Bash reporting adds work relative to the upstream no-hook control; "
            "both hook fixtures emit three reports during 10,000 commands, including the existing-hook case. "
            "The medium animation control performs five steps instead of full mode's 17; this measures work, not perceived smoothness.", "",
            "The next profiling priorities are pure Unicode parsing, cached UI text and cache churn, and native Vulkan sparse/opaque background execution. "
            "These are measured regressions or tradeoffs, rather than inferred opportunities from a single favorable microbenchmark." , ""]
    lines += ["## Changes with repeatable timing differences", "",
        "| Case | Upstream µs/op | Branch µs/op | Time change | Paired range |", "|---|---:|---:|---:|---:|"]
    for row in results:
        if row["classification"] not in ("faster","slower"): continue
        lines.append(f"| `{row['case']}` | {row['upstream_ns']/1000:,.3f} | {row['branch_ns']/1000:,.3f} | {row['time_change_percent']:+.1f}% | {row['pair_min_percent']:+.1f}% to {row['pair_max_percent']:+.1f}% |")
    lines += ["", "## New feature costs", "",
              "These cases measure work added by the branch. An upstream control, where present, provides context but does less or different work.",
              "They are not included in the repeatable-speedup table above.", "",
              "| Case | Upstream control µs/op | Branch µs/op |", "|---|---:|---:|"]
    for row in results:
        if row["classification"]!="new feature cost": continue
        before = f"{row['upstream_ns']/1000:,.3f}" if row["upstream_ns"] is not None else "No equivalent"
        after = f"{row['branch_ns']/1000:,.3f}" if row["branch_ns"] is not None else "Missing"
        lines.append(f"| `{row['case']}` | {before} | {after} |")
    lines += ["", "The full CSV also retains mixed, unchanged, incomplete, and feature-cost cases. A shader measurement is GPU time per draw.",
              "WGPU frame measurements include encoding, submission, and completion. Native Vulkan recording measurements are CPU work without submission.",
              "Application entries are Rio process CPU time for a four-second replay, not wall time per rendered frame. They exclude child-process CPU time.","",
              "## Measurement limits", "",
              "- Allocation probes count allocations in the whole process during a separate warmed pass; GPU-driver threads can contribute. Bytes mean cumulative allocated bytes, not retained memory or peak RSS.",
              ("- Windows application CPU accounting uses `GetProcessTimes`. Its resolution does not establish frame latency. Native motion captures use instrumentation and are separate from the uninstrumented upstream comparison." if is_windows else "- Application CPU accounting has a 10 ms tick resolution on this host. Small absolute differences, especially idle workloads, are coarse. The virtual display differs from an interactive desktop."),
              "- Native Vulkan recording cases update rows, upload host data, and record reset/unsubmitted command buffers. They do not time GPU text execution. WGPU frame cases submit and wait, but their glyphs have zero area.",
              "- Shader timestamps cover repeated background draws, not a complete terminal frame. Only the WGSL probe checks GPU framebuffer equality; native Vulkan background timestamps are not a framebuffer-equivalence test.",
              "- Transparent draw elision tests the canonical `CellBg::TRANSPARENT` sentinel. Other zero-alpha encodings may still issue a draw and benefit only from the shader early-out.",
              "- UI raster hashes differ intentionally with bitmap-origin alignment. Stable hashes record the output but do not establish visual correctness of that difference.",
              "- Power policy and GPU clocks were not fixed. Alternating pairs reduce order bias but do not isolate every source of system variation.","",
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
    lines += ["", ("P12 and P13 cover the shared WGPU/WGSL path here. P14 is Linux native Vulkan code. Bash integration P28 is not exercised on this Windows host. P29 and P31 use uninstrumented upstream/branch controls; P30 uses branch-only instrumented motion captures." if is_windows else "P29–P31 are Windows lifecycle and display-clock cases. Their runner is committed, but Linux cannot validate those native behaviors."),
              ("These measurements use Vulkan on Windows. They do not establish DirectX 12 performance." if is_windows else "Linux WGPU results concern the shared renderer implementation; they are not Windows measurements."),"",
              "## Reproduction and retained data", "", "```sh", "python3 misc/benchmarks/run.py build",
              f"WGPU_BACKEND=vulkan python3 misc/benchmarks/run.py run --runs {run['runs']} --samples {run['samples']} --ms {run['sample_ms']} --cpu {run['cpu']}",
              "python3 misc/benchmarks/report.py", "```", "",
              "The ignored `target/branch-performance` directory retains all samples, allocation probes, output hashes, build logs, isolated-display logs, and executable copies.",
              f"Compiled Rust fixture SHA-256: `{build['fixtures_sha256']}`.", ""]
    if is_windows:
        lines += ["Native captures: `python -X utf8 misc/benchmarks/windows.py <rio-profile.exe> --runs 3 --output target/branch-performance/windows`.",
                  f"On Linux, generate this report from copied Windows artifacts with `python3 misc/benchmarks/report.py --input {out.relative_to(ROOT)}`.",""]
    doc.write_text("\n".join(lines))
    print(json.dumps({"report":str(doc),"csv":str(csv_path),"cases":len(results),"changes":len(changes),"missing":missing,"check_errors":errors},indent=2))
    if missing or errors:
        raise SystemExit("Coverage or output checks need investigation; report is incomplete")


if __name__=="__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input",type=Path,default=OUT)
    main(parser.parse_args().input.resolve())
