"""Run sequential cmd startup captures. Uses only the Python standard library."""
import argparse
import hashlib
import json
import os
import platform
from pathlib import Path
import statistics
import subprocess
import time


def process_cpu_ms(process):
    if os.name != "nt":
        return None
    import ctypes
    from ctypes import wintypes
    get_times = ctypes.WinDLL("kernel32", use_last_error=True).GetProcessTimes
    get_times.argtypes = [wintypes.HANDLE] + [ctypes.POINTER(wintypes.FILETIME)] * 4
    get_times.restype = wintypes.BOOL
    created, exited, kernel, user = (wintypes.FILETIME() for _ in range(4))
    if not get_times(int(process._handle), ctypes.byref(created), ctypes.byref(exited),
                     ctypes.byref(kernel), ctypes.byref(user)):
        raise ctypes.WinError(ctypes.get_last_error())
    ticks = lambda value: (value.dwHighDateTime << 32) | value.dwLowDateTime
    return (ticks(kernel) + ticks(user)) / 10_000


parser = argparse.ArgumentParser()
parser.add_argument("executable", type=Path)
parser.add_argument("--runs", type=int, default=5)
parser.add_argument("--frames", type=int, default=0)
parser.add_argument("--tabs", type=int, default=1)
parser.add_argument("--windows", type=int, default=1)
parser.add_argument("--backend", choices=("dx12", "vulkan", "gl"))
parser.add_argument("--full-redraw", action="store_true",
                    help="fill the terminal with text and rebuild every visible row each frame")
parser.add_argument("--env", action="append", default=[], metavar="NAME=VALUE",
                    help="override an environment variable for the profiled process")
parser.add_argument("--output", type=Path, default=Path("target/profiles/baseline"))
args = parser.parse_args()
if args.runs < 1 or args.frames < 0 or not 1 <= args.tabs <= 16 or not 1 <= args.windows <= 4:
    parser.error("runs must be positive, frames nonnegative, tabs 1 through 16, and windows 1 through 4")
overrides = {}
for assignment in args.env:
    name, separator, value = assignment.partition("=")
    if not separator or not name:
        parser.error("--env requires NAME=VALUE")
    overrides[name] = value
args.output.mkdir(parents=True, exist_ok=True)
captures = []
for index in range(args.runs):
    report = (args.output / f"run-{index + 1}.json").resolve()
    env = dict(os.environ, RIO_PROFILE_OUTPUT=str(report),
               RIO_PROFILE_FRAMES=str(args.frames), RIO_PROFILE_TABS=str(args.tabs),
               RIO_PROFILE_WINDOWS=str(args.windows))
    if args.backend:
        env["WGPU_BACKEND"] = args.backend
    if args.full_redraw:
        env["RIO_PROFILE_FULL_REDRAW"] = "1"
    env.update(overrides)
    start = time.perf_counter()
    with (args.output / f"run-{index + 1}.log").open("w") as log:
        with subprocess.Popen([str(args.executable.resolve())], env=env,
                              stdout=log, stderr=log) as process:
            try:
                returncode = process.wait(timeout=35)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
                raise
            cpu_ms = process_cpu_ms(process)
    if returncode:
        raise RuntimeError(f"Capture failed: exit {returncode}, see {log.name}")
    capture = json.loads(report.read_text())
    if capture["shell_ready_ms"] is None or capture["first_frame_ms"] is None or capture.get("shell_frame_ms", 0) is None:
        raise RuntimeError(f"Shell or renderer did not become ready: {report}")
    if capture.get("frames", 0) < capture.get("target_frames", args.frames):
        raise RuntimeError(f"Frame workload timed out: {report}")
    if capture.get("ready_tabs") != args.tabs * args.windows or capture.get("target_tabs") != args.tabs * args.windows:
        raise RuntimeError(f"Requested tab workload did not become ready: {report}")
    capture["process_ms"] = (time.perf_counter() - start) * 1000
    capture["process_cpu_ms"] = cpu_ms
    report.write_text(json.dumps(capture, indent=2) + "\n")
    captures.append(capture)
    print(f"run {index + 1}: ready {capture['shell_ready_ms']:.1f} ms, "
          f"first frame {capture['first_frame_ms']:.1f} ms", flush=True)

summary = {key: statistics.median(c[key] for c in captures)
           for key in ("shell_ready_ms", "first_frame_ms", "process_ms", "shell_frame_ms")
           if key in captures[0]}
if captures[0].get("process_cpu_ms") is not None:
    summary["process_cpu_ms"] = statistics.median(c["process_cpu_ms"] for c in captures)
summary["metrics"] = {
    name: {key: statistics.median(c["metrics"][name][key] for c in captures)
           for key in captures[0]["metrics"][name]}
    for name in captures[0]["metrics"]
}
with args.executable.open("rb") as executable:
    executable_hash = hashlib.file_digest(executable, "sha256").hexdigest()
summary["capture"] = {
    "runs": args.runs,
    "tabs": args.tabs,
    "windows": args.windows,
    "frames": args.frames,
    "full_redraw": args.full_redraw,
    "backend_override": args.backend,
    "environment_overrides": overrides,
    "platform": platform.platform(),
    "executable_sha256": executable_hash,
}
(args.output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
print(json.dumps(summary, indent=2))
