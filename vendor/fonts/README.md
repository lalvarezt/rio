# Bundled Windows terminal fonts

These are unchanged copies of the files used by the Windows Rio configuration
when the runtime was validated on 2026-10-24. Rio embeds them in the Windows
executable and uses them directly when their configured family is requested.
No separate font installation is required.

| File | Configured use | Version in font metadata |
| --- | --- | --- |
| `JetBrainsMonoNerdFontMono-Regular.ttf` | Regular text; bold aliases regular when `fonts.bold.style = false` | JetBrains Mono 2.304, Nerd Fonts 3.1.1 |
| `MapleMono-Italic.ttf` | Italic text, weight 400 | Maple Mono 7.900 |
| `MapleMono-BoldItalic.ttf` | Bold italic text, weight 700 | Maple Mono 7.900 |
| `SymbolsNerdFontMono-Regular.ttf` | Private-use prompt and status symbols | Nerd Fonts 3.4.0 |

The files were copied from the active per-user Windows font directory. The
machine also contained newer system-wide JetBrains files; those were not the
files selected by the current configuration. `SHA256SUMS` identifies the exact
copies. No glyphs, family names, or font metadata were modified here.

Sources: [JetBrains Mono](https://github.com/JetBrains/JetBrainsMono),
[Nerd Fonts](https://github.com/ryanoasis/nerd-fonts), and
[Maple Mono](https://github.com/subframe7536/maple-font).
Redistribution terms are in [LICENSE.txt](LICENSE.txt), copied beside Rio as
`Font-LICENSE.txt` by the Windows `just install*` runtime preparation.

The direct family mappings live in [mod.rs](../../sugarloaf/src/font/mod.rs). These are the
configured terminal faces; they do not include a color emoji font.
