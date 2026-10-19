# UI presets and geometry

Choose a preset in the existing navigation configuration:

```toml
[navigation]
preset = "square"
```

`pill` is the default. It keeps the existing rounded tabs and upstream panel
defaults. `square` has square tabs touching the top and left edges, no tab gaps,
and always-visible close buttons. Its panes have square corners, zero margins,
and one-pixel borders. Presets work on Linux, macOS, and Windows.

Presets supply defaults. Explicit settings override individual defaults,
including values of zero. Changing a preset leaves those overrides intact.
You can reproduce either preset entirely with explicit tab and panel settings.
The renderer uses the resolved values for both presets.

Complete editable definitions are in [pill.toml](presets/pill.toml) and
[square.toml](presets/square.toml). They can be used as config files or copied
into your existing config. Those files make every geometry value explicit;
remove values that you want to inherit when changing the selected preset.

Existing `tab-style = "pill"` and `tab-style = "windows"` configurations still
work. `"windows"` is an alias for `"square"`. Prefer `preset` in new configs,
and use only one of `preset` or `tab-style` in a navigation table.

## Customize the layout

Keep the upstream root `margin` and `[panel]` settings for pane layout. Tab
geometry goes under `[navigation.tabs]`:

```toml
# Window edge to the pane layout, in top/right/bottom/left order.
margin = [6, 10, 8, 12]

[navigation]
preset = "square"
min-tab-width = 120
max-tab-width = 440
tab-icon-size = 28

[navigation.tabs]
height = 78
margin = [10, 22, 12, 18]
padding = [2, 14, 6, 20]
gap = 12
radius = 10
font-size = 18
text-offset-y = 2
icon-gap = 8
close-button = "never"

[panel]
margin = [2, 6, 3, 8]
padding = [10, 14, 12, 16]
row-gap = 10
column-gap = 14
border-width = 2
border-radius = 3
```

This keeps the square preset as a starting point while giving its tabs rounded
corners, adding gaps, and hiding their close buttons. A zero `border-radius`
keeps pane corners square. A zero `border-width` removes pane borders.

All geometry uses logical pixels and scales with display DPI. Arrays use
upstream's CSS-style shorthand: `[all]`, `[vertical, horizontal]`, or
`[top, right, bottom, left]`.

There are three different distances to control:

- Root `margin` places the pane layout inside the window. Its top value is
  added below the bottom edge of the tabs.
- `[panel].margin` places a pane inside its layout slot. The visible distance
  between two panes includes both neighboring margins plus the row/column gap.
- `[panel].padding` separates terminal cells from the pane edge. The border is
  painted at the edge; increasing its width does not add layout padding.

Tab `margin` is independent of root `margin`. Its left and right values reserve
space around the strip. Top and bottom inset the tab shapes inside `height`.
The terminal layout starts at `height - margin.bottom`, plus root top margin
and the pane's own margin and padding. Increase root top margin to add space
between the tab shapes and panes without making the tabs taller.

## Tab geometry reference

All fields in `[navigation.tabs]` are optional.

| Field | Pill default | Square default | Meaning |
|---|---|---|---|
| `height` | `54` | `54` | Preferred overall strip height |
| `margin` | `[6, 11, 6, 9]` | `[0, 8, 6, 0]` | Top, right, bottom, left outer insets |
| `padding` | `[0, 14, 0, 12]` | Same | Interior spacing for icon/title content |
| `gap` | `6` | `0` | Space between tab shapes |
| `radius` | `19` | `0` | Tab corner radius, limited by the shape size |
| `font-size` | `15` | `15` | Tab title font size |
| `text-offset-y` | `2` | `2` | Optical title adjustment relative to centered icons |
| `icon-gap` | `8` | `8` | Space after each app or SSH icon |
| `close-button` | `"hover"` | `"always"` | `"always"`, `"hover"`, or `"never"` |
| `close-button-size` | `20` | `20` | Close-button target and visual size |
| `close-button-inset` | `12` | `12` | Space from the shape's right edge to that target |
| `close-button-gap` | `8` | `8` | Space between the close-button target and title |

`"hover"` shows the close button on a hovered tab when multiple tabs are open.
`"always"` reserves title space and displays the button even with a single tab.
`"never"` removes both the button and its reserved space. On very narrow tabs,
the button disappears when its target cannot fit. Keyboard close actions
remain available.

The preferred height is clamped to 32–512 logical pixels, then increased if
necessary to fit the configured font, icon, button, and vertical insets.
Font size is clamped to 8–96 and button size to 8–96. Other distances are
nonnegative and limited to 1024; text offset permits -64–64. Non-finite values
fall back to the selected preset. A small radius never makes other fields
switch behavior: gaps, close buttons, and panel settings remain independent.

Existing navigation settings still control title width, icon size, animation,
glass reflection, status-border thickness, tab colors, and inactive pane dimming.
See [tab glass](tab-glass.md) and [app icons](app-icons.md).

## Panel preset defaults

These use the existing `[panel]` configuration keys:

| Field | Pill default | Square default |
|---|---|---|
| `margin` | `[4]` | `[0]` |
| `padding` | `[8]` | `[8]` |
| `row-gap` | `0` | `0` |
| `column-gap` | `0` | `0` |
| `border-width` | `2` | `1` |
| `border-radius` | `8` | `0` |

Borders follow the existing split rendering rules. Configuring a preset does
not force a border around a single unsplit terminal.

## Platform overrides and reload

Platform settings merge individual fields before resolving the preset:

```toml
[navigation]
preset = "pill"

[navigation.tabs]
font-size = 17

[panel]
padding = [10]

[platform.windows.navigation]
preset = "square"

[platform.windows.navigation.tabs]
height = 60

[platform.windows.panel]
column-gap = 4
```

On Windows this selects square geometry while retaining the global 17-pixel
title font and panel padding. On other systems the global pill preset applies.
These tables are optional; the same presets and overrides work without any
platform-specific section.

Geometry changes reload in place, including existing splits. Drawn shapes,
title measurement, hit targets, and terminal padding use the same resolved
geometry. Native window controls retain their platform styles and protected
areas: Windows caption-button width is reserved, and macOS reserves at least
76 logical pixels on the left for traffic lights. Use the existing `[window]`
settings for native decorations and traffic-light positions.

## Linux verification

The config tests cover legacy aliases, independent explicit overrides, zero
values, platform merges, sparse serialization, margin round trips, and invalid
numbers. The frontend geometry test checks painting, input targets, close-button
bounds, and terminal top spacing against the same custom values.

Live Linux checks render pill, square, custom geometry, and a square layout
manually reproduced from pill. The manual version is pixel-identical to the
preset. Changing the geometry with three panes open and restoring the original
config restores the original rendered pixels. The fixture redraws after PTY
resize so terminal text reflow is not mistaken for a geometry mismatch.

Validation passed on Linux with 223 backend tests and 212 frontend tests.
Both the software renderer and native Vulkan passed the live pixel comparisons.
The optimized `branch_bench_geometry` probe covers built-in and custom settings
for both presets. Pinned to logical CPU 2 with seven samples per case, layout
plus twenty pointer checks took 0.235–0.262 microseconds per batch and allocated
zero times in every case. These are focused CPU checks, not frame-time results
or a before/after speedup claim. Logs and previews are under `target/ui-presets`.
