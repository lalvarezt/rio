# Linux branch performance report

Upstream: `59203ccd5fa1e6b760a738c1ad653bb29f2f3465`. Branch executable revision: `fc873453f9a459a9072eec7f5e5240fb334befde`.
Benchmark driver revision: `fe06749a34fc068a0c92a02fc1336c3966540456`. The runner permits only Python-driver and report changes after the executable revision; compiled sources and fixture hashes must match.
This report compares the final branch with upstream, not with intermediate optimization commits.

## Measurement environment

- System: Linux-7.2.6-arch2-1-x86_64-with-glibc2.44; 13th Gen Intel(R) Core(TM) i7-1370P.
- Toolchain: rustc 1.96.1 (31fca3adb 2026-06-26); cargo 1.96.1 (356927216 2026-06-26).
- CPU affinity: logical CPU 2. No power policy was changed.
- GPU adapters reported by the probes: Intel(R) Iris(R) Xe Graphics (RPL-P).
- 3 alternating executable pairs; 11 CPU samples per pair and case.
- CPU samples target 100 ms. Allocation counting uses a separate warmed pass.
- Matching optimized profiles in separate build directories: LTO disabled, 16 codegen units, debug information and incremental compilation disabled, no stripping.
- Native application replays use an isolated KWin virtual display with Xwayland.
- Builds finish before timing. The suite includes the existing VT and poll Criterion benchmarks.

## Reading the results

A negative change means less time. The paired range shows the smallest and largest time change across runs.
A case is labeled faster or slower only when every pair changes in the same direction by more than 2%.
This classification is a descriptive noise guard, not a confidence interval or a statistical significance test.
New features with no equivalent upstream work are cost measurements. Their relative timings do not establish a feature-preserving speedup.
The results cannot be added into a single terminal speedup: the cases measure different operations and overlapping pipeline stages.

The complete per-case timing and allocation results are in [the CSV](linux-branch-performance-report.csv).

## Findings

The suite contains 137 cases: 78 faster, 13 slower, 25 mixed or within the noise guard, and 21 new-feature costs. These counts give equal weight to small helpers and whole-workload probes; they are not an overall performance score.

### Terminal input and CPU rendering

Mixed Unicode/ASCII input time changes by -64.9% relative to upstream. The existing `unicode_ascii` VT benchmark agrees (-64.9%). Plain ASCII input is within the noise guard. Pure Unicode is a repeatable regression: +11.0% in the new fixture and +6.4% in the existing VT benchmark. The branch therefore does not improve every input distribution.

Full plain snapshot time changes by -81.7%; styled snapshot time changes by -41.3%. Both remove 60 warmed allocations and 76,800 allocated bytes per operation. Cached row processing improves across all five fixtures, with smaller gains when row content changes. Glyph bitmap reuse reduces warmed allocations, but the rasterization fixtures do not show a consistent timing win; the Unicode rasterization case is slower. Narrow selection invalidation reduces the fixture's dirty rows from 60 to 4.

### GPU and application workloads

Native Vulkan CPU packing, upload, and command-recording cases improve with both one and two atlases. The shared WGPU frame fixtures also improve, as does creating a second WGPU window context. These are focused controls: zero-area glyphs isolate storage and upload work rather than dense text execution.

The native Vulkan background timestamps are mixed: the canonical transparent sentinel skips the draw, and the cursor case improves slightly, but sparse and opaque cases regress substantially (+72.6% and +83.8%). The separate WGSL probe shows lower GPU time for zero-alpha-heavy backgrounds, including nonzero-RGB/zero-alpha cells, while opaque and faint backgrounds stay within the noise guard. These probes use different pipelines and execution patterns. The WGSL result does not explain away the native Vulkan regressions; their cause needs a separate investigation.

Four-second application replays consume less Rio CPU time for cursor updates, scrolling, and search. Dense text, sparse text, Unicode, and palette replays are mixed or within the noise guard. The tab replay uses more CPU with the branch's animated tab UI; the upstream control lacks equivalent work. None of these process-CPU measurements establishes improved frame latency or fewer missed frames.

### UI, search, and feature costs

Palette scoring removes most allocations, and stable results/navigation reuse cached matches. The large helper-level gains do not become a measurable palette application-replay gain in this run. Changing a search query costs about 2 ns more in its isolated setter probe; unchanged queries are cheaper. The existing poll benchmark is within the noise guard (+0.1%).

Cached UI text is a repeatable regression despite removing its warmed allocation: short and Unicode labels are about 50% slower, and long labels about 74% slower. The 4,096-title churn fixture exposes a larger time/memory tradeoff: the branch retains 2,048 shapes instead of upstream's 4,096, so repeated sequential measurement evicts and reshapes titles. This fixture measures `Text::measure` only; its roughly 160-fold cost increase does not describe a complete measure-and-draw frame. It still identifies a workload where the bounded cache is substantially worse.

Icon catalog lookups, cached upload checks, animation steps, and status revision hits have small isolated costs. SVG rasterization is a miss-only cost. Bash reporting adds work relative to the upstream no-hook control; both hook fixtures emit three reports during 10,000 commands, including the existing-hook case. The medium animation control performs five steps instead of full mode's 17; this measures work, not perceived smoothness.

The next profiling priorities are pure Unicode parsing, cached UI text and cache churn, and native Vulkan sparse/opaque background execution. These are measured regressions or tradeoffs, rather than inferred opportunities from a single favorable microbenchmark.

## Changes with repeatable timing differences

| Case | Upstream µs/op | Branch µs/op | Time change | Paired range |
|---|---:|---:|---:|---:|
| `application/cursor` | 80,000.000 | 60,000.000 | -25.0% | -25.0% to -14.3% |
| `application/scroll` | 180,000.000 | 140,000.000 | -22.2% | -27.8% to -17.6% |
| `application/search` | 140,000.000 | 90,000.000 | -30.8% | -46.7% to -28.6% |
| `background/combining/plain` | 0.279 | 0.257 | -7.6% | -7.9% to -6.8% |
| `background/combining/selected` | 0.431 | 0.376 | -12.9% | -13.4% to -12.6% |
| `background/plain/plain` | 0.279 | 0.258 | -7.6% | -7.8% to -7.1% |
| `background/plain/selected` | 0.432 | 0.375 | -13.3% | -13.6% to -12.9% |
| `background/preedit/plain` | 0.432 | 0.352 | -18.5% | -21.2% to -18.4% |
| `background/preedit/selected` | 0.449 | 0.392 | -12.7% | -15.0% to -12.5% |
| `background/styled/plain` | 0.279 | 0.259 | -7.6% | -7.9% to -7.1% |
| `background/styled/selected` | 0.433 | 0.376 | -13.2% | -13.6% to -12.6% |
| `background/unicode/plain` | 0.279 | 0.257 | -7.7% | -7.8% to -7.2% |
| `background/unicode/selected` | 0.432 | 0.375 | -13.1% | -13.5% to -12.9% |
| `gpu/background/cursor` | 424.757 | 412.482 | -2.9% | -2.9% to -2.9% |
| `gpu/background/opaque` | 347.568 | 638.862 | +83.8% | +83.8% to +100.3% |
| `gpu/background/sparse` | 239.030 | 412.529 | +72.6% | +72.6% to +72.6% |
| `gpu/background/transparent` | 147.253 | 0.936 | -99.4% | -99.4% to -99.3% |
| `gpu/record/1_atlas/full` | 34.523 | 18.751 | -46.2% | -53.8% to -44.6% |
| `gpu/record/1_atlas/one_row` | 29.983 | 14.806 | -50.6% | -52.0% to -50.3% |
| `gpu/record/1_atlas/rotating_slots` | 29.837 | 10.030 | -66.4% | -67.2% to -66.1% |
| `gpu/record/1_atlas/unchanged` | 3.408 | 2.739 | -19.6% | -19.9% to -18.3% |
| `gpu/record/2_atlas/full` | 38.407 | 33.030 | -14.0% | -14.7% to -14.0% |
| `gpu/record/2_atlas/one_row` | 34.529 | 28.626 | -16.8% | -17.5% to -14.9% |
| `gpu/record/2_atlas/rotating_slots` | 34.639 | 15.178 | -56.0% | -56.2% to -55.6% |
| `gpu/record/2_atlas/unchanged` | 3.558 | 2.895 | -18.6% | -19.9% to -18.1% |
| `input/ansi` | 925.660 | 842.611 | -9.3% | -9.8% to -8.7% |
| `input/csi` | 1,266.679 | 1,112.710 | -12.2% | -12.4% to -10.6% |
| `input/mixed` | 1,726.406 | 605.936 | -64.9% | -65.2% to -64.5% |
| `input/unicode` | 1,834.909 | 2,040.157 | +11.0% | +9.7% to +13.2% |
| `internal/vt_input/ansi_bare_csi_parser_only` | 8,934.253 | 8,444.001 | -5.5% | -5.9% to -3.5% |
| `internal/vt_input/ansi_two_styles_parser_only` | 10,891.854 | 10,500.666 | -3.5% | -3.7% to -3.3% |
| `internal/vt_input/truecolor_unique` | 16,183.466 | 15,075.434 | -6.8% | -7.7% to -5.4% |
| `internal/vt_input/truecolor_unique_parser_only` | 6,583.630 | 6,876.856 | +4.6% | +3.4% to +4.6% |
| `internal/vt_input/unicode` | 28,016.341 | 29,796.888 | +6.4% | +6.1% to +6.6% |
| `internal/vt_input/unicode_ascii` | 27,146.761 | 9,543.993 | -64.9% | -65.0% to -64.8% |
| `internal/vt_input/unicode_ascii_parser_only` | 3,126.915 | 3,265.844 | +4.8% | +4.3% to +5.7% |
| `internal/vt_input/unicode_parser_only` | 2,313.590 | 2,500.137 | +7.9% | +7.7% to +8.1% |
| `palette/append_query` | 0.303 | 0.064 | -79.1% | -79.1% to -79.0% |
| `palette/edit_query` | 147.220 | 29.685 | -79.8% | -79.9% to -79.7% |
| `palette/navigation` | 293.020 | 0.007 | -100.0% | -100.0% to -100.0% |
| `palette/score_ascii` | 135.537 | 27.874 | -79.4% | -79.5% to -79.4% |
| `palette/score_unicode` | 277.630 | 172.907 | -37.7% | -37.8% to -37.4% |
| `palette/stable_results` | 146.306 | 0.007 | -100.0% | -100.0% to -100.0% |
| `row/combining/cached` | 3.744 | 2.185 | -41.7% | -42.9% to -41.5% |
| `row/combining/changing` | 30.478 | 28.405 | -6.9% | -7.1% to -5.9% |
| `row/combining/selected` | 3.680 | 2.464 | -33.1% | -34.2% to -32.6% |
| `row/plain/cached` | 4.157 | 2.573 | -38.1% | -39.0% to -35.4% |
| `row/plain/changing` | 38.408 | 36.134 | -5.9% | -6.4% to -5.1% |
| `row/plain/selected` | 4.110 | 2.934 | -28.6% | -29.7% to -25.7% |
| `row/preedit/cached` | 4.516 | 2.836 | -37.2% | -38.5% to -35.7% |
| `row/preedit/changing` | 12.021 | 10.082 | -16.3% | -16.5% to -14.8% |
| `row/preedit/selected` | 4.488 | 3.158 | -29.3% | -30.9% to -28.6% |
| `row/styled/cached` | 4.460 | 3.390 | -25.1% | -25.1% to -23.1% |
| `row/styled/changing` | 7.178 | 5.974 | -16.5% | -17.4% to -16.0% |
| `row/styled/selected` | 4.514 | 3.399 | -25.6% | -25.8% to -22.8% |
| `row/unicode/cached` | 3.981 | 2.616 | -34.8% | -35.0% to -31.0% |
| `row/unicode/changing` | 6.643 | 5.190 | -21.9% | -22.5% to -20.5% |
| `row/unicode/rasterize` | 94.765 | 96.700 | +2.2% | +2.0% to +17.0% |
| `row/unicode/selected` | 3.990 | 2.903 | -28.0% | -28.4% to -24.6% |
| `search/changing_query` | 0.007 | 0.009 | +27.1% | +26.5% to +27.7% |
| `search/unchanged_query` | 0.007 | 0.002 | -68.2% | -68.4% to -68.1% |
| `selection/three_rows_snapshot` | 9.276 | 0.199 | -97.9% | -97.9% to -97.8% |
| `snapshot/plain/full` | 8.567 | 1.558 | -81.7% | -81.9% to -81.7% |
| `snapshot/plain/one_row` | 0.260 | 0.157 | -39.6% | -39.9% to -39.6% |
| `snapshot/styled/full` | 20.450 | 11.981 | -41.3% | -41.6% to -41.2% |
| `snapshot/styled/one_row` | 0.445 | 0.321 | -28.0% | -28.0% to -27.7% |
| `ui_text/cached_long` | 0.354 | 0.618 | +74.4% | +74.1% to +78.8% |
| `ui_text/cached_short` | 0.110 | 0.167 | +51.9% | +49.6% to +52.8% |
| `ui_text/cached_unicode` | 0.107 | 0.159 | +49.3% | +49.0% to +55.1% |
| `ui_text/title_churn` | 0.047 | 7.536 | +15863.3% | +15861.2% to +15867.5% |
| `wgpu/frame/cursor_blink` | 1,047.549 | 410.172 | -60.8% | -66.0% to -60.7% |
| `wgpu/frame/full` | 1,073.954 | 613.103 | -42.9% | -51.2% to -42.5% |
| `wgpu/frame/one_row` | 891.798 | 452.120 | -49.3% | -63.7% to -49.0% |
| `wgpu/frame/unchanged` | 1,073.235 | 410.434 | -61.8% | -62.1% to -61.0% |
| `wgpu/frame/uniform_change` | 950.544 | 453.953 | -52.2% | -57.7% to -52.0% |
| `wgpu/second_window_context` | 554,854.953 | 9,514.609 | -98.3% | -98.3% to -98.3% |
| `wgsl/background/cursor-half/0` | 143.384 | 88.525 | -38.3% | -38.3% to -38.3% |
| `wgsl/background/cursor-half/1` | 152.362 | 88.527 | -41.9% | -41.9% to -41.9% |
| `wgsl/background/cursor-half/2` | 152.362 | 88.532 | -41.9% | -41.9% to -41.9% |
| `wgsl/background/cursor/0` | 143.368 | 88.525 | -38.3% | -38.3% to -38.2% |
| `wgsl/background/cursor/1` | 152.371 | 88.535 | -41.9% | -41.9% to -41.9% |
| `wgsl/background/cursor/2` | 152.358 | 88.522 | -41.9% | -41.9% to -41.9% |
| `wgsl/background/mixed/0` | 143.621 | 121.305 | -15.5% | -15.5% to -15.4% |
| `wgsl/background/mixed/1` | 152.546 | 127.503 | -16.4% | -16.4% to -16.4% |
| `wgsl/background/mixed/2` | 152.508 | 127.493 | -16.4% | -16.4% to -16.4% |
| `wgsl/background/sparse/0` | 143.278 | 88.503 | -38.2% | -38.2% to -38.2% |
| `wgsl/background/sparse/1` | 152.349 | 88.555 | -41.9% | -41.9% to -41.9% |
| `wgsl/background/sparse/2` | 152.337 | 88.548 | -41.9% | -41.9% to -41.9% |
| `wgsl/background/transparent-rgb/0` | 143.327 | 88.522 | -38.2% | -38.3% to -38.2% |
| `wgsl/background/transparent-rgb/1` | 152.326 | 88.522 | -41.9% | -41.9% to -41.9% |
| `wgsl/background/transparent-rgb/2` | 152.326 | 88.521 | -41.9% | -41.9% to -41.9% |

## New feature costs

These cases measure work added by the branch. An upstream control, where present, provides context but does less or different work.
They are not included in the repeatable-speedup table above.

| Case | Upstream control µs/op | Branch µs/op |
|---|---:|---:|
| `animation/full` | No equivalent | 0.078 |
| `animation/idle_deadline` | No equivalent | 0.011 |
| `animation/medium` | No equivalent | 0.023 |
| `animation/none` | No equivalent | 0.000 |
| `application/idle` | 10,000.000 | 20,000.000 |
| `application/tab_repeat` | 600,000.000 | 640,000.000 |
| `application/tabs` | 70,000.000 | 240,000.000 |
| `bash/command_loop` | 14,551.187 | 487,048.615 |
| `bash/command_loop_existing_hook` | 83,140.054 | 557,359.267 |
| `icons/atlas_hit` | No equivalent | 0.012 |
| `icons/catalog/local` | No equivalent | 0.076 |
| `icons/catalog/remote` | No equivalent | 0.097 |
| `icons/catalog/versioned` | No equivalent | 0.094 |
| `icons/catalog/windows_path` | No equivalent | 0.087 |
| `icons/failed_upload_hit` | No equivalent | 0.008 |
| `icons/svg_raster/24` | No equivalent | 27.334 |
| `icons/svg_raster/48` | No equivalent | 39.954 |
| `status/idle_deadline` | No equivalent | 0.012 |
| `status/oversized_sequence` | 405.979 | 6.900 |
| `status/report_and_sync` | No equivalent | 0.187 |
| `status/revision_hit` | No equivalent | 0.001 |

The full CSV also retains mixed, unchanged, incomplete, and feature-cost cases. A shader measurement is GPU time per draw.
WGPU frame measurements include encoding, submission, and completion. Native Vulkan recording measurements are CPU work without submission.
Application entries are Rio process CPU time for a four-second replay, not wall time per rendered frame. They exclude child-process CPU time.

## Measurement limits

- New microbenchmarks use the shared counting allocator over Rust's `System` allocator, including its disabled counter check during timing. Application replays use the normal application allocator. Isolated allocation-heavy timing ratios need not match end-to-end ratios.
- Allocation probes count calls through Rust's global allocator across the process during a separate warmed pass. Native driver allocations and GPU memory are excluded; driver-related Rust workers can contribute. Bytes mean cumulative allocated bytes, not retained memory or peak RSS.
- Application CPU accounting has a 10 ms tick resolution on this host. Small absolute differences, especially idle workloads, are coarse. The virtual display differs from an interactive desktop.
- Native Vulkan recording cases update rows, upload host data, and record reset/unsubmitted command buffers. They do not time GPU text execution. WGPU frame cases submit and wait, but their glyphs have zero area.
- Shader timestamps cover repeated background draws, not a complete terminal frame. Only the WGSL probe checks GPU framebuffer equality; native Vulkan background timestamps are not a framebuffer-equivalence test.
- Transparent draw elision tests the canonical `CellBg::TRANSPARENT` sentinel. Other zero-alpha encodings may still issue a draw and benefit only from the shader early-out.
- UI raster hashes differ intentionally with bitmap-origin alignment. Stable hashes record the output but do not establish visual correctness of that difference.
- Power policy and GPU clocks were not fixed. Alternating pairs reduce order bias but do not isolate every source of system variation.

## Storage and work counters

- `animation/full`: `{"branch": {"steps": 17}}`.
- `animation/medium`: `{"branch": {"steps": 5}}`.
- `animation/none`: `{"branch": {"steps": 0}}`.
- `bash/command_loop`: `{"branch": {"reports": 3}, "upstream": {"reports": 0}}`.
- `bash/command_loop_existing_hook`: `{"branch": {"reports": 3}, "upstream": {"reports": 0}}`.
- `selection/three_rows_snapshot`: `{"branch": {"dirty_rows": 4}, "upstream": {"dirty_rows": 60}}`.
- `ui_text/title_churn`: `{"branch": {"retained_shapes": 2048}, "upstream": {"retained_shapes": 4096}}`.

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
| P12 | Measured |
| P13 | Measured |
| P14 | Measured |
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
| P28 | Measured |
| P29 | Windows only; not run |
| P30 | Windows only; not run |
| P31 | Windows only; not run |

P29–P31 are Windows lifecycle and display-clock cases. Their runner is committed, but Linux cannot validate those native behaviors.
Linux WGPU results concern the shared renderer implementation; they are not Windows measurements.

## Reproduction and retained data

```sh
python3 misc/benchmarks/run.py build
WGPU_BACKEND=vulkan python3 misc/benchmarks/run.py run --runs 3 --samples 11 --ms 100 --cpu 2
python3 misc/benchmarks/report.py
```

The ignored `target/branch-performance` directory retains all samples, allocation probes, output hashes, build logs, isolated-display logs, and executable copies.
Compiled Rust fixture SHA-256: `1268aa8acebff1839f4ba308db50839a59dfe7e29874b1cfbd87426e2ec0cc81`.
