# Windows tab glass

The Windows tab pill has a thin reflective border around its rounded contour.
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

For a Windows override in a shared configuration, use
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
Liquid Glass renderer. Other operating systems keep their existing tab style.

## Tab motion and controls

Windows tabs animate into place when opened, closed, reordered, or resized by
a title change. Rapid changes retarget the current motion. Title widths are
cached, and shrinking waits briefly for transient title updates to stop.
Creating, selecting, or closing a tab moves the selection capsule while terminal
focus changes immediately. Keyboard selection wraps through the strip's edges
in the direction of navigation.

Mouse dragging tracks the pointer directly. Adjacent keyboard moves slide the
selected tab above its neighbors. Moving between the first and last positions
wraps the selection capsule through the strip's edges. Hover follows the tab currently beneath the pointer, including
while tabs move.

With multiple tabs, hovering a tab reveals its close button. Closing an inactive
tab preserves the selected tab. The close button uses the existing tab width and
shortens the visible title when needed, without moving neighboring tabs.
