// Copyright (c) 2023-present, Raphael Amorim.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

use rio_backend::sugarloaf::font::FontLibrary;
use rio_backend::sugarloaf::text::DrawOpts;
use rio_backend::sugarloaf::Sugarloaf;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ConfirmQuitAction {
    Close,
    #[default]
    Cancel,
}

#[derive(Default)]
pub struct ConfirmQuit {
    clipboard: Option<(
        usize,
        std::sync::Arc<rio_backend::clipboard::kitty::Request>,
    )>,
    active: bool,
    tab_id: Option<usize>,
    other_tabs: bool,
    hovered: Option<ConfirmQuitAction>,
    pressed: Option<ConfirmQuitAction>,
    focused: ConfirmQuitAction,
    font_library: Option<FontLibrary>,
    font_id: Option<usize>,
}

impl ConfirmQuit {
    #[inline]
    pub fn is_active(&self) -> bool {
        self.active
    }
    #[inline]
    pub fn set_active(&mut self, active: bool) {
        self.clipboard = None;
        self.active = active;
        self.tab_id = None;
        self.other_tabs = false;
        self.hovered = None;
        self.pressed = None;
        self.focused = ConfirmQuitAction::Cancel;
    }

    pub fn confirm_tab(&mut self, tab_id: usize) {
        self.set_active(true);
        self.tab_id = Some(tab_id);
    }

    pub fn confirm_clipboard(
        &mut self,
        route: usize,
        request: std::sync::Arc<rio_backend::clipboard::kitty::Request>,
    ) {
        self.set_active(true);
        self.clipboard = Some((route, request));
    }

    pub fn take_clipboard(
        &mut self,
    ) -> Option<(
        usize,
        std::sync::Arc<rio_backend::clipboard::kitty::Request>,
    )> {
        let pending = self.clipboard.take()?;
        self.set_active(false);
        Some(pending)
    }

    pub fn clipboard_route(&self) -> Option<usize> {
        self.clipboard.as_ref().map(|(route, _)| *route)
    }
    pub fn clipboard_request(&self) -> Option<&rio_backend::clipboard::kitty::Request> {
        self.clipboard.as_ref().map(|(_, request)| request.as_ref())
    }

    pub fn tab_id(&self) -> Option<usize> {
        self.tab_id
    }

    pub fn confirm_other_tabs(&mut self, tab_id: usize) {
        self.confirm_tab(tab_id);
        self.other_tabs = true;
    }

    pub fn other_tabs(&self) -> bool {
        self.other_tabs
    }

    fn panel_rect(dimensions: (f32, f32, f32)) -> [f32; 4] {
        let (width, height, scale) = dimensions;
        let width = width / scale;
        let height = height / scale;
        let panel_width = 380.0_f32.min((width - 32.0).max(1.0));
        let panel_height = 164.0_f32.min((height - 16.0).max(1.0));
        [
            (width - panel_width) / 2.0,
            (height - panel_height) / 2.0,
            panel_width,
            panel_height,
        ]
    }

    fn button_rect(action: ConfirmQuitAction, dimensions: (f32, f32, f32)) -> [f32; 4] {
        let [x, y, width, height] = Self::panel_rect(dimensions);
        let padding = 20.0_f32.min(width / 8.0).min(height / 8.0);
        let gap = 8.0_f32.min(width / 8.0);
        let button_width = (width - padding * 2.0 - gap) / 2.0;
        let button_height = 32.0_f32.min((height - padding * 2.0).max(0.0));
        [
            x + padding
                + if action == ConfirmQuitAction::Close {
                    button_width + gap
                } else {
                    0.0
                },
            y + height - padding - button_height,
            button_width,
            button_height,
        ]
    }

    pub fn hit_test(
        &self,
        x: f32,
        y: f32,
        dimensions: (f32, f32, f32),
    ) -> Option<ConfirmQuitAction> {
        if !self.active {
            return None;
        }
        [ConfirmQuitAction::Cancel, ConfirmQuitAction::Close]
            .into_iter()
            .find(|action| {
                let [bx, by, bw, bh] = Self::button_rect(*action, dimensions);
                x >= bx && x < bx + bw && y >= by && y < by + bh
            })
    }

    pub fn hovered_button(&self) -> Option<ConfirmQuitAction> {
        self.hovered
    }

    pub fn clear_hover(&mut self) -> bool {
        self.hovered.take().is_some()
    }

    pub fn hover(&mut self, x: f32, y: f32, dimensions: (f32, f32, f32)) -> bool {
        let hovered = self.hit_test(x, y, dimensions);
        let changed = hovered != self.hovered;
        self.hovered = hovered;
        changed
    }

    pub fn press(&mut self, x: f32, y: f32, dimensions: (f32, f32, f32)) {
        self.hover(x, y, dimensions);
        self.pressed = self.hovered;
        if let Some(action) = self.pressed {
            self.focused = action;
        }
    }

    pub fn release(
        &mut self,
        x: f32,
        y: f32,
        dimensions: (f32, f32, f32),
    ) -> Option<ConfirmQuitAction> {
        self.hover(x, y, dimensions);
        self.pressed
            .take()
            .filter(|action| Some(*action) == self.hovered)
    }

    pub fn toggle_focus(&mut self) {
        self.focused = match self.focused {
            ConfirmQuitAction::Close => ConfirmQuitAction::Cancel,
            ConfirmQuitAction::Cancel => ConfirmQuitAction::Close,
        };
    }

    pub fn focused_button(&self) -> ConfirmQuitAction {
        self.focused
    }

    pub fn render(
        &mut self,
        sugarloaf: &mut Sugarloaf,
        dimensions: (f32, f32, f32),
        theme: &super::window_chrome::Theme,
    ) {
        if !self.active {
            return;
        }
        let library = sugarloaf.font_library();
        if self
            .font_library
            .as_ref()
            .is_none_or(|old| !std::sync::Arc::ptr_eq(&old.inner, &library.inner))
        {
            self.font_id = super::window_chrome::load_ui_font(library);
            self.font_library = Some(library.clone());
        }
        let (width, height, scale) = dimensions;
        let width = width / scale;
        let height = height / scale;
        let [x, y, panel_width, panel_height] = Self::panel_rect(dimensions);
        sugarloaf.rect(0.0, 0.0, width, height, [0.0, 0.0, 0.0, 0.2], 0.0, 30);
        sugarloaf.rounded_rect(
            x,
            y,
            panel_width,
            panel_height,
            theme.border,
            0.1,
            theme.radius,
            30,
        );
        let pixel = (1.0 / scale).min(panel_width / 2.0).min(panel_height / 2.0);
        sugarloaf.rounded_rect(
            x + pixel,
            y + pixel,
            panel_width - pixel * 2.0,
            panel_height - pixel * 2.0,
            theme.surface,
            0.11,
            (theme.radius - pixel).max(0.0),
            30,
        );
        let clipboard_detail = self.clipboard.as_ref().map(|(_, request)| {
            let name: String = request
                .requester_name()
                .chars()
                .filter(|c| !c.is_control())
                .take(80)
                .collect();
            if name.is_empty() {
                "A terminal program requests clipboard access.".to_owned()
            } else {
                format!("{name} requests access for this session.")
            }
        });
        for (text, size, color, offset) in [
            (
                if let Some((_, request)) = &self.clipboard {
                    match request.as_ref() {
                        rio_backend::clipboard::kitty::Request::Read { .. } => {
                            "Allow clipboard read?"
                        }
                        rio_backend::clipboard::kitty::Request::Write { .. } => {
                            "Allow clipboard write?"
                        }
                    }
                } else if self.other_tabs {
                    "Close other tabs?"
                } else if self.tab_id.is_some() {
                    "Close this tab?"
                } else {
                    "Close this window?"
                },
                16.0,
                theme.foreground,
                20.0,
            ),
            (
                clipboard_detail
                    .as_deref()
                    .unwrap_or("Running sessions will be closed."),
                13.0,
                theme.muted,
                52.0,
            ),
            (
                if self.clipboard.is_some() {
                    "Y to allow · N / Esc to deny"
                } else {
                    "Y to close · N / Esc to cancel"
                },
                11.0,
                theme.muted,
                76.0,
            ),
        ] {
            let opts = DrawOpts {
                font_size: size,
                font_id: self.font_id,
                color: color.map(|v| (v.clamp(0.0, 1.0) * 255.0) as u8),
                ..DrawOpts::default()
            };
            let text = super::window_chrome::fit_text(
                sugarloaf,
                text,
                (panel_width - 40.0).max(0.0),
                &opts,
            );
            sugarloaf
                .text_mut()
                .draw(x + 20.0, y + offset, &text, &opts);
        }

        for (action, label) in [
            (
                ConfirmQuitAction::Cancel,
                if self.clipboard.is_some() {
                    "Deny"
                } else {
                    "Cancel"
                },
            ),
            (
                ConfirmQuitAction::Close,
                if self.clipboard.is_some() {
                    "Allow"
                } else if self.other_tabs {
                    "Close other tabs"
                } else if self.tab_id.is_some() {
                    "Close tab"
                } else {
                    "Close window"
                },
            ),
        ] {
            let [bx, by, bw, bh] = Self::button_rect(action, dimensions);
            let hovered = self.hovered == Some(action);
            let pressed = hovered && self.pressed == Some(action);
            let focused = self.focused == action;
            let fill = if pressed {
                theme.border
            } else if hovered {
                theme.hover
            } else {
                theme.surface
            };
            sugarloaf.rounded_rect(
                bx,
                by,
                bw,
                bh,
                if focused { theme.accent } else { theme.border },
                0.2,
                theme.control_radius(),
                30,
            );
            sugarloaf.rounded_rect(
                bx + pixel,
                by + pixel,
                (bw - pixel * 2.0).max(0.0),
                (bh - pixel * 2.0).max(0.0),
                fill,
                0.21,
                (theme.control_radius() - pixel).max(0.0),
                30,
            );
            let opts = DrawOpts {
                font_size: 13.0,
                font_id: self.font_id,
                color: theme.foreground.map(|v| (v.clamp(0.0, 1.0) * 255.0) as u8),
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
            let label_width = ui.measure(&label, &opts);
            ui.draw(bx + (bw - label_width) / 2.0, label_y, &label, &opts);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_activation_requires_press_and_release_on_the_same_button() {
        for scale in [1.0, 1.25, 2.0] {
            let dims = (800.0 * scale, 600.0 * scale, scale);
            let mut dialog = ConfirmQuit::default();
            dialog.set_active(true);
            let [x, y, w, h] = ConfirmQuit::button_rect(ConfirmQuitAction::Close, dims);
            let (x, y) = (x + w / 2.0, y + h / 2.0);
            assert!(dialog.hover(x, y, dims));
            assert!(!dialog.hover(x, y, dims));
            assert_eq!(dialog.hovered_button(), Some(ConfirmQuitAction::Close));
            assert_eq!(dialog.release(x, y, dims), None);
            dialog.press(x, y, dims);
            assert_eq!(dialog.release(0.0, 0.0, dims), None);
            dialog.press(x, y, dims);
            assert_eq!(dialog.release(x, y, dims), Some(ConfirmQuitAction::Close));
            assert_eq!(dialog.release(x, y, dims), None);
            assert!(dialog.clear_hover());
            assert_eq!(dialog.hovered_button(), None);
            dialog.set_active(false);
            assert_eq!(dialog.hit_test(x, y, dims), None);
        }
    }

    #[test]
    fn reopening_resets_focus_and_pressed_state() {
        let mut dialog = ConfirmQuit::default();
        dialog.set_active(true);
        assert_eq!(dialog.focused_button(), ConfirmQuitAction::Cancel);
        dialog.toggle_focus();
        assert_eq!(dialog.focused_button(), ConfirmQuitAction::Close);
        dialog.set_active(false);
        dialog.set_active(true);
        assert_eq!(dialog.focused_button(), ConfirmQuitAction::Cancel);
        assert_eq!(dialog.hovered_button(), None);
        assert_eq!(dialog.pressed, None);
    }
}
