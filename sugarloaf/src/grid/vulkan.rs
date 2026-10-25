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

fn changed_cell_range(previous: &[CellText], updated: &[CellText]) -> Range<usize> {
    debug_assert_eq!(previous.len(), updated.len());
    let previous: &[[u8; std::mem::size_of::<CellText>()]] =
        bytemuck::cast_slice(previous);
    let updated: &[[u8; std::mem::size_of::<CellText>()]] = bytemuck::cast_slice(updated);
    let start = previous
        .iter()
        .zip(updated)
        .position(|(old, new)| old != new)
        .unwrap_or(updated.len());
    let end = previous[start..]
        .iter()
        .zip(&updated[start..])
        .rposition(|(old, new)| old != new)
        .map_or(start, |last| start + last + 1);
    start..end
}

/// Patch a row whose glyphs still belong to the same atlas buckets. Offsets
/// refer to the grouped staging layout, which can place adjacent cells far apart.
#[inline(never)]
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

/// Preserve first-seen bucket order while writing each glyph directly to staging.
#[cold]
#[inline(never)]
fn pack_mixed_rows(
    rows: &[Vec<CellText>],
    staging: &mut Vec<CellText>,
    buckets: &mut Vec<TextBucket>,
) {
    buckets.clear();
    let Some(first) = rows.iter().flatten().next() else {
        staging.clear();
        return;
    };
    // Keep the common one/two-bucket counts and write positions in locals.
    let first_key = (first.atlas, first.page);
    let mut second_key = None;
    let mut first_count = 0;
    let mut second_count = 0;
    for cell in rows.iter().flatten() {
        let key = (cell.atlas, cell.page);
        if key == first_key {
            first_count += 1;
        } else if second_key.is_none() || second_key == Some(key) {
            second_key = Some(key);
            second_count += 1;
        } else {
            pack_many_bucket_rows(rows, staging, buckets);
            return;
        }
    }
    buckets.push(TextBucket {
        kind: first_key.0,
        page: first_key.1,
        start: 0,
        count: first_count as u32,
    });
    if let Some((kind, page)) = second_key {
        buckets.push(TextBucket {
            kind,
            page,
            start: first_count as u32,
            count: second_count as u32,
        });
    }
    staging.resize(first_count + second_count, CellText::default());
    let (first, second) = staging.split_at_mut(first_count);
    let mut first = first.iter_mut();
    let mut second = second.iter_mut();
    for cell in rows.iter().flatten() {
        let destination = if (cell.atlas, cell.page) == first_key {
            first.next().unwrap()
        } else {
            second.next().unwrap()
        };
        *destination = *cell;
    }
}

#[cold]
#[inline(never)]
fn pack_many_bucket_rows(
    rows: &[Vec<CellText>],
    staging: &mut Vec<CellText>,
    buckets: &mut Vec<TextBucket>,
) {
    buckets.clear();
    // The two atlas kinds use byte-sized page indices. Other kinds retain
    // the general lookup behavior rather than indexing outside this table.
    let mut counts = [0usize; 512];
    for cell in rows.iter().flatten() {
        let key = ((cell.atlas as usize) << 8) | cell.page as usize;
        if let Some(count) = counts.get_mut(key) {
            if *count == 0 {
                buckets.push(TextBucket {
                    kind: cell.atlas,
                    page: cell.page,
                    start: 0,
                    count: 0,
                });
            }
            *count += 1;
        } else {
            let index = buckets
                .iter()
                .position(|bucket| (bucket.kind, bucket.page) == (cell.atlas, cell.page))
                .unwrap_or_else(|| {
                    buckets.push(TextBucket {
                        kind: cell.atlas,
                        page: cell.page,
                        start: 0,
                        count: 0,
                    });
                    buckets.len() - 1
                });
            buckets[index].count += 1;
        }
    }
    let mut total = 0;
    // Reuse the histogram for each bucket's next staging position.
    for bucket in buckets.iter_mut() {
        let key = ((bucket.kind as usize) << 8) | bucket.page as usize;
        if let Some(count) = counts.get_mut(key) {
            bucket.count = *count as u32;
            *count = total;
        }
        bucket.start = total as u32;
        total += bucket.count as usize;
    }
    // Keep initialized storage so same-sized rebuilds do not zero it again.
    staging.resize(total, CellText::default());
    let mut next: smallvec::SmallVec<[usize; MAX_PAGES * 2]> =
        buckets.iter().map(|bucket| bucket.start as usize).collect();
    for cell in rows.iter().flatten() {
        let key = ((cell.atlas as usize) << 8) | cell.page as usize;
        let offset = if let Some(offset) = counts.get_mut(key) {
            let current = *offset;
            *offset += 1;
            current
        } else {
            let index = buckets
                .iter()
                .position(|bucket| (bucket.kind, bucket.page) == (cell.atlas, cell.page))
                .unwrap();
            let current = next[index];
            next[index] += 1;
            current
        };
        staging[offset] = *cell;
    }
}

#[cfg(test)]
mod row_patch_tests {
    use super::*;

    #[test]
    fn mixed_packing_matches_first_seen_grouping_across_rebuilds() {
        let mut staging = vec![CellText::default(); 400];
        let mut buckets = Vec::new();
        let mut seed = 0x12345678u32;
        for iteration in 0..200 {
            let mut random = || {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                seed
            };
            let rows: Vec<Vec<_>> = (0..random() % 9)
                .map(|row| {
                    (0..random() % 40)
                        .map(|col| {
                            let key = random();
                            CellText {
                                atlas: if iteration % 3 == 0 {
                                    (key >> 8) as u8
                                } else {
                                    (key & 1) as u8
                                },
                                page: if iteration % 3 == 1 {
                                    0
                                } else {
                                    (key >> 16) as u8
                                },
                                grid_pos: [col as u16, row as u16],
                                color: key.to_le_bytes(),
                                ..Default::default()
                            }
                        })
                        .collect()
                })
                .collect();
            let mut expected: Vec<((u8, u8), Vec<CellText>)> = Vec::new();
            for cell in rows.iter().flatten() {
                let key = (cell.atlas, cell.page);
                let index = expected
                    .iter()
                    .position(|(old, _)| *old == key)
                    .unwrap_or_else(|| {
                        expected.push((key, Vec::new()));
                        expected.len() - 1
                    });
                expected[index].1.push(*cell);
            }
            pack_mixed_rows(&rows, &mut staging, &mut buckets);
            assert_eq!(
                staging,
                expected
                    .iter()
                    .flat_map(|(_, cells)| cells.iter())
                    .copied()
                    .collect::<Vec<_>>()
            );
            assert_eq!(buckets.len(), expected.len());
            let mut start = 0;
            for (bucket, (key, cells)) in buckets.iter().zip(&expected) {
                assert_eq!((bucket.kind, bucket.page), *key);
                assert_eq!(bucket.start as usize, start);
                assert_eq!(bucket.count as usize, cells.len());
                start += cells.len();
            }
        }
    }

    mod x11 {
        use crate as sugarloaf;
        include!("../../../misc/benchmarks/x11.rs");
    }

    #[test]
    #[ignore = "requires an X11 display and Vulkan device"]
    fn mixed_atlas_edits_reach_every_frame_slot() {
        for cols in [5, 160] {
            check_mixed_atlas_edits(cols);
        }
    }

    fn check_mixed_atlas_edits(cols: u32) {
        use crate::renderer::vulkan as renderer;
        use bytemuck::Zeroable;
        let window = x11::Window::new();
        let mut ctx = VulkanContext::new(window.handle());
        let mut grid = VulkanGridRenderer::new(&ctx, cols, 3);
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
                (0..cols as u16)
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
        u.grid_size = [cols, 3];
        u.cursor_pos = [u32::MAX; 2];
        let mut sparse_uploads = 0;
        let mut bounded_uploads = 0;
        for step in 0..30 {
            // Several edits can accumulate before a slot is visited. Also force
            // rebuilds by changing lengths, bucket membership, and empty rows.
            if step == 5 {
                rows[2].pop();
            }
            if step == 9 {
                rows[0][0].atlas = 1;
            }
            if step == 10 {
                rows[0][2].atlas ^= 1;
            }
            if step == 11 {
                rows[2].last_mut().unwrap().atlas ^= 1;
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
            // After structural churn, consume a bounded broad upload, then
            // extend the other slots' pending bounds into the last bucket.
            if step == 25 {
                rows[2][0].color[1] ^= 1;
            }
            for (row, cells) in rows.iter_mut().enumerate() {
                if (row == 0 && step != 25) || (step % 7 == 0 && row == 2) {
                    let len = cells.len();
                    for (index, cell) in cells.iter_mut().enumerate() {
                        if step % 5 == 0
                            || (step % 5 == 1 && index == 0)
                            || (step % 5 == 2 && index == len / 2)
                            || (step % 5 == 3 && index + 1 == len)
                            || (step % 5 == 4 && (index == 0 || index + 1 == len))
                        {
                            cell.color[0] = step as u8;
                        }
                    }
                }
                let mut bg = vec![CellBg::TRANSPARENT; cols as usize];
                bg[4].rgba = [step as u8, 0, 0, 255];
                let len = (step + row) % (cols as usize + 1);
                grid.write_row(row as u32, &bg[..len], cells);
                if step < 24 && step % 4 == 0 {
                    grid.clear_row(row as u32);
                    grid.clear_row(row as u32);
                    grid.write_row(row as u32, &[], cells);
                }
            }
            let visible: Vec<_> = grid
                .bg_cpu
                .chunks(cols as usize)
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
            if !grid.fg_staging_dirty
                && grid.fg_mixed_row_offsets_valid
                && grid.fg_dirty[frame.slot]
                    .as_ref()
                    .is_some_and(|range| range.len() < grid.fg_staging.len())
            {
                bounded_uploads += 1;
            }
            if !grid.fg_staging_dirty && grid.fg_dirty[frame.slot].is_none() {
                let ranges = &grid.fg_mixed_dirty[frame.slot];
                let nonempty = ranges.iter().filter(|range| !range.is_empty()).count();
                let copied: usize = ranges.iter().map(Range::len).sum();
                if nonempty > 1 && copied < grid.fg_staging.len() {
                    sparse_uploads += 1;
                }
                for (range, bucket) in ranges.iter().zip(&grid.fg_buckets) {
                    if !range.is_empty() {
                        assert!(range.start >= bucket.start as usize);
                        assert!(range.end <= (bucket.start + bucket.count) as usize);
                    }
                }
            }
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
        assert!(
            sparse_uploads > 0,
            "test must exercise separated partial uploads"
        );
        if cols == 160 {
            assert!(
                bounded_uploads > 0,
                "test must exercise bounded broad uploads"
            );
        }
        unsafe {
            ctx.device().device_wait_idle().unwrap();
        }
        grid.resize(2, 2);
        assert_eq!(grid.bg_visible_rows, 0);
        assert_eq!(grid.bg_row_visible, [false, false]);
    }

    #[test]
    #[ignore = "requires an X11 display and Vulkan device"]
    fn retained_pixels_match_compact_across_frame_slots() {
        use bytemuck::Zeroable;
        let window = x11::Window::new();
        let ctx = VulkanContext::new(window.handle());
        let mut reference = VulkanGridRenderer::new(&ctx, 4, 3);
        let mut retained = VulkanGridRenderer::new(&ctx, 4, 3);
        let mut glyphs = Vec::new();
        for grid in [&mut reference, &mut retained] {
            let mut slots = Vec::new();
            for kind in 0..2 {
                let bytes = if kind == 0 {
                    vec![160; 24 * 24]
                } else {
                    [30, 110, 190, 192].repeat(24 * 24)
                };
                let glyph = RasterizedGlyph {
                    width: 24,
                    height: 24,
                    bearing_x: 0,
                    bearing_y: 16,
                    bytes: &bytes,
                };
                let key = GlyphKey {
                    font_id: 0,
                    glyph_id: kind,
                    size_bucket: 64,
                };
                slots.push(
                    if kind == 0 {
                        grid.insert_glyph(key, glyph)
                    } else {
                        grid.insert_glyph_color(key, glyph)
                    }
                    .unwrap(),
                );
            }
            glyphs.push(slots);
        }
        let mut uniforms = GridUniforms::zeroed();
        uniforms.cell_size = [12.0, 12.0];
        uniforms.grid_size = [4, 3];
        uniforms.cursor_pos = [u32::MAX; 2];
        uniforms.projection = [
            2.0 / 64.0,
            0.0,
            0.0,
            0.0,
            0.0,
            2.0 / 64.0,
            0.0,
            0.0,
            0.0,
            0.0,
            1.0,
            0.0,
            -1.0,
            -1.0,
            0.0,
            1.0,
        ];
        // Reuse immutable payloads, move them, and exceed the seven-entry cache
        // to exercise replacement while other slots still contain old bytes.
        let make_rows =
            |slots: &[AtlasSlot], generation: usize| -> Vec<Arc<[CellText]>> {
                (0..3)
                    .map(|row| {
                        (0..3)
                            .map(|col| {
                                let kind = (row + col + generation) % 2;
                                let slot = slots[kind];
                                CellText {
                                    glyph_pos: [slot.x as u32, slot.y as u32],
                                    glyph_size: [24, 24],
                                    bearings: [-3, 16],
                                    grid_pos: [col as u16, 0],
                                    atlas: kind as u8,
                                    page: slot.page,
                                    color: [
                                        40 + generation as u8 * 3,
                                        180 - row as u8 * 40,
                                        70,
                                        170,
                                    ],
                                    ..Default::default()
                                }
                            })
                            .collect::<Vec<_>>()
                            .into()
                    })
                    .collect()
            };
        let mut payloads = [make_rows(&glyphs[0], 0), make_rows(&glyphs[1], 0)];
        for step in 0..24 {
            if step % 3 == 0 {
                payloads = [make_rows(&glyphs[0], step), make_rows(&glyphs[1], step)];
            }
            for (which, grid) in [&mut reference, &mut retained].into_iter().enumerate() {
                for row in 0..3 {
                    let cells = &payloads[which][(row + step) % 3];
                    if which == 1 && row != step % 3 {
                        grid.write_retained_row(row as u32, &[], cells);
                    } else {
                        let absolute: Vec<_> = cells
                            .iter()
                            .map(|cell| {
                                let mut cell = *cell;
                                cell.grid_pos[1] = row as u16;
                                cell
                            })
                            .collect();
                        grid.write_row(row as u32, &[], &absolute);
                    }
                }
                let mut cursor = payloads[which][0][0];
                cursor.color = [200, 30, 60, 210];
                grid.set_cursor(
                    &[cursor],
                    &[CellText {
                        grid_pos: [1, 1],
                        ..cursor
                    }],
                );
            }
            // Leave the other slots untouched until their next fence-safe turn.
            {
                let slot = step % FRAMES_IN_FLIGHT;
                let expected = read_text_pixels(&ctx, &mut reference, slot, &uniforms);
                let actual = read_text_pixels(&ctx, &mut retained, slot, &uniforms);
                assert!(
                    expected.chunks_exact(4).any(|pixel| pixel != [0, 0, 0, 0]),
                    "reference must draw visible pixels"
                );
                assert_eq!(actual, expected, "step={step} slot={slot}");
            }
        }
    }

    fn read_text_pixels(
        ctx: &VulkanContext,
        grid: &mut VulkanGridRenderer,
        slot: usize,
        uniforms: &GridUniforms,
    ) -> Vec<u8> {
        read_grid_pixels(ctx, grid, slot, uniforms, None, [0.0; 4])
    }

    fn read_grid_pixels(
        ctx: &VulkanContext,
        grid: &mut VulkanGridRenderer,
        slot: usize,
        uniforms: &GridUniforms,
        reference_bg: Option<vk::Pipeline>,
        clear: [f32; 4],
    ) -> Vec<u8> {
        // A separate attachment avoids requiring TRANSFER_SRC on the swapchain.
        let image = ctx.allocate_sampled_image(
            64,
            64,
            ctx.swapchain_format(),
            vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC,
        );
        let output = ctx.allocate_host_visible_buffer(
            64 * 64 * 4,
            vk::BufferUsageFlags::TRANSFER_DST,
        );
        let range = vk::ImageSubresourceRange::default()
            .aspect_mask(vk::ImageAspectFlags::COLOR)
            .level_count(1)
            .layer_count(1);
        ctx.submit_oneshot(|cmd| unsafe {
            let device = ctx.device();
            let barrier = |old, new, src, dst| {
                vk::ImageMemoryBarrier::default()
                    .image(image.handle())
                    .subresource_range(range)
                    .old_layout(old)
                    .new_layout(new)
                    .src_access_mask(src)
                    .dst_access_mask(dst)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            };
            device.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier(
                    vk::ImageLayout::UNDEFINED,
                    vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                    vk::AccessFlags::empty(),
                    vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
                )],
            );
            grid.prepare(ctx, cmd, slot);
            let attachments = [vk::RenderingAttachmentInfo::default()
                .image_view(image.view())
                .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .load_op(vk::AttachmentLoadOp::CLEAR)
                .store_op(vk::AttachmentStoreOp::STORE)
                .clear_value(vk::ClearValue {
                    color: vk::ClearColorValue { float32: clear },
                })];
            let rect = vk::Rect2D {
                offset: vk::Offset2D::default(),
                extent: vk::Extent2D {
                    width: 64,
                    height: 64,
                },
            };
            device.cmd_begin_rendering(
                cmd,
                &vk::RenderingInfo::default()
                    .render_area(rect)
                    .layer_count(1)
                    .color_attachments(&attachments),
            );
            let flipped = uniforms.projection[5] < 0.0;
            device.cmd_set_viewport(
                cmd,
                0,
                &[vk::Viewport {
                    x: 0.0,
                    y: if flipped { 64.0 } else { 0.0 },
                    width: 64.0,
                    height: if flipped { -64.0 } else { 64.0 },
                    min_depth: 0.0,
                    max_depth: 1.0,
                }],
            );
            device.cmd_set_scissor(cmd, 0, &[rect]);
            grid.render_bg(ctx, cmd, slot, uniforms);
            if let Some(pipeline) = reference_bg {
                // render_bg publishes the slot's cells/uniforms. Clear its draw
                // before replaying the original fullscreen shader independently.
                device.cmd_clear_attachments(
                    cmd,
                    &[vk::ClearAttachment::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .color_attachment(0)
                        .clear_value(attachments[0].clear_value)],
                    &[vk::ClearRect::default().rect(rect).layer_count(1)],
                );
                device.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipeline);
                device.cmd_bind_descriptor_sets(
                    cmd,
                    vk::PipelineBindPoint::GRAPHICS,
                    grid.pipelines.bg_pipeline_layout,
                    0,
                    &[grid.bg_descriptor_sets[slot]],
                    &[],
                );
                device.cmd_draw(cmd, 3, 1, 0, 0);
            }
            grid.render_text(ctx, cmd, slot, uniforms);
            device.cmd_end_rendering(cmd);
            device.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier(
                    vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
                    vk::AccessFlags::TRANSFER_READ,
                )],
            );
            device.cmd_copy_image_to_buffer(
                cmd,
                image.handle(),
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                output.handle(),
                &[vk::BufferImageCopy::default()
                    .image_subresource(
                        vk::ImageSubresourceLayers::default()
                            .aspect_mask(vk::ImageAspectFlags::COLOR)
                            .layer_count(1),
                    )
                    .image_extent(vk::Extent3D {
                        width: 64,
                        height: 64,
                        depth: 1,
                    })],
            );
            device.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::HOST,
                vk::DependencyFlags::empty(),
                &[],
                &[vk::BufferMemoryBarrier::default()
                    .buffer(output.handle())
                    .size(vk::WHOLE_SIZE)
                    .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                    .dst_access_mask(vk::AccessFlags::HOST_READ)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)],
                &[],
            );
        });
        // submit_oneshot waits for its fence before buffers/images are read/dropped.
        unsafe { std::slice::from_raw_parts(output.as_mut_ptr(), 64 * 64 * 4).to_vec() }
    }

    #[test]
    #[ignore = "requires an X11 display and Vulkan device"]
    fn background_pixels_match_fullscreen_reference() {
        use bytemuck::Zeroable;
        let window = x11::Window::new();
        let ctx = VulkanContext::new(window.handle());
        let mut grid = VulkanGridRenderer::new(&ctx, 4, 3);
        let reference = build_pipeline(
            ctx.device(),
            ctx.pipeline_cache(),
            grid.pipelines.bg_pipeline_layout,
            ctx.swapchain_format(),
            include_bytes!(concat!(env!("OUT_DIR"), "/grid_bg_reference.vert.spv")),
            include_bytes!(concat!(env!("OUT_DIR"), "/grid_bg_reference.frag.spv")),
            &[],
            &[],
            vk::PrimitiveTopology::TRIANGLE_LIST,
            BlendMode::Premultiplied,
        );
        let clear = [0.13, 0.27, 0.41, 0.67];
        for (colorspace, flipped) in (0..3).flat_map(|cs| [(cs, false), (cs, true)]) {
            for padding in 0..16 {
                for pattern in 0..3 {
                    for geometry in 0..5 {
                        let mut u = GridUniforms::zeroed();
                        u.projection = [
                            2.0 / 64.0,
                            0.0,
                            0.0,
                            0.0,
                            0.0,
                            2.0 / 64.0,
                            0.0,
                            0.0,
                            0.0,
                            0.0,
                            1.0,
                            0.0,
                            -1.0,
                            -1.0,
                            0.0,
                            1.0,
                        ];
                        if flipped {
                            u.projection[5] = -u.projection[5];
                            u.projection[13] = -u.projection[13];
                        }
                        u.grid_size = [4, 3];
                        u.input_colorspace = colorspace;
                        u.padding_extend = padding;
                        u.cell_size = [
                            [12.0, 14.0],
                            [10.5, 11.25],
                            [9.6, 13.6],
                            [0.25, 0.375],
                            [4096.25, 8192.5],
                        ][geometry];
                        let [left, top] = [
                            [0.0, 0.0],
                            [4.25, 3.75],
                            [-3.25, -1.5],
                            [0.4, 0.3],
                            [-4096.0, -8192.25],
                        ][geometry];
                        u.grid_padding = [top, 0.0, 0.0, left];
                        u.cursor_pos =
                            [[0, 0], [u32::MAX; 2], [3, 2], [1, 1], [1, 1]][geometry];
                        u.cursor_bg_color = [0.71, 0.29, 0.13, 0.63];
                        if geometry == 1 {
                            u.panel_rect = [2.25, 1.5, 54.5, 55.5];
                            u.panel_radius = 7.25;
                        }
                        for y in 0..3 {
                            let cells: Vec<_> = (0..4)
                                .map(|x| {
                                    let i = y * 4 + x;
                                    CellBg {
                                        rgba: if pattern == 0 && padding & 1 == 0 {
                                            [0; 4]
                                        } else if pattern == 0 || (pattern == 1 && i != 5)
                                        {
                                            [137, 61, 229, 0]
                                        } else {
                                            [
                                                17 + i * 19,
                                                241 - i * 13,
                                                23 + i * 11,
                                                [255, 128, 33][i as usize % 3],
                                            ]
                                        },
                                    }
                                })
                                .collect();
                            grid.write_row(y as u32, &cells, &[]);
                        }
                        let slot = (padding as usize + geometry) % FRAMES_IN_FLIGHT;
                        let expected = read_grid_pixels(
                            &ctx,
                            &mut grid,
                            slot,
                            &u,
                            Some(reference),
                            clear,
                        );
                        let actual =
                            read_grid_pixels(&ctx, &mut grid, slot, &u, None, clear);
                        assert_eq!(actual, expected,
                            "colorspace={colorspace} flipped={flipped} padding={padding} pattern={pattern} geometry={geometry}");
                    }
                }
            }
        }
        unsafe {
            ctx.device().destroy_pipeline(reference, None);
        }
    }

    #[test]
    #[ignore = "requires an X11 display and Vulkan device"]
    fn color_glyph_pixels_ignore_foreground_and_cursor_tint() {
        use bytemuck::Zeroable;
        let window = x11::Window::new();
        let ctx = VulkanContext::new(window.handle());
        let mut grid = VulkanGridRenderer::new(&ctx, 1, 1);
        let bytes = [30, 110, 190, 192].repeat(8 * 16);
        let slot = grid
            .insert_glyph_color(
                GlyphKey {
                    font_id: 0,
                    glyph_id: 1,
                    size_bucket: 64,
                },
                RasterizedGlyph {
                    width: 8,
                    height: 16,
                    bearing_x: 0,
                    bearing_y: 16,
                    bytes: &bytes,
                },
            )
            .unwrap();
        let mut u = GridUniforms::zeroed();
        u.projection = [
            2.0 / 64.0,
            0.0,
            0.0,
            0.0,
            0.0,
            2.0 / 64.0,
            0.0,
            0.0,
            0.0,
            0.0,
            1.0,
            0.0,
            -1.0,
            -1.0,
            0.0,
            1.0,
        ];
        u.grid_size = [1, 1];
        u.cell_size = [16.0, 16.0];
        u.cursor_pos = [0, 0];
        let mut expected = None;
        for cs in 0..3 {
            for (index, color) in [[255; 4], [0; 4], [213, 17, 91, 71]]
                .into_iter()
                .enumerate()
            {
                u.input_colorspace = cs;
                u.cursor_color = if index == 0 {
                    [0.0; 4]
                } else {
                    [0.9, 0.1, 0.7, 0.5]
                };
                grid.write_row(
                    0,
                    &[],
                    &[CellText {
                        glyph_pos: [slot.x as u32, slot.y as u32],
                        glyph_size: [slot.w as u32, slot.h as u32],
                        bearings: [0, 16],
                        color,
                        atlas: CellText::ATLAS_COLOR,
                        ..CellText::default()
                    }],
                );
                let pixels = read_text_pixels(&ctx, &mut grid, index, &u);
                if let Some(expected) = &expected {
                    assert_eq!(&pixels, expected, "colorspace={cs} tint={index}");
                } else {
                    assert!(pixels.iter().any(|&b| b != 0));
                    expected = Some(pixels);
                }
            }
        }
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

/// Bounded immutable payload cache. Replacing an entry only marks its three
/// buffers dirty: an in-flight slot's allocation is never touched or dropped.
/// Each slot is refreshed only when that slot becomes available for rendering.
#[derive(Default)]
struct RetainedTextRow {
    cells: Arc<[CellText]>,
    packed: Vec<CellText>,
    buckets: Vec<TextBucket>,
    buffers: [TextBuffer; FRAMES_IN_FLIGHT],
    dirty: [bool; FRAMES_IN_FLIGHT],
    used: u64,
}

impl RetainedTextRow {
    fn set_cells(&mut self, cells: &Arc<[CellText]>) {
        self.cells = cells.clone();
        self.buckets.clear();
        for cell in cells.iter() {
            if let Some(bucket) = self
                .buckets
                .iter_mut()
                .find(|b| (b.kind, b.page) == (cell.atlas, cell.page))
            {
                bucket.count += 1;
            } else {
                self.buckets.push(TextBucket {
                    kind: cell.atlas,
                    page: cell.page,
                    start: 0,
                    count: 1,
                });
            }
        }
        let mut offset = 0;
        for bucket in &mut self.buckets {
            bucket.start = offset;
            offset += bucket.count;
        }
        let mut positions: smallvec::SmallVec<[usize; 4]> =
            self.buckets.iter().map(|b| b.start as usize).collect();
        self.packed.resize(cells.len(), CellText::default());
        for cell in cells.iter() {
            let bucket = self
                .buckets
                .iter()
                .position(|b| (b.kind, b.page) == (cell.atlas, cell.page))
                .unwrap();
            self.packed[positions[bucket]] = *cell;
            positions[bucket] += 1;
        }
        self.dirty = [true; FRAMES_IN_FLIGHT];
    }
}

#[derive(Default)]
struct RetainedText {
    entries: Vec<RetainedTextRow>,
    placement: Vec<Option<usize>>,
    active: usize,
    clock: u64,
}

impl RetainedText {
    // At most 32 MiB of original/packed CPU payload and 48 MiB of GPU payload,
    // including power-of-two buffer rounding. Oversized/unusual rows fall back.
    const MAX_ENTRIES: usize = 256;
    const MAX_CELLS: usize = 2048;

    fn place(&mut self, row: usize, cells: &Arc<[CellText]>) -> bool {
        if row >= self.placement.len() || cells.len() > Self::MAX_CELLS {
            return false;
        }
        let existing = self
            .entries
            .iter()
            .position(|entry| Arc::ptr_eq(&entry.cells, cells));
        let index = if let Some(index) = existing {
            index
        } else {
            // Keep up to one previous viewport for scroll reuse. Never replace
            // an entry referenced by another visible row (duplicates may share).
            let limit =
                (self.placement.len().saturating_mul(2) + 1).min(Self::MAX_ENTRIES);
            let index = if self.entries.len() < limit {
                self.entries.push(RetainedTextRow::default());
                self.entries.len() - 1
            } else if let Some(index) = (0..self.entries.len())
                .filter(|index| {
                    !self
                        .placement
                        .iter()
                        .enumerate()
                        .any(|(y, entry)| y != row && *entry == Some(*index))
                })
                .min_by_key(|&index| self.entries[index].used)
            {
                index
            } else {
                return false;
            };
            self.entries[index].set_cells(cells);
            index
        };
        self.clock = self.clock.wrapping_add(1);
        self.entries[index].used = self.clock;
        if self.placement[row].replace(index).is_none() {
            self.active += 1;
        }
        true
    }

    fn unplace(&mut self, row: usize) {
        if self.placement[row].take().is_some() {
            self.active -= 1;
        }
    }

    fn invalidate(&mut self) {
        self.placement.fill(None);
        self.active = 0;
        for entry in &mut self.entries {
            entry.cells = Arc::default();
            entry.packed.clear();
            entry.buckets.clear();
            entry.dirty = [true; FRAMES_IN_FLIGHT];
        }
    }
}

#[cfg(test)]
mod retained_text_tests {
    use super::*;

    fn cells(keys: &[(u8, u8)]) -> Arc<[CellText]> {
        keys.iter()
            .enumerate()
            .map(|(x, &(atlas, page))| CellText {
                grid_pos: [x as u16, 0],
                atlas,
                page,
                ..Default::default()
            })
            .collect()
    }

    #[test]
    fn moves_and_duplicate_rows_share_payload_without_uploads() {
        let mut retained = RetainedText {
            placement: vec![None; 3],
            ..Default::default()
        };
        let a = cells(&[(0, 0); 2]);
        let b = cells(&[(0, 0); 4]);
        assert!(retained.place(0, &a));
        assert!(retained.place(1, &b));
        for entry in &mut retained.entries {
            entry.dirty.fill(false);
        }
        let a_index = retained.placement[0];
        let b_index = retained.placement[1];
        assert!(retained.place(2, &a));
        assert!(retained.place(0, &b));
        assert!(retained.place(1, &a));
        assert_eq!(retained.placement, [b_index, a_index, a_index]);
        assert_eq!(retained.active, 3);
        assert_eq!(retained.entries.len(), 2);
        assert!(retained
            .entries
            .iter()
            .all(|entry| entry.dirty == [false; FRAMES_IN_FLIGHT]));
        assert!(Arc::ptr_eq(&retained.entries[a_index.unwrap()].cells, &a));
        assert!(a.iter().all(|cell| cell.grid_pos[1] == 0));
        retained.unplace(1);
        retained.unplace(1);
        assert_eq!(retained.active, 2);
        assert!(retained.place(1, &a));
        assert_eq!(retained.active, 3);
        retained.invalidate();
        assert_eq!(retained.active, 0);
    }

    #[test]
    fn changed_lengths_and_atlas_runs_propagate_to_each_slot_independently() {
        assert_eq!(FRAMES_IN_FLIGHT, 3);
        let mut retained = RetainedText {
            placement: vec![None],
            ..Default::default()
        };
        let old = cells(&[(0, 0); 2]);
        let changed = cells(&[(0, 1), (1, 0), (0, 1)]);
        assert!(retained.place(0, &old));
        let index = retained.placement[0].unwrap();
        // Model slots completing in a different order, with a content change
        // between completions. Unavailable slots must still receive the latest row.
        retained.entries[index].dirty[2] = false;
        assert!(retained.place(0, &changed));
        let index = retained.placement[0].unwrap();
        assert_eq!(retained.entries[index].dirty, [true; 3]);
        for slot in [1, 0, 2] {
            let entry = &mut retained.entries[index];
            assert!(entry.dirty[slot]);
            assert_eq!(entry.cells.len(), 3);
            entry.dirty[slot] = false;
        }
        assert_eq!(retained.entries[index].dirty, [false; 3]);
        let runs: Vec<_> = text_runs(&changed, 0)
            .map(|run| (run.kind, run.page, run.start, run.count))
            .collect();
        assert_eq!(runs, [(0, 1, 0, 1), (1, 0, 1, 1), (0, 1, 2, 1)]);
        assert_eq!(
            retained.entries[0].cells.len(),
            2,
            "changing one row does not repack an older payload"
        );
    }

    #[test]
    fn eviction_is_bounded_and_clear_invalidates_all_placements() {
        let mut retained = RetainedText {
            placement: vec![None; 2],
            ..Default::default()
        };
        for len in 1..20 {
            assert!(retained.place(0, &cells(&vec![(0, 0); len])));
        }
        assert_eq!(retained.entries.len(), 5);
        let oversized = cells(&vec![(0, 0); RetainedText::MAX_CELLS + 1]);
        assert!(!retained.place(1, &oversized));
        retained.invalidate();
        assert!(retained.placement.iter().all(Option::is_none));
        assert!(retained
            .entries
            .iter()
            .all(|entry| entry.cells.is_empty() && entry.dirty == [true; 3]));
        let fresh = cells(&[(1, 2)]);
        assert!(retained.place(1, &fresh));
        let entry = &retained.entries[retained.placement[1].unwrap()];
        assert!(Arc::ptr_eq(&entry.cells, &fresh));
        assert_eq!(entry.dirty, [true; 3]);
    }
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

    fn upload_ranges(
        &mut self,
        ctx: &VulkanContext,
        cells: &[CellText],
        ranges: &mut [Range<usize>],
    ) {
        let mut pending = 0..0;
        for range in ranges {
            let range = std::mem::replace(range, cells.len()..0);
            if range.is_empty() {
                continue;
            }
            if pending.end == range.start {
                pending.end = range.end;
            } else {
                if !pending.is_empty() {
                    self.upload_range(ctx, cells, pending);
                }
                pending = range;
            }
        }
        if !pending.is_empty() {
            self.upload_range(ctx, cells, pending);
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
        let text_push_constants = [
            vk::PushConstantRange::default()
                .stage_flags(vk::ShaderStageFlags::FRAGMENT)
                .offset(0)
                .size(4),
            vk::PushConstantRange::default()
                .stage_flags(vk::ShaderStageFlags::VERTEX)
                .offset(4)
                .size(4),
        ];
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
    retained_text: RetainedText,
    fg_staging: Vec<CellText>,
    fg_staging_dirty: bool,
    /// Prefix offsets for a single-atlas layout. Atlas regrouping and row
    /// length changes rebuild the layout; same-length edits patch one range.
    fg_row_offsets: Vec<usize>,
    /// Per-cell staging offsets for mixed-atlas layouts. Constructed on the first
    /// same-layout edit, so structural churn only rebuilds the staging buffer.
    fg_mixed_row_offsets: Vec<Vec<usize>>,
    /// One staging span per atlas bucket, including empty spans for absent buckets.
    fg_mixed_row_ranges: Vec<smallvec::SmallVec<[Range<usize>; 4]>>,
    fg_mixed_row_offsets_valid: bool,
    fg_buffers: [TextBuffer; FRAMES_IN_FLIGHT],
    fg_dirty: [Option<Range<usize>>; FRAMES_IN_FLIGHT],
    fg_mixed_dirty: [smallvec::SmallVec<[Range<usize>; 4]>; FRAMES_IN_FLIGHT],
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
    atlas_generation: u64,

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
            retained_text: RetainedText {
                placement: vec![None; rows as usize],
                ..Default::default()
            },
            fg_staging: Vec::new(),
            fg_staging_dirty: true,
            fg_row_offsets: Vec::new(),
            fg_mixed_row_offsets: Vec::new(),
            fg_mixed_row_ranges: Vec::new(),
            fg_mixed_row_offsets_valid: false,
            fg_buffers: std::array::from_fn(|_| TextBuffer::default()),
            fg_dirty: std::array::from_fn(|_| None),
            fg_mixed_dirty: std::array::from_fn(|_| smallvec::SmallVec::new()),
            cursor_buffers: std::array::from_fn(|_| TextBuffer::default()),
            cursor_dirty: [true; FRAMES_IN_FLIGHT],
            fg_buckets: Vec::new(),
            text_descriptor_pool,
            text_uniform_descriptor_sets,
            atlas_grayscale,
            atlas_color,
            atlas_generation: super::next_atlas_generation(),
            needs_full_rebuild: true,
        }
    }

    #[inline]
    pub fn needs_full_rebuild(&self) -> bool {
        self.needs_full_rebuild
    }

    pub fn atlas_generation(&self) -> u64 {
        self.atlas_generation
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
        // resize waited for all slots above, so dropping their buffers is safe.
        self.retained_text = RetainedText {
            placement: vec![None; rows as usize],
            ..Default::default()
        };
        self.fg_staging_dirty = true;
        self.cursor_dirty = [true; FRAMES_IN_FLIGHT];
        self.needs_full_rebuild = true;
    }

    pub fn write_row(&mut self, row: u32, bg: &[CellBg], fg: &[CellText]) {
        if row >= self.rows {
            return;
        }
        self.retained_text.unplace(row as usize);
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
            let changed = if !self.fg_staging_dirty
                && self.fg_mixed_row_offsets_valid
                && slot.len() == fg.len()
            {
                changed_cell_range(slot, fg)
            } else {
                0..fg.len()
            };
            let mut mixed_range = None;
            let range = if !self.fg_staging_dirty
                && slot.len() == fg.len()
                && self.fg_row_offsets.len() == self.rows as usize + 1
                && self.fg_buckets.first().is_some_and(|bucket| {
                    fg[changed.clone()]
                        .iter()
                        .all(|cell| (cell.atlas, cell.page) == (bucket.kind, bucket.page))
                }) {
                let start = self.fg_row_offsets[row as usize];
                let range = start + changed.start..start + changed.end;
                self.fg_staging[range.clone()].copy_from_slice(&fg[changed.clone()]);
                Some(range)
            } else if !self.fg_staging_dirty && self.fg_mixed_row_offsets_valid {
                let range = self
                    .fg_mixed_row_offsets
                    .get(row as usize)
                    .filter(|offsets| offsets.len() == fg.len())
                    .and_then(|offsets| {
                        patch_mixed_row(
                            &mut self.fg_staging,
                            &offsets[changed.clone()],
                            &slot[changed.clone()],
                            &fg[changed.clone()],
                        )
                    });
                if range.is_some() && !self.fg_dirty.iter().all(Option::is_some) {
                    mixed_range = range;
                    None
                } else {
                    range
                }
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
            } else if mixed_range.is_none() {
                self.fg_staging_dirty = true;
            }
            if slot.len() == fg.len() {
                slot[changed.clone()].copy_from_slice(&fg[changed]);
            } else {
                slot.clear();
                slot.extend_from_slice(fg);
            }
            if let Some(range) = mixed_range {
                self.mark_mixed_row_dirty(row as usize, range);
            }
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

    /// Place immutable row-local glyphs without translating or uploading them
    /// again. Ordinary writes retain the compact atlas-bucket path.
    pub fn write_retained_row(&mut self, row: u32, bg: &[CellBg], fg: &Arc<[CellText]>) {
        if row >= self.rows {
            return;
        }
        // Clear any ordinary payload and update backgrounds through the existing
        // damage path. place() sets the retained mapping after write_row clears it.
        if fg.len() <= RetainedText::MAX_CELLS {
            self.write_row(row, bg, &[]);
            if self.retained_text.place(row as usize, fg) {
                return;
            }
        }
        let translated: Vec<_> = fg
            .iter()
            .map(|cell| {
                let mut cell = *cell;
                cell.grid_pos[1] = row as u16;
                cell
            })
            .collect();
        self.write_row(row, bg, &translated);
    }

    // Keep bucket bookkeeping from enlarging the row-copy hot path.
    #[inline(never)]
    fn mark_mixed_row_dirty(&mut self, row: usize, changed: Range<usize>) {
        let ranges = &self.fg_mixed_row_ranges[row];
        for (full, dirty) in self.fg_dirty.iter_mut().zip(&mut self.fg_mixed_dirty) {
            if let Some(full) = full {
                full.start = full.start.min(changed.start);
                full.end = full.end.max(changed.end);
            } else {
                for (dirty, range) in dirty.iter_mut().zip(ranges) {
                    let start = range.start.max(changed.start);
                    let end = range.end.min(changed.end);
                    if start < end {
                        dirty.start = dirty.start.min(start);
                        dirty.end = dirty.end.max(end);
                    }
                }
                // Broad edits use one copy and stop tracking bucket spans until upload.
                let limit = (self.fg_buckets[0].count as usize / 4).max(64);
                if dirty[0].len() >= limit {
                    *full = Some(dirty.iter().filter(|range| !range.is_empty()).fold(
                        self.fg_staging.len()..0,
                        |bounds, range| {
                            bounds.start.min(range.start)..bounds.end.max(range.end)
                        },
                    ));
                    dirty.fill(self.fg_staging.len()..0);
                }
            }
        }
    }

    #[cold]
    #[inline(never)]
    fn cache_mixed_row_offsets(&mut self) {
        let mut next: smallvec::SmallVec<[usize; 4]> = self
            .fg_buckets
            .iter()
            .map(|bucket| bucket.start as usize)
            .collect();
        self.fg_mixed_row_offsets
            .resize_with(self.rows as usize, Vec::new);
        self.fg_mixed_row_ranges
            .resize_with(self.rows as usize, smallvec::SmallVec::new);
        for ((row, offsets), ranges) in self.fg_rows[1..self.fg_rows.len() - 1]
            .iter()
            .zip(&mut self.fg_mixed_row_offsets)
            .zip(&mut self.fg_mixed_row_ranges)
        {
            offsets.clear();
            ranges.clear();
            ranges.extend(next.iter().map(|&start| start..start));
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
            for (range, &end) in ranges.iter_mut().zip(&next) {
                range.end = end;
            }
        }
        for dirty in &mut self.fg_mixed_dirty {
            dirty.clear();
            dirty.resize(self.fg_buckets.len(), self.fg_staging.len()..0);
        }
        self.fg_mixed_row_offsets_valid = true;
    }

    pub fn clear_row(&mut self, row: u32) {
        if row >= self.rows {
            return;
        }
        self.retained_text.unplace(row as usize);
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
        self.atlas_generation = super::next_atlas_generation();
        self.retained_text.invalidate();
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
        if self.atlas_grayscale.lookup(key).is_some() {
            self.atlas_generation = super::next_atlas_generation();
            self.retained_text.invalidate();
            self.needs_full_rebuild = true;
        }
        if let Some(slot) = self.atlas_grayscale.insert(key, glyph) {
            return Some(slot);
        }
        self.atlas_grayscale.clear();
        self.needs_full_rebuild = true;
        self.atlas_generation = super::next_atlas_generation();
        self.retained_text.invalidate();
        self.cursor_dirty = [true; FRAMES_IN_FLIGHT];
        self.fg_staging_dirty = true;
        self.atlas_grayscale.insert(key, glyph)
    }

    pub fn insert_glyph_color(
        &mut self,
        key: GlyphKey,
        glyph: RasterizedGlyph<'_>,
    ) -> Option<AtlasSlot> {
        if self.atlas_color.lookup(key).is_some() {
            self.atlas_generation = super::next_atlas_generation();
            self.retained_text.invalidate();
            self.needs_full_rebuild = true;
        }
        if let Some(slot) = self.atlas_color.insert(key, glyph) {
            return Some(slot);
        }
        self.atlas_color.clear();
        self.needs_full_rebuild = true;
        self.atlas_generation = super::next_atlas_generation();
        self.retained_text.invalidate();
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
    /// uniforms for `frame_slot` first, then issues instanced cell quads.
    /// Caller must have opened the dynamic-rendering pass +
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

        let (instances, first) = if self.bg_visible_rows == 0 {
            let [x, y] = uniforms.cursor_pos;
            if x >= self.cols || y >= self.rows {
                return;
            }
            (1, y * self.cols + x)
        } else {
            (self.cols * self.rows, 0)
        };

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
            self.shared.cmd_draw(cmd, 4, instances, 0, first);
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
                self.fg_staging.clear();
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
                pack_mixed_rows(rows, &mut self.fg_staging, &mut self.fg_buckets);
            }
            for dirty in &mut self.fg_dirty {
                *dirty = Some(0..self.fg_staging.len());
            }
            for dirty in &mut self.fg_mixed_dirty {
                dirty.clear();
            }
            self.fg_staging_dirty = false;
        }
        if let Some(range) = self.fg_dirty[slot].take() {
            self.fg_buffers[slot].upload_range(ctx, &self.fg_staging, range);
        } else {
            self.fg_buffers[slot].upload_ranges(
                ctx,
                &self.fg_staging,
                &mut self.fg_mixed_dirty[slot],
            );
        }
        let block = &self.fg_rows[0];
        let non_block = &self.fg_rows[content_end];
        if self.cursor_dirty[slot] {
            self.cursor_buffers[slot].upload(ctx, &[block, non_block]);
            self.cursor_dirty[slot] = false;
        }
        let has_retained = self.retained_text.active != 0;
        if has_retained {
            for index in self.retained_text.placement.iter().flatten().copied() {
                let entry = &mut self.retained_text.entries[index];
                if entry.dirty[slot] {
                    entry.buffers[slot].upload(ctx, &[&entry.packed]);
                    entry.dirty[slot] = false;
                }
            }
        }
        if self.fg_staging.is_empty()
            && block.is_empty()
            && non_block.is_empty()
            && !has_retained
        {
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
            self.draw_text(cmd, &self.cursor_buffers[slot], text_runs(block, 0), 0);
        }
        if !has_retained {
            if !self.fg_staging.is_empty() {
                self.draw_text(
                    cmd,
                    &self.fg_buffers[slot],
                    self.fg_buckets.iter().copied(),
                    0,
                );
            }
        } else {
            self.draw_content_with_retained(cmd, slot);
        }
        if !non_block.is_empty() {
            self.draw_text(
                cmd,
                &self.cursor_buffers[slot],
                text_runs(non_block, block.len() as u32),
                0,
            );
        }
    }

    fn draw_content_with_retained(&self, cmd: vk::CommandBuffer, slot: usize) {
        // Match pack_mixed_rows: first-seen atlas buckets, then row/cell order
        // within each bucket. Overhanging glyphs can overlap adjacent rows, so
        // drawing ordinary rows and retained rows in separate passes is invalid.
        let mut keys = smallvec::SmallVec::<[(u8, u8); 4]>::new();
        for row in 0..self.rows as usize {
            if let Some(index) = self.retained_text.placement[row] {
                for bucket in &self.retained_text.entries[index].buckets {
                    let key = (bucket.kind, bucket.page);
                    if !keys.contains(&key) {
                        keys.push(key);
                    }
                }
                continue;
            }
            let cells = match self.retained_text.placement[row] {
                Some(index) => &*self.retained_text.entries[index].cells,
                None => &self.fg_rows[row + 1],
            };
            for cell in cells {
                let key = (cell.atlas, cell.page);
                if !keys.contains(&key) {
                    keys.push(key);
                }
            }
        }
        for (kind, page) in keys {
            let mut ordinary_start = self
                .fg_buckets
                .iter()
                .find(|bucket| (bucket.kind, bucket.page) == (kind, page))
                .map_or(0, |bucket| bucket.start);
            for row in 0..self.rows as usize {
                if let Some(index) = self.retained_text.placement[row] {
                    let entry = &self.retained_text.entries[index];
                    if let Some(bucket) = entry
                        .buckets
                        .iter()
                        .find(|b| (b.kind, b.page) == (kind, page))
                    {
                        self.draw_text(
                            cmd,
                            &entry.buffers[slot],
                            std::iter::once(*bucket),
                            row as u32,
                        );
                    }
                } else {
                    let count = self.fg_rows[row + 1]
                        .iter()
                        .filter(|cell| (cell.atlas, cell.page) == (kind, page))
                        .count() as u32;
                    if count != 0 {
                        self.draw_text(
                            cmd,
                            &self.fg_buffers[slot],
                            std::iter::once(TextBucket {
                                kind,
                                page,
                                start: ordinary_start,
                                count,
                            }),
                            0,
                        );
                        ordinary_start += count;
                    }
                }
            }
        }
    }

    fn draw_text(
        &self,
        cmd: vk::CommandBuffer,
        buffer: &TextBuffer,
        buckets: impl Iterator<Item = TextBucket>,
        row_offset: u32,
    ) {
        unsafe {
            self.shared.cmd_push_constants(
                cmd,
                self.pipelines.text_pipeline_layout,
                vk::ShaderStageFlags::VERTEX,
                4,
                &row_offset.to_ne_bytes(),
            );
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
            .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT),
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
        vk::PrimitiveTopology::TRIANGLE_STRIP,
        BlendMode::Premultiplied,
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
