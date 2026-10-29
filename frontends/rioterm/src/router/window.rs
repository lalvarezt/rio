use rio_backend::config::window::{Decorations, WindowMode};
use rio_backend::config::Config;
use rio_window::window::{
    CursorIcon, Fullscreen, Icon, ImePurpose, Window, WindowAttributes,
};

pub const LOGO_ICON: &[u8; 410598] = include_bytes!("./resources/images/rio-logo.ico");
// Terminal W/H constraints
pub const DEFAULT_MINIMUM_WINDOW_HEIGHT: i32 = 200;
pub const DEFAULT_MINIMUM_WINDOW_WIDTH: i32 = 300;

#[cfg(all(
    any(feature = "wayland", feature = "x11"),
    not(any(target_os = "macos", windows))
))]
pub const APPLICATION_ID: &str = "Rio";

pub fn create_window_builder(
    title: &str,
    config: &Config,
    #[allow(unused_variables)] app_id: Option<&str>,
    quake: bool,
) -> WindowAttributes {
    let image_icon = image_rs::load_from_memory(LOGO_ICON).unwrap();
    let icon = Icon::from_rgba(
        image_icon.to_rgba8().into_raw(),
        image_icon.width(),
        image_icon.height(),
    )
    .unwrap();

    let mut window_builder = WindowAttributes::default()
        .with_title(title)
        .with_min_inner_size(rio_window::dpi::LogicalSize {
            width: DEFAULT_MINIMUM_WINDOW_WIDTH,
            height: DEFAULT_MINIMUM_WINDOW_HEIGHT,
        })
        .with_resizable(true)
        .with_decorations(true)
        .with_transparent(config.window.opacity < 1.)
        .with_blur(config.window.blur.into())
        .with_window_icon(Some(icon));

    if quake {
        window_builder = window_builder
            .with_decorations(false)
            .with_window_level(rio_window::window::WindowLevel::AlwaysOnTop);
    }

    match config.window.decorations {
        Decorations::Disabled => {
            window_builder = window_builder.with_decorations(false);
        }
        Decorations::Transparent => {
            #[cfg(target_os = "macos")]
            {
                use rio_window::platform::macos::WindowAttributesExtMacOS;
                window_builder = window_builder.with_titlebar_transparent(true)
            }
        }
        Decorations::Buttonless => {
            #[cfg(target_os = "macos")]
            {
                use rio_window::platform::macos::WindowAttributesExtMacOS;
                window_builder = window_builder.with_titlebar_buttons_hidden(true)
            }
        }
        _ => {}
    };

    #[cfg(all(feature = "x11", not(any(target_os = "macos", windows))))]
    {
        use rio_window::platform::x11::WindowAttributesExtX11;
        let app_name = app_id.unwrap_or(APPLICATION_ID);
        window_builder = window_builder.with_name(app_name.to_lowercase(), app_name);
    }

    #[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
    {
        use rio_window::platform::wayland::WindowAttributesExtWayland;
        let app_name = app_id.unwrap_or(APPLICATION_ID);
        window_builder = window_builder.with_name(app_name.to_lowercase(), app_name);
    }

    #[cfg(target_os = "windows")]
    {
        use rio_window::platform::windows::WindowAttributesExtWindows;
        window_builder = window_builder.with_decorations(false);
        if config.window.windows_use_undecorated_shadow.is_none() {
            window_builder = window_builder.with_undecorated_shadow(true);
        }
        if let Some(use_undecorated_shadow) = config.window.windows_use_undecorated_shadow
        {
            window_builder =
                window_builder.with_undecorated_shadow(use_undecorated_shadow);
        }

        use rio_backend::sugarloaf::wgpu::Backends;

        // Vulkan/DX12 present independently of GDI. Its backing bitmap can
        // remain opaque beneath their surface and flash during teardown.
        // CPU and OpenGL presentation still need that bitmap.
        let native_gpu = !config.renderer.use_cpu
            && Backends::from_env().map_or(true, |backends| {
                backends.intersects(Backends::VULKAN | Backends::DX12)
            });
        window_builder = window_builder.with_no_redirection_bitmap(
            config
                .window
                .windows_use_no_redirection_bitmap
                .unwrap_or(native_gpu),
        );
    }

    #[cfg(target_os = "macos")]
    {
        use rio_window::platform::macos::WindowAttributesExtMacOS;
        // MacOS is always transparent
        window_builder = window_builder.with_transparent(true);

        // Configure colorspace
        window_builder = window_builder
            .with_colorspace(config.window.colorspace.to_rio_window_colorspace());

        window_builder = window_builder
            .with_title_hidden(true)
            .with_titlebar_transparent(true)
            .with_fullsize_content_view(true)
            .with_traffic_light_position(
                config.window.macos_traffic_light_position_x.unwrap_or(12.0),
                config.window.macos_traffic_light_position_y.unwrap_or(
                    (config.navigation.tab_geometry().height as f64 - 14.0) / 2.0,
                ),
            )
            .with_mouse_down_can_move_window(false);
    }

    #[cfg(target_os = "windows")]
    {
        use rio_window::platform::windows::WindowAttributesExtWindows;
        // On windows cloak (hide) the window initially, we later reveal it after the first draw.
        // This is a workaround to hide the "white flash" that occurs during application startup.
        window_builder = window_builder.with_cloaked(true);
    }

    // The quake window ignores `window.mode`: a fullscreen or
    // maximized mode would defeat the dropdown, and its geometry is
    // recomputed from the target monitor on every show anyway.
    if !quake {
        match config.window.mode {
            WindowMode::Fullscreen => {
                window_builder =
                    window_builder.with_fullscreen(Some(Fullscreen::Borderless(None)));
            }
            WindowMode::Maximized => {
                window_builder = window_builder.with_maximized(true);
            }
            _ => {
                window_builder =
                    window_builder.with_inner_size(rio_window::dpi::LogicalSize {
                        width: config.window.width,
                        height: config.window.height,
                    })
            }
        };
    }

    window_builder
}

pub fn configure_window(
    winit_window: &Window,
    config: &Config,
    #[allow(unused_variables)] quake: bool,
) {
    if config.effects.custom_mouse_cursor {
        winit_window.set_cursor_visible(false);
    } else {
        winit_window.set_cursor_visible(true);
        let current_mouse_cursor = CursorIcon::Text;
        winit_window.set_cursor(current_mouse_cursor);
    }

    // https://docs.rs/winit/latest/winit;/window/enum.ImePurpose.html#variant.Terminal
    winit_window.set_ime_purpose(ImePurpose::Terminal);
    winit_window.set_ime_allowed(true);

    // TODO: Update ime position based on cursor
    // winit_window.set_ime_cursor_area(rio_window::dpi::PhysicalPosition::new(500.0, 500.0), rio_window::dpi::LogicalSize::new(400, 400));

    // This will ignore diacritical marks and accent characters from
    // being processed as received characters. Instead, the input
    // device's raw character will be placed in event queues with the
    // Alt modifier set.
    #[cfg(target_os = "macos")]
    {
        // OnlyLeft - The left `Option` key is treated as `Alt`.
        // OnlyRight - The right `Option` key is treated as `Alt`.
        // Both - Both `Option` keys are treated as `Alt`.
        // None - No special handling is applied for `Option` key.
        use rio_window::keyboard::ModifiersState;
        use rio_window::platform::macos::{OptionAsAlt, WindowExtMacOS};

        match config.option_as_alt.to_lowercase().as_str() {
            "both" => winit_window.set_option_as_alt(OptionAsAlt::Both),
            "left" => winit_window.set_option_as_alt(OptionAsAlt::OnlyLeft),
            "right" => winit_window.set_option_as_alt(OptionAsAlt::OnlyRight),
            _ => {}
        }

        let mut mask = ModifiersState::empty();
        for name in &config.keyboard.forward_to_ime_modifier_mask {
            match name.to_lowercase().as_str() {
                "shift" => mask |= ModifiersState::SHIFT,
                "ctrl" | "control" => mask |= ModifiersState::CONTROL,
                "alt" | "option" => mask |= ModifiersState::ALT,
                "super" | "cmd" | "command" => mask |= ModifiersState::SUPER,
                other => tracing::warn!(
                    "ignoring unknown forward-to-ime-modifier-mask value: {other:?}"
                ),
            }
        }
        winit_window.set_forward_to_ime_modifier_mask(mask);
    }

    let is_transparent = config.window.opacity < 1.;
    winit_window.set_transparent(is_transparent);

    #[cfg(target_os = "macos")]
    {
        use rio_window::platform::macos::WindowExtMacOS;
        let bg_color = config.colors.background.1;
        winit_window.set_background_color(
            bg_color.r,
            bg_color.g,
            bg_color.b,
            config.window.opacity as f64,
        );

        if !config.window.macos_use_shadow {
            winit_window.set_has_shadow(false);
        }
    }

    #[cfg(target_os = "windows")]
    {
        use rio_backend::config::window::WindowsCornerPreference;
        use rio_window::platform::windows::{Color, WindowExtWindows};

        winit_window.set_decorations(false);
        if config.window.windows_use_undecorated_shadow.is_none() {
            winit_window.set_undecorated_shadow(true);
        }

        let background = config.colors.background.0;
        let foreground = config.colors.tabs_active;
        let native_color = |color: [f32; 4]| {
            Color::from_rgb(
                (color[0] * 255.0).round() as u8,
                (color[1] * 255.0).round() as u8,
                (color[2] * 255.0).round() as u8,
            )
        };
        winit_window.set_title_background_color(Some(native_color(background)));
        winit_window.set_title_text_color(native_color(foreground));
        let border = std::array::from_fn(|i| {
            background[i] + (foreground[i] - background[i]) * 0.12
        });
        winit_window.set_border_color(Some(native_color(border)));

        if let Some(with_corner_preference) = &config.window.windows_corner_preference {
            let preference = match with_corner_preference {
                WindowsCornerPreference::Default => {
                    rio_window::platform::windows::CornerPreference::Default
                }
                WindowsCornerPreference::DoNotRound => {
                    rio_window::platform::windows::CornerPreference::DoNotRound
                }
                WindowsCornerPreference::Round => {
                    rio_window::platform::windows::CornerPreference::Round
                }
                WindowsCornerPreference::RoundSmall => {
                    rio_window::platform::windows::CornerPreference::RoundSmall
                }
            };

            winit_window.set_corner_preference(preference);
        }
    }
    if let Some(title) = &config.title.placeholder {
        winit_window.set_title(title);
    }

    // macOS-only: stash the opacity value before set_blur so the
    // initial glass install reads the correct tint. No-op on macOS <
    // 26 / non-macOS — only matters when liquid-glass actually
    // engages.
    #[cfg(target_os = "macos")]
    {
        use rio_window::platform::macos::WindowExtMacOS;
        winit_window.set_glass_opacity(config.window.opacity as f64);
    }

    winit_window.set_blur(config.window.blur.into());
}
