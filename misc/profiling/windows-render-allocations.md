# Terminal snapshot and WGPU grid allocations

This pass starts at `1c4a6793e3`. Static analysis identified two costs outside
the Windows tab bar that could be removed through existing implementations.
The earlier mailbox change already applies to presentation of the whole window;
other animations do not need their own copy of that logic.

## Static findings and measurement choices

1. `Crosswords::snapshot_visible` cleared its destination on full damage, dropping
   every row's cell allocation before cloning the source rows. Partial snapshots
   already reused storage. The existing `fill_visible_rows` helper provides that
   same reuse for full snapshots, including viewport size changes. The prediction
   was fewer allocations and less snapshot time under repeated full damage.
2. `WgpuGridRenderer` allocated three CPU background vectors, three background
   GPU buffers and three foreground GPU buffers. Every read, write and draw used
   index zero; the frame counter was unused. The prediction was a two-thirds
   reduction in those allocations without changing upload or draw behavior.
3. Cursor changes can rebuild and upload the foreground stream. Splitting that
   upload would introduce more dirty state and draw ranges. This pass leaves it
   unchanged; neither the static inspection nor these workloads establishes it
   as a useful next optimization.

## Snapshot benchmark

An ignored native Windows probe calls the real `snapshot_visible` implementation.
Each case warms its destination, then measures seven batches of 300 full
snapshots. A counting allocator records allocation calls and requested bytes
inside the measured loop. The table reports the median batch time per snapshot.
These are fast debug builds, not release throughput estimates.

| Viewport and content | Time before / after, µs | Allocations before / after | Bytes before / after |
| --- | ---: | ---: | ---: |
| 120×35, plain | 42.4 / 15.9 | 35 / 0 | 33,600 / 0 |
| 240×70, plain | 74.5 / 48.7 | 70 / 0 | 134,400 / 0 |
| 240×70, styles and extras | 357.1 / 321.3 | 104 / 34 | 134,536 / 136 |

The change removes row allocations in every case. Rich content still clones
combining-character and hyperlink extras, which accounts for the remaining
small allocations. It does not introduce another cache or invalidation rule.

The same probe compares reused snapshots against fresh full snapshots after
scrolling, shrinking, growing and changing content. It checks cells, resolved
styles, extras, row metadata and dirty flags, then checks cursor-only snapshots.
All comparisons pass before and after. The allocation check fails on the
baseline and passes after reuse:

```sh
node target/analyze-snapshot.js target/rio-snapshot-after.jsonl --check
```

## Native WGPU measurements

Three runs per build open a cmd-hosted dense-text fixture using Vulkan. Each run
completes 360 frames and processes 29 grid dimension changes. Temporary probes
record the actual CPU vector capacities, GPU buffer sizes and elapsed grid
resize setup time. No compilation or screenshot capture runs during measurement.

At the final 186×43 grid size:

| Allocated storage, bytes | Before | After |
| --- | ---: | ---: |
| CPU background cells | 95,976 | 31,992 |
| GPU background cells | 95,976 | 31,992 |
| GPU foreground capacity | 767,808 | 255,936 |

These are cell-buffer allocations, not total GPU memory or process RSS. Glyph
atlases, pipelines and other renderer resources are unchanged. Every measured
dimension has the expected three-to-one reduction.

The median of the per-run grid-resize medians falls from 547 to 413 µs, about
24%. Median frame intervals remain about 16.4 ms. Screen-render medians are
2.79 ms before and 2.94 ms after; these batches do not show an overall frame-time
improvement. The savings are in allocation churn, grid storage and resize setup.

## Validation and artifacts

Only Windows debug builds are used. The normal inspection executable is
`D:\git-repos\rio\target\debug\rio-render-inspect.exe`; `rio.exe` receives the
same executable. The final build passes in 27.52 seconds. Native captures retain
dense text and colors through resize, tab creation and returning to the previous
tab, with opaque and blurred transparent backgrounds.

No repository tests,
runtime counters, logging or public interfaces are added. The runtime change
removes 11 Rust lines and reuses the existing row-copy implementation.

Benchmark helpers and raw results stay under ignored `target/`:
`rio-snapshot-{before,after}.jsonl`, `rio-snapshot-*.check.log`, and
`profiles/grid-profile-{before,after}`. The temporary grid instrumentation is
removed from the final source and executable. Final visual captures are under
`profiles/grid-capture-final`.
