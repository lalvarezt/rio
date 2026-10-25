//! Composed VT snapshot → row preparation → CPU grid storage probe.
//! Excludes raster painting, GPU submission, window events, and presentation.

use super::*;
use rio_backend::crosswords::{
    grid::{row::Row, Scroll},
    snapshot::{DeferredStyles, RowStyles},
    square::Square,
};
use rio_grid::prepared::{PreparedRows, RowInput};

const COLS: usize = 160;
const ROWS: usize = 48;

struct Frame {
    terminal: Crosswords<VoidListener>,
    processor: Processor,
    rows: Vec<Row<Square>>,
    styles: Vec<RowStyles>,
    extras: ExtrasMap,
    deferred: DeferredStyles,
    prepared: PreparedRows,
    rasterizer: GridGlyphRasterizer,
    grid: GridRenderer,
    background: Vec<rio_backend::sugarloaf::grid::CellBg>,
    foreground: Vec<rio_backend::sugarloaf::grid::CellText>,
    rewrite: [Vec<u8>; 2],
    output: Vec<u8>,
    output_digits: usize,
    sequence: usize,
    hits: usize,
    emitted: usize,
    fully_modified: usize,
}

fn line(kind: &str, row: usize, alternate: bool) -> String {
    let mut line = format!("{row:08}");
    if kind == "unicode" {
        line.push_str(
            &if alternate {
                "日本語 μ "
            } else {
                "日本語 λ "
            }
            .repeat(16),
        );
        line.push_str("        ");
    } else {
        line.extend(
            if alternate {
                "let other = compute(input) + 43; "
            } else {
                "let value = compute(input) + 42; "
            }
            .chars()
            .cycle()
            .take(COLS - 8),
        );
    }
    if kind == "styled" {
        format!(
            "\x1b[{}m{line}\x1b[0m",
            if row.is_multiple_of(2) { 32 } else { 36 }
        )
    } else {
        line
    }
}

impl Frame {
    fn new(kind: &str) -> Self {
        let mut terminal = Crosswords::new(
            CrosswordsSize::new(COLS, ROWS),
            CursorShape::Hidden,
            VoidListener,
            WindowId::from(0),
            0,
            256,
        );
        let mut processor = Processor::default();
        for row in 0..192 {
            processor.advance(&mut terminal, line(kind, row, false).as_bytes());
            processor.advance(&mut terminal, b"\r\n");
        }
        assert!(terminal.history_size() >= 64);
        let rewrite = [false, true].map(|alternate| {
            (0..ROWS)
                .map(|y| format!("\x1b[{};1H{}", y + 1, line(kind, y, alternate)))
                .collect::<String>()
                .into_bytes()
        });
        let output = format!("{}\r\n", line(kind, 0, false));
        let output_digits = output.find("00000000").unwrap();
        Self {
            terminal,
            processor,
            rows: Vec::new(),
            styles: Vec::new(),
            extras: ExtrasMap::default(),
            deferred: DeferredStyles::default(),
            prepared: PreparedRows::default(),
            rasterizer: GridGlyphRasterizer::new(),
            grid: GridRenderer::Cpu(CpuGridRenderer::new(COLS as u32, ROWS as u32)),
            background: Vec::new(),
            foreground: Vec::new(),
            rewrite,
            output: output.into_bytes(),
            output_digits,
            sequence: 0,
            hits: 0,
            emitted: 0,
            fully_modified: 0,
        }
    }

    fn capture(&mut self, mode: &str) -> (bool, bool) {
        self.sequence += 1;
        let full = match mode {
            "output" => {
                let mut number = self.sequence;
                for offset in (0..8).rev() {
                    self.output[self.output_digits + offset] = b'0' + (number % 10) as u8;
                    number /= 10;
                }
                self.processor.advance(&mut self.terminal, &self.output);
                true
            }
            "scroll" => {
                self.terminal.scroll_display(Scroll::Delta(
                    if (self.sequence - 1) % 128 < 64 {
                        1
                    } else {
                        -1
                    },
                ));
                true
            }
            "rewrite" => {
                self.processor
                    .advance(&mut self.terminal, &self.rewrite[self.sequence % 2]);
                true
            }
            "one_row" => {
                let mut bytes = *b"\x1b[24;1H00000000";
                bytes[7] = b'0' + (self.sequence % 2) as u8;
                self.processor.advance(&mut self.terminal, &bytes);
                false
            }
            "idle" => false,
            _ => unreachable!(),
        };
        let damage = if full {
            TerminalDamage::Full
        } else if mode == "idle" {
            TerminalDamage::Noop
        } else {
            TerminalDamage::Partial
        };
        let modified = self
            .terminal
            .snapshot_visible_deferred(
                &damage,
                &mut self.rows,
                &mut self.styles,
                &mut self.extras,
                &mut self.deferred,
            )
            .resolve();
        self.fully_modified += modified as usize;
        (full, modified)
    }

    fn emit(
        &mut self,
        full: bool,
        modified: bool,
        cached: bool,
        fonts: &FontLibrary,
        palette: &Palette,
        colors: &TermColors,
    ) {
        if cached {
            self.prepared
                .sync_atlas_generation(self.grid.atlas_generation());
        }
        let force = full || self.grid.needs_full_rebuild();
        if self.grid.needs_full_rebuild() {
            self.prepared.clear();
        }
        let reuse = cached && force && (self.prepared.is_empty() || !modified);
        if reuse {
            self.prepared.begin(COLS, [16.0, 9.0, 20.0], colors);
        }
        self.grid.mark_full_rebuild_done();
        for (y, row) in self.rows.iter_mut().enumerate() {
            if !force && !row.dirty {
                continue;
            }
            let input = reuse.then(|| {
                RowInput::new(&row.inner, &self.styles[y], &self.extras, None, &[], None)
            });
            if let Some(saved) = input.as_ref().and_then(|input| self.prepared.get(input))
            {
                let (background, foreground) = saved.retained();
                self.grid
                    .write_retained_row(y as u32, background, foreground);
                self.hits += 1;
                row.dirty = false;
                continue;
            } else {
                rio_grid::build_row_bg(
                    row,
                    COLS,
                    &self.styles[y],
                    palette,
                    colors,
                    None,
                    &[],
                    None,
                    &mut self.background,
                );
                rio_grid::build_row_fg(
                    row,
                    COLS,
                    y as u16,
                    &self.styles[y],
                    &self.extras,
                    palette,
                    colors,
                    &mut self.rasterizer,
                    &mut self.grid,
                    16.0,
                    9.0,
                    20.0,
                    None,
                    &[],
                    None,
                    fonts,
                    0,
                    None,
                    &mut self.foreground,
                );
                self.emitted += 1;
                if let Some(input) = &input {
                    self.prepared.put(input, &self.background, &self.foreground);
                }
            }
            self.grid
                .write_row(y as u32, &self.background, &self.foreground);
            row.dirty = false;
        }
        assert!(
            !self.grid.needs_full_rebuild(),
            "fixture must stay within the atlas"
        );
        if reuse {
            self.prepared.finish(ROWS);
        }
    }

    fn pixels(&self) -> Vec<u32> {
        let mut uniforms: GridUniforms = unsafe { std::mem::zeroed() };
        uniforms.cell_size = [9.0, 20.0];
        uniforms.grid_size = [COLS as u32, ROWS as u32];
        uniforms.cursor_pos = [u32::MAX; 2];
        let mut pixels = vec![0; COLS * 9 * ROWS * 20];
        if let GridRenderer::Cpu(grid) = &self.grid {
            grid.render_bg(
                &mut pixels,
                (COLS * 9) as u32,
                (ROWS * 20) as u32,
                &uniforms,
            );
            grid.render_text(
                &mut pixels,
                (COLS * 9) as u32,
                (ROWS * 20) as u32,
                &uniforms,
            );
        }
        pixels
    }
}

pub fn measure(fonts: &FontLibrary, palette: &Palette, colors: &TermColors) {
    for kind in ["plain", "styled", "unicode"] {
        for mode in ["idle", "one_row", "scroll", "output", "rewrite"] {
            if !(["snapshot", "direct", "prepared"]
                .iter()
                .any(|path| support::selected(&format!("pipeline/{kind}/{mode}/{path}"))))
            {
                continue;
            }
            // Differential check uses identical histories, not the variable timing iteration count.
            let mut direct = Frame::new(kind);
            let mut cached = Frame::new(kind);
            for _ in 0..66 {
                let (full, modified) = direct.capture(mode);
                direct.emit(full, modified, false, fonts, palette, colors);
                let (full, modified) = cached.capture(mode);
                cached.emit(full, modified, true, fonts, palette, colors);
            }
            let pixels = direct.pixels();
            assert!(pixels.iter().any(|pixel| *pixel != 0));
            assert_eq!(
                pixels,
                cached.pixels(),
                "{kind}/{mode}: prepared and direct frames differ"
            );
            if mode == "scroll" {
                assert!(
                    cached.hits > ROWS,
                    "scroll must exercise prepared row reuse"
                );
            }
            support::check(
                &format!("pipeline/{kind}/{mode}"),
                support::hash(pixels.iter().flat_map(|pixel| pixel.to_le_bytes())),
            );
            for path in ["snapshot", "direct", "prepared"] {
                let name = format!("pipeline/{kind}/{mode}/{path}");
                if !support::selected(&name) {
                    continue;
                }
                let mut frame = Frame::new(kind);
                support::measure(&name, COLS * ROWS, || {
                    let (full, modified) = frame.capture(mode);
                    if path != "snapshot" {
                        frame.emit(
                            full,
                            modified,
                            path == "prepared",
                            fonts,
                            palette,
                            colors,
                        );
                    }
                    black_box((&frame.rows, &frame.grid));
                    frame.rows.len() as u64
                });
                println!("RIO_PIPELINE {{\"case\":\"{name}\",\"frames\":{},\"prepared_hits\":{},\"emitted_rows\":{},\"fully_modified_frames\":{}}}", frame.sequence, frame.hits, frame.emitted, frame.fully_modified);
            }
        }
    }
}
