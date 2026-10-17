# Branch performance changes relative to upstream

This inventory compares the final branch implementation with upstream
`59203ccd5fa1e6b760a738c1ad653bb29f2f3465`. That commit is both the branch's
merge base and upstream `main` when this inventory was prepared. The runtime
changes include `c21490ca1e`, the final Linux and Windows shader pass.

Each entry describes a change that survives in the final tree. Intermediate
implementations, reverted experiments, and comparisons between optimization
commits are excluded. Several commits can contribute to one entry. Existing
upstream optimizations are not credited to this branch.

The machine-readable coverage map is [changes.json](../benchmarks/changes.json).
The Linux measurements are recorded separately in
[linux-branch-performance-report.md](linux-branch-performance-report.md).

## Terminal input and snapshots

| ID | Final difference from upstream | Production code | Dedicated benchmark cases |
|---|---|---|---|
| P01 | Printable ASCII runs in decoded Unicode batches use the narrow-run writer while grapheme clustering is enabled. Scalar fallbacks remain for mapped charsets, insert mode, images, and disabled wrapping. | `Crosswords::input_codepoints` | `input/mixed`, with ASCII, Unicode, ANSI, and CSI controls |
| P02 | Full snapshots refill existing row allocations instead of replacing the row vector. | `Crosswords::snapshot_visible` | `snapshot/plain/full`, `snapshot/styled/full` |
| P03 | An empty style vector represents a row of default styles. Consumers resolve defaults without filling a vector per row. | `Grid::resolve_row_styles`, `rio_grid::resolve_style` | Plain and styled snapshot, foreground, and background cases |
| P04 | Screen preparation retains panel and row scratch across frames. | `Screen::render` | Real-window dense, sparse, and scroll replays |

Upstream already had SIMD text parsing, bulk ASCII writes, damage tracking,
partial snapshots, and glyph caches. The branch does not introduce those
systems. The existing VT benchmark remains a separate regression control.

## Terminal rows, shaping, and rasterization

| ID | Final difference from upstream | Production code | Dedicated benchmark cases |
|---|---|---|---|
| P05 | Cached shapes share immutable glyphs, source-cell mappings, and font metrics. Row emission avoids copying cached runs and recomputing cluster mappings for each glyph. Grid text and preedit use the same shape cache. Row-level flags and decoration state are prepared once. | `GridGlyphRasterizer`, `shape_cached`, `build_row_fg` | Cached, changing, selected, and rasterization rows with plain, styled, Unicode, combining, and preedit content |
| P06 | Shaping keys hash completed text and source-cell buffers once instead of maintaining a hash incrementally during run construction. | `shape_cached` | Changing plain, styled, and Unicode rows |
| P07 | Native glyph rasterization retains bitmap storage across glyph misses. | `ensure_glyph_by_id`, `rasterize_glyph_native` | The `row/*/rasterize` cases clear the actual atlas before rebuilding a warmed shape |

The row fixtures use a small deterministic palette to isolate grid preparation.
They do not measure the frontend's full theme resolver. The real-window replays
exercise that resolver. Font discovery and initial renderer construction happen
before row timing. Rasterization cases deliberately include atlas clearing and
glyph misses. Other row cases retain their atlases.

## GPU storage, uploads, and drawing

| ID | Final difference from upstream | Production code | Dedicated benchmark cases |
|---|---|---|---|
| P08 | WGPU drops unused duplicate cell storage and retains packed foreground storage. | `WgpuGridRenderer` | `wgpu/frame/full`, `wgpu/frame/unchanged`, with separate warmed allocation counts |
| P09 | Changed WGPU backgrounds and foreground rows determine upload ranges. A foreground length change invalidates the shifted suffix. | `write_bg_row`, `write_fg_row`, `render_text` | `wgpu/frame/one_row`, `wgpu/frame/full` |
| P10 | A zero-area block-cursor slot keeps resident WGPU text offsets stable when the block cursor disappears. | `WgpuGridRenderer::set_cursor` | `wgpu/frame/cursor_blink` |
| P11 | WGPU skips uniform writes when all uniform bytes match the previous frame. | `WgpuGridRenderer::render_bg` | Unchanged versus changing-uniform frames |
| P12 | WGPU and native Vulkan skip the background draw when no cell background or block cursor contributes. Background visibility follows changed rows, clearing, and resize. | Both grid renderers | Transparent and cursor-only background cases, plus unchanged WGPU frames |
| P13 | WGSL and native Vulkan background fragments return before color conversion when cell alpha is zero. The cursor overlay runs first. | `grid.wgsl`, `grid_bg.frag.glsl` | Sparse, mixed, opaque, faint, transparent-RGB, and cursor cases across all WGSL input color spaces; native Vulkan controls |
| P14 | Native Vulkan retains bucket vectors, copies a single-atlas layout directly, and packs a changed layout once for all frame slots. | `VulkanGridRenderer::render_text` | One and two atlas kinds with full, single-row, rotating-slot, and unchanged recording cases |

Native Vulkan CPU recording tests call the production renderer using a real
device and an unsubmitted command buffer. The buffer is reset per operation.
Their times include row updates, host uploads, and command recording. They do
not include GPU execution or presentation. Zero-area glyph instances keep this
fixture focused on packing and uploads rather than text shading.

The WGPU frame fixtures also use zero-area glyph instances. They measure real
buffer updates, command encoding, submission, and device completion against an
offscreen target. They are upload controls, not a dense text-rendering FPS test.
The real-window replays provide the terminal rendering measurement.

The shader benchmark loads the exact upstream and branch WGSL files into one
process, alternates their order, uses GPU timestamp queries, and compares full
framebuffer hashes. The native Vulkan shader benchmark uses the production
renderer and separate executable revisions. Neither shader result establishes
an equal improvement in whole-frame latency.

## Search, selection, and palette

| ID | Final difference from upstream | Production code | Dedicated benchmark cases |
|---|---|---|---|
| P15 | Stable search frames reuse visible matches. New scans use the viewport's wrapped-line bounds instead of rebuilding broader match state on every frame. | `Screen::render`, `visible_regex_match_iter` | Real-window `application/search`, with cursor movement over unchanged matching text |
| P16 | Selection changes dirty the visible union of the old and new row ranges instead of invalidating the full panel. | `Context::set_selection` | `selection/three_rows_snapshot`, including the real pending-damage consumption and snapshot |
| P17 | Unchanged search strings keep their allocation and caret state. | `SearchOverlay::set_active_search` | Unchanged and changing search queries |
| P18 | Palette scoring iterates text without temporary character vectors. ASCII candidates avoid lowercase-string allocation. A query is normalized once per edit. | `score`, `score_chars`, `rebuild_matches` | ASCII and Unicode scoring, plus query edits over 1,024 font names |
| P19 | Palette results are rebuilt on edits or mode changes. Navigation and activation reuse those results. Query appends reuse the existing string. | `CommandPalette` | Stable results, navigation, query edits, and real-window palette navigation |

## UI text and the new tab system

| ID | Final implementation | Production code | Dedicated benchmark cases |
|---|---|---|---|
| P20 | Measurement and drawing share cached shaped UI runs. | `Text::shape_for`, `measure`, `draw` | Short, long, and Unicode cached labels |
| P21 | UI shaping history has a fixed bound. Misses evict one old entry rather than clearing the complete cache. | `Text::shape_cache_order` | A working set of 4,096 changing titles, twice the current cache bound, plus a retained-entry counter |
| P22 | Immutable SVG artwork is parsed and rasterized only on a shared UI color-atlas miss. Draws reuse the uploaded slot. | `logos::raster`, `Text::draw_icon` | Actual SVG rasterization and warmed atlas hits |
| P23 | Failed immutable icon uploads are cached until atlas reinitialization. | `Text::draw_icon` | Repeated failed artwork, with an assertion that rasterization runs only once |
| P24 | Each render shares one resolved tab presentation between measurement, drawing, and hit geometry. Unchanged titles retain tab measurements. Catalog lookup replaces platform-specific app-detection special cases. | `Tabs::update`, `TabPresentation`, `app_icons::Library` | Catalog cases and real-window tab replays |
| P25 | Repeated tab navigation replaces unfinished travel. Motion settles at a physical-pixel tolerance. Animation levels control motion and cadence. Idle UI stops requesting continuation frames; delayed effects share the earliest timer. | `Tabs`, `Position`, `Spring`, `Island::next_frame_in`, `Scheduler::schedule_once` | None, medium, and full motion; idle deadlines; real-window idle and repeated-tab input |
| P26 | Desktop WGPU contexts reuse an instance for the requested backend. Adapter and surface selection remain per window. | `WgpuContext::new` | `wgpu/second_window_context` with a first context kept alive |

Upstream has no equivalent of the new SVG app catalog, retained animated tab
presentations, or animation-level configuration. P22–P25 therefore include
cost benchmarks. Their timings must not be presented as speedups of an
equivalent upstream feature. The bounded title history also makes an explicit
memory-versus-recomputation tradeoff under churn.

## Status, shell integration, and Windows lifecycle

| ID | Final implementation | Dedicated benchmark cases | Linux applicability |
|---|---|---|---|
| P27 | OSC 7501 accumulation is bounded. Status presentation synchronizes only when the focused pane or store revision changes. The status rim does not require an indeterminate heartbeat animation. | Oversized input, revision hits, and reports followed by synchronization | Runs locally. Upstream has no equivalent status protocol. |
| P28 | Bash reports an app once per command and leaves existing preexec and prompt hooks intact. Repeated DEBUG events take the guarded path. | A 10,000-command loop, with and without an existing DEBUG trap | Runs locally. The upstream control has no Rio Bash app-reporting hook, so the result measures added feature cost. |
| P29 | Windows checks pending exit requests before blocking for more messages. | `windows/startup_exit` | Windows only. |
| P30 | Windows continuation frames use native vblank pacing and prefer Mailbox presentation when supported. | Actual keyboard-driven tab motion at each animation level | Windows only. Forced-redraw profiling is not used as a pacing benchmark. |
| P31 | Windows minimize notifications preserve terminal dimensions instead of resizing the terminal to a degenerate surface. | `windows/minimize_restore` | Windows only. This also fixes terminal correctness. |

Windows-only cases have a committed runner and are explicitly skipped in the
Linux report. Linux WGPU measurements exercise shared implementation code;
they do not prove Windows lifecycle, DWM, or display-clock performance.

## Changes without a performance claim

The branch also changes tab styling, command-palette layout, app identity,
status semantics, window controls, transparency, pixel alignment, and ConPTY
packaging. These are features or correctness changes. Their rendering costs
are included in the relevant application replays, but this inventory does not
invent an optimization or a throughput gain for each visual adjustment.

The branch adds debug/profiling instrumentation behind the existing `profiling`
feature and tracing paths. The benchmark module hooks are `cfg(test)` only.
Ordinary application runs in this comparison do not enable profiling.

## Benchmark coverage and interpretation

Every P-number has named cases in `changes.json`. The report generator checks
that each applicable entry has results, and records platform skips separately.
Shared cases cover interacting changes. For example, selection-to-snapshot
timing includes both row invalidation and snapshot reuse. Such a result cannot
be divided into independent percentage gains and added together.

All comparisons use the final branch against the same upstream commit. The
upstream worktree receives the benchmark fixtures, `cfg(test)` module hooks,
and small API adapters for names and string ownership. Its production
algorithms are unchanged. Both revisions get the same added Unicode fixtures
in the existing VT benchmark. Benchmark-only X11 feature selection changes one
lockfile dependency edge, without updating dependency versions.

The reusable measurement helper emits individual samples and warmed allocation
counts. Allocation counting runs separately from timed samples and is process-wide,
so GPU driver threads can contribute to GPU-fixture allocation counts. Native GPU
queries report GPU time. Application replays report Rio process CPU time over
a four-second fixed workload and exclude child-shell CPU time. The runner
alternates executable order between pairs and uses the same CPU affinity and
build settings for both revisions.

Build instructions and artifact formats are in
[the benchmark guide](../benchmarks/README.md).
