"""Calibrate common operation counts, then alternate two saved binaries."""

import argparse
import copy
import json
import math
import platform
import statistics
import subprocess
import sys
from pathlib import Path

import suite


def aggregate(captures, warmups):
    result = copy.deepcopy(captures[0])
    for capture in captures:
        suite.validate_result(capture)
        if (
            suite.operation_options(capture) != suite.operation_options(result)
            or capture["checksum"] != result["checksum"]
        ):
            raise ValueError(
                f"output or settings changed between paired samples: {result['case']}"
            )
    result["sample_ns"] = [capture["sample_ns"][0] for capture in captures]
    result["options"]["samples"] = len(captures)
    result["options"]["warmups"] = warmups
    result["median_ns_per_operation"] = (
        statistics.median(result["sample_ns"]) / result["options"]["loops"]
    )
    suite.validate_result(result)
    return result


def invoke(binary, case, loops, args):
    name, action, profile, flags, _ = case
    options = {
        "--samples": 1,
        "--warmups": 0,
        "--loops": loops,
        "--terminal-cols": args.cols,
        "--terminal-rows": args.rows,
        "--chunk-size": args.chunk_size,
        "--io": args.io,
        "--scrollback-lines": args.scrollback_lines,
        "--fill-lines": args.fill_lines,
    }
    command = [str(binary), action, *flags]
    for key, value in options.items():
        command.extend([key, str(value)])
    if profile:
        command.extend(["--data", str(args.data_dir / f"{profile}.bin")])
    completed = subprocess.run(command, check=True, capture_output=True, text=True)
    result = json.loads(completed.stdout)
    result["case"] = name
    result["command"] = command
    suite.validate_result(result)
    if not result["optimized_build"]:
        raise ValueError("paired timing requires optimized executables")
    return result


def run_pairs(args):
    args.data_dir = args.data_dir.resolve()
    args.output_dir.mkdir(parents=True, exist_ok=True)
    manifest = json.loads((args.data_dir / "manifest.json").read_text())
    suite.validate_manifest(manifest)
    selected = [case for case in suite.cases() if args.filter in case[0]]
    if (
        not selected
        or args.warmups < 0
        or not math.isfinite(args.min_sample_ms)
        or args.min_sample_ms <= 0
    ):
        raise ValueError("invalid selection, warmups, or minimum sample duration")
    for _, _, profile, _, _ in selected:
        if profile and f"{profile}.bin" not in manifest["files"]:
            raise ValueError(f"selected corpus missing from manifest: {profile}")
    for filename, info in manifest["files"].items():
        data = (args.data_dir / filename).read_bytes()
        if len(data) != info["bytes"] or suite.sha256(data) != info["sha256"]:
            raise ValueError(f"corpus does not match manifest: {filename}")
    binaries = [args.before_binary.resolve(), args.after_binary.resolve()]
    binary_hashes = [suite.sha256(binary.read_bytes()) for binary in binaries]
    results = [[], []]
    failures = []
    for case in selected:
        try:
            base = case[4]
            calibration = [invoke(binary, case, base, args) for binary in binaries]
            fastest = min(result["median_ns_per_operation"] for result in calibration)
            loops = max(base, math.ceil(args.min_sample_ms * 1_000_000 / fastest))
            for _ in range(args.warmups):
                for binary in binaries:
                    invoke(binary, case, loops, args)
            captures = [[], []]
            for sample in range(args.samples):
                # AB/BA alternation limits drift while keeping all measurements serial.
                for index in [0, 1] if sample % 2 == 0 else [1, 0]:
                    captures[index].append(invoke(binaries[index], case, loops, args))
            aggregated = [aggregate(capture, args.warmups) for capture in captures]
            for index in range(2):
                results[index].append(aggregated[index])
            print(f"{case[0]}: {loops} operations/sample", file=sys.stderr)
        except subprocess.CalledProcessError as error:
            failures.append(
                {"case": case[0], "command": error.cmd, "error": error.stderr[-4000:]}
            )
            print(f"{case[0]}: failed; recorded and continuing", file=sys.stderr)
    if not results[0]:
        raise ValueError("no paired workloads completed")
    for index, name in enumerate(("before", "after")):
        if suite.sha256(binaries[index].read_bytes()) != binary_hashes[index]:
            raise ValueError("benchmark executable changed during the run")
        report = {
            "suite": "ghostty-bench",
            "version": 1,
            "measurement_strategy": "paired-alternating",
            "min_sample_ms": args.min_sample_ms,
            "binary_revision": results[index][0]["rio_revision"],
            "binary_dirty": results[index][0]["rio_dirty"],
            "binary_sha256": binary_hashes[index],
            "platform": platform.platform(),
            "corpora": manifest,
            "results": results[index],
            "failures": failures,
        }
        suite.validate_capture(report)
        (args.output_dir / f"{name}.json").write_text(
            json.dumps(report, indent=2, allow_nan=False) + "\n"
        )
    suite.compare(
        argparse.Namespace(
            before=args.output_dir / "before.json",
            after=args.output_dir / "after.json",
            output=args.output_dir / "comparison.json",
            report_output_differences=True,
        )
    )
    (args.output_dir / "failures.json").write_text(
        json.dumps(failures, indent=2) + "\n"
    )


def add_command(commands):
    command = commands.add_parser("paired", help=__doc__)
    command.add_argument("--before-binary", type=Path, required=True)
    command.add_argument("--after-binary", type=Path, required=True)
    command.add_argument("--data-dir", type=Path, required=True)
    command.add_argument("--output-dir", type=Path, required=True)
    command.add_argument("--filter", default="")
    command.add_argument("--samples", type=suite.positive, default=7)
    command.add_argument("--warmups", type=int, default=2)
    command.add_argument("--min-sample-ms", type=float, default=25)
    command.add_argument("--cols", type=suite.positive, default=120)
    command.add_argument("--rows", type=suite.positive, default=80)
    command.add_argument("--chunk-size", type=suite.positive, default=65536)
    command.add_argument("--scrollback-lines", type=int, default=10000)
    command.add_argument("--fill-lines", type=int, default=10000)
    command.add_argument("--io", choices=("file", "memory"), default="file")
    command.set_defaults(func=run_pairs)
