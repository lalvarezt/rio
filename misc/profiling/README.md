# Profile Rio on Windows

Build the profiling executable from a Windows terminal:

```cmd
set CARGO_PROFILE_RELEASE_LTO=false
set CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
set CARGO_PROFILE_RELEASE_DEBUG=0
cargo build -p rioterm --release --bin rio-profile --features profiling
```

Run captures in your logged-in desktop session. The Windows OpenSSH session
does not provide the same graphics environment.

```cmd
python misc\profiling\run-windows.py target\release\rio-profile.exe --runs 5 --output target\profiles\startup
python misc\profiling\run-windows.py target\release\rio-profile.exe --runs 5 --frames 180 --tabs 8 --output target\profiles\eight-tabs
python misc\profiling\run-windows.py target\release\rio-profile.exe --runs 5 --windows 2 --output target\profiles\two-windows
python misc\profiling\run-windows.py target\release\rio-profile.exe --runs 5 --frames 600 --tabs 8 --full-redraw --backend vulkan --output target\profiles\vulkan-text
```

Python 3 is the only runner dependency. Each launch uses your Rio configuration,
overrides the shell with `cmd /d /q /k`, and keeps the tab strip visible.
The runner never changes your configuration file. Stop builds and other heavy
work before collecting timings.

The executable waits for all requested shells to emit a title marker and for
the renderer to submit a frame. It records two seconds after readiness and
exits after the requested frame workload completes. A capture fails if a shell
does not become ready or the workload exceeds the time limit.

`--frames 0` records startup and idle behavior. A positive frame count requests
overlay redraws through the normal event loop and presentation path. `--tabs`
accepts 1 through 16 per window. `--windows` accepts 1 through 4 and waits for
every shell in every window. `--backend dx12` and `--backend vulkan` set `WGPU_BACKEND`
for the child process, overriding backend selection for that capture.
`--full-redraw` fills the terminal with ASCII text before readiness, then rebuilds
all visible rows on each requested frame. This measures terminal row processing
in addition to overlay drawing. It does not generate continuous PTY traffic.
`--env NAME=VALUE` adds a process-local environment override and can be repeated.
The summary records these overrides so driver-filtering experiments are repeatable.

The output directory contains each run's JSON capture, its console log, and
`summary.json`. The summary reports medians across runs. Each timing contains
the call count, total, median, 95th percentile, maximum, and first-call duration,
in milliseconds. Spans nest, so their totals overlap. Frame submission is a CPU
timing and includes any wait inside presentation; it does not measure GPU
execution time.
`frame.interval` measures time between successfully submitted frames after shell
readiness. `gpu.acquire`, `gpu.submit`, and `gpu.present` separate surface acquisition,
queue submission, and presentation. These are CPU wall-clock scopes, including
driver waits. They do not measure display latency or GPU execution time.
On Windows, `process_cpu_ms` reports the Rio process's kernel and user CPU time
from `GetProcessTimes`. It excludes child shells and distinguishes CPU work from
time spent waiting. The runner also records wall-clock `process_ms` in each capture.

Set `RIO_PROFILE_WATCHDOG=1` to save an additional `run-N.watchdog.json`
snapshot after ten seconds. It includes active timing scopes and completed
work, even if the main thread stops progressing. Leave it unset for timing
comparisons.

The `profiling` feature enables instrumentation and automatic exit. Leave it
off for normal builds. Timings stay in memory until exit. The targeted
`rio_profile` tracing spans in Sugarloaf are collected only by the profiling
subscriber, unless another subscriber explicitly enables them.

To launch through Task Scheduler from SSH, create an interactive task that calls
`misc\profiling\run-session.cmd` with the same arguments as the Python runner.
The wrapper changes to the repository directory and writes its exit code to
`target\rio-profile-run.exitcode`. Use a task name specific to profiling and
delete that task when finished.

Native transparency and close-transition measurements are recorded in
[Windows composition captures](windows-composition.md).
