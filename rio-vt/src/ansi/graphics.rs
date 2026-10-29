// graphics.rs was retired from a alacritty PR made by ayosec
// Alacritty is licensed under Apache 2.0 license.
// https://github.com/alacritty/alacritty/pull/4763/files

use crate::ansi::kitty_animation::{Animation, Command as AnimationCommand};
use crate::ansi::sixel;
use crate::config::colors::ColorRgb;
use crate::crosswords::grid::Dimensions;
use parking_lot::Mutex;
use rio_graphics::{GraphicData, GraphicId};
use rustc_hash::FxHashMap;
use std::mem;
use std::sync::Arc;
use tracing::debug;

#[derive(Debug, Clone)]
pub struct UpdateQueues {
    /// Atlas graphics (sixel/iTerm2) read from the PTY.
    pub pending: Vec<GraphicData>,

    /// Image textures (kitty) keyed by image_id.
    pub pending_images: Vec<(u32, GraphicData)>,

    /// Image keys removed from the grid or evicted
    /// (`rio_graphics::graphics::kitty_image_key` / `atlas_image_key`).
    pub remove_queue: Vec<u64>,
}

/// Kitty graphics Unicode placeholder character
pub const KITTY_PLACEHOLDER: char = '\u{10EEEE}';

/// Stored image data for Kitty graphics protocol
#[derive(Debug, Clone, PartialEq)]
pub struct StoredImage {
    /// Stable across animation ticks; changes only on image retransmission.
    pub image_generation: u64,
    pub data: GraphicData,
    pub transmission_time: crate::time::Instant,
}

/// Overlay placement for a kitty graphics image.
/// Stored separately from grid cells — rendered as an overlay layer.
///
/// Kitty images use the protocol's `image_id: u32` directly, not `GraphicId`.
/// `GraphicId` is for atlas-based graphics (sixel/iTerm2) which share a
/// sequential ID space. Kitty image_ids come from the protocol and would
/// collide with atlas IDs. They also use a completely separate rendering
/// pipeline (per-image GPU textures, not atlas), so there's no reason to
/// wrap them in `GraphicId`.
#[derive(Debug, Clone, PartialEq)]
pub struct KittyPlacement {
    /// The anchor was pruned. Keep its storage reference until an explicit
    /// deletion or the next successful placement reaps it.
    pub expired: bool,
    /// Parent identity and signed cell offsets for a relative placement.
    pub relative: Option<RelativePlacement>,
    /// Kitty protocol image ID (i= parameter).
    pub image_id: u32,
    /// Storage identity. Internal and client-specified IDs have separate tags.
    pub placement_id: u64,
    /// Source rectangle within the image, exactly as requested
    /// (`x=`/`y=`/`w=`/`h=`; zero width/height means "to the image
    /// edge"). Stored raw and resolved against the image's dimensions
    /// at read time.
    pub source_x: u32,
    pub source_y: u32,
    pub source_width: u32,
    pub source_height: u32,
    /// Grid column of the top-left corner.
    pub dest_col: usize,
    /// Absolute row (scrollback-aware) of the top-left corner.
    pub dest_row: i64,
    /// Display size in cells.
    pub columns: u32,
    pub rows: u32,
    /// The `c=`/`r=` span the client requested (0 = derived). Kept
    /// separate from `columns`/`rows` so a cell size change can tell
    /// cell-sized placements (which track the grid) apart from
    /// native-size ones (which keep their pixel size).
    pub requested_columns: u32,
    pub requested_rows: u32,
    /// Cached display pixel size for grid footprint bookkeeping. The
    /// render path resolves size per frame and never reads these.
    pub pixel_width: u32,
    pub pixel_height: u32,
    /// Sub-cell pixel offset.
    pub cell_x_offset: u32,
    pub cell_y_offset: u32,
    /// Z-index layer for rendering order.
    pub z_index: i32,
    /// Transmission timestamp for cache invalidation.
    pub transmit_time: crate::time::Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelativePlacement {
    pub image_id: u32,
    /// Public ID (or zero for preferred) in a request; tagged key after resolution.
    pub placement_id: u64,
    pub horizontal: i32,
    pub vertical: i32,
}

// Low 32 bits hold the ID; bit 32 separates allocator IDs from wire IDs.
const INTERNAL_PLACEMENT_TAG: u64 = 1 << 32;

/// Client-specified IDs occupy the full u32 range. An implicit placement has
/// public ID zero, regardless of the allocator key used to retain it.
pub fn public_placement_id(storage_id: u64) -> u32 {
    u32::try_from(storage_id).unwrap_or(0)
}

/// Resolve a raw kitty source rectangle against the image's current
/// dimensions: origin clamped to the edges, zero width/height meaning
/// "to the edge". Returns `None` when nothing of the crop lies inside
/// the image (or the image is empty).
pub fn resolve_source_rect(
    source_x: u32,
    source_y: u32,
    source_width: u32,
    source_height: u32,
    image_width: usize,
    image_height: usize,
) -> Option<(usize, usize, usize, usize)> {
    let rect = kitty_source_rect(
        source_x,
        source_y,
        source_width,
        source_height,
        image_width,
        image_height,
    );
    (rect.2 > 0 && rect.3 > 0).then_some(rect)
}

/// Clamp the source rectangle, retaining empty axes for placement sizing.
pub fn kitty_source_rect(
    source_x: u32,
    source_y: u32,
    source_width: u32,
    source_height: u32,
    image_width: usize,
    image_height: usize,
) -> (usize, usize, usize, usize) {
    let x = (source_x as usize).min(image_width);
    let y = (source_y as usize).min(image_height);
    let width = if source_width > 0 {
        (source_width as usize).min(image_width - x)
    } else {
        image_width - x
    };
    let height = if source_height > 0 {
        (source_height as usize).min(image_height - y)
    } else {
        image_height - y
    };
    (x, y, width, height)
}

/// Display pixel size for a kitty placement: the source rectangle
/// scaled to the requested cell span, keeping aspect when only one
/// axis is given, or shown at native size when no span is requested.
pub fn kitty_display_size(
    source_width: usize,
    source_height: usize,
    requested_columns: u32,
    requested_rows: u32,
    cell_width: usize,
    cell_height: usize,
    cell_offset: (u32, u32),
) -> (usize, usize) {
    let x_offset = (cell_offset.0 as usize).min(cell_width.saturating_sub(1));
    let y_offset = (cell_offset.1 as usize).min(cell_height.saturating_sub(1));
    let max = u32::MAX as usize;
    let scale = |value: usize, numerator: usize, denominator: usize| {
        if denominator == 0 {
            return 0;
        }
        ((value as u64 * numerator as u64 + denominator as u64 / 2) / denominator as u64)
            .min(u64::from(u32::MAX)) as usize
    };
    match (requested_columns, requested_rows) {
        (0, 0) => (source_width, source_height),
        (c, 0) => {
            let w = (c as usize)
                .saturating_mul(cell_width)
                .min(max)
                .saturating_sub(x_offset);
            let h = scale(w, source_height, source_width);
            (w, h)
        }
        (0, r) => {
            let h = (r as usize)
                .saturating_mul(cell_height)
                .min(max)
                .saturating_sub(y_offset);
            let w = scale(h, source_width, source_height);
            (w, h)
        }
        (c, r) => (
            (c as usize)
                .saturating_mul(cell_width)
                .min(max)
                .saturating_sub(x_offset),
            (r as usize)
                .saturating_mul(cell_height)
                .min(max)
                .saturating_sub(y_offset),
        ),
    }
}

pub(crate) fn kitty_grid_size(
    pixels: (usize, usize),
    requested: (u32, u32),
    cell: (usize, usize),
    offset: (u32, u32),
) -> (u32, u32) {
    if requested.0 > 0 && requested.1 > 0 {
        return requested;
    }
    let span = |pixels: usize, cell: usize, offset: u32| {
        if cell == 0 {
            return 0;
        }
        pixels
            .saturating_add((offset as usize).min(cell - 1))
            .min(u32::MAX as usize)
            .div_ceil(cell) as u32
    };
    (
        span(pixels.0, cell.0, offset.0),
        span(pixels.1, cell.1, offset.1),
    )
}

impl KittyPlacement {
    /// Clip rows crossed by a scroll margin, retaining image pixels that
    /// are still visible. Match Kitty/Ghostty's scaled and native crops.
    pub(crate) fn clip_rows(
        &mut self,
        image_size: (usize, usize),
        cell_size: (usize, usize),
        count: u32,
        from_top: bool,
    ) -> bool {
        let span = self.rows;
        if count == 0 {
            return true;
        }
        if count >= span || cell_size.1 == 0 {
            return false;
        }
        let Some((x, y, width, height)) = resolve_source_rect(
            self.source_x,
            self.source_y,
            self.source_width,
            self.source_height,
            image_size.0,
            image_size.1,
        ) else {
            return false;
        };
        let scaled = self.requested_rows > 0;
        let (next_y, next_height) = if scaled || from_top {
            let crop = if scaled {
                (height as u64 * u64::from(count) / u64::from(span)) as usize
            } else {
                cell_size.1.saturating_mul(count as usize)
            };
            if crop >= height {
                return false;
            }
            (y + if from_top { crop } else { 0 }, height - crop)
        } else {
            let visible = cell_size.1.saturating_mul((span - count) as usize);
            let offset = (self.cell_y_offset as usize).min(cell_size.1 - 1);
            if visible <= offset {
                return false;
            }
            (y, height.min(visible - offset))
        };
        self.source_x = x as u32;
        self.source_y = next_y as u32;
        self.source_width = width as u32;
        self.source_height = next_height as u32;
        if scaled {
            self.requested_rows -= count;
        }
        self.rescale(image_size.0, image_size.1, cell_size.0, cell_size.1);
        true
    }

    /// Recompute display size and cell span against the image's
    /// current dimensions and a cell size. Cell-sized placements
    /// track the grid; native-size ones keep their pixel dimensions
    /// but re-derive how many cells they cover. Called on resize and
    /// source crop changes.
    pub fn rescale(
        &mut self,
        image_width: usize,
        image_height: usize,
        cell_width: usize,
        cell_height: usize,
    ) {
        if cell_width == 0 || cell_height == 0 {
            return;
        }
        let (_, _, source_width, source_height) = kitty_source_rect(
            self.source_x,
            self.source_y,
            self.source_width,
            self.source_height,
            image_width,
            image_height,
        );
        let (w, h) = kitty_display_size(
            source_width,
            source_height,
            self.requested_columns,
            self.requested_rows,
            cell_width,
            cell_height,
            (self.cell_x_offset, self.cell_y_offset),
        );
        self.pixel_width = w as u32;
        self.pixel_height = h as u32;
        // Offsets are stored raw and clamped where they're read, so a
        // shrink-then-grow of the cell size can't lose the original.
        (self.columns, self.rows) = kitty_grid_size(
            (w, h),
            (self.requested_columns, self.requested_rows),
            (cell_width, cell_height),
            (self.cell_x_offset, self.cell_y_offset),
        );
    }
}

/// On-screen quad for a direct kitty placement, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KittyOverlayGeometry {
    pub x: f32,
    pub y: f32,
    /// Retain saturated protocol dimensions until conversion to a GPU quad.
    pub width: f64,
    pub height: f64,
    /// Normalized source rectangle within the image texture.
    pub source_rect: [f32; 4],
}

/// Clip an overlay quad to a panel's content rectangle, shrinking the
/// normalized source rect proportionally so the visible slice shows
/// exactly the covered part of the image. Returns `false` when nothing
/// of the quad lies inside the rect (drop the overlay).
///
/// Without this, an image wider or taller than its panel paints across
/// split dividers onto neighbor panels, since the image pipeline draws
/// global quads with no per-panel scissor.
pub fn clip_overlay_to_rect(
    overlay: &mut rio_graphics::GraphicOverlay,
    clip_x0: f32,
    clip_y0: f32,
    clip_x1: f32,
    clip_y1: f32,
) -> bool {
    if overlay.width <= 0.0 || overlay.height <= 0.0 {
        return false;
    }
    let x0 = overlay.x;
    let y0 = overlay.y;
    let x1 = overlay.x + overlay.width;
    let y1 = overlay.y + overlay.height;

    let nx0 = x0.max(clip_x0);
    let ny0 = y0.max(clip_y0);
    let nx1 = x1.min(clip_x1);
    let ny1 = y1.min(clip_y1);
    if nx1 <= nx0 || ny1 <= ny0 {
        return false;
    }

    let [u0, v0, u1, v1] = overlay.source_rect;
    let fx0 = (nx0 - x0) / overlay.width;
    let fx1 = (nx1 - x0) / overlay.width;
    let fy0 = (ny0 - y0) / overlay.height;
    let fy1 = (ny1 - y0) / overlay.height;
    overlay.source_rect = [
        u0 + (u1 - u0) * fx0,
        v0 + (v1 - v0) * fy0,
        u0 + (u1 - u0) * fx1,
        v0 + (v1 - v0) * fy1,
    ];
    overlay.x = nx0;
    overlay.y = ny0;
    overlay.width = nx1 - nx0;
    overlay.height = ny1 - ny0;
    true
}

/// Viewport parameters for placement geometry: the panel content
/// origin in physical pixels, the canonical cell stride the grid
/// paints with, and the scroll state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OverlayViewport {
    pub cell_width: f32,
    pub cell_height: f32,
    pub origin_x: f32,
    pub origin_y: f32,
    pub history_size: i64,
    pub display_offset: i64,
    pub screen_lines: i64,
}

/// Compute where a direct kitty placement lands on screen.
///
/// The crop and display size are resolved here, per frame, against
/// the image's current dimensions and the viewport's cell stride —
/// nothing baked at place time can go stale when the image is
/// retransmitted or the cell size changes. The viewport cell stride
/// must be the same one the grid paints with, so image position and
/// text stay in lockstep. Returns `None` when the placement is fully
/// outside the viewport or has zero pixel size; a partially
/// visible placement keeps its full quad and the GPU clips it.
pub fn kitty_overlay_geometry(
    placement: &KittyPlacement,
    image_width: usize,
    image_height: usize,
    viewport: &OverlayViewport,
) -> Option<KittyOverlayGeometry> {
    if placement.expired {
        return None;
    }
    // dest_row is absolute (scrollback aware); the viewport top sits
    // at history_size - display_offset, so scrolling up moves the
    // placement down relative to the viewport.
    let horizontal = placement.relative.map_or(0, |r| r.horizontal);
    let vertical = placement.relative.map_or(0, |r| r.vertical);
    let screen_row = placement
        .dest_row
        .saturating_sub(viewport.history_size - viewport.display_offset)
        .saturating_add(i64::from(vertical));
    let bottom_row = screen_row + placement.rows as i64;
    if bottom_row <= 0 || screen_row >= viewport.screen_lines {
        return None;
    }

    let (source_x, source_y, source_width, source_height) = kitty_source_rect(
        placement.source_x,
        placement.source_y,
        placement.source_width,
        placement.source_height,
        image_width,
        image_height,
    );

    let cell_width_px = viewport.cell_width.round() as usize;
    let cell_height_px = viewport.cell_height.round() as usize;
    if cell_width_px == 0 || cell_height_px == 0 {
        return None;
    }
    let (display_width, display_height) = kitty_display_size(
        source_width,
        source_height,
        placement.requested_columns,
        placement.requested_rows,
        cell_width_px,
        cell_height_px,
        (placement.cell_x_offset, placement.cell_y_offset),
    );
    if display_width == 0 || display_height == 0 {
        return None;
    }

    // Normalized `[u0, v0, u1, v1]` (origin, end), the convention all
    // three image shaders share.
    let image_width = image_width as f32;
    let image_height = image_height as f32;
    let source_rect = [
        source_x as f32 / image_width,
        source_y as f32 / image_height,
        (source_x + source_width) as f32 / image_width,
        (source_y + source_height) as f32 / image_height,
    ];

    // Per the kitty spec the sub-cell offset stays inside the cell
    // box; stored values are raw, so clamp against the current cell
    // size here.
    let x_offset = (placement.cell_x_offset as f32).min(viewport.cell_width - 1.0);
    let y_offset = (placement.cell_y_offset as f32).min(viewport.cell_height - 1.0);

    Some(KittyOverlayGeometry {
        x: viewport.origin_x
            + (placement.dest_col as f32 + horizontal as f32) * viewport.cell_width
            + x_offset,
        y: viewport.origin_y + screen_row as f32 * viewport.cell_height + y_offset,
        width: display_width as f64,
        height: display_height as f64,
        source_rect,
    })
}

/// One displayed sixel/iTerm2 image region, anchored to grid content
/// (DEC semantics: unlike floating kitty placements, these clip under
/// text and erase operations and shift with region scrolls).
///
/// The source rectangle lives in display-pixel space at insert time:
/// initially `(0, 0, total_w, total_h)`; overwrite splits subtract
/// cell-aligned holes, producing children referencing the same texture
/// with adjusted crops (no pixel copies). Normalized texture
/// coordinates fall out directly since display space is a uniform
/// scale of the image.
#[derive(Debug, Clone, PartialEq)]
pub struct AtlasPlacement {
    /// Texture key (`atlas_image_key(GraphicId)`).
    pub image_key: u64,
    /// Anchor row in the stable absolute space
    /// (`lines_evicted + history + screen_row` at insert).
    pub abs_row: i64,
    /// Leftmost grid column.
    pub col: usize,
    /// Cell span of this (possibly split) region.
    pub columns: usize,
    pub rows: usize,
    /// Crop in display pixels at insert scale.
    pub src_x: u32,
    pub src_y: u32,
    pub src_width: u32,
    pub src_height: u32,
    /// Full display size at insert (normalization denominator).
    pub total_width: u32,
    pub total_height: u32,
    /// Cell stride at insert; rendering scales by live/insert.
    pub insert_cell_w: u16,
    pub insert_cell_h: u16,
}

impl AtlasPlacement {
    /// Child covering `rows` [row0, row1) x `cols` [col0, col1) of this
    /// placement (caller guarantees a non-empty intersection with the
    /// placement's rect). The crop follows in display-pixel space;
    /// edges that coincide with the parent's keep its exact bounds so
    /// partial edge cells stay partial.
    pub(crate) fn slice(
        &self,
        row0: i64,
        row1: i64,
        col0: usize,
        col1: usize,
    ) -> AtlasPlacement {
        let icw = self.insert_cell_w as u32;
        let ich = self.insert_cell_h as u32;
        let src_x0 = self.src_x + (col0 - self.col) as u32 * icw;
        let src_y0 = self.src_y + (row0 - self.abs_row) as u32 * ich;
        let src_x1 = (self.src_x + (col1 - self.col) as u32 * icw)
            .min(self.src_x + self.src_width);
        let src_y1 = (self.src_y + (row1 - self.abs_row) as u32 * ich)
            .min(self.src_y + self.src_height);
        AtlasPlacement {
            image_key: self.image_key,
            abs_row: row0,
            col: col0,
            columns: col1 - col0,
            rows: (row1 - row0) as usize,
            src_x: src_x0,
            src_y: src_y0,
            src_width: src_x1.saturating_sub(src_x0),
            src_height: src_y1.saturating_sub(src_y0),
            total_width: self.total_width,
            total_height: self.total_height,
            insert_cell_w: self.insert_cell_w,
            insert_cell_h: self.insert_cell_h,
        }
    }

    /// Subtract a cell-aligned hole (rows [hr0, hr1) x cols [hc0, hc1))
    /// from this placement. Returns `None` when the hole misses the
    /// placement entirely (keep it as is); otherwise the surviving
    /// pieces are appended to `out`: up to four children referencing the
    /// same texture with adjusted crops, zero pixel copies (an
    /// empty append means the hole swallowed the whole placement).
    pub fn subtract_rect(
        &self,
        hr0: i64,
        hr1: i64,
        hc0: usize,
        hc1: usize,
        out: &mut Vec<AtlasPlacement>,
    ) -> Option<()> {
        let p_r0 = self.abs_row;
        let p_r1 = self.abs_row + self.rows as i64;
        let p_c0 = self.col;
        let p_c1 = self.col + self.columns;
        if hr1 <= p_r0 || hr0 >= p_r1 || hc1 <= p_c0 || hc0 >= p_c1 {
            return None;
        }
        let ir0 = hr0.max(p_r0);
        let ir1 = hr1.min(p_r1);
        // Filter at each push (not a whole-vec retain: `out` may
        // accumulate children of many placements in the clip loops).
        let mut push = |child: AtlasPlacement| {
            if child.columns > 0
                && child.rows > 0
                && child.src_width > 0
                && child.src_height > 0
            {
                out.push(child);
            }
        };
        // Top and bottom keep the full placement width; left and right
        // cover only the hole's rows.
        if ir0 > p_r0 {
            push(self.slice(p_r0, ir0, p_c0, p_c1));
        }
        if ir1 < p_r1 {
            push(self.slice(ir1, p_r1, p_c0, p_c1));
        }
        if hc0 > p_c0 {
            push(self.slice(ir0, ir1, p_c0, hc0.min(p_c1)));
        }
        if hc1 < p_c1 {
            push(self.slice(ir0, ir1, hc1.max(p_c0), p_c1));
        }
        Some(())
    }
}

/// Compute where an atlas placement lands on screen. Same viewport
/// contract as `kitty_overlay_geometry`; display size scales from the
/// insert-time cell stride to the live one so images track font size.
pub fn atlas_overlay_geometry(
    placement: &AtlasPlacement,
    viewport: &OverlayViewport,
) -> Option<KittyOverlayGeometry> {
    let screen_row =
        placement.abs_row - (viewport.history_size - viewport.display_offset);
    let bottom_row = screen_row + placement.rows as i64;
    if bottom_row <= 0 || screen_row >= viewport.screen_lines {
        return None;
    }
    if placement.src_width == 0
        || placement.src_height == 0
        || placement.total_width == 0
        || placement.total_height == 0
    {
        return None;
    }

    let scale_x = viewport.cell_width / placement.insert_cell_w.max(1) as f32;
    let scale_y = viewport.cell_height / placement.insert_cell_h.max(1) as f32;
    let total_w = placement.total_width as f32;
    let total_h = placement.total_height as f32;

    Some(KittyOverlayGeometry {
        x: viewport.origin_x + placement.col as f32 * viewport.cell_width,
        y: viewport.origin_y + screen_row as f32 * viewport.cell_height,
        width: placement.src_width as f64 * f64::from(scale_x),
        height: placement.src_height as f64 * f64::from(scale_y),
        source_rect: [
            placement.src_x as f32 / total_w,
            placement.src_y as f32 / total_h,
            (placement.src_x + placement.src_width) as f32 / total_w,
            (placement.src_y + placement.src_height) as f32 / total_h,
        ],
    })
}

/// Virtual placement metadata for Kitty graphics protocol
/// Stored separately from direct graphics in cells
#[derive(Debug, Clone, PartialEq)]
pub struct VirtualPlacement {
    pub image_id: u32,
    pub placement_id: u64,
    pub columns: u32,
    pub rows: u32,
    /// Raw source rectangle (`x=`/`y=`/`w=`/`h=`), resolved against
    /// the image's current dimensions at render time like direct
    /// placements.
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Per-screen Kitty graphics state.
///
/// Each terminal has two screens (main and alt). Per the kitty graphics
/// spec each screen owns its own image cache, placements, and number
/// mappings, so swapping into the alt screen hides main-screen images
/// (and vice versa) instead of leaking them across the boundary.
///
/// `Graphics` keeps the *active* screen's state inline (so existing
/// rendering code can read `graphics.kitty_*` directly without changes)
/// and stores the *inactive* screen's state in this struct. On
/// `swap_kitty_screen_state` the two are exchanged with `mem::swap`.
#[derive(Debug, Default)]
pub struct KittyScreenState {
    pub next_internal_placement_id: u32,
    pub kitty_chunking_state: crate::ansi::kitty_graphics_protocol::KittyGraphicsState,
    pub kitty_images: FxHashMap<u32, StoredImage>,
    pub kitty_animations: FxHashMap<u32, Animation>,
    pub kitty_image_numbers: FxHashMap<u32, u32>,
    pub kitty_placements: FxHashMap<(u32, u64), KittyPlacement>,
    pub kitty_virtual_placements: FxHashMap<(u32, u64), VirtualPlacement>,
    pub atlas_placements: Vec<AtlasPlacement>,
    pub atlas_key_refs: FxHashMap<u64, u32>,
}

/// Track changes in the grid to add or to remove graphics.
#[derive(Debug)]
pub struct Graphics {
    /// Enable only when the consumer acknowledges every UpdateGraphics batch.
    pub coalesce_updates: bool,
    /// One batch may own pixels in the desktop event queue at a time.
    pub update_in_flight: bool,
    /// Last generated identifier.
    pub last_id: u64,

    /// New atlas graphics (sixel/iTerm2), received from the PTY.
    pub pending: Vec<GraphicData>,

    /// New image textures (kitty), keyed by image_id.
    pub pending_images: Vec<(u32, GraphicData)>,

    /// Graphics removed from the grid.
    pub texture_operations: Arc<Mutex<Vec<u64>>>,

    /// Shared palette for Sixel graphics.
    pub sixel_shared_palette: Option<Vec<ColorRgb>>,

    /// Cell height in pixels.
    pub cell_height: f32,

    /// Cell width in pixels.
    pub cell_width: f32,

    /// Current Sixel parser.
    pub sixel_parser: Option<Box<sixel::Parser>>,

    /// Kitty graphics: Cache of transmitted images (by image_id)
    /// Allows placing the same image multiple times without re-transmission
    pub kitty_images: FxHashMap<u32, StoredImage>,
    /// Frame buffers stay in the terminal, outside renderer image snapshots.
    pub kitty_animations: FxHashMap<u32, Animation>,

    /// Kitty graphics: Image number to ID mapping (for I= parameter)
    /// Maps image number to the most recently transmitted image with that number
    pub kitty_image_numbers: FxHashMap<u32, u32>,

    /// Kitty graphics: Virtual placements (when U=1)
    /// Key is (image_id, placement_id), value is placement metadata
    pub kitty_virtual_placements: FxHashMap<(u32, u64), VirtualPlacement>,

    /// Kitty graphics: State for chunked image transmissions
    /// Owns the active screen's single pending load and automatic image IDs.
    pub kitty_chunking_state: crate::ansi::kitty_graphics_protocol::KittyGraphicsState,

    /// Total bytes of image data currently stored in memory
    /// Includes both pending graphics and stored Kitty images
    pub total_bytes: usize,

    /// Memory limit for graphics storage (default 320MB per kitty spec)
    /// If this is exceeded, oldest/unused images will be evicted
    pub total_limit: usize,

    /// Tracks when each graphic was added (for eviction priority)
    /// Maps GraphicId to insertion timestamp
    pub image_timestamps: FxHashMap<GraphicId, crate::time::Instant>,

    /// Byte sizes of tracked atlas (sixel/iTerm2) graphics. The pixels
    /// leave for the frontend via `take_queues`, so when a graphic's
    /// last placement dies this map is the only way to give its bytes
    /// back to `total_bytes` — without it every atlas image counted
    /// against the budget forever and starved kitty images into
    /// eviction.
    pub graphic_sizes: FxHashMap<GraphicId, usize>,

    /// Weak references to placed textures, for O(1) liveness checks.
    /// Avoids scanning the entire grid to find which graphics are in use.
    /// When an image loses its last placement, the
    /// will report strong_count() == 0, meaning the graphic is no longer displayed.

    /// Kitty graphics: Overlay placements.
    /// Key is (image_id, placement_id). Rendered as overlays, not in grid cells.
    pub kitty_placements: FxHashMap<(u32, u64), KittyPlacement>,

    /// Sixel/iTerm2 placements (DEC grid-plane semantics: clip under
    /// text/erase, shift with region scrolls, expire off the ring).
    pub atlas_placements: Vec<AtlasPlacement>,

    /// How many placements reference each atlas image key; the last
    /// release queues the key for pixel-store + GPU texture removal.
    pub atlas_key_refs: FxHashMap<u64, u32>,

    /// Kitty graphics state for the *inactive* screen.
    /// When the terminal toggles between main and alt screens this is
    /// swapped with the active fields (`kitty_images`, `kitty_placements`,
    /// `kitty_image_numbers`, `kitty_virtual_placements`) so each screen
    /// keeps its own image set.
    pub kitty_inactive_screen: KittyScreenState,

    /// Which pixels the frontend texture for each kitty image id holds,
    /// by the `transmission_time` of the store they were queued from.
    /// Textures outlive the main/alt screen split while image stores
    /// are per screen, so this is deliberately not swapped with the
    /// screens: a same-id image on the other screen overwrites the
    /// texture, and an eviction on either screen can free it. Other
    /// terminals in the window draw from keys namespaced by route, so
    /// they never share a texture with this one.
    pub kitty_texture_contents: FxHashMap<u32, crate::time::Instant>,

    /// Counter for auto-assigning internal placement IDs.
    ///
    /// Per kitty spec, when a client asks to place an image without an
    /// explicit `p=` (or with `p=0`), the terminal must allocate a
    /// unique placement_id internally so multiple placements of the
    /// same image don't collide. Storage keys tag this per-screen counter
    /// separately from the complete u32 range of client-specified IDs.
    pub next_internal_placement_id: u32,
    /// Preserved across resets so old chunks cannot target a new image.
    pub next_kitty_image_generation: u64,

    /// Signals the renderer that overlay placements have changed.
    pub kitty_graphics_dirty: bool,
}

impl Default for Graphics {
    fn default() -> Self {
        Self {
            coalesce_updates: false,
            update_in_flight: false,
            last_id: 0,
            pending: Vec::new(),
            pending_images: Vec::new(),
            texture_operations: Arc::new(Mutex::new(Vec::new())),
            sixel_shared_palette: None,
            cell_height: 0.0,
            cell_width: 0.0,
            sixel_parser: None,
            kitty_images: FxHashMap::default(),
            kitty_animations: FxHashMap::default(),
            kitty_image_numbers: FxHashMap::default(),
            kitty_virtual_placements: FxHashMap::default(),
            kitty_chunking_state:
                crate::ansi::kitty_graphics_protocol::KittyGraphicsState::default(),
            total_bytes: 0,
            total_limit: 320 * 1024 * 1024, // 320MB per kitty spec
            image_timestamps: FxHashMap::default(),
            graphic_sizes: FxHashMap::default(),
            kitty_placements: FxHashMap::default(),
            atlas_placements: Vec::new(),
            atlas_key_refs: FxHashMap::default(),
            kitty_inactive_screen: KittyScreenState::default(),
            kitty_texture_contents: FxHashMap::default(),
            next_internal_placement_id: 0,
            next_kitty_image_generation: 0,
            kitty_graphics_dirty: false,
        }
    }
}

impl Graphics {
    pub fn kitty_image_identity(
        &self,
        image_id: u32,
        image_number: u32,
    ) -> Result<crate::ansi::kitty_animation::ImageIdentity, &'static str> {
        Self::image_identity(
            &self.kitty_images,
            &self.kitty_image_numbers,
            image_id,
            image_number,
        )
    }

    pub(crate) fn image_identity(
        images: &FxHashMap<u32, StoredImage>,
        numbers: &FxHashMap<u32, u32>,
        image_id: u32,
        image_number: u32,
    ) -> Result<crate::ansi::kitty_animation::ImageIdentity, &'static str> {
        if image_id == 0 && image_number == 0 {
            return Err("EINVAL: image ID or number required");
        }
        let image_id = if image_id != 0 {
            image_id
        } else {
            *numbers
                .get(&image_number)
                .ok_or("ENOENT: image not found")?
        };
        let stored = images.get(&image_id).ok_or("ENOENT: image not found")?;
        Ok(crate::ansi::kitty_animation::ImageIdentity {
            image_id,
            generation: stored.image_generation,
        })
    }
    pub fn execute_animation(
        &mut self,
        image_id: u32,
        image_number: u32,
        mut command: AnimationCommand,
    ) -> Result<(u32, Option<u32>), crate::ansi::kitty_animation::Failure> {
        let mut frame = None;
        let result = (|| -> Result<(u32, Option<u32>), &'static str> {
            if image_id == 0 && image_number == 0 {
                return Err("EINVAL: image ID or number required");
            }
            let image_id = if image_id != 0 {
                image_id
            } else {
                *self
                    .kitty_image_numbers
                    .get(&image_number)
                    .ok_or("ENOENT: image not found")?
            };
            let stored = self
                .kitty_images
                .get(&image_id)
                .ok_or("ENOENT: image not found")?;
            let pixel_count = stored
                .data
                .width
                .checked_mul(stored.data.height)
                .ok_or("EINVAL:invalid image dimensions")?;
            if pixel_count == 0 {
                return Err("EINVAL:invalid image dimensions");
            }
            let root_bytes = pixel_count
                .checked_mul(4)
                .ok_or("EINVAL:invalid image dimensions")?;
            let bpp = if stored.data.color_type == rio_graphics::ColorType::Rgb {
                3
            } else {
                4
            };
            if pixel_count.checked_mul(bpp) != Some(stored.data.pixels.len()) {
                return Err("EINVAL:invalid image pixels");
            }
            if let AnimationCommand::Frame(upload) = &mut command {
                Self::animation_rgba(&mut upload.data)?;
            }
            let empty =
                (!self.kitty_animations.contains_key(&image_id)).then(Animation::default);
            let animation = self
                .kitty_animations
                .get(&image_id)
                .or(empty.as_ref())
                .unwrap();
            if let AnimationCommand::Frame(upload) = &command {
                frame = Some(animation.resolve_upload_frame(&stored.data, upload)?);
            }
            let additional = match &command {
                AnimationCommand::Frame(upload) => {
                    animation.validate_upload(&stored.data, upload)?.1
                }
                AnimationCommand::Compose(composition) => {
                    animation.validate_composition(&stored.data, *composition)?;
                    0
                }
                AnimationCommand::Control(_) => 0,
            };
            if additional != 0
                && self
                    .kitty_animations
                    .values()
                    .chain(self.kitty_inactive_screen.kitty_animations.values())
                    .map(|animation| animation.frame_count() - 1)
                    .sum::<usize>()
                    >= 4096
            {
                return Err("ENOSPC:animation frame count limit");
            }
            let convert_root = !matches!(&command, AnimationCommand::Control(_));
            let upgrade = if convert_root {
                root_bytes.saturating_sub(stored.data.pixels.len())
            } else {
                0
            };
            let old_bytes = stored.data.pixels.len() + animation.extra_bytes();
            let reserve = additional.saturating_add(upgrade);
            if self.total_bytes.saturating_add(reserve) > self.total_limit {
                let mut used = self.collect_active_graphic_ids();
                used.insert(image_id as u64);
                if !self.evict_images_except(reserve, &used, Some(image_id)) {
                    return Err("ENOSPC: animation frame storage full");
                }
            }
            let image = &mut self.kitty_images.get_mut(&image_id).unwrap().data;
            if convert_root {
                Self::animation_rgba(image)?;
            }
            let animation = self.kitty_animations.entry(image_id).or_default();
            let outcome = match command {
                AnimationCommand::Frame(upload) => animation
                    .upload(image, upload)
                    .map(|(frame, changed)| (Some(frame), changed)),
                AnimationCommand::Compose(composition) => animation
                    .compose(image, composition)
                    .map(|changed| (None, changed)),
                AnimationCommand::Control(control) => {
                    Ok((None, animation.control(image, control)))
                }
            };
            self.total_bytes = self
                .total_bytes
                .saturating_sub(old_bytes)
                .saturating_add(image.pixels.len() + animation.extra_bytes());
            let (frame, changed) = outcome?;
            if changed || upgrade != 0 {
                self.animation_frame_changed(image_id);
            }
            Ok((image_id, frame))
        })();
        result.map_err(|message| crate::ansi::kitty_animation::Failure { message, frame })
    }

    fn animation_rgba(image: &mut GraphicData) -> Result<(), &'static str> {
        let count = image
            .width
            .checked_mul(image.height)
            .ok_or("EINVAL:invalid image dimensions")?;
        let bpp = if image.color_type == rio_graphics::ColorType::Rgb {
            3
        } else {
            4
        };
        if count.checked_mul(bpp) != Some(image.pixels.len()) {
            return Err("EINVAL:invalid image pixels");
        }
        if bpp == 3 {
            let mut pixels = Vec::with_capacity(
                count
                    .checked_mul(4)
                    .ok_or("EINVAL:invalid image dimensions")?,
            );
            for rgb in image.pixels.chunks_exact(3) {
                pixels.extend_from_slice(rgb);
                pixels.push(255);
            }
            image.pixels = pixels;
            image.color_type = rio_graphics::ColorType::Rgba;
        }
        Ok(())
    }

    fn animation_frame_changed(&mut self, image_id: u32) {
        let previous = self
            .kitty_images
            .get(&image_id)
            .map(|stored| stored.transmission_time)
            .into_iter()
            .chain(
                self.kitty_inactive_screen
                    .kitty_images
                    .get(&image_id)
                    .map(|stored| stored.transmission_time),
            )
            .chain(self.kitty_texture_contents.get(&image_id).copied())
            .max();
        let mut stamp = crate::time::Instant::now();
        if let Some(previous) = previous {
            if stamp <= previous {
                stamp = previous + crate::time::Duration::from_nanos(1);
            }
        }
        let image = self.kitty_images.get_mut(&image_id).unwrap();
        image.transmission_time = stamp;
        image.data.transmit_time = stamp;
        image.data.is_opaque = false;
        self.kitty_graphics_dirty = true;
        if self
            .kitty_placements
            .keys()
            .chain(self.kitty_virtual_placements.keys())
            .any(|key| key.0 == image_id)
        {
            self.queue_kitty_upload(image_id);
        }
    }

    pub fn pause_kitty_animations(&mut self, now: crate::time::Instant) {
        for animation in self.kitty_animations.values_mut() {
            animation.pause(now);
        }
    }

    pub fn delete_animation_frame(
        &mut self,
        image_id: u32,
        image_number: u32,
        number: u32,
        delete_data: bool,
    ) -> bool {
        let image_id = if image_id != 0 {
            image_id
        } else {
            let Some(&id) = self.kitty_image_numbers.get(&image_number) else {
                return false;
            };
            id
        };
        if !self.kitty_images.contains_key(&image_id) {
            return false;
        }
        if self
            .kitty_animations
            .get(&image_id)
            .is_none_or(|animation| animation.frame_count() <= 1)
        {
            if !delete_data {
                return false;
            }
            self.delete_kitty_images(|id, _| *id == image_id);
            return true;
        }
        let animation = self.kitty_animations.get_mut(&image_id).unwrap();
        let image = &mut self.kitty_images.get_mut(&image_id).unwrap().data;
        let (released, changed) = animation.delete_frame(image, number);
        self.total_bytes = self.total_bytes.saturating_sub(released);
        if changed {
            self.animation_frame_changed(image_id);
        }
        released != 0
    }

    pub fn tick_kitty_animations(
        &mut self,
        now: crate::time::Instant,
    ) -> Option<crate::time::Duration> {
        if self.kitty_animations.is_empty() {
            return None;
        }
        let placed: std::collections::HashSet<_> = self
            .kitty_placements
            .keys()
            .chain(self.kitty_virtual_placements.keys())
            .map(|key| key.0)
            .collect();
        let mut changed = Vec::new();
        let mut next: Option<crate::time::Duration> = None;
        for (&id, animation) in &mut self.kitty_animations {
            if !placed.contains(&id) {
                animation.pause(now);
                continue;
            }
            let Some(image) = self.kitty_images.get_mut(&id) else {
                continue;
            };
            let (updated, delay) = animation.tick(&mut image.data, now);
            if updated {
                changed.push(id);
            }
            if let Some(delay) = delay {
                next = Some(next.map_or(delay, |current| current.min(delay)));
            }
        }
        for id in changed {
            self.animation_frame_changed(id);
        }
        next
    }

    /// Resolve Q=0 deterministically, then validate the proposed chain before
    /// replacing a placement. Image transmission may already have replaced
    /// the old image and its placements. Eight parent links match Kitty.
    pub fn resolve_placement_parent(
        &self,
        request: &crate::ansi::kitty_graphics_protocol::PlacementRequest,
    ) -> Result<Option<RelativePlacement>, &'static str> {
        let Some(mut relative) = request.relative else {
            return Ok(None);
        };
        if request.virtual_placement {
            return Err("EINVAL: virtual placement cannot refer to a parent");
        }
        if !self.kitty_images.contains_key(&relative.image_id) {
            return Err("ENOPARENT: parent image not found");
        }
        if relative.placement_id == 0 {
            relative.placement_id = self
                .kitty_placements
                .keys()
                .chain(self.kitty_virtual_placements.keys())
                .filter(|key| key.0 == relative.image_id)
                .map(|key| key.1)
                .min()
                .ok_or("ENOPARENT: parent placement not found")?;
        }
        let parent = (relative.image_id, relative.placement_id);
        if self.kitty_placements.get(&parent).is_none_or(|p| p.expired)
            && !self.kitty_virtual_placements.contains_key(&parent)
        {
            return Err("ENOPARENT: parent placement not found");
        }
        let child = (request.image_id, u64::from(request.placement_id));
        if request.placement_id != 0 && parent == child {
            return Err("EINVAL: placement cannot be its own parent");
        }
        let mut key = parent;
        for _ in 0..8 {
            if request.placement_id != 0 && key == child {
                return Err("ECYCLE: parent chain creates a cycle");
            }
            if self.kitty_virtual_placements.contains_key(&key) {
                return Ok(Some(relative));
            }
            let placement = self
                .kitty_placements
                .get(&key)
                .ok_or("ENOENT: parent chain ancestor not found")?;
            if placement.expired {
                return Err("ENOPARENT: parent placement not found");
            }
            match placement.relative {
                Some(ancestor) => key = (ancestor.image_id, ancestor.placement_id),
                None => return Ok(Some(relative)),
            }
        }
        Err("ETOODEEP: parent chain too deep")
    }

    /// Owned geometry for parent chains. Virtual roots retain their identity
    /// for positioning from the renderer's owned placeholder rows each frame.
    pub fn resolved_kitty_placements(&self) -> Vec<KittyPlacement> {
        self.kitty_placements
            .values()
            .filter_map(|placement| {
                if placement.expired
                    || !self.kitty_images.contains_key(&placement.image_id)
                {
                    return None;
                }
                let mut result = placement.clone();
                let Some(mut relative) = placement.relative else {
                    return Some(result);
                };
                let mut key = (relative.image_id, relative.placement_id);
                for _ in 0..8 {
                    if self.kitty_virtual_placements.contains_key(&key) {
                        relative.image_id = key.0;
                        relative.placement_id = key.1;
                        result.relative = Some(relative);
                        return Some(result);
                    }
                    let parent = self.kitty_placements.get(&key)?;
                    if parent.expired {
                        return None;
                    }
                    match parent.relative {
                        Some(ancestor) => {
                            relative.horizontal =
                                relative.horizontal.saturating_add(ancestor.horizontal);
                            relative.vertical =
                                relative.vertical.saturating_add(ancestor.vertical);
                            key = (ancestor.image_id, ancestor.placement_id);
                        }
                        None => {
                            result.dest_col = parent.dest_col;
                            result.dest_row = parent.dest_row;
                            relative.image_id = key.0;
                            relative.placement_id = key.1;
                            result.relative = Some(relative);
                            return Some(result);
                        }
                    }
                }
                None
            })
            .collect()
    }

    pub fn reap_expired_kitty_placements(&mut self) {
        let before = self.kitty_placements.len();
        self.kitty_placements
            .retain(|_, placement| !placement.expired);
        if before != self.kitty_placements.len() {
            self.remove_orphaned_placements();
            self.kitty_graphics_dirty = true;
        }
    }

    pub fn remove_orphaned_placements(&mut self) {
        fn prune(
            placements: &mut FxHashMap<(u32, u64), KittyPlacement>,
            virtuals: &FxHashMap<(u32, u64), VirtualPlacement>,
        ) -> bool {
            let mut changed = false;
            loop {
                let victims: Vec<_> = placements
                    .iter()
                    .filter_map(|(&key, p)| {
                        p.relative
                            .filter(|r| {
                                !placements.contains_key(&(r.image_id, r.placement_id))
                                    && !virtuals
                                        .contains_key(&(r.image_id, r.placement_id))
                            })
                            .map(|_| key)
                    })
                    .collect();
                if victims.is_empty() {
                    return changed;
                }
                changed = true;
                for key in victims {
                    placements.remove(&key);
                }
            }
        }
        let changed = prune(&mut self.kitty_placements, &self.kitty_virtual_placements);
        prune(
            &mut self.kitty_inactive_screen.kitty_placements,
            &self.kitty_inactive_screen.kitty_virtual_placements,
        );
        self.kitty_graphics_dirty |= changed;
    }

    pub fn can_place_kitty(
        &self,
        image: u32,
        placement: u32,
        virtual_placement: bool,
    ) -> bool {
        let replacing = (virtual_placement || placement != 0)
            && (self
                .kitty_virtual_placements
                .contains_key(&(image, u64::from(placement)))
                || self
                    .kitty_placements
                    .contains_key(&(image, u64::from(placement))));
        let other = self.kitty_virtual_placements.len()
            + self.kitty_inactive_screen.kitty_placements.len()
            + self.kitty_inactive_screen.kitty_virtual_placements.len();
        if replacing || self.kitty_placements.len() + other < 4096 {
            return true;
        }
        // A successful placement reaps expired anchors and their descendants.
        self.kitty_placements.values().any(|p| p.expired)
            && self.resolved_kitty_placements().len() + other < 4096
    }

    /// Create a new instance, and initialize it with the dimensions of the
    /// window.
    pub fn new<S: Dimensions>(size: &S) -> Self {
        let mut graphics = Graphics::default();
        graphics.resize(size);
        graphics
    }

    /// Generate a new graphic identifier (for sixel/iTerm2 atlas graphics).
    pub fn next_id(&mut self) -> GraphicId {
        self.last_id += 1;
        GraphicId::new(self.last_id)
    }

    /// Get queues to update graphics in the grid.
    ///
    /// If all queues are empty, it returns `None`.
    pub fn has_pending_updates(&self) -> bool {
        !self.pending.is_empty()
            || !self.pending_images.is_empty()
            || !self.texture_operations.lock().is_empty()
    }

    pub fn take_queues(&mut self) -> Option<UpdateQueues> {
        let remove_queue = {
            let mut queue = self.texture_operations.lock();
            if queue.is_empty() {
                Vec::new()
            } else {
                mem::take(&mut *queue)
            }
        };

        if remove_queue.is_empty()
            && self.pending.is_empty()
            && self.pending_images.is_empty()
        {
            return None;
        }

        Some(UpdateQueues {
            pending: mem::take(&mut self.pending),
            pending_images: mem::take(&mut self.pending_images),
            remove_queue,
        })
    }

    /// Update cell dimensions.
    pub fn resize<S: Dimensions>(&mut self, size: &S) {
        self.cell_height = size.square_height();
        self.cell_width = size.square_width();
    }

    /// Allocate a unique internal placement_id.
    ///
    /// Used by `place_kitty_overlay` whenever a placement request comes
    /// in with `placement_id == 0`. Without this, two `a=T` calls (or a
    /// client running `kitten icat` repeatedly) for the same image_id
    /// would each insert at key `(image_id, 0)` and the second would
    /// silently overwrite the first.
    pub fn allocate_internal_placement_id(&mut self) -> u64 {
        let id = self.next_internal_placement_id;
        self.next_internal_placement_id = id.wrapping_add(1);
        INTERNAL_PLACEMENT_TAG | u64::from(id)
    }

    /// Swap kitty graphics state between the active and inactive screen.
    ///
    /// Called by `Crosswords::swap_alt` so each screen (main vs alt)
    /// keeps its own image cache, placements, number mappings, and
    /// virtual placements. Marks the kitty overlay layer dirty so the
    /// renderer rebuilds its overlay set against the new active screen.
    /// Recount atlas image key references from the placement vec (the
    /// single source of truth) after any placement mutation. Keys that
    /// lost their last placement are queued so the frontend frees the
    /// pixel store and the GPU texture. The vec is tiny, so a full
    /// recount beats distributed per-child bookkeeping.
    pub fn recount_atlas_keys(&mut self) {
        Self::bound_atlas_placements(&mut self.atlas_placements);
        let lost = Self::recount_atlas_keys_for(
            &self.atlas_placements,
            &mut self.atlas_key_refs,
            &self.texture_operations,
        );
        self.untrack_atlas_keys(&lost);
    }

    pub fn recount_inactive_atlas_keys(&mut self) {
        Self::bound_atlas_placements(&mut self.kitty_inactive_screen.atlas_placements);
        let lost = Self::recount_atlas_keys_for(
            &self.kitty_inactive_screen.atlas_placements,
            &mut self.kitty_inactive_screen.atlas_key_refs,
            &self.texture_operations,
        );
        self.untrack_atlas_keys(&lost);
    }

    fn bound_atlas_placements(placements: &mut Vec<AtlasPlacement>) {
        // Erasing through an image splits placements. Evict the oldest image
        // as a whole when fragmentation exceeds the per-screen metadata cap.
        while placements.len() > 4096 {
            let victim = placements[0].image_key;
            placements.retain(|placement| placement.image_key != victim);
        }
    }

    fn recount_atlas_keys_for(
        placements: &[AtlasPlacement],
        key_refs: &mut FxHashMap<u64, u32>,
        texture_operations: &std::sync::Arc<parking_lot::Mutex<Vec<u64>>>,
    ) -> Vec<u64> {
        let mut refs: FxHashMap<u64, u32> = FxHashMap::default();
        for placement in placements {
            *refs.entry(placement.image_key).or_insert(0) += 1;
        }
        let mut lost = Vec::new();
        let mut removals = texture_operations.lock();
        for key in key_refs.keys() {
            if !refs.contains_key(key) {
                removals.push(*key);
                lost.push(*key);
            }
        }
        drop(removals);
        *key_refs = refs;
        lost
    }

    /// Give the bytes of released atlas keys back to the budget. Their
    /// pixels left with the frontend on `take_queues`, so the eviction
    /// scan can never find them again — this is the only release path.
    /// A key still referenced by the other screen keeps its bytes.
    pub fn untrack_atlas_keys(&mut self, keys: &[u64]) {
        for &key in keys {
            if self.atlas_key_refs.contains_key(&key)
                || self.kitty_inactive_screen.atlas_key_refs.contains_key(&key)
            {
                continue;
            }
            // Atlas keys live above 2^32; recover the GraphicId part.
            let graphic_id = GraphicId::new(key.saturating_sub(1u64 << 32));
            self.pending.retain(|graphic| graphic.id != graphic_id);
            if let Some(bytes) = self.graphic_sizes.remove(&graphic_id) {
                self.image_timestamps.remove(&graphic_id);
                self.total_bytes = self.total_bytes.saturating_sub(bytes);
            }
        }
    }

    /// Returns whether pixel uploads were queued: an image on the
    /// incoming screen whose id was also uploaded by the outgoing
    /// screen no longer owns its texture and, if placed, is resent.
    pub fn swap_kitty_screen_state(&mut self) -> bool {
        std::mem::swap(
            &mut self.next_internal_placement_id,
            &mut self.kitty_inactive_screen.next_internal_placement_id,
        );
        self.pause_kitty_animations(crate::time::Instant::now());
        std::mem::swap(
            &mut self.kitty_chunking_state,
            &mut self.kitty_inactive_screen.kitty_chunking_state,
        );
        std::mem::swap(
            &mut self.kitty_images,
            &mut self.kitty_inactive_screen.kitty_images,
        );
        std::mem::swap(
            &mut self.kitty_animations,
            &mut self.kitty_inactive_screen.kitty_animations,
        );
        std::mem::swap(
            &mut self.kitty_image_numbers,
            &mut self.kitty_inactive_screen.kitty_image_numbers,
        );
        std::mem::swap(
            &mut self.kitty_placements,
            &mut self.kitty_inactive_screen.kitty_placements,
        );
        std::mem::swap(
            &mut self.kitty_virtual_placements,
            &mut self.kitty_inactive_screen.kitty_virtual_placements,
        );
        std::mem::swap(
            &mut self.atlas_placements,
            &mut self.kitty_inactive_screen.atlas_placements,
        );
        std::mem::swap(
            &mut self.atlas_key_refs,
            &mut self.kitty_inactive_screen.atlas_key_refs,
        );
        self.kitty_graphics_dirty = true;
        self.requeue_placed_kitty_images()
    }

    /// Whether the frontend texture for `image_id` holds the pixels of
    /// the active screen's stored image.
    pub fn is_kitty_uploaded(&self, image_id: u32) -> bool {
        match self.kitty_images.get(&image_id) {
            Some(stored) => {
                self.kitty_texture_contents.get(&image_id)
                    == Some(&stored.transmission_time)
            }
            None => false,
        }
    }

    /// Queue the stored pixels of `image_id` for the GPU unless the
    /// texture already holds them. Returns whether an upload was queued.
    pub fn queue_kitty_upload(&mut self, image_id: u32) -> bool {
        if self.is_kitty_uploaded(image_id) {
            return false;
        }
        let Some(stored) = self.kitty_images.get(&image_id) else {
            return false;
        };
        self.kitty_texture_contents
            .insert(image_id, stored.transmission_time);
        // Only the latest generation can be uploaded. Older queued versions
        // otherwise retain pixels after the image store replaces them.
        self.pending_images.retain(|(id, _)| *id != image_id);
        self.pending_images.push((image_id, stored.data.clone()));
        true
    }

    /// Resend every placed image on the active screen whose texture no
    /// longer holds its pixels. Returns whether anything was queued.
    fn requeue_placed_kitty_images(&mut self) -> bool {
        let mut stale: Vec<u32> = self
            .kitty_placements
            .keys()
            .chain(self.kitty_virtual_placements.keys())
            .map(|(img, _)| *img)
            .filter(|img| !self.is_kitty_uploaded(*img))
            .collect();
        stale.sort_unstable();
        stale.dedup();
        let mut queued = false;
        for img in stale {
            queued |= self.queue_kitty_upload(img);
        }
        queued
    }

    /// Clear all kitty graphics state on both screens. Used by full reset.
    pub fn clear_all_kitty_state(&mut self) {
        let image_bytes: usize = self
            .kitty_images
            .values()
            .chain(self.kitty_inactive_screen.kitty_images.values())
            .map(|s| s.data.pixels.len())
            .sum::<usize>()
            + self
                .kitty_animations
                .values()
                .chain(self.kitty_inactive_screen.kitty_animations.values())
                .map(Animation::extra_bytes)
                .sum::<usize>();
        self.total_bytes = self.total_bytes.saturating_sub(image_bytes);

        self.kitty_images.clear();
        self.kitty_animations.clear();
        self.kitty_chunking_state = Default::default();
        self.next_internal_placement_id = 0;
        self.kitty_image_numbers.clear();
        self.kitty_placements.clear();
        self.kitty_virtual_placements.clear();
        {
            let mut removals = self.texture_operations.lock();
            for id in self.kitty_texture_contents.keys() {
                removals.push(rio_graphics::kitty_image_key(*id));
            }
        }
        self.kitty_texture_contents.clear();
        self.pending_images.clear();

        // Sixel/iTerm2 placements die with the reset too; queue every
        // referenced key (both screens) so the frontend frees the
        // pixel store and GPU textures.
        {
            let mut removals = self.texture_operations.lock();
            for key in self.atlas_key_refs.keys() {
                removals.push(*key);
            }
            for key in self.kitty_inactive_screen.atlas_key_refs.keys() {
                removals.push(*key);
            }
        }
        self.atlas_placements.clear();
        self.atlas_key_refs.clear();

        // Give the atlas bytes back to the budget with the textures —
        // except graphics still waiting in `pending`, whose bytes the
        // eviction scan can still see and free itself.
        let pending_ids: std::collections::HashSet<GraphicId> =
            self.pending.iter().map(|g| g.id).collect();
        let released: Vec<GraphicId> = self
            .graphic_sizes
            .keys()
            .filter(|id| !pending_ids.contains(id))
            .copied()
            .collect();
        for id in released {
            if let Some(bytes) = self.graphic_sizes.remove(&id) {
                self.image_timestamps.remove(&id);
                self.total_bytes = self.total_bytes.saturating_sub(bytes);
            }
        }

        self.kitty_inactive_screen = KittyScreenState::default();
        self.kitty_graphics_dirty = true;
    }

    /// Store a kitty graphics image for later placement.
    /// Evicts old images if over memory limit.
    /// Returns false if admission fails, preserving the image being replaced.
    pub fn store_kitty_image(
        &mut self,
        image_id: u32,
        image_number: Option<u32>,
        mut data: GraphicData,
    ) -> bool {
        let new_bytes = data.pixels.len();
        let old_bytes = self
            .kitty_images
            .get(&image_id)
            .map_or(0, |old| old.data.pixels.len())
            + self
                .kitty_animations
                .get(&image_id)
                .map_or(0, Animation::extra_bytes);
        if new_bytes > self.total_limit
            || (!self.kitty_images.contains_key(&image_id)
                && self.kitty_images.len()
                    + self.kitty_inactive_screen.kitty_images.len()
                    >= 4096)
        {
            return false;
        }
        // transmission_time doubles as the texture generation, so two
        // stores of one id must never share it (web_time only has
        // millisecond resolution).
        let mut now = crate::time::Instant::now();
        let previous = self
            .kitty_images
            .get(&image_id)
            .map(|old| old.transmission_time)
            .into_iter()
            .chain(
                self.kitty_inactive_screen
                    .kitty_images
                    .get(&image_id)
                    .map(|old| old.transmission_time),
            )
            .chain(self.kitty_texture_contents.get(&image_id).copied())
            .max();
        if let Some(previous) = previous {
            if now <= previous {
                now = previous + crate::time::Duration::from_nanos(1);
            }
        }
        data.transmit_time = now;

        // Evict before storing to protect images with active placements
        let additional = new_bytes.saturating_sub(old_bytes);
        if self.total_bytes.saturating_add(additional) > self.total_limit {
            // Collect active IDs — images with placements are protected
            let mut active = std::collections::HashSet::new();
            for placement in self.kitty_placements.values() {
                active.insert(placement.image_id as u64);
            }
            for (img, _) in self.kitty_virtual_placements.keys() {
                active.insert(*img as u64);
            }
            // Also protect the image we're about to store
            active.insert(image_id as u64);
            if !self.evict_images_except(additional, &active, Some(image_id)) {
                return false;
            }
        }

        // If replacing an existing image, subtract its bytes first
        if let Some(old_time) = self
            .kitty_images
            .get(&image_id)
            .map(|old| old.transmission_time)
        {
            self.total_bytes = self.total_bytes.saturating_sub(old_bytes);
            self.kitty_placements.retain(|_, p| p.image_id != image_id);
            self.kitty_virtual_placements
                .retain(|(id, _), _| *id != image_id);
            self.remove_orphaned_placements();
            self.release_kitty_texture(image_id, old_time);
        }
        self.kitty_animations.remove(&image_id);

        self.pending_images.retain(|(id, _)| *id != image_id);
        self.next_kitty_image_generation =
            self.next_kitty_image_generation.wrapping_add(1);
        self.kitty_images.insert(
            image_id,
            StoredImage {
                image_generation: self.next_kitty_image_generation,
                data,
                transmission_time: now,
            },
        );
        self.total_bytes += new_bytes;
        self.kitty_graphics_dirty = true;

        // Update image number mapping if provided
        if let Some(number) = image_number {
            self.kitty_image_numbers.insert(number, image_id);
        }
        true
    }

    /// Get a stored kitty graphics image by ID
    pub fn get_kitty_image(&self, image_id: u32) -> Option<&StoredImage> {
        self.kitty_images.get(&image_id)
    }

    /// Get a stored kitty graphics image by number (I= parameter)
    /// Returns the most recently transmitted image with that number
    pub fn get_kitty_image_by_number(&self, image_number: u32) -> Option<&StoredImage> {
        self.kitty_image_numbers
            .get(&image_number)
            .and_then(|id| self.kitty_images.get(id))
    }

    /// Delete kitty graphics images
    pub fn delete_kitty_images(
        &mut self,
        predicate: impl Fn(&u32, &StoredImage) -> bool,
    ) {
        let mut released = Vec::new();
        self.kitty_images.retain(|id, img| {
            let delete = predicate(id, img);
            if delete {
                released.push((*id, img.transmission_time, img.data.pixels.len()));
            }
            !delete
        });
        if !released.is_empty() {
            self.kitty_graphics_dirty = true;
        }
        for (id, transmission_time, bytes) in released {
            let frames = self
                .kitty_animations
                .remove(&id)
                .map_or(0, |animation| animation.extra_bytes());
            self.total_bytes = self
                .total_bytes
                .saturating_sub(bytes.saturating_add(frames));
            self.release_kitty_texture(id, transmission_time);
        }
        // Clean up stale number mappings and placements of the images
        // that are gone.
        self.kitty_image_numbers
            .retain(|_, id| self.kitty_images.contains_key(id));
        self.kitty_placements
            .retain(|_, p| self.kitty_images.contains_key(&p.image_id));
        self.kitty_virtual_placements
            .retain(|(img, _), _| self.kitty_images.contains_key(img));
    }

    /// Free the frontend texture for `image_id` if it holds the pixels
    /// of the store stamped `transmission_time`; a texture holding the
    /// other screen's image of the same id stays. Returns whether a
    /// removal was queued.
    fn release_kitty_texture(
        &mut self,
        image_id: u32,
        transmission_time: crate::time::Instant,
    ) -> bool {
        if self.kitty_texture_contents.get(&image_id) != Some(&transmission_time) {
            return false;
        }
        self.kitty_texture_contents.remove(&image_id);
        // An upload of these pixels still waiting in the queue would
        // land after the removal.
        self.pending_images.retain(|(img, _)| *img != image_id);
        self.texture_operations
            .lock()
            .push(rio_graphics::kitty_image_key(image_id));
        true
    }

    /// Calculate the memory size of a graphic in bytes
    fn calculate_graphic_bytes(graphic: &GraphicData) -> usize {
        graphic.pixels.len()
    }

    /// Evict images to make space for required_bytes.
    /// Returns true if enough space was freed, false otherwise.
    ///
    /// Eviction priority (per kitty spec, extended for per-screen state):
    /// 1. Inactive-screen images (the user is not looking at them)
    /// 2. Active-screen unused images (no live placement)
    /// 3. Active-screen used images (visible — last resort)
    ///
    /// Within each tier we evict oldest by timestamp first.
    pub fn evict_images(
        &mut self,
        required_bytes: usize,
        used_ids: &std::collections::HashSet<u64>,
    ) -> bool {
        self.evict_images_except(required_bytes, used_ids, None)
    }

    fn evict_images_except(
        &mut self,
        required_bytes: usize,
        used_ids: &std::collections::HashSet<u64>,
        replacing: Option<u32>,
    ) -> bool {
        use tracing::debug;

        if self.total_bytes.saturating_add(required_bytes) <= self.total_limit {
            return true; // No eviction needed
        }

        let bytes_to_free = self
            .total_bytes
            .saturating_add(required_bytes)
            .saturating_sub(self.total_limit);
        debug!("Graphics memory: need to evict {} bytes (current: {}, limit: {}, required: {})",
            bytes_to_free, self.total_bytes, self.total_limit, required_bytes);

        /// Where an eviction candidate lives, so removal touches the
        /// right map. `Pending` is a sixel/iTerm2 atlas graphic in
        /// `self.pending`; the kitty variants are screen-scoped.
        #[derive(Copy, Clone, PartialEq, Eq)]
        enum CandidateSource {
            Pending,
            ActiveKitty,
            InactiveKitty,
        }

        // Tier scale: lower number = evict first
        // 0 = inactive kitty, 1 = active unused, 2 = active used (pending or kitty)
        fn tier_for(source: CandidateSource, is_used: bool) -> u8 {
            match source {
                CandidateSource::InactiveKitty => 0,
                CandidateSource::Pending | CandidateSource::ActiveKitty => {
                    if is_used {
                        2
                    } else {
                        1
                    }
                }
            }
        }

        // Candidate: (sentinel GraphicId, timestamp, is_used, bytes, source)
        let mut candidates: Vec<(
            GraphicId,
            crate::time::Instant,
            bool,
            usize,
            CandidateSource,
        )> = Vec::new();

        // Check pending graphics (sixel/iTerm2 — atlas based, single screen)
        for graphic in &self.pending {
            if let Some(&timestamp) = self.image_timestamps.get(&graphic.id) {
                let is_used = used_ids.contains(&graphic.id.get());
                // A used pending graphic has grid cells referencing it
                // while its pixels haven't reached the renderer yet;
                // evicting it here would blank those cells permanently
                // (nothing re-triggers the upload). Prefer briefly
                // exceeding the byte budget over losing the image.
                if is_used {
                    continue;
                }
                let bytes = Self::calculate_graphic_bytes(graphic);
                candidates.push((
                    graphic.id,
                    timestamp,
                    is_used,
                    bytes,
                    CandidateSource::Pending,
                ));
            }
        }

        // Check active-screen kitty images
        for (&kitty_id, stored) in &self.kitty_images {
            // Replacement accounting already credits this allocation. Evicting
            // it here would count the same bytes twice and lose its placements.
            if replacing == Some(kitty_id) {
                continue;
            }
            let id_as_u64 = kitty_id as u64;
            let is_used = used_ids.contains(&id_as_u64);
            let bytes = Self::calculate_graphic_bytes(&stored.data)
                + self
                    .kitty_animations
                    .get(&kitty_id)
                    .map_or(0, Animation::extra_bytes);
            candidates.push((
                GraphicId::new(id_as_u64),
                stored.transmission_time,
                is_used,
                bytes,
                CandidateSource::ActiveKitty,
            ));
        }

        // Check inactive-screen kitty images. These are not visible to
        // the user, so they're the first tier to evict regardless of
        // whether they have a placement.
        for (&kitty_id, stored) in &self.kitty_inactive_screen.kitty_images {
            let id_as_u64 = kitty_id as u64;
            let bytes = Self::calculate_graphic_bytes(&stored.data)
                + self
                    .kitty_inactive_screen
                    .kitty_animations
                    .get(&kitty_id)
                    .map_or(0, Animation::extra_bytes);
            candidates.push((
                GraphicId::new(id_as_u64),
                stored.transmission_time,
                false, // not displayed (we're on the other screen)
                bytes,
                CandidateSource::InactiveKitty,
            ));
        }

        if candidates.is_empty() {
            debug!("No candidates for eviction");
            return false;
        }
        if candidates
            .iter()
            .fold(0usize, |n, candidate| n.saturating_add(candidate.3))
            < bytes_to_free
        {
            return false;
        }

        // Sort by tier (ascending), then oldest first within tier.
        candidates.sort_by(|a, b| {
            let ta = tier_for(a.4, a.2);
            let tb = tier_for(b.4, b.2);
            ta.cmp(&tb).then_with(|| a.1.cmp(&b.1))
        });

        let mut freed_bytes = 0usize;
        let mut evicted: Vec<(GraphicId, CandidateSource)> = Vec::new();

        for (graphic_id, _, is_used, bytes, source) in candidates {
            if freed_bytes >= bytes_to_free {
                break;
            }

            evicted.push((graphic_id, source));
            freed_bytes += bytes;

            debug!(
                "Evicting graphic id={}, bytes={}, used={}",
                graphic_id.get(),
                bytes,
                is_used
            );
        }

        // Actually remove the evicted graphics from the right home.
        for (id, source) in evicted {
            let evicted_u32 = id.get() as u32;
            let removed_kitty = match source {
                CandidateSource::Pending => {
                    self.pending.retain(|g| g.id != id);
                    // Its bytes join `freed_bytes` below; drop the size
                    // entry so a later placement release cannot subtract
                    // them a second time.
                    self.graphic_sizes.remove(&id);
                    None
                }
                CandidateSource::ActiveKitty => {
                    self.kitty_animations.remove(&evicted_u32);
                    self.kitty_image_numbers.retain(|_, v| *v != evicted_u32);
                    self.kitty_graphics_dirty = true;
                    self.kitty_images.remove(&evicted_u32)
                }
                CandidateSource::InactiveKitty => {
                    self.kitty_inactive_screen
                        .kitty_animations
                        .remove(&evicted_u32);
                    self.kitty_inactive_screen
                        .kitty_image_numbers
                        .retain(|_, v| *v != evicted_u32);
                    self.kitty_inactive_screen.kitty_images.remove(&evicted_u32)
                }
            };

            // Remove timestamp (only used for pending atlas graphics)
            self.image_timestamps.remove(&id);

            // Add to removal queue so GPU textures get cleaned up.
            // The key namespace depends on where the image lived:
            // kitty ids map verbatim, atlas graphics live above 2^32.
            // A kitty texture is only freed when it holds the evicted
            // pixels; otherwise it belongs to the other screen's image
            // of the same id and stays.
            match source {
                CandidateSource::Pending => {
                    self.texture_operations
                        .lock()
                        .push(rio_graphics::atlas_image_key(id.get()));
                }
                CandidateSource::ActiveKitty | CandidateSource::InactiveKitty => {
                    if let Some(img) = removed_kitty {
                        self.release_kitty_texture(evicted_u32, img.transmission_time);
                    }
                }
            }
        }

        // Sweep dangling placements on both screens. A placement is
        // dangling if its referenced image_id is no longer in the
        // matching screen's image cache. This catches both:
        //   - kitty placements whose image was just evicted
        //   - cross-namespace coincidences where a sixel/iTerm2 atlas
        //     graphic with the same numeric id as a kitty image is
        //     evicted (the test_eviction_removes_dangling_placements
        //     test pins this defensive behaviour)
        let active_ids: std::collections::HashSet<u32> =
            self.kitty_images.keys().copied().collect();
        self.kitty_placements
            .retain(|_, p| active_ids.contains(&p.image_id));
        self.kitty_virtual_placements
            .retain(|(img, _), _| active_ids.contains(img));
        let inactive_ids: std::collections::HashSet<u32> = self
            .kitty_inactive_screen
            .kitty_images
            .keys()
            .copied()
            .collect();
        self.kitty_inactive_screen
            .kitty_placements
            .retain(|_, p| inactive_ids.contains(&p.image_id));
        self.kitty_inactive_screen
            .kitty_virtual_placements
            .retain(|(img, _), _| inactive_ids.contains(img));

        // Update total_bytes
        self.total_bytes = self.total_bytes.saturating_sub(freed_bytes);

        // Evicting an image whose texture the active screen still draws
        // from (its own copy, freed above) needs the pixels back.
        self.requeue_placed_kitty_images();

        debug!(
            "Evicted {} bytes, new total: {}",
            freed_bytes, self.total_bytes
        );
        freed_bytes >= bytes_to_free
    }

    /// Collect IDs of graphics still displayed on the grid or as
    /// overlays. O(number of placements).
    pub fn collect_active_graphic_ids(&mut self) -> std::collections::HashSet<u64> {
        let mut active = std::collections::HashSet::new();
        // Sixel/iTerm2 liveness: placements are the single owners.
        for placement in &self.atlas_placements {
            // Atlas keys live above 2^32; recover the GraphicId part.
            active.insert(placement.image_key - (1u64 << 32));
        }
        // Overlay-based (kitty) liveness — use image_id directly
        for placement in self.kitty_placements.values() {
            active.insert(placement.image_id as u64);
        }
        for (img, _) in self.kitty_virtual_placements.keys() {
            active.insert(*img as u64);
        }
        active
    }

    /// Track a new graphic's memory usage and timestamp
    pub fn track_graphic(&mut self, graphic_id: GraphicId, bytes: usize) {
        self.image_timestamps
            .insert(graphic_id, crate::time::Instant::now());
        self.graphic_sizes.insert(graphic_id, bytes);
        self.total_bytes += bytes;
        debug!(
            "Tracked graphic id={}, bytes={}, total_bytes={}",
            graphic_id.0, bytes, self.total_bytes
        );
    }

    /// Update total_bytes when a graphic is removed
    pub fn untrack_graphic(&mut self, graphic_id: GraphicId, bytes: usize) {
        self.image_timestamps.remove(&graphic_id);
        self.graphic_sizes.remove(&graphic_id);
        self.total_bytes = self.total_bytes.saturating_sub(bytes);
        debug!(
            "Untracked graphic id={}, bytes={}, total_bytes={}",
            graphic_id.0, bytes, self.total_bytes
        );
    }
}

#[test]
fn image_budget_rejects_oversize_replacements_and_coalesces_uploads() {
    let mut graphics = Graphics {
        total_limit: 16,
        ..Default::default()
    };
    let image = |size, value| GraphicData {
        id: GraphicId::new(1),
        width: 1,
        height: 1,
        color_type: rio_graphics::ColorType::Rgba,
        pixels: vec![value; size],
        is_opaque: true,
        resize: None,
        display_width: None,
        display_height: None,
        transmit_time: crate::time::Instant::now(),
    };
    assert!(graphics.store_kitty_image(1, None, image(4, 1)));
    assert!(!graphics.store_kitty_image(1, None, image(17, 2)));
    assert_eq!(graphics.total_bytes, 4);
    assert_eq!(graphics.kitty_images[&1].data.pixels[0], 1);
    for value in 2..100 {
        assert!(graphics.store_kitty_image(1, None, image(4, value)));
        graphics.queue_kitty_upload(1);
        assert_eq!(graphics.pending_images.len(), 1);
    }
    assert_eq!(graphics.pending_images[0].1.pixels[0], 99);
    assert_eq!(graphics.total_bytes, 4);
}

#[test]
fn replacement_budget_does_not_count_the_replaced_image_as_an_eviction() {
    let mut graphics = Graphics {
        total_limit: 12,
        ..Default::default()
    };
    let image = |id, bytes| GraphicData {
        id: GraphicId::new(id),
        width: 1,
        height: 1,
        color_type: rio_graphics::ColorType::Rgba,
        pixels: vec![0; bytes],
        is_opaque: true,
        resize: None,
        display_width: None,
        display_height: None,
        transmit_time: crate::time::Instant::now(),
    };
    assert!(graphics.store_kitty_image(1, None, image(1, 4)));
    assert!(graphics.store_kitty_image(2, None, image(2, 8)));
    for id in [1, 2] {
        graphics.kitty_virtual_placements.insert(
            (id, 1),
            VirtualPlacement {
                image_id: id,
                placement_id: 1,
                columns: 1,
                rows: 1,
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            },
        );
    }
    assert!(graphics.store_kitty_image(1, None, image(1, 8)));
    assert!(graphics.total_bytes <= graphics.total_limit);
    assert!(!graphics.kitty_virtual_placements.contains_key(&(1, 1)));
    assert_eq!(graphics.kitty_images[&1].data.pixels.len(), 8);
}

#[test]
fn placement_budget_counts_both_screens_and_allows_replacements() {
    let mut graphics = Graphics::default();
    for id in 0..4096 {
        graphics
            .kitty_inactive_screen
            .kitty_virtual_placements
            .insert(
                (1, id),
                VirtualPlacement {
                    image_id: 1,
                    placement_id: id,
                    columns: 1,
                    rows: 1,
                    x: 0,
                    y: 0,
                    width: 0,
                    height: 0,
                },
            );
    }
    assert!(!graphics.can_place_kitty(1, 5000, true));
    graphics.swap_kitty_screen_state();
    assert!(graphics.can_place_kitty(1, 5, true));
    assert!(!graphics.can_place_kitty(1, 0, false));
}

#[test]
fn atlas_fragment_pressure_releases_the_oldest_image_and_its_budget() {
    let mut graphics = Graphics::default();
    let old_key = rio_graphics::atlas_image_key(1);
    let new_key = rio_graphics::atlas_image_key(2);
    let placement = AtlasPlacement {
        image_key: old_key,
        abs_row: 0,
        col: 0,
        columns: 1,
        rows: 1,
        src_x: 0,
        src_y: 0,
        src_width: 1,
        src_height: 1,
        total_width: 1,
        total_height: 1,
        insert_cell_w: 1,
        insert_cell_h: 1,
    };
    graphics.atlas_placements = vec![placement.clone(); 4096];
    graphics.atlas_placements.push(AtlasPlacement {
        image_key: new_key,
        ..placement
    });
    graphics.atlas_key_refs.insert(old_key, 4096);
    graphics.atlas_key_refs.insert(new_key, 1);
    graphics.track_graphic(GraphicId::new(1), 400);
    graphics.track_graphic(GraphicId::new(2), 400);
    graphics.recount_atlas_keys();
    assert_eq!(graphics.atlas_placements.len(), 1);
    assert_eq!(graphics.atlas_placements[0].image_key, new_key);
    assert_eq!(graphics.total_bytes, 400);
    assert!(graphics.texture_operations.lock().contains(&old_key));
}

/// A placed atlas graphic's pixels leave via `take_queues`; when its
/// last placement dies, the recount must give its bytes back to the
/// budget — before this, every sixel/iTerm2 image counted against the
/// 320MB kitty budget forever and starved kitty images into eviction.
#[test]
fn atlas_key_release_returns_bytes_to_the_budget() {
    let mut graphics = Graphics::default();
    let key = rio_graphics::atlas_image_key(1);

    graphics.track_graphic(GraphicId::new(1), 5_000);
    graphics.atlas_key_refs.insert(key, 1);
    assert_eq!(graphics.total_bytes, 5_000);

    // The placement vec is the source of truth; it is empty, so the
    // recount releases the key.
    graphics.recount_atlas_keys();
    assert_eq!(graphics.total_bytes, 0, "released key must free its bytes");
    assert!(graphics.graphic_sizes.is_empty());
    assert!(graphics.image_timestamps.is_empty());
    assert_eq!(
        graphics.texture_operations.lock().as_slice(),
        &[key],
        "the texture removal must still be queued"
    );
}

/// A key still referenced by the inactive screen keeps its bytes.
#[test]
fn atlas_key_release_respects_the_other_screen() {
    let mut graphics = Graphics::default();
    let key = rio_graphics::atlas_image_key(1);

    graphics.track_graphic(GraphicId::new(1), 5_000);
    graphics.atlas_key_refs.insert(key, 1);
    graphics.kitty_inactive_screen.atlas_key_refs.insert(key, 1);

    graphics.recount_atlas_keys();
    assert_eq!(
        graphics.total_bytes, 5_000,
        "bytes stay while the inactive screen references the key"
    );

    graphics.kitty_inactive_screen.atlas_key_refs.clear();
    graphics.untrack_atlas_keys(&[key]);
    assert_eq!(graphics.total_bytes, 0);
}

#[test]
fn check_opaque_region() {
    use rio_graphics::ColorType;
    let graphic = GraphicData {
        id: GraphicId::new(1),
        width: 10,
        height: 10,
        color_type: ColorType::Rgb,
        pixels: vec![255; 10 * 10 * 3],
        is_opaque: true,
        resize: None,
        display_width: None,
        display_height: None,
        transmit_time: crate::time::Instant::now(),
    };

    assert!(graphic.is_filled(1, 1, 3, 3));
    assert!(!graphic.is_filled(8, 8, 10, 10));

    let pixels = {
        // Put a transparent 3x3 box inside the picture.
        let mut data = vec![255; 10 * 10 * 4];
        for y in 3..6 {
            let offset = y * 10 * 4;
            data[offset..offset + 3 * 4].fill(0);
        }
        data
    };

    let graphic = GraphicData {
        id: GraphicId::new(1),
        pixels,
        width: 10,
        height: 10,
        color_type: ColorType::Rgba,
        is_opaque: false,
        resize: None,
        display_width: None,
        display_height: None,
        transmit_time: crate::time::Instant::now(),
    };

    assert!(graphic.is_filled(0, 0, 3, 3));
    assert!(!graphic.is_filled(1, 1, 4, 4));
}

#[test]
fn test_graphics_memory_tracking() {
    use rio_graphics::ColorType;
    let mut graphics = Graphics::default();

    // Create a small graphic (100x100 RGBA = 40,000 bytes)
    let pixels = vec![255u8; 100 * 100 * 4];
    let graphic = GraphicData {
        id: GraphicId::new(1),
        width: 100,
        height: 100,
        color_type: ColorType::Rgba,
        pixels,
        is_opaque: true,
        resize: None,
        display_width: None,
        display_height: None,
        transmit_time: crate::time::Instant::now(),
    };

    let bytes = Graphics::calculate_graphic_bytes(&graphic);
    assert_eq!(bytes, 40_000);

    // Track the graphic
    graphics.track_graphic(GraphicId::new(1), bytes);
    assert_eq!(graphics.total_bytes, 40_000);
    assert!(graphics.image_timestamps.contains_key(&GraphicId::new(1)));

    // Untrack the graphic
    graphics.untrack_graphic(GraphicId::new(1), bytes);
    assert_eq!(graphics.total_bytes, 0);
    assert!(!graphics.image_timestamps.contains_key(&GraphicId::new(1)));
}

#[test]
fn test_graphics_eviction_unused_first() {
    use rio_graphics::ColorType;
    let mut graphics = Graphics {
        total_limit: 100_000, // 100KB limit for testing
        ..Graphics::default()
    };

    // Add 3 graphics (50KB each = 150KB total, will exceed limit)
    let mut used_ids = std::collections::HashSet::new();

    // Graphic 1: 50KB, used
    let pixels1 = vec![255u8; 50_000];
    let graphic1 = GraphicData {
        id: GraphicId::new(1),
        width: 100,
        height: 125,
        color_type: ColorType::Rgba,
        pixels: pixels1.clone(),
        is_opaque: true,
        resize: None,
        display_width: None,
        display_height: None,
        transmit_time: crate::time::Instant::now(),
    };
    graphics.pending.push(graphic1);
    graphics.track_graphic(GraphicId::new(1), pixels1.len());
    used_ids.insert(1); // Mark as used

    std::thread::sleep(std::time::Duration::from_millis(10));

    // Graphic 2: 50KB, unused (should be evicted first)
    let pixels2 = vec![255u8; 50_000];
    let graphic2 = GraphicData {
        id: GraphicId::new(2),
        width: 100,
        height: 125,
        color_type: ColorType::Rgba,
        pixels: pixels2.clone(),
        is_opaque: true,
        resize: None,
        display_width: None,
        display_height: None,
        transmit_time: crate::time::Instant::now(),
    };
    graphics.pending.push(graphic2);
    graphics.track_graphic(GraphicId::new(2), pixels2.len());
    // Not marked as used

    // Try to add Graphic 3 (will trigger eviction)
    let pixels3_len = 50_000;
    let success = graphics.evict_images(pixels3_len, &used_ids);

    assert!(success, "Eviction should succeed");
    // Graphic 2 (unused) should be evicted, Graphic 1 (used) should remain
    assert_eq!(graphics.pending.len(), 1);
    assert_eq!(graphics.pending[0].id, GraphicId::new(1));
    assert!(graphics.image_timestamps.contains_key(&GraphicId::new(1)));
    assert!(!graphics.image_timestamps.contains_key(&GraphicId::new(2)));
}

#[test]
fn test_graphics_eviction_oldest_first() {
    use rio_graphics::ColorType;
    let mut graphics = Graphics {
        total_limit: 100_000, // 100KB limit
        ..Graphics::default()
    };

    let used_ids = std::collections::HashSet::new(); // No images used

    // Add 3 graphics, all unused
    // Graphic 1: oldest
    let pixels1 = vec![255u8; 50_000];
    let graphic1 = GraphicData {
        id: GraphicId::new(1),
        width: 100,
        height: 125,
        color_type: ColorType::Rgba,
        pixels: pixels1.clone(),
        is_opaque: true,
        resize: None,
        display_width: None,
        display_height: None,
        transmit_time: crate::time::Instant::now(),
    };
    graphics.pending.push(graphic1);
    graphics.track_graphic(GraphicId::new(1), pixels1.len());

    std::thread::sleep(std::time::Duration::from_millis(10));

    // Graphic 2: middle
    let pixels2 = vec![255u8; 50_000];
    let graphic2 = GraphicData {
        id: GraphicId::new(2),
        width: 100,
        height: 125,
        color_type: ColorType::Rgba,
        pixels: pixels2.clone(),
        is_opaque: true,
        resize: None,
        display_width: None,
        display_height: None,
        transmit_time: crate::time::Instant::now(),
    };
    graphics.pending.push(graphic2);
    graphics.track_graphic(GraphicId::new(2), pixels2.len());

    // Try to add Graphic 3 (will trigger eviction, oldest should go first)
    let pixels3_len = 50_000;
    let success = graphics.evict_images(pixels3_len, &used_ids);

    assert!(success);
    // Graphic 1 (oldest) should be evicted
    assert_eq!(graphics.pending.len(), 1);
    assert_eq!(graphics.pending[0].id, GraphicId::new(2));
}

#[test]
fn test_graphics_eviction_fails_when_not_enough_space() {
    use rio_graphics::ColorType;
    let mut graphics = Graphics {
        total_limit: 100_000, // 100KB limit
        ..Graphics::default()
    };

    let mut used_ids = std::collections::HashSet::new();

    // Add one 90KB graphic that's in use
    let pixels1 = vec![255u8; 90_000];
    let graphic1 = GraphicData {
        id: GraphicId::new(1),
        width: 150,
        height: 150,
        color_type: ColorType::Rgba,
        pixels: pixels1.clone(),
        is_opaque: true,
        resize: None,
        display_width: None,
        display_height: None,
        transmit_time: crate::time::Instant::now(),
    };
    graphics.pending.push(graphic1);
    graphics.track_graphic(GraphicId::new(1), pixels1.len());
    used_ids.insert(1); // Mark as used

    // Try to add another 90KB (total would be 180KB, exceeds limit).
    // A used pending graphic must NOT be evicted: its cells reference
    // pixels that haven't reached the renderer yet, so eviction would
    // blank them permanently. The byte budget is soft here.
    let pixels2_len = 90_000;
    let success = graphics.evict_images(pixels2_len, &used_ids);

    assert!(
        !success,
        "no evictable candidates: the pending image is in use"
    );
    assert_eq!(graphics.pending.len(), 1, "used pending image survives");
}

#[test]
fn test_graphics_no_eviction_when_under_limit() {
    use rio_graphics::ColorType;
    let mut graphics = Graphics {
        total_limit: 200_000, // 200KB limit
        ..Graphics::default()
    };

    let used_ids = std::collections::HashSet::new();

    // Add one 50KB graphic
    let pixels1 = vec![255u8; 50_000];
    let graphic1 = GraphicData {
        id: GraphicId::new(1),
        width: 100,
        height: 125,
        color_type: ColorType::Rgba,
        pixels: pixels1.clone(),
        is_opaque: true,
        resize: None,
        display_width: None,
        display_height: None,
        transmit_time: crate::time::Instant::now(),
    };
    graphics.pending.push(graphic1);
    graphics.track_graphic(GraphicId::new(1), pixels1.len());

    // Try to add another 50KB (total 100KB, well under limit)
    let pixels2_len = 50_000;
    let success = graphics.evict_images(pixels2_len, &used_ids);

    assert!(success);
    // No eviction should occur
    assert_eq!(graphics.pending.len(), 1);
    assert_eq!(graphics.total_bytes, 50_000);
}

#[test]
fn test_kitty_eviction_protects_virtual_placements_and_sweeps_dangling() {
    use rio_graphics::ColorType;
    let mut graphics = Graphics {
        total_limit: 100_000,
        ..Graphics::default()
    };
    let image = |id: u32| GraphicData {
        id: GraphicId::new(id as u64),
        width: 100,
        height: 100,
        color_type: ColorType::Rgba,
        pixels: vec![0u8; 40_000],
        is_opaque: true,
        resize: None,
        display_width: None,
        display_height: None,
        transmit_time: crate::time::Instant::now(),
    };
    let vp = |id: u32| VirtualPlacement {
        image_id: id,
        placement_id: 1,
        columns: 2,
        rows: 2,
        x: 0,
        y: 0,
        width: 0,
        height: 0,
    };

    graphics.store_kitty_image(1, None, image(1));
    graphics.kitty_virtual_placements.insert((1, 1), vp(1));
    std::thread::sleep(std::time::Duration::from_millis(5));
    graphics.store_kitty_image(2, None, image(2));
    graphics.kitty_virtual_placements.insert((2, 1), vp(2));
    graphics
        .kitty_inactive_screen
        .kitty_virtual_placements
        .insert((7, 1), vp(7));
    std::thread::sleep(std::time::Duration::from_millis(5));

    // Image 1 is older but virtually placed; image 2 lost its placement
    // and goes first. The inactive screen's placement of a missing
    // image is swept as dangling.
    graphics.kitty_virtual_placements.remove(&(2, 1));
    graphics.store_kitty_image(3, None, image(3));
    assert!(graphics.kitty_images.contains_key(&1));
    assert!(!graphics.kitty_images.contains_key(&2));
    assert!(graphics.kitty_images.contains_key(&3));
    assert!(graphics.kitty_virtual_placements.contains_key(&(1, 1)));
    assert!(!graphics
        .kitty_inactive_screen
        .kitty_virtual_placements
        .contains_key(&(7, 1)));
}

#[test]
fn test_kitty_texture_contents_tracks_uploads_across_screens() {
    use rio_graphics::ColorType;
    let mut graphics = Graphics {
        total_limit: 8_000,
        ..Graphics::default()
    };
    let image = |id: u32, side: usize| GraphicData {
        id: GraphicId::new(id as u64),
        width: side,
        height: side,
        color_type: ColorType::Rgba,
        pixels: vec![0u8; side * side * 4],
        is_opaque: true,
        resize: None,
        display_width: None,
        display_height: None,
        transmit_time: crate::time::Instant::now(),
    };
    let vp = VirtualPlacement {
        image_id: 1,
        placement_id: 1,
        columns: 2,
        rows: 2,
        x: 0,
        y: 0,
        width: 0,
        height: 0,
    };

    // Unknown image: nothing to upload, nothing recorded.
    assert!(!graphics.queue_kitty_upload(1));
    assert!(!graphics.is_kitty_uploaded(1));

    graphics.store_kitty_image(1, None, image(1, 10));
    assert!(!graphics.is_kitty_uploaded(1));
    assert!(graphics.queue_kitty_upload(1));
    assert!(graphics.is_kitty_uploaded(1));
    assert!(!graphics.queue_kitty_upload(1));
    assert_eq!(graphics.pending_images.len(), 1);
    graphics.kitty_virtual_placements.insert((1, 1), vp.clone());

    // A retransmit changes the transmission time; the texture is stale.
    std::thread::sleep(std::time::Duration::from_millis(2));
    graphics.store_kitty_image(1, None, image(1, 20));
    assert!(!graphics.is_kitty_uploaded(1));
    assert!(graphics.queue_kitty_upload(1));
    assert!(!graphics.kitty_virtual_placements.contains_key(&(1, 1)));
    graphics.kitty_virtual_placements.insert((1, 1), vp.clone());
    graphics.texture_operations.lock().clear();

    // The other screen uploads its own image 1: the record follows the
    // texture, not the screen.
    assert!(!graphics.swap_kitty_screen_state());
    graphics.store_kitty_image(1, None, image(1, 30));
    assert!(graphics.queue_kitty_upload(1));
    graphics.pending_images.clear();

    // Coming back, the placed main-screen image is resent once.
    assert!(graphics.swap_kitty_screen_state());
    assert_eq!(graphics.pending_images.len(), 1);
    assert_eq!(graphics.pending_images[0].1.width, 20);
    assert!(graphics.is_kitty_uploaded(1));
    graphics.pending_images.clear();

    // Evicting the inactive same-id image leaves the texture alone: it
    // holds the active copy, so nothing is freed or resent.
    graphics.store_kitty_image(2, None, image(2, 40));
    assert!(graphics.kitty_inactive_screen.kitty_images.is_empty());
    assert!(graphics.is_kitty_uploaded(1));
    assert!(graphics.pending_images.is_empty());
    assert!(graphics.texture_operations.lock().is_empty());

    // The reverse: the inactive copy owns the texture and the active
    // placed image is stale. Evicting the inactive copy frees the
    // texture and the active image is resent right away.
    graphics.kitty_images.remove(&2);
    graphics.total_bytes = 1600;
    std::thread::sleep(std::time::Duration::from_millis(2));
    graphics.kitty_inactive_screen.kitty_images.insert(
        1,
        StoredImage {
            image_generation: 0,
            data: image(1, 30),
            transmission_time: crate::time::Instant::now(),
        },
    );
    graphics.total_bytes += 3600;
    let inactive_time = graphics.kitty_inactive_screen.kitty_images[&1].transmission_time;
    graphics.kitty_texture_contents.insert(1, inactive_time);
    assert!(!graphics.is_kitty_uploaded(1));
    graphics.store_kitty_image(2, None, image(2, 40));
    assert!(graphics.kitty_inactive_screen.kitty_images.is_empty());
    assert!(graphics
        .texture_operations
        .lock()
        .contains(&rio_graphics::kitty_image_key(1)));
    assert!(graphics.is_kitty_uploaded(1));
    assert_eq!(graphics.pending_images.len(), 1);
    assert_eq!(graphics.pending_images[0].0, 1);
    assert_eq!(graphics.pending_images[0].1.width, 20);

    // Full reset forgets the texture contents.
    graphics.clear_all_kitty_state();
    assert!(graphics.kitty_texture_contents.is_empty());
}

#[test]
fn test_kitty_store_never_repeats_transmission_time() {
    use rio_graphics::ColorType;
    let mut graphics = Graphics::default();
    let image = || GraphicData {
        id: GraphicId::new(1),
        width: 1,
        height: 1,
        color_type: ColorType::Rgba,
        pixels: vec![0u8; 4],
        is_opaque: true,
        resize: None,
        display_width: None,
        display_height: None,
        transmit_time: crate::time::Instant::now(),
    };
    graphics.store_kitty_image(1, None, image());
    let first = graphics.get_kitty_image(1).unwrap().transmission_time;
    assert!(graphics.queue_kitty_upload(1));
    graphics.store_kitty_image(1, None, image());
    let second = graphics.get_kitty_image(1).unwrap().transmission_time;
    assert!(second > first);
    assert!(!graphics.is_kitty_uploaded(1));
    assert_eq!(
        graphics.get_kitty_image(1).unwrap().data.transmit_time,
        second
    );
}

#[test]
fn test_kitty_eviction_cancels_pending_upload_of_evicted_image() {
    use rio_graphics::ColorType;
    let mut graphics = Graphics {
        total_limit: 8_000,
        ..Graphics::default()
    };
    let image = |id: u32, side: usize| GraphicData {
        id: GraphicId::new(id as u64),
        width: side,
        height: side,
        color_type: ColorType::Rgba,
        pixels: vec![0u8; side * side * 4],
        is_opaque: true,
        resize: None,
        display_width: None,
        display_height: None,
        transmit_time: crate::time::Instant::now(),
    };

    // Image 1 is uploaded but not placed, so it is the eviction
    // candidate when image 2 does not fit; both happen in one batch.
    graphics.store_kitty_image(1, None, image(1, 30));
    assert!(graphics.queue_kitty_upload(1));
    graphics.store_kitty_image(2, None, image(2, 40));
    assert!(graphics.get_kitty_image(1).is_none());
    assert!(graphics.pending_images.iter().all(|(img, _)| *img != 1));
    assert!(graphics
        .texture_operations
        .lock()
        .contains(&rio_graphics::kitty_image_key(1)));
    assert!(!graphics.kitty_texture_contents.contains_key(&1));
}

#[test]
fn test_kitty_delete_frees_texture_only_when_it_holds_the_pixels() {
    use rio_graphics::ColorType;
    let mut graphics = Graphics::default();
    let image = |id: u32| GraphicData {
        id: GraphicId::new(id as u64),
        width: 1,
        height: 1,
        color_type: ColorType::Rgba,
        pixels: vec![0u8; 4],
        is_opaque: true,
        resize: None,
        display_width: None,
        display_height: None,
        transmit_time: crate::time::Instant::now(),
    };
    graphics.store_kitty_image(1, None, image(1));
    assert!(graphics.queue_kitty_upload(1));
    graphics.store_kitty_image(2, None, image(2));

    // Image 2 was never uploaded: nothing to free.
    graphics.delete_kitty_images(|id, _| *id == 2);
    assert!(graphics.texture_operations.lock().is_empty());

    // The other screen's copy of id 1 owns the texture: deleting the
    // active copy leaves it alone.
    assert!(!graphics.swap_kitty_screen_state());
    graphics.store_kitty_image(1, None, image(1));
    assert!(graphics.queue_kitty_upload(1));
    assert!(!graphics.swap_kitty_screen_state());
    graphics.delete_kitty_images(|id, _| *id == 1);
    assert!(graphics.texture_operations.lock().is_empty());
    assert!(graphics.kitty_texture_contents.contains_key(&1));

    // Deleting the owning copy frees it and drops a pending resend.
    assert!(!graphics.swap_kitty_screen_state());
    graphics.pending_images.clear();
    graphics.delete_kitty_images(|id, _| *id == 1);
    assert!(graphics
        .texture_operations
        .lock()
        .contains(&rio_graphics::kitty_image_key(1)));
    assert!(!graphics.kitty_texture_contents.contains_key(&1));
    assert!(graphics.pending_images.is_empty());

    // A full reset frees whatever the record still names.
    graphics.store_kitty_image(3, None, image(3));
    assert!(graphics.queue_kitty_upload(3));
    graphics.texture_operations.lock().clear();
    graphics.clear_all_kitty_state();
    assert!(graphics
        .texture_operations
        .lock()
        .contains(&rio_graphics::kitty_image_key(3)));
    assert!(graphics.pending_images.is_empty());
    assert!(graphics.kitty_texture_contents.is_empty());
}
