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
