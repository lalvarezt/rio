#!/usr/bin/env python3
"""Build committed branch tips and Ghostty HEAD, then retain a three-way run.

No working checkout is reset or patched. The same committed Rio harness is
overlaid on both detached Rio worktrees. Adapter patches fail closed when a
moving head changes their context; core performance fixes are never backported.
"""
import argparse
import datetime
import hashlib
import io
import json
import math
import os
from pathlib import Path
import platform
import re
import shlex
import shutil
import statistics
import subprocess
import tarfile
import time
import uuid

import native
import report as html_report
import suite

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
GHOSTTY_URL = "https://github.com/ghostty-org/ghostty.git"
ENGINES = html_report.ENGINES


def digest(data):
    return hashlib.sha256(data).hexdigest()


def git(repo, *args):
    return subprocess.check_output(["git", "-C", str(repo), *args], text=True).strip()


def resolve_branches(root):
    # Fully qualified refs avoid an 'upstream' remote shadowing the local branch.
    return {name: {"ref": f"refs/heads/{name}",
                   "commit": git(root, "rev-parse", "--verify", f"refs/heads/{name}^{{commit}}")}
            for name in ENGINES[:2]}


def new_run(parent):
    parent.mkdir(parents=True, exist_ok=True)
    stamp = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%S.%fZ")
    path = parent / f"{stamp}-{uuid.uuid4().hex[:8]}"
    path.mkdir()  # Never overwrite or remove another run, including failed runs.
    return path


def save(directory, report):
    for name, value in (("report.json", json.dumps(report, indent=2, allow_nan=False) + "\n"),
                        ("report.html", html_report.render(report, directory))):
        temporary = directory / (name + ".tmp")
        temporary.write_text(value)
        temporary.replace(directory / name)


def apply_adapter(tree, patch, evidence):
    saved = evidence / patch.name
    shutil.copyfile(patch, saved)
    command = ["git", "-C", str(tree), "apply"]
    check = subprocess.run([*command, "--check", str(saved)], capture_output=True, text=True)
    if check.returncode == 0:
        subprocess.run([*command, str(saved)], check=True)
        status = "applied"
    elif subprocess.run([*command, "--reverse", "--check", str(saved)],
                        capture_output=True).returncode == 0:
        status = "already-present"
    else:
        raise RuntimeError(f"Adapter {patch.name} no longer matches {tree.name}; "
                           f"update the committed patch. {check.stderr[-1200:]}")
    return {"name": patch.name, "sha256": digest(saved.read_bytes()),
            "status": status, "path": str(saved.relative_to(evidence.parent.parent))}


def overlay_harness(root, commit, tree, evidence):
    data = subprocess.check_output(["git", "-C", str(root), "archive", commit,
                                    "misc/ghostty-bench"])
    (evidence / "rio-harness.tar").write_bytes(data)
    # Extract only regular source files. Never follow archive symlinks or accept
    # traversal, even when a user runs this against an unfamiliar local branch.
    with tarfile.open(fileobj=io.BytesIO(data)) as archive:
        for member in archive.getmembers():
            path = Path(member.name)
            if member.isdir():
                continue
            if not member.isfile() or path.is_absolute() or ".." in path.parts:
                raise ValueError(f"Unsafe harness archive entry: {member.name}")
            target = tree / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(archive.extractfile(member).read())
    return {"commit": commit, "archive_sha256": digest(data)}


def check_native_contract(tree):
    contracts = {
        "ScreenClone": {
            "stepClone": r"for \(0\.\.1000\)",
            "stepRender": r"for \(0\.\.50_000 \* @as\(u64, self.opts.loops\)\)",
            "stepRenderClean": r"for \(0\.\.3_000_000 \* @as\(u64, self.opts.loops\)\)",
            "stepRenderPartial": r"for \(0\.\.2_000_000 \* @as\(u64, self.opts.loops\)\)",
            "renderScrollLoop": r"for \(0\.\.50_000 \* @as\(u64, self.opts.loops\)\)",
            "stepRenderOutput": r"for \(0\.\.50_000 \* @as\(u64, self.opts.loops\)\)",
        },
        "TerminalResize": {
            "stepNoop": r"for \(0\.\.50_000_000 \* @as\(u64, self.opts.loops\)\)",
            "stepCols": (r"\.cols => 25,", r"resizeTerminal\(target, rows\)", r"resizeTerminal\(cols, rows\)"),
            "stepRows": (r"for \(0\.\.2_500_000 \* @as\(u64, self.opts.loops\)\)",
                         r"resizeTerminal\(cols, target\)", r"resizeTerminal\(cols, rows\)"),
            "stepBoth": (r"for \(0\.\.25 \* @as\(u64, self.opts.loops\)\)",
                         r"resizeTerminal\(target_cols, target_rows\)", r"resizeTerminal\(cols, rows\)"),
        },
        "TerminalFormatter": {
            "stepNoop": r"for \(0\.\.self.opts.loops\)",
            "stepFormat": r"for \(0\.\.self.opts.loops\)",
        },
        "HyperlinkMap": {
            "stepLookup": r"for \(0\.\.self.opts.loops\)",
            "stepChurn": r"for \(0\.\.self.opts.loops\)",
        },
    }
    hashes = {}
    for module, functions in contracts.items():
        path = tree / f"src/benchmark/{module}.zig"
        source = path.read_text()
        hashes[str(path.relative_to(tree))] = digest(path.read_bytes())
        for function, patterns in functions.items():
            match = re.search(r"(?:pub )?fn " + function + r"\([\s\S]*?(?=\n(?:pub )?fn |\Z)", source)
            if isinstance(patterns, str):
                patterns = (patterns,)
            if not match or not all(re.search(p, match[0]) for p in patterns):
                raise RuntimeError(f"Ghostty operation-count contract changed: {module}.{function}")
    return hashes


def logged(command, cwd, path, env=None):
    print("Build/setup:", " ".join(map(str, command)), flush=True)
    with path.open("wb") as output:
        result = subprocess.run(list(map(str, command)), cwd=cwd, env=env,
                                stdout=output, stderr=subprocess.STDOUT)
    if result.returncode:
        raise RuntimeError(f"Command exited {result.returncode}; see {path}")


def prepare_data(directory, size, stream_size, seed):
    short, long = directory / "corpora", directory / "stream-corpora"
    suite.generate(argparse.Namespace(output=short, bytes=size, seed=seed))
    long.mkdir()
    manifest = {"short": json.loads((short / "manifest.json").read_text()), "stream": {}}
    for source in short.glob("*.bin"):
        data = source.read_bytes()
        # Repeat complete generation units; never truncate a VT sequence, UTF-8
        # scalar or length-prefixed OSC record to meet an arbitrary byte count.
        expanded = data * max(1, (stream_size + len(data) - 1) // len(data))
        target = long / source.name
        target.write_bytes(expanded)
        manifest["stream"][source.name] = {"bytes": len(expanded), "sha256": digest(expanded)}
    return short, long, manifest


def sample(engine, case, binary, directory, label, cpu, timeout):
    args = case["ghostty_args" if engine == "ghostty" else "rio_args"]
    command = [str(binary), *args]
    directory.mkdir(parents=True, exist_ok=True)
    stdout, stderr = directory / f"{label}.stdout", directory / f"{label}.stderr"
    def affinity():
        if cpu is not None:
            os.sched_setaffinity(0, {cpu})
    started = time.perf_counter_ns()
    started_utc = datetime.datetime.now(datetime.timezone.utc).isoformat()
    with stdout.open("wb") as out, stderr.open("wb") as err:
        result = subprocess.run(command, stdout=out, stderr=err, timeout=timeout,
                                preexec_fn=affinity if cpu is not None else None)
    wall = time.perf_counter_ns() - started
    if result.returncode:
        raise RuntimeError(f"{engine} {case['case']} exited {result.returncode}; see {stderr}")
    raw = None
    if engine == "ghostty":
        # Native diagnostics can be large. Search incrementally, keeping the
        # full stream on disk without retaining a multi-megabyte Python string.
        matches, states = [], []
        with stderr.open(errors="replace") as log:
            for line in log:
                found = re.search(r"GHOSTTY_BENCH_RESULT (\d+) (\d+)", line)
                if found:
                    matches.append(tuple(map(int, found.groups())))
                state = re.search(r"GHOSTTY_BENCH_STATE (\d+) (\d+) (\d+)", line)
                if state:
                    states.append(dict(zip(("columns", "rows", "history_lines"), map(int, state.groups()))))
        if len(matches) != 1 or matches[0][0] != 1:
            raise RuntimeError(f"Expected one native internal step timer; see {stderr}")
        elapsed = matches[0][1]
        setup = states[0] if len(states) == 1 else None
    else:
        raw = json.loads(stdout.read_text())
        if len(raw["sample_ns"]) != 1:
            raise ValueError("Expected exactly one Rio internal sample")
        if raw["options"]["loops"] != case["operations"]:
            raise ValueError("Rio operation count does not match the native normalization")
        if not raw["optimized_build"]:
            raise ValueError("Rio harness was not built with optimization")
        elapsed = raw["sample_ns"][0]
        setup = raw.get("setup_metrics")
    if elapsed <= 0:
        raise ValueError("Nonpositive internal timer")
    return {"started_utc": started_utc, "step_ns": elapsed, "ns_per_operation": elapsed / case["operations"],
            "wall_ns": wall, "command": command, "rio_report": raw, "setup": setup,
            "stdout": str(stdout), "stderr": str(stderr)}


def enable_native_rust(env):
    # Encoded flags take precedence over RUSTFLAGS. Preserve their argument
    # boundaries and any caller flags while selecting the host CPU explicitly.
    encoded = env.get("CARGO_ENCODED_RUSTFLAGS")
    flags = (encoded.split("\x1f") if encoded else []) if encoded is not None else shlex.split(env.get("RUSTFLAGS", ""))
    env["CARGO_ENCODED_RUSTFLAGS"] = "\x1f".join([*flags, "-C", "target-cpu=native"])


def build_engines(args, directory, report, worktrees):
    cache = args.cache_dir.resolve()
    cache.mkdir(parents=True, exist_ok=True)
    ghost_repo = cache / "ghostty.git"
    if not ghost_repo.exists():
        logged(["git", "init", "--bare", ghost_repo], ROOT, directory / "ghostty-init.log")
    # Fetch just the official default-branch tip; local Ghostty branch state and
    # a stale origin/HEAD symbolic ref must not choose the revision being tested.
    logged(["git", "-C", ghost_repo, "fetch", "--depth=1", GHOSTTY_URL, "HEAD"],
           ROOT, directory / "ghostty-fetch.log")
    ghost_commit = git(ghost_repo, "rev-parse", "FETCH_HEAD^{commit}")
    report["engines"]["ghostty"] = {"ref": GHOSTTY_URL + " HEAD", "commit": ghost_commit}
    driver = report["driver"]["commit"]
    for engine in ENGINES:
        metadata = report["engines"][engine]
        repo = ghost_repo if engine == "ghostty" else ROOT
        tree = cache / f"{directory.name}-{engine}"
        logged(["git", "-C", repo, "worktree", "add", "--detach", tree, metadata["commit"]],
               ROOT, directory / f"{engine}-worktree.log")
        worktrees.append((repo, tree))
        evidence = directory / "sources" / engine
        evidence.mkdir(parents=True)
        metadata["tree"] = git(repo, "rev-parse", metadata["commit"] + "^{tree}")
        metadata["patches"] = []
        if engine == "ghostty":
            metadata["operation_contracts"] = check_native_contract(tree)
            patches = ("ghostty-timing.patch", "ghostty-history.patch", "ghostty-unicode-retry.patch",
                       "ghostty-resize-history.patch")
            metadata["patch_purpose"] = (
                "Expose internal timing; match stream and resize history to 10000 lines; "
                "retry unconsumed UTF-8 bytes in Unicode probes. This is a patched Ghostty benchmark.")
        else:
            metadata["harness"] = overlay_harness(ROOT, driver, tree, evidence)
            patches = ("upstream-harness.patch",) if engine == "upstream" else ()
            metadata["patch_purpose"] = "Register the common harness in the workspace/lockfile; core code is unchanged."
        for patch in patches:
            metadata["patches"].append(apply_adapter(tree, HERE / patch, evidence))
        diff = subprocess.check_output(["git", "-C", str(tree), "diff", "--binary", "HEAD"])
        (evidence / "effective.patch").write_bytes(diff)
        metadata["effective_diff_sha256"] = digest(diff)
        metadata["worktree_status"] = git(tree, "status", "--porcelain", "--untracked-files=all")
        env = dict(os.environ)
        if engine == "ghostty":
            prefix = cache / "ghostty-install"
            command = ["zig", "build", "-Demit-bench", "-Doptimize=ReleaseFast",
                       "-Dapp-runtime=none", "-Demit-exe=false",
                       "-Dcpu=native" if args.native else "-Dcpu=baseline",
                       "--cache-dir", str(cache / "zig-local"), "--prefix", str(prefix)]
            source_binary = prefix / "bin/ghostty-bench"
            metadata["toolchain"] = subprocess.check_output(["zig", "version"], text=True).strip()
        else:
            if args.native:
                enable_native_rust(env)
            target = cache / "targets" / engine
            env["CARGO_TARGET_DIR"] = str(target)
            command = ["cargo", "build", "--release", "--locked", "-p", "ghostty-bench"]
            source_binary = target / "release/ghostty-bench"
            metadata["toolchain"] = subprocess.check_output(["rustc", "--version"], cwd=tree, text=True).strip()
        metadata["build_command"] = command
        metadata["codegen_target"] = "native" if args.native else "baseline"
        # Capture only build-affecting variables, never credentials or all env.
        metadata["build_environment"] = {k: v for k, v in env.items() if k in (
            "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "RUSTUP_TOOLCHAIN", "CC", "CXX",
            "CARGO_BUILD_TARGET", "CARGO_BUILD_RUSTFLAGS", "ZIG_GLOBAL_CACHE_DIR",
        ) or k.startswith("CARGO_PROFILE_") or k == "CARGO_TARGET_DIR"}
        save(directory, report)
        logged(command, tree, directory / f"{engine}-build.log", env)
        binary = directory / f"{engine}-bench"
        shutil.copy2(source_binary, binary)
        metadata["binary"] = {"path": str(binary), "sha256": digest(binary.read_bytes()),
                              "bytes": binary.stat().st_size}
        save(directory, report)


def measure(args, directory, report, cases):
    for case in cases:
        entry = dict(case, samples={e: [] for e in ENGINES}, warmups={e: [] for e in ENGINES}, order=[])
        report["results"].append(entry)
        print("Measure:", case["case"], flush=True)
        try:
            for phase, count in (("warmups", args.warmups), ("samples", args.samples)):
                for index in range(count):
                    order = ENGINES[index % 3:] + ENGINES[:index % 3]
                    entry["order"].append({"phase": phase, "index": index, "engines": order})
                    for engine in order:
                        binary = Path(report["engines"][engine]["binary"]["path"])
                        value = sample(engine, case, binary,
                                       directory / "samples" / case["case"] / engine,
                                       f"{phase}-{index}", report["cpu"], args.timeout)
                        if engine != "ghostty" and value["rio_report"]["rio_revision"] != report["engines"][engine]["commit"]:
                            raise ValueError(f"{engine} binary revision does not match the selected branch tip")
                        entry[phase][engine].append(value)
                    if case["action"] == "terminal-resize" and report.get("require_matched_resize_setup"):
                        states = [entry[phase][e][-1].get("setup") for e in ENGINES]
                        if not all(states) or any(s != states[0] for s in states[1:]):
                            raise ValueError(f"Resize setup differs across engines: {states}")
                    save(directory, report)
            entry["median_ns"] = {e: statistics.median(s["ns_per_operation"] for s in values)
                                   for e, values in entry["samples"].items()}
            entry["river_speedup"] = {e: entry["median_ns"][e] / entry["median_ns"]["river-raptor"]
                                      for e in ("upstream", "ghostty")}
            entry["spread_ns"] = {e: {"min": min(s["ns_per_operation"] for s in v),
                                      "max": max(s["ns_per_operation"] for s in v)}
                                    for e, v in entry["samples"].items()}
            for engine in ENGINES[:2]:
                checksums = {json.dumps(s["rio_report"]["checksum"], sort_keys=True)
                             for s in entry["samples"][engine]}
                if len(checksums) != 1:
                    raise ValueError(f"Unstable per-engine checksum: {engine}")
        except (RuntimeError, subprocess.TimeoutExpired, ValueError, KeyError) as error:
            entry.pop("median_ns", None)
            entry.pop("river_speedup", None)
            entry["error"] = str(error)
            report["failures"].append({"case": case["case"], "error": str(error)})
            print("FAILED:", error, flush=True)
        save(directory, report)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--results-dir", type=Path, default=ROOT / "bench-results")
    parser.add_argument("--cache-dir", type=Path, default=ROOT / "bench-cache")
    parser.add_argument("--samples", type=int, default=9)
    parser.add_argument("--warmups", type=int, default=3)
    parser.add_argument("--bytes", type=int, default=1024 * 1024, help="Short corpus target size")
    parser.add_argument("--stream-bytes", type=int, default=16 * 1024 * 1024)
    parser.add_argument("--seed", type=int, default=1)
    parser.add_argument("--filter", default="", help="Case name substring; all 53 cases by default")
    parser.add_argument("--cpu", type=int, help="Allowed logical CPU; default: first allowed CPU on Linux")
    parser.add_argument("--timeout", type=float, default=120, help="Seconds per sample process")
    parser.add_argument("--keep-worktrees", action="store_true")
    parser.add_argument(
        "--native", action="store_true",
        help="Use host CPU features for both Rust release and Zig ReleaseFast builds; "
             "omit for the portable baseline (inherited build flags are recorded)",
    )
    args = parser.parse_args(argv)
    if (min(args.samples, args.bytes, args.stream_bytes) < 1 or args.warmups < 0
            or not math.isfinite(args.timeout) or args.timeout <= 0):
        parser.error("Sizes/samples/timeout must be positive; warmups must be nonnegative")
    allowed = sorted(os.sched_getaffinity(0)) if hasattr(os, "sched_getaffinity") else []
    cpu = args.cpu if args.cpu is not None else (allowed[0] if allowed else None)
    if cpu is not None and cpu not in allowed:
        parser.error(f"CPU {cpu} is not in the allowed affinity set {allowed}")
    if not any(args.filter in c[0] for c in suite.cases()):
        parser.error("Filter selects no cases")
    directory = new_run(args.results_dir.resolve())
    report = {"schema_version": 1, "id": directory.name, "status": "preparing",
              "started_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
              "driver": {}, "engines": {}, "options": {k: str(v) if isinstance(v, Path) else v
                                                          for k, v in vars(args).items()},
              "host": {"platform": platform.platform(), "machine": platform.machine(),
                       "python": platform.python_version(), "allowed_cpus": allowed},
              "cpu": cpu, "results": [], "failures": [], "require_matched_resize_setup": True,
              "limits": "Descriptive native microbenchmark costs, not semantic equivalence. "
                        "Native Ghostty has no result checksum. Noop controls have different work. "
                        "See per-case notes and applied adapters; no global win score is computed."}
    worktrees = []
    lock = args.cache_dir.resolve() / "run.lock"
    locked = False
    print("Results:", directory, flush=True)
    try:
        lock.parent.mkdir(parents=True, exist_ok=True)
        try:
            lock.mkdir()
        except FileExistsError:
            raise RuntimeError(f"Benchmark cache is locked: {lock}. Another run may be active; "
                               "remove this directory only after verifying its owner has stopped.")
        locked = True
        (lock / "owner.json").write_text(json.dumps({"pid": os.getpid(), "run": str(directory)}))
        report["driver"] = {"commit": git(ROOT, "rev-parse", "HEAD"),
                             "status": git(ROOT, "status", "--porcelain", "--untracked-files=all")}
        # Preserve the actual invoked driver as well as its committed harness:
        # dirty local tooling must not masquerade as the recorded driver commit.
        driver_dir = directory / "driver"
        driver_dir.mkdir()
        hashes = {}
        for path in HERE.iterdir():
            if path.is_file():
                shutil.copy2(path, driver_dir / path.name)
                hashes[path.name] = digest(path.read_bytes())
        report["driver"]["file_sha256"] = hashes
        report["engines"] = resolve_branches(ROOT)
        if Path("/proc/cpuinfo").exists():
            model = re.search(r"^model name\s*:\s*(.+)$", Path("/proc/cpuinfo").read_text(), re.M)
            report["host"]["cpu_model"] = model[1] if model else "unknown"
        if cpu is not None:
            governor = Path(f"/sys/devices/system/cpu/cpu{cpu}/cpufreq/scaling_governor")
            report["host"]["cpu_governor"] = governor.read_text().strip() if governor.exists() else "unknown"
        save(directory, report)
        short, long, report["corpora"] = prepare_data(directory, args.bytes, args.stream_bytes, args.seed)
        cases = [c for c in native.cases(short, long) if args.filter in c["case"]]
        report["selected_cases"] = [c["case"] for c in cases]
        report["status"] = "building"
        save(directory, report)
        build_engines(args, directory, report, worktrees)
        report["status"] = "running"
        save(directory, report)
        measure(args, directory, report, cases)
        report["status"] = "failed" if report["failures"] else "complete"
    except (Exception, KeyboardInterrupt) as error:
        report["status"] = "failed"
        report["error"] = f"{type(error).__name__}: {error}"
        print(report["error"], flush=True)
    finally:
        if not args.keep_worktrees:
            for repo, tree in reversed(worktrees):
                result = subprocess.run(["git", "-C", str(repo), "worktree", "remove", "--force", str(tree)],
                                        capture_output=True, text=True)
                if result.returncode:
                    report.setdefault("cleanup_errors", []).append(result.stderr)
        if locked:
            try:
                shutil.rmtree(lock)
            except OSError as error:
                report.setdefault("cleanup_errors", []).append(str(error))
        report["finished_utc"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
        save(directory, report)
        print("HTML:", directory / "report.html", "\nJSON:", directory / "report.json", flush=True)
    return 0 if report["status"] == "complete" else 1


if __name__ == "__main__":
    raise SystemExit(main())
