#[path = "../../misc/benchmarks/support.rs"]
mod support;

#[cfg(target_os = "linux")]
mod native {
    use super::support;
    use ash::vk;
    use raw_window_handle::{
        RawDisplayHandle, RawWindowHandle, XlibDisplayHandle, XlibWindowHandle,
    };
    use std::{
        ffi::{c_int, c_ulong, c_void},
        ptr::NonNull,
    };
    use sugarloaf::{
        context::vulkan::{VulkanContext, FRAMES_IN_FLIGHT},
        grid::{
            vulkan::VulkanGridRenderer, CellBg, CellText, GlyphKey, GridUniforms,
            RasterizedGlyph,
        },
        renderer::vulkan::{
            build_color_attachment, build_rendering_info,
            cmd_acquire_image_for_rendering, cmd_release_image_to_present,
        },
        SugarloafWindow, SugarloafWindowSize,
    };

    #[link(name = "X11")]
    unsafe extern "C" {
        fn XOpenDisplay(name: *const i8) -> *mut c_void;
        fn XDefaultScreen(display: *mut c_void) -> c_int;
        fn XDefaultRootWindow(display: *mut c_void) -> c_ulong;
        fn XCreateSimpleWindow(
            display: *mut c_void,
            parent: c_ulong,
            x: c_int,
            y: c_int,
            width: u32,
            height: u32,
            border: u32,
            border_color: c_ulong,
            bg: c_ulong,
        ) -> c_ulong;
        fn XMapWindow(display: *mut c_void, window: c_ulong) -> c_int;
        fn XSync(display: *mut c_void, discard: c_int) -> c_int;
        fn XDestroyWindow(display: *mut c_void, window: c_ulong) -> c_int;
        fn XCloseDisplay(display: *mut c_void) -> c_int;
    }

    pub fn run() {
        unsafe {
            let display = XOpenDisplay(std::ptr::null());
            assert!(
                !display.is_null(),
                "branch_gpu needs an X11 display; use misc/benchmarks/run.py"
            );
            let window = XCreateSimpleWindow(
                display,
                XDefaultRootWindow(display),
                0,
                0,
                1536,
                960,
                0,
                0,
                0,
            );
            XMapWindow(display, window);
            XSync(display, 0);
            {
                let mut ctx = VulkanContext::new(SugarloafWindow {
                    handle: RawWindowHandle::Xlib(XlibWindowHandle::new(window)),
                    display: RawDisplayHandle::Xlib(XlibDisplayHandle::new(
                        NonNull::new(display),
                        XDefaultScreen(display),
                    )),
                    size: SugarloafWindowSize {
                        width: 1536.0,
                        height: 960.0,
                    },
                    scale: 1.0,
                });
                let device = ctx.device().clone();
                let mut grid = VulkanGridRenderer::new(&ctx, 160, 48);
                // Color pages are lazy. The two-atlas case must bind an actual
                // allocated page even though its glyph quads have zero area.
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
                        bytes: &[255, 0, 0, 255],
                    },
                )
                .expect("color atlas fixture");
                let mut uniforms: GridUniforms = std::mem::zeroed();
                uniforms.projection[15] = 1.0;
                uniforms.cell_size = [9.6, 20.0];
                uniforms.grid_size = [160, 48];
                uniforms.cursor_pos = [u32::MAX; 2];
                let properties = ctx
                    .instance()
                    .get_physical_device_properties(ctx.physical_device());
                let adapter = std::ffi::CStr::from_ptr(properties.device_name.as_ptr())
                    .to_string_lossy();
                println!(
                    "RIO_DEVICE {{\"adapter\":\"{adapter}\",\"timestamp_period\":{}}}",
                    properties.limits.timestamp_period
                );
                let family = ctx
                    .instance()
                    .get_physical_device_queue_family_properties(ctx.physical_device())
                    .iter()
                    .position(|family| {
                        family.queue_flags.contains(vk::QueueFlags::GRAPHICS)
                    })
                    .unwrap() as u32;
                let pool = device
                    .create_command_pool(
                        &vk::CommandPoolCreateInfo::default()
                            .queue_family_index(family)
                            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                        None,
                    )
                    .unwrap();
                let command = device
                    .allocate_command_buffers(
                        &vk::CommandBufferAllocateInfo::default()
                            .command_pool(pool)
                            .command_buffer_count(1),
                    )
                    .unwrap()[0];
                let frame = ctx.acquire_frame().expect("acquire benchmark image");
                let bg = vec![CellBg::TRANSPARENT; 160];
                for buckets in [1, 2] {
                    let mut cells: Vec<_> = (0..160)
                        .map(|x| CellText {
                            grid_pos: [x, 0],
                            atlas: (x % buckets) as u8,
                            ..Default::default()
                        })
                        .collect();
                    for row in 0..48 {
                        grid.write_row(row, &bg, &cells);
                    }
                    for mode in ["full", "one_row", "rotating_slots", "unchanged"] {
                        let name = format!("gpu/record/{buckets}_atlas/{mode}");
                        let mut sequence = 0usize;
                        support::measure(&name, 160 * 48, || {
                            sequence += 1;
                            let slot = sequence % FRAMES_IN_FLIGHT;
                            if mode != "unchanged"
                                && (mode != "rotating_slots" || slot == 0)
                            {
                                cells[0].color[0] ^= 1;
                                if mode == "full" {
                                    for row in 0..48 {
                                        grid.write_row(row, &bg, &cells);
                                    }
                                } else {
                                    grid.write_row(0, &bg, &cells);
                                }
                            }
                            device
                                .reset_command_buffer(
                                    command,
                                    vk::CommandBufferResetFlags::empty(),
                                )
                                .unwrap();
                            device
                                .begin_command_buffer(
                                    command,
                                    &vk::CommandBufferBeginInfo::default(),
                                )
                                .unwrap();
                            let attachments =
                                [build_color_attachment(&frame, [0.1, 0.1, 0.15, 1.0])];
                            device.cmd_begin_rendering(
                                command,
                                &build_rendering_info(&frame, &attachments),
                            );
                            device.cmd_set_viewport(
                                command,
                                0,
                                &[vk::Viewport {
                                    x: 0.0,
                                    y: 0.0,
                                    width: 1536.0,
                                    height: 960.0,
                                    min_depth: 0.0,
                                    max_depth: 1.0,
                                }],
                            );
                            device.cmd_set_scissor(
                                command,
                                0,
                                &[vk::Rect2D {
                                    offset: vk::Offset2D { x: 0, y: 0 },
                                    extent: frame.extent,
                                }],
                            );
                            grid.render_bg(&ctx, command, slot, &uniforms);
                            grid.render_text(&ctx, command, slot, &uniforms);
                            device.cmd_end_rendering(command);
                            device.end_command_buffer(command).unwrap();
                            sequence as u64
                        });
                    }
                }
                // The CPU recording loop never submits its command buffer. A real
                // frame releases the acquired image before the GPU timing phase.
                grid.prepare(&ctx, frame.cmd_buffer, frame.slot);
                cmd_acquire_image_for_rendering(&device, frame.cmd_buffer, frame.image);
                let attachments = [build_color_attachment(&frame, [0.1, 0.1, 0.15, 1.0])];
                device.cmd_begin_rendering(
                    frame.cmd_buffer,
                    &build_rendering_info(&frame, &attachments),
                );
                device.cmd_end_rendering(frame.cmd_buffer);
                cmd_release_image_to_present(&device, frame.cmd_buffer, frame.image);
                ctx.present_frame(frame);
                device.device_wait_idle().unwrap();
                device.destroy_command_pool(pool, None);
                let query = device
                    .create_query_pool(
                        &vk::QueryPoolCreateInfo::default()
                            .query_type(vk::QueryType::TIMESTAMP)
                            .query_count(2),
                        None,
                    )
                    .unwrap();
                // Short batches can measure different idle GPU clock states.
                let draws = std::env::var("RIO_BENCH_GPU_DRAWS")
                    .ok()
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(256)
                    .max(1);
                for case in ["transparent", "sparse", "opaque", "cursor"] {
                    let name = format!("gpu/background/{case}");
                    if !support::selected(&name) {
                        continue;
                    }
                    let mut bg = vec![CellBg::TRANSPARENT; 160];
                    if case == "sparse" {
                        bg[0].rgba = [40, 100, 200, 160];
                    }
                    if case == "opaque" {
                        bg.fill(CellBg {
                            rgba: [40, 100, 200, 255],
                        });
                    }
                    for row in 0..48 {
                        grid.write_row(row, &bg, &[]);
                    }
                    uniforms.cursor_bg_color = if case == "cursor" {
                        [0.8, 0.5, 0.3, 1.0]
                    } else {
                        [0.0; 4]
                    };
                    uniforms.cursor_pos = if case == "cursor" {
                        [4, 4]
                    } else {
                        [u32::MAX; 2]
                    };
                    for sample in 0..21 {
                        let frame = ctx.acquire_frame().expect("acquire timestamp image");
                        let cmd = frame.cmd_buffer;
                        cmd_acquire_image_for_rendering(&device, cmd, frame.image);
                        device.cmd_reset_query_pool(cmd, query, 0, 2);
                        let attachments =
                            [build_color_attachment(&frame, [0.1, 0.1, 0.15, 1.0])];
                        device.cmd_begin_rendering(
                            cmd,
                            &build_rendering_info(&frame, &attachments),
                        );
                        device.cmd_set_viewport(
                            cmd,
                            0,
                            &[vk::Viewport {
                                x: 0.0,
                                y: 0.0,
                                width: 1536.0,
                                height: 960.0,
                                min_depth: 0.0,
                                max_depth: 1.0,
                            }],
                        );
                        device.cmd_set_scissor(
                            cmd,
                            0,
                            &[vk::Rect2D {
                                offset: vk::Offset2D { x: 0, y: 0 },
                                extent: frame.extent,
                            }],
                        );
                        device.cmd_write_timestamp(
                            cmd,
                            vk::PipelineStageFlags::TOP_OF_PIPE,
                            query,
                            0,
                        );
                        for _ in 0..draws {
                            grid.render_bg(&ctx, cmd, frame.slot, &uniforms);
                        }
                        device.cmd_write_timestamp(
                            cmd,
                            vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                            query,
                            1,
                        );
                        device.cmd_end_rendering(cmd);
                        cmd_release_image_to_present(&device, cmd, frame.image);
                        ctx.present_frame(frame);
                        device.device_wait_idle().unwrap();
                        let mut times = [0u64; 2];
                        device
                            .get_query_pool_results(
                                query,
                                0,
                                &mut times,
                                vk::QueryResultFlags::TYPE_64
                                    | vk::QueryResultFlags::WAIT,
                            )
                            .unwrap();
                        if sample >= 5 {
                            let ns = (times[1] - times[0]) as f64
                                * f64::from(properties.limits.timestamp_period)
                                / f64::from(draws);
                            println!("RIO_BENCH {{\"case\":\"{name}\",\"sample\":{},\"iterations\":{draws},\"units\":1,\"ns\":{ns}}}", sample-5);
                        }
                    }
                }
                device.destroy_query_pool(query, None);
            }
            XDestroyWindow(display, window);
            XCloseDisplay(display);
        }
    }
}

fn main() {
    #[cfg(target_os = "linux")]
    native::run();
    #[cfg(not(target_os = "linux"))]
    println!("Native Vulkan benchmarks apply to Linux only");
}
