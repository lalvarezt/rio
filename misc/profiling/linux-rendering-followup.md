# Linux rendering performance follow-up

This pass investigates the Unicode-input, cached-text, and native Vulkan
background results in the [original upstream comparison](linux-branch-performance-report.md).
It changes the shared input and UI-text paths. The Vulkan shader is unchanged.

## CPU changes

With grapheme clustering enabled, Unicode spans now use the scalar input loop
until printable ASCII needs run detection. ASCII runs still use the bulk writer.
Combining characters, wrapping, and graphics retain their existing handlers.

UI text stores the width with its immutable shaped run. Measurement and drawing
reuse it instead of summing glyph advances again. Drawing snaps the shared
baseline once and still rounds individually positioned glyphs. Cached atlas
lookups stay in the glyph loop. This removes call overhead without another cache
or a second rendering implementation.

The implementation commit is `88cf341b91`, following `e7d01e24fe`. The comparison
uses the same benchmark fixtures before and after these production changes.
The retained upstream-comparison VT executable has the same pre-change input
code. All executables use matching optimized benchmark profiles, CPU 2,
11 samples targeting 100 ms, and three alternating pairs. Each table value is
the median of the three per-pair medians. Percentage ranges use paired ratios.

| Operation | Before | After | Paired time change |
| --- | ---: | ---: | ---: |
| Unicode input, µs | 1964.510 | 1826.987 | -7.5% to -6.8% |
| Mixed input, µs | 589.931 | 579.151 | -2.1% to -1.6% |
| ASCII input, µs | 251.528 | 249.751 | -1.0% to -0.2% |
| ANSI input, µs | 810.882 | 815.659 | +0.5% to +1.1% |
| CSI input, µs | 1076.756 | 1072.806 | -0.5% to +0.7% |
| Draw cached short label, ns | 155.710 | 104.872 | -33.4% to -25.5% |
| Draw cached long label, ns | 597.017 | 369.922 | -40.1% to -37.4% |
| Draw cached Unicode label, ns | 150.782 | 104.450 | -31.0% to -29.4% |
| Measure cached short label, ns | 16.238 | 15.134 | -10.0% to -5.7% |
| Measure cached long label, ns | 28.696 | 15.834 | -47.4% to -44.7% |
| Measure cached Unicode label, ns | 17.318 | 15.386 | -11.5% to -10.5% |
| Measure with 4096-title churn, ns | 7310.216 | 7298.350 | -0.8% to +0.3% |

Input hashes and UI raster hashes match before and after in every pair.
Allocation counts and bytes are unchanged. Cached UI drawing and measurement
have zero warmed allocations. These are operation timings, not display latency.

The 2048-entry shape-cache bound remains. Cycling sequentially through 4096
titles still evicts and reshapes them. Increasing the bound would move the same
failure to a larger working set. This remains a documented memory/performance
tradeoff, not a claim that every title workload is faster than upstream.

## Native Vulkan clock diagnosis

The original timestamp probe records 16 background draws per interval. Repeating
the same executables produced distinct timing bands, including roughly 425 and
639 µs for opaque draws. Alternating executable order did not remove them.

A temporary diagnostic sampled the Intel GPU's requested clock after each
measured batch. With 256 draws per interval, every measured batch that issued
draws reported 1500 MHz in both revisions' diagnostic runs. The skipped-draw
case does not keep the GPU active. No power policy or frequency limit was changed.
Requested clock samples
do not establish constant instantaneous frequency, but the longer batches also
produced tightly grouped timestamps and repeatable comparisons.

Two upstream/branch pairs with the longer batches measured:

| Background | Upstream, µs/draw | Branch, µs/draw |
| --- | ---: | ---: |
| Sparse | 135.998–136.003 | 88.002–88.003 |
| Opaque | 136.487–136.508 | 136.779–137.125 |
| Cursor | 136.039–136.040 | 88.000–88.036 |

The opaque difference is below 1%. Sparse and cursor cases take about 35% less
GPU time. The large native Vulkan regressions in the original short probe are
not reproduced under these conditions.

Removing the transparent-cell early return was also measured at the same
reported clock. It raised sparse and cursor time to about 136 µs without a
meaningful opaque improvement. That experiment was discarded. The committed
probe now defaults to 256 draws and accepts `RIO_BENCH_GPU_DRAWS`; it does not
change GPU power settings. The temporary machine-specific logging was removed.

These timestamps cover repeated background draws, not complete frames. They
do not establish text-rendering latency or native framebuffer equivalence.

## Validation and reproduction

The existing resolver test expected an allocated default-style vector, although
`resolve_row_styles` documents an empty vector for default styles. That assertion
failed with the pre-change code too. The test now checks the documented contract
and verifies that reusing a styled output clears it.

Existing terminal tests pass, 561 total. Existing renderer tests pass, 119 total,
with eight performance probes ignored. No new unit tests were added.
The WGPU compilation check passes. Native Vulkan debug application replays pass
for Unicode output, four tabs with distinct contents, and command-palette
navigation. Each replay runs for four seconds and exits cleanly. These debug
runs validate integration; their CPU times are not an optimized performance claim.

The focused harness and raw pair logs are retained in `target/linux-followup/`,
with its driver at `target/linux-followup.py`. Optimized benchmark builds reuse
the original suite's profile settings. Application validation uses a debug build.

The committed suite includes cached-measurement cases and the longer native
GPU batches. To reproduce the upstream comparison with the updated fixtures:

```sh
python3 misc/benchmarks/run.py build
python3 misc/benchmarks/run.py run --runs 3 --samples 11 --ms 100 --cpu 2 --filter input/
python3 misc/benchmarks/run.py run --runs 3 --samples 11 --ms 100 --cpu 2 --filter ui_text/
python3 misc/benchmarks/run.py run --runs 3 --samples 11 --ms 100 --cpu 2 --filter gpu/background/
```

Each filtered run replaces `results.jsonl`; retain separate copies if all three
sets are needed. The full suite runs all areas in one invocation without `--filter`.
