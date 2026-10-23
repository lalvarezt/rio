//! Renderer workloads independent of a window system. Run with
//! `cargo bench -p sugarloaf --bench rendering`; RIO_BENCH_FILTER selects a case.
#[path = "../../misc/benchmarks/support.rs"]
mod support;

#[path = "../../misc/benchmarks/cpu_state.rs"]
mod state;

use std::hint::black_box;
use sugarloaf::{
    font::FontLibrary,
    grid::{
        atlas::RasterizedGlyph, cpu::CpuGridRenderer, CellBg, CellText, GlyphKey,
        GridUniforms,
    },
    text::{DrawOpts, Text},
};

fn main() {
    backgrounds();
    glyphs();
    text();
    state::hashes();
    state::atlases();
}

fn uniforms() -> GridUniforms {
    let mut uniforms = bytemuck::Zeroable::zeroed();
    let u: &mut GridUniforms = &mut uniforms;
    u.cell_size = [9.6, 20.0];
    u.grid_size = [160, 48];
    u.cursor_pos = [u32::MAX; 2];
    u.panel_rect = [0.0, 0.0, 1536.0, 960.0];
    uniforms
}

fn backgrounds() {
    let mut grid = CpuGridRenderer::new(160, 48);
    let mut pixels = vec![0x0012_3456; 1536 * 960];
    for kind in [
        "transparent",
        "sparse",
        "opaque",
        "alternating",
        "translucent",
        "rounded",
    ] {
        let name = format!("cpu/background/{kind}");
        if !support::selected(&name) {
            continue;
        }
        let mut u = uniforms();
        if kind == "rounded" {
            u.panel_radius = 24.0;
        }
        let row: Vec<_> = (0..160)
            .map(|col| CellBg {
                rgba: match kind {
                    "transparent" => [0; 4],
                    "sparse" if col % 16 != 0 => [0; 4],
                    "alternating" if col % 2 == 0 => [61, 33, 90, 255],
                    "translucent" => [31, 53, 79, 128],
                    _ => [21, 38, 56, 255],
                },
            })
            .collect();
        for row_index in 0..48 {
            grid.write_row(row_index, &row, &[]);
        }
        support::measure(&name, 160 * 48, || {
            grid.render_bg(black_box(&mut pixels), 1536, 960, black_box(&u));
            black_box(&pixels);
            0
        });
        pixels.fill(0x0012_3456);
        grid.render_bg(&mut pixels, 1536, 960, &u);
        support::check(
            &name,
            support::hash(pixels.iter().flat_map(|p| p.to_le_bytes())),
        );
    }
}

fn glyphs() {
    let mut grid = CpuGridRenderer::new(160, 48);
    // Synthetic antialiased glyph and premultiplied color tile isolate blitting
    // from font discovery and rasterization. UI workloads below use real fonts.
    let mask: Vec<_> = (0..8 * 16)
        .map(|i| if i % 8 < 2 { 0 } else { (i * 37) as u8 })
        .collect();
    let color: Vec<_> = mask
        .iter()
        .flat_map(|&a| [a / 2, a / 3, a / 4, a])
        .collect();
    let key = GlyphKey {
        font_id: 0,
        glyph_id: 1,
        size_bucket: 64,
    };
    let raster = RasterizedGlyph {
        width: 8,
        height: 16,
        bearing_x: 0,
        bearing_y: 16,
        bytes: &mask,
    };
    let mono = grid.insert_glyph(key, raster).unwrap();
    let rgba = grid
        .insert_glyph_color(
            key,
            RasterizedGlyph {
                bytes: &color,
                ..raster
            },
        )
        .unwrap();
    let mut pixels = vec![0x0012_3456; 1536 * 960];
    let u = uniforms();
    for kind in ["mask", "color", "mixed"] {
        let name = format!("cpu/glyphs/{kind}");
        if !support::selected(&name) {
            continue;
        }
        for row in 0..48 {
            let cells: Vec<_> = (0..160)
                .map(|col| {
                    let is_color = kind == "color" || (kind == "mixed" && col % 8 == 0);
                    let slot = if is_color { rgba } else { mono };
                    CellText {
                        glyph_pos: [slot.x as u32, slot.y as u32],
                        glyph_size: [8, 16],
                        bearings: [0, 16],
                        grid_pos: [col, row as u16],
                        color: [173, 193, 219, 255],
                        atlas: u8::from(is_color),
                        ..Default::default()
                    }
                })
                .collect();
            grid.write_row(row, &[CellBg::TRANSPARENT; 160], &cells);
        }
        support::measure(&name, 160 * 48, || {
            grid.render_text(black_box(&mut pixels), 1536, 960, black_box(&u));
            black_box(&pixels);
            0
        });
        pixels.fill(0x0012_3456);
        grid.render_text(&mut pixels, 1536, 960, &u);
        support::check(
            &name,
            support::hash(pixels.iter().flat_map(|p| p.to_le_bytes())),
        );
    }
}

fn text() {
    let fonts = FontLibrary::default();
    let mut text = Text::new(&fonts);
    text.init_cpu();
    let opts = DrawOpts::default();
    for (kind, label) in [
        ("ascii", "let value = compute(input) + 42; // rendering"),
        ("unicode", "日本語 한글 e\u{301} 👩\u{200d}💻 terminal"),
    ] {
        let name = format!("text/draw/{kind}");
        support::measure(&name, 1, || {
            text.clear();
            let width = text.draw(12.0, 10.0, black_box(label), &opts);
            black_box(text.instances());
            width.to_bits() as u64
        });
        text.clear();
        text.draw(12.0, 10.0, label, &opts);
        let mut pixels = vec![0x0012_3456; 1024 * 64];
        support::measure(&format!("text/raster/{kind}"), 1, || {
            text.render_cpu(black_box(&mut pixels), 1024, 64);
            black_box(&pixels);
            0
        });
        pixels.fill(0x0012_3456);
        text.render_cpu(&mut pixels, 1024, 64);
        support::check(
            &name,
            support::hash(pixels.iter().flat_map(|p| p.to_le_bytes())),
        );
    }
}
