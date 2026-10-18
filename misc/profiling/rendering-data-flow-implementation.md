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

## Retained artifacts

`target/data-flow-implementation` contains the immutable before/after binaries,
build logs, paired samples, summaries, and test logs. `measure.py` runs the
paired CPU comparisons. The snapshot workload is the audit's saved probe,
adapted to the new extras owner without changing its inputs. `baseline/` is
the detached baseline worktree. Further implementation results will be added
here rather than inferred from the audit's sampled CPU percentages.
