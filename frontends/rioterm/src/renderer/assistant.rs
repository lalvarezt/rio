// Copyright (c) 2023-present, Raphael Amorim.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

use rio_backend::error::{RioError, RioErrorLevel, RioErrorType};
use rio_backend::sugarloaf::text::DrawOpts;
use rio_backend::sugarloaf::Sugarloaf;

/// Convert `[f32; 4]` colour to `[u8; 4]` for the `Text` API (the
/// vertex shader premultiplies, so pass non-premul RGBA).
#[inline]
fn color_u8(c: [f32; 4]) -> [u8; 4] {
    [
        (c[0].clamp(0.0, 1.0) * 255.0) as u8,
        (c[1].clamp(0.0, 1.0) * 255.0) as u8,
        (c[2].clamp(0.0, 1.0) * 255.0) as u8,
        (c[3].clamp(0.0, 1.0) * 255.0) as u8,
    ]
}

// Layout
const OVERLAY_WIDTH: f32 = 480.0;
const OVERLAY_MARGIN_TOP: f32 = 8.0;
const OVERLAY_MARGIN_RIGHT: f32 = 8.0;
const OVERLAY_PADDING: f32 = 16.0;

const HEADING_FONT_SIZE: f32 = 16.0;
const BODY_FONT_SIZE: f32 = 12.0;
const BUTTON_FONT_SIZE: f32 = 13.0;

const LINE_HEIGHT: f32 = 18.0;
const HEADING_HEIGHT: f32 = 28.0;
const ACTION_ROW_HEIGHT: f32 = 32.0;
const ACTION_GAP: f32 = 8.0;
const BODY_ACTION_GAP: f32 = 12.0;
const MAX_VISIBLE_LINES: usize = 16;

// Colors
const BACKDROP_COLOR: [f32; 4] = [0.0, 0.0, 0.0, 0.2];

// Depth / order
const DEPTH_BACKDROP: f32 = 0.0;
const DEPTH_BG: f32 = 0.1;
const DEPTH_ELEMENT: f32 = 0.2;
const ORDER: u8 = 20;

/// Actions triggered by clicking assistant overlay buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssistantOverlayAction {
    Close,
    OpenDocs,
}

pub struct AssistantOverlay {
    error: Option<RioError>,
    top_inset: f32,
    hovered_button: Option<AssistantOverlayAction>,
    lines: Vec<String>,
    font_library: Option<rio_backend::sugarloaf::font::FontLibrary>,
    font_id: Option<usize>,
}

impl Default for AssistantOverlay {
    fn default() -> Self {
        Self {
            error: None,
            top_inset: 0.0,
            hovered_button: None,
            lines: Vec::new(),
            font_library: None,
            font_id: None,
        }
    }
}

impl AssistantOverlay {
    pub fn set_top_inset(&mut self, top_inset: f32) {
        self.top_inset = top_inset;
    }

    #[inline]
    pub fn is_active(&self) -> bool {
        self.error.is_some()
    }

    /// Whether the active toast is a hard error. Errors are modal
    /// (keys and IME blocked, Enter dismisses); warnings render but
    /// must never block input over a working terminal.
    #[inline]
    pub fn is_error(&self) -> bool {
        self.error
            .as_ref()
            .is_some_and(|error| error.level == RioErrorLevel::Error)
    }

    #[inline]
    pub fn set_error(&mut self, error: RioError) {
        self.lines = error
            .report
            .to_string()
            .lines()
            .map(str::to_owned)
            .collect();
        self.hovered_button = None;
        self.error = Some(error);
    }

    #[inline]
    pub fn clear(&mut self) {
        self.error = None;
        self.hovered_button = None;
    }

    pub fn clear_hover(&mut self) -> bool {
        self.hovered_button.take().is_some()
    }

    /// Returns (overlay_x, overlay_y, overlay_width, overlay_height) in logical coords.
    fn overlay_rect(&self, window_width: f32, scale_factor: f32) -> (f32, f32, f32, f32) {
        let logical_width = window_width / scale_factor;
        let width =
            OVERLAY_WIDTH.min((logical_width - OVERLAY_MARGIN_RIGHT * 2.0).max(1.0));
        let x = logical_width - width - OVERLAY_MARGIN_RIGHT;
        let y = self.top_inset + OVERLAY_MARGIN_TOP;
        let line_count = self.body_line_count().min(MAX_VISIBLE_LINES);
        let h = OVERLAY_PADDING
            + HEADING_HEIGHT
            + (line_count as f32 * LINE_HEIGHT)
            + BODY_ACTION_GAP
            + ACTION_ROW_HEIGHT
            + OVERLAY_PADDING;
        (x, y, width, h)
    }

    fn body_line_count(&self) -> usize {
        self.lines.len()
    }

    /// Returns the close button rect.
    fn close_button_rect(
        &self,
        overlay_x: f32,
        overlay_y: f32,
        overlay_width: f32,
    ) -> (f32, f32, f32, f32) {
        let width = (overlay_width - OVERLAY_PADDING * 2.0 - ACTION_GAP) / 2.0;
        let bx = overlay_x + overlay_width - OVERLAY_PADDING - width;
        let by = overlay_y
            + OVERLAY_PADDING
            + HEADING_HEIGHT
            + self.body_line_count().min(MAX_VISIBLE_LINES) as f32 * LINE_HEIGHT
            + BODY_ACTION_GAP;
        (bx, by, width, ACTION_ROW_HEIGHT)
    }

    fn docs_button_rect(
        &self,
        overlay_x: f32,
        overlay_y: f32,
        overlay_width: f32,
    ) -> (f32, f32, f32, f32) {
        let (_, by, bw, bh) = self.close_button_rect(overlay_x, overlay_y, overlay_width);
        (overlay_x + OVERLAY_PADDING, by, bw, bh)
    }

    #[inline]
    pub fn hovered_button(&self) -> Option<AssistantOverlayAction> {
        self.hovered_button
    }

    fn hit_test_button(
        mouse_x: f32,
        mouse_y: f32,
        bx: f32,
        by: f32,
        bw: f32,
        bh: f32,
    ) -> bool {
        mouse_x >= bx && mouse_x <= bx + bw && mouse_y >= by && mouse_y <= by + bh
    }

    /// Hit-test a mouse click. Returns Some(action) if a button was clicked.
    /// Returns Err(()) if clicked outside the overlay entirely.
    pub fn hit_test(
        &self,
        mouse_x: f32,
        mouse_y: f32,
        window_width: f32,
        scale_factor: f32,
    ) -> Result<Option<AssistantOverlayAction>, ()> {
        if !self.is_active() {
            return Err(());
        }

        let (ox, oy, ow, oh) = self.overlay_rect(window_width, scale_factor);

        if mouse_x < ox || mouse_x > ox + ow || mouse_y < oy || mouse_y > oy + oh {
            return Err(());
        }

        let (bx, by, bw, bh) = self.close_button_rect(ox, oy, ow);
        if Self::hit_test_button(mouse_x, mouse_y, bx, by, bw, bh) {
            return Ok(Some(AssistantOverlayAction::Close));
        }

        let (bx, by, bw, bh) = self.docs_button_rect(ox, oy, ow);
        if Self::hit_test_button(mouse_x, mouse_y, bx, by, bw, bh) {
            return Ok(Some(AssistantOverlayAction::OpenDocs));
        }

        Ok(None)
    }

    /// Warnings own only their panel; hard errors also own the backdrop.
    pub fn captures_pointer(&self, x: f32, y: f32, width: f32, scale: f32) -> bool {
        self.is_error() || self.hit_test(x, y, width, scale).is_ok()
    }

    /// Update hover state based on mouse position. Returns true if changed.
    pub fn hover(
        &mut self,
        mouse_x: f32,
        mouse_y: f32,
        window_width: f32,
        scale_factor: f32,
    ) -> bool {
        if !self.is_active() {
            return false;
        }

        let (ox, oy, ow, _oh) = self.overlay_rect(window_width, scale_factor);

        let (bx, by, bw, bh) = self.close_button_rect(ox, oy, ow);
        let mut new_hover = if Self::hit_test_button(mouse_x, mouse_y, bx, by, bw, bh) {
            Some(AssistantOverlayAction::Close)
        } else {
            None
        };

        if new_hover.is_none() {
            let (bx, by, bw, bh) = self.docs_button_rect(ox, oy, ow);
            if Self::hit_test_button(mouse_x, mouse_y, bx, by, bw, bh) {
                new_hover = Some(AssistantOverlayAction::OpenDocs);
            }
        }

        if new_hover != self.hovered_button {
            self.hovered_button = new_hover;
            return true;
        }
        false
    }

    pub fn render(
        &mut self,
        sugarloaf: &mut Sugarloaf,
        dimensions: (f32, f32, f32),
        theme: &super::window_chrome::Theme,
    ) {
        if !self.is_active() {
            // Immediate mode: not drawing == not visible.
            return;
        }

        let (window_width, window_height, scale_factor) = dimensions;
        let library = sugarloaf.font_library();
        if self
            .font_library
            .as_ref()
            .is_none_or(|old| !std::sync::Arc::ptr_eq(&old.inner, &library.inner))
        {
            self.font_id = super::window_chrome::load_ui_font(library);
            self.font_library = Some(library.clone());
        }
        let body_opts = DrawOpts {
            font_size: BODY_FONT_SIZE,
            font_id: self.font_id,
            color: color_u8(theme.foreground),
            ..DrawOpts::default()
        };
        let (_, _, width, _) = self.overlay_rect(window_width, scale_factor);
        self.lines = super::text_layout::wrap_text(
            &self.error.as_ref().unwrap().report.to_string(),
            (width - OVERLAY_PADDING * 2.0).max(1.0),
            |text| sugarloaf.text_mut().measure(text, &body_opts),
        );
        let available = ((window_height / scale_factor
            - self.top_inset
            - OVERLAY_MARGIN_TOP * 2.0
            - OVERLAY_PADDING * 2.0
            - HEADING_HEIGHT
            - BODY_ACTION_GAP
            - ACTION_ROW_HEIGHT)
            / LINE_HEIGHT)
            .max(1.0) as usize;
        self.lines.truncate(available.min(MAX_VISIBLE_LINES));

        let (ox, oy, ow, oh) = self.overlay_rect(window_width, scale_factor);

        // Backdrop
        if self.is_error() {
            sugarloaf.rect(
                0.0,
                0.0,
                window_width / scale_factor,
                window_height / scale_factor,
                BACKDROP_COLOR,
                DEPTH_BACKDROP,
                ORDER,
            );
        }

        // Background
        sugarloaf.rounded_rect(
            ox,
            oy,
            ow,
            oh,
            theme.border,
            DEPTH_BG,
            theme.radius,
            ORDER,
        );
        let pixel = (1.0 / scale_factor).min(ow / 2.0).min(oh / 2.0);
        sugarloaf.rounded_rect(
            ox + pixel,
            oy + pixel,
            ow - pixel * 2.0,
            oh - pixel * 2.0,
            theme.surface,
            DEPTH_BG + 0.01,
            (theme.radius - pixel).max(0.0),
            ORDER,
        );

        let error = self.error.clone().unwrap();
        let is_error = error.level == RioErrorLevel::Error;
        let heading_color = theme.foreground;

        // Heading
        let heading_text =
            if matches!(error.report, RioErrorType::InvalidConfigurationTheme(_)) {
                "Theme unavailable"
            } else if is_error {
                "Error"
            } else {
                "Warning"
            };
        let text_x = ox + OVERLAY_PADDING;
        let heading_y = oy + OVERLAY_PADDING;
        let heading_opts = DrawOpts {
            font_size: HEADING_FONT_SIZE,
            font_id: self.font_id,
            color: color_u8(heading_color),
            ..DrawOpts::default()
        };
        sugarloaf
            .text_mut()
            .draw(text_x, heading_y, heading_text, &heading_opts);

        // Body lines
        let body_y_start = heading_y + HEADING_HEIGHT;
        for (i, line_text) in self.lines.iter().enumerate() {
            let line_y = body_y_start + (i as f32 * LINE_HEIGHT);
            sugarloaf
                .text_mut()
                .draw(text_x, line_y, line_text, &body_opts);
        }

        for (action, label, rect) in [
            (
                AssistantOverlayAction::OpenDocs,
                "Documentation",
                self.docs_button_rect(ox, oy, ow),
            ),
            (
                AssistantOverlayAction::Close,
                "Dismiss",
                self.close_button_rect(ox, oy, ow),
            ),
        ] {
            let (bx, by, bw, bh) = rect;
            sugarloaf.rounded_rect(
                bx,
                by,
                bw,
                bh,
                theme.border,
                DEPTH_ELEMENT,
                theme.control_radius(),
                ORDER,
            );
            sugarloaf.rounded_rect(
                bx + pixel,
                by + pixel,
                (bw - pixel * 2.0).max(0.0),
                (bh - pixel * 2.0).max(0.0),
                if self.hovered_button == Some(action) {
                    theme.hover
                } else {
                    theme.surface
                },
                DEPTH_ELEMENT + 0.01,
                (theme.control_radius() - pixel).max(0.0),
                ORDER,
            );
            let opts = DrawOpts {
                font_size: BUTTON_FONT_SIZE,
                font_id: self.font_id,
                color: color_u8(theme.foreground),
                ..DrawOpts::default()
            };
            let label = super::window_chrome::fit_text(
                sugarloaf,
                label,
                (bw - 12.0).max(0.0),
                &opts,
            );
            let label_y = super::window_chrome::centered_text_y(
                sugarloaf,
                &label,
                by + bh / 2.0,
                &opts,
            );
            let ui = sugarloaf.text_mut();
            let label_w = ui.measure(&label, &opts);
            ui.draw(bx + (bw - label_w) / 2.0, label_y, &label, &opts);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rio_backend::error::RioErrorType;

    #[test]
    fn diagnostic_panel_stays_below_window_chrome() {
        let mut overlay = AssistantOverlay::default();
        overlay.set_error(RioError {
            report: RioErrorType::InvalidConfigurationTheme("missing theme".into()),
            level: RioErrorLevel::Warning,
        });
        overlay.set_top_inset(48.0);
        for scale in [1.0, 1.5, 2.0] {
            let (_, top, _, _) = overlay.overlay_rect(1200.0 * scale, scale);
            assert!(
                top >= 56.0,
                "diagnostic covers window chrome at scale {scale}"
            );
            assert_eq!(
                overlay.hit_test(1190.0, 16.0, 1200.0 * scale, scale),
                Err(())
            );
        }
    }

    #[test]
    fn warnings_leave_pointer_input_outside_the_panel_available() {
        let mut overlay = AssistantOverlay::default();
        overlay.set_top_inset(48.0);
        overlay.set_error(RioError {
            report: RioErrorType::InvalidConfigurationTheme("missing theme".into()),
            level: RioErrorLevel::Warning,
        });
        assert!(!overlay.captures_pointer(1190.0, 16.0, 1200.0, 1.0));
        let (x, y, width, _) = overlay.overlay_rect(1200.0, 1.0);
        for (action, rect) in [
            (
                AssistantOverlayAction::Close,
                overlay.close_button_rect(x, y, width),
            ),
            (
                AssistantOverlayAction::OpenDocs,
                overlay.docs_button_rect(x, y, width),
            ),
        ] {
            let (bx, by, bw, bh) = rect;
            assert_eq!(
                overlay.hit_test(bx + bw / 2.0, by + bh / 2.0, 1200.0, 1.0),
                Ok(Some(action))
            );
            assert!(overlay.captures_pointer(bx + bw / 2.0, by + bh / 2.0, 1200.0, 1.0));
        }
        overlay.set_error(RioError {
            report: RioErrorType::InitializationError("failed".into()),
            level: RioErrorLevel::Error,
        });
        assert!(overlay.captures_pointer(1190.0, 16.0, 1200.0, 1.0));
    }
}
