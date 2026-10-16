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
use rio_backend::event::{EventProxy, ProgressReport, ProgressState};
use rio_backend::program_status::{
    BlockedKind, ProgramState, ProgramStatusRecord, ProgramStatusStore,
};
use rio_backend::sugarloaf::text::DrawOpts;
use rio_backend::sugarloaf::Sugarloaf;
use rustc_hash::FxHashMap;
use std::time::Instant;

pub const ISLAND_HEIGHT: f32 = crate::renderer::window_chrome::HEADER_HEIGHT;
const PROGRESS_BAR_HEIGHT: f32 = 3.0;

const PROGRESS_BAR_TIMEOUT_SECS: u64 = 15;
const TITLE_FONT_SIZE: f32 = 15.0;

const TAB_TITLE_PADDING_RIGHT: f32 = 14.0;
const TAB_ICON_INSET_X: f32 = 12.0;
const TAB_ICON_GAP: f32 = 8.0;
const TAB_GAP: f32 = 6.0;
const TAB_INSET_BOTTOM: f32 = 6.0;
const TAB_INSET_TOP: f32 = TAB_INSET_BOTTOM;
const TAB_CENTER_Y: f32 = (TAB_INSET_TOP + ISLAND_HEIGHT - TAB_INSET_BOTTOM) / 2.0;
const TAB_RADIUS: f32 = 19.0;
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
const ISLAND_MARGIN_RIGHT: f32 = 8.0;

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

const CLOSE_MARGIN_RIGHT: f32 = 22.0;
const CLOSE_GLYPH_HALF: f32 = 3.5;
const CLOSE_MIN_ISLAND_WIDTH: f32 = 64.0;
const CLOSE_HOVER_HALF: f32 = 10.0;
const CLOSE_HOVER_CORNER_RADIUS: f32 = 5.0;
const CLOSE_HIT_HALF_WIDTH: f32 = 10.0;
const CLOSE_ALPHA_IDLE: f32 = 0.55;
const CLOSE_ALPHA_HOVER: f32 = 0.95;
const CLOSE_STROKE_WIDTH: f32 = 1.2;
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

fn tab_title_padding(remote: bool, icon_size: f32) -> f32 {
    let icons = if remote { 2.0 } else { 1.0 };
    TAB_GAP / 2.0 + TAB_ICON_INSET_X + icons * (icon_size + TAB_ICON_GAP)
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

/// Compute the tab strip layout from the physical window width.
/// `max_tab_width` comes from `navigation.max-tab-width` (logical px).
pub fn tab_strip_layout(
    window_width: f32,
    scale_factor: f32,
    num_tabs: usize,
    max_tab_width: f32,
) -> TabStripLayout {
    #[cfg(target_os = "macos")]
    let left_margin = ISLAND_MARGIN_LEFT_MACOS;
    #[cfg(not(target_os = "macos"))]
    let left_margin = 6.0;

    let right_margin =
        ISLAND_MARGIN_RIGHT + crate::renderer::window_chrome::controls_width();
    let available_width =
        ((window_width / scale_factor) - right_margin - left_margin).max(0.0);
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

struct IslandFills {
    active: [f32; 4],
    close_hover: [f32; 4],
}

fn tab_fills(bg: [f32; 4]) -> IslandFills {
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
    x: f32,
    color: [f32; 4],
    order: u8,
    icon: super::app_icons::Icon,
    font_id: Option<usize>,
    size: f32,
) {
    let cy = TAB_CENTER_Y;
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
        sugarloaf.rounded_rect(None, x, y, w, h, fill, 0.05, radius, order);
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

#[inline]
fn island_rect(slot_x: f32, tab_width: f32) -> (f32, f32, f32, f32, f32) {
    let x = slot_x + TAB_GAP / 2.0;
    let w = (tab_width - TAB_GAP).max(0.0);
    let y = TAB_INSET_TOP;
    let h = ISLAND_HEIGHT - TAB_INSET_TOP - TAB_INSET_BOTTOM;
    let radius = TAB_RADIUS.min(w / 2.0).min(h / 2.0);
    (x, y, w, h, radius)
}

#[inline]
fn close_button_center(island_x: f32, island_w: f32) -> Option<f32> {
    (island_w >= CLOSE_MIN_ISLAND_WIDTH)
        .then_some(island_x + island_w - CLOSE_MARGIN_RIGHT)
}

#[inline]
fn close_button_center_x(layout: &TabStripLayout, tab_index: usize) -> Option<f32> {
    let slot_x = layout.left(tab_index);
    let (ix, _, iw, _, _) = island_rect(slot_x, layout.width(tab_index));
    close_button_center(ix, iw)
}

#[inline]
pub fn close_button_hit(
    layout: &TabStripLayout,
    tab_index: usize,
    x_unscaled: f32,
) -> bool {
    if layout.topmost == Some(tab_index) {
        return false;
    }
    close_button_center_x(layout, tab_index)
        .is_some_and(|cx| (x_unscaled - cx).abs() <= CLOSE_HIT_HALF_WIDTH)
}

fn draw_close_button(
    sugarloaf: &mut Sugarloaf,
    cx: f32,
    color: [f32; 4],
    hover: bool,
    order: u8,
) {
    let cy = TAB_CENTER_Y;
    let r = CLOSE_GLYPH_HALF;
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
        CLOSE_STROKE_WIDTH,
        0.0,
        color,
        order,
    );
    sugarloaf.line(
        cx - r,
        cy + r,
        cx + r,
        cy - r,
        CLOSE_STROKE_WIDTH,
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
    /// Current progress bar state
    progress_state: Option<ProgressState>,
    /// Current progress value (0-100)
    progress_value: Option<u8>,
    /// When the *current* state began. Reset only when transitioning into a
    /// new state, so the indeterminate animation phase is not yanked back to
    /// zero by repeated identical OSC 9;4 reports (issue #1509).
    progress_started_at: Option<Instant>,
    /// Last time we saw an OSC 9;4 report — bumped on every report, used by
    /// the stale-bar dismissal timer. Decoupled from `progress_started_at`
    /// for the same reason.
    progress_last_seen: Option<Instant>,
    program_status: Option<ProgramStatusRecord>,
    program_status_source: Option<(usize, u64)>,
    status_colors: [[f32; 4]; 3],
    /// Progress bar color
    pub progress_bar_color: [f32; 4],
    /// Progress bar error color
    pub progress_bar_error_color: [f32; 4],
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
    hover_x: Option<f32>,
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
            progress_state: None,
            progress_value: None,
            progress_started_at: None,
            progress_last_seen: None,
            program_status: None,
            program_status_source: None,
            status_colors: [colors.green, colors.yellow, colors.red],
            progress_bar_color: colors.blue,
            progress_bar_error_color: colors.red,
            color_picker_tab: None,
            rename_input: String::new(),
            rename_caret_time: Instant::now(),
            drag: None,
            last_anim_frame: Instant::now(),
            hover_x: None,
        }
    }

    pub fn configure(
        &mut self,
        navigation: &rio_backend::config::navigation::Navigation,
    ) {
        self.animation = navigation.animation;
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
        {
            self.app_icons.configure(&navigation.app_icons);
            self.tabs.invalidate_measurements();
            self.tab_glass_intensity =
                rio_backend::config::navigation::clamp_tab_glass_intensity(
                    navigation.tab_glass_intensity,
                );
        }
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
        if width < tab_title_padding(tab.remote, self.tab_icon_size) - TAB_GAP / 2.0 {
            return;
        }
        let mut x = x + TAB_ICON_INSET_X;
        if tab.remote {
            draw_tab_icon(
                sugarloaf,
                x,
                color,
                order,
                super::app_icons::SSH_ICON,
                self.window_header.app_font,
                self.tab_icon_size,
            );
            x += self.tab_icon_size + TAB_ICON_GAP;
        }
        draw_tab_icon(
            sugarloaf,
            x,
            color,
            order,
            tab.icon,
            self.window_header.app_font,
            self.tab_icon_size,
        );
    }

    /// Set whether the cursor hovers the active island's close button.
    /// Returns true when the state changed (the caller redraws).

    pub fn set_tab_hover(&mut self, x: Option<f32>) -> bool {
        let changed =
            self.tabs.pointer_target(self.hover_x) != self.tabs.pointer_target(x);
        self.hover_x = x;
        changed
    }

    pub fn update_colors(&mut self, colors: &Colors) {
        self.window_header.theme = super::window_chrome::Theme::new(colors);
        self.inactive_text_color = colors.tabs;
        self.active_text_color = colors.tabs_active;
        self.status_colors = [colors.green, colors.yellow, colors.red];
        self.progress_bar_color = colors.blue;
        self.progress_bar_error_color = colors.red;
    }

    /// Update the progress bar state from an OSC 9;4 report.
    ///
    /// `progress_last_seen` is bumped on every (non-Remove) report so the
    /// stale-bar dismissal timer keeps the bar alive while the TUI is
    /// actively reporting. `progress_started_at` is reset only when the
    /// state actually transitions, so a TUI sending the same `OSC 9;4;3`
    /// every 100 ms (issue #1509) doesn't yank the indeterminate animation
    /// phase back to zero on every report.
    pub fn set_progress_report(&mut self, report: ProgressReport) {
        match report.state {
            ProgressState::Remove => {
                self.progress_state = None;
                self.progress_value = None;
                self.progress_started_at = None;
                self.progress_last_seen = None;
            }
            new_state => {
                let now = Instant::now();
                self.progress_last_seen = Some(now);

                let transitioning = self.progress_state != Some(new_state);
                self.progress_state = Some(new_state);
                self.progress_value = report.progress;
                if transitioning {
                    self.progress_started_at = Some(now);
                }
            }
        }
    }

    /// Pull only the focused terminal, and only when its records change.
    /// A focus change also clears an old pane's animation and timeout.
    fn sync_focused_program_status<
        T: rio_backend::event::EventListener + Clone + Send + 'static,
    >(
        &mut self,
        context_manager: &ContextManager<T>,
    ) {
        let context = context_manager.current();
        let terminal = context.terminal.lock();
        self.sync_program_status(context.route_id, &terminal.program_status);
    }

    fn sync_program_status(&mut self, route_id: usize, store: &ProgramStatusStore) {
        let source = (route_id, store.revision());
        if self.program_status_source == Some(source) {
            return;
        }
        if self.program_status_source.map(|s| s.0) != Some(route_id) {
            self.set_progress_report(ProgressReport {
                state: ProgressState::Remove,
                progress: None,
            });
        }
        self.program_status_source = Some(source);
        self.program_status = store.current().cloned();
        if let Some(record) = &mut self.program_status {
            record.app = store.app_for(&record.id).map(str::to_owned);
        }
        // OSC 7501 belongs to the pill. Only legacy OSC 9;4 drives the
        // window bar and its indeterminate animation.
        let report =
            store
                .legacy_progress()
                .map(|(report, _)| report)
                .unwrap_or(ProgressReport {
                    state: ProgressState::Remove,
                    progress: None,
                });
        self.set_progress_report(report);
        if let Some((_, updated_at)) = store.legacy_progress() {
            // Returning to a pane must not revive its expired OSC 9;4 bar.
            self.progress_last_seen = Some(updated_at);
        }
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
            return context_manager.program_status_for_tab(tab_index);
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
        let mut title = format!("[{state}{percent}] {}", status_display_text(label));
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
        if matches!(self.progress_state, Some(ProgressState::Indeterminate)) {
            Some(std::time::Duration::ZERO)
        } else {
            self.tabs.next_frame_in(scale, self.hover_x).map(|delay| {
                if self.is_dragging() {
                    std::time::Duration::ZERO
                } else if self.animation
                    == rio_backend::config::navigation::Animation::Medium
                {
                    delay.max(frame_interval * 2)
                } else {
                    delay
                }
            })
        }
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

    /// Check if the progress bar should be auto-dismissed due to timeout.
    /// Uses `progress_last_seen` (heartbeat), not `progress_started_at`, so
    /// a long-running TUI that keeps reporting stays visible.
    fn check_progress_timeout(&mut self) {
        if let Some(last_seen) = self.progress_last_seen {
            if last_seen.elapsed().as_secs() >= PROGRESS_BAR_TIMEOUT_SECS {
                self.progress_state = None;
                self.progress_value = None;
                self.progress_started_at = None;
                self.progress_last_seen = None;
            }
        }
    }

    /// Render the progress bar below the tab strip, or at the top when hidden.
    fn render_progress_bar(
        &mut self,
        sugarloaf: &mut Sugarloaf,
        window_width: f32,
        scale_factor: f32,
        y_position: f32,
    ) {
        // Check for timeout first
        self.check_progress_timeout();

        let state = match self.progress_state {
            Some(s) => s,
            None => return, // No progress bar to render
        };

        let width = window_width / scale_factor;

        // Determine color based on state
        let color = match state {
            ProgressState::Error => self.progress_bar_error_color,
            _ => self.progress_bar_color,
        };

        match state {
            ProgressState::Remove => {
                // Should not reach here, but just in case
            }
            ProgressState::Set | ProgressState::Error | ProgressState::Pause => {
                // Render progress bar with specific percentage
                let progress = self.progress_value.unwrap_or(0) as f32 / 100.0;
                let bar_width = width * progress;

                if bar_width > 0.0 {
                    sugarloaf.rect(
                        None,
                        0.0,
                        y_position,
                        bar_width,
                        PROGRESS_BAR_HEIGHT,
                        color,
                        0.0, // Same depth as other rects
                        0,
                    );
                }
            }
            ProgressState::Indeterminate => {
                // For indeterminate, show a pulsing/moving indicator.
                // Phase is anchored to `progress_started_at` (set only on
                // state transition) — using `progress_last_seen` here would
                // freeze the bar at position 0 for any TUI that heartbeats
                // its OSC 9;4;3 faster than `cycle_ms`. (Issue #1509.)
                let elapsed = self
                    .progress_started_at
                    .map(|t| t.elapsed().as_millis() as f32)
                    .unwrap_or(0.0);

                // Move the bar from left to right over 2 seconds, then repeat
                let cycle_ms = 2000.0;
                let position = (elapsed % cycle_ms) / cycle_ms;
                let bar_fraction = 0.2; // 20% of width
                let bar_width = width * bar_fraction;
                let x_pos = position * (width - bar_width);

                sugarloaf.rect(
                    None,
                    x_pos,
                    y_position,
                    bar_width,
                    PROGRESS_BAR_HEIGHT,
                    color,
                    0.0,
                    0,
                );
            }
        }
    }

    /// Get the height of the island
    #[inline]
    pub fn height(&self) -> f32 {
        ISLAND_HEIGHT
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
        let picker_y = ISLAND_HEIGHT;

        // Check if click is within picker vertical range
        if mouse_y_unscaled < picker_y || mouse_y_unscaled > picker_y + PICKER_HEIGHT {
            // Click outside picker — apply rename and close
            self.apply_rename(context_manager);
            self.color_picker_tab = None;
            return false;
        }

        // Total picker width — N color swatches + 1 reset swatch
        let slot_count = PICKER_COLORS.len() + 1;
        let total_swatches_width = slot_count as f32 * PICKER_SWATCH_SIZE
            + (slot_count - 1) as f32 * PICKER_SWATCH_GAP;
        let picker_start_x = tab_x + (tab_width - total_swatches_width) / 2.0;

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
    fn render_color_picker(
        &mut self,
        sugarloaf: &mut Sugarloaf,
        tab_x: f32,
        tab_width: f32,
        selected_color: Option<[f32; 4]>,
    ) {
        let padding = PICKER_PADDING;
        let bg_y = ISLAND_HEIGHT;

        // Compute total swatches width to derive the consistent inner content width
        // N color swatches + 1 reset swatch
        let slot_count = PICKER_COLORS.len() + 1;
        let total_swatches_width = slot_count as f32 * PICKER_SWATCH_SIZE
            + (slot_count - 1) as f32 * PICKER_SWATCH_GAP;
        let inner_width = total_swatches_width;
        let bg_width = inner_width + padding * 2.0;
        let bg_x = tab_x + (tab_width - bg_width) / 2.0;
        let content_x = bg_x + padding;

        // Background
        sugarloaf.rounded_rect(
            None,
            bg_x,
            bg_y,
            bg_width,
            PICKER_HEIGHT,
            [0.15, 0.15, 0.15, 1.0],
            0.0,
            4.0,
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
                    None,
                    sx - border,
                    swatch_y - border,
                    PICKER_SWATCH_SIZE + border * 2.0,
                    PICKER_SWATCH_SIZE + border * 2.0,
                    [1.0, 1.0, 1.0, 1.0],
                    0.0,
                    4.0,
                    10,
                );
            }

            sugarloaf.rounded_rect(
                None,
                sx,
                swatch_y,
                PICKER_SWATCH_SIZE,
                PICKER_SWATCH_SIZE,
                *color,
                0.0,
                3.0,
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
                None,
                reset_x - border,
                swatch_y - border,
                PICKER_SWATCH_SIZE + border * 2.0,
                PICKER_SWATCH_SIZE + border * 2.0,
                [1.0, 1.0, 1.0, 1.0],
                0.0,
                4.0,
                10,
            );
        }
        sugarloaf.rounded_rect(
            None,
            reset_x,
            swatch_y,
            PICKER_SWATCH_SIZE,
            PICKER_SWATCH_SIZE,
            [0.22, 0.22, 0.22, 1.0],
            0.0,
            3.0,
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
            [0.86, 0.26, 0.27, 1.0],
            10,
        );

        // Rename text input — same left/right edge as swatches
        let input_y = swatch_y + PICKER_SWATCH_SIZE + PICKER_INPUT_MARGIN_TOP;
        let input_x = content_x;
        let input_width = inner_width;

        // Input background
        sugarloaf.rounded_rect(
            None,
            input_x,
            input_y,
            input_width,
            PICKER_INPUT_HEIGHT,
            [0.10, 0.10, 0.10, 1.0],
            0.0,
            3.0,
            10,
        );

        let text_inset = 6.0;
        let text_x = input_x + text_inset;
        let max_text_width = input_width - text_inset * 2.0;
        let text_y = input_y + (PICKER_INPUT_HEIGHT - PICKER_INPUT_FONT_SIZE) / 2.0;

        let text_color = if self.rename_input.is_empty() {
            [0.45, 0.45, 0.45, 1.0]
        } else {
            [0.93, 0.93, 0.93, 1.0]
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
                    None,
                    caret_x,
                    input_y + 4.0,
                    1.5,
                    PICKER_INPUT_HEIGHT - 8.0,
                    [0.93, 0.93, 0.93, 1.0],
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
    fn island_geometry_invariants() {
        const {
            assert!(TAB_INSET_TOP + TAB_INSET_BOTTOM < ISLAND_HEIGHT);
            assert!(CLOSE_MARGIN_RIGHT + CLOSE_HIT_HALF_WIDTH < CLOSE_MIN_ISLAND_WIDTH);
            assert!(
                CLOSE_HOVER_HALF * 2.0
                    <= ISLAND_HEIGHT - TAB_INSET_TOP - TAB_INSET_BOTTOM
            );
        }
    }

    #[test]
    fn island_rect_insets_slot_and_clamps_radius() {
        // Slot at x=100, width 180 → island inset by half the gap on
        // each side and the configured top and bottom insets.
        let (x, y, w, h, radius) = island_rect(100.0, 180.0);
        assert_eq!(x, 100.0 + TAB_GAP / 2.0);
        assert_eq!(y, TAB_INSET_TOP);
        assert_eq!(w, 180.0 - TAB_GAP);
        assert_eq!(h, ISLAND_HEIGHT - TAB_INSET_TOP - TAB_INSET_BOTTOM);
        assert_eq!(radius, TAB_RADIUS);

        let (_, _, w, h, radius) = island_rect(0.0, 4.0);
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
        assert_eq!(island.height(), ISLAND_HEIGHT);
    }

    fn test_island() -> Island {
        Island::new([0.5, 0.5, 0.5, 1.0], [0.9, 0.9, 0.9, 1.0], false, 240.0)
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
        assert!(island.progress_state.is_none());
        assert_eq!(island.program_status_title("shell"), "[Working 40%] cargo");

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
        assert!(island.progress_state.is_none());

        // Update the background terminal while the first remains focused.
        cm.set_current(1);
        let background = cm.current().terminal.clone();
        cm.set_current(0);
        parser.advance(&mut *background.lock(), b"\x1b]7501;state=done\x07");
        island.sync_focused_program_status(&cm);
        assert_eq!(island.program_status.as_ref().unwrap().progress, Some(40));
        assert!(island.progress_state.is_none());
        cm.set_current(1);
        island.sync_focused_program_status(&cm);
        assert_eq!(
            island.program_status.as_ref().unwrap().state,
            ProgramState::Done
        );

        cm.add_context(true, 0);
        island.sync_focused_program_status(&cm);
        assert!(island.program_status.is_none());
        assert!(island.progress_state.is_none());
    }

    #[test]
    fn program_status_has_no_heartbeat_timeout_and_strips_direction_controls() {
        use rio_backend::crosswords::{Crosswords, CrosswordsSize};
        use rio_backend::event::{VoidListener, WindowId};
        use rio_backend::performer::handler::Processor;
        let mut terminal = Crosswords::new(
            CrosswordsSize::new(10, 10),
            rio_backend::ansi::CursorShape::Block,
            VoidListener {},
            WindowId::from(0),
            1,
            10,
        );
        Processor::default().advance(&mut terminal, b"\x1b]7501;state=working\x07");
        let mut island = test_island();
        island.sync_program_status(1, &terminal.program_status);
        island.progress_last_seen =
            Some(Instant::now() - std::time::Duration::from_secs(60));
        island.check_progress_timeout();
        assert!(island.progress_state.is_none());
        island.sync_program_status(1, &terminal.program_status);
        assert_eq!(
            island.program_status.as_ref().unwrap().state,
            ProgramState::Working
        );
        assert!(island
            .next_frame_in(1.0, std::time::Duration::ZERO)
            .is_none());
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
            "[Done] build"
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
    fn program_status_rim_does_not_drive_legacy_window_bar() {
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
        assert!(island.progress_state.is_none());
        let mut colors = Colors::default();
        colors.green = [0.1, 0.8, 0.4, 1.0];
        colors.yellow = [0.9, 0.8, 0.2, 1.0];
        colors.red = [0.9, 0.2, 0.3, 1.0];
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
        assert!(island.program_status.is_none());
        assert!(
            island
                .next_frame_in(1.0, std::time::Duration::ZERO)
                .is_some(),
            "visible legacy indeterminate progress must animate"
        );
    }

    #[test]
    fn progress_first_report_seeds_started_and_seen() {
        let mut island = test_island();
        island.set_progress_report(ProgressReport {
            state: ProgressState::Indeterminate,
            progress: None,
        });
        assert!(island.progress_started_at.is_some());
        assert!(island.progress_last_seen.is_some());
        assert_eq!(island.progress_state, Some(ProgressState::Indeterminate));
    }

    #[test]
    fn progress_repeated_same_state_keeps_started_at_stable() {
        // Issue #1509: a TUI that heartbeats `OSC 9;4;3` (or any same-state
        // report) must NOT restart the indeterminate animation phase, or the
        // pulsing block snaps back to the left edge on every report.
        let mut island = test_island();
        island.set_progress_report(ProgressReport {
            state: ProgressState::Indeterminate,
            progress: None,
        });
        let first_started = island.progress_started_at.unwrap();
        let first_seen = island.progress_last_seen.unwrap();

        // Sleep so a subsequent Instant::now() is observably later — the
        // started_at field must stay equal while last_seen advances.
        std::thread::sleep(std::time::Duration::from_millis(15));
        island.set_progress_report(ProgressReport {
            state: ProgressState::Indeterminate,
            progress: None,
        });

        assert_eq!(
            island.progress_started_at,
            Some(first_started),
            "started_at must not move on a same-state heartbeat"
        );
        assert!(
            island.progress_last_seen.unwrap() > first_seen,
            "last_seen must advance on every report"
        );
    }

    #[test]
    fn progress_state_transition_resets_started_at() {
        // Set → Indeterminate is a real state change, so the animation
        // anchor should be reseated. (Set has no animation, but the
        // started_at field still becomes meaningful as soon as we hit
        // Indeterminate.)
        let mut island = test_island();
        island.set_progress_report(ProgressReport {
            state: ProgressState::Set,
            progress: Some(50),
        });
        let first = island.progress_started_at.unwrap();

        std::thread::sleep(std::time::Duration::from_millis(15));
        island.set_progress_report(ProgressReport {
            state: ProgressState::Indeterminate,
            progress: None,
        });

        assert!(
            island.progress_started_at.unwrap() > first,
            "transitioning into a new state must move started_at forward"
        );
        assert_eq!(island.progress_state, Some(ProgressState::Indeterminate));
    }

    #[test]
    fn progress_set_value_change_does_not_reseat_started_at() {
        // Same `Set` state with a different percentage is still the same
        // state — only the value updates. started_at stays put; the bar
        // just redraws at the new fraction.
        let mut island = test_island();
        island.set_progress_report(ProgressReport {
            state: ProgressState::Set,
            progress: Some(20),
        });
        let first = island.progress_started_at.unwrap();

        std::thread::sleep(std::time::Duration::from_millis(15));
        island.set_progress_report(ProgressReport {
            state: ProgressState::Set,
            progress: Some(60),
        });

        assert_eq!(island.progress_started_at, Some(first));
        assert_eq!(island.progress_value, Some(60));
    }

    #[test]
    fn tab_strip_layout_geometry() {
        let layout = tab_strip_layout(1000.0, 2.0, 4, 240.0);
        let available = 500.0
            - layout.left_margin
            - ISLAND_MARGIN_RIGHT
            - crate::renderer::window_chrome::controls_width();
        assert_eq!(layout.tab_width, available / 4.0);
        assert_eq!(layout.tabs_width, available);
        assert!(tab_strip_layout(1000.0, 2.0, 0, 240.0)
            .tab_width
            .is_finite());
    }

    #[test]
    fn tab_strip_layout_caps_slot_width() {
        let layout = tab_strip_layout(3000.0, 2.0, 2, 240.0);
        assert_eq!(layout.tab_width, 240.0);
        assert_eq!(layout.tabs_width, 480.0);

        // The cap is configurable via navigation.max-tab-width.
        let layout = tab_strip_layout(3000.0, 2.0, 2, 280.0);
        assert_eq!(layout.tab_width, 280.0);
        assert_eq!(layout.tabs_width, 560.0);
        // The tabs region ends well before the 1500 logical px strip.
        assert!(layout.left_margin + layout.tabs_width < 1500.0);

        // Pathologically narrow window: width clamps at 0 instead of
        // going negative.
        let layout = tab_strip_layout(10.0, 2.0, 4, 240.0);
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
        // the button centers at 354 - CLOSE_MARGIN_RIGHT.
        let layout = TabStripLayout {
            left_margin: 0.0,
            tab_width: 180.0,
            tabs_width: 360.0,
            widths: Vec::new(),
            positions: Vec::new(),
            topmost: None,
        };
        let cx = close_button_center_x(&layout, 1).unwrap();
        assert_eq!(
            cx,
            180.0 + TAB_GAP / 2.0 + (180.0 - TAB_GAP) - CLOSE_MARGIN_RIGHT
        );
        // The whole forgiving hit box stays inside the island.
        assert!(cx + CLOSE_HIT_HALF_WIDTH <= 360.0 - TAB_GAP / 2.0);

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
        assert_eq!(close_button_center_x(&narrow, 3), None);
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
    fn progress_remove_clears_all_progress_state() {
        let mut island = test_island();
        island.set_progress_report(ProgressReport {
            state: ProgressState::Set,
            progress: Some(50),
        });
        island.set_progress_report(ProgressReport {
            state: ProgressState::Remove,
            progress: None,
        });
        assert!(island.progress_state.is_none());
        assert!(island.progress_value.is_none());
        assert!(island.progress_started_at.is_none());
        assert!(island.progress_last_seen.is_none());
    }
}
