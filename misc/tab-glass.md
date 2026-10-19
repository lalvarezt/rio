# Tab glass

Linux, macOS, and Windows tabs share a thin reflective border. Choose their
shape and reflection strength in `config.toml`:

```toml
[navigation]
preset = "pill"
tab-glass-intensity = 0.5
```

`preset` defaults to `"pill"`, the existing rounded shape. `"square"`
uses square corners, touches the upper-left edge, and places tabs directly
next to each other. Its close buttons stay visible, including on a single tab,
and their space is included in the preferred tab width. On macOS, the native
traffic-light area remains reserved. Both styles share colors, icons,
reflections, status borders, and animations.
The preset reloads with the configuration and supports platform overrides.
The old `tab-style` key and `"windows"` name remain accepted aliases.
See [UI presets and geometry](ui-presets.md) for tab and split overrides.

The default glass intensity is `0.5`. Use `0.0` for a flat tab or `1.0` for the strongest
reflections. Values outside this range are clamped. Changes reload with the
configuration and follow the tab's selection and hover transitions.

The intensity changes only the border. The tab's interior color and opacity
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

## Program status

[OSC 7501 reports](https://www.superlogical.com/rex/docs/build/program-status)
color the pill's border using the terminal theme: green for working or done,
yellow for blocked, and red for errors. Idle and cleared reports restore the
ordinary glass border.

A working or blocked report with `progress=0` through `progress=100` fills the
contour clockwise from the upper-left corner. On a pill, it starts where the
upper-left curve meets the straight top edge. The remaining contour is a faint track.
Without a percentage, the whole border carries the status color. The glass
reflection remains over the color; setting `tab-glass-intensity = 0` removes
only the reflection.

Set the status border thickness in physical pixels:

```toml
[navigation]
tab-status-width = 2.0
```

The default is `1.0`. Values are clamped to `0.0` through `6.0`; `0.0` hides
the status border. Thickness grows inward and leaves the pill size unchanged.
The setting reloads with the configuration and supports platform navigation
overrides. Tabs without a status retain their ordinary glass border.

Each tab shows its focused split's status, including background tabs. The
status border is static between reports and does not need heartbeat updates.
It replaces the OSC 7501 underline and window bar. Hiding the tab strip also
hides its status border. Legacy OSC 9;4 reports keep the window progress bar.

## Window controls and command palette

Windows uses square-cornered caption buttons at the upper-right edge, with
46 × 32 logical-pixel hit areas, system Segoe caption glyphs, light and dark
foregrounds, and the standard red close hover. The colors and glyphs follow
[Microsoft Terminal's caption resources](https://github.com/microsoft/terminal/blob/main/src/cascadia/TerminalApp/MinMaxCloseControl.xaml).
The add-tab button is hidden. macOS keeps its native traffic lights on the
left, and Linux keeps its window-manager controls.

The command palette uses the terminal theme, a proportional system UI font,
and grouped commands. Its height follows the results and available window
space. Keyboard and mouse selection share the same actions, including opening
the font browser and copying a font family name. The result viewport keeps its
height while navigating or scrolling. Group headings share that space; very
short windows omit headings to keep commands accessible.

Search results update when the query or list changes. Pointer movement and
repaints reuse those results. The search caret follows `cursor.blinking` and
`cursor.blinking-interval`; a steady caret schedules no idle frames. The
scrollbar stays visible whenever results overflow and schedules no idle frames.
Mouse-wheel and trackpad scrolling stay inside the palette while it is open.

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

With multiple pill-style tabs, hovering a tab reveals its close button and
shortens the visible title without moving neighboring tabs. Window-style close
buttons are always visible and reserve their width before the title is measured.
Closing an inactive tab preserves the selected tab.

Tabs grow to fit their titles between `navigation.min-tab-width` and
`navigation.max-tab-width`. The width includes the app icon, optional SSH
badge, and persistent close button in window style. Titles get an ellipsis when
the configured maximum or available strip
space cannot fit the complete title.

Linux keeps its window-manager decorations. Windows draws its minimize,
maximize, and close buttons in the header.
macOS keeps its native traffic lights, with the tabs inset to leave room for them.
