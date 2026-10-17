# Windows branch performance report

Upstream: `59203ccd5fa1e6b760a738c1ad653bb29f2f3465`. Branch executable revision: `eca0102589caf9b8f34cbc6434944d858126be5e`.
Benchmark collection revision: `ddd0555d1e039741806192b1c4695c916e1473b4`. Executable revisions and the compiled fixture hash are recorded separately.
This report compares the recorded branch revision with upstream, not with intermediate optimization commits.

## Measurement environment

- System: Windows-11-10.0.22631-SP0; 13th Gen Intel(R) Core(TM) i7-1370P.
- Toolchain: rustc 1.96.1 (31fca3adb 2026-06-26); cargo 1.96.1 (356927216 2026-06-26).
- CPU affinity: logical CPU 2. Process priority: Normal. No power policy was changed.
- GPU adapters reported by the probes: NVIDIA RTX A500 Laptop GPU.
- 3 alternating executable pairs; 11 CPU samples per pair and case.
- CPU samples target 100 ms. Allocation counting uses a separate warmed pass.
- Matching optimized profiles in separate build directories: LTO disabled, 16 codegen units, debug information and incremental compilation disabled, no stripping.
- Native application replays use the interactive Windows desktop.
- Builds finish before timing. The suite includes the existing VT and poll Criterion benchmarks.

## Reading the results

A negative change means less time. The paired range shows the smallest and largest time change across runs.
A case is labeled faster or slower only when every pair changes in the same direction by more than 2%.
This classification is a descriptive noise guard, not a confidence interval or a statistical significance test.
New features with no equivalent upstream work are cost measurements. Their relative timings do not establish a feature-preserving speedup.
The results cannot be added into a single terminal speedup: the cases measure different operations and overlapping pipeline stages.

The complete per-case timing and allocation results are in [the CSV](windows-branch-performance-report.csv).

Collection provenance: kernels pairs 1, 2, 3 with driver `eca0102589caf9b8f34cbc6434944d858126be5e`; applications pairs 1, 2, 3 with driver `a62f9a1f541edfbde27f0aa21689044bbf44476b`. All parts use the same compiled fixtures. Driver revisions are recorded separately from executable revisions.

GPU probe adapters by suite: `shader`: NVIDIA RTX A500 Laptop GPU; `wgpu`: NVIDIA RTX A500 Laptop GPU.

Build cache provenance: Unchanged registry dependencies seeded from the matching upstream cache; Cargo validates inputs and compiler settings. Project packages rebuilt in separate directories.

## Findings

The shared suite contains 129 cases: 60 faster, 14 slower, 35 mixed or within the noise guard, 0 with a zero recorded time, and 20 new-feature costs. These counts are not an overall performance score. The following cases connect the isolated stages to application workloads.

| Case | Paired time change | Classification |
|---|---:|---|
| `input/mixed` | -64.6% | faster |
| `input/unicode` | +4.6% | mixed or within noise |
| `snapshot/plain/full` | -86.1% | faster |
| `row/plain/cached` | -37.4% | faster |
| `ui_text/cached_long` | +80.7% | slower |
| `ui_text/title_churn` | +7989.0% | slower |
| `wgpu/frame/full` | +14.7% | mixed or within noise |
| `wgpu/frame/unchanged` | -53.4% | faster |
| `wgpu/second_window_context` | +2.0% | mixed or within noise |
| `application/dense` | +173.6% | slower |
| `application/scroll` | +108.0% | slower |
| `application/search` | +27.0% | slower |
| `application/palette` | -21.1% | faster |
| `application/tabs` | +7.6% | new feature cost |
| `application/tab_repeat` | -10.2% | new feature cost |
| `application/tabs_distinct` | +9.4% | new feature cost |
| `windows/startup_exit` | +6.1% | mixed or within noise |
| `windows/minimize_restore` | -43.4% | mixed or within noise |
| `internal/vt_input/unicode_ascii` | -61.6% | faster |
| `internal/vt_input/unicode` | -1.2% | mixed or within noise |
| `internal/bench_poll` | -7.0% | mixed or within noise |

UI cache churn measures `Text::measure` over 4,096 distinct titles. The branch caps retained shapes at 2,048. This is a bounded-memory tradeoff, not a complete measure-and-draw frame. Warmed allocation counts and the retained-shape counter are recorded separately.

The Windows application replays use the same fixed terminal workloads as Linux and the Vulkan WGPU backend. They measure Rio process CPU time, excluding child Python interpreters. Timing starts after the fixture's ready file and a 300 ms wait, rather than a first-frame acknowledgement. The fixture targets four seconds; PTY backpressure and scheduling can extend its wall duration. The exit probe measures the fixture stop-file signal to Rio process exit, including the child's 10 ms polling interval and shutdown. It does not isolate event-loop dispatch latency. Minimize/restore also samples the child terminal's dimensions throughout 20 cycles and fails if the branch changes them.

| Replay | Upstream CPU ms | Branch CPU ms | Upstream wall ms | Branch wall ms |
|---|---:|---:|---:|---:|
| `application/cursor` | 4046.875 | 5078.125 | 4254.667 | 5337.999 |
| `application/dense` | 3687.500 | 10125.000 | 4242.218 | 10287.427 |
| `application/idle` | 0.000 | 78.125 | 4168.851 | 4194.752 |
| `application/palette` | 3859.375 | 2921.875 | 4264.895 | 4172.940 |
| `application/scroll` | 3531.250 | 7312.500 | 4232.198 | 7569.299 |
| `application/search` | 3953.125 | 5000.000 | 4293.886 | 5215.584 |
| `application/sparse` | 3984.375 | 5437.500 | 4252.144 | 5629.582 |
| `application/tab_repeat` | 3375.000 | 3515.625 | 4255.158 | 4464.272 |
| `application/tabs` | 3781.250 | 3906.250 | 4295.844 | 4317.150 |
| `application/tabs_distinct` | 3656.250 | 3890.625 | 4259.795 | 4349.941 |
| `application/unicode` | 4000.000 | 5453.125 | 4313.026 | 5546.079 |
| `windows/minimize_restore` | 812.500 | 468.750 | 4174.294 | 4203.352 |

These are medians across three replays per revision. A zero recorded CPU time has no valid percentage ratio. The raw records retain every replay, including idle variation. Single-core replays also constrain the child fixture and driver workers; they do not establish performance at normal affinity. See the [normal-affinity controls](windows-rendering-followup.md) for a separate comparison.

## Native Windows captures

These captures instrument the branch only and use `cmd`, rather than PowerShell. They do not establish a speedup relative to upstream. Frame interval statistics include pacing waits and are not input latency. The native capture process is not restricted to the shared suite's single CPU. Table values are medians across captures; the p95 column is the median of per-capture p95 values. CPU totals include startup. None mode deliberately renders fewer frames, so frame intervals are not a smoothness score.

Instrumented executable revision: `ddd0555d1e039741806192b1c4695c916e1473b4`. The minimize/restore probe uses a ten-second settling interval so all 20 cycles can finish. Other native captures use the default two-second interval. Each capture records `settle_ms`.

| Motion level | Frames | Process CPU ms | Frame interval median ms | Frame interval p95 ms |
|---|---:|---:|---:|---:|
| none | 21 | 187.500 | 54.258 | 69.036 |
| medium | 86 | 265.625 | 16.138 | 36.087 |
| full | 125 | 328.125 | 16.028 | 19.314 |

| Branch workload | Shell ready ms | First frame ms | Shell frame ms | Process CPU ms |
|---|---:|---:|---:|---:|
| `startup_exit` | 1634.076 | 1609.526 | 1645.347 | 531.250 |
| `second_window` | 2180.781 | 2142.903 | 2201.910 | 703.125 |
| `many_tabs` | 2664.171 | 2630.037 | 2666.415 | 781.250 |
| `dense` | 2372.721 | 1677.238 | 2377.376 | 140.625 |

## Changes with repeatable timing differences

| Case | Upstream µs/op | Branch µs/op | Time change | Paired range |
|---|---:|---:|---:|---:|
| `application/dense` | 3,687,500.000 | 10,125,000.000 | +173.6% | +135.6% to +228.8% |
| `application/palette` | 3,859,375.000 | 2,921,875.000 | -21.1% | -24.3% to -17.1% |
| `application/scroll` | 3,531,250.000 | 7,312,500.000 | +108.0% | +88.7% to +128.8% |
| `application/search` | 3,953,125.000 | 5,000,000.000 | +27.0% | +9.8% to +59.3% |
| `application/sparse` | 3,984,375.000 | 5,437,500.000 | +32.3% | +20.8% to +125.2% |
| `application/unicode` | 4,000,000.000 | 5,453,125.000 | +36.9% | +15.9% to +43.4% |
| `background/combining/plain` | 0.890 | 0.837 | -6.8% | -7.6% to -6.0% |
| `background/combining/selected` | 1.402 | 1.176 | -16.1% | -19.2% to -12.5% |
| `background/plain/plain` | 0.896 | 0.832 | -7.0% | -11.0% to -6.9% |
| `background/plain/selected` | 1.382 | 1.152 | -16.6% | -19.1% to -15.8% |
| `background/preedit/plain` | 1.484 | 1.054 | -28.5% | -29.0% to -28.1% |
| `background/preedit/selected` | 1.470 | 1.213 | -17.5% | -19.3% to -16.8% |
| `background/styled/selected` | 1.394 | 1.174 | -14.9% | -16.1% to -10.2% |
| `background/unicode/plain` | 0.890 | 0.827 | -7.6% | -12.7% to -6.9% |
| `background/unicode/selected` | 1.403 | 1.152 | -16.0% | -21.5% to -15.7% |
| `input/csi` | 4,042.262 | 3,801.387 | -6.0% | -7.4% to -2.8% |
| `input/mixed` | 5,806.241 | 2,120.620 | -64.6% | -64.7% to -63.1% |
| `internal/vt_input/truecolor_unique_parser_only` | 20,599.125 | 21,589.975 | +4.8% | +3.3% to +6.0% |
| `internal/vt_input/unicode_ascii` | 92,068.750 | 35,661.850 | -61.6% | -63.7% to -61.1% |
| `internal/vt_input/unicode_ascii_parser_only` | 13,096.733 | 13,916.225 | +5.4% | +3.7% to +8.0% |
| `palette/append_query` | 2.181 | 0.428 | -80.4% | -80.8% to -80.1% |
| `palette/edit_query` | 981.707 | 98.024 | -90.0% | -90.7% to -89.9% |
| `palette/navigation` | 1,969.491 | 0.091 | -100.0% | -100.0% to -100.0% |
| `palette/score_ascii` | 302.748 | 92.293 | -69.3% | -71.1% to -69.1% |
| `palette/score_unicode` | 1,414.263 | 549.224 | -60.5% | -66.1% to -60.5% |
| `palette/stable_results` | 986.566 | 0.088 | -100.0% | -100.0% to -100.0% |
| `row/combining/cached` | 12.059 | 7.648 | -35.6% | -38.1% to -35.0% |
| `row/combining/changing` | 96.710 | 94.699 | -2.3% | -3.8% to -2.1% |
| `row/combining/rasterize` | 257.119 | 288.524 | +12.4% | +3.5% to +14.8% |
| `row/combining/selected` | 11.534 | 8.385 | -27.3% | -28.0% to -26.7% |
| `row/plain/cached` | 4.105 | 2.547 | -37.4% | -38.8% to -36.9% |
| `row/plain/changing` | 38.041 | 74.170 | +76.8% | +11.1% to +111.5% |
| `row/plain/rasterize` | 435.173 | 413.923 | -4.9% | -9.9% to -2.3% |
| `row/plain/selected` | 12.935 | 9.952 | -22.4% | -28.4% to -21.3% |
| `row/preedit/cached` | 14.015 | 9.284 | -33.8% | -36.5% to -32.2% |
| `row/preedit/changing` | 39.221 | 33.449 | -14.7% | -18.8% to -13.4% |
| `row/preedit/selected` | 13.956 | 10.399 | -25.4% | -30.2% to -22.5% |
| `row/styled/cached` | 13.713 | 11.141 | -18.8% | -26.0% to -12.3% |
| `row/styled/changing` | 22.435 | 20.729 | -7.9% | -8.3% to -7.6% |
| `row/styled/selected` | 13.615 | 11.112 | -19.8% | -20.2% to -12.6% |
| `row/unicode/cached` | 12.100 | 9.125 | -24.4% | -26.3% to -23.3% |
| `row/unicode/changing` | 20.618 | 17.934 | -13.0% | -15.3% to -10.5% |
| `row/unicode/selected` | 12.078 | 9.884 | -18.0% | -19.8% to -16.3% |
| `search/unchanged_query` | 0.086 | 0.008 | -90.2% | -91.0% to -89.9% |
| `selection/three_rows_snapshot` | 14.105 | 0.187 | -98.6% | -98.7% to -98.6% |
| `snapshot/plain/full` | 40.367 | 5.666 | -86.1% | -87.0% to -86.0% |
| `snapshot/plain/one_row` | 0.892 | 0.500 | -43.3% | -44.3% to -42.4% |
| `snapshot/styled/full` | 72.418 | 38.741 | -46.5% | -48.6% to -42.7% |
| `snapshot/styled/one_row` | 1.450 | 1.004 | -31.3% | -31.7% to -27.1% |
| `ui_text/cached_long` | 1.097 | 1.986 | +80.7% | +77.6% to +88.0% |
| `ui_text/cached_short` | 0.134 | 0.181 | +34.7% | +32.4% to +46.2% |
| `ui_text/cached_unicode` | 0.417 | 0.583 | +41.4% | +38.5% to +41.4% |
| `ui_text/measure_long` | 0.319 | 0.123 | -61.4% | -62.1% to -59.6% |
| `ui_text/measure_short` | 0.071 | 0.119 | +66.6% | +53.7% to +80.7% |
| `ui_text/measure_unicode` | 0.228 | 0.124 | -45.8% | -51.4% to -45.0% |
| `ui_text/title_churn` | 0.310 | 25.415 | +7989.0% | +7926.0% to +8123.9% |
| `wgpu/frame/cursor_blink` | 384.032 | 237.898 | -36.0% | -46.3% to -35.0% |
| `wgpu/frame/one_row` | 469.509 | 253.256 | -44.1% | -67.9% to -43.5% |
| `wgpu/frame/unchanged` | 546.722 | 254.935 | -53.4% | -72.5% to -35.6% |
| `wgsl/background/cursor-half/0` | 56.000 | 26.688 | -52.3% | -52.3% to -52.2% |
| `wgsl/background/cursor-half/1` | 55.325 | 26.816 | -51.5% | -51.5% to -51.5% |
| `wgsl/background/cursor-half/2` | 57.664 | 26.845 | -53.4% | -53.5% to -53.4% |
| `wgsl/background/cursor/0` | 56.000 | 26.752 | -52.3% | -52.3% to -52.2% |
| `wgsl/background/cursor/1` | 55.104 | 26.751 | -51.5% | -51.6% to -51.5% |
| `wgsl/background/cursor/2` | 57.664 | 26.816 | -53.5% | -53.5% to -53.4% |
| `wgsl/background/mixed/0` | 66.880 | 56.512 | -15.5% | -15.5% to -15.4% |
| `wgsl/background/mixed/1` | 68.480 | 57.664 | -15.8% | -15.8% to -15.7% |
| `wgsl/background/mixed/2` | 70.303 | 59.200 | -15.8% | -15.8% to -15.7% |
| `wgsl/background/sparse/0` | 63.360 | 30.336 | -52.1% | -52.1% to -52.1% |
| `wgsl/background/sparse/1` | 55.232 | 26.880 | -51.3% | -51.3% to -51.3% |
| `wgsl/background/sparse/2` | 57.792 | 27.008 | -53.3% | -53.3% to -53.2% |
| `wgsl/background/transparent-rgb/0` | 56.128 | 26.752 | -52.3% | -52.5% to -52.3% |
| `wgsl/background/transparent-rgb/1` | 55.232 | 26.717 | -51.6% | -51.7% to -51.6% |
| `wgsl/background/transparent-rgb/2` | 57.792 | 26.816 | -53.6% | -53.6% to -53.6% |

## New feature costs

These cases measure work added by the branch. An upstream control, where present, provides context but does less or different work.
They are not included in the repeatable-speedup table above.

| Case | Upstream control µs/op | Branch µs/op |
|---|---:|---:|
| `animation/full` | No equivalent | 0.309 |
| `animation/idle_deadline` | No equivalent | 0.081 |
| `animation/medium` | No equivalent | 0.096 |
| `animation/none` | No equivalent | 0.003 |
| `application/idle` | 0.000 | 78,125.000 |
| `application/tab_repeat` | 3,375,000.000 | 3,515,625.000 |
| `application/tabs` | 3,781,250.000 | 3,906,250.000 |
| `application/tabs_distinct` | 3,656,250.000 | 3,890,625.000 |
| `icons/atlas_hit` | No equivalent | 0.040 |
| `icons/catalog/local` | No equivalent | 0.289 |
| `icons/catalog/remote` | No equivalent | 0.342 |
| `icons/catalog/versioned` | No equivalent | 0.365 |
| `icons/catalog/windows_path` | No equivalent | 0.381 |
| `icons/failed_upload_hit` | No equivalent | 0.022 |
| `icons/svg_raster/24` | No equivalent | 98.333 |
| `icons/svg_raster/48` | No equivalent | 140.666 |
| `status/idle_deadline` | No equivalent | 0.082 |
| `status/oversized_sequence` | 1,686.062 | 58.337 |
| `status/report_and_sync` | No equivalent | 1.024 |
| `status/revision_hit` | No equivalent | 0.004 |

The full CSV also retains mixed, unchanged, incomplete, and feature-cost cases. A shader measurement is GPU time per draw.
WGPU frame measurements include encoding, submission, and completion. Native Vulkan recording measurements are CPU work without submission.
Application entries are Rio process CPU time for a four-second replay, not wall time per rendered frame. They exclude child-process CPU time.

## Measurement limits

- New microbenchmarks use the shared counting allocator over Rust's `System` allocator, including its disabled counter check during timing. Application replays use the normal application allocator. Isolated allocation-heavy timing ratios need not match end-to-end ratios.
- Allocation probes count calls through Rust's global allocator across the process during a separate warmed pass. Native driver allocations and GPU memory are excluded; driver-related Rust workers can contribute. Bytes mean cumulative allocated bytes, not retained memory or peak RSS.
- Windows application CPU accounting uses `GetProcessTimes`. Its resolution does not establish frame latency. Native motion captures use instrumentation and are separate from the uninstrumented upstream comparison.
- Native Vulkan recording cases update rows, upload host data, and record reset/unsubmitted command buffers. They do not time GPU text execution. WGPU frame cases submit and wait, but their glyphs have zero area.
- Shader timestamps cover repeated background draws, not a complete terminal frame. Only the WGSL probe checks GPU framebuffer equality; native Vulkan background timestamps are not a framebuffer-equivalence test.
- Transparent draw elision tests the canonical `CellBg::TRANSPARENT` sentinel. Other zero-alpha encodings may still issue a draw and benefit only from the shader early-out.
- UI raster hashes differ intentionally with bitmap-origin alignment. Stable hashes record the output but do not establish visual correctness of that difference.
- Power policy and GPU clocks were not fixed. Alternating pairs reduce order bias but do not isolate every source of system variation.

## Storage and work counters

- `animation/full`: `{"branch": {"steps": 17}}`.
- `animation/medium`: `{"branch": {"steps": 5}}`.
- `animation/none`: `{"branch": {"steps": 0}}`.
- `selection/three_rows_snapshot`: `{"branch": {"dirty_rows": 4}, "upstream": {"dirty_rows": 60}}`.
- `ui_text/title_churn`: `{"branch": {"retained_shapes": 2048}, "upstream": {"retained_shapes": 4096}}`.
- `windows/minimize_restore`: `{"branch": {"terminal_size_variants": 1}, "upstream": {"terminal_size_variants": 1}}`.

## Output checks

| Check | Result |
|---|---|
| `input/ansi` | Identical upstream and branch hashes |
| `input/ascii` | Identical upstream and branch hashes |
| `input/csi` | Identical upstream and branch hashes |
| `input/mixed` | Identical upstream and branch hashes |
| `input/unicode` | Identical upstream and branch hashes |
| `palette/selected` | Identical upstream and branch hashes |
| `row/combining` | Identical upstream and branch hashes |
| `row/plain` | Identical upstream and branch hashes |
| `row/preedit` | Identical upstream and branch hashes |
| `row/styled` | Identical upstream and branch hashes |
| `row/unicode` | Identical upstream and branch hashes |
| `ui_text/cached_long` | Different UI raster hashes; bitmap-origin alignment changed on the branch |
| `ui_text/cached_short` | Different UI raster hashes; bitmap-origin alignment changed on the branch |
| `ui_text/cached_unicode` | Different UI raster hashes; bitmap-origin alignment changed on the branch |
| `wgsl/background/cursor-half/0` | Identical upstream and branch hashes |
| `wgsl/background/cursor-half/1` | Identical upstream and branch hashes |
| `wgsl/background/cursor-half/2` | Identical upstream and branch hashes |
| `wgsl/background/cursor/0` | Identical upstream and branch hashes |
| `wgsl/background/cursor/1` | Identical upstream and branch hashes |
| `wgsl/background/cursor/2` | Identical upstream and branch hashes |
| `wgsl/background/faint/0` | Identical upstream and branch hashes |
| `wgsl/background/faint/1` | Identical upstream and branch hashes |
| `wgsl/background/faint/2` | Identical upstream and branch hashes |
| `wgsl/background/mixed/0` | Identical upstream and branch hashes |
| `wgsl/background/mixed/1` | Identical upstream and branch hashes |
| `wgsl/background/mixed/2` | Identical upstream and branch hashes |
| `wgsl/background/opaque/0` | Identical upstream and branch hashes |
| `wgsl/background/opaque/1` | Identical upstream and branch hashes |
| `wgsl/background/opaque/2` | Identical upstream and branch hashes |
| `wgsl/background/sparse/0` | Identical upstream and branch hashes |
| `wgsl/background/sparse/1` | Identical upstream and branch hashes |
| `wgsl/background/sparse/2` | Identical upstream and branch hashes |
| `wgsl/background/transparent-rgb/0` | Identical upstream and branch hashes |
| `wgsl/background/transparent-rgb/1` | Identical upstream and branch hashes |
| `wgsl/background/transparent-rgb/2` | Identical upstream and branch hashes |

These checks cover the recorded fixtures. They do not prove every terminal, font, or platform combination.
Application UI workloads assert that requested UI changes affect the rendered client image before timing.

## Coverage of the branch inventory

| Change | Result |
|---|---|
| P01 | Measured |
| P02 | Measured |
| P03 | Measured |
| P04 | Measured |
| P05 | Measured |
| P06 | Measured |
| P07 | Measured |
| P08 | Measured |
| P09 | Measured |
| P10 | Measured |
| P11 | Measured |
| P12 | Measured shared WGPU/WGSL work; Linux native Vulkan excluded |
| P13 | Measured shared WGPU/WGSL work; Linux native Vulkan excluded |
| P14 | Not run: Linux native Vulkan |
| P15 | Measured |
| P16 | Measured |
| P17 | Measured |
| P18 | Measured |
| P19 | Measured |
| P20 | Measured |
| P21 | Measured |
| P22 | Measured |
| P23 | Measured |
| P24 | Measured |
| P25 | Measured |
| P26 | Measured |
| P27 | Measured |
| P28 | Not run: Bash integration |
| P29 | Measured |
| P30 | Measured branch-only native captures; no upstream instrumentation |
| P31 | Measured |

P12 and P13 cover the shared WGPU/WGSL path here. P14 is Linux native Vulkan code. Bash integration P28 is not exercised on this Windows host. P29 and P31 use uninstrumented upstream/branch controls; P30 uses branch-only instrumented motion captures.
These measurements use Vulkan on Windows. They do not establish DirectX 12 performance.

## Reproduction and retained data

```sh
python3 misc/benchmarks/run.py build
WGPU_BACKEND=vulkan python3 misc/benchmarks/run.py run --runs 3 --samples 11 --ms 100 --cpu 2
python3 misc/benchmarks/report.py
```

The ignored `target/branch-performance` directory retains all samples, allocation probes, output hashes, build logs, isolated-display logs, and executable copies.
Compiled Rust fixture SHA-256: `c7df318a20f754c8116ad03f3fda21a34a2c37794f83f68380f99858a9d32050`.

Native captures: `python -X utf8 misc/benchmarks/windows.py <rio-profile.exe> --runs 3 --output target/branch-performance/windows`.
On Linux, generate this report from copied Windows artifacts with `python3 misc/benchmarks/report.py --input target/windows-branch-performance`.
