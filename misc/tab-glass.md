# Tab glass

Linux, macOS, and Windows tab pills have a thin reflective border around their rounded contours.
Set its strength in `config.toml`:

```toml
[navigation]
tab-glass-intensity = 0.5
```

The default is `0.5`. Use `0.0` for the flat pill or `1.0` for the strongest
reflections. Values outside this range are clamped. Changes reload with the
configuration and follow the tab's selection and hover transitions.

The intensity changes only the border. The pill's interior color and opacity
stay the same, with no sheen or gradient painted over its contents.

For a platform override in a shared configuration, use
`[platform.linux.navigation]`, `[platform.macos.navigation]`, or
`[platform.windows.navigation]` instead of `[navigation]`.

The effect also works with an opaque window. Enable the Windows backdrop
separately for a blurred background:

```toml
[window]
opacity = 0.10
blur = true
```

Glass intensity changes the border's reflections, not the window opacity or the
Windows Acrylic material. It does not refract the desktop or use Apple's native
Liquid Glass renderer.

## Tab motion and controls

Set the amount of tab motion in `config.toml`:

```toml
[navigation]
animation = "full"
```

- `full` is the default. It keeps the existing slides, fades, and hover transitions.
- `medium` uses shorter slides at half the monitor refresh rate and applies
  opacity and hover changes immediately. On a 60 Hz display its animation budget
  is 30 fps. Dragging still follows the pointer at the full monitor rate.
- `none` applies tab positions, widths, selection, and hover changes immediately.
  Title widths also shrink immediately.

The setting reloads with the configuration and supports the same platform
overrides as `tab-glass-intensity`. It controls tab and header decoration,
while progress indicators and the separately configured cursor effects retain
their behavior. Dragging continues to follow the pointer in every mode.

On Windows, animation continuations use the existing DWM display clock.
The fast part of `full` follows display refresh. `medium` uses a two-refresh
interval budget between animation frames.
Keyboard and terminal input request redraws independently of that delay.
During the 120 ms title-shrink wait, the renderer schedules a wakeup instead of
redrawing an unchanged strip. Near the end of a transition, it requests a wakeup
after 33 ms when all remaining position and width offsets are below half a
physical pixel, their velocities are low, and fade differences are below 1%.
This reduces work in the nearly stationary part of an animation while visible
travel keeps the normal monitor pacing.

Motion uses a critically damped spring rather than linear interpolation.
Position and velocity carry across interruptions, so a held shortcut or a rapid
reversal retargets the current movement without restarting a fixed curve.
See the [Windows profiling results](profiling/windows-animation-modes.md) for
frame counts, input timings, and the shared repaint improvement.

Linux, macOS, and Windows tabs animate into place when opened, closed, reordered, or resized by
a title change. Rapid changes retarget the current motion. Title widths are
cached, and shrinking waits briefly for transient title updates to stop.
Creating, selecting, or closing a tab moves the selection capsule while terminal
focus changes immediately. Keyboard selection wraps through the strip's edges
in the direction of navigation.
Holding a navigation shortcut retargets unfinished circular motion to the nearest
copy of the latest selected tab, preventing extra laps from building up.

Mouse dragging tracks the pointer directly. Adjacent keyboard moves slide the
selected tab above its neighbors. Moving between the first and last positions
wraps the selection capsule through the strip's edges. Hover follows the tab currently beneath the pointer, including
while tabs move.

With multiple tabs, hovering a tab reveals its close button. Closing an inactive
tab preserves the selected tab. The close button uses the existing tab width and
shortens the visible title when needed, without moving neighboring tabs.

Tabs grow to fit their titles between `navigation.min-tab-width` and
`navigation.max-tab-width`. The width includes the app icon and optional SSH
badge. Titles get an ellipsis when the configured maximum or available strip
space cannot fit the complete title.

Linux keeps its window-manager decorations and shows a new-tab button in the
strip. Windows draws its minimize, maximize, and close buttons in the header.
macOS keeps its native traffic lights, with the tabs inset to leave room for them.
