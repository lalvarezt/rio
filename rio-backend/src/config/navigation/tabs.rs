use crate::config::layout::Margin;
use serde::{Deserialize, Serialize};

use super::TabStyle;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CloseButton {
    Always,
    Hover,
    Never,
}

/// Optional overrides keep omitted values attached to the selected preset.
#[derive(Debug, Default, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Tabs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub margin: Option<Margin>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub padding: Option<Margin>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gap: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub radius: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_size: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_offset_y: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_gap: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub close_button: Option<CloseButton>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub close_button_size: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub close_button_inset: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub close_button_gap: Option<f32>,
}

/// Logical-pixel geometry. Resolve at configuration changes, then reuse it for
/// drawing, measurement, hit testing, and the terminal's top margin.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TabGeometry {
    pub height: f32,
    pub margin: Margin,
    pub padding: Margin,
    pub gap: f32,
    pub radius: f32,
    pub font_size: f32,
    pub text_offset_y: f32,
    pub icon_gap: f32,
    pub close_button: CloseButton,
    pub close_button_size: f32,
    pub close_button_inset: f32,
    pub close_button_gap: f32,
}

impl TabStyle {
    // Presets only supply defaults; renderers consume one resolved geometry so
    // individual overrides do not create separate pill/square drawing paths.
    pub fn geometry(self) -> TabGeometry {
        let square = self == Self::Square;
        TabGeometry {
            height: 54.0,
            margin: if square {
                Margin::new(0.0, 8.0, 6.0, 0.0)
            } else {
                Margin::new(6.0, 11.0, 6.0, 9.0)
            },
            padding: Margin::new(0.0, 14.0, 0.0, 12.0),
            gap: if square { 0.0 } else { 6.0 },
            radius: if square { 0.0 } else { 19.0 },
            font_size: 15.0,
            text_offset_y: 2.0,
            icon_gap: 8.0,
            close_button: if square {
                CloseButton::Always
            } else {
                CloseButton::Hover
            },
            close_button_size: 20.0,
            close_button_inset: 12.0,
            close_button_gap: 8.0,
        }
    }
}

pub(crate) fn finite(value: f32, fallback: f32, min: f32, max: f32) -> f32 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        fallback
    }
}

pub(crate) fn spacing(value: f32, fallback: f32) -> f32 {
    finite(value, fallback, 0.0, 1024.0)
}

pub(crate) fn margin(value: Margin, fallback: Margin) -> Margin {
    Margin::new(
        spacing(value.top, fallback.top),
        spacing(value.right, fallback.right),
        spacing(value.bottom, fallback.bottom),
        spacing(value.left, fallback.left),
    )
}

impl Tabs {
    pub fn resolve(self, preset: TabStyle, icon_size: f32) -> TabGeometry {
        let base = preset.geometry();
        let mut geometry = TabGeometry {
            height: finite(self.height.unwrap_or(base.height), base.height, 32.0, 512.0),
            margin: margin(self.margin.unwrap_or(base.margin), base.margin),
            padding: margin(self.padding.unwrap_or(base.padding), base.padding),
            gap: spacing(self.gap.unwrap_or(base.gap), base.gap),
            radius: spacing(self.radius.unwrap_or(base.radius), base.radius),
            font_size: finite(
                self.font_size.unwrap_or(base.font_size),
                base.font_size,
                8.0,
                96.0,
            ),
            text_offset_y: finite(
                self.text_offset_y.unwrap_or(base.text_offset_y),
                base.text_offset_y,
                -64.0,
                64.0,
            ),
            icon_gap: spacing(self.icon_gap.unwrap_or(base.icon_gap), base.icon_gap),
            close_button: self.close_button.unwrap_or(base.close_button),
            close_button_size: finite(
                self.close_button_size.unwrap_or(base.close_button_size),
                base.close_button_size,
                8.0,
                96.0,
            ),
            close_button_inset: spacing(
                self.close_button_inset.unwrap_or(base.close_button_inset),
                base.close_button_inset,
            ),
            close_button_gap: spacing(
                self.close_button_gap.unwrap_or(base.close_button_gap),
                base.close_button_gap,
            ),
        };
        // A configured font or icon must fit even if height was left at its
        // preset value. Vertical padding may be asymmetric intentionally.
        let close_size = if geometry.close_button == CloseButton::Never {
            0.0
        } else {
            geometry.close_button_size
        };
        let content = icon_size
            .max(geometry.font_size + geometry.text_offset_y.abs() * 2.0)
            .max(close_size);
        geometry.height = geometry.height.max(
            geometry.margin.top
                + geometry.margin.bottom
                + geometry.padding.top
                + geometry.padding.bottom
                + content,
        );
        geometry
    }

    pub fn merge(&mut self, other: Self) {
        self.height = other.height.or(self.height);
        self.margin = other.margin.or(self.margin);
        self.padding = other.padding.or(self.padding);
        self.gap = other.gap.or(self.gap);
        self.radius = other.radius.or(self.radius);
        self.font_size = other.font_size.or(self.font_size);
        self.text_offset_y = other.text_offset_y.or(self.text_offset_y);
        self.icon_gap = other.icon_gap.or(self.icon_gap);
        self.close_button = other.close_button.or(self.close_button);
        self.close_button_size = other.close_button_size.or(self.close_button_size);
        self.close_button_inset = other.close_button_inset.or(self.close_button_inset);
        self.close_button_gap = other.close_button_gap.or(self.close_button_gap);
    }
}

impl TabGeometry {
    pub fn content_top(&self) -> f32 {
        // The pane gap is controlled by root margin, not another tab-bottom gap.
        self.height - self.margin.bottom
    }
}
