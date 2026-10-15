# Rendering pipeline audit

This audit starts at `d072658df3` and follows terminal damage through
presentation. Measurements use native Windows debug binaries. Runtime changes
are compared with identical dependency optimization settings. Probes and raw
data remain under ignored `target/`; no repository tests are added.

## 1. Damage and snapshot preparation

Damage notifications coalesce while an event is in flight. Clean/cursor-only
snapshots avoid cell copies, partial snapshots copy dirty rows, and full
snapshots reuse existing row allocations. These mechanisms remain in place.

Unstyled rows previously materialized a default `Style` for every column.
Snapshots now leave those inner vectors empty, matching the existing
`resolve_style` fallback. Styled rows still snapshot resolved values, preserving
style-ID reuse safety. `Style::DEFAULT` is the shared constant behind `Default`
and fallback resolution. Foreground emission resolves an unstyled row's normal
foreground once; selection, hints, IME and color-atlas rules retain their paths.

Three interleaved rounds, each with seven batches of 300 full snapshots:

| Snapshot | Before, µs | After, µs |
| --- | ---: | ---: |
| 120×35 plain | 28.546 | 3.055 |
| 240×70 plain | 54.462 | 7.181 |
| 240×70 styles and extras | 276.025 | 226.774 |

Allocation reuse remains intact. Plain rows no longer allocate style vectors
on their first snapshot. Existing vectors retain capacity when a styled row
becomes unstyled, avoiding allocation churn on later style changes.

A separate row-emission probe uses seven batches of 1,000 calls and 7,001
distinct changing fixtures. Snapshot setup is outside timing. Before/after
medians are 7.024/6.241 µs for short cache hits, 43.843/41.434 µs for long cache
hits and 132.636/138.537 µs for changing long rows. The last case is about 4%
slower in these host-loaded batches; no cold-row improvement is claimed here.
The next row-construction stage investigates its remaining per-cell work.

All 20 background/foreground stream hashes match in each of three rounds,
including full 64-bit hashes, warm emissions, selection, hover and IME. Snapshot
probes also pass scrolling, shrinking/growing, metadata, styles and extras
comparisons. Artifacts: `target/profiles/pipeline-default-final`.

## Remaining stages

Screen preparation and scratch buffers, overlays, background emission, font
resolution, shaping keys, glyph rasterization/atlases, foreground emission,
cursor streams, GPU uploads/uniforms, draw encoding, presentation and resize
lifetime are being inspected in order. Changes require measured benefit and
output validation; an inspected stage does not require a speculative rewrite.
