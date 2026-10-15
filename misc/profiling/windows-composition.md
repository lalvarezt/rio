# Windows transparency and close captures

The pale rectangle came from the Windows redirection surface beneath GPU
presentation. Extending the native DWM frame across the custom title bar also
exposed native caption buttons. These were separate from drawing the Rio tab
strip twice.

Rio now requests `WS_EX_NOREDIRECTIONBITMAP` before creating a native GPU window.
Changing that style after creation did not remove an existing rectangle in the
probe. CPU rendering and explicit OpenGL-only backend requests retain the
redirection bitmap because their presentation needs it. The existing
`windows-use-no-redirection-bitmap` setting still overrides the automatic choice.

Undecorated windows use zero DWM frame margins. Decorated windows retain the
existing full-client frame extension for system backdrops. The native Acrylic
request is unchanged.

## Capture results

These runs use the logged-in Windows desktop, a cmd shell, the user's theme,
and temporary configuration copies. The real configuration is not changed.
Transparency uses `opacity = 0.20`; the blur cases also enable `blur = true`.
Quit confirmation is disabled in those copies so closing reaches teardown.
Each capture samples approximately 1.35 seconds, including the close transition.

The harness sends button input to Rio's HWND rather than moving the system
pointer. It records process exit, checks that terminal text is visible, and
excludes pixels covered by other top-level windows. This prevents a blank
OpenGL window or another foreground application from being counted as a pass.

| Configuration | Captures | Close attempts | White rectangle detected |
| --- | ---: | ---: | --- |
| Vulkan, blur | 5 | 4 | No |
| Vulkan, opacity only | 3 | 2 | No |
| Vulkan, opaque | 3 | 2 | No |
| DX12, blur | 3 | 2 | No |
| OpenGL, blur | 3 | 2 | No |
| CPU, opaque | 3 | 2 | No |

All 14 close attempts exit successfully. They include the custom close button
and `WM_CLOSE`. Every capture contains visible terminal content.
The CPU case sets `use-cpu = true` in `[renderer]`; platform renderer overrides
do not expose that setting. Its native window retains the redirection bitmap.

Restoring the legacy redirection bitmap and full-client frame reproduces a
362,080-pixel pale rectangle and the duplicate native caption controls. The
corrected Vulkan blur captures contain at most 593 light text pixels, with no
large pale region. The explicit legacy override remains a failing control.

Two additional patterned-background captures verify opacity transmission and
keep the native Acrylic mode enabled. DWM reports backdrop type 3 in the blur
captures. The native material is flat gray with both the legacy and corrected
backing settings, including full-frame probes. These runs
therefore establish that the material request is retained, rather than proving
a particular blur radius or background sampling behavior.

## Reproduction artifacts

The temporary desktop capture harness and probes are under `target`, separate
from the application and normal builds. Their JSON records and peak-frame PNGs
are in `D:\git-repos\rio\target\profiles` and the corresponding local `target`
directory. The final capture labels begin with `close-final-`; the failing
control is `close-material-legacy`.

The normal and profiling executables are built on Windows with the fast release
settings in [the profiling guide](README.md). The profiling executable exits on
its readiness timer, so the close captures use the normal `rio.exe`.

The temporary harness can be run from an interactive Windows desktop terminal:

```powershell
powershell -NoProfile -STA -ExecutionPolicy Bypass -File target\rio-close-probe.ps1 -Label reproduction -Blur -Backend vulkan -Modes inspect,close,wmclose
```

Three startup captures and three eight-tab, 180-frame full-text redraw captures
pass after the fix. Every shell reaches readiness and every frame workload
completes. The median shell frame arrives at 1768 ms for one tab and 2946 ms
for eight tabs. Text captures have a 16.611 ms median frame interval and an
18.558 ms p95. The median tab-strip render scope is 0.072 ms. These are CPU
wall-clock timings, including driver waits, rather than GPU execution times.
They are regression measurements, not an attributed startup speedup.

Those reports are under `target/profiles/composition-startup` and
`target/profiles/composition-text` in both checkouts.

The earlier startup, text-rendering, and backend measurements remain in
[Windows profiling results](windows-results.md) and
[renderer selection](windows-renderer-selection.md).
