# Terminal text shaping and row emission

This pass starts at `d93b872384`. Native Windows profiling identifies text
shaping and foreground row construction as the main CPU cost in dense terminal
updates. The changes reuse the existing shape cache and damage tracking.

## Hotspot

A cmd-hosted producer updates a 224×58 terminal at a requested 25 Hz. Temporary
spans separate snapshots, backgrounds, foregrounds and foreground uploads. A
deeper probe measures shape-cache hits and Swash construction, input and shaping.
The profiler runs dense repaint, sparse-update and scrolling workloads using
Vulkan. Each run must observe shell readiness, real row rebuilds and uploads;
an initial idle-shell attempt was rejected.

In the original unoptimized debug dense run, 7,018 foreground row builds consume
8,862 ms. Shaping accounts for 8,092 ms, about 91% of that time. Snapshot copying
uses 13.7 ms, backgrounds 49.8 ms and foreground uploads 19.8 ms. Changing every
row's counter produces 6,960 shaping misses and no hits. GPU uploads and terminal
snapshots are not the dominant cost in this workload.

## Runtime change

The shape cache now retains the glyph ID and its source-cell index together,
along with the run's ascent and summed advance. A shared immutable result remains
valid while emission updates the glyph atlases. This removes the second shape
cache lookup, repeated cluster-to-cell mapping, temporary emission vector and
separate ascent lookup on cache hits. IME composition uses the same result and
retains its advance-based overflow handling.

Each cached glyph occupies 4 bytes instead of 20. The discarded x/y/advance
fields were unused by terminal emission; the summed advance remains available
for IME. The 80% reduction applies to glyph payload, not total cache memory.
There is no additional cache or invalidation state. The unused `smallvec`
dependency is removed from `rio-grid`.

Foreground emission also combines a row's style flags once, skipping underline
and strikethrough passes when they cannot emit anything. Hyperlink hover still
forces the underline pass, and IME caret rendering remains independent.

## Matched runtime benchmark

An ignored native probe calls the actual `build_row_fg` with the default font
library and a CPU grid atlas. Snapshot parsing and fixture setup are outside
timing. Both binaries use identical debug package optimization settings, so this
comparison isolates the runtime change. Each case has seven batches of 50 row
builds, with three interleaved before/after rounds. Allocation sampling is
separate. The table reports medians of per-run median times.

| Row workload | Before, µs | After, µs | Time reduction | Allocations before / after |
| --- | ---: | ---: | ---: | ---: |
| Cached short, 12 glyphs | 17.916 | 6.522 | 64% | 0 / 0 |
| Cached long, 198 glyphs | 65.248 | 40.928 | 37% | 2 / 0 |
| Changing long, 222 glyphs | 132.042 | 118.376 | 10% | 10 / 9 |

Long cache hits previously requested 1,536 temporary allocation bytes per row;
they now request none. Changing long rows request 8,768 bytes instead of 18,384.
The changing case has no visible cursor splitting the run, and changes the whole
run on every call. These are CPU row-construction measurements in fast debug
builds, not release throughput or input-to-display latency estimates.

## Separate debug-build improvement

The development profile optimizes `swash` and `rio-grid` at level 2 while leaving
the frontend at level 0. Release settings are unchanged. Rebuilding these
dependencies initially takes 43.04 seconds. The normal Windows executable builds
in 28.83 seconds, followed by an 8.15-second rebuild after comment cleanup.

With the original runtime code, the dense UI run's foreground median changes
from 0.9724 to 0.2353 ms after this package tuning. The isolated changing-row
probe also shows about five times lower cost from tuning alone. This is a
development-build benefit and must not be attributed to the runtime refactor
or claimed as a release speedup.

Final UI profiling confirms real terminal activity across all three workloads.
Its foreground medians are 0.3236 ms for dense repaint, 0.0571 ms for sparse
updates and 0.1385 ms for scrolling. The dense median is higher than the earlier
tuned baseline, while sparse and scroll medians are lower. These individual UI
runs are noisy and do not establish a whole-frame improvement; the matched
interleaved row benchmark supplies the runtime comparison above. Profiler frame
intervals also include clean forced redraws, so they are not FPS measurements.

## Output validation

All 20 foreground/background stream checks match exactly before and after in
each of three confirming rounds. Comparisons preserve the full 64-bit hashes.
Fixtures cover long text, ligature-like sequences, styles and decorations,
combining/wide characters, emoji input and OSC 8 hyperlinks, each with plain,
selection, hover and IME states. Repeated warm emissions also match.

The normal Windows executable is captured at 1800×1000, after shrinking to
1200×800 and after restoring to 1800×1000. Terminal-body RGB output matches the
original executable pixel for pixel at all three sizes: zero changed pixels
across 4,193,728 compared pixels. This includes a steady visible cursor inside a
long text run. Window chrome and outer frame pixels are excluded. The installed
font's existing fallback behavior, including missing emoji ink in this capture,
is preserved.

No repository tests or runtime probes are added. Temporary counters, timing
spans and the profiling shell override are removed. Native formatting and the
normal Windows debug build pass. macOS compilation is not performed; its
CoreText adapter uses the same compact result without changing source offsets.

Ignored artifacts remain under `target/`: `profiles/terminal-render-before`,
`profiles/terminal-render-tuned-before`, `profiles/terminal-render-final`,
`profiles/terminal-kernel-final-compare`, and
`profiles/terminal-visual-{before,after}`. Helpers are `rio-terminal-*` and
`compare-terminal-visual.js`. The final executable is
`D:\git-repos\rio\target\debug\rio-render-inspect.exe`, also copied to `rio.exe`.
