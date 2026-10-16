# Windows animation modes and frame pacing

`navigation.animation` accepts `none`, `medium`, and `full`. The default is
`full`. The setting reloads live and supports platform overrides such as
`platform.windows.navigation.animation`. See [tab motion controls](../tab-glass.md#tab-motion-and-controls).

`none` updates tab geometry, selection, opacity, and hover immediately. `medium`
uses shorter springs, immediate fades, and an animation-only frame budget of
half the monitor refresh rate. `full` preserves the existing spring and fades
and requests animation continuations on the native display clock. Dragging
continues to track the pointer in every mode.

These frame budgets apply to animation continuations. Fresh keyboard, mouse,
or terminal changes can request a paint immediately. The mode setting controls
tabs and header decoration; cursor trails retain their separate effects
settings, and progress indicators continue to animate as functional feedback.

## Existing motion already eases

Tab positions use a critically damped spring, not linear interpolation. Its
offset follows `(a + (a * omega + velocity) * t) * exp(-omega * t)`. Retargeting
preserves the current position and velocity. A fixed Bézier transition would
need additional interruption handling to keep that behavior. Replacing the
curve would not remove rendering work by itself.

Frame allocation follows the visible work instead. Full motion uses display
ticks during travel. Title shrink waits schedule a wakeup rather than redrawing
an unchanged layout. When offsets are below half a physical pixel, velocities
are low, and fade differences are below 1%, the next wakeup can wait 33 ms.
Existing final spring and fade thresholds still complete the transition.

On a 60 Hz display, medium's animation budget is 30 fps. This divides evenly
into display refreshes. A fixed 24 fps budget on that display would alternate
between two and three refreshes per animation frame.

## Shared Windows fix

Windows already has a DwmFlush worker for display timing. A leftover WM_PAINT
continuation immediately invalidated the window again, bypassing that worker
and producing hundreds of frames per second during animation.

WM_PAINT now rearms the worker after clearing the paint. Requests made during
painting wait for the next composition tick. Requests made outside painting
invalidate immediately, preserving input responsiveness. The existing request
flag represents whether painting is active and whether another frame was
requested; nested paints preserve that state. The Window object's duplicate
atomic flag handle is removed, while the existing worker registry owns it.

This change applies to Windows repaint continuations across the application.
It is shared by tab and header motion, cursor trails, scrollbars, and progress
indicators. The audit found that scrollbars already sleep during their visible
hold period and that cursor trails stop requesting frames when settled.
Palettes and assistant overlays redraw on input rather than running a separate
animation loop. Their curves and timings do not need replacement to benefit
from the shared pacing fix.

Delayed UI work uses the existing window scheduler. Its earliest deadline wins,
so an earlier animation wakeup can replace a pending title-shrink or scrollbar
deadline. The scheduled redraw marks UI damage, including the final settled
frame, without rebuilding unchanged terminal cells.

## Validation method

The baseline is `97225ef5fb`. Both baseline and changed executables are Windows
debug builds with the same profiling feature. Temporary, identical startup
probes allow configured cmd fixtures and manual shutdown. They do not force
continuous frames. The probes and replay helpers remain under ignored
`target/`; no production profiling interface or repository tests are added.

The desktop reports 1920 by 1080 at 60 Hz. Each replay uses a 1600 by 800 window,
the same Vulkan configuration, two cmd-hosted fixtures, and disabled cursor
blinking. Three interleaved rounds compare baseline, full, medium, and none.
Each replay sends ten tab selections and three title growth/shrink pairs,
spaced 650 ms apart. A separate three-round comparison moves a visible cursor
sixteen times with the existing cursor trail enabled and no tab actions.

Presented-frame counts start after shell readiness and include common setup
and settling. Process CPU time covers the roughly 10.6 second action replay.
CPU measurements vary with Windows background activity; frame counts are the
stronger evidence for removed work. Compilation and capture runs do not overlap
the timing replays.

## Frame and CPU results

The table gives medians across three valid runs. Presented frames include
startup settling after both fixture shells become ready.

| Tab replay | Presented frames | Fewer frames than baseline |
| --- | ---: | ---: |
| Before | 2,401 | |
| Full | 306 | 87.3% |
| Medium | 115 | 95.2% |
| None | 20 | 99.2% |

The cursor-trail comparison exercises a separate animation path. The replay
explicitly focuses the window and selects the fixture tab before measuring.
Runs that do not exercise the trail are rejected.

| Cursor replay | Presented frames | Process CPU time |
| --- | ---: | ---: |
| Before | 2,910 | 8.50 s |
| After | 425 | 2.20 s |

That removes 85.4% of presented frames and reduces reported CPU time by 74.1%
in this cursor workload. Its spring, opacity, and decay settings are unchanged.
Tab-mode CPU samples vary substantially with system activity and do not give a
reliable ordering between medium and none; their reproducible frame counts
show the removed animation work more clearly.

An intermediate attempt delayed every frame through a software timer. It
produced roughly 32 ms frame intervals on this 60 Hz desktop, so it was
discarded. Another attempt deferred fresh input to the display worker and
added a refresh of input latency. The final implementation limits repaint
continuations while allowing fresh requests to invalidate immediately.

## Ordinary debug-build checks

Both revisions were also built without the profiling feature or temporary
startup probes. Three runs each measured key-message to visible terminal
background change using an 8 by 8 desktop sample. Each run sent 16 spaced
selections, 96 held-next selections, and 48 held-previous selections. Held
requests had a minimum 33 ms interval and waited for each visible response;
this measures response latency rather than an unlimited input queue.

| Input workload | Before median / p95 | After median / p95 |
| --- | ---: | ---: |
| Spaced selections | 34.1 / 52.4 ms | 33.2 / 51.5 ms |
| Held next | 45.2 / 105.7 ms | 34.1 / 52.5 ms |
| Held previous | 50.5 / 121.3 ms | 34.0 / 51.9 ms |

All three final runs completed for both revisions. An earlier baseline attempt
failed the helper's one-second response deadline and was rerun; its incomplete
samples are excluded. Desktop sampling includes composition and capture
overhead, so these values are not internal action-dispatch timings.

Native desktop captures cover all three modes, two- and three-tab selection,
wraparound, interrupted movement, creation, closing by keyboard and mouse,
selection during a pending title-shrink wakeup, and live reload between none
and full. Platform overrides were exercised with a global full setting.
The final ordinary executable also completed 180 held navigation requests at
33 ms intervals with the animation setting omitted, exercising default full.

The ordinary Windows debug build succeeds. Existing renderer tests (90) and
backend configuration tests (83) pass, along with formatting and compile checks.
No release build or new repository tests were added. Linux and macOS use the
shared mode and scheduling changes; the native repaint correction is Windows
only, and its timing results should not be generalized to other platforms.

Raw final timing samples remain under ignored `target/profiles/` in
`animation-responsive-{before,full,medium,none}-{1,2,3}` and
`animation-cursor-{before,after}-{1,2,3}`. Normal-build input samples are in
`switch-latency-animation-normal-{before,after}-input`. Earlier diagnostic runs are retained
separately and are not included in these results.
