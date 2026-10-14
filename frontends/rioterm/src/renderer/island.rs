// Copyright (c) 2023-present, Raphael Amorim.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.
//
// island.rs was originally retired from boo editor
// which is licensed under MIT license.

use crate::context::ContextManager;
use crate::renderer::helpers::spring::Spring;
use rio_backend::event::{EventProxy, ProgressReport, ProgressState};
use rio_backend::program_status::{
    BlockedKind, ProgramState, ProgramStatusRecord, ProgramStatusStore,
};
use rio_backend::sugarloaf::text::DrawOpts;
#[cfg(not(target_os = "windows"))]
use rio_backend::sugarloaf::Attributes;
use rio_backend::sugarloaf::Sugarloaf;
use rustc_hash::FxHashMap;
use std::borrow::Cow;
use std::time::Instant;

#[cfg(not(target_os = "windows"))]
pub const ISLAND_HEIGHT: f32 = 38.0;
#[cfg(target_os = "windows")]
pub const ISLAND_HEIGHT: f32 = crate::renderer::windows_chrome::HEADER_HEIGHT;
const PROGRESS_BAR_HEIGHT: f32 = 3.0;

const PROGRESS_BAR_TIMEOUT_SECS: u64 = 15;
#[cfg(not(target_os = "windows"))]
const TITLE_FONT_SIZE: f32 = 12.0;
#[cfg(target_os = "windows")]
const TITLE_FONT_SIZE: f32 = 15.0;

#[cfg(not(target_os = "windows"))]
const TAB_PADDING_X: f32 = 27.0;
#[cfg(target_os = "windows")]
const TAB_PADDING_X: f32 = 47.0;
#[cfg(target_os = "windows")]
const TAB_TITLE_PADDING_RIGHT: f32 = 14.0;
#[cfg(target_os = "windows")]
const TAB_ICON_INSET_X: f32 = 12.0;
const TAB_GAP: f32 = 6.0;
#[cfg(not(target_os = "windows"))]
const TAB_INSET_Y: f32 = 7.0;
#[cfg(target_os = "windows")]
const TAB_INSET_Y: f32 = 8.0;
#[cfg(not(target_os = "windows"))]
const TAB_RADIUS: f32 = 6.0;
#[cfg(target_os = "windows")]
const TAB_RADIUS: f32 = 19.0;
const TITLE_ELLIPSIS: char = '…';
/// Bell dot for tabs that rang while in the background, and the gap
/// between it and the tab title. A dot, not a glyph: the UI text layer
/// resolves one font per run, so a bell glyph exists only where an
/// emoji font is installed and resolvable, while a dot renders
/// identically everywhere. Drawn at rect order 1: the tab backgrounds
/// are submitted at order 0 AFTER the title pass, so a same-order dot
/// would be painted over and invisible in every multi-tab strip.
const BELL_DOT_SIZE: f32 = 6.0;
const BELL_DOT_ORDER: u8 = 1;
const BELL_GAP: f32 = 4.0;
const DRAG_THRESHOLD: f32 = 4.0;
const DRAG_ANIMATION_LENGTH: f32 = 0.15;
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

#[cfg(not(target_os = "windows"))]
const CLOSE_MARGIN_RIGHT: f32 = 14.0;
const CLOSE_GLYPH_HALF: f32 = 3.5;
#[cfg(not(target_os = "windows"))]
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

#[cfg(target_os = "windows")]
struct TabPresentation {
    title: String,
    status: Option<ProgramStatusRecord>,
    icon: super::app_icons::Icon,
    remote: bool,
    bell: bool,
}

fn fit_title_to_width<'a>(
    sugarloaf: &mut Sugarloaf,
    title: &'a str,
    max_width: f32,
    #[allow(unused)] opts: &DrawOpts,
) -> Cow<'a, str> {
    #[cfg(feature = "profiling")]
    let _timing = crate::profiling::span("tabs.fit_title");
    #[cfg(target_os = "windows")]
    {
        let ui = sugarloaf.text_mut();
        if ui.measure(title, opts) <= max_width {
            return Cow::Borrowed(title);
        }
        let ellipsis = TITLE_ELLIPSIS.to_string();
        if ui.measure(&ellipsis, opts) > max_width {
            return Cow::Borrowed("");
        }
        let boundaries: Vec<usize> = title.char_indices().map(|(i, _)| i).collect();
        let mut low = 0;
        let mut high = boundaries.len();
        while low < high {
            let mid = (low + high) / 2;
            let candidate = format!("{}{}", &title[..boundaries[mid]], ellipsis);
            if ui.measure(&candidate, opts) <= max_width {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        let end = low.checked_sub(1).map_or(0, |i| boundaries[i]);
        return Cow::Owned(format!("{}{}", &title[..end], ellipsis));
    }
    #[cfg(not(target_os = "windows"))]
    {
        let attrs = Attributes::default();
        fit_title_with_widths(title, max_width, |c| {
            sugarloaf.char_advance(c, attrs, TITLE_FONT_SIZE)
        })
    }
}

#[cfg(any(test, not(target_os = "windows")))]
fn fit_title_with_widths<'a>(
    title: &'a str,
    max_width: f32,
    mut char_width: impl FnMut(char) -> f32,
) -> Cow<'a, str> {
    let suffix_width = char_width(TITLE_ELLIPSIS);

    // `truncate_ix` tracks the last byte offset at which the prefix so
    // far still has room for the suffix. Updated before adding the next
    // char's width so the moment we detect overflow we already know
    // where to cut.
    let mut accumulated: f32 = 0.0;
    let mut truncate_ix: usize = 0;
    for (ix, c) in title.char_indices() {
        if accumulated + suffix_width <= max_width {
            truncate_ix = ix;
        }
        accumulated += char_width(c);
        if accumulated > max_width {
            let mut out = String::with_capacity(truncate_ix + TITLE_ELLIPSIS.len_utf8());
            out.push_str(&title[..truncate_ix]);
            out.push(TITLE_ELLIPSIS);
            return Cow::Owned(out);
        }
    }
    Cow::Borrowed(title)
}

#[derive(Clone, PartialEq)]
pub struct TabStripLayout {
    pub left_margin: f32,
    pub tab_width: f32,
    pub tabs_width: f32,
    widths: Vec<f32>,
}

impl TabStripLayout {
    pub fn width(&self, index: usize) -> f32 {
        self.widths.get(index).copied().unwrap_or(self.tab_width)
    }

    pub fn left(&self, index: usize) -> f32 {
        self.left_margin
            + if self.widths.is_empty() {
                index as f32 * self.tab_width
            } else {
                self.widths.iter().take(index).sum::<f32>()
            }
    }

    pub fn tab_at(&self, x: f32) -> Option<usize> {
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
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let left_margin = 0.0;
    #[cfg(target_os = "windows")]
    let left_margin = 6.0;

    #[cfg(target_os = "windows")]
    let right_margin = ISLAND_MARGIN_RIGHT
        + crate::renderer::windows_chrome::controls_width(window_width / scale_factor)
        + 8.0;
    #[cfg(not(target_os = "windows"))]
    let right_margin = ISLAND_MARGIN_RIGHT;
    let available_width =
        ((window_width / scale_factor) - right_margin - left_margin).max(0.0);
    let tab_width =
        (available_width / num_tabs.max(1) as f32).clamp(0.0, max_tab_width.max(0.0));
    TabStripLayout {
        left_margin,
        tab_width,
        tabs_width: tab_width * num_tabs as f32,
        widths: Vec::new(),
    }
}

struct IslandFills {
    inactive: [f32; 4],
    active: [f32; 4],
    outline: Option<[f32; 4]>,
    close_hover: [f32; 4],
}

fn island_fills(bg: [f32; 4]) -> IslandFills {
    let luminance = 0.2126 * bg[0] + 0.7152 * bg[1] + 0.0722 * bg[2];
    if luminance > 0.5 {
        IslandFills {
            inactive: [0.0, 0.0, 0.0, 0.05],
            active: [1.0, 1.0, 1.0, 0.92],
            outline: Some([0.0, 0.0, 0.0, 0.14]),
            close_hover: [0.0, 0.0, 0.0, 0.09],
        }
    } else {
        IslandFills {
            inactive: [1.0, 1.0, 1.0, 0.05],
            active: [1.0, 1.0, 1.0, 0.18],
            outline: None,
            close_hover: [1.0, 1.0, 1.0, 0.14],
        }
    }
}

#[cfg(target_os = "windows")]
fn windows_island_fills(bg: [f32; 4]) -> IslandFills {
    let mut fills = island_fills(bg);
    fills.inactive[3] = 0.0;
    if 0.2126 * bg[0] + 0.7152 * bg[1] + 0.0722 * bg[2] > 0.5 {
        fills.outline = Some([0.0, 0.0, 0.0, 0.12]);
    } else {
        fills.active = [1.0, 1.0, 1.0, 0.11];
        fills.outline = Some([1.0, 1.0, 1.0, 0.16]);
    }
    fills
}

#[cfg(target_os = "windows")]
fn mix_color(from: [f32; 4], to: [f32; 4], amount: f32) -> [f32; 4] {
    std::array::from_fn(|i| from[i] + (to[i] - from[i]) * amount)
}

#[cfg(target_os = "windows")]
fn fade_toward(value: &mut f32, target: f32, dt: f32) {
    *value += (target - *value) * (1.0 - (-dt / 0.045).exp());
    if (*value - target).abs() < 0.005 {
        *value = target;
    }
}

#[cfg(target_os = "windows")]
fn draw_tab_icon(
    sugarloaf: &mut Sugarloaf,
    x: f32,
    color: [f32; 4],
    order: u8,
    icon: super::app_icons::Icon,
    font_id: Option<usize>,
    size: f32,
) {
    let cy = ISLAND_HEIGHT / 2.0;
    if super::app_icons::draw(
        sugarloaf,
        icon,
        x + size / 2.0,
        cy,
        size,
        color,
        font_id,
        order,
    ) {
        return;
    }
    if crate::renderer::windows_chrome::draw_icon(
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

#[inline]
fn over(dst: [f32; 4], src: [f32; 4]) -> [f32; 4] {
    let a = src[3];
    [
        src[0] * a + dst[0] * (1.0 - a),
        src[1] * a + dst[1] * (1.0 - a),
        src[2] * a + dst[2] * (1.0 - a),
        dst[3],
    ]
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
    outline: Option<[f32; 4]>,
    punch: Option<[f32; 4]>,
    order: u8,
) {
    #[cfg(target_os = "windows")]
    let (x, y, w, h) = {
        let scale = sugarloaf.scale_factor();
        let snap = |v| crate::renderer::windows_chrome::snap(v, scale);
        let left = snap(x);
        let top = snap(y);
        (left, top, snap(x + w) - left, snap(y + h) - top)
    };
    #[cfg(target_os = "windows")]
    let border = 1.0 / sugarloaf.scale_factor();
    #[cfg(not(target_os = "windows"))]
    let border = 1.0;
    match outline {
        Some(ring) => {
            sugarloaf.rounded_rect(None, x, y, w, h, ring, 0.05, radius, order);
            let fw = (w - border * 2.0).max(0.0);
            let fh = (h - border * 2.0).max(0.0);
            let fr = (radius - border).clamp(0.0, fw.min(fh) / 2.0);
            if let Some(bg) = punch {
                sugarloaf.rounded_rect(
                    None,
                    x + border,
                    y + border,
                    fw,
                    fh,
                    bg,
                    0.05,
                    fr,
                    order,
                );
            }
            sugarloaf.rounded_rect(
                None,
                x + border,
                y + border,
                fw,
                fh,
                fill,
                0.05,
                fr,
                order,
            );
        }
        None => sugarloaf.rounded_rect(None, x, y, w, h, fill, 0.05, radius, order),
    }
}

#[inline]
fn island_rect(slot_x: f32, tab_width: f32) -> (f32, f32, f32, f32, f32) {
    let x = slot_x + TAB_GAP / 2.0;
    let w = (tab_width - TAB_GAP).max(0.0);
    let y = TAB_INSET_Y;
    let h = ISLAND_HEIGHT - TAB_INSET_Y * 2.0;
    let radius = TAB_RADIUS.min(w / 2.0).min(h / 2.0);
    (x, y, w, h, radius)
}

/// How much width a lone tab's title may occupy, in logical pixels.
///
/// `window_width` is physical, as `render` receives it, while everything
/// drawn is logical, the same conversion `tab_strip_layout` makes.
#[inline]
fn single_title_budget(window_width: f32, scale_factor: f32, left_margin: f32) -> f32 {
    ((window_width / scale_factor)
        - left_margin
        - ISLAND_MARGIN_RIGHT
        - TAB_PADDING_X * 2.0)
        .max(0.0)
}

/// Where a lone tab's title starts: centred on the strip, but never far
/// enough left to sit under the traffic lights on macOS. Physical in,
/// logical out, as above.
#[inline]
#[cfg(any(test, not(target_os = "windows")))]
fn single_title_x(
    window_width: f32,
    scale_factor: f32,
    text_width: f32,
    left_margin: f32,
) -> f32 {
    (((window_width / scale_factor) - text_width) / 2.0).max(left_margin + TAB_PADDING_X)
}

#[inline]
fn close_button_center(island_x: f32, island_w: f32) -> Option<f32> {
    #[cfg(target_os = "windows")]
    {
        let _ = (island_x, island_w);
        None
    }
    #[cfg(not(target_os = "windows"))]
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
    let cy = ISLAND_HEIGHT / 2.0;
    let r = CLOSE_GLYPH_HALF;
    let alpha = if hover {
        CLOSE_ALPHA_HOVER
    } else {
        CLOSE_ALPHA_IDLE
    };
    let color = [color[0], color[1], color[2], color[3] * alpha];
    #[cfg(target_os = "windows")]
    if crate::renderer::windows_chrome::draw_icon(
        sugarloaf, "\u{e8bb}", cx, cy, 8.0, color,
    ) {
        return;
    }
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
    #[cfg(target_os = "windows")]
    pub window_header: crate::renderer::windows_chrome::Header,
    pub hide_if_single: bool,
    /// Cap on tab width in logical px (`navigation.max-tab-width`).
    pub max_tab_width: f32,
    pub min_tab_width: f32,
    pub tab_icon_size: f32,
    #[cfg(target_os = "windows")]
    preferred_tab_widths: Vec<f32>,
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
    progress_persistent: bool,
    window_progress_visible: bool,
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
    /// Per-tab x-offset springs: displaced tabs sliding into their slot
    /// and the released tab settling after a drag. Keyed by tab index.
    slide_springs: FxHashMap<usize, Spring>,
    /// Timestamp of the last spring advance, for per-frame dt.
    last_anim_frame: Instant,
    /// Cursor is over the active island's close button — draws the
    /// hover backdrop. Updated on every cursor move by `Screen`.
    close_hover: bool,
    #[cfg(target_os = "windows")]
    hovered_tab: Option<usize>,
    #[cfg(target_os = "windows")]
    selected_tab: usize,
    /// Selection and hover opacity, animated only while changing.
    #[cfg(target_os = "windows")]
    tab_appearance: FxHashMap<usize, (f32, f32)>,
    #[cfg(target_os = "windows")]
    close_hover_opacity: f32,
}

impl Island {
    pub fn new(
        inactive_text_color: [f32; 4],
        active_text_color: [f32; 4],
        hide_if_single: bool,
        max_tab_width: f32,
    ) -> Self {
        Self {
            #[cfg(target_os = "windows")]
            window_header: crate::renderer::windows_chrome::Header::new(),
            hide_if_single,
            max_tab_width,
            min_tab_width: rio_backend::config::navigation::default_min_tab_width(),
            tab_icon_size: rio_backend::config::navigation::default_tab_icon_size(),
            #[cfg(target_os = "windows")]
            preferred_tab_widths: Vec::new(),
            inactive_text_color,
            active_text_color,
            progress_state: None,
            progress_value: None,
            progress_started_at: None,
            progress_last_seen: None,
            program_status: None,
            program_status_source: None,
            progress_persistent: false,
            window_progress_visible: true,
            // Default progress bar color (blue-ish)
            progress_bar_color: [0.3, 0.6, 1.0, 1.0],
            // Default error color (red-ish)
            progress_bar_error_color: [1.0, 0.3, 0.3, 1.0],
            color_picker_tab: None,
            rename_input: String::new(),
            rename_caret_time: Instant::now(),
            drag: None,
            slide_springs: FxHashMap::default(),
            last_anim_frame: Instant::now(),
            close_hover: false,
            #[cfg(target_os = "windows")]
            hovered_tab: None,
            #[cfg(target_os = "windows")]
            selected_tab: 0,
            #[cfg(target_os = "windows")]
            tab_appearance: FxHashMap::default(),
            #[cfg(target_os = "windows")]
            close_hover_opacity: 0.0,
        }
    }

    pub fn configure(
        &mut self,
        navigation: &rio_backend::config::navigation::Navigation,
    ) {
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
    }

    /// Share per-tab geometry between rendering, dragging, and hit testing.
    pub fn tab_layout(
        &self,
        window_width: f32,
        scale_factor: f32,
        num_tabs: usize,
    ) -> TabStripLayout {
        #[cfg(target_os = "windows")]
        {
            let mut layout =
                tab_strip_layout(window_width, scale_factor, num_tabs, f32::MAX);
            let available = layout.tabs_width;
            layout.widths = (0..num_tabs)
                .map(|index| {
                    self.preferred_tab_widths
                        .get(index)
                        .copied()
                        .unwrap_or(self.max_tab_width)
                        .clamp(self.min_tab_width, self.max_tab_width)
                })
                .collect();
            let desired: f32 = layout.widths.iter().sum();
            if desired > available {
                let minimum = self.min_tab_width * num_tabs as f32;
                if minimum < available {
                    let shrink = (desired - available) / (desired - minimum);
                    for width in &mut layout.widths {
                        *width -= (*width - self.min_tab_width) * shrink;
                    }
                } else {
                    layout.widths.fill(available / num_tabs.max(1) as f32);
                }
            }
            layout.tabs_width = layout.widths.iter().sum();
            layout.tab_width = layout.tabs_width / num_tabs.max(1) as f32;
            layout
        }
        #[cfg(not(target_os = "windows"))]
        tab_strip_layout(window_width, scale_factor, num_tabs, self.max_tab_width)
    }

    #[cfg(target_os = "windows")]
    fn title_padding(&self) -> f32 {
        TAB_GAP / 2.0 + TAB_ICON_INSET_X + self.tab_icon_size + 8.0
    }

    #[cfg(target_os = "windows")]
    fn measure_tab_width(&mut self, sugarloaf: &mut Sugarloaf, tabs: &[TabPresentation]) {
        #[cfg(feature = "profiling")]
        let _timing = crate::profiling::span("tabs.measure_widths");
        let opts = DrawOpts {
            font_size: crate::renderer::windows_chrome::snap(
                TITLE_FONT_SIZE,
                sugarloaf.scale_factor(),
            ),
            font_id: Some(rio_backend::sugarloaf::font::FONT_ID_REGULAR),
            ..DrawOpts::default()
        };
        self.preferred_tab_widths.clear();
        for tab in tabs {
            let padding = self.title_padding()
                + TAB_TITLE_PADDING_RIGHT
                + if tab.remote {
                    self.tab_icon_size + 4.0
                } else {
                    0.0
                }
                + if tab.bell {
                    BELL_DOT_SIZE + BELL_GAP
                } else {
                    0.0
                };
            let width = sugarloaf.text_mut().measure(&tab.title, &opts) + padding;
            self.preferred_tab_widths
                .push(width.clamp(self.min_tab_width, self.max_tab_width));
        }
    }

    /// Set whether the cursor hovers the active island's close button.
    /// Returns true when the state changed (the caller redraws).
    pub fn set_close_hover(&mut self, hover: bool) -> bool {
        let changed = self.close_hover != hover;
        self.close_hover = hover;
        changed
    }

    #[cfg(target_os = "windows")]
    pub fn set_tab_hover(&mut self, tab: Option<usize>) -> bool {
        let changed = self.hovered_tab != tab;
        self.hovered_tab = tab;
        changed
    }

    pub fn update_colors(
        &mut self,
        inactive_text_color: [f32; 4],
        active_text_color: [f32; 4],
    ) {
        self.inactive_text_color = inactive_text_color;
        self.active_text_color = active_text_color;
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
        let report = self
            .program_status
            .as_ref()
            .map(|r| ProgressReport {
                state: match r.state {
                    ProgramState::Working if r.progress.is_none() => {
                        ProgressState::Indeterminate
                    }
                    ProgramState::Working | ProgramState::Done => ProgressState::Set,
                    ProgramState::Blocked => ProgressState::Pause,
                    ProgramState::Error => ProgressState::Error,
                    ProgramState::Idle | ProgramState::Clear => ProgressState::Remove,
                },
                progress: match r.state {
                    ProgramState::Done | ProgramState::Error => Some(100),
                    ProgramState::Blocked => Some(r.progress.unwrap_or(100)),
                    _ => r.progress,
                },
            })
            .or_else(|| store.legacy_progress().map(|(report, _)| report))
            .unwrap_or(ProgressReport {
                state: ProgressState::Remove,
                progress: None,
            });
        self.set_progress_report(report);
        self.progress_persistent = store.received_report();
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
            #[cfg(any(target_os = "windows", target_os = "linux"))]
            return context_manager.program_status_for_tab(tab_index);
            #[cfg(not(any(target_os = "windows", target_os = "linux")))]
            None
        }
    }

    fn status_color(&self, record: &ProgramStatusRecord) -> Option<[f32; 4]> {
        match record.state {
            ProgramState::Working => Some(self.progress_bar_color),
            ProgramState::Blocked => Some([0.90, 0.57, 0.22, 1.0]),
            ProgramState::Done => Some([0.34, 0.70, 0.38, 1.0]),
            ProgramState::Error => Some(self.progress_bar_error_color),
            ProgramState::Idle | ProgramState::Clear => None,
        }
    }

    fn uses_window_progress(&self, num_tabs: usize) -> bool {
        // Multiple tabs already draw one status line per tab. A lone title
        // (or hidden strip) and legacy OSC 9;4 still use the window bar.
        num_tabs <= 1 || !self.progress_persistent
    }

    fn render_tab_status_line(
        &self,
        sugarloaf: &mut Sugarloaf,
        record: Option<&ProgramStatusRecord>,
        x: f32,
        width: f32,
        order: u8,
    ) {
        if let Some(color) = record.and_then(|r| self.status_color(r)) {
            sugarloaf.rounded_rect(
                None,
                x,
                ISLAND_HEIGHT - PROGRESS_BAR_HEIGHT,
                width,
                PROGRESS_BAR_HEIGHT,
                color,
                0.0,
                PROGRESS_BAR_HEIGHT / 2.0,
                order,
            );
        }
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

    /// Check if the island needs continuous rendering (for animations)
    pub fn needs_redraw(&self) -> bool {
        #[cfg(target_os = "windows")]
        if self.window_header.needs_redraw() {
            return true;
        }
        #[cfg(target_os = "windows")]
        if self.close_hover_opacity != u8::from(self.close_hover) as f32
            || self.tab_appearance.iter().any(|(&i, &(active, hover))| {
                active != u8::from(i == self.selected_tab) as f32
                    || hover != u8::from(Some(i) == self.hovered_tab) as f32
            })
        {
            return true;
        }
        // A held drag doesn't need continuous frames: the floating tab
        // only moves on CursorMoved (which requests its own redraws);
        // only the slide springs animate between input events.
        (self.window_progress_visible
            && matches!(self.progress_state, Some(ProgressState::Indeterminate)))
            || !self.slide_springs.is_empty()
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
        if let (Some(floating_left), Some(drag)) = (
            self.drag_floating_left(layout),
            self.drag.as_ref().filter(|d| d.started),
        ) {
            let slot_x = layout.left(drag.tab_index);
            let offset = floating_left - slot_x;
            if offset.abs() > 0.01 {
                let spring = self
                    .slide_springs
                    .entry(drag.tab_index)
                    .or_insert_with(Spring::new);
                spring.position = offset;
            }
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
    /// `ContextManager::move_current_tab_to`), then seed slide springs
    /// on the displaced tabs so they animate into their new slot.
    pub fn remap_tab_move(&mut self, from: usize, to: usize, tab_width: f32) {
        if from == to {
            return;
        }

        self.slide_springs = self
            .slide_springs
            .drain()
            .map(|(i, v)| (Self::remap_index(i, from, to), v))
            .collect();
        #[cfg(target_os = "windows")]
        {
            if from < self.preferred_tab_widths.len()
                && to < self.preferred_tab_widths.len()
            {
                let width = self.preferred_tab_widths.remove(from);
                self.preferred_tab_widths.insert(to, width);
            }
            self.tab_appearance = self
                .tab_appearance
                .drain()
                .map(|(i, v)| (Self::remap_index(i, from, to), v))
                .collect();
            self.selected_tab = Self::remap_index(self.selected_tab, from, to);
        }
        if let Some(picker) = self.color_picker_tab {
            self.color_picker_tab = Some(Self::remap_index(picker, from, to));
        }
        if let Some(ref mut drag) = self.drag {
            drag.tab_index = Self::remap_index(drag.tab_index, from, to);
        }

        // Displaced tabs shifted one slot away from `from` toward `to`'s
        // side; seed (or accumulate into) a spring so each one starts at
        // its old x and slides to the new slot. The moved tab itself ends
        // at `to`, which both ranges exclude — while dragging it floats,
        // and on a keyboard move it jumps (no old position to animate
        // from that wouldn't fight the selection change).
        let (range, delta) = if from < to {
            // Tabs at from+1..=to moved left by one: now at from..to.
            (from..to, tab_width)
        } else {
            // Tabs at to..from moved right by one: now at to+1..=from.
            (to + 1..from + 1, -tab_width)
        };
        for i in range {
            let spring = self.slide_springs.entry(i).or_insert_with(Spring::new);
            spring.position += delta;
        }
    }

    /// Re-key per-tab state after tabs `a` and `b` swapped places —
    /// `ContextManager::move_current_to_prev/next` semantics, which swap
    /// (including the wrap-around end-to-end case) instead of rotating.
    /// Adjacent swaps get slide springs; wrap-around jumps don't (a
    /// full-bar slide reads as glitch, not motion).
    pub fn remap_tab_swap(&mut self, a: usize, b: usize, tab_width: f32) {
        if a == b {
            return;
        }

        let swap_key = |i: usize| {
            if i == a {
                b
            } else if i == b {
                a
            } else {
                i
            }
        };
        self.slide_springs = self
            .slide_springs
            .drain()
            .map(|(i, v)| (swap_key(i), v))
            .collect();
        #[cfg(target_os = "windows")]
        {
            if a < self.preferred_tab_widths.len() && b < self.preferred_tab_widths.len()
            {
                self.preferred_tab_widths.swap(a, b);
            }
            self.tab_appearance = self
                .tab_appearance
                .drain()
                .map(|(i, v)| (swap_key(i), v))
                .collect();
            self.selected_tab = swap_key(self.selected_tab);
        }
        if let Some(picker) = self.color_picker_tab {
            self.color_picker_tab = Some(swap_key(picker));
        }

        if a.abs_diff(b) == 1 {
            let delta = (b as f32 - a as f32) * tab_width;
            let spring = self.slide_springs.entry(a).or_insert_with(Spring::new);
            spring.position += delta;
            let spring = self.slide_springs.entry(b).or_insert_with(Spring::new);
            spring.position -= delta;
        }
    }

    pub fn remap_tab_swap_with_layout(
        &mut self,
        a: usize,
        b: usize,
        layout: &TabStripLayout,
    ) {
        self.remap_tab_swap(a, b, 0.0);
        if a.abs_diff(b) == 1 {
            let low = a.min(b);
            let high = a.max(b);
            self.slide_springs
                .entry(low)
                .or_insert_with(Spring::new)
                .position += layout.width(low);
            self.slide_springs
                .entry(high)
                .or_insert_with(Spring::new)
                .position -= layout.width(high);
        }
    }

    /// Check if the progress bar should be auto-dismissed due to timeout.
    /// Uses `progress_last_seen` (heartbeat), not `progress_started_at`, so
    /// a long-running TUI that keeps reporting stays visible.
    fn check_progress_timeout(&mut self) {
        if self.progress_persistent {
            return;
        }
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
            ProgressState::Pause if self.progress_persistent => [0.90, 0.57, 0.22, 1.0],
            ProgressState::Set
                if self
                    .program_status
                    .as_ref()
                    .is_some_and(|r| r.state == ProgramState::Done) =>
            {
                [0.34, 0.70, 0.38, 1.0]
            }
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

    /// Render tabs using equal-width layout
    #[inline]
    pub fn render(
        &mut self,
        sugarloaf: &mut Sugarloaf,
        dimensions: (f32, f32, f32),
        context_manager: &ContextManager<EventProxy>,
        bg_color: [f32; 4],
    ) {
        #[cfg(feature = "profiling")]
        let _timing = crate::profiling::span("tabs.render");
        let (window_width, _window_height, scale_factor) = dimensions;
        let num_tabs = context_manager.len();
        let current_tab_index = context_manager.current_index();
        #[cfg(target_os = "windows")]
        self.window_header.render(
            sugarloaf,
            window_width / scale_factor,
            self.active_text_color,
        );

        #[cfg(any(target_os = "windows", target_os = "linux"))]
        self.sync_focused_program_status(context_manager);
        self.window_progress_visible = self.uses_window_progress(num_tabs);

        // Immediate-mode: no cached ids to hide. If we early-return
        // without drawing, the tabs just don't appear this frame.
        if self.hide_if_single && num_tabs == 1 {
            // No tab strip — drop any leftover drag/slide state so
            // `needs_redraw` doesn't keep frames alive for invisible
            // tabs.
            self.drag = None;
            self.slide_springs.clear();
            #[cfg(target_os = "windows")]
            {
                self.tab_appearance.clear();
                self.close_hover_opacity = u8::from(self.close_hover) as f32;
            }
            let progress_y = if cfg!(target_os = "windows") {
                ISLAND_HEIGHT
            } else {
                0.0
            };
            self.render_progress_bar(sugarloaf, window_width, scale_factor, progress_y);
            return;
        }

        // A lone tab cannot be reordered. Cancel a drag if the other tab's
        // shell exits while it is in progress.
        if num_tabs == 1 {
            self.drag = None;
            self.slide_springs.clear();
        }

        // A reorder that didn't come from this drag (tab closed via
        // shell exit, keyboard move) breaks the drag.tab_index ==
        // current_index invariant — drop the drag instead of floating
        // a phantom tab over the wrong slot.
        if self
            .drag
            .as_ref()
            .is_some_and(|d| d.tab_index != current_tab_index)
        {
            self.drag = None;
        }

        // Advance the slide springs (drag-reorder animation) by this
        // frame's dt; settled springs drop out of the map.
        let now = Instant::now();
        let dt = now
            .duration_since(self.last_anim_frame)
            .as_secs_f32()
            .min(DRAG_MAX_DT);
        self.last_anim_frame = now;
        self.slide_springs
            .retain(|_, s| s.update(dt, DRAG_ANIMATION_LENGTH));

        #[cfg(target_os = "windows")]
        {
            self.selected_tab = current_tab_index;
            self.tab_appearance.retain(|&i, _| i < num_tabs);
            for i in 0..num_tabs {
                let active = u8::from(i == current_tab_index) as f32;
                let hover = u8::from(Some(i) == self.hovered_tab) as f32;
                let appearance = self.tab_appearance.entry(i).or_insert((active, 0.0));
                fade_toward(&mut appearance.0, active, dt);
                fade_toward(&mut appearance.1, hover, dt);
            }
            fade_toward(
                &mut self.close_hover_opacity,
                u8::from(self.close_hover) as f32,
                dt,
            );
        }

        #[cfg(target_os = "windows")]
        let tabs: Vec<_> = (0..num_tabs)
            .map(|index| {
                let raw_title = self.get_title_for_tab(context_manager, index);
                let status = self.status_for_tab(context_manager, index);
                let (icon, remote) = context_manager
                    .application_for_tab(index)
                    .map(|app| {
                        (
                            super::app_icons::for_program(
                                &app.program,
                                &app.title,
                                status.as_ref(),
                                app.reported,
                            ),
                            app.remote,
                        )
                    })
                    .unwrap_or((super::app_icons::DEFAULT_ICON, false));
                TabPresentation {
                    title: Self::status_title(status.as_ref(), &raw_title),
                    status,
                    icon,
                    remote,
                    bell: context_manager.bell(index),
                }
            })
            .collect();
        #[cfg(target_os = "windows")]
        self.measure_tab_width(sugarloaf, &tabs);
        #[cfg(target_os = "windows")]
        let tab_title_y = crate::renderer::windows_chrome::centered_text_y(
            sugarloaf,
            "Mg",
            ISLAND_HEIGHT / 2.0 + 2.0,
            &DrawOpts {
                font_size: crate::renderer::windows_chrome::snap(
                    TITLE_FONT_SIZE,
                    scale_factor,
                ),
                font_id: Some(rio_backend::sugarloaf::font::FONT_ID_REGULAR),
                ..DrawOpts::default()
            },
        );
        #[cfg(target_os = "windows")]
        let tab_padding_x = self.title_padding();
        #[cfg(not(target_os = "windows"))]
        let tab_padding_x = TAB_PADDING_X;
        let layout = self.tab_layout(window_width, scale_factor, num_tabs);
        let TabStripLayout { left_margin, .. } = layout;

        // Starting from left edge (with margin on macOS for traffic lights)
        let mut x_position = left_margin;

        // Active drag: the dragged tab is skipped in the slot loop and
        // drawn floating (after the loop, on a higher layer) instead.
        let drag_index = self.drag_index();
        let floating_left = self.drag_floating_left(&layout);

        // Derive the chrome from the effective background so OSC 11 and
        // theme changes keep the tab strip coherent with the terminal.
        #[cfg(not(target_os = "windows"))]
        let fills = island_fills(bg_color);
        #[cfg(target_os = "windows")]
        let fills = windows_island_fills(bg_color);
        #[cfg(target_os = "windows")]
        let group_fill = {
            let mut color = fills.inactive;
            color[3] = 0.045;
            color
        };
        #[cfg(target_os = "windows")]
        {
            sugarloaf.rounded_rect(
                None,
                left_margin,
                6.0,
                layout.tabs_width,
                ISLAND_HEIGHT - 12.0,
                group_fill,
                0.05,
                ((ISLAND_HEIGHT - 12.0) / 2.0).min(layout.tabs_width / 2.0),
                0,
            );
            let mut divider = fills.outline.unwrap_or(fills.active);
            divider[3] *= 0.55;
            sugarloaf.rect(
                None,
                0.0,
                ISLAND_HEIGHT - 1.0,
                window_width / scale_factor,
                1.0,
                divider,
                0.0,
                0,
            );
        }

        // Render each tab
        for tab_index in 0..num_tabs {
            let tab_width = layout.width(tab_index);
            // The dragged tab floats — drawn after the loop instead.
            if Some(tab_index) == drag_index {
                x_position += tab_width;
                continue;
            }

            let is_active = tab_index == current_tab_index;

            // Slot position plus any slide-spring offset (tab still
            // animating into its slot after a reorder).
            let tab_x = x_position
                + self
                    .slide_springs
                    .get(&tab_index)
                    .map_or(0.0, |s| s.position);

            // Get title for this tab, then truncate with a trailing
            // ellipsis so overflowing titles can't bleed into the next
            // tab or past the left edge (issue #1508).
            #[cfg(not(target_os = "windows"))]
            let raw_title = self.get_title_for_tab(context_manager, tab_index);
            #[cfg(not(target_os = "windows"))]
            let program_status = self.status_for_tab(context_manager, tab_index);
            #[cfg(target_os = "windows")]
            let tab = &tabs[tab_index];
            #[cfg(target_os = "windows")]
            let (app_icon, remote) = (tab.icon, tab.remote);
            #[cfg(target_os = "windows")]
            let program_status = &tab.status;
            #[cfg(target_os = "windows")]
            let remote_padding = if remote {
                self.tab_icon_size + 4.0
            } else {
                0.0
            };
            #[cfg(not(target_os = "windows"))]
            let raw_title = Self::status_title(program_status.as_ref(), &raw_title);
            #[cfg(target_os = "windows")]
            let raw_title = &tab.title;
            if raw_title.is_empty() {
                x_position += tab_width;
                continue;
            }
            // Windows keeps the same pill for one or many tabs. Other
            // platforms centre a lone title without an island.
            let single = num_tabs == 1 && !cfg!(target_os = "windows");

            #[cfg(not(target_os = "windows"))]
            let text_color = if single {
                match context_manager.custom_color(tab_index) {
                    Some(mut custom) => {
                        custom[3] = 1.0;
                        custom
                    }
                    None => self.active_text_color,
                }
            } else if is_active {
                self.active_text_color
            } else {
                self.inactive_text_color
            };
            #[cfg(target_os = "windows")]
            let text_color = mix_color(
                self.inactive_text_color,
                self.active_text_color,
                self.tab_appearance[&tab_index].0,
            );

            let title_opts = DrawOpts {
                font_size: TITLE_FONT_SIZE,
                color: color_u8(text_color),
                #[cfg(target_os = "windows")]
                font_id: Some(rio_backend::sugarloaf::font::FONT_ID_REGULAR),
                ..DrawOpts::default()
            };
            #[cfg(target_os = "windows")]
            let title_opts = DrawOpts {
                font_size: crate::renderer::windows_chrome::snap(
                    TITLE_FONT_SIZE,
                    scale_factor,
                ),
                ..title_opts
            };

            // The bell mark is its own text run: the UI text layer resolves
            // a single font per draw from the run's first char, so gluing it
            // onto the title would shape the whole title in the emoji font.
            #[cfg(not(target_os = "windows"))]
            let bell = context_manager.bell(tab_index);
            #[cfg(target_os = "windows")]
            let bell = tab.bell;
            let bell_width = if bell { BELL_DOT_SIZE + BELL_GAP } else { 0.0 };

            let max_text_width = if single {
                single_title_budget(window_width, scale_factor, left_margin) - bell_width
            } else {
                #[cfg(target_os = "windows")]
                let right_padding = TAB_TITLE_PADDING_RIGHT;
                #[cfg(not(target_os = "windows"))]
                let right_padding = TAB_PADDING_X;
                #[cfg(target_os = "windows")]
                let right_padding = right_padding + remote_padding;
                tab_width - tab_padding_x - right_padding - bell_width
            }
            .max(0.0);
            let title =
                fit_title_to_width(sugarloaf, &raw_title, max_text_width, &title_opts);

            // UI text always paints in a final pass above every rect,
            // so the floating tab's opaque background can't occlude
            // titles passing underneath it — skip a title once the
            // floating tab intrudes past the slot's text padding (the
            // widest a centered title can reach).
            let hidden_by_drag = floating_left.is_some_and(|fl| {
                let floating_width = layout.width(drag_index.unwrap_or(tab_index));
                let overlap =
                    (tab_x + tab_width).min(fl + floating_width) - tab_x.max(fl);
                overlap > tab_padding_x
            });

            let show_title = !hidden_by_drag;
            #[cfg(target_os = "windows")]
            let show_title = show_title && max_text_width >= TITLE_FONT_SIZE;
            if show_title {
                // Measure → centre → draw. Immediate mode, no cached
                // text_id bookkeeping. The bell mark and the title are
                // centred as one group.
                #[cfg(not(target_os = "windows"))]
                let text_width =
                    sugarloaf.text_mut().measure(&title, &title_opts) + bell_width;
                #[cfg(not(target_os = "windows"))]
                let text_x = if single {
                    single_title_x(window_width, scale_factor, text_width, left_margin)
                } else {
                    tab_x + (tab_width - text_width) / 2.0
                };
                #[cfg(target_os = "windows")]
                let text_x = tab_x + tab_padding_x + remote_padding;
                #[cfg(not(target_os = "windows"))]
                let text_y = (ISLAND_HEIGHT / 2.0) - (TITLE_FONT_SIZE / 2.);
                #[cfg(target_os = "windows")]
                let (text_x, text_y) = (
                    crate::renderer::windows_chrome::snap(text_x, scale_factor),
                    tab_title_y,
                );
                if bell {
                    // Centered on the title's lowercase body, not the strip:
                    // the em box hangs from `text_y` with its optical middle
                    // about two thirds down, so a strip-centered dot rides
                    // visibly high next to lowercase titles.
                    let dot_y =
                        text_y + TITLE_FONT_SIZE * (2.0 / 3.0) - BELL_DOT_SIZE / 2.0;
                    sugarloaf.rounded_rect(
                        None,
                        text_x,
                        dot_y,
                        BELL_DOT_SIZE,
                        BELL_DOT_SIZE,
                        text_color,
                        0.0,
                        BELL_DOT_SIZE / 2.0,
                        BELL_DOT_ORDER,
                    );
                }
                sugarloaf.text_mut().draw(
                    text_x + bell_width,
                    text_y,
                    &title,
                    &title_opts,
                );
            }

            // Nothing is drawn behind a lone title.
            if single {
                x_position += tab_width;
                continue;
            }

            // Rounded island for this tab. A custom color (picker /
            // color-automation) becomes the island fill: the active
            // tab keeps it vivid while inactive siblings are muted
            // toward the strip — a white "active" overlay would
            // bleach custom colors to pastel on light themes, so the
            // hierarchy is carried by the mute instead.
            let (ix, iy, iw, ih, radius) = island_rect(tab_x, tab_width);
            #[cfg(not(target_os = "windows"))]
            let fill = match context_manager.custom_color(tab_index) {
                Some(mut custom) => {
                    if !is_active {
                        custom[3] *= INACTIVE_CUSTOM_MUTE;
                    }
                    custom
                }
                None => {
                    if is_active {
                        fills.active
                    } else {
                        fills.inactive
                    }
                }
            };
            #[cfg(target_os = "windows")]
            let fill = {
                let (active, hover) = self.tab_appearance[&tab_index];
                match context_manager.custom_color(tab_index) {
                    Some(mut custom) => {
                        custom[3] *=
                            INACTIVE_CUSTOM_MUTE + (1.0 - INACTIVE_CUSTOM_MUTE) * active;
                        custom
                    }
                    None => {
                        let mut fill = mix_color(fills.inactive, fills.active, active);
                        fill[3] += hover * (1.0 - active) * 0.04;
                        fill
                    }
                }
            };
            #[cfg(not(target_os = "windows"))]
            let outline = fills.outline;
            #[cfg(target_os = "windows")]
            let outline = fills
                .outline
                .filter(|_| self.tab_appearance[&tab_index].0 > 0.0)
                .map(|mut color| {
                    color[3] *= self.tab_appearance[&tab_index].0;
                    color
                });
            #[cfg(not(target_os = "windows"))]
            let tab_background = bg_color;
            #[cfg(target_os = "windows")]
            let tab_background = over(bg_color, group_fill);
            draw_island(
                sugarloaf,
                ix,
                iy,
                iw,
                ih,
                radius,
                fill,
                outline,
                Some(tab_background),
                0,
            );
            self.render_tab_status_line(sugarloaf, program_status.as_ref(), ix, iw, 1);

            #[cfg(target_os = "windows")]
            if !hidden_by_drag
                && iw >= TAB_ICON_INSET_X + self.tab_icon_size + remote_padding + 4.0
            {
                if remote {
                    draw_tab_icon(
                        sugarloaf,
                        ix + TAB_ICON_INSET_X,
                        text_color,
                        1,
                        super::app_icons::SSH_ICON,
                        self.window_header.app_font,
                        self.tab_icon_size,
                    );
                }
                draw_tab_icon(
                    sugarloaf,
                    ix + TAB_ICON_INSET_X + remote_padding,
                    text_color,
                    1,
                    app_icon,
                    self.window_header.app_font,
                    self.tab_icon_size,
                );
            }

            if is_active && num_tabs > 1 {
                if let Some(cx) = close_button_center(ix, iw) {
                    #[cfg(not(target_os = "windows"))]
                    let hover_opacity = u8::from(self.close_hover) as f32;
                    #[cfg(target_os = "windows")]
                    let hover_opacity = self.close_hover_opacity;
                    if hover_opacity > 0.0 {
                        let mut hover_fill = fills.close_hover;
                        hover_fill[3] *= hover_opacity;
                        sugarloaf.rounded_rect(
                            None,
                            cx - CLOSE_HOVER_HALF,
                            ISLAND_HEIGHT / 2.0 - CLOSE_HOVER_HALF,
                            CLOSE_HOVER_HALF * 2.0,
                            CLOSE_HOVER_HALF * 2.0,
                            hover_fill,
                            0.05,
                            CLOSE_HOVER_CORNER_RADIUS,
                            1,
                        );
                    }
                    draw_close_button(
                        sugarloaf,
                        cx,
                        self.active_text_color,
                        self.close_hover,
                        2,
                    );
                }
            }

            // Move to next tab position
            x_position += tab_width;
        }

        // Draw the floating (dragged) tab above the slot tabs.
        if let (Some(drag_idx), Some(floating_x)) = (drag_index, floating_left) {
            let tab_width = layout.width(drag_idx);
            let (ix, iy, iw, ih, radius) = island_rect(floating_x, tab_width);

            // Soft elevation: a slightly inflated dark halo behind the
            // lifted island so it reads as floating over the strip.
            sugarloaf.rounded_rect(
                None,
                ix - 2.0,
                iy - 1.0,
                iw + 4.0,
                ih + 3.0,
                [0.0, 0.0, 0.0, 0.18],
                0.05,
                radius + 2.0,
                11,
            );

            let fill = match context_manager.custom_color(drag_idx) {
                Some(mut custom) => {
                    custom[3] = 1.0;
                    custom
                }
                None => {
                    let mut base = bg_color;
                    base[3] = 1.0;
                    over(base, fills.active)
                }
            };
            draw_island(
                sugarloaf,
                ix,
                iy,
                iw,
                ih,
                radius,
                fill,
                fills.outline,
                None,
                11,
            );

            if let Some(cx) = close_button_center(ix, iw) {
                draw_close_button(sugarloaf, cx, self.active_text_color, false, 12);
            }

            #[cfg(not(target_os = "windows"))]
            let program_status = self.status_for_tab(context_manager, drag_idx);
            #[cfg(target_os = "windows")]
            let tab = &tabs[drag_idx];
            #[cfg(target_os = "windows")]
            let (app_icon, remote) = (tab.icon, tab.remote);
            #[cfg(target_os = "windows")]
            let program_status = &tab.status;
            #[cfg(target_os = "windows")]
            let remote_padding = if remote {
                self.tab_icon_size + 4.0
            } else {
                0.0
            };
            self.render_tab_status_line(sugarloaf, program_status.as_ref(), ix, iw, 12);
            #[cfg(target_os = "windows")]
            if iw >= TAB_ICON_INSET_X + self.tab_icon_size + remote_padding + 4.0 {
                if remote {
                    draw_tab_icon(
                        sugarloaf,
                        ix + TAB_ICON_INSET_X,
                        self.active_text_color,
                        12,
                        super::app_icons::SSH_ICON,
                        self.window_header.app_font,
                        self.tab_icon_size,
                    );
                }
                draw_tab_icon(
                    sugarloaf,
                    ix + TAB_ICON_INSET_X + remote_padding,
                    self.active_text_color,
                    12,
                    app_icon,
                    self.window_header.app_font,
                    self.tab_icon_size,
                );
            }

            #[cfg(not(target_os = "windows"))]
            let raw_title = self.get_title_for_tab(context_manager, drag_idx);
            #[cfg(not(target_os = "windows"))]
            let raw_title = Self::status_title(program_status.as_ref(), &raw_title);
            #[cfg(target_os = "windows")]
            let raw_title = &tab.title;
            if !raw_title.is_empty() {
                #[cfg(target_os = "windows")]
                let right_padding = TAB_TITLE_PADDING_RIGHT;
                #[cfg(target_os = "windows")]
                let right_padding = right_padding + remote_padding;
                #[cfg(not(target_os = "windows"))]
                let right_padding = TAB_PADDING_X;
                let max_text_width = (tab_width - tab_padding_x - right_padding).max(0.0);
                let title_opts = DrawOpts {
                    font_size: TITLE_FONT_SIZE,
                    color: color_u8(self.active_text_color),
                    #[cfg(target_os = "windows")]
                    font_id: Some(rio_backend::sugarloaf::font::FONT_ID_REGULAR),
                    ..DrawOpts::default()
                };
                #[cfg(target_os = "windows")]
                let title_opts = DrawOpts {
                    font_size: crate::renderer::windows_chrome::snap(
                        TITLE_FONT_SIZE,
                        scale_factor,
                    ),
                    ..title_opts
                };
                let title = fit_title_to_width(
                    sugarloaf,
                    &raw_title,
                    max_text_width,
                    &title_opts,
                );
                #[cfg(not(target_os = "windows"))]
                let text_width = sugarloaf.text_mut().measure(&title, &title_opts);
                #[cfg(not(target_os = "windows"))]
                let text_x = floating_x + (tab_width - text_width) / 2.0;
                #[cfg(target_os = "windows")]
                let text_x = floating_x + tab_padding_x + remote_padding;
                #[cfg(not(target_os = "windows"))]
                let text_y = (ISLAND_HEIGHT / 2.0) - (TITLE_FONT_SIZE / 2.);
                #[cfg(target_os = "windows")]
                let (text_x, text_y) = (
                    crate::renderer::windows_chrome::snap(text_x, scale_factor),
                    tab_title_y,
                );
                sugarloaf
                    .text_mut()
                    .draw(text_x, text_y, &title, &title_opts);
            }
        }

        // Render color picker if open
        if let Some(picker_tab) = self.color_picker_tab {
            if picker_tab < num_tabs {
                let picker_tab_x = layout.left(picker_tab);
                let tab_width = layout.width(picker_tab);
                let selected = context_manager.custom_color(picker_tab);
                self.render_color_picker(sugarloaf, picker_tab_x, tab_width, selected);
            }
        }

        // Render the progress bar below the island.
        if self.window_progress_visible {
            self.render_progress_bar(
                sugarloaf,
                window_width,
                scale_factor,
                ISLAND_HEIGHT,
            );
        }
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
        let layout = self.tab_layout(window_width, scale_factor, num_tabs);
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
            assert!(TAB_INSET_Y * 2.0 < ISLAND_HEIGHT);
            assert!(CLOSE_MARGIN_RIGHT + CLOSE_HIT_HALF_WIDTH < CLOSE_MIN_ISLAND_WIDTH);
            assert!(CLOSE_HOVER_HALF * 2.0 <= ISLAND_HEIGHT - TAB_INSET_Y * 2.0);
        }
    }

    /// The regression that shipped: `window_width` is physical while draws
    /// are logical, so centring on it put the title off the right edge of a
    /// 2x display and nothing appeared at all.
    #[test]
    fn single_title_is_centred_in_logical_pixels() {
        // 1600 physical at 2x is an 800pt strip, so a 100pt title starts at
        // 350, not at 750 (which would be centred on the physical width and
        // sit past the right edge).
        let x = single_title_x(1600.0, 2.0, 100.0, 0.0);
        assert_eq!(x, 350.0);
        assert!(x + 100.0 <= 800.0, "title must stay on screen: {x}");

        // At 1x the two agree, which is why this only showed up on retina.
        assert_eq!(single_title_x(800.0, 1.0, 100.0, 0.0), 350.0);
    }

    #[test]
    fn single_title_never_reaches_under_the_traffic_lights() {
        let margin = 76.0;
        // A title wider than the strip would centre at a negative x.
        let x = single_title_x(1600.0, 2.0, 900.0, margin);
        assert_eq!(x, margin + TAB_PADDING_X);
    }

    #[test]
    fn single_title_budget_leaves_both_margins() {
        // 800pt strip, no left margin: full width less the right margin and
        // the padding on each side.
        assert_eq!(
            single_title_budget(1600.0, 2.0, 0.0),
            800.0 - ISLAND_MARGIN_RIGHT - TAB_PADDING_X * 2.0
        );
        // The macOS left margin comes off the top of that.
        assert_eq!(
            single_title_budget(1600.0, 2.0, 76.0),
            800.0 - 76.0 - ISLAND_MARGIN_RIGHT - TAB_PADDING_X * 2.0
        );
        // A window too narrow to hold any text yields no budget, not a
        // negative one that would underflow the truncation.
        assert_eq!(single_title_budget(100.0, 2.0, 76.0), 0.0);
    }

    /// A lone title gets far more room than a tab slot would give it, which
    /// is the point of dropping the island.
    #[test]
    fn single_title_budget_beats_a_tab_slot() {
        let slot = tab_strip_layout(1600.0, 2.0, 1, 240.0).tab_width;
        let slot_budget = (slot - TAB_PADDING_X * 2.0).max(0.0);
        assert!(
            single_title_budget(1600.0, 2.0, 0.0) > slot_budget,
            "expected more than a slot's {slot_budget}"
        );
    }

    #[test]
    fn island_rect_insets_slot_and_clamps_radius() {
        // Slot at x=100, width 180 → island inset by half the gap on
        // each side and TAB_INSET_Y vertically.
        let (x, y, w, h, radius) = island_rect(100.0, 180.0);
        assert_eq!(x, 100.0 + TAB_GAP / 2.0);
        assert_eq!(y, TAB_INSET_Y);
        assert_eq!(w, 180.0 - TAB_GAP);
        assert_eq!(h, ISLAND_HEIGHT - TAB_INSET_Y * 2.0);
        assert_eq!(radius, TAB_RADIUS);

        let (_, _, w, h, radius) = island_rect(0.0, 4.0);
        assert_eq!(w, 0.0);
        assert_eq!(radius, 0.0);
        assert!(radius <= h / 2.0);
    }

    #[test]
    fn island_fills_adapt_to_background_luminance() {
        let dark = island_fills([0.06, 0.05, 0.06, 1.0]);
        let light = island_fills([0.98, 0.98, 0.97, 1.0]);
        // Dark themes lighten the islands (white overlays); light
        // themes recess inactive cards (black overlay) and elevate the
        // active one (near-opaque white).
        assert_eq!(dark.inactive[0], 1.0);
        assert_eq!(light.inactive[0], 0.0);
        // The active step must stay well above the inactive step —
        // sub-10% deltas disappear on laptop panels.
        assert!(dark.active[3] >= dark.inactive[3] + 0.1);
        // On light themes the active island must read as the brighter,
        // elevated card: a strong white overlay against the recessed
        // black-tinted inactive fill.
        assert_eq!(light.active[0], 1.0);
        assert!(light.active[3] >= 0.8);
        // White overlays can't brighten a near-white bg, so light
        // themes must carry the card silhouette with an outline ring;
        // dark themes get by on shade steps alone.
        assert!(light.outline.is_some());
        assert!(dark.outline.is_none());
    }

    #[test]
    fn over_composites_source_over_destination() {
        let dst = [0.2, 0.4, 0.6, 1.0];
        let out = over(dst, [1.0, 1.0, 1.0, 0.25]);
        assert!((out[0] - 0.4).abs() < 1e-6);
        assert!((out[1] - 0.55).abs() < 1e-6);
        assert!((out[2] - 0.7).abs() < 1e-6);
        assert_eq!(out[3], 1.0);
        // Zero-alpha source is a no-op; full-alpha replaces.
        assert_eq!(over(dst, [0.9, 0.1, 0.3, 0.0]), dst);
        assert_eq!(over(dst, [0.9, 0.1, 0.3, 1.0]), [0.9, 0.1, 0.3, 1.0]);
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
        assert_eq!(island.progress_value, Some(40));
        assert_eq!(island.program_status_title("shell"), "[Working 40%] cargo");
        let animation_start = island.progress_started_at;

        cm.add_context(true, 0);
        parser.advance(
            &mut *cm.current().terminal.lock(),
            b"\x1b]7501;state=blocked:kind=permission:msg=QXBwcm92ZQ==\x07",
        );
        island.sync_focused_program_status(&cm);
        assert_eq!(island.progress_state, Some(ProgressState::Pause));
        assert_eq!(
            island.program_status_title("shell"),
            "[Needs permission] shell: Approve"
        );
        cm.set_current(0);
        island.sync_focused_program_status(&cm);
        assert_eq!(island.progress_value, Some(40));
        assert!(island.progress_started_at >= animation_start);

        // Update the background terminal while the first remains focused.
        cm.set_current(1);
        let background = cm.current().terminal.clone();
        cm.set_current(0);
        parser.advance(&mut *background.lock(), b"\x1b]7501;state=done\x07");
        island.sync_focused_program_status(&cm);
        assert_eq!(island.progress_value, Some(40));
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
        let started = island.progress_started_at;
        island.progress_last_seen =
            Some(Instant::now() - std::time::Duration::from_secs(60));
        island.check_progress_timeout();
        assert_eq!(island.progress_state, Some(ProgressState::Indeterminate));
        island.sync_program_status(1, &terminal.program_status);
        assert_eq!(island.progress_started_at, started);
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
            Some([0.34, 0.70, 0.38, 1.0])
        );
        assert_eq!(island.progress_value, Some(20));
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
            Some([0.90, 0.57, 0.22, 1.0])
        );
        assert_eq!(island.progress_value, Some(20));
        parser.advance(
            &mut *background_terminal.lock(),
            b"\x1b]7501;state=clear\x07",
        );
        assert!(island.status_for_tab(&cm, 0).is_none());
    }

    #[test]
    fn program_status_tab_underline_does_not_duplicate_window_bar() {
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
        assert!(!island.uses_window_progress(2),
            "the selected tab already has an underline; do not also draw a full-window bar");
        assert!(
            island.uses_window_progress(1),
            "a lone title has no separate tab underline"
        );
        island.hide_if_single = true;
        assert!(
            island.uses_window_progress(1),
            "a hidden strip still needs one progress bar"
        );

        parser.advance(
            &mut *cm.current().terminal.lock(),
            b"\x1b]7501;state=working\x07",
        );
        island.sync_focused_program_status(&cm);
        island.window_progress_visible = island.uses_window_progress(2);
        assert!(
            !island.needs_redraw(),
            "a static tab underline must not animate an invisible window bar"
        );

        parser.advance(&mut *cm.current().terminal.lock(), b"\x1bc\x1b]9;4;3\x07");
        island.sync_focused_program_status(&cm);
        island.window_progress_visible = island.uses_window_progress(2);
        assert!(
            island.window_progress_visible,
            "legacy progress still uses the window bar"
        );
        assert!(
            island.needs_redraw(),
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

    /// Each char = 1.0 wide, including the ellipsis. Easy arithmetic.
    fn fixed_unit_width(_c: char) -> f32 {
        1.0
    }

    fn rendered_width(s: &str, char_width: impl FnMut(char) -> f32) -> f32 {
        s.chars().map(char_width).sum()
    }

    #[test]
    fn title_fits_is_returned_unchanged() {
        assert_eq!(
            fit_title_with_widths("hello", 10.0, fixed_unit_width),
            "hello"
        );
        assert_eq!(fit_title_with_widths("hi", 2.0, fixed_unit_width), "hi");
    }

    #[test]
    fn title_that_fits_borrows_without_allocating() {
        // Confirms the zero-allocation "no truncation" hot path: when the
        // full title fits, the returned Cow must stay Borrowed so the
        // render loop doesn't allocate a new String every frame.
        let out = fit_title_with_widths("ok", 10.0, fixed_unit_width);
        assert!(
            matches!(out, Cow::Borrowed(_)),
            "expected borrowed, got {out:?}"
        );
    }

    #[test]
    fn title_zero_budget_returns_ellipsis() {
        // Historically this was short-circuited to return the full title;
        // now it falls through the loop and returns "…" consistently with
        // tiny-but-positive budgets.
        assert_eq!(fit_title_with_widths("abc", 0.0, fixed_unit_width), "…");
    }

    #[test]
    fn title_overflow_gets_ellipsized_and_fits_budget() {
        // "hello world" budgeted at 5 → best we can do without exceeding
        // is "hell" (4) + "…" (1) = 5. Anything more overflows.
        let out = fit_title_with_widths("hello world", 5.0, fixed_unit_width);
        assert_eq!(out, "hell…");
        assert!(
            rendered_width(&out, fixed_unit_width) <= 5.0,
            "truncated width {} must be ≤ budget 5",
            rendered_width(&out, fixed_unit_width)
        );
    }

    #[test]
    fn title_respects_budget_with_wide_chars() {
        // Mixed widths: 'W' = 2.0, others (including ellipsis) = 1.0.
        // Title "WxWxW", budget 4.0. Walk:
        // ix=0 W: before add, 0+1(suffix) ≤ 4 → truncate_ix=0; accum→2
        // ix=1 x: 2+1 ≤ 4 → truncate_ix=1; accum→3
        // ix=2 W: 3+1 ≤ 4 → truncate_ix=2; accum→5; 5>4 → cut.
        // Output: title[..2] + "…" = "Wx…", width 2+1+1 = 4 ≤ 4 ✓
        let widths = |c: char| if c == 'W' { 2.0 } else { 1.0 };
        let out = fit_title_with_widths("WxWxW", 4.0, widths);
        assert_eq!(out, "Wx…");
        assert!(rendered_width(&out, widths) <= 4.0);
    }

    #[test]
    fn title_truncation_preserves_utf8_boundaries() {
        // Each emoji/char = 2.0 wide; ellipsis = 2.0.
        // Title "🎟🎟🎟" = 6.0. Budget 4.0 → one emoji + "…" = 4.0 ≤ 4 ✓.
        // Crucial: the byte index we cut at must be on a UTF-8 boundary.
        let w = |_c: char| 2.0;
        let out = fit_title_with_widths("🎟🎟🎟", 4.0, w);
        assert_eq!(out, "🎟…");
        assert!(out.chars().count() == 2, "{out:?} should be 2 graphemes");
    }

    #[test]
    fn title_budget_smaller_than_ellipsis_still_returns_ellipsis() {
        // Budget 0.5 < ellipsis_width 1.0: first char overflows, prefix is
        // empty, we return just "…" so the user at least sees *something*
        // indicating truncation rather than a blank tab label.
        let out = fit_title_with_widths("abc", 0.5, fixed_unit_width);
        assert_eq!(out, "…");
    }

    #[test]
    fn title_empty_input_returned_as_is() {
        assert_eq!(fit_title_with_widths("", 10.0, fixed_unit_width), "");
    }

    #[test]
    fn title_exact_fit_not_truncated() {
        // Title "abcd" = 4.0, budget 4.0 → fits exactly, no truncation.
        assert_eq!(fit_title_with_widths("abcd", 4.0, fixed_unit_width), "abcd");
    }

    #[test]
    fn tab_strip_layout_geometry() {
        // 1000 physical px @ 2x scale → 500 logical px window. Slots
        // stay below the cap here, so the math matches the old
        // fill-the-strip layout.
        let layout = tab_strip_layout(1000.0, 2.0, 4, 240.0);
        #[cfg(target_os = "macos")]
        {
            assert_eq!(layout.left_margin, ISLAND_MARGIN_LEFT_MACOS);
            assert_eq!(layout.tab_width, (500.0 - 8.0 - 76.0) / 4.0);
            assert_eq!(layout.tabs_width, layout.tab_width * 4.0);
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            assert_eq!(layout.left_margin, 0.0);
            assert_eq!(layout.tab_width, 123.0);
            assert_eq!(layout.tabs_width, 492.0);
        }
        #[cfg(target_os = "windows")]
        {
            assert_eq!(layout.left_margin, 6.0);
            assert_eq!(layout.tab_width, 85.5);
            assert_eq!(layout.tabs_width, 342.0);
        }
        // Zero tabs clamps the divisor.
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
    fn remap_tab_move_carries_picker_and_springs() {
        let mut island = test_island();
        island.color_picker_tab = Some(3);

        // Tab 1 → 3 (rotate): the open picker shifts 3 → 2. Per-tab colors
        // and titles now live on the tab in ContextManager (see
        // context::test::test_custom_color_* / test_custom_title_*), so they
        // no longer need remapping here.
        island.remap_tab_move(1, 3, 100.0);
        assert_eq!(island.color_picker_tab, Some(2));

        // Displaced tabs (now at 1 and 2) got slide springs of +width.
        assert_eq!(island.slide_springs.len(), 2);
        assert_eq!(island.slide_springs.get(&1).unwrap().position, 100.0);
        assert_eq!(island.slide_springs.get(&2).unwrap().position, 100.0);
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
        }
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn close_button_anchors_to_island_right_edge() {
        // Full-width slot: slot 1 spans 180..360, island 183..354, so
        // the button centers at 354 - CLOSE_MARGIN_RIGHT.
        let layout = TabStripLayout {
            left_margin: 0.0,
            tab_width: 180.0,
            tabs_width: 360.0,
            widths: Vec::new(),
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
        };
        assert_eq!(close_button_center_x(&narrow, 3), None);
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn close_hit_box_clears_the_title_budget() {
        // A max-width centered title on slot 0 ends at
        // slot_right - TAB_PADDING_X; the close hit box must start at
        // or after that point, or clicking visible title glyphs would
        // close the tab.
        let layout = TabStripLayout {
            left_margin: 0.0,
            tab_width: 180.0,
            tabs_width: 360.0,
            widths: Vec::new(),
        };
        let cx = close_button_center_x(&layout, 0).unwrap();
        let title_max_right = layout.tab_width - TAB_PADDING_X;
        assert!(cx - CLOSE_HIT_HALF_WIDTH >= title_max_right);
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
    fn end_drag_seeds_settle_spring() {
        let mut island = test_island();
        island.start_drag(2, 0.0, 200.0);
        island.update_drag(250.0); // floating left = 250, slot x = 200
        island.end_drag(&test_layout());
        assert!(!island.is_dragging());
        let spring = island.slide_springs.get(&2).unwrap();
        assert_eq!(spring.position, 50.0);
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
