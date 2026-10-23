# Sugarloaf follow-up profiling

Measurements from 2026-10-23 compare the rendering engine at `e73ae99000`
with the changes in this pass. The earlier checkpoint was rewritten from
`5a90342ec1` to `e73ae99000` before this session resumed.

## CPU frame gate

`CpuGridRenderer::hash_state` previously scanned all background and foreground
bytes on every frame. It now hashes a renderer identity and a paint revision.
Mutation sites compare incoming cells with stored cells before advancing the
revision. Identical writes preserve the frame key. Cursor changes, resize,
atlas writes and clearing, and explicit rebuild requests invalidate it.
Renderer identity prevents different grids with equal revision counts from
sharing a frame key.

Three paired runs alternated reference and current execution order, pinned to
CPU 0, with 11 samples per case and a target sample duration of 100 ms.
The following values are medians across those runs:

- At 80×24, unchanged hashing fell from 2,768.9 ns to 2.3 ns.
- At 160×48, unchanged hashing fell from 11,061.9 ns to 2.3 ns.
- At 320×96, unchanged hashing fell from 44,059.3 ns to 2.3 ns.
- At 160×48, writing one row and hashing fell from 11,047.7 ns to 41.1 ns.
- At 160×48, writing every row and hashing fell from 14,929.6 ns to
  3,362.8 ns, a 77.5% reduction.
- At 320×96, writing every row and hashing fell from 76,055.2 ns to
  14,650.1 ns, an 80.7% reduction.

Revision checks move work to mutation sites. An isolated 320-column row write
rose from 65.4 ns to 82.3 ns. Including the following frame hash, the same
case fell from 44,295.7 ns to 82.6 ns. Timed CPU state cases allocated no
memory. These fixtures measure the frame-key operation and row storage;
they do not measure complete application frame time.

The retained binaries, source hashes, individual samples, and summaries are in
`target/sugarloaf-profile/captures/resumed-cpu-revisions/`. Reproduce the paired
measurement with the retained reference binary:

```sh
	python3 misc/benchmarks/sugarloaf.py cpu-recheck \
		--reference pending-cpu-baseline --filter cpu/state_hash \
		--samples 11 --ms 100 --repeats 3
```

## Vulkan row storage and recording

Foreground bytes are now compared once before considering mixed-atlas layout
maintenance. Background-only writes avoid a second foreground scan. Each row
also caches whether its background contains a nontransparent cell, removing
the scan of the previous background when maintaining the visible-row count.

Three paired runs used 11 samples per case, 75 ms target samples, and CPU 0
on Intel Iris Xe Graphics RPL-P. Before narrowing background uploads:

- Tail-change row storage fell from 4,904.2 ns to 2,914.7 ns, a 40.6% reduction.
- Single-atlas background-only full updates fell from 9,499.4 ns to
  8,643.9 ns, a 9.0% reduction.
- Mixed-atlas background-only full updates fell from 19,770.9 ns to
  8,803.0 ns, a 55.5% reduction.
- Mixed-atlas updates to the first and last rows fell from 4,498.1 ns to
  4,046.9 ns, a 10.0% reduction.

These results and four sampled profiles are retained in
`target/sugarloaf-profile/captures/resumed-gpu/`.

### Background upload spans

Each frame slot now accumulates its own dirty span of background cells.
Initialization, resize, and atlas clearing mark the full buffer. Row writes
and clears merge their bounds into every slot's pending span, which is
consumed only when that slot is uploaded. A one-row update on a 160×48 grid
can copy 640 bytes instead of 30,720 bytes after each slot is initialized.
Changes to the first and last rows still require a full-span copy.

Three paired runs against the preceding optimized binary, with the same
11-sample, 75 ms settings, isolated this change:

- Single-atlas one-row background recording fell from 3,777.1 ns to
  3,238.6 ns, a 14.3% reduction.
- Mixed-atlas one-row background recording fell from 3,945.6 ns to
  3,401.8 ns, a 13.8% reduction.
- Mixed-atlas distant-row recording was effectively unchanged at 4,046.1 ns.
- Mixed-atlas background-only full updates fell from 8,883.0 ns to 8,592.0 ns.

These captures are retained in
`target/sugarloaf-profile/captures/resumed-bg-spans/`.
The native test compares each slot's entire uploaded background buffer with
the expected CPU state through short writes, repeated clears, and rotating
frame slots. It also checks visibility metadata after resize.

### Foreground regression check

The first implementation held a `foreground_changed` flag across the
mixed-atlas offset-cache check. Broad measurements caught an 8.1% slowdown
in mixed-atlas full foreground updates. The final implementation guards the
whole foreground update with the byte comparison, then enters layout
maintenance only for changed cells. This also removes a redundant checked
row lookup.

Four paired runs against the original baseline, using 15 samples and
150 ms target samples, measured 21,376.2 ns before and 20,895.5 ns after.
The earlier slowdown did not persist with this structure. This result
does not isolate which compiler decision caused the initial slowdown.
Captures are retained in
`target/sugarloaf-profile/captures/resumed-fg-branch/`.

### Remaining foreground upload cost

The two-atlas one-row recording profile contains 1,896 samples. Of those,
58.4% contain `render_text`; 703 samples, or 37.1% of the total, are in a libc
call directly below `render_text`. Disassembly resolves that call to `memcpy`.
The full-update profile contains 1,976 samples, with 58.4% containing
`write_row` and 30.1% containing `render_text`.

Mixed-atlas cells are grouped into separate atlas buckets. A single row can
occupy distant portions of that grouped buffer. The current foreground dirty
range spans the gap, so a small row edit can copy a large part of the staging
buffer. Tracking separate dirty spans per bucket is the next measured
opportunity. It needs a policy for falling back to a full copy during broad
updates, when many small copies could cost more. This pass preserves the
foreground upload algorithm.

## Cold atlas growth

The new cold fixtures create a fresh atlas for each sample and time only the
transition. Setup, population, and destruction are outside the timer.
They cover grayscale and color lookup, batch insertion, both growth steps,
and exhaustion followed by cache clearing.

The initial dense color-atlas 2048→4096 baseline was 5.563 ms, ranging from
5.478 to 5.987 ms. It allocates a 64 MiB destination and copies the 16 MiB
source into rows with the new stride. The existing growth algorithm remains
unchanged. In-place growth would also need to clear exposed padding and
could force writes to pages that zeroed allocation leaves untouched.

The first sampled capture contained only 41 samples, including fixture setup.
Cold-growth profiling now uses 401 transitions instead of five to obtain a
longer sample. Timings and profiles remain separate because profiling adds
overhead. Tests verify existing slot coordinates, copied pixels, and zero
padding after both growth steps for both atlas formats.

Three further paired runs, with 31 samples per atlas case, measured dense
color growth at 5.464 ms before and 5.461 ms after. The atlas implementation
is unchanged. A small-growth slowdown in the seven-sample broad sweep did
not repeat: color growth from 1024 measured 604.8 µs before and 594.2 µs
after. Lookup measured about 2.4 ns before and 2.6 ns after; this subnanosecond
difference remains visible despite the unchanged lookup implementation.
These captures are retained in
`target/sugarloaf-profile/captures/resumed-atlas-repeat/`.

The longer DWARF profile captured 1,460 samples, but most libc samples lost
their caller frames. A second capture using hardware last-branch records
recovered 1,599 samples with usable application attribution. It places
1,053 samples under atlas growth and 490 under fixture upload. Of the growth
samples, 930 point to the row-copy `memcpy` call site, verified against
disassembly. Setup also grows the atlas once, so these counts include both
growth sizes. The profile is retained as
`resumed-final/cpu-atlas-color-grow_2048-lbr.perf.data` under the captures
directory. The main timed growth call accounts for 766 samples, with 765
at the `memcpy` call site. That copy can also incur first-touch page faults,
so this does not separate memory transfer from page materialization. It
supports investigating relocation cost before replacing the zeroed allocation
strategy.

## Final validation

The final broad capture, `target/sugarloaf-profile/captures/resumed-final/`,
uses the final source with seven samples per case and 50 ms target samples.
It covers 41 renderer CPU cases, seven core cases, and 30 Vulkan cases.
All 15 deterministic output checks match the retained baseline. In this
sweep, tail-change row storage fell from 4,934.7 ns to 2,839.6 ns and
mixed-atlas background-only full updates fell from 19,554.9 ns to
8,531.5 ns. Mixed-atlas foreground full updates measured 22,314.3 ns
before and 22,406.4 ns after, within 0.5%.

Validation passed:

- Renderer library tests: 121 passed, nine ignored by the default run.
- The ignored native Vulkan frame-slot test: one passed in a dedicated run.
- Strict Sugarloaf Clippy across all targets with dependency linting excluded.
- Sugarloaf formatting, benchmark-helper formatting, and `git diff --check`.

The unrestricted Clippy command encounters existing dead-code warnings in
`rio-window/src/platform_impl/input_rate.rs`. The scoped command passes:

```sh
	cargo clippy -p sugarloaf --all-targets --features rio-window/x11 \
		--no-deps -- -D warnings
```

The complete broad capture can be repeated against the retained baseline:

```sh
	python3 misc/benchmarks/sugarloaf.py final-recheck --gpu --validate \
		--reference pending-gpu-baseline --samples 7 --ms 50 --profile
```

## Measurement boundaries

These are local synthetic workloads. CPU affinity reduces scheduling noise;
it does not eliminate frequency changes, thermal effects, or allocator
variation. The Vulkan row-storage and recording fixtures measure CPU costs.
Separate background and glyph fixtures use GPU timestamp queries. These
measurements do not measure display latency. Native validation checks actual
Vulkan frame-slot memory independently of timing samples.

The profiling harness retains executable hashes and source hashes alongside
every capture. Paths under `target/` are local artifacts and are not committed.
