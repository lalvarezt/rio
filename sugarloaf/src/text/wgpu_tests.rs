use super::*;
use wgpu::util::DeviceExt;

fn render_panel_background(
    uniforms: crate::grid::GridUniforms,
    cells: &[u8],
    clear: wgpu::Color,
) -> Vec<u8> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter =
        futures::executor::block_on(instance.request_adapter(&Default::default()))
            .unwrap();
    let (device, queue) =
        futures::executor::block_on(adapter.request_device(&Default::default())).unwrap();
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: None,
        source: wgpu::ShaderSource::Wgsl(
            include_str!("../grid/shaders/grid.wgsl").into(),
        ),
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: None,
        layout: None,
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("grid_bg_vertex"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("grid_bg_fragment"),
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba8Unorm,
                blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    });
    let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: bytemuck::bytes_of(&uniforms),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let cells = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: cells,
        usage: wgpu::BufferUsages::STORAGE,
    });
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: cells.as_entire_binding(),
            },
        ],
    });
    let size = wgpu::Extent3d {
        width: 64,
        height: 64,
        depth_or_array_layers: 1,
    };
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&Default::default());
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 64 * 64 * 4,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(clear),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.draw(0..3, 0..1);
    }
    encoder.copy_texture_to_buffer(
        target.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(256),
                rows_per_image: Some(64),
            },
        },
        size,
    );
    queue.submit([encoder.finish()]);
    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |result| result.unwrap());
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let pixels = slice.get_mapped_range().unwrap();
    pixels.to_vec()
}

fn panel_uniforms() -> crate::grid::GridUniforms {
    let mut uniforms: crate::grid::GridUniforms = bytemuck::Zeroable::zeroed();
    uniforms.grid_padding = [16.0, 0.0, 0.0, 16.0];
    uniforms.grid_size = [2, 2];
    uniforms.cell_size = [16.0; 2];
    uniforms.padding_extend = crate::grid::GridUniforms::PADDING_EXTEND_HORIZONTAL;
    uniforms.input_colorspace = 1;
    uniforms.panel_rect = [8.0, 8.0, 48.0, 48.0];
    uniforms.panel_radius = 8.0;
    uniforms
}

#[test]
#[ignore = "requires a graphics adapter; run explicitly with --ignored"]
fn status_line_background_does_not_stretch_into_bottom_padding() {
    let mut cells = [40, 190, 70, 255].repeat(2);
    cells.extend([200, 40, 30, 255].repeat(2));
    // The shorter cells leave six extra pixels after the last complete row,
    // in addition to the pane's configured eight-pixel bottom padding.
    for cell_height in [16, 13] {
        let mut uniforms = panel_uniforms();
        uniforms.cell_size[1] = cell_height as f32;
        let pixels = render_panel_background(
            uniforms,
            &cells,
            wgpu::Color {
                r: 10.0 / 255.0,
                g: 20.0 / 255.0,
                b: 30.0 / 255.0,
                a: 1.0,
            },
        );
        let status_top = 16 + cell_height;
        let status_bottom = status_top + cell_height;
        for y in status_top..status_bottom {
            let offset = (y * 64 + 32) * 4;
            assert_eq!(
                &pixels[offset..offset + 4],
                &[200, 40, 30, 255],
                "status row height={cell_height} y={y}"
            );
        }
        for y in (8..16).chain(status_bottom..55) {
            let offset = (y * 64 + 32) * 4;
            assert_eq!(
                &pixels[offset..offset + 4],
                &[10, 20, 30, 255],
                "padding height={cell_height} y={y}"
            );
        }
    }
}

#[test]
#[ignore = "requires a graphics adapter; run explicitly with --ignored"]
fn rounded_panel_extends_edge_cells_without_painting_neighbors() {
    let mut uniforms = panel_uniforms();
    // Explicit extension remains supported for renderers that request it.
    uniforms.padding_extend |= crate::grid::GridUniforms::PADDING_EXTEND_UP
        | crate::grid::GridUniforms::PADDING_EXTEND_DOWN;
    let pixels = render_panel_background(
        uniforms,
        &[40, 190, 70, 128].repeat(4),
        wgpu::Color::TRANSPARENT,
    );
    for (x, y) in [(0, 32), (63, 32), (8, 8), (55, 55)] {
        let offset = (y * 64 + x) * 4;
        assert_eq!(
            &pixels[offset..offset + 4],
            &[0; 4],
            "outside/corner ({x},{y})"
        );
    }
    for (x, y) in [(9, 32), (32, 9), (32, 32), (54, 32), (32, 54)] {
        let offset = (y * 64 + x) * 4;
        assert_eq!(
            &pixels[offset..offset + 4],
            &[20, 95, 35, 128],
            "cell/padding ({x},{y})"
        );
    }
}

#[test]
#[ignore = "requires a graphics adapter; run explicitly with --ignored"]
fn color_icon_uses_promoted_atlas_on_first_and_cached_draw() {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = futures::executor::block_on(
        instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
    )
    .expect("graphics adapter for color-atlas regression");
    let (device, queue) = futures::executor::block_on(
        adapter.request_device(&wgpu::DeviceDescriptor::default()),
    )
    .unwrap();
    let mut text = Text::new(&FontLibrary::default());
    text.init_wgpu(&device, &queue, wgpu::TextureFormat::Rgba8Unorm);
    let size = wgpu::Extent3d {
        width: 64,
        height: 64,
        depth_or_array_layers: 1,
    };
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("color icon regression"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&Default::default());
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("color icon pixels"),
        size: 64 * 64 * 4,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    for frame in 0..2 {
        text.instances.clear();
        assert!(
            text.draw_icon("regression", 32.0, 32.0, 24.0, [255; 4], |side| {
                assert_eq!(frame, 0, "cached artwork must not be rasterized again");
                [40, 190, 70, 255].repeat(side as usize * side as usize)
            })
        );
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            text.render_wgpu(&mut pass, [64.0, 64.0]);
        }
        encoder.copy_texture_to_buffer(
            target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(64),
                },
            },
            size,
        );
        queue.submit([encoder.finish()]);
        let slice = readback.slice(..);
        let (send, receive) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            send.send(result).unwrap()
        });
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        receive.recv().unwrap().unwrap();
        let pixels = slice.get_mapped_range().unwrap();
        let center = (32 * 64 + 32) * 4;
        assert_eq!(
            &pixels[center..center + 4],
            &[40, 190, 70, 255],
            "frame {frame}"
        );
        drop(pixels);
        readback.unmap();
    }
}
