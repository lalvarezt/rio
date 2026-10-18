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

## Retained artifacts

`target/data-flow-implementation` contains the immutable before/after binaries,
build logs, paired samples, summaries, and test logs. `measure.py` runs the
paired CPU comparisons. The snapshot workload is the audit's saved probe,
adapted to the new extras owner without changing its inputs. `baseline/` is
the detached baseline worktree. Further implementation results will be added
here rather than inferred from the audit's sampled CPU percentages.
