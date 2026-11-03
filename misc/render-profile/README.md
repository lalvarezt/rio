# Profile the rendering pipeline

Build the opt-in executable with symbols. The regular `rio` binary does not collect timings.

```sh
CARGO_PROFILE_RELEASE_STRIP=none cargo build --release --locked -p rioterm --features profiling --bin rio-profile
```

On Linux, use an isolated virtual Wayland output with `kwin_wayland`, `dbus-run-session`, Node.js, and a Vulkan-capable GPU. This command creates and stops the compositor without changing the desktop session.

```sh
node misc/render-profile/virtual.cjs /tmp/rio-profile-ascii target/release/rio-profile ascii 3
```

Choose `cached`, `full`, `ascii`, or `unicode`. `cached` repeatedly presents unchanged content. `full` forces terminal damage without changing the text. `ascii` and `unicode` rewrite the viewport every 16 ms using synchronized output.

The default capture requests 360 frames and excludes the first 60 ready frames from `steady_metrics`. The original `metrics` object still includes startup and warmup. Configure the counts with `RIO_PROFILE_FRAMES` and `RIO_PROFILE_WARMUP_FRAMES`. Keep the requested duration below the executable's existing 20-second watchdog.

To use an existing display, run `run.cjs` with the binary first and the output directory second. Set `WAYLAND_DISPLAY` and `XDG_RUNTIME_DIR`, or `DISPLAY`, for that session. Xvfb is an automatic fallback when neither display variable is set, but it cannot present through this machine's Intel Vulkan driver because it lacks DRI3.

Collect a longer CPU profile with Linux `perf`. The recorder starts sampling after 1.5 seconds and retains DWARF call chains. Use a fresh output directory for each command.

```sh
RIO_PROFILE_FRAMES=900 node misc/render-profile/virtual.cjs /tmp/rio-profile-cpu-ascii target/release/rio-profile ascii 1 --perf
node misc/render-profile/analyze-perf.cjs /tmp/cpu-summary.json /tmp/rio-profile-cpu-ascii
```

`analyze-perf.cjs` reports self cycles for the `rio-profile` main thread. It excludes the PTY parser and Node.js workload processes. It weights hybrid CPU events by their observed sample periods and disables network symbol lookup. Native driver and libc symbols may remain unresolved.

Combine timing captures and regenerate the chart with these commands. Pass additional capture directories after the first one to include more scenarios.

```sh
node misc/render-profile/summarize.cjs /tmp/baseline.json /tmp/rio-profile-ascii
node misc/render-profile/chart.cjs /tmp/baseline.json /tmp/rendering-profile.html
```

The summarizer rejects incomplete captures, missing stage samples, and invalid timing nesting. It combines each statistic using the median across runs. It does not compute a pooled percentile.

The stage timers measure main-thread wall time. `gpu.acquire` includes the fence wait, swapchain acquisition, and command-buffer reset. `gpu.upload` measures CPU preparation and command recording, while `gpu.encode` measures render-command recording. GPU execution needs timestamp queries and is outside this capture.

Compare two timing summaries with:

```sh
node misc/render-profile/compare.cjs /tmp/before.json /tmp/after.json /tmp/foreground-comparison
```
