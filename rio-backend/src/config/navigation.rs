use crate::config::colors::{deserialize_to_arr_opt, ColorArray};
use crate::config::default_bool_true;
use serde::{Deserialize, Serialize};

pub mod tabs;
pub use tabs::{CloseButton, TabGeometry, Tabs};

#[inline]
pub fn default_unfocused_split_opacity() -> f32 {
    0.7
}

/// macOS hides the strip for a lone tab (the native-app feel);
/// Linux and Windows keep the tab pill visible.
#[inline]
pub fn default_hide_if_single() -> bool {
    cfg!(target_os = "macos")
}

#[inline]
pub fn default_max_tab_width() -> f32 {
    240.0
}

#[inline]
pub fn default_min_tab_width() -> f32 {
    120.0
}

#[inline]
pub fn default_tab_icon_size() -> f32 {
    24.0
}

#[inline]
pub fn default_tab_glass_intensity() -> f32 {
    0.5
}

#[inline]
pub fn default_tab_status_width() -> f32 {
    1.0
}

#[inline]
pub fn clamp_tab_status_width(v: f32) -> f32 {
    if v.is_finite() {
        v.clamp(0.0, 6.0)
    } else {
        default_tab_status_width()
    }
}

#[inline]
pub fn clamp_tab_glass_intensity(v: f32) -> f32 {
    if v.is_finite() {
        v.clamp(0.0, 1.0)
    } else {
        default_tab_glass_intensity()
    }
}

/// Clamp tab widths to a finite, usable range in logical pixels.
#[inline]
pub fn clamp_max_tab_width(v: f32) -> f32 {
    if !v.is_finite() {
        return default_max_tab_width();
    }
    v.clamp(80.0, 1024.0)
}

#[inline]
pub fn clamp_min_tab_width(v: f32, max: f32) -> f32 {
    let v = if v.is_finite() {
        v
    } else {
        default_min_tab_width()
    };
    v.clamp(80.0, max)
}

#[inline]
pub fn clamp_tab_icon_size(v: f32) -> f32 {
    let v = if v.is_finite() {
        v
    } else {
        default_tab_icon_size()
    };
    v.clamp(8.0, 32.0)
}

/// Clamp `unfocused_split_opacity` to `[0.15, 1.0]`.
///
/// A value of `0.0` makes the inactive pane invisible, which is never what
/// the user wants; the lower bound keeps the pane legible at the darkest
/// setting.
#[inline]
pub fn clamp_unfocused_split_opacity(v: f32) -> f32 {
    v.clamp(0.15, 1.0)
}

/// Medium keeps geometric motion but skips fades; Full adds hover and opacity
/// transitions. Both stop requesting frames once the visible motion settles.
#[derive(Debug, Default, PartialEq, Eq, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Animation {
    None,
    Medium,
    #[default]
    Full,
}

#[derive(Debug, Default, PartialEq, Eq, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TabStyle {
    #[default]
    Pill,
    Square,
}

#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Navigation {
    #[serde(
        default = "default_bool_true",
        rename = "current-working-directory",
        alias = "cwd"
    )]
    pub current_working_directory: bool,
    #[serde(default = "default_hide_if_single", rename = "hide-if-single")]
    pub hide_if_single: bool,
    #[serde(default = "default_bool_true", rename = "use-split")]
    pub use_split: bool,
    #[serde(default = "default_bool_true", rename = "open-config-with-split")]
    pub open_config_with_split: bool,
    /// The opacity level of an unfocused split. A value of `1.0` disables the
    /// dim; lower values fade the pane out. Clamped to `[0.15, 1.0]` at load
    /// time — a value of `0` makes the pane invisible, which is never useful.
    #[serde(
        default = "default_unfocused_split_opacity",
        rename = "unfocused-split-opacity"
    )]
    pub unfocused_split_opacity: f32,
    /// The color used to dim an unfocused split. The overlay's alpha is
    /// derived from `unfocused_split_opacity` — this field is an RGB tint
    /// only. When unset, the terminal's background color is used.
    #[serde(
        default = "Option::default",
        deserialize_with = "deserialize_to_arr_opt",
        rename = "unfocused-split-fill"
    )]
    pub unfocused_split_fill: Option<ColorArray>,
    /// Maximum tab width in logical pixels, clamped to `[80.0, 1024.0]`.
    /// Desktop tabs size to their titles between the configured bounds.
    /// Tabs shrink further when necessary to fit the window.
    #[serde(default = "default_max_tab_width", rename = "max-tab-width")]
    pub max_tab_width: f32,
    /// Minimum preferred Desktop tab width, bounded by `max-tab-width`.
    #[serde(default = "default_min_tab_width", rename = "min-tab-width")]
    pub min_tab_width: f32,
    /// Desktop tab icon size in logical pixels, clamped to `[8.0, 32.0]`.
    #[serde(default = "default_tab_icon_size", rename = "tab-icon-size")]
    pub tab_icon_size: f32,
    /// UI preset; explicit tab and panel settings override its defaults.
    #[serde(default, rename = "preset")]
    pub tab_style: TabStyle,
    #[serde(default)]
    pub tabs: Tabs,
    /// Desktop tab border reflections, from `0.0` (off) to `1.0`.
    #[serde(
        default = "default_tab_glass_intensity",
        rename = "tab-glass-intensity"
    )]
    pub tab_glass_intensity: f32,
    /// Program status border thickness in physical pixels, from `0.0` to `6.0`.
    /// Zero hides the status border, preserving the ordinary glass reflection.
    #[serde(default = "default_tab_status_width", rename = "tab-status-width")]
    pub tab_status_width: f32,
    /// Tab motion and hover transitions. Defaults to the full effect.
    #[serde(default)]
    pub animation: Animation,
    /// Map command names to a bundled app or named icon in Desktop tabs.
    #[serde(default, rename = "app-icons")]
    pub app_icons: std::collections::BTreeMap<String, String>,
}

impl Default for Navigation {
    fn default() -> Navigation {
        Navigation {
            current_working_directory: true,
            hide_if_single: default_hide_if_single(),
            use_split: true,
            unfocused_split_opacity: default_unfocused_split_opacity(),
            unfocused_split_fill: None,
            open_config_with_split: true,
            max_tab_width: default_max_tab_width(),
            min_tab_width: default_min_tab_width(),
            tab_icon_size: default_tab_icon_size(),
            tab_style: TabStyle::default(),
            tabs: Tabs::default(),
            tab_glass_intensity: default_tab_glass_intensity(),
            tab_status_width: default_tab_status_width(),
            animation: Animation::default(),
            app_icons: std::collections::BTreeMap::new(),
        }
    }
}

impl Navigation {
    pub fn tab_geometry(&self) -> TabGeometry {
        self.tabs
            .resolve(self.tab_style, clamp_tab_icon_size(self.tab_icon_size))
    }

    /// Whether the rio-rendered tab strip ("island") is actually painted
    /// this frame. Input layers
    /// (click routing, cursor override) must agree with the renderer so
    /// the empty band over a hidden island doesn't intercept events.
    #[inline]
    pub fn island_visible(&self, num_tabs: usize) -> bool {
        !(self.hide_if_single && num_tabs == 1)
    }

    /// Whether the top band is reserved as custom window chrome this
    /// frame, painted island or not. On macOS the full-size content
    /// view keeps the band even
    /// with `hide-if-single` hiding the strip; other platforms render
    /// the terminal from the top when the island is hidden. Must agree
    /// with `padding_top_from_config`, which reserves the band's
    /// height under the same condition.
    #[inline]
    pub fn chrome_band_reserved(&self, num_tabs: usize) -> bool {
        if cfg!(any(target_os = "macos", target_os = "windows")) {
            true
        } else {
            self.island_visible(num_tabs)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chrome_band_follows_padding_contract_for_both_styles() {
        for tab_style in [TabStyle::Pill, TabStyle::Square] {
            let nav = Navigation {
                tab_style,
                hide_if_single: true,
                ..Navigation::default()
            };
            assert!(nav.island_visible(2));
            assert!(nav.chrome_band_reserved(2));
            assert!(!nav.island_visible(1));
            assert_eq!(
                nav.chrome_band_reserved(1),
                cfg!(any(target_os = "macos", target_os = "windows"))
            );
        }
    }

    #[test]
    fn hide_if_single_platform_default() {
        let nav: Navigation = toml::from_str("").unwrap();
        assert_eq!(nav.hide_if_single, cfg!(target_os = "macos"));
        assert_eq!(nav.island_visible(1), !cfg!(target_os = "macos"));
        assert!(nav.island_visible(2));
    }

    #[test]
    fn hide_if_single_explicit_override() {
        for hidden in [true, false] {
            let nav: Navigation =
                toml::from_str(&format!("hide-if-single = {hidden}")).unwrap();
            assert_eq!(nav.island_visible(1), !hidden);
            assert!(nav.island_visible(2));
        }
    }
}
