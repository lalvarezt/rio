#[path = "../../misc/benchmarks/support.rs"]
mod support;

#[cfg(target_os = "linux")]
#[path = "../../misc/benchmarks/x11.rs"]
mod x11;

#[cfg(target_os = "linux")]
fn main() {
    use sugarloaf::{
        context::webgpu::WgpuContext,
        grid::{webgpu::WgpuGridRenderer, CellBg, CellText, GridUniforms},
    };
    let window = x11::Window::new();
    let context =
        || WgpuContext::new(window.handle(), Default::default(), wgpu::Backends::VULKAN);
    // Keep a first context alive: the benchmark concerns subsequent windows.
    let ctx = context();
    println!(
        "RIO_DEVICE {{\"adapter\":\"{}\",\"backend\":\"WGPU Vulkan\"}}",
        ctx.adapter_info.name
    );
    support::measure("wgpu/second_window_context", 1, || {
        let next = context();
        next.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        std::hint::black_box(next);
        0
    });
    let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("branch benchmark"),
        size: wgpu::Extent3d {
            width: 1536,
            height: 960,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: ctx.format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());
    support::measure("wgpu/grid_construct", 160 * 48, || {
        std::hint::black_box(WgpuGridRenderer::new(&ctx, 160, 48));
        ctx.queue.submit(std::iter::empty());
        ctx.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        0
    });
    let mut grid = WgpuGridRenderer::new(&ctx, 160, 48);
    let mut uniforms: GridUniforms = unsafe { std::mem::zeroed() };
    uniforms.projection[15] = 1.0;
    uniforms.cell_size = [9.6, 20.0];
    uniforms.grid_size = [160, 48];
    uniforms.cursor_pos = [u32::MAX; 2];
    let backgrounds = vec![CellBg::TRANSPARENT; 160];
    let mut foreground: Vec<_> = (0..160)
        .map(|x| CellText {
            grid_pos: [x, 0],
            ..Default::default()
        })
        .collect();
    for row in 0..48 {
        grid.write_row(row, &backgrounds, &foreground);
    }
    for mode in [
        "unchanged",
        "one_row",
        "full",
        "uniform_change",
        "cursor_blink",
    ] {
        let mut sequence = 0u32;
        support::measure(&format!("wgpu/frame/{mode}"), 160 * 48, || {
            sequence += 1;
            match mode {
                "one_row" => {
                    foreground[0].color[0] ^= 1;
                    grid.write_row(24, &backgrounds, &foreground);
                }
                "full" => {
                    foreground[0].color[0] ^= 1;
                    for row in 0..48 {
                        grid.write_row(row, &backgrounds, &foreground);
                    }
                }
                "uniform_change" => uniforms.cursor_pos[0] = sequence % 160,
                "cursor_blink" => grid.set_cursor(
                    if sequence % 2 == 0 {
                        &foreground[..1]
                    } else {
                        &[]
                    },
                    &[],
                ),
                _ => (),
            }
            let mut encoder = ctx.device.create_command_encoder(&Default::default());
            {
                let attachments = [Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })];
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: None,
                    color_attachments: &attachments,
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                grid.render_bg(&mut pass, &uniforms);
                grid.render_text(&mut pass, &uniforms);
            }
            ctx.queue.submit([encoder.finish()]);
            ctx.device
                .poll(wgpu::PollType::wait_indefinitely())
                .unwrap();
            sequence as u64
        });
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    println!("Use the Windows runtime benchmark driver for native-window measurements");
}
