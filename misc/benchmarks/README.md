# Run the branch performance comparison

The suite compares upstream `59203ccd5f` with the committed branch. It calls
the production parser, terminal, shaping code, UI modules, and GPU renderers.
The complete change inventory is
[branch-performance.md](../profiling/branch-performance.md).

## Build the Linux executables

Install the project's normal Linux build dependencies, Python 3, KWin,
Xwayland, and `dbus-run-session`. The GPU runner creates its own virtual
display. It does not need a login desktop or change your power settings.

Commit the benchmark sources and inventory before measuring. Then run:

```sh
python3 misc/benchmarks/run.py build
python3 misc/benchmarks/run.py run --runs 3 --cpu 2
python3 misc/benchmarks/report.py
```

The build uses optimized code with LTO disabled, 16 codegen units, incremental
compilation disabled, and matching settings for both revisions. Each revision
has a separate build directory. It preserves immutable executable copies under
`target/branch-performance/binaries`. The upstream worktree receives benchmark
fixtures and test hooks. The worktree remains detached at the pinned upstream
commit. The runner does not switch or reset your working branch.

Python driver and report changes do not require recompiling unchanged Rust
executables. The run records both revisions. Production-code, asset, manifest,
and compiled-fixture changes still require a rebuild.

To run without a GPU or virtual display, add `--no-gpu` to both build and run.
To repeat one area, set `--filter`, for example `--filter input/mixed`.
The Rust helpers support `RIO_BENCH_SAMPLES`, `RIO_BENCH_MS`, and
`RIO_BENCH_FILTER`. The runner sets these from its arguments.

The main comparison includes the existing `rio-vt/vt_input` and
`corcovado/bench_poll` Criterion benches. Their measurements use the same
fixtures and Criterion settings on both revisions. They remain separate from
the new benchmarks in the report.

## Run an individual benchmark

```sh
cargo bench -p rio-vt --no-default-features --bench branch_performance
cargo bench -p rio-grid --bench branch_performance
cargo bench -p sugarloaf --features rio-window/x11 --bench branch_performance
cargo bench -p sugarloaf --features rio-window/x11 --bench branch_gpu
cargo bench -p sugarloaf --features wgpu,rio-window/x11 --bench branch_wgpu
cargo test --release -p rioterm --bin rio -- branch_bench --ignored --nocapture --test-threads=1
```

The GPU commands require an X11 display. Use the complete runner to provide an
isolated display. The frontend cases are ignored performance tests so they
can reach private production functions without widening application APIs.
They do not run in ordinary test executions.

The WGSL comparison needs a baseline worktree:

```sh
RIO_BENCH_BASELINE="$PWD/target/branch-performance/upstream" cargo bench -p sugarloaf --features wgpu,rio-window/x11 --bench branch_shader
```

## Run Windows-only cases

Build `rio-profile` on Windows with the repository's normal fast profiling
configuration. Run the following command in an interactive Windows desktop:

```powershell
python misc/benchmarks/windows.py target/release/rio-profile.exe
```

The runner covers startup and exit, multiple windows, multiple tabs, dense
redraw, keyboard-driven motion at all three levels, and minimize/restore.
It uses temporary configuration under `target`, does not replace your installed
Rio, and refuses to send keyboard input if the profiling window loses focus.
Motion captures fail if there are not enough frames to prove the input was
exercised. These Windows cases are not part of the Linux result set.

## Inspect generated artifacts

The runner writes these files under `target/branch-performance`:

- `build.json`: toolchain, build settings, source revision, fixture hash, and executables.
- `run.json`: affinity, sample settings, pair count, and display mode.
- `results.jsonl`: individual timings, allocation probes, counters, and image checks.
- `run-*.log`: unfiltered output for each executable and pair.
- `application/`: temporary configs, shell readiness markers, and process logs.
- `display.log`: isolated compositor diagnostics.

The report retains per-case results, applicable change coverage, and checks
that ran. A missing result is a coverage gap, not a zero-cost operation.
