# Windows rendering follow-up

The [full Windows report](windows-branch-performance-report.md) contains 129
cases, three alternating upstream/branch runs, and 24 native captures. Its
9,414 records passed coverage and output validation. The executable comparison
is upstream `59203ccd5fa1e6b760a738c1ad653bb29f2f3465` against branch
`eca0102589caf9b8f34cbc6434944d858126be5e`, using Vulkan on the NVIDIA RTX A500
Laptop GPU. Native profiling uses `ddd0555d1e039741806192b1c4695c916e1473b4`.

## Check normal CPU affinity before interpreting the replay regressions

The main suite pins Rio and its child fixture to logical CPU 2. Under that
constraint, dense replay CPU time increases by 173.6% and scrolling by 108.0%.
Those are single-core stress results.

A separate control ran dense output and scrolling with the normal affinity of
all 20 logical CPUs. It reused the same executable copies and `application.run`
workloads, checked each process's actual affinity, used Normal priority, and
waited three seconds after fixture readiness. Three pairs alternated execution
order. CPU time excludes the child interpreter.

| Replay | Upstream CPU ms | Branch CPU ms | Median paired CPU change | Paired range | Upstream wall ms | Branch wall ms |
|---|---:|---:|---:|---:|---:|---:|
| Dense | 46.875 | 171.875 | +266.7% | -83.3% to +850.0% | 4160.099 | 4163.645 |
| Scroll | 296.875 | 593.750 | +72.7% | +31.6% to +683.3% | 4162.380 | 4163.047 |

Times are medians across replays; percentage changes are medians of paired
ratios. Dense results change direction and do not establish a repeatable
regression or improvement at normal affinity. Scrolling consumes more CPU in
every pair, while both revisions finish the replay in about 4.16 seconds.
The wide ranges and coarse Windows CPU accounting limit the precision of these
estimates. These are CPU costs, not frame latency measurements.

A warm-up diagnostic also retained CPU 2 and compared the usual 300 ms wait
with three seconds. After the longer wait, dense replay used 3656.250 ms of
upstream CPU and 7921.875 ms of branch CPU, with wall durations of 4198.574 ms
and 8083.622 ms. The constrained slowdown therefore survives a longer warm-up.
This diagnostic has one pair per wait duration and is separate from both
three-pair comparisons.

Only dense output and scrolling received the normal-affinity control. The
other application comparisons retain the main report's single-core scope.

## Startup and the next profiling targets

The branch-only `cmd` startup captures have a median first frame at 1609.526 ms
and shell readiness at 1634.076 ms. GPU context construction accounts for
1058.689 ms, including 817.227 ms in GPU instance construction. Surface
configuration records 188.160 ms, device creation 38.325 ms, and pipeline
creation 3.456 ms. These instrumented wall spans overlap; do not add them.

GPU instance construction is the first startup target to investigate.
These captures do not establish that a power-policy change would remove that
work. Power policy was unchanged, and DirectX 12 was not measured.

Scrolling at normal affinity is the next application target. The isolated
cached long UI label draw also regresses from about 1.10 to 1.99 microseconds
per operation. Mixed input parsing, snapshots, and cached terminal rows improve
in the controlled kernel suite, so their gains alone do not establish a faster
Windows application.

## Measurement fixes and retained data

The benchmark verifier now captures the complete Windows client, including
upstream's search box outside the previous crop. Native summaries include only
metrics present in every capture: a one-frame capture has no frame interval.
Zero CPU readings do not produce percentage ratios.

The opt-in profiler records `RIO_PROFILE_SETTLE_MS`, defaulting to two seconds.
Native minimize/restore uses ten seconds to finish all 20 cycles before exit.
This module is excluded from normal Rio builds. All six paired terminal-size
checks and all three native restore captures passed.

Raw samples, metadata, executable hashes, native captures, and the supplementary
control scripts and records are retained in
`target/windows-branch-performance`. The transfer archive is
`target/windows-current-results.zip`. Diagnostics are under `diagnostics/` and
are excluded from the main suite's case counts. No installed executable or
user configuration was replaced.
