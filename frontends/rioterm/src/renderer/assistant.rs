// Copyright (c) 2023-present, Raphael Amorim.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

use rio_backend::error::{RioError, RioErrorLevel};
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
const OVERLAY_CORNER_RADIUS: f32 = 10.0;
const OVERLAY_MARGIN_TOP: f32 = 8.0;
const OVERLAY_MARGIN_RIGHT: f32 = 8.0;
const OVERLAY_PADDING: f32 = 16.0;

const HEADING_FONT_SIZE: f32 = 16.0;
const BODY_FONT_SIZE: f32 = 12.0;
const BUTTON_FONT_SIZE: f32 = 14.0;
const LINK_FONT_SIZE: f32 = 12.0;

const BUTTON_SIZE: f32 = 24.0;
const BUTTON_CORNER_RADIUS: f32 = 4.0;

const LINE_HEIGHT: f32 = 18.0;
const HEADING_HEIGHT: f32 = 28.0;
const LINK_ROW_HEIGHT: f32 = 24.0;
const MAX_VISIBLE_LINES: usize = 16;

const DOCS_URL: &str = "rioterm.com/docs/config";

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
    hovered_button: Option<AssistantOverlayAction>,
    /// Latest rendered width of the docs-link button text — used by
    /// `docs_button_rect` to size the hit target. Updated by `render`.
    link_button_width: f32,
    lines: Vec<String>,
    font_library: Option<rio_backend::sugarloaf::font::FontLibrary>,
    font_id: Option<usize>,
}

impl Default for AssistantOverlay {
    fn default() -> Self {
        Self {
            error: None,
            hovered_button: None,
            link_button_width: 0.0,
            lines: Vec::new(),
            font_library: None,
            font_id: None,
        }
    }
}

impl AssistantOverlay {
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
        let y = OVERLAY_MARGIN_TOP;
        let line_count = self.body_line_count().min(MAX_VISIBLE_LINES);
        let h = OVERLAY_PADDING
            + HEADING_HEIGHT
            + (line_count as f32 * LINE_HEIGHT)
            + LINK_ROW_HEIGHT
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
        let bx = overlay_x + overlay_width - OVERLAY_PADDING - BUTTON_SIZE;
        let by = overlay_y + OVERLAY_PADDING / 2.0;
        (bx, by, BUTTON_SIZE, BUTTON_SIZE)
    }

    /// Returns the docs link button rect (covers the link text area).
    fn docs_button_rect(&self, overlay_x: f32, overlay_y: f32) -> (f32, f32, f32, f32) {
        let line_count = self.body_line_count().min(MAX_VISIBLE_LINES);
        let by = overlay_y
            + OVERLAY_PADDING
            + HEADING_HEIGHT
            + (line_count as f32 * LINE_HEIGHT);
        let bx = overlay_x + OVERLAY_PADDING - 4.0;
        let bw = self.link_button_width + 8.0;
        (bx, by, bw, LINK_ROW_HEIGHT)
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

        let (bx, by, bw, bh) = self.docs_button_rect(ox, oy);
        if Self::hit_test_button(mouse_x, mouse_y, bx, by, bw, bh) {
            return Ok(Some(AssistantOverlayAction::OpenDocs));
        }

        Ok(None)
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
            let (bx, by, bw, bh) = self.docs_button_rect(ox, oy);
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
        colors: &rio_backend::config::colors::Colors,
    ) {
        if !self.is_active() {
            // Immediate mode: not drawing == not visible.
            return;
        }

        let (window_width, window_height, scale_factor) = dimensions;
        let theme = super::window_chrome::Theme::new(colors);
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
            - OVERLAY_MARGIN_TOP * 2.0
            - OVERLAY_PADDING * 2.0
            - HEADING_HEIGHT
            - LINK_ROW_HEIGHT)
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
            OVERLAY_CORNER_RADIUS,
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
            (OVERLAY_CORNER_RADIUS - pixel).max(0.0),
            ORDER,
        );

        let error = self.error.clone().unwrap();
        let is_error = error.level == RioErrorLevel::Error;
        let heading_color = theme.foreground;

        // Heading
        let heading_text = if is_error { "Error" } else { "Warning" };
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
        let visible_count = self.lines.len();
        for (i, line_text) in self.lines.iter().enumerate() {
            let line_y = body_y_start + (i as f32 * LINE_HEIGHT);
            sugarloaf
                .text_mut()
                .draw(text_x, line_y, line_text, &body_opts);
        }

        // Docs link button
        let line_count = visible_count;
        let link_area_y =
            oy + OVERLAY_PADDING + HEADING_HEIGHT + (line_count as f32 * LINE_HEIGHT);
        let link_x = ox + OVERLAY_PADDING;
        let link_y = link_area_y + (LINK_ROW_HEIGHT - LINK_FONT_SIZE) / 2.0;
        let link_opts = DrawOpts {
            font_size: LINK_FONT_SIZE,
            font_id: self.font_id,
            color: color_u8(theme.accent),
            ..DrawOpts::default()
        };
        let rendered_width = sugarloaf
            .text_mut()
            .draw(link_x, link_y, DOCS_URL, &link_opts);
        self.link_button_width = rendered_width;

        let (dbx, dby, dbw, dbh) = self.docs_button_rect(ox, oy);
        let docs_hovered = self.hovered_button == Some(AssistantOverlayAction::OpenDocs);

        if docs_hovered {
            sugarloaf.rounded_rect(
                dbx,
                dby,
                dbw,
                dbh,
                theme.hover,
                DEPTH_ELEMENT,
                BUTTON_CORNER_RADIUS,
                ORDER,
            );
        }

        // Close button
        let (bx, by, bw, bh) = self.close_button_rect(ox, oy, ow);
        let is_hovered = self.hovered_button == Some(AssistantOverlayAction::Close);

        if is_hovered {
            sugarloaf.rounded_rect(
                bx,
                by,
                bw,
                bh,
                theme.hover,
                DEPTH_ELEMENT,
                BUTTON_CORNER_RADIUS,
                ORDER,
            );
        }

        let close_opts = DrawOpts {
            font_size: BUTTON_FONT_SIZE,
            font_id: self.font_id,
            color: color_u8(theme.muted),
            ..DrawOpts::default()
        };
        let ui = sugarloaf.text_mut();
        let label_w = ui.measure("×", &close_opts);
        let label_x = bx + (bw - label_w) / 2.0;
        let label_y = by + (bh - BUTTON_FONT_SIZE) / 2.0;
        ui.draw(label_x, label_y, "×", &close_opts);
    }
}
