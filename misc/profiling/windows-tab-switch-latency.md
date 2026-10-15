# Windows tab switch latency

The animated tab bar exposed a presentation queue bottleneck. With FIFO
presentation, repeated next-tab selection took about 100 ms to reach visible
terminal content, compared with 53 ms before the Windows top-bar changes.
Keeping the animations and using supported mailbox presentation reduced that
median to 19 ms on the development Windows machine.

## Comparison

All executables were debug builds on the same Windows desktop:

- Original: `cc1f59432e`, before the first Windows top-bar commit.
- Before fix: `c919aa7c7f`, including the previous animation and cache fixes.
- After fix: the presentation change accompanying this report.

Each run opened two cmd-hosted terminal fixtures with different background
colors and dense fixed text. The client measured 1600 by 800 pixels with font
size 14. The comparison explicitly selected Vulkan for every executable and
used opaque windows. No compilation ran during measurement.

A native desktop probe posted the next/previous-tab key bindings and polled an
8 by 8 pixel region in the terminal content with BitBlt. Latency runs from
posting the input until the terminal background changes. It includes desktop
capture and polling overhead; it is not a GPU timestamp or physical display
scanout measurement. The bindings invoke the same selection actions as
Ctrl+Shift+L/H.

Three interleaved rounds compared original, before-fix and after-fix builds.
Each run observed 16 spaced selections, 96 repeated next selections and 48
repeated previous selections: 1,440 observed changes in total. Spaced requests
had a minimum gap of 250 ms; repeated requests had a minimum gap of 33 ms and
used the key-repeat bit. Each request waited for visible completion before
the next request. This comparison measures individual input latency under
ongoing animation, rather than an unconditional 30 Hz input flood.

The table reports the median of the three run medians and, separately, the
median of their p95 values. All values are milliseconds.

| Selection | Original median / p95 | Before fix median / p95 | After fix median / p95 |
| --- | ---: | ---: | ---: |
| Spaced | 53.2 / 132.2 | 100.2 / 159.7 | 35.4 / 66.6 |
| Repeated next | 52.7 / 83.0 | 100.0 / 119.1 | 19.1 / 49.9 |
| Repeated previous | 56.9 / 93.3 | 100.2 / 128.0 | 29.5 / 38.0 |

The measurement readiness and sample-count checks passed in every confirming
run. A separate after-fix run without a backend override measured repeated-next
median 30.9 ms and p95 49.2 ms. These results describe this machine and workload;
occasional outliers remain, including a 102.9 ms repeated-next sample after
the fix.

## Cause and change

A diagnostic build that disabled only tab motion, retaining FIFO presentation,
restored repeated-next median latency to 51.3 ms. Chrome CPU rendering was
roughly 0.3 ms, and frame cadence alone did not explain the delay. Together with
the mailbox comparison, this points to continuous animation filling the FIFO
presentation queue with older frames ahead of new terminal content.

`WgpuContext::resize` now selects mailbox presentation on Windows when the
surface advertises support. Other cases retain FIFO. Initialization calls this
existing configuration path, removing the duplicate surface setup. The runtime
change adds no state or public interface and removes 15 source lines overall.
Tab motion, easing, wrap behavior and rendering remain unchanged.

## Native validation

The final normal debug executable built successfully. A separate replay posted
180 repeated next-tab events at 33 ms intervals with two tabs. It completed
without a hang; the process used 1.61 CPU seconds during the 12.73 second
launch, setup, replay and shutdown run. This replay did not force continuous
redraws through the profiling harness.

Native captures with blur and opacity enabled were inspected for ordinary
selection, wraparound and tab closing. The captured pills retain rounded edges
and the intended motion. The older pixel checker reported split pills because
its fixed scanline intersected text at the current display scale; its automatic
visual verdict is not used as validation for this run.

No release build or repository tests were added. Temporary timing, capture and
analysis helpers remain under ignored `target/`. The Windows checkout retains
the original, before-fix and final debug executables for comparison:
`rio-switch-original.exe`, `rio-switch-current.exe` and `rio-switch-final.exe`.
The normal `target/debug/rio.exe` contains the final change.

Raw confirming samples are under
`target/profiles/switch-latency-{original,current,final}-confirm-{1,2,3}/`.
Additional results are in `switch-latency-final-default`,
`held-tabs-mailbox-normal` and `tab-regression-mailbox-final` under the same
directory. These ignored artifacts are retained locally and in the Windows
checkout, rather than committed as permanent profiling infrastructure.
