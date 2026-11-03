use super::*;
use rio_backend::config::colors::{ColorArray, ColorRgb, Colors};
use rio_backend::sugarloaf::grid::cpu::CpuGridRenderer;
use std::cell::Cell;

#[derive(Default)]
struct Palette {
    colors: Colors,
    calls: Cell<usize>,
    keep_selection_color: bool,
}

impl GridPalette for Palette {
    fn named_colors(&self) -> &Colors {
        &self.colors
    }

    fn compute_color(
        &self,
        color: &AnsiColor,
        flags: StyleFlags,
        term_colors: &TermColors,
    ) -> ColorArray {
        self.calls.set(self.calls.get() + 1);
        let mut color = match color {
            AnsiColor::Spec(rgb) => rgb.to_arr(),
            AnsiColor::Named(NamedColor::Background) => self.colors.background.0,
            AnsiColor::Named(named) => self.color(*named as usize, term_colors),
            AnsiColor::Indexed(index) => self.color(*index as usize, term_colors),
        };
        if flags.contains(StyleFlags::DIM) {
            for channel in &mut color[..3] {
                *channel *= 0.5;
            }
        }
        color
    }

    fn compute_bg_color(&self, style: &Style, term_colors: &TermColors) -> ColorArray {
        self.compute_color(&style.bg, style.flags, term_colors)
    }

    fn color(&self, index: usize, term_colors: &TermColors) -> ColorArray {
        term_colors[index].unwrap_or(self.colors.foreground)
    }

    fn use_drawable_chars(&self) -> bool {
        true
    }

    fn opacity_cells(&self) -> bool {
        false
    }

    fn cell_bg_alpha(&self) -> u8 {
        255
    }

    fn ignore_selection_fg_color(&self) -> bool {
        self.keep_selection_color
    }
}

fn row() -> (Row<Square>, RowStyles) {
    let mut row = Row::new(128);
    row.inner.fill(Square::from_char('x'));
    let mut styles = RowStyles::default();
    let cells = styles.make_mut(128);
    for (index, style) in cells.iter_mut().enumerate() {
        style.fg = AnsiColor::Spec(ColorRgb {
            r: 240,
            g: 80,
            b: 32,
        });
        style.bg = AnsiColor::Spec(ColorRgb {
            r: 16,
            g: 96,
            b: 192,
        });
        style.flags = match index / 32 {
            0 => StyleFlags::empty(),
            1 => StyleFlags::DIM,
            2 => StyleFlags::INVERSE,
            _ => {
                style.fg = AnsiColor::Named(NamedColor::Foreground);
                StyleFlags::empty()
            }
        };
    }
    (row, styles)
}

fn render(
    row: &Row<Square>,
    styles: &RowStyles,
    palette: &Palette,
    colors: &TermColors,
    selection: Option<RowSelection>,
    hints: &[RowHint],
) -> Vec<CellText> {
    let fonts = FontLibrary::default();
    let mut rasterizer = GridGlyphRasterizer::new();
    let mut grid = GridRenderer::Cpu(CpuGridRenderer::new(128, 1));
    let mut foreground = Vec::new();
    build_row_fg(
        row,
        128,
        0,
        styles,
        &ExtrasMap::default(),
        palette,
        colors,
        &mut rasterizer,
        &mut grid,
        16.0,
        8.0,
        20.0,
        selection,
        hints,
        None,
        &fonts,
        0,
        None,
        &mut foreground,
    );
    assert_eq!(foreground.len(), 128);
    foreground
}

#[test]
fn mixed_row_foregrounds_reuse_palette_resolution() {
    let (row, styles) = row();
    let palette = Palette::default();
    let mut colors = TermColors::default();
    for foreground in [[0.25, 0.5, 0.75, 1.0], [0.75, 0.25, 0.5, 1.0]] {
        colors[NamedColor::Foreground] = Some(foreground);
        palette.calls.set(0);
        let glyphs = render(&row, &styles, &palette, &colors, None, &[]);
        assert!(
            palette.calls.get() <= 8,
            "expected color reuse across style runs, got {} palette calls",
            palette.calls.get()
        );
        for glyph in glyphs {
            let column = glyph.grid_pos[0] as usize;
            assert_eq!(
                glyph.color,
                cell_fg(row[Column(column)], styles.at(column), &palette, &colors)
            );
        }
    }
}

#[test]
fn mixed_row_foregrounds_preserve_selection_and_hint_precedence() {
    let (row, styles) = row();
    let colors = TermColors::default();
    let selection = RowSelection { lo: 16, hi: 47 };
    let hint = RowHint {
        lo: 32,
        hi: 79,
        tag: HintTag::Match,
    };
    for keep_selection_color in [false, true] {
        let palette = Palette {
            keep_selection_color,
            ..Palette::default()
        };
        let glyphs = render(&row, &styles, &palette, &colors, Some(selection), &[hint]);
        for glyph in glyphs {
            let column = glyph.grid_pos[0];
            let expected = if (selection.lo..=selection.hi).contains(&column) {
                cell_fg_selected(
                    row[Column(column as usize)],
                    styles.at(column as usize),
                    &palette,
                    &colors,
                )
            } else if (hint.lo..=hint.hi).contains(&column) {
                cell_fg_hinted(HintTag::Match, &palette)
            } else {
                cell_fg(
                    row[Column(column as usize)],
                    styles.at(column as usize),
                    &palette,
                    &colors,
                )
            };
            assert_eq!(glyph.color, expected);
        }
    }
}
