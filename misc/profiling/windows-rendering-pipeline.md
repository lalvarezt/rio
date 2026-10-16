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

## 2. Screen preparation

Background, foreground and hint scratch vectors now belong to the screen and
are reused across panels and frames. They retain capacity, contain no cached
rendering decisions, and are cleared before use. Grid dimensions, styles,
selection and IME retain their existing invalidation behavior.

A temporary thread-local allocator probe counts allocations inside the actual
`Screen::render`, excluding the first ten calls. At 224 columns, clean frames
fall from 211 allocations / 39,381 requested bytes to 209 / 31,317. Scrolling
medians similarly fall from 229 / 41,190 to 227 / 33,126. The exact scratch
saving is two allocations and 8,064 bytes per frame in this fixture. Other
window UI work is included in these totals; this is not a claim that terminal
rendering itself needs 209 allocations.

The native profiling build completes idle, sparse, scrolling and dense
workloads. Full/sparse frame distributions differ between runs, so those
allocation medians are not used for a whole-frame speed claim. Artifacts:
`target/profiles/terminal-render-pipeline-screen-{before,after}`.

## 3. Row construction and shaping keys

The cache now hashes its completed platform text buffer and source-cell boundary
buffer in bulk, together with font and size. This removes the retained hasher,
per-character/position writes, separate combining-character hash walk and IME
hash namespace. Grid and IME shapes share the same complete key definition.
Source-cell boundaries remain part of the key: identical text split into
different cells must not reuse an incompatible mapping. Absolute screen columns
remain outside the key, so moving identical text can reuse its shape.

Three interleaved rounds of the sustained row benchmark give 6.408/6.283 µs
before/after for short cache hits and 50.131/38.627 µs for long hits, about 23%
less time in the latter case. Changing long rows measure 131.911/139.434 µs;
individual runs vary from 121 to 143 µs after the change, so these batches do
not establish a cold-row gain. No font features or shaping steps are disabled.
The change removes about 90 Rust lines rather than introducing another cache.

All 20 stream hashes match in each of three rounds, including Unicode cell
mapping, hover, selection, IME and repeated warm emissions. Artifacts:
`target/profiles/pipeline-hash`.

## 4. GPU staging and uploads

Foreground invalidation now records a range of rows. Equal-length edits only
repack that range; a length change includes the packed suffix whose offsets
move. Unchanged rows do not invalidate their resident stream. Cursor updates
use the same writer. Background invalidation similarly records changed cells
and skips identical rows. No second staging cache or offset table is added.

Buffer growth uploads the complete new allocation. Resize clears staging and
invalidates the new buffers; shrinking updates the live instance count even
when the stream becomes empty. Atlas clears still force a complete rebuild.

Three interleaved native runs per variant use the same 224×58 cmd/Python
producer at 25 Hz, Vulkan and 120 target frames. Upload timing excludes probe
log I/O. Medians of per-run medians:

| Workload | Foreground preparation/upload before, ms | After, ms |
| --- | ---: | ---: |
| Sparse row edits | 0.2863 | 0.05305 |
| Scrolling | 0.2867 | 0.2120 |
| Dense updates | 0.2571 | 0.1868 |

Sparse foreground transfers fall from 389,760 to 6,720 bytes, a 98.3% reduction.
Scrolling and dense updates still transfer the full foreground stream. Each
after-run needs one initial background upload instead of 52–53 uploads for
unchanged default backgrounds. Artifacts:
`target/profiles/terminal-render-pipeline-gpu-final-{before,after}-{1,2,3}`.

Acquire, submit and present CPU timings vary materially with the host load.
These measurements establish CPU staging/upload savings, not a whole-frame
FPS gain or shader execution time. Uniform writes and draw encoding remain
unchanged: the grid uses one background draw and one instanced text draw.

Eight native GPU captures have identical terminal-body pixels to the starting
binary. They cover row growth/shrinkage, packed-buffer growth, decorations,
inline backgrounds and clearing, Unicode, block/bar cursors and both resize
directions. The capture waits for startup and uses matching focus state;
initial unsynchronized captures were discarded. Artifacts:
`target/profiles/pipeline-visual-{before,after}`.

## 5. Glyph rasterization and background emission

The non-CoreText rasterizer retains its temporary bitmap allocation between
glyphs and borrows cached font data instead of cloning its `Arc`. The atlas
still copies each bitmap before the scratch returns to the rasterizer. Failed
rasterization and failed atlas insertion also return the scratch allocation.
Cached atlas hits retain their existing early return.

The optimized-core native probe clears the atlas and rasterizes 94 distinct
ASCII glyphs per iteration, with font/shaping caches warmed. Three interleaved
rounds, each with seven batches of 100 iterations, show 94 temporary bitmap
allocations / 8,054 requested bytes falling to zero after warming. Rendered
bitmap pixels match in all three rounds. Median run times are 6.730/6.297 ms
before/after, but host variance is substantial; the established result is the
allocation reduction, not a latency guarantee. Artifacts:
`target/profiles/pipeline-raster-confirm`.

A separate background fast-path experiment was discarded. Optimized native
240-cell rows measured 297/188 ns for plain backgrounds, but styled rows
regressed from 378 to 435 ns and inline backgrounds from 367 to 437 ns. The
small plain-row saving does not justify the added branch and styled regression.
Background output hashes matched; production background emission stays on the
existing path. Artifacts: `target/profiles/pipeline-bg`.

## 6. Search and selection invalidation

Search no longer forces full terminal damage on every frame. It compares the
visible matches and the last painted focused match, so moving focus still
repaints when the match list stays identical. Query changes continue to compute
the visible matches. The owned match list moves into the frame and back with
the other snapshot buffers; collecting the regex results no longer clones the
new vector immediately.
The unused match-tracking wrapper is removed; rendering collects the existing
visible-match iterator directly.

Selection changes mark the visible rows intersecting the old and new ranges
dirty, then use the existing partial-snapshot path. Clipping uses the current
scroll offset under the terminal lock. No extra dirty bitmap or row cache is
introduced. Existing full damage from scrolling/resizing still takes priority.

Three interleaved native runs per variant use a stable 224×58 screen and 120
target frames. Search stays open; selection alternates an endpoint within a
three-row range. Temporary hooks assert that each requested overlay was entered.

| Workload | Rows emitted before | After | Median screen CPU before, ms | After, ms |
| --- | ---: | ---: | ---: | ---: |
| Stable search | 7,018 | 116 | 18.465 | 6.566 |
| Three-row selection changes | 7,018–7,064 | 528–574 | 7.938 | 4.110 |

Counts include initial frames and occasional startup/resize frames. Search
still has two foreground uploads in both variants because unchanged streams
are already filtered by stage 4. Selection uploads remain 121–122; each edit
does change its foreground colors. CPU times vary with host load, while the
row-count reduction repeats in every run. Artifacts:
`target/profiles/terminal-render-overlay-confirm-{before,after}-{1,2,3}-{search,selection}`.

The screen spans include the temporary allocation probe's per-frame log write;
the repeated row counts are stronger evidence than those host-sensitive times.

All 14 native terminal-body captures match the baseline pixel for pixel. They
cover focused-match navigation, query edits and no matches, search dismissal,
selection growth and movement, block selection, clearing, scrollback clipping
and a wholly offscreen selection. Artifacts:
`target/profiles/overlay-visual-{before,after}-{search,selection}`.

## 7. Search scanning

After limiting redraws, repeated regex scans remained measurable: each stable
search frame still walked the viewport. The search overlay now reports whether
its existing query changed and only owns a new string on a change. Matching
reuses the existing terminal damage, snapshot dimensions/scroll offset and
renderer active-route identity. Cursor/chrome frames can keep their matches;
query edits, terminal edits, resizing, scrolling and context switches rescan.
Focused-match invalidation from stage 6 remains independent of this gate.
No additional query or viewport cache is introduced.

Three interleaved 120-frame native runs reduce regex scans from 120 to one in
every run. Before scan medians range from 3.286 to 6.965 ms. Screen-span medians
are 5.646/2.208, 7.072/1.617 and 9.658/1.604 ms before/after. In these runs the
allocation probe's log write happens after the screen timing span. Host load
and pre-ready screen-call counts still vary, so these screen medians do not
establish a latency percentage. The consistent eliminated scans are the
strongest result. Artifacts: `target/profiles/terminal-render-query-{before,after}-{1,2,3}`.

The same captures time the remaining uniform-buffer upload at 0.080–0.138 ms
per call, making it the next GPU preparation candidate.

Eleven native GPU captures match stage 6 pixel for pixel through query/focus
edits, scrollback, new PTY output, resize in both directions, switching between
two tabs while search remains active, and dismissal. Artifacts:
`target/profiles/query-visual-{before,after}-search`.

## 8. Uniform uploads

Each WGPU grid now retains the last uniform value it uploaded and compares
the POD bytes before issuing another write. This adds one inline value per
grid, with no allocation. New grids always upload; changes to cursor state,
dimensions, padding, color controls or projection still upload. Resize retains
the same uniform buffer, so comparing its resident value remains valid.

Three interleaved stable-screen runs reduce 121, 217 and 219 uniform writes
(including pre-ready frames) to one initial write in each run. Those baseline
writes take 12.90–14.30 ms total per run; each after-run spends 0.031–0.097 ms
on its initial write. No whole-frame speedup is inferred from differently sized
startup samples. Artifacts:
`target/profiles/terminal-render-uniform-{before,after}-{1,2,3}`.

The normal Windows debug build passes. All eight native rendering captures
still match the starting binary pixel for pixel, including block/bar cursor
changes and both resize directions. Artifacts:
`target/profiles/pipeline-visual-uniform`.

## Remaining stages

Overlays, background emission, font resolution, shaping keys, glyph
rasterization/atlases, foreground emission,
cursor streams, GPU uploads/uniforms, draw encoding, presentation and resize
lifetime are being inspected in order. Changes require measured benefit and
output validation; an inspected stage does not require a speculative rewrite.
