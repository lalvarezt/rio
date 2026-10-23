// Copyright (c) 2023-present, Raphael Amorim.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

use ash::vk;
use rustc_hash::FxHashMap;
use std::ops::Range;
use std::sync::Arc;

use super::atlas::{AtlasSlot, GlyphKey, RasterizedGlyph};
use super::cell::{CellBg, CellText, GridUniforms};
use crate::context::vulkan::{
    allocate_host_visible_buffer_raw, VkShared, VulkanBuffer, VulkanContext, VulkanImage,
    FRAMES_IN_FLIGHT,
};
use crate::renderer::image_cache::atlas::AtlasAllocator;

// Compiled at build time by `sugarloaf/build.rs`. Source GLSL lives
// in `sugarloaf/src/grid/shaders/`; edit those, not the .spv.
const BG_VERT_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/grid_bg.vert.spv"));
const BG_FRAG_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/grid_bg.frag.spv"));
const TEXT_VERT_SPV: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/grid_text.vert.spv"));
const TEXT_FRAG_SPV: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/grid_text.frag.spv"));

/// Extra slots appended to `fg_rows` for cursor glyphs. Mirrors the
/// Metal layout so the CPU emit code is byte-identical.
const CURSOR_ROW_SLOTS: usize = 2;

/// Per-page atlas side. 2048² @ R8 = 4 MiB; @ RGBA8 = 16 MiB. Pages
/// are never resized at runtime; this is the side every page is
/// created at.
const ATLAS_PAGE_SIZE: u16 = 2048;

/// Cap on pages per kind. 16 pages × 2048² = 64 M pixels, matching the
/// effective area of an 8192² atlas while keeping each page small
/// enough to allocate on demand without ever touching an in-use image.
/// Memory is lazy — typical sessions use 1–2 pages.
const MAX_PAGES: usize = 16;

/// One pending glyph upload — `bytes` were copied at insert time, so
/// the rasterizer's buffer can be reused immediately. Drained by
/// `flush_uploads` on the next frame's command buffer.
struct PendingUpload {
    x: u16,
    y: u16,
    w: u16,
    h: u16,
    bytes: Vec<u8>,
}

/// One contiguous run of cells in the per-slot foreground vertex
/// buffer that all share the same `(atlas_kind, page)`. `render_text`
/// binds the matching page's descriptor set + a push constant for the
/// kind and issues one `cmd_draw` per bucket.
#[derive(Clone, Copy)]
struct TextBucket {
    kind: u8,
    page: u8,
    start: u32,
    count: u32,
}

fn same_atlas_layout(previous: &[CellText], updated: &[CellText]) -> bool {
    previous.len() == updated.len()
        && previous
            .iter()
            .zip(updated)
            .all(|(old, new)| (old.atlas, old.page) == (new.atlas, new.page))
}

/// Patch a row whose glyphs still belong to the same atlas buckets. Offsets
/// refer to the grouped staging layout, which can place adjacent cells far apart.
fn patch_mixed_row(
    staging: &mut [CellText],
    offsets: &[usize],
    previous: &[CellText],
    updated: &[CellText],
) -> Option<Range<usize>> {
    if offsets.len() != updated.len() || !same_atlas_layout(previous, updated) {
        return None;
    }
    let mut range = staging.len()..0;
    for (&offset, cell) in offsets.iter().zip(updated) {
        staging[offset] = *cell;
        range.start = range.start.min(offset);
        range.end = range.end.max(offset + 1);
    }
    Some(if offsets.is_empty() { 0..0 } else { range })
}

#[cfg(test)]
mod row_patch_tests {
    use super::*;

    mod x11 {
        use crate as sugarloaf;
        include!("../../../misc/benchmarks/x11.rs");
    }

    #[test]
    #[ignore = "requires an X11 display and Vulkan device"]
    fn mixed_atlas_edits_reach_every_frame_slot() {
        use crate::renderer::vulkan as renderer;
        use bytemuck::Zeroable;
        let window = x11::Window::new();
        let mut ctx = VulkanContext::new(window.handle());
        let mut grid = VulkanGridRenderer::new(&ctx, 5, 3);
        grid.insert_glyph_color(
            GlyphKey {
                font_id: 0,
                glyph_id: 0,
                size_bucket: 64,
            },
            RasterizedGlyph {
                width: 1,
                height: 1,
                bearing_x: 0,
                bearing_y: 0,
                bytes: &[0, 0, 0, 0],
            },
        )
        .unwrap();
        let mut rows: Vec<Vec<CellText>> = (0..3)
            .map(|row| {
                (0..5)
                    .map(|col| CellText {
                        grid_pos: [col, row],
                        atlas: (col % 2) as u8,
                        ..Default::default()
                    })
                    .collect()
            })
            .collect();
        let mut u = GridUniforms::zeroed();
        u.cell_size = [9.6, 20.0];
        u.grid_size = [5, 3];
        u.cursor_pos = [u32::MAX; 2];
        for step in 0..24 {
            // Several edits can accumulate before a slot is visited. Also force
            // rebuilds by changing lengths, bucket membership, and empty rows.
            if step == 5 {
                rows[2].pop();
            }
            if step == 9 {
                rows[0][0].atlas = 1;
            }
            if step == 12 {
                rows[1].clear();
            }
            if step == 16 {
                for cell in rows.iter_mut().flatten() {
                    cell.atlas = 0;
                }
            }
            if step == 20 {
                rows[2][0].atlas = 1;
            }
            for (row, cells) in rows.iter_mut().enumerate() {
                if step % 3 == row {
                    for cell in cells.iter_mut() {
                        cell.color[0] = step as u8;
                    }
                }
                let mut bg = [CellBg::TRANSPARENT; 5];
                bg[4].rgba = [step as u8, 0, 0, 255];
                let len = (step + row) % 6;
                grid.write_row(row as u32, &bg[..len], cells);
                if step % 4 == 0 {
                    grid.clear_row(row as u32);
                    grid.clear_row(row as u32);
                    grid.write_row(row as u32, &[], cells);
                }
            }
            let visible: Vec<_> = grid
                .bg_cpu
                .chunks(5)
                .map(|row| row.iter().any(|cell| *cell != CellBg::TRANSPARENT))
                .collect();
            assert_eq!(grid.bg_row_visible, visible, "visibility step={step}");
            assert_eq!(grid.bg_visible_rows, visible.iter().filter(|&&v| v).count());
            let frame = ctx.acquire_frame().expect("test frame");
            let device = ctx.device();
            let cmd = frame.cmd_buffer;
            grid.prepare(&ctx, cmd, frame.slot);
            let attachments = [renderer::build_color_attachment(&frame, [0.0; 4])];
            unsafe {
                renderer::cmd_acquire_image_for_rendering(device, cmd, frame.image);
                device.cmd_begin_rendering(
                    cmd,
                    &renderer::build_rendering_info(&frame, &attachments),
                );
                device.cmd_set_viewport(
                    cmd,
                    0,
                    &[vk::Viewport {
                        x: 0.0,
                        y: 0.0,
                        width: frame.extent.width as f32,
                        height: frame.extent.height as f32,
                        min_depth: 0.0,
                        max_depth: 1.0,
                    }],
                );
                device.cmd_set_scissor(
                    cmd,
                    0,
                    &[vk::Rect2D {
                        offset: vk::Offset2D::default(),
                        extent: frame.extent,
                    }],
                );
            }
            grid.render_bg(&ctx, cmd, frame.slot, &u);
            let uploaded_bg = unsafe {
                std::slice::from_raw_parts(
                    grid.bg_buffers[frame.slot].as_mut_ptr() as *const CellBg,
                    grid.bg_cpu.len(),
                )
            };
            assert_eq!(uploaded_bg, grid.bg_cpu, "background upload step={step}");
            grid.render_text(&ctx, cmd, frame.slot, &u);
            if matches!(step, 0 | 5 | 9 | 12 | 16 | 20) {
                assert!(
                    grid.fg_mixed_row_offsets.iter().all(Vec::is_empty),
                    "structural edits must defer offset construction, step={step}"
                );
            }
            let mut buckets: Vec<((u8, u8), Vec<CellText>)> = Vec::new();
            for cell in rows.iter().flatten() {
                let key = (cell.atlas, cell.page);
                let index = buckets
                    .iter()
                    .position(|(old, _)| *old == key)
                    .unwrap_or_else(|| {
                        buckets.push((key, Vec::new()));
                        buckets.len() - 1
                    });
                buckets[index].1.push(*cell);
            }
            let expected: Vec<_> =
                buckets.into_iter().flat_map(|(_, cells)| cells).collect();
            assert_eq!(grid.fg_staging, expected, "staging step={step}");
            let uploaded = unsafe {
                std::slice::from_raw_parts(
                    grid.fg_buffers[frame.slot]
                        .buffer
                        .as_ref()
                        .unwrap()
                        .as_mut_ptr() as *const CellText,
                    expected.len(),
                )
            };
            assert_eq!(uploaded, expected, "upload step={step} slot={}", frame.slot);
            unsafe {
                device.cmd_end_rendering(cmd);
                renderer::cmd_release_image_to_present(device, cmd, frame.image);
            }
            ctx.present_frame(frame);
        }
        unsafe {
            ctx.device().device_wait_idle().unwrap();
        }
        grid.resize(2, 2);
        assert_eq!(grid.bg_visible_rows, 0);
        assert_eq!(grid.bg_row_visible, [false, false]);
    }

    #[test]
    fn mixed_row_patch_preserves_other_rows_and_tracks_upload_bounds() {
        let cell = |atlas, page, glyph| CellText {
            atlas,
            page,
            glyph_pos: [glyph, 0],
            ..Default::default()
        };
        let old = [cell(0, 0, 1), cell(1, 0, 2), cell(0, 1, 3), cell(0, 0, 4)];
        let new = old.map(|mut cell| {
            cell.color = [71, 129, 210, 192];
            cell
        });
        let untouched = cell(0, 0, 99);
        let mut staging = vec![untouched; 9];
        let offsets = [1, 5, 8, 2];
        for (&offset, cell) in offsets.iter().zip(old) {
            staging[offset] = cell;
        }
        let before = staging.clone();
        assert_eq!(
            patch_mixed_row(&mut staging, &offsets, &old, &new),
            Some(1..9)
        );
        for index in 0..staging.len() {
            match offsets.iter().position(|&offset| offset == index) {
                Some(cell) => assert_eq!(staging[index], new[cell]),
                None => assert_eq!(staging[index], before[index]),
            }
        }
        for change in 0..3 {
            let mut changed = new.to_vec();
            match change {
                0 => {
                    changed[2].page = 2;
                }
                1 => {
                    changed[1].atlas = 0;
                }
                _ => {
                    changed.pop();
                }
            }
            let before = staging.clone();
            assert_eq!(
                patch_mixed_row(&mut staging, &offsets, &new, &changed),
                None
            );
            assert_eq!(
                staging, before,
                "rejected layouts must not partially patch staging"
            );
        }
    }
}

#[derive(Default)]
struct TextBuffer {
    buffer: Option<VulkanBuffer>,
    capacity: usize,
}

impl TextBuffer {
    fn reserve(&mut self, ctx: &VulkanContext, needed: usize) -> bool {
        if needed > self.capacity {
            self.capacity = needed.next_power_of_two().max(64);
            self.buffer = Some(ctx.allocate_host_visible_buffer(
                (self.capacity * std::mem::size_of::<CellText>()) as u64,
                vk::BufferUsageFlags::VERTEX_BUFFER,
            ));
            return true;
        }
        false
    }

    fn upload(&mut self, ctx: &VulkanContext, parts: &[&[CellText]]) {
        let needed = parts.iter().map(|part| part.len()).sum::<usize>();
        self.reserve(ctx, needed);
        if needed != 0 {
            let mut dst = self.buffer.as_ref().unwrap().as_mut_ptr() as *mut CellText;
            for part in parts {
                unsafe {
                    std::ptr::copy_nonoverlapping(part.as_ptr(), dst, part.len());
                    dst = dst.add(part.len());
                }
            }
        }
    }

    fn upload_range(
        &mut self,
        ctx: &VulkanContext,
        cells: &[CellText],
        mut range: Range<usize>,
    ) {
        if self.reserve(ctx, cells.len()) {
            range = 0..cells.len();
        }
        if !range.is_empty() {
            unsafe {
                let dst = self.buffer.as_ref().unwrap().as_mut_ptr() as *mut CellText;
                std::ptr::copy_nonoverlapping(
                    cells.as_ptr().add(range.start),
                    dst.add(range.start),
                    range.len(),
                );
            }
        }
    }
}

fn text_runs(cells: &[CellText], offset: u32) -> impl Iterator<Item = TextBucket> + '_ {
    let mut start = offset;
    cells
        .chunk_by(|a, b| (a.atlas, a.page) == (b.atlas, b.page))
        .map(move |run| {
            let bucket = TextBucket {
                kind: run[0].atlas,
                page: run[0].page,
                start,
                count: run.len() as u32,
            };
            start += bucket.count;
            bucket
        })
}

/// One atlas page: backing image, slot allocator, per-page upload
/// queue, and a descriptor set bound to the image + the shared
/// sampler.
///
/// Pages are appended on demand by `VulkanGlyphAtlas::insert` and
/// never resized, copied, or freed at runtime — so the descriptor set
/// is written once at creation and never updated again, sidestepping
/// the "update a bound descriptor while in-flight frames sample it"
/// hazard that resizing a single atlas image had.
struct VulkanAtlasPage {
    image: VulkanImage,
    allocator: AtlasAllocator,
    pending: Vec<PendingUpload>,
    /// `true` once the image has been transitioned out of `UNDEFINED`.
    /// `make_page` does the initial `UNDEFINED → SHADER_READ_ONLY`
    /// transition synchronously, so this is `true` from page birth;
    /// kept around so `upload_to_page` can pick the right
    /// `src_stage`/`src_access` for its leading barrier — same shape
    /// the previous single-image atlas's flag had.
    initialized: bool,
    /// Descriptor set with the page's image + the shared sampler at
    /// binding 0. Allocated from the renderer's descriptor pool when
    /// the page is created; bound by `render_text` for each draw of
    /// cells that live in this page.
    descriptor_set: vk::DescriptorSet,
}

/// Per-kind glyph atlas as a list of fixed-size pages.
///
/// One instance per atlas kind (R8 grayscale, RGBA8 color). Owned by
/// either `VulkanGridRenderer` (per-panel terminal grids) or
/// `sugarloaf::text::Text`'s Vulkan state (UI overlay text).
///
/// On `insert`, the atlas tries each existing page in order; if none
/// has room it appends a new page (up to `MAX_PAGES`). Old pages and
/// their slot coordinates stay valid forever — nothing is ever
/// resized, copied, or freed at runtime. That removes the
/// use-after-free hazard of resizing an in-use image and the
/// `TRANSFER_SRC` usage requirement (no GPU-side copy).
///
/// The descriptor pool, set layout, and sampler are owned by the
/// caller (the renderer) so all pages across both atlases share one
/// pool and one layout; the atlas just allocates a fresh set from
/// that pool whenever it grows a page.
pub struct VulkanGlyphAtlas {
    pages: Vec<VulkanAtlasPage>,
    slots: FxHashMap<GlyphKey, AtlasSlot>,
    format: vk::Format,
    bytes_per_pixel: u32,
    /// One staging buffer per frame-in-flight slot, sized for the
    /// largest single-frame upload (sum across all pages). Reused
    /// across frames within a slot — the `acquire_frame` fence wait
    /// inside `VulkanContext` proves the previous use of slot N's
    /// staging is GPU-complete.
    staging: [Option<crate::context::vulkan::VulkanBuffer>; FRAMES_IN_FLIGHT],
    staging_capacity: [usize; FRAMES_IN_FLIGHT],
    /// Pieces needed to spin up additional pages on demand from
    /// inside `insert` (which has no `&VulkanContext` borrow).
    shared: Arc<VkShared>,
    sampler: vk::Sampler,
    descriptor_pool: vk::DescriptorPool,
    descriptor_set_layout: vk::DescriptorSetLayout,
}

impl VulkanGlyphAtlas {
    pub fn new_grayscale(
        ctx: &VulkanContext,
        descriptor_pool: vk::DescriptorPool,
        descriptor_set_layout: vk::DescriptorSetLayout,
        sampler: vk::Sampler,
    ) -> Self {
        Self::new(
            ctx,
            vk::Format::R8_UNORM,
            1,
            descriptor_pool,
            descriptor_set_layout,
            sampler,
            true,
        )
    }

    pub fn new_color(
        ctx: &VulkanContext,
        descriptor_pool: vk::DescriptorPool,
        descriptor_set_layout: vk::DescriptorSetLayout,
        sampler: vk::Sampler,
    ) -> Self {
        Self::new(
            ctx,
            vk::Format::R8G8B8A8_UNORM,
            4,
            descriptor_pool,
            descriptor_set_layout,
            sampler,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new(
        ctx: &VulkanContext,
        format: vk::Format,
        bytes_per_pixel: u32,
        descriptor_pool: vk::DescriptorPool,
        descriptor_set_layout: vk::DescriptorSetLayout,
        sampler: vk::Sampler,
        eager_first_page: bool,
    ) -> Self {
        let shared = ctx.shared().clone();
        let pages = if eager_first_page {
            vec![make_page(
                &shared,
                format,
                descriptor_pool,
                descriptor_set_layout,
                sampler,
            )]
        } else {
            Vec::new()
        };
        Self {
            pages,
            slots: FxHashMap::default(),
            format,
            bytes_per_pixel,
            staging: std::array::from_fn(|_| None),
            staging_capacity: [0; FRAMES_IN_FLIGHT],
            shared,
            sampler,
            descriptor_pool,
            descriptor_set_layout,
        }
    }

    /// Drain each page's `pending` into the per-slot staging buffer
    /// (growing it if needed), then record one copy + barrier pair per
    /// non-empty page into `cmd`. Caller MUST be outside a
    /// dynamic-rendering pass — Vulkan 1.3 spec
    /// `VUID-vkCmdCopyBufferToImage-renderpass` forbids transfer
    /// commands inside one. New empty pages also receive their initial layout;
    /// later calls without pending uploads only inspect the page flags.
    ///
    /// We take `&Arc<VkShared>` rather than `&VulkanContext` so the
    /// text overlay path can call this without holding an immutable
    /// borrow on the context (`Sugarloaf::render_vulkan` keeps
    /// `ctx: &mut VulkanContext` for the swapchain acquire/present
    /// cycle).
    pub fn flush_uploads(
        &mut self,
        shared: &Arc<VkShared>,
        cmd: vk::CommandBuffer,
        slot: usize,
    ) {
        let total_bytes: usize = self
            .pages
            .iter()
            .flat_map(|p| p.pending.iter())
            .map(|u| (u.w as usize) * (u.h as usize) * self.bytes_per_pixel as usize)
            .sum();
        if total_bytes == 0 {
            for page in &mut self.pages {
                initialize_empty_page(&shared.raw, cmd, page);
            }
            return;
        }

        // Grow per-slot staging if needed. The `max(256K)` floor keeps
        // us from churning allocations during the first-frame burst.
        if total_bytes > self.staging_capacity[slot] {
            let new_cap = total_bytes.next_power_of_two().max(256 * 1024);
            self.staging[slot] = Some(allocate_host_visible_buffer_raw(
                shared,
                new_cap as u64,
                vk::BufferUsageFlags::TRANSFER_SRC,
            ));
            self.staging_capacity[slot] = new_cap;
        }
        let staging = self.staging[slot].as_ref().unwrap();
        let staging_ptr = staging.as_mut_ptr();
        let staging_handle = staging.handle();
        let bpp = self.bytes_per_pixel as usize;

        let mut offset: u64 = 0;
        for page in &mut self.pages {
            if page.pending.is_empty() {
                initialize_empty_page(&shared.raw, cmd, page);
                continue;
            }
            let mut copies: Vec<vk::BufferImageCopy> =
                Vec::with_capacity(page.pending.len());
            unsafe {
                for upload in page.pending.drain(..) {
                    let bytes = (upload.w as usize) * (upload.h as usize) * bpp;
                    std::ptr::copy_nonoverlapping(
                        upload.bytes.as_ptr(),
                        staging_ptr.add(offset as usize),
                        bytes,
                    );
                    copies.push(image_copy_region(
                        offset, upload.x, upload.y, upload.w, upload.h,
                    ));
                    offset += bytes as u64;
                }
            }
            upload_to_page(&shared.raw, cmd, staging_handle, page, &copies);
        }
    }

    #[inline]
    pub fn lookup(&self, key: GlyphKey) -> Option<AtlasSlot> {
        self.slots.get(&key).copied()
    }

    /// Descriptor set for the given page index. Bound to set=1 by
    /// `render_text` for each `(kind, page)` bucket of cells.
    #[inline]
    pub fn page_descriptor_set(&self, page: u8) -> vk::DescriptorSet {
        self.pages[page as usize].descriptor_set
    }

    /// Number of pages currently allocated.
    #[inline]
    pub fn num_pages(&self) -> usize {
        self.pages.len()
    }

    /// Pack + queue a glyph for upload. Tries each existing page in
    /// order; if none have room, appends a new page (up to
    /// `MAX_PAGES`) and packs into that. Returns `None` only when the
    /// glyph won't fit anywhere even after appending the last allowed
    /// page — same contract as the previous single-image atlas
    /// hitting its hard cap.
    pub fn insert(
        &mut self,
        key: GlyphKey,
        glyph: RasterizedGlyph<'_>,
    ) -> Option<AtlasSlot> {
        let slot = self.upload(glyph)?;
        self.slots.insert(key, slot);
        Some(slot)
    }

    /// Pack pixels without a glyph-cache entry, for artwork cached by the caller.
    pub fn upload(&mut self, glyph: RasterizedGlyph<'_>) -> Option<AtlasSlot> {
        if glyph.width == 0 || glyph.height == 0 {
            // Whitespace / control glyphs — record an empty slot in
            // page 0 so lookups don't keep retrying. The slot has
            // zero area, so it doesn't actually consume page space.
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

        // Try existing pages in order. Glyphs pack into the earliest
        // page that fits — keeps the active working set on page 0/1.
        for (i, page) in self.pages.iter_mut().enumerate() {
            if let Some((x, y)) = page.allocator.allocate(glyph.width, glyph.height) {
                let slot = AtlasSlot {
                    x,
                    y,
                    w: glyph.width,
                    h: glyph.height,
                    bearing_x: glyph.bearing_x,
                    bearing_y: glyph.bearing_y,
                    page: i as u8,
                };
                page.pending.push(PendingUpload {
                    x,
                    y,
                    w: glyph.width,
                    h: glyph.height,
                    bytes: glyph.bytes.to_vec(),
                });
                return Some(slot);
            }
        }

        // No existing page fit — append a new one (within the cap).
        if self.pages.len() >= MAX_PAGES {
            return None;
        }
        let new_page = make_page(
            &self.shared,
            self.format,
            self.descriptor_pool,
            self.descriptor_set_layout,
            self.sampler,
        );
        let new_idx = self.pages.len();
        self.pages.push(new_page);
        let page = &mut self.pages[new_idx];
        let (x, y) = page.allocator.allocate(glyph.width, glyph.height)?;
        let slot = AtlasSlot {
            x,
            y,
            w: glyph.width,
            h: glyph.height,
            bearing_x: glyph.bearing_x,
            bearing_y: glyph.bearing_y,
            page: new_idx as u8,
        };
        page.pending.push(PendingUpload {
            x,
            y,
            w: glyph.width,
            h: glyph.height,
            bytes: glyph.bytes.to_vec(),
        });
        Some(slot)
    }

    /// Forget every cached glyph. Pages are kept and their allocators
    /// reset; the caller forces a full row rebuild before the next draw.
    pub fn clear(&mut self) {
        self.slots.clear();
        for page in &mut self.pages {
            page.allocator.clear();
            page.pending.clear();
        }
    }
}

/// Allocate a fresh atlas page: create the image + view, transition
/// it to `SHADER_READ_ONLY_OPTIMAL` via a oneshot submit, allocate a
/// descriptor set from `pool`, and point that set at the image +
/// sampler.
///
/// The transition + descriptor write are safe to do mid-frame because
/// the page is brand new — no submitted command buffer can reference
/// it yet, so there's no in-flight-frame hazard. Compare the previous
/// single-image atlas's `grow`, which had to either stall the device
/// or accept a use-after-free on every resize.
fn make_page(
    shared: &Arc<VkShared>,
    format: vk::Format,
    descriptor_pool: vk::DescriptorPool,
    descriptor_set_layout: vk::DescriptorSetLayout,
    sampler: vk::Sampler,
) -> VulkanAtlasPage {
    let image = allocate_atlas_page_image(shared, format);

    let descriptor_set =
        allocate_one_descriptor_set(&shared.raw, descriptor_pool, descriptor_set_layout);
    update_page_descriptor_set(&shared.raw, descriptor_set, &image, sampler);

    VulkanAtlasPage {
        image,
        allocator: AtlasAllocator::new(ATLAS_PAGE_SIZE, ATLAS_PAGE_SIZE),
        pending: Vec::new(),
        initialized: false,
        descriptor_set,
    }
}

/// Inline equivalent of `VulkanContext::allocate_sampled_image` for
/// callers that only hold an `Arc<VkShared>` — used by `make_page`
/// when growing the atlas mid-frame from inside `insert`.
fn allocate_atlas_page_image(shared: &Arc<VkShared>, format: vk::Format) -> VulkanImage {
    let image_info = vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(format)
        .extent(vk::Extent3D {
            width: ATLAS_PAGE_SIZE as u32,
            height: ATLAS_PAGE_SIZE as u32,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::OPTIMAL)
        // No `TRANSFER_SRC` — pages are never copied from; only the
        // glyph-byte staging buffer copies INTO them via TRANSFER_DST.
        .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED);
    let image = unsafe {
        shared
            .create_image(&image_info, None)
            .expect("atlas page: create_image")
    };

    let req = unsafe { shared.get_image_memory_requirements(image) };
    let mem_type = crate::context::vulkan::find_memory_type(
        &shared.instance,
        shared.physical_device,
        req.memory_type_bits,
        vk::MemoryPropertyFlags::DEVICE_LOCAL,
    )
    .expect("atlas page: no DEVICE_LOCAL memory type");

    let alloc_info = vk::MemoryAllocateInfo::default()
        .allocation_size(req.size)
        .memory_type_index(mem_type);
    let memory = unsafe {
        shared
            .allocate_memory(&alloc_info, None)
            .expect("atlas page: allocate_memory")
    };
    unsafe {
        shared
            .bind_image_memory(image, memory, 0)
            .expect("atlas page: bind_image_memory");
    }

    let view_info = vk::ImageViewCreateInfo::default()
        .image(image)
        .view_type(vk::ImageViewType::TYPE_2D)
        .format(format)
        .components(vk::ComponentMapping::default())
        .subresource_range(color_subresource_range());
    let view = unsafe {
        shared
            .create_image_view(&view_info, None)
            .expect("atlas page: create_image_view")
    };

    VulkanImage {
        shared: shared.clone(),
        image,
        view,
        memory,
        width: ATLAS_PAGE_SIZE as u32,
        height: ATLAS_PAGE_SIZE as u32,
        format,
    }
}

/// Write a page's combined-image-sampler binding (=0) to point at
/// `image` + `sampler`. Called exactly once when the page is created;
/// the set is brand new at that point, so the write has no in-flight
/// hazard.
fn update_page_descriptor_set(
    device: &ash::Device,
    set: vk::DescriptorSet,
    image: &VulkanImage,
    sampler: vk::Sampler,
) {
    let image_info = vk::DescriptorImageInfo::default()
        .sampler(sampler)
        .image_view(image.view())
        .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
    let image_infos = [image_info];
    let write = vk::WriteDescriptorSet::default()
        .dst_set(set)
        .dst_binding(0)
        .dst_array_element(0)
        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
        .image_info(&image_infos);
    unsafe { device.update_descriptor_sets(&[write], &[]) };
}

/// Immutable grid state shared by all terminals using this device and target
/// format. The context owns the cache; the device owner has no back-reference.
pub(crate) struct GridPipelines {
    shared: Arc<VkShared>,
    bg_descriptor_set_layout: vk::DescriptorSetLayout,
    bg_pipeline_layout: vk::PipelineLayout,
    bg_pipeline: vk::Pipeline,
    text_uniform_descriptor_set_layout: vk::DescriptorSetLayout,
    text_atlas_descriptor_set_layout: vk::DescriptorSetLayout,
    text_pipeline_layout: vk::PipelineLayout,
    text_pipeline: vk::Pipeline,
    sampler: vk::Sampler,
}

impl GridPipelines {
    fn new(ctx: &VulkanContext) -> Self {
        let shared = ctx.shared().clone();
        let device = &shared.raw;
        let bg_descriptor_set_layout = create_bg_descriptor_set_layout(device);
        let bg_pipeline_layout =
            create_pipeline_layout(device, &[bg_descriptor_set_layout], &[]);
        let bg_pipeline = create_bg_pipeline(
            device,
            ctx.pipeline_cache(),
            bg_pipeline_layout,
            ctx.swapchain_format(),
        );
        let sampler = create_sampler(device);
        let text_uniform_descriptor_set_layout =
            create_text_uniform_descriptor_set_layout(device);
        let text_atlas_descriptor_set_layout =
            create_text_atlas_descriptor_set_layout(device);
        // Push constant `is_color: u32` switches sampling mode per draw.
        let text_push_constants = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::FRAGMENT)
            .offset(0)
            .size(4)];
        let text_pipeline_layout = create_pipeline_layout(
            device,
            &[
                text_uniform_descriptor_set_layout,
                text_atlas_descriptor_set_layout,
            ],
            &text_push_constants,
        );
        let text_pipeline = create_text_pipeline(
            device,
            ctx.pipeline_cache(),
            text_pipeline_layout,
            ctx.swapchain_format(),
        );
        Self {
            shared,
            bg_descriptor_set_layout,
            bg_pipeline_layout,
            bg_pipeline,
            text_uniform_descriptor_set_layout,
            text_atlas_descriptor_set_layout,
            text_pipeline_layout,
            text_pipeline,
            sampler,
        }
    }
}

impl Drop for GridPipelines {
    fn drop(&mut self) {
        // The context and each grid idle the device before releasing their
        // ownership. The last owner therefore cannot leave a draw in flight.
        unsafe {
            self.shared.destroy_pipeline(self.text_pipeline, None);
            self.shared
                .destroy_pipeline_layout(self.text_pipeline_layout, None);
            self.shared.destroy_descriptor_set_layout(
                self.text_atlas_descriptor_set_layout,
                None,
            );
            self.shared.destroy_descriptor_set_layout(
                self.text_uniform_descriptor_set_layout,
                None,
            );
            self.shared.destroy_sampler(self.sampler, None);
            self.shared.destroy_pipeline(self.bg_pipeline, None);
            self.shared
                .destroy_pipeline_layout(self.bg_pipeline_layout, None);
            self.shared
                .destroy_descriptor_set_layout(self.bg_descriptor_set_layout, None);
        }
    }
}

pub struct VulkanGridRenderer {
    /// Shared device handle. Cloned from `VulkanContext` at
    /// construction so this renderer's `Drop` can call `destroy_*` on
    /// pipelines/descriptor pools/etc. without depending on
    /// `VulkanContext` still being alive — `vkDestroyDevice` runs
    /// only when the last `Arc<VkShared>` clone (across all
    /// renderers, buffers, images, atlases) is dropped. See
    /// `VkShared`. Also lets `resize` (which only has `&mut self`)
    /// allocate bg buffers via `allocate_host_visible_buffer_raw`
    /// without needing a `&VulkanContext` borrow.
    shared: Arc<VkShared>,
    pipelines: Arc<GridPipelines>,

    cols: u32,
    rows: u32,

    bg_buffers: [VulkanBuffer; FRAMES_IN_FLIGHT],
    /// Cell spans accumulate independently until each frame slot is uploaded.
    bg_dirty: [Option<Range<usize>>; FRAMES_IN_FLIGHT],
    bg_cpu: Vec<CellBg>,
    bg_row_visible: Vec<bool>,
    bg_visible_rows: usize,

    uniform_buffers: [VulkanBuffer; FRAMES_IN_FLIGHT],

    bg_descriptor_pool: vk::DescriptorPool,
    bg_descriptor_sets: [vk::DescriptorSet; FRAMES_IN_FLIGHT],

    fg_rows: Vec<Vec<CellText>>,
    fg_staging: Vec<CellText>,
    fg_staging_dirty: bool,
    /// Prefix offsets for a single-atlas layout. Atlas regrouping and row
    /// length changes rebuild the layout; same-length edits patch one range.
    fg_row_offsets: Vec<usize>,
    /// Per-cell staging offsets for mixed-atlas layouts. Constructed on the first
    /// same-layout edit, so structural churn only rebuilds the staging buffer.
    fg_mixed_row_offsets: Vec<Vec<usize>>,
    fg_mixed_row_offsets_valid: bool,
    fg_bucket_scratch: Vec<((u8, u8), Vec<CellText>)>,
    fg_buffers: [TextBuffer; FRAMES_IN_FLIGHT],
    fg_dirty: [Option<Range<usize>>; FRAMES_IN_FLIGHT],
    cursor_buffers: [TextBuffer; FRAMES_IN_FLIGHT],
    cursor_dirty: [bool; FRAMES_IN_FLIGHT],
    /// Layout of the current CPU staging data. Each dirty GPU slot receives
    /// this same layout before drawing, so grouping happens once per edit.
    fg_buckets: Vec<TextBucket>,

    /// Pool sized for `FRAMES_IN_FLIGHT` uniform sets plus
    /// `MAX_PAGES × 2` atlas page sets (grayscale + color atlases).
    text_descriptor_pool: vk::DescriptorPool,
    text_uniform_descriptor_sets: [vk::DescriptorSet; FRAMES_IN_FLIGHT],

    pub atlas_grayscale: VulkanGlyphAtlas,
    pub atlas_color: VulkanGlyphAtlas,

    needs_full_rebuild: bool,
}

impl VulkanGridRenderer {
    pub fn new(ctx: &VulkanContext, cols: u32, rows: u32) -> Self {
        let shared = ctx.shared().clone();
        let device = &shared.raw;
        let pipelines = ctx
            .grid_pipelines
            .lock()
            .unwrap()
            .entry(ctx.swapchain_format())
            .or_insert_with(|| Arc::new(GridPipelines::new(ctx)))
            .clone();

        let bg_buffers = std::array::from_fn(|_| alloc_bg_buffer(ctx, cols, rows));
        let uniform_buffers = std::array::from_fn(|_| {
            ctx.allocate_host_visible_buffer(
                std::mem::size_of::<GridUniforms>() as u64,
                vk::BufferUsageFlags::UNIFORM_BUFFER,
            )
        });

        let bg_descriptor_pool = create_bg_descriptor_pool(device);
        let bg_descriptor_sets = allocate_descriptor_sets(
            device,
            bg_descriptor_pool,
            pipelines.bg_descriptor_set_layout,
        );
        for slot in 0..FRAMES_IN_FLIGHT {
            update_bg_descriptor_set(
                device,
                bg_descriptor_sets[slot],
                &uniform_buffers[slot],
                &bg_buffers[slot],
            );
        }
        // Text pipeline plumbing — built BEFORE the atlases so each
        // atlas can allocate its initial page's descriptor set from
        // the pool we hand it.
        let text_descriptor_pool = create_text_descriptor_pool(device);

        let text_uniform_descriptor_sets = allocate_descriptor_sets(
            device,
            text_descriptor_pool,
            pipelines.text_uniform_descriptor_set_layout,
        );
        for slot in 0..FRAMES_IN_FLIGHT {
            update_text_uniform_descriptor_set(
                device,
                text_uniform_descriptor_sets[slot],
                &uniform_buffers[slot],
            );
        }

        let atlas_grayscale = VulkanGlyphAtlas::new_grayscale(
            ctx,
            text_descriptor_pool,
            pipelines.text_atlas_descriptor_set_layout,
            pipelines.sampler,
        );
        let atlas_color = VulkanGlyphAtlas::new_color(
            ctx,
            text_descriptor_pool,
            pipelines.text_atlas_descriptor_set_layout,
            pipelines.sampler,
        );

        let bg_len = (cols as usize) * (rows as usize);
        Self {
            shared,
            pipelines,
            cols,
            rows,
            bg_buffers,
            bg_dirty: std::array::from_fn(|_| Some(0..bg_len)),
            bg_cpu: vec![CellBg::TRANSPARENT; bg_len],
            bg_row_visible: vec![false; rows as usize],
            bg_visible_rows: 0,
            uniform_buffers,
            bg_descriptor_pool,
            bg_descriptor_sets,
            fg_rows: init_fg_rows(rows),
            fg_staging: Vec::new(),
            fg_staging_dirty: true,
            fg_row_offsets: Vec::new(),
            fg_mixed_row_offsets: Vec::new(),
            fg_mixed_row_offsets_valid: false,
            fg_bucket_scratch: Vec::with_capacity(4),
            fg_buffers: std::array::from_fn(|_| TextBuffer::default()),
            fg_dirty: std::array::from_fn(|_| None),
            cursor_buffers: std::array::from_fn(|_| TextBuffer::default()),
            cursor_dirty: [true; FRAMES_IN_FLIGHT],
            fg_buckets: Vec::new(),
            text_descriptor_pool,
            text_uniform_descriptor_sets,
            atlas_grayscale,
            atlas_color,
            needs_full_rebuild: true,
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

    #[inline]
    pub fn request_full_rebuild(&mut self) {
        self.needs_full_rebuild = true;
    }

    pub fn resize(&mut self, cols: u32, rows: u32) {
        if cols == self.cols && rows == self.rows {
            return;
        }
        unsafe {
            let _ = self.shared.device_wait_idle();
        }

        self.cols = cols;
        self.rows = rows;
        let bg_len = (cols as usize) * (rows as usize);
        self.bg_cpu = vec![CellBg::TRANSPARENT; bg_len];
        self.bg_row_visible = vec![false; rows as usize];
        self.bg_visible_rows = 0;

        // Reallocate bg buffers via the cached `Arc<VkShared>` and
        // re-wire descriptor sets to the new buffer handles.
        let bg_byte_size = (bg_len * std::mem::size_of::<CellBg>())
            .max(std::mem::size_of::<CellBg>()) as u64;
        self.bg_buffers = std::array::from_fn(|_| {
            allocate_host_visible_buffer_raw(
                &self.shared,
                bg_byte_size,
                vk::BufferUsageFlags::STORAGE_BUFFER,
            )
        });
        for slot in 0..FRAMES_IN_FLIGHT {
            update_bg_descriptor_set(
                &self.shared.raw,
                self.bg_descriptor_sets[slot],
                &self.uniform_buffers[slot],
                &self.bg_buffers[slot],
            );
        }
        self.bg_dirty = std::array::from_fn(|_| Some(0..bg_len));

        // Reset fg state — emit loop will re-populate after resize.
        self.fg_rows = init_fg_rows(rows);
        self.fg_staging_dirty = true;
        self.cursor_dirty = [true; FRAMES_IN_FLIGHT];
        self.needs_full_rebuild = true;
    }

    pub fn write_row(&mut self, row: u32, bg: &[CellBg], fg: &[CellText]) {
        if row >= self.rows {
            return;
        }
        // Identical row writes must not invalidate every frame-slot upload.
        let idx = (row as usize) + 1;
        if bytemuck::cast_slice::<_, u8>(self.fg_rows[idx].as_slice())
            != bytemuck::cast_slice(fg)
        {
            if !self.fg_staging_dirty
                && !self.fg_mixed_row_offsets_valid
                && self.fg_row_offsets.is_empty()
                && same_atlas_layout(&self.fg_rows[idx], fg)
            {
                self.cache_mixed_row_offsets();
            }
            let slot = &mut self.fg_rows[idx];
            let range = if !self.fg_staging_dirty
                && slot.len() == fg.len()
                && self.fg_row_offsets.len() == self.rows as usize + 1
                && self.fg_buckets.first().is_some_and(|bucket| {
                    fg.iter()
                        .all(|cell| (cell.atlas, cell.page) == (bucket.kind, bucket.page))
                }) {
                let range = self.fg_row_offsets[row as usize]
                    ..self.fg_row_offsets[row as usize + 1];
                self.fg_staging[range.clone()].copy_from_slice(fg);
                Some(range)
            } else if !self.fg_staging_dirty && self.fg_mixed_row_offsets_valid {
                self.fg_mixed_row_offsets
                    .get(row as usize)
                    .and_then(|offsets| {
                        patch_mixed_row(&mut self.fg_staging, offsets, slot, fg)
                    })
            } else {
                None
            };
            if let Some(range) = range {
                for dirty in &mut self.fg_dirty {
                    *dirty = Some(match dirty.take() {
                        Some(old) => old.start.min(range.start)..old.end.max(range.end),
                        None => range.clone(),
                    });
                }
            } else {
                self.fg_staging_dirty = true;
            }
            slot.clear();
            slot.extend_from_slice(fg);
        }

        let row_start = (row as usize) * (self.cols as usize);
        let row_len = (self.cols as usize).min(bg.len());
        let cpu = &mut self.bg_cpu[row_start..row_start + self.cols as usize];
        if bytemuck::cast_slice::<_, u8>(&cpu[..row_len])
            != bytemuck::cast_slice(&bg[..row_len])
            || cpu[row_len..]
                .iter()
                .any(|cell| *cell != CellBg::TRANSPARENT)
        {
            let was_visible = self.bg_row_visible[row as usize];
            cpu[..row_len].copy_from_slice(&bg[..row_len]);
            cpu[row_len..].fill(CellBg::TRANSPARENT);
            let is_visible = cpu.iter().any(|cell| *cell != CellBg::TRANSPARENT);
            self.bg_row_visible[row as usize] = is_visible;
            self.bg_visible_rows =
                self.bg_visible_rows + is_visible as usize - was_visible as usize;
            self.mark_bg_dirty(row_start..row_start + self.cols as usize);
        }
    }

    fn cache_mixed_row_offsets(&mut self) {
        let mut next: smallvec::SmallVec<[usize; 4]> = self
            .fg_buckets
            .iter()
            .map(|bucket| bucket.start as usize)
            .collect();
        self.fg_mixed_row_offsets
            .resize_with(self.rows as usize, Vec::new);
        for (row, offsets) in self.fg_rows[1..self.fg_rows.len() - 1]
            .iter()
            .zip(&mut self.fg_mixed_row_offsets)
        {
            offsets.clear();
            for cell in row {
                let bucket = self
                    .fg_buckets
                    .iter()
                    .position(|bucket| {
                        (bucket.kind, bucket.page) == (cell.atlas, cell.page)
                    })
                    .unwrap();
                offsets.push(next[bucket]);
                next[bucket] += 1;
            }
        }
        self.fg_mixed_row_offsets_valid = true;
    }

    pub fn clear_row(&mut self, row: u32) {
        if row >= self.rows {
            return;
        }
        let idx = (row as usize) + 1;
        if let Some(slot) = self.fg_rows.get_mut(idx) {
            if !slot.is_empty() {
                self.fg_staging_dirty = true;
            }
            slot.clear();
        }
        let row_start = (row as usize) * (self.cols as usize);
        let cpu = &mut self.bg_cpu[row_start..row_start + self.cols as usize];
        if self.bg_row_visible[row as usize] {
            cpu.fill(CellBg::TRANSPARENT);
            self.bg_row_visible[row as usize] = false;
            self.bg_visible_rows -= 1;
            self.mark_bg_dirty(row_start..row_start + self.cols as usize);
        }
    }

    fn mark_bg_dirty(&mut self, range: Range<usize>) {
        for dirty in &mut self.bg_dirty {
            *dirty = Some(match dirty.take() {
                Some(old) => old.start.min(range.start)..old.end.max(range.end),
                None => range.clone(),
            });
        }
    }

    pub fn set_cursor(&mut self, block: &[CellText], non_block: &[CellText]) {
        // Cursor motion must not repack or upload the resident terminal text.
        let mut changed = false;
        if let Some(slot) = self.fg_rows.first_mut() {
            if slot.as_slice() != block {
                slot.clear();
                slot.extend_from_slice(block);
                changed = true;
            }
        }
        let last = self.fg_rows.len().saturating_sub(1);
        if last > 0 {
            if let Some(slot) = self.fg_rows.get_mut(last) {
                if slot.as_slice() != non_block {
                    slot.clear();
                    slot.extend_from_slice(non_block);
                    changed = true;
                }
            }
        }
        if changed {
            self.cursor_dirty = [true; FRAMES_IN_FLIGHT];
        }
    }

    #[inline]
    pub fn lookup_glyph(&self, key: GlyphKey) -> Option<AtlasSlot> {
        self.atlas_grayscale.lookup(key)
    }

    #[inline]
    pub fn lookup_glyph_color(&self, key: GlyphKey) -> Option<AtlasSlot> {
        self.atlas_color.lookup(key)
    }

    /// Drop every cached glyph and force a full rebuild. Called when
    /// the font library is swapped, since the new library reuses font ids.
    pub fn clear_atlas(&mut self) {
        self.atlas_grayscale.clear();
        self.atlas_color.clear();
        self.needs_full_rebuild = true;
        self.fg_staging_dirty = true;
        self.cursor_dirty = [true; FRAMES_IN_FLIGHT];
        self.mark_bg_dirty(0..self.bg_cpu.len());
    }

    /// The atlas's own `insert` walks existing pages and appends a
    /// new one when none fit. At `MAX_PAGES` the atlas is cleared
    /// once and every row rebuilt.
    pub fn insert_glyph(
        &mut self,
        key: GlyphKey,
        glyph: RasterizedGlyph<'_>,
    ) -> Option<AtlasSlot> {
        if let Some(slot) = self.atlas_grayscale.insert(key, glyph) {
            return Some(slot);
        }
        self.atlas_grayscale.clear();
        self.needs_full_rebuild = true;
        self.cursor_dirty = [true; FRAMES_IN_FLIGHT];
        self.fg_staging_dirty = true;
        self.atlas_grayscale.insert(key, glyph)
    }

    pub fn insert_glyph_color(
        &mut self,
        key: GlyphKey,
        glyph: RasterizedGlyph<'_>,
    ) -> Option<AtlasSlot> {
        if let Some(slot) = self.atlas_color.insert(key, glyph) {
            return Some(slot);
        }
        self.atlas_color.clear();
        self.needs_full_rebuild = true;
        self.cursor_dirty = [true; FRAMES_IN_FLIGHT];
        self.fg_staging_dirty = true;
        self.atlas_color.insert(key, glyph)
    }

    /// Drain pending atlas uploads into `cmd`. MUST be called BEFORE
    /// `Sugarloaf::render_vulkan` opens its dynamic-rendering pass —
    /// `vkCmdCopyBufferToImage` is forbidden inside a render pass.
    /// Initializes new pages and flushes queued glyph uploads.
    pub fn prepare(
        &mut self,
        ctx: &VulkanContext,
        cmd: vk::CommandBuffer,
        frame_slot: usize,
    ) {
        debug_assert!(frame_slot < FRAMES_IN_FLIGHT);
        // Initial layouts and glyph copies share the frame submission.
        self.flush_pending_uploads(ctx, cmd, frame_slot);
    }

    /// Record the cell-bg pass into `cmd`. Uploads bg cells +
    /// uniforms for `frame_slot` first, then issues the fullscreen
    /// triangle. Caller must have opened the dynamic-rendering pass +
    /// set viewport/scissor + flushed atlas uploads via `prepare()`.
    /// Pair with `render_text`, with any `kitty_below_text` images
    /// composited in between.
    pub fn render_bg(
        &mut self,
        _ctx: &VulkanContext,
        cmd: vk::CommandBuffer,
        frame_slot: usize,
        uniforms: &GridUniforms,
    ) {
        debug_assert!(frame_slot < FRAMES_IN_FLIGHT);
        let slot = frame_slot;

        if let Some(range) = self.bg_dirty[slot].take() {
            unsafe {
                let dst = self.bg_buffers[slot].as_mut_ptr() as *mut CellBg;
                std::ptr::copy_nonoverlapping(
                    self.bg_cpu.as_ptr().add(range.start),
                    dst.add(range.start),
                    range.len(),
                );
            }
        }
        // The text pass shares these uniforms even when backgrounds are skipped.
        unsafe {
            let dst = self.uniform_buffers[slot].as_mut_ptr() as *mut GridUniforms;
            std::ptr::write(dst, *uniforms);
        }

        if self.bg_visible_rows == 0 && uniforms.cursor_bg_color[3] == 0.0 {
            return;
        }

        unsafe {
            self.shared.cmd_bind_pipeline(
                cmd,
                vk::PipelineBindPoint::GRAPHICS,
                self.pipelines.bg_pipeline,
            );
            self.shared.cmd_bind_descriptor_sets(
                cmd,
                vk::PipelineBindPoint::GRAPHICS,
                self.pipelines.bg_pipeline_layout,
                0,
                &[self.bg_descriptor_sets[slot]],
                &[],
            );
            self.shared.cmd_draw(cmd, 3, 1, 0, 0);
        }
    }

    /// Record the cell-text pass into `cmd`. One instanced quad per
    /// `CellText`. Packs changed rows once, then uploads that layout
    /// independently to each dirty frame slot.
    pub fn render_text(
        &mut self,
        ctx: &VulkanContext,
        cmd: vk::CommandBuffer,
        frame_slot: usize,
        _uniforms: &GridUniforms,
    ) {
        debug_assert!(frame_slot < FRAMES_IN_FLIGHT);
        let slot = frame_slot;
        let content_end = self.fg_rows.len() - 1;

        if self.fg_staging_dirty {
            // Bucket cells by (atlas_kind, page) before uploading.
            // Each `cmd_draw` binds exactly one page's descriptor set
            // + one kind, so cells with different `(kind, page)` go
            // into separate ranges of the same vertex buffer.
            // Typical session: ≤ 4 buckets (2 kinds × 1–2 pages), so
            // the linear-search bucketing here is cheap.
            self.fg_staging.clear();
            self.fg_buckets.clear();
            self.fg_row_offsets.clear();
            self.fg_mixed_row_offsets_valid = false;
            for offsets in &mut self.fg_mixed_row_offsets {
                offsets.clear();
            }
            let rows = &self.fg_rows[1..content_end];
            // One atlas page keeps rows contiguous, allowing later row-range uploads.
            let first_key = rows
                .iter()
                .find_map(|row| row.first())
                .map(|cell| (cell.atlas, cell.page));
            if let Some((kind, page)) = first_key.filter(|&key| {
                rows.iter()
                    .flatten()
                    .all(|cell| (cell.atlas, cell.page) == key)
            }) {
                self.fg_row_offsets.push(0);
                for row in rows {
                    self.fg_staging.extend_from_slice(row);
                    self.fg_row_offsets.push(self.fg_staging.len());
                }
                self.fg_buckets.push(TextBucket {
                    kind,
                    page,
                    start: 0,
                    count: self.fg_staging.len() as u32,
                });
            } else {
                let scratch = &mut self.fg_bucket_scratch;
                let mut used = 0;
                for row in rows {
                    for cell in row {
                        let key = (cell.atlas, cell.page);
                        let bucket =
                            match scratch[..used].iter().position(|(k, _)| *k == key) {
                                Some(index) => index,
                                None => {
                                    if used == scratch.len() {
                                        scratch.push((key, Vec::new()));
                                    } else {
                                        scratch[used].0 = key;
                                        scratch[used].1.clear();
                                    }
                                    used += 1;
                                    used - 1
                                }
                            };
                        scratch[bucket].1.push(*cell);
                    }
                }
                for ((kind, page), cells) in &scratch[..used] {
                    let start = self.fg_staging.len() as u32;
                    let count = cells.len() as u32;
                    self.fg_staging.extend_from_slice(cells);
                    self.fg_buckets.push(TextBucket {
                        kind: *kind,
                        page: *page,
                        start,
                        count,
                    });
                }
            }
            for dirty in &mut self.fg_dirty {
                *dirty = Some(0..self.fg_staging.len());
            }
            self.fg_staging_dirty = false;
        }
        if let Some(range) = self.fg_dirty[slot].take() {
            self.fg_buffers[slot].upload_range(ctx, &self.fg_staging, range);
        }
        let block = &self.fg_rows[0];
        let non_block = &self.fg_rows[content_end];
        if self.cursor_dirty[slot] {
            self.cursor_buffers[slot].upload(ctx, &[block, non_block]);
            self.cursor_dirty[slot] = false;
        }
        if self.fg_staging.is_empty() && block.is_empty() && non_block.is_empty() {
            return;
        }

        unsafe {
            self.shared.cmd_bind_pipeline(
                cmd,
                vk::PipelineBindPoint::GRAPHICS,
                self.pipelines.text_pipeline,
            );
            // Uniform set at slot 0 stays the same across every bucket.
            self.shared.cmd_bind_descriptor_sets(
                cmd,
                vk::PipelineBindPoint::GRAPHICS,
                self.pipelines.text_pipeline_layout,
                0,
                &[self.text_uniform_descriptor_sets[slot]],
                &[],
            );
        }
        if !block.is_empty() {
            self.draw_text(cmd, &self.cursor_buffers[slot], text_runs(block, 0));
        }
        if !self.fg_staging.is_empty() {
            self.draw_text(cmd, &self.fg_buffers[slot], self.fg_buckets.iter().copied());
        }
        if !non_block.is_empty() {
            self.draw_text(
                cmd,
                &self.cursor_buffers[slot],
                text_runs(non_block, block.len() as u32),
            );
        }
    }

    fn draw_text(
        &self,
        cmd: vk::CommandBuffer,
        buffer: &TextBuffer,
        buckets: impl Iterator<Item = TextBucket>,
    ) {
        unsafe {
            let buf = buffer.buffer.as_ref().unwrap();
            self.shared
                .cmd_bind_vertex_buffers(cmd, 0, &[buf.handle()], &[0]);

            // One draw per `(kind, page)` bucket. Each draw binds
            // that page's descriptor set at slot 1 and pushes the
            // sampling-mode flag for the fragment shader.
            for bucket in buckets {
                let set = if bucket.kind == CellText::ATLAS_COLOR {
                    self.atlas_color.page_descriptor_set(bucket.page)
                } else {
                    self.atlas_grayscale.page_descriptor_set(bucket.page)
                };
                self.shared.cmd_bind_descriptor_sets(
                    cmd,
                    vk::PipelineBindPoint::GRAPHICS,
                    self.pipelines.text_pipeline_layout,
                    1,
                    &[set],
                    &[],
                );
                let is_color: u32 = if bucket.kind == CellText::ATLAS_COLOR {
                    1
                } else {
                    0
                };
                self.shared.cmd_push_constants(
                    cmd,
                    self.pipelines.text_pipeline_layout,
                    vk::ShaderStageFlags::FRAGMENT,
                    0,
                    &is_color.to_ne_bytes(),
                );
                self.shared.cmd_draw(cmd, 4, bucket.count, 0, bucket.start);
            }
        }
    }

    /// Delegate to each atlas's own `flush_uploads`. Each atlas owns
    /// its own per-slot staging buffer ring now — see
    /// `VulkanGlyphAtlas::flush_uploads`.
    fn flush_pending_uploads(
        &mut self,
        _ctx: &VulkanContext,
        cmd: vk::CommandBuffer,
        slot: usize,
    ) {
        self.atlas_grayscale.flush_uploads(&self.shared, cmd, slot);
        self.atlas_color.flush_uploads(&self.shared, cmd, slot);
    }
}

/// An empty page can still be bound by a zero-area glyph. Initialize its
/// descriptor layout on the frame command buffer, without a separate submission.
fn initialize_empty_page(
    device: &ash::Device,
    cmd: vk::CommandBuffer,
    page: &mut VulkanAtlasPage,
) {
    if page.initialized {
        return;
    }
    let barriers = [vk::ImageMemoryBarrier2::default()
        .src_stage_mask(vk::PipelineStageFlags2::TOP_OF_PIPE)
        .dst_stage_mask(vk::PipelineStageFlags2::FRAGMENT_SHADER)
        .dst_access_mask(vk::AccessFlags2::SHADER_READ)
        .old_layout(vk::ImageLayout::UNDEFINED)
        .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(page.image.handle())
        .subresource_range(color_subresource_range())];
    unsafe {
        device.cmd_pipeline_barrier2(
            cmd,
            &vk::DependencyInfo::default().image_memory_barriers(&barriers),
        );
    }
    page.initialized = true;
}

/// Record an atlas upload: barrier image → `TRANSFER_DST_OPTIMAL`,
/// `cmd_copy_buffer_to_image`, barrier image → `SHADER_READ_ONLY_OPTIMAL`.
///
/// Both barriers are required: the first synchronizes any prior
/// fragment-shader read of the atlas (steady state) against the
/// upcoming transfer write; the second synchronizes the transfer
/// write against the *next* fragment-shader read (which happens in
/// the same command buffer, in the text pipeline draw a few hundred
/// instructions later). Without the trailing barrier the GPU is free
/// to start the fragment work before the copy completes, producing
/// transient garbage glyphs.
///
/// Caller (`flush_pending_uploads`) must ensure this is invoked
/// *outside* a dynamic-rendering pass — Vulkan 1.3 spec
/// VUID-vkCmdCopyBufferToImage-renderpass forbids transfer commands
/// inside a render pass. `Sugarloaf::render_vulkan` honours this by
/// calling `prepare_vulkan` before `cmd_begin_rendering`.
fn upload_to_page(
    device: &ash::Device,
    cmd: vk::CommandBuffer,
    staging: vk::Buffer,
    page: &mut VulkanAtlasPage,
    copies: &[vk::BufferImageCopy],
) {
    let old_layout = if page.initialized {
        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
    } else {
        vk::ImageLayout::UNDEFINED
    };
    unsafe {
        // → TRANSFER_DST
        let to_transfer = vk::ImageMemoryBarrier2::default()
            .src_stage_mask(if page.initialized {
                vk::PipelineStageFlags2::FRAGMENT_SHADER
            } else {
                vk::PipelineStageFlags2::TOP_OF_PIPE
            })
            .src_access_mask(if page.initialized {
                vk::AccessFlags2::SHADER_READ
            } else {
                vk::AccessFlags2::empty()
            })
            .dst_stage_mask(vk::PipelineStageFlags2::COPY)
            .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
            .old_layout(old_layout)
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(page.image.handle())
            .subresource_range(color_subresource_range());
        let barriers = [to_transfer];
        let dep = vk::DependencyInfo::default().image_memory_barriers(&barriers);
        device.cmd_pipeline_barrier2(cmd, &dep);

        // copy
        device.cmd_copy_buffer_to_image(
            cmd,
            staging,
            page.image.handle(),
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            copies,
        );

        // → SHADER_READ
        let to_shader_read = vk::ImageMemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::COPY)
            .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::FRAGMENT_SHADER)
            .dst_access_mask(vk::AccessFlags2::SHADER_READ)
            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(page.image.handle())
            .subresource_range(color_subresource_range());
        let barriers = [to_shader_read];
        let dep = vk::DependencyInfo::default().image_memory_barriers(&barriers);
        device.cmd_pipeline_barrier2(cmd, &dep);
    }

    page.initialized = true;
}

impl Drop for VulkanGridRenderer {
    fn drop(&mut self) {
        unsafe {
            // Idle the queue before destroying descriptor
            // resources. The shared `Arc<VkShared>` keeps the
            // underlying device alive across this whole Drop —
            // `vkDestroyDevice` runs only after every clone is gone.
            let _ = self.shared.device_wait_idle();
            self.shared
                .destroy_descriptor_pool(self.text_descriptor_pool, None);
            self.shared
                .destroy_descriptor_pool(self.bg_descriptor_pool, None);
            // Buffers + atlas images drop themselves.
        }
    }
}

#[inline]
fn alloc_bg_buffer(ctx: &VulkanContext, cols: u32, rows: u32) -> VulkanBuffer {
    let size = (cols as u64)
        .saturating_mul(rows as u64)
        .saturating_mul(std::mem::size_of::<CellBg>() as u64)
        .max(std::mem::size_of::<CellBg>() as u64);
    ctx.allocate_host_visible_buffer(size, vk::BufferUsageFlags::STORAGE_BUFFER)
}

#[inline]
fn init_fg_rows(rows: u32) -> Vec<Vec<CellText>> {
    (0..(rows as usize + CURSOR_ROW_SLOTS))
        .map(|_| Vec::new())
        .collect()
}

#[inline]
fn color_subresource_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange::default()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .base_mip_level(0)
        .level_count(1)
        .base_array_layer(0)
        .layer_count(1)
}

#[inline]
fn image_copy_region(
    buffer_offset: u64,
    x: u16,
    y: u16,
    w: u16,
    h: u16,
) -> vk::BufferImageCopy {
    vk::BufferImageCopy::default()
        .buffer_offset(buffer_offset)
        .buffer_row_length(0) // tightly packed — same as bytes_per_row = w * bpp
        .buffer_image_height(0) // tightly packed
        .image_subresource(
            vk::ImageSubresourceLayers::default()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .mip_level(0)
                .base_array_layer(0)
                .layer_count(1),
        )
        .image_offset(vk::Offset3D {
            x: x as i32,
            y: y as i32,
            z: 0,
        })
        .image_extent(vk::Extent3D {
            width: w as u32,
            height: h as u32,
            depth: 1,
        })
}

fn create_bg_descriptor_set_layout(device: &ash::Device) -> vk::DescriptorSetLayout {
    let bindings = [
        vk::DescriptorSetLayoutBinding::default()
            .binding(0)
            .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT),
        vk::DescriptorSetLayoutBinding::default()
            .binding(1)
            .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::FRAGMENT),
    ];
    let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    unsafe {
        device
            .create_descriptor_set_layout(&info, None)
            .expect("create_descriptor_set_layout(grid.bg)")
    }
}

fn create_bg_descriptor_pool(device: &ash::Device) -> vk::DescriptorPool {
    let sizes = [
        vk::DescriptorPoolSize {
            ty: vk::DescriptorType::UNIFORM_BUFFER,
            descriptor_count: FRAMES_IN_FLIGHT as u32,
        },
        vk::DescriptorPoolSize {
            ty: vk::DescriptorType::STORAGE_BUFFER,
            descriptor_count: FRAMES_IN_FLIGHT as u32,
        },
    ];
    let info = vk::DescriptorPoolCreateInfo::default()
        .max_sets(FRAMES_IN_FLIGHT as u32)
        .pool_sizes(&sizes);
    unsafe {
        device
            .create_descriptor_pool(&info, None)
            .expect("create_descriptor_pool(grid.bg)")
    }
}

fn create_text_uniform_descriptor_set_layout(
    device: &ash::Device,
) -> vk::DescriptorSetLayout {
    let bindings = [vk::DescriptorSetLayoutBinding::default()
        .binding(0)
        .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
        .descriptor_count(1)
        .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)];
    let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    unsafe {
        device
            .create_descriptor_set_layout(&info, None)
            .expect("create_descriptor_set_layout(grid.text uniform)")
    }
}

/// One combined-image-sampler binding — the single atlas page that
/// each draw binds. With page-list atlases there's no longer a
/// grayscale-vs-color binding split; the bound *page* implies the
/// kind, and the text pipeline picks sampling mode from a push
/// constant.
fn create_text_atlas_descriptor_set_layout(
    device: &ash::Device,
) -> vk::DescriptorSetLayout {
    let bindings = [vk::DescriptorSetLayoutBinding::default()
        .binding(0)
        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
        .descriptor_count(1)
        .stage_flags(vk::ShaderStageFlags::FRAGMENT)];
    let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    unsafe {
        device
            .create_descriptor_set_layout(&info, None)
            .expect("create_descriptor_set_layout(grid.text atlas)")
    }
}

/// Pool sized for the uniform sets + every page either atlas can ever
/// allocate (`MAX_PAGES × 2`). Pages are never freed during the
/// renderer's lifetime, so the pool is sized once and never grown.
fn create_text_descriptor_pool(device: &ash::Device) -> vk::DescriptorPool {
    let max_atlas_sets = (MAX_PAGES * 2) as u32;
    let sizes = [
        vk::DescriptorPoolSize {
            ty: vk::DescriptorType::UNIFORM_BUFFER,
            descriptor_count: FRAMES_IN_FLIGHT as u32,
        },
        vk::DescriptorPoolSize {
            ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
            descriptor_count: max_atlas_sets,
        },
    ];
    let info = vk::DescriptorPoolCreateInfo::default()
        .max_sets(FRAMES_IN_FLIGHT as u32 + max_atlas_sets)
        .pool_sizes(&sizes);
    unsafe {
        device
            .create_descriptor_pool(&info, None)
            .expect("create_descriptor_pool(grid.text)")
    }
}

fn allocate_descriptor_sets(
    device: &ash::Device,
    pool: vk::DescriptorPool,
    layout: vk::DescriptorSetLayout,
) -> [vk::DescriptorSet; FRAMES_IN_FLIGHT] {
    let layouts = [layout; FRAMES_IN_FLIGHT];
    let info = vk::DescriptorSetAllocateInfo::default()
        .descriptor_pool(pool)
        .set_layouts(&layouts);
    let sets = unsafe {
        device
            .allocate_descriptor_sets(&info)
            .expect("allocate_descriptor_sets")
    };
    let mut out = [vk::DescriptorSet::null(); FRAMES_IN_FLIGHT];
    out.copy_from_slice(&sets);
    out
}

fn allocate_one_descriptor_set(
    device: &ash::Device,
    pool: vk::DescriptorPool,
    layout: vk::DescriptorSetLayout,
) -> vk::DescriptorSet {
    let layouts = [layout];
    let info = vk::DescriptorSetAllocateInfo::default()
        .descriptor_pool(pool)
        .set_layouts(&layouts);
    unsafe {
        device
            .allocate_descriptor_sets(&info)
            .expect("allocate_descriptor_sets(one)")[0]
    }
}

fn update_bg_descriptor_set(
    device: &ash::Device,
    set: vk::DescriptorSet,
    uniform: &VulkanBuffer,
    cells: &VulkanBuffer,
) {
    let uniform_info = vk::DescriptorBufferInfo::default()
        .buffer(uniform.handle())
        .offset(0)
        .range(uniform.size());
    let uniform_infos = [uniform_info];
    let cells_info = vk::DescriptorBufferInfo::default()
        .buffer(cells.handle())
        .offset(0)
        .range(cells.size());
    let cells_infos = [cells_info];

    let writes = [
        vk::WriteDescriptorSet::default()
            .dst_set(set)
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
            .buffer_info(&uniform_infos),
        vk::WriteDescriptorSet::default()
            .dst_set(set)
            .dst_binding(1)
            .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
            .buffer_info(&cells_infos),
    ];
    unsafe {
        device.update_descriptor_sets(&writes, &[]);
    }
}

fn update_text_uniform_descriptor_set(
    device: &ash::Device,
    set: vk::DescriptorSet,
    uniform: &VulkanBuffer,
) {
    let uniform_info = vk::DescriptorBufferInfo::default()
        .buffer(uniform.handle())
        .offset(0)
        .range(uniform.size());
    let uniform_infos = [uniform_info];
    let writes = [vk::WriteDescriptorSet::default()
        .dst_set(set)
        .dst_binding(0)
        .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
        .buffer_info(&uniform_infos)];
    unsafe {
        device.update_descriptor_sets(&writes, &[]);
    }
}

fn create_pipeline_layout(
    device: &ash::Device,
    set_layouts: &[vk::DescriptorSetLayout],
    push_constant_ranges: &[vk::PushConstantRange],
) -> vk::PipelineLayout {
    let info = vk::PipelineLayoutCreateInfo::default()
        .set_layouts(set_layouts)
        .push_constant_ranges(push_constant_ranges);
    unsafe {
        device
            .create_pipeline_layout(&info, None)
            .expect("create_pipeline_layout(grid)")
    }
}

fn create_sampler(device: &ash::Device) -> vk::Sampler {
    // Nearest filter + clamp-to-edge — matches Metal's
    // `filter::nearest, address::clamp_to_edge`. Not used for
    // sampling per se (we use `texelFetch` in the fragment shader),
    // but the COMBINED_IMAGE_SAMPLER descriptor still requires a
    // sampler object.
    let info = vk::SamplerCreateInfo::default()
        .mag_filter(vk::Filter::NEAREST)
        .min_filter(vk::Filter::NEAREST)
        .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
        .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE);
    unsafe {
        device
            .create_sampler(&info, None)
            .expect("create_sampler(grid.text)")
    }
}

fn create_bg_pipeline(
    device: &ash::Device,
    pipeline_cache: vk::PipelineCache,
    layout: vk::PipelineLayout,
    color_format: vk::Format,
) -> vk::Pipeline {
    build_pipeline(
        device,
        pipeline_cache,
        layout,
        color_format,
        BG_VERT_SPV,
        BG_FRAG_SPV,
        &[], // no vertex bindings
        &[],
        vk::PrimitiveTopology::TRIANGLE_LIST,
        BlendMode::Premultiplied, // bg uses src=SRC_ALPHA
    )
}

fn create_text_pipeline(
    device: &ash::Device,
    pipeline_cache: vk::PipelineCache,
    layout: vk::PipelineLayout,
    color_format: vk::Format,
) -> vk::Pipeline {
    let bindings = [vk::VertexInputBindingDescription::default()
        .binding(0)
        .stride(std::mem::size_of::<CellText>() as u32)
        .input_rate(vk::VertexInputRate::INSTANCE)];
    let attrs = [
        // 0: glyph_pos uvec2 @ 0
        vk::VertexInputAttributeDescription::default()
            .location(0)
            .binding(0)
            .format(vk::Format::R32G32_UINT)
            .offset(0),
        // 1: glyph_size uvec2 @ 8
        vk::VertexInputAttributeDescription::default()
            .location(1)
            .binding(0)
            .format(vk::Format::R32G32_UINT)
            .offset(8),
        // 2: bearings ivec2 @ 16 (stored as i16x2)
        vk::VertexInputAttributeDescription::default()
            .location(2)
            .binding(0)
            .format(vk::Format::R16G16_SINT)
            .offset(16),
        // 3: grid_pos uvec2 @ 20 (stored as u16x2)
        vk::VertexInputAttributeDescription::default()
            .location(3)
            .binding(0)
            .format(vk::Format::R16G16_UINT)
            .offset(20),
        // 4: color vec4 @ 24 (UNORM8)
        vk::VertexInputAttributeDescription::default()
            .location(4)
            .binding(0)
            .format(vk::Format::R8G8B8A8_UNORM)
            .offset(24),
        // `atlas` (offset 28) and `page` (offset 30) live in the
        // host-side struct so `render_text` can bucket cells, but the
        // shader doesn't read them — the bound descriptor set already
        // implies which page is sampled, and a push constant carries
        // the kind. No vertex attribute for either.
        // 5: bools u8 @ 29 → uint
        vk::VertexInputAttributeDescription::default()
            .location(5)
            .binding(0)
            .format(vk::Format::R8_UINT)
            .offset(29),
    ];
    build_pipeline(
        device,
        pipeline_cache,
        layout,
        color_format,
        TEXT_VERT_SPV,
        TEXT_FRAG_SPV,
        &bindings,
        &attrs,
        vk::PrimitiveTopology::TRIANGLE_STRIP,
        BlendMode::PremultipliedOverFromOne, // text fragment returns premultiplied
    )
}

#[derive(Copy, Clone)]
enum BlendMode {
    /// Source RGB factor = `SRC_ALPHA`. For shaders that return
    /// non-premultiplied RGBA + alpha (the bg pass).
    Premultiplied,
    /// Source RGB factor = `ONE`. For shaders that return
    /// already-premultiplied RGBA (the text pass — `in.color * mask_a`
    /// and the color atlas sample are both premultiplied).
    PremultipliedOverFromOne,
}

#[allow(clippy::too_many_arguments)]
fn build_pipeline(
    device: &ash::Device,
    pipeline_cache: vk::PipelineCache,
    layout: vk::PipelineLayout,
    color_format: vk::Format,
    vert_spv: &[u8],
    frag_spv: &[u8],
    vertex_bindings: &[vk::VertexInputBindingDescription],
    vertex_attrs: &[vk::VertexInputAttributeDescription],
    topology: vk::PrimitiveTopology,
    blend: BlendMode,
) -> vk::Pipeline {
    let vert = load_shader_module(device, vert_spv);
    let frag = load_shader_module(device, frag_spv);

    let entry = c"main";
    let stages = [
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::VERTEX)
            .module(vert)
            .name(entry),
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::FRAGMENT)
            .module(frag)
            .name(entry),
    ];

    let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
        .vertex_binding_descriptions(vertex_bindings)
        .vertex_attribute_descriptions(vertex_attrs);

    let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
        .topology(topology)
        .primitive_restart_enable(false);

    let viewport_state = vk::PipelineViewportStateCreateInfo::default()
        .viewport_count(1)
        .scissor_count(1);

    let rasterization = vk::PipelineRasterizationStateCreateInfo::default()
        .polygon_mode(vk::PolygonMode::FILL)
        .cull_mode(vk::CullModeFlags::NONE)
        .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
        .line_width(1.0);

    let multisample = vk::PipelineMultisampleStateCreateInfo::default()
        .rasterization_samples(vk::SampleCountFlags::TYPE_1);

    let (src_rgb, dst_rgb) = match blend {
        BlendMode::Premultiplied => (
            vk::BlendFactor::SRC_ALPHA,
            vk::BlendFactor::ONE_MINUS_SRC_ALPHA,
        ),
        BlendMode::PremultipliedOverFromOne => {
            (vk::BlendFactor::ONE, vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
        }
    };
    let blend_attachment = vk::PipelineColorBlendAttachmentState::default()
        .blend_enable(true)
        .src_color_blend_factor(src_rgb)
        .dst_color_blend_factor(dst_rgb)
        .color_blend_op(vk::BlendOp::ADD)
        .src_alpha_blend_factor(vk::BlendFactor::ONE)
        .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
        .alpha_blend_op(vk::BlendOp::ADD)
        .color_write_mask(vk::ColorComponentFlags::RGBA);
    let blend_attachments = [blend_attachment];
    let color_blend =
        vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachments);

    let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
    let dynamic_state =
        vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);

    let color_attachment_formats = [color_format];
    let mut rendering = vk::PipelineRenderingCreateInfo::default()
        .color_attachment_formats(&color_attachment_formats);

    let pipeline_info = vk::GraphicsPipelineCreateInfo::default()
        .stages(&stages)
        .vertex_input_state(&vertex_input)
        .input_assembly_state(&input_assembly)
        .viewport_state(&viewport_state)
        .rasterization_state(&rasterization)
        .multisample_state(&multisample)
        .color_blend_state(&color_blend)
        .dynamic_state(&dynamic_state)
        .layout(layout)
        .push_next(&mut rendering);

    let pipeline = unsafe {
        device
            .create_graphics_pipelines(pipeline_cache, &[pipeline_info], None)
            .map_err(|(_, e)| e)
            .expect("create_graphics_pipelines(grid)")[0]
    };

    unsafe {
        device.destroy_shader_module(vert, None);
        device.destroy_shader_module(frag, None);
    }
    pipeline
}

fn load_shader_module(device: &ash::Device, bytes: &[u8]) -> vk::ShaderModule {
    let code = ash::util::read_spv(&mut std::io::Cursor::new(bytes))
        .expect("read_spv (embedded grid shader is valid)");
    let info = vk::ShaderModuleCreateInfo::default().code(&code);
    unsafe {
        device
            .create_shader_module(&info, None)
            .expect("create_shader_module(grid)")
    }
}
