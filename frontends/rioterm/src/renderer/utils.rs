use crate::constants;
use crate::layout::ContextDimension;
use rio_backend::config::navigation::Navigation;
use rio_backend::config::Config;
use rio_window::window::Theme;

#[inline]
pub fn padding_top_from_config(
    navigation: &Navigation,
    padding_y_top: f32,
    #[allow(unused)] num_tabs: usize,
) -> f32 {
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    if navigation.hide_if_single && num_tabs <= 1 {
        return constants::PADDING_Y + padding_y_top;
    }
    navigation.tab_geometry().content_top() + padding_y_top
}

#[inline]
pub fn terminal_dimensions(layout: &ContextDimension) -> rio_backend::event::WindowSize {
    let width = layout.width - layout.margin.left - layout.margin.right;
    let height = layout.height - layout.margin.top - layout.margin.bottom;
    rio_backend::event::WindowSize {
        width: width as u16,
        height: height as u16,
        cols: layout.columns as u16,
        rows: layout.lines as u16,
    }
}

#[inline]
pub fn update_colors_based_on_theme(config: &mut Config, theme_opt: Option<Theme>) {
    if let Some(theme) = theme_opt {
        if let Some(adaptive_colors) = &config.adaptive_colors {
            match theme {
                Theme::Light => {
                    if let Some(light_colors) = adaptive_colors.light {
                        config.colors = light_colors;
                    }
                }
                Theme::Dark => {
                    if let Some(darkcolors) = adaptive_colors.dark {
                        config.colors = darkcolors;
                    }
                }
            }
        }
    }
}
