# Rendering data flow and remaining bottlenecks

Audited revision: `0bcc2f353075c143a0979c7d5fe7a412b57698d5`.
Measurements were run on Linux only, on 2026-10-18. No production code was
changed. The findings concern shared terminal, snapshot, search, shaping, and
renderer data structures. Backend-specific observations are identified below.

The strongest small changes are to deduplicate snapshot extras and stop
foreground changes from invalidating unrelated data. The larger opportunity
is preserving row identity and prepared text through scrolling and repainting.
Another pass over parser instructions is a lower priority for these workloads.

## Evidence and scope

The audit combines source inspection with these new Linux measurements:

- Ten sampled application replays: dense text, scrolling, Unicode, search,
  and repeated tabs, each repeated twice. The application executable was built
  from the clean audited revision.
- Three runs of eight snapshot probes, twenty existing row-preparation probes,
  twenty-four foreground recording probes, and one WGPU construction probe.
  Each case has seven samples per run. Timing targets are 100 ms for snapshots
  and 75 ms for the other probes. Allocation counting is a separate warmed pass.
- Logical CPU 2, Rust 1.96.1, optimized builds, no LTO, 16 codegen units,
  no debug information or incremental compilation. Linux kernel 7.2.6-arch2-1,
  Core i7-1370P, Intel Iris Xe. No power policy was changed.

Sampling uses `perf record -e cpu-clock:u -c 100000 --call-graph dwarf,8192`,
attached after readiness and before the workload starts. These are approximate
self-sample shares of user CPU, including application worker and driver threads.
They exclude sleeping time and kernel CPU. They are not frame-time shares or
speedup estimates. Call-chain unwinding is incomplete, so the conclusions use
leaf symbols. All ten captures report zero lost samples.

| Workload | Sampled CPU concentration, two runs |
|---|---|
| Dense text | `rio_grid` 31.7–33.5%; Swash shaping 9.7–11.6% |
| Scroll | `rio_grid` 27.7–28.9%; Swash shaping 21.0–23.0% |
| Search | Regex search and grid traversal 43.4–47.8% |
| Repeated tabs | Polling, clock reads, and channel receive functions 47.0–50.9% |

The tab result is a separate wakeup/event-delivery investigation. These samples
do not prove a busy loop or identify which timer or event source is responsible.
There is no basis here for replacing the scheduler's small `VecDeque` with a heap.

The foreground probe uses native Vulkan on Linux, with zero-area glyphs to
isolate CPU storage and command recording. It does not submit its timed command
buffers. WGPU construction includes submission and a device wait. Neither probe
measures dense glyph GPU execution. No Windows benchmark or profiling was run.

## 1. Snapshot extras are copied per cell instead of per unique record

In `Crosswords::refresh_row_extras`, every cell carrying an extras ID performs
`extras.insert(id, live.clone())`. The terminal already interns equal extras,
so many cells can reference one immutable record. The snapshot immediately
replaces the clone it inserted for the preceding cell.

This is measurable even when the snapshot and terminal both contain only one
unique extras record. At 160 columns by 60 rows:

| Snapshot content | Full snapshot | Warmed allocations | Allocated bytes |
|---|---:|---:|---:|
| Plain | 1.635 us | 0 | 0 |
| One uniform style | 11.638 us | 0 | 0 |
| Repeated combining character | 96.460 us | 9,600 | 38,400 |
| One repeated hyperlink | 127.524 us | 0 | 0 |

The combining fixture contains `e` plus one combining mark in every cell.
Each clone allocates another `Vec<char>`. The hyperlink fixture shares its
underlying hyperlink allocation, but still repeats hash-table replacement
and reference-count operations for every cell. This fixture demonstrates the
duplication; it does not imply ordinary screens spend these percentages here.

Updating one row takes 0.147 us plain, 0.320 us styled, 1.748 us combining,
and 2.282 us hyperlink. The combining row still makes 159 allocations after
its first character is replaced with plain text.

**Recommended change:** refresh each distinct extras record once per snapshot
update, with explicit snapshot ownership of row-to-extras references. A reusable
visited-ID structure can remove duplicate refreshes. Sharing immutable extras
payloads can then remove deep copies across snapshots.

Do not simply replace `insert` with `or_insert`. Terminal extras IDs are reused
after reclamation, so an existing snapshot ID may refer to older content.
Refresh tracking must distinguish an update generation or validate the payload.
Removing the repeated clones is more direct than changing every combining-mark
vector to a small-vector type, which had mixed timings in the previous pass.

Source: `rio-vt/src/crosswords/mod.rs:2264`.

## 2. Partial snapshots retain extras that no visible cell references

Partial snapshots refresh entries but never remove obsolete entries. The map
is cleared on a full snapshot. Replacing one linked cell 1,024 times produced:

| State | Snapshot extras entries | Referenced IDs |
|---|---:|---:|
| After 1,024 partial updates | 1,024 | 1 |
| After a full snapshot | 1 | 1 |

The payloads remain retained until full damage, context destruction, or another
cleanup. Clearing the map drops obsolete payloads but retains map capacity.
The ID space is finite; the issue is that retention follows historical IDs
rather than the visible working set.

**Recommended change:** keep extras dependencies per snapshot row. Replacing a
dirty row removes its old dependencies and adds its current ones. Drop a payload
when no snapshot row references it. Encapsulate this in the snapshot module so
the frontend does not need to coordinate pruning or generations.

This belongs with the deduplication change above. It should preserve the single
terminal lock and the immutable values needed after that lock is released.
The `RenderableContent::extras` comment currently promises a visible-cell bound
that partial updates do not enforce.

Sources: `rio-vt/src/crosswords/mod.rs:2193`,
`frontends/rioterm/src/context/renderable.rs:84`.

## 3. Foreground invalidation amplifies tiny changes into full-stream work

Native Vulkan `write_row` clears and copies foreground data and marks every
frame slot dirty even when the incoming foreground is identical. Any cursor
geometry change also dirties the complete stream. `render_text` then rebuilds
all atlas buckets and copies the complete foreground stream into the active
frame-slot buffer.

Median CPU recording times from three runs, in microseconds:

| Operation | 48 rows, one atlas | 48 rows, two atlases | 192 rows, one atlas | 192 rows, two atlases |
|---|---:|---:|---:|---:|
| Unchanged | 2.789 | 2.931 | 2.796 | 2.949 |
| One row changes | 15.768 | 27.766 | 83.952 | 142.262 |
| Identical row written | 15.659 | 27.826 | 84.238 | 140.148 |
| Cursor blink | 15.398 | 27.612 | 84.574 | 129.817 |

All cases allocate zero times when warmed. This is redundant traversal and
copying rather than allocator churn. With 160 glyphs per row and 32 bytes per
`CellText`, 48 content rows occupy 245,760 bytes. A single-row edit causes the
current Vulkan path to repack those content instances and copy them to the
active frame-slot buffer. It marks all three frame slots for later uploads.

WGPU already compares foreground rows before invalidation and preserves a
fixed block-cursor slot. Its remaining problem is variable row length: changing
one row's glyph count shifts the packed suffix. A single dirty range also
joins separated dirty rows and includes unchanged rows between them. Metal
already keeps cursor uploads separate from content uploads, but its content
row writes still invalidate the full content stream.

**Recommended sequence:**

1. Add the existing WGPU equality gate to the native foreground row writers.
2. Reuse the separate cursor-storage approach where a cursor change still
   invalidates content.
3. Evaluate stable row allocations and per-frame-slot dirty ranges. Repack only
   when an allocation grows or atlas grouping changes.

Frame-slot buffers are necessary to avoid writing data still used by the GPU.
Their validity must be tracked independently. Removing the ring or uploading
only to the current slot without version tracking would introduce corruption.
Stable row allocations also trade fragmentation or extra draw metadata for less
copying; they need measurement rather than a wholesale storage rewrite.

Sources: `sugarloaf/src/grid/vulkan.rs:855`, `:908`, `:1061`;
`sugarloaf/src/grid/webgpu.rs:378`, `:495`;
`sugarloaf/src/grid/metal.rs:543`, `:663`.

## 4. Scrolling loses row identity at the rendering interface

The terminal uses a row ring, but a full-screen scroll becomes full damage.
The snapshot copies rows by viewport index, resolves styles again, refreshes
extras again, and rebuilds foreground instances. Unchanged text moving up one
row goes through preparation again. `CellText` embeds the absolute grid row,
so moving text also changes its instance data.

The Linux scroll samples put about half the sampled user CPU in row preparation
and shaping. The existing 160-column row fixture shows the cost of rebuilding
a changed run:

| Row kind | Cached | Changing an eight-character counter |
|---|---:|---:|
| Plain | 2.701 us | 36.983 us |
| Styled | 3.360 us | 6.015 us |
| Unicode | 2.629 us | 5.227 us |
| Combining | 2.257 us | 28.808 us |
| Preedit | 2.835 us | 10.459 us |

The changing cases allocate 4–9 times per operation as shapes are inserted.
Cached cases allocate zero times, but still construct run scratch, hash it,
walk glyphs, resolve atlas slots, and emit instances. Shape-cache hits do not
eliminate row preparation. The whole-application samples also place the glyph
resolver and atlas lookup among the larger dense-text costs.

**Larger design change:** give prepared rows stable identities and content
generations. Keep shaped geometry separately from viewport position and paint
state. A scroll should move row references and prepare newly exposed content.
Selection and color changes should avoid reshaping unchanged text where the
existing ligature-boundary rules allow it.

This is the largest architectural opportunity, but also the highest-risk one.
It must preserve wrap boundaries, source-cell mappings, combining characters,
wide spacers, font fallback, selection boundaries, custom glyphs, IME content,
and atlas generations. A shared shape cache cannot retain an unchecked atlas
slot: different panels own different atlases. Arbitrary fixed-length run splits
would also break shaping behavior.

Sources: `rio-vt/src/crosswords/mod.rs:1247`, `:2193`;
`rio-grid/src/lib.rs:1779`, `:1823`, `:2091`, `:2284`;
`sugarloaf/src/grid/cell.rs:50`.

## 5. Style snapshots expand compact cells into repeated values

`Square` is eight bytes. Once a row has styles, its snapshot also stores one
14-byte `Style` value per cell. In the 160x60 uniform-style fixture, cell-vector
capacity is 76,800 bytes and style-vector capacity is another 134,400 bytes,
although the screen uses one style. Plain rows already use an empty vector and
avoid this expansion.

**Recommended change:** retain the safe value snapshot, but represent rows as
default, uniform, or mixed styles. Uniform rows can keep one immutable value.
Consider style runs or a snapshot-local dictionary for mixed rows only after
measuring the extra lookup cost.

Referencing live terminal style IDs after releasing the lock is unsafe because
the terminal reuses IDs. Compression must keep snapshot values or pin a table
generation. The full styled snapshot takes 11.638 us versus 1.635 us plain,
but the one-row difference is only about 0.17 us, so this ranks below extras
cloning and foreground amplification.

Sources: `rio-vt/src/crosswords/grid/mod.rs:726`,
`frontends/rioterm/src/context/renderable.rs:79`.

## 6. Search restreams unchanged cells after any terminal content damage

Search already reuses its compiled DFA and skips rescanning on cursor-only or
chrome-only frames. On content damage, however, it rebuilds the visible match
list by walking the viewport again. The DFA path extracts each cell's character
sequence, encodes it to UTF-8, and traverses it byte by byte. A changed match
list can then promote painting to full damage.

**Recommended change:** retain search text and results by logical row identity,
content generation, and query generation. Cache the UTF-8 text and byte-to-cell
mapping for unchanged logical rows. Invalidate the affected wrap group rather
than every visible cell when the query permits this.

Multi-line and unbounded regular expressions require a wider invalidation
region or a full-scan fallback. Combining characters and wide cells require the
byte-to-cell mapping. Caching only by physical row number or searching raw
packed cells would produce incorrect matches. The per-search temporary
`Vec<char>` is a smaller cleanup than avoiding repeated viewport scans.

Sources: `frontends/rioterm/src/screen/mod.rs:4034`,
`frontends/rioterm/src/screen/hint.rs:10`,
`rio-vt/src/crosswords/search.rs:300`, `:488`.

## 7. Resource ownership retains and duplicates more than the working set

These are source-confirmed opportunities. Their speedup has not been measured.

| Data | Current ownership | Proposed direction |
|---|---|---|
| WGPU glyph textures | Every grid immediately creates 2048x2048 R8 and RGBA8 atlases: 4 MiB plus 16 MiB of logical texture storage | Lazily create the color atlas, with a small placeholder binding until needed |
| Glyph data across panes | Each grid has its own glyph atlas, including common font glyphs | Explore a window-owned atlas with correct custom-glyph keys and generation tracking |
| WGPU pipelines and layouts | Every grid constructs pipelines and layouts for the same device and format | Share immutable pipeline state within the renderer context |
| Font-resolution results | `font_resolve` keys include route ID for every non-fast-path character and have no route-removal cleanup | Share ordinary font results across routes; retain route scope for custom PUA glyphs; remove closed-route entries |
| Terminal extras intern keys | Both the slot table and hash-map key own an `Extras` value, duplicating combining-mark vectors | Consider shared immutable payloads or a hash-to-slot index with full equality checks |
| Historical UI shapes | Entry count is bounded, retained glyph bytes are not separately budgeted | Measure a byte budget before changing the existing cache policy |

WGPU construction on Linux takes 1.151–1.185 ms across run medians, with
4,762 warmed CPU allocations and 1,129,400 allocated bytes per construction.
That operation includes GPU submission and a wait; it does not isolate pipeline
creation or atlas allocation. The 20 MiB atlas figure is logical texture size,
not measured physical GPU residency or process RSS. Sharing all atlases is a
larger change than deferring the unused color texture.

Sources: `sugarloaf/src/grid/webgpu.rs:20`, `:214`;
`rio-grid/src/lib.rs:1320`, `:1433`, `:1467`;
`rio-vt/src/crosswords/grid/mod.rs:139`, `:194`;
`sugarloaf/src/text.rs:618`.

## 8. Immediate-mode UI work is a smaller secondary target

UI text instances are cleared and reconstructed for each frame, then uploaded
again even when the label is unchanged. Native Vulkan text rendering also
creates temporary grouping vectors and copies instances into a second vector
on every call. Overlay preparation can run before a frame is discarded.

Reusing grouping scratch and retaining stable instance ranges are reasonable
small experiments. They rank below the measured terminal and snapshot costs.
The current atlas-slot cache already removes repeated UI glyph resolution, so
another parallel text cache would add complexity without addressing this data
movement. No new UI speedup is claimed by this audit.

Sources: `sugarloaf/src/text.rs:254`, `:1200`, `:1274`;
`frontends/rioterm/src/screen/mod.rs:4806`.

## Preserve the useful retention

Not every clear or copy is wasteful. `Vec::clear` on the row scratch buffers
retains capacity and usually only changes the length. `mem::take` moves buffer
ownership rather than copying cell payloads. Snapshotting mutable terminal rows
before releasing the PTY lock is required. Frame-slot buffers protect in-flight
GPU reads. Retaining inactive tab grids makes switching back inexpensive.
Image state is already copied on its graphics-dirty flag rather than blindly
cloned on every frame.

Do not shrink buffers per frame, remove atlas invalidation, or discard inactive
tab state to lower a memory counter. The useful target is unnecessary payload
duplication and invalidation that spreads beyond the changed data.

The recommended implementation order is snapshot extras first, foreground
equality and cursor isolation second, lazy unused resources third, then the
larger prepared-row and search-generation design. Style compression should be
measured alongside that row design so another representation is not added and
then replaced.

## Artifacts

The local diagnostic sources, executable provenance, raw samples, summaries,
and sampled CPU profiles are under `target/data-structure-analysis`. Main files:

- `profile.py`, `profile-results.json`, `cpu-symbols.json`, and per-replay
  `perf.data` files.
- `vt_probe.rs`, `snapshot-results.csv`.
- `gpu_probe.rs`, `gpu-results.csv`.
- `grid-results.csv`, `wgpu-results.csv`.
- `summarize.py`, `summary.log`, and build logs.

The temporary example and benchmark edits were removed after building immutable
diagnostic executables. Production source matches the audited commit. The
diagnostic files are ignored local artifacts; this document is the retained
analysis. Future implementation work needs before/after replays and output
checks, not speedup estimates derived from these sample percentages.
