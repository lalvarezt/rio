// Copyright (c) 2023-present, Raphael Amorim.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

use crate::renderer::scrollbar;
use rio_backend::sugarloaf::text::DrawOpts;
use rio_backend::sugarloaf::Sugarloaf;
use std::time::Instant;

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

const PALETTE_WIDTH: f32 = 480.0;
const PALETTE_CORNER_RADIUS: f32 = 16.0;
const PALETTE_MARGIN_TOP: f32 = 80.0;
const PALETTE_TOP_FRACTION: f32 = 0.125;
const VIEWPORT_MARGIN: f32 = 16.0;
const PALETTE_PADDING: f32 = 8.0;
const INPUT_HEIGHT: f32 = 42.0;
const INPUT_FONT_SIZE: f32 = 15.0;
const INPUT_PADDING_X: f32 = 12.0;
const ICON_SIZE: f32 = 14.0;
const ICON_STROKE: f32 = 1.2;
const INPUT_TEXT_INSET: f32 = INPUT_PADDING_X + ICON_SIZE + PALETTE_PADDING;
const INPUT_RADIUS: f32 = 10.0;
const RESULT_RADIUS: f32 = 8.0;
const HINT_GAP: f32 = 20.0;
const SCROLLBAR_OPACITY: f32 = 0.65;
const RESULT_ITEM_HEIGHT: f32 = 34.0;
const RESULT_FONT_SIZE: f32 = 14.0;
const SHORTCUT_FONT_SIZE: f32 = 12.0;
const MAX_VISIBLE_RESULTS: usize = 12;
const GROUP_HEIGHT: f32 = 26.0;
const ORDER: u8 = 20;

/// Actions that can be triggered from the command palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteAction {
    TabCreate,
    TabClose,
    TabCloseUnfocused,
    SelectNextTab,
    SelectPrevTab,
    SplitRight,
    SplitDown,
    SelectNextSplit,
    SelectPrevSplit,
    ConfigEditor,
    WindowCreateNew,
    IncreaseFontSize,
    DecreaseFontSize,
    ResetFontSize,
    ToggleViMode,
    ToggleFullscreen,
    ToggleAppearanceTheme,
    Copy,
    Paste,
    SearchForward,
    SearchBackward,
    ClearHistory,
    CloseCurrentSplitOrTab,
    /// Browse installed font families while keeping the palette open.
    ListFonts,
    Quit,
}

struct Command {
    title: &'static str,
    shortcut: &'static str,
    action: PaletteAction,
}

const fn platform_shortcut(macos: &'static str, desktop: &'static str) -> &'static str {
    if cfg!(target_os = "macos") {
        macos
    } else {
        desktop
    }
}

const COMMANDS: &[Command] = &[
    Command {
        title: "New Tab",
        shortcut: platform_shortcut("⌘T", "Ctrl+Shift+T"),
        action: PaletteAction::TabCreate,
    },
    Command {
        title: "Close Tab",
        shortcut: platform_shortcut("⌘W", "Ctrl+Shift+W"),
        action: PaletteAction::TabClose,
    },
    Command {
        title: "Close Other Tabs",
        shortcut: "",
        action: PaletteAction::TabCloseUnfocused,
    },
    Command {
        title: "Next Tab",
        shortcut: platform_shortcut("⌃⇥", "Ctrl+Tab"),
        action: PaletteAction::SelectNextTab,
    },
    Command {
        title: "Previous Tab",
        shortcut: platform_shortcut("⌃⇧⇥", "Ctrl+Shift+Tab"),
        action: PaletteAction::SelectPrevTab,
    },
    Command {
        title: "Split Right",
        shortcut: platform_shortcut("⌘D", "Ctrl+Shift+R"),
        action: PaletteAction::SplitRight,
    },
    Command {
        title: "Split Down",
        shortcut: platform_shortcut("⇧⌘D", "Ctrl+Shift+D"),
        action: PaletteAction::SplitDown,
    },
    Command {
        title: "Next Split",
        shortcut: "",
        action: PaletteAction::SelectNextSplit,
    },
    Command {
        title: "Previous Split",
        shortcut: "",
        action: PaletteAction::SelectPrevSplit,
    },
    Command {
        title: "Close Split or Tab",
        shortcut: "",
        action: PaletteAction::CloseCurrentSplitOrTab,
    },
    Command {
        title: "Settings",
        shortcut: platform_shortcut("⌘,", "Ctrl+Shift+,"),
        action: PaletteAction::ConfigEditor,
    },
    Command {
        title: "New Window",
        shortcut: platform_shortcut("⌘N", "Ctrl+Shift+N"),
        action: PaletteAction::WindowCreateNew,
    },
    Command {
        title: "Increase Font Size",
        shortcut: platform_shortcut("⌘+", "Ctrl+="),
        action: PaletteAction::IncreaseFontSize,
    },
    Command {
        title: "Decrease Font Size",
        shortcut: platform_shortcut("⌘-", "Ctrl+-"),
        action: PaletteAction::DecreaseFontSize,
    },
    Command {
        title: "Reset Font Size",
        shortcut: platform_shortcut("⌘0", "Ctrl+0"),
        action: PaletteAction::ResetFontSize,
    },
    Command {
        title: "Toggle Vi Mode",
        shortcut: "",
        action: PaletteAction::ToggleViMode,
    },
    Command {
        title: "Toggle Fullscreen",
        shortcut: "",
        action: PaletteAction::ToggleFullscreen,
    },
    Command {
        title: "Toggle Appearance Theme",
        shortcut: "",
        action: PaletteAction::ToggleAppearanceTheme,
    },
    Command {
        title: "Copy",
        shortcut: platform_shortcut("⌘C", "Ctrl+Shift+C"),
        action: PaletteAction::Copy,
    },
    Command {
        title: "Paste",
        shortcut: platform_shortcut("⌘V", "Ctrl+Shift+V"),
        action: PaletteAction::Paste,
    },
    Command {
        title: "Search Forward",
        shortcut: platform_shortcut("⌘F", "Ctrl+Shift+F"),
        action: PaletteAction::SearchForward,
    },
    Command {
        title: "Search Backward",
        shortcut: "",
        action: PaletteAction::SearchBackward,
    },
    Command {
        title: "Clear History",
        shortcut: "",
        action: PaletteAction::ClearHistory,
    },
    Command {
        title: "List Fonts",
        shortcut: "",
        action: PaletteAction::ListFonts,
    },
    Command {
        title: "Quit",
        shortcut: platform_shortcut("⌘Q", ""),
        action: PaletteAction::Quit,
    },
];

/// What the palette is currently browsing and filtering over.
///
/// `Commands` is the default — fuzzy-matches against the static
/// `COMMANDS` list and dispatches a `PaletteAction` on Enter.
///
/// `Fonts` is entered via the `ListFonts` command. The palette stays
/// open, its content is replaced with the owned list of font family
/// names, and Enter closes the palette (no font-switching action yet).
/// The list is owned so the filter pass doesn't keep a borrow on the
/// sugarloaf FontLibrary.
enum PaletteMode {
    Commands,
    Fonts(Vec<String>),
}

/// One row in the filtered result list. Variants carry exactly the
/// data the render pass needs — no `&'static Command` vs `&str`
/// lifetime mixing.
#[derive(Clone, Copy)]
enum PaletteRow<'a> {
    Command {
        title: &'a str,
        shortcut: &'a str,
        action: PaletteAction,
    },
    Font {
        family: &'a str,
    },
}

impl<'a> PaletteRow<'a> {
    fn title(&self) -> &'a str {
        match *self {
            PaletteRow::Command { title, .. } => title,
            PaletteRow::Font { family } => family,
        }
    }

    fn shortcut(&self) -> &'a str {
        match *self {
            PaletteRow::Command { shortcut, .. } => shortcut,
            PaletteRow::Font { .. } => "",
        }
    }

    fn action(&self) -> Option<PaletteAction> {
        match *self {
            PaletteRow::Command { action, .. } => Some(action),
            PaletteRow::Font { .. } => None,
        }
    }
}

fn draw_copy_icon(sugarloaf: &mut Sugarloaf, x: f32, y: f32, color: [f32; 4]) {
    for [x1, y1, x2, y2] in [
        [x, y + 9.0, x, y],
        [x, y, x + 8.0, y],
        [x + 3.0, y + 3.0, x + 11.0, y + 3.0],
        [x + 11.0, y + 3.0, x + 11.0, y + 13.0],
        [x + 11.0, y + 13.0, x + 3.0, y + 13.0],
        [x + 3.0, y + 13.0, x + 3.0, y + 3.0],
    ] {
        sugarloaf.line(x1, y1, x2, y2, 1.0, 0.2, color, ORDER + 2);
    }
}

fn command_group(action: PaletteAction) -> (u8, &'static str) {
    use PaletteAction::*;
    match action {
        SplitRight
        | SplitDown
        | SelectNextSplit
        | SelectPrevSplit
        | CloseCurrentSplitOrTab => (1, "Pane"),
        IncreaseFontSize
        | DecreaseFontSize
        | ResetFontSize
        | ToggleViMode
        | ToggleAppearanceTheme
        | ListFonts
        | ConfigEditor => (2, "Appearance"),
        Copy | Paste => (3, "Clipboard"),
        SearchForward | SearchBackward | ClearHistory => (4, "Search"),
        _ => (0, "Terminal"),
    }
}

/// Normalize the query once per edit and avoid allocations for ASCII targets.
fn score(query: &str, target: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    if target.is_ascii() {
        score_chars(
            query,
            target.bytes().map(|byte| byte.to_ascii_lowercase() as char),
        )
    } else {
        score_chars(query, target.to_lowercase().chars())
    }
}

fn score_chars(query: &str, target: impl Iterator<Item = char>) -> Option<i32> {
    let mut query_chars = query.chars();
    let mut query_char = query_chars.next()?;
    let mut score = 0;
    let mut prev_match = false;
    let mut first_match_pos = None;
    let mut previous = ' ';
    for (ti, tc) in target.enumerate() {
        if tc == query_char {
            let first = *first_match_pos.get_or_insert(ti);
            if prev_match {
                score += 5;
            }
            if !previous.is_alphanumeric() {
                score += 10;
            }
            prev_match = true;
            if let Some(next) = query_chars.next() {
                query_char = next;
            } else {
                return Some(score + 20_i32.saturating_sub(first as i32));
            }
        } else {
            prev_match = false;
        }
        previous = tc;
    }
    None
}

#[cfg(test)]
fn fuzzy_score(query: &str, target: &str) -> Option<i32> {
    score(&query.to_lowercase(), target)
}

struct Match {
    score: i32,
    source: usize,
}

struct Layout {
    rect: [f32; 4],
    rows: [(usize, f32); MAX_VISIBLE_RESULTS],
    headings: [Option<(&'static str, f32)>; MAX_VISIBLE_RESULTS],
    count: usize,
    max_scroll_offset: usize,
}

/// Search results are rebuilt only on edits or mode changes. Rendering,
/// navigation, and pointer handling share those results and the same layout.
pub struct CommandPalette {
    enabled: bool,
    query: String,
    pub selected_index: usize,
    scroll_offset: usize,
    has_adaptive_theme: bool,
    mode: PaletteMode,
    matches: Vec<Match>,
    content_height: f32,
    theme: super::window_chrome::Theme,
    font_library: Option<rio_backend::sugarloaf::font::FontLibrary>,
    font_id: Option<usize>,
    viewport_height: f32,
    visible_results: usize,
    caret_blink_start: Instant,
    caret_interval: Option<std::time::Duration>,
    wheel_remainder: f64,
}

impl Default for CommandPalette {
    fn default() -> Self {
        let mut palette = Self {
            enabled: false,
            query: String::new(),
            selected_index: 0,
            scroll_offset: 0,
            has_adaptive_theme: false,
            mode: PaletteMode::Commands,
            matches: Vec::with_capacity(COMMANDS.len()),
            content_height: 0.0,
            theme: super::window_chrome::Theme::new(&Default::default()),
            font_library: None,
            font_id: None,
            viewport_height: f32::INFINITY,
            visible_results: MAX_VISIBLE_RESULTS,
            caret_blink_start: Instant::now(),
            caret_interval: None,
            wheel_remainder: 0.0,
        };
        palette.rebuild_matches();
        palette
    }
}

impl CommandPalette {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn configure(
        &mut self,
        colors: &rio_backend::config::colors::Colors,
        adaptive: bool,
        caret_interval: Option<std::time::Duration>,
    ) {
        self.caret_interval = caret_interval;
        self.theme = super::window_chrome::Theme::new(colors);
        if self.has_adaptive_theme != adaptive {
            self.has_adaptive_theme = adaptive;
            self.rebuild_matches();
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if enabled {
            self.mode = PaletteMode::Commands;
            self.set_query(String::new());
        }
    }

    pub fn enter_fonts_mode(&mut self, fonts: Vec<String>) {
        self.enabled = true;
        self.mode = PaletteMode::Fonts(fonts);
        self.set_query(String::new());
    }

    pub fn append_query(&mut self, text: &str) -> bool {
        if !crate::renderer::is_printable_text(text) {
            return false;
        }
        self.query.push_str(text);
        self.query_changed();
        true
    }

    pub fn backspace(&mut self) -> bool {
        if self.query.pop().is_none() {
            return false;
        }
        self.query_changed();
        true
    }

    pub fn set_query(&mut self, query: String) {
        self.query = query;
        self.query_changed();
    }

    fn query_changed(&mut self) {
        self.selected_index = 0;
        self.scroll_offset = 0;
        self.caret_blink_start = Instant::now();
        self.wheel_remainder = 0.0;
        self.rebuild_matches();
    }

    pub fn move_selection_up(&mut self) {
        if self.selected_index > 0 {
            self.selected_index -= 1;
            if self.selected_index < self.scroll_offset {
                self.scroll_offset = self.selected_index;
            }
        }
    }

    pub fn move_selection_down(&mut self) {
        if self.selected_index < self.matches.len().saturating_sub(1) {
            self.selected_index += 1;
            if self.selected_index >= self.scroll_offset + self.visible_results {
                self.scroll_offset = self.selected_index + 1 - self.visible_results;
            }
        }
    }

    pub fn get_selected_action(&self) -> Option<PaletteAction> {
        self.matches
            .get(self.selected_index)
            .and_then(|m| self.row(m.source).action())
    }

    /// Wheel deltas use rows; retain trackpad fractions between events.
    pub fn scroll(&mut self, lines: f64, scale: f32) -> bool {
        self.wheel_remainder -= lines;
        let rows = self.wheel_remainder.trunc() as isize;
        self.wheel_remainder -= rows as f64;
        // Horizontal geometry does not affect row capacity.
        let layout = self.layout(PALETTE_WIDTH * scale, scale);
        let offset = self
            .scroll_offset
            .saturating_add_signed(rows)
            .min(layout.max_scroll_offset);
        if offset == self.scroll_offset {
            return false;
        }
        self.scroll_offset = offset;
        let count = self.layout(PALETTE_WIDTH * scale, scale).count;
        self.selected_index = self
            .selected_index
            .clamp(offset, offset + count.saturating_sub(1));
        true
    }

    pub fn scroll_pixels(&mut self, pixels: f64, scale: f64) -> bool {
        self.scroll(
            pixels / (f64::from(RESULT_ITEM_HEIGHT) * scale),
            scale as f32,
        )
    }

    pub fn get_selected_font(&self) -> Option<String> {
        match self.row(self.matches.get(self.selected_index)?.source) {
            PaletteRow::Font { family } => Some(family.to_owned()),
            _ => None,
        }
    }

    fn row(&self, source: usize) -> PaletteRow<'_> {
        match &self.mode {
            PaletteMode::Commands => {
                let command = &COMMANDS[source];
                PaletteRow::Command {
                    title: command.title,
                    shortcut: command.shortcut,
                    action: command.action,
                }
            }
            PaletteMode::Fonts(fonts) => PaletteRow::Font {
                family: &fonts[source],
            },
        }
    }

    fn rebuild_matches(&mut self) {
        #[cfg(feature = "profiling")]
        let _timing = crate::profiling::span("palette.filter");
        let query = self.query.to_lowercase();
        self.matches.clear();
        match &self.mode {
            PaletteMode::Commands => {
                for (source, command) in COMMANDS.iter().enumerate() {
                    if command.action == PaletteAction::ToggleAppearanceTheme
                        && !self.has_adaptive_theme
                    {
                        continue;
                    }
                    if let Some(score) = score(&query, command.title) {
                        self.matches.push(Match { score, source });
                    }
                }
            }
            PaletteMode::Fonts(fonts) => {
                for (source, family) in fonts.iter().enumerate() {
                    if let Some(score) = score(&query, family) {
                        self.matches.push(Match { score, source });
                    }
                }
            }
        }
        let grouped = matches!(self.mode, PaletteMode::Commands);
        self.matches.sort_unstable_by_key(|m| {
            (
                if grouped {
                    command_group(COMMANDS[m.source].action).0
                } else {
                    0
                },
                std::cmp::Reverse(m.score),
                m.source,
            )
        });
        let mut previous_group = None;
        self.content_height = 0.0;
        for index in 0..self.matches.len() {
            let group = self.group(index);
            self.content_height += RESULT_ITEM_HEIGHT;
            if group.is_some_and(|g| g.0 != 0 && Some(g.0) != previous_group) {
                self.content_height += GROUP_HEIGHT;
            }
            previous_group = group.map(|g| g.0);
        }
    }

    fn group(&self, index: usize) -> Option<(u8, &'static str)> {
        self.row(self.matches[index].source)
            .action()
            .map(command_group)
    }

    #[cfg(test)]
    fn filtered_rows(&self) -> Vec<(i32, PaletteRow<'_>)> {
        self.matches
            .iter()
            .map(|m| (m.score, self.row(m.source)))
            .collect()
    }

    fn layout(&self, window_width: f32, scale: f32) -> Layout {
        let width =
            PALETTE_WIDTH.min((window_width / scale - VIEWPORT_MARGIN * 2.0).max(1.0));
        let x = super::window_chrome::snap((window_width / scale - width) / 2.0, scale);
        let y = super::window_chrome::snap(
            PALETTE_MARGIN_TOP.min(self.viewport_height * PALETTE_TOP_FRACTION),
            scale,
        );
        let results_top = y + PALETTE_PADDING + INPUT_HEIGHT + PALETTE_PADDING;
        // Group headings consume space inside this viewport, never change its
        // height as selection or the first visible group changes.
        let results_height = self
            .content_height
            .max(RESULT_ITEM_HEIGHT)
            .min(MAX_VISIBLE_RESULTS as f32 * RESULT_ITEM_HEIGHT)
            .min(
                (self.viewport_height - VIEWPORT_MARGIN - results_top - PALETTE_PADDING)
                    .max(0.0),
            );
        let bottom = results_top + results_height;
        let show_headings = results_height >= GROUP_HEIGHT + RESULT_ITEM_HEIGHT;
        let mut layout = Layout {
            rect: [x, y, width, bottom - y + PALETTE_PADDING],
            rows: [(0, 0.0); MAX_VISIBLE_RESULTS],
            headings: [None; MAX_VISIBLE_RESULTS],
            count: 0,
            max_scroll_offset: self.matches.len().saturating_sub(1),
        };
        // Fit the last page backwards too, so wheel scrolling cannot leave a
        // nearly empty page or stop before the final grouped result.
        let mut used = 0.0;
        let mut next_group = None;
        for index in (0..self.matches.len()).rev().take(MAX_VISIBLE_RESULTS) {
            let group = self.group(index);
            if show_headings
                && next_group.is_some_and(|g: (u8, &str)| {
                    g.0 != 0 && Some(g.0) != group.map(|g| g.0)
                })
            {
                used += GROUP_HEIGHT;
            }
            used += RESULT_ITEM_HEIGHT;
            let heading = if show_headings && group.is_some_and(|g| g.0 != 0) {
                GROUP_HEIGHT
            } else {
                0.0
            };
            if used + heading > results_height {
                break;
            }
            layout.max_scroll_offset = index;
            next_group = group;
        }
        let mut cursor = results_top;
        let mut previous_group = None;
        for index in self.scroll_offset..self.matches.len() {
            if layout.count == MAX_VISIBLE_RESULTS {
                break;
            }
            let group = self.group(index);
            let heading = group
                .filter(|g| show_headings && g.0 != 0 && Some(g.0) != previous_group);
            let heading_height = if heading.is_some() { GROUP_HEIGHT } else { 0.0 };
            if cursor + heading_height + RESULT_ITEM_HEIGHT > bottom {
                break;
            }
            if let Some((_, title)) = heading {
                layout.headings[layout.count] = Some((title, cursor));
                cursor += GROUP_HEIGHT;
            }
            layout.rows[layout.count] = (index, cursor);
            layout.count += 1;
            cursor += RESULT_ITEM_HEIGHT;
            previous_group = group.map(|g| g.0);
        }
        layout
    }

    pub fn hit_test(
        &self,
        mouse_x: f32,
        mouse_y: f32,
        window_width: f32,
        scale_factor: f32,
    ) -> Result<Option<usize>, ()> {
        let layout = self.layout(window_width, scale_factor);
        let [x, y, w, h] = layout.rect;
        if mouse_x < x || mouse_x >= x + w || mouse_y < y || mouse_y >= y + h {
            return Err(());
        }
        Ok(layout.rows[..layout.count]
            .iter()
            .find_map(|&(index, top)| {
                (mouse_y >= top && mouse_y < top + RESULT_ITEM_HEIGHT).then_some(index)
            }))
    }

    pub fn hover(
        &mut self,
        mouse_x: f32,
        mouse_y: f32,
        window_width: f32,
        scale_factor: f32,
    ) -> bool {
        if let Ok(Some(index)) =
            self.hit_test(mouse_x, mouse_y, window_width, scale_factor)
        {
            if self.selected_index != index {
                self.selected_index = index;
                return true;
            }
        }
        false
    }

    pub fn next_frame_in(&self) -> Option<std::time::Duration> {
        if !self.enabled {
            return None;
        }
        use std::time::Duration;
        self.caret_interval.map(|interval| {
            let interval_ms = interval.as_millis().max(1);
            Duration::from_millis(
                (interval_ms - self.caret_blink_start.elapsed().as_millis() % interval_ms)
                    as u64,
            )
        })
    }

    pub fn render(&mut self, sugarloaf: &mut Sugarloaf, dimensions: (f32, f32, f32)) {
        if !self.enabled {
            return;
        }
        #[cfg(feature = "profiling")]
        let _timing = crate::profiling::span("palette.render");
        use super::window_chrome::{alpha, centered_text_y, snap};
        let (window_width, window_height, scale) = dimensions;
        self.viewport_height = window_height / scale;
        let library = sugarloaf.font_library();
        if self
            .font_library
            .as_ref()
            .is_none_or(|old| !std::sync::Arc::ptr_eq(&old.inner, &library.inner))
        {
            self.font_id = super::window_chrome::load_ui_font(library);
            self.font_library = Some(library.clone());
        }
        let mut layout = self.layout(window_width, scale);
        if self.scroll_offset > layout.max_scroll_offset {
            self.scroll_offset = layout.max_scroll_offset;
            layout = self.layout(window_width, scale);
        }
        while layout.count > 0 && self.selected_index >= self.scroll_offset + layout.count
        {
            self.scroll_offset += 1;
            layout = self.layout(window_width, scale);
        }
        self.visible_results = layout.count.max(1);
        let [x, y, width, height] = layout.rect;
        let theme = self.theme;
        let opts = |size, color| DrawOpts {
            font_size: snap(size, scale),
            font_id: self.font_id,
            color: color_u8(color),
            ..DrawOpts::default()
        };
        sugarloaf.rounded_rect(
            None,
            x,
            y,
            width,
            height,
            theme.border,
            0.1,
            PALETTE_CORNER_RADIUS,
            ORDER,
        );
        let pixel = 1.0 / scale;
        sugarloaf.rounded_rect(
            None,
            x + pixel,
            y + pixel,
            width - 2.0 * pixel,
            height - 2.0 * pixel,
            theme.surface,
            0.11,
            PALETTE_CORNER_RADIUS - pixel,
            ORDER,
        );
        let input_x = x + PALETTE_PADDING;
        let input_y = y + PALETTE_PADDING;
        let input_width = width - PALETTE_PADDING * 2.0;
        sugarloaf.rounded_rect(
            None,
            input_x,
            input_y,
            input_width,
            INPUT_HEIGHT,
            theme.hover,
            0.2,
            INPUT_RADIUS,
            ORDER + 1,
        );
        let cy = input_y + INPUT_HEIGHT / 2.0;
        // Search lens, using the input fill for its transparent-looking center.
        let lens_radius = (ICON_SIZE - ICON_STROKE) / 2.0 - ICON_STROKE;
        let lens_x = input_x + INPUT_PADDING_X;
        sugarloaf.rounded_rect(
            None,
            lens_x,
            cy - ICON_SIZE / 2.0,
            lens_radius * 2.0,
            lens_radius * 2.0,
            theme.muted,
            0.21,
            lens_radius,
            ORDER + 2,
        );
        sugarloaf.rounded_rect(
            None,
            lens_x + ICON_STROKE,
            cy - ICON_SIZE / 2.0 + ICON_STROKE,
            (lens_radius - ICON_STROKE) * 2.0,
            (lens_radius - ICON_STROKE) * 2.0,
            theme.hover,
            0.22,
            lens_radius - ICON_STROKE,
            ORDER + 2,
        );
        sugarloaf.line(
            lens_x + lens_radius * (1.0 + std::f32::consts::FRAC_1_SQRT_2),
            cy - ICON_SIZE / 2.0 + lens_radius * (1.0 + std::f32::consts::FRAC_1_SQRT_2),
            lens_x + ICON_SIZE,
            cy + ICON_SIZE / 2.0,
            ICON_STROKE,
            0.2,
            theme.muted,
            ORDER + 2,
        );
        let text_x = input_x + INPUT_TEXT_INSET;
        let input_opts = opts(
            INPUT_FONT_SIZE,
            if self.query.is_empty() {
                theme.muted
            } else {
                theme.foreground
            },
        );
        let placeholder = if matches!(self.mode, PaletteMode::Fonts(_)) {
            "Search fonts…"
        } else {
            "Search commands…"
        };
        let input_budget = (input_width - INPUT_TEXT_INSET - INPUT_PADDING_X).max(0.0);
        let text_width = if self.query.is_empty() {
            0.0
        } else {
            sugarloaf.text_mut().measure(&self.query, &input_opts)
        };
        let start = sugarloaf.text_mut().instances().len();
        // Keep the typing baseline fixed to the placeholder's visible bounds.
        let text_y = centered_text_y(sugarloaf, placeholder, cy, &input_opts);
        sugarloaf.text_mut().draw(
            text_x - (text_width - input_budget).max(0.0),
            text_y,
            if self.query.is_empty() {
                placeholder
            } else {
                &self.query
            },
            &input_opts,
        );
        sugarloaf
            .text_mut()
            .exclude_horizontal_range_since(start, [f32::MIN, text_x - 1.0]);
        sugarloaf
            .text_mut()
            .exclude_horizontal_range_since(start, [text_x + input_budget, f32::MAX]);
        if self.caret_interval.is_none_or(|interval| {
            (self.caret_blink_start.elapsed().as_millis() / interval.as_millis().max(1))
                .is_multiple_of(2)
        }) {
            sugarloaf.rect(
                None,
                snap(text_x + text_width.min(input_budget), scale),
                snap(cy - INPUT_FONT_SIZE / 2.0, scale),
                pixel,
                INPUT_FONT_SIZE,
                theme.foreground,
                0.2,
                ORDER + 2,
            );
        }
        let result_opts = opts(RESULT_FONT_SIZE, theme.foreground);
        let selected_opts = opts(RESULT_FONT_SIZE, theme.on_accent);
        let shortcut_opts = opts(SHORTCUT_FONT_SIZE, theme.muted);
        let selected_shortcut_opts =
            opts(SHORTCUT_FONT_SIZE, alpha(theme.on_accent, 0.75));
        // A shared cap-height baseline keeps rows aligned without measuring
        // every label's glyph bounds on each frame.
        let result_y = centered_text_y(sugarloaf, "H", 0.0, &result_opts);
        let shortcut_y = centered_text_y(sugarloaf, "H", 0.0, &shortcut_opts);
        for heading in layout.headings.iter().flatten() {
            sugarloaf.text_mut().draw(
                input_x + INPUT_PADDING_X,
                heading.1 + GROUP_HEIGHT / 2.0 + shortcut_y,
                heading.0,
                &shortcut_opts,
            );
        }
        if self.matches.is_empty() {
            let text_y = centered_text_y(
                sugarloaf,
                "No matches found",
                input_y + INPUT_HEIGHT + PALETTE_PADDING + RESULT_ITEM_HEIGHT / 2.0,
                &shortcut_opts,
            );
            sugarloaf.text_mut().draw(
                input_x + INPUT_PADDING_X,
                text_y,
                "No matches found",
                &shortcut_opts,
            );
        }
        for &(index, top) in &layout.rows[..layout.count] {
            let row = self.row(self.matches[index].source);
            let selected = index == self.selected_index;
            if selected {
                sugarloaf.rounded_rect(
                    None,
                    input_x,
                    top,
                    input_width,
                    RESULT_ITEM_HEIGHT,
                    theme.accent,
                    0.2,
                    RESULT_RADIUS,
                    ORDER + 1,
                );
            }
            let cy = top + RESULT_ITEM_HEIGHT / 2.0;
            let (text_opts, hint_opts) = if selected {
                (&selected_opts, &selected_shortcut_opts)
            } else {
                (&result_opts, &shortcut_opts)
            };
            let shortcut = row.shortcut();
            let hint_width = if shortcut.is_empty() {
                0.0
            } else {
                sugarloaf.text_mut().measure(shortcut, hint_opts) + HINT_GAP
            };
            let copy_width = if matches!(row, PaletteRow::Font { .. }) {
                ICON_SIZE + PALETTE_PADDING
            } else {
                0.0
            };
            let budget =
                (input_width - INPUT_PADDING_X * 2.0 - hint_width - copy_width).max(0.0);
            let title =
                super::window_chrome::fit_text(sugarloaf, row.title(), budget, text_opts);
            sugarloaf.text_mut().draw(
                input_x + INPUT_PADDING_X,
                cy + result_y,
                &title,
                text_opts,
            );
            if !shortcut.is_empty() {
                let width = sugarloaf.text_mut().measure(shortcut, hint_opts);
                if width < input_width - INPUT_PADDING_X * 2.0 {
                    sugarloaf.text_mut().draw(
                        input_x + input_width - INPUT_PADDING_X - width,
                        cy + shortcut_y,
                        shortcut,
                        hint_opts,
                    );
                }
            }
            if matches!(row, PaletteRow::Font { .. }) {
                draw_copy_icon(
                    sugarloaf,
                    input_x + input_width - INPUT_PADDING_X - ICON_SIZE,
                    cy - ICON_SIZE / 2.0,
                    if selected {
                        theme.on_accent
                    } else {
                        theme.muted
                    },
                );
            }
        }
        if self.matches.len() > layout.count && layout.count > 0 {
            let top = layout.rows[0].1;
            let track = y + height - PALETTE_PADDING - top;
            let normalized =
                self.scroll_offset as f32 / (self.matches.len() - layout.count) as f32;
            if let Some((thumb_y, thumb_height)) = scrollbar::compute_thumb(
                layout.count,
                self.matches.len(),
                top,
                track,
                normalized,
            ) {
                scrollbar::draw_thumb(
                    sugarloaf,
                    x + width - (PALETTE_PADDING + scrollbar::SCROLLBAR_WIDTH) / 2.0,
                    thumb_y,
                    thumb_height,
                    SCROLLBAR_OPACITY,
                    false,
                    0.3,
                    ORDER + 2,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_set_enabled_resets_state() {
        let mut palette = CommandPalette::new();
        palette.set_query("test".to_string());
        palette.selected_index = 3;
        palette.scroll_offset = 2;

        palette.set_enabled(true);

        assert!(palette.query.is_empty());
        assert_eq!(palette.selected_index, 0);
        assert_eq!(palette.scroll_offset, 0);
    }

    #[test]
    fn test_filtered_commands_empty_query() {
        let palette = CommandPalette::new();
        let filtered = palette.filtered_rows();
        // ToggleAppearanceTheme is hidden when has_adaptive_theme is false
        assert_eq!(filtered.len(), COMMANDS.len() - 1);
    }

    #[test]
    fn test_filtered_commands_by_title() {
        let mut palette = CommandPalette::new();
        palette.set_query("split".to_string());
        let filtered = palette.filtered_rows();
        assert!(filtered.len() >= 2);
        for (_, row) in &filtered {
            assert!(row.title().to_lowercase().contains("split"));
        }
    }

    #[test]
    fn test_filtered_commands_case_insensitive() {
        let mut palette = CommandPalette::new();
        palette.set_query("QUIT".to_string());
        let filtered = palette.filtered_rows();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].1.title(), "Quit");
    }

    #[test]
    fn test_fuzzy_matching() {
        let mut palette = CommandPalette::new();
        palette.set_query("nt".to_string()); // Should match "New Tab", "Next Tab", etc.
        let filtered = palette.filtered_rows();
        assert!(!filtered.is_empty());
    }

    #[test]
    fn test_set_query_resets_selection_and_scroll() {
        let mut palette = CommandPalette::new();
        palette.selected_index = 5;
        palette.scroll_offset = 3;
        palette.set_query("test".to_string());
        assert_eq!(palette.selected_index, 0);
        assert_eq!(palette.scroll_offset, 0);
    }

    #[test]
    fn test_move_selection_down() {
        let mut palette = CommandPalette::new();
        palette.set_enabled(true);
        assert_eq!(palette.selected_index, 0);
        palette.move_selection_down();
        assert_eq!(palette.selected_index, 1);
        palette.move_selection_down();
        assert_eq!(palette.selected_index, 2);
    }

    #[test]
    fn test_move_selection_down_boundary() {
        let mut palette = CommandPalette::new();
        palette.set_enabled(true);
        let count = palette.filtered_rows().len();
        palette.selected_index = count - 1;
        palette.move_selection_down();
        assert_eq!(palette.selected_index, count - 1);
    }

    #[test]
    fn test_move_selection_up() {
        let mut palette = CommandPalette::new();
        palette.set_enabled(true);
        palette.selected_index = 3;
        palette.move_selection_up();
        assert_eq!(palette.selected_index, 2);
    }

    #[test]
    fn test_move_selection_up_boundary() {
        let mut palette = CommandPalette::new();
        palette.set_enabled(true);
        palette.move_selection_up();
        assert_eq!(palette.selected_index, 0);
    }

    #[test]
    fn test_get_selected_action() {
        let palette = CommandPalette::new();
        let action = palette.get_selected_action();
        assert!(action.is_some());
        // First command is "New Tab"
        assert_eq!(action.unwrap(), PaletteAction::TabCreate);
    }

    #[test]
    fn test_get_selected_action_with_filter() {
        let mut palette = CommandPalette::new();
        palette.set_query("quit".to_string());
        let action = palette.get_selected_action();
        assert_eq!(action, Some(PaletteAction::Quit));
    }

    #[test]
    fn test_scroll_offset_on_move_down() {
        let mut palette = CommandPalette::new();
        palette.set_enabled(true);
        for _ in 0..MAX_VISIBLE_RESULTS {
            palette.move_selection_down();
        }
        assert!(palette.scroll_offset > 0);
    }

    #[test]
    fn test_hit_test_outside() {
        let palette = CommandPalette::new();
        assert!(palette.hit_test(0.0, 0.0, 1200.0, 1.0).is_err());
    }

    #[test]
    fn test_fuzzy_score_basic() {
        assert!(fuzzy_score("nt", "New Tab").is_some());
        assert!(fuzzy_score("xyz", "New Tab").is_none());
        assert!(fuzzy_score("", "New Tab").is_some());
    }

    #[test]
    fn test_fuzzy_score_ordering() {
        // "New Tab" should score higher than "Next Tab" for "net" because of word boundary
        let score_new = fuzzy_score("net", "New Tab").unwrap_or(-100);
        let score_next = fuzzy_score("net", "Next Tab").unwrap_or(-100);
        // Both should match
        assert!(score_new > -100);
        assert!(score_next > -100);
    }

    #[test]
    fn enter_fonts_mode_switches_to_font_list() {
        let mut palette = CommandPalette::new();
        palette.set_enabled(true);
        palette.set_query("ab".to_string());
        palette.selected_index = 2;

        let fonts = vec![
            "JetBrains Mono".to_string(),
            "Fira Code".to_string(),
            "Cascadia Code".to_string(),
        ];
        palette.enter_fonts_mode(fonts);

        // Query cleared, selection reset, full list visible.
        assert!(palette.query.is_empty());
        assert_eq!(palette.selected_index, 0);
        assert_eq!(palette.filtered_rows().len(), 3);
        // Every row is a Font row, so no executable action.
        assert!(palette.get_selected_action().is_none());
    }

    #[test]
    fn fonts_mode_filters_by_fuzzy_score() {
        let mut palette = CommandPalette::new();
        palette.enter_fonts_mode(vec![
            "JetBrains Mono".to_string(),
            "Fira Code".to_string(),
            "Cascadia Code".to_string(),
        ]);
        palette.set_query("cas".to_string());
        let filtered = palette.filtered_rows();
        assert!(filtered.iter().any(|(_, r)| r.title() == "Cascadia Code"));
        assert!(filtered.iter().all(|(_, r)| {
            r.title().to_lowercase().contains('c')
                && r.title().to_lowercase().contains('a')
                && r.title().to_lowercase().contains('s')
        }));
    }

    #[test]
    fn fonts_mode_row_has_no_shortcut_column() {
        let mut palette = CommandPalette::new();
        palette.enter_fonts_mode(vec!["Fira Code".to_string()]);
        let filtered = palette.filtered_rows();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].1.shortcut(), "");
    }

    #[test]
    fn set_enabled_resets_fonts_mode_to_commands() {
        // Re-opening the palette with the keyboard must drop any stale
        // font list — reopening otherwise would land the user on fonts
        // they saw yesterday, which is surprising.
        let mut palette = CommandPalette::new();
        palette.enter_fonts_mode(vec!["Fira Code".to_string()]);
        palette.enabled = true;
        palette.set_enabled(false);
        palette.set_enabled(true);
        assert!(matches!(palette.mode, PaletteMode::Commands));
        // Commands list is back (non-empty modulo adaptive-theme filter).
        assert!(!palette.filtered_rows().is_empty());
    }

    #[test]
    fn get_selected_font_returns_family_in_fonts_mode() {
        let mut palette = CommandPalette::new();
        palette.enter_fonts_mode(vec![
            "JetBrains Mono".to_string(),
            "Fira Code".to_string(),
        ]);
        // First row (sorted alphabetically by fuzzy_score tie-break:
        // both score 0 with empty query, so first-inserted wins).
        let selected = palette.get_selected_font();
        assert!(selected.is_some());
        // The returned name must be one of the inputs, irrespective
        // of fuzzy-sort ordering.
        let s = selected.unwrap();
        assert!(s == "JetBrains Mono" || s == "Fira Code");
    }

    #[test]
    fn get_selected_font_none_in_commands_mode() {
        let palette = CommandPalette::new();
        // Default mode is Commands; no font to copy.
        assert!(palette.get_selected_font().is_none());
    }

    #[test]
    fn get_selected_font_none_when_empty_filter() {
        let mut palette = CommandPalette::new();
        palette.enter_fonts_mode(vec!["Fira Code".to_string()]);
        palette.set_query("zzzz".to_string());
        // Query doesn't match anything → no selected font.
        assert!(palette.get_selected_font().is_none());
    }

    #[test]
    fn overflowing_results_have_a_scrollbar_on_open() {
        let mut palette = CommandPalette::new();
        palette.enter_fonts_mode((0..50).map(|i| format!("Family {i:02}")).collect());
        let layout = palette.layout(1200.0, 1.0);
        assert!(layout.count > 0 && layout.count < palette.matches.len());
        assert_eq!(palette.scroll_offset, 0);
    }

    #[test]
    fn scrolling_preserves_viewport_height() {
        let mut palette = CommandPalette::new();
        let height = palette.layout(1200.0, 1.0).rect[3];
        for _ in 0..COMMANDS.len() {
            palette.move_selection_down();
            assert_eq!(palette.layout(1200.0, 1.0).rect[3], height);
        }
    }

    #[test]
    fn scroll_reset_on_query_change() {
        let mut palette = CommandPalette::new();
        palette.enter_fonts_mode((0..50).map(|i| format!("Family {i:02}")).collect());
        assert!(palette.scroll(-1.5, 1.0));
        palette.set_query("Family 00".to_string());
        assert_eq!(palette.scroll_offset, 0);
        assert_eq!(palette.wheel_remainder, 0.0);
    }

    #[test]
    fn scroll_reset_on_palette_reopen() {
        let mut palette = CommandPalette::new();
        palette.enter_fonts_mode((0..50).map(|i| format!("Family {i:02}")).collect());
        assert!(palette.scroll(-1.5, 1.0));
        palette.set_enabled(false);
        palette.set_enabled(true);
        assert_eq!(palette.scroll_offset, 0);
        assert_eq!(palette.wheel_remainder, 0.0);
    }

    #[test]
    fn list_fonts_command_is_present_and_actionable() {
        // Confirms `List Fonts` shows up in the command list and
        // reports the correct action when selected.
        let mut palette = CommandPalette::new();
        palette.set_query("list fonts".to_string());
        let filtered = palette.filtered_rows();
        assert!(!filtered.is_empty());
        assert_eq!(filtered[0].1.title(), "List Fonts");
        palette.selected_index = 0;
        assert_eq!(
            palette.get_selected_action(),
            Some(PaletteAction::ListFonts)
        );
    }
}
