# Sugarloaf full updates, 24 October 2026

Mixed-atlas recording for the existing all-rows update fixture falls from
22.2 µs to 10.9 µs, a 50.7% improvement over `608fc99f38`. Single-atlas
recording stays approximately unchanged. The new control that changes every
glyph also stays approximately unchanged across repeated comparisons.

## What the fixtures measure

The existing `gpu/record/*/full` fixture calls `write_row` for all 48 rows of
a 160-column grid, but only the first glyph's color changes in each row.
It therefore measures broad row invalidation with small content changes.

This pass adds `full_all_cells`, which changes every glyph's color before
rewriting every row. Both cases are included in timing and profiling. The
50.7% improvement applies to the original fixture, not to every possible
full-screen repaint or to GPU execution time.

## Changes

Stable mixed-atlas rows now trim equal cells from both ends using byte-array
comparisons. Atlas membership validation, staging writes, and stored-row
copies operate on the remaining span. Length or bucket-membership changes
still rebuild staging. Single-atlas rows retain contiguous copies because
scanning them to trim small differences was more expensive.

Damage tracking intersects the actual patch bounds with cached row/bucket
spans. Crossing the broad-update threshold merges the pending spans into
their bounding range rather than expanding to the entire staging buffer.
Later edits expand any pending range independently for every frame slot.
That expansion is essential when some slots have broad pending damage while
others still track separate bucket spans.

In the original mixed-atlas full fixture, stored-row and staging writes shrink
from 160 cells to one cell per row. The accumulated upload covers only the
affected portion of the first bucket rather than both complete buckets.
When every glyph changes, the complete staging buffer is still uploaded.

The mixed patch loop is explicitly separate from `write_row`. Without this
boundary, the bounded-upload candidate regressed the every-glyph control by
14.4% in pooled measurements. Three focused paired repetitions with the
separate loop measured that control 5.3% faster overall; the final broader
comparison measured it 2.0% faster. Individual process pairs vary in both
directions, so these measurements support treating the control as roughly
unchanged. They do not establish the precise cause of the code-generation
effect.

## Paired results

`full-baseline` contains production code from `608fc99f38` with the added
benchmark control. `full-final-paired` alternates reference/current order
over two repetitions, with nine samples per case and a 50 ms target per
sample. Values are pooled medians in nanoseconds; lower is better.

| Recording workload | Before | After | Change |
| --- | ---: | ---: | ---: |
| One atlas, all rows | 16,002.0 | 15,975.9 | -0.2% |
| One atlas, every glyph | 16,294.5 | 16,227.1 | -0.4% |
| Two atlases, all rows | 22,173.6 | 10,939.1 | -50.7% |
| Two atlases, every glyph | 22,768.2 | 22,308.0 | -2.0% |
| Two atlases, one row | 3,370.1 | 3,081.4 | -8.6% |
| Two atlases, rotating slots | 3,163.8 | 2,980.8 | -5.8% |
| Two atlases, unchanged | 2,927.7 | 2,916.8 | -0.4% |
| Two atlases, row-length change | 21,808.4 | 21,787.1 | -0.1% |
| Two atlases, atlas change | 21,966.0 | 21,841.2 | -0.6% |
| Two atlases, atlas change then edit | 22,294.7 | 22,167.4 | -0.6% |

The original mixed-atlas full case improves by 49.3% and 51.8% in the two
individual pairs. The every-glyph control is 6.8% slower in one pair and 6.8%
faster in the other. Structural updates show similar process variation;
their pooled timings are approximately unchanged. Every warmed recording
case still reports zero allocations.

Earlier candidates are retained for diagnosis. `full-trim` trimmed all rows
and made single-atlas full updates 28.2% slower. `full-trim-mixed` limits
trimming to mixed rows and uses byte comparisons, improving the original
mixed full fixture by 34.3%. `full-bounded-checked` adds narrower upload
bounds but exposed the every-glyph regression described above. The final
implementation retains the separate patch loop.

## Broad sweep and tradeoffs

`full-validated` runs all 80 cases with seven samples and a 50 ms target.
Its source hashes match the selected implementation. All 15 deterministic
output checks match the reference. Mixed-atlas all-rows recording improves
by 50.6%; changing every glyph is 2.2% slower in this single broad run.
Single-atlas all-rows and every-glyph recording are 0.5% and 1.2% slower.
GPU execution cases remain within 3%.

Two background-only row-storage fixtures regress: transparent rows move from
472.9 to 538.0 ns per 48-row batch, and opaque rows from 474.2 to 537.5 ns.
That costs about 0.06 µs per batch, compared with roughly 11 µs saved by the
mixed full-update fixture. A separate foreground-update function removes
that small cost but adds overhead to the every-glyph path. The selected
version favors full updates and retains this measured background tradeoff.

The sweep also records changes in tiny CPU cases despite unchanged CPU
implementation: Unicode draw moves from 97.9 to 105.2 ns, cached short text
from 83.3 to 87.7 ns, and 80-column one-row hash/write cases from about 25
to 32 ns. The preceding report already demonstrates substantial process
variation in those hash cases. They remain measurement limits, not evidence
of a CPU optimization in this pass.

The discarded foreground split is captured in `full-fg-outlined` and
`full-final`. An additional cached whole-row bounds experiment,
`full-cached-control`, makes the mixed every-glyph control 7.7% slower over
three paired repetitions. Both experiments are removed. The checked source
matches `full-final-paired` and `full-validated` exactly; the later experiment
names do not indicate the selected implementation.

## Profiles and remaining work

For the original mixed full fixture, exclusive samples are split between
`write_row` at 23.2%, `patch_mixed_row` at 23.2%, and a libc copy address at
25.0%. In the every-glyph control, `patch_mixed_row` accounts for 44.2%,
the copy address for 32.3%, and `write_row` for 3.2%.

The next opportunity for complete glyph replacement is the mixed patch's
membership validation, scatter, and bounds reduction, followed by buffer
copying. Further trimming cannot remove those writes when all glyphs change.
These are exclusive sampled locations. Incomplete sparse GPU DWARF stacks
still prevent precise attribution of all driver and caller costs.

## Validation and reproduction

- 122 library tests pass; nine remain ignored by default.
- The explicit native Vulkan test passes for both 5- and 160-column grids.
  It checks all three frame slots over 30 steps at each width, comparing
  staging and uploaded buffers with expected state.
- Native cases cover first, middle, last, separated, and whole-row edits,
  membership/length changes, empty rows, clearing, and resizing. A stable
  phase consumes bounded broad uploads and then extends pending bounds.
- Disabling pending-bound expansion makes the native test fail with stale
  uploaded data at step 24. Restoring expansion passes.
- Independent read-only review found no actionable correctness issues.
- Scoped all-target Clippy with warnings denied, rustfmt, and diff checks pass.
- Selected capture source hashes match the checked source after discarding
  the later experiments.

Captures are under `target/sugarloaf-profile/captures/`. Reproduce with fresh
capture labels:

```sh
python3 misc/benchmarks/sugarloaf.py full-final-paired --gpu --validate \
    --reference full-baseline --filter gpu/record --samples 9 --ms 50 \
    --repeats 2 --profile
python3 misc/benchmarks/sugarloaf.py full-validated --gpu \
    --reference full-baseline --samples 7 --ms 50
```

The focused every-glyph comparison is `full-patch-outlined`, using three
repetitions and `--filter full_all_cells`. The intentionally failing coverage
mutation is retained separately as `full-expansion-negative`; it is not a
performance reference.
