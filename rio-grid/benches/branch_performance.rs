#[path = "../../misc/benchmarks/support.rs"]
mod support;

use rio_backend::{
    ansi::CursorShape,
    config::colors::{term::TermColors, AnsiColor, ColorArray, Colors},
    crosswords::{
        pos::Column,
        style::{Style, StyleFlags},
        Crosswords, CrosswordsSize,
    },
    event::{TerminalDamage, VoidListener, WindowId},
    performer::handler::Processor,
    sugarloaf::{
        font::FontLibrary,
        grid::{cpu::CpuGridRenderer, GridRenderer, GridUniforms},
    },
};
use rio_grid::preedit::{PreeditCursor, PreeditLine};
use rio_grid::{ExtrasMap, GridGlyphRasterizer, GridPalette, PreeditRow, RowSelection};
use std::hint::black_box;

// Deliberately small palette: this suite isolates grid preparation and shaping.
// Full application runs exercise the frontend's theme and color resolution.
struct Palette(Colors);
impl GridPalette for Palette {
    fn named_colors(&self) -> &Colors {
        &self.0
    }
    fn compute_color(
        &self,
        color: &AnsiColor,
        _: StyleFlags,
        _: &TermColors,
    ) -> ColorArray {
        match color {
            AnsiColor::Spec(rgb) => rgb.to_arr(),
            AnsiColor::Indexed(index) => [f32::from(*index) / 255.0, 0.6, 0.8, 1.0],
            _ => self.0.foreground,
        }
    }
    fn compute_bg_color(&self, style: &Style, colors: &TermColors) -> ColorArray {
        let mut color = self.compute_color(&style.bg, style.flags, colors);
        color[3] = 0.0;
        color
    }
    fn color(&self, _: usize, _: &TermColors) -> ColorArray {
        self.0.foreground
    }
    fn use_drawable_chars(&self) -> bool {
        true
    }
    fn opacity_cells(&self) -> bool {
        false
    }
    fn cell_bg_alpha(&self) -> u8 {
        0
    }
    fn ignore_selection_fg_color(&self) -> bool {
        false
    }
}

fn main() {
    let fonts = FontLibrary::default();
    let palette = Palette(Colors::default());
    let term_colors = TermColors::default();
    for kind in ["plain", "styled", "unicode", "combining", "preedit"] {
        let columns = 160;
        let mut term = Crosswords::new(
            CrosswordsSize::new(columns, 3),
            CursorShape::Block,
            VoidListener,
            WindowId::from(0),
            0,
            1000,
        );
        let mut processor = Processor::default();
        let mut fixture = String::new();
        let text = match kind {
            "unicode" => "日本語 한글 λ α Ж unicode ",
            "combining" => "e\u{301} a\u{308} n\u{303} combining ",
            _ => "let value = compute(input) + 42; // render benchmark ",
        };
        for i in 0..8 {
            if kind == "styled" {
                fixture.push_str(if i % 2 == 0 {
                    "\x1b[1;32m"
                } else {
                    "\x1b[3;36m"
                });
            }
            fixture.push_str(text);
        }
        processor.advance(&mut term, fixture.as_bytes());
        let mut rows = Vec::new();
        let mut styles = Vec::new();
        let mut extras = ExtrasMap::default();
        term.snapshot_visible(&TerminalDamage::Full, &mut rows, &mut styles, &mut extras);
        let preedit =
            PreeditLine::new("にほんご input", PreeditCursor::Byte(9), 0, 30, columns)
                .unwrap();
        let preedit = (kind == "preedit").then_some(PreeditRow {
            line: &preedit,
            block_bg: [100, 130, 190, 255],
        });
        let mut rasterizer = GridGlyphRasterizer::new();
        let mut grid = GridRenderer::Cpu(CpuGridRenderer::new(columns as u32, 3));
        let mut foreground = Vec::new();
        let mut background = Vec::new();
        for mode in ["cached", "changing", "rasterize", "selected"] {
            let name = format!("row/{kind}/{mode}");
            if !support::selected(&name) {
                continue;
            }
            let mut sequence = 0u64;
            support::measure(&name, columns, || {
                if mode == "changing" {
                    sequence += 1;
                    for (x, c) in format!("{sequence:08}").chars().take(8).enumerate() {
                        rows[0][Column(x)].set_c(c);
                    }
                }
                if mode == "rasterize" {
                    grid.clear_atlas();
                }
                let selection =
                    (mode == "selected").then_some(RowSelection { lo: 5, hi: 35 });
                rio_grid::build_row_fg(
                    &rows[0],
                    columns,
                    0,
                    &styles[0],
                    &extras,
                    &palette,
                    &term_colors,
                    &mut rasterizer,
                    &mut grid,
                    16.0,
                    9.0,
                    20.0,
                    selection,
                    &[],
                    preedit.as_ref(),
                    &fonts,
                    0,
                    None,
                    &mut foreground,
                );
                black_box(&foreground);
                foreground.len() as u64
            });
            assert!(!foreground.is_empty(), "row benchmark must emit glyphs");
        }
        for mode in ["plain", "selected"] {
            let name = format!("background/{kind}/{mode}");
            support::measure(&name, columns, || {
                rio_grid::build_row_bg(
                    &rows[0],
                    columns,
                    &styles[0],
                    &palette,
                    &term_colors,
                    (mode == "selected").then_some(RowSelection { lo: 5, hi: 35 }),
                    &[],
                    preedit.as_ref(),
                    &mut background,
                );
                black_box(&background);
                background.len() as u64
            });
        }
        // Canonical replay: changing-row sample counts must not change the image check.
        for (x, c) in "00000000".chars().enumerate() {
            rows[0][Column(x)].set_c(c);
        }
        rio_grid::build_row_fg(
            &rows[0],
            columns,
            0,
            &styles[0],
            &extras,
            &palette,
            &term_colors,
            &mut rasterizer,
            &mut grid,
            16.0,
            9.0,
            20.0,
            None,
            &[],
            preedit.as_ref(),
            &fonts,
            0,
            None,
            &mut foreground,
        );
        grid.write_row(0, &background, &foreground);
        let mut uniforms: GridUniforms = unsafe { std::mem::zeroed() };
        uniforms.cell_size = [9.0, 20.0];
        uniforms.grid_size = [columns as u32, 3];
        uniforms.cursor_pos = [u32::MAX; 2];
        let mut pixels = vec![0u32; columns * 9 * 20 * 3];
        if let GridRenderer::Cpu(cpu) = &grid {
            cpu.render_text(&mut pixels, (columns * 9) as u32, 60, &uniforms);
        }
        assert!(pixels.iter().any(|pixel| *pixel != 0));
        support::check(
            &format!("row/{kind}"),
            support::hash(pixels.iter().flat_map(|pixel| pixel.to_le_bytes())),
        );
    }
}
