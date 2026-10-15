# Windows branch integration audit

Reviewed `git diff cc1f59432e...9eee9edb2b`, covering all 24 Windows commits,
91 changed files and 30 Rust source files. Of the changed files, 38 are SVG
assets. Standards and behavior reviews ran independently, alongside a review
of redraw costs and integration seams. This is a source review with focused
native validation, not a claim of maximum throughput.

## Standards

No hard documented-standard violations were found. Three judgment calls identify
useful follow-up work:

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
3. The shared SVG cache avoids repeated rasterization but emits a shape for every
   equal-color pixel run on every redraw, holding the cache mutex while drawing.
   Colored logos can generate many runs. This is potential overhead, not a measured
   regression. The existing image-rectangle interface hardcodes drawing order zero;
   using it directly would place icons underneath floating pills. A replacement
   must support ordering, partial occlusion, opacity and the CPU renderer together.

Stable tab IDs, Windows-specific animation state, the shared SVG renderer and
shared immutable shaped text each have a clear purpose. The old Neovim-specific
renderer has already been removed. Non-Windows drag springs are compiled out on
Windows. No additional cache or renderer abstraction was added in this audit.

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

Standards: three findings, with transport duplication fixed and lookup allocation
reduced; presentation rebuilding and SVG shape emission remain profiling targets.
Spec: one pipeline-reporting gap remains documented.

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
