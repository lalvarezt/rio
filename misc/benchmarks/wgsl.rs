use std::{borrow::Cow, sync::mpsc};
use wgpu::util::DeviceExt;

fn read(device: &wgpu::Device, buffer: &wgpu::Buffer) -> Vec<u8> {
    let (tx, rx) = mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    rx.recv().unwrap().unwrap();
    let data = buffer.slice(..).get_mapped_range().unwrap().to_vec();
    buffer.unmap();
    data
}

fn main() {
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = wgpu::Backends::VULKAN;
    let instance = wgpu::Instance::new(descriptor);
    let adapter = futures::executor::block_on(instance.request_adapter(
        &wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        },
    ))
    .unwrap();
    let info = adapter.get_info();
    println!(
        "RIO_DEVICE {{\"adapter\":{:?},\"backend\":{:?}}}",
        info.name,
        format!("{:?}", info.backend)
    );
    let (device, queue) =
        futures::executor::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features: wgpu::Features::TIMESTAMP_QUERY,
            ..Default::default()
        }))
        .unwrap();
    let source = std::fs::read_to_string(
        std::path::Path::new(
            &std::env::var("RIO_BENCH_BASELINE")
                .expect("set RIO_BENCH_BASELINE to the upstream worktree"),
        )
        .join("sugarloaf/src/grid/shaders/grid.wgsl"),
    )
    .unwrap();
    let optimized =
        std::fs::read_to_string("sugarloaf/src/grid/shaders/grid.wgsl").unwrap();
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
        entries: &[0, 1].map(|binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: if binding == 0 {
                    wgpu::BufferBindingType::Uniform
                } else {
                    wgpu::BufferBindingType::Storage { read_only: true }
                },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }),
    });
    let pipeline_layout =
        device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
    let pipelines: Vec<_> = [source, optimized]
        .iter()
        .map(|source| {
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: None,
                source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(source)),
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: None,
                layout: Some(&pipeline_layout),
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
                        format: wgpu::TextureFormat::Bgra8Unorm,
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
            })
        })
        .collect();
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: 1536,
            height: 960,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Bgra8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());
    let query = device.create_query_set(&wgpu::QuerySetDescriptor {
        label: None,
        ty: wgpu::QueryType::Timestamp,
        count: 2,
    });
    let resolve = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 16,
        usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let timing = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 16,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let pixels = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 1536 * 960 * 4,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    for colorspace in 0..3u32 {
        for case in [
            "sparse",
            "opaque",
            "cursor",
            "transparent-rgb",
            "mixed",
            "faint",
            "cursor-half",
        ] {
            let name = format!("wgsl/background/{case}/{colorspace}");
            if std::env::var("RIO_BENCH_FILTER")
                .is_ok_and(|filter| !name.contains(&filter))
            {
                continue;
            }
            let mut cells = vec![0u32; 160 * 48];
            match case {
                "sparse" => {
                    for row in 0..48 {
                        cells[row * 160] = 0xa0c86428;
                    }
                }
                "opaque" => cells.fill(0xffc86428),
                "transparent-rgb" => cells.fill(0x00c86428),
                "mixed" => {
                    for (i, cell) in cells.iter_mut().enumerate() {
                        *cell = if i % 2 == 0 { 0xa0c86428 } else { 0x00ff2301 };
                    }
                }
                "faint" => cells.fill(0x01c86428),
                _ => (),
            }
            let mut uniforms = [0u32; 40];
            uniforms[28] = 9.6f32.to_bits();
            uniforms[29] = 20f32.to_bits();
            uniforms[30] = 160;
            uniforms[31] = 48;
            uniforms[32] = 4;
            uniforms[33] = 4;
            uniforms[39] = colorspace;
            if case.starts_with("cursor") {
                for (i, c) in [
                    0.8f32,
                    0.5,
                    0.3,
                    if case == "cursor-half" { 0.25 } else { 1.0 },
                ]
                .iter()
                .enumerate()
                {
                    uniforms[24 + i] = c.to_bits();
                }
            }
            let bytes = |values: &[u32]| {
                values
                    .iter()
                    .flat_map(|x| x.to_ne_bytes())
                    .collect::<Vec<_>>()
            };
            let ub = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: None,
                contents: &bytes(&uniforms),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let cb = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: None,
                contents: &bytes(&cells),
                usage: wgpu::BufferUsages::STORAGE,
            });
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: ub.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: cb.as_entire_binding(),
                    },
                ],
            });
            let mut expected = None;
            for repeat in 0..40 {
                for variant in if repeat % 2 == 0 { [0, 1] } else { [1, 0] } {
                    let mut encoder = device.create_command_encoder(&Default::default());
                    {
                        let attachments = [Some(wgpu::RenderPassColorAttachment {
                            view: &view,
                            depth_slice: None,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color {
                                    r: 0.1,
                                    g: 0.1,
                                    b: 0.15,
                                    a: 1.0,
                                }),
                                store: wgpu::StoreOp::Store,
                            },
                        })];
                        let mut pass =
                            encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                                label: None,
                                color_attachments: &attachments,
                                depth_stencil_attachment: None,
                                timestamp_writes: Some(wgpu::RenderPassTimestampWrites {
                                    query_set: &query,
                                    beginning_of_pass_write_index: Some(0),
                                    end_of_pass_write_index: Some(1),
                                }),
                                occlusion_query_set: None,
                                multiview_mask: None,
                            });
                        pass.set_pipeline(&pipelines[variant]);
                        pass.set_bind_group(0, &group, &[]);
                        for _ in 0..16 {
                            pass.draw(0..3, 0..1);
                        }
                    }
                    encoder.resolve_query_set(&query, 0..2, &resolve, 0);
                    encoder.copy_buffer_to_buffer(&resolve, 0, &timing, 0, 16);
                    if repeat == 0 {
                        encoder.copy_texture_to_buffer(
                            texture.as_image_copy(),
                            wgpu::TexelCopyBufferInfo {
                                buffer: &pixels,
                                layout: wgpu::TexelCopyBufferLayout {
                                    offset: 0,
                                    bytes_per_row: Some(1536 * 4),
                                    rows_per_image: None,
                                },
                            },
                            texture.size(),
                        );
                    }
                    queue.submit([encoder.finish()]);
                    let data = read(&device, &timing);
                    let start = u64::from_ne_bytes(data[..8].try_into().unwrap());
                    let end = u64::from_ne_bytes(data[8..].try_into().unwrap());
                    if repeat == 0 {
                        let data = read(&device, &pixels);
                        let hash = data.iter().fold(0xcbf29ce484222325u64, |a, b| {
                            (a ^ *b as u64).wrapping_mul(0x100000001b3)
                        });
                        if let Some(expected) = expected {
                            assert_eq!(hash, expected);
                        } else {
                            expected = Some(hash);
                        }
                        println!(
                            "RIO_CHECK {{\"case\":\"wgsl/background/{case}/{colorspace}\",\"variant\":{variant},\"value\":\"{hash:016x}\"}}"
                        );
                    }
                    if repeat >= 10 {
                        let ns = (end - start) as f64
                            * queue.get_timestamp_period() as f64
                            / 16.0;
                        println!(
                            "RIO_BENCH {{\"case\":\"wgsl/background/{case}/{colorspace}\",\"sample\":{repeat},\"variant\":{variant},\"iterations\":16,\"units\":1,\"ns\":{ns}}}"
                        );
                    }
                }
            }
        }
    }
}
