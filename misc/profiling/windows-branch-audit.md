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
