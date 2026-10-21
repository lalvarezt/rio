#!/usr/bin/env python3
"""Generate immutable corpora, run Rio probes serially, or compare two captures.

Workload ideas: Ghostty 48dde97e0413a27064e4c6182db3df6ee999db7d,
src/synthetic and src/benchmark. See LICENSE.ghostty and README.md.
"""

import argparse
import hashlib
import json
import math
import platform
import random
import statistics
import struct
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
GHOSTTY_REVISION = "48dde97e0413a27064e4c6182db3df6ee999db7d"
PROFILES = (
    "ascii",
    "ascii-lines",
    "utf8",
    "invalid-utf8",
    "styled",
    "graphemes",
    "osc8",
    "osc-stream",
    "osc-records",
    "kitty",
)


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def record(profile, rng):
    """Return a whole record so byte targets never cut a UTF-8 or VT sequence."""
    if profile == "ascii":
        return bytes(rng.randrange(32, 127) for _ in range(256))
    if profile == "ascii-lines":
        return (
            bytes(rng.randrange(32, 127) for _ in range(rng.randint(30, 300))) + b"\r\n"
        )
    if profile == "invalid-utf8":
        return rng.choice(
            (
                b"\x80",
                b"\xc0\xaf",
                b"\xe2\x28\xa1",
                b"\xed\xa0\x80",
                b"\xf4\x90\x80\x80",
                b"ok ",
            )
        )
    if profile == "utf8":
        ranges = ((0x20, 0x7F), (0xA0, 0x800), (0x800, 0xD800), (0x10000, 0x110000))
        return "".join(
            chr(rng.randrange(*rng.choice(ranges))) for _ in range(80)
        ).encode()
    if profile in ("styled", "graphemes", "osc8"):
        alphabet = (
            "abcdefghijklmnopqrstuvwxyz    "
            if profile == "styled"
            else "abc     éλ日本語漢😀"
        )
        parts = []
        for _ in range(rng.randint(4, 12)):
            if profile == "styled" or rng.random() < 0.3:
                parts.append(
                    f"\x1b[1;38;2;{rng.randrange(256)};{rng.randrange(256)};{rng.randrange(256)}m"
                )
            if profile == "osc8" and rng.random() < 0.3:
                parts.append(
                    f"\x1b]8;id=bench-{rng.randrange(16)};https://example.com/{rng.randrange(32)}\x1b\\"
                )
            for _ in range(rng.randint(4, 16)):
                parts.append(rng.choice(alphabet))
                if profile != "styled" and rng.random() < 0.1:
                    parts.append("\u0301")
            if profile == "osc8":
                parts.append("\x1b]8;;\x1b\\")
        return ("".join(parts) + "\x1b[0m\r\n").encode()
    if profile.startswith("osc-"):
        body = rng.choice(
            (
                b"0;benchmark title",
                b"2;Unicode \xe6\xbc\xa2",
                b"4;1;rgb:ff/00/00",
                b"8;id=bench;https://example.com",
                b"8;;",
                b"10;?",
                b"52;c;dGVzdA==",
                b"9;4;1;50",
                b"999999;unknown",
                b"4;broken;rgb:gg/00/00",
                b";malformed",
            )
        )
        return (
            struct.pack("<Q", len(body)) + body
            if profile == "osc-records"
            else b"\x1b]" + body + b"\x07"
        )
    if profile == "kitty":
        # Valid framing and base64, but no transmit/decode work is claimed by
        # Rio's APC-only probe. Stream probes use the headless feature set.
        return b"\x1b_Ga=t,f=32,s=32,v=32,i=1,m=0;" + b"AAAA" * 1024 + b"\x1b\\"
    raise ValueError(profile)


def generate(args):
    args.output.mkdir(parents=True, exist_ok=True)
    manifest = {
        "version": 1,
        "ghostty_revision": GHOSTTY_REVISION,
        "seed": args.seed,
        "target_bytes": args.bytes,
        "files": {},
    }
    for profile in PROFILES:
        # A separate stable seed per profile keeps existing corpora unchanged
        # when another profile is added or generation order changes.
        rng = random.Random(f"ghostty-bench-v1:{args.seed}:{profile}")
        data = bytearray()
        while len(data) < args.bytes:
            data.extend(record(profile, rng))
        filename = f"{profile}.bin"
        path = args.output / filename
        if path.exists() and path.read_bytes() != data:
            raise ValueError(
                f"refusing to change existing corpus {path}; use a new directory"
            )
        path.write_bytes(data)
        manifest["files"][filename] = {"bytes": len(data), "sha256": sha256(data)}
    manifest_path = args.output / "manifest.json"
    if manifest_path.exists() and json.loads(manifest_path.read_text()) != manifest:
        raise ValueError("existing manifest differs; use a new directory")
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
    print(manifest_path)


def cases():
    for profile in (
        "ascii",
        "ascii-lines",
        "utf8",
        "invalid-utf8",
        "styled",
        "osc8",
        "osc-stream",
        "kitty",
    ):
        for action in ("terminal-parser", "terminal-stream"):
            yield f"{action}/{profile}", action, profile, [], 1
    for action in ("codepoint-width", "grapheme-break"):
        for profile in ("ascii", "utf8", "invalid-utf8", "graphemes"):
            for mode in ("noop", "table"):
                yield f"{action}/{profile}/{mode}", action, profile, ["--mode", mode], 1
    yield "osc-parser/records", "osc-parser", "osc-records", [], 1
    yield "apc-parser/kitty", "apc-parser", "kitty", [], 1
    for mode, loops in (("cols", 25), ("rows", 1000), ("both", 25), ("noop", 10000)):
        yield (
            f"terminal-resize/{mode}",
            "terminal-resize",
            "styled",
            ["--mode", mode],
            loops,
        )
    for mode in (
        "clone",
        "render",
        "render-clean",
        "render-partial",
        "render-scroll",
        "render-scroll-page",
        "render-output",
    ):
        yield f"screen-clone/{mode}", "screen-clone", "osc8", ["--mode", mode], 100
    for emit, mode in (
        ("plain", "format"),
        ("vt", "format"),
        ("vt", "roundtrip"),
        ("vt", "noop"),
    ):
        yield (
            f"terminal-formatter/{emit}/{mode}",
            "terminal-formatter",
            "styled",
            ["--emit", emit, "--mode", mode],
            25,
        )
    for mode in ("lookup", "churn"):
        for load in (25, 100):
            yield (
                f"hyperlink-map/{mode}/{load}",
                "hyperlink-map",
                None,
                ["--mode", mode, "--load-percent", str(load)],
                100,
            )


def capture(args):
    directory = args.data_dir.resolve()
    manifest = json.loads((directory / "manifest.json").read_text())
    validate_manifest(manifest)
    selected = [case for case in cases() if args.filter in case[0]]
    if not selected:
        raise ValueError(f"no workloads match {args.filter!r}")
    for _, _, profile, _, _ in selected:
        if profile and f"{profile}.bin" not in manifest["files"]:
            raise ValueError(f"selected corpus missing from manifest: {profile}.bin")
    for filename, info in manifest["files"].items():
        data = (directory / filename).read_bytes()
        if len(data) != info["bytes"] or sha256(data) != info["sha256"]:
            raise ValueError(f"corpus does not match manifest: {filename}")
    binary = args.binary.resolve()
    results = []
    for name, action, profile, flags, loops in selected:
        command = [
            str(binary),
            action,
            *flags,
            "--samples",
            str(args.samples),
            "--warmups",
            str(args.warmups),
            "--loops",
            str(loops * args.loops),
            "--terminal-cols",
            str(args.cols),
            "--terminal-rows",
            str(args.rows),
            "--chunk-size",
            str(args.chunk_size),
            "--io",
            args.io,
            "--scrollback-lines",
            str(args.scrollback_lines),
            "--fill-lines",
            str(args.fill_lines),
        ]
        if profile:
            command += ["--data", str(directory / f"{profile}.bin")]
        completed = subprocess.run(command, check=True, capture_output=True, text=True)
        result = json.loads(completed.stdout)
        result["case"] = name
        result["command"] = command
        validate_result(result)
        results.append(result)
        print(f"{name}: {result['median_ns_per_operation']:.0f} ns/op", file=sys.stderr)
    revision = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()
    dirty = (
        subprocess.run(
            ["git", "status", "--porcelain"],
            cwd=ROOT,
            check=True,
            capture_output=True,
            text=True,
        ).stdout
        != ""
    )
    report = {
        "suite": "ghostty-bench",
        "version": 1,
        "harness_revision": revision,
        "harness_dirty": dirty,
        "binary_revision": results[0]["rio_revision"],
        "binary_dirty": results[0]["rio_dirty"],
        "platform": platform.platform(),
        "binary_sha256": sha256(binary.read_bytes()),
        "corpora": manifest,
        "results": results,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2, allow_nan=False) + "\n")
    print(args.output)


def validate_manifest(manifest):
    if (
        not isinstance(manifest, dict)
        or type(manifest.get("version")) is not int
        or manifest["version"] != 1
        or not isinstance(manifest.get("files"), dict)
        or not manifest["files"]
    ):
        raise ValueError("invalid or unsupported corpus manifest")
    for filename, info in manifest["files"].items():
        if Path(filename).name != filename or not isinstance(info, dict):
            raise ValueError("invalid corpus manifest entry")
        digest = info.get("sha256")
        if (
            type(info.get("bytes")) is not int
            or info["bytes"] <= 0
            or not isinstance(digest, str)
            or len(digest) != 64
            or any(c not in "0123456789abcdef" for c in digest)
        ):
            raise ValueError(f"invalid corpus size or hash: {filename}")


def validate_result(result):
    if (
        not isinstance(result, dict)
        or result.get("suite") != "ghostty-bench"
        or type(result.get("version")) is not int
        or result["version"] != 1
        or type(result.get("optimized_build")) is not bool
    ):
        raise ValueError("invalid benchmark result schema or build mode")
    if not isinstance(result.get("case"), str) or not result["case"]:
        raise ValueError("missing workload name")
    options = result.get("options")
    if not isinstance(options, dict):
        raise TypeError("missing workload options")
    for key in (
        "cols",
        "rows",
        "resize_cols",
        "resize_rows",
        "entries",
        "chunk_size",
        "loops",
        "samples",
    ):
        if type(options.get(key)) is not int or options[key] <= 0:
            raise ValueError(f"invalid workload option: {key}")
    for key in ("fill_lines", "scrollback", "load_percent", "warmups"):
        if type(options.get(key)) is not int or options[key] < 0:
            raise ValueError(f"invalid workload option: {key}")
    for key in ("action", "mode", "emit", "io"):
        if not isinstance(options.get(key), str):
            raise TypeError(f"missing workload option: {key}")
    if "data" not in options or (
        options["data"] is not None and not isinstance(options["data"], str)
    ):
        raise ValueError("invalid workload data path")
    times = result.get("sample_ns")
    median = result.get("median_ns_per_operation")
    if (
        not isinstance(times, list)
        or len(times) != options["samples"]
        or any(type(t) is not int or t <= 0 for t in times)
    ):
        raise ValueError("invalid benchmark sample times")
    if (
        type(median) not in (int, float)
        or not math.isfinite(median)
        or median <= 0
        or not math.isclose(
            median, statistics.median(times) / options["loops"], rel_tol=1e-12
        )
    ):
        raise ValueError("invalid or inconsistent benchmark median")
    checksum = result.get("checksum")
    if (
        not isinstance(checksum, str)
        or len(checksum) != 16
        or any(c not in "0123456789abcdef" for c in checksum)
    ):
        raise ValueError("invalid output fingerprint")
    revision = result.get("ghostty_revision")
    if (
        not isinstance(revision, str)
        or len(revision) != 40
        or any(c not in "0123456789abcdef" for c in revision)
    ):
        raise ValueError("missing or invalid Ghostty source revision")


def validate_capture(report):
    if (
        not isinstance(report, dict)
        or report.get("suite") != "ghostty-bench"
        or type(report.get("version")) is not int
        or report["version"] != 1
        or not isinstance(report.get("results"), list)
        or not report["results"]
    ):
        raise ValueError("invalid benchmark capture schema")
    validate_manifest(report.get("corpora"))
    seen = set()
    for result in report["results"]:
        validate_result(result)
        if result["case"] in seen:
            raise ValueError("duplicate workload in capture")
        seen.add(result["case"])


def compare(args):
    before = json.loads(args.before.read_text())
    after = json.loads(args.after.read_text())
    validate_capture(before)
    validate_capture(after)
    if before["corpora"] != after["corpora"]:
        raise ValueError("corpora differ; timing ratios would be misleading")
    old = {r["case"]: r for r in before["results"]}
    new = {r["case"]: r for r in after["results"]}
    if old.keys() != new.keys():
        raise ValueError("workload selections differ")
    comparisons = []
    for name, result in new.items():
        previous = old[name]
        # File locations and sampling counts do not define the operation.
        if operation_options(previous) != operation_options(result):
            raise ValueError(f"workload settings differ: {name}")
        if (
            previous["optimized_build"] != result["optimized_build"]
            or previous["ghostty_revision"] != result["ghostty_revision"]
        ):
            raise ValueError(
                f"build modes or Ghostty workload revisions differ: {name}"
            )
        if previous["checksum"] != result["checksum"]:
            if not getattr(args, "report_output_differences", False):
                raise ValueError(f"workload output differs: {name}")
            comparisons.append(
                {
                    "case": name,
                    "comparable": False,
                    "before_checksum": previous["checksum"],
                    "after_checksum": result["checksum"],
                }
            )
            continue
        ratio = result["median_ns_per_operation"] / previous["median_ns_per_operation"]
        comparisons.append(
            {
                "case": name,
                "comparable": True,
                "after_over_before": ratio,
                "change_percent": (ratio - 1) * 100,
            }
        )
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(comparisons, indent=2, allow_nan=False) + "\n")
    print(args.output)


def operation_options(result):
    ignored = {"data", "samples", "warmups"}
    return {k: v for k, v in result["options"].items() if k not in ignored}


def check_chunks(args):
    data = bytes.fromhex(args.hex) if args.hex else args.data.read_bytes()
    if not data:
        raise ValueError("empty chunk-check input")
    results = []
    for action in ("terminal-parser", "terminal-stream"):
        captures = []
        for chunk in args.chunk_sizes:
            command = [
                str(args.binary.resolve()),
                action,
                "--data",
                "-",
                "--io",
                "memory",
                "--chunk-size",
                str(chunk),
                "--samples",
                "1",
                "--warmups",
                "0",
            ]
            completed = subprocess.run(
                command, input=data, check=True, capture_output=True
            )
            result = json.loads(completed.stdout)
            result["case"] = f"{action}/chunk-{chunk}"
            validate_result(result)
            captures.append(result)
        results.append(
            {
                "action": action,
                "matched": len({r["checksum"] for r in captures}) == 1,
                "captures": captures,
            }
        )
    report = {
        "suite": "ghostty-bench",
        "version": 1,
        "kind": "chunk-equivalence",
        "input_sha256": sha256(data),
        "input_bytes": len(data),
        "results": results,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2, allow_nan=False) + "\n")
    print(args.output)
    return all(result["matched"] for result in results)


def positive(text):
    value = int(text)
    if value <= 0:
        raise argparse.ArgumentTypeError("must be positive")
    return value


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    gen = commands.add_parser(
        "generate", help="write seeded corpora separately from measurements"
    )
    gen.add_argument("--output", type=Path, required=True)
    gen.add_argument("--bytes", type=positive, default=1024 * 1024)
    gen.add_argument("--seed", type=int, default=42)
    gen.set_defaults(func=generate)
    run = commands.add_parser("run", help="run the selected workloads sequentially")
    run.add_argument("--data-dir", type=Path, required=True)
    run.add_argument(
        "--binary", type=Path, default=ROOT / "target/release/ghostty-bench"
    )
    run.add_argument("--output", type=Path, required=True)
    run.add_argument("--filter", default="")
    run.add_argument("--samples", type=positive, default=5)
    run.add_argument("--warmups", type=int, default=2)
    run.add_argument(
        "--loops",
        type=positive,
        default=1,
        help="multiply per-workload operation counts",
    )
    run.add_argument("--cols", type=positive, default=120)
    run.add_argument("--rows", type=positive, default=80)
    run.add_argument("--chunk-size", type=positive, default=65536)
    run.add_argument("--scrollback-lines", type=int, default=10000)
    run.add_argument("--fill-lines", type=int, default=10000)
    run.add_argument("--io", choices=("file", "memory"), default="file")
    run.set_defaults(func=capture)
    diff = commands.add_parser(
        "compare", help="compare matching captures; ratios below 1 are faster"
    )
    diff.add_argument("before", type=Path)
    diff.add_argument("after", type=Path)
    diff.add_argument("--output", type=Path, required=True)
    diff.add_argument(
        "--report-output-differences",
        action="store_true",
        help="record differing fingerprints without assigning performance ratios",
    )
    diff.set_defaults(func=compare)
    from paired import add_command

    add_command(commands)
    chunks = commands.add_parser(
        "check-chunks", help="compare output fingerprints across input read boundaries"
    )
    source = chunks.add_mutually_exclusive_group(required=True)
    source.add_argument("--data", type=Path)
    source.add_argument("--hex", help="small reproduction input in hexadecimal")
    chunks.add_argument(
        "--binary", type=Path, default=ROOT / "target/release/ghostty-bench"
    )
    chunks.add_argument(
        "--chunk-sizes", nargs="+", type=positive, default=[1, 17, 65536]
    )
    chunks.add_argument("--output", type=Path, required=True)
    chunks.set_defaults(func=check_chunks)
    args = parser.parse_args()
    try:
        if args.func(args) is False:
            parser.exit(
                1, "ghostty-bench: chunk fingerprints differ; see the output report\n"
            )
    except (
        ValueError,
        TypeError,
        KeyError,
        OSError,
        subprocess.CalledProcessError,
    ) as err:
        parser.exit(1, f"ghostty-bench: {err}\n")


if __name__ == "__main__":
    main()
