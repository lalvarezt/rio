# Rendering data-flow implementation

This implements the [data-flow audit](rendering-data-flow-audit.md) in stages.
The baseline production revision is `0bcc2f3530`; `71f69ba95e` adds the audit
without changing production code. Benchmarks and profiling run on Linux only.
Later stages and whole-application comparisons remain in progress.

## Snapshot extras ownership

The live extras table now shares immutable records between its slot table,
interning keys, and snapshots. `SnapshotExtras` owns per-row dependencies,
reference counts, and refresh generations. Refreshing a row visits each ID
once, releases records after their last snapshot-row reference disappears, and
refreshes recycled terminal IDs. Older snapshots retain their immutable data.
The frontend and library renderer use the same owner.

The visited-ID bitset is allocated only after finding an actual extras ID.
Conservative row hints on plain text do not allocate it. Its maximum size is
8 KiB, derived from the terminal's 16-bit ID space. Ordinary buffer capacity
is retained between updates.

Three alternating before/after pairs use logical CPU 2, seven samples targeting
75 ms, optimized code, no LTO, 16 codegen units, and no incremental compilation
or debug information. Allocation counting is a separate warmed pass. Times
below are medians of per-run medians; changes are medians of paired ratios.

| Snapshot operation | Before, us | After, us | Paired change |
|---|---:|---:|---:|
| Plain, full | 1.621 | 1.539 | -4.9% |
| Plain, one row | 0.148 | 0.157 | +5.6% |
| Uniform style, full | 11.630 | 11.526 | -0.9% |
| Uniform style, one row | 0.320 | 0.320 | Within noise |
| Combining marks, full | 97.926 | 11.485 | -88.3% |
| Combining marks, one row | 1.765 | 0.320 | -81.9% |
| Hyperlinks, full | 127.515 | 11.521 | -91.0% |
| Hyperlinks, one row | 2.252 | 0.325 | -85.6% |

The combining fixture repeats one record across 9,600 cells. Full-snapshot
allocations fall from 9,600 to zero, removing 38,400 allocated bytes per update.
Its one-row update falls from 159 allocations to zero. Every snapshot case
allocates zero times when warmed after the change.

Replacing one linked cell 1,024 times now retains one extras record for its
one referenced ID, rather than 1,024 records. A subsequent full snapshot also
retains one record. This fixes retention independently of timing results.
Plain partial updates add roughly nine nanoseconds of dependency bookkeeping;
the change is not a universal snapshot speedup.

Matched input controls keep allocation counts and output hashes unchanged.
ASCII, Unicode, mixed, ANSI, and CSI timing differences stay within 1.3% across
the three pairs. They do not establish an input-processing improvement.

An alternative with extra empty-map guards and conditional generation updates
was 2.8–4.3% slower on partial snapshots than the simpler implementation and
was removed. Restoring the simpler code reproduced its executable hash.
Deferring the bitset until actual extras are found changed focused timings
by less than 2% in most pairs and avoids unused allocation on plain screens;
that alternative was retained.

Validation passes 563 terminal tests, including last-reference removal and
ID-reuse checks. Both frontend snapshot-convergence tests pass. Compilation
checks pass for the terminal frontend, shared grid, and library frontend.

## Native foreground uploads

The Vulkan and Metal row writers now compare foreground bytes before
invalidating buffers, matching the existing WebGPU behavior. Three alternating
Linux Vulkan pairs reduce identical-row recording from 16.1 to 2.9 us at
48 rows with one atlas, and from 131.7 to 3.1 us at 192 rows with two atlases.
The median paired reductions span 82–98%. Other changed-row operations show
no consistent improvement from this gate alone.

A separately built Vulkan candidate gives cursor glyphs their own buffer per
frame slot, preserving block-cursor, content, and overlay-cursor ordering.
It shares buffer allocation/upload code with content instead of fixing an
arbitrary cursor glyph capacity. Against the equality-gate candidate, cursor
blink recording falls from 15.0 to 2.9 us at 48 rows with one atlas and from
126.5 to 3.1 us at 192 rows with two atlases. Paired reductions span 81–98%.
Unchanged recording adds about 20–35 ns in the median runs.

The retained Vulkan layout stores row offsets while all content uses one
atlas page. Same-length writes patch that staging range and union its dirty
range independently for each GPU frame slot. A new buffer receives the full
staging data. Length changes, atlas changes, resize, and atlas resets rebuild
the packed layout and invalidate every slot. Mixed-atlas content keeps the
existing grouped layout. Cursor and content buffers share allocation logic;
redundant per-slot foreground live counts are removed.

Against the separate-cursor candidate, a one-row edit falls from 15.1 to
3.1 us at 48 rows and from 82.9 to 3.1 us at 192 rows with one atlas. Full
same-atlas updates improve by 9–14% in paired medians. The 48-row variable-
length case is 3.5% slower (roughly 0.8 us); the 192-row result is within
noise. Mixed-atlas operations keep their full packing path. All 24 warmed
recording cases allocate zero times.

These probes rotate all three frame slots and measure CPU storage and Vulkan
command recording. Zero-area glyphs isolate that work. They do not measure
submitted GPU text rendering or establish whole-application speedups.

A diagnostic build independently compares mapped GPU buffer contents against
the current packed data after each slot catches up, compares the packed glyph
multiset against source rows, and checks bucket coverage and atlas membership.
Its 36 cases add atlas changes, cleared rows, and varying block/overlay cursor
shapes to the timing fixtures. All checks pass. The native renderer library
passes 119 tests; eight existing ignored tests remain ignored. Metal receives
only the equality gate in this stage; no Metal runtime measurements are claimed.

## WebGPU resource ownership

Color atlases initially bind a one-texel texture and allocate the full 2048-square
RGBA texture on the first nonempty color glyph. Monochrome grids avoid its
16 MiB logical allocation; their grayscale atlas remains 4 MiB. The color binding
is replaced once when that texture appears. WebGPU retains resources referenced
by previously recorded or submitted work. Atlas clears reuse allocated storage.
This changes logical texture allocation, not measured physical GPU residency.

The context owns immutable grid pipelines and bind-group layouts, cached by
target format. Each grid retains shared ownership and keeps its own uniforms,
bindings, cell buffers, and glyph atlases. Pipeline lookup happens at grid
construction. Draws take no cache lock. Replacing or closing a grid cannot
invalidate another grid's pipeline state.

Deferring the color atlas alone leaves CPU construction timings within noise.
First-color construction adds 25 small CPU allocations for the deferred texture
and replacement binding. Sharing pipelines is the measured CPU improvement:
against the lazy-atlas candidate, warmed monochrome grid construction falls
from 1.218 ms to 0.152 ms (87.5% less in paired medians). CPU allocations fall
from 4,762 / 1,129,400 bytes to 99 / 40,073 bytes. Construction with one color
glyph falls from 2.476 ms to 0.930 ms, with more timing variation, and from
4,809 / 1,140,471 bytes to 146 / 51,144 bytes. These include submit-and-wait and
exclude first-use pipeline construction through warmup. No steady-frame
improvement is inferred.

A background-only row-storage control is about 0.2 us slower across 48 rows in
focused paired medians, despite no change to its write algorithm. That local
tradeoff remains visible rather than being described as a universal improvement.
Submitted frame controls show no consistent broad speedup. Three GPU readbacks
check actual monochrome, first-color, and cleared-atlas pixels while leaving
earlier bindings in flight; before/after hashes match.

An alternative sharing the one-texel placeholder saved 11 additional allocations
but made monochrome construction about 5% slower and increased the row-storage
control cost. It was removed. Source hashes match the retained pipeline-only
candidate exactly after restoration.

The first lazy-atlas comparison accidentally reused Cargo's baseline artifact
across worktrees. Identical executable hashes exposed this. Those samples are
labelled `invalid-cache-reuse-*` and excluded. The current package was cleaned
and rebuilt; the valid before/after hashes differ. Subsequent baseline builds
use a separate target directory. The earlier foreground candidates were built
in one working tree after edits and compared with immutable baseline binaries.

## UI text storage

The native Vulkan UI text candidate retains grouping scratch and compares the
current instance bytes with the previous batch. Changed batches preserve the
first-occurrence atlas/page order and write their groups directly into each
dirty frame slot. This removes temporary grouping allocations and the extra
flattening copy. Unchanged batches reuse grouping, draw ranges, and uploads.
One retained instance copy makes equality exact rather than relying on a hash.

Three paired CPU-recording comparisons cover 32, 256, and 4,096 glyphs, one or
two atlases, and stable, rebuilt-identical, and moving batches. All eighteen
warmed allocation counts become zero, from 8–16 allocations and 3,488–442,484
bytes per operation. Stable recording improves by 9–75%; at 4,096 glyphs with
one atlas it falls from 20.7 to 5.2 us. Rebuilding identical text improves by
9–40%, while moving batches improve by 5–8%. The rebuilt and moving cases
include cached shaping lookup and CPU instance emission. These are recording
measurements, not submitted GPU latency.

Thirty-six diagnostic workloads independently reconstruct expected grouping
and compare mapped GPU contents. They exercise every frame slot, changing
first-atlas order, empty batches, and color-only transitions. All checks pass.

Shaped-run caching now has a 4 MiB payload budget alongside the existing
2,048-entry limit. Accounting includes the run object and glyph-vector capacity;
the separately count-bounded map, FIFO, and allocation headers are outside this
payload figure. Ordinary title churn still retains 2,048 runs / 2,457,600 payload
bytes. After 256 distinct long labels, retention falls from 153,157,632 to
4,129,104 bytes. An additional oversized label previously raised retention to
162,593,664 bytes; it now leaves the seven cached runs and their payload size
unchanged. Oversized runs remain drawable without entering the cache.

The budget is above the measured ordinary title working set. Cache hits keep
their existing lookup path; misses evict oldest entries until both bounds fit.
Atlas-cache resets also reset accounting; the memory diagnostic checks both
the computed payload and the accounting counter, clears the cache, and shapes
again successfully.

Seven CPU text controls retain their raster hashes and allocation counts.
Title churn is about 1% slower; short cached drawing adds roughly four
nanoseconds, while cached Unicode drawing is about 5% faster. These results
do not establish a general CPU shaping improvement. The renderer library
passes 119 tests. Debug application replays exercise dense text, Unicode,
cursor movement, palette navigation, and repeated tab selection. Compositor
captures show the latest numbered content and correctly rendered overlays.
These debug runs include screenshot overhead and are functional checks only.

Direct X11 client captures in the isolated XWayland session sometimes return
an earlier frame. This initially looked like a rendering regression. Isolated
builds and actual-frame buffer tracing showed current data reaching all GPU
slots; a compositor capture then showed updated Unicode while a client capture
from the same run still showed initial ASCII. Validation now uses background
Spectacle captures from the isolated compositor and hashes PNG image data
without timestamp metadata. No foreground implementation was reverted on the
basis of the stale client readback.

## Closed terminal resources and font resolution

The window now releases a terminal's grid renderer and cached font choices
when the terminal closes. Previously only its custom glyph registry was
removed; the window's grid map had no removal path. A debug lifecycle replay
opens sixteen auxiliary terminals, waits for each to upload real glyphs, and
closes them. The previous path retains seventeen grids until window teardown.
The new path retains one live grid after every auxiliary close and zero after
the final terminal closes. This releases 64 MiB of obsolete logical grayscale
atlas allocations, plus the associated grid buffers. Vulkan destruction waits
for in-flight device work; measured cleanup takes roughly 0.3–1.5 ms in this
debug replay. This is a retention fix, not a claim that destruction is free.

Font-resolution keys pack the Unicode scalar and eight style bits into one
`u32`, alongside the terminal route. Closed-route entries are removed together
with the grid. Three paired cache controls improve by about 1.5–2% for ordinary
and private-use characters across one or sixty-four routes, with zero warmed
allocations and identical font choices. Twenty row controls preserve output
checks and allocation counts: cached Unicode improves about 3%, while full
rasterization controls vary from roughly 1% to 2% slower. The rasterization
controls clear font caches on every operation and do not establish a general
rendering speedup. Seventeen grid tests and the frontend/library integration
check pass.

Sharing ordinary-character resolution between routes was also benchmarked.
Normalizing the existing key increased lookup cost by 26–28%. Separate maps
reduced duplication but slowed ordinary lookups by 5–11% and private-use
lookups by 73–79%. Packing those separate keys improved ordinary lookups by
3–4% but still slowed private-use lookups by about 20%; a packed normalized
single map was also slower. These sharing alternatives were discarded.
Keeping the route preserves custom glyph isolation and gives the simplest
measured improvement, while lifecycle cleanup bounds obsolete entries.

## Compact row style snapshots

`RowStyles` now owns either one uniform style or a vector of mixed cell styles.
The snapshot resolves values while holding the terminal lock, so style-ID reuse
still cannot change an earlier snapshot. Grid emission, hint-label overlays,
virtual image placements, and both library adapters use the same representation.
Uniform foreground emission also resolves its color once per row.

For a 160-by-60 uniformly styled viewport, style-vector payload falls from
134,400 bytes to zero. Row-owner storage grows from 1,440 to 2,400 bytes; mixed
rows retain their previous vector payload. A row that becomes uniform after
being mixed retains its allocation for later reuse. Dropping that allocation
was measured separately: alternating uniform/mixed snapshots then allocate
thirty times / 67,200 bytes per operation and take about 9% longer. The retained
allocation is bounded by the snapshot row working set.

Three paired comparisons reduce uniform full snapshots from about 11.7 to
5.9 us and partial updates from 320 to 237 ns. Rows with only the final cell
different improve from 11.9 to 10.4 us; alternating uniform/mixed full snapshots
improve about 26%. Rapidly mixed full snapshots increase about 2%, and their
partial updates increase about 2%; the late-mixed partial control adds roughly
nine nanoseconds. Plain snapshots remain close to baseline. All ten warmed
snapshot controls allocate zero times.

The initial candidate scanned a uniform prefix twice when a difference appeared
late in the row, causing a 37% regression in that control. It was replaced by a
prefix fill followed by resolution of only the mixed remainder. Twenty shaping
controls preserve raster checks and allocation counts; paired median changes
range from roughly 2.4% faster to 0.8% slower. These controls support a snapshot
storage improvement, not a broad shaping speedup. Validation passes 565 terminal
tests, seventeen grid tests, two frontend snapshot tests, and the integration
check including `libsugarloaf`. New checks cover frozen uniform values after
style-ID reuse and hint-style expansion without losing the surrounding style.
Five debug application replays also pass with compositor captures covering dense
text, Unicode, cursor changes, palette navigation, and repeated tab selection.

## Search character streaming

Regex search now borrows each cell's attached codepoints instead of allocating,
copying, and reversing a temporary character vector for every forward/backward
search pass. The DFA still consumes the same UTF-8 bytes in the same order and
retains its existing cell-position and empty-match handling. There is no new
search cache or retained character stream in this change.

Fourteen paired controls cover ASCII, absent matches, Unicode, combining marks,
clusters longer than the old initial vector capacity, empty-match patterns, and
anchors, in both directions. All match-position checks agree with baseline.
Warmed allocation counts become zero, from 1–1,441 per scan. Paired median
times improve by roughly 8–25%: ASCII forward matching falls from 170 to 143 us,
and the long-cluster forward case falls from 127 to 96 us. The terminal library
passes all 565 tests.
A debug search-navigation replay also passes; the compositor capture shows the
current match highlighted at the expected cell position.

## Rejected row mutation metadata

Two prototypes added content versions to rows to support reuse after scrolling.
One minted a version on the first write after a snapshot; the other kept a
stable identity and advanced a local revision on every mutable access. Both
preserved versions in snapshot copies and gave independent clones new identities.
The lazy prototype is also insufficient for search, which can observe multiple
edits before a snapshot clears the row's dirty bit.

Neither prototype was retained. Paired parser controls slow roughly 8–9% in
several Unicode and escape-sequence cases with the lazy prototype, and roughly
5–6% with local revisions. Both also increase row-construction cost and metadata
storage. These costs would apply even when the terminal is parsing without
drawing. Prepared-row reuse will instead be evaluated with exact content checks
in the rendering path, keeping the existing cell-write path unchanged.

## Retained artifacts

`target/data-flow-implementation` contains the immutable before/after binaries,
build logs, paired samples, summaries, and test logs. `measure.py` runs the
paired CPU comparisons. The snapshot workload is the audit's saved probe,
adapted to the new extras owner without changing its inputs. `baseline/` is
the detached baseline worktree. Further implementation results will be added
here rather than inferred from the audit's sampled CPU percentages.
