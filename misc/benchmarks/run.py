#!/usr/bin/env python3
"""Build identical fixtures against upstream and the branch, then run paired benches."""

import argparse
import contextlib
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import shlex
import statistics
import subprocess
import sys
import time

import application
import bash

ROOT = Path(__file__).resolve().parents[2]
BASE = "59203ccd5fa1e6b760a738c1ad653bb29f2f3465"
OUT = ROOT / "target/branch-performance"
UPSTREAM = OUT / "upstream"
SUITES = {
    "vt": ("rio-vt", "branch_performance"),
    "grid": ("rio-grid", "branch_performance"),
    "ui": ("sugarloaf", "branch_performance"),
    "gpu": ("sugarloaf", "branch_gpu"),
    "wgpu": ("sugarloaf", "branch_wgpu"),
    "icons": ("sugarloaf", "branch_features"),
    "shader": ("rioterm", "branch_shader"),
    "vt_internal": ("rio-vt", "vt_input"),
    "poll_internal": ("corcovado", "bench_poll"),
}


def git(*args, cwd=ROOT):
    return subprocess.check_output(["git", "-c", "log.showSignature=false", *args], cwd=cwd, text=True).strip()


def replace_once(path, old, new):
    text = path.read_text()
    if text.count(old) != 1:
        raise RuntimeError(f"API adapter no longer matches {path}: {old!r}")
    path.write_text(text.replace(old, new))


def append_module(path, filename, depth, module="branch_benchmarks"):
    path.write_text(path.read_text() + f'\n#[cfg(test)]\n#[path = "{"../" * depth}misc/benchmarks/{filename}"]\nmod {module};\n')


def prepare():
    OUT.mkdir(parents=True, exist_ok=True)
    if not UPSTREAM.exists():
        subprocess.run(["git", "worktree", "add", "--detach", str(UPSTREAM), BASE], cwd=ROOT, check=True)
    if git("rev-parse", "HEAD", cwd=UPSTREAM) != BASE:
        raise RuntimeError("The upstream worktree is at a different revision")
    marker = UPSTREAM / ".branch-bench-overlay"
    if marker.exists():
        if marker.read_text().strip() != BASE:
            raise RuntimeError("Unknown upstream benchmark overlay")
        # Only undo the files this tool owns; never reset an arbitrary checkout.
        owned = ["rio-vt/Cargo.toml", "rio-grid/Cargo.toml", "sugarloaf/Cargo.toml", "sugarloaf/src/text.rs",
                 "frontends/rioterm/src/main.rs", "frontends/rioterm/src/context/mod.rs",
                 "frontends/rioterm/src/renderer/command_palette.rs", "frontends/rioterm/src/renderer/search.rs"]
        subprocess.run(["git", "restore", "--source=HEAD", "--", *owned], cwd=UPSTREAM, check=True)
    elif git("status", "--porcelain", cwd=UPSTREAM):
        raise RuntimeError("The upstream worktree has unrelated changes")
    shutil.copytree(ROOT / "misc/benchmarks", UPSTREAM / "misc/benchmarks", dirs_exist_ok=True)
    for package, names in {
        "rio-vt": ["branch_performance"],
        "rio-grid": ["branch_performance"],
        "sugarloaf": ["branch_performance", "branch_gpu", "branch_wgpu"],
    }.items():
        for name in names:
            target = UPSTREAM / package / "benches" / f"{name}.rs"
            target.parent.mkdir(exist_ok=True)
            shutil.copy2(ROOT / package / "benches" / f"{name}.rs", target)
            with (UPSTREAM / package / "Cargo.toml").open("a") as manifest:
                manifest.write(f'\n[[bench]]\nname = "{name}"\nharness = false\n')
                if name == "branch_wgpu":
                    manifest.write('required-features = ["wgpu"]\n')
        if package == "rio-grid":
            with (UPSTREAM / package / "Cargo.toml").open("a") as manifest:
                manifest.write('\n[target.\'cfg(target_os = "linux")\'.dev-dependencies]\nrio-window = { workspace = true, features = ["x11"] }\n')
    # The existing VT bench gets the same Unicode fixtures on both revisions.
    shutil.copy2(ROOT / "rio-vt/benches/vt_input.rs", UPSTREAM / "rio-vt/benches/vt_input.rs")
    append_module(UPSTREAM / "frontends/rioterm/src/main.rs", "support.rs", 3, "performance")
    append_module(UPSTREAM / "frontends/rioterm/src/context/mod.rs", "frontend_context.rs", 4)
    append_module(UPSTREAM / "frontends/rioterm/src/renderer/command_palette.rs", "frontend_palette.rs", 4)
    append_module(UPSTREAM / "frontends/rioterm/src/renderer/search.rs", "frontend_search.rs", 4)
    append_module(UPSTREAM / "sugarloaf/src/text.rs", "text_cache.rs", 2)
    # These adapt names and ownership at existing production interfaces. They
    # do not copy an old algorithm into the candidate or change production work.
    replace_once(UPSTREAM / "misc/benchmarks/frontend_palette.rs", 'score(black_box("font 00"),', 'fuzzy_score(black_box("font 00"),')
    replace_once(UPSTREAM / "misc/benchmarks/frontend_search.rs", 'Some(value)', 'Some(value.to_owned())')
    # The benchmark-only X11 dependency changes rio-grid's lockfile entry.
    # Preserve the upstream lock's versions while resolving that local edge.
    with (OUT/"upstream-metadata.log").open("w") as log:
        subprocess.run(["cargo","metadata","--offline","--format-version","1"],cwd=UPSTREAM,
                       stdout=subprocess.DEVNULL,stderr=log,check=True)
    marker.write_text(BASE + "\n")


def build_environment():
    env = dict(os.environ, CARGO_INCREMENTAL="0")
    for profile in ["RELEASE", "BENCH"]:
        for key, value in {"LTO": "false", "CODEGEN_UNITS": "16", "DEBUG": "0", "STRIP": "none", "INCREMENTAL": "false"}.items():
            env[f"CARGO_PROFILE_{profile}_{key}"] = value
    return env


def build_one(revision, suite, cwd, env):
    env = dict(env,CARGO_TARGET_DIR=str(OUT/"build"/revision))
    if suite == "bash":
        return shutil.which("bash")
    if suite == "ui_internal":
        command = ["cargo", "test", "--release", "-p", "sugarloaf", "--lib", "--features", "rio-window/x11", "--no-run"]
    elif suite == "frontend":
        command = ["cargo", "test", "--release", "-p", "rioterm", "--bin", "rio", "--no-run"]
    elif suite == "application":
        command = ["cargo", "build", "--release", "-p", "rioterm", "--bin", "rio"]
    else:
        package, name = SUITES[suite]
        command = ["cargo", "bench", "-p", package, "--bench", name, "--no-run"]
        if package in ("sugarloaf", "rio-grid"):
            command += ["--features", "rio-window/x11" + (",wgpu" if suite == "wgpu" else "")]
    command += ["--locked", "--message-format=json"]
    log = OUT / f"build-{revision}-{suite}.log"
    print(f"Building {revision} {suite}", flush=True)
    with log.open("w") as output:
        process = subprocess.run(command, cwd=cwd, env=env, stdout=output, stderr=subprocess.STDOUT)
    if process.returncode:
        raise RuntimeError(f"Build failed; see {log}")
    artifacts = []
    for line in log.read_text().splitlines():
        try:
            row = json.loads(line)
        except json.JSONDecodeError:
            continue
        if row.get("reason") == "compiler-artifact" and row.get("executable"):
            target = row["target"]
            if suite == "ui_internal" and row["profile"]["test"] and target["name"] == "sugarloaf":
                artifacts.append(row["executable"])
            elif suite == "frontend" and row["profile"]["test"] and target["name"] == "rio":
                artifacts.append(row["executable"])
            elif suite == "application" and not row["profile"]["test"] and target["name"] == "rio":
                artifacts.append(row["executable"])
            elif suite not in ("frontend", "ui_internal", "application") and target["name"] == SUITES[suite][1]:
                artifacts.append(row["executable"])
    if len(artifacts) != 1:
        raise RuntimeError(f"Expected one executable for {suite}, got {artifacts}")
    destination = OUT / "binaries" / revision / suite
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(artifacts[0], destination)
    return str(destination)


def build(args):
    prepare()
    env = build_environment()
    binaries = {}
    common = ["vt", "grid", "ui", "ui_internal", "frontend", "vt_internal", "poll_internal", "bash", "application"]
    if not args.no_gpu:
        common += ["gpu", "wgpu"]
    for revision, cwd in [("upstream", UPSTREAM), ("branch", ROOT)]:
        suites = common + (["icons", "shader"] if revision == "branch" else [])
        if args.no_gpu:
            suites = [suite for suite in suites if suite != "shader"]
        binaries[revision] = {suite: build_one(revision, suite, cwd, env) for suite in suites}
    metadata = {
        "baseline": BASE, "branch": git("rev-parse", "HEAD"), "binaries": binaries,
        "rustc": subprocess.check_output(["rustc", "-Vv"], text=True),
        "cargo": subprocess.check_output(["cargo", "--version"], text=True).strip(),
        "profile": {key: value for key,value in env.items() if key.startswith("CARGO_PROFILE_")},
        "fixtures_sha256": hashlib.sha256(b"".join(path.read_bytes() for path in sorted((ROOT/"misc/benchmarks").glob("*.rs")))).hexdigest(),
    }
    (OUT / "build.json").write_text(json.dumps(metadata, indent=2) + "\n")


@contextlib.contextmanager
def display():
    runtime = OUT / "runtime"
    runtime.mkdir(mode=0o700, exist_ok=True)
    session = OUT / "display.json"
    session.unlink(missing_ok=True)
    env = dict(os.environ, XDG_RUNTIME_DIR=str(runtime), QT_QPA_PLATFORM="offscreen", RIO_BENCH_DISPLAY_FILE=str(session))
    env.pop("DISPLAY", None)
    env.pop("WAYLAND_DISPLAY", None)
    command = ["dbus-run-session", "--", "kwin_wayland", "--virtual", "--xwayland", "--no-lockscreen",
               "--width", "1600", "--height", "1000", "--exit-with-session", shlex.join([sys.executable,str(ROOT/'misc/benchmarks/display.py')])]
    with (OUT / "display.log").open("w") as log:
        process = subprocess.Popen(command, env=env, stdout=log, stderr=log, start_new_session=True)
        try:
            deadline = time.monotonic() + 30
            while not session.exists():
                if process.poll() is not None or time.monotonic() > deadline:
                    raise RuntimeError("Isolated display failed; see display.log")
                time.sleep(0.1)
            result = json.loads(session.read_text())
            result.pop("WAYLAND_DISPLAY", None)  # All display fixtures use X11.
            yield result
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()


def records(text):
    for line in text.splitlines():
        if line.startswith("RIO_") and " {" in line:
            kind, value = line.split(" ", 1)
            yield {"kind": kind.removeprefix("RIO_").lower(), **json.loads(value)}


def execute(path, arguments, env, cpu, log):
    def pin():
        os.sched_setaffinity(0, {cpu})
    with log.open("w") as output:
        process = subprocess.run([path, *arguments], cwd=ROOT, env=env, stdout=output, stderr=subprocess.STDOUT,
                                 preexec_fn=pin, timeout=300)
    if process.returncode:
        raise RuntimeError(f"Benchmark failed ({process.returncode}); see {log}")
    return list(records(log.read_text()))


def run(args):
    if git("status", "--porcelain"):
        raise RuntimeError("Commit the benchmark suite and document before measuring")
    metadata = json.loads((OUT / "build.json").read_text())
    if metadata["branch"] != git("rev-parse", "HEAD"):
        raise RuntimeError("Rebuild benchmarks at the committed branch revision")
    if args.cpu not in os.sched_getaffinity(0):
        raise RuntimeError(f"CPU {args.cpu} is unavailable")
    env = dict(os.environ, RIO_BENCH_SAMPLES=str(args.samples), RIO_BENCH_MS=str(args.ms),
               RIO_BENCH_BASELINE=str(UPSTREAM), CARGO_TARGET_DIR=str(ROOT / "target"))
    if args.filter:
        env["RIO_BENCH_FILTER"] = args.filter
    manager = contextlib.nullcontext({}) if args.no_gpu else display()
    with manager as display_env, (OUT / "results.jsonl").open("w") as output:
        env.update(display_env)
        suites = list(dict.fromkeys(suite for binaries in metadata["binaries"].values() for suite in binaries))
        for repeat in range(args.runs):
            order = ["upstream", "branch"] if repeat % 2 == 0 else ["branch", "upstream"]
            for suite in suites:
                for revision in order:
                    binary = metadata["binaries"][revision].get(suite)
                    if binary is None:
                        continue
                    if suite == "bash":
                        if args.filter and args.filter not in "bash/command_loop":
                            continue
                        rows = bash.run(binary,ROOT/"frontends/rioterm/src/shell_integration/rio.bash",revision=="branch",env,args.cpu,args.samples)
                        for row in rows:
                            output.write(json.dumps({"revision":revision,"repeat":repeat,"suite":suite,**row})+"\n")
                        output.flush()
                        continue
                    if suite == "application":
                        if args.no_gpu:
                            continue
                        for mode in ["dense","sparse","scroll","cursor","unicode","search","idle","tabs","tab_repeat","palette"]:
                            name = f"application/{mode}"
                            if args.filter and args.filter not in name:
                                continue
                            print(f"Running pair {repeat+1}/{args.runs}: {revision} {name}", flush=True)
                            row = application.run(binary,mode,OUT/"application"/f"{repeat}-{revision}-{mode}",env,args.cpu)
                            output.write(json.dumps({"revision":revision,"repeat":repeat,"suite":suite,**row})+"\n")
                            output.flush()
                        continue
                    if args.no_gpu and suite in ("gpu", "wgpu", "shader"):
                        continue
                    print(f"Running pair {repeat+1}/{args.runs}: {revision} {suite}", flush=True)
                    arguments = ["branch_bench", "--ignored", "--nocapture", "--test-threads=1"] if suite in ("frontend","ui_internal") else []
                    if suite in ("vt_internal","poll_internal"):
                        arguments = ["--bench", "--noplot", "--warm-up-time", "0.2", "--measurement-time", "0.5", "--save-baseline", f"{revision}-{repeat}"]
                    log = OUT / f"run-{repeat}-{revision}-{suite}.log"
                    rows = execute(binary, arguments, env, args.cpu, log)
                    if suite in ("vt_internal","poll_internal"):
                        for estimate in (ROOT/"target/criterion").glob(f"**/{revision}-{repeat}/estimates.json"):
                            if (suite == "vt_internal") != ("vt_input" in estimate.parts):
                                continue
                            values = json.loads(estimate.read_text())
                            name = "/".join(estimate.relative_to(ROOT/"target/criterion").parts[:-2])
                            rows.append({"kind":"bench", "case":f"internal/{name}", "sample":0, "ns":values["median"]["point_estimate"]})
                    if not rows:
                        raise RuntimeError(f"No benchmark records in {log}")
                    for row in rows:
                        row_revision = ["upstream", "branch"][row["variant"]] if suite == "shader" else revision
                        output.write(json.dumps({"revision":row_revision,"repeat":repeat,"suite":suite,**row})+"\n")
                    output.flush()
    (OUT / "run.json").write_text(json.dumps({"runs":args.runs,"samples":args.samples,"sample_ms":args.ms,"cpu":args.cpu,"platform":sys.platform,"display":"isolated KWin Xwayland" if not args.no_gpu else None}, indent=2)+"\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["prepare", "build", "run"])
    parser.add_argument("--no-gpu", action="store_true")
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--samples", type=int, default=11)
    parser.add_argument("--ms", type=int, default=100)
    parser.add_argument("--cpu", type=int, default=2)
    parser.add_argument("--filter")
    args = parser.parse_args()
    {"prepare":prepare, "build":lambda:build(args), "run":lambda:run(args)}[args.action]()


if __name__ == "__main__":
    main()
