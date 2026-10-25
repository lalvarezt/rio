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
// shelf-allocated grayscale (R8) or color (RGBA8 premul) pages with
// the same `AtlasAllocator` the GPU paths use, so the atlas slot
// coordinates round-trip across backends.
//
// Position math in `render` mirrors `grid.metal`'s vertex shaders so
// glyph placement matches the GPU paths pixel-for-pixel:
//   - cell origin = (col * cell_w + grid_padding.left,
//                    row * cell_h + grid_padding.top)
//   - glyph origin = cell_origin + (bearing_x, cell_h - bearing_y)

use rustc_hash::FxHashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use super::atlas::{AtlasSlot, GlyphKey, RasterizedGlyph};
use super::cell::{CellBg, CellText, GridUniforms};
use crate::renderer::image_cache::atlas::AtlasAllocator;

/// Standard page side. Pages are lazy: 1 MiB grayscale or 4 MiB color.
/// Large glyphs use a larger page, charged against the same texel budget.
const ATLAS_SIZE: u16 = 1024;
const ATLAS_MAX_SIZE: u16 = 4096;

/// Slot 0 = block-cursor cells, slot `rows + 1` = non-block-cursor
/// cells. Matches the Metal layout in `metal::init_fg_rows`.
const CURSOR_ROW_SLOTS: usize = 2;
static NEXT_GRID_ID: AtomicU64 = AtomicU64::new(1);

/// CPU-side glyph atlas. R8 for grayscale masks, RGBA8 premultiplied
/// for color emoji. Pages and slots are append-only until a full clear.
/// The pixel budget is 16 MiB grayscale or 64 MiB color. Large pages
/// consume that budget sooner, and shelf fragmentation can force a clear
/// before every texel is occupied.
pub struct CpuGridAtlas {
    pages: Vec<CpuAtlasPage>,
    bytes_per_pixel: u8,
    slots: FxHashMap<GlyphKey, AtlasSlot>,
}

struct CpuAtlasPage {
    pixels: Vec<u8>,
    side: u16,
    allocator: AtlasAllocator,
    clear_padding: bool,
}

impl CpuGridAtlas {
    fn new(bytes_per_pixel: u8) -> Self {
        Self {
            pages: Vec::new(),
            bytes_per_pixel,
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

        if !self.accepts(glyph) {
            return None;
        }
        let mut allocation = None;
        for (index, page) in self.pages.iter_mut().enumerate() {
            if let Some((x, y)) = page.allocator.allocate(glyph.width, glyph.height) {
                allocation = Some((index, x, y));
                break;
            }
        }
        let (page_index, x, y) = match allocation {
            Some(allocation) => allocation,
            None => {
                let side = ATLAS_SIZE
                    .max((glyph.width.max(glyph.height) + 1).next_power_of_two());
                if !self.append_page(side) {
                    return None;
                }
                let index = self.pages.len() - 1;
                let (x, y) = self.pages[index]
                    .allocator
                    .allocate(glyph.width, glyph.height)?;
                (index, x, y)
            }
        };
        let slot = AtlasSlot {
            x,
            y,
            w: glyph.width,
            h: glyph.height,
            bearing_x: glyph.bearing_x,
            bearing_y: glyph.bearing_y,
            page: page_index as u8,
        };
        self.write_pixels(
            page_index,
            x as usize,
            y as usize,
            glyph.width as usize,
            glyph.height as usize,
            glyph.bytes,
        );
        Some(slot)
    }

    fn accepts(&self, glyph: RasterizedGlyph<'_>) -> bool {
        glyph.width < ATLAS_MAX_SIZE
            && glyph.height < ATLAS_MAX_SIZE
            && glyph.bytes.len()
                >= glyph.width as usize
                    * glyph.height as usize
                    * self.bytes_per_pixel as usize
    }

    fn write_pixels(
        &mut self,
        page: usize,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        src: &[u8],
    ) {
        let bpp = self.bytes_per_pixel as usize;
        let page = &mut self.pages[page];
        let stride = page.side as usize * bpp;
        let row_bytes = w * bpp;
        for row in 0..h {
            let src_off = row * row_bytes;
            let dst_off = (y + row) * stride + x * bpp;
            page.pixels[dst_off..dst_off + row_bytes]
                .copy_from_slice(&src[src_off..src_off + row_bytes]);
        }
        if page.clear_padding {
            // Reset pages retain old pixels. Only the new allocation and its
            // one-pixel border can be sampled, so leave unused texels alone.
            for row in 0..h {
                let right = (y + row) * stride + (x + w) * bpp;
                page.pixels[right..right + bpp].fill(0);
            }
            let bottom = (y + h) * stride + x * bpp;
            page.pixels[bottom..bottom + row_bytes + bpp].fill(0);
        }
    }

    /// Append an empty standard page without moving existing pixels or slots.
    /// Insertion also allocates pages lazily. The budget is 4096² texels per kind.
    pub fn grow(&mut self) -> bool {
        self.append_page(ATLAS_SIZE)
    }

    fn append_page(&mut self, side: u16) -> bool {
        let area = (side as usize).pow(2);
        let used: usize = self
            .pages
            .iter()
            .map(|page| (page.side as usize).pow(2))
            .sum();
        if used + area > (ATLAS_MAX_SIZE as usize).pow(2) {
            return false;
        }
        self.pages.push(CpuAtlasPage {
            pixels: vec![0; area * self.bytes_per_pixel as usize],
            side,
            allocator: AtlasAllocator::new(side, side),
            clear_padding: false,
        });
        true
    }

    pub fn clear(&mut self) {
        // Dropping pages also removes stale padding before slot reuse.
        self.pages.clear();
        self.slots.clear();
    }

    fn reset_for(&mut self, glyph: RasterizedGlyph<'_>) {
        // Capacity exhaustion does not need to fault a new allocation back in.
        // Reuse pages if one can hold this glyph after resetting its allocator.
        let required = glyph.width.max(glyph.height);
        if !self.pages.iter().any(|page| page.side > required) {
            self.clear();
            return;
        }
        self.slots.clear();
        for page in &mut self.pages {
            page.allocator.clear();
            page.clear_padding = true;
        }
    }

    #[inline]
    /// First-page pixels, or an empty slice before the first allocation.
    pub fn pixels(&self) -> &[u8] {
        self.page_pixels(0).unwrap_or(&[])
    }

    #[inline]
    /// First-page side, or the standard side before the first allocation.
    pub fn side(&self) -> u16 {
        self.page_side(0).unwrap_or(ATLAS_SIZE)
    }

    pub fn page_pixels(&self, page: u8) -> Option<&[u8]> {
        self.pages
            .get(page as usize)
            .map(|page| page.pixels.as_slice())
    }

    pub fn page_side(&self, page: u8) -> Option<u16> {
        self.pages.get(page as usize).map(|page| page.side)
    }
}

pub struct CpuGridRenderer {
    id: u64,
    atlas_generation: u64,
    paint_revision: u64,
    cols: u32,
    rows: u32,
    /// `cols * rows` flat. Indexed `row * cols + col`.
    bg_cells: Vec<CellBg>,
    /// Per-row fg storage with the same indexing scheme as the GPU
    /// backends — slot 0 holds the block cursor, 1..=rows hold content
    /// rows, slot `rows + 1` holds the non-block cursor decoration.
    fg_rows: Vec<Vec<CellText>>,
    retained_scratch: Vec<CellText>,
    atlas_grayscale: CpuGridAtlas,
    atlas_color: CpuGridAtlas,
    needs_full_rebuild: bool,
}

impl CpuGridRenderer {
    pub fn new(cols: u32, rows: u32) -> Self {
        Self {
            id: NEXT_GRID_ID.fetch_add(1, Ordering::Relaxed),
            atlas_generation: super::next_atlas_generation(),
            paint_revision: 0,
            cols,
            rows,
            bg_cells: vec![CellBg::TRANSPARENT; bg_capacity(cols, rows)],
            fg_rows: init_fg_rows(rows),
            retained_scratch: Vec::new(),
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
        self.paint_revision = self.paint_revision.wrapping_add(1);
        // Fresh buffers = zero contents; emission path must rewrite
        // every row on the next frame even if no damage came in.
        self.needs_full_rebuild = true;
    }

    pub fn write_retained_row(&mut self, row: u32, bg: &[CellBg], fg: &[CellText]) {
        let mut cells = std::mem::take(&mut self.retained_scratch);
        cells.clear();
        cells.extend(fg.iter().map(|cell| {
            let mut cell = *cell;
            cell.grid_pos[1] = row as u16;
            cell
        }));
        self.write_row(row, bg, &cells);
        self.retained_scratch = cells;
    }

    pub fn write_row(&mut self, row: u32, bg: &[CellBg], fg: &[CellText]) {
        let idx = (row as usize) + 1;
        let mut changed = false;
        if let Some(slot) = self.fg_rows.get_mut(idx) {
            changed |= replace_text_row(slot, fg);
        }

        if row < self.rows {
            let cols = self.cols as usize;
            let row_start = (row as usize) * cols;
            let row_len = cols.min(bg.len());
            let dst = &mut self.bg_cells[row_start..row_start + cols];
            if bytemuck::cast_slice::<_, u8>(&dst[..row_len])
                != bytemuck::cast_slice(&bg[..row_len])
                || dst[row_len..]
                    .iter()
                    .any(|cell| *cell != CellBg::TRANSPARENT)
            {
                dst[..row_len].copy_from_slice(&bg[..row_len]);
                dst[row_len..].fill(CellBg::TRANSPARENT);
                changed = true;
            }
        }
        if changed {
            self.paint_revision = self.paint_revision.wrapping_add(1);
        }
    }

    pub fn clear_row(&mut self, row: u32) {
        let idx = (row as usize) + 1;
        let mut changed = false;
        if let Some(slot) = self.fg_rows.get_mut(idx) {
            changed |= !slot.is_empty();
            slot.clear();
        }
        if row < self.rows {
            let cols = self.cols as usize;
            let row_start = (row as usize) * cols;
            let dst = &mut self.bg_cells[row_start..row_start + cols];
            if dst.iter().any(|cell| *cell != CellBg::TRANSPARENT) {
                dst.fill(CellBg::TRANSPARENT);
                changed = true;
            }
        }
        if changed {
            self.paint_revision = self.paint_revision.wrapping_add(1);
        }
    }

    pub fn set_cursor(&mut self, block: &[CellText], non_block: &[CellText]) {
        let mut changed = false;
        if let Some(slot) = self.fg_rows.first_mut() {
            changed |= replace_text_row(slot, block);
        }
        let last = self.fg_rows.len().saturating_sub(1);
        if last > 0 {
            if let Some(slot) = self.fg_rows.get_mut(last) {
                changed |= replace_text_row(slot, non_block);
            }
        }
        if changed {
            self.paint_revision = self.paint_revision.wrapping_add(1);
        }
    }

    #[inline]
    pub fn lookup_glyph(&self, key: GlyphKey) -> Option<AtlasSlot> {
        self.atlas_grayscale.lookup(key)
    }

    /// Drop every cached glyph and force a full rebuild. Called when
    /// the font library is swapped, since the new library reuses font ids.
    pub fn clear_atlas(&mut self) {
        self.atlas_generation = super::next_atlas_generation();
        self.atlas_grayscale.clear();
        self.atlas_color.clear();
        self.needs_full_rebuild = true;
        self.paint_revision = self.paint_revision.wrapping_add(1);
    }

    pub fn atlas_generation(&self) -> u64 {
        self.atlas_generation
    }

    pub fn insert_glyph(
        &mut self,
        key: GlyphKey,
        glyph: RasterizedGlyph<'_>,
    ) -> Option<AtlasSlot> {
        if self.atlas_grayscale.lookup(key).is_some() {
            self.atlas_generation = super::next_atlas_generation();
            self.needs_full_rebuild = true;
        }
        if !self.atlas_grayscale.accepts(glyph) {
            return None;
        }
        let mut cleared = false;
        loop {
            if let Some(slot) = self.atlas_grayscale.insert(key, glyph) {
                self.paint_revision = self.paint_revision.wrapping_add(1);
                return Some(slot);
            }
            if cleared {
                return None;
            }
            self.atlas_grayscale.reset_for(glyph);
            self.atlas_generation = super::next_atlas_generation();
            self.paint_revision = self.paint_revision.wrapping_add(1);
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
        if self.atlas_color.lookup(key).is_some() {
            self.atlas_generation = super::next_atlas_generation();
            self.needs_full_rebuild = true;
        }
        if !self.atlas_color.accepts(glyph) {
            return None;
        }
        let mut cleared = false;
        loop {
            if let Some(slot) = self.atlas_color.insert(key, glyph) {
                self.paint_revision = self.paint_revision.wrapping_add(1);
                return Some(slot);
            }
            if cleared {
                return None;
            }
            self.atlas_color.reset_for(glyph);
            self.atlas_generation = super::next_atlas_generation();
            self.paint_revision = self.paint_revision.wrapping_add(1);
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
        self.paint_revision = self.paint_revision.wrapping_add(1);
    }

    /// Hash the grid's identity and paint revision for the CPU frame gate.
    /// Exact comparisons at mutation sites preserve the key for identical
    /// writes. Identity distinguishes grids with equal mutation counts.
    pub fn hash_state<H: std::hash::Hasher>(&self, h: &mut H) {
        h.write_u64(self.id);
        h.write_u64(self.paint_revision);
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
                    let Some(page) = self.atlas_color.pages.get(glyph.page as usize)
                    else {
                        continue;
                    };
                    blit_color(
                        buf,
                        buf_w_i,
                        buf_h_i,
                        glyph_x,
                        glyph_y,
                        gw,
                        gh,
                        &page.pixels,
                        page.side as usize,
                        ax,
                        ay,
                    );
                } else {
                    let Some(page) = self.atlas_grayscale.pages.get(glyph.page as usize)
                    else {
                        continue;
                    };
                    blit_mask(
                        buf,
                        buf_w_i,
                        buf_h_i,
                        glyph_x,
                        glyph_y,
                        gw,
                        gh,
                        &page.pixels,
                        page.side as usize,
                        ax,
                        ay,
                        color,
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
fn replace_text_row(slot: &mut Vec<CellText>, cells: &[CellText]) -> bool {
    if bytemuck::cast_slice::<_, u8>(slot.as_slice()) == bytemuck::cast_slice(cells) {
        return false;
    }
    slot.clear();
    slot.extend_from_slice(cells);
    true
}

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
    fn atlas_pages_preserve_pixels_slots_and_zero_padding() {
        for bpp in [1, 4] {
            let mut atlas = CpuGridAtlas::new(bpp);
            let bytes: Vec<_> = (0..13 * 17 * bpp as usize)
                .map(|i| (i * 73) as u8)
                .collect();
            let key = GlyphKey {
                font_id: 0,
                glyph_id: 1,
                size_bucket: 64,
            };
            let slot = atlas
                .insert(
                    key,
                    RasterizedGlyph {
                        width: 13,
                        height: 17,
                        bearing_x: 0,
                        bearing_y: 17,
                        bytes: &bytes,
                    },
                )
                .unwrap();
            let original = atlas.pixels().as_ptr();
            for _ in 0..15 {
                let side = atlas.side() as usize;
                let pixels = atlas.pixels().to_vec();
                assert!(atlas.grow());
                assert_eq!(original, atlas.pixels().as_ptr());
                assert!(atlas.pages.last().unwrap().pixels.iter().all(|&b| b == 0));
                let found = atlas.lookup(key).unwrap();
                assert_eq!(
                    (
                        found.x,
                        found.y,
                        found.w,
                        found.h,
                        found.bearing_x,
                        found.bearing_y,
                        found.page
                    ),
                    (
                        slot.x,
                        slot.y,
                        slot.w,
                        slot.h,
                        slot.bearing_x,
                        slot.bearing_y,
                        slot.page
                    )
                );
                let new_stride = atlas.side() as usize * bpp as usize;
                let old_stride = side * bpp as usize;
                for row in 0..side {
                    assert_eq!(
                        &atlas.pixels()[row * new_stride..row * new_stride + old_stride],
                        &pixels[row * old_stride..(row + 1) * old_stride]
                    );
                    assert!(atlas.pixels()
                        [row * new_stride + old_stride..(row + 1) * new_stride]
                        .iter()
                        .all(|&b| b == 0));
                }
                assert!(atlas.pixels()[side * new_stride..].iter().all(|&b| b == 0));
            }
            assert!(!atlas.grow());
            assert_eq!(atlas.pages.len(), 16);
            atlas.clear();
            assert!(atlas.lookup(key).is_none());
            assert!(atlas.pages.is_empty());
            let fresh = atlas
                .insert(
                    key,
                    RasterizedGlyph {
                        width: 1,
                        height: 1,
                        bearing_x: 0,
                        bearing_y: 0,
                        bytes: &[255; 4],
                    },
                )
                .unwrap();
            assert_eq!(fresh.page, 0);
            assert!(atlas.pixels()[bpp as usize..].iter().all(|&b| b == 0));
        }
    }

    #[test]
    fn atlas_exhaustion_invalidates_generation_and_preserves_large_glyph_padding() {
        for color in [false, true] {
            let mut grid = CpuGridRenderer::new(1, 1);
            let bpp = if color { 4 } else { 1 };
            let bytes = vec![127; 4095 * 4095 * bpp];
            let glyph = RasterizedGlyph {
                width: 4095,
                height: 4095,
                bearing_x: 0,
                bearing_y: 0,
                bytes: &bytes,
            };
            let key = GlyphKey {
                font_id: 0,
                glyph_id: 1,
                size_bucket: 64,
            };
            let generation = grid.atlas_generation();
            let insert = |grid: &mut CpuGridRenderer, key| {
                if color {
                    grid.insert_glyph_color(key, glyph)
                } else {
                    grid.insert_glyph(key, glyph)
                }
            };
            assert!(insert(&mut grid, key).is_some());
            assert_eq!(generation, grid.atlas_generation());
            let original_pixels = if color {
                grid.atlas_color.pixels().as_ptr()
            } else {
                grid.atlas_grayscale.pixels().as_ptr()
            };
            grid.mark_full_rebuild_done();
            let next_key = GlyphKey { glyph_id: 2, ..key };
            assert!(insert(&mut grid, next_key).is_some());
            assert_ne!(generation, grid.atlas_generation());
            assert!(grid.needs_full_rebuild());
            let atlas = if color {
                &grid.atlas_color
            } else {
                &grid.atlas_grayscale
            };
            assert!(atlas.lookup(key).is_none());
            assert_eq!(atlas.pages.len(), 1);
            let pixels = atlas.pixels();
            assert_eq!(
                pixels.as_ptr(),
                original_pixels,
                "capacity reset reuses committed pixel storage"
            );
            for row in 0..4095 {
                let offset = row * 4096 * bpp;
                assert!(pixels[offset..offset + 4095 * bpp]
                    .iter()
                    .all(|&b| b == 127));
                assert!(pixels[offset + 4095 * bpp..offset + 4096 * bpp]
                    .iter()
                    .all(|&b| b == 0));
            }
            assert!(pixels[4095 * 4096 * bpp..].iter().all(|&b| b == 0));
        }
    }

    #[test]
    fn reused_pages_clear_new_padding_without_clearing_unused_pixels() {
        for bpp in [1, 4] {
            let mut atlas = CpuGridAtlas::new(bpp);
            let bytes = vec![127; 13 * 17 * bpp as usize];
            atlas
                .upload(RasterizedGlyph {
                    width: 13,
                    height: 17,
                    bearing_x: 0,
                    bearing_y: 0,
                    bytes: &bytes,
                })
                .unwrap();
            let glyph = RasterizedGlyph {
                width: 1,
                height: 1,
                bearing_x: 0,
                bearing_y: 0,
                bytes: &[31; 4],
            };
            atlas.reset_for(glyph);
            let slot = atlas.upload(glyph).unwrap();
            assert_eq!((slot.x, slot.y), (0, 0));
            let bpp = bpp as usize;
            let pixels = atlas.pixels();
            assert!(pixels[..bpp].iter().all(|&b| b == 31));
            assert!(pixels[bpp..2 * bpp].iter().all(|&b| b == 0));
            let next_row = ATLAS_SIZE as usize * bpp;
            assert!(pixels[next_row..next_row + 2 * bpp].iter().all(|&b| b == 0));
            assert!(pixels[2 * bpp..3 * bpp].iter().all(|&b| b == 127));
        }
    }

    #[test]
    fn invalid_glyphs_do_not_allocate_or_invalidate() {
        let mut grid = CpuGridRenderer::new(1, 1);
        grid.mark_full_rebuild_done();
        let generation = grid.atlas_generation();
        let key = GlyphKey {
            font_id: 0,
            glyph_id: 1,
            size_bucket: 64,
        };
        for (width, height) in [(4096, 1), (1, 4096), (2, 2)] {
            let glyph = RasterizedGlyph {
                width,
                height,
                bearing_x: 0,
                bearing_y: 0,
                bytes: &[],
            };
            assert!(grid.insert_glyph(key, glyph).is_none());
            assert!(grid.insert_glyph_color(key, glyph).is_none());
        }
        assert_eq!(generation, grid.atlas_generation());
        assert!(!grid.needs_full_rebuild());
        assert!(grid.atlas_grayscale.pages.is_empty());
        assert!(grid.atlas_color.pages.is_empty());
    }

    #[test]
    fn render_text_samples_the_selected_page_for_each_atlas_kind() {
        for color in [false, true] {
            let mut grid = CpuGridRenderer::new(1, 1);
            let generation = grid.atlas_generation();
            let bpp = if color { 4 } else { 1 };
            let full_bytes = vec![0; 1023 * 1023 * bpp];
            let full = RasterizedGlyph {
                width: 1023,
                height: 1023,
                bearing_x: 0,
                bearing_y: 1,
                bytes: &full_bytes,
            };
            let small = RasterizedGlyph {
                width: 1,
                height: 1,
                bytes: &[255; 4],
                ..full
            };
            let key = GlyphKey {
                font_id: 0,
                glyph_id: 1,
                size_bucket: 64,
            };
            let slot = if color {
                grid.insert_glyph_color(key, full).unwrap();
                grid.insert_glyph_color(GlyphKey { glyph_id: 2, ..key }, small)
                    .unwrap()
            } else {
                grid.insert_glyph(key, full).unwrap();
                grid.insert_glyph(GlyphKey { glyph_id: 2, ..key }, small)
                    .unwrap()
            };
            assert_eq!(slot.page, 1);
            assert_eq!(generation, grid.atlas_generation());
            let cell = CellText {
                glyph_pos: [slot.x as u32, slot.y as u32],
                glyph_size: [1, 1],
                bearings: [0, 1],
                color: [255; 4],
                atlas: if color {
                    CellText::ATLAS_COLOR
                } else {
                    CellText::ATLAS_GRAYSCALE
                },
                page: slot.page,
                ..Default::default()
            };
            grid.write_row(0, &[], &[cell]);
            let mut uniforms = GridUniforms::zeroed();
            uniforms.cell_size = [1.0, 1.0];
            uniforms.grid_size = [1, 1];
            let mut pixels = [0];
            grid.render_text(&mut pixels, 1, 1, &uniforms);
            assert_eq!(pixels[0] & 0x00ff_ffff, 0x00ff_ffff);
            grid.write_row(0, &[], &[CellText { page: 0, ..cell }]);
            pixels.fill(0);
            grid.render_text(&mut pixels, 1, 1, &uniforms);
            assert_eq!(pixels, [0]);
            grid.write_row(0, &[], &[CellText { page: 255, ..cell }]);
            grid.render_text(&mut pixels, 1, 1, &uniforms);
            assert_eq!(pixels, [0]);
            grid.resize(2, 1);
            assert_eq!(generation, grid.atlas_generation());
            grid.clear_atlas();
            assert_ne!(generation, grid.atlas_generation());
        }
    }

    #[test]
    fn frame_hash_tracks_mutations_and_ignores_identical_writes() {
        use std::hash::Hasher;
        let hash = |grid: &CpuGridRenderer| {
            let mut h = rustc_hash::FxHasher::default();
            grid.hash_state(&mut h);
            h.finish()
        };
        let mut grid = CpuGridRenderer::new(2, 2);
        let bg = [CellBg {
            rgba: [10, 20, 30, 255],
        }; 2];
        let mut fg = [CellText {
            color: [100, 120, 140, 255],
            ..Default::default()
        }];
        let empty = hash(&grid);
        grid.write_row(0, &bg, &fg);
        let first = hash(&grid);
        assert_ne!(empty, first);
        grid.write_row(0, &bg, &fg);
        assert_eq!(first, hash(&grid));
        fg[0].color[0] ^= 1;
        grid.write_row(0, &bg, &fg);
        assert_ne!(first, hash(&grid));
        let before_cursor = hash(&grid);
        grid.set_cursor(&fg, &fg);
        let cursor = hash(&grid);
        assert_ne!(before_cursor, cursor);
        grid.set_cursor(&fg, &fg);
        assert_eq!(cursor, hash(&grid));
        grid.clear_row(0);
        let cleared = hash(&grid);
        assert_ne!(cursor, cleared);
        grid.clear_row(0);
        assert_eq!(cleared, hash(&grid));
        grid.resize(3, 2);
        let resized = hash(&grid);
        assert_ne!(cleared, resized);
        grid.resize(3, 2);
        assert_eq!(resized, hash(&grid));
        let mut other = CpuGridRenderer::new(2, 2);
        other.write_row(0, &bg, &[]);
        assert_ne!(
            first,
            hash(&other),
            "different grids must not alias at the same mutation count"
        );
    }

    #[test]
    fn frame_hash_tracks_background_truncation_and_atlas_replacement() {
        use std::hash::Hasher;
        let hash = |grid: &CpuGridRenderer| {
            let mut h = rustc_hash::FxHasher::default();
            grid.hash_state(&mut h);
            h.finish()
        };
        let mut grid = CpuGridRenderer::new(2, 1);
        let bg = [CellBg {
            rgba: [30, 40, 50, 255],
        }; 2];
        let empty = hash(&grid);
        grid.write_row(0, &bg, &[]);
        let filled = hash(&grid);
        assert_ne!(empty, filled);
        grid.write_row(0, &bg[..1], &[]);
        let truncated = hash(&grid);
        assert_ne!(filled, truncated);
        assert_eq!(grid.bg_cells[1], CellBg::TRANSPARENT);
        grid.write_row(0, &bg[..1], &[]);
        assert_eq!(truncated, hash(&grid));
        for color in [false, true] {
            let key = GlyphKey {
                font_id: 0,
                glyph_id: 1,
                size_bucket: 64,
            };
            for value in [100, 200] {
                let bytes = [value; 4];
                let glyph = RasterizedGlyph {
                    width: 1,
                    height: 1,
                    bearing_x: 0,
                    bearing_y: 1,
                    bytes: &bytes,
                };
                let before = hash(&grid);
                if color {
                    grid.insert_glyph_color(key, glyph).unwrap();
                } else {
                    grid.insert_glyph(key, glyph).unwrap();
                }
                assert_ne!(before, hash(&grid), "atlas replacement affects painting");
            }
        }
        let before = hash(&grid);
        grid.clear_atlas();
        assert_ne!(before, hash(&grid));
        let before = hash(&grid);
        grid.request_full_rebuild();
        assert_ne!(before, hash(&grid));
        let before = hash(&grid);
        grid.mark_full_rebuild_done();
        assert_eq!(before, hash(&grid));
    }

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
