# Rendering optimization pass

This pass compares `9884c1abe8e4017900b98f374d8c50b1f518bd82` with the
retained changes in `058f8b11f7` and `a3b060dd94`. Measurement changes are in
`5e154b8b7c`. These results are incremental to the earlier
[Linux](linux-branch-performance-report.md) and
[Windows](windows-branch-performance-report.md) reports. The
[branch inventory](branch-performance.md) remains the comparison with upstream.

Two production changes survived measurement: resolved atlas slots are reused
by UI text, and packed terminal background rows use byte comparisons. Both
reduce focused CPU costs on Linux and Windows. Whole-application results vary
by workload, and the Windows frame captures do not establish smoother rendering.

The [CSV](rendering-optimization-pass.csv) contains all 35 comparisons, including
mixed results, allocations, process CPU time, and Windows process cycles.

## Retained implementation

### Reuse UI text atlas slots

Each cached `ShapedGlyph` retains its resolved atlas slot in a `OnceLock`.
Drawing and vertical-bound measurement share the same lookup. Successful hits
avoid the atlas hash lookup and backend dispatch. A small forced-inline hit
path calls a non-inlined resolver on misses. Failed resolutions remain
retryable. Atlas initialization clears shaped runs and icons on every backend;
atlas growth preserves existing slot coordinates.

The unused cluster field was removed. Glyph storage grows from 20 to 36 bytes
per element. In the 4,096-title storage fixture, the existing 2,048-entry cache
retains 1,310,720 bytes of glyph-vector capacity before the change and
2,359,296 afterward: an additional 1 MiB. This counts glyph-vector capacity,
not atlas storage, map overhead, or process memory. The entry limit is unchanged;
the fixture's byte count is not a universal memory bound.

### Compare packed background rows

`CellBg` contains four byte channels and implements `Pod`. Vulkan and WGPU now
compare the populated prefix as byte slices, allowing an optimized byte
comparison instead of repeated element comparisons. Short-row tail clearing,
dirty ranges, visibility detection, uploads, and shaders are unchanged.
This change adds no runtime storage or allocations.

The new shared `grid_storage.rs` fixture calls the production row writer for
48 rows of 160 cells. It covers unchanged transparent rows, unchanged opaque
rows, and a change to the final cell. Timings exclude uploads, command
recording, GPU execution, and waits.

## Measurement conditions

- Date: 2026-10-17. Both machines report a Core i7-1370P.
- Linux: Arch Linux, kernel 7.2.6-arch2-1, Intel Iris Xe, native Vulkan and WGPU.
- Windows: Windows 11 build 22631, NVIDIA RTX A500 Laptop GPU, WGPU Vulkan.
- Rust and Cargo: 1.96.1. Optimized builds use no LTO, 16 codegen units,
  no debug information, and no incremental compilation.
- Microbenchmarks: logical CPU 2, three alternating before/after pairs,
  11 samples per case per run, targeting 100 ms per sample. Allocation counts
  come from a separate warmed pass.
- Application replays: Linux uses logical CPU 2. Windows uses normal affinity
  across all 20 logical CPUs and Normal priority. No power policy was changed.
- Replays use immutable executable copies and the same active executable path
  for both revisions. Source and executable hashes identify staged builds.
  Installed Rio and the user's configuration are unchanged.

Tables report the median of the three paired percentage changes. Before and
after times in the CSV are medians of their respective run medians, so their
ratio can differ from the median paired change. A result is classified as
faster only when all three pairs improve by more than 2%. This is a descriptive
noise guard, not a significance test or confidence interval.

## Focused CPU results

Negative changes mean less time. All warmed drawing and row-storage cases
allocate zero times before and after.

| Case | Linux change | Windows change |
|---|---:|---:|
| Cached short UI text | -26.6% | -5.0% |
| Cached long UI text | -27.1% | -8.8%, mixed |
| Cached Unicode UI text | -29.8% | -12.6% |
| Measure short UI text | +0.6%, mixed | -3.0%, mixed |
| Measure long UI text | -0.1%, mixed | -5.1%, mixed |
| Measure Unicode UI text | -0.8%, mixed | -6.9% |
| Changing UI titles | +0.9%, mixed | -0.8%, mixed |

Linux cached drawing improves in every pair: short text ranges from -26.8%
to -25.9%, long text from -27.7% to -21.2%, and Unicode from -30.2% to -29.8%.
Windows short text ranges from -5.6% to -3.2%, and Unicode from -14.4% to -6.4%.
Windows long text includes a +1.6% pair, so its median is not a repeatable win.

| Background storage, 48-row batch | Before | After | Paired change |
|---|---:|---:|---:|
| Linux Vulkan, transparent | 2.076 us | 0.472 us | -77.7% |
| Linux Vulkan, opaque | 2.085 us | 0.475 us | -77.5% |
| Linux Vulkan, final cell changes | 6.805 us | 5.194 us | -23.6% |
| Linux WGPU, transparent | 2.323 us | 0.529 us | -77.3% |
| Linux WGPU, opaque | 2.329 us | 0.527 us | -77.6% |
| Linux WGPU, final cell changes | 6.686 us | 4.685 us | -29.9% |
| Windows WGPU, transparent | 2.446 us | 0.861 us | -64.6% |
| Windows WGPU, opaque | 8.045 us | 2.867 us | -64.4% |
| Windows WGPU, final cell changes | 23.052 us | 16.904 us | -27.7% |

All row-storage pairs improve. The first Windows transparent pair has a large
before outlier (-89.2%); the other two improve by approximately 64%.
Opaque Windows pairs range from -64.6% to -63.9%, and changed-tail pairs
from -29.4% to -25.9%. These are CPU storage results, not GPU frame speedups.

## Whole-application controls

Each platform ran 36 uninstrumented replays: six workloads, three alternating
pairs, and two revisions. Each workload issues 240 updates over four seconds.
Measurement includes the final 150 ms settling interval. Process CPU counters
exclude child workload interpreters. Tab workloads create three terminals;
their ready sizes match before and after on both OSes: 179x52, 178x48, and
177x44. Single-terminal workloads use 179x52.

Linux now reads the process CPU clock through `clock_getcpuclockid`, avoiding
the 10 ms quantization of `/proc` accounting. Windows retains `GetProcessTimes`
and also records `QueryProcessCycleTime`. Windows CPU time remains quantized
and variable, so the cycle counter is the more useful additional control here.
Cycles are neither elapsed time nor retired instructions and cannot be compared
across machines or converted to time without processor-specific information.
See Microsoft's [QueryProcessCycleTime documentation](https://learn.microsoft.com/en-us/windows/win32/api/realtimeapiset/nf-realtimeapiset-queryprocesscycletime).

| Workload | Linux process CPU | Windows process cycles |
|---|---:|---:|
| Dense text | -6.2%, lower in all pairs | -4.4%, mixed |
| Palette | -10.1%, lower in all pairs | -4.4%, mixed |
| Scroll | +22.6%, mixed | -0.2%, mixed |
| Search | -5.5%, mixed | -4.0%, mixed |
| Repeated tabs | -11.8%, mixed | -30.9%, lower in all pairs |
| Unicode | +0.7%, mixed | +1.6%, mixed |

Linux dense CPU ranges from -15.3% to -5.3%; palette ranges from -24.5% to
-4.1%. Windows repeated-tab cycles range from -38.7% to -5.6%. Windows search
cycles decrease in every pair, but one change is only -1.7%, inside the guard.
Other workloads change direction across pairs. These controls do not establish
a general application speedup, FPS improvement, or input-latency reduction.

## Frame controls

Each OS also ran 18 instrumented captures: dense text, scroll, and search,
three pairs each. The profiler preserves the workload shell and recognizes
its readiness title. Every Windows capture sampled foreground ownership
throughout the workload; none recorded an unfocused sample.

Successful submitted render counts are comparable before and after. These
counts include startup and settling; they do not count displayed frames.

| Workload | Linux before / after, three pairs | Windows before / after, three pairs |
|---|---|---|
| Dense | 250/251, 247/252, 249/253 | 257/256, 254/256, 255/256 |
| Scroll | 254/254, 255/255, 254/255 | 488/483, 485/487, 485/486 |
| Search | 255/253, 253/252, 252/254 | 258/257, 258/256, 256/258 |

Linux dense `frame.screen` median wall spans improve in all three captures:
0.232 to 0.200 ms, 0.233 to 0.216 ms, and 0.218 to 0.206 ms. Scroll and search
spans are mixed. Windows median spans are mixed in all three workloads.
Windows dense p95 spans increase from 1.875 to 2.006 ms, 1.876 to 1.908 ms,
and 1.740 to 1.898 ms. These captures do not support a Windows frame-time or
smoothness improvement.

Profiler scopes are nested CPU wall spans, can include waits, and must not be
added together or interpreted as GPU execution time. Windows presentation and
submission waits remain substantial. Comparable render counts rule out simply
suppressing frames as the explanation for the focused CPU gains.

## Experiments removed

- A portable double-precision pixel-rounding helper matched all float bit
  patterns under the comparison policy but was slower.
- Moving an inline hint from the wide-character seam helper to the full cell
  writer regressed results. The original hint placement was restored.
- Splitting CPU glyph-rasterization misses into a cold function regressed.
- Grid glyph-lookup inline hints gave mixed results across 20 row cases.
- Small-vector combining-mark variants removed 19,784 warmed allocations
  and 257,192 allocated bytes per Unicode replay. Unicode, mixed, ANSI, and
  CSI timings traded places, so the storage changes were removed.

Nine opaque Windows cmd startup captures compared explicit Vulkan, DX12, and
the default backend. First-frame medians were 1,617, 1,737, and 2,264 ms,
respectively, with a large first Vulkan outlier. DX12 was not a repeatable
win over explicit Vulkan. Transparency also constrains supported alpha modes.
No backend-selection policy was changed. These captures explore configuration;
they do not measure startup improvements from the retained code.

## Validation and excluded measurements

The Sugarloaf library run passes 119 tests on each OS, with eight ignored.
Across the paired UI probes, all 24 raster comparisons match. Positioned
render checks cover negative and fractional origins, scale factors 1, 1.25,
and 2, bold and italic text, Unicode, emoji, and color glyphs.

An initial Windows source-staging script preserved an old modification time.
Cargo reused the baseline background-row library, producing identical before
and after row-probe executable hashes. Those row measurements are an A/A
control and are excluded, along with the corresponding old application runs.
The affected source was rewritten, the library rebuilt, and both row probes
and application controls rerun. Build logs and executable hashes verify the
replacement. The original Windows UI probes and Linux runs were unaffected.

The valid Windows row hashes are
`d97010467de609c66672b87bb7d954a388b9f1974c494c02fa2e58af9833c655`
before and
`015725e9665772cd8bda3e50cad7e487d0131622cb1eb628c58eae1ce9e5c646`
after. Superseded runs remain outside the report dataset.

Raw local artifacts are under `target/optimization-pass`,
`target/optimization-frame-controls`, and
`target/windows-optimization-corrected`. They include executable provenance,
sample logs, allocations, application records, and frame captures. They are
ignored build artifacts, not committed fixtures. The committed benchmark
cases and runner instructions are in [misc/benchmarks](../benchmarks/README.md).
