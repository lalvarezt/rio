# Linux rendering performance pass

Baseline: `8385ecbd4d` (`Bundle ConPTY 1.25 and add Windows install recipes`).
Measurements were taken locally on Linux with an i7-1370P and Intel Iris Xe.
Benchmark processes were pinned to CPU 2. Both revisions used the same release
settings: LTO disabled, 16 codegen units, debug information retained, and the
default native Vulkan renderer. No Windows build or machine was used.

## Changes

Decoded Unicode batches can contain long printable ASCII runs. With grapheme
clustering enabled, those runs previously used the scalar character path.
They now use the existing narrow-run writer. Single ASCII characters and
Unicode characters retain scalar handling; insert mode, mapped character sets,
image placements, and disabled line wrapping retain their existing fallback.
This follows the terminal's existing rule that printable ASCII does not extend
a preceding grapheme cluster.

Native Vulkan text packing now retains its bucket storage and packs a changed
layout once for all frame slots. A single-atlas layout copies rows directly
into staging. Mixed-atlas layouts preserve the original first-seen bucket order
and cell order. Each GPU frame slot still uploads independently after its fence
is available. Content, cursor, resize, and atlas invalidation dirty both the
canonical CPU layout and every GPU slot.

## Isolated measurements

Seven alternating before/after pairs were measured after warmup. Raw input used
256 KiB batches and the real parser and terminal, with a counting parser sink
measured separately. Values below are medians, in decimal MB/s.

| Input | Baseline | Updated | CPU time change |
| --- | ---: | ---: | ---: |
| ASCII | 1,015 | 1,016 | -0.1% |
| ANSI styles | 284 | 274 | +3.6% |
| CSI sequences | 205 | 204 | +0.5% |
| Unicode-heavy | 140 | 144 | -2.7% |
| Unicode followed by long ASCII runs | 148 | 418 | -64.6% |
| OSC title/status sequences | 308 | 333 | -7.5% |

The mixed-input gain is specific to the formerly scalar path. The ANSI-heavy
case consistently became slightly slower; these results do not establish a
universal input speedup. Parser-only results stayed within approximately 2%.
The parser implementation was unchanged.

The Vulkan packing probe extracted the actual before/after packing bodies and
used real `CellText` values: 60 rows × 160 cells. It checked packed cells and
bucket layouts for equality before timing seven alternating pairs.

| Atlas buckets | Baseline µs | Updated µs | CPU time reduction | Allocations per pack |
| --- | ---: | ---: | ---: | ---: |
| 1 | 20.63 | 8.49 | 58.9% | 15 → 0 |
| 2 | 32.68 | 25.98 | 20.5% | 27 → 0 |
| 8 | 38.83 | 35.25 | 9.2% | 90 → 0 |

These times measure CPU packing only. They exclude GPU uploads and presentation.
They also exclude the additional saving from reusing a packed layout when the
next frame slot requires an upload without another content change.

## Native application measurements

An isolated KWin virtual display with Xwayland exercised the actual Intel
Vulkan renderer at 1440 × 900. Each active terminal fixture produced 120 updates
at 60 Hz. Three alternating before/after pairs covered each workload. Tab runs
switched between four tabs 30 times; palette runs opened the palette and moved
the selection 30 times. Keys were sent to Rio's client window, located by PID.
Tab runs rendered 95–96 frames, confirming that the transitions were exercised.

The table shows the median of each run's median `frame.screen` wall time.
It includes terminal preparation and presentation, rather than CPU packing alone.

| Workload | Baseline ms | Updated ms |
| --- | ---: | ---: |
| Dense repaint | 0.373 | 0.369 |
| Single-row updates | 0.182 | 0.153 |
| Scrolling | 0.430 | 0.566 |
| Moving cursor | 0.136 | 0.088 |
| Unicode row updates | 0.116 | 0.081 |
| Tab transitions | 0.102 | 0.079 |
| Palette navigation | 0.035 | 0.035 |

Scrolling initially looked worse. Five additional alternating comparisons
included a build with only the Vulkan change: baseline 0.478 ms, Vulkan-only
0.442 ms, combined changes 0.402 ms. The direction reversed, so the initial
matrix does not establish a scrolling regression or a reliable overall frame
speedup. Presentation and scheduling variation are substantial at these small
durations. The isolated CPU measurements above are the stronger evidence for
the retained changes. No animation frame rate or duration was changed.

## Correctness checks

- A temporary equivalence probe compared 128 deterministic terminal cases
  against the baseline. It covered narrow grids, wrapping, wide-cell overwrite,
  combining marks, emoji sequences, styles, hyperlinks, insert mode, alternate
  screen, mapped character sets, and disabled clustering. All snapshots,
  styles, extras, and cursor hashes matched.
- All raw-input fixtures produced matching before/after output hashes across
  the seven repetitions.
- Packing probes asserted identical cell data and bucket layouts for one,
  two, and eight atlas buckets, plus repeated changes in bucket order and
  transitions through empty layouts.
- Native before/after screenshots of a static fixture containing colors,
  Unicode, emoji, underlines, and a visible cursor had zero differing pixels.
- The existing VT suite passed 550 tests. Its
  `resolver_fast_path_matches_slow_path` test failed identically with the
  optimization reverted: the default-style fast path returns an empty style
  vector while the test expects three explicit default styles. That baseline
  failure is outside this change.
- With that baseline failure excluded, all 550 remaining VT tests passed.
  The benchmark target compiled, formatting checks passed, and the profiling
  application built successfully with the native Vulkan renderer.

## Scope of the pipeline review

| Stage | Finding and action |
| --- | --- |
| Byte and escape parsing | Measured ASCII, ANSI, CSI, Unicode, and OSC separately; retained the existing parser. |
| Terminal writes | Found and optimized mixed Unicode/ASCII batches through the existing writer. |
| Damage and snapshots | Measured full, single-row, and cursor snapshots; retained existing reuse. |
| Row preparation and shaping | Measured cached and changed plain/styled rows. A foreground-color cache did not reliably improve results and was removed. |
| Native Vulkan upload preparation | Removed repeated bucket allocation and duplicate CPU packing across frame slots. |
| Tab and palette frames | Exercised real keyboard interactions in the isolated display; retained current animation behavior. |
| Presentation | Measured complete native frames separately from CPU kernels; presentation includes driver and scheduling variability. |

## Reproduction

The existing `rio-vt` benchmark now includes `unicode` and `unicode_ascii`
fixtures in addition to its ASCII and escape-heavy cases:

```sh
taskset -c 2 cargo bench -p rio-vt --no-default-features --bench vt_input -- unicode
```

The pass's temporary probes, baseline binaries, full measurement logs, and
isolated-display driver are retained locally under `target/linux-pass/`.
`raw-final.jsonl` contains all raw-input pairs; `pack-fast-results.jsonl`
contains all packing pairs; `native-final.log` contains the native workload
matrix. These generated artifacts are excluded from version control.
`scroll-final.log` contains the focused comparison and `check-*.jsonl` the
terminal equivalence hashes.
