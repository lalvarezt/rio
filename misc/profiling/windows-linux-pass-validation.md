# Windows validation of the Linux performance changes

The Windows baseline is `8385ecbd4d`. The updated build applies the source
changes from the two Linux passes. Both binaries were built and run on
`BASLTOFF0370`; no Windows cross-compilation was used.

Only the mixed Unicode and ASCII terminal writer affects Windows. The native
Vulkan text packing, background bookkeeping, and GLSL changes are compiled
only on Linux. Windows uses WGPU's Vulkan implementation, which is a separate
renderer. The Linux GPU improvements therefore do not carry over to Windows.

## Setup

The machine has an i7-1370P and NVIDIA RTX A500 Laptop GPU. A diagnostic launch
confirmed NVIDIA Vulkan with driver 596.71 and the bundled ConPTY DLL. Both
executables use ConPTY 1.25.260930003 from the same directory.

Both builds use release optimization, LTO disabled, 16 codegen units, no debug
information, and incremental compilation. The cold baseline build took 8m20s;
the updated build took 57.5 seconds against the warmed cache. Compilation had
finished before measurements began.

Raw-input probes run the real parser and terminal on logical CPU 2. Seven
alternating pairs each process 256 KiB fixtures for at least 250 ms per sample.
The parser-only counting sink is measured separately from terminal mutation.

Rendering captures run in the logged-in Windows desktop session, Session 1,
through a temporary interactive scheduled task. They use `cmd /d /q /k`, an
opaque window configured at 1280 × 800, font size 14, and a visible tab strip.
GPU selection is held at Vulkan. The configuration is temporary.

## Raw-input results

Median throughput is in decimal MB/s. The terminal column includes parsing,
grapheme handling, and grid mutation, without GPU rendering or child-shell work.

| Terminal fixture | Baseline | Updated | Batch time change |
| --- | ---: | ---: | ---: |
| ASCII | 883.3 | 854.4 | +3.4% |
| ANSI styles | 285.3 | 280.2 | +1.8% |
| CSI sequences | 209.7 | 210.2 | -0.3% |
| Unicode-heavy | 100.6 | 104.3 | -3.5% |
| Unicode followed by long ASCII runs | 139.4 | 347.5 | -59.9% |
| OSC title and status sequences | 144.4 | 173.7 | -16.9% |

The mixed-input path is about 2.49 times faster. This is the path changed by
the optimization. Parser-only results varied by approximately -8% to +4% in
time despite unchanged parser source, so smaller differences require caution.
The OSC result is not evidence of a new OSC optimization.

A focused rerun used eleven alternating pairs per fixture. ASCII measured
871.6 → 834.3 MB/s, or 4.5% more time per batch. The smaller ASCII slowdown
therefore remains a tradeoff in this build. ANSI measured 283.4 → 294.8 MB/s,
reversing the small regression in the first suite. The ANSI result is not
consistent enough to establish a regression or a gain. These are elapsed-time
throughput measurements of CPU-bound kernels, not direct thread CPU counters.

## Correctness

All 128 deterministic before/after terminal cases matched. These include
wrapping, wide-cell overwrite, combining marks, emoji sequences, styles,
hyperlinks, insert mode, alternate screen, mapped character sets, and disabled
clustering. All raw fixture output hashes also matched across seven pairs.

No permanent tests or new production changes were added for this validation.
Windows source changes were applied without committing there.
After measurement, the temporary source patch and probe binary source were
removed from the Windows checkout. Captured binaries and results remain in
`target/windows-linux-pass`; the interactive scheduled task was removed.

## Rendering controls

The short suite covers startup and idle behavior, 180 full-text redraws, and
180 redraws with four tabs. Each workload has an unmeasured warmup pair and
three alternating measured pairs. Short captures showed substantial startup
and scheduling variation.

A longer suite uses 4,000 frames per process for full-text and four-tab-strip
redraws. It also discards a warmup pair and measures three alternating pairs.
All sixteen long captures completed the requested frames and shell readiness
in desktop Session 1.

The table shows the median of each run's median `frame.screen` wall time.
Process CPU includes kernel and user time, startup, and all captured frames.
It excludes child shells. CPU times are measured with `GetProcessTimes`.

| 4,000-frame workload | Baseline screen ms | Updated screen ms | Baseline process CPU ms | Updated process CPU ms |
| --- | ---: | ---: | ---: | ---: |
| Full-text redraw | 0.536 | 0.652 | 437.5 | 1,015.6 |
| Four-tab strip redraw | 0.517 | 0.467 | 718.8 | 453.1 |

The control results do not establish a consistent rendering gain. Full-text
redraw was slower by the median in this suite while tab-strip redraw was
faster. Process CPU varied from 219 to 1,391 ms within updated full-text runs
and from 297 to 1,688 ms within baseline tab-strip runs. The short suite also
gave different directions for some comparisons. No Windows GPU execution-time
or frame-rate improvement is claimed from these captures.

These are forced redraw workloads, not display-latency or presented-FPS
measurements. The tab-strip workload does not measure tab-switch latency.
Attempts to automate switching did not produce enough verified transition
frames and were excluded from performance results. The shared input writer
does not directly alter Windows GPU rendering.

## Artifacts

Local copies of the raw captures, terminal hashes, build scripts, and metadata
are under `target/windows-linux-pass/`. Windows binaries and captures are under
`D:\git-repos\rio\target\windows-linux-pass`. Generated probes are excluded
from version control.

`raw-results.jsonl` contains seven input pairs; `focused-ascii.jsonl` and
`focused-ansi.jsonl` contain eleven focused pairs each. `check-before.jsonl`
and `check-after.jsonl` hold the 128 terminal equivalence cases.
`desktop-results.jsonl` contains the short controls, and
`desktop-long-results.jsonl` contains the 4,000-frame controls. `metadata.json`
records the toolchain, source hash, and executable hashes.
