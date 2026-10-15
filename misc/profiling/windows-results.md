# Windows profiling results

The Windows tab changes reduce the CPU cost of drawing the tab strip.
Profiling also reproduced a Windows shutdown stall and identified GPU backend
initialization as the largest startup cost.

## Setup

Captures ran in the logged-in Windows desktop session on the NVIDIA RTX A500
Laptop GPU machine. They used the existing Rio configuration, with the shell
overridden to `cmd /d /q /k`. Each comparison uses five sequential launches.
The redraw workloads submit 180 frames after shell readiness. Timings are
medians across the five runs, in milliseconds.

The executable uses the release profile with LTO disabled, 16 codegen units,
debug information disabled, and the `profiling` feature enabled. The build and
capture commands are in [the profiling guide](README.md). Builds finished
before captures started. No reboot or driver-cache purge was performed.
The individual captures retain the slower first launch in each series.

## Tab strip

The baseline is `7d08bce5f9`; the optimization is `e6e889db2e`.

| Workload | Before | After | Reduction |
| --- | ---: | ---: | ---: |
| One tab, median tab-strip render | 0.03750 | 0.03460 | 7.7% |
| Eight tabs, median tab-strip render | 0.08380 | 0.07105 | 15.2% |
| Eight tabs, p95 tab-strip render | 0.12120 | 0.09270 | 23.5% |

Each frame now prepares a tab's title, status, application icon, remote flag,
and bell state once for measurement and drawing. Eight tabs require eight
application metadata reads per frame instead of sixteen. Title baselines are
calculated once per frame. The total glyph-bounds calls fall from twenty to
thirteen per frame for this workload.

Cached shaped text uses shared immutable runs, so measurement, bounds, and
drawing reuse the glyph vector. Explicit font choices skip fallback lookup.
The seven unused fallback lookups during startup disappear in these captures.

The saving is small in absolute time. GPU initialization still dominates
startup, and presentation dominates most warm frame time.

Raw captures on Windows are under `D:\git-repos\rio\target\profiles`:
`baseline-single-fixed-exit`, `baseline-eight-fixed-exit`, `optimized-single`,
and `optimized-eight`.
Both tab baselines used executable SHA-256 prefix `b885713b890a4e25`;
both optimized workloads used `2cdd4f389c86cd8e`. Every capture satisfied
its shell-readiness and frame-count checks.

## Shutdown

The repeated redraw workload sometimes completed every frame and then stayed
alive until the runner's 35-second timeout. An optional watchdog snapshot
showed all shells ready, all 180 frames complete, and no active drawing scope.

`rio-window` called `prepare_wait()`, which dispatches `AboutToWait`, and then
entered the native wait even when that callback requested exit. With control
flow set to `Wait`, shutdown depended on an unrelated Windows message.
Commit `7d08bce5f9` checks the exit flag before the native wait. Five consecutive
eight-tab captures then completed and exited successfully. Subsequent tab and
backend capture series also completed without that stall.

## Backend startup

The later [renderer comparison](windows-renderer-selection.md) adds full terminal
redraws, OpenGL, integrated graphics, and GPU-residency measurements.

| Backend selection | Median shell frame |
| --- | ---: |
| Automatic, optimized one-tab workload | 2169.78 |
| Explicit Vulkan, startup and idle | 1670.99 |
| Explicit DX12, startup and idle | 1568.09 |

The automatic one-tab capture spends about 1.6 seconds in GPU context creation.
Instance creation, adapter discovery, and device creation account for most of
that time. Explicit backend selection avoids work for other graphics APIs on
this machine. Backend choice also affects presentation pacing and transparency,
so the default selection policy is retained.

The old `primary` alias in a source comment failed surface creation with the
pinned wgpu version. The runner offers `dx12`, `vulkan`, and `gl` as explicit
backend overrides.

Raw backend captures are in `backend-vulkan` and `backend-dx12` under the same
Windows profiles directory.

## Additional Windows windows

The two-window baseline is `eb21753de0`; instance reuse is `792fff25c2`.
Each fresh process creates two windows
and waits for both cmd shells. The second-call durations below are calculated
from each capture before taking the median across five runs.

| Second-window stage | Before | After | Reduction |
| --- | ---: | ---: | ---: |
| GPU instance creation | 44.0968 | 0.0026 | about 44 ms removed |
| GPU context setup | 230.6544 | 182.0528 | 21.1% |
| Sugarloaf initialization | 233.3144 | 185.6931 | 20.4% |
| Screen initialization | 368.1757 | 301.2753 | 18.2% |

Windows now reuses a GPU instance for the same requested backend set. Adapter,
device, surface, colorspace, and transparency decisions remain per window.
The first window still pays for driver initialization. The cache retains the
instance for the process lifetime.

Raw captures are in `baseline-two-windows` and `optimized-two-windows` under
the Windows profiles directory.

The shared-instance path also passed two two-window launches with explicit
Vulkan and two with explicit DX12, captured in `validate-two-windows-vulkan`
and `validate-two-windows-dx12`.

## Final validation

The ten comparison and backend-validation series above contain 44 successful
captures after the shutdown fix. Every capture completed its readiness and
frame-count checks.

Both the profiling executable and the normal `rio.exe` release build compiled
on Windows. The normal executable ran `cmd /d /c echo RIO_NORMAL_BUILD_OK`
and exited with code zero. It created no profiling output even when
`RIO_PROFILE_OUTPUT` was set, confirming that the normal build excludes the
profiling behavior.

The resulting executables are `D:\git-repos\rio\target\release\rio.exe` and
`D:\git-repos\rio\target\release\rio-profile.exe`.

## Measurement limits

These are instrumented CPU timings, including waits inside presentation.
They do not measure GPU execution or visually verify the interface. Timing
scopes nest, so their totals overlap. Configuration, font installation, GPU
driver state, and the first launch affect startup results.
