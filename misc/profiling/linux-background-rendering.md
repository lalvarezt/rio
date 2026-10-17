# Linux background rendering pass

This pass starts from the prepared tree described in
[the previous Linux pass](linux-rendering-pass.md), including its mixed-input
and Vulkan text-packing changes. Results here measure additional changes.

The production change is confined to native Vulkan background handling:

- Unchanged cell backgrounds retain their GPU buffers instead of marking every
  frame slot dirty again.
- A count of nonempty background rows allows the renderer to omit its fullscreen
  background draw when every cell is transparent and no block cursor needs it.
- When a draw is required, transparent fragments return before color conversion.

Uniform buffers still update before the draw is skipped because the text pass
shares those uniforms. Colored backgrounds and block cursors retain their draw.
The row bookkeeping follows the existing WebGPU implementation. It adds one
counter per grid and no allocation in the frame loop.

## Measurements

The machine is the same i7-1370P with Intel Iris Xe used in the previous pass.
Processes are pinned to CPU 2. Release builds use identical settings with LTO
disabled, 16 codegen units, and debug information retained. All work is local
to Linux.

CPU sampling attaches after the shell readiness marker. This excludes startup
font loading and window creation. Fixtures cover dense output at 300 updates
per second, scrolling, four-tab transitions, and palette navigation. The
dominant CPU work remains row preparation, shaping, and glyph lookup.

Several glyph-lookup experiments were rejected. Splitting and inlining the
cache-hit path made cached row preparation 26–36% slower. A smaller return value
also regressed those rows. Inlining only the renderer dispatch methods and
preventing native-rasterizer inlining produced no useful improvement. None of
these changes remain in the production code.

Raw input was measured in three alternating pairs against the preceding tree.
ASCII, styled ANSI, CSI, Unicode, mixed Unicode and ASCII, and OSC throughput
remained within approximately 2%. All 36 paired output hashes matched.

### GPU timing method

A temporary probe calls the actual `VulkanGridRenderer` methods and uses Vulkan
timestamp queries around 16 background-pass calls at 1440 × 900. Clear, short
row replacement, resize, transparent cells, colored cells, and block cursors
exercise both drawing and skipping. Repeated draws isolate GPU work from
presentation waits. Residual time when no draws are recorded includes query
and render-pass overhead divided across those calls.

GPU clock changes were substantial, so short elapsed-time comparisons are
insufficient for the shader change. A separate control build keeps the row
bookkeeping but uses the original shader. Longer opaque and cursor workloads
discard 30 warmup frames and collect 60 frames per process, recording the
requested GPU frequency before and after submission. No clock settings or
system governors are changed.

### GPU results

The original transparent pass took about 374 µs per call in the final mixed
probe. The updated renderer records no background draw in that case. Residual
query and render-pass time was about 0.9 µs per call. Clearing rows, replacing
colored rows with short transparent rows, and resizing also reached this path.

For draws that remain, the table compares samples with the same reported
frequency before and after submission. The control uses the original shader.

| Background | Reported MHz | Control µs | Updated µs | Samples per revision |
| --- | ---: | ---: | ---: | --- |
| Transparent cells with a block cursor | 300 | 561.1 | 363.0 | 8 and 14 |
| One colored cell per row | 300 | 561.0 | 363.1 | 7 and 14 |
| Every cell opaque | 350 | 481.2 | 482.3 | 26 and 47 |
| Every cell opaque | 383 | 421.0 | 422.2 | 16 and 20 |
| Every cell opaque | 417 | 374.5 | 375.2 | 20 and 12 |

The transparent-fragment shortcut reduced the sparse background pass by about
35%. Fully opaque backgrounds stayed within 0.3% at matching reported clocks.
These numbers describe the background pass, not total frame latency or FPS.
Reported frequency is a diagnostic comparison filter, not a hardware lock or
a direct measurement of every clock transition during a draw.

### Application CPU and frame results

Three alternating native application pairs covered dense repaint, single-row
updates, scrolling, cursor movement, tabs, and palette navigation. Each active
terminal fixture produced 120 updates at 60 Hz. Tab fixtures switched four tabs
30 times and rendered 96 frames in both revisions. Palette fixtures opened the
palette, moved selection 30 times, and rendered 39 frames in both revisions.
Animation timing and input behavior were unchanged.

Frame wall times varied enough to reverse apparent gains in some workloads.
A second three-pair comparison used `perf stat` after shell readiness. The
following medians measure user CPU time across application threads, including
driver threads, during approximately two seconds of output. They exclude the
separate fixture producer.

| Workload | Baseline user CPU ms | Updated user CPU ms | Baseline instructions, millions | Updated instructions, millions |
| --- | ---: | ---: | ---: | ---: |
| Dense repaint | 92.08 | 65.22 | 822.8 | 726.3 |
| Single-row updates | 37.98 | 35.58 | 172.4 | 146.1 |
| Scrolling | 74.22 | 70.91 | 926.0 | 924.7 |

Dense repaint used about 29% less user CPU time in these runs. The other CPU
results were smaller and varied between runs. These short workloads establish
neither a universal CPU reduction nor a frame-rate improvement.

## Correctness checks

Native screenshots matched exactly for three static fixtures: colored
backgrounds, transparent backgrounds with a bar cursor, and transparent
backgrounds with a block cursor. The fixtures include Unicode, emoji,
underlines, and foreground colors. All three comparisons had zero differing
pixels.

The GPU probe exercised the actual write, clear, and resize methods, including
empty trailing cells after a short row replacement. Cleared and resized grids
omitted the background draw. Nonempty backgrounds and block cursors retained it.
Raw-input output hashes matched, and the native profiling build, Rust format
check, and Git whitespace check passed. No permanent tests were added.

## Local artifacts

The temporary probes, binaries, and full logs are under `target/linux-pass2/`.
They are excluded from version control. The existing profiling application is
built with:

```sh
CARGO_TARGET_DIR=target/linux-perf/build \
CARGO_PROFILE_RELEASE_LTO=false \
CARGO_PROFILE_RELEASE_DEBUG=1 \
CARGO_PROFILE_RELEASE_STRIP=none \
CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 \
cargo build --release -p rioterm --bin rio-profile --features profiling --locked
```

`profile-baseline.log` and its `perf.data` files capture steady-state CPU
profiles. `raw-final.jsonl` records the raw-input pairs. `native-final.log`
records the frame workload matrix. `cpu-stat-final.log` and each run's
`perf-stat.csv` record the CPU comparison. `gpu-*-opaque-*.jsonl`,
`gpu-*-cursor-*.jsonl`, and `gpu-*-all-*.jsonl` contain GPU queries and clock
samples. `visual-final.log` identifies the screenshot runs.
