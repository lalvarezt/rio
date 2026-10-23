// Copyright (c) 2023-present, Raphael Amorim.
//
// CPU backend for the grid renderer.
//
// Mirrors the GPU backends (`metal`, `vulkan`, `webgpu`) in scope and
// data layout, but rasterises directly into the softbuffer u32 pixel
// buffer instead of recording draw calls. Storage matches the Metal
// renderer cell-for-cell: one flat `Vec<CellBg>` indexed by
// `row * cols + col`, plus per-row `Vec<CellText>` slots (slot 0 =
// block-cursor cells, 1..=rows = content rows, last = non-block
// cursor) so the row-rebuild path in `frontends/rioterm/src/grid_emit`
// stays backend-agnostic.
//
// The atlases live in RAM. `CpuGridAtlas` packs glyph bitmaps into a
// shelf-allocated grayscale (R8) or color (RGBA8 premul) buffer with
// the same `AtlasAllocator` the GPU paths use, so the atlas slot
// coordinates round-trip across backends.
//
// Position math in `render` mirrors `grid.metal`'s vertex shaders so
// glyph placement matches the GPU paths pixel-for-pixel:
//   - cell origin = (col * cell_w + grid_padding.left,
//                    row * cell_h + grid_padding.top)
//   - glyph origin = cell_origin + (bearing_x, cell_h - bearing_y)

use rustc_hash::FxHashMap;

use super::atlas::{AtlasSlot, GlyphKey, RasterizedGlyph};
use super::cell::{CellBg, CellText, GridUniforms};
use crate::renderer::image_cache::atlas::AtlasAllocator;

/// Initial atlas side. 1024² bytes_per_pixel = 1 MiB grayscale,
/// 4 MiB color. Smaller than the Metal default (2048²) since CPU
/// builds are usually memory-constrained machines.
const ATLAS_SIZE: u16 = 1024;
const ATLAS_MAX_SIZE: u16 = 4096;

/// Slot 0 = block-cursor cells, slot `rows + 1` = non-block-cursor
/// cells. Matches the Metal layout in `metal::init_fg_rows`.
const CURSOR_ROW_SLOTS: usize = 2;

/// CPU-side glyph atlas. R8 for grayscale masks, RGBA8 premultiplied
/// for color emoji. Uses the same shelf allocator as the GPU atlases
/// so `AtlasSlot` coords round-trip.
pub struct CpuGridAtlas {
    pixels: Vec<u8>,
    side: u16,
    bytes_per_pixel: u8,
    allocator: AtlasAllocator,
    slots: FxHashMap<GlyphKey, AtlasSlot>,
}

impl CpuGridAtlas {
    fn new(bytes_per_pixel: u8) -> Self {
        let side = ATLAS_SIZE;
        let pixels =
            vec![0u8; (side as usize) * (side as usize) * (bytes_per_pixel as usize)];
        Self {
            pixels,
            side,
            bytes_per_pixel,
            allocator: AtlasAllocator::new(side, side),
            slots: FxHashMap::default(),
        }
    }

    pub fn new_grayscale() -> Self {
        Self::new(1)
    }

    pub fn new_color() -> Self {
        Self::new(4)
    }

    #[inline]
    pub fn lookup(&self, key: GlyphKey) -> Option<AtlasSlot> {
        self.slots.get(&key).copied()
    }

    pub fn insert(
        &mut self,
        key: GlyphKey,
        glyph: RasterizedGlyph<'_>,
    ) -> Option<AtlasSlot> {
        let slot = self.upload(glyph)?;
        self.slots.insert(key, slot);
        Some(slot)
    }

    /// Upload pixels without adding a font-glyph cache entry.
    pub(crate) fn upload(&mut self, glyph: RasterizedGlyph<'_>) -> Option<AtlasSlot> {
        if glyph.width == 0 || glyph.height == 0 {
            // Zero-sized glyphs (e.g. spaces) still need a cache entry
            // so the rasterizer doesn't keep producing them, but they
            // occupy no atlas space.
            let slot = AtlasSlot {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
                bearing_x: glyph.bearing_x,
                bearing_y: glyph.bearing_y,
                page: 0,
            };
            return Some(slot);
        }

        let (x, y) = self.allocator.allocate(glyph.width, glyph.height)?;
        let slot = AtlasSlot {
            x,
            y,
            w: glyph.width,
            h: glyph.height,
            bearing_x: glyph.bearing_x,
            bearing_y: glyph.bearing_y,
            page: 0,
        };
        self.write_pixels(
            x as usize,
            y as usize,
            glyph.width as usize,
            glyph.height as usize,
            glyph.bytes,
        );
        Some(slot)
    }

    fn write_pixels(&mut self, x: usize, y: usize, w: usize, h: usize, src: &[u8]) {
        let bpp = self.bytes_per_pixel as usize;
        let stride = self.side as usize * bpp;
        let row_bytes = w * bpp;
        for row in 0..h {
            let src_off = row * row_bytes;
            let dst_off = (y + row) * stride + x * bpp;
            self.pixels[dst_off..dst_off + row_bytes]
                .copy_from_slice(&src[src_off..src_off + row_bytes]);
        }
    }

    /// Double the atlas side, copying old pixels into the top-left of
    /// the new buffer. Existing `AtlasSlot`s stay valid because their
    /// `(x, y)` fall inside the unchanged old region. Returns `false`
    /// when already at `ATLAS_MAX_SIZE`.
    pub fn grow(&mut self) -> bool {
        let (old_w, old_h) = self.allocator.dimensions();
        if old_w >= ATLAS_MAX_SIZE {
            return false;
        }
        let new_side = old_w.saturating_mul(2).min(ATLAS_MAX_SIZE);
        if new_side <= old_w {
            return false;
        }
        let bpp = self.bytes_per_pixel as usize;
        let mut new_pixels = vec![0u8; (new_side as usize) * (new_side as usize) * bpp];

        let old_stride = old_w as usize * bpp;
        let new_stride = new_side as usize * bpp;
        for row in 0..old_h as usize {
            let src_off = row * old_stride;
            let dst_off = row * new_stride;
            new_pixels[dst_off..dst_off + old_stride]
                .copy_from_slice(&self.pixels[src_off..src_off + old_stride]);
        }
        self.pixels = new_pixels;
        self.side = new_side;
        self.allocator.grow_to(new_side, new_side);
        true
    }

    pub fn clear(&mut self) {
        self.allocator.clear();
        self.slots.clear();
    }

    #[inline]
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    #[inline]
    pub fn side(&self) -> u16 {
        self.side
    }
}

pub struct CpuGridRenderer {
    cols: u32,
    rows: u32,
    /// `cols * rows` flat. Indexed `row * cols + col`.
    bg_cells: Vec<CellBg>,
    /// Per-row fg storage with the same indexing scheme as the GPU
    /// backends — slot 0 holds the block cursor, 1..=rows hold content
    /// rows, slot `rows + 1` holds the non-block cursor decoration.
    fg_rows: Vec<Vec<CellText>>,
    atlas_grayscale: CpuGridAtlas,
    atlas_color: CpuGridAtlas,
    needs_full_rebuild: bool,
}

impl CpuGridRenderer {
    pub fn new(cols: u32, rows: u32) -> Self {
        Self {
            cols,
            rows,
            bg_cells: vec![CellBg::TRANSPARENT; bg_capacity(cols, rows)],
            fg_rows: init_fg_rows(rows),
            atlas_grayscale: CpuGridAtlas::new_grayscale(),
            atlas_color: CpuGridAtlas::new_color(),
            needs_full_rebuild: true,
        }
    }

    pub fn resize(&mut self, cols: u32, rows: u32) {
        if cols == self.cols && rows == self.rows {
            return;
        }
        self.cols = cols;
        self.rows = rows;
        self.bg_cells = vec![CellBg::TRANSPARENT; bg_capacity(cols, rows)];
        self.fg_rows = init_fg_rows(rows);
        // Fresh buffers = zero contents; emission path must rewrite
        // every row on the next frame even if no damage came in.
        self.needs_full_rebuild = true;
    }

    pub fn write_row(&mut self, row: u32, bg: &[CellBg], fg: &[CellText]) {
        let idx = (row as usize) + 1;
        if let Some(slot) = self.fg_rows.get_mut(idx) {
            slot.clear();
            slot.extend_from_slice(fg);
        }

        if row >= self.rows {
            return;
        }
        let cols = self.cols as usize;
        let row_start = (row as usize) * cols;
        let row_len = cols.min(bg.len());
        let dst = &mut self.bg_cells[row_start..row_start + cols];
        dst[..row_len].copy_from_slice(&bg[..row_len]);
        for slot in &mut dst[row_len..] {
            *slot = CellBg::TRANSPARENT;
        }
    }

    pub fn clear_row(&mut self, row: u32) {
        let idx = (row as usize) + 1;
        if let Some(slot) = self.fg_rows.get_mut(idx) {
            slot.clear();
        }
        if row >= self.rows {
            return;
        }
        let cols = self.cols as usize;
        let row_start = (row as usize) * cols;
        for slot in &mut self.bg_cells[row_start..row_start + cols] {
            *slot = CellBg::TRANSPARENT;
        }
    }

    pub fn set_cursor(&mut self, block: &[CellText], non_block: &[CellText]) {
        if let Some(slot) = self.fg_rows.first_mut() {
            slot.clear();
            slot.extend_from_slice(block);
        }
        let last = self.fg_rows.len().saturating_sub(1);
        if last > 0 {
            if let Some(slot) = self.fg_rows.get_mut(last) {
                slot.clear();
                slot.extend_from_slice(non_block);
            }
        }
    }

    #[inline]
    pub fn lookup_glyph(&self, key: GlyphKey) -> Option<AtlasSlot> {
        self.atlas_grayscale.lookup(key)
    }

    /// Drop every cached glyph and force a full rebuild. Called when
    /// the font library is swapped, since the new library reuses font ids.
    pub fn clear_atlas(&mut self) {
        self.atlas_grayscale.clear();
        self.atlas_color.clear();
        self.needs_full_rebuild = true;
    }

    pub fn insert_glyph(
        &mut self,
        key: GlyphKey,
        glyph: RasterizedGlyph<'_>,
    ) -> Option<AtlasSlot> {
        let mut cleared = false;
        loop {
            if let Some(slot) = self.atlas_grayscale.insert(key, glyph) {
                return Some(slot);
            }
            if self.atlas_grayscale.grow() {
                continue;
            }
            if cleared {
                return None;
            }
            self.atlas_grayscale.clear();
            self.needs_full_rebuild = true;
            cleared = true;
        }
    }

    #[inline]
    pub fn lookup_glyph_color(&self, key: GlyphKey) -> Option<AtlasSlot> {
        self.atlas_color.lookup(key)
    }

    pub fn insert_glyph_color(
        &mut self,
        key: GlyphKey,
        glyph: RasterizedGlyph<'_>,
    ) -> Option<AtlasSlot> {
        let mut cleared = false;
        loop {
            if let Some(slot) = self.atlas_color.insert(key, glyph) {
                return Some(slot);
            }
            if self.atlas_color.grow() {
                continue;
            }
            if cleared {
                return None;
            }
            self.atlas_color.clear();
            self.needs_full_rebuild = true;
            cleared = true;
        }
    }

    #[inline]
    pub fn needs_full_rebuild(&self) -> bool {
        self.needs_full_rebuild
    }

    #[inline]
    pub fn mark_full_rebuild_done(&mut self) {
        self.needs_full_rebuild = false;
    }

    /// Force the next frame to rebuild every row, even when nothing dirtied
    /// the cells. Used when state outside the grid (e.g. the color theme)
    /// changed and every cell must re-resolve.
    #[inline]
    pub fn request_full_rebuild(&mut self) {
        self.needs_full_rebuild = true;
    }

    /// Hash all state that affects what `render` will paint. The CPU
    /// rasterizer's frame-skip path uses this to short-circuit when
    /// the previous frame's output is still valid.
    pub fn hash_state<H: std::hash::Hasher>(&self, h: &mut H) {
        // bg_cells is a flat `Vec<CellBg>` — hash as raw bytes.
        h.write(bytemuck::cast_slice(self.bg_cells.as_slice()));
        // Per-row CellText bytes. Length-prefix so two adjacent rows
        // can't be confused with one wider row.
        for row in &self.fg_rows {
            h.write_usize(row.len());
            h.write(bytemuck::cast_slice(row.as_slice()));
        }
    }

    /// Paint the grid (bg cells + cursor + fg glyphs) into the
    /// caller's `0x00RRGGBB` u32 buffer. Mirrors the bg + text passes
    /// of `grid.metal`'s shaders, in the same draw order so glyphs
    /// composite correctly over their cell backgrounds.
    /// Paint the cell-bg pass into `buf`. Pair with `render_text`,
    /// with any `kitty_below_text` images composited in between.
    pub fn render_bg(
        &self,
        buf: &mut [u32],
        buf_w: u32,
        buf_h: u32,
        uniforms: &GridUniforms,
    ) {
        let cell_w = uniforms.cell_size[0];
        let cell_h = uniforms.cell_size[1];
        if cell_w <= 0.0 || cell_h <= 0.0 {
            return;
        }
        let cols = uniforms.grid_size[0];
        let rows = uniforms.grid_size[1];
        if cols == 0 || rows == 0 {
            return;
        }
        let pad_top = uniforms.grid_padding[0];
        let pad_left = uniforms.grid_padding[3];

        let buf_w_i = buf_w as i32;
        let buf_h_i = buf_h as i32;
        let cursor_x = uniforms.cursor_pos[0];
        let cursor_y = uniforms.cursor_pos[1];
        let cursor_bg_active = uniforms.cursor_bg_color[3] > 0.0;
        let cursor_bg = normalize_color(uniforms.cursor_bg_color);
        let panel = if uniforms.panel_rect[2] > 0.0 && uniforms.panel_rect[3] > 0.0 {
            uniforms.panel_rect
        } else {
            [0.0, 0.0, buf_w as f32, buf_h as f32]
        };

        let buf_cols = self.cols as usize;
        let row_count = (rows as usize).min(self.rows as usize);
        let col_count = (cols as usize).min(self.cols as usize);
        for row in 0..row_count {
            let row_off = row * buf_cols;
            let y0 = (pad_top + (row as f32) * cell_h).round() as i32;
            let y1 = (pad_top + ((row + 1) as f32) * cell_h).round() as i32;
            let mut col = 0;
            while col < col_count {
                let rgba = self.bg_cells[row_off + col].rgba;
                let is_cursor =
                    cursor_bg_active && cursor_x == col as u32 && cursor_y == row as u32;
                if rgba[3] == 0 && !is_cursor {
                    col += 1;
                    continue;
                }
                // Adjacent equal backgrounds share a rectangle. Keep the cursor
                // separate because it replaces the cell color, including alpha.
                let mut end_col = col + 1;
                if !is_cursor {
                    while end_col < col_count
                        && self.bg_cells[row_off + end_col].rgba == rgba
                        && !(cursor_bg_active
                            && cursor_x == end_col as u32
                            && cursor_y == row as u32)
                    {
                        end_col += 1;
                    }
                }
                let x0 = (pad_left + (col as f32) * cell_w).round() as i32;
                let x1 = (pad_left + (end_col as f32) * cell_w).round() as i32;
                let extend = uniforms.padding_extend;
                let bounds = [
                    if col == 0 && extend & GridUniforms::PADDING_EXTEND_LEFT != 0 {
                        panel[0].round() as i32
                    } else {
                        x0
                    },
                    if row == 0 && extend & GridUniforms::PADDING_EXTEND_UP != 0 {
                        panel[1].round() as i32
                    } else {
                        y0
                    },
                    if end_col == col_count
                        && extend & GridUniforms::PADDING_EXTEND_RIGHT != 0
                    {
                        (panel[0] + panel[2]).round() as i32
                    } else {
                        x1
                    },
                    if row + 1 == row_count
                        && extend & GridUniforms::PADDING_EXTEND_DOWN != 0
                    {
                        (panel[1] + panel[3]).round() as i32
                    } else {
                        y1
                    },
                ];
                let mut paint = |bounds, color| {
                    fill_panel_rect(
                        buf,
                        buf_w_i,
                        buf_h_i,
                        bounds,
                        color,
                        panel,
                        uniforms.panel_radius,
                    );
                };
                if is_cursor {
                    // Cursor backgrounds belong to the cell, never its padding.
                    // Paint only the surrounding strips with the original color
                    // so translucent cursors replace, rather than blend over, it.
                    for strip in [
                        [bounds[0], bounds[1], bounds[2], y0],
                        [bounds[0], y1, bounds[2], bounds[3]],
                        [bounds[0], y0, x0, y1],
                        [x1, y0, bounds[2], y1],
                    ] {
                        paint(strip, rgba);
                    }
                    paint([x0, y0, x1, y1], cursor_bg);
                } else {
                    paint(bounds, rgba);
                }
                col = end_col;
            }
        }
    }

    /// Paint the cell-text pass into `buf`. Walks `fg_rows` and blits
    /// each glyph's atlas slot at the cell origin computed from
    /// `grid_pos + bearings`.
    pub fn render_text(
        &self,
        buf: &mut [u32],
        buf_w: u32,
        buf_h: u32,
        uniforms: &GridUniforms,
    ) {
        let cell_w = uniforms.cell_size[0];
        let cell_h = uniforms.cell_size[1];
        if cell_w <= 0.0 || cell_h <= 0.0 {
            return;
        }
        let cols = uniforms.grid_size[0];
        let rows = uniforms.grid_size[1];
        if cols == 0 || rows == 0 {
            return;
        }
        let pad_top = uniforms.grid_padding[0];
        let pad_left = uniforms.grid_padding[3];

        let buf_w_i = buf_w as i32;
        let buf_h_i = buf_h as i32;
        let cursor_x = uniforms.cursor_pos[0];
        let cursor_y = uniforms.cursor_pos[1];
        let cursor_fg_active = uniforms.cursor_color[3] > 0.0;
        let cursor_fg = normalize_color(uniforms.cursor_color);

        let mask = self.atlas_grayscale.pixels();
        let mask_side = self.atlas_grayscale.side as usize;
        let color_atlas = self.atlas_color.pixels();
        let color_side = self.atlas_color.side as usize;

        for fg in &self.fg_rows {
            for glyph in fg {
                let gw = glyph.glyph_size[0] as i32;
                let gh = glyph.glyph_size[1] as i32;
                if gw <= 0 || gh <= 0 {
                    continue;
                }

                let cell_pos_x = (glyph.grid_pos[0] as f32) * cell_w + pad_left;
                let cell_pos_y = (glyph.grid_pos[1] as f32) * cell_h + pad_top;
                let glyph_x = (cell_pos_x + glyph.bearings[0] as f32) as i32;
                let glyph_y = (cell_pos_y + cell_h - glyph.bearings[1] as f32) as i32;

                let mut color = glyph.color;
                if cursor_fg_active
                    && (glyph.bools & CellText::BOOL_IS_CURSOR_GLYPH) == 0
                    && cursor_x == glyph.grid_pos[0] as u32
                    && cursor_y == glyph.grid_pos[1] as u32
                {
                    color = cursor_fg;
                }

                let ax = glyph.glyph_pos[0] as usize;
                let ay = glyph.glyph_pos[1] as usize;

                if glyph.atlas == CellText::ATLAS_COLOR {
                    blit_color(
                        buf,
                        buf_w_i,
                        buf_h_i,
                        glyph_x,
                        glyph_y,
                        gw,
                        gh,
                        color_atlas,
                        color_side,
                        ax,
                        ay,
                    );
                } else {
                    blit_mask(
                        buf, buf_w_i, buf_h_i, glyph_x, glyph_y, gw, gh, mask, mask_side,
                        ax, ay, color,
                    );
                }
            }
        }
    }
}

#[inline]
fn bg_capacity(cols: u32, rows: u32) -> usize {
    (cols as usize) * (rows as usize)
}

#[inline]
fn init_fg_rows(rows: u32) -> Vec<Vec<CellText>> {
    (0..(rows as usize + CURSOR_ROW_SLOTS))
        .map(|_| Vec::new())
        .collect()
}

#[inline]
fn normalize_color(c: [f32; 4]) -> [u8; 4] {
    [
        (c[0].clamp(0.0, 1.0) * 255.0) as u8,
        (c[1].clamp(0.0, 1.0) * 255.0) as u8,
        (c[2].clamp(0.0, 1.0) * 255.0) as u8,
        (c[3].clamp(0.0, 1.0) * 255.0) as u8,
    ]
}

#[inline]
fn premul(c: [u8; 4]) -> [u8; 4] {
    let a = c[3] as u32;
    if a == 255 {
        return c;
    }
    if a == 0 {
        return [0, 0, 0, 0];
    }
    [
        ((c[0] as u32 * a + 127) / 255) as u8,
        ((c[1] as u32 * a + 127) / 255) as u8,
        ((c[2] as u32 * a + 127) / 255) as u8,
        c[3],
    ]
}

use crate::premul::{
    blend_premul_over as blend_over, blit_color, blit_mask, pack_opaque,
};

fn fill_panel_rect(
    buf: &mut [u32],
    buf_w: i32,
    buf_h: i32,
    bounds: [i32; 4],
    rgba: [u8; 4],
    panel: [f32; 4],
    radius: f32,
) {
    if rgba[3] == 0 {
        return;
    }
    let x0 = bounds[0].max(panel[0].round() as i32);
    let y0 = bounds[1].max(panel[1].round() as i32);
    let x1 = bounds[2].min((panel[0] + panel[2]).round() as i32);
    let y1 = bounds[3].min((panel[1] + panel[3]).round() as i32);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let radius = radius.max(0.0).min(panel[2] * 0.5).min(panel[3] * 0.5);
    // Most cells lie entirely inside the straight part of the pane.
    if radius == 0.0
        || (y0 as f32 >= panel[1] + radius && y1 as f32 <= panel[1] + panel[3] - radius)
        || (x0 as f32 >= panel[0] + radius && x1 as f32 <= panel[0] + panel[2] - radius)
    {
        fill_rect(buf, buf_w, buf_h, [x0, y0, x1, y1], rgba);
        return;
    }
    for y in y0.max(0)..y1.min(buf_h) {
        let center_y = y as f32 + 0.5;
        let dy = (panel[1] + radius - center_y)
            .max(center_y - (panel[1] + panel[3] - radius))
            .max(0.0);
        let inset = radius - (radius * radius - dy * dy).max(0.0).sqrt();
        let left = (panel[0] + inset - 0.5).ceil() as i32;
        let right = (panel[0] + panel[2] - inset - 0.5).ceil() as i32;
        fill_rect(
            buf,
            buf_w,
            buf_h,
            [x0.max(left), y, x1.min(right), y + 1],
            rgba,
        );
    }
}

fn fill_rect(buf: &mut [u32], buf_w: i32, buf_h: i32, bounds: [i32; 4], rgba: [u8; 4]) {
    let x0 = bounds[0].max(0);
    let y0 = bounds[1].max(0);
    let x1 = bounds[2].min(buf_w);
    let y1 = bounds[3].min(buf_h);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let pre = premul(rgba);
    let stride = buf_w as usize;
    if pre[3] == 255 {
        let opaque = pack_opaque(pre[0], pre[1], pre[2]);
        for y in y0..y1 {
            let row_start = (y as usize) * stride + (x0 as usize);
            let row_end = (y as usize) * stride + (x1 as usize);
            buf[row_start..row_end].fill(opaque);
        }
    } else {
        for y in y0..y1 {
            let row_off = (y as usize) * stride;
            for x in x0..x1 {
                let idx = row_off + (x as usize);
                buf[idx] = blend_over(pre, buf[idx]);
            }
        }
    }
}

#[cfg(test)]
mod panel_tests {
    use super::*;
    use bytemuck::Zeroable;

    #[test]
    fn color_blit_matches_pixel_reference_when_clipped() {
        for side in [7, 16] {
            let atlas: Vec<_> = (0..side * side)
                .flat_map(|i| {
                    let a = (i * 73) as u8;
                    // Include non-premultiplied and transparent colored pixels
                    // to preserve the scalar helper's saturation and identity.
                    if i % 7 == 0 {
                        [255, 127, 60, 0]
                    } else {
                        [a, a / 2, 255, a]
                    }
                })
                .collect();
            for width in [0, 1, 3, 4, 5, 8, 9] {
                for x in [-3, 0, 13] {
                    for y in [-2, 0, 9] {
                        for (ax, ay) in
                            [(0, 0), (1, 2), (side - 2, side - 1), (side + 1, 0)]
                        {
                            let mut actual: Vec<_> = (0..17 * 13)
                                .map(|i| {
                                    [0, 0xff12_3456, 0x9912_3456, 0x017f_ff80][i % 4]
                                })
                                .collect();
                            let mut expected = actual.clone();
                            for dy in 0..13i32 {
                                for dx in 0..17i32 {
                                    let sx = dx - x;
                                    let sy = dy - y;
                                    if !(0..width).contains(&sx) || !(0..7).contains(&sy)
                                    {
                                        continue;
                                    }
                                    let sx = sx as usize + ax;
                                    let sy = sy as usize + ay;
                                    if sx >= side || sy >= side {
                                        continue;
                                    }
                                    let off = (sy * side + sx) * 4;
                                    let idx = dy as usize * 17 + dx as usize;
                                    expected[idx] = blend_over(
                                        atlas[off..off + 4].try_into().unwrap(),
                                        expected[idx],
                                    );
                                }
                            }
                            blit_color(
                                &mut actual,
                                17,
                                13,
                                x,
                                y,
                                width,
                                7,
                                &atlas,
                                side,
                                ax,
                                ay,
                            );
                            assert_eq!(
                                actual, expected,
                                "side={side} width={width} x={x} y={y} atlas=({ax},{ay})"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn mask_blit_matches_pixel_reference_when_clipped() {
        for side in [7, 16] {
            let atlas: Vec<_> = (0..side * side).map(|i| (i * 73 % 256) as u8).collect();
            for width in [0, 1, 3, 4, 5, 8, 9] {
                for x in [-3, 0, 13] {
                    for y in [-2, 0, 9] {
                        for (ax, ay) in
                            [(0, 0), (1, 2), (side - 2, side - 1), (side + 1, 0)]
                        {
                            for alpha in [0, 1, 127, 254, 255] {
                                for rgb in [[0, 0, 0], [255, 255, 255], [37, 180, 91]] {
                                    let color = [rgb[0], rgb[1], rgb[2], alpha];
                                    let mut actual: Vec<_> = (0..17 * 13)
                                        .map(|i| {
                                            [0, 0xff12_3456, 0x9912_3456, 0x017f_ff80]
                                                [i % 4]
                                        })
                                        .collect();
                                    let mut expected = actual.clone();
                                    for dy in 0..13i32 {
                                        for dx in 0..17i32 {
                                            let sx = dx - x;
                                            let sy = dy - y;
                                            if !(0..width).contains(&sx)
                                                || !(0..7).contains(&sy)
                                            {
                                                continue;
                                            }
                                            let sx = sx as usize + ax;
                                            let sy = sy as usize + ay;
                                            if sx >= side || sy >= side {
                                                continue;
                                            }
                                            let a = (atlas[sy * side + sx] as u32
                                                * alpha as u32
                                                + 127)
                                                / 255;
                                            let src = [
                                                ((rgb[0] as u32 * a + 127) / 255) as u8,
                                                ((rgb[1] as u32 * a + 127) / 255) as u8,
                                                ((rgb[2] as u32 * a + 127) / 255) as u8,
                                                a as u8,
                                            ];
                                            let idx = dy as usize * 17 + dx as usize;
                                            expected[idx] =
                                                blend_over(src, expected[idx]);
                                        }
                                    }
                                    blit_mask(
                                        &mut actual,
                                        17,
                                        13,
                                        x,
                                        y,
                                        width,
                                        7,
                                        &atlas,
                                        side,
                                        ax,
                                        ay,
                                        color,
                                    );
                                    assert_eq!(actual, expected, "side={side} width={width} x={x} y={y} atlas=({ax},{ay}) color={color:?}");
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn background_runs_match_cells_painted_individually() {
        let mut grid = CpuGridRenderer::new(7, 3);
        let mut isolated = CpuGridRenderer::new(7, 3);
        for cell_size in [[9.6, 13.25], [0.4, 0.75], [12.0, 16.0]] {
            for origin in [-3.5, 8.25] {
                for alpha in [0, 127, 255] {
                    for radius in [0.0, 17.0] {
                        for extend in [0, 15] {
                            for cursor in [[0, 0], [3, 1], [6, 2], [u32::MAX; 2]] {
                                let u = GridUniforms {
                                    grid_padding: [origin, 0.0, 0.0, origin],
                                    cell_size,
                                    grid_size: [7, 3],
                                    panel_rect: [0.0, 0.0, 88.0, 64.0],
                                    panel_radius: radius,
                                    padding_extend: extend,
                                    cursor_pos: cursor,
                                    cursor_bg_color: [0.1, 0.8, 0.3, 0.5],
                                    ..GridUniforms::zeroed()
                                };
                                let mut actual = vec![0x9912_3456; 88 * 64];
                                let mut expected = actual.clone();
                                for row in 0..3 {
                                    let cells: Vec<_> = (0..7)
                                        .map(|col| CellBg {
                                            rgba: [
                                                if col < 5 { 80 } else { 30 },
                                                60,
                                                20,
                                                alpha,
                                            ],
                                        })
                                        .collect();
                                    grid.write_row(row, &cells, &[]);
                                    for col in 0..7 {
                                        let mut one = [CellBg::TRANSPARENT; 7];
                                        one[col] = cells[col];
                                        isolated.write_row(row, &one, &[]);
                                        let mut cell_uniforms = u;
                                        if cursor != [col as u32, row] {
                                            cell_uniforms.cursor_bg_color[3] = 0.0;
                                        }
                                        isolated.render_bg(
                                            &mut expected,
                                            88,
                                            64,
                                            &cell_uniforms,
                                        );
                                    }
                                    isolated.clear_row(row);
                                }
                                grid.render_bg(&mut actual, 88, 64, &u);
                                assert_eq!(actual, expected, "cell_size={cell_size:?}, origin={origin}, alpha={alpha}, radius={radius}, extend={extend}, cursor={cursor:?}");
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn terminal_background_stays_out_of_all_padding_and_margins() {
        let mut grid = CpuGridRenderer::new(2, 2);
        let red = CellBg {
            rgba: [200, 40, 30, 255],
        };
        grid.write_row(0, &[red; 2], &[]);
        grid.write_row(1, &[red; 2], &[]);
        for radius in [0.0, 8.0] {
            let uniforms = GridUniforms {
                grid_padding: [18.0, 0.0, 0.0, 21.0],
                cell_size: [13.0, 11.0],
                grid_size: [2; 2],
                padding_extend: GridUniforms::PADDING_EXTEND_NONE,
                panel_rect: [8.0, 8.0, 48.0, 48.0],
                panel_radius: radius,
                ..GridUniforms::zeroed()
            };
            let clear = 0x000a141e;
            let mut pixels = vec![clear; 64 * 64];
            grid.render_bg(&mut pixels, 64, 64, &uniforms);
            for y in 0..64 {
                for x in 0..64 {
                    let expected = if (21..47).contains(&x) && (18..40).contains(&y) {
                        0x00c8281e
                    } else {
                        clear
                    };
                    assert_eq!(
                        pixels[y * 64 + x] & 0x00ffffff,
                        expected,
                        "radius={radius} x={x} y={y}"
                    );
                }
            }
        }
    }

    #[test]
    fn backgrounds_respect_panel_padding_corners_and_cursor_bounds() {
        let mut grid = CpuGridRenderer::new(2, 2);
        let red = CellBg {
            rgba: [200, 40, 30, 255],
        };
        grid.write_row(0, &[red; 2], &[]);
        grid.write_row(1, &[red; 2], &[]);
        let uniforms = GridUniforms {
            grid_padding: [16.0, 0.0, 0.0, 16.0],
            cell_size: [16.0; 2],
            grid_size: [2; 2],
            padding_extend: GridUniforms::PADDING_EXTEND_LEFT
                | GridUniforms::PADDING_EXTEND_RIGHT,
            panel_rect: [8.0, 8.0, 48.0, 48.0],
            panel_radius: 20.0,
            cursor_pos: [0, 0],
            cursor_bg_color: [0.0, 1.0, 0.0, 1.0],
            ..GridUniforms::zeroed()
        };
        let clear = 0x000a141e;
        let mut pixels = vec![clear; 64 * 64];
        grid.render_bg(&mut pixels, 64, 64, &uniforms);
        let pixel = |x: usize, y: usize| pixels[y * 64 + x] & 0x00ffffff;
        assert_eq!(pixel(8, 32), 0x00c8281e, "edge color fills side padding");
        assert_eq!(pixel(8, 16), clear, "rounded corner stays outside the pane");
        assert_eq!(pixel(16, 20), 0x0000ff00, "cursor fills its cell");
        assert_eq!(
            pixel(10, 20),
            0x00c8281e,
            "cursor does not extend into padding"
        );
        assert_eq!(pixel(32, 12), clear, "top padding retains the backdrop");
        assert_eq!(pixel(32, 50), clear, "bottom padding retains the backdrop");
        assert_eq!(pixel(4, 32), clear, "neighboring panes stay untouched");

        let mut uniforms = uniforms;
        uniforms.cursor_bg_color[3] = 0.5;
        pixels.fill(clear);
        grid.render_bg(&mut pixels, 64, 64, &uniforms);
        let cursor_on_color = pixels[20 * 64 + 16];
        grid.write_row(0, &[CellBg::TRANSPARENT; 2], &[]);
        pixels.fill(clear);
        grid.render_bg(&mut pixels, 64, 64, &uniforms);
        assert_eq!(
            pixels[20 * 64 + 16],
            cursor_on_color,
            "translucent cursor replaces the original background"
        );
    }
}
