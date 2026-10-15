# Windows app icons

Windows tabs use a bundled app catalog for AI tools, editors, file managers,
fuzzy selectors, monitors, version control, infrastructure, databases and other
terminal apps. Codex uses the OpenAI mark, Claude uses its starburst, htop uses
a bar chart, fzf uses a filter and yazi uses a duck. Unknown apps use the terminal
icon. An SSH badge stays before the app icon.

The catalog resolves command aliases, Windows executable and launcher suffixes
such as `.exe`, `.cmd`, `.bat` and `.ps1`, and versioned language runtime names.
Shell app reports and active program status identify the app. Terminal titles
provide a fallback when executable reporting is unavailable.
Returning to a reported shell prompt restores its shell icon, even if an app
left its title behind. Directory paths do not identify apps.

## Custom mappings

Map your command names to any bundled app or named icon in `config.toml`:

```toml
[navigation.app-icons]
my-agent = "codex"
my-files = "yazi"
my-monitor = "htop"
my-tui = "terminal"
```

Named icons such as `folder`, `filter`, `database`, `monitor`, `code`, `ai`,
`openai` and `claude` are also accepted. These mappings reload with the config.
They can override a built-in command's icon. Invalid targets produce a warning
and keep the normal fallback.

For Windows overrides in a shared config, use
`[platform.windows.navigation.app-icons]`. Platform mappings merge with base
mappings, with the platform value winning for the same key.

Icon size remains independent:

```toml
[navigation]
tab-icon-size = 24
```

## App reporting over SSH

An icon catalog cannot discover remote processes through SSH. Source Rio's
shell integration in the remote interactive shell to report the running app and
restore the shell icon at its prompt. The supplied Bash, Zsh, Fish and PowerShell
hooks report `RIO_APP` and `RIO_SSH` through OSC 1337. Source files live under
`frontends/rioterm/src/shell_integration/`.

Title fallback helps when the app updates its terminal title. Apps that neither
report their identity nor set a recognizable title retain the default icon.
This also applies to shells without pre-execution reporting, including cmd.
The renderer does not poll process lists or run subprocesses to guess an app.

## Extending the bundled catalog

Add an app record to
`frontends/rioterm/src/renderer/app_icons/catalog.toml`. An app needs an `id`
and an `icon`; `aliases`, `kind` and `versioned` are optional. Reuse an icon from
the `[icons]` table or a registered logo. Adding a command or alias needs no
renderer change. The catalog loads once, and duplicate aliases or missing icon
references are rejected.

Glyph fallbacks come from the existing bundled Symbols Nerd Font. Check that a
new glyph exists in that font, rather than relying on a newer Nerd Fonts release.
For a brand mark, vendor its SVG and register it in `app_icons/logos.rs`. The
shared renderer supports multiple paths, fills, gradients, transforms, and
non-square view boxes. It preserves the original aspect ratio and brand colors;
dark monochrome marks can use the header tint for contrast.

SVG parsing and rasterization happen once per logo and physical size. Cached
colored spans preserve opacity, drawing order, and partial clipping during tab
dragging. Neovim uses this same path; it has no separate renderer or cache.
No system fonts, SVG text shaping, raster-image decoders, or network access are
enabled in the SVG renderer.

## Icon sources

The OpenAI and Claude SVG files are unchanged copies from Microsoft's
[Codicons project](https://github.com/microsoft/vscode-codicons/tree/main/src/icons),
licensed under [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/).
Sources: [OpenAI](https://github.com/microsoft/vscode-codicons/blob/main/src/icons/openai.svg)
and [Claude](https://github.com/microsoft/vscode-codicons/blob/main/src/icons/claude.svg).
Runtime rendering scales and tints these marks to match the tab.

Glyph names and codepoints follow the
[Nerd Fonts glyph catalog](https://github.com/ryanoasis/nerd-fonts/blob/master/glyphnames.json).
The [bundled-logo source list](../frontends/rioterm/src/renderer/app_icons/logos/README.md)
pins the additional SVG files to upstream revisions and records their licenses.
Neovim uses Jason Long's original gradient mark. Other project marks include
Helix, Goose, Jujutsu, Superfile, and Television. Simple Icons supplies brand
silhouettes for htop, Nushell, Gemini, Copilot, Podman, and other catalog entries.

Tools without a bundled vector keep their glyph fallback. Name matches are
checked against the actual project identity; similarly named products are not
interchangeable. Yazi and fzf currently publish raster logos, so their existing
glyphs remain in use.
