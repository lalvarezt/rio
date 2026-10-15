# Windows branch integration audit

Reviewed `git diff cc1f59432e...9eee9edb2b`, covering all 24 Windows commits,
91 changed files and 30 Rust source files. Of the changed files, 38 are SVG
assets. Standards and behavior reviews ran independently, alongside a review
of redraw costs and integration seams. This is a source review with focused
native validation, not a claim of maximum throughput.

## Standards

No hard documented-standard violations were found. The initial pass identified
three integration issues:

1. Transport detection duplicated command normalization and aliases between the
   context manager and icon catalog. The context manager recognized `ssh` and
   `mosh`, while the catalog also recognized `mosh-client`. Because transport
   app icons are suppressed in favor of the SSH badge, that alias could show
   neither. The catalog now resolves the app icon and transport identity together.
   `RIO_SSH` remains authoritative evidence of a remote shell, and changing an
   SSH command's icon does not remove its badge.
2. Each redraw rebuilds tab presentations, clones terminal metadata and resolves
   app names. Normalization previously allocated even for lowercase command
   names and allocated again to strip launcher suffixes. It now borrows those
   names, allocating only when ASCII case conversion is necessary. Title fallback
   no longer eagerly allocates a candidate vector and copies every candidate.
   Terminal snapshots and formatted status titles still allocate. A presentation
   cache would need measured benefit and explicit invalidation for shell metadata,
   focused splits, config reloads and status changes.
3. The shared SVG cache emitted a shape for every equal-color pixel run on each
   redraw, holding the cache mutex while drawing. The atlas integration below
   removes that work while preserving ordering, partial occlusion, opacity and
   CPU rendering. Its timing comparison measures the resulting improvement.

Stable tab IDs, Windows-specific animation state, the shared SVG renderer and
shared immutable shaped text each have a clear purpose. The old Neovim-specific
renderer has already been removed. Non-Windows drag springs are compiled out on
Windows. The later full-branch pass below integrates SVGs into the existing UI atlas.

## Spec

One remaining behavior gap affects pipeline-driven interactive tools. Bash's
fallback preexec hook reports the first eligible command, and PowerShell selects
the first command AST. Thus `printf ... | fzf` or `Get-Content list | fzf` can
report the producer rather than `fzf`. Adding a catalog entry does not discover
the consumer. Existing authoritative-report precedence prevents a later title
hint from correcting that identity.

This requires a focused shell-reporting change. Treating unknown app reports as
permission to reuse a title would resurrect stale editor icons. Process polling
would add work without solving remote process discovery. Neither workaround was
introduced here.

No other concrete behavior regression was found in the inspected native caption
actions, resize hit testing, tab geometry and transition paths, SVG/text clipping,
or profiling feature gates. This does not replace manual interaction coverage
across different Windows versions and graphics backends.

## Validation of the consolidation

The Windows fast debug build passed in 14.85 seconds, producing
`target/debug/rio-app-icons-inspect.exe`. An ignored native checker exercised
catalog aliases, case and launcher normalization, versioned runtimes, title
fallback, stale-title protection, config merging and icon overrides.

Transport checks cover `ssh`, `SSH.EXE`, `mosh`, `mosh-client` and a Windows
`MOSH-CLIENT.CMD` path. WSL stays local without remote metadata; custom icon-only
mappings do not invent an SSH connection; overriding `ssh`'s icon preserves its
transport identity. Twenty thousand steady lowercase resolutions, including a
launcher suffix, perform zero allocations after catalog initialization.

The existing asset checker also passes for 38 SVGs at six sizes. This audit adds
no repository tests and makes no frame-rate or GPU throughput claim.

Standards: transport duplication and SVG shape emission are removed, and lookup
allocation is reduced. Presentation rebuilding remains a profiling target.
Spec: one pipeline-reporting gap remains documented.

## Full-branch consolidation

The second pass reviewed all 26 commits from `cc1f59432e` through `7f1d1fc743`,
including the navigation fixes below. Earlier standards and behavior reviews
checked the branch. The final atlas patch received source inspection and native
validation; an additional independent review could not start because the provider
had reached its usage limit.

### Standards and integration

Tab title padding now has one calculation for measuring and drawing. Screen
dragging and release share one platform layout dispatch. No hard documented
standards violations were found.

SVGs now use the existing UI color atlas and text instances. This removes the
global cache mutex, cached pixel-run vectors, per-run rectangle emission and
icon-specific clipping and drawing-order parameters. The existing tab text
exclusion crops icons too. Each cached logo emits one instance, with the same
premultiplied tint and opacity on WGPU and CPU. Ordinary color glyphs retain
their original opaque-white tint.

Atlas slots belong to the window's renderer. A small icon-key map keeps artwork
identity separate from font glyph keys. WGPU text initialization now happens
before frontend drawing, beside CPU initialization. Leaving it until the render
pass caused eight failed cache attempts and a ninth rasterization on the next
frame in the eight-tab workload. Native profiling confirms one rasterization
after the move. Upload itself takes less than 0.5 ms in those runs.

The fixed WGPU atlas still falls back when full. Repeated uncached requests can
then rasterize again, as with the existing glyph-atlas capacity limitation. No
eviction or extra presentation-invalidation system was added without evidence
that normal use needs it. Application lookup and title fitting cost about
1 microsecond and 0.5 microseconds per call respectively in this workload.

### Behavior

The review found a hit-testing mismatch after reordering: a floating tab paints
above its neighbors, but reverse-index input lookup could select the tab beneath
it. Both hover and click lookup now give the floating tab the same priority as
drawing. Its hidden close button is not an input target. A throwaway native
replay using the actual layout and hit-testing methods fails before this fix
and passes afterward.

The pipeline-consumer reporting gap described above remains. No other concrete
behavior regression was found in the current consolidation.

### Native validation and timing

The focused catalog and asset checker covers all 38 SVGs at six sizes, including
gradient colors and premultiplied alpha. CPU checks cover tint, fading, partial
atlas-coordinate clipping, renderer-local caches and invalid raster fallback.
Ten thousand cached icon draws allocate zero times. GPU and CPU captures retain
the navigation and closing fixes, with colored icons and partial fading intact.
Both capture sets pass the ordinary-navigation and rounded-closing-edge checks.
Diagnostic helpers remain under ignored `target/`; no repository tests are added.

Three runs per build use eight cmd tabs with a Neovim logo override and 360
frames per run. All readiness and frame-count gates pass. No compilation or
screenshot capture runs during timing. The table reports medians of per-run
statistics, comparing `7f1d1fc743` with the final atlas and initialization path.

| SVG workload, ms | Before | After |
| --- | ---: | ---: |
| Tab drawing median | 0.840 | 0.503 |
| Tab drawing p95 | 1.333 | 0.932 |
| Screen drawing median | 3.760 | 3.316 |
| Frame interval median | 16.608 | 16.619 |
| Frame interval p95 | 19.740 | 19.617 |

Steady tab drawing falls about 40%, with a 30% reduction at p95. Both builds
remain near 60 Hz. First tab draws range from 5.990–14.214 ms before and
13.280–35.971 ms after. Early initialization also makes glyph uploads possible
on the first frame, so these scopes do different work; these batches establish
no cold-start improvement. They also do not resolve the intermittent surface
waits described below. Timings are CPU wall-clock measurements from debug builds.

The final profiles are under `target/profiles/svg-timing-init`; baseline profiles
are under `target/profiles/svg-timing-before`. Native captures are under
`target/profiles/tab-regression-atlas-gpu` and `tab-regression-atlas-cpu`.
The normal Windows debug build also passes, producing `target/debug/rio.exe`.
The inspection executable remains `target/debug/rio-app-icons-inspect.exe`.

## Two-tab navigation and closing follow-up

Native Windows captures reproduced two separate visual regressions after
`d2ac829557`. With two tabs, ordinary next/previous selection produced duplicate
pill fragments and discontinuous movement. Closing either tab cut the pill's
right edge into a vertical rectangle.

The wrap check tested endpoint indices without considering direction. With two
tabs every selection change reaches the other endpoint. It now requires an
actual crossing in the requested direction: next from last to first, or previous
from first to last. Ordinary moves use the existing direct transition.

Selection clipping used the new target strip width while the pill was still
moving and shrinking. Circular copies still clip to their wrap period; other
transitions clip to the available header width, preserving the rounded edge.
This also removes the duplicate stored strip width.

The native capture loop exercises two-tab ordinary moves, both wrap directions,
rapid reversal, closing the first and last tabs, mouse closing and three-tab
wraparound. An ignored image-analysis command distinguishes split ordinary
transitions and flat closing edges:

```sh
node target/analyze-tab-regression.js target/profiles/tab-regression-before --check
```

The original capture fails both checks. A navigation-only build passes the
ordinary-move check while closing still fails. The combined fix passes both.
Additional captures and temporary geometry traces retain all three tab
presentations through wraparound. Diagnostic helpers and captures remain under
ignored `target/`; tracing is removed from the application source.

### Timing comparison

Three static and three animated runs per build each complete 240 frames with
two cmd tabs: 2,880 measured frames across both builds. Animated runs repeatedly
select next/previous tabs, including reversals. Readiness and frame-count checks
pass. These are fast debug builds using the existing opt-in profiler, with no
compilation or screenshot capture during timing runs. The table reports medians
of the three per-run statistics, except the final row, which is the largest
observed gap.

| Animated workload, ms | Before | After |
| --- | ---: | ---: |
| Tab drawing median | 0.254 | 0.402 |
| Tab drawing p95 | 0.574 | 0.789 |
| Frame interval median | 16.696 | 16.710 |
| Frame interval p95 | 18.436 | 18.907 |
| Largest frame interval | 76.210 | 86.389 |

These batches show no throughput improvement and do not establish a sustained
tab-rendering bottleneck. Both builds usually present at about 60 Hz, but both
have occasional long frame gaps. One updated run records a 64.195 ms surface
acquisition scope. These are CPU wall-clock timings, which include driver waits
and OS descheduling; they do not identify GPU execution time or prove system
load is the cause. Overall system CPU load varies from 6.8% to 27.9% across runs.
The intermittent stalls remain a separate performance investigation.

Timing artifacts are under `target/profiles/tab-timing-before` and
`target/profiles/tab-timing-after`. Visual captures are under
`target/profiles/tab-regression-before`, `tab-regression-after-nav` and
`tab-regression-after`. The normal inspection executable is
`target/debug/rio-tab-inspect.exe`. No repository tests or runtime logging are
added by these fixes.
