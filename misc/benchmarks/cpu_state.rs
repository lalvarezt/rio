//! Frame-gate hashing and synthetic cold atlas transitions, without fonts.
use super::support;
use std::{hash::Hasher, hint::black_box};
use sugarloaf::grid::{
    cpu::{CpuGridAtlas, CpuGridRenderer},
    CellBg, CellText, GlyphKey, RasterizedGlyph,
};

fn state_hash(grid: &CpuGridRenderer) -> u64 {
    let mut hash = rustc_hash::FxHasher::default();
    black_box(grid).hash_state(&mut hash);
    hash.finish()
}

pub fn hashes() {
    for (cols, rows) in [(80, 24), (160, 48), (320, 96)] {
        let mut grid = CpuGridRenderer::new(cols, rows);
        let bg = vec![CellBg::TRANSPARENT; cols as usize];
        let mut fg: Vec<_> = (0..cols)
            .map(|x| CellText {
                grid_pos: [x as u16, 0],
                color: [100, 120, 140, 255],
                ..Default::default()
            })
            .collect();
        for row in 0..rows {
            grid.write_row(row, &bg, &fg);
        }
        let before = state_hash(&grid);
        fg[0].color[0] ^= 1;
        grid.write_row(0, &bg, &fg);
        assert_ne!(
            before,
            state_hash(&grid),
            "foreground bytes must affect frame skipping"
        );
        for mode in [
            "unchanged",
            "one_row",
            "full",
            "cursor",
            "one_row_write",
            "full_write",
        ] {
            support::measure(
                &format!("cpu/state_hash/{cols}x{rows}/{mode}"),
                (cols * rows) as usize,
                || {
                    if mode != "unchanged" {
                        fg[0].color[0] ^= 1;
                        if mode == "cursor" {
                            grid.set_cursor(&fg[..1], &[]);
                        } else {
                            let changed_rows =
                                if mode.starts_with("full") { rows } else { 1 };
                            for row in 0..changed_rows {
                                grid.write_row(row, &bg, &fg);
                            }
                        }
                    }
                    if mode.ends_with("_write") {
                        black_box(&grid);
                        0
                    } else {
                        state_hash(&grid)
                    }
                },
            );
        }
    }
}

fn key(id: u32) -> GlyphKey {
    GlyphKey {
        font_id: 0,
        glyph_id: id,
        size_bucket: 64,
    }
}

pub fn atlases() {
    for (kind, bpp) in [("mask", 1), ("color", 4)] {
        let prefix = format!("cpu/atlas/{kind}");
        if !support::selected(&prefix)
            && !std::env::var("RIO_BENCH_FILTER")
                .unwrap_or_default()
                .starts_with(&prefix)
        {
            continue;
        }
        let new = || {
            if bpp == 1 {
                CpuGridAtlas::new_grayscale()
            } else {
                CpuGridAtlas::new_color()
            }
        };
        let bytes = vec![127; 12 * 24 * bpp];
        let glyph = RasterizedGlyph {
            width: 12,
            height: 24,
            bearing_x: 0,
            bearing_y: 24,
            bytes: &bytes,
        };
        let mut atlas = new();
        atlas.insert(key(0), glyph).unwrap();
        support::measure(&format!("{prefix}/lookup"), 1, || {
            black_box(atlas.lookup(black_box(key(0))).unwrap()).x as u64
        });
        support::measure_cold(&format!("{prefix}/insert_batch"), 128, new, |atlas| {
            for id in 0..128 {
                black_box(atlas.insert(key(id), glyph).unwrap());
            }
            atlas.side() as u64
        });
        for side in [1024u16, 2048] {
            let name = format!("{prefix}/grow_{side}");
            if !support::selected(&name) {
                continue;
            }
            let bytes = vec![127; (side as usize - 1).pow(2) * bpp];
            let full = RasterizedGlyph {
                width: side - 1,
                height: side - 1,
                bytes: &bytes,
                ..glyph
            };
            support::measure_cold(
                &name,
                side as usize * side as usize * bpp,
                || {
                    let mut atlas = new();
                    if side == 2048 {
                        assert!(atlas.grow());
                    }
                    atlas.insert(key(0), full).unwrap();
                    atlas
                },
                |atlas| {
                    assert!(atlas.grow());
                    assert!(atlas.lookup(key(0)).is_some());
                    black_box(atlas.pixels());
                    atlas.side() as u64
                },
            );
        }
        let name = format!("{prefix}/exhaustion");
        if support::selected(&name) {
            let bytes = vec![127; 4095 * 4095 * bpp];
            let full = RasterizedGlyph {
                width: 4095,
                height: 4095,
                bytes: &bytes,
                ..glyph
            };
            support::measure_cold(
                &name,
                bytes.len(),
                || {
                    let mut grid = CpuGridRenderer::new(1, 1);
                    if bpp == 1 {
                        grid.insert_glyph(key(0), full).unwrap();
                    } else {
                        grid.insert_glyph_color(key(0), full).unwrap();
                    }
                    grid.mark_full_rebuild_done();
                    grid
                },
                |grid| {
                    let slot = if bpp == 1 {
                        grid.insert_glyph(key(1), full)
                    } else {
                        grid.insert_glyph_color(key(1), full)
                    };
                    assert!(slot.is_some());
                    assert!(grid.needs_full_rebuild());
                    u64::from(grid.needs_full_rebuild())
                },
            );
        }
    }
}
