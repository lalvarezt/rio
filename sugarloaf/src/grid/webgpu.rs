// Copyright (c) 2023-present, Raphael Amorim.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! wgpu backend for the grid renderer.
//!
//! Data-side mirror of `super::metal`. Phase 1b added the bg pass;
//! Phase 1d here adds the text pass — per-instance vertex buffer of
//! `CellText`, grayscale glyph atlas, instanced quad draws.

use rustc_hash::FxHashMap;
use std::sync::Arc;

use super::atlas::{AtlasSlot, GlyphKey, RasterizedGlyph};
use super::cell::{CellBg, CellText, GridUniforms};
use crate::context::webgpu::WgpuContext;
use crate::renderer::image_cache::atlas::AtlasAllocator;

const CURSOR_ROW_SLOTS: usize = 2;
const ATLAS_SIZE: u32 = 2048;

pub struct WgpuGlyphAtlas {
    device: wgpu::Device,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    allocator: AtlasAllocator,
    slots: FxHashMap<GlyphKey, AtlasSlot>,
    queue: wgpu::Queue,
    bytes_per_pixel: u32,
}

impl WgpuGlyphAtlas {
    pub fn new_grayscale(device: &wgpu::Device, queue: wgpu::Queue) -> Self {
        Self::new_with_format(
            device,
            queue,
            wgpu::TextureFormat::R8Unorm,
            1,
            "grid.atlas_grayscale",
            ATLAS_SIZE,
        )
    }

    /// A single texel satisfies the binding until a nonempty color glyph needs
    /// the full atlas. Existing bind groups keep the old texture alive.
    pub fn new_color(device: &wgpu::Device, queue: wgpu::Queue) -> Self {
        Self::new_with_format(
            device,
            queue,
            wgpu::TextureFormat::Rgba8Unorm,
            4,
            "grid.atlas_color",
            1,
        )
    }

    fn new_with_format(
        device: &wgpu::Device,
        queue: wgpu::Queue,
        format: wgpu::TextureFormat,
        bytes_per_pixel: u32,
        label: &'static str,
        size: u32,
    ) -> Self {
        let texture = Self::create_texture(device, format, label, size);
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            device: device.clone(),
            texture,
            view,
            allocator: AtlasAllocator::new(ATLAS_SIZE as u16, ATLAS_SIZE as u16),
            slots: FxHashMap::default(),
            queue,
            bytes_per_pixel,
        }
    }

    fn create_texture(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        label: &'static str,
        size: u32,
    ) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        })
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
        if self.texture.width() != ATLAS_SIZE {
            self.texture = Self::create_texture(
                &self.device,
                self.texture.format(),
                "grid.atlas_color",
                ATLAS_SIZE,
            );
            self.view = self.texture.create_view(&Default::default());
        }
        let slot = AtlasSlot {
            x,
            y,
            w: glyph.width,
            h: glyph.height,
            bearing_x: glyph.bearing_x,
            bearing_y: glyph.bearing_y,
            page: 0,
        };
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: x as u32,
                    y: y as u32,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            glyph.bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(glyph.width as u32 * self.bytes_per_pixel),
                rows_per_image: Some(glyph.height as u32),
            },
            wgpu::Extent3d {
                width: glyph.width as u32,
                height: glyph.height as u32,
                depth_or_array_layers: 1,
            },
        );
        Some(slot)
    }

    pub fn clear(&mut self) {
        self.allocator.clear();
        self.slots.clear();
    }

    #[inline]
    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    pub(crate) fn size(&self) -> u32 {
        self.texture.width()
    }
}

/// Immutable grid state shared by panels using one context and target format.
pub(crate) struct GridPipelines {
    bg_layout: wgpu::BindGroupLayout,
    bg: wgpu::RenderPipeline,
    text_uniform_layout: wgpu::BindGroupLayout,
    text_atlas_layout: wgpu::BindGroupLayout,
    text: wgpu::RenderPipeline,
}

impl GridPipelines {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let bg_layout = create_bg_bind_group_layout(device);
        let text_uniform_layout = create_text_uniform_bgl(device);
        let text_atlas_layout = create_text_atlas_bgl(device);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("grid.wgsl"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/grid.wgsl").into()),
        });
        let bg = build_bg_pipeline(device, format, &bg_layout, &shader);
        let text = build_text_pipeline(
            device,
            format,
            &[Some(&text_uniform_layout), Some(&text_atlas_layout)],
            &shader,
        );
        Self {
            bg_layout,
            bg,
            text_uniform_layout,
            text_atlas_layout,
            text,
        }
    }
}

pub struct WgpuGridRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,

    cols: u32,
    rows: u32,

    bg_cpu: Vec<CellBg>,
    bg_buffer: wgpu::Buffer,
    bg_visible_rows: usize,

    fg_rows: Vec<Vec<CellText>>,
    fg_buffer: wgpu::Buffer,
    fg_capacity: usize,
    fg_staging: Vec<CellText>,

    /// GPU-resident instance count in `fg_buffer` from the last
    /// flush. Reused on Noop/CursorOnly frames to skip the concat
    /// and `write_buffer` call. Same pattern as `MetalGridRenderer`.
    fg_live_count: u32,
    /// Any row-level write since the last flush.
    fg_dirty: std::ops::Range<usize>,
    /// Cell range changed since the last background upload.
    bg_dirty: std::ops::Range<usize>,

    uniform_buffer: wgpu::Buffer,
    last_uniforms: Option<GridUniforms>,

    pipelines: Arc<GridPipelines>,
    bg_bind_group: wgpu::BindGroup,

    // Text uniforms and atlas textures use separate bind groups, so allocating
    // the first color glyph can replace its texture without touching uniforms.
    text_uniform_bg: wgpu::BindGroup,
    text_atlas_bg: wgpu::BindGroup,

    atlas_grayscale: WgpuGlyphAtlas,
    atlas_color: WgpuGlyphAtlas,

    /// Mirror of `MetalGridRenderer::needs_full_rebuild`. Set on
    /// `new` / `resize`, cleared via `mark_full_rebuild_done`.
    needs_full_rebuild: bool,
}

impl WgpuGridRenderer {
    pub fn new(ctx: &WgpuContext<'_>, cols: u32, rows: u32) -> Self {
        let device = ctx.device.clone();
        let queue = ctx.queue.clone();
        let pipelines = ctx
            .grid_pipelines
            .lock()
            .unwrap()
            .entry(ctx.format)
            .or_insert_with(|| Arc::new(GridPipelines::new(&device, ctx.format)))
            .clone();

        let bg_len = (cols as usize) * (rows as usize);
        let bg_cpu = vec![CellBg::TRANSPARENT; bg_len];
        let bg_buffer = alloc_bg_buffer(&device, cols, rows);

        let initial_fg_capacity = bg_len.saturating_add(CURSOR_ROW_SLOTS);
        let fg_buffer = alloc_fg_buffer(&device, initial_fg_capacity);

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("grid.uniforms"),
            size: std::mem::size_of::<GridUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // bg pipeline — uniforms + storage buffer.
        let bg_bind_group = create_bg_bind_group(
            &device,
            &pipelines.bg_layout,
            &uniform_buffer,
            &bg_buffer,
        );

        // text pipeline — uniforms in group(0), atlas textures in group(1).
        let atlas_grayscale = WgpuGlyphAtlas::new_grayscale(&device, queue.clone());
        let atlas_color = WgpuGlyphAtlas::new_color(&device, queue.clone());
        let text_uniform_bg = create_text_uniform_bg(
            &device,
            &pipelines.text_uniform_layout,
            &uniform_buffer,
        );
        let text_atlas_bg = create_text_atlas_bg(
            &device,
            &pipelines.text_atlas_layout,
            atlas_grayscale.view(),
            atlas_color.view(),
        );

        Self {
            device,
            queue,
            cols,
            rows,
            bg_cpu,
            bg_buffer,
            bg_visible_rows: 0,
            fg_rows: init_fg_rows(rows),
            fg_buffer,
            fg_capacity: initial_fg_capacity,
            fg_staging: Vec::new(),
            fg_live_count: 0,
            fg_dirty: 0..rows as usize + CURSOR_ROW_SLOTS,
            bg_dirty: 0..(cols as usize * rows as usize),
            uniform_buffer,
            last_uniforms: None,
            pipelines,
            bg_bind_group,
            text_uniform_bg,
            text_atlas_bg,
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
        self.cols = cols;
        self.rows = rows;
        let bg_len = (cols as usize) * (rows as usize);
        self.bg_cpu = vec![CellBg::TRANSPARENT; bg_len];
        self.bg_visible_rows = 0;
        self.bg_buffer = alloc_bg_buffer(&self.device, cols, rows);
        self.fg_rows = init_fg_rows(rows);
        let initial_fg_capacity = bg_len.saturating_add(CURSOR_ROW_SLOTS);
        self.fg_buffer = alloc_fg_buffer(&self.device, initial_fg_capacity);
        self.fg_capacity = initial_fg_capacity;
        self.needs_full_rebuild = true;
        self.fg_dirty = 0..self.fg_rows.len();
        self.bg_dirty = 0..bg_len;
        self.fg_staging.clear();
        self.fg_live_count = 0;
        self.bg_bind_group = create_bg_bind_group(
            &self.device,
            &self.pipelines.bg_layout,
            &self.uniform_buffer,
            &self.bg_buffer,
        );
    }

    pub fn write_row(&mut self, row: u32, bg: &[CellBg], fg: &[CellText]) {
        self.write_fg_row(row as usize + 1, fg);
        self.write_bg_row(row, bg);
    }

    pub fn clear_row(&mut self, row: u32) {
        self.write_fg_row(row as usize + 1, &[]);
        self.write_bg_row(row, &[]);
    }

    fn write_bg_row(&mut self, row: u32, bg: &[CellBg]) {
        if row >= self.rows {
            return;
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
            let was_visible = cpu.iter().any(|cell| *cell != CellBg::TRANSPARENT);
            cpu[..row_len].copy_from_slice(&bg[..row_len]);
            cpu[row_len..].fill(CellBg::TRANSPARENT);
            let is_visible = cpu.iter().any(|cell| *cell != CellBg::TRANSPARENT);
            self.bg_visible_rows =
                self.bg_visible_rows + is_visible as usize - was_visible as usize;
            extend_damage(
                &mut self.bg_dirty,
                row_start..row_start + self.cols as usize,
            );
        }
    }

    fn write_fg_row(&mut self, idx: usize, cells: &[CellText]) {
        let row_count = self.fg_rows.len();
        let Some(slot) = self.fg_rows.get_mut(idx) else {
            return;
        };
        if bytemuck::cast_slice::<_, u8>(slot.as_slice()) == bytemuck::cast_slice(cells) {
            return;
        }
        // A length change shifts every subsequent row in the packed GPU stream.
        let end = if slot.len() == cells.len() {
            idx + 1
        } else {
            row_count
        };
        slot.clear();
        slot.extend_from_slice(cells);
        extend_damage(&mut self.fg_dirty, idx..end);
    }

    pub fn set_cursor(&mut self, block: &[CellText], non_block: &[CellText]) {
        // A zero-area instance keeps text offsets stable while the block blinks.
        let hidden = [CellText::default()];
        let block = if block.is_empty() { &hidden } else { block };
        self.write_fg_row(0, block);
        let last = self.fg_rows.len().saturating_sub(1);
        if last > 0 {
            self.write_fg_row(last, non_block);
        }
    }

    pub fn lookup_glyph(&self, key: GlyphKey) -> Option<AtlasSlot> {
        self.atlas_grayscale.lookup(key)
    }

    pub fn lookup_glyph_color(&self, key: GlyphKey) -> Option<AtlasSlot> {
        self.atlas_color.lookup(key)
    }

    /// Drop every cached glyph and force a full rebuild. Called when
    /// the font library is swapped, since the new library reuses font ids.
    pub fn clear_atlas(&mut self) {
        self.atlas_grayscale.clear();
        self.atlas_color.clear();
        self.needs_full_rebuild = true;
        self.fg_dirty = 0..self.fg_rows.len();
        self.bg_dirty = 0..self.bg_cpu.len();
    }

    /// The wgpu atlas is fixed-size (no grow yet), so on atlas-full
    /// clear it once and rebuild every row.
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
        self.fg_dirty = 0..self.fg_rows.len();
        self.atlas_grayscale.insert(key, glyph)
    }

    pub fn insert_glyph_color(
        &mut self,
        key: GlyphKey,
        glyph: RasterizedGlyph<'_>,
    ) -> Option<AtlasSlot> {
        let deferred = self.atlas_color.texture.width() != ATLAS_SIZE;
        let slot = if let Some(slot) = self.atlas_color.insert(key, glyph) {
            Some(slot)
        } else {
            self.atlas_color.clear();
            self.needs_full_rebuild = true;
            self.fg_dirty = 0..self.fg_rows.len();
            self.atlas_color.insert(key, glyph)
        };
        if deferred && self.atlas_color.texture.width() == ATLAS_SIZE {
            self.text_atlas_bg = create_text_atlas_bg(
                &self.device,
                &self.pipelines.text_atlas_layout,
                self.atlas_grayscale.view(),
                self.atlas_color.view(),
            );
        }
        slot
    }

    /// Record the cell-bg pass, uploading changed uniforms and background cells.
    /// Pair with `render_text`, with any
    /// `kitty_below_text` images composited in between.
    pub fn render_bg(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        uniforms: &GridUniforms,
    ) {
        if self
            .last_uniforms
            .as_ref()
            .is_none_or(|last| bytemuck::bytes_of(last) != bytemuck::bytes_of(uniforms))
        {
            self.queue.write_buffer(
                &self.uniform_buffer,
                0,
                bytemuck::bytes_of(uniforms),
            );
            self.last_uniforms = Some(*uniforms);
        }

        if !self.bg_dirty.is_empty() {
            self.queue.write_buffer(
                &self.bg_buffer,
                (self.bg_dirty.start * std::mem::size_of::<CellBg>()) as u64,
                bytemuck::cast_slice(&self.bg_cpu[self.bg_dirty.clone()]),
            );
            self.bg_dirty = 0..0;
        }

        if self.bg_visible_rows == 0 && uniforms.cursor_bg_color[3] == 0.0 {
            return;
        }
        render_pass.set_pipeline(&self.pipelines.bg);
        render_pass.set_bind_group(0, &self.bg_bind_group, &[]);
        render_pass.draw(0..3, 0..1);
    }

    /// Record the cell-text pass, flushing only changed packed rows.
    pub fn render_text(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        _uniforms: &GridUniforms,
    ) {
        if !self.fg_dirty.is_empty() {
            let start = self.fg_rows[..self.fg_dirty.start]
                .iter()
                .map(Vec::len)
                .sum::<usize>();
            let rows = &self.fg_rows[self.fg_dirty.clone()];
            let end = start + rows.iter().map(Vec::len).sum::<usize>();
            if self.fg_dirty.end == self.fg_rows.len() {
                self.fg_staging.resize(end, CellText::default());
            }
            let mut offset = start;
            for row in rows {
                let end = offset + row.len();
                self.fg_staging[offset..end].copy_from_slice(row);
                offset = end;
            }
            let mut dirty = start..end;

            if self.fg_staging.len() > self.fg_capacity {
                let new_cap = self.fg_staging.len().next_power_of_two();
                self.fg_buffer = alloc_fg_buffer(&self.device, new_cap);
                self.fg_capacity = new_cap;
                dirty = 0..self.fg_staging.len();
            }
            if !dirty.is_empty() {
                self.queue.write_buffer(
                    &self.fg_buffer,
                    (dirty.start * std::mem::size_of::<CellText>()) as u64,
                    bytemuck::cast_slice(&self.fg_staging[dirty]),
                );
            }
            self.fg_live_count = self.fg_staging.len() as u32;
            self.fg_dirty = 0..0;
        }

        let instance_count = self.fg_live_count as usize;
        if instance_count == 0
            || (instance_count == 1 && self.fg_rows[0][0].glyph_size == [0, 0])
        {
            return;
        }

        render_pass.set_pipeline(&self.pipelines.text);
        render_pass.set_bind_group(0, &self.text_uniform_bg, &[]);
        render_pass.set_bind_group(1, &self.text_atlas_bg, &[]);
        render_pass.set_vertex_buffer(0, self.fg_buffer.slice(..));
        render_pass.draw(0..4, 0..instance_count as u32);
    }
}

// ---------- buffer / layout / pipeline helpers ----------

fn extend_damage(damage: &mut std::ops::Range<usize>, range: std::ops::Range<usize>) {
    if damage.is_empty() {
        *damage = range;
    } else {
        damage.start = damage.start.min(range.start);
        damage.end = damage.end.max(range.end);
    }
}

fn alloc_bg_buffer(device: &wgpu::Device, cols: u32, rows: u32) -> wgpu::Buffer {
    let size = (cols as u64)
        .saturating_mul(rows as u64)
        .saturating_mul(std::mem::size_of::<CellBg>() as u64)
        .max(std::mem::size_of::<CellBg>() as u64);
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("grid.bg_cells"),
        size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn alloc_fg_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
    let size = (capacity as u64)
        .saturating_mul(std::mem::size_of::<CellText>() as u64)
        .max(std::mem::size_of::<CellText>() as u64);
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("grid.fg_cells"),
        size,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn init_fg_rows(rows: u32) -> Vec<Vec<CellText>> {
    (0..(rows as usize + CURSOR_ROW_SLOTS))
        .map(|index| {
            if index == 0 {
                vec![CellText::default()]
            } else {
                Vec::new()
            }
        })
        .collect()
}

fn create_bg_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("grid.bg_bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: std::num::NonZeroU64::new(std::mem::size_of::<
                        GridUniforms,
                    >()
                        as u64),
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    })
}

fn create_bg_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    uniform_buffer: &wgpu::Buffer,
    bg_buffer: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("grid.bg_bg"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: bg_buffer.as_entire_binding(),
            },
        ],
    })
}

fn create_text_uniform_bgl(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("grid.text_uniform_bgl"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: std::num::NonZeroU64::new(std::mem::size_of::<
                    GridUniforms,
                >() as u64),
            },
            count: None,
        }],
    })
}

fn create_text_uniform_bg(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    uniform_buffer: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("grid.text_uniform_bg"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: uniform_buffer.as_entire_binding(),
        }],
    })
}

fn create_text_atlas_bgl(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("grid.text_atlas_bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
        ],
    })
}

fn create_text_atlas_bg(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    grayscale: &wgpu::TextureView,
    color: &wgpu::TextureView,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("grid.text_atlas_bg"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(grayscale),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(color),
            },
        ],
    })
}

fn premultiplied_blend() -> wgpu::BlendState {
    // Premultiplied-over, matching Text fragment returns
    // premultiplied RGBA (`in.color * mask_a` for grayscale, atlas
    // sample for color), so source RGB must be `One`.
    wgpu::BlendState {
        color: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        },
        alpha: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        },
    }
}

fn build_bg_pipeline(
    device: &wgpu::Device,
    color_format: wgpu::TextureFormat,
    bg_bgl: &wgpu::BindGroupLayout,
    shader: &wgpu::ShaderModule,
) -> wgpu::RenderPipeline {
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("grid.bg_pl"),
        bind_group_layouts: &[Some(bg_bgl)],
        immediate_size: 0,
    });

    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("grid.bg"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("grid_bg_vertex"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("grid_bg_fragment"),
            targets: &[Some(wgpu::ColorTargetState {
                format: color_format,
                blend: Some(premultiplied_blend()),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

fn build_text_pipeline(
    device: &wgpu::Device,
    color_format: wgpu::TextureFormat,
    bgls: &[Option<&wgpu::BindGroupLayout>],
    shader: &wgpu::ShaderModule,
) -> wgpu::RenderPipeline {
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("grid.text_pl"),
        bind_group_layouts: bgls,
        immediate_size: 0,
    });

    // Per-instance vertex buffer layout — mirrors `CellText`.
    let attrs = &[
        // @location(0) glyph_pos: vec2<u32> @ offset 0
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Uint32x2,
            offset: 0,
            shader_location: 0,
        },
        // @location(1) glyph_size: vec2<u32> @ offset 8
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Uint32x2,
            offset: 8,
            shader_location: 1,
        },
        // @location(2) bearings: vec2<i32> @ offset 16 — stored as i16x2,
        // widened to i32 in the shader via `Sint16x2`.
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Sint16x2,
            offset: 16,
            shader_location: 2,
        },
        // @location(3) grid_pos: vec2<u32> @ offset 20 — stored as u16x2.
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Uint16x2,
            offset: 20,
            shader_location: 3,
        },
        // @location(4) color: vec4<f32> @ offset 24 — UNorm8x4 → 0..1.
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Unorm8x4,
            offset: 24,
            shader_location: 4,
        },
        // @location(5) atlas: u32 @ offset 28 — u8 widened.
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Uint8,
            offset: 28,
            shader_location: 5,
        },
        // @location(6) bools: u32 @ offset 29 — u8 widened.
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Uint8,
            offset: 29,
            shader_location: 6,
        },
    ];
    let vbuf_layout = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<CellText>() as u64,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: attrs,
    };

    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("grid.text"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("grid_text_vertex"),
            buffers: &[Some(vbuf_layout)],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("grid_text_fragment"),
            targets: &[Some(wgpu::ColorTargetState {
                format: color_format,
                blend: Some(premultiplied_blend()),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleStrip,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}
