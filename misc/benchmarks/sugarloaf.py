#!/usr/bin/env python3
"""Capture Sugarloaf timings, allocations, pixel hashes, and sampled CPU stacks."""
import argparse
import contextlib
import datetime
import hashlib
import json
import os
from pathlib import Path
import shutil
import statistics
import subprocess

from run import OUT, ROOT, display


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("label", help="Capture directory name, e.g. baseline or optimized")
    parser.add_argument("--gpu", action="store_true", help="Use the isolated KWin/Xwayland display")
    parser.add_argument("--profile", action="store_true", help="Record perf stacks for selected workloads")
    parser.add_argument("--validate", action="store_true", help="Run library tests and, with --gpu, native Vulkan byte and pixel checks")
    parser.add_argument("--reference", help="Compare binaries retained in an earlier capture")
    parser.add_argument("--source", type=Path, default=ROOT, help="Build a separate checkout with the same benchmark fixtures")
    parser.add_argument("--repeats", type=int, default=1, help="Alternate reference/current run order on each repeat")
    parser.add_argument("--cpu", type=int, default=min(os.sched_getaffinity(0)))
    parser.add_argument("--samples", type=int, default=11)
    parser.add_argument("--ms", type=int, default=100)
    parser.add_argument("--filter", default="")
    args = parser.parse_args()
    if Path(args.label).name != args.label or args.label in (".", ".."):
        parser.error("label must be a directory name")
    if args.reference and (Path(args.reference).name != args.reference or args.reference in (".", "..")):
        parser.error("reference must be a capture directory name")
    if args.repeats < 1:
        parser.error("repeats must be positive")
    out = ROOT / "target/sugarloaf-profile/captures" / args.label
    out.mkdir(parents=True, exist_ok=False)
    source = args.source.resolve()
    target = ROOT / "target/sugarloaf-profile"
    if source != ROOT:
        # Cargo artifacts can collide across worktrees with the same package
        # names. Each source checkout needs its own compilation directory.
        target = target / "checkouts" / hashlib.sha256(str(source).encode()).hexdigest()[:12]
    env = dict(os.environ, CARGO_TARGET_DIR=str(target),
               CARGO_PROFILE_BENCH_LTO="false", CARGO_PROFILE_BENCH_CODEGEN_UNITS="16",
               CARGO_PROFILE_BENCH_DEBUG="1", CARGO_PROFILE_BENCH_STRIP="none",
               RIO_BENCH_SAMPLES=str(args.samples), RIO_BENCH_MS=str(args.ms))
    benches = ["rendering", "branch_performance"] + (["branch_gpu"] if args.gpu else [])
    command = ["cargo", "bench", "-p", "sugarloaf", "--features", "rio-window/x11", "--no-run", "--message-format=json"]
    for bench in benches:
        command.extend(["--bench", bench])
    print("Building Sugarloaf profiling workloads", flush=True)
    with (out / "build.log").open("w") as errors:
        build = subprocess.run(command, cwd=args.source, env=env, text=True, stdout=subprocess.PIPE, stderr=errors)
    (out / "build.jsonl").write_text(build.stdout)
    build.check_returncode()
    binaries = {}
    for line in build.stdout.splitlines():
        row = json.loads(line)
        if row.get("executable") and row["target"]["name"] in benches:
            name = row["target"]["name"]
            binaries[name] = str(out / name)
            shutil.copy2(row["executable"], binaries[name])
            debug = Path(row["executable"] + ".dwp")
            if debug.is_file():
                shutil.copy2(debug, binaries[name] + ".dwp")
    assert set(binaries) == set(benches)
    binary_hashes = {name: hashlib.sha256(Path(binary).read_bytes()).hexdigest()
                     for name, binary in binaries.items()}
    source_hashes = {name: hashlib.sha256((source / name).read_bytes()).hexdigest() for name in (
        "sugarloaf/src/grid/cpu.rs", "sugarloaf/src/grid/vulkan.rs", "sugarloaf/src/renderer/cpu.rs",
        "sugarloaf/src/grid/mod.rs", "sugarloaf/src/grid/atlas.rs",
        "sugarloaf/src/grid/shaders/grid_text.vert.glsl",
        "sugarloaf/src/premul.rs", "sugarloaf/src/text.rs",
        "sugarloaf/benches/rendering.rs", "sugarloaf/benches/branch_gpu.rs",
        "misc/benchmarks/cpu_state.rs", "misc/benchmarks/support.rs")}
    for shader in sorted((source / "sugarloaf/src/grid/shaders").glob("*.glsl")):
        source_hashes[str(shader.relative_to(source))] = hashlib.sha256(shader.read_bytes()).hexdigest()
    if args.reference:
        reference_hashes = {name: hashlib.sha256((out.parent / args.reference / name).read_bytes()).hexdigest()
                            for name in binaries}
        if binary_hashes == reference_hashes:
            raise RuntimeError("All reference/current executables are identical; check the build directories")
    metadata = {"date": datetime.datetime.now(datetime.timezone.utc).isoformat(),
                "head": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=args.source, text=True).strip(),
                "source": str(source), "binaries_sha256": binary_hashes, "source_sha256": source_hashes,
                "cpu": args.cpu, "samples": args.samples, "ms": args.ms,
                "rustc": subprocess.check_output(["rustc", "-Vv"], text=True),
                "profile": {k: v for k, v in env.items() if k.startswith("CARGO_PROFILE_")},
                "filter": args.filter, "reference": args.reference, "repeats": args.repeats}
    (out / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
    records = []
    if args.gpu:
        OUT.mkdir(parents=True, exist_ok=True)
    with display() if args.gpu else contextlib.nullcontext({}) as graphics:
        env.update(graphics)
        if args.validate:
            tests = [["cargo", "test", "-p", "sugarloaf", "-p", "rio-grid", "--profile", "bench", "--lib"]]
            if args.gpu:
                tests.append(tests[0] + ["mixed_atlas_edits_reach_every_frame_slot", "--", "--ignored"])
                tests.append(tests[0] + ["retained_pixels_match_compact_across_frame_slots", "--", "--ignored"])
                tests.append(tests[0] + ["background_pixels_match_fullscreen_reference", "--", "--ignored"])
                tests.append(tests[0] + ["color_glyph_pixels_ignore_foreground_and_cursor_tint", "--", "--ignored"])
            for index, test in enumerate(tests):
                print("Validating " + " ".join(test), flush=True)
                with (out / f"tests-{index}.log").open("w") as log:
                    subprocess.run(test, cwd=args.source, env=env, stdout=log, stderr=subprocess.STDOUT, check=True)
        for name in benches:
            binary = binaries[name]
            variants = [("current", binary)]
            if args.reference:
                reference = out.parent / args.reference / name
                if not reference.is_file():
                    raise RuntimeError("Missing reference binary: " + str(reference))
                variants.insert(0, ("reference", str(reference)))
            for repeat in range(args.repeats):
                for revision, executable in variants[::1 if repeat % 2 == 0 else -1]:
                    print(f"Measuring {name} {revision} repeat {repeat}", flush=True)
                    run_env = dict(env, RIO_BENCH_FILTER=args.filter)
                    log = subprocess.run(["taskset", "-c", str(args.cpu), executable], cwd=ROOT,
                                         env=run_env, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
                    (out / f"{name}-{revision}-{repeat}.log").write_text(log.stdout)
                    log.check_returncode()
                    for line in log.stdout.splitlines():
                        kind, _, value = line.partition(" ")
                        if kind in ("RIO_BENCH", "RIO_ALLOC", "RIO_CHECK", "RIO_DEVICE"):
                            records.append({"kind": kind, "suite": name, "revision": revision,
                                            "repeat": repeat, **json.loads(value)})
        (out / "records.json").write_text(json.dumps(records, indent=2) + "\n")
        checks = {}
        for row in records:
            if row["kind"] == "RIO_CHECK":
                checks.setdefault(row["case"], set()).add(row["value"])
        mismatches = [case for case, values in checks.items() if len(values) != 1]
        if mismatches:
            raise RuntimeError("Pixel checks differ between runs: " + ", ".join(mismatches))
        if args.profile:
            cases = [("rendering", "cpu/background/opaque"),
                     ("rendering", "cpu/background/translucent"),
                     ("rendering", "cpu/glyphs/mask"),
                     ("rendering", "cpu/glyphs/color"),
                     ("rendering", "cpu/state_hash/160x48/unchanged"),
                     ("rendering", "cpu/atlas/color/append_page_after_8"),
                     ("branch_performance", "title_churn")]
            if args.gpu:
                cases.append(("branch_gpu", "gpu/row_storage/tail_change"))
                cases.extend(("branch_gpu", f"gpu/record/2_atlas/{mode}") for mode in ("full", "full_all_cells", "one_row", "unchanged", "atlas_change"))
            for suite, case in cases:
                if args.filter and args.filter not in case:
                    continue
                stem = case.replace("/", "-")
                print("Profiling " + case, flush=True)
                # Cold growth runs once per fixture rather than for a timed
                # iteration loop. Collect enough transitions for sampled stacks.
                profile_samples = "401" if "/grow_" in case else "5"
                profile_env = dict(env, RIO_BENCH_FILTER=case,
                                   RIO_BENCH_SAMPLES=profile_samples, RIO_BENCH_MS="400")
                with (out / (stem + ".log")).open("w") as log:
                    subprocess.run(["perf", "record", "-F", "499", "--call-graph", "dwarf,16384", "-o",
                                    str(out / (stem + ".perf.data")), "--", "taskset", "-c", str(args.cpu), binaries[suite]],
                                   cwd=ROOT, env=profile_env, stdout=log, stderr=subprocess.STDOUT, check=True)
                with (out / (stem + ".perf.txt")).open("w") as report:
                    subprocess.run(["perf", "report", "--stdio", "--no-children", "--percent-limit", "1",
                                    "-i", str(out / (stem + ".perf.data"))], stdout=report, stderr=subprocess.DEVNULL, check=True)
    summaries = []
    for revision, case in dict.fromkeys((row["revision"], row["case"]) for row in records if row["kind"] == "RIO_BENCH"):
        group = [row for row in records if row.get("case") == case and row["revision"] == revision]
        samples = [row["ns"] for row in group if row["kind"] == "RIO_BENCH"]
        alloc = next((r for r in group if r["kind"] == "RIO_ALLOC"), {})
        summaries.append({"case": case, "revision": revision, "median_ns": statistics.median(samples),
                          "min_ns": min(samples), "max_ns": max(samples),
                          "allocations": alloc.get("allocations"), "bytes": alloc.get("bytes")})
    (out / "summary.json").write_text(json.dumps(summaries, indent=2) + "\n")
    print(str(out), flush=True)
    for row in summaries:
        print(f'{row["revision"]} {row["case"]}: {row["median_ns"]:.1f} ns, {row["allocations"]} allocations')


if __name__ == "__main__":
    main()
