// Copyright (c) 2023-present, Raphael Amorim.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.
//
// island.rs was originally retired from boo editor
// which is licensed under MIT license.

mod tabs;

use crate::context::ContextManager;
use crate::renderer::helpers::spring::Spring;
use crate::renderer::tab_glass::Status;
use rio_backend::config::colors::Colors;
use rio_backend::config::navigation::{CloseButton, TabGeometry, TabStyle};
use rio_backend::event::EventProxy;
use rio_backend::program_status::{BlockedKind, ProgramState, ProgramStatusRecord};
use rio_backend::sugarloaf::text::DrawOpts;
use rio_backend::sugarloaf::Sugarloaf;
use rustc_hash::FxHashMap;
use std::time::Instant;

/// Bell dot for tabs that rang while in the background, and the gap
/// between it and the tab title. A dot, not a glyph: the UI text layer
/// resolves one font per run, so a bell glyph exists only where an
/// emoji font is installed and resolvable, while a dot renders
/// identically everywhere. Drawn at rect order 1: the tab backgrounds
/// are submitted at order 0 AFTER the title pass, so a same-order dot
/// would be painted over and invisible in every multi-tab strip.
const BELL_DOT_SIZE: f32 = 6.0;
const BELL_GAP: f32 = 4.0;
const DRAG_THRESHOLD: f32 = 4.0;
const DRAG_MAX_DT: f32 = 0.05;

/// Color picker constants
const PICKER_SWATCH_SIZE: f32 = 18.0;
const PICKER_SWATCH_GAP: f32 = 4.0;
const PICKER_PADDING: f32 = 6.0;
const PICKER_INPUT_HEIGHT: f32 = 26.0;
const PICKER_INPUT_FONT_SIZE: f32 = 12.0;
const PICKER_INPUT_MARGIN_TOP: f32 = 8.0;
const PICKER_TOP_PADDING: f32 = 4.0;
const PICKER_HEIGHT: f32 = PICKER_TOP_PADDING
    + PICKER_SWATCH_SIZE
    + PICKER_PADDING * 2.0
    + PICKER_INPUT_MARGIN_TOP
    + PICKER_INPUT_HEIGHT
    + PICKER_PADDING;
const PICKER_COLORS: [[f32; 4]; 6] = [
    // red
    [0.86, 0.26, 0.27, 1.0],
    // orange
    [0.90, 0.57, 0.22, 1.0],
    // yellow
    [0.85, 0.78, 0.25, 1.0],
    // green
    [0.34, 0.70, 0.38, 1.0],
    // blue
    [0.30, 0.55, 0.85, 1.0],
    // purple
    [0.68, 0.40, 0.80, 1.0],
];

/// Left margin on macOS to account for traffic light buttons
#[cfg(target_os = "macos")]
const ISLAND_MARGIN_LEFT_MACOS: f32 = 76.0;

const CLOSE_GLYPH_RATIO: f32 = 0.175;
const CLOSE_MIN_ISLAND_WIDTH: f32 = 64.0;
const CLOSE_ALPHA_IDLE: f32 = 0.55;
const CLOSE_ALPHA_HOVER: f32 = 0.95;
const CLOSE_STROKE_RATIO: f32 = 0.06;
const INACTIVE_CUSTOM_MUTE: f32 = 0.55;

/// Status strings are plain text. Strip invisible direction/format controls
/// before showing program-provided labels outside the terminal grid.
fn status_display_text(text: &str) -> String {
    text.chars()
        .filter(|c| {
            !matches!(c,
                '\u{00ad}' | '\u{061c}' | '\u{200b}'..='\u{200f}'
                | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}'
            )
        })
        .collect()
}

struct TabDrag {
    // Index of the dragged tab, follows the tab as it reorders.
    tab_index: usize,
    // Mouse x at press (unscaled), for the drag threshold.
    press_x: f32,
    // press_x − tab_left_x, keeps the grab point under the cursor.
    grab_offset: f32,
    // Latest unscaled mouse x.
    current_x: f32,
    // True once movement exceeded `DRAG_THRESHOLD`.
    started: bool,
}

struct TabPresentation {
    id: usize,
    custom_color: Option<[f32; 4]>,
    title: String,
    status: Option<ProgramStatusRecord>,
    icon: super::app_icons::Icon,
    remote: bool,
    bell: bool,
}

#[derive(Clone, PartialEq)]
pub struct TabStripLayout {
    pub left_margin: f32,
    pub tab_width: f32,
    pub tabs_width: f32,
    widths: Vec<f32>,
    positions: Vec<f32>,
    topmost: Option<usize>,
}

impl TabStripLayout {
    pub fn width(&self, index: usize) -> f32 {
        self.widths.get(index).copied().unwrap_or(self.tab_width)
    }

    pub fn left(&self, index: usize) -> f32 {
        if let Some(x) = self.positions.get(index) {
            return *x;
        }
        self.left_margin
            + if self.widths.is_empty() {
                index as f32 * self.tab_width
            } else {
                self.widths.iter().take(index).sum::<f32>()
            }
    }

    pub fn tab_at(&self, x: f32) -> Option<usize> {
        if !self.positions.is_empty() {
            if let Some(index) = self.topmost {
                if x >= self.left(index) && x < self.left(index) + self.width(index) {
                    return Some(index);
                }
            }
            return self
                .positions
                .iter()
                .enumerate()
                .rev()
                .find_map(|(i, left)| {
                    (x >= *left && x < *left + self.width(i)).then_some(i)
                });
        }
        let mut remaining = x - self.left_margin;
        if remaining < 0.0 || remaining >= self.tabs_width || self.tab_width <= 0.0 {
            return None;
        }
        if self.widths.is_empty() {
            return Some((remaining / self.tab_width) as usize);
        }
        for (index, width) in self.widths.iter().enumerate() {
            if remaining < *width {
                return Some(index);
            }
            remaining -= width;
        }
        None
    }
}

struct IslandFills {
    active: [f32; 4],
    close_hover: [f32; 4],
}

fn tab_fills(bg: [f32; 4]) -> IslandFills {
    // Translucent contrast overlays preserve the window's blur and opacity.
    let light = 0.2126 * bg[0] + 0.7152 * bg[1] + 0.0722 * bg[2] > 0.5;
    if light {
        IslandFills {
            active: [0.0, 0.0, 0.0, 0.08],
            close_hover: [0.0, 0.0, 0.0, 0.09],
        }
    } else {
        IslandFills {
            active: [1.0, 1.0, 1.0, 0.11],
            close_hover: [1.0, 1.0, 1.0, 0.14],
        }
    }
}

fn draw_tab_icon(
    sugarloaf: &mut Sugarloaf,
    [x, cy]: [f32; 2],
    color: [f32; 4],
    order: u8,
    icon: super::app_icons::Icon,
    font_id: Option<usize>,
    size: f32,
) {
    if super::app_icons::draw(sugarloaf, icon, x + size / 2.0, cy, size, color, font_id) {
        return;
    }
    #[cfg(target_os = "windows")]
    if crate::renderer::window_chrome::draw_icon(
        sugarloaf,
        "\u{e756}",
        x + size / 2.0,
        cy,
        size,
        color,
    ) {
        return;
    }
    let unit = size / 24.0;
    sugarloaf.line(
        x + 5.0 * unit,
        cy - 3.0 * unit,
        x + 8.0 * unit,
        cy,
        1.2,
        0.0,
        color,
        order,
    );
    sugarloaf.line(
        x + 8.0 * unit,
        cy,
        x + 5.0 * unit,
        cy + 3.0 * unit,
        1.2,
        0.0,
        color,
        order,
    );
    sugarloaf.line(
        x + 11.0 * unit,
        cy + 3.0 * unit,
        x + 15.0 * unit,
        cy + 3.0 * unit,
        1.2,
        0.0,
        color,
        order,
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_island(
    sugarloaf: &mut Sugarloaf,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radius: f32,
    fill: [f32; 4],
    glass_intensity: f32,
    status: Option<Status>,
    order: u8,
) {
    let (x, y, w, h) = {
        let scale = sugarloaf.scale_factor();
        let snap = |v| crate::renderer::window_chrome::snap(v, scale);
        let left = snap(x);
        let top = snap(y);
        (left, top, snap(x + w) - left, snap(y + h) - top)
    };
    if fill[3] > 0.0 {
        sugarloaf.rounded_rect(x, y, w, h, fill, 0.05, radius, order);
    }
    super::tab_glass::draw(
        sugarloaf,
        [x, y, w, h],
        radius,
        glass_intensity,
        status,
        order,
    );
}

/// Style geometry shared by measurement, drawing, and pointer targets.
#[derive(Clone, Copy)]
struct TabAppearance {
    geometry: TabGeometry,
}

impl Default for TabAppearance {
    fn default() -> Self {
        Self::from(TabStyle::default())
    }
}

impl From<TabStyle> for TabAppearance {
    fn from(style: TabStyle) -> Self {
        Self {
            geometry: style.geometry(),
        }
    }
}

impl TabAppearance {
    fn layout(
        &self,
        window_width: f32,
        scale_factor: f32,
        num_tabs: usize,
        max_tab_width: f32,
    ) -> TabStripLayout {
        #[cfg(target_os = "macos")]
        let left_margin = (ISLAND_MARGIN_LEFT_MACOS + self.geometry.gap / 2.0)
            .max(self.geometry.margin.left)
            - self.geometry.gap / 2.0;
        #[cfg(not(target_os = "macos"))]
        let left_margin = self.geometry.margin.left - self.geometry.gap / 2.0;
        let right_margin = self.geometry.margin.right - self.geometry.gap / 2.0
            + crate::renderer::window_chrome::controls_width();
        let available_width =
            (window_width / scale_factor - right_margin - left_margin).max(0.0);
        let tab_width =
            (available_width / num_tabs.max(1) as f32).clamp(0.0, max_tab_width.max(0.0));
        TabStripLayout {
            left_margin,
            tab_width,
            tabs_width: tab_width * num_tabs as f32,
            widths: Vec::new(),
            positions: Vec::new(),
            topmost: None,
        }
    }

    fn icon_padding(&self, remote: bool, icon_size: f32) -> f32 {
        let icons = if remote { 2.0 } else { 1.0 };
        self.geometry.padding.left + icons * (icon_size + self.geometry.icon_gap)
    }

    fn title_padding(&self, remote: bool, icon_size: f32) -> f32 {
        self.geometry.gap / 2.0 + self.icon_padding(remote, icon_size)
    }

    fn radius(&self, pill_radius: f32) -> f32 {
        self.geometry.radius.min(pill_radius)
    }

    fn center_y(&self) -> f32 {
        (self.geometry.margin.top
            + self.geometry.padding.top
            + self.geometry.content_top()
            - self.geometry.padding.bottom)
            / 2.0
    }

    fn rect(&self, slot_x: f32, tab_width: f32) -> (f32, f32, f32, f32, f32) {
        // Slot halves carry the gap so visible margins stay independent of tab spacing.
        let x = slot_x + self.geometry.gap / 2.0;
        let w = (tab_width - self.geometry.gap).max(0.0);
        let h = self.geometry.content_top() - self.geometry.margin.top;
        (
            x,
            self.geometry.margin.top,
            w,
            h,
            self.radius(w.min(h) / 2.0),
        )
    }

    fn shows_close(&self, count: usize) -> bool {
        match self.geometry.close_button {
            CloseButton::Always => true,
            CloseButton::Hover => count > 1,
            CloseButton::Never => false,
        }
    }

    fn contains(&self, slot_x: f32, tab_width: f32, x: f32, y: f32) -> bool {
        let (left, top, width, height, radius) = self.rect(slot_x, tab_width);
        if x < left || x >= left + width || y < top || y >= top + height {
            return false;
        }
        let dx = ((x - left - width / 2.0).abs() - (width / 2.0 - radius)).max(0.0);
        let dy = ((y - top - height / 2.0).abs() - (height / 2.0 - radius)).max(0.0);
        dx * dx + dy * dy <= radius * radius
    }

    fn title_end_inset(&self, hovered_close: bool) -> f32 {
        if self.geometry.close_button == CloseButton::Always
            || (self.geometry.close_button == CloseButton::Hover && hovered_close)
        {
            self.geometry.gap / 2.0
                + self.geometry.close_button_inset
                + self.geometry.close_button_size
                + self.geometry.close_button_gap
        } else {
            self.geometry.gap / 2.0 + self.geometry.padding.right
        }
    }

    fn close_center(&self, slot_x: f32, tab_width: f32) -> Option<f32> {
        let (x, _, w, _, _) = self.rect(slot_x, tab_width);
        if self.geometry.close_button == CloseButton::Never {
            return None;
        }
        let minimum = CLOSE_MIN_ISLAND_WIDTH.max(
            self.geometry.close_button_size + self.geometry.close_button_inset * 2.0,
        );
        (w >= minimum).then_some(
            x + w
                - (self.geometry.close_button_inset
                    + self.geometry.close_button_size / 2.0)
                    .min(w / 2.0),
        )
    }

    #[cfg(test)]
    fn close_button_center_x(
        &self,
        layout: &TabStripLayout,
        tab_index: usize,
    ) -> Option<f32> {
        self.close_center(layout.left(tab_index), layout.width(tab_index))
    }

    fn close_hit(&self, slot_x: f32, width: f32, x: f32, y: f32) -> bool {
        self.close_center(slot_x, width).is_some_and(|cx| {
            let half = self.geometry.close_button_size / 2.0;
            (x - cx).abs() <= half && (y - self.center_y()).abs() <= half
        })
    }
}

fn draw_close_button(
    sugarloaf: &mut Sugarloaf,
    cx: f32,
    cy: f32,
    size: f32,
    color: [f32; 4],
    hover: bool,
    order: u8,
) {
    let r = size * CLOSE_GLYPH_RATIO;
    let alpha = if hover {
        CLOSE_ALPHA_HOVER
    } else {
        CLOSE_ALPHA_IDLE
    };
    let color = [color[0], color[1], color[2], color[3] * alpha];
    sugarloaf.line(
        cx - r,
        cy - r,
        cx + r,
        cy + r,
        size * CLOSE_STROKE_RATIO,
        0.0,
        color,
        order,
    );
    sugarloaf.line(
        cx - r,
        cy + r,
        cx + r,
        cy - r,
        size * CLOSE_STROKE_RATIO,
        0.0,
        color,
        order,
    );
}

pub struct Island {
    pub window_header: crate::renderer::window_chrome::Header,
    pub hide_if_single: bool,
    /// Cap on tab width in logical px (`navigation.max-tab-width`).
    pub max_tab_width: f32,
    pub min_tab_width: f32,
    pub tab_icon_size: f32,
    pub animation: rio_backend::config::navigation::Animation,
    tab_glass_intensity: f32,
    tab_status_width: f32,
    app_icons: super::app_icons::Library,
    tabs: tabs::Tabs,
    pub inactive_text_color: [f32; 4],
    pub active_text_color: [f32; 4],
    program_status: Option<ProgramStatusRecord>,
    program_status_source: Option<(usize, u64)>,
    status_colors: [[f32; 4]; 3],
    /// Which tab has the color picker open (None = closed)
    color_picker_tab: Option<usize>,
    /// Current rename input text while picker is open
    rename_input: String,
    /// Caret blink timer
    rename_caret_time: Instant,
    /// In-progress tab drag (reorder by dragging)
    drag: Option<TabDrag>,
    /// Timestamp of the last spring advance, for per-frame dt.
    last_anim_frame: Instant,
    pointer: Option<[f32; 2]>,
}

impl Island {
    pub fn new(
        inactive_text_color: [f32; 4],
        active_text_color: [f32; 4],
        hide_if_single: bool,
        max_tab_width: f32,
    ) -> Self {
        let colors = Colors::default();
        Self {
            window_header: crate::renderer::window_chrome::Header::new(),
            hide_if_single,
            max_tab_width,
            min_tab_width: rio_backend::config::navigation::default_min_tab_width(),
            tab_icon_size: rio_backend::config::navigation::default_tab_icon_size(),
            animation: rio_backend::config::navigation::Animation::default(),
            tab_glass_intensity:
                rio_backend::config::navigation::default_tab_glass_intensity(),
            tab_status_width: rio_backend::config::navigation::default_tab_status_width(),
            app_icons: super::app_icons::Library::default(),
            tabs: tabs::Tabs::default(),
            inactive_text_color,
            active_text_color,
            program_status: None,
            program_status_source: None,
            status_colors: [colors.green, colors.yellow, colors.red],
            color_picker_tab: None,
            rename_input: String::new(),
            rename_caret_time: Instant::now(),
            drag: None,
            last_anim_frame: Instant::now(),
            pointer: None,
        }
    }

    pub fn configure(
        &mut self,
        navigation: &rio_backend::config::navigation::Navigation,
    ) {
        self.hide_if_single = navigation.hide_if_single;
        self.window_header.theme =
            self.window_header.theme.with_style(navigation.tab_style);
        self.animation = navigation.animation;
        self.tabs.appearance = TabAppearance {
            geometry: navigation.tab_geometry(),
        };
        self.max_tab_width = rio_backend::config::navigation::clamp_max_tab_width(
            navigation.max_tab_width,
        );
        self.min_tab_width = rio_backend::config::navigation::clamp_min_tab_width(
            navigation.min_tab_width,
            self.max_tab_width,
        );
        self.tab_icon_size = rio_backend::config::navigation::clamp_tab_icon_size(
            navigation.tab_icon_size,
        );
        self.tab_status_width = rio_backend::config::navigation::clamp_tab_status_width(
            navigation.tab_status_width,
        );
        self.app_icons.configure(&navigation.app_icons);
        self.tabs.invalidate_measurements();
        self.tab_glass_intensity =
            rio_backend::config::navigation::clamp_tab_glass_intensity(
                navigation.tab_glass_intensity,
            );
    }

    pub fn shows_close_button(&self, count: usize) -> bool {
        self.tabs.appearance.shows_close(count)
    }

    pub fn close_button_hit(
        &self,
        layout: &TabStripLayout,
        index: usize,
        x: f32,
        y: f32,
    ) -> bool {
        layout.topmost != Some(index)
            && self.tabs.appearance.close_hit(
                layout.left(index),
                layout.width(index),
                x,
                y,
            )
    }

    /// Share per-tab geometry between rendering, dragging, and hit testing.
    pub fn tab_layout(
        &self,
        window_width: f32,
        scale_factor: f32,
        num_tabs: usize,
        context_manager: &ContextManager<EventProxy>,
    ) -> TabStripLayout {
        debug_assert_eq!(num_tabs, context_manager.len());
        self.tabs.layout(
            window_width,
            scale_factor,
            context_manager,
            self.max_tab_width,
        )
    }

    fn render_tab_icons(
        &self,
        sugarloaf: &mut Sugarloaf,
        tab: &TabPresentation,
        x: f32,
        width: f32,
        color: [f32; 4],
        order: u8,
    ) {
        if width
            < self
                .tabs
                .appearance
                .icon_padding(tab.remote, self.tab_icon_size)
        {
            // Crowded strips can shrink below the normal icon padding. Keep
            // one centered icon visible so compressed tabs remain identifiable.
            let size = self.tab_icon_size.min((width - 4.0).max(0.0));
            if size > 0.0 {
                draw_tab_icon(
                    sugarloaf,
                    [x + (width - size) / 2.0, self.tabs.appearance.center_y()],
                    color,
                    order,
                    if tab.remote {
                        super::app_icons::SSH_ICON
                    } else {
                        tab.icon
                    },
                    self.window_header.app_font,
                    size,
                );
            }
            return;
        }
        let mut x = x + self.tabs.appearance.geometry.padding.left;
        if tab.remote {
            draw_tab_icon(
                sugarloaf,
                [x, self.tabs.appearance.center_y()],
                color,
                order,
                super::app_icons::SSH_ICON,
                self.window_header.app_font,
                self.tab_icon_size,
            );
            x += self.tab_icon_size + self.tabs.appearance.geometry.icon_gap;
        }
        draw_tab_icon(
            sugarloaf,
            [x, self.tabs.appearance.center_y()],
            color,
            order,
            tab.icon,
            self.window_header.app_font,
            self.tab_icon_size,
        );
    }

    /// Update the pointer used for tab and close-button hover.
    /// Returns true when the state changed (the caller redraws).
    pub fn set_tab_hover(&mut self, pointer: Option<[f32; 2]>) -> bool {
        let changed =
            self.tabs.pointer_target(self.pointer) != self.tabs.pointer_target(pointer);
        self.pointer = pointer;
        changed
    }

    pub fn update_colors(&mut self, colors: &Colors) {
        let radius = self.window_header.theme.radius;
        self.window_header.theme = super::window_chrome::Theme::new(colors);
        self.window_header.theme.radius = radius;
        self.inactive_text_color = colors.tabs;
        self.active_text_color = colors.tabs_active;
        self.status_colors = [colors.green, colors.yellow, colors.red];
    }

    /// Read the published status without waiting for the PTY parser.
    fn sync_focused_program_status<
        T: rio_backend::event::EventListener + Clone + Send + 'static,
    >(
        &mut self,
        context_manager: &ContextManager<T>,
    ) {
        let context = context_manager.current();
        let metadata = context.metadata.lock();
        self.sync_program_status(context.route_id, &metadata);
    }

    fn sync_program_status(
        &mut self,
        route_id: usize,
        metadata: &rio_backend::crosswords::TerminalMetadata,
    ) {
        let source = (route_id, metadata.status_revision);
        if self.program_status_source == Some(source) {
            return;
        }
        self.program_status_source = Some(source);
        self.program_status = metadata.status.clone();
    }

    #[cfg(test)]
    fn program_status_title(&self, fallback: &str) -> String {
        Self::status_title(self.program_status.as_ref(), fallback)
    }

    fn status_for_tab<T: rio_backend::event::EventListener + Clone + Send + 'static>(
        &self,
        context_manager: &ContextManager<T>,
        tab_index: usize,
    ) -> Option<ProgramStatusRecord> {
        if tab_index == context_manager.current_index() {
            self.program_status.clone()
        } else {
            context_manager.program_status_for_tab(tab_index)
        }
    }

    fn status_color(&self, record: &ProgramStatusRecord) -> Option<[f32; 4]> {
        match record.state {
            ProgramState::Working | ProgramState::Done => Some(self.status_colors[0]),
            ProgramState::Blocked => Some(self.status_colors[1]),
            ProgramState::Error => Some(self.status_colors[2]),
            ProgramState::Idle | ProgramState::Clear => None,
        }
    }

    fn status_rim(
        &self,
        record: Option<&ProgramStatusRecord>,
        opacity: f32,
    ) -> Option<Status> {
        if self.tab_status_width <= 0.0 {
            return None;
        }
        let record = record?;
        let mut color = self.status_color(record)?;
        color[3] *= opacity;
        Some(Status {
            color,
            progress: record.progress,
            width: self.tab_status_width,
        })
    }

    fn status_title(record: Option<&ProgramStatusRecord>, fallback: &str) -> String {
        let Some(record) = record else {
            return fallback.to_owned();
        };
        let state = match (record.state, record.kind) {
            (ProgramState::Blocked, Some(BlockedKind::Permission)) => "Needs permission",
            (ProgramState::Blocked, Some(BlockedKind::Question)) => "Needs answer",
            (ProgramState::Blocked, Some(BlockedKind::Auth)) => "Needs login",
            (ProgramState::Blocked, _) => "Blocked",
            (ProgramState::Working, _) => "Working",
            (ProgramState::Done, _) => "Done",
            (ProgramState::Error, _) => "Error",
            (ProgramState::Idle, _) => "Idle",
            (ProgramState::Clear, _) => return fallback.to_owned(),
        };
        let percent = record
            .progress
            .map(|p| format!(" {p}%"))
            .unwrap_or_default();
        let label = record
            .title
            .as_deref()
            .or(record.app.as_deref())
            .unwrap_or(fallback);
        let identity = status_display_text(fallback);
        let label = status_display_text(label);
        let mut title = format!("[{state}{percent}] {identity}");
        if !label.is_empty() && label != identity {
            if !identity.is_empty() {
                title.push_str(" · ");
            }
            title.push_str(&label);
        }
        if let Some(msg) = record.msg.as_deref().filter(|msg| !msg.is_empty()) {
            title.push_str(": ");
            title.push_str(&status_display_text(msg));
        }
        title
    }

    pub fn next_frame_in(
        &self,
        scale: f32,
        frame_interval: std::time::Duration,
    ) -> Option<std::time::Duration> {
        self.tabs.next_frame_in(scale, self.pointer).map(|delay| {
            if self.is_dragging() {
                std::time::Duration::ZERO
            } else if self.animation == rio_backend::config::navigation::Animation::Medium
            {
                delay.max(frame_interval * 2)
            } else {
                delay
            }
        })
    }

    /// Arm a tab drag at mouse press. The drag only `started`s once the
    /// pointer moves past `DRAG_THRESHOLD`.
    pub fn start_drag(&mut self, tab_index: usize, grab_offset: f32, x: f32) {
        self.drag = Some(TabDrag {
            tab_index,
            press_x: x,
            grab_offset,
            current_x: x,
            started: false,
        });
    }

    /// Feed a mouse move into the armed drag. Returns `true` once the
    /// drag is active (threshold exceeded).
    pub fn update_drag(&mut self, x: f32) -> bool {
        match self.drag.as_mut() {
            Some(drag) => {
                drag.current_x = x;
                if !drag.started && (x - drag.press_x).abs() > DRAG_THRESHOLD {
                    drag.started = true;
                }
                drag.started
            }
            None => false,
        }
    }

    /// Whether a drag is armed or active.
    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Index of the dragged tab, if a drag is active.
    pub fn drag_index(&self) -> Option<usize> {
        self.drag
            .as_ref()
            .filter(|d| d.started)
            .map(|d| d.tab_index)
    }

    /// Left edge of the floating (dragged) tab, clamped to the tabs
    /// region (`left_margin..left_margin + tabs_width`) — the empty
    /// chrome beyond the last slot is not a valid drop area.
    fn drag_floating_left(&self, layout: &TabStripLayout) -> Option<f32> {
        let drag = self.drag.as_ref().filter(|d| d.started)?;
        let left = drag.current_x - drag.grab_offset;
        // `.max(0.0)` keeps the clamp range valid (min ≤ max) even if a
        // pathologically narrow window makes tabs_width < tab_width.
        let max_left = layout.left_margin
            + (layout.tabs_width - layout.width(drag.tab_index)).max(0.0);
        Some(left.clamp(layout.left_margin, max_left))
    }

    /// Center x of the floating tab — the reference point that decides
    /// which slot the drag targets.
    pub fn drag_center(&self, layout: &TabStripLayout) -> Option<f32> {
        self.drag_floating_left(layout)
            .map(|left| left + layout.width(self.drag_index().unwrap_or(0)) / 2.0)
    }

    /// Finish a drag: seed a settle spring from the floating position
    /// into the slot so the tab slides into place.
    pub fn end_drag(&mut self, layout: &TabStripLayout) {
        if let (Some(left), Some(index)) =
            (self.drag_floating_left(layout), self.drag_index())
        {
            self.tabs.release(index, left);
        }
        self.drag = None;
    }

    /// Drop an armed/active drag without any settle animation.
    pub fn cancel_drag(&mut self) {
        self.drag = None;
    }

    /// New index of tab `i` after the tab at `from` rotated to `to`.
    fn remap_index(i: usize, from: usize, to: usize) -> usize {
        if i == from {
            to
        } else if from < to && i > from && i <= to {
            i - 1
        } else if to < from && i >= to && i < from {
            i + 1
        } else {
            i
        }
    }

    /// Re-key all per-tab-index state after the tab at `from` moved to
    /// `to` (rotate semantics, matching
    /// `ContextManager::move_current_tab_to`).
    pub fn remap_tab_move(&mut self, from: usize, to: usize) {
        if from == to {
            return;
        }
        self.tabs.move_tab(from, to);
        if let Some(picker) = self.color_picker_tab {
            self.color_picker_tab = Some(Self::remap_index(picker, from, to));
        }
        if let Some(ref mut drag) = self.drag {
            drag.tab_index = Self::remap_index(drag.tab_index, from, to);
        }
    }

    /// Re-key per-tab state after tabs `a` and `b` swapped places —
    /// `ContextManager::move_current_to_prev/next` semantics, which swap
    /// (including the wrap-around end-to-end case) instead of rotating.
    pub fn remap_tab_swap(&mut self, a: usize, b: usize) {
        if a == b {
            return;
        }
        self.tabs.swap_tabs(a, b);
        if let Some(picker) = self.color_picker_tab {
            self.color_picker_tab = Some(if picker == a {
                b
            } else if picker == b {
                a
            } else {
                picker
            });
        }
    }

    /// Get the height of the island
    #[inline]
    pub fn height(&self) -> f32 {
        self.tabs.appearance.geometry.height
    }

    pub fn content_top(&self) -> f32 {
        self.tabs.appearance.geometry.content_top()
    }

    pub fn tab_at(&self, layout: &TabStripLayout, x: f32, y: f32) -> Option<usize> {
        let index = layout.tab_at(x)?;
        self.tabs
            .appearance
            .contains(layout.left(index), layout.width(index), x, y)
            .then_some(index)
    }

    /// Render the platform's tab strip.
    #[inline]
    pub fn render(
        &mut self,
        sugarloaf: &mut Sugarloaf,
        dimensions: (f32, f32, f32),
        context_manager: &mut ContextManager<EventProxy>,
        bg_color: [f32; 4],
    ) {
        #[cfg(feature = "profiling")]
        let _timing = crate::profiling::span("tabs.render");
        self.render_tabs(
            sugarloaf,
            dimensions.0,
            dimensions.2,
            context_manager,
            bg_color,
        );
    }

    /// Toggle the color picker for a given tab index
    pub fn toggle_color_picker(
        &mut self,
        tab_index: usize,
        current_title: &str,
        context_manager: &mut ContextManager<EventProxy>,
    ) {
        if self.color_picker_tab == Some(tab_index) {
            self.apply_rename(context_manager);
            self.color_picker_tab = None;
        } else {
            self.color_picker_tab = Some(tab_index);
            // Initialize rename input with custom title or current displayed title
            self.rename_input = context_manager
                .custom_title(tab_index)
                .map(str::to_string)
                .unwrap_or_else(|| current_title.to_string());
            self.rename_caret_time = Instant::now();
        }
    }

    /// Close the color picker, applying any pending rename
    pub fn close_color_picker(
        &mut self,
        context_manager: &mut ContextManager<EventProxy>,
    ) {
        if self.color_picker_tab.is_some() {
            self.apply_rename(context_manager);
        }
        self.color_picker_tab = None;
    }

    /// Dismiss the picker WITHOUT committing a pending rename. Used when the
    /// tab set changes underneath it (e.g. a tab close), where the anchored
    /// index may no longer point at the same tab.
    pub fn dismiss_color_picker(&mut self) {
        self.color_picker_tab = None;
    }

    /// Apply the rename input as a custom title for the current picker tab
    fn apply_rename(&mut self, context_manager: &mut ContextManager<EventProxy>) {
        if let Some(tab) = self.color_picker_tab {
            let trimmed = self.rename_input.trim().to_string();
            let title = (!trimmed.is_empty()).then_some(trimmed);
            context_manager.set_custom_title(tab, title);
        }
    }

    /// Handle keyboard input while the color picker (with rename field) is open.
    /// Returns true if input was consumed.
    /// Handle one key event for the rename input. The caller
    /// (`has_key_wait`'s `Modal::IslandRename` arm) already verified
    /// the picker is open via `active_modal` and consumes the event
    /// unconditionally, so there is nothing to return.
    pub fn handle_rename_input(
        &mut self,
        key_event: &rio_window::event::KeyEvent,
        context_manager: &mut ContextManager<EventProxy>,
    ) {
        use rio_window::event::ElementState;
        use rio_window::keyboard::{Key, NamedKey};

        if key_event.state != ElementState::Pressed {
            return; // consume release events too
        }

        match &key_event.logical_key {
            Key::Named(NamedKey::Escape) => {
                // Cancel — discard input, close picker
                self.color_picker_tab = None;
            }
            Key::Named(NamedKey::Enter) => {
                // Confirm — apply rename and close
                self.apply_rename(context_manager);
                self.color_picker_tab = None;
            }
            Key::Named(NamedKey::Backspace) => {
                self.rename_input.pop();
                self.rename_caret_time = Instant::now();
            }
            _ => {
                if let Some(text) = key_event.text.as_ref() {
                    self.append_rename_text(text.as_str());
                }
            }
        }
    }

    /// Append committed or typed text to the rename input, applying
    /// the shared overlay input policy (`is_printable_text`) so the
    /// key path and the IME commit path can never drift. Returns
    /// whether text was actually appended (same contract as
    /// `CommandPalette::append_query`).
    pub fn append_rename_text(&mut self, text: &str) -> bool {
        if self.color_picker_tab.is_none() || !crate::renderer::is_printable_text(text) {
            return false;
        }
        self.rename_input.push_str(text);
        self.rename_caret_time = Instant::now();
        true
    }

    /// Check if a click hits a color swatch in the picker.
    /// Returns true if the click was consumed.
    pub fn handle_color_picker_click(
        &mut self,
        mouse_x: f32,
        mouse_y: f32,
        scale_factor: f32,
        window_width: f32,
        num_tabs: usize,
        context_manager: &mut ContextManager<EventProxy>,
    ) -> bool {
        let picker_tab = match self.color_picker_tab {
            Some(t) => t,
            None => return false,
        };

        let mouse_x_unscaled = mouse_x / scale_factor;
        let mouse_y_unscaled = mouse_y / scale_factor;

        // Compute the same tab layout as render()
        let layout =
            self.tab_layout(window_width, scale_factor, num_tabs, context_manager);
        let tab_x = layout.left(picker_tab);
        let tab_width = layout.width(picker_tab);

        // Picker is rendered just below the island
        let picker_y = self.height();

        // Check if click is within picker vertical range
        if mouse_y_unscaled < picker_y || mouse_y_unscaled > picker_y + PICKER_HEIGHT {
            // Click outside picker — apply rename and close
            self.apply_rename(context_manager);
            self.color_picker_tab = None;
            return false;
        }

        let picker_start_x =
            Self::picker_left(tab_x, tab_width, window_width / scale_factor)
                + PICKER_PADDING;

        // Check each swatch
        let swatch_y = picker_y + PICKER_PADDING + PICKER_TOP_PADDING;
        let swatch_y_end = swatch_y + PICKER_SWATCH_SIZE;
        for (i, color) in PICKER_COLORS.iter().enumerate() {
            let swatch_x =
                picker_start_x + i as f32 * (PICKER_SWATCH_SIZE + PICKER_SWATCH_GAP);
            if mouse_x_unscaled >= swatch_x
                && mouse_x_unscaled <= swatch_x + PICKER_SWATCH_SIZE
                && mouse_y_unscaled >= swatch_y
                && mouse_y_unscaled <= swatch_y_end
            {
                context_manager.set_custom_color(picker_tab, Some(*color));
                self.apply_rename(context_manager);
                self.color_picker_tab = None;
                return true;
            }
        }

        // Reset swatch — clears any custom color for this tab
        let reset_x = picker_start_x
            + PICKER_COLORS.len() as f32 * (PICKER_SWATCH_SIZE + PICKER_SWATCH_GAP);
        if mouse_x_unscaled >= reset_x
            && mouse_x_unscaled <= reset_x + PICKER_SWATCH_SIZE
            && mouse_y_unscaled >= swatch_y
            && mouse_y_unscaled <= swatch_y_end
        {
            context_manager.set_custom_color(picker_tab, None);
            self.apply_rename(context_manager);
            self.color_picker_tab = None;
            return true;
        }

        // Clicked in picker area but not on a swatch
        true
    }

    /// Render the color picker dropdown below a tab
    fn picker_left(tab_x: f32, tab_width: f32, window_width: f32) -> f32 {
        let slots = PICKER_COLORS.len() + 1;
        let width = slots as f32 * PICKER_SWATCH_SIZE
            + (slots - 1) as f32 * PICKER_SWATCH_GAP
            + PICKER_PADDING * 2.0;
        (tab_x + (tab_width - width) / 2.0).clamp(0.0, (window_width - width).max(0.0))
    }

    fn render_color_picker(
        &mut self,
        sugarloaf: &mut Sugarloaf,
        tab_x: f32,
        tab_width: f32,
        selected_color: Option<[f32; 4]>,
    ) {
        let padding = PICKER_PADDING;
        let bg_y = self.height();
        let theme = self.window_header.theme;

        // Compute total swatches width to derive the consistent inner content width
        // N color swatches + 1 reset swatch
        let slot_count = PICKER_COLORS.len() + 1;
        let total_swatches_width = slot_count as f32 * PICKER_SWATCH_SIZE
            + (slot_count - 1) as f32 * PICKER_SWATCH_GAP;
        let inner_width = total_swatches_width;
        let bg_width = inner_width + padding * 2.0;
        let bg_x = Self::picker_left(
            tab_x,
            tab_width,
            sugarloaf.window_size().width / sugarloaf.scale_factor(),
        );
        let content_x = bg_x + padding;

        // Background
        sugarloaf.rounded_rect(
            bg_x,
            bg_y,
            bg_width,
            PICKER_HEIGHT,
            theme.border,
            0.0,
            theme.radius,
            10,
        );
        let pixel = 1.0 / sugarloaf.scale_factor();
        sugarloaf.rounded_rect(
            bg_x + pixel,
            bg_y + pixel,
            bg_width - pixel * 2.0,
            PICKER_HEIGHT - pixel * 2.0,
            theme.surface,
            0.01,
            (theme.radius - pixel).max(0.0),
            10,
        );

        // Swatches — aligned to content_x
        let swatch_y = bg_y + padding + PICKER_TOP_PADDING;
        for (i, color) in PICKER_COLORS.iter().enumerate() {
            let sx = content_x + i as f32 * (PICKER_SWATCH_SIZE + PICKER_SWATCH_GAP);
            let is_selected = selected_color == Some(*color);

            // Draw white border behind selected swatch
            if is_selected {
                let border = 2.0;
                sugarloaf.rounded_rect(
                    sx - border,
                    swatch_y - border,
                    PICKER_SWATCH_SIZE + border * 2.0,
                    PICKER_SWATCH_SIZE + border * 2.0,
                    theme.accent,
                    0.0,
                    theme.control_radius(),
                    10,
                );
            }

            sugarloaf.rounded_rect(
                sx,
                swatch_y,
                PICKER_SWATCH_SIZE,
                PICKER_SWATCH_SIZE,
                *color,
                0.0,
                theme.control_radius(),
                10,
            );
        }

        // Reset swatch — neutral box with a diagonal slash, selected when no color is set
        let reset_x = content_x
            + PICKER_COLORS.len() as f32 * (PICKER_SWATCH_SIZE + PICKER_SWATCH_GAP);
        let reset_selected = selected_color.is_none();
        if reset_selected {
            let border = 2.0;
            sugarloaf.rounded_rect(
                reset_x - border,
                swatch_y - border,
                PICKER_SWATCH_SIZE + border * 2.0,
                PICKER_SWATCH_SIZE + border * 2.0,
                theme.accent,
                0.0,
                theme.control_radius(),
                10,
            );
        }
        sugarloaf.rounded_rect(
            reset_x,
            swatch_y,
            PICKER_SWATCH_SIZE,
            PICKER_SWATCH_SIZE,
            theme.hover,
            0.0,
            theme.control_radius(),
            10,
        );
        let slash_inset = 3.0;
        sugarloaf.line(
            reset_x + slash_inset,
            swatch_y + PICKER_SWATCH_SIZE - slash_inset,
            reset_x + PICKER_SWATCH_SIZE - slash_inset,
            swatch_y + slash_inset,
            1.5,
            0.0,
            theme.muted,
            10,
        );

        // Rename text input — same left/right edge as swatches
        let input_y = swatch_y + PICKER_SWATCH_SIZE + PICKER_INPUT_MARGIN_TOP;
        let input_x = content_x;
        let input_width = inner_width;

        // Input background
        sugarloaf.rounded_rect(
            input_x,
            input_y,
            input_width,
            PICKER_INPUT_HEIGHT,
            theme.hover,
            0.0,
            theme.control_radius(),
            10,
        );

        let text_inset = 6.0;
        let text_x = input_x + text_inset;
        let max_text_width = input_width - text_inset * 2.0;
        let text_y = input_y + (PICKER_INPUT_HEIGHT - PICKER_INPUT_FONT_SIZE) / 2.0;

        let text_color = if self.rename_input.is_empty() {
            theme.muted
        } else {
            theme.foreground
        };
        let rename_opts = DrawOpts {
            font_size: PICKER_INPUT_FONT_SIZE,
            color: color_u8(text_color),
            ..DrawOpts::default()
        };

        // Determine visible text: trim from the front if it overflows.
        let display_text: String = if self.rename_input.is_empty() {
            "Tab title...".to_string()
        } else {
            let input = self.rename_input.as_str();
            let chars: Vec<char> = input.chars().collect();
            let ui = sugarloaf.text_mut();
            let mut start = 0;
            let full_width = ui.measure(input, &rename_opts);
            if full_width > max_text_width {
                let mut lo = 0;
                let mut hi = chars.len();
                while lo < hi {
                    let mid = (lo + hi) / 2;
                    let substr: String = chars[mid..].iter().collect();
                    let w = ui.measure(&substr, &rename_opts);
                    if w > max_text_width {
                        lo = mid + 1;
                    } else {
                        hi = mid;
                    }
                }
                start = lo;
            }
            chars[start..].iter().collect()
        };

        let rendered_width =
            sugarloaf
                .text_mut()
                .draw(text_x, text_y, &display_text, &rename_opts);
        let rendered_width = if self.rename_input.is_empty() {
            0.0
        } else {
            rendered_width
        };

        // Blinking caret
        let elapsed = self.rename_caret_time.elapsed().as_millis();
        let show_caret = (elapsed / 500).is_multiple_of(2);
        if show_caret {
            let caret_x = text_x + rendered_width;
            if caret_x <= input_x + input_width {
                sugarloaf.rect(
                    caret_x,
                    input_y + 4.0,
                    1.5,
                    PICKER_INPUT_HEIGHT - 8.0,
                    theme.foreground,
                    0.0,
                    10,
                );
            }
        }
    }

    /// Whether the color picker is currently open
    pub fn is_color_picker_open(&self) -> bool {
        self.color_picker_tab.is_some()
    }

    /// Get the title text for a specific tab index: the one displayed
    /// chain, shared with the native titlebar.
    fn get_title_for_tab(
        &self,
        context_manager: &ContextManager<EventProxy>,
        tab_index: usize,
    ) -> String {
        context_manager.displayed_title_for_tab(tab_index)
    }
}

#[inline]
fn color_u8(c: [f32; 4]) -> [u8; 4] {
    [
        (c[0].clamp(0.0, 1.0) * 255.0) as u8,
        (c[1].clamp(0.0, 1.0) * 255.0) as u8,
        (c[2].clamp(0.0, 1.0) * 255.0) as u8,
        (c[3].clamp(0.0, 1.0) * 255.0) as u8,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configure_updates_single_tab_visibility() {
        let mut island = test_island();
        let mut navigation = rio_backend::config::navigation::Navigation::default();
        for hidden in [true, false] {
            navigation.hide_if_single = hidden;
            island.configure(&navigation);
            assert_eq!(island.hide_if_single, hidden);
        }
    }

    #[test]
    fn island_geometry_invariants() {
        use rio_backend::config::navigation::Navigation;

        for style in [TabStyle::Pill, TabStyle::Square] {
            let geometry = style.geometry();
            assert!(geometry.margin.top + geometry.margin.bottom < geometry.height);
            assert!(
                geometry.close_button_size
                    <= geometry.content_top() - geometry.margin.top
            );
            let (_, y, _, height, _) = TabAppearance::from(style).rect(0.0, 180.0);
            let navigation = Navigation {
                tab_style: style,
                hide_if_single: false,
                ..Default::default()
            };
            assert_eq!(
                super::super::utils::padding_top_from_config(&navigation, 0.0, 2),
                y + height
            );
        }
    }

    #[test]
    fn island_rect_insets_slot_and_clamps_radius() {
        // Slot at x=100, width 180 → island inset by half the gap on
        // each side and the configured top and bottom insets.
        let appearance = TabAppearance::default();
        let geometry = appearance.geometry;
        let (x, y, w, h, radius) = appearance.rect(100.0, 180.0);
        assert_eq!(x, 100.0 + geometry.gap / 2.0);
        assert_eq!(y, geometry.margin.top);
        assert_eq!(w, 180.0 - geometry.gap);
        assert_eq!(h, geometry.content_top() - geometry.margin.top);
        assert_eq!(radius, geometry.radius);

        let (_, _, w, h, radius) = appearance.rect(0.0, 4.0);
        assert_eq!(w, 0.0);
        assert_eq!(radius, 0.0);
        assert!(radius <= h / 2.0);
    }

    #[test]
    fn tab_fills_adapt_to_background_luminance() {
        let dark = tab_fills([0.06, 0.05, 0.06, 1.0]);
        let light = tab_fills([0.98, 0.98, 0.97, 1.0]);
        assert_eq!(dark.active, [1.0, 1.0, 1.0, 0.11]);
        assert_eq!(light.active, [0.0, 0.0, 0.0, 0.08]);
    }

    #[test]
    fn test_island_initialization() {
        let inactive_color = [0.5, 0.5, 0.5, 1.0];
        let active_color = [0.9, 0.9, 0.9, 1.0];

        let island = Island::new(inactive_color, active_color, true, 240.0);

        assert_eq!(island.inactive_text_color, inactive_color);
        assert_eq!(island.active_text_color, active_color);
        assert!(island.hide_if_single);
    }

    #[test]
    fn test_island_height() {
        let island =
            Island::new([0.8, 0.8, 0.8, 1.0], [1.0, 1.0, 1.0, 1.0], false, 240.0);
        assert_eq!(island.height(), 54.0);
    }

    fn test_island() -> Island {
        Island::new([0.5, 0.5, 0.5, 1.0], [0.9, 0.9, 0.9, 1.0], false, 240.0)
    }

    #[test]
    fn legacy_progress_uses_the_same_tab_pill_as_native_status() {
        use rio_backend::event::{VoidListener, WindowId};
        use rio_backend::performer::handler::Processor;
        let mut cm =
            ContextManager::start_with_capacity(2, VoidListener {}, WindowId::from(0))
                .unwrap();
        let mut parser = Processor::default();
        parser.advance(&mut *cm.current().terminal.lock(), b"\x1b]9;4;1;35\x07");
        let mut island = test_island();
        island.sync_focused_program_status(&cm);
        let record = island
            .program_status
            .as_ref()
            .expect("progress in the pill");
        assert_eq!(record.state, ProgramState::Working);
        assert_eq!(record.progress, Some(35));
        assert_eq!(
            island.program_status_title("project"),
            "[Working 35%] project"
        );

        cm.add_context(true, 0);
        island.sync_focused_program_status(&cm);
        assert!(island.program_status.is_none());
        assert_eq!(island.status_for_tab(&cm, 0).unwrap().progress, Some(35));
        parser.advance(
            &mut *cm.current().terminal.lock(),
            b"\x1b]7501;state=blocked:kind=permission\x07",
        );
        island.sync_focused_program_status(&cm);
        assert_eq!(
            island.program_status.as_ref().unwrap().state,
            ProgramState::Blocked
        );
        assert_eq!(island.status_for_tab(&cm, 0).unwrap().progress, Some(35));

        cm.set_current(0);
        for (sequence, state, progress) in [
            (&b"\x1b]9;4;3\x07"[..], ProgramState::Working, None),
            (&b"\x1b]9;4;4;65\x07"[..], ProgramState::Blocked, Some(65)),
            (&b"\x1b]9;4;2;65\x07"[..], ProgramState::Error, Some(65)),
        ] {
            parser.advance(&mut *cm.current().terminal.lock(), sequence);
            island.sync_focused_program_status(&cm);
            let record = island.program_status.as_ref().unwrap();
            assert_eq!(record.state, state);
            assert_eq!(record.progress, progress);
            assert_eq!(
                island.status_rim(Some(record), 1.0).unwrap().progress,
                progress
            );
        }
        parser.advance(&mut *cm.current().terminal.lock(), b"\x1b]9;4;0\x07");
        island.sync_focused_program_status(&cm);
        assert!(island.program_status.is_none());
    }

    #[test]
    fn progress_from_a_later_command_is_not_suppressed_by_earlier_native_status() {
        use rio_backend::event::{VoidListener, WindowId};
        use rio_backend::performer::handler::Processor;
        let cm =
            ContextManager::start_with_capacity(1, VoidListener {}, WindowId::from(0))
                .unwrap();
        let mut parser = Processor::default();
        let mut island = test_island();
        parser.advance(
            &mut *cm.current().terminal.lock(),
            b"\x1b]7501;state=done:app=build\x07\x1b]9;4;1;20\x07",
        );
        island.sync_focused_program_status(&cm);
        assert_eq!(
            island.program_status.as_ref().unwrap().state,
            ProgramState::Done
        );

        parser.advance(
            &mut *cm.current().terminal.lock(),
            b"\x1b]133;A\x07\x1b]9;4;1;20\x07",
        );
        island.sync_focused_program_status(&cm);
        let record = island.program_status.as_ref().unwrap();
        assert_eq!(record.state, ProgramState::Working);
        assert_eq!(record.progress, Some(20));
        parser.advance(
            &mut *cm.current().terminal.lock(),
            b"\x1b]7501;state=blocked:kind=question\x07",
        );
        island.sync_focused_program_status(&cm);
        assert_eq!(
            island.program_status.as_ref().unwrap().state,
            ProgramState::Blocked
        );
    }

    #[test]
    fn program_status_keeps_tab_location_and_custom_name() {
        use rio_backend::event::{VoidListener, WindowId};
        use rio_backend::performer::handler::Processor;

        let mut cm =
            ContextManager::start_with_capacity(1, VoidListener {}, WindowId::from(0))
                .unwrap();
        cm.config.title.content = "{{ RELATIVE_PATH }}".into();
        let mut parser = Processor::default();
        parser.advance(
            &mut *cm.current().terminal.lock(),
            b"\x1b]7;file://remote.invalid/rio-project\x07\x1b]7501;state=working:progress=40:app=cargo\x07",
        );
        cm.update_titles(true);
        let mut island = test_island();
        island.sync_focused_program_status(&cm);
        assert_eq!(
            island.program_status_title(&cm.displayed_title_for_current_tab()),
            "[Working 40%] /rio-project · cargo"
        );

        cm.set_custom_title(0, Some("Production".into()));
        parser.advance(
            &mut *cm.current().terminal.lock(),
            b"\x1b]7501;state=blocked:kind=permission:app=cargo\x07",
        );
        island.sync_focused_program_status(&cm);
        assert_eq!(
            island.program_status_title(&cm.displayed_title_for_current_tab()),
            "[Needs permission] Production · cargo"
        );
        parser.advance(&mut *cm.current().terminal.lock(), b"\x1b]133;A\x07");
        island.sync_focused_program_status(&cm);
        assert_eq!(
            island.program_status_title(&cm.displayed_title_for_current_tab()),
            "Production"
        );
    }

    #[test]
    fn program_status_follows_focus_and_keeps_background_updates_separate() {
        use rio_backend::event::{VoidListener, WindowId};
        use rio_backend::performer::handler::Processor;
        let mut cm =
            ContextManager::start_with_capacity(3, VoidListener {}, WindowId::from(0))
                .unwrap();
        let mut parser = Processor::default();
        parser.advance(
            &mut *cm.current().terminal.lock(),
            b"\x1b]7501;state=working:progress=40:app=cargo\x07",
        );
        let mut island = test_island();
        island.sync_focused_program_status(&cm);
        assert_eq!(island.program_status.as_ref().unwrap().progress, Some(40));
        assert_eq!(
            island.program_status_title("shell"),
            "[Working 40%] shell · cargo"
        );

        cm.add_context(true, 0);
        parser.advance(
            &mut *cm.current().terminal.lock(),
            b"\x1b]7501;state=blocked:kind=permission:msg=QXBwcm92ZQ==\x07",
        );
        island.sync_focused_program_status(&cm);
        assert_eq!(
            island.program_status.as_ref().unwrap().state,
            ProgramState::Blocked
        );
        assert_eq!(
            island.program_status_title("shell"),
            "[Needs permission] shell: Approve"
        );
        cm.set_current(0);
        island.sync_focused_program_status(&cm);
        assert_eq!(island.program_status.as_ref().unwrap().progress, Some(40));

        // Update the background terminal while the first remains focused.
        cm.set_current(1);
        let background = cm.current().terminal.clone();
        cm.set_current(0);
        parser.advance(&mut *background.lock(), b"\x1b]7501;state=done\x07");
        island.sync_focused_program_status(&cm);
        assert_eq!(island.program_status.as_ref().unwrap().progress, Some(40));
        cm.set_current(1);
        island.sync_focused_program_status(&cm);
        assert_eq!(
            island.program_status.as_ref().unwrap().state,
            ProgramState::Done
        );

        cm.add_context(true, 0);
        island.sync_focused_program_status(&cm);
        assert!(island.program_status.is_none());
    }

    #[test]
    fn status_text_strips_direction_controls() {
        assert_eq!(status_display_text("a\u{202e}b\u{2066}c\u{200b}d"), "abcd");
    }

    #[test]
    fn program_status_background_tab_keeps_its_own_status() {
        use rio_backend::event::{VoidListener, WindowId};
        use rio_backend::performer::handler::Processor;
        let mut cm =
            ContextManager::start_with_capacity(3, VoidListener {}, WindowId::from(0))
                .unwrap();
        let mut parser = Processor::default();
        parser.advance(
            &mut *cm.current().terminal.lock(),
            b"\x1b]7501;state=done:app=build\x07",
        );
        let mut island = test_island();
        island.sync_focused_program_status(&cm);
        cm.add_context(true, 0);
        parser.advance(
            &mut *cm.current().terminal.lock(),
            b"\x1b]7501;state=working:progress=20:app=test\x07",
        );
        island.sync_focused_program_status(&cm);
        let background = island.status_for_tab(&cm, 0);
        assert_eq!(
            background.as_ref().map(|r| r.state),
            Some(ProgramState::Done)
        );
        assert_eq!(
            Island::status_title(background.as_ref(), "shell"),
            "[Done] shell · build"
        );
        assert_eq!(
            background.as_ref().and_then(|r| island.status_color(r)),
            Some(island.status_colors[0])
        );
        assert_eq!(island.program_status.as_ref().unwrap().progress, Some(20));
        let background_route = cm.contexts_mut()[0].current().route_id;
        assert!(cm.is_route_focused_in_tab(background_route));
        assert!(!cm.is_route_focused_in_tab(usize::MAX));

        // Reports that arrive while the tab is in the background must be
        // visible immediately, without selecting it to refresh its cache.
        let background_terminal = cm.contexts_mut()[0].current().terminal.clone();
        parser.advance(
            &mut *background_terminal.lock(),
            b"\x1b]7501;state=blocked:kind=question:app=build\x07",
        );
        let background = island.status_for_tab(&cm, 0);
        assert_eq!(
            background.as_ref().map(|r| r.state),
            Some(ProgramState::Blocked)
        );
        assert_eq!(
            background.as_ref().and_then(|r| island.status_color(r)),
            Some(island.status_colors[1])
        );
        assert_eq!(island.program_status.as_ref().unwrap().progress, Some(20));
        parser.advance(
            &mut *background_terminal.lock(),
            b"\x1b]7501;state=clear\x07",
        );
        assert!(island.status_for_tab(&cm, 0).is_none());
    }

    #[test]
    fn status_rim_uses_theme_colors_and_progress_stays_in_the_pill() {
        use rio_backend::event::{VoidListener, WindowId};
        use rio_backend::performer::handler::Processor;
        let cm =
            ContextManager::start_with_capacity(3, VoidListener {}, WindowId::from(0))
                .unwrap();
        let mut parser = Processor::default();
        parser.advance(
            &mut *cm.current().terminal.lock(),
            b"\x1b]7501;state=done\x07",
        );
        let mut island = test_island();
        island.sync_focused_program_status(&cm);
        assert!(island
            .program_status
            .as_ref()
            .and_then(|r| island.status_color(r))
            .is_some());
        let colors = Colors {
            green: [0.1, 0.8, 0.4, 1.0],
            yellow: [0.9, 0.8, 0.2, 1.0],
            red: [0.9, 0.2, 0.3, 1.0],
            ..Colors::default()
        };
        island.update_colors(&colors);
        let rim = island
            .status_rim(island.program_status.as_ref(), 1.0)
            .unwrap();
        assert_eq!(rim.color, colors.green);
        assert_eq!(rim.progress, None);

        parser.advance(
            &mut *cm.current().terminal.lock(),
            b"\x1b]7501;state=working\x07",
        );
        island.sync_focused_program_status(&cm);
        assert!(
            island
                .next_frame_in(1.0, std::time::Duration::ZERO)
                .is_none(),
            "an indeterminate native status must not schedule idle frames"
        );

        parser.advance(&mut *cm.current().terminal.lock(), b"\x1bc\x1b]9;4;3\x07");
        island.sync_focused_program_status(&cm);
        assert_eq!(
            island.program_status.as_ref().unwrap().state,
            ProgramState::Working
        );
        assert!(island
            .next_frame_in(1.0, std::time::Duration::ZERO)
            .is_none());
    }

    #[test]
    fn tab_strip_layout_geometry() {
        let layout = TabAppearance::default().layout(1000.0, 2.0, 4, 240.0);
        let available =
            500.0 - layout.left_margin - TabStyle::Pill.geometry().margin.right
                + TabStyle::Pill.geometry().gap / 2.0
                - crate::renderer::window_chrome::controls_width();
        assert_eq!(layout.tab_width, available / 4.0);
        assert_eq!(layout.tabs_width, available);
        assert!(TabAppearance::default()
            .layout(1000.0, 2.0, 0, 240.0)
            .tab_width
            .is_finite());
    }

    #[test]
    fn tab_strip_layout_caps_slot_width() {
        let layout = TabAppearance::default().layout(3000.0, 2.0, 2, 240.0);
        assert_eq!(layout.tab_width, 240.0);
        assert_eq!(layout.tabs_width, 480.0);

        // The cap is configurable via navigation.max-tab-width.
        let layout = TabAppearance::default().layout(3000.0, 2.0, 2, 280.0);
        assert_eq!(layout.tab_width, 280.0);
        assert_eq!(layout.tabs_width, 560.0);
        // The tabs region ends well before the 1500 logical px strip.
        assert!(layout.left_margin + layout.tabs_width < 1500.0);

        // Pathologically narrow window: width clamps at 0 instead of
        // going negative.
        let layout = TabAppearance::default().layout(10.0, 2.0, 4, 240.0);
        assert_eq!(layout.tab_width, 0.0);
        assert_eq!(layout.tabs_width, 0.0);
    }

    #[test]
    fn remap_tab_move_forward_rotates_indices() {
        // Move tab 1 → 3: tabs 2 and 3 shift left by one.
        assert_eq!(Island::remap_index(1, 1, 3), 3);
        assert_eq!(Island::remap_index(2, 1, 3), 1);
        assert_eq!(Island::remap_index(3, 1, 3), 2);
        assert_eq!(Island::remap_index(0, 1, 3), 0);
        assert_eq!(Island::remap_index(4, 1, 3), 4);
    }

    #[test]
    fn remap_tab_move_backward_rotates_indices() {
        // Move tab 3 → 0: tabs 0, 1, 2 shift right by one.
        assert_eq!(Island::remap_index(3, 3, 0), 0);
        assert_eq!(Island::remap_index(0, 3, 0), 1);
        assert_eq!(Island::remap_index(1, 3, 0), 2);
        assert_eq!(Island::remap_index(2, 3, 0), 3);
        assert_eq!(Island::remap_index(4, 3, 0), 4);
    }

    #[test]
    fn drag_threshold_gates_start() {
        let mut island = test_island();
        island.start_drag(0, 10.0, 50.0);
        assert!(island.is_dragging());
        assert_eq!(island.drag_index(), None, "not started below threshold");
        assert!(!island.update_drag(52.0));
        assert!(island.update_drag(58.0), "8px exceeds threshold");
        assert_eq!(island.drag_index(), Some(0));
        island.cancel_drag();
        assert!(!island.is_dragging());
    }

    fn test_layout() -> TabStripLayout {
        TabStripLayout {
            left_margin: 0.0,
            tab_width: 100.0,
            tabs_width: 400.0,
            widths: Vec::new(),
            positions: Vec::new(),
            topmost: None,
        }
    }

    #[test]
    fn close_button_anchors_to_island_right_edge() {
        // Full-width slot: slot 1 spans 180..360, island 183..354, so
        // the button center stays inside the shape's right inset.
        let layout = TabStripLayout {
            left_margin: 0.0,
            tab_width: 180.0,
            tabs_width: 360.0,
            widths: Vec::new(),
            positions: Vec::new(),
            topmost: None,
        };
        let appearance = TabAppearance::default();
        let geometry = appearance.geometry;
        let cx = appearance.close_button_center_x(&layout, 1).unwrap();
        assert_eq!(
            cx,
            180.0 + geometry.gap / 2.0 + (180.0 - geometry.gap)
                - geometry.close_button_inset
                - geometry.close_button_size / 2.0
        );
        // The whole forgiving hit box stays inside the island.
        assert!(cx + geometry.close_button_size / 2.0 <= 360.0 - geometry.gap / 2.0);

        // Narrow islands (many tabs) drop the button — no hit box, so
        // rendering and click handling agree via the shared helper.
        let narrow = TabStripLayout {
            left_margin: 0.0,
            tab_width: 60.0,
            tabs_width: 600.0,
            widths: Vec::new(),
            positions: Vec::new(),
            topmost: None,
        };
        for mode in [CloseButton::Hover, CloseButton::Always] {
            let mut appearance = TabAppearance::default();
            appearance.geometry.close_button = mode;
            assert_eq!(appearance.close_button_center_x(&narrow, 3), None);
        }
    }

    #[test]
    fn drag_center_clamps_to_strip() {
        let mut island = test_island();
        // Tab 0 grabbed 10px from its left edge, tabs region spans
        // 0..400 with 100-wide slots.
        island.start_drag(0, 10.0, 50.0);
        island.update_drag(200.0); // started
        let center = island.drag_center(&test_layout()).unwrap();
        assert_eq!(center, 190.0 + 50.0);

        // Dragged far right: floating left clamps to 300, center 350.
        island.update_drag(1000.0);
        assert_eq!(island.drag_center(&test_layout()), Some(350.0));

        // Far left: clamps to 0, center 50.
        island.update_drag(-500.0);
        assert_eq!(island.drag_center(&test_layout()), Some(50.0));
    }

    #[test]
    fn custom_geometry_shares_paint_input_and_terminal_bounds() {
        use rio_backend::config::layout::Margin;
        use rio_backend::config::navigation::{Navigation, Tabs};
        let navigation = Navigation {
            tab_style: TabStyle::Square,
            tabs: Tabs {
                height: Some(82.0),
                margin: Some(Margin::new(12.0, 14.0, 10.0, 16.0)),
                padding: Some(Margin::new(2.0, 13.0, 4.0, 17.0)),
                gap: Some(8.0),
                radius: Some(16.0),
                close_button_size: Some(28.0),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut island = test_island();
        island.configure(&navigation);
        let appearance = island.tabs.appearance;
        let layout = appearance.layout(1000.0, 1.0, 2, 240.0);
        let (x, y, width, height, radius) =
            appearance.rect(layout.left(0), layout.width(0));
        assert_eq!((y, height, radius), (12.0, 60.0, 16.0));
        assert_eq!(island.content_top(), y + height);
        assert_eq!(
            super::super::utils::padding_top_from_config(&navigation, 4.0, 2),
            y + height + 4.0
        );
        assert_eq!(island.tab_at(&layout, x + width / 2.0, y + 1.0), Some(0));
        assert_eq!(island.tab_at(&layout, x + 1.0, y + 1.0), None);
        assert_eq!(island.tab_at(&layout, x - 1.0, y + 1.0), None);
        assert_eq!(island.tab_at(&layout, x + width / 2.0, y - 1.0), None);
        let close = appearance.close_button_center_x(&layout, 0).unwrap();
        assert!(island.close_button_hit(&layout, 0, close, appearance.center_y()));
        assert!(!island.close_button_hit(
            &layout,
            0,
            close,
            appearance.center_y() + 15.0
        ));
        assert!(!island.close_button_hit(
            &layout,
            0,
            close + 15.0,
            appearance.center_y()
        ));
        let mut hidden = navigation;
        hidden.tabs.close_button = Some(CloseButton::Never);
        island.configure(&hidden);
        assert!(!island.shows_close_button(2));
        assert!(!island.close_button_hit(&layout, 0, close, appearance.center_y()));

        hidden.tabs.margin = Some(Margin::all(0.0));
        hidden.tabs.gap = Some(10.0);
        hidden.tabs.radius = Some(0.0);
        island.configure(&hidden);
        let edge_layout = island.tabs.appearance.layout(600.0, 1.0, 4, 240.0);
        let (edge, _, _, _, _) = island
            .tabs
            .appearance
            .rect(edge_layout.left(0), edge_layout.width(0));
        #[cfg(not(target_os = "macos"))]
        assert_eq!(edge, 0.0);
        assert_eq!(island.tab_at(&edge_layout, edge, 1.0), Some(0));
    }
}
