/*
    Crosswords -> Rio's grid manager

    |----------------------------------|
    |-$-bash:-echo-1-------------------|
    |-1--------------------------------|
    |----------------------------------|
    |----------------------------------|
    |----------------------------------|
    |----------------------------------|
    |----------------------------------|

// Crosswords (mod.rs) was originally taken from https://github.com/alacritty/alacritty/blob/e35e5ad14fce8456afdd89f2b392b9924bb27471/alacritty_terminal/src/term/mod.rs
// which is licensed under Apache 2.0 license.
*/

pub mod attr;
pub mod formatter;
pub mod grid;
pub mod pos;
pub mod search;
pub mod snapshot;
pub mod square;
pub mod style;
pub mod vi_mode;

use crate::ansi::graphics::Graphics;
use crate::ansi::graphics::KittyPlacement;
use crate::ansi::graphics::UpdateQueues;
use crate::ansi::mode::NamedMode;
use crate::ansi::mode::NamedPrivateMode;
use crate::ansi::mode::PrivateMode;
use crate::ansi::sixel;
use crate::ansi::{
    mode::Mode as AnsiMode, ClearMode, CursorShape, KeyboardModes,
    KeyboardModesApplyBehavior, LineClearMode, TabulationClearMode,
};
use crate::clipboard::ClipboardType;
use crate::config::colors::{self, ColorRgb};
use crate::crosswords::colors::term::TermColors;
use crate::crosswords::grid::{Dimensions, Grid, GridSquare, Scroll};
use crate::crosswords::square::{CellFlags, Wide};
use crate::event::WindowId;
use crate::event::{EventListener, RioEvent, TerminalDamage};
use crate::performer::handler::Handler;
use crate::performer::parser::Params;
use crate::selection::{Selection, SelectionRange, SelectionType};
use crate::simd_utf8;
use attr::*;
use bitflags::bitflags;
use grid::row::Row;
use pos::{
    Boundary, CharsetIndex, Column, Cursor, CursorState, Direction, Line, Pos, Side,
};
use rio_graphics::{GraphicData, MAX_GRAPHIC_DIMENSIONS};
use square::{Hyperlink, LineLength, Square};
use std::mem;
use std::ops::{Index, IndexMut, Range};
use std::option::Option;
use std::ptr;
use std::sync::Arc;
use tracing::{debug, info, trace, warn};
use vi_mode::{ViModeCursor, ViMotion};

pub type NamedColor = colors::NamedColor;

pub const MIN_COLUMNS: usize = 2;
pub const MIN_LINES: usize = 1;

// Max. number of graphics stored in a single cell.
// const MAX_GRAPHICS_PER_CELL: usize = 20;

bitflags! {
     #[derive(Debug, Copy, Clone)]
     pub struct Mode: u32 {
        const NONE                    = 0;
        const SHOW_CURSOR             = 1;
        const APP_CURSOR              = 1 << 1;
        const APP_KEYPAD              = 1 << 2;
        const MOUSE_REPORT_CLICK      = 1 << 3;
        const BRACKETED_PASTE         = 1 << 4;
        const SGR_MOUSE               = 1 << 5;
        const MOUSE_MOTION            = 1 << 6;
        const LINE_WRAP               = 1 << 7;
        const LINE_FEED_NEW_LINE      = 1 << 8;
        const ORIGIN                  = 1 << 9;
        const INSERT                  = 1 << 10;
        const FOCUS_IN_OUT            = 1 << 11;
        const ALT_SCREEN              = 1 << 12;
        const MOUSE_DRAG              = 1 << 13;
        const UTF8_MOUSE              = 1 << 14;
        const ALTERNATE_SCROLL        = 1 << 15;
        const VI                      = 1 << 16;
        const URGENCY_HINTS           = 1 << 17;
        const DISAMBIGUATE_ESC_CODES  = 1 << 18;
        const REPORT_EVENT_TYPES      = 1 << 19;
        const REPORT_ALTERNATE_KEYS   = 1 << 20;
        const REPORT_ALL_KEYS_AS_ESC  = 1 << 21;
        const REPORT_ASSOCIATED_TEXT  = 1 << 22;
        const MOUSE_REPORT_X10        = 1 << 23;
        /// DEC private mode 2027: grapheme cluster processing. Shared
        /// across main/alt screens (the mode lives on the terminal,
        /// not the grid), matching contour.
        const GRAPHEME_CLUSTER        = 1 << 24;
        const KITTY_PASTE_EVENTS       = 1 << 25;
        const MOUSE_MODE = Self::MOUSE_REPORT_CLICK.bits() | Self::MOUSE_MOTION.bits() | Self::MOUSE_DRAG.bits() | Self::MOUSE_REPORT_X10.bits();
        const KITTY_KEYBOARD_PROTOCOL = Self::DISAMBIGUATE_ESC_CODES.bits()
                                      | Self::REPORT_EVENT_TYPES.bits()
                                      | Self::REPORT_ALTERNATE_KEYS.bits()
                                      | Self::REPORT_ALL_KEYS_AS_ESC.bits()
                                      | Self::REPORT_ASSOCIATED_TEXT.bits();
        const ANY                    = u32::MAX;

        const SIXEL_DISPLAY             = 1 << 28;
        const SIXEL_PRIV_PALETTE        = 1 << 29;
        const SIXEL_CURSOR_TO_THE_RIGHT = 1 << 31;
    }
}

/// The state of the [`Mode`] and [`PrivateMode`].
#[repr(u8)]
#[derive(Debug, Clone, Copy)]
enum ModeState {
    /// The mode is not supported.
    NotSupported = 0,
    /// The mode is currently set.
    Set = 1,
    /// The mode is currently not set.
    Reset = 2,
}

impl From<bool> for ModeState {
    fn from(value: bool) -> Self {
        if value {
            Self::Set
        } else {
            Self::Reset
        }
    }
}

impl Default for Mode {
    fn default() -> Mode {
        Mode::SHOW_CURSOR
            | Mode::LINE_WRAP
            | Mode::ALTERNATE_SCROLL
            | Mode::URGENCY_HINTS
            | Mode::SIXEL_PRIV_PALETTE
    }
}

impl From<KeyboardModes> for Mode {
    fn from(value: KeyboardModes) -> Self {
        let mut mode = Self::empty();
        mode.set(
            Mode::DISAMBIGUATE_ESC_CODES,
            value.contains(KeyboardModes::DISAMBIGUATE_ESC_CODES),
        );
        mode.set(
            Mode::REPORT_EVENT_TYPES,
            value.contains(KeyboardModes::REPORT_EVENT_TYPES),
        );
        mode.set(
            Mode::REPORT_ALTERNATE_KEYS,
            value.contains(KeyboardModes::REPORT_ALTERNATE_KEYS),
        );
        mode.set(
            Mode::REPORT_ALL_KEYS_AS_ESC,
            value.contains(KeyboardModes::REPORT_ALL_KEYS_AS_ESC),
        );
        mode.set(
            Mode::REPORT_ASSOCIATED_TEXT,
            value.contains(KeyboardModes::REPORT_ASSOCIATED_TEXT),
        );
        mode
    }
}

/// Terminal damage information collected since the last [`Term::reset_damage`] call.
#[derive(Debug)]
pub enum TermDamage<'a> {
    /// The entire terminal is damaged.
    Full,

    /// Iterator over damaged lines in the terminal.
    Partial(TermDamageIterator<'a>),
}

/// Iterator over the terminal's viewport damaged lines.
#[derive(Clone, Debug)]
pub struct TermDamageIterator<'a> {
    line_damage: std::slice::Iter<'a, LineDamage>,
    display_offset: usize,
}

impl<'a> TermDamageIterator<'a> {
    pub fn new(line_damage: &'a [LineDamage], display_offset: usize) -> Self {
        let num_lines = line_damage.len();
        // Filter out invisible damage.
        let line_damage = &line_damage[..num_lines.saturating_sub(display_offset)];
        Self {
            display_offset,
            line_damage: line_damage.iter(),
        }
    }
}

impl Iterator for TermDamageIterator<'_> {
    type Item = LineDamage;

    fn next(&mut self) -> Option<Self::Item> {
        self.line_damage.find_map(|line| {
            line.is_damaged()
                .then_some(LineDamage::new(line.line + self.display_offset, true))
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LineDamage {
    /// Line number.
    pub line: usize,
    /// Whether this line is damaged.
    pub damaged: bool,
}

impl LineDamage {
    #[inline]
    pub fn new(line: usize, damaged: bool) -> Self {
        Self { line, damaged }
    }

    #[inline]
    pub fn undamaged(line: usize) -> Self {
        Self {
            line,
            damaged: false,
        }
    }

    #[inline]
    pub fn reset(&mut self) {
        self.damaged = false;
    }

    #[inline]
    pub fn is_damaged(&self) -> bool {
        self.damaged
    }

    #[inline]
    pub fn mark_damaged(&mut self) {
        self.damaged = true;
    }
}

#[derive(Debug, Clone)]
struct TermDamageState {
    /// Hint whether terminal should be damaged entirely regardless of the actual damage changes.
    full: bool,

    /// Information about damage on terminal lines.
    lines: Vec<LineDamage>,
    /// Bounds of marked lines; len..0 is the empty sentinel.
    dirty_lines: Range<usize>,

    /// Old terminal cursor point.
    last_cursor: Pos,

    /// Last Vi cursor point.
    last_vi_cursor_point: Option<Pos>,
    // Old selection range.
    last_selection: Option<SelectionRange>,
}

impl TermDamageState {
    fn new(num_lines: usize) -> Self {
        let lines = (0..num_lines).map(LineDamage::undamaged).collect();

        Self {
            full: true,
            lines,
            dirty_lines: num_lines..0,
            last_cursor: Default::default(),
            last_vi_cursor_point: Default::default(),
            last_selection: Default::default(),
        }
    }

    #[inline]
    fn resize(&mut self, num_lines: usize) {
        // Reset point, so old cursor won't end up outside of the viewport.
        self.last_cursor = Default::default();
        self.last_vi_cursor_point = None;
        self.last_selection = None;
        self.full = true;

        self.lines.clear();
        self.dirty_lines = num_lines..0;
        // The exact-size iterator lets Vec reserve once and initialize
        // the whole range without a capacity check and length write per row.
        self.lines.extend((0..num_lines).map(LineDamage::undamaged));
    }

    /// Damage a line
    #[inline]
    fn damage_line(&mut self, line: usize) {
        self.lines[line].mark_damaged();
        self.dirty_lines.start = self.dirty_lines.start.min(line);
        self.dirty_lines.end = self.dirty_lines.end.max(line + 1);
    }

    fn damage_selection(&mut self, selection: SelectionRange, display_offset: usize) {
        let display_offset = display_offset as i32;
        let last_visible_line = self.lines.len() as i32 - 1;

        // Don't damage invisible selection.
        if selection.end.row.0 + display_offset < 0
            || selection.start.row.0.abs() < display_offset - last_visible_line
        {
            return;
        };

        let start = std::cmp::max(selection.start.row.0 + display_offset, 0);
        let end = (selection.end.row.0 + display_offset).clamp(0, last_visible_line);
        for line in start as usize..=end as usize {
            self.damage_line(line);
        }
    }

    /// Reset information about terminal damage.
    fn reset(&mut self) {
        self.full = false;
        if !self.dirty_lines.is_empty() {
            self.lines[self.dirty_lines.clone()]
                .iter_mut()
                .for_each(|line| line.reset());
        }
        self.dirty_lines = self.lines.len()..0;
    }
}

#[derive(Debug, Clone)]
struct TabStops {
    tabs: Vec<bool>,
}

/// Default tab interval, corresponding to terminfo `it` value.
const INITIAL_TABSTOPS: usize = 8;

impl TabStops {
    #[inline]
    fn new(columns: usize) -> TabStops {
        TabStops {
            tabs: (0..columns).map(|i| i % INITIAL_TABSTOPS == 0).collect(),
        }
    }

    /// Remove all tabstops.
    #[inline]
    fn clear_all(&mut self) {
        unsafe {
            ptr::write_bytes(self.tabs.as_mut_ptr(), 0, self.tabs.len());
        }
    }

    /// Increase tabstop capacity.
    #[inline]
    fn resize(&mut self, columns: usize) {
        let mut index = self.tabs.len();
        self.tabs.resize_with(columns, || {
            let is_tabstop = index.is_multiple_of(INITIAL_TABSTOPS);
            index += 1;
            is_tabstop
        });
    }
}

impl Index<Column> for TabStops {
    type Output = bool;

    fn index(&self, index: Column) -> &bool {
        &self.tabs[index.0]
    }
}

impl IndexMut<Column> for TabStops {
    fn index_mut(&mut self, index: Column) -> &mut bool {
        self.tabs.index_mut(index.0)
    }
}

/// Terminal version for escape sequence reports.
///
/// This returns the current terminal version as a unique number based on rio's
/// semver version. The different versions are padded to ensure that a higher semver version will
/// always report a higher version number.
fn version_number(mut version: &str) -> usize {
    if let Some(separator) = version.rfind('-') {
        version = &version[..separator];
    }

    let mut version_number = 0;

    let semver_versions = version.split('.');
    for (i, semver_version) in semver_versions.rev().enumerate() {
        let semver_number = semver_version.parse::<usize>().unwrap_or(0);
        version_number += usize::pow(100, i as u32) * semver_number;
    }

    version_number
}

/// True when (`base`, `vs`) appears in Unicode's `emoji-variation-sequences.txt`,
/// i.e. `vs` (U+FE0F or U+FE0E) is defined to have an effect on this base.
/// Equivalent to kitty's `is_emoji_presentation_base` guard — the
/// actual widen/narrow decision is then
/// gated on the current cell's `Wide` state by the callers.
pub(crate) fn vs_is_valid_base(base: char, vs: char) -> bool {
    use rio_grapheme_width::emoji::Presentation;
    let mut buf = [0u8; 8];
    let n1 = base.encode_utf8(&mut buf).len();
    let n2 = vs.encode_utf8(&mut buf[n1..]).len();
    // SAFETY: two valid chars written consecutively remain valid UTF-8.
    let s = unsafe { core::str::from_utf8_unchecked(&buf[..n1 + n2]) };
    // `for_grapheme` returns `Some(explicit)` iff the sequence is in
    // VARIATION_MAP (forked from wezterm's emoji-variation-sequences.txt).
    Presentation::for_grapheme(s).1.is_some()
}

// Max size of the window title stack.
const TITLE_STACK_MAX_DEPTH: usize = 4096;

// Max size of the keyboard modes.
const KEYBOARD_MODE_STACK_MAX_DEPTH: usize = 8;

/// Metadata read by window chrome independently of terminal parsing.
#[derive(Debug, Default)]
pub struct TerminalMetadata {
    pub title: String,
    pub app: Option<String>,
    pub remote: bool,
    pub status: Option<crate::program_status::ProgramStatusRecord>,
    pub status_revision: u64,
}

#[derive(Debug)]
pub struct Crosswords<U>
where
    U: EventListener,
{
    active_charset: CharsetIndex,
    single_shift: Option<CharsetIndex>,
    /// Independent DECRQM latches for modes 47, 1047, 1048 and 1049.
    screen_mode_state: [bool; 4],
    mode: Mode,
    pub vi_mode_cursor: ViModeCursor,
    semantic_escape_chars: String,
    pub grid: Grid<Square>,
    inactive_grid: Grid<Square>,
    scroll_region: Range<Line>,
    tabs: TabStops,
    event_proxy: U,
    pub selection: Option<Selection>,
    pub colors: TermColors,
    pub title: String,
    pub program_status: crate::program_status::ProgramStatusStore,
    pub metadata: Arc<parking_lot::Mutex<TerminalMetadata>>,
    damage: TermDamageState,
    pub graphics: Graphics,
    /// Per-session registry of glyphs registered over Glyph Protocol
    /// (APC `25a1`). `None` until a program in this session actually
    /// uses the protocol, so terminals that never see a Glyph
    /// Protocol message pay zero cost — no Arc allocation, no
    /// per-frame attach call. Lazily initialised by `glyph_register`.
    /// Two tabs can register conflicting glyphs for the same
    /// codepoint; each `Crosswords` owns its own registry once
    /// initialised.
    #[cfg(feature = "graphics")]
    pub glyph_registry: Option<rio_graphics::glyph::glyph_registry::GlyphRegistry>,
    pub cursor_shape: CursorShape,
    pub default_cursor_shape: CursorShape,
    pub blinking_cursor: bool,
    pub window_id: WindowId,
    pub route_id: usize,
    title_stack: Vec<String>,
    pub current_directory: Option<std::path::PathBuf>,
    pub remote_directory: Option<(String, std::path::PathBuf)>,
    /// Shell state from `OSC 1337 ; SetUserVar` (iTerm2 style).
    pub user_vars: rustc_hash::FxHashMap<String, String>,

    /// Whether a `TerminalDamaged` event is already in flight to the renderer.
    /// Set by PTY thread before sending; cleared by renderer after extracting damage.
    pub damage_event_in_flight: bool,

    /// What DECRQM callers see after a full reset: the
    /// consumer-configured default for grapheme clustering (mode
    /// 2027). On by default; embedders serving legacy wcwidth
    /// consumers flip it
    /// via [`set_grapheme_clustering`](Self::set_grapheme_clustering).
    grapheme_clustering_default: bool,

    // The stack for the keyboard modes.
    modify_other_keys: u8,
    kitty_clipboard_session: crate::clipboard::kitty::Session,
    clipboard_policy: crate::clipboard::ClipboardPolicy,
    keyboard_mode_stack: [u8; KEYBOARD_MODE_STACK_MAX_DEPTH],
    keyboard_mode_idx: usize,
    inactive_keyboard_mode_stack: [u8; KEYBOARD_MODE_STACK_MAX_DEPTH],
    inactive_keyboard_mode_idx: usize,
}

impl<U: EventListener> Crosswords<U> {
    /// Configure primary-screen retention. The alternate screen keeps no history.
    pub fn set_scrollback_limits(&mut self, lines: usize, bytes: Option<usize>) {
        if lines == 0 || bytes == Some(0) {
            self.clear_kitty_history_placements(self.mode.contains(Mode::ALT_SCREEN));
        }
        let primary = if self.mode.contains(Mode::ALT_SCREEN) {
            &mut self.inactive_grid
        } else {
            &mut self.grid
        };
        primary.update_history(lines);
        primary.set_history_byte_limit(bytes);
        self.expire_kitty_placements();
        self.mark_fully_damaged();
    }

    pub fn new<D: Dimensions>(
        dimensions: D,
        cursor_shape: CursorShape,
        event_proxy: U,
        window_id: WindowId,
        route_id: usize,
        scrollback_history_limit: usize,
    ) -> Crosswords<U> {
        let cols = dimensions.columns();
        let rows = dimensions.screen_lines();
        let grid = Grid::new(rows, cols, scrollback_history_limit);
        let alt = Grid::new(rows, cols, 0);

        let scroll_region = Line(0)..Line(rows as i32);
        let semantic_escape_chars = String::from(",│`|:\"' ()[]{}<>\t\0");
        let term_colors = TermColors::default();
        // Regex used for the default URL hint.
        let _url_regex: &str = "(ipfs:|ipns:|magnet:|mailto:|gemini://|gopher://|https://|http://|news:|file:|git://|ssh:|ftp://)\
                         [^\u{0000}-\u{001F}\u{007F}-\u{009F}<>\"\\s{-}\\^⟨⟩`\\\\]+";

        Crosswords {
            vi_mode_cursor: ViModeCursor::new(grid.cursor.pos),
            semantic_escape_chars,
            selection: None,
            grid,
            inactive_grid: alt,
            active_charset: CharsetIndex::default(),
            single_shift: None,
            screen_mode_state: [false; 4],
            scroll_region,
            event_proxy,
            colors: term_colors,
            title: String::from(""),
            program_status: Default::default(),
            metadata: Default::default(),
            tabs: TabStops::new(cols),
            mode: Mode::SHOW_CURSOR
                | Mode::LINE_WRAP
                | Mode::ALTERNATE_SCROLL
                | Mode::URGENCY_HINTS
                | Mode::GRAPHEME_CLUSTER,
            grapheme_clustering_default: true,
            damage: TermDamageState::new(rows),
            graphics: Graphics::new(&dimensions),
            #[cfg(feature = "graphics")]
            glyph_registry: None,
            default_cursor_shape: cursor_shape,
            cursor_shape,
            blinking_cursor: false,
            window_id,
            route_id,
            title_stack: Default::default(),
            current_directory: None,
            remote_directory: None,
            user_vars: rustc_hash::FxHashMap::default(),
            damage_event_in_flight: false,
            modify_other_keys: 0,
            kitty_clipboard_session: Default::default(),
            clipboard_policy: Default::default(),
            keyboard_mode_stack: Default::default(),
            keyboard_mode_idx: 0,
            inactive_keyboard_mode_stack: Default::default(),
            inactive_keyboard_mode_idx: 0,
        }
    }

    /// Configure the default for grapheme cluster processing (DEC
    /// private mode 2027): applied immediately and restored by RIS. A
    /// program's DECSET/DECRST still wins at runtime. Defaults to
    /// enabled; consumers whose renderers or downstream protocols
    /// assume legacy wcwidth cell layout can turn it off.
    pub fn set_grapheme_clustering(&mut self, enabled: bool) {
        self.grapheme_clustering_default = enabled;
        self.mode.set(Mode::GRAPHEME_CLUSTER, enabled);
    }

    pub fn mark_fully_damaged(&mut self) {
        // Only emit event if we weren't already fully damaged
        let was_damaged = self.damage.full;
        self.damage.full = true;

        // Request a render to display the damage
        if !was_damaged {
            self.event_proxy
                .send_event(RioEvent::RenderRoute(self.route_id), self.window_id);
        }
    }

    #[inline]
    pub fn is_fully_damaged(&self) -> bool {
        self.damage.full
    }

    /// Update selection damage tracking.
    /// This should be called when the selection changes to damage only the affected lines.
    pub fn update_selection_damage(
        &mut self,
        new_selection: Option<SelectionRange>,
        display_offset: usize,
    ) {
        // Damage old selection lines if they exist
        if let Some(old_selection) = self.damage.last_selection {
            self.damage.damage_selection(old_selection, display_offset);
        }

        // Damage new selection lines if they exist
        if let Some(new_selection) = new_selection {
            self.damage.damage_selection(new_selection, display_offset);
        }

        // Update the stored selection
        self.damage.last_selection = new_selection;
    }
    #[must_use]
    pub fn damage(&mut self) -> TermDamage<'_> {
        // Ensure the entire terminal is damaged after entering insert mode.
        // Leaving is handled in the ansi handler.
        if self.mode.contains(Mode::INSERT) {
            self.mark_fully_damaged();
        }

        let previous_cursor =
            mem::replace(&mut self.damage.last_cursor, self.grid.cursor.pos);

        if self.damage.full {
            return TermDamage::Full;
        }

        // Add information about old cursor position and new one if they are not the same, so we
        // cover everything that was produced by `Term::input`.
        if self.damage.last_cursor != previous_cursor {
            // Damage the entire line where the previous cursor was
            let previous_line = previous_cursor.row.0 as usize;
            self.damage.damage_line(previous_line);
        }

        // Always damage current cursor.
        // self.damage_cursor();

        // NOTE: damage which changes all the content when the display offset is non-zero (e.g.
        // scrolling) is handled via full damage.
        let display_offset = self.grid.display_offset();
        TermDamage::Partial(TermDamageIterator::new(&self.damage.lines, display_offset))
    }

    /// Peek damage event based on current damage state
    pub fn peek_damage_event(&self) -> Option<TerminalDamage> {
        if self.damage.full {
            Some(TerminalDamage::Full)
        } else if !self.damage.dirty_lines.is_empty() {
            Some(TerminalDamage::Partial)
        } else if self.damage.last_cursor != self.grid.cursor.pos {
            Some(TerminalDamage::CursorOnly)
        } else {
            None
        }
    }

    #[inline]
    pub fn reset_damage(&mut self) {
        self.damage.reset();
        // The consumer snapshots the cursor along with the rows, so the
        // position is painted once this frame is consumed. Without this,
        // `peek_damage_event` reports CursorOnly forever after the first
        // cursor movement (`last_cursor` was only maintained by the
        // legacy `damage()` path), so every PTY drain fires a redraw
        // event even when nothing changed.
        self.damage.last_cursor = self.grid.cursor.pos;
    }

    #[inline]
    pub fn display_offset(&self) -> usize {
        self.grid.display_offset()
    }

    #[inline]
    pub fn clear_saved_history(&mut self) {
        self.clear_screen(ClearMode::Saved);
    }

    /// Scroll so the previous (`forward = false`) or next prompt row
    /// starts at the top of the viewport. Prompts come from OSC 133
    /// marks; a run of consecutive prompt rows counts as one prompt.
    pub fn scroll_to_prompt(&mut self, forward: bool) {
        use crate::crosswords::grid::row::SemanticPrompt;

        let display_offset = self.grid.display_offset() as i32;
        let history = self.grid.history_size() as i32;
        let screen_lines = self.grid.screen_lines() as i32;
        let top = -display_offset;

        let is_marked =
            |line: i32| self.grid[Line(line)].semantic_prompt != SemanticPrompt::None;
        // First row of a prompt run: marked, with an unmarked row (or
        // the top of history) above it.
        let is_prompt_start =
            |line: i32| is_marked(line) && (line == -history || !is_marked(line - 1));

        let target = if forward {
            (top + 1..screen_lines).find(|line| is_prompt_start(*line))
        } else {
            (-history..top).rev().find(|line| is_prompt_start(*line))
        };

        if let Some(line) = target {
            let new_offset = (-line).max(0);
            self.scroll_display(Scroll::Delta(new_offset - display_offset));
        }
    }

    #[inline]
    pub fn scroll_display(&mut self, scroll: Scroll) {
        let old_display_offset = self.grid.display_offset();
        self.grid.scroll_display(scroll);
        self.event_proxy
            .send_event(RioEvent::MouseCursorDirty, self.window_id);

        // Clamp vi mode cursor to the viewport.
        let viewport_start = -(self.grid.display_offset() as i32);
        let viewport_end = viewport_start + self.grid.bottommost_line().0;
        let vi_cursor_line = &mut self.vi_mode_cursor.pos.row.0;
        *vi_cursor_line =
            std::cmp::min(viewport_end, std::cmp::max(viewport_start, *vi_cursor_line));
        self.vi_mode_recompute_selection();

        // Damage everything if display offset changed.
        if old_display_offset != self.grid.display_offset() {
            self.mark_fully_damaged();
            // Scrolling changes image positions on screen
            if !self.graphics.kitty_placements.is_empty() {
                self.graphics.kitty_graphics_dirty = true;
            }
        }
    }

    /// Lines ever evicted off the scrollback ring (see
    /// `Grid::lines_evicted`).
    #[inline]
    pub fn lines_evicted(&self) -> u64 {
        self.grid.lines_evicted()
    }

    #[inline]
    pub fn bottommost_line(&self) -> Line {
        self.grid.bottommost_line()
    }

    #[inline]
    pub fn colors(&self) -> &TermColors {
        &self.colors
    }

    /// Get queues to update graphic data. If both queues are empty, it returns
    /// `None`.
    #[inline]
    pub fn graphics_take_queues(&mut self) -> Option<UpdateQueues> {
        self.graphics.take_queues()
    }

    #[inline]
    pub fn send_graphics_updates(&mut self) {
        if self.graphics.coalesce_updates && self.graphics.update_in_flight {
            return;
        }
        if self.graphics.has_pending_updates() {
            if let Some(queues) = self.graphics.take_queues() {
                self.graphics.update_in_flight = self.graphics.coalesce_updates;
                self.event_proxy.send_event(
                    RioEvent::UpdateGraphics {
                        route_id: self.route_id,
                        queues,
                    },
                    self.window_id,
                );
            }
        }
    }

    #[inline]
    pub fn exit(&mut self)
    where
        U: EventListener,
    {
        self.end_program_activity();
        self.event_proxy
            .send_event(RioEvent::CloseTerminal(self.route_id), self.window_id);
    }

    /// Desktop consumers enable coalescing and acknowledge each delivered
    /// batch. At most one batch then owns pixels outside the terminal queue.
    pub fn acknowledge_graphics_update(&mut self) {
        self.graphics.update_in_flight = false;
        self.send_graphics_updates();
    }

    fn program_status_changed(&self) {
        {
            let mut metadata = self.metadata.lock();
            metadata.status = self.program_status.display_status();
            metadata.status_revision = self.program_status.revision();
        }
        self.event_proxy.send_event(
            RioEvent::ProgramStatusChanged(self.route_id),
            self.window_id,
        );
    }

    fn end_program_activity(&mut self) {
        if self.program_status.end_activity() {
            self.program_status_changed();
        }
    }

    pub fn resize<S: Dimensions>(&mut self, size: S) {
        let old_cols = self.grid.columns();
        let old_lines = self.grid.screen_lines();
        let num_cols = size.columns();
        let num_lines = size.screen_lines();

        if old_cols == num_cols && old_lines == num_lines {
            // Same grid, but the cell size may still have changed (a
            // font or DPI change that kept cols/rows constant). Keep
            // the graphics cell metrics and placements in sync so
            // cell-sized images keep tracking the grid.
            if self.graphics.cell_width != size.square_width()
                || self.graphics.cell_height != size.square_height()
            {
                self.graphics.resize(&size);
                let cell_w = self.graphics.cell_width.round() as usize;
                let cell_h = self.graphics.cell_height.round() as usize;
                if cell_w > 0 && cell_h > 0 {
                    for p in self.graphics.kitty_placements.values_mut() {
                        let (iw, ih) = self
                            .graphics
                            .kitty_images
                            .get(&p.image_id)
                            .map(|s| (s.data.width, s.data.height))
                            .unwrap_or((0, 0));
                        p.rescale(iw, ih, cell_w, cell_h);
                    }
                    for p in self
                        .graphics
                        .kitty_inactive_screen
                        .kitty_placements
                        .values_mut()
                    {
                        let (iw, ih) = self
                            .graphics
                            .kitty_inactive_screen
                            .kitty_images
                            .get(&p.image_id)
                            .map(|s| (s.data.width, s.data.height))
                            .unwrap_or((0, 0));
                        p.rescale(iw, ih, cell_w, cell_h);
                    }
                    if !self.graphics.kitty_placements.is_empty()
                        || !self
                            .graphics
                            .kitty_inactive_screen
                            .kitty_placements
                            .is_empty()
                    {
                        self.graphics.kitty_graphics_dirty = true;
                    }
                }
            }
            info!("Crosswords::resize dimensions unchanged");
            return;
        }
        // Move vi mode cursor with the content.
        let history_size = self.history_size();
        let mut delta = num_lines as i32 - old_lines as i32;
        let min_delta =
            std::cmp::min(0, num_lines as i32 - self.grid.cursor.pos.row.0 - 1);

        delta = std::cmp::min(std::cmp::max(delta, min_delta), history_size as i32);
        self.vi_mode_cursor.pos.row += delta;

        // Track Kitty anchor cells and atlas anchor rows through column
        // reflow. Only screens with placements need these mappings.
        // Height changes preserve anchors in absolute row space.
        let active_has_placements = !self.graphics.kitty_placements.is_empty()
            || !self.graphics.atlas_placements.is_empty();
        let inactive_has_placements = !self
            .graphics
            .kitty_inactive_screen
            .kitty_placements
            .is_empty()
            || !self
                .graphics
                .kitty_inactive_screen
                .atlas_placements
                .is_empty();

        let is_alt = self.mode.contains(Mode::ALT_SCREEN);
        self.grid.track_reflow_remap = active_has_placements;
        self.inactive_grid.track_reflow_remap = inactive_has_placements;
        self.grid.reflow_protected_rows.extend(
            self.graphics
                .kitty_placements
                .values()
                .filter(|p| p.relative.is_none())
                .map(|p| p.dest_row)
                .chain(self.graphics.atlas_placements.iter().map(|p| p.abs_row)),
        );
        self.inactive_grid.reflow_protected_rows.extend(
            self.graphics
                .kitty_inactive_screen
                .kitty_placements
                .values()
                .filter(|p| p.relative.is_none())
                .map(|p| p.dest_row)
                .chain(
                    self.graphics
                        .kitty_inactive_screen
                        .atlas_placements
                        .iter()
                        .map(|p| p.abs_row),
                ),
        );
        self.grid.reflow_protected_cells.extend(
            self.graphics
                .kitty_placements
                .values()
                .filter(|p| p.relative.is_none())
                .map(|p| (p.dest_row, p.dest_col)),
        );
        self.inactive_grid.reflow_protected_cells.extend(
            self.graphics
                .kitty_inactive_screen
                .kitty_placements
                .values()
                .filter(|p| p.relative.is_none())
                .map(|p| (p.dest_row, p.dest_col)),
        );
        let reflow_primary = self.mode.contains(Mode::LINE_WRAP);
        self.grid
            .resize(!is_alt && reflow_primary, num_lines, num_cols);
        self.inactive_grid
            .resize(is_alt && reflow_primary, num_lines, num_cols);
        self.grid.reflow_protected_rows.clear();
        self.inactive_grid.reflow_protected_rows.clear();
        self.grid.reflow_protected_cells.clear();
        self.inactive_grid.reflow_protected_cells.clear();
        self.grid.track_reflow_remap = false;
        self.inactive_grid.track_reflow_remap = false;
        let active_remap = self.grid.reflow_remap.take();
        let inactive_remap = self.inactive_grid.reflow_remap.take();

        // Invalidate selection and tabs only when necessary.
        if old_cols != num_cols {
            self.selection = None;

            // Recreate tabs list.
            self.tabs.resize(num_cols);
        } else if let Some(selection) = self.selection.take() {
            let max_lines = std::cmp::max(num_lines, old_lines) as i32;
            let range = Line(0)..Line(max_lines);
            self.selection = selection.rotate(&self.grid, &range, -delta);
        }

        // Clamp vi cursor to viewport.
        let vi_pos = self.vi_mode_cursor;
        let viewport_top = Line(-(self.grid.display_offset() as i32));
        let viewport_bottom = viewport_top + self.bottommost_line();
        self.vi_mode_cursor.pos.row =
            std::cmp::max(std::cmp::min(vi_pos.pos.row, viewport_bottom), viewport_top);
        self.vi_mode_cursor.pos.col =
            std::cmp::min(vi_pos.pos.col, self.grid.last_column());

        // Reset scrolling region.
        self.scroll_region = Line(0)..Line(self.grid.screen_lines() as i32);

        // Resize damage information.
        self.damage.resize(num_lines);

        // Update size information for graphics.
        self.graphics.resize(&size);

        // Rescale overlay placements for the new cell size (cell-sized
        // placements track the grid; native-size ones keep their pixel
        // dimensions). Active and inactive screens both get the
        // treatment so alt-screen images aren't stale on swap-back.
        let cell_w = self.graphics.cell_width.round() as usize;
        let cell_h = self.graphics.cell_height.round() as usize;
        if cell_w > 0 && cell_h > 0 {
            for p in self.graphics.kitty_placements.values_mut() {
                let (iw, ih) = self
                    .graphics
                    .kitty_images
                    .get(&p.image_id)
                    .map(|s| (s.data.width, s.data.height))
                    .unwrap_or((0, 0));
                p.rescale(iw, ih, cell_w, cell_h);
            }
            for p in self
                .graphics
                .kitty_inactive_screen
                .kitty_placements
                .values_mut()
            {
                let (iw, ih) = self
                    .graphics
                    .kitty_inactive_screen
                    .kitty_images
                    .get(&p.image_id)
                    .map(|s| (s.data.width, s.data.height))
                    .unwrap_or((0, 0));
                p.rescale(iw, ih, cell_w, cell_h);
            }
        }

        // Kitty uses the mapped anchor cell; atlas images follow their
        // row's content. Dropped atlas regions are destroyed, while Kitty
        // placement expiry follows the history lifetime.
        if let Some(remap) = &active_remap {
            let max_abs =
                self.grid.lines_evicted() as i64 + self.grid.total_lines() as i64;
            for p in self.graphics.kitty_placements.values_mut() {
                if let Some((row, col)) = remap.remap_cell(p.dest_row, p.dest_col) {
                    p.dest_row = row;
                    p.dest_col = col;
                }
            }
            let before = self.graphics.atlas_placements.len();
            self.graphics.atlas_placements.retain_mut(|p| {
                match remap.remap_abs(p.abs_row) {
                    Some(abs) if abs < max_abs => {
                        p.abs_row = abs;
                        true
                    }
                    _ => false,
                }
            });
            if self.graphics.atlas_placements.len() != before {
                self.graphics.recount_atlas_keys();
                self.send_graphics_updates();
            }
        }
        if let Some(remap) = &inactive_remap {
            let max_abs = self.inactive_grid.lines_evicted() as i64
                + self.inactive_grid.total_lines() as i64;
            let inactive = &mut self.graphics.kitty_inactive_screen;
            for p in inactive.kitty_placements.values_mut() {
                if let Some((row, col)) = remap.remap_cell(p.dest_row, p.dest_col) {
                    p.dest_row = row;
                    p.dest_col = col;
                }
            }
            let before = inactive.atlas_placements.len();
            inactive
                .atlas_placements
                .retain_mut(|p| match remap.remap_abs(p.abs_row) {
                    Some(abs) if abs < max_abs => {
                        p.abs_row = abs;
                        true
                    }
                    _ => false,
                });
            if inactive.atlas_placements.len() != before {
                self.graphics.recount_inactive_atlas_keys();
                self.send_graphics_updates();
            }
        }

        // The renderer only refreshes its placement snapshot when it
        // sees the dirty flag, and resize invalidates positions even
        // when no placement field changed (history and viewport moved).
        if active_has_placements || inactive_has_placements {
            self.graphics.kitty_graphics_dirty = true;
        }
        self.expire_kitty_placements();
        self.expire_atlas_placements();
    }

    /// Toggle the vi mode.
    #[inline]
    pub fn toggle_vi_mode(&mut self)
    where
        U: EventListener,
    {
        self.mode ^= Mode::VI;

        if self.mode.contains(Mode::VI) {
            let display_offset = self.grid.display_offset() as i32;
            if self.grid.cursor.pos.row > self.grid.bottommost_line() - display_offset {
                // Move cursor to top-left if terminal cursor is not visible.
                let pos = Pos::new(Line(-display_offset), Column(0));
                self.vi_mode_cursor.pos = pos;
            } else {
                // Reset vi mode cursor position to match primary cursor.
                self.vi_mode_cursor.pos = self.grid.cursor.pos;
            }
        }

        // Update UI about cursor blinking state changes.
        self.event_proxy
            .send_event(RioEvent::CursorBlinkingChange, self.window_id);
    }

    /// Update the active selection to match the vi mode cursor position.
    #[inline]
    fn vi_mode_recompute_selection(&mut self) {
        // Require vi mode to be active.
        if !self.mode.contains(Mode::VI) {
            return;
        }

        // Update only if non-empty selection is present.
        if let Some(selection) = self.selection.as_mut().filter(|s| !s.is_empty()) {
            selection.update(self.vi_mode_cursor.pos, Side::Left);
            selection.include_all();
        }
    }

    #[inline]
    pub fn vi_motion(&mut self, motion: ViMotion)
    where
        U: EventListener,
    {
        // Require vi mode to be active.
        if !self.mode.contains(Mode::VI) {
            return;
        }

        // Move cursor.
        self.vi_mode_cursor = self.vi_mode_cursor.motion(self, motion);
        self.vi_mode_recompute_selection();
    }

    /// Move vi cursor to a point in the grid.
    #[inline]
    pub fn vi_goto_pos(&mut self, pos: Pos)
    where
        U: EventListener,
    {
        // Move viewport to make pos visible.
        self.scroll_to_pos(pos);

        // Move vi cursor to the pos.
        self.vi_mode_cursor.pos = pos;

        self.vi_mode_recompute_selection();
    }

    /// Scroll display to point if it is outside of viewport.
    #[inline]
    pub fn scroll_to_pos(&mut self, pos: Pos)
    where
        U: EventListener,
    {
        let display_offset = self.grid.display_offset() as i32;
        let screen_lines = self.grid.screen_lines() as i32;

        if pos.row < -display_offset {
            let lines = pos.row + display_offset;
            self.scroll_display(Scroll::Delta(-lines.0));
        } else if pos.row >= (screen_lines - display_offset) {
            let lines = pos.row + display_offset - screen_lines + 1i32;
            self.scroll_display(Scroll::Delta(-lines.0));
        }
    }

    /// Jump to the end of a wide cell.
    pub fn expand_wide(&self, mut pos: Pos, direction: Direction) -> Pos {
        use crate::crosswords::square::Wide;
        let wide = self.grid[pos.row][pos.col].wide();

        match direction {
            Direction::Right if matches!(wide, Wide::LeadingSpacer) => {
                pos.col = Column(1);
                pos.row += 1;
            }
            Direction::Right if matches!(wide, Wide::Wide) => {
                pos.col = std::cmp::min(pos.col + 1, self.grid.last_column());
            }
            Direction::Left if matches!(wide, Wide::Wide | Wide::Spacer) => {
                if matches!(wide, Wide::Spacer) {
                    pos.col -= 1;
                }

                let prev = pos.sub(&self.grid, Boundary::Grid, 1);
                if matches!(self.grid[prev].wide(), Wide::LeadingSpacer) {
                    pos = prev;
                }
            }
            _ => (),
        }

        pos
    }

    #[inline]
    pub fn semantic_escape_chars(&self) -> &str {
        &self.semantic_escape_chars
    }

    #[inline]
    pub fn wrapline(&mut self) {
        if !self.mode.contains(Mode::LINE_WRAP) {
            return;
        }

        self.grid.cursor_cell().set_wrapline(true);

        if self.grid.cursor.pos.row + 1 >= self.scroll_region.end {
            self.linefeed();
        } else {
            self.damage_cursor();
            self.grid.cursor.pos.row += 1;
        }

        self.grid.cursor.pos.col = Column(0);
        let row = self.grid.cursor.pos.row;
        self.grid[row].wrap_continuation = true;
        self.grid.cursor.should_wrap = false;
        self.damage_cursor();
    }

    pub fn history_size(&self) -> usize {
        self.grid
            .total_lines()
            .saturating_sub(self.grid.screen_lines())
    }

    /// Damage the entire line at the cursor position
    #[inline]
    pub fn damage_cursor_line(&mut self) {
        let cursor_line = self.grid.cursor.pos.row.0 as usize;
        self.damage_line(cursor_line);
    }

    /// Damage an entire line
    #[inline]
    pub fn damage_line(&mut self, line: usize) {
        self.damage.damage_line(line);
    }

    #[inline]
    pub fn damage_cursor(&mut self) {
        self.damage_cursor_line();

        // self.event_proxy.send_event(
        // RioEvent::TerminalDamaged {
        // route_id: self.route_id,
        // damage: TerminalDamage::CursorOnly(self.grid.cursor.pos.line, None),
        // },
        // self.window_id,
        // );
    }

    #[inline]
    pub fn damage_cursor_blink(&mut self) {
        // Only damage cursor for blink if cursor is visible and blinking is enabled
        let cursor_state = self.cursor();
        if cursor_state.is_visible() {
            // Use line-based damage for cursor blinking
            self.damage_cursor_line();

            // self.event_proxy.send_event(
            // RioEvent::TerminalDamaged {
            // route_id: self.route_id,
            // damage: TerminalDamage::CursorOnly,
            // },
            // self.window_id,
            // );
        }
    }

    /// Check if any rendering is actually needed
    #[inline]
    pub fn needs_render(&self) -> bool {
        // Always render if fully damaged
        if self.is_fully_damaged() {
            return true;
        }

        // Check if there's any partial damage
        if !self.damage.dirty_lines.is_empty() {
            return true;
        }

        // No rendering needed if no damage
        false
    }

    #[inline]
    fn scroll_down_relative(
        &mut self,
        origin: Line,
        mut lines: usize,
        move_images: bool,
    ) {
        debug!(
            "Scrolling down relative: origin={}, lines={}",
            origin, lines
        );

        lines = std::cmp::min(
            lines,
            (self.scroll_region.end - self.scroll_region.start).0 as usize,
        );
        lines = std::cmp::min(lines, (self.scroll_region.end - origin).0 as usize);

        let region = origin..self.scroll_region.end;

        // Scroll selection.
        self.selection = self
            .selection
            .take()
            .and_then(|s| s.rotate(&self.grid, &region, -(lines as i32)));

        // Scroll vi mode cursor.
        let line = &mut self.vi_mode_cursor.pos.row;
        if region.start <= *line && region.end > *line {
            *line = std::cmp::min(*line + lines, region.end - 1);
        }

        // Scroll between origin and bottom
        self.grid.sync_template_style();
        let old_top = self.grid.lines_evicted() as i64 + self.history_size() as i64;
        let last = self.grid.last_column();
        for row in region.start.0..region.end.0 {
            self.grid[Line(row)][last].set_wrapline(false);
            self.grid[Line(row)].wrap_continuation = false;
        }
        self.grid.scroll_down(&region, lines);
        if move_images {
            self.scroll_kitty_with_margins(&region, lines as i64, old_top);
        }
        self.mark_fully_damaged();
        // Partial-region scrolls move grid-plane images with their
        // content (kitty placements float and are not adjusted).
        self.shift_atlas_placements_in_region(&region, lines as i64);
        if !self.graphics.kitty_placements.is_empty() {
            self.graphics.kitty_graphics_dirty = true;
        }
    }

    #[inline]
    pub fn scroll_up_relative(&mut self, origin: Line, lines: usize) {
        self.scroll_up_relative_inner(origin, lines, true, true);
    }

    fn scroll_up_relative_inner(
        &mut self,
        origin: Line,
        mut lines: usize,
        preserve_wrap: bool,
        move_images: bool,
    ) {
        lines = std::cmp::min(
            lines,
            (self.scroll_region.end - self.scroll_region.start).0 as usize,
        );

        let region = origin..self.scroll_region.end;

        let history = preserve_wrap && region.start == 0;

        // Scroll selection.
        if self.selection.is_some() {
            self.selection = self
                .selection
                .take()
                .and_then(|s| s.rotate(&self.grid, &region, lines as i32));
        }

        self.grid.sync_template_style();
        let pinned_offset = self.grid.display_offset();
        let old_top = self.grid.lines_evicted() as i64 + self.history_size() as i64;
        if history {
            self.grid.scroll_up(&region, lines);
        } else {
            if !preserve_wrap {
                let last = self.grid.last_column();
                for row in region.start.0..region.end.0 {
                    self.grid[Line(row)][last].set_wrapline(false);
                    self.grid[Line(row)].wrap_continuation = false;
                }
            }
            self.grid.delete_lines(&region, lines);
        }
        if history
            && region.end.0 as usize == self.grid.screen_lines()
            && self.grid.history_is_disabled()
        {
            // No-history scrolling shifts active rows in place. Tracked image
            // pins move up with them and clamp at row zero, unlike pins whose
            // history rows are erased or evicted.
            let new_top = self.grid.lines_evicted() as i64 + self.history_size() as i64;
            for p in self.graphics.kitty_placements.values_mut() {
                let row = p.dest_row - old_top;
                if p.relative.is_none()
                    && (0..self.grid.screen_lines() as i64).contains(&row)
                {
                    let next = new_top + (row - lines as i64).max(0);
                    self.graphics.kitty_graphics_dirty |= next != p.dest_row;
                    p.dest_row = next;
                }
            }
        }
        if move_images {
            self.scroll_kitty_with_margins(&region, -(lines as i64), old_top);
        }

        // A viewport pinned in scrollback normally absorbs the scroll:
        // the offset grows and the same absolute rows stay on screen.
        // At the history cap the offset clamps, the view slides, and
        // every visible row changes while the region damage below only
        // covers active-area lines. Nothing less than full damage is
        // correct then.
        if history
            && region.start == 0
            && pinned_offset != 0
            && self.grid.display_offset() != pinned_offset + lines
        {
            self.mark_fully_damaged();
        }

        // Scroll vi mode cursor.
        let viewport_top = Line(-(self.grid.display_offset() as i32));
        let top = if history && region.start == 0 {
            viewport_top
        } else {
            region.start
        };
        let line = &mut self.vi_mode_cursor.pos.row;
        if (top <= *line) && region.end > *line {
            *line = std::cmp::max(*line - lines, top);
        }
        // Mark the scroll region as damaged; a full-screen region
        // coalesces into full damage, like scroll_down_relative.
        if region.start.0 == 0 && region.end.0 as usize == self.grid.screen_lines() {
            self.mark_fully_damaged();
        } else {
            for line in region.start.0..region.end.0 {
                self.damage.damage_line(line as usize);
            }
        }
        if !self.graphics.kitty_placements.is_empty() {
            // Placements whose rows all scrolled off the ring expire,
            // like kitty: the image data survives for future
            // placements, the placement itself dies with its content.
            self.expire_kitty_placements();
            self.graphics.kitty_graphics_dirty = true;
        }
        // Grid-plane images follow their content through region
        // scrolls; kitty placements float and are not adjusted.
        if history && region.start.0 == 0 {
            if (region.end.0 as usize) < self.grid.screen_lines() {
                // History grew under the fixed bottom rows.
                self.shift_atlas_placements_below(region.end, lines as i64);
            }
            // In-region content keeps its absolute rows (they moved
            // into history), so nothing else to do.
        } else {
            self.shift_atlas_placements_in_region(&region, -(lines as i64));
        }
        self.expire_atlas_placements();
    }

    /// Kitty images fully inside margins scroll and crop at the margins.
    /// Other images keep their viewport position, including when the row
    /// operation added history above a fixed part of the screen.
    fn scroll_kitty_with_margins(
        &mut self,
        region: &Range<Line>,
        delta: i64,
        old_top: i64,
    ) {
        if self.graphics.kitty_placements.is_empty()
            || (region.start == 0 && region.end.0 as usize == self.grid.screen_lines())
        {
            return;
        }
        let new_top = self.grid.lines_evicted() as i64 + self.history_size() as i64;
        let screen_lines = self.grid.screen_lines() as i64;
        let cell_size = (
            self.graphics.cell_width.round() as usize,
            self.graphics.cell_height.round() as usize,
        );
        let top = i64::from(region.start.0);
        let bottom = i64::from(region.end.0);
        let images = &self.graphics.kitty_images;
        let mut changed = false;
        self.graphics.kitty_placements.retain(|_, p| {
            if p.relative.is_some() {
                return true;
            }
            let y = p.dest_row - old_top;
            if y < 0 || y >= screen_lines {
                return true;
            }
            let mut next = y;
            let rows = i64::from(p.rows);
            if p.columns > 0 && rows > 0 && y >= top && y + rows <= bottom {
                next = y + delta;
                let above = (top - next).max(0);
                let below = (next + rows - bottom).max(0);
                if above >= rows || below >= rows {
                    changed = true;
                    return false;
                }
                let crop = above.max(below);
                if crop > 0 {
                    let Some(image) = images.get(&p.image_id) else {
                        return true;
                    };
                    if !p.clip_rows(
                        (image.data.width, image.data.height),
                        cell_size,
                        crop as u32,
                        above > 0,
                    ) {
                        changed = true;
                        return false;
                    }
                    changed = true;
                    next = next.max(top);
                }
            }
            let row = new_top + next;
            changed |= row != p.dest_row;
            p.dest_row = row;
            true
        });
        if changed {
            self.graphics.remove_orphaned_placements();
            self.graphics.kitty_graphics_dirty = true;
            self.send_graphics_updates();
        }
    }

    /// Drop sixel/iTerm2 placements whose rows all scrolled off the
    /// ring; the key recount frees images that lost their last
    /// placement (pixel store and GPU texture).
    fn expire_atlas_placements(&mut self) {
        if self.graphics.atlas_placements.is_empty() {
            return;
        }
        let base = self.grid.lines_evicted() as i64;
        let before = self.graphics.atlas_placements.len();
        self.graphics
            .atlas_placements
            .retain(|p| p.abs_row + p.rows as i64 > base);
        if self.graphics.atlas_placements.len() != before {
            self.graphics.recount_atlas_keys();
            self.graphics.kitty_graphics_dirty = true;
            self.send_graphics_updates();
        }
    }

    /// Clip sixel/iTerm2 placements against a cell-aligned hole in
    /// visible screen coordinates (DEC semantics: text and erasure
    /// remove exactly the covered image cells; the surviving pieces
    /// keep referencing the same texture with adjusted crops).
    pub fn clip_atlas_placements(
        &mut self,
        screen_row_start: i32,
        screen_row_end: i32,
        col_start: usize,
        col_end: usize,
    ) {
        if self.graphics.atlas_placements.is_empty() {
            return;
        }
        let base = self.grid.lines_evicted() as i64 + self.history_size() as i64;
        let hr0 = base + screen_row_start as i64;
        let hr1 = base + screen_row_end as i64;

        let old = std::mem::take(&mut self.graphics.atlas_placements);
        let mut next: Vec<crate::ansi::graphics::AtlasPlacement> =
            Vec::with_capacity(old.len());
        let mut changed = false;
        for placement in old {
            match placement.subtract_rect(hr0, hr1, col_start, col_end, &mut next) {
                None => next.push(placement),
                Some(()) => changed = true,
            }
        }
        self.graphics.atlas_placements = next;
        if changed {
            self.graphics.recount_atlas_keys();
            self.graphics.kitty_graphics_dirty = true;
            self.send_graphics_updates();
        }
    }

    /// Shift sixel/iTerm2 placements with a partial scroll region
    /// (DECSTBM): content inside the region moves by `delta` rows and
    /// clips at the region boundary, exactly like text; content
    /// outside is untouched. Full-screen scrolls never come here;
    /// absolute anchoring already handles them.
    fn shift_atlas_placements_in_region(
        &mut self,
        region: &std::ops::Range<Line>,
        delta: i64,
    ) {
        if self.graphics.atlas_placements.is_empty() {
            return;
        }
        let base = self.grid.lines_evicted() as i64 + self.history_size() as i64;
        let r0 = base + region.start.0 as i64;
        let r1 = base + region.end.0 as i64;

        let old = std::mem::take(&mut self.graphics.atlas_placements);
        let mut next: Vec<crate::ansi::graphics::AtlasPlacement> =
            Vec::with_capacity(old.len());
        let mut changed = false;
        for placement in old {
            let p_r0 = placement.abs_row;
            let p_r1 = placement.abs_row + placement.rows as i64;
            if p_r1 <= r0 || p_r0 >= r1 {
                next.push(placement);
                continue;
            }
            changed = true;
            // Pieces outside the region stay put.
            placement.subtract_rect(r0, r1, 0, usize::MAX, &mut next);
            // The piece inside shifts, then clips at the region bounds.
            let ir0 = p_r0.max(r0);
            let ir1 = p_r1.min(r1);
            let inside = placement.slice(
                ir0,
                ir1,
                placement.col,
                placement.col + placement.columns,
            );
            let shifted_r0 = (inside.abs_row + delta).max(r0);
            let shifted_r1 = (inside.abs_row + delta + inside.rows as i64).min(r1);
            if shifted_r1 > shifted_r0 {
                // Clip in the pre-shift frame, then translate.
                let mut clipped = inside.slice(
                    shifted_r0 - delta,
                    shifted_r1 - delta,
                    inside.col,
                    inside.col + inside.columns,
                );
                clipped.abs_row += delta;
                next.push(clipped);
            }
        }
        self.graphics.atlas_placements = next;
        if changed {
            self.graphics.recount_atlas_keys();
            self.graphics.kitty_graphics_dirty = true;
            self.send_graphics_updates();
        }
    }

    /// A top-anchored partial scroll region grows history while the
    /// rows below the region stay visually fixed, which advances
    /// their absolute coordinates. Shift placements on those rows
    /// (splitting any placement straddling the boundary) so they stay
    /// glued to their fixed content.
    fn shift_atlas_placements_below(&mut self, boundary: Line, delta: i64) {
        if self.graphics.atlas_placements.is_empty() {
            return;
        }
        // The boundary in the space placements were anchored in,
        // BEFORE this scroll grew history by `delta`.
        let base = self.grid.lines_evicted() as i64 + self.history_size() as i64 - delta;
        let b = base + boundary.0 as i64;

        let old = std::mem::take(&mut self.graphics.atlas_placements);
        let mut next: Vec<crate::ansi::graphics::AtlasPlacement> =
            Vec::with_capacity(old.len());
        let mut changed = false;
        for placement in old {
            let p_r0 = placement.abs_row;
            let p_r1 = placement.abs_row + placement.rows as i64;
            if p_r1 <= b {
                next.push(placement);
                continue;
            }
            changed = true;
            if p_r0 < b {
                next.push(placement.slice(
                    p_r0,
                    b,
                    placement.col,
                    placement.col + placement.columns,
                ));
            }
            let mut below = placement.slice(
                p_r0.max(b),
                p_r1,
                placement.col,
                placement.col + placement.columns,
            );
            below.abs_row += delta;
            next.push(below);
        }
        self.graphics.atlas_placements = next;
        if changed {
            self.graphics.recount_atlas_keys();
            self.graphics.kitty_graphics_dirty = true;
        }
    }

    #[inline(always)]
    pub fn write_at_cursor(&mut self, c: char) {
        let charset = self.single_shift.take().unwrap_or(self.active_charset);
        let c = self.grid.cursor.charsets[charset].map(c);
        self.write_at_cursor_with_wide(c, Wide::Narrow);
    }

    fn write_at_cursor_with_wide(&mut self, c: char, wide: Wide) {
        self.grid.sync_template_style();
        let template = self.cell_template();
        self.write_cell_with_wide(c, template, wide);
    }

    /// Write an already-charset-mapped `c` into the cursor cell, taking a
    /// prebuilt `template` (a `Square` carrying the cursor's style / extras /
    /// flags with a zero codepoint). Split out of `write_at_cursor` so a
    /// print run (`input_str`) hoists the charset map and template build out
    /// of its per-cell loop; the write is then a single packed store, and the
    /// cursor cell is resolved once (not twice) in the common narrow case.
    #[inline]
    /// Clear the wide pair that a cell boundary before `col` would
    /// split, plus the cross-row links a mutation at that boundary
    /// severs: each
    /// range mutation names the exact seams it cuts, O(1) per seam,
    /// instead of sweeping the row afterwards.
    ///
    /// `col` may equal `columns`, meaning the boundary to the right of
    /// the final cell — that clears stale pre-wrap padding
    /// (`LeadingSpacer`) so a shift can't drag it into the interior.
    ///
    /// A cleared cell takes `blank` but keeps WRAPLINE: the flag marks
    /// the logical line for reflow and lives on the row's last cell,
    /// which the mutation never targeted.
    fn split_wide_seam(
        &mut self,
        line: Line,
        col: usize,
        blank: crate::crosswords::square::Square,
    ) {
        use crate::crosswords::square::Wide;
        let columns = self.grid.columns();

        // Boundary right of the final cell.
        if col >= columns {
            let last = Column(columns - 1);
            if self.grid[line][last].wrapline()
                && matches!(self.grid[line][last].wide(), Wide::LeadingSpacer)
            {
                self.grid[line][last] = blank;
                self.grid[line][last].set_wrapline(true);
            }
            return;
        }

        // Only a wrap continuation links the first wide cell to the
        // preceding spacer head, which may be in history.
        if col <= 1
            && self.grid[line].wrap_continuation
            && matches!(self.grid[line][Column(0)].wide(), Wide::Wide)
            && line != self.grid.topmost_line()
        {
            let last = self.grid.last_column();
            let prev = &mut self.grid[line - 1i32][last];
            if matches!(prev.wide(), Wide::LeadingSpacer) {
                let wrapped = prev.wrapline();
                *prev = blank;
                prev.set_wrapline(wrapped);
                if line.0 > 0 {
                    self.damage.damage_line((line.0 - 1) as usize);
                }
            }
        }
        if col == 0 {
            return;
        }

        // Interior boundary: splitting between col-1 and col cuts a
        // pair whose lead sits at col-1. Erasing half a wide char
        // erases the whole char (the xterm convention).
        if matches!(self.grid[line][Column(col - 1)].wide(), Wide::Wide) {
            self.clear_pair_preserving_wrapline(line, col - 1, blank);
        }
    }

    /// Clear both halves of the wide pair with its lead at `col`,
    /// keeping each cell's WRAPLINE bit.
    fn clear_pair_preserving_wrapline(
        &mut self,
        line: Line,
        col: usize,
        blank: crate::crosswords::square::Square,
    ) {
        let columns = self.grid.columns();
        for c in col..(col + 2).min(columns) {
            let cell = &mut self.grid[line][Column(c)];
            let wrapline = cell.wrapline();
            *cell = blank;
            if wrapline {
                cell.set_wrapline(true);
            }
        }
    }

    /// Append `c` to the preceding cell's cluster. At column zero only a
    /// pending wrap leaves a preceding base under the cursor.
    fn attach_to_prev_cell(&mut self, c: char) {
        let Some(column) = self.prev_cell_col() else {
            return;
        };
        let column = Column(column);
        let row = self.grid.cursor.pos.row;

        // A bg-only cell reuses the extras-id bits for its color
        // channel: reading them as an id would clone an unrelated
        // slot onto this cell, and writing one back would corrupt
        // the color. A combining mark with no base character has
        // nothing to attach to, so drop it.
        if !matches!(
            self.grid[row][column].content_tag(),
            crate::crosswords::square::ContentTag::Codepoint
        ) {
            return;
        }
        // Copy-on-write: slots are interned and shared; every
        // cell written under one OSC 8 template references the
        // same slot, so pushing into it in place would attach the
        // mark to the whole hyperlink span. Clone, extend, and
        // re-intern instead; the marked cell gets its own
        // (marks + hyperlink) slot and its neighbors keep theirs.
        let existing_id = self.grid[row][column].extras_id();
        let mut extras = existing_id
            .and_then(|id| self.grid.extras_table.get(id).cloned())
            .unwrap_or_default();
        extras.zerowidth.push(c);
        let id = self.grid.alloc_extras(extras);
        if id != 0 {
            let cell = &mut self.grid[row][column];
            cell.set_extras_id(Some(id));
            cell.insert_cell_flag(CellFlags::GRAPHEME);
            self.grid[row].has_extras = true;
            // The attach mutated an already-painted cell and the input
            // paths return without touching the write-path damage
            // bookkeeping: record it here or the renderer never
            // repaints the row (stale glyphs until a full redraw).
            self.damage.damage_line(row.0 as usize);
        }
        // id == 0: slot space exhausted. Keep the cell's existing
        // extras (hyperlink, earlier marks) rather than erasing
        // them; only the new mark is lost.
    }

    /// Mode-2027 cluster continuation. `true` means `c` was consumed:
    /// appended to the previous cell's cluster (possibly widening it),
    /// or deliberately ignored (an invalid variation
    /// selector). `false` means a grapheme break:
    /// the caller writes `c` through the normal paths.
    ///
    /// There is no cross-call segmentation state. Any no-break
    /// sequence accumulates into a single cell, so everything the
    /// break rules can look behind at, an emoji ZWJ run (GB11),
    /// regional-indicator parity (GB12/13), an Indic conjunct chain
    /// (GB9c), is exactly the previous cell's contents, and the
    /// `BreakState` is rebuilt from them (a handful of codepoints).
    /// Cursor movement, clears, scrolling, and screen switches
    /// invalidate a cluster automatically: whatever cell precedes the
    /// cursor *is* the truth, with no reset hooks to forget.
    fn try_cluster_append(&mut self, c: char, width: usize) -> bool {
        use crate::grapheme_lut::{class_of, is_break_lut, start_state};

        let row = self.grid.cursor.pos.row;
        let Some(base_col) = self.prev_cell_col() else {
            return false;
        };
        let cell = self.grid[row][Column(base_col)];
        if !matches!(
            cell.content_tag(),
            crate::crosswords::square::ContentTag::Codepoint
        ) {
            return false;
        }
        let base = cell.c();
        if base == '\0' {
            // Never-written cell: nothing to continue.
            return false;
        }

        // Reconstruct the segmentation state from the cluster itself.
        // Flat-table hops: one class lookup per
        // codepoint and one transition index per step, no rule chain.
        let mut prev = class_of(base);
        let mut state = start_state(prev);
        let mut last_cp = base;
        if let Some(id) = cell.extras_id() {
            if let Some(extras) = self.grid.extras_table.get(id) {
                for &attached in &extras.zerowidth {
                    let class = class_of(attached);
                    let _ = is_break_lut(prev, class, &mut state);
                    prev = class;
                    last_cp = attached;
                }
            }
        }
        if is_break_lut(prev, class_of(c), &mut state) {
            return false;
        }

        // Joined. Decide the width effect: variation selectors flip a valid
        // base's width and are otherwise ignored outright; any other
        // width-bearing continuation makes the whole cluster wide
        // (the base already contributed one column); zero-width
        // continuations change nothing.
        match c {
            '\u{FE0F}' => {
                // A selector modifies the codepoint immediately before
                // it, so validity is judged against the *last* codepoint
                // of the cluster, not its base. The width helpers are
                // no-ops when the cell is already in the target state.
                if vs_is_valid_base(last_cp, c) && self.widen_prev_cell() {
                    self.attach_to_prev_cell(c);
                }
                // Invalid selector: ignore. The cell is untouched, so
                // the reconstructed state next time is identical.
            }
            '\u{FE0E}' => {
                if vs_is_valid_base(last_cp, c) {
                    self.narrow_prev_cell();
                    self.attach_to_prev_cell(c);
                }
            }
            _ if width == 0 => self.attach_to_prev_cell(c),
            _ => {
                if self.widen_prev_cell() {
                    self.attach_to_prev_cell(c);
                }
            }
        }
        true
    }

    // A split wide pair leaves a blank with the current background. Preserve
    // the logical line's wrap marker, but discard the old glyph's attributes.
    fn clear_wide_neighbor(&mut self, line: Line, col: Column) {
        let bg = self.grid.template_style().bg;
        let mut blank = self.grid.blank_with_bg(bg);
        blank.set_wrapline(self.grid[line][col].wrapline());
        self.grid[line][col] = blank;
        self.grid[line].has_styles |= blank.carries_style();
    }

    #[inline]
    fn write_cell(&mut self, c: char, template: crate::crosswords::square::Square) {
        self.write_cell_with_wide(c, template, Wide::Narrow);
    }

    fn write_cell_with_wide(&mut self, c: char, template: Square, new_wide: Wide) {
        use crate::crosswords::square::{Square, Wide};

        // DEC semantics: printing into a cell covered by a sixel or iTerm2
        // image clips exactly that cell out of the placement. One branch
        // when no images exist.
        if !self.graphics.atlas_placements.is_empty() {
            let pos = self.grid.cursor.pos;
            self.clip_atlas_placements(
                pos.row.0,
                pos.row.0 + 1,
                pos.col.0,
                pos.col.0 + 1,
            );
        }

        let has_extras = template.extras_id().is_some();
        let styled = template.carries_style();
        let cursor_square = self.grid.cursor_square();
        let wrapline = cursor_square.wrapline();

        // Matching widths need no neighbor cleanup. Narrow destinations also
        // have no old wide pair to repair.
        if cursor_square.wide() == new_wide
            || !matches!(cursor_square.wide(), Wide::Wide | Wide::Spacer)
        {
            *cursor_square = Square::from_template(template, c);
            cursor_square.set_wide(new_wide);
            cursor_square.set_wrapline(wrapline);
            if has_extras || styled {
                let row = self.grid.cursor.pos.row;
                self.grid[row].has_extras |= has_extras;
                self.grid[row].has_styles |= styled;
            }
            return;
        }

        // Slow path: overwriting a wide char / spacer needs neighbour cleanup.
        let wide = matches!(cursor_square.wide(), Wide::Wide);
        let point = self.grid.cursor.pos;
        if wide && point.col < self.grid.last_column() {
            self.clear_wide_neighbor(point.row, point.col + 1);
        } else if point.col > 0 {
            self.clear_wide_neighbor(point.row, point.col - 1);
        }
        // Remove leading spacers.
        if (point.col == Column(0) || (!wide && point.col == Column(1)))
            && point.row.0 > 0
        {
            let column = self.grid.last_column();
            let prev = &mut self.grid[point.row - 1i32][column];
            if matches!(prev.wide(), Wide::LeadingSpacer) {
                prev.set_wide(Wide::Narrow);
                if point.row.0 > 0 {
                    self.damage.damage_line((point.row.0 - 1) as usize);
                }
            }
        }

        *self.grid.cursor_cell() = Square::from_template(template, c);
        self.grid.cursor_cell().set_wide(new_wide);
        self.grid.cursor_cell().set_wrapline(wrapline);
        if has_extras || styled {
            let row = self.grid.cursor.pos.row;
            self.grid[row].has_extras |= has_extras;
            self.grid[row].has_styles |= styled;
        }
    }

    /// Write one non-zero-width codepoint at the cursor with scalar wrap,
    /// placeholder and wide-pair handling; fallback for the run writers.
    fn write_codepoint_cell(&mut self, cp: u32, c: char, width: u8) {
        if self.grid.cursor.should_wrap {
            self.wrapline();
        }

        let columns = self.grid.columns();

        // Kitty placeholder bookkeeping: cp can't be ASCII here (parser
        // routes ASCII through `input_str`) but the placeholder lives at
        // U+10EEEE, so the check is still needed.
        if cp == crate::ansi::kitty_virtual::PLACEHOLDER as u32 {
            let row = self.grid.cursor.pos.row;
            self.grid[row].kitty_virtual_placeholder = true;
        }

        // A one-column grid can never hold a wide pair, and the wrap-for-room
        // branch below cannot create room on it: wrapping lands back on
        // column 0, which is still the last column. Drop the glyph and leave
        // a blank narrow cell, rather
        // than running the Spacer off the end of the row. Embedders are
        // entitled to such a grid: drag-resize and tiling layouts produce
        // them transiently.
        let (c, width) = if width == 2 && columns < 2 {
            (' ', 1)
        } else {
            (c, width)
        };

        if width == 2 {
            if self.grid.cursor.pos.col + 1 >= columns {
                if self.mode.contains(Mode::LINE_WRAP) {
                    self.write_at_cursor(' ');
                    self.grid
                        .cursor_cell()
                        .set_wide(crate::crosswords::square::Wide::LeadingSpacer);
                    self.wrapline();
                } else {
                    self.grid.cursor.should_wrap = true;
                    return;
                }
            }

            self.write_at_cursor_with_wide(c, Wide::Wide);
            self.grid.cursor.pos.col += 1;
            self.write_at_cursor_with_wide(' ', Wide::Spacer);
        } else {
            self.write_at_cursor(c);
        }

        let cursor_line = self.grid.cursor.pos.row.0 as usize;
        self.damage.damage_line(cursor_line);

        if self.grid.cursor.pos.col + 1 < columns {
            self.grid.cursor.pos.col += 1;
        } else {
            self.grid.cursor.should_wrap = true;
        }
    }

    /// Write ordinary mixed-width text up to the current row boundary.
    /// Complex clusters and row-edge wide characters retain the scalar paths.
    fn write_ordinary_row(
        &mut self,
        template: Square,
        cps: &[u32],
        table: &[u8],
        clustering: bool,
    ) -> usize {
        use crate::crosswords::square::Wide;

        if !self.mode.contains(Mode::LINE_WRAP) {
            return 0;
        }
        if self.grid.cursor.should_wrap {
            self.wrapline();
        }
        let columns = self.grid.columns();
        let start = self.grid.cursor.pos.col.0;
        let line = self.grid.cursor.pos.row;
        let row = &mut self.grid[line];
        let wrapline = row.inner[columns - 1].wrapline();
        let mut col = start;
        let mut written = 0;
        let mut spacer = Square::from_template(template, ' ');
        spacer.set_wide(Wide::Spacer);
        let clear_left = start != 0 && row.inner[start].wide() == Wide::Spacer;
        let mut clear_right = false;
        let mut clear_previous_leading = false;
        for &cp in cps {
            let properties = crate::codepoint_width::print_properties_in(table, cp);
            let width = match properties.width() {
                Some(width @ (1 | 2)) => width as usize,
                _ => break,
            };
            if (clustering && !properties.ordinary())
                || cp == crate::ansi::kitty_virtual::PLACEHOLDER as u32
                || col + width > columns
            {
                break;
            }
            // Everything inside the written span is replaced. Only a pair
            // crossing either edge can leave an old neighbour behind.
            clear_right = row.inner[col + width - 1].wide() == Wide::Wide;
            if col <= 1 && !(col == 0 && width == 2) {
                clear_previous_leading |=
                    Square::any_needs_wide_cleanup(&row.inner[col..(col + width).min(2)]);
            }
            let c = char::from_u32(cp).unwrap_or('\u{fffd}');
            let mut cell = Square::from_template(template, c);
            if width == 2 {
                cell.set_wide(Wide::Wide);
                row.inner[col + 1] = spacer;
            }
            row.inner[col] = cell;
            col += width;
            written += 1;
        }
        if written != 0 {
            row.inner[columns - 1].set_wrapline(wrapline);
            row.occ = row.occ.max(col);
            row.dirty = true;
            row.has_extras |= template.extras_id().is_some();
            row.has_styles |= template.carries_style();
            if clear_left {
                self.clear_wide_neighbor(line, Column(start - 1));
            }
            if clear_right && col < columns {
                self.clear_wide_neighbor(line, Column(col));
            }
            if clear_previous_leading && line.0 > 0 {
                let previous = &mut self.grid[line - 1i32][Column(columns - 1)];
                if previous.wide() == Wide::LeadingSpacer {
                    previous.set_wide(Wide::Narrow);
                    if line.0 > 0 {
                        self.damage.damage_line((line.0 - 1) as usize);
                    }
                }
            }
            self.grid.cursor.pos.col = Column(col.min(columns - 1));
            self.grid.cursor.should_wrap = col == columns;
            self.damage.damage_line(line.0 as usize);
        }
        written
    }

    /// Bulk-write a run of width-1 codepoints, mirroring the ASCII bulk
    /// path in `input_ascii_str`: one destination scan, packed stores,
    /// cursor and damage updated per row chunk instead of per codepoint.
    fn write_narrow_run(
        &mut self,
        template: crate::crosswords::square::Square,
        template_has_extras: bool,
        cps: &[u32],
    ) {
        let mut idx = 0;
        while idx < cps.len() {
            if self.grid.cursor.should_wrap {
                self.wrapline();
            }

            let columns = self.grid.columns();
            let cursor_col = self.grid.cursor.pos.col.0;
            let remaining = columns.saturating_sub(cursor_col);
            if remaining == 0 {
                for &cp in &cps[idx..] {
                    let c = char::from_u32(cp).unwrap_or('\u{FFFD}');
                    self.write_codepoint_cell(cp, c, 1);
                }
                return;
            }

            let take = (cps.len() - idx).min(remaining);
            let mut wrote_bulk = false;
            {
                let line = self.grid.cursor.pos.row;
                let row = &mut self.grid[line];
                let wrapline = row.inner[columns - 1].wrapline();
                let cells = &mut row[Column(cursor_col)..Column(cursor_col + take)];
                let needs_cleanup = Square::any_needs_wide_cleanup(cells);
                if !needs_cleanup {
                    for (cell, &cp) in cells.iter_mut().zip(&cps[idx..idx + take]) {
                        let c = char::from_u32(cp).unwrap_or('\u{FFFD}');
                        *cell =
                            crate::crosswords::square::Square::from_template(template, c);
                    }
                    row.inner[columns - 1].set_wrapline(wrapline);
                    if template_has_extras {
                        row.has_extras = true;
                    }
                    if template.carries_style() {
                        row.has_styles = true;
                    }
                    wrote_bulk = true;
                }
            }

            if wrote_bulk {
                let end = cursor_col + take;
                if end < columns {
                    self.grid.cursor.pos.col = Column(end);
                } else {
                    self.grid.cursor.pos.col = Column(columns - 1);
                    self.grid.cursor.should_wrap = true;
                }
                let row = self.grid.cursor.pos.row;
                self.damage.damage_line(row.0 as usize);
            } else {
                for &cp in &cps[idx..idx + take] {
                    let c = char::from_u32(cp).unwrap_or('\u{FFFD}');
                    self.write_codepoint_cell(cp, c, 1);
                }
            }
            idx += take;
        }
    }

    /// Bulk-write a run of width-2 codepoints as (wide, spacer) cell pairs.
    /// Pairs never split across rows; the end-of-row leading-spacer case
    /// defers to the scalar writer.
    fn write_wide_run(
        &mut self,
        template: crate::crosswords::square::Square,
        template_has_extras: bool,
        cps: &[u32],
    ) {
        use crate::crosswords::square::{Square, Wide};

        let mut spacer = Square::from_template(template, ' ');
        spacer.set_wide(Wide::Spacer);

        let mut idx = 0;
        while idx < cps.len() {
            if self.grid.cursor.should_wrap {
                self.wrapline();
            }

            let columns = self.grid.columns();
            let cursor_col = self.grid.cursor.pos.col.0;
            let remaining = columns.saturating_sub(cursor_col);
            if remaining < 2 {
                let cp = cps[idx];
                let c = char::from_u32(cp).unwrap_or('\u{FFFD}');
                self.write_codepoint_cell(cp, c, 2);
                idx += 1;
                continue;
            }

            let take = (cps.len() - idx).min(remaining / 2);
            let mut wrote_bulk = false;
            {
                let line = self.grid.cursor.pos.row;
                let row = &mut self.grid[line];
                let wrapline = row.inner[columns - 1].wrapline();
                let cells = &mut row[Column(cursor_col)..Column(cursor_col + take * 2)];
                let needs_cleanup = Square::any_needs_wide_cleanup(cells);
                if !needs_cleanup {
                    for (pair, &cp) in
                        cells.chunks_exact_mut(2).zip(&cps[idx..idx + take])
                    {
                        let c = char::from_u32(cp).unwrap_or('\u{FFFD}');
                        let mut wide = Square::from_template(template, c);
                        wide.set_wide(Wide::Wide);
                        pair[0] = wide;
                        pair[1] = spacer;
                    }
                    row.inner[columns - 1].set_wrapline(wrapline);
                    if template_has_extras {
                        row.has_extras = true;
                    }
                    if template.carries_style() {
                        row.has_styles = true;
                    }
                    wrote_bulk = true;
                }
            }

            if wrote_bulk {
                let end = cursor_col + take * 2;
                if end < columns {
                    self.grid.cursor.pos.col = Column(end);
                } else {
                    self.grid.cursor.pos.col = Column(columns - 1);
                    self.grid.cursor.should_wrap = true;
                }
                let row = self.grid.cursor.pos.row;
                self.damage.damage_line(row.0 as usize);
                idx += take;
            } else {
                for &cp in &cps[idx..idx + take] {
                    let c = char::from_u32(cp).unwrap_or('\u{FFFD}');
                    self.write_codepoint_cell(cp, c, 2);
                }
                idx += take;
            }
        }
    }

    /// Build the cursor's current style/extras/flags into a zero-codepoint
    /// `Square` template for `write_cell`.
    #[inline]
    fn cell_template(&self) -> crate::crosswords::square::Square {
        let mut t = crate::crosswords::square::Square::default();
        t.set_style_id(self.grid.cursor.template.style_id());
        t.set_extras_id(self.grid.cursor.template.extras_id());
        t.set_cell_flags(self.grid.cursor.template.cell_flags());
        t
    }

    /// The column of the cell most recently written at the cursor: the
    /// cursor column itself while a pending wrap holds the cursor past
    /// the edge, otherwise the column before it, stepping over a wide
    /// pair's spacer to its lead. `None` when no such cell exists.
    fn prev_cell_col(&self) -> Option<usize> {
        let col = self.grid.cursor.pos.col.0;
        let wrap = self.mode.contains(Mode::LINE_WRAP);
        let current = self.grid[self.grid.cursor.pos.row][Column(col)];
        let cluster_at_edge = !wrap
            && self.mode.contains(Mode::GRAPHEME_CLUSTER)
            && col > 0
            && col == self.grid.columns() - 1
            && current.c() != '\0';
        let col = if (wrap && self.grid.cursor.should_wrap) || cluster_at_edge {
            col
        } else {
            col.checked_sub(1)?
        };
        let row = self.grid.cursor.pos.row;
        Some(match self.grid[row][Column(col)].wide() {
            Wide::Spacer => col.checked_sub(1)?,
            _ => col,
        })
    }

    /// Widen the narrow cell preceding the cursor into a wide pair,
    /// handling the row-edge wrap. Shared by VS16 promotion and
    /// mode-2027 cluster continuation (a width-bearing codepoint
    /// joining a narrow cluster makes the whole cluster wide).
    #[inline(never)]
    fn widen_prev_cell(&mut self) -> bool {
        let columns = self.grid.columns();
        // No wide pair fits on one column, so leave the base narrow: the
        // wrap branch below would place the trailing Spacer at column 1 of
        // the new row, off the end of it.
        if columns < 2 {
            return false;
        }
        let row = self.grid.cursor.pos.row;
        let Some(base_col) = self.prev_cell_col() else {
            return false;
        };
        if !matches!(self.grid[row][Column(base_col)].wide(), Wide::Narrow) {
            return true;
        }

        let spacer_col = base_col + 1;
        if spacer_col >= columns {
            // Base is at the final column → no room for a Spacer on this
            // row. Mirror kitty's `move_widened_char_past_multiline_chars`
            // (screen.c): turn
            // the trailing cell into a `LeadingSpacer` (signals "wide char
            // continues on next line"), wrap, and re-place the wide base
            // on the new row, preserving the original cell's style and
            // any extras (zerowidth combining chars attached before VS16).
            if !self.mode.contains(Mode::LINE_WRAP) {
                return false;
            }

            // Snapshot the base cell — `write_at_cursor` below replaces
            // it with a fresh `Square` and would otherwise lose the
            // codepoint, style, and extras_id we want to move.
            let base_snapshot = self.grid[row][Column(base_col)];

            self.grid.cursor.pos.col = Column(base_col);
            self.grid.cursor.should_wrap = false;
            self.write_at_cursor(' ');
            self.grid.cursor_cell().set_wide(Wide::LeadingSpacer);

            self.wrapline();

            let new_row = self.grid.cursor.pos.row;
            let mut moved = base_snapshot;
            moved.set_wide(Wide::Wide);
            // Install the requested width during cleanup, so an existing
            // spacer cannot erase the moved base or its previous-row link.
            self.grid.sync_template_style();
            self.write_cell_with_wide(
                base_snapshot.c(),
                self.cell_template(),
                Wide::Wide,
            );
            self.grid[new_row][Column(0)] = moved;
            // IndexMut doesn't maintain the row hints; without them,
            // reclaim would sweep this cell's live slots.
            if moved.extras_id().is_some() {
                self.grid[new_row].has_extras = true;
            }
            if moved.carries_style() {
                self.grid[new_row].has_styles = true;
            }

            self.grid.cursor.pos.col = Column(1);
            self.write_cell_with_wide(' ', self.cell_template(), Wide::Spacer);

            if 2 < columns {
                self.grid.cursor.pos.col = Column(2);
            } else {
                self.grid.cursor.should_wrap = true;
            }

            self.damage.damage_line(row.0 as usize);
            self.damage.damage_line(new_row.0 as usize);
            return true;
        }

        self.grid[row][Column(base_col)].set_wide(Wide::Wide);

        self.grid.cursor.pos.col = Column(spacer_col);
        self.grid.cursor.should_wrap = false;
        self.write_at_cursor(' ');
        self.grid.cursor_cell().set_wide(Wide::Spacer);

        if spacer_col + 1 < columns {
            self.grid.cursor.pos.col = Column(spacer_col + 1);
        } else {
            self.grid.cursor.should_wrap = true;
        }

        self.damage.damage_line(row.0 as usize);
        true
    }

    /// Shrink the wide pair preceding the cursor back to a narrow cell,
    /// releasing its spacer and retracting the cursor. No-op unless a
    /// wide pair is there. Validity-free counterpart of
    /// [`widen_prev_cell`]: VS15 demotion (via `apply_emoji_vs15`,
    /// which gates on the base) and mode-2027 cluster continuation
    /// (which gates on the cluster's last codepoint) share it.
    fn narrow_prev_cell(&mut self) {
        let row = self.grid.cursor.pos.row;
        let Some(base_col) = self.prev_cell_col() else {
            return;
        };

        if !matches!(self.grid[row][Column(base_col)].wide(), Wide::Wide) {
            return;
        }

        let spacer_col = base_col + 1;
        self.grid[row][Column(base_col)].set_wide(Wide::Narrow);
        self.grid[row][Column(spacer_col)].set_wide(Wide::Narrow);
        self.grid[row][Column(spacer_col)].set_c('\0');

        self.grid.cursor.pos.col = Column(spacer_col);
        self.grid.cursor.should_wrap = false;

        self.damage.damage_line(row.0 as usize);
    }

    /// Read the hyperlink (if any) for the cell at `(line, col)`.
    /// Looks up the cell's `extras_id` in the per-grid extras table.
    /// Used by hint matching (`find_hyperlink_matches`) to locate
    /// clickable OSC 8 link spans on screen.
    #[inline]
    pub fn cell_hyperlink(&self, line: Line, col: Column) -> Option<Hyperlink> {
        let cell = &self.grid[line][col];
        if !cell.has_hyperlink() {
            return None;
        }
        let extras_id = cell.extras_id()?;
        self.grid
            .extras_table
            .get(extras_id)
            .and_then(|e| e.hyperlink.clone())
    }

    /// Read the cell's `extras_id` if it carries a hyperlink. Cheaper
    /// than `cell_hyperlink` for span scans because it returns just the
    /// 16-bit id; matching consecutive cells is then a u16 compare.
    #[inline]
    pub fn cell_hyperlink_id(&self, line: Line, col: Column) -> Option<u16> {
        let cell = &self.grid[line][col];
        if !cell.has_hyperlink() {
            return None;
        }
        cell.extras_id()
    }

    #[inline]
    pub fn visible_rows(&self) -> Vec<Row<Square>> {
        let mut buf = Vec::with_capacity(self.grid.screen_lines());
        self.fill_visible_rows(&mut buf);
        buf
    }

    /// Copy visible state and report whether every source row was modified.
    /// This content hint is independent of full damage from viewport moves;
    /// destination dirty flags retain their existing redraw semantics.
    pub fn snapshot_visible(
        &mut self,
        damage: &crate::event::TerminalDamage,
        dst: &mut Vec<Row<Square>>,
        row_styles: &mut Vec<snapshot::RowStyles>,
        extras: &mut snapshot::SnapshotExtras,
    ) -> bool {
        self.snapshot_visible_impl(damage, dst, row_styles, extras, None)
    }

    /// Capture rows and the style values they need under the terminal lock.
    /// Resolve the returned token after unlocking. Its borrow prevents reading
    /// partially resolved styles; dropping it also completes the snapshot.
    pub fn snapshot_visible_deferred<'a>(
        &mut self,
        damage: &crate::event::TerminalDamage,
        dst: &'a mut Vec<Row<Square>>,
        row_styles: &'a mut Vec<snapshot::RowStyles>,
        extras: &mut snapshot::SnapshotExtras,
        deferred: &'a mut snapshot::DeferredStyles,
    ) -> snapshot::CapturedStyles<'a> {
        let all_rows_modified =
            self.snapshot_visible_impl(damage, dst, row_styles, extras, Some(deferred));
        snapshot::CapturedStyles {
            rows: dst,
            styles: row_styles,
            deferred,
            all_rows_modified,
        }
    }

    fn snapshot_visible_impl(
        &mut self,
        damage: &crate::event::TerminalDamage,
        dst: &mut Vec<Row<Square>>,
        row_styles: &mut Vec<snapshot::RowStyles>,
        extras: &mut snapshot::SnapshotExtras,
        mut deferred: Option<&mut snapshot::DeferredStyles>,
    ) -> bool {
        use crate::event::TerminalDamage;
        let (start, end) = self.visible_line_bounds();
        let count = (end - start) as usize;

        let needs_full = matches!(damage, TerminalDamage::Full)
            || dst.len() != count
            || row_styles.len() != count
            || extras.row_count() != count;

        // A Noop/CursorOnly frame normally has nothing to copy, but rows
        // written after the damage event was consumed (e.g. a graphics
        // insert racing a redraw) still carry their dirty bit.
        let dirty = if needs_full {
            0..count
        } else {
            self.grid.raw.dirty_line_range(Line(start), count)
        };
        if !needs_full && dirty.is_empty() {
            self.grid.raw.acknowledge_snapshot();
            return false;
        }

        if let Some(deferred) = deferred.as_mut() {
            let cells = if self.grid.styles().len() <= 1 {
                0
            } else {
                dirty
                    .clone()
                    .map(|y| &self.grid[Line(start + y as i32)])
                    .filter(|row| row.has_styles && (needs_full || row.dirty))
                    .map(|row| row.inner.len())
                    .sum()
            };
            deferred.begin(self.grid.styles(), cells);
        }

        // Styles are resolved to values per copied row, so the snapshot
        // holds no style ids at all: sweeps reusing ids, table growth,
        // and grid swaps can't affect rows copied on earlier frames.
        if needs_full {
            return self.snapshot_full(dst, row_styles, extras, deferred);
        }

        extras.begin(count, false);
        let mut modified = 0;
        #[allow(clippy::needless_range_loop)]
        for y in dirty {
            let row_idx = start + y as i32;
            if !self.grid[Line(row_idx)].dirty {
                continue;
            }
            let src = &self.grid[Line(row_idx)];
            // Glyphs/flags can change while style and extras IDs stay put.
            // The branchless reduction vectorizes over compact cell words;
            // managed values are still checked against their live tables.
            let unchanged = snapshot::metadata_matches(src, &dst[y]);
            dst[y].copy_snapshot_from(src);
            dst[y].dirty = true;
            if !src.has_styles
                || !deferred
                    .as_mut()
                    .is_some_and(|styles| styles.capture(y, unchanged))
            {
                if unchanged {
                    row_styles[y].resolve_unchanged(src, self.grid.styles());
                } else {
                    self.grid.resolve_row_styles(src, &mut row_styles[y]);
                }
            }
            if unchanged {
                extras.refresh_unchanged(y, &self.grid.extras_table);
            } else {
                extras.refresh(y, src, &self.grid.extras_table);
            }
            self.grid[Line(row_idx)].dirty = false;
            modified += 1;
        }
        self.grid.raw.acknowledge_snapshot();
        modified == count
    }

    // Full capture needs rotation and dependency validation scratch. Keep it
    // out of the small dirty-row path's stack frame and register allocation.
    #[inline(never)]
    fn snapshot_full(
        &mut self,
        dst: &mut Vec<Row<Square>>,
        row_styles: &mut Vec<snapshot::RowStyles>,
        extras: &mut snapshot::SnapshotExtras,
        mut deferred: Option<&mut snapshot::DeferredStyles>,
    ) -> bool {
        let (start, end) = self.visible_line_bounds();
        let count = (end - start) as usize;
        let retained = dst.len() == count
            && row_styles.len() == count
            && extras.row_count() == count;
        if !retained {
            self.fill_visible_rows(dst);
            row_styles.resize_with(count, snapshot::RowStyles::default);
        }
        extras.begin(count, !retained);
        let top = self
            .lines_evicted()
            .saturating_add((self.history_size() - self.display_offset()) as u64);
        extras.align(dst, row_styles, top, self.columns());
        for (i, row_idx) in (start..end).enumerate() {
            let src = &self.grid[Line(row_idx)];
            // Full redraws include scrolling and UI damage. Reuse the
            // moved row's dependencies when only glyphs/flags changed;
            // IDs alone never establish that their values are unchanged.
            let unchanged = retained && snapshot::metadata_matches(src, &dst[i]);
            if retained {
                dst[i].copy_snapshot_from(src);
            }
            dst[i].dirty = true;
            if !src.has_styles
                || !deferred
                    .as_mut()
                    .is_some_and(|styles| styles.capture(i, unchanged))
            {
                if unchanged {
                    row_styles[i].resolve_unchanged(src, self.grid.styles());
                } else {
                    self.grid.resolve_row_styles(src, &mut row_styles[i]);
                }
            }
            if unchanged {
                extras.refresh_unchanged(i, &self.grid.extras_table);
            } else {
                extras.refresh(i, src, &self.grid.extras_table);
            }
        }
        let all_modified = self.grid.raw.take_lines_dirty(Line(start), count);
        self.grid.raw.acknowledge_snapshot();
        all_modified
    }

    /// Half-open grid line range currently on screen, as `(start, end)`.
    ///
    /// DECSTBM limits scrolling, not visibility; using it here drops rows when
    /// a TUI narrows the scrolling region.
    #[inline]
    fn visible_line_bounds(&self) -> (i32, i32) {
        let scroll = self.display_offset() as i32;
        (-scroll, self.grid.screen_lines() as i32 - scroll)
    }

    /// Retain the viewport and each row's allocation across full redraws, so
    /// scrolling and tab switches do not allocate another screen of cells.
    #[inline]
    pub fn fill_visible_rows(&self, dst: &mut Vec<Row<Square>>) {
        let (start, end) = self.visible_line_bounds();
        let count = (end - start) as usize;

        for (i, row_idx) in (start..end).enumerate() {
            let src = &self.grid[Line(row_idx)];
            if let Some(dst_row) = dst.get_mut(i) {
                dst_row.copy_snapshot_from(src);
            } else {
                dst.push(src.clone());
            }
        }
        if dst.len() > count {
            dst.truncate(count);
        }
    }

    /// Get terminal dimensions
    #[inline]
    pub fn columns(&self) -> usize {
        self.grid.columns()
    }

    /// Get terminal screen lines
    #[inline]
    pub fn screen_lines(&self) -> usize {
        self.grid.screen_lines()
    }

    fn deccolm(&mut self)
    where
        U: EventListener,
    {
        // Setting 132 column font makes no sense, but run the other side effects.
        // Clear scrolling region.
        self.set_scrolling_region(1, None);

        // Clear grid.
        self.grid.sync_template_style();
        self.grid.reset_region(..);
        self.mark_fully_damaged();
    }

    /// The active xterm `modifyOtherKeys` level, or `None` when disabled.
    ///
    /// Set by `CSI > 4 ; n m`. An embedder that encodes key events needs this
    /// to know whether the application wants keys like ctrl+enter reported as
    /// `CSI 27 ; …` rather than as their legacy control bytes.
    #[inline]
    pub fn modify_other_keys(&self) -> Option<u8> {
        (self.modify_other_keys > 0).then_some(self.modify_other_keys)
    }

    /// The kitty keyboard protocol flags currently in effect, i.e. the top of
    /// the mode stack. This is the same value the terminal reports back to the
    /// application, so an embedder encoding key events itself does not have to
    /// rebuild the flag byte out of [`Mode`] bits and keep that bit order in
    /// sync by hand.
    #[inline]
    pub fn keyboard_mode(&self) -> KeyboardModes {
        KeyboardModes::from_bits_truncate(
            self.keyboard_mode_stack[self.keyboard_mode_idx],
        )
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    #[inline]
    pub fn cursor(&self) -> CursorState {
        let mut content = self.cursor_shape;
        let vi_mode = self.mode.contains(Mode::VI);
        let scroll = self.display_offset() as i32;
        let mut pos = if vi_mode {
            let mut vi_cursor_pos = self.vi_mode_cursor.pos;
            if scroll > 0 {
                vi_cursor_pos.row += scroll;
            }
            vi_cursor_pos
        } else {
            if scroll != 0 {
                content = CursorShape::Hidden;
            }
            self.grid.cursor.pos
        };
        if matches!(self.grid[pos].wide(), Wide::Spacer) {
            pos.col -= 1;
        }

        // If the cursor is hidden then set content as hidden
        if !vi_mode && !self.mode.contains(Mode::SHOW_CURSOR) {
            content = CursorShape::Hidden;
        }

        CursorState { pos, content }
    }

    pub fn swap_alt(&mut self) {
        let entering = !self.mode.contains(Mode::ALT_SCREEN);
        self.screen_mode_state[3] = entering;
        if entering {
            self.save_cursor_position();
            self.swap_alt_inner(false);
            self.clear_screen(ClearMode::All);
            // Clear with the alternate screen's old pen, then carry over
            // the primary cursor and re-intern its style in this grid.
            self.grid.cursor = self.inactive_grid.cursor.clone();
            self.grid.cursor.style_dirty = true;
            self.grid.sync_template_style();
        } else {
            self.swap_alt_inner(false);
        }
    }

    fn swap_alt_inner(&mut self, copy_cursor: bool) {
        // Written cells keep their links; the outgoing cursor ends its link.
        self.set_hyperlink(None);
        if copy_cursor {
            self.inactive_grid.cursor = self.grid.cursor.clone();
            // Style ids belong to a grid. Re-intern the copied style below.
            self.inactive_grid.cursor.style_dirty = true;
        }

        mem::swap(
            &mut self.keyboard_mode_stack,
            &mut self.inactive_keyboard_mode_stack,
        );
        mem::swap(
            &mut self.keyboard_mode_idx,
            &mut self.inactive_keyboard_mode_idx,
        );

        mem::swap(&mut self.grid, &mut self.inactive_grid);
        self.grid.sync_template_style();
        self.mode ^= Mode::ALT_SCREEN;
        self.selection = None;

        // Swap kitty graphics state per screen so each screen owns its
        // own image cache, placements, number map, and virtual placements.
        // (Marks the overlay layer dirty as a side effect so the renderer
        // rebuilds against the new active screen.)
        if self.graphics.swap_kitty_screen_state() {
            self.send_graphics_updates();
        }
        self.mark_fully_damaged();
    }

    fn report_cursor_position(&self, private: bool) {
        let pos = self.grid.cursor.pos;
        let row = if self.mode.contains(Mode::ORIGIN) {
            pos.row.0.saturating_sub(self.scroll_region.start.0).max(0)
        } else {
            pos.row.0
        };
        let text = if private {
            // DECXCPR includes a page number; there is only one page.
            format!("\x1b[?{};{};1R", row + 1, pos.col + 1)
        } else {
            format!("\x1b[{};{}R", row + 1, pos.col + 1)
        };
        self.event_proxy
            .send_event(RioEvent::PtyWrite(self.route_id, text), self.window_id);
    }

    #[inline]
    pub fn mark_line_damaged(&mut self, line: Line) {
        let line_idx = line.0 as usize;
        self.damage.damage_line(line_idx);
    }

    pub fn selection_to_string(&self) -> Option<String> {
        let selection_range = self.selection.as_ref().and_then(|s| s.to_range(self))?;
        let SelectionRange { start, end, .. } = selection_range;

        let mut res = String::new();

        match self.selection.as_ref() {
            Some(Selection {
                ty: SelectionType::Block,
                ..
            }) => {
                for line in (start.row.0..end.row.0).map(Line::from) {
                    res +=
                        &self.line_to_string(line, start.col..end.col, start.col.0 != 0);
                    res += "\n";
                }

                res += &self.line_to_string(end.row, start.col..end.col, true);
            }
            Some(Selection {
                ty: SelectionType::Lines,
                ..
            }) => {
                res = self.bounds_to_string(start, end) + "\n";
            }
            _ => {
                res = self.bounds_to_string(start, end);
            }
        }

        Some(res)
    }

    pub fn bounds_to_string(&self, start: Pos, end: Pos) -> String {
        let mut text = String::new();
        self.bounds_to_string_into(start, end, &mut text);
        text
    }

    fn bounds_to_string_into(&self, start: Pos, end: Pos, text: &mut String) {
        text.clear();
        let mut blank_rows: usize = 0;
        let mut blank_cells: usize = 0;
        let last_col = self.grid.last_column();

        for line in (start.row.0..=end.row.0).map(Line::from) {
            let start_col = if line == start.row {
                start.col
            } else {
                Column(0)
            };
            let end_col = if line == end.row { end.col } else { last_col };

            // Carry buffered blank cells across wrap continuations only.
            // Without this, `aaa \n aaa"` (where row N wraps into N+1) would
            // collapse the cross-row gap from two spaces to one.
            let is_wrap_continuation =
                line.0 > start.row.0 && self.grid[line - 1i32][last_col].wrapline();
            if !is_wrap_continuation {
                blank_cells = 0;
            }

            let row_start = text.len();
            let had_content = self.append_cells(
                text,
                line,
                start_col..end_col,
                line == end.row,
                &mut blank_cells,
            );

            if !had_content {
                // Defer entirely-blank rows; trailing blank rows get dropped.
                blank_rows += 1;
                continue;
            }

            if blank_rows != 0 {
                text.insert_str(row_start, &"\n".repeat(blank_rows));
            }
            blank_rows = 0;

            let cur_wraps = self.grid[line][last_col].wrapline();
            if end_col >= last_col && !cur_wraps {
                text.push('\n');
                blank_cells = 0;
            }
        }

        if text.ends_with('\n') {
            text.pop();
        }
    }

    /// Convert a single line in the grid to a String. Used by Block selection;
    /// trailing blank cells are dropped. No trailing newline is appended —
    /// the caller controls row separation.
    fn line_to_string(
        &self,
        line: Line,
        cols: Range<Column>,
        include_wrapped_wide: bool,
    ) -> String {
        let mut text = String::new();
        let mut blank_cells = 0;
        self.append_cells(
            &mut text,
            line,
            cols,
            include_wrapped_wide,
            &mut blank_cells,
        );
        text
    }

    /// Append cells from a single line to `text`, buffering blank cells
    /// (`\0` and trailing spaces) so that:
    /// - `\0` cells inside a run of content become real spaces
    /// - trailing blanks at end of the run are dropped (caller decides
    ///   whether to flush them via the `blank_cells` accumulator)
    ///
    /// Returns true if the line emitted any non-blank content.
    fn append_cells(
        &self,
        text: &mut String,
        line: Line,
        mut cols: Range<Column>,
        include_wrapped_wide: bool,
        blank_cells: &mut usize,
    ) -> bool {
        let mut had_content = false;
        let grid_line = &self.grid[line];
        let line_length = std::cmp::min(grid_line.line_length(), cols.end + 1);

        // Include wide char when trailing spacer is selected.
        if cols.start < self.grid.columns()
            && matches!(grid_line[cols.start].wide(), Wide::Spacer)
        {
            cols.start -= 1;
        }

        let mut tab_mode = false;
        let cells = grid_line
            .inner
            .get(cols.start.0..line_length.0)
            .unwrap_or_default();
        // Copy ordinary ASCII rows as one UTF-8 span. The general selection
        // path below still owns tabs, wide spacers and combining characters.
        // Preserve its deferred trailing spaces across soft-wrapped rows.
        if !grid_line.has_extras && cells.len() <= 256 {
            let mut ascii = [0u8; 256];
            let mut end = 0;
            let ordinary =
                cells
                    .iter()
                    .zip(&mut ascii)
                    .enumerate()
                    .all(|(i, (cell, byte))| {
                        let c = cell.c();
                        if c > '\u{7f}' || c == '\t' || cell.wide() != Wide::Narrow {
                            return false;
                        }
                        *byte = if c == '\0' { b' ' } else { c as u8 };
                        if *byte != b' ' {
                            end = i + 1;
                        }
                        true
                    });
            if ordinary {
                if end != 0 {
                    for _ in 0..*blank_cells {
                        text.push(' ');
                    }
                    // SAFETY: every stored byte is ASCII.
                    text.push_str(unsafe {
                        std::str::from_utf8_unchecked(&ascii[..end])
                    });
                    *blank_cells = cells.len() - end;
                    return true;
                }
                *blank_cells += cells.len();
                return false;
            }
        }
        for (offset, cell) in cells.iter().enumerate() {
            let column = Column(cols.start.0 + offset);

            // Skip over cells until next tab-stop once a tab was found.
            if tab_mode {
                if self.tabs[column] || cell.c() != '\0' {
                    tab_mode = false;
                } else {
                    continue;
                }
            }

            if cell.c() == '\t' {
                tab_mode = true;
            }

            if matches!(cell.wide(), Wide::Spacer | Wide::LeadingSpacer) {
                continue;
            }

            let c = cell.c();
            let has_extras = cell.has_extras();

            // Buffer blank cells. They only get emitted as real spaces if a
            // non-blank cell follows (on this row or a wrap continuation).
            if !has_extras && (c == '\0' || c == ' ') {
                *blank_cells += 1;
                continue;
            }

            for _ in 0..*blank_cells {
                text.push(' ');
            }
            *blank_cells = 0;

            text.push(c);
            if let Some(extras_id) = cell.extras_id_checked() {
                if let Some(extras) = self.grid.extras_table.get(extras_id) {
                    for c in &extras.zerowidth {
                        text.push(*c);
                    }
                }
            }
            had_content = true;
        }

        // If wide char is not part of the selection, but leading spacer is, include it.
        if line_length == self.grid.columns()
            && line_length.0 >= 2
            && matches!(grid_line[line_length - 1].wide(), Wide::LeadingSpacer)
            && include_wrapped_wide
        {
            for _ in 0..*blank_cells {
                text.push(' ');
            }
            *blank_cells = 0;
            text.push(self.grid[line - 1i32][Column(0)].c());
            had_content = true;
        }

        had_content
    }

    #[inline]
    fn set_keyboard_mode(&mut self, mode: u8, apply: KeyboardModesApplyBehavior) {
        // println!("{:?}", mode);
        let active_mode = self.keyboard_mode_stack[self.keyboard_mode_idx];
        let new_mode = match apply {
            KeyboardModesApplyBehavior::Replace => mode,
            KeyboardModesApplyBehavior::Union => active_mode | mode,
            KeyboardModesApplyBehavior::Difference => active_mode & !mode,
        };
        info!("Setting keyboard mode to {new_mode:?}");
        self.keyboard_mode_stack[self.keyboard_mode_idx] = new_mode;

        // Sync self.mode with keyboard_mode_stack
        self.mode &= !Mode::KITTY_KEYBOARD_PROTOCOL;
        self.mode |= Mode::from(KeyboardModes::from_bits_truncate(new_mode));
    }

    /// Find the beginning of the current line across linewraps.
    pub fn row_search_left(&self, mut point: Pos) -> Pos {
        while point.row > self.grid.topmost_line()
            && self.grid[point.row - 1i32][self.grid.last_column()].wrapline()
        {
            point.row -= 1;
        }

        point.col = Column(0);

        point
    }

    /// Find the end of the current line across linewraps.
    pub fn row_search_right(&self, mut point: Pos) -> Pos {
        while point.row + 1 < self.grid.screen_lines()
            && self.grid[point.row][self.grid.last_column()].wrapline()
        {
            point.row += 1;
        }

        point.col = self.grid.last_column();

        point
    }
}

impl<U: EventListener> Crosswords<U> {
    pub fn set_clipboard_policy(&mut self, policy: crate::clipboard::ClipboardPolicy) {
        self.clipboard_policy = policy;
        if policy.write == crate::clipboard::Access::Deny {
            self.kitty_clipboard_session.cancel_write();
        }
    }

    pub fn clipboard_policy(&self) -> crate::clipboard::ClipboardPolicy {
        self.clipboard_policy
    }

    pub fn invalidate_clipboard_requests(&mut self) {
        self.kitty_clipboard_session.reset();
    }

    pub fn clipboard_generation(&self) -> u64 {
        self.kitty_clipboard_session.generation()
    }

    pub fn clipboard_search_paste(
        &self,
        generation: u64,
        search_generation: u64,
        text: Arc<crate::clipboard::buffer::ResponseBuffer>,
    ) {
        if generation == self.clipboard_generation() {
            self.event_proxy.send_event(
                RioEvent::ClipboardSearchPaste(
                    self.route_id,
                    generation,
                    search_generation,
                    text,
                ),
                self.window_id,
            );
        }
    }

    fn emit_clipboard_reply(&self, bytes: &[u8]) {
        use crate::clipboard::buffer::{Budget, ResponseBuffer};
        use std::io::Write;
        let mut response = ResponseBuffer::new(Budget::global());
        if response.write_all(bytes).is_ok() {
            self.event_proxy.send_event(
                RioEvent::ClipboardReply(self.route_id, Arc::new(response)),
                self.window_id,
            );
        }
    }

    pub fn clipboard_paste_event(
        &mut self,
        location: ClipboardType,
        available: &[String],
    ) -> std::io::Result<Option<crate::clipboard::buffer::ResponseBuffer>> {
        if !self.mode.contains(Mode::KITTY_PASTE_EVENTS) {
            return Ok(None);
        }
        self.kitty_clipboard_session
            .paste_event(location, available)
            .map(Some)
    }

    pub fn clipboard_request_current(
        &self,
        request: &crate::clipboard::kitty::Request,
    ) -> bool {
        self.kitty_clipboard_session.current(request)
    }
    pub fn clipboard_request_granted(
        &mut self,
        request: &crate::clipboard::kitty::Request,
    ) -> bool {
        self.kitty_clipboard_session.granted(request)
    }
    pub fn approve_clipboard_request(
        &mut self,
        request: &crate::clipboard::kitty::Request,
    ) -> bool {
        self.kitty_clipboard_session.approve(request)
    }
    /// Ids of graphics still displayed (atlas placements + kitty).
    fn collect_used_graphic_ids(&mut self) -> std::collections::HashSet<u64> {
        self.graphics.collect_active_graphic_ids()
    }

    fn split_erase_boundary(&mut self, row: Line, col: usize, blank: Square) {
        // EL/ED do not sever the spacer head on the preceding wrapped row.
        if col > 0
            && col < self.grid.columns()
            && self.grid[row][Column(col - 1)].wide() == Wide::Wide
        {
            self.clear_pair_preserving_wrapline(row, col - 1, blank);
        }
    }

    fn split_ech_boundary(&mut self, row: Line, col: usize, blank: Square) {
        let last = self.grid.last_column();
        if col <= 1
            && self.grid[row].wrap_continuation
            && self.grid[row][Column(0)].wide() == Wide::Wide
            && row != self.grid.topmost_line()
            && self.grid[row - 1i32][last].wide() == Wide::LeadingSpacer
        {
            let wrapped = self.grid[row - 1i32][last].wrapline();
            self.grid[row - 1i32][last] = blank;
            self.grid[row - 1i32][last].set_wrapline(wrapped);
            if row.0 > 0 {
                self.damage.damage_line(row.0 as usize - 1);
            }
        }
        if col >= self.grid.columns()
            && self.grid[row][last].wrapline()
            && self.grid[row][last].wide() == Wide::LeadingSpacer
        {
            let wrapped = self.grid[row][last].wrapline();
            self.grid[row][last] = blank;
            self.grid[row][last].set_wrapline(wrapped);
        }
        self.split_wide_seam(row, col, blank);
    }

    fn reset_erase_wrap(&mut self, row: Line, blank: Square) {
        let last = self.grid.last_column();
        if !self.grid[row][last].wrapline() {
            return;
        }
        self.grid[row][last].set_wrapline(false);
        if row.0 + 1 < self.grid.screen_lines() as i32 {
            self.grid[row + 1i32].wrap_continuation = false;
            self.damage.damage_line(row.0 as usize + 1);
        }
        if self.grid[row][last].wide() == Wide::LeadingSpacer {
            self.grid[row][last] = blank;
        }
    }

    fn erase_unprotected_range(
        &mut self,
        row: Line,
        left: usize,
        right: usize,
        blank: Square,
        preserve_wrap: bool,
    ) {
        use crate::crosswords::square::CellFlags;
        if !preserve_wrap {
            self.reset_erase_wrap(row, blank);
        }
        let last = self.grid.last_column();
        let wrapped = self.grid[row][last].wrapline();
        let mut col = left;
        while col < right {
            if self.grid[row][Column(col)]
                .cell_flags()
                .contains(CellFlags::PROTECTED)
            {
                col += 1;
                continue;
            }
            let start = col;
            col += 1;
            while col < right
                && !self.grid[row][Column(col)]
                    .cell_flags()
                    .contains(CellFlags::PROTECTED)
            {
                col += 1;
            }
            self.split_erase_boundary(row, start, blank);
            self.split_erase_boundary(row, col, blank);
            self.grid[row].has_styles |= blank.carries_style();
            for cell in &mut self.grid[row][Column(start)..Column(col)] {
                *cell = blank;
            }
            self.clip_atlas_placements(row.0, row.0 + 1, start, col);
        }
        self.grid[row][last].set_wrapline(preserve_wrap && wrapped);
        self.damage.damage_line(row.0 as usize);
    }

    fn erase_display(&mut self, mode: ClearMode, protected: bool) {
        use crate::crosswords::grid::row::SemanticPrompt;

        if !matches!(mode, ClearMode::Saved) {
            self.grid.cursor.should_wrap = false;
        }
        self.grid.sync_template_style();
        let bg = self.grid.template_style().bg;
        let blank = self.grid.blank_with_bg(bg);

        let screen_lines = self.grid.screen_lines();

        match mode {
            ClearMode::Above => {
                let cursor = self.grid.cursor.pos;

                if cursor.row > 0 {
                    // Fully clear all lines before the current line.
                    if protected {
                        for row in 0..cursor.row.0 {
                            self.erase_unprotected_range(
                                Line(row),
                                0,
                                self.grid.columns(),
                                blank,
                                true,
                            );
                        }
                    } else {
                        self.grid.reset_region(..cursor.row);
                    }
                }

                let wrapped = self.grid[cursor.row][self.grid.last_column()].wrapline();
                // Clear up to the current column in the current line.
                let end = std::cmp::min(cursor.col + 1, Column(self.grid.columns()));
                if protected {
                    self.erase_unprotected_range(cursor.row, 0, end.0, blank, true);
                } else {
                    self.split_erase_boundary(cursor.row, 0, blank);
                    self.split_erase_boundary(cursor.row, end.0, blank);
                    self.grid[cursor.row].has_styles |= blank.carries_style();
                    for cell in &mut self.grid[cursor.row][..end] {
                        *cell = blank;
                    }
                }

                let last = self.grid.last_column();
                self.grid[cursor.row][last].set_wrapline(wrapped);
                let range = Line(0)..=cursor.row;
                self.selection =
                    self.selection.take().filter(|s| !s.intersects_range(range));

                let columns = self.grid.columns();
                self.clip_atlas_placements(0, cursor.row.0, 0, columns);
                self.clip_atlas_placements(cursor.row.0, cursor.row.0 + 1, 0, end.0);
            }
            ClearMode::Below => {
                let cursor = self.grid.cursor.pos;
                self.reset_erase_wrap(cursor.row, blank);
                if protected {
                    self.erase_unprotected_range(
                        cursor.row,
                        cursor.col.0,
                        self.grid.columns(),
                        blank,
                        false,
                    );
                } else {
                    self.split_erase_boundary(cursor.row, cursor.col.0, blank);
                    self.grid[cursor.row].has_styles |= blank.carries_style();
                    for cell in &mut self.grid[cursor.row][cursor.col..] {
                        *cell = blank;
                    }
                }

                if (cursor.row.0 as usize) < screen_lines - 1 {
                    if protected {
                        for row in cursor.row.0 + 1..screen_lines as i32 {
                            self.erase_unprotected_range(
                                Line(row),
                                0,
                                self.grid.columns(),
                                blank,
                                true,
                            );
                        }
                    } else {
                        self.grid.reset_region((cursor.row + 1)..);
                    }
                }

                let range = cursor.row..Line(screen_lines as i32);
                self.selection =
                    self.selection.take().filter(|s| !s.intersects_range(range));

                let columns = self.grid.columns();
                self.clip_atlas_placements(
                    cursor.row.0,
                    cursor.row.0 + 1,
                    cursor.col.0,
                    columns,
                );
                self.clip_atlas_placements(
                    cursor.row.0 + 1,
                    screen_lines as i32,
                    0,
                    columns,
                );
            }
            ClearMode::All | ClearMode::Scroll => {
                // Like Ghostty, ED2 preserves screen contents in history
                // only when the last nonempty row belongs to a prompt.
                // ED22 explicitly requests that behavior for any content.
                let at_prompt = (0..screen_lines)
                    .rev()
                    .find_map(|row| {
                        let row = &self.grid[Line(row as i32)];
                        if row.semantic_prompt != SemanticPrompt::None {
                            Some(true)
                        } else if row.inner.iter().any(|cell| {
                            !cell.is_bg_only()
                                && !matches!(cell.c(), '\0' | '\t')
                                && !matches!(
                                    cell.wide(),
                                    Wide::Spacer | Wide::LeadingSpacer
                                )
                        }) {
                            Some(false)
                        } else {
                            None
                        }
                    })
                    .unwrap_or(false);
                if self.mode.contains(Mode::ALT_SCREEN)
                    || (!matches!(mode, ClearMode::Scroll) && !at_prompt)
                {
                    if protected && matches!(mode, ClearMode::All) {
                        for row in 0..screen_lines {
                            self.erase_unprotected_range(
                                Line(row as i32),
                                0,
                                self.grid.columns(),
                                blank,
                                true,
                            );
                        }
                    } else {
                        self.grid.reset_region(..);
                    }
                    let columns = self.grid.columns();
                    self.clip_atlas_placements(0, screen_lines as i32, 0, columns);
                } else {
                    // The viewport scrolls into history below; atlas
                    // placements are absolutely anchored and follow
                    // their content into scrollback untouched.
                    let old_offset = self.grid.display_offset();

                    self.grid.clear_viewport();

                    // Compute number of lines scrolled by clearing the viewport.
                    let lines = self.grid.display_offset().saturating_sub(old_offset);

                    self.vi_mode_cursor.pos.row = (self.vi_mode_cursor.pos.row - lines)
                        .grid_clamp(&self.grid, Boundary::Grid);
                }

                // Erase any image content still visible after the optional
                // scroll. Placements wholly in history remain intact.
                self.clip_atlas_placements(
                    0,
                    screen_lines as i32,
                    0,
                    self.grid.columns(),
                );
                if !self.graphics.kitty_images.is_empty() {
                    self.delete_graphics(
                        crate::ansi::kitty_graphics_protocol::DeleteRequest {
                            frame_number: 0,
                            action: b'A',
                            image_id: 0,
                            image_number: 0,
                            placement_id: 0,
                            x: 0,
                            y: 0,
                            z_index: 0,
                            delete_data: true,
                        },
                    );
                    // Erasing the display also reclaims stored images that
                    // have no remaining direct or virtual placement.
                    let used: std::collections::HashSet<_> = self
                        .graphics
                        .kitty_placements
                        .keys()
                        .chain(self.graphics.kitty_virtual_placements.keys())
                        .map(|key| key.0)
                        .collect();
                    self.graphics
                        .delete_kitty_images(|id, _| !used.contains(id));
                    self.send_graphics_updates();
                }
                if matches!(mode, ClearMode::Scroll) {
                    self.grid.cursor.pos = Pos::new(Line(0), Column(0));
                }
                self.grid.cursor.should_wrap = false;
                self.selection = None;
            }
            ClearMode::Saved if self.history_size() > 0 => {
                self.clear_kitty_history_placements(false);
                self.grid.clear_history();
                self.expire_kitty_placements();
                self.expire_atlas_placements();

                self.vi_mode_cursor.pos.row = self
                    .vi_mode_cursor
                    .pos
                    .row
                    .grid_clamp(&self.grid, Boundary::Cursor);

                self.selection = self
                    .selection
                    .take()
                    .filter(|s| !s.intersects_range(..Line(0)));
            }
            // We have no history to clear.
            ClearMode::Saved => (),
        }

        // Mark affected lines as damaged based on clear mode
        match mode {
            ClearMode::Above => {
                let cursor_row = self.grid.cursor.pos.row.0 as usize;
                for line in 0..=cursor_row {
                    self.damage.damage_line(line);
                }
            }
            ClearMode::Below => {
                let cursor_row = self.grid.cursor.pos.row.0 as usize;
                for line in cursor_row..screen_lines {
                    self.damage.damage_line(line);
                }
            }
            ClearMode::All | ClearMode::Scroll | ClearMode::Saved => {
                self.mark_fully_damaged();
            }
        }
        if !self.graphics.kitty_placements.is_empty() {
            self.graphics.kitty_graphics_dirty = true;
        }
    }

    fn erase_line(&mut self, mode: LineClearMode, protected: bool) {
        let bg = self.grid.template_style().bg;
        let blank = self.grid.blank_with_bg(bg);
        let point = self.grid.cursor.pos;
        self.grid.cursor.should_wrap = false;

        let (left, right) = match mode {
            LineClearMode::Right => (point.col, Column(self.grid.columns())),
            LineClearMode::Left => (Column(0), point.col + 1),
            LineClearMode::All => (Column(0), Column(self.grid.columns())),
        };

        self.damage.damage_line(point.row.0 as usize);
        let wrapped = self.grid[point.row][self.grid.last_column()].wrapline();
        if !matches!(mode, LineClearMode::Left) {
            self.reset_erase_wrap(point.row, blank);
        }

        if protected {
            let left = if left.0 > 0 && self.grid[point.row][left].wide() == Wide::Spacer
            {
                left - 1
            } else {
                left
            };
            let right = if right.0 < self.grid.columns()
                && self.grid[point.row][right - 1].wide() == Wide::Wide
            {
                right + 1
            } else {
                right
            };
            self.erase_unprotected_range(
                point.row,
                left.0,
                right.0,
                blank,
                matches!(mode, LineClearMode::Left),
            );
        } else {
            self.split_erase_boundary(point.row, left.0, blank);
            self.split_erase_boundary(point.row, right.0, blank);
            let row = &mut self.grid[point.row];
            row.has_styles |= blank.carries_style();
            for cell in &mut row[left..right] {
                *cell = blank;
            }
        }
        if matches!(mode, LineClearMode::Left) {
            let last = self.grid.last_column();
            self.grid[point.row][last].set_wrapline(wrapped);
        }
        let range = self.grid.cursor.pos.row..=self.grid.cursor.pos.row;
        self.selection = self.selection.take().filter(|s| !s.intersects_range(range));
        if !protected {
            self.clip_atlas_placements(point.row.0, point.row.0 + 1, left.0, right.0);
        }
        if !self.graphics.kitty_placements.is_empty() {
            self.graphics.kitty_graphics_dirty = true;
        }
    }

    fn clear_kitty_history_placements(&mut self, inactive: bool) {
        let grid = if inactive {
            &self.inactive_grid
        } else {
            &self.grid
        };
        if grid.history_size() == 0 {
            return;
        }
        let base = grid.lines_evicted() as i64;
        let top = base + grid.history_size() as i64;
        let placements = if inactive {
            &mut self.graphics.kitty_inactive_screen.kitty_placements
        } else {
            &mut self.graphics.kitty_placements
        };
        let before = placements.len();
        placements
            .retain(|_, p| p.relative.is_some() || (!p.expired && p.dest_row >= top));
        let changed = before != placements.len();
        self.graphics.remove_orphaned_placements();
        self.graphics.kitty_graphics_dirty |= changed && !inactive;
    }

    fn expire_kitty_placements(&mut self) {
        let base = self.grid.lines_evicted() as i64;
        for placement in self.graphics.kitty_placements.values_mut() {
            if !placement.expired
                && placement.relative.is_none()
                && placement.dest_row < base
            {
                placement.expired = true;
                self.graphics.kitty_graphics_dirty = true;
            }
        }
        let base = self.inactive_grid.lines_evicted() as i64;
        for placement in self
            .graphics
            .kitty_inactive_screen
            .kitty_placements
            .values_mut()
        {
            if placement.relative.is_none() && placement.dest_row < base {
                placement.expired = true;
            }
        }
    }
}

impl<U: EventListener> Handler for Crosswords<U> {
    fn kitty_animation_image_identity(
        &self,
        image_id: u32,
        image_number: u32,
    ) -> Result<crate::ansi::kitty_animation::ImageIdentity, &'static str> {
        self.graphics.kitty_image_identity(image_id, image_number)
    }
    fn kitty_image_by_number(&self, number: u32) -> Option<u32> {
        self.graphics.kitty_image_numbers.get(&number).copied()
    }
    fn kitty_assign_image_number(&mut self, image_id: u32, number: u32) {
        self.graphics.kitty_image_numbers.insert(number, image_id);
    }
    fn kitty_animation(
        &mut self,
        request: crate::ansi::kitty_animation::Request,
    ) -> Result<(u32, Option<u32>), crate::ansi::kitty_animation::Failure> {
        if let Some(target) = request.target {
            if self
                .graphics
                .kitty_image_identity(request.image_id, request.image_number)?
                != target
            {
                return Err("ENOENT: image not found".into());
            }
        }
        let result = self.graphics.execute_animation(
            request.image_id,
            request.image_number,
            request.command,
        )?;
        self.mark_fully_damaged();
        self.send_graphics_updates();
        Ok(result)
    }
    fn finish_input_batch(&mut self) {
        self.grid.enforce_history_byte_limit();
        self.inactive_grid.enforce_history_byte_limit();
        self.expire_kitty_placements();
    }

    #[inline]
    fn set_mode(&mut self, mode: AnsiMode) {
        let mode = match mode {
            AnsiMode::Named(mode) => mode,
            AnsiMode::Unknown(mode) => {
                debug!("Ignoring unknown mode {} in set_mode", mode);
                return;
            }
        };

        trace!("Setting public mode: {:?}", mode);
        match mode {
            NamedMode::Insert => self.mode.insert(Mode::INSERT),
            NamedMode::LineFeedNewLine => self.mode.insert(Mode::LINE_FEED_NEW_LINE),
        }
    }

    #[inline]
    fn unset_mode(&mut self, mode: AnsiMode) {
        let mode = match mode {
            AnsiMode::Named(mode) => mode,
            AnsiMode::Unknown(mode) => {
                debug!("Ignoring unknown mode {} in unset_mode", mode);
                return;
            }
        };

        trace!("Setting public mode: {:?}", mode);
        match mode {
            NamedMode::Insert => {
                self.mode.remove(Mode::INSERT);
                self.mark_fully_damaged();
            }
            NamedMode::LineFeedNewLine => self.mode.remove(Mode::LINE_FEED_NEW_LINE),
        }
    }

    #[inline]
    fn report_mode(&mut self, mode: AnsiMode) {
        trace!("Reporting mode {mode:?}");
        let state = match mode {
            AnsiMode::Named(mode) => match mode {
                NamedMode::Insert => self.mode.contains(Mode::INSERT).into(),
                NamedMode::LineFeedNewLine => {
                    self.mode.contains(Mode::LINE_FEED_NEW_LINE).into()
                }
            },
            AnsiMode::Unknown(_) => ModeState::NotSupported,
        };

        self.event_proxy.send_event(
            RioEvent::PtyWrite(
                self.route_id,
                format!("\x1b[{};{}$y", mode.raw(), state as u8,),
            ),
            self.window_id,
        );
    }

    #[inline]
    fn set_private_mode(&mut self, mode: PrivateMode) {
        let mode = match mode {
            PrivateMode::Named(mode) => mode,

            // SixelDisplay
            PrivateMode::Unknown(80) => {
                self.mode.insert(Mode::SIXEL_DISPLAY);
                return;
            }

            // SixelPrivateColorRegisters
            PrivateMode::Unknown(1070) => {
                self.mode.insert(Mode::SIXEL_PRIV_PALETTE);
                return;
            }

            // SixelCursorToTheRight
            PrivateMode::Unknown(8452) => {
                self.mode.insert(Mode::SIXEL_CURSOR_TO_THE_RIGHT);
                return;
            }

            PrivateMode::Unknown(mode) => {
                debug!("Ignoring unknown mode {} in set_private_mode", mode);
                return;
            }
        };

        trace!("Setting private mode: {:?}", mode);
        match mode {
            NamedPrivateMode::UrgencyHints => self.mode.insert(Mode::URGENCY_HINTS),
            NamedPrivateMode::SwapScreen | NamedPrivateMode::SwapScreenAndClear => {
                self.screen_mode_state
                    [usize::from(mode == NamedPrivateMode::SwapScreenAndClear)] = true;
                if !self.mode.contains(Mode::ALT_SCREEN) {
                    self.swap_alt_inner(true);
                }
            }
            NamedPrivateMode::SaveRestoreCursor => {
                self.screen_mode_state[2] = true;
                self.save_cursor_position();
            }
            NamedPrivateMode::SwapScreenAndSetRestoreCursor => {
                self.screen_mode_state[3] = true;
                if !self.mode.contains(Mode::ALT_SCREEN) {
                    self.swap_alt();
                } else {
                    self.save_cursor_position();
                    self.clear_screen(ClearMode::All);
                }
            }
            NamedPrivateMode::ShowCursor => self.mode.insert(Mode::SHOW_CURSOR),
            NamedPrivateMode::CursorKeys => self.mode.insert(Mode::APP_CURSOR),
            // Mouse protocols are mutually exclusive.
            NamedPrivateMode::ReportX10MouseClicks => {
                self.mode.remove(Mode::MOUSE_MODE);
                self.mode.insert(Mode::MOUSE_REPORT_X10);
                self.event_proxy
                    .send_event(RioEvent::MouseCursorDirty, self.window_id);
            }
            NamedPrivateMode::ReportMouseClicks => {
                self.mode.remove(Mode::MOUSE_MODE);
                self.mode.insert(Mode::MOUSE_REPORT_CLICK);
                self.event_proxy
                    .send_event(RioEvent::MouseCursorDirty, self.window_id);
            }
            NamedPrivateMode::ReportCellMouseMotion => {
                self.mode.remove(Mode::MOUSE_MODE);
                self.mode.insert(Mode::MOUSE_DRAG);
                self.event_proxy
                    .send_event(RioEvent::MouseCursorDirty, self.window_id);
            }
            NamedPrivateMode::ReportAllMouseMotion => {
                self.mode.remove(Mode::MOUSE_MODE);
                self.mode.insert(Mode::MOUSE_MOTION);
                self.event_proxy
                    .send_event(RioEvent::MouseCursorDirty, self.window_id);
            }
            NamedPrivateMode::ReportFocusInOut => self.mode.insert(Mode::FOCUS_IN_OUT),
            NamedPrivateMode::BracketedPaste => self.mode.insert(Mode::BRACKETED_PASTE),
            NamedPrivateMode::KittyPasteEvents => {
                self.mode.insert(Mode::KITTY_PASTE_EVENTS)
            }
            // Mouse encodings are mutually exclusive.
            NamedPrivateMode::SgrMouse => {
                self.mode.remove(Mode::UTF8_MOUSE);
                self.mode.insert(Mode::SGR_MOUSE);
            }
            NamedPrivateMode::Utf8Mouse => {
                self.mode.remove(Mode::SGR_MOUSE);
                self.mode.insert(Mode::UTF8_MOUSE);
            }
            NamedPrivateMode::AlternateScroll => self.mode.insert(Mode::ALTERNATE_SCROLL),
            NamedPrivateMode::LineWrap => self.mode.insert(Mode::LINE_WRAP),
            NamedPrivateMode::Origin => {
                self.mode.insert(Mode::ORIGIN);
                self.goto(Line(0), Column(0));
            }
            NamedPrivateMode::ColumnMode => self.deccolm(),
            NamedPrivateMode::BlinkingCursor => {
                self.blinking_cursor = true;
                self.event_proxy
                    .send_event(RioEvent::CursorBlinkingChange, self.window_id);
            }
            NamedPrivateMode::GraphemeCluster => self.mode.insert(Mode::GRAPHEME_CLUSTER),
            NamedPrivateMode::SyncUpdate => (),
        }
    }

    #[inline]
    fn unset_private_mode(&mut self, mode: PrivateMode) {
        let mode = match mode {
            PrivateMode::Named(mode) => mode,

            // SixelDisplay
            PrivateMode::Unknown(80) => {
                self.mode.remove(Mode::SIXEL_DISPLAY);
                return;
            }

            // SixelPrivateColorRegisters
            PrivateMode::Unknown(1070) => {
                self.graphics.sixel_shared_palette = None;
                self.mode.remove(Mode::SIXEL_PRIV_PALETTE);
                return;
            }

            // SixelCursorToTheRight
            PrivateMode::Unknown(8452) => {
                self.mode.remove(Mode::SIXEL_CURSOR_TO_THE_RIGHT);
                return;
            }

            PrivateMode::Unknown(mode) => {
                debug!("Ignoring unknown mode {} in unset_private_mode", mode);
                return;
            }
        };

        trace!("Unsetting private mode: {:?}", mode);
        match mode {
            NamedPrivateMode::UrgencyHints => self.mode.remove(Mode::URGENCY_HINTS),
            NamedPrivateMode::SwapScreen | NamedPrivateMode::SwapScreenAndClear => {
                self.screen_mode_state
                    [usize::from(mode == NamedPrivateMode::SwapScreenAndClear)] = false;
                if self.mode.contains(Mode::ALT_SCREEN) {
                    if mode == NamedPrivateMode::SwapScreenAndClear {
                        self.clear_screen(ClearMode::All);
                    }
                    self.swap_alt_inner(true);
                }
            }
            NamedPrivateMode::SaveRestoreCursor => {
                self.screen_mode_state[2] = false;
                self.restore_cursor_position();
            }
            NamedPrivateMode::SwapScreenAndSetRestoreCursor => {
                self.screen_mode_state[3] = false;
                if self.mode.contains(Mode::ALT_SCREEN) {
                    self.swap_alt_inner(false);
                }
                self.restore_cursor_position();
            }
            NamedPrivateMode::ShowCursor => self.mode.remove(Mode::SHOW_CURSOR),
            NamedPrivateMode::CursorKeys => self.mode.remove(Mode::APP_CURSOR),
            // The mouse protocols are one setting, so resetting any of them
            // turns reporting off. There is no protocol underneath to fall
            // back to: setting one already cleared the others.
            NamedPrivateMode::ReportX10MouseClicks
            | NamedPrivateMode::ReportMouseClicks
            | NamedPrivateMode::ReportCellMouseMotion
            | NamedPrivateMode::ReportAllMouseMotion => {
                self.mode.remove(Mode::MOUSE_MODE);
                self.event_proxy
                    .send_event(RioEvent::MouseCursorDirty, self.window_id);
            }
            NamedPrivateMode::ReportFocusInOut => self.mode.remove(Mode::FOCUS_IN_OUT),
            NamedPrivateMode::BracketedPaste => self.mode.remove(Mode::BRACKETED_PASTE),
            NamedPrivateMode::KittyPasteEvents => {
                self.mode.remove(Mode::KITTY_PASTE_EVENTS)
            }
            NamedPrivateMode::SgrMouse => self.mode.remove(Mode::SGR_MOUSE),
            NamedPrivateMode::Utf8Mouse => self.mode.remove(Mode::UTF8_MOUSE),
            NamedPrivateMode::AlternateScroll => self.mode.remove(Mode::ALTERNATE_SCROLL),
            NamedPrivateMode::LineWrap => self.mode.remove(Mode::LINE_WRAP),
            NamedPrivateMode::Origin => {
                self.mode.remove(Mode::ORIGIN);
                self.goto(Line(0), Column(0));
            }
            NamedPrivateMode::ColumnMode => self.deccolm(),
            NamedPrivateMode::BlinkingCursor => {
                self.blinking_cursor = false;
                self.event_proxy
                    .send_event(RioEvent::CursorBlinkingChange, self.window_id);
            }
            NamedPrivateMode::GraphemeCluster => self.mode.remove(Mode::GRAPHEME_CLUSTER),
            NamedPrivateMode::SyncUpdate => (),
        }
    }

    #[inline]
    fn report_private_mode(&mut self, mode: PrivateMode) {
        info!("Reporting private mode {mode:?}");
        let state = match mode {
            PrivateMode::Named(mode) => match mode {
                NamedPrivateMode::CursorKeys => {
                    self.mode.contains(Mode::APP_CURSOR).into()
                }
                NamedPrivateMode::Origin => self.mode.contains(Mode::ORIGIN).into(),
                NamedPrivateMode::LineWrap => self.mode.contains(Mode::LINE_WRAP).into(),
                NamedPrivateMode::BlinkingCursor => self.blinking_cursor.into(),
                NamedPrivateMode::ShowCursor => {
                    self.mode.contains(Mode::SHOW_CURSOR).into()
                }
                NamedPrivateMode::ReportX10MouseClicks => {
                    self.mode.contains(Mode::MOUSE_REPORT_X10).into()
                }
                NamedPrivateMode::ReportMouseClicks => {
                    self.mode.contains(Mode::MOUSE_REPORT_CLICK).into()
                }
                NamedPrivateMode::ReportCellMouseMotion => {
                    self.mode.contains(Mode::MOUSE_DRAG).into()
                }
                NamedPrivateMode::ReportAllMouseMotion => {
                    self.mode.contains(Mode::MOUSE_MOTION).into()
                }
                NamedPrivateMode::ReportFocusInOut => {
                    self.mode.contains(Mode::FOCUS_IN_OUT).into()
                }
                NamedPrivateMode::Utf8Mouse => {
                    self.mode.contains(Mode::UTF8_MOUSE).into()
                }
                NamedPrivateMode::SgrMouse => self.mode.contains(Mode::SGR_MOUSE).into(),
                NamedPrivateMode::AlternateScroll => {
                    self.mode.contains(Mode::ALTERNATE_SCROLL).into()
                }
                NamedPrivateMode::UrgencyHints => {
                    self.mode.contains(Mode::URGENCY_HINTS).into()
                }
                NamedPrivateMode::SwapScreen => self.screen_mode_state[0].into(),
                NamedPrivateMode::SwapScreenAndClear => self.screen_mode_state[1].into(),
                NamedPrivateMode::SaveRestoreCursor => self.screen_mode_state[2].into(),
                NamedPrivateMode::SwapScreenAndSetRestoreCursor => {
                    self.screen_mode_state[3].into()
                }
                NamedPrivateMode::BracketedPaste => {
                    self.mode.contains(Mode::BRACKETED_PASTE).into()
                }
                NamedPrivateMode::GraphemeCluster => {
                    if self.mode.contains(Mode::GRAPHEME_CLUSTER) {
                        ModeState::Set
                    } else {
                        ModeState::Reset
                    }
                }
                NamedPrivateMode::SyncUpdate => ModeState::Reset,
                NamedPrivateMode::KittyPasteEvents => {
                    self.mode.contains(Mode::KITTY_PASTE_EVENTS).into()
                }
                NamedPrivateMode::ColumnMode => ModeState::NotSupported,
            },
            PrivateMode::Unknown(_) => ModeState::NotSupported,
        };

        self.event_proxy.send_event(
            RioEvent::PtyWrite(
                self.route_id,
                format!("\x1b[?{};{}$y", mode.raw(), state as u8,),
            ),
            self.window_id,
        );
    }

    #[inline]
    fn dynamic_color_sequence(&mut self, prefix: String, index: usize, terminator: &str) {
        debug!(
            "Requested write of escape sequence for color code {}: color[{}]",
            prefix, index
        );

        let terminator = terminator.to_owned();
        self.event_proxy.send_event(
            RioEvent::ColorRequest(
                self.route_id,
                index,
                Arc::new(move |color| {
                    format!(
                        "\x1b]{};rgb:{1:02x}{1:02x}/{2:02x}{2:02x}/{3:02x}{3:02x}{4}",
                        prefix, color.r, color.g, color.b, terminator
                    )
                }),
            ),
            self.window_id,
        );
    }

    #[inline]
    fn goto(&mut self, line: Line, col: Column) {
        trace!("Going to: line={}, col={}", line, col);
        let (y_offset, max_y) = if self.mode.contains(Mode::ORIGIN) {
            (self.scroll_region.start, self.scroll_region.end - 1)
        } else {
            (Line(0), self.grid.bottommost_line())
        };

        self.damage_cursor();
        self.grid.cursor.pos.row =
            std::cmp::max(std::cmp::min(line + y_offset, max_y), Line(0));
        self.grid.cursor.pos.col = std::cmp::min(col, self.grid.last_column());
        self.damage_cursor();
        self.grid.cursor.should_wrap = false;
    }

    #[inline]
    fn set_active_charset(&mut self, index: CharsetIndex) {
        self.active_charset = index;
    }

    fn single_shift(&mut self, index: CharsetIndex) {
        self.single_shift = Some(index);
    }

    #[inline]
    fn move_forward(&mut self, cols: Column) {
        let last_column =
            std::cmp::min(self.grid.cursor.pos.col + cols, self.grid.last_column());

        let cursor_line = self.grid.cursor.pos.row.0 as usize;
        self.damage.damage_line(cursor_line);

        self.grid.cursor.pos.col = last_column;
        self.grid.cursor.should_wrap = false;
    }

    #[inline]
    fn move_backward(&mut self, cols: Column) {
        let column = self.grid.cursor.pos.col.saturating_sub(cols.0);

        let cursor_line = self.grid.cursor.pos.row.0 as usize;
        self.damage.damage_line(cursor_line);

        self.grid.cursor.pos.col = Column(column);
        self.grid.cursor.should_wrap = false;
    }

    #[inline]
    fn move_backward_tabs(&mut self, count: u16) {
        trace!("Moving backward {} tabs", count);

        for _ in 0..count {
            let col = self.grid.cursor.pos.col;

            if col == 0 {
                break;
            }

            let mut destination = Column(0);
            for i in (0..(col.0)).rev() {
                if self.tabs[Column(i)] {
                    destination = Column(i);
                    break;
                }
            }
            self.grid.cursor.pos.col = destination;
        }
        self.grid.cursor.should_wrap = false;

        let line = self.grid.cursor.pos.row.0 as usize;
        self.damage.damage_line(line);
    }

    #[inline]
    fn goto_line(&mut self, line: Line) {
        self.goto(line, self.grid.cursor.pos.col)
    }

    #[inline]
    fn goto_col(&mut self, col: Column) {
        self.damage_cursor();
        self.grid.cursor.pos.col = std::cmp::min(col, self.grid.last_column());
        self.grid.cursor.should_wrap = false;
        self.damage_cursor();
    }

    #[inline]
    fn decaln(&mut self) {
        for line in (0..self.grid.screen_lines()).map(Line::from) {
            for column in 0..self.grid.columns() {
                let cell = &mut self.grid[line][Column(column)];
                *cell = Square::default();
                cell.set_c('E');
            }
        }

        self.mark_fully_damaged();
    }

    #[inline]
    fn move_up(&mut self, rows: usize) {
        let row = self.grid.cursor.pos.row;
        let top = if row >= self.scroll_region.start {
            self.scroll_region.start
        } else {
            Line(0)
        };
        self.damage_cursor();
        self.grid.cursor.pos.row =
            std::cmp::max(row - rows.min(self.grid.screen_lines()), top);
        self.grid.cursor.should_wrap = false;
        self.damage_cursor();
    }

    #[inline]
    fn move_down(&mut self, rows: usize) {
        let row = self.grid.cursor.pos.row;
        let bottom = if row < self.scroll_region.end {
            self.scroll_region.end - 1
        } else {
            self.grid.bottommost_line()
        };
        self.damage_cursor();
        self.grid.cursor.pos.row =
            std::cmp::min(row + rows.min(self.grid.screen_lines()), bottom);
        self.grid.cursor.should_wrap = false;
        self.damage_cursor();
    }

    #[inline]
    fn move_down_and_cr(&mut self, rows: usize) {
        self.move_down(rows);
        self.carriage_return();
    }

    #[inline]
    fn move_up_and_cr(&mut self, lines: usize) {
        self.move_up(lines);
        self.carriage_return();
    }

    #[inline]
    fn scroll_up(&mut self, lines: usize) {
        let origin = self.scroll_region.start;
        self.scroll_up_relative_inner(origin, lines, origin == 0, true);
    }

    #[inline]
    fn delete_lines(&mut self, lines: usize) {
        let origin = self.grid.cursor.pos.row;
        let lines = std::cmp::min(self.grid.screen_lines() - origin.0 as usize, lines);

        if lines > 0 && self.scroll_region.contains(&origin) {
            self.scroll_up_relative_inner(origin, lines, false, false);
            self.carriage_return();
        }
    }

    #[inline]
    fn push_title(&mut self) {
        trace!("Pushing '{:?}' onto title stack", self.title);

        if self.title_stack.len() >= TITLE_STACK_MAX_DEPTH {
            let removed = self.title_stack.remove(0);
            trace!(
                "Removing '{:?}' from bottom of title stack that exceeds its maximum depth",
                removed
            );
        }

        self.title_stack.push(self.title.clone());
    }

    #[inline]
    fn pop_title(&mut self) {
        trace!("Attempting to pop title from stack...");

        if let Some(popped) = self.title_stack.pop() {
            trace!("Title '{:?}' popped from stack", popped);
            self.set_title(Some(popped));
        }
    }

    #[inline]
    fn erase_chars(&mut self, count: Column) {
        self.grid.cursor.should_wrap = false;
        let start = self.grid.cursor.pos.col;
        let mut end = std::cmp::min(start + count, Column(self.grid.columns()));
        let line = self.grid.cursor.pos.row;
        if end.0 > 0
            && end.0 < self.grid.columns()
            && self.grid[line][end - 1].wide() == Wide::Wide
        {
            end += 1;
        }

        // Cleared cells have current background color set.
        let bg = self.grid.template_style().bg;
        let blank = self.grid.blank_with_bg(bg);
        self.damage.damage_line(line.0 as usize);
        self.split_ech_boundary(line, start.0, blank);
        self.split_ech_boundary(line, end.0, blank);
        self.reset_erase_wrap(line, blank);
        if self.grid.protection_mode == Some(crate::ansi::ProtectionMode::Iso) {
            self.erase_unprotected_range(line, start.0, end.0, blank, true);
            return;
        }
        let row = &mut self.grid[line];
        row.has_styles |= blank.carries_style();
        for cell in &mut row[start..end] {
            *cell = blank;
        }
        self.clip_atlas_placements(line.0, line.0 + 1, start.0, end.0);
        if !self.graphics.kitty_placements.is_empty() {
            self.graphics.kitty_graphics_dirty = true;
        }
    }

    #[inline]
    fn delete_chars(&mut self, count: usize) {
        let columns = self.grid.columns();
        let bg = self.grid.template_style().bg;
        let blank = self.grid.blank_with_bg(bg);

        // Ensure deleting within terminal bounds.
        let start = self.grid.cursor.pos.col.0;
        let count = std::cmp::min(count, columns - start);
        self.grid.cursor.should_wrap = false;
        if count == 0 {
            return;
        }
        let end = start + count;
        let num_cells = columns - end;

        let line = self.grid.cursor.pos.row;
        self.damage.damage_line(line.0 as usize);
        self.split_wide_seam(line, start, blank);
        self.split_wide_seam(line, (start + count).min(columns), blank);
        self.split_wide_seam(line, columns, blank);
        // Reset before shifting so the wrap marker cannot move into the text.
        self.reset_erase_wrap(line, blank);
        self.grid[line].has_styles |= blank.carries_style();
        let row = &mut self.grid[line][..];
        for offset in 0..num_cells {
            row.swap(start + offset, end + offset);
        }

        // Clear last `count` cells in the row. If deleting 1 char, need to delete
        // 1 cell.
        let end = columns - count;
        for cell in &mut row[end..] {
            *cell = blank;
        }
        // Image cells in the shifted tail stop showing their slices
        // (placement-model approximation, like other placement-based
        // terminals).
        self.clip_atlas_placements(line.0, line.0 + 1, start, columns);
    }

    #[inline]
    fn scroll_down(&mut self, lines: usize) {
        let origin = self.scroll_region.start;
        self.scroll_down_relative(origin, lines, true);
    }

    #[inline]
    fn insert_blank_lines(&mut self, lines: usize) {
        let origin = self.grid.cursor.pos.row;
        if lines > 0 && self.scroll_region.contains(&origin) {
            self.scroll_down_relative(origin, lines, false);
            self.carriage_return();
        }
    }

    #[inline]
    fn insert_blank(&mut self, count: usize) {
        self.grid.cursor.should_wrap = false;
        if count == 0 {
            return;
        }
        let columns = self.grid.columns();
        let bg = self.grid.template_style().bg;
        let blank = self.grid.blank_with_bg(bg);

        // Ensure inserting within terminal bounds
        let count =
            std::cmp::min(count, self.grid.columns() - self.grid.cursor.pos.col.0);

        let source = self.grid.cursor.pos.col;
        let destination = self.grid.cursor.pos.col.0 + count;
        let num_cells = self.grid.columns() - destination;

        let line = self.grid.cursor.pos.row;
        self.damage.damage_line(line.0 as usize);
        // ICH repairs an in-row split pair without clearing a spacer head
        // on the preceding row, unlike DCH and ECH.
        self.split_erase_boundary(line, source.0, blank);
        // The last shifted cell lands on the row's final column; if it
        // is a wide lead its spacer will not survive the shift, so
        // clear the pair up front.
        if num_cells > 0 {
            let last_src = source.0 + num_cells - 1;
            if matches!(
                self.grid[line][Column(last_src)].wide(),
                crate::crosswords::square::Wide::Wide
            ) {
                self.clear_pair_preserving_wrapline(line, last_src, blank);
            }
        }

        self.grid[line].has_styles |= blank.carries_style();
        let row = &mut self.grid[line][..];
        // ICH preserves the logical row's wrap state, independently of the
        // cells that move. The marker belongs on the final column.
        let wrapped = row[columns - 1].wrapline();
        row[columns - 1].set_wrapline(false);

        for offset in (0..num_cells).rev() {
            row.swap(destination + offset, source.0 + offset);
        }

        // Squares were just moved out toward the end of the line;
        // fill in between source and dest with blanks.
        for cell in &mut row[source.0..destination] {
            *cell = blank;
        }
        row[columns - 1].set_wrapline(wrapped);
        // Placement-model approximation: image cells from the insert
        // point onward stop showing their slices.
        self.clip_atlas_placements(line.0, line.0 + 1, source.0, columns);
    }

    #[inline]
    fn reverse_index(&mut self) {
        // If cursor is at the top.
        if self.grid.cursor.pos.row == self.scroll_region.start {
            self.scroll_down(1);
        } else {
            self.damage_cursor();
            self.grid.cursor.pos.row =
                std::cmp::max(self.grid.cursor.pos.row - 1, Line(0));
            self.damage_cursor();
        }
    }

    #[inline]
    fn soft_reset(&mut self) {
        self.mode
            .remove(Mode::INSERT | Mode::ORIGIN | Mode::APP_CURSOR | Mode::APP_KEYPAD);
        self.mode.insert(Mode::SHOW_CURSOR | Mode::LINE_WRAP);
        self.scroll_region = Line(0)..Line(self.grid.screen_lines() as i32);
        self.grid.cursor.pending_style = Default::default();
        self.grid.cursor.style_dirty = true;
        self.grid.cursor.charsets = Default::default();
        self.grid
            .cursor
            .template
            .remove_cell_flag(crate::crosswords::square::CellFlags::PROTECTED);
        self.grid.protection_mode = None;
        self.active_charset = Default::default();
        self.single_shift = None;
        self.grid.saved_cursor = Default::default();
        self.grid.saved_origin = false;
        self.grid.saved_charset = Default::default();
        self.grid.saved_single_shift = None;
        self.modify_other_keys = 0;
        self.set_cursor_style(None, false);
        for index in 0..256 {
            if self.colors[index].is_some() {
                self.reset_color(index);
            }
        }
        self.mark_fully_damaged();
    }

    fn set_protection(&mut self, protected: bool, mode: crate::ansi::ProtectionMode) {
        if protected {
            self.grid.protection_mode = Some(mode);
            self.grid
                .cursor
                .template
                .insert_cell_flag(crate::crosswords::square::CellFlags::PROTECTED);
        } else {
            self.grid
                .cursor
                .template
                .remove_cell_flag(crate::crosswords::square::CellFlags::PROTECTED);
        }
    }

    fn reset_state(&mut self) {
        self.kitty_clipboard_session.reset();
        self.program_status.reset();
        self.program_status_changed();
        if self.mode.contains(Mode::ALT_SCREEN) {
            std::mem::swap(&mut self.grid, &mut self.inactive_grid);
        }
        self.active_charset = Default::default();
        self.single_shift = None;
        self.screen_mode_state = [false; 4];
        self.cursor_shape = self.default_cursor_shape;
        self.grid.reset();
        self.inactive_grid.reset();
        self.scroll_region = Line(0)..Line(self.grid.screen_lines() as i32);
        self.tabs = TabStops::new(self.grid.columns());
        self.title_stack = Vec::new();
        self.modify_other_keys = 0;
        self.keyboard_mode_stack = [0; KEYBOARD_MODE_STACK_MAX_DEPTH];
        self.inactive_keyboard_mode_stack = [0; KEYBOARD_MODE_STACK_MAX_DEPTH];
        self.keyboard_mode_idx = 0;
        self.inactive_keyboard_mode_idx = 0;
        if !self.title.is_empty() {
            self.title = String::from("");
            self.metadata.lock().title.clear();
            self.event_proxy.send_event(
                RioEvent::Title(self.route_id, String::new()),
                self.window_id,
            );
        }
        self.selection = None;
        self.vi_mode_cursor = Default::default();
        self.keyboard_mode_stack = Default::default();
        self.inactive_keyboard_mode_stack = Default::default();

        // Clear all graphics on full reset (both screens, kitty and
        // sixel/iTerm2) and dispatch the queued texture removals.
        self.graphics.clear_all_kitty_state();
        self.send_graphics_updates();

        // Preserve vi mode across resets.
        self.mode &= Mode::VI;
        self.mode.insert(Mode::default());
        self.mode
            .set(Mode::GRAPHEME_CLUSTER, self.grapheme_clustering_default);

        self.event_proxy
            .send_event(RioEvent::CursorBlinkingChange, self.window_id);
        self.mark_fully_damaged();
    }

    #[inline]
    fn terminal_attribute(&mut self, attr: Attr) {
        trace!("Setting attribute: {:?}", attr);
        use crate::crosswords::style::{StyleFlags, UnderlineKind};
        match attr {
            Attr::Foreground(color) => self.grid.update_template_style(|s| s.fg = color),
            Attr::Background(color) => self.grid.update_template_style(|s| s.bg = color),
            Attr::UnderlineColor(color) => self
                .grid
                .update_template_style(|s| s.underline_color = color),
            Attr::Reset => self
                .grid
                .set_template_style(crate::crosswords::style::Style::default()),
            Attr::Reverse => self
                .grid
                .update_template_style(|s| s.flags.insert(StyleFlags::INVERSE)),
            Attr::CancelReverse => self
                .grid
                .update_template_style(|s| s.flags.remove(StyleFlags::INVERSE)),
            Attr::Bold => self
                .grid
                .update_template_style(|s| s.flags.insert(StyleFlags::BOLD)),
            Attr::CancelBold => self
                .grid
                .update_template_style(|s| s.flags.remove(StyleFlags::BOLD)),
            Attr::Dim => self
                .grid
                .update_template_style(|s| s.flags.insert(StyleFlags::DIM)),
            Attr::CancelBoldDim => self.grid.update_template_style(|s| {
                s.flags.remove(StyleFlags::BOLD | StyleFlags::DIM)
            }),
            Attr::Italic => self
                .grid
                .update_template_style(|s| s.flags.insert(StyleFlags::ITALIC)),
            Attr::CancelItalic => self
                .grid
                .update_template_style(|s| s.flags.remove(StyleFlags::ITALIC)),
            Attr::Underline => self.grid.update_template_style(|s| {
                s.flags.set_underline(Some(UnderlineKind::Single))
            }),
            Attr::DoubleUnderline => self.grid.update_template_style(|s| {
                s.flags.set_underline(Some(UnderlineKind::Double))
            }),
            Attr::Undercurl => self.grid.update_template_style(|s| {
                s.flags.set_underline(Some(UnderlineKind::Curly))
            }),
            Attr::DottedUnderline => self.grid.update_template_style(|s| {
                s.flags.set_underline(Some(UnderlineKind::Dotted))
            }),
            Attr::DashedUnderline => self.grid.update_template_style(|s| {
                s.flags.set_underline(Some(UnderlineKind::Dashed))
            }),
            Attr::BlinkSlow => self.grid.update_template_style(|s| {
                s.flags.remove(StyleFlags::ALL_BLINK);
                s.flags.insert(StyleFlags::SLOW_BLINK);
            }),
            Attr::BlinkFast => self.grid.update_template_style(|s| {
                s.flags.remove(StyleFlags::ALL_BLINK);
                s.flags.insert(StyleFlags::RAPID_BLINK);
            }),
            Attr::CancelBlink => self
                .grid
                .update_template_style(|s| s.flags.remove(StyleFlags::ALL_BLINK)),
            Attr::CancelUnderline => self
                .grid
                .update_template_style(|s| s.flags.set_underline(None)),
            Attr::Hidden => self
                .grid
                .update_template_style(|s| s.flags.insert(StyleFlags::HIDDEN)),
            Attr::CancelHidden => self
                .grid
                .update_template_style(|s| s.flags.remove(StyleFlags::HIDDEN)),
            Attr::Strike => self
                .grid
                .update_template_style(|s| s.flags.insert(StyleFlags::STRIKEOUT)),
            Attr::CancelStrike => self
                .grid
                .update_template_style(|s| s.flags.remove(StyleFlags::STRIKEOUT)),
            // _ => {
            // warn!("Term got unhandled attr: {:?}", attr);
            // }
        }
    }

    fn set_title(&mut self, title: Option<String>) {
        let title = title.unwrap_or_default();
        if title != self.title {
            self.title = title;
            self.metadata.lock().title.clone_from(&self.title);
            self.event_proxy.send_event(
                RioEvent::Title(self.route_id, self.title.clone()),
                self.window_id,
            );
        }
    }

    fn set_progress_report(&mut self, report: crate::event::ProgressReport) {
        if self.program_status.set_legacy_progress(report) {
            // Preserve the legacy event for embedders; Rio's frontend uses
            // the pane-scoped change event to select the visible report.
            self.event_proxy
                .send_event(RioEvent::ProgressReport(report), self.window_id);
            self.program_status_changed();
        }
    }

    fn set_program_status(&mut self, report: crate::program_status::ProgramStatusRecord) {
        self.program_status.apply(report);
        self.program_status_changed();
    }

    fn query_program_status(&mut self, terminator: &str) {
        self.event_proxy.send_event(RioEvent::PtyWrite(self.route_id,
            format!("\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth{terminator}")),
            self.window_id);
    }

    fn shell_prompt_start(&mut self) {
        self.end_program_activity();
    }

    fn set_current_directory(&mut self, path: std::path::PathBuf) {
        trace!("Setting working directory {:?}", path);
        self.remote_directory = None;
        if self.current_directory.as_deref() == Some(path.as_path()) {
            return;
        }
        self.current_directory = Some(path);
        self.event_proxy.send_event(
            RioEvent::CurrentDirectoryChanged(self.route_id),
            self.window_id,
        );
    }

    fn set_remote_directory(&mut self, host: String, path: std::path::PathBuf) {
        if self.remote_directory.as_ref() == Some(&(host.clone(), path.clone())) {
            return;
        }
        self.remote_directory = Some((host, path));
        self.event_proxy.send_event(
            RioEvent::CurrentDirectoryChanged(self.route_id),
            self.window_id,
        );
    }

    fn set_semantic_prompt(
        &mut self,
        mark: crate::crosswords::grid::row::SemanticPrompt,
    ) {
        let row = self.grid.cursor.pos.row;
        self.grid[row].semantic_prompt = mark;
    }

    fn set_user_var(&mut self, name: String, value: String) {
        if self.user_vars.get(&name) != Some(&value) {
            match name.as_str() {
                "RIO_APP" => {
                    self.metadata.lock().app = (!value.is_empty()).then(|| value.clone());
                }
                "RIO_SSH" => self.metadata.lock().remote = value == "1",
                _ => (),
            }
            self.user_vars.insert(name, value);
            self.event_proxy
                .send_event(RioEvent::Render, self.window_id);
        }
    }

    #[inline]
    fn set_cursor_style(&mut self, style: Option<CursorShape>, blinking: bool) {
        if let Some(cursor_shape) = style {
            self.cursor_shape = cursor_shape;
        } else {
            self.cursor_shape = self.default_cursor_shape;
        }

        self.blinking_cursor = blinking;
        self.event_proxy
            .send_event(RioEvent::CursorBlinkingChange, self.window_id);
    }

    #[inline]
    fn set_cursor_shape(&mut self, shape: CursorShape) {
        self.cursor_shape = shape;
    }

    #[inline]
    fn set_keypad_application_mode(&mut self) {
        trace!("Setting keypad application mode");
        self.mode.insert(Mode::APP_KEYPAD);
    }

    #[inline]
    fn unset_keypad_application_mode(&mut self) {
        trace!("Unsetting keypad application mode");
        self.mode.remove(Mode::APP_KEYPAD);
    }

    fn kitty_clipboard(&mut self, metadata: &[u8], payload: &[u8], terminator: &str) {
        use crate::clipboard::kitty::{Effect, Terminator};
        let terminator = if terminator == "\x07" {
            Terminator::Bell
        } else {
            Terminator::String
        };
        if let Some(effect) = self.kitty_clipboard_session.handle(
            metadata,
            payload,
            terminator,
            self.clipboard_policy,
        ) {
            let event = match effect {
                Effect::Reply(bytes) => {
                    self.emit_clipboard_reply(&bytes);
                    return;
                }
                Effect::Request(request) => RioEvent::KittyClipboardRequest(
                    self.route_id,
                    std::sync::Arc::new(request),
                ),
            };
            self.event_proxy.send_event(event, self.window_id);
        }
    }

    fn kitty_clipboard_abort(&mut self, terminator: &str) {
        use crate::clipboard::kitty::Terminator;
        if let Some(bytes) = self.kitty_clipboard_session.abort(if terminator == "\x07" {
            Terminator::Bell
        } else {
            Terminator::String
        }) {
            self.emit_clipboard_reply(&bytes);
        }
    }

    /// Store data into clipboard.
    #[inline]
    fn clipboard_store(&mut self, clipboard: u8, base64: &[u8]) {
        if self.clipboard_policy.write == crate::clipboard::Access::Deny {
            return;
        }
        let clipboard_type = match clipboard {
            b'c' => ClipboardType::Clipboard,
            b'p' | b's' => ClipboardType::Selection,
            _ => return,
        };

        if let Some(bytes) = crate::simd_base64::decode(base64) {
            if let Ok(text) = simd_utf8::validate(&bytes) {
                if let Ok(request) = self.kitty_clipboard_session.legacy_request(
                    clipboard_type,
                    clipboard,
                    Some(text.to_owned()),
                    crate::clipboard::kitty::Terminator::String,
                ) {
                    self.event_proxy.send_event(
                        RioEvent::KittyClipboardRequest(self.route_id, Arc::new(request)),
                        self.window_id,
                    );
                }
            }
        }
    }

    #[inline]
    fn configure_charset(
        &mut self,
        index: pos::CharsetIndex,
        charset: pos::StandardCharset,
    ) {
        trace!("Configuring charset {:?} as {:?}", index, charset);
        self.grid.cursor.charsets[index] = charset;
    }

    #[inline(never)]
    fn input(&mut self, c: char) {
        // A single shift is consumed even by a combining character that
        // attaches to an earlier cell instead of writing a new one.
        let charset = self.single_shift.take().unwrap_or(self.active_charset);
        let c = self.grid.cursor.charsets[charset].map(c);
        // A Glyph Protocol registration does NOT change a codepoint's
        // logical width: the cell layout stays consistent with system
        // `wcwidth` so the cursor / selection / copy never desync with a
        // width-unaware consumer (a shell line editor, etc.). The
        // declared `width` is honoured purely at render time, where the
        // glyph overflows into the following cell(s) in pixels.
        let width = match crate::codepoint_width::codepoint_width(c as u32) {
            Some(w) => w as usize,
            None => return,
        };

        // Mode 2027: try to continue the previous cell's grapheme
        // cluster first. On a break (or nothing to continue) fall
        // through to the legacy paths: zero-width codepoints attach
        // wcwidth-style, everything else writes a fresh cell.
        //
        // Codepoints <= 0xFF never continue a cluster (the only
        // UAX29 rule this waives is Prepend x
        // Latin-1, which the bulk ASCII writer already waives; this
        // keeps scalar and bulk agreeing regardless of how the parser
        // chunks the stream) and are the common case, so that check
        // goes first.
        if c > '\u{FF}'
            && self.mode.contains(Mode::GRAPHEME_CLUSTER)
            && self.try_cluster_append(c, width)
        {
            return;
        }

        // Handle zero-width characters.
        if width == 0 {
            // Mode 2027: the cluster path above is the only legitimate
            // attach. Reaching here means there was no base to join:
            // a column-0 orphan, an empty or bg-only previous cell.
            // Attaching wcwidth-style would contradict the boundary
            // the mode just computed, so drop the codepoint.
            if self.mode.contains(Mode::GRAPHEME_CLUSTER) {
                return;
            }
            // Legacy (clustering opted out or reset by the program):
            // attach the zero-width codepoint without touching the
            // base cell's width, exactly wcwidth semantics. Variation
            // selector width effects live only in the mode-2027
            // cluster path: a width the application cannot predict
            // desyncs every wcwidth-based redraw (tmux positions its
            // cursor from its own per-codepoint math and leaves stray
            // cells behind whenever the terminal disagrees).
            //
            // Selectors only attach after an emoji base; anywhere else
            // they are presentation noise and would pollute the cell's
            // extras (copy, serialize, round-trip) for no render effect.
            if matches!(c, '\u{FE0F}' | '\u{FE0E}') {
                let emoji_base = self.prev_cell_col().is_some_and(|col| {
                    let cell = self.grid[self.grid.cursor.pos.row][Column(col)];
                    matches!(
                        cell.content_tag(),
                        crate::crosswords::square::ContentTag::Codepoint
                    ) && crate::grapheme_lut::class_of(cell.c())
                        == rio_unicode::grapheme::GraphemeClass::ExtPic as u8
                });
                if !emoji_base {
                    return;
                }
            }
            self.attach_to_prev_cell(c);
            return;
        }

        if self.grid.cursor.should_wrap {
            self.wrapline();
        }

        let columns = self.grid.columns();
        if self.mode.contains(Mode::INSERT) && self.grid.cursor.pos.col + width < columns
        {
            self.insert_blank(width);
        }

        // Set the per-row kitty placeholder flag so the renderer can
        // skip the U+10EEEE scan on rows that don't have any.
        if c == crate::ansi::kitty_virtual::PLACEHOLDER {
            let line = self.grid.cursor.pos.row;
            self.grid[line].kitty_virtual_placeholder = true;
        }

        // A one-column grid can never hold a wide pair, and the
        // wrap-for-room branch below cannot create room on it: wrapping
        // lands back on column 0, still the last column, and the Spacer
        // write runs off the end of the row. Drop the glyph for a blank
        // narrow cell, matching `write_codepoint_cell` on the same
        // degenerate size.
        let (c, width) = if width == 2 && columns < 2 {
            (' ', 1)
        } else {
            (c, width)
        };

        if width == 1 {
            self.write_at_cursor_with_wide(c, Wide::Narrow);
        } else {
            if self.grid.cursor.pos.col + 1 >= columns {
                if self.mode.contains(Mode::LINE_WRAP) {
                    // Insert placeholder before wide char if glyph does not
                    // fit in this row. Mark it as LeadingSpacer post-write.
                    self.write_at_cursor(' ');
                    self.grid.cursor_cell().set_wide(Wide::LeadingSpacer);
                    self.wrapline();
                } else {
                    // Prevent out of bounds crash when linewrapping is disabled.
                    self.grid.cursor.should_wrap = true;
                    return;
                }
            }

            // Wide character itself.
            self.write_at_cursor_with_wide(c, Wide::Wide);

            // Spacer cell after it.
            self.grid.cursor.pos.col += 1;
            self.write_at_cursor_with_wide(' ', Wide::Spacer);
        }

        // Mark cursor line as damaged for partial rendering
        let cursor_line = self.grid.cursor.pos.row.0 as usize;
        self.damage.damage_line(cursor_line);

        if self.grid.cursor.pos.col + 1 < columns {
            self.grid.cursor.pos.col += 1;
        } else {
            self.grid.cursor.should_wrap = true;
        }
    }

    fn input_codepoints(&mut self, codepoints: &[u32]) {
        // Insert mode falls back: cell-rotation is per-char and the cost of
        // bulk-handling it correctly outweighs the win. Non-ASCII charsets
        // fall back too: decode runs can carry ASCII that needs mapping.
        let active = self.grid.cursor.charsets[self.active_charset];
        let clustering = self.mode.contains(Mode::GRAPHEME_CLUSTER);
        if self.single_shift.is_some()
            || self.mode.contains(Mode::INSERT)
            || active != crate::crosswords::pos::StandardCharset::Ascii
            || (clustering
                && (!self.mode.contains(Mode::LINE_WRAP)
                    || !self.graphics.atlas_placements.is_empty()))
        {
            for &cp in codepoints {
                let c = char::from_u32(cp).unwrap_or('\u{FFFD}');
                self.input(c);
            }
            return;
        }

        // Registered glyphs keep their system `wcwidth` here (see the
        // note in `input`); the declared width is a render-time hint
        // only, applied as pixel overflow in the renderer.
        let table = crate::codepoint_width::width_table();

        // Image placements can clip cells, which the scalar writer owns.
        if !self.graphics.atlas_placements.is_empty() {
            for &cp in codepoints {
                let width = match crate::codepoint_width::width_in(table, cp) {
                    Some(w) => w,
                    None => continue,
                };
                let c = char::from_u32(cp).unwrap_or('\u{FFFD}');
                if width == 0 {
                    self.input(c);
                } else {
                    self.write_codepoint_cell(cp, c, width);
                }
            }
            return;
        }

        self.grid.sync_template_style();
        let template = self.cell_template();
        let template_has_extras = template.extras_id().is_some();

        let table = crate::codepoint_width::print_table();
        let mut i = 0;
        let n = codepoints.len();
        let mut next_properties = None;
        let mut previous_ordinary = false;
        while i < n {
            let cp = codepoints[i];
            let properties = next_properties.take().unwrap_or_else(|| {
                crate::codepoint_width::print_properties_in(table, cp)
            });
            let width = match properties.width() {
                Some(w) => w,
                None => {
                    // Preserve scalar replacement for non-scalars passed
                    // through the public codepoint API in cluster mode.
                    if clustering && char::from_u32(cp).is_none() {
                        self.input('\u{fffd}');
                    }
                    previous_ordinary = false;
                    i += 1;
                    continue;
                }
            };

            if width == 0 {
                // Combining marks, VS15/VS16. Defer to scalar `input` which
                // owns the emoji-presentation flip and grapheme-extension
                // logic (attaches to preceding cell rather than writing a
                // new one).
                let c = char::from_u32(cp).unwrap_or('\u{FFFD}');
                self.input(c);
                previous_ordinary = false;
                i += 1;
                continue;
            }

            // Other cannot extend a cluster except after Prepend. Resolve
            // that boundary, then batch ordinary same-width characters.
            // Combining marks, Hangul, emoji and Indic classes keep the full
            // cluster path. Latin-1 retains input()'s existing break policy.
            if clustering && !properties.ordinary() {
                self.input(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                previous_ordinary = false;
                i += 1;
                continue;
            }
            if clustering && !previous_ordinary && cp > 0xff {
                let after_prepend = self.prev_cell_col().is_some_and(|col| {
                    let cell = self.grid[self.grid.cursor.pos.row][Column(col)];
                    if !matches!(
                        cell.content_tag(),
                        crate::crosswords::square::ContentTag::Codepoint
                    ) {
                        return false;
                    }
                    let last = cell
                        .extras_id()
                        .and_then(|id| self.grid.extras_table.get(id))
                        .and_then(|extras| extras.zerowidth.last().copied())
                        .unwrap_or(cell.c());
                    last > '\u{ff}'
                        && crate::grapheme_lut::class_of(last)
                            == rio_unicode::grapheme::GraphemeClass::Prepend as u8
                });
                if after_prepend {
                    self.input(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                    previous_ordinary = false;
                    i += 1;
                    continue;
                }
            }

            if cp == crate::ansi::kitty_virtual::PLACEHOLDER as u32 {
                let c = char::from_u32(cp).unwrap_or('\u{FFFD}');
                self.write_codepoint_cell(cp, c, width);
                previous_ordinary = false;
                i += 1;
                continue;
            }

            // Maximal run of same-width codepoints, written in bulk.
            let mut j = i + 1;
            while j < n {
                let next = codepoints[j];
                let properties = crate::codepoint_width::print_properties_in(table, next);
                if next == crate::ansi::kitty_virtual::PLACEHOLDER as u32
                    || properties.width() != Some(width)
                    || (clustering && !properties.ordinary())
                {
                    next_properties = Some(properties);
                    break;
                }
                j += 1;
            }

            // Short width changes otherwise repeat row setup and destination
            // scans. Keep long homogeneous runs on their vectorizable path.
            if j - i <= 4
                && next_properties.is_some_and(|next| {
                    matches!(next.width(), Some(1 | 2))
                        && (!clustering || next.ordinary())
                })
            {
                let written = self.write_ordinary_row(
                    template,
                    &codepoints[i..],
                    table,
                    clustering,
                );
                if written != 0 {
                    next_properties = None;
                    previous_ordinary = true;
                    i += written;
                    continue;
                }
            }

            if width == 1 {
                self.write_narrow_run(template, template_has_extras, &codepoints[i..j]);
            } else {
                self.write_wide_run(template, template_has_extras, &codepoints[i..j]);
            }
            previous_ordinary = true;
            i = j;
        }
    }

    fn input_str(&mut self, s: &str) {
        if !s.is_ascii() {
            for c in s.chars() {
                self.input(c);
            }
            return;
        }
        self.input_ascii_str(s);
    }

    fn input_ascii_str(&mut self, s: &str) {
        // Fast path: ASCII printable runs are the common case (vim redraws,
        // log tails, prompt rendering). Side-step the per-char `input()`
        // dispatch which does width lookup, wide-char/zero-width checks,
        // kitty placeholder bookkeeping, and per-byte wrap branching.
        // The caller guarantees `s` is ASCII; charset mapping and insert
        // mode still defer to the scalar path.
        debug_assert!(s.is_ascii());
        let active = self.grid.cursor.charsets[self.active_charset];
        if active != crate::crosswords::pos::StandardCharset::Ascii
            || self.single_shift.is_some()
            || self.mode.contains(Mode::INSERT)
        {
            for c in s.chars() {
                self.input(c);
            }
            return;
        }

        // The cursor template (colors + flags) is constant across a printable
        // run, and ASCII needs no charset map — build the cell template once
        // and write each cell as a single packed store.
        self.grid.sync_template_style();
        let template = self.cell_template();
        // Two per-run invariants, hoisted out of the cell loop below: whether
        // any image placement could clip a cell, and whether the template
        // carries extras (so `has_extras` is set once for the whole run).
        let has_atlas = !self.graphics.atlas_placements.is_empty();
        let template_has_extras = template.extras_id().is_some();

        let bytes = s.as_bytes();
        let mut idx = 0;
        while idx < bytes.len() {
            if self.grid.cursor.should_wrap {
                if !self.mode.contains(Mode::LINE_WRAP) {
                    // LINE_WRAP off: cursor is parked on the last column and
                    // each new char overwrites that cell. Defer to scalar
                    // `input()` for those exact semantics.
                    for c in s[idx..].chars() {
                        self.input(c);
                    }
                    return;
                }
                self.wrapline();
            }

            let columns = self.grid.columns();
            let cursor_col = self.grid.cursor.pos.col.0;
            let remaining_in_row = columns.saturating_sub(cursor_col);
            if remaining_in_row == 0 {
                for c in s[idx..].chars() {
                    self.input(c);
                }
                return;
            }

            let take = (bytes.len() - idx).min(remaining_in_row);

            // Bulk fill: when no image can clip this run, classify the target
            // cells and, if none needs wide-char cleanup, store the whole run
            // as `template | codepoint`. Splitting the scan from the fill lets
            // the compiler vectorize both, versus the branchy per-cell path.
            let mut wrote_bulk = false;
            if !has_atlas {
                let line = self.grid.cursor.pos.row;
                let row = &mut self.grid[line];
                let wrapline = row.inner[columns - 1].wrapline();
                let cells = &mut row[Column(cursor_col)..Column(cursor_col + take)];
                let needs_cleanup = Square::any_needs_wide_cleanup(cells);
                if !needs_cleanup {
                    let src = &bytes[idx..idx + take];
                    for (cell, &b) in cells.iter_mut().zip(src) {
                        *cell = crate::crosswords::square::Square::from_template(
                            template, b as char,
                        );
                    }
                    row.inner[columns - 1].set_wrapline(wrapline);
                    if template_has_extras {
                        row.has_extras = true;
                    }
                    if template.carries_style() {
                        row.has_styles = true;
                    }
                    wrote_bulk = true;
                }
            }

            if wrote_bulk {
                // The run wrote columns `cursor_col ..= cursor_col + take - 1`.
                // A full row means park on the last column with a pending wrap;
                // otherwise advance past the last written cell.
                let end = cursor_col + take;
                if end < columns {
                    self.grid.cursor.pos.col = Column(end);
                } else {
                    self.grid.cursor.pos.col = Column(columns - 1);
                    self.grid.cursor.should_wrap = true;
                }
            } else {
                for i in 0..take {
                    let c = bytes[idx + i] as char;
                    self.write_cell(c, template);
                    if self.grid.cursor.pos.col + 1 < columns {
                        self.grid.cursor.pos.col += 1;
                    } else {
                        self.grid.cursor.should_wrap = true;
                        break;
                    }
                }
            }

            let row = self.grid.cursor.pos.row;
            self.damage.damage_line(row.0 as usize);
            idx += take;
        }
    }

    #[inline]
    fn identify_terminal(&mut self, intermediate: Option<char>) {
        match intermediate {
            None => {
                trace!("Reporting primary device attributes");
                // 62: VT220, 4: sixel, 6: selective erase, 22: ANSI
                // color, 52: clipboard access via OSC 52 (probed by
                // tcell and others before enabling clipboard writes).
                let text = String::from("\x1b[?62;4;6;22;52c");
                self.event_proxy
                    .send_event(RioEvent::PtyWrite(self.route_id, text), self.window_id);
            }
            Some('>') => {
                trace!("Reporting secondary device attributes");
                let version = version_number(env!("CARGO_PKG_VERSION"));
                let text = format!("\x1b[>0;{version};1c");
                self.event_proxy
                    .send_event(RioEvent::PtyWrite(self.route_id, text), self.window_id);
            }
            _ => debug!("Unsupported device attributes intermediate"),
        }
    }

    #[inline]
    fn report_version(&mut self) {
        trace!("Reporting terminal version (XTVERSION)");
        let version = env!("CARGO_PKG_VERSION");
        let text = format!("\x1bP>|Rio {version}\x1b\\");
        self.event_proxy
            .send_event(RioEvent::PtyWrite(self.route_id, text), self.window_id);
    }

    #[inline]
    fn set_modify_other_keys(&mut self, level: u8) {
        self.modify_other_keys = level;
    }

    #[inline]
    fn report_keyboard_mode(&mut self) {
        let current_mode = self.keyboard_mode_stack[self.keyboard_mode_idx];
        let text = format!("\x1b[?{current_mode}u");
        self.event_proxy
            .send_event(RioEvent::PtyWrite(self.route_id, text), self.window_id);
    }

    #[inline]
    fn push_keyboard_mode(&mut self, mode: KeyboardModes) {
        self.keyboard_mode_idx = self.keyboard_mode_idx.wrapping_add(1);
        if self.keyboard_mode_idx >= KEYBOARD_MODE_STACK_MAX_DEPTH {
            self.keyboard_mode_idx %= KEYBOARD_MODE_STACK_MAX_DEPTH;
        }
        self.keyboard_mode_stack[self.keyboard_mode_idx] = mode.bits();

        // Sync self.mode with keyboard_mode_stack
        self.mode &= !Mode::KITTY_KEYBOARD_PROTOCOL;
        self.mode |= Mode::from(mode);
    }

    #[inline]
    fn pop_keyboard_modes(&mut self, to_pop: u16) {
        // If popping more modes than we have, just clear the stack.
        if usize::from(to_pop) >= KEYBOARD_MODE_STACK_MAX_DEPTH {
            self.keyboard_mode_stack.fill(KeyboardModes::NO_MODE.bits());
            self.keyboard_mode_idx = 0;
            self.mode &= !Mode::KITTY_KEYBOARD_PROTOCOL;
            return;
        }
        for _ in 0..to_pop {
            self.keyboard_mode_stack[self.keyboard_mode_idx] =
                KeyboardModes::NO_MODE.bits();
            self.keyboard_mode_idx = self.keyboard_mode_idx.wrapping_sub(1);
            if self.keyboard_mode_idx >= KEYBOARD_MODE_STACK_MAX_DEPTH {
                self.keyboard_mode_idx %= KEYBOARD_MODE_STACK_MAX_DEPTH;
            }
        }

        // Sync self.mode with keyboard_mode_stack
        let current_mode = self.keyboard_mode_stack[self.keyboard_mode_idx];
        self.mode &= !Mode::KITTY_KEYBOARD_PROTOCOL;
        self.mode |= Mode::from(KeyboardModes::from_bits_truncate(current_mode));
    }

    #[inline]
    fn set_keyboard_mode(
        &mut self,
        mode: KeyboardModes,
        apply: KeyboardModesApplyBehavior,
    ) {
        self.set_keyboard_mode(mode.bits(), apply);
    }

    #[inline]
    fn device_status(&mut self, arg: usize) {
        trace!("Reporting device status: {}", arg);
        match arg {
            5 => {
                let text = String::from("\x1b[0n");
                self.event_proxy
                    .send_event(RioEvent::PtyWrite(self.route_id, text), self.window_id);
            }
            6 => self.report_cursor_position(false),
            _ => debug!("unknown device status query: {}", arg),
        };
    }

    #[inline]
    fn private_device_status(&mut self, arg: usize) {
        match arg {
            6 => self.report_cursor_position(true),
            _ => debug!("unknown private device status query: {}", arg),
        }
    }

    #[inline]
    fn newline(&mut self) {
        self.linefeed();

        if self.mode.contains(Mode::LINE_FEED_NEW_LINE) {
            self.carriage_return();
        }
    }

    #[inline]
    fn backspace(&mut self) {
        if self.grid.cursor.pos.col > Column(0) {
            let line = self.grid.cursor.pos.row.0 as usize;
            self.grid.cursor.pos.col -= 1;
            self.grid.cursor.should_wrap = false;
            self.damage.damage_line(line);
        }
    }

    #[inline]
    fn clear_screen(&mut self, mode: ClearMode) {
        self.erase_display(
            mode,
            self.grid.protection_mode == Some(crate::ansi::ProtectionMode::Iso),
        );
    }

    fn clear_screen_selective(&mut self, mode: ClearMode) {
        self.erase_display(mode, true);
    }

    #[inline]
    fn clear_tabs(&mut self, mode: TabulationClearMode) {
        match mode {
            TabulationClearMode::Current => {
                self.tabs[self.grid.cursor.pos.col] = false;
            }
            TabulationClearMode::All => {
                self.tabs.clear_all();
            }
        }
    }

    #[inline]
    fn linefeed(&mut self) {
        self.grid.cursor.should_wrap = false;
        let next = self.grid.cursor.pos.row + 1;
        if next == self.scroll_region.end {
            self.scroll_up_relative(self.scroll_region.start, 1);
        } else if next < self.grid.screen_lines() {
            self.damage_cursor();
            self.grid.cursor.pos.row += 1;
            self.damage_cursor();
        }
    }

    #[inline]
    fn set_horizontal_tabstop(&mut self) {
        self.tabs[self.grid.cursor.pos.col] = true;
    }

    #[inline]
    fn set_hyperlink(&mut self, hyperlink: Option<Hyperlink>) {
        // OSC 8 hyperlinks live in `Grid::extras_table` after the cell
        // repack. Setting one allocates a side-table slot holding the
        // Hyperlink and stores its id on the cursor template, so every
        // subsequent cell write picks up the id via
        // `write_at_cursor`'s `template_extras_id` propagation.
        //
        // Extras slots are not reference-counted by design; a slot
        // lives until the cadence-driven `reclaim_extras` mark-and-sweep
        // finds no cell referencing it (bounded drift, see
        // `ExtrasTable::should_reclaim`). Ref-counting would free slots
        // sooner but requires Clone/Drop hooks on `Square`, taxing row
        // drops and template copies even when no extras exist; the
        // sweep costs nothing unless extras are actually allocated.
        match hyperlink {
            Some(hl) => {
                let id = self.grid.alloc_extras(crate::crosswords::square::Extras {
                    hyperlink: Some(hl),
                    ..Default::default()
                });
                self.grid.cursor.template.set_extras_id(Some(id));
                self.grid
                    .cursor
                    .template
                    .insert_cell_flag(crate::crosswords::square::CellFlags::HYPERLINK);
            }
            None => {
                self.grid.cursor.template.set_extras_id(None);
                self.grid
                    .cursor
                    .template
                    .remove_cell_flag(crate::crosswords::square::CellFlags::HYPERLINK);
            }
        }
    }

    /// Set the indexed color value.
    #[inline]
    fn set_color(&mut self, index: usize, color: ColorRgb) {
        // Damage terminal if the color changed and it's not the cursor.
        let color_arr = color.to_arr();

        if index != NamedColor::Cursor as usize && self.colors[index] != Some(color_arr) {
            self.mark_fully_damaged();
        }

        self.colors[index] = Some(color_arr);
        self.event_proxy.send_event(
            RioEvent::ColorChange(self.route_id, index, Some(color)),
            self.window_id,
        );
    }

    #[inline]
    fn reset_color(&mut self, index: usize) {
        // Damage terminal if the color changed and it's not the cursor.
        if index != NamedColor::Cursor as usize && self.colors[index].is_some() {
            self.mark_fully_damaged();
        }

        self.colors[index] = None;
        self.event_proxy.send_event(
            RioEvent::ColorChange(self.route_id, index, None),
            self.window_id,
        );
    }

    #[inline]
    fn bell(&mut self) {
        self.event_proxy
            .send_event(RioEvent::Bell(self.route_id), self.window_id);
    }

    #[inline]
    fn desktop_notification(&mut self, title: String, body: String) {
        self.event_proxy.send_event(
            RioEvent::DesktopNotification { title, body },
            self.window_id,
        );
    }

    #[inline]
    fn substitute(&mut self) {
        warn!("[unimplemented] Substitute");
    }

    #[inline]
    fn clipboard_load(&mut self, clipboard: u8, terminator: &str) {
        if self.clipboard_policy.read == crate::clipboard::Access::Deny {
            return;
        }
        let clipboard_type = match clipboard {
            b'c' => ClipboardType::Clipboard,
            b'p' | b's' => ClipboardType::Selection,
            _ => return,
        };

        let terminator = if terminator == "\x07" {
            crate::clipboard::kitty::Terminator::Bell
        } else {
            crate::clipboard::kitty::Terminator::String
        };

        if let Ok(request) = self.kitty_clipboard_session.legacy_request(
            clipboard_type,
            clipboard,
            None,
            terminator,
        ) {
            self.event_proxy.send_event(
                RioEvent::KittyClipboardRequest(self.route_id, Arc::new(request)),
                self.window_id,
            );
        }
    }

    #[inline]
    fn put_tab(&mut self, mut count: u16) {
        // HT moves within the current row. A pending wrap is consumed only
        // by printing, not by a tab at the right edge.
        if self.grid.cursor.should_wrap {
            return;
        }

        while self.grid.cursor.pos.col < self.grid.columns() && count != 0 {
            count -= 1;

            let c = self.grid.cursor.charsets[self.active_charset].map('\t');
            let cell = self.grid.cursor_square();
            if cell.c() == '\0' {
                cell.set_c(c);
            }

            loop {
                if (self.grid.cursor.pos.col + 1) == self.grid.columns() {
                    break;
                }

                self.grid.cursor.pos.col += 1;

                if self.tabs[self.grid.cursor.pos.col] {
                    break;
                }
            }
        }
    }

    #[inline]
    fn carriage_return(&mut self) {
        let new_col = 0;
        let row = self.grid.cursor.pos.row.0 as usize;
        self.damage.damage_line(row);
        self.grid.cursor.pos.col = Column(new_col);
        self.grid.cursor.should_wrap = false;
    }

    #[inline]
    fn move_forward_tabs(&mut self, count: u16) {
        trace!("Moving forward {} tabs", count);
        let num_cols = self.columns();
        for _ in 0..count {
            let mut col = self.grid.cursor.pos.col;

            if col == num_cols - 1 {
                break;
            }

            for i in col.0 + 1..num_cols {
                col = Column(i);
                if self.tabs[col] {
                    break;
                }
            }

            self.grid.cursor.pos.col = col;
        }

        let line = self.grid.cursor.pos.row.0 as usize;
        self.damage.damage_line(line);
    }

    #[inline]
    fn save_cursor_position(&mut self) {
        self.grid.saved_cursor = self.grid.cursor.clone();
        self.grid.saved_origin = self.mode.contains(Mode::ORIGIN);
        self.grid.saved_charset = self.active_charset;
        self.grid.saved_single_shift = self.single_shift;
    }

    #[inline]
    fn restore_cursor_position(&mut self) {
        trace!("Restoring cursor position");

        self.damage_cursor();
        // OSC 8 state is independent of the saved cursor. Keep the active
        // template's link rather than reviving the link present at DECSC.
        let active_template = self.grid.cursor.template;
        self.grid.cursor = self.grid.saved_cursor.clone();
        self.mode.set(Mode::ORIGIN, self.grid.saved_origin);
        self.active_charset = self.grid.saved_charset;
        self.single_shift = self.grid.saved_single_shift;
        self.grid
            .cursor
            .template
            .set_extras_id(active_template.extras_id());
        if active_template.has_hyperlink() {
            self.grid
                .cursor
                .template
                .insert_cell_flag(crate::crosswords::square::CellFlags::HYPERLINK);
        } else {
            self.grid
                .cursor
                .template
                .remove_cell_flag(crate::crosswords::square::CellFlags::HYPERLINK);
        }
        self.damage_cursor();
    }

    #[inline]
    fn clear_line(&mut self, mode: LineClearMode) {
        self.erase_line(
            mode,
            self.grid.protection_mode == Some(crate::ansi::ProtectionMode::Iso),
        );
    }

    fn clear_line_selective(&mut self, mode: LineClearMode) {
        self.erase_line(mode, true);
    }

    #[inline]
    fn set_scrolling_region(&mut self, top: usize, bottom: Option<usize>) {
        // Fallback to the last line as default.
        let bottom = bottom.unwrap_or_else(|| self.grid.screen_lines());

        if top >= bottom {
            warn!("Invalid scrolling region: ({};{})", top, bottom);
            return;
        }

        // Bottom should be included in the range, but range end is not
        // usually included. One option would be to use an inclusive
        // range, but instead we just let the open range end be 1
        // higher.
        let start = Line(top as i32 - 1);
        let end = Line(bottom as i32);

        debug!("Setting scrolling region: ({};{})", start, end);

        let screen_lines = Line(self.grid.screen_lines() as i32);
        self.scroll_region.start = std::cmp::min(start, screen_lines);
        self.scroll_region.end = std::cmp::min(end, screen_lines);
        self.goto(Line(0), Column(0));
    }

    #[inline]
    fn text_area_size_pixels(&mut self) {
        debug!("text_area_size_pixels");
        self.event_proxy.send_event(
            RioEvent::TextAreaSizeRequest(
                self.route_id,
                Arc::new(move |window_size| {
                    let height = window_size.height;
                    let width = window_size.width;
                    format!("\x1b[4;{height};{width}t")
                }),
            ),
            self.window_id,
        );
    }

    #[inline]
    fn cells_size_pixels(&mut self) {
        // https://terminalguide.namepad.de/seq/csi_st-16/
        let text = format!(
            "\x1b[6;{};{}t",
            self.graphics.cell_height, self.graphics.cell_width
        );
        debug!("cells_size_pixels {:?}", text);
        self.event_proxy
            .send_event(RioEvent::PtyWrite(self.route_id, text), self.window_id);
    }

    #[inline]
    fn text_area_size_chars(&mut self) {
        let text = format!(
            "\x1b[8;{};{}t",
            self.grid.screen_lines(),
            self.grid.columns()
        );
        debug!("text_area_size_chars {:?}", text);
        self.event_proxy
            .send_event(RioEvent::PtyWrite(self.route_id, text), self.window_id);
    }

    #[inline]
    fn graphics_attribute(&mut self, pi: u16, pa: u16) {
        // From Xterm documentation:
        //
        // CSI ? Pi ; Pa ; Pv S
        //
        // Pi = 1 -> item is number of color registers.
        // Pi = 2 -> item is Sixel graphics geometry (in pixels).
        // Pi = 3 -> item is ReGIS graphics geometry (in pixels).
        //
        // Pa = 1 -> read attribute.
        // Pa = 2 -> reset to default.
        // Pa = 3 -> set to value in Pv.
        // Pa = 4 -> read the maximum allowed value.
        //
        // Pv is ignored by xterm except when setting (Pa == 3).
        // Pv = n <- A single integer is used for color registers.
        // Pv = width ; height <- Two integers for graphics geometry.
        //
        // xterm replies with a control sequence of the same form:
        //
        // CSI ? Pi ; Ps ; Pv S
        //
        // where Ps is the status:
        // Ps = 0 <- success.
        // Ps = 1 <- error in Pi.
        // Ps = 2 <- error in Pa.
        // Ps = 3 <- failure.
        //
        // On success, Pv represents the value read or set.

        fn generate_response(pi: u16, ps: u16, pv: &[usize]) -> String {
            use std::fmt::Write;
            let mut text = format!("\x1b[?{pi};{ps}");
            for item in pv {
                let _ = write!(&mut text, ";{item}");
            }
            text.push('S');
            text
        }

        let (ps, pv) = match pi {
            1 => {
                match pa {
                    1 => (0, &[sixel::MAX_COLOR_REGISTERS][..]), // current value is always the
                    // maximum
                    2 => (3, &[][..]), // Report unsupported
                    3 => (3, &[][..]), // Report unsupported
                    4 => (0, &[sixel::MAX_COLOR_REGISTERS][..]),
                    _ => (2, &[][..]), // Report error in Pa
                }
            }

            2 => {
                match pa {
                    1 => {
                        self.event_proxy.send_event(
                            RioEvent::TextAreaSizeRequest(
                                self.route_id,
                                Arc::new(move |window_size| {
                                    let width = window_size.width;
                                    let height = window_size.height;
                                    let graphic_dimensions = [
                                        std::cmp::min(
                                            width as usize,
                                            MAX_GRAPHIC_DIMENSIONS[0],
                                        ),
                                        std::cmp::min(
                                            height as usize,
                                            MAX_GRAPHIC_DIMENSIONS[1],
                                        ),
                                    ];

                                    let (ps, pv) = (0, &graphic_dimensions[..]);
                                    generate_response(pi, ps, pv)
                                }),
                            ),
                            self.window_id,
                        );
                        return;
                    }
                    2 => (3, &[][..]), // Report unsupported
                    3 => (3, &[][..]), // Report unsupported
                    4 => (0, &MAX_GRAPHIC_DIMENSIONS[..]),
                    _ => (2, &[][..]), // Report error in Pa
                }
            }

            3 => {
                (1, &[][..]) // Report error in Pi (ReGIS unknown)
            }

            _ => {
                (1, &[][..]) // Report error in Pi
            }
        };

        self.event_proxy.send_event(
            RioEvent::PtyWrite(self.route_id, generate_response(pi, ps, pv)),
            self.window_id,
        );
    }

    #[inline]
    fn sixel_graphic_start(&mut self, params: &Params) {
        let palette = self.graphics.sixel_shared_palette.take();
        self.graphics.sixel_parser = Some(Box::new(sixel::Parser::new(params, palette)));
    }

    #[inline]
    fn is_sixel_graphic_active(&self) -> bool {
        self.graphics.sixel_parser.is_some()
    }

    #[inline]
    fn sixel_graphic_put(&mut self, byte: u8) -> Result<(), sixel::Error> {
        if let Some(parser) = &mut self.graphics.sixel_parser {
            parser.put(byte)
        } else {
            self.sixel_graphic_reset();
            Err(sixel::Error::NonExistentParser)
        }
    }

    #[inline]
    fn sixel_graphic_reset(&mut self) {
        self.graphics.sixel_parser = None;
    }

    #[inline]
    fn sixel_graphic_finish(&mut self) {
        let parser = self.graphics.sixel_parser.take();
        if let Some(parser) = parser {
            match parser.finish() {
                // Sixel uses None to indicate traditional Sixel cursor behavior
                Ok((graphic, palette)) => {
                    self.insert_graphic(graphic, Some(palette), None)
                }
                Err(err) => warn!("Failed to parse Sixel data: {}", err),
            }
        } else {
            warn!("Failed to sixel_graphic_finish");
        }
    }

    #[inline]
    fn insert_graphic(
        &mut self,
        graphic: GraphicData,
        palette: Option<Vec<ColorRgb>>,
        cursor_movement: Option<u8>,
    ) {
        debug!(
            "insert_graphic called: id={}, {}x{}, format={:?}, cursor_movement={:?}",
            graphic.id.get(),
            graphic.width,
            graphic.height,
            graphic.color_type,
            cursor_movement
        );
        let cell_width = self.graphics.cell_width as usize;
        let cell_height = self.graphics.cell_height as usize;

        // Headless embedders never report cell dimensions; without this
        // guard the row-fill loop below would `step_by(0)` and panic.
        if cell_width == 0 || cell_height == 0 {
            return;
        }

        // Store last palette if we receive a new one, and it is shared.
        if let Some(palette) = palette {
            if !self.mode.contains(Mode::SIXEL_PRIV_PALETTE) {
                self.graphics.sixel_shared_palette = Some(palette);
            }
        }

        // Compute display dimensions without CPU pixel resampling.
        // The GPU will scale the original texture to these dimensions.
        let view_width = cell_width * self.grid.columns();
        let view_height = cell_height * self.grid.screen_lines();
        let (display_w, display_h) = graphic.compute_display_dimensions(
            cell_width,
            cell_height,
            view_width,
            view_height,
        );

        if display_w > MAX_GRAPHIC_DIMENSIONS[0] || display_h > MAX_GRAPHIC_DIMENSIONS[1]
        {
            debug!(
                "insert_graphic: display dimensions too large {}x{}, max is {:?}",
                display_w, display_h, MAX_GRAPHIC_DIMENSIONS
            );
            return;
        }

        let width = display_w as u16;
        let height = display_h as u16;

        if width == 0 || height == 0 {
            debug!("insert_graphic: zero width or height, aborting");
            return;
        }

        let mut graphic = graphic;
        graphic.display_width = Some(display_w);
        graphic.display_height = Some(display_h);

        // Calculate bytes for this graphic
        let graphic_bytes = graphic.pixels.len();

        debug!(
            "insert_graphic: image needs {} bytes, current total: {}/{}",
            graphic_bytes, self.graphics.total_bytes, self.graphics.total_limit
        );

        // Only scan the grid for used IDs when eviction is actually needed
        if graphic_bytes > self.graphics.total_limit
            || self.graphics.pending.len() >= 4096
            || self.graphics.atlas_placements.len() >= 4096
        {
            return;
        }
        if self.graphics.total_bytes.saturating_add(graphic_bytes)
            > self.graphics.total_limit
        {
            let used_ids = self.collect_used_graphic_ids();
            debug!(
                "insert_graphic: {} images currently in use in grid, need eviction",
                used_ids.len()
            );

            if !self.graphics.evict_images(graphic_bytes, &used_ids) {
                warn!(
                    "Failed to evict enough images for {} bytes, image may not display",
                    graphic_bytes
                );
                return;
            }
        }

        let graphic_id = self.graphics.next_id();

        debug!("insert_graphic: assigned new id {}", graphic_id.0);

        // Track this graphic's memory usage
        self.graphics.track_graphic(graphic_id, graphic_bytes);

        // If SIXEL_DISPLAY is disabled, the start of the graphic is the
        // cursor position, and the grid can be scrolled if the graphic is
        // larger than the screen. The cursor is moved to the next line
        // after the graphic.
        //
        // If it is disabled, the graphic starts at (0, 0), the grid is never
        // scrolled, and the cursor position is unmodified.

        let scrolling = !self.mode.contains(Mode::SIXEL_DISPLAY);

        // For "don't move the cursor" placements, the fill loop below
        // still linefeeds through the image rows; the position is
        // restored afterwards.
        let saved_cursor = self.grid.cursor.pos;

        let leftmost = if scrolling {
            self.grid.cursor.pos.col.0
        } else {
            0
        };

        // Anchor of the image's top row in the stable absolute space,
        // captured before the fill loop scrolls anything. DECSDM
        // (no scrolling) draws from the screen's top-left instead.
        let anchor_abs_row = self.grid.lines_evicted() as i64
            + self.history_size() as i64
            + if scrolling {
                self.grid.cursor.pos.row.0 as i64
            } else {
                0
            };

        // Advance through the image's rows: damage each covered row and
        // scroll as the image grows past the bottom margin. The
        // placement below carries the pixels; cells stay untouched
        // (DEC semantics are enforced by placement clipping instead).
        for (top, offset_y) in (0..).zip((0..height).step_by(cell_height)) {
            let line = if scrolling {
                self.grid.cursor.pos.row
            } else {
                // Check if the image is beyond the screen limit.
                if top >= self.grid.screen_lines() as i32 {
                    break;
                }

                Line(top)
            };

            self.mark_line_damaged(line);

            if scrolling && offset_y < height.saturating_sub(cell_height as u16) {
                self.linefeed();
            }
        }

        // Handle cursor movement based on cursor_movement parameter:
        // - None: Sixel (traditional behavior - move to next line after image)
        // - Some(0): Kitty C=0 (cursor stays on last row of image)
        // - Some(1): Kitty C=1 (cursor doesn't move at all)
        // Display width in cells, from the size actually drawn (the
        // requested display size, not the source pixel size).
        let graphic_columns = (width as usize).div_ceil(cell_width);

        match cursor_movement {
            None => {
                // Sixel: the fill loop left the cursor on the image's
                // last row; the column follows DEC STD 070 (image start
                // column), or mode 8452 (first column right of the
                // image). This matches foot, xterm and contour.
                if scrolling {
                    let col = if self.mode.contains(Mode::SIXEL_CURSOR_TO_THE_RIGHT) {
                        (leftmost + graphic_columns).min(self.grid.columns() - 1)
                    } else {
                        leftmost
                    };
                    self.grid.cursor.pos.col = Column(col);
                    self.grid.cursor.should_wrap = false;
                }
            }
            Some(1) => {
                // Don't move the cursor at all (kitty C=1, and the
                // OSC 1337 doNotMoveCursor=1 extension). The fill
                // loop advanced through the image rows; put the
                // cursor back where the image was requested.
                self.grid.cursor.pos = saved_cursor;
            }
            Some(2) => {
                // OSC 1337 inline image: iTerm2 leaves the cursor on
                // the image's last row, in the first column after the
                // image.
                if scrolling {
                    let col = (leftmost + graphic_columns).min(self.grid.columns() - 1);
                    self.grid.cursor.pos.col = Column(col);
                    self.grid.cursor.should_wrap = false;
                }
            }
            Some(_) => {
                // Legacy/unknown values: last row, image start column.
                if scrolling {
                    self.grid.cursor.pos.col = Column(leftmost);
                    self.grid.cursor.should_wrap = false;
                }
            }
        }

        // Record the placement: the single source of truth for where
        // this image lives on the grid. Cell fills above remain only
        // until the extras-based path is removed.
        {
            let key = rio_graphics::atlas_image_key(graphic_id.get());
            let placement_columns = (width as usize).div_ceil(cell_width);
            let placement_rows = (height as usize).div_ceil(cell_height);

            // A new image overwrites what it covers, like text does:
            // clip existing placements against its rect so a fully
            // covered predecessor drops (the recount below frees its
            // texture).
            if !self.graphics.atlas_placements.is_empty() {
                let old = std::mem::take(&mut self.graphics.atlas_placements);
                let mut next = Vec::with_capacity(old.len());
                for placement in old {
                    if placement
                        .subtract_rect(
                            anchor_abs_row,
                            anchor_abs_row + placement_rows as i64,
                            leftmost,
                            leftmost + placement_columns,
                            &mut next,
                        )
                        .is_none()
                    {
                        next.push(placement);
                    }
                }
                self.graphics.atlas_placements = next;
            }

            self.graphics
                .atlas_placements
                .push(crate::ansi::graphics::AtlasPlacement {
                    image_key: key,
                    abs_row: anchor_abs_row,
                    col: leftmost,
                    columns: placement_columns,
                    rows: placement_rows,
                    src_x: 0,
                    src_y: 0,
                    src_width: width as u32,
                    src_height: height as u32,
                    total_width: width as u32,
                    total_height: height as u32,
                    insert_cell_w: cell_width as u16,
                    insert_cell_h: cell_height as u16,
                });
            self.graphics.recount_atlas_keys();
            self.graphics.kitty_graphics_dirty = true;
        }

        // Add the graphic data to the pending queue.
        debug!(
            "insert_graphic: adding to pending queue, graphic_id={}, final size={}x{}",
            graphic_id.0, width, height
        );
        self.graphics.pending.push(GraphicData {
            id: graphic_id,
            ..graphic
        });

        // Send graphics update event
        debug!("insert_graphic: sending graphics updates");
        self.send_graphics_updates();
    }

    #[inline]
    fn begin_graphic_transmission(&mut self, image_id: u32) {
        if self.graphics.kitty_images.contains_key(&image_id) {
            self.delete_graphics(crate::ansi::kitty_graphics_protocol::DeleteRequest {
                frame_number: 0,
                action: b'I',
                image_id,
                image_number: 0,
                placement_id: 0,
                x: 0,
                y: 0,
                z_index: 0,
                delete_data: true,
            });
        }
    }

    #[inline]
    fn store_graphic(&mut self, graphic: GraphicData) -> bool {
        // a=t: Store graphic without displaying.
        // No GPU upload here — pixel data is sent to GPU when a=p placement arrives.
        let image_id = graphic.id.get() as u32;
        debug!(
            "Storing kitty graphic: id={}, {}x{}",
            image_id, graphic.width, graphic.height
        );
        if !self.graphics.store_kitty_image(image_id, None, graphic) {
            return false;
        }
        self.send_graphics_updates();
        true
    }

    #[inline]
    fn graphic_placement_error(
        &self,
        placement: &crate::ansi::kitty_graphics_protocol::PlacementRequest,
    ) -> Option<&'static str> {
        self.graphics.resolve_placement_parent(placement).err()
    }

    #[inline]
    fn can_place_graphic(
        &self,
        placement: &crate::ansi::kitty_graphics_protocol::PlacementRequest,
    ) -> bool {
        self.graphics.can_place_kitty(
            placement.image_id,
            placement.placement_id,
            placement.virtual_placement,
        )
    }

    fn place_graphic(
        &mut self,
        placement: crate::ansi::kitty_graphics_protocol::PlacementRequest,
    ) -> bool {
        if self.graphics.resolve_placement_parent(&placement).is_err() {
            return false;
        }
        if !self.graphics.can_place_kitty(
            placement.image_id,
            placement.placement_id,
            placement.virtual_placement,
        ) {
            return false;
        }
        debug!(
            "Kitty graphics placement: image_id={}, x={}, y={}, columns={}, rows={}, virtual={}",
            placement.image_id,
            placement.x,
            placement.y,
            placement.columns,
            placement.rows,
            placement.virtual_placement,
        );

        // `a=p` always references a previously transmitted image; per
        // the kitty spec placing an unknown id is ENOENT, which the
        // dispatcher reports from this return value. This covers
        // virtual placements too — registering placeholder metadata
        // for an image that does not exist would render nothing while
        // telling the client everything worked.
        let image_id = placement.image_id;
        if self.graphics.get_kitty_image(image_id).is_none() {
            warn!(
                "Attempted to place non-existent kitty graphic: id={}",
                placement.image_id
            );
            return false;
        }

        // `U=1` → virtual placement: store metadata, the application
        // emits U+10EEEE placeholder cells itself. The renderer scans
        // visible cells and composites the image at those positions.
        if placement.virtual_placement {
            // `a=t` defers the GPU upload to the placement; without it
            // the placeholder cells stay blank (the `a=t` + `a=p,U=1`
            // sequence snacks.image and yazi emit).
            self.ensure_kitty_upload(image_id);
            self.place_virtual_graphic(placement);
            return true;
        }

        // Direct placement: use overlay path
        self.place_kitty_overlay(image_id, &placement);
        true
    }

    #[inline]
    fn delete_graphics(
        &mut self,
        delete: crate::ansi::kitty_graphics_protocol::DeleteRequest,
    ) {
        debug!(
            "Kitty graphics delete: action={}, image_id={}, x={}, y={}, z_index={}",
            delete.action as char, delete.image_id, delete.x, delete.y, delete.z_index
        );

        let mut overlay_changed = false;
        let previous: Vec<_> = if delete.delete_data {
            self.graphics
                .kitty_placements
                .keys()
                .chain(self.graphics.kitty_virtual_placements.keys())
                .copied()
                .collect()
        } else {
            Vec::new()
        };
        let mut reclaim = std::collections::HashSet::new();
        if delete.delete_data {
            match delete.action {
                b'i' | b'I' if delete.placement_id == 0 => {
                    reclaim.insert(delete.image_id);
                }
                b'n' | b'N' if delete.placement_id == 0 => {
                    let number = if delete.image_number != 0 {
                        delete.image_number
                    } else {
                        delete.image_id
                    };
                    if let Some(&id) = self.graphics.kitty_image_numbers.get(&number) {
                        reclaim.insert(id);
                    }
                }
                b'r' | b'R' if delete.y != 0 && delete.x <= delete.y => {
                    reclaim.extend(
                        self.graphics
                            .kitty_images
                            .keys()
                            .copied()
                            .filter(|id| *id >= delete.x && *id <= delete.y),
                    );
                }
                _ => {}
            }
        }

        match delete.action {
            b'f' | b'F' => {
                overlay_changed = self.graphics.delete_animation_frame(
                    delete.image_id,
                    delete.image_number,
                    delete.frame_number,
                    delete.delete_data,
                );
            }
            b'a' | b'A' => {
                // "All placements visible on screen": virtual (U=1)
                // placements are not visible by themselves and survive,
                // as do the images they keep alive (kitty's
                // clear_filter_func skips virtual refs).
                let top = self.grid.lines_evicted() as i64 + self.history_size() as i64;
                let bottom = top + self.grid.screen_lines() as i64;
                let before = self.graphics.kitty_placements.len();
                self.graphics.kitty_placements.retain(|_, p| {
                    p.expired
                        || p.relative.is_some()
                        || p.dest_row >= bottom
                        || p.dest_row.saturating_add(i64::from(p.rows)) <= top
                });
                overlay_changed = before != self.graphics.kitty_placements.len();
            }
            b'i' | b'I' => {
                let image_id_to_match = delete.image_id;
                // Delete overlay and virtual placements for this image
                let before = self.graphics.kitty_placements.len()
                    + self.graphics.kitty_virtual_placements.len();
                if delete.placement_id != 0 {
                    self.graphics
                        .kitty_placements
                        .remove(&(image_id_to_match, u64::from(delete.placement_id)));
                    self.graphics
                        .kitty_virtual_placements
                        .remove(&(image_id_to_match, u64::from(delete.placement_id)));
                } else {
                    self.graphics
                        .kitty_placements
                        .retain(|k, _| k.0 != image_id_to_match);
                    self.graphics
                        .kitty_virtual_placements
                        .retain(|k, _| k.0 != image_id_to_match);
                }
                overlay_changed = self.graphics.kitty_placements.len()
                    + self.graphics.kitty_virtual_placements.len()
                    != before;
            }
            b'c' | b'C' => {
                let cursor_pos = self.grid.cursor.pos;
                // Delete overlays intersecting cursor
                let col = cursor_pos.col.0;
                let abs_row = self.grid.lines_evicted() as i64
                    + self.history_size() as i64
                    + cursor_pos.row.0 as i64;
                let before = self.graphics.kitty_placements.len();
                self.graphics.kitty_placements.retain(|_, p| {
                    p.expired
                        || p.relative.is_some()
                        || !(p.dest_col <= col
                            && col < p.dest_col + p.columns as usize
                            && p.dest_row <= abs_row
                            && abs_row < p.dest_row + p.rows as i64)
                });
                overlay_changed = self.graphics.kitty_placements.len() != before;
            }
            b'p' | b'P' => {
                if delete.x > 0 && delete.y > 0 {
                    let col = Column((delete.x - 1) as usize);
                    let row = Line((delete.y - 1) as i32);
                    // Delete overlays at position
                    let abs_row = self.grid.lines_evicted() as i64
                        + self.history_size() as i64
                        + row.0 as i64;
                    let c = col.0;
                    let before = self.graphics.kitty_placements.len();
                    self.graphics.kitty_placements.retain(|_, p| {
                        p.expired
                            || p.relative.is_some()
                            || !(p.dest_col <= c
                                && c < p.dest_col + p.columns as usize
                                && p.dest_row <= abs_row
                                && abs_row < p.dest_row + p.rows as i64)
                    });
                    overlay_changed = self.graphics.kitty_placements.len() != before;
                }
            }
            b'x' | b'X' => {
                if delete.x > 0 {
                    let col = Column((delete.x - 1) as usize);
                    let c = col.0;
                    let before = self.graphics.kitty_placements.len();
                    self.graphics.kitty_placements.retain(|_, p| {
                        p.expired
                            || p.relative.is_some()
                            || !(p.dest_col <= c && c < p.dest_col + p.columns as usize)
                    });
                    overlay_changed = self.graphics.kitty_placements.len() != before;
                }
            }
            b'y' | b'Y' => {
                if delete.y > 0 {
                    let row = Line((delete.y - 1) as i32);
                    let abs_row = self.grid.lines_evicted() as i64
                        + self.history_size() as i64
                        + row.0 as i64;
                    let before = self.graphics.kitty_placements.len();
                    self.graphics.kitty_placements.retain(|_, p| {
                        p.expired
                            || p.relative.is_some()
                            || !(p.dest_row <= abs_row
                                && abs_row < p.dest_row + p.rows as i64)
                    });
                    overlay_changed = self.graphics.kitty_placements.len() != before;
                }
            }
            b'z' | b'Z' => {
                let z = delete.z_index;
                let before = self.graphics.kitty_placements.len();
                self.graphics.kitty_placements.retain(|_, p| p.z_index != z);
                overlay_changed = self.graphics.kitty_placements.len() != before;
            }
            b'n' | b'N' => {
                // Delete by image number — look up image_id from the
                // number map. Prefer the canonical `I=` channel
                // (`delete.image_number`), but fall back to `image_id`
                // for older clients that shove the number into `i=`.
                let lookup_number = if delete.image_number > 0 {
                    delete.image_number
                } else {
                    delete.image_id
                };
                if let Some(&image_id) =
                    self.graphics.kitty_image_numbers.get(&lookup_number)
                {
                    let before = self.graphics.kitty_placements.len()
                        + self.graphics.kitty_virtual_placements.len();
                    if delete.placement_id != 0 {
                        self.graphics
                            .kitty_placements
                            .remove(&(image_id, u64::from(delete.placement_id)));
                        self.graphics
                            .kitty_virtual_placements
                            .remove(&(image_id, u64::from(delete.placement_id)));
                    } else {
                        self.graphics
                            .kitty_placements
                            .retain(|k, _| k.0 != image_id);
                        self.graphics
                            .kitty_virtual_placements
                            .retain(|k, _| k.0 != image_id);
                    }
                    overlay_changed = self.graphics.kitty_placements.len()
                        + self.graphics.kitty_virtual_placements.len()
                        != before;
                }
            }
            b'q' | b'Q' => {
                // Delete at cell position with z-index filter
                if delete.x > 0 && delete.y > 0 {
                    let col = Column((delete.x - 1) as usize);
                    let row = Line((delete.y - 1) as i32);
                    // Delete overlays at position with z-index filter
                    let z = delete.z_index;
                    let abs_row = self.grid.lines_evicted() as i64
                        + self.history_size() as i64
                        + row.0 as i64;
                    let c = col.0;
                    let before = self.graphics.kitty_placements.len();
                    self.graphics.kitty_placements.retain(|_, p| {
                        p.expired
                            || p.relative.is_some()
                            || !(p.z_index == z
                                && p.dest_col <= c
                                && c < p.dest_col + p.columns as usize
                                && p.dest_row <= abs_row
                                && abs_row < p.dest_row + p.rows as i64)
                    });
                    overlay_changed = self.graphics.kitty_placements.len() != before;
                }
            }
            b'r' | b'R' => {
                // Delete by image ID range [x..y]
                let range_start = delete.x;
                let range_end = delete.y;
                if range_end != 0 && range_end >= range_start {
                    let before = self.graphics.kitty_placements.len()
                        + self.graphics.kitty_virtual_placements.len();
                    self.graphics
                        .kitty_placements
                        .retain(|k, _| k.0 < range_start || k.0 > range_end);
                    self.graphics
                        .kitty_virtual_placements
                        .retain(|k, _| k.0 < range_start || k.0 > range_end);
                    overlay_changed = self.graphics.kitty_placements.len()
                        + self.graphics.kitty_virtual_placements.len()
                        != before;
                }
            }
            _ => {
                debug!(
                    "Kitty graphics delete mode '{}' not implemented",
                    delete.action as char
                );
            }
        }

        let before = self.graphics.kitty_placements.len();
        self.graphics.remove_orphaned_placements();
        overlay_changed |= before != self.graphics.kitty_placements.len();
        if delete.delete_data {
            let remaining: std::collections::HashSet<_> = self
                .graphics
                .kitty_placements
                .keys()
                .chain(self.graphics.kitty_virtual_placements.keys())
                .copied()
                .collect();
            reclaim.extend(
                previous
                    .into_iter()
                    .filter(|key| !remaining.contains(key))
                    .map(|key| key.0),
            );
            let used: std::collections::HashSet<_> =
                remaining.iter().map(|key| key.0).collect();
            self.graphics
                .delete_kitty_images(|id, _| reclaim.contains(id) && !used.contains(id));
        }

        if overlay_changed {
            self.graphics.kitty_graphics_dirty = true;
            // Placement-only deletes produce no cell damage, and the
            // damage event is what drives a repaint; without this a
            // deleted image stays visible until unrelated output.
            self.mark_fully_damaged();
        }
        self.send_graphics_updates();
    }

    #[inline]
    fn kitty_graphics_response(&mut self, response: String) {
        // Send response back to the terminal
        self.event_proxy
            .send_event(RioEvent::PtyWrite(self.route_id, response), self.window_id);
    }

    #[inline]
    fn xtgettcap_response(&mut self, response: String) {
        self.event_proxy
            .send_event(RioEvent::PtyWrite(self.route_id, response), self.window_id);
    }

    #[inline]
    fn glyph_protocol_response(&mut self, response: String) {
        self.event_proxy
            .send_event(RioEvent::PtyWrite(self.route_id, response), self.window_id);
    }

    #[cfg(feature = "graphics")]
    #[cfg(feature = "graphics")]
    fn glyph_register(
        &mut self,
        cp: u32,
        payload: crate::ansi::glyph_protocol::GlyphPayload,
        width: u8,
    ) -> Result<(), crate::ansi::glyph_protocol::RegisterError> {
        use crate::ansi::glyph_protocol::{is_pua, GlyphPayload, RegisterError};
        use rio_graphics::glyph::glyf_decode;
        use rio_graphics::glyph::glyph_registry::{
            GlyphRegistry, RegisterRejection, StoredPayload,
        };

        // PUA check first — callers that bypass the wire parser (direct
        // API, tests) still get the rejection without accidentally
        // allocating the registry on a doomed request.
        if !is_pua(cp) {
            return Err(RegisterError::OutOfNamespace);
        }

        // Translate a glyf_decode error into the protocol's defined
        // `reason=` codes.
        fn translate(err: glyf_decode::DecodeError) -> RegisterError {
            match err {
                glyf_decode::DecodeError::Composite => {
                    RegisterError::CompositeUnsupported
                }
                glyf_decode::DecodeError::Hinted => RegisterError::HintingUnsupported,
                glyf_decode::DecodeError::Malformed => RegisterError::MalformedPayload,
                glyf_decode::DecodeError::TooLarge => RegisterError::OutlineTooLarge,
            }
        }

        // §8.2: a container's outlines must sum to at most
        // MAX_DECODED_POINTS. Peeks each record header, no decoding.
        fn container_budget(glyphs: &[Vec<u8>]) -> Result<(), RegisterError> {
            let mut total = 0usize;
            for glyf in glyphs {
                total += glyf_decode::peek_point_count(glyf).map_err(translate)?;
                if total > glyf_decode::MAX_DECODED_POINTS {
                    return Err(RegisterError::OutlineTooLarge);
                }
            }
            Ok(())
        }

        // Validate the monochrome `glyf` payload at register time so a
        // bad outline produces a clear error response. For COLR
        // containers, structural validation stays render-time only:
        // re-decoding every carried outline (up to 1024 per
        // registration) on the hot register path costs more than it
        // saves, and the parser already catches the common-case
        // malformation. A structurally-broken COLR payload manifests
        // as tofu at first render rather than a register-time error;
        // that's the accepted trade-off. The one register-time check
        // containers do get is the §8.2 decoded-size budget, because
        // it exists precisely to stop hostile payloads from reaching
        // the decoder's allocations.
        let (stored, upm) = match payload {
            GlyphPayload::Glyf { glyf, upm } => {
                glyf_decode::decode(&glyf).map_err(translate)?;
                (StoredPayload::Glyf { glyf }, upm)
            }
            GlyphPayload::ColrV0 { container, upm } => {
                container_budget(&container.glyphs)?;
                (
                    StoredPayload::ColrV0 {
                        glyphs: container.glyphs,
                        colr: container.colr,
                        cpal: container.cpal,
                    },
                    upm,
                )
            }
            GlyphPayload::ColrV1 { container, upm } => {
                container_budget(&container.glyphs)?;
                (
                    StoredPayload::ColrV1 {
                        glyphs: container.glyphs,
                        colr: container.colr,
                        cpal: container.cpal,
                    },
                    upm,
                )
            }
        };

        // Lazily allocate the registry — idle terminals that never see
        // Glyph Protocol traffic stay at `None` and pay nothing per
        // frame. The first allocation fires `GlyphProtocolInstalled`
        // so the frontend can wire the registry into the font library
        // exactly once per session.
        let was_uninitialised = self.glyph_registry.is_none();
        let registry = self.glyph_registry.get_or_insert_with(GlyphRegistry::new);

        let result = match registry.register_with_width(cp, stored, upm, width) {
            Ok(_evicted) => Ok(()),
            Err(RegisterRejection::OutOfNamespace) => {
                // Unreachable given the is_pua check above, but the
                // registry double-checks so the defence exists.
                Err(RegisterError::OutOfNamespace)
            }
        };

        if was_uninitialised && result.is_ok() {
            let registry = registry.clone();
            self.event_proxy.send_event(
                RioEvent::GlyphProtocolInstalled {
                    route_id: self.route_id,
                    registry,
                },
                self.window_id,
            );
        }

        // §7.3: re-registering a codepoint must repaint cells already
        // showing it — partial damage can't know which rows hold the
        // codepoint, so a register is a full-screen invalidation.
        if result.is_ok() {
            self.mark_fully_damaged();
        }

        result
    }

    #[cfg(feature = "graphics")]
    #[cfg(feature = "graphics")]
    fn glyph_clear(&mut self, cp: Option<u32>) {
        // Nothing to clear if nothing was ever registered.
        let Some(registry) = self.glyph_registry.as_ref() else {
            return;
        };
        match cp {
            None => registry.clear_all(),
            Some(cp) => registry.clear_one(cp),
        }
        // §7.3: cleared codepoints fall back to font rendering on the
        // next frame — repaint everything that might show them.
        self.mark_fully_damaged();
    }

    fn glyph_query(&mut self, cp: u32) {
        // Defer to the frontend: only it has access to both the per-
        // route registry and the FontLibrary, so only it can compute
        // the System / Glossary / Both bits accurately. The frontend
        // formats the reply and writes it back to this pane's PTY
        // asynchronously.
        self.event_proxy.send_event(
            RioEvent::GlyphProtocolQuery {
                route_id: self.route_id,
                cp,
            },
            self.window_id,
        );
    }

    fn parse_kitty_graphics(
        &mut self,
        params: &[&[u8]],
    ) -> Option<crate::ansi::kitty_graphics_protocol::KittyGraphicsResponse> {
        let images = &self.graphics.kitty_images;
        let numbers = &self.graphics.kitty_image_numbers;
        crate::ansi::kitty_graphics_protocol::parse_with_resources(
            params,
            &mut self.graphics.kitty_chunking_state,
            |mut candidate| {
                for _ in 0..images.len() + 2 {
                    if candidate != 0 && !images.contains_key(&candidate) {
                        return candidate;
                    }
                    candidate = candidate.wrapping_add(1);
                }
                unreachable!("an unused image ID exists beyond the retained images")
            },
            |image_id, image_number| {
                crate::ansi::graphics::Graphics::image_identity(
                    images,
                    numbers,
                    image_id,
                    image_number,
                )
                .map(Some)
            },
        )
    }

    #[inline]
    fn kitty_chunking_state_mut(
        &mut self,
    ) -> Option<&mut crate::ansi::kitty_graphics_protocol::KittyGraphicsState> {
        Some(&mut self.graphics.kitty_chunking_state)
    }
}

pub struct CrosswordsSize {
    pub columns: usize,
    pub screen_lines: usize,
    pub width: u32,
    pub height: u32,
    pub square_width: u32,
    pub square_height: u32,
}

impl CrosswordsSize {
    pub fn new(columns: usize, screen_lines: usize) -> Self {
        Self {
            columns,
            screen_lines,
            width: 0,
            height: 0,
            square_width: 0,
            square_height: 0,
        }
    }

    pub fn new_with_dimensions(
        columns: usize,
        screen_lines: usize,
        width: u32,
        height: u32,
        square_width: u32,
        square_height: u32,
    ) -> Self {
        Self {
            columns,
            screen_lines,
            width,
            height,
            square_width,
            square_height,
        }
    }
}

impl Dimensions for CrosswordsSize {
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }

    fn screen_lines(&self) -> usize {
        self.screen_lines
    }

    fn columns(&self) -> usize {
        self.columns
    }

    fn square_width(&self) -> f32 {
        self.square_width as f32
    }

    fn square_height(&self) -> f32 {
        self.square_height as f32
    }
}

impl<T: EventListener> Dimensions for Crosswords<T> {
    #[inline]
    fn columns(&self) -> usize {
        self.grid.columns()
    }

    #[inline]
    fn screen_lines(&self) -> usize {
        self.grid.screen_lines()
    }

    #[inline]
    fn total_lines(&self) -> usize {
        self.grid.total_lines()
    }

    fn square_width(&self) -> f32 {
        self.graphics.cell_width
    }

    fn square_height(&self) -> f32 {
        self.graphics.cell_height
    }
}

// Additional Crosswords methods (not part of Handler trait)
impl<U: EventListener> Crosswords<U> {
    /// Place a kitty image as an overlay (not in grid cells).
    /// Used for a=T (transmit+display) and a=p (place stored image).
    fn place_kitty_overlay(
        &mut self,
        image_id: u32,
        placement: &crate::ansi::kitty_graphics_protocol::PlacementRequest,
    ) {
        let Ok(relative) = self.graphics.resolve_placement_parent(placement) else {
            return;
        };
        if !self
            .graphics
            .can_place_kitty(image_id, placement.placement_id, false)
        {
            return;
        }
        let (image_width, image_height, transmit_time) =
            match self.graphics.get_kitty_image(image_id) {
                Some(s) => (s.data.width, s.data.height, s.transmission_time),
                None => {
                    warn!("place_kitty_overlay: image {} not found", image_id);
                    return;
                }
            };
        if image_width == 0 || image_height == 0 {
            return;
        }

        let cell_width = self.graphics.cell_width.round() as usize;
        let cell_height = self.graphics.cell_height.round() as usize;

        if cell_width == 0 || cell_height == 0 {
            return;
        }

        // Resolve the source rectangle (kitty `x=`/`y=`/`w=`/`h=`)
        // against the image. The crop is what gets displayed; it
        // never affects where the placement lands. Both crop and
        // display size are re-resolved at render time, so these
        // values only drive the grid footprint (spans and cursor
        // movement). Empty source axes still participate in sizing:
        // requested spans and subcell offsets can give them a cell footprint.
        let (_, _, source_width, source_height) =
            crate::ansi::graphics::kitty_source_rect(
                placement.x,
                placement.y,
                placement.width,
                placement.height,
                image_width,
                image_height,
            );
        let (display_w, display_h) = crate::ansi::graphics::kitty_display_size(
            source_width,
            source_height,
            placement.columns,
            placement.rows,
            cell_width,
            cell_height,
            (placement.cell_x_offset, placement.cell_y_offset),
        );

        // Memory is managed in store_kitty_image (eviction happens there)

        // Per the kitty spec a placement always renders at the cursor
        // position; `x=`/`y=` select the source rectangle within the
        // image, never the destination cell.
        let dest_col = self.grid.cursor.pos.col.0;
        let cursor_row = self.grid.cursor.pos.row.0;
        // Absolute row in the stable space: lines ever evicted off the
        // ring + current history + screen-relative row. Stays glued to
        // content even after scrollback saturates.
        let dest_row = self.grid.lines_evicted() as i64
            + self.history_size() as i64
            + cursor_row as i64;

        // Include clamped subcell offsets when deriving a cell span.
        // Keep the raw offsets so future cell-size changes can re-clamp them.
        let (columns, rows) = crate::ansi::graphics::kitty_grid_size(
            (display_w, display_h),
            (placement.columns, placement.rows),
            (cell_width, cell_height),
            (placement.cell_x_offset, placement.cell_y_offset),
        );

        // Create overlay placement.
        self.graphics.reap_expired_kitty_placements();
        //
        // Per kitty spec, when the client doesn't supply `p=` (or sends
        // p=0) the terminal must allocate a unique internal placement_id
        // so multiple placements of the same image don't collide. Without
        // this, two `kitten icat` invocations referencing the same
        // image_id would both store at key (image_id, 0) and only the
        // last one would survive.
        let placement_id = if placement.placement_id == 0 {
            self.graphics.allocate_internal_placement_id()
        } else {
            u64::from(placement.placement_id)
        };
        let kitty_placement = KittyPlacement {
            expired: false,
            relative,
            image_id,
            placement_id,
            source_x: placement.x,
            source_y: placement.y,
            source_width: placement.width,
            source_height: placement.height,
            dest_col,
            dest_row,
            columns,
            rows,
            requested_columns: placement.columns,
            requested_rows: placement.rows,
            pixel_width: display_w as u32,
            pixel_height: display_h as u32,
            cell_x_offset: placement.cell_x_offset,
            cell_y_offset: placement.cell_y_offset,
            z_index: placement.z_index,
            transmit_time,
        };

        self.graphics
            .kitty_placements
            .insert((image_id, placement_id), kitty_placement);
        self.graphics
            .kitty_virtual_placements
            .remove(&(image_id, placement_id));
        self.graphics.kitty_graphics_dirty = true;

        self.graphics.queue_kitty_upload(image_id);
        self.send_graphics_updates();

        // Handle cursor movement per kitty spec
        if relative.is_some() {
            return;
        }
        match placement.cursor_movement {
            0 => {
                // C=0: Move cursor to after the image
                self.advance_cursor_past_placement(dest_col, columns, rows);
            }
            1 => {
                // C=1: Don't move cursor
            }
            _ => {
                // Default: treat as C=0
                self.advance_cursor_past_placement(dest_col, columns, rows);
            }
        }
    }

    /// Refresh the grid footprint of every live placement of an image
    /// after its pixel data changed dimensions.
    /// Queue the stored pixels of `image_id` for the GPU unless the
    /// texture already holds them. Returns whether an upload was queued.
    fn ensure_kitty_upload(&mut self, image_id: u32) -> bool {
        if self.graphics.queue_kitty_upload(image_id) {
            self.graphics.kitty_graphics_dirty = true;
            self.send_graphics_updates();
            true
        } else {
            false
        }
    }

    /// Move past a placement, wrapping once if it reaches the right edge.
    /// Allow movement to the scroll-region bottom and one screen of scrolling.
    fn advance_cursor_past_placement(
        &mut self,
        dest_col: usize,
        columns: u32,
        rows: u32,
    ) {
        let target_col = dest_col.saturating_add(columns as usize);
        let wraps = target_col >= self.grid.columns();
        let requested_rows =
            (rows.saturating_sub(1) as usize).saturating_add(usize::from(wraps));
        let rows_before_scroll = if self.scroll_region.contains(&self.grid.cursor.pos.row)
        {
            (self.scroll_region.end.0 - 1 - self.grid.cursor.pos.row.0) as usize
        } else {
            0
        };
        let rows_to_move = requested_rows
            .min(rows_before_scroll.saturating_add(self.grid.screen_lines()));
        for _ in 0..rows_to_move {
            self.linefeed();
        }
        self.damage_cursor();
        self.grid.cursor.pos.col = Column(if wraps { 0 } else { target_col });
        // Any cursor repositioning discards a pending wrap, otherwise
        // the next printed character wraps a row below the intended
        // caption position.
        self.grid.cursor.should_wrap = false;
        self.damage_cursor();
    }

    /// Register a virtual placement (kitty graphics `a=p,U=1`).
    ///
    /// Per the spec, this command only declares metadata — it does NOT
    /// write any cells. The application (e.g. `kitten icat
    /// --unicode-placeholder`, see kitty's `kittens/icat/transmit.go:221`)
    /// emits the U+10EEEE placeholder cells itself as ordinary text right
    /// after this APC. The renderer scans visible cells for U+10EEEE,
    /// decodes the image_id from the foreground color and the row/col
    /// indices from the combining-mark diacritics (kitty_virtual::*), and
    /// looks up the metadata stored here to know which image to composite.
    fn place_virtual_graphic(
        &mut self,
        placement: crate::ansi::kitty_graphics_protocol::PlacementRequest,
    ) {
        if !self.graphics.can_place_kitty(
            placement.image_id,
            placement.placement_id,
            true,
        ) {
            return;
        }
        use crate::ansi::graphics::VirtualPlacement;

        debug!(
            "Virtual placement: image_id={}, placement_id={}, columns={}, rows={}",
            placement.image_id, placement.placement_id, placement.columns, placement.rows
        );

        let placement_id = if placement.placement_id == 0 {
            self.graphics.allocate_internal_placement_id()
        } else {
            u64::from(placement.placement_id)
        };
        let vp = VirtualPlacement {
            image_id: placement.image_id,
            placement_id,
            columns: placement.columns,
            rows: placement.rows,
            x: placement.x,
            y: placement.y,
            width: placement.width,
            height: placement.height,
        };
        self.graphics.reap_expired_kitty_placements();
        self.graphics
            .kitty_virtual_placements
            .insert((placement.image_id, placement_id), vp);
        self.graphics
            .kitty_placements
            .remove(&(placement.image_id, placement_id));
        self.graphics.kitty_graphics_dirty = true;
    }
}

#[cfg(test)]
mod conformance_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crosswords::pos::{Column, Line, Pos, Side};
    use crate::crosswords::CrosswordsSize;
    use crate::event::VoidListener;

    /// Event-capturing listener for tests that assert on emitted
    /// `RioEvent`s. One definition; four tests used to carry
    /// byte-identical local copies.
    #[derive(Clone)]
    struct TestListener {
        events: std::rc::Rc<std::cell::RefCell<Vec<RioEvent>>>,
    }

    impl EventListener for TestListener {
        fn send_event(&self, event: RioEvent, _id: WindowId) {
            self.events.borrow_mut().push(event);
        }
    }

    fn make_crosswords() -> Crosswords<VoidListener> {
        let size = CrosswordsSize::new(4, 4);
        let window_id = crate::event::WindowId::from(0);
        Crosswords::new(size, CursorShape::Block, VoidListener {}, window_id, 0, 10)
    }

    #[test]
    fn implicit_kitty_placements_do_not_alias_explicit_ids() {
        use crate::performer::handler::Processor;
        for id in [1, 0x80000000, u32::MAX] {
            for implicit_first in [false, true] {
                for delete_action in ['i', 'I'] {
                    let mut terminal = make_crosswords();
                    terminal.graphics.cell_width = 8.0;
                    terminal.graphics.cell_height = 16.0;
                    let mut processor = Processor::default();
                    let first_id = if implicit_first { 0 } else { id };
                    let second_id = if implicit_first { id } else { 0 };
                    processor.advance(&mut terminal, format!(
                        "\x1b_Ga=T,i=1,p={first_id},f=32,s=1,v=1,C=1;AQIDBA==\x1b\\\x1b[3;3H\x1b_Ga=p,i=1,p={second_id},C=1\x1b\\"
                    ).as_bytes());
                    assert_eq!(terminal.graphics.kitty_placements.len(), 2);
                    assert!(terminal
                        .graphics
                        .kitty_placements
                        .contains_key(&(1, u64::from(id))));
                    processor.advance(
                        &mut terminal,
                        format!("\x1b_Ga=d,d={delete_action},i=1,p={id}\x1b\\")
                            .as_bytes(),
                    );
                    assert_eq!(terminal.graphics.kitty_placements.len(), 1);
                    let retained =
                        terminal.graphics.kitty_placements.values().next().unwrap();
                    assert_eq!(
                        crate::ansi::graphics::public_placement_id(retained.placement_id),
                        0
                    );
                    assert_eq!(retained.dest_col, if implicit_first { 0 } else { 2 });
                    assert!(terminal.graphics.kitty_images.contains_key(&1));
                }
            }
        }
    }

    #[test]
    fn kitty_visible_delete_preserves_history_virtual_chains_and_unplaced_images() {
        use crate::performer::handler::Processor;
        let mut terminal = make_crosswords();
        terminal.graphics.cell_width = 8.0;
        terminal.graphics.cell_height = 16.0;
        let mut processor = Processor::default();
        for (id, extra) in [
            (1, ""),
            (2, ",P=1,Q=1"),
            (3, ",a=t"),
            (4, ",U=1"),
            (5, ",P=4,Q=1"),
        ] {
            processor.advance(
                &mut terminal,
                format!("\x1b_Ga=T,i={id},p=1,f=32,s=1,v=1,C=1{extra};AQIDBA==\x1b\\")
                    .as_bytes(),
            );
        }
        processor.advance(&mut terminal, b"\n\n\n\n\n\x1b_Ga=T,i=6,p=1,f=32,s=1,v=1,C=1;AQIDBA==\x1b\\\x1b_Ga=d,d=A\x1b\\");
        for id in [1, 2, 3, 4, 5] {
            assert!(
                terminal.graphics.kitty_images.contains_key(&id),
                "image {id}"
            );
        }
        for id in [1, 2, 5] {
            assert!(
                terminal.graphics.kitty_placements.contains_key(&(id, 1)),
                "placement {id}"
            );
        }
        assert!(terminal
            .graphics
            .kitty_virtual_placements
            .contains_key(&(4, 1)));
        assert!(!terminal.graphics.kitty_images.contains_key(&6));
    }

    #[test]
    fn kitty_uppercase_delete_only_reclaims_selected_unused_images() {
        use crate::performer::handler::Processor;
        let mut terminal = make_crosswords();
        terminal.graphics.cell_width = 8.0;
        terminal.graphics.cell_height = 16.0;
        let mut processor = Processor::default();
        processor.advance(&mut terminal, b"\x1b_Ga=T,i=1,p=1,f=32,s=1,v=1,C=1;AQIDBA==\x1b\\\x1b_Ga=p,i=1,p=2,C=1\x1b\\\x1b_Ga=t,i=3,f=32,s=1,v=1;AQIDBA==\x1b\\\x1b_Ga=d,d=I,i=1,p=1\x1b\\");
        assert!(terminal.graphics.kitty_images.contains_key(&1));
        assert!(terminal.graphics.kitty_placements.contains_key(&(1, 2)));
        processor.advance(&mut terminal, b"\x1b_Ga=d,d=I,i=3,p=99\x1b\\");
        assert!(terminal.graphics.kitty_images.contains_key(&3));
        processor.advance(&mut terminal, b"\x1b_Ga=d,d=I,i=1,p=2\x1b\\");
        assert!(!terminal.graphics.kitty_images.contains_key(&1));
        assert!(terminal.graphics.kitty_images.contains_key(&3));
    }

    #[test]
    fn animation_chunks_survive_control_but_reject_replaced_or_deleted_targets() {
        use crate::performer::handler::Processor;
        let mut terminal = make_crosswords();
        let mut processor = Processor::default();
        let root = b"\x1b_Ga=t,i=1,f=32,s=1,v=1;/wAA/w==\x1b\\";
        processor.advance(&mut terminal, root);
        processor.advance(&mut terminal,b"\x1b_Ga=f,i=1,f=32,s=1,v=1;AAD//w==\x1b\\\x1b_Ga=f,i=1,r=3,f=32,s=1,v=1,m=1;/wA=\x1b\\\x1b_Ga=a,i=1,c=2\x1b\\");
        assert_eq!(
            terminal.graphics.kitty_images[&1].data.pixels,
            [0, 0, 255, 255]
        );
        processor.advance(
            &mut terminal,
            b"\x1b_Gm=0;AP8=\x1b\\\x1b_Ga=a,i=1,c=3\x1b\\",
        );
        assert_eq!(terminal.graphics.kitty_animations[&1].frame_count(), 3);
        assert_eq!(
            terminal.graphics.kitty_images[&1].data.pixels,
            [255, 0, 0, 255]
        );

        processor.advance(&mut terminal, root);
        processor.advance(&mut terminal, b"\x1b_Ga=f,i=1,f=32,s=1,v=1,m=1;AAA=\x1b\\");
        terminal.store_graphic(kitty_test_image(1, 1, 1));
        let replaced = terminal.graphics.kitty_images[&1].data.pixels.clone();
        processor.advance(&mut terminal, b"\x1b_Gm=0;//8=\x1b\\");
        assert!(terminal.graphics.kitty_animations.is_empty());
        assert_eq!(terminal.graphics.kitty_images[&1].data.pixels, replaced);
        assert_eq!(terminal.graphics.total_bytes, 4);

        processor.advance(&mut terminal,b"\x1b_Ga=f,i=1,f=32,s=1,v=1,m=1;AAA=\x1b\\\x1b_Ga=d,d=I,i=1\x1b\\\x1b_Gm=0;//8=\x1b\\");
        assert!(terminal.graphics.kitty_images.is_empty());
        assert!(terminal.graphics.kitty_animations.is_empty());
        assert_eq!(terminal.graphics.total_bytes, 0);

        processor.advance(&mut terminal, root);
        let identity = terminal.graphics.kitty_image_identity(1, 0).unwrap();
        processor.advance(&mut terminal, b"\x1b_Ga=f,i=1,f=32,s=1,v=1,m=1;AAA=\x1b\\");
        terminal.graphics.clear_all_kitty_state();
        terminal.store_graphic(kitty_test_image(1, 1, 1));
        assert_ne!(
            terminal.graphics.kitty_image_identity(1, 0).unwrap(),
            identity
        );
        processor.advance(&mut terminal, b"\x1b_Gm=0;//8=\x1b\\");
        assert!(terminal.graphics.kitty_animations.is_empty());
        assert_eq!(terminal.graphics.total_bytes, 4);
    }

    #[test]
    fn numbered_images_bind_put_and_animation_replies_to_the_allocated_id() {
        use crate::performer::handler::Processor;
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut terminal = Crosswords::new(
            CrosswordsSize::new(80, 24),
            CursorShape::Block,
            TestListener {
                events: events.clone(),
            },
            WindowId::from(0),
            0,
            100,
        );
        terminal.graphics.cell_width = 8.0;
        terminal.graphics.cell_height = 16.0;
        let mut processor = Processor::default();
        processor.advance(&mut terminal, b"\x1b_Ga=t,I=42,f=32,s=1,v=1;/wAA/w==\x1b\\");
        let id = terminal.graphics.kitty_image_numbers[&42];
        events.borrow_mut().clear();
        processor.advance(
            &mut terminal,
            b"\x1b_Ga=p,I=42,p=3,C=1\x1b\\\x1b_Ga=f,I=42,f=32,s=1,v=1;AAD//w==\x1b\\",
        );
        let replies: Vec<_> = events
            .borrow()
            .iter()
            .filter_map(|event| match event {
                RioEvent::PtyWrite(_, reply) => Some(reply.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            replies,
            [
                format!("\x1b_Gi={id},I=42,p=3;OK\x1b\\"),
                format!("\x1b_Gi={id},I=42,r=2;OK\x1b\\")
            ]
        );
        assert!(terminal.graphics.kitty_placements.contains_key(&(id, 3)));
    }

    #[test]
    fn kitty_animation_errors_preserve_pixels_and_obey_quiet_modes() {
        use crate::performer::handler::Processor;
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut terminal = Crosswords::new(
            CrosswordsSize::new(80, 24),
            CursorShape::Block,
            TestListener {
                events: events.clone(),
            },
            WindowId::from(0),
            0,
            100,
        );
        let mut processor = Processor::default();
        processor.advance(&mut terminal, b"\x1b_Ga=t,i=1,f=32,s=1,v=1;/wAA/w==\x1b\\");
        terminal.graphics.total_limit = 4;
        for quiet in 0..=2 {
            for (command, payload, error) in [
                (
                    "a=f,i=1,r=9,f=32,s=1,v=1",
                    "AQI=",
                    "i=1;ENODATA: insufficient data",
                ),
                (
                    "a=f,i=99,r=9,f=99,s=1,v=1",
                    "AQI=",
                    "i=99,r=9;ENOENT: image not found",
                ),
                (
                    "a=f,i=1,r=9,f=99,s=1,v=1",
                    "AQI=",
                    "i=1,r=9;EINVAL: unsupported format",
                ),
                (
                    "a=f,i=1,r=2,c=99,f=32,s=1,v=1",
                    "AQIDBA==",
                    "i=1,r=2;EINVAL: base frame not found",
                ),
                (
                    "a=f,i=99,r=2,f=32,s=1,v=1",
                    "AQIDBA==",
                    "i=99,r=2;ENOENT: image not found",
                ),
                (
                    "a=f,i=1,r=2,f=32,s=2,v=1",
                    "AQIDBAECAwQ=",
                    "i=1;EINVAL: frame dimensions exceed image",
                ),
                (
                    "a=c,i=1,r=1,c=99",
                    "",
                    "i=1;ENOENT:destination frame not found",
                ),
                (
                    "a=c,i=1,r=1,c=1",
                    "",
                    "i=1;EINVAL:source and destination rectangles overlap",
                ),
                (
                    "a=f,i=1,r=2,f=32,s=1,v=1",
                    "AQIDBA==",
                    "i=1,r=2;ENOSPC: animation frame storage full",
                ),
            ] {
                events.borrow_mut().clear();
                processor.advance(
                    &mut terminal,
                    format!("\x1b_G{command},q={quiet};{payload}\x1b\\x").as_bytes(),
                );
                let replies: Vec<_> = events
                    .borrow()
                    .iter()
                    .filter_map(|event| match event {
                        RioEvent::PtyWrite(_, reply) => Some(reply.clone()),
                        _ => None,
                    })
                    .collect();
                assert_eq!(
                    replies,
                    if quiet == 2 {
                        vec![]
                    } else {
                        vec![format!("\x1b_G{error}\x1b\\")]
                    }
                );
                assert_eq!(
                    terminal.graphics.kitty_images[&1].data.pixels,
                    [255, 0, 0, 255]
                );
                assert_eq!(terminal.graphics.total_bytes, 4);
                assert!(terminal.graphics.kitty_animations.is_empty());
            }
        }
        assert_eq!(terminal.grid.cursor.pos.col, Column(27));
    }

    #[test]
    fn kitty_animation_frame_deletion_promotes_root_and_supports_image_numbers() {
        use crate::performer::handler::Processor;
        let mut terminal = make_crosswords();
        let mut processor = Processor::default();
        processor.advance(&mut terminal, b"\x1b_Ga=t,I=42,f=32,s=1,v=1;/wAA/w==\x1b\\");
        let id = terminal.graphics.kitty_image_numbers[&42];
        assert_ne!(id, 0);
        processor.advance(&mut terminal,b"\x1b_Ga=f,I=42,f=32,s=1,v=1;AAD//w==\x1b\\\x1b_Ga=a,I=42,c=2\x1b\\\x1b_Ga=d,d=f,I=42,r=1\x1b\\");
        assert_eq!(terminal.graphics.total_bytes, 4);
        assert_eq!(
            terminal.graphics.kitty_images[&id].data.pixels,
            [0, 0, 255, 255]
        );
        assert_eq!(terminal.graphics.kitty_animations[&id].frame_count(), 1);
        processor.advance(&mut terminal, b"\x1b_Ga=d,d=f,I=42,r=99\x1b\\");
        assert!(terminal.graphics.kitty_images.contains_key(&id));
        processor.advance(&mut terminal, b"\x1b_Ga=d,d=F,I=42,r=99\x1b\\");
        assert!(terminal.graphics.kitty_images.is_empty());
        assert!(terminal.graphics.kitty_animations.is_empty());
        assert_eq!(terminal.graphics.total_bytes, 0);
    }

    #[test]
    fn kitty_animation_bytes_follow_eviction_replacement_and_both_screens() {
        use crate::performer::handler::Processor;
        let mut terminal = make_crosswords();
        let mut processor = Processor::default();
        let root = b"\x1b_Ga=t,i=1,f=32,s=1,v=1;/wAA/w==\x1b\\";
        let frame = b"\x1b_Ga=f,i=1,f=32,s=1,v=1;AAD//w==\x1b\\";
        processor.advance(&mut terminal, root);
        processor.advance(&mut terminal, frame);
        assert_eq!(terminal.graphics.total_bytes, 8);
        terminal.swap_alt();
        processor.advance(&mut terminal, root);
        processor.advance(&mut terminal, frame);
        assert_eq!(terminal.graphics.total_bytes, 16);
        terminal.swap_alt();
        terminal.graphics.total_limit = 16;
        processor.advance(&mut terminal, frame);
        assert_eq!(terminal.graphics.total_bytes, 12);
        assert_eq!(terminal.graphics.kitty_animations[&1].frame_count(), 3);
        assert!(terminal
            .graphics
            .kitty_inactive_screen
            .kitty_images
            .is_empty());
        assert!(terminal
            .graphics
            .kitty_inactive_screen
            .kitty_animations
            .is_empty());
        processor.advance(&mut terminal, root);
        assert!(terminal.graphics.kitty_animations.is_empty());
        assert_eq!(terminal.graphics.total_bytes, 4);
        terminal.graphics.clear_all_kitty_state();
        assert_eq!(terminal.graphics.total_bytes, 0);
    }

    #[test]
    fn chunked_animation_frame_keeps_the_first_command_metadata() {
        use crate::performer::handler::Processor;
        let mut terminal = make_crosswords();
        let mut processor = Processor::default();
        processor.advance(&mut terminal,b"\x1b_Ga=t,i=1,f=32,s=1,v=1;/wAA/w==\x1b\\\x1b_Ga=f,i=1,f=32,s=1,v=1,m=1,z=60;AAA=\x1b\\");
        assert!(terminal.graphics.kitty_animations.is_empty());
        processor.advance(
            &mut terminal,
            b"\x1b_Gm=0;//8=\x1b\\\x1b_Ga=a,i=1,c=2\x1b\\",
        );
        assert_eq!(
            terminal.graphics.kitty_images[&1].data.pixels,
            [0, 0, 255, 255]
        );
        assert_eq!(terminal.graphics.kitty_animations[&1].frame_count(), 2);
        assert_eq!(terminal.graphics.total_bytes, 8);
    }

    #[test]
    fn kitty_animation_frame_control_and_composition_use_the_real_protocol_path() {
        use crate::performer::handler::Processor;
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut terminal = Crosswords::new(
            CrosswordsSize::new(80, 24),
            CursorShape::Block,
            TestListener {
                events: events.clone(),
            },
            WindowId::from(0),
            0,
            100,
        );
        terminal.graphics.cell_width = 8.0;
        terminal.graphics.cell_height = 16.0;
        let mut processor = Processor::default();
        processor.advance(
            &mut terminal,
            b"\x1b_Ga=T,i=7,p=1,f=32,s=1,v=1,C=1;/wAA/w==\x1b\\",
        );
        events.borrow_mut().clear();
        processor.advance(
            &mut terminal,
            b"\x1b_Ga=f,i=7,f=32,s=1,v=1,z=50;AAD//w==\x1b\\",
        );
        let replies: Vec<_> = events
            .borrow()
            .iter()
            .filter_map(|event| match event {
                RioEvent::PtyWrite(_, reply) => Some(reply.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(replies, ["\x1b_Gi=7,r=2;OK\x1b\\"]);
        assert_eq!(terminal.graphics.total_bytes, 8);
        assert_eq!(
            terminal.graphics.kitty_images[&7].data.pixels,
            [255, 0, 0, 255]
        );
        let stamp = terminal.graphics.kitty_images[&7].transmission_time;
        events.borrow_mut().clear();
        processor.advance(&mut terminal, b"\x1b_Ga=a,i=7,c=2\x1b\\");
        assert!(!events
            .borrow()
            .iter()
            .any(|event| matches!(event, RioEvent::PtyWrite(..))));
        assert_eq!(
            terminal.graphics.kitty_images[&7].data.pixels,
            [0, 0, 255, 255]
        );
        assert!(terminal.graphics.kitty_images[&7].transmission_time > stamp);
        processor.advance(
            &mut terminal,
            b"\x1b_Ga=c,i=7,r=2,c=1,w=1,h=1,C=1\x1b\\\x1b_Ga=a,i=7,c=1\x1b\\",
        );
        assert_eq!(
            terminal.graphics.kitty_images[&7].data.pixels,
            [0, 0, 255, 255]
        );
        processor.advance(&mut terminal, b"\x1b_Ga=d,d=I,i=7\x1b\\");
        assert_eq!(terminal.graphics.total_bytes, 0);
        assert!(terminal.graphics.kitty_animations.is_empty());
    }

    #[test]
    fn kitty_animation_budget_rejection_preserves_rgb_root_until_conversion_fits() {
        use crate::performer::handler::Processor;
        use rio_graphics::ColorType;
        let mut terminal = make_crosswords();
        let mut root = kitty_test_image(1, 1, 1);
        root.color_type = ColorType::Rgb;
        root.pixels = vec![1, 2, 3];
        terminal.store_graphic(root);
        terminal.graphics.total_limit = 7;
        let mut processor = Processor::default();
        let frame = b"\x1b_Ga=f,i=1,f=32,s=1,v=1;AQIDBA==\x1b\\";
        processor.advance(&mut terminal, frame);
        assert_eq!(terminal.graphics.total_bytes, 3);
        assert_eq!(
            terminal.graphics.kitty_images[&1].data.color_type,
            ColorType::Rgb
        );
        assert!(terminal.graphics.kitty_animations.is_empty());
        terminal.graphics.total_limit = 8;
        processor.advance(&mut terminal, frame);
        assert_eq!(terminal.graphics.total_bytes, 8);
        assert_eq!(
            terminal.graphics.kitty_images[&1].data.pixels,
            [1, 2, 3, 255]
        );
        assert_eq!(terminal.graphics.kitty_animations[&1].frame_count(), 2);
        terminal.graphics.clear_all_kitty_state();
        assert_eq!(terminal.graphics.total_bytes, 0);
    }

    #[test]
    fn relative_geometry_keeps_its_own_crop_and_offsets_across_reflow() {
        use crate::ansi::graphics::{kitty_overlay_geometry, OverlayViewport};
        use crate::performer::handler::Processor;
        let mut terminal = Crosswords::new(
            CrosswordsSize::new_with_dimensions(50, 10, 500, 200, 10, 20),
            CursorShape::Block,
            VoidListener {},
            WindowId::from(0),
            0,
            100,
        );
        let mut processor = Processor::default();
        processor.advance(&mut terminal, b"$ kitten icat /path/to/image.png\r\n\x1b_Ga=T,i=1,p=1,f=32,s=1,v=1,C=1,X=8,Y=10;AQIDBA==\x1b\\\x1b_Ga=T,i=2,p=1,f=32,s=2,v=1,c=1,r=1,x=1,w=1,X=3,Y=4,P=1,Q=1,H=2,V=1;AQIDBAECAwQ=\x1b\\");
        for (resize, x, y) in [(false, 23.0, 44.0), (true, 27.0, 76.0)] {
            if resize {
                terminal.resize(CrosswordsSize::new_with_dimensions(
                    20, 10, 240, 240, 12, 24,
                ));
            }
            let snapshot = terminal.graphics.resolved_kitty_placements();
            let child = snapshot.iter().find(|p| p.image_id == 2).unwrap();
            let viewport = OverlayViewport {
                origin_x: 0.0,
                origin_y: 0.0,
                cell_width: terminal.graphics.cell_width,
                cell_height: terminal.graphics.cell_height,
                history_size: (terminal.grid.lines_evicted()
                    + terminal.history_size() as u64)
                    as i64,
                display_offset: 0,
                screen_lines: 10,
            };
            let geometry = kitty_overlay_geometry(child, 2, 1, &viewport).unwrap();
            if resize {
                // The wrapped command shifts the root to absolute row 2;
                // trailing blanks absorb the added row without growing history.
                assert_eq!(terminal.graphics.kitty_placements[&(1, 1)].dest_row, 2);
                assert_eq!(terminal.history_size(), 0);
            }
            assert_eq!((geometry.x, geometry.y), (x, y));
            assert_eq!(geometry.source_rect, [0.5, 0.0, 1.0, 1.0]);
        }
    }

    #[test]
    fn retransmission_removes_dormant_crop_placements() {
        use crate::performer::handler::Processor;
        let mut terminal = make_crosswords();
        terminal.graphics.cell_width = 8.0;
        terminal.graphics.cell_height = 16.0;
        let mut processor = Processor::default();
        processor.advance(
            &mut terminal,
            b"\x1b_Ga=T,i=1,p=1,f=32,s=1,v=1,x=2,C=1;AQIDBA==\x1b\\",
        );
        let placement = &terminal.graphics.kitty_placements[&(1, 1)];
        assert_eq!((placement.columns, placement.rows), (0, 1));
        processor.advance(
            &mut terminal,
            b"\x1b_Ga=t,i=1,f=32,s=4,v=1;AQIDBAECAwQBAgMEAQIDBA==\x1b\\",
        );
        assert!(!terminal.graphics.kitty_placements.contains_key(&(1, 1)));
        processor.advance(&mut terminal, b"\x1b_Ga=p,i=1,p=1,x=2,C=1\x1b\\");
        assert_eq!(terminal.graphics.kitty_placements[&(1, 1)].rows, 1);
    }

    #[test]
    fn reducing_primary_history_on_alt_screen_keeps_alt_parent_chains() {
        use crate::performer::handler::Processor;
        let mut terminal = make_crosswords();
        terminal.graphics.cell_width = 8.0;
        terminal.graphics.cell_height = 16.0;
        let mut processor = Processor::default();
        let images = b"\x1b_Ga=T,i=1,p=1,f=32,s=1,v=1,C=1;AQIDBA==\x1b\\\x1b_Ga=T,i=2,p=1,f=32,s=1,v=1,P=1,Q=1,H=1,V=1;AQIDBA==\x1b\\";
        processor.advance(&mut terminal, images);
        processor.advance(&mut terminal, b"\n\n\n\n\n\n");
        terminal.swap_alt();
        processor.advance(&mut terminal, images);
        terminal.set_scrollback_limits(0, None);
        assert_eq!(terminal.graphics.kitty_placements.len(), 2);
        assert!(terminal
            .graphics
            .kitty_inactive_screen
            .kitty_placements
            .is_empty());
        terminal.swap_alt();
        assert!(terminal.graphics.kitty_placements.is_empty());
        assert_eq!(
            terminal
                .graphics
                .kitty_inactive_screen
                .kitty_placements
                .len(),
            2
        );
    }

    #[test]
    fn clearing_or_reducing_history_reaps_relative_parent_chains() {
        use crate::performer::handler::Processor;
        for clear in [true, false] {
            let mut terminal = make_crosswords();
            terminal.graphics.cell_width = 8.0;
            terminal.graphics.cell_height = 16.0;
            let mut processor = Processor::default();
            processor.advance(&mut terminal, b"\x1b_Ga=T,i=1,p=1,f=32,s=1,v=1,C=1;AQIDBA==\x1b\\\x1b_Ga=T,i=2,p=1,f=32,s=1,v=1,P=1,Q=1,H=1,V=1;AQIDBA==\x1b\\\n\n\n\n\n\n");
            assert!(terminal.history_size() > 0);
            assert_eq!(terminal.graphics.kitty_placements.len(), 2);
            if clear {
                terminal.clear_saved_history();
            } else {
                terminal.set_scrollback_limits(0, None);
            }
            assert!(
                terminal.graphics.kitty_placements.is_empty(),
                "clear={clear}"
            );
            assert_eq!(terminal.graphics.kitty_images.len(), 2);
        }
    }

    #[test]
    fn omitted_relative_parent_id_selects_a_stable_virtual_root() {
        use crate::performer::handler::Processor;
        let mut terminal = make_crosswords();
        terminal.graphics.cell_width = 8.0;
        terminal.graphics.cell_height = 16.0;
        let mut processor = Processor::default();
        processor.advance(&mut terminal, b"\x1b_Ga=T,i=1,p=4,f=32,s=1,v=1,U=1;AQIDBA==\x1b\\\x1b_Ga=p,i=1,p=2,U=1\x1b\\\x1b_Ga=T,i=2,p=1,f=32,s=1,v=1,P=1;AQIDBA==\x1b\\");
        assert_eq!(
            terminal.graphics.kitty_placements[&(2, 1)]
                .relative
                .unwrap()
                .placement_id,
            2
        );
        processor.advance(&mut terminal, b"\x1b_Ga=d,d=i,i=1,p=2\x1b\\");
        // Q=0 resolves once; deleting that root must not attach the child
        // to a different placement of the same image.
        assert!(terminal.graphics.kitty_placements.is_empty());
        assert!(terminal
            .graphics
            .kitty_virtual_placements
            .contains_key(&(1, 4)));
    }

    #[test]
    fn relative_chains_can_root_at_virtual_placements_and_follow_kind_replacement() {
        use crate::performer::handler::Processor;
        let mut terminal = make_crosswords();
        terminal.graphics.cell_width = 8.0;
        terminal.graphics.cell_height = 16.0;
        let mut processor = Processor::default();
        processor.advance(&mut terminal, b"\x1b_Ga=T,i=1,p=4,f=32,s=1,v=1,U=1;AQIDBA==\x1b\\\x1b_Ga=T,i=2,p=1,f=32,s=1,v=1,P=1,Q=4,H=-1,V=2;AQIDBA==\x1b\\\x1b_Ga=T,i=3,p=1,f=32,s=1,v=1,P=2,Q=1,H=2,V=1;AQIDBA==\x1b\\");
        assert_eq!(terminal.graphics.kitty_placements.len(), 2);
        let snapshot = terminal.graphics.resolved_kitty_placements();
        let child = snapshot.iter().find(|p| p.image_id == 3).unwrap();
        let root = child.relative.unwrap();
        assert_eq!(
            (
                root.image_id,
                root.placement_id,
                root.horizontal,
                root.vertical
            ),
            (1, 4, 1, 3)
        );
        // Changing the root's kind keeps the same public identity and children.
        processor.advance(&mut terminal, b"\x1b_Ga=p,i=1,p=4,C=1\x1b\\");
        assert!(!terminal
            .graphics
            .kitty_virtual_placements
            .contains_key(&(1, 4)));
        assert_eq!(terminal.graphics.kitty_placements.len(), 3);
        processor.advance(&mut terminal, b"\x1b_Ga=p,i=1,p=4,U=1\x1b\\");
        assert!(!terminal.graphics.kitty_placements.contains_key(&(1, 4)));
        assert_eq!(terminal.graphics.kitty_placements.len(), 2);
        processor.advance(&mut terminal, b"\x1b_Ga=d,d=i,i=1,p=4\x1b\\");
        assert!(terminal.graphics.kitty_placements.is_empty());
    }

    #[test]
    fn relative_placement_errors_reach_the_client_with_quiet_modes() {
        use crate::performer::handler::Processor;
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut terminal = Crosswords::new(
            CrosswordsSize::new(80, 24),
            CursorShape::Block,
            TestListener {
                events: events.clone(),
            },
            WindowId::from(0),
            0,
            100,
        );
        terminal.graphics.cell_width = 8.0;
        terminal.graphics.cell_height = 16.0;
        let mut processor = Processor::default();
        processor.advance(&mut terminal, b"\x1b_Ga=T,i=1,p=1,f=32,s=1,v=1,C=1;AQIDBA==\x1b\\\x1b_Ga=T,i=2,p=1,f=32,s=1,v=1,P=1,Q=1;AQIDBA==\x1b\\");
        for quiet in 0..=2 {
            for (parent, error) in [
                ("P=1,Q=1", "EINVAL: placement cannot be its own parent"),
                ("P=99", "ENOPARENT: parent image not found"),
                ("P=1,Q=99", "ENOPARENT: parent placement not found"),
                ("P=2,Q=1", "ECYCLE: parent chain creates a cycle"),
            ] {
                events.borrow_mut().clear();
                processor.advance(
                    &mut terminal,
                    format!("\x1b_Ga=p,i=1,p=1,{parent},q={quiet}\x1b\\ok").as_bytes(),
                );
                let replies: Vec<_> = events
                    .borrow()
                    .iter()
                    .filter_map(|event| match event {
                        RioEvent::PtyWrite(_, reply) => Some(reply.clone()),
                        _ => None,
                    })
                    .collect();
                assert_eq!(
                    replies,
                    if quiet == 2 {
                        vec![]
                    } else {
                        vec![format!("\x1b_Gi=1,p=1;{error}\x1b\\")]
                    }
                );
                assert!(terminal.graphics.kitty_placements[&(1, 1)]
                    .relative
                    .is_none());
            }
        }
        assert_eq!(terminal.grid.cursor.pos.col, Column(24));
    }

    #[test]
    fn relative_placements_follow_parent_chains_and_die_with_the_parent() {
        use crate::ansi::graphics::{kitty_overlay_geometry, OverlayViewport};
        use crate::performer::handler::Processor;
        let mut terminal = make_crosswords();
        terminal.graphics.cell_width = 8.0;
        terminal.graphics.cell_height = 16.0;
        let mut processor = Processor::default();
        for (id, parent) in [(1, ""), (2, ",P=1,Q=1,H=-1,V=2"), (3, ",P=2,Q=1,H=2,V=-1")]
        {
            processor.advance(
                &mut terminal,
                format!("\x1b_Ga=T,i={id},p=1,f=32,s=1,v=1,C=1{parent};AQIDBA==\x1b\\")
                    .as_bytes(),
            );
        }
        assert_eq!(terminal.graphics.kitty_placements.len(), 3);
        assert_eq!(terminal.grid.cursor.pos, Pos::new(Line(0), Column(0)));
        let snapshot = terminal.graphics.resolved_kitty_placements();
        let child = snapshot.iter().find(|p| p.image_id == 3).unwrap();
        let viewport = OverlayViewport {
            origin_x: 0.0,
            origin_y: 0.0,
            cell_width: 8.0,
            cell_height: 16.0,
            history_size: 0,
            display_offset: 0,
            screen_lines: 4,
        };
        let geometry = kitty_overlay_geometry(child, 1, 1, &viewport).unwrap();
        assert_eq!((geometry.x, geometry.y), (8.0, 16.0));
        // Replacing the root moves descendants without changing their metadata.
        processor.advance(&mut terminal, b"\x1b[2;2H\x1b_Ga=p,i=1,p=1,C=1\x1b\\");
        let moved = terminal.graphics.resolved_kitty_placements();
        let child = moved.iter().find(|p| p.image_id == 3).unwrap();
        let geometry = kitty_overlay_geometry(child, 1, 1, &viewport).unwrap();
        assert_eq!((geometry.x, geometry.y), (16.0, 32.0));
        processor.advance(&mut terminal, b"\x1b_Ga=d,d=i,i=1,p=1\x1b\\");
        assert!(terminal.graphics.kitty_placements.is_empty());
        assert_eq!(terminal.graphics.kitty_images.len(), 3);
    }

    #[test]
    fn relative_placement_rejection_preserves_existing_pixels_and_placements() {
        use crate::performer::handler::Processor;
        let mut terminal = make_crosswords();
        terminal.graphics.cell_width = 8.0;
        terminal.graphics.cell_height = 16.0;
        let mut processor = Processor::default();
        processor.advance(
            &mut terminal,
            b"\x1b_Ga=T,i=1,p=1,f=32,s=1,v=1,C=1;AQIDBA==\x1b\\",
        );
        let original = terminal.graphics.kitty_images[&1].transmission_time;
        for parent in [",P=1,Q=1", ",P=99", ",P=1,Q=99"] {
            processor.advance(
                &mut terminal,
                format!("\x1b_Ga=p,i=1,p=1,C=1{parent}\x1b\\").as_bytes(),
            );
            assert_eq!(
                terminal.graphics.kitty_images[&1].transmission_time,
                original
            );
            assert!(terminal.graphics.kitty_placements[&(1, 1)]
                .relative
                .is_none());
        }
        processor.advance(
            &mut terminal,
            b"\x1b_Ga=T,i=2,p=1,f=32,s=1,v=1,C=1,P=1;AQIDBA==\x1b\\",
        );
        processor.advance(&mut terminal, b"\x1b_Ga=p,i=1,p=1,P=2,Q=1\x1b\\");
        assert!(terminal.graphics.kitty_placements[&(1, 1)]
            .relative
            .is_none());
        for id in 3..=9 {
            processor.advance(
                &mut terminal,
                format!(
                    "\x1b_Ga=T,i={id},p=1,f=32,s=1,v=1,C=1,P={},Q=1;AQIDBA==\x1b\\",
                    id - 1
                )
                .as_bytes(),
            );
            assert!(terminal.graphics.kitty_placements.contains_key(&(id, 1)));
        }
        processor.advance(
            &mut terminal,
            b"\x1b_Ga=T,i=10,p=1,f=32,s=1,v=1,C=1,P=9,Q=1;AQIDBA==\x1b\\",
        );
        assert!(terminal.graphics.kitty_images.contains_key(&10));
        assert!(!terminal.graphics.kitty_placements.contains_key(&(10, 1)));
    }

    #[test]
    fn graphics_budget_errors_remove_replaced_images_and_allow_text_recovery() {
        use crate::performer::handler::Processor;
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let listener = TestListener {
            events: events.clone(),
        };
        let mut terminal = Crosswords::new(
            CrosswordsSize::new(80, 24),
            CursorShape::Block,
            listener,
            WindowId::from(0),
            0,
            100,
        );
        terminal.graphics.total_limit = 4;
        let mut processor = Processor::default();
        for quiet in [0, 1, 2] {
            processor
                .advance(&mut terminal, b"\x1b_Ga=t,i=1,f=32,s=1,v=1;AQIDBA==\x1b\\");
            events.borrow_mut().clear();
            let command =
                format!("\x1b_Ga=t,i=1,f=32,s=2,v=1,q={quiet};AQIDBAUGBwg=\x1b\\");
            processor.advance(&mut terminal, command.as_bytes());
            let replies: Vec<String> = events
                .borrow()
                .iter()
                .filter_map(|event| match event {
                    RioEvent::PtyWrite(_, value) => Some(value.clone()),
                    _ => None,
                })
                .collect();
            if quiet == 2 {
                assert!(replies.is_empty());
            } else {
                assert_eq!(replies.len(), 1);
                assert!(replies[0].contains("ENOSPC"), "{replies:?}");
            }
            assert_eq!(terminal.graphics.total_bytes, 0);
            assert!(!terminal.graphics.kitty_images.contains_key(&1));
        }
        processor.advance(&mut terminal, b"\x1b_Ga=t,i=1,f=32,s=1,v=1;AQIDBA==\x1b\\");
        for id in 0..4096 {
            processor.advance(
                &mut terminal,
                format!("\x1b_Ga=p,U=1,i=1,p={id},q=2\x1b\\").as_bytes(),
            );
        }
        assert_eq!(terminal.graphics.kitty_virtual_placements.len(), 4096);
        events.borrow_mut().clear();
        processor.advance(&mut terminal, b"\x1b_Ga=p,U=1,i=1,p=9999,q=1\x1b\\ok");
        assert!(events.borrow().iter().any(|event| matches!(event, RioEvent::PtyWrite(_, reply) if reply.contains("ENOSPC"))));
        assert_eq!(terminal.graphics.kitty_virtual_placements.len(), 4096);
        assert_eq!(terminal.grid[Line(0)][Column(0)].c(), 'o');
        assert_eq!(terminal.grid[Line(0)][Column(1)].c(), 'k');
    }

    #[test]
    fn graphics_updates_allow_one_outstanding_batch_and_keep_latest_pixels() {
        use crate::performer::handler::Processor;
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let listener = TestListener {
            events: events.clone(),
        };
        let mut terminal = Crosswords::new(
            CrosswordsSize::new(80, 24),
            CursorShape::Block,
            listener,
            WindowId::from(0),
            0,
            100,
        );
        terminal.graphics.coalesce_updates = true;
        let mut processor = Processor::default();
        for _ in 0..100 {
            processor.advance(
                &mut terminal,
                b"\x1b_Ga=T,U=1,i=1,f=32,s=1,v=1,q=2;AQIDBA==\x1b\\",
            );
        }
        let batches = || {
            events
                .borrow()
                .iter()
                .filter(|event| matches!(event, RioEvent::UpdateGraphics { .. }))
                .count()
        };
        assert_eq!(batches(), 1);
        assert_eq!(terminal.graphics.pending_images.len(), 1);
        terminal.acknowledge_graphics_update();
        assert_eq!(batches(), 2);
        assert!(terminal.graphics.pending_images.is_empty());
        terminal.acknowledge_graphics_update();
        assert!(!terminal.graphics.update_in_flight);
        assert_eq!(batches(), 2);
    }

    #[test]
    fn history_byte_configuration_follows_primary_screen_and_survives_reset() {
        use crate::performer::handler::Processor;
        let mut terminal = make_crosswords();
        let mut processor = Processor::default();
        terminal.set_scrollback_limits(100, Some(0));
        processor.advance(&mut terminal, &b"line\r\n".repeat(20));
        assert_eq!(terminal.history_size(), 0);
        processor.advance(&mut terminal, b"\x1b[?1049h");
        terminal.set_scrollback_limits(100, Some(10_000));
        processor.advance(&mut terminal, &b"line\r\n".repeat(20));
        assert_eq!(terminal.history_size(), 0);
        processor.advance(&mut terminal, b"\x1b[?1049l\x1bc");
        processor.advance(&mut terminal, &b"line\r\n".repeat(20));
        assert!(terminal.history_size() > 0);
        assert!(terminal.grid.history_budget_charge() <= 10_000);
    }

    #[test]
    fn disabling_history_erases_image_anchors_and_restores_live_viewport() {
        use crate::performer::handler::Processor;
        let mut terminal = Crosswords::new(
            CrosswordsSize::new(8, 4),
            CursorShape::Block,
            VoidListener,
            WindowId::from(0),
            0,
            100,
        );
        terminal.graphics.cell_width = 8.0;
        terminal.graphics.cell_height = 16.0;
        let mut processor = Processor::default();
        processor.advance(&mut terminal, b"\x1b[2;3H\x1b_Ga=T,i=1,p=1,f=32,s=1,v=1,c=2,r=8,C=1;AQIDBA==\x1b\\\x1b[H\n\n\n\n\n\n\n");
        assert_eq!(terminal.history_size(), 4);
        assert!(terminal.graphics.kitty_placements.contains_key(&(1, 1)));
        terminal.scroll_display(Scroll::Delta(2));
        terminal.set_scrollback_limits(100, Some(0));
        assert_eq!(terminal.history_size(), 0);
        assert!(terminal.graphics.kitty_placements.is_empty());
        assert!(terminal.graphics.kitty_images.contains_key(&1));
        terminal.set_scrollback_limits(100, None);
        processor.advance(&mut terminal, b"\n\n");
        assert_eq!(terminal.grid.display_offset(), 0);
    }

    #[test]
    fn history_budget_includes_styles_created_by_erase_and_sync_replay() {
        use crate::performer::handler::Processor;
        let mut terminal = make_crosswords();
        let mut processor = Processor::default();
        processor.advance(&mut terminal, &b"line\r\n".repeat(30));
        let budget = terminal.grid.history_budget_charge();
        terminal.set_scrollback_limits(10, Some(budget));
        for input in [
            b"\x1b[4;44m\x1b[2J".as_slice(),
            b"\x1b[?2026h\x1b[9;45m\x1b[2J\x1b[?2026l",
        ] {
            processor.advance(&mut terminal, input);
            assert!(terminal.grid.history_budget_charge() <= budget);
        }
    }

    #[test]
    fn program_status_protocol_replies_route_and_validates_atomically() {
        use crate::performer::handler::Processor;
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut terminal = Crosswords::new(
            CrosswordsSize::new(10, 10),
            CursorShape::Block,
            TestListener {
                events: events.clone(),
            },
            WindowId::from(0),
            42,
            10,
        );
        let mut parser = Processor::default();
        for terminator in ["\x07", "\x1b\\"] {
            parser.advance(&mut terminal, format!("\x1b]7501;?{terminator}").as_bytes());
            assert!(events.borrow().iter().any(|e| matches!(e,
                RioEvent::PtyWrite(42, text) if text == &format!(
                    "\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth{terminator}"))));
        }
        assert!(!terminal.program_status.received_report());
        for chunk in b"\x1b]7501;state=blocked:kind=question:msg=aGk=\x1b\\".chunks(3) {
            parser.advance(&mut terminal, chunk);
        }
        assert_eq!(
            terminal.program_status.current().unwrap().msg.as_deref(),
            Some("hi")
        );
        assert!(events
            .borrow()
            .iter()
            .any(|e| matches!(e, RioEvent::ProgramStatusChanged(42))));
        let revision = terminal.program_status.revision();
        for body in [
            "state=error:msg=AA==".to_owned(),
            "state=clear:id=bad//id".to_owned(),
            "state=done;app=cargo".to_owned(),
            format!("state=done:unknown={}", "a".repeat(4096)),
        ] {
            parser.advance(&mut terminal, format!("\x1b]7501;{body}\x07").as_bytes());
            assert_eq!(terminal.program_status.revision(), revision);
        }
        // Detection never resets existing records.
        parser.advance(&mut terminal, b"\x1b]7501;?\x07");
        assert_eq!(terminal.program_status.revision(), revision);
    }

    #[test]
    fn program_status_lifetime_is_independent_of_screen_and_soft_reset() {
        use crate::performer::handler::Processor;
        use crate::program_status::ProgramState;
        let mut terminal = make_crosswords();
        let mut parser = Processor::default();
        parser.advance(
            &mut terminal,
            b"\x1b]7501;state=working\x07\x1b]7501;state=done:id=result\x07",
        );
        parser.advance(&mut terminal, b"\x1b[?1049h\x1b[!p\x1b[?1049l");
        assert_eq!(terminal.program_status.records().len(), 2);
        // Continuation markers do not finish the program's lifetime.
        parser.advance(&mut terminal, b"\x1b]133;P;k=c\x07");
        assert_eq!(terminal.program_status.records().len(), 2);
        parser.advance(&mut terminal, b"\x1b]133;A\x07");
        assert_eq!(terminal.program_status.records().len(), 1);
        assert_eq!(
            terminal.program_status.current().unwrap().state,
            ProgramState::Done
        );
        parser.advance(&mut terminal, b"\x1b]7501;state=blocked:id=child\x07");
        terminal.exit();
        assert_eq!(terminal.program_status.records().len(), 1);
        parser.advance(&mut terminal, b"\x1bc");
        assert!(terminal.program_status.records().is_empty());
        assert!(!terminal.program_status.received_report());
    }

    #[test]
    fn legacy_progress_clear_does_not_resurrect_an_earlier_native_result() {
        use crate::performer::handler::Processor;
        use crate::program_status::ProgramState;
        let mut terminal = make_crosswords();
        let mut parser = Processor::default();
        parser.advance(
            &mut terminal,
            b"\x1b]7501;state=done:app=old-build\x07\x1b]133;A\x07\x1b]9;4;1;40\x07",
        );
        assert_eq!(
            terminal.program_status.display_status().unwrap().progress,
            Some(40)
        );
        parser.advance(&mut terminal, b"\x1b]9;4;0\x07");
        assert!(terminal.program_status.display_status().is_none());
        parser.advance(&mut terminal, b"\x1b]133;A\x07");
        assert!(terminal.program_status.display_status().is_none());
        parser.advance(&mut terminal, b"\x1b]7501;state=working:app=new-build\x07");
        let status = terminal.program_status.display_status().unwrap();
        assert_eq!(status.state, ProgramState::Working);
        assert_eq!(status.app.as_deref(), Some("new-build"));
    }

    #[test]
    fn native_status_owns_the_command_despite_interleaved_legacy_reports() {
        use crate::performer::handler::Processor;
        for terminator in ["\x07", "\x1b\\"] {
            for state in ["working", "blocked", "done", "error", "idle", "clear"] {
                let mut terminal = make_crosswords();
                let mut parser = Processor::default();
                parser.advance(&mut terminal, b"\x1b]9;4;1;20\x07");
                let native = format!("\x1b]7501;state={state}:app=native{terminator}");
                for chunk in native.as_bytes().chunks(3) {
                    parser.advance(&mut terminal, chunk);
                }
                let expected = terminal.program_status.display_status();
                let revision = terminal.program_status.revision();
                for legacy in ["1;90", "3", "4;65", "2;10", "0"] {
                    let sequence = format!("\x1b]9;4;{legacy}{terminator}");
                    for chunk in sequence.as_bytes().chunks(2) {
                        parser.advance(&mut terminal, chunk);
                    }
                    assert_eq!(terminal.program_status.display_status(), expected);
                    assert_eq!(terminal.program_status.revision(), revision);
                }
            }
        }
    }

    #[test]
    fn status_detection_and_malformed_native_reports_do_not_claim_progress() {
        use crate::performer::handler::Processor;
        let mut terminal = make_crosswords();
        let mut parser = Processor::default();
        for native in ["?", "state=unknown", "state=working:id=bad//id"] {
            parser.advance(&mut terminal, format!("\x1b]7501;{native}\x07").as_bytes());
            assert!(!terminal.program_status.received_report());
        }
        parser.advance(&mut terminal, b"\x1b]9;4;1;60\x07");
        assert_eq!(
            terminal.program_status.display_status().unwrap().progress,
            Some(60)
        );
    }

    #[test]
    fn program_status_sequence_limit_includes_actual_terminator() {
        use crate::performer::handler::Processor;
        let mut terminal = make_crosswords();
        let mut parser = Processor::default();
        for terminator in ["\x07", "\x1b\\"] {
            let header = "\x1b]7501;state=working:unknown=";
            let padding = crate::program_status::MAX_SEQUENCE_BYTES
                - header.len()
                - terminator.len();
            let sequence = format!("{header}{}{terminator}", "a".repeat(padding));
            let before = terminal.program_status.revision();
            parser.advance(&mut terminal, sequence.as_bytes());
            assert_eq!(terminal.program_status.revision(), before + 1);
            let oversized = format!("{header}{}{terminator}", "a".repeat(padding + 1));
            parser.advance(&mut terminal, oversized.as_bytes());
            assert_eq!(terminal.program_status.revision(), before + 1);
        }
    }

    #[test]
    fn legacy_alternate_modes_preserve_primary_and_clear_on_exit() {
        use crate::performer::handler::Processor;
        for mode in [47, 1047] {
            let mut terminal = make_crosswords();
            let mut parser = Processor::default();
            parser.advance(&mut terminal, format!("P\x1b[H\x1b[?{mode}hA").as_bytes());
            assert!(terminal.mode.contains(Mode::ALT_SCREEN));
            assert_eq!(terminal.inactive_grid[Line(0)][Column(0)].c(), 'P');
            parser.advance(&mut terminal, format!("\x1b[?{mode}l").as_bytes());
            assert_eq!(terminal.grid[Line(0)][Column(0)].c(), 'P');
            parser.advance(&mut terminal, b"\x1b[?47h");
            assert_eq!(
                terminal.grid[Line(0)][Column(0)].c(),
                if mode == 47 { 'A' } else { '\0' }
            );
        }
    }

    #[test]
    fn repeated_1049_clears_alt_and_restores_each_saved_cursor() {
        use crate::performer::handler::Processor;
        let mut terminal = make_crosswords();
        let mut parser = Processor::default();
        parser.advance(&mut terminal, b"P\x1b[?1049hA\x1b[?1049h");
        assert_eq!(terminal.grid[Line(0)][Column(1)].c(), '\0');
        parser.advance(&mut terminal, b"B\x1b8Y");
        assert_eq!(terminal.grid[Line(0)][Column(2)].c(), 'Y');
        parser.advance(&mut terminal, b"\x1b[?1049lX\x1b[?1049lZ");
        assert_eq!(terminal.grid[Line(0)][Column(0)].c(), 'P');
        assert_eq!(terminal.grid[Line(0)][Column(1)].c(), 'Z');
        assert_eq!(terminal.grid[Line(0)][Column(2)].c(), '\0');
    }

    #[test]
    fn saved_cursor_restores_origin_and_selected_charset() {
        use crate::performer::handler::Processor;
        let mut terminal = make_crosswords();
        let mut parser = Processor::default();
        parser.advance(&mut terminal, b"\x1b[2;4r\x1b[?6h\x1b)0\x0e\x1b7");
        parser.advance(&mut terminal, b"\x1b[?6l\x0f\x1b8\x1b[1;1Hq");
        assert!(terminal.mode.contains(Mode::ORIGIN));
        assert_eq!(terminal.grid[Line(1)][Column(0)].c(), '\u{2500}');
    }

    #[test]
    fn display_erasure_reclaims_unplaced_images_and_keeps_virtual_references() {
        use crate::performer::handler::Processor;
        let mut terminal = make_crosswords();
        let mut parser = Processor::default();
        parser.advance(&mut terminal, b"\x1b_Ga=t,i=1,f=32,s=1,v=1;AQIDBA==\x1b\\\x1b_Ga=T,i=2,U=1,c=1,r=1,f=32,s=1,v=1;AQIDBA==\x1b\\");
        assert!(terminal.graphics.kitty_images.contains_key(&1));
        assert!(terminal.graphics.kitty_images.contains_key(&2));
        parser.advance(&mut terminal, b"\x1b[2J");
        assert!(!terminal.graphics.kitty_images.contains_key(&1));
        assert!(terminal.graphics.kitty_images.contains_key(&2));
    }

    #[test]
    fn soft_reset_discards_only_the_active_saved_cursor() {
        use crate::performer::handler::Processor;
        let mut terminal = make_crosswords();
        let mut parser = Processor::default();
        parser.advance(
            &mut terminal,
            b"\x1b[2;3H\x1b7\x1b[?1049hA\x1b[3;4H\x1b7\x1b[!p",
        );
        assert_eq!(terminal.grid.cursor.pos, Pos::new(Line(2), Column(3)));
        parser.advance(&mut terminal, b"\x1b8");
        assert_eq!(terminal.grid.cursor.pos, Pos::new(Line(0), Column(0)));
        parser.advance(&mut terminal, b"\x1b[?1049l");
        assert_eq!(terminal.grid.cursor.pos, Pos::new(Line(1), Column(2)));
    }

    #[test]
    fn single_shift_applies_to_one_printed_character_across_chunks() {
        use crate::performer::handler::Processor;
        for chunk in [1, 2, 17, 65536] {
            for (text, expected) in [
                ("\x1b*0\x1bNqq", ['\u{2500}', 'q']),
                ("\x1b(0\x1b*B\x1bNqq", ['q', '\u{2500}']),
            ] {
                let mut terminal = make_crosswords();
                let mut parser = Processor::default();
                for bytes in text.as_bytes().chunks(chunk) {
                    parser.advance(&mut terminal, bytes);
                }
                assert_eq!(terminal.grid[Line(0)][Column(0)].c(), expected[0]);
                assert_eq!(terminal.grid[Line(0)][Column(1)].c(), expected[1]);
            }
        }
    }

    #[test]
    fn protected_cells_survive_selective_erasure_in_scalar_and_bulk_input() {
        use crate::crosswords::square::CellFlags;
        use crate::performer::handler::Processor;
        for iso in [false, true] {
            for chunk in [1, 17, 65536] {
                let mut terminal = new_term(8, 4);
                let mut parser = Processor::default();
                let start = if iso { "\x1bV" } else { "\x1b[1\"q" };
                let end = if iso { "\x1bW" } else { "\x1b[0\"q" };
                let text =
                    format!("{start}\x1b[41mP界e\u{301} {end}\x1b[0mU\r\x1b[44m\x1b[?2K");
                for bytes in text.as_bytes().chunks(chunk) {
                    parser.advance(&mut terminal, bytes);
                }
                assert_eq!(terminal.grid[Line(0)][Column(0)].c(), 'P');
                assert_eq!(terminal.grid[Line(0)][Column(1)].wide(), Wide::Wide);
                assert_eq!(terminal.grid[Line(0)][Column(2)].wide(), Wide::Spacer);
                assert!(terminal.grid[Line(0)][Column(4)]
                    .cell_flags()
                    .contains(CellFlags::PROTECTED));
                assert_eq!(terminal.grid[Line(0)][Column(5)].c(), '\0');
                parser.advance(&mut terminal, b"\x1b[2K");
                assert_eq!(
                    terminal.grid[Line(0)][Column(0)].c(),
                    if iso { 'P' } else { '\0' }
                );
            }
        }
    }

    #[test]
    fn mixed_state_insert_printing_keeps_wide_cells_and_wraps() {
        use crate::performer::handler::Processor;
        for chunk in [1, 17, 65536] {
            let mut term = new_term(3, 5);
            let mut parser = Processor::default();
            let text = "q界q\x1b[4h\x1b[Hq界\r\nZ\x1b[6n";
            for bytes in text.as_bytes().chunks(chunk) {
                parser.advance(&mut term, bytes);
            }
            assert!(term.grid[Line(0)][Column(2)].wrapline());
            assert_eq!(term.grid[Line(0)][Column(1)].c(), '界');
            assert_eq!(term.grid[Line(0)][Column(1)].wide(), Wide::Wide);
            assert_eq!(term.grid[Line(0)][Column(2)].wide(), Wide::Spacer);
        }
    }

    #[test]
    fn mixed_state_protection_end_retains_last_enabled_mode() {
        use crate::performer::handler::Processor;
        for (text, expected) in [
            ("\x1bVabc\x1b[0\"q\r\x1b[2K", 'a'),
            ("\x1b[1\"qabc\x1bW\r\x1b[2K", '\0'),
        ] {
            let mut term = new_term(8, 3);
            Processor::default().advance(&mut term, text.as_bytes());
            assert_eq!(term.grid[Line(0)][Column(0)].c(), expected);
        }
    }

    #[test]
    fn mixed_state_display_erasure_cancels_pending_wrap() {
        use crate::performer::handler::Processor;
        for command in ["J", "1J", "2J", "?J", "?1J", "?2J", "3J"] {
            let mut term = new_term(3, 3);
            let mut parser = Processor::default();
            parser.advance(&mut term, b"abc");
            assert!(term.grid.cursor.should_wrap);
            parser.advance(&mut term, format!("\x1b[{command}").as_bytes());
            assert_eq!(term.grid.cursor.should_wrap, command == "3J", "{command}");
        }
    }

    #[test]
    fn mixed_state_alt_entry_clears_with_destination_background() {
        use crate::config::colors::AnsiColor;
        use crate::performer::handler::Processor;
        let mut term = new_term(8, 3);
        let mut parser = Processor::default();
        parser.advance(&mut term, b"\x1b[44m\x1b[?1049h");
        assert_eq!(
            term.grid.style_of(&term.grid[Line(0)][Column(0)]).bg,
            AnsiColor::Named(NamedColor::Background)
        );
        parser.advance(&mut term, b"X\x1b[?1049l\x1b[42m\x1b[?1049h");
        assert_eq!(
            term.grid.style_of(&term.grid[Line(0)][Column(0)]).bg,
            AnsiColor::Indexed(4)
        );
        parser.advance(&mut term, b"Y");
        assert_eq!(
            term.grid.style_of(&term.grid[Line(0)][Column(0)]).bg,
            AnsiColor::Named(NamedColor::Green)
        );
    }

    #[test]
    fn mixed_state_edits_preserve_history_spacer() {
        use crate::performer::handler::Processor;
        for edit in ["e\u{301}", "\x1b[2@"] {
            let mut term = new_term(3, 3);
            let mut parser = Processor::default();
            parser.advance(&mut term, "ab界\r\ncd\r\nEF\x1b8".as_bytes());
            assert_eq!(term.grid[Line(-1)][Column(2)].wide(), Wide::LeadingSpacer);
            parser.advance(&mut term, edit.as_bytes());
            assert_eq!(term.grid[Line(-1)][Column(2)].wide(), Wide::LeadingSpacer);
        }
    }

    #[test]
    fn mixed_state_dch_resets_following_wrap_continuation() {
        use crate::performer::handler::Processor;
        let mut term = new_term(3, 5);
        let mut parser = Processor::default();
        parser.advance(&mut term, "ab界\r\ncd\r\nEF\x1b8".as_bytes());
        assert!(term.grid[Line(1)].wrap_continuation);
        parser.advance(&mut term, b"\x1b[P");
        assert!(!term.grid[Line(1)].wrap_continuation);
    }

    #[test]
    fn swap_alt_reinterns_cursor_style_into_alt_table() {
        use crate::config::colors::{AnsiColor, ColorRgb};
        let mut cw = make_crosswords();

        // Pad the primary table so the carried style's id differs from
        // what a fresh alt-table intern will assign; a foreign id would
        // then resolve to the default fallback instead of the carried
        // style.
        for r in 1..4 {
            cw.terminal_attribute(Attr::Background(AnsiColor::Spec(ColorRgb {
                r,
                g: 0,
                b: 0,
            })));
            cw.input('x');
        }
        let red = AnsiColor::Spec(ColorRgb {
            r: 200,
            g: 10,
            b: 10,
        });
        cw.terminal_attribute(Attr::Background(red));
        cw.input('x');

        cw.swap_alt();

        // The alt cursor template must resolve, in the ALT table, to the
        // style value carried over from the primary screen.
        let style = cw.grid.style_of(&cw.grid.cursor.template);
        assert_eq!(style.bg, red);
        // The alternate screen was cleared with its own default pen before
        // the primary cursor style was copied.
        let sq = cw.grid[Line(0)][Column(0)];
        assert_eq!(
            cw.grid.style_of(&sq).bg,
            AnsiColor::Named(NamedColor::Background)
        );
    }

    #[test]
    fn scrolling_snapshots_move_retained_row_allocations() {
        use std::fmt::Write;
        let mut cw = new_term(120, 80);
        let mut processor = crate::performer::handler::Processor::default();
        let mut input = String::new();
        for n in 0..200 {
            write!(input, "\x1b[31m{n}\x1b[0m e\u{301}\r\n").unwrap();
        }
        processor.advance(&mut cw, input.as_bytes());
        cw.scroll_display(Scroll::Top);
        let (mut rows, mut styles, mut extras) = Default::default();
        cw.snapshot_visible(&TerminalDamage::Full, &mut rows, &mut styles, &mut extras);
        let allocation = rows[1].inner.as_ptr();
        let expected = rows[1].clone();
        let expected_style = styles[1].clone();
        cw.scroll_display(Scroll::Delta(-1));
        cw.snapshot_visible(&TerminalDamage::Full, &mut rows, &mut styles, &mut extras);
        assert_eq!(rows[0], expected);
        assert_eq!(styles[0], expected_style);
        assert_eq!(rows[0].inner.as_ptr(), allocation);
        assert!(rows.iter().all(|row| row.dirty));
    }

    #[test]
    fn moving_snapshot_caches_match_fresh_captures() {
        use std::collections::HashSet;
        use std::fmt::Write;
        for deferred in [false, true] {
            let mut cw = new_term(32, 8);
            cw.grid.update_history(32);
            let mut processor = crate::performer::handler::Processor::default();
            let mut input = String::new();
            for n in 0..100 {
                write!(input, "\x1b[38;5;{}m{n} \x1b[0m\x1b]8;id={n};https://example.test/{n}\x07e\u{301}界\x1b]8;;\x07\r\n", n % 16).unwrap();
            }
            processor.advance(&mut cw, input.as_bytes());
            let (mut rows, mut styles, mut extras): (
                Vec<Row<Square>>,
                Vec<snapshot::RowStyles>,
                snapshot::SnapshotExtras,
            ) = Default::default();
            let mut scratch = snapshot::DeferredStyles::default();
            for step in 0..160 {
                let mut damage = TerminalDamage::Full;
                match step % 16 {
                    0 => cw.scroll_display(Scroll::Top),
                    1 | 2 => cw.scroll_display(Scroll::Delta(-1)),
                    3 => cw.scroll_display(Scroll::Delta(1)),
                    4 => cw.scroll_display(Scroll::Delta(-4)),
                    5 => cw.scroll_display(Scroll::Bottom),
                    6 => processor.advance(&mut cw, input.as_bytes()),
                    7 => {
                        processor.advance(&mut cw, b"\x1b[1;1H\x1b[44m\x1b[2K");
                        damage = TerminalDamage::Partial;
                    }
                    8 => processor.advance(&mut cw, b"\x1b[?1049h\x1b[34malt"),
                    9 => processor.advance(&mut cw, b"\x1b[?1049l"),
                    10 => cw.resize(CrosswordsSize::new(19, 5)),
                    11 => cw.resize(CrosswordsSize::new(32, 8)),
                    12 => {
                        // Overlays edit resolved values, not the source IDs.
                        for row in &mut styles {
                            row.make_mut(cw.columns())[0].flags |=
                                style::StyleFlags::INVERSE;
                        }
                    }
                    13 => {
                        cw = new_term(32, 8);
                        cw.grid.update_history(32);
                        processor = crate::performer::handler::Processor::default();
                        processor.advance(&mut cw, input.as_bytes());
                    }
                    14 => damage = TerminalDamage::Noop,
                    _ => processor.advance(&mut cw, b"\x1b[3J\x1b[0m\x1b[2J"),
                }
                let (start, end) = cw.visible_line_bounds();
                let all_modified = (start..end).all(|y| cw.grid[Line(y)].dirty);
                let actual = if deferred {
                    cw.snapshot_visible_deferred(
                        &damage,
                        &mut rows,
                        &mut styles,
                        &mut extras,
                        &mut scratch,
                    )
                    .resolve()
                } else {
                    cw.snapshot_visible(&damage, &mut rows, &mut styles, &mut extras)
                };
                assert_eq!(actual, all_modified, "source damage at {step}");
                let (mut fresh_rows, mut fresh_styles, mut fresh_extras) =
                    Default::default();
                cw.snapshot_visible(
                    &TerminalDamage::Full,
                    &mut fresh_rows,
                    &mut fresh_styles,
                    &mut fresh_extras,
                );
                assert_eq!(rows, fresh_rows, "cells at {step}, deferred={deferred}");
                assert_eq!(
                    styles, fresh_styles,
                    "styles at {step}, deferred={deferred}"
                );
                let mut ids = HashSet::new();
                for row in &rows {
                    if matches!(damage, TerminalDamage::Full) {
                        assert!(row.dirty, "redraw at {step}");
                    }
                    for cell in &row.inner {
                        if let Some(id) = cell.extras_id_checked() {
                            ids.insert(id);
                            assert_eq!(
                                extras.get(&id),
                                fresh_extras.get(&id),
                                "extras {id} at {step}"
                            );
                        }
                    }
                }
                assert_eq!(extras.len(), ids.len(), "offscreen extras at {step}");
                for row in &mut rows {
                    row.dirty = false;
                }
                cw.reset_damage();
            }
        }
    }

    #[test]
    fn full_snapshots_refresh_reused_ids_and_preserve_frozen_values() {
        let mut cw = new_term(120, 80);
        let mut processor = crate::performer::handler::Processor::default();
        processor.advance(&mut cw, "\x1b[31me\u{301}\x1b[0m default".as_bytes());
        let (mut rows, mut styles, mut extras) = Default::default();
        cw.snapshot_visible(&TerminalDamage::Full, &mut rows, &mut styles, &mut extras);
        let frozen_styles = styles.clone();
        let frozen_extras = extras.clone();
        let id = rows[0][Column(0)].extras_id().unwrap();
        let raw = rows[0][Column(0)];

        // Both intern tables restart, with the same raw cell bits but new
        // meanings. Coordinate alignment must never bypass value validation.
        cw = new_term(120, 80);
        processor.advance(&mut cw, "\x1b[34me\u{308}\x1b[0m default".as_bytes());
        assert_eq!(cw.grid[Line(0)][Column(0)], raw);
        let mut scratch = snapshot::DeferredStyles::default();
        let captured = cw.snapshot_visible_deferred(
            &TerminalDamage::Full,
            &mut rows,
            &mut styles,
            &mut extras,
            &mut scratch,
        );
        // Palette ownership must still hold if the terminal changes again
        // between capture and deferred style resolution.
        let expected_style = cw.grid.style_of(&raw);
        cw = new_term(120, 80);
        processor.advance(&mut cw, b"\x1b[32mgreen");
        captured.resolve();
        assert_eq!(styles[0].at(0), expected_style);
        assert_ne!(styles[0], frozen_styles[0]);
        assert_eq!(extras.get(&id).unwrap().zerowidth, ['\u{308}']);
        assert_eq!(frozen_extras.get(&id).unwrap().zerowidth, ['\u{301}']);
    }

    #[test]
    fn snapshot_releases_extras_after_the_last_visible_reference() {
        let mut cw = make_crosswords();
        let mut processor = crate::performer::handler::Processor::default();
        processor.advance(&mut cw, "e\u{301}\x1b[2;1He\u{301}".as_bytes());
        let (mut rows, mut styles, mut extras) = Default::default();
        cw.snapshot_visible(&TerminalDamage::Full, &mut rows, &mut styles, &mut extras);
        assert_eq!(extras.len(), 1);

        processor.advance(&mut cw, b"\x1b[1;1Hx");
        cw.snapshot_visible(
            &TerminalDamage::Partial,
            &mut rows,
            &mut styles,
            &mut extras,
        );
        assert_eq!(extras.len(), 1);
        let id = rows[1][Column(0)].extras_id().unwrap();
        assert_eq!(extras.get(&id).unwrap().zerowidth, ['\u{301}']);

        processor.advance(&mut cw, b"\x1b[2;1Hy");
        cw.snapshot_visible(
            &TerminalDamage::Partial,
            &mut rows,
            &mut styles,
            &mut extras,
        );
        assert!(extras.is_empty());
    }

    #[test]
    fn snapshot_refreshes_recycled_extras_without_mutating_older_snapshots() {
        let mut cw = make_crosswords();
        let mut processor = crate::performer::handler::Processor::default();
        processor.advance(&mut cw, "e\u{301}".as_bytes());
        let (mut rows, mut styles, mut extras) = Default::default();
        cw.snapshot_visible(&TerminalDamage::Full, &mut rows, &mut styles, &mut extras);
        let id = rows[0][Column(0)].extras_id().unwrap();
        let older = extras.clone();

        processor.advance(&mut cw, b"\x1b[1;1Hx");
        cw.grid
            .extras_table
            .sweep_unmarked(&vec![0; grid::ID_BITSET_WORDS]);
        processor.advance(&mut cw, "\x1b[1;1He\u{308}".as_bytes());
        assert_eq!(cw.grid[Line(0)][Column(0)].extras_id(), Some(id));
        cw.snapshot_visible(
            &TerminalDamage::Partial,
            &mut rows,
            &mut styles,
            &mut extras,
        );
        assert_eq!(extras.get(&id).unwrap().zerowidth, ['\u{308}']);
        assert_eq!(older.get(&id).unwrap().zerowidth, ['\u{301}']);
        assert_eq!(extras.len(), 1);
    }

    #[test]
    fn deferred_snapshots_match_eager_updates() {
        use crate::performer::handler::Processor;
        let mut eager = new_term(120, 80);
        let mut deferred = new_term(120, 80);
        let mut eager_parser = Processor::default();
        let mut deferred_parser = Processor::default();
        let (mut rows, mut styles, mut extras) = Default::default();
        let (mut reference_rows, mut reference_styles, mut reference_extras) =
            Default::default();
        let mut scratch = snapshot::DeferredStyles::default();
        let mut initial = String::new();
        for n in 0..1000 {
            use std::fmt::Write;
            write!(initial, "\x1b[38;5;{}mline {n} e\u{301}界\r\n", n % 64).unwrap();
        }
        let inputs: &[&[u8]] = &[
            initial.as_bytes(),
            b"",                         // Clean frame.
            b"\x1b[1;1H\x1b[31mchanged", // Partial, palette larger than one row.
            b"\x1b[42m\x1b[2K",          // Inline erased background.
            b"",                         // Scroll history in the next three steps.
            b"",
            b"",
            b"\x1b[?1049h\x1b[34malternate",
            b"\x1b[?1049l",
            b"\x1b]8;;https://example.test\x07linked\x1b]8;;\x07",
            b"", // Resize with reflow, then grow again.
            b"",
        ];
        for (step, input) in inputs.iter().enumerate() {
            eager_parser.advance(&mut eager, input);
            deferred_parser.advance(&mut deferred, input);
            for terminal in [&mut eager, &mut deferred] {
                match step {
                    4 => terminal.scroll_display(Scroll::Top),
                    5 => terminal.scroll_display(Scroll::Delta(-1)),
                    6 => terminal.scroll_display(Scroll::Bottom),
                    10 => terminal.resize(CrosswordsSize::new(61, 25)),
                    11 => terminal.resize(CrosswordsSize::new(120, 80)),
                    _ => (),
                }
            }
            let damage = eager.peek_damage_event().unwrap_or(TerminalDamage::Noop);
            let expected = eager.snapshot_visible(
                &damage,
                &mut reference_rows,
                &mut reference_styles,
                &mut reference_extras,
            );
            let actual = deferred
                .snapshot_visible_deferred(
                    &damage,
                    &mut rows,
                    &mut styles,
                    &mut extras,
                    &mut scratch,
                )
                .resolve();
            assert_eq!(actual, expected, "source damage at step {step}");
            assert_eq!(rows, reference_rows, "cells at step {step}");
            assert_eq!(styles, reference_styles, "styles at step {step}");
            eager.reset_damage();
            deferred.reset_damage();
        }
    }

    #[test]
    fn deferred_snapshot_owns_styles_across_grid_replacement() {
        use crate::performer::handler::Processor;
        let mut terminal = new_term(120, 80);
        let mut processor = Processor::default();
        processor.advance(&mut terminal, b"\x1b[31mred\x1b[0m default");
        let old_id = terminal.grid[Line(0)][Column(0)].style_id();
        let expected = terminal.grid.styles()[old_id as usize];
        let (mut rows, mut styles, mut extras) = Default::default();
        let mut deferred = snapshot::DeferredStyles::default();
        let captured = terminal.snapshot_visible_deferred(
            &TerminalDamage::Full,
            &mut rows,
            &mut styles,
            &mut extras,
            &mut deferred,
        );

        // Mimic a grid swap or style-ID reclamation while the renderer works
        // unlocked. The same ID now means blue in the live terminal.
        terminal = new_term(120, 80);
        processor.advance(&mut terminal, b"\x1b[34mblue\x1b[0m default");
        assert_eq!(terminal.grid[Line(0)][Column(0)].style_id(), old_id);
        assert_ne!(terminal.grid.styles()[old_id as usize], expected);
        captured.resolve();
        assert_eq!(styles[0].at(0), expected);
        assert_eq!(styles[0].at(4), style::Style::DEFAULT);

        drop(terminal.snapshot_visible_deferred(
            &TerminalDamage::Full,
            &mut rows,
            &mut styles,
            &mut extras,
            &mut deferred,
        ));
        let previous = styles.clone();
        terminal.reset_damage();
        processor.advance(&mut terminal, b"\x1b[2;1H\x1b[32mgreen");
        terminal
            .snapshot_visible_deferred(
                &TerminalDamage::Partial,
                &mut rows,
                &mut styles,
                &mut extras,
                &mut deferred,
            )
            .resolve();
        assert_eq!(styles[0], previous[0]);
        assert_ne!(styles[1], previous[1]);
        let mut reference_rows = Vec::new();
        let mut reference_styles = Vec::new();
        let mut reference_extras = snapshot::SnapshotExtras::default();
        terminal.snapshot_visible(
            &TerminalDamage::Full,
            &mut reference_rows,
            &mut reference_styles,
            &mut reference_extras,
        );
        assert_eq!(rows, reference_rows);
        assert_eq!(styles, reference_styles);
    }

    #[test]
    #[ignore = "manual snapshot lock-time measurement; run in release mode"]
    fn snapshot_style_capture_timings() {
        use crate::config::colors::{AnsiColor, ColorRgb};
        use std::hint::black_box;
        use std::time::{Duration, Instant};
        for colors in [0, 2, 64, 4096, 9600] {
            let mut cw = new_term(120, 80);
            for y in 0..80 {
                cw.goto(Line(y), Column(0));
                for x in 0..120 {
                    if colors == 0 {
                        cw.input('x');
                        continue;
                    }
                    let n = (y as usize * 120 + x) % colors;
                    cw.terminal_attribute(Attr::Foreground(AnsiColor::Spec(ColorRgb {
                        r: n as u8,
                        g: (n >> 8) as u8,
                        b: 100,
                    })));
                    cw.input('x');
                }
            }
            let mut rows = Vec::new();
            let mut styles = Vec::new();
            let mut extras = snapshot::SnapshotExtras::default();
            let mut deferred = snapshot::DeferredStyles::default();
            for round in 0..4 {
                let mut eager = Duration::ZERO;
                let mut capture = Duration::ZERO;
                let mut finish = Duration::ZERO;
                for _ in 0..2000 {
                    let start = Instant::now();
                    cw.snapshot_visible(
                        &TerminalDamage::Full,
                        &mut rows,
                        &mut styles,
                        &mut extras,
                    );
                    eager += start.elapsed();
                    black_box((&rows, &styles));
                    let start = Instant::now();
                    let captured = cw.snapshot_visible_deferred(
                        &TerminalDamage::Full,
                        &mut rows,
                        &mut styles,
                        &mut extras,
                        &mut deferred,
                    );
                    capture += start.elapsed();
                    let start = Instant::now();
                    captured.resolve();
                    finish += start.elapsed();
                    black_box((&rows, &styles));
                }
                eprintln!(
                    "colors={colors} round={round} eager={:?} capture={:?} finish={:?}",
                    eager / 2000,
                    capture / 2000,
                    finish / 2000
                );
            }
        }
    }

    #[test]
    fn snapshot_resolves_styles_per_row() {
        use crate::config::colors::{AnsiColor, ColorRgb};
        use crate::crosswords::style::Style;
        use crate::event::TerminalDamage;
        let mut cw = make_crosswords();
        let mut rows = Vec::new();
        let mut row_styles = Vec::new();
        let mut extras = snapshot::SnapshotExtras::default();

        let red = AnsiColor::Spec(ColorRgb { r: 250, g: 0, b: 0 });
        cw.terminal_attribute(Attr::Background(red));
        cw.input('x');
        let damage = cw.peek_damage_event().unwrap();
        cw.snapshot_visible(&damage, &mut rows, &mut row_styles, &mut extras);
        cw.reset_damage();
        assert_eq!(row_styles.len(), rows.len());
        assert_eq!(row_styles[0].at(0).bg, red);
        assert_eq!(row_styles[0].at(1).bg, Style::default().bg);

        // Rows copied on earlier frames hold resolved values, so table
        // mutations between frames cannot retint them: only the row the
        // new style landed on changes.
        let blue = AnsiColor::Spec(ColorRgb { r: 0, g: 0, b: 250 });
        cw.goto(Line(1), Column(0));
        cw.terminal_attribute(Attr::Background(blue));
        cw.input('z');
        let damage = cw.peek_damage_event().unwrap();
        cw.snapshot_visible(&damage, &mut rows, &mut row_styles, &mut extras);
        cw.reset_damage();
        assert_eq!(row_styles[0].at(0).bg, red);
        assert_eq!(row_styles[1].at(0).bg, blue);

        // An instance switch arrives as Full damage and re-resolves
        // everything against the newly active grid's table.
        cw.swap_alt();
        let damage = cw.peek_damage_event().unwrap();
        assert!(matches!(damage, TerminalDamage::Full));
        cw.snapshot_visible(&damage, &mut rows, &mut row_styles, &mut extras);
        cw.reset_damage();
        // The destination was cleared before the blue cursor was copied.
        assert_eq!(rows[0][Column(0)], Square::default());
        assert_eq!(row_styles[0].at(0), Style::default());
    }

    #[test]
    fn snapshot_separates_source_changes_from_full_redraw_flags() {
        use crate::event::TerminalDamage;
        let mut cw = make_crosswords();
        let (mut rows, mut styles, mut extras) =
            (Vec::new(), Vec::new(), snapshot::SnapshotExtras::default());
        assert!(cw.snapshot_visible(
            &TerminalDamage::Full,
            &mut rows,
            &mut styles,
            &mut extras
        ));
        for row in &mut rows {
            row.dirty = false;
        }
        assert!(!cw.snapshot_visible(
            &TerminalDamage::Full,
            &mut rows,
            &mut styles,
            &mut extras
        ));
        assert!(
            rows.iter().all(|row| row.dirty),
            "full redraw remains visible to library consumers"
        );
        cw.grid[Line(0)][Column(0)].set_c('x');
        assert!(!cw.snapshot_visible(
            &TerminalDamage::Partial,
            &mut rows,
            &mut styles,
            &mut extras
        ));
        for y in 0..cw.screen_lines() {
            cw.grid[Line(y as i32)][Column(0)].set_c('y');
        }
        assert!(cw.snapshot_visible(
            &TerminalDamage::Full,
            &mut rows,
            &mut styles,
            &mut extras
        ));
    }

    #[test]
    fn style_sweep_bounds_table_under_unique_style_churn() {
        use crate::config::colors::{AnsiColor, ColorRgb};
        let mut cw = make_crosswords();

        // Overwrite one cell with tens of thousands of distinct styles;
        // only a handful stay referenced, so the sweep must keep the
        // table well below the number of styles ever interned.
        let total = 40_000u32;
        for i in 0..total {
            cw.goto(Line(0), Column(0));
            cw.terminal_attribute(Attr::Background(AnsiColor::Spec(ColorRgb {
                r: (i & 0xFF) as u8,
                g: ((i >> 8) & 0xFF) as u8,
                b: ((i >> 16) & 0xFF) as u8,
            })));
            cw.input('x');
        }
        assert!(
            cw.grid.styles().len() < 35_000,
            "sweep never reclaimed ids: table has {} entries after {} interns",
            cw.grid.styles().len(),
            total,
        );

        // The last style written is still live and resolves correctly.
        let last = total - 1;
        let sq = cw.grid[Line(0)][Column(0)];
        assert_eq!(
            cw.grid.style_of(&sq).bg,
            AnsiColor::Spec(ColorRgb {
                r: (last & 0xFF) as u8,
                g: ((last >> 8) & 0xFF) as u8,
                b: ((last >> 16) & 0xFF) as u8,
            }),
        );
    }

    #[test]
    fn styled_write_sets_row_hint() {
        use crate::config::colors::{AnsiColor, ColorRgb};
        let mut cw = make_crosswords();
        cw.input('a');
        assert!(!cw.grid[Line(0)].has_styles);

        cw.terminal_attribute(Attr::Background(AnsiColor::Spec(ColorRgb {
            r: 1,
            g: 2,
            b: 3,
        })));
        cw.input('b');
        assert!(cw.grid[Line(0)].has_styles);
        assert!(!cw.grid[Line(1)].has_styles);
    }

    #[test]
    fn wide_wrap_carries_row_hint_to_next_row() {
        use crate::config::colors::{AnsiColor, ColorRgb};
        let mut cw = make_crosswords();
        cw.terminal_attribute(Attr::Background(AnsiColor::Spec(ColorRgb {
            r: 9,
            g: 9,
            b: 9,
        })));
        // Styled wide char at the last column wraps the base onto the
        // next row; the hint must travel with it.
        cw.goto(Line(0), Column(3));
        cw.input('你');
        assert!(cw.grid[Line(1)].has_styles);
    }

    #[test]
    fn styled_blank_fills_flag_rows_and_bg_only_fills_do_not() {
        use crate::config::colors::{AnsiColor, NamedColor};
        let mut cw = make_crosswords();

        // Truecolor / palette backgrounds erase via bg-only cells, which
        // carry no style id: the hint must stay clear.
        cw.terminal_attribute(Attr::Background(AnsiColor::Indexed(42)));
        cw.erase_chars(Column(2));
        assert!(!cw.grid[Line(0)].has_styles);

        // Special named backgrounds fall back to an interned style, so
        // the fill must flag the row.
        cw.terminal_attribute(Attr::Background(AnsiColor::Named(NamedColor::DimRed)));
        cw.goto(Line(1), Column(0));
        cw.erase_chars(Column(2));
        assert!(cw.grid[Line(1)].has_styles);
    }

    #[test]
    fn scrolled_styled_rows_keep_hint_in_history() {
        use crate::config::colors::{AnsiColor, ColorRgb};
        let mut cw = make_crosswords();
        let teal = AnsiColor::Spec(ColorRgb { r: 0, g: 90, b: 90 });
        cw.terminal_attribute(Attr::Background(teal));
        cw.input('x');
        cw.terminal_attribute(Attr::Reset);

        // Scroll the styled row into history; the flag must travel with
        // it so the sweep keeps its id live.
        for _ in 0..8 {
            cw.goto(Line(3), Column(0));
            cw.linefeed();
        }
        cw.grid.reclaim_styles();
        assert!(cw.grid.styles().iter().any(|s| s.bg == teal));
    }

    #[test]
    fn unstyled_session_never_flags_rows() {
        let mut cw = make_crosswords();
        for _ in 0..3 {
            cw.input('a');
            cw.linefeed();
        }
        let styled_rows = cw.grid.raw.rows().filter(|r| r.has_styles).count();
        assert_eq!(styled_rows, 0);
        // And the sweep over such a grid frees nothing and trips no
        // skip-contract assertion.
        let len = cw.grid.styles().len();
        cw.grid.reclaim_styles();
        assert_eq!(cw.grid.styles().len(), len);
    }

    #[test]
    fn sweep_keeps_styles_through_column_reflow() {
        use crate::config::colors::{AnsiColor, ColorRgb};
        let mut cw = make_crosswords();
        let purple = AnsiColor::Spec(ColorRgb {
            r: 120,
            g: 0,
            b: 200,
        });
        cw.terminal_attribute(Attr::Background(purple));
        for _ in 0..4 {
            cw.input('x');
        }
        cw.terminal_attribute(Attr::Reset);

        // Reflow to narrower and back: styled cells splice across rows,
        // and the conservative row hints must keep their ids live
        // through both sweeps.
        cw.resize(CrosswordsSize::new(2, 4));
        cw.grid.reclaim_styles();
        assert!(cw.grid.styles().iter().any(|s| s.bg == purple));

        cw.resize(CrosswordsSize::new(4, 4));
        cw.grid.reclaim_styles();
        let found = cw.grid.raw.rows().any(|row| {
            row.inner
                .iter()
                .any(|sq| !sq.is_bg_only() && cw.grid.style_of(sq).bg == purple)
        });
        assert!(found, "reflowed styled cells lost their style after sweeps");
    }

    #[test]
    fn sweep_keeps_styles_of_rows_cached_by_a_shrink() {
        use crate::config::colors::{AnsiColor, ColorRgb};
        let mut cw = make_crosswords();

        // Style the bottom row, then move the cursor to the top and
        // reset the SGR state so no cursor template keeps the style
        // alive on its own.
        let green = AnsiColor::Spec(ColorRgb {
            r: 0,
            g: 200,
            b: 50,
        });
        cw.goto(Line(3), Column(0));
        cw.terminal_attribute(Attr::Background(green));
        cw.input('x');
        cw.terminal_attribute(Attr::Reset);
        cw.goto(Line(0), Column(0));
        cw.input('y');

        // Shrinking the window drops the bottom rows into Storage's
        // hidden cache (content intact, re-exposed verbatim by a later
        // grow), so the sweep must keep their style ids live.
        cw.resize(CrosswordsSize::new(4, 2));
        cw.grid.reclaim_styles();
        assert!(
            cw.grid.styles().iter().any(|s| s.bg == green),
            "sweep freed a style still referenced by a cached row",
        );

        // Growing back keeps the row (visible or in history) with its
        // style still resolving against the swept table.
        cw.resize(CrosswordsSize::new(4, 4));
        let found = cw.grid.raw.rows().any(|row| {
            row.inner
                .iter()
                .any(|sq| !sq.is_bg_only() && cw.grid.style_of(sq).bg == green)
        });
        assert!(found, "cached row lost its style across shrink/grow");
    }

    #[test]
    fn terminal_attributes_preserve_blink_kind() {
        use crate::crosswords::style::StyleFlags;

        let mut cw = make_crosswords();
        cw.terminal_attribute(Attr::BlinkSlow);
        let style = cw.grid.template_style();
        assert!(style.flags.contains(StyleFlags::SLOW_BLINK));
        assert!(!style.flags.contains(StyleFlags::RAPID_BLINK));

        cw.terminal_attribute(Attr::BlinkFast);
        let style = cw.grid.template_style();
        assert!(!style.flags.contains(StyleFlags::SLOW_BLINK));
        assert!(style.flags.contains(StyleFlags::RAPID_BLINK));

        cw.terminal_attribute(Attr::CancelBlink);
        let style = cw.grid.template_style();
        assert!(!style.flags.intersects(StyleFlags::ALL_BLINK));

        // And the flags survive interning when a glyph is written.
        cw.terminal_attribute(Attr::BlinkSlow);
        cw.input('x');
        let sq = cw.grid[Line(0)][Column(0)];
        assert!(cw.grid.style_of(&sq).flags.contains(StyleFlags::SLOW_BLINK));
    }

    // Minimum-valid simple glyph: one contour, one on-curve point.
    #[cfg(feature = "graphics")]
    fn minimal_glyf_bytes() -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&1i16.to_be_bytes()); // numberOfContours
        v.extend_from_slice(&[0u8; 8]); // bounding box
        v.extend_from_slice(&0u16.to_be_bytes()); // endPtsOfContours[0]
        v.extend_from_slice(&0u16.to_be_bytes()); // instructionLength
        v.push(0x01); // flags: on-curve, no shorts
        v.extend_from_slice(&0i16.to_be_bytes()); // x delta
        v.extend_from_slice(&0i16.to_be_bytes()); // y delta
        v
    }

    #[cfg(feature = "graphics")]
    #[cfg(feature = "graphics")]
    fn glyf_payload(
        bytes: Vec<u8>,
        upm: u16,
    ) -> crate::ansi::glyph_protocol::GlyphPayload {
        crate::ansi::glyph_protocol::GlyphPayload::Glyf { glyf: bytes, upm }
    }

    #[cfg(feature = "graphics")]
    #[cfg(feature = "graphics")]
    fn registry_contains(cw: &Crosswords<VoidListener>, cp: u32) -> bool {
        cw.glyph_registry.as_ref().is_some_and(|r| r.contains(cp))
    }

    #[cfg(feature = "graphics")]
    #[cfg(feature = "graphics")]
    fn registry_len(cw: &Crosswords<VoidListener>) -> usize {
        cw.glyph_registry.as_ref().map_or(0, |r| r.len())
    }

    #[test]
    fn semantic_prompt_marks_and_navigation() {
        use crate::crosswords::grid::row::SemanticPrompt;

        let mut cw = make_crosswords();
        // Three prompts, each followed by two lines of output. The
        // 4-row screen pushes earlier prompts into scrollback.
        for _ in 0..3 {
            cw.set_semantic_prompt(SemanticPrompt::Prompt);
            cw.linefeed();
            cw.linefeed();
            cw.linefeed();
        }
        assert_eq!(cw.grid.history_size(), 6);
        assert_eq!(cw.grid[Line(-6)].semantic_prompt, SemanticPrompt::Prompt);
        assert_eq!(cw.grid[Line(-3)].semantic_prompt, SemanticPrompt::Prompt);
        assert_eq!(cw.grid[Line(0)].semantic_prompt, SemanticPrompt::Prompt);

        cw.scroll_to_prompt(false);
        assert_eq!(cw.display_offset(), 3);
        cw.scroll_to_prompt(false);
        assert_eq!(cw.display_offset(), 6);
        // No prompt further up: stays put.
        cw.scroll_to_prompt(false);
        assert_eq!(cw.display_offset(), 6);

        cw.scroll_to_prompt(true);
        assert_eq!(cw.display_offset(), 3);
        cw.scroll_to_prompt(true);
        assert_eq!(cw.display_offset(), 0);
        cw.scroll_to_prompt(true);
        assert_eq!(cw.display_offset(), 0);
    }

    #[test]
    fn semantic_prompt_run_counts_as_one() {
        use crate::crosswords::grid::row::SemanticPrompt;

        let mut cw = make_crosswords();
        for _ in 0..6 {
            cw.linefeed();
        }
        // A two-row prompt: continuation directly below the start.
        cw.grid[Line(-3)].semantic_prompt = SemanticPrompt::Prompt;
        cw.grid[Line(-2)].semantic_prompt = SemanticPrompt::PromptContinuation;

        cw.scroll_to_prompt(false);
        assert_eq!(cw.display_offset(), 3);
        cw.scroll_to_prompt(false);
        assert_eq!(cw.display_offset(), 3);
    }

    #[test]
    fn shell_integration_oscs_dispatch_from_raw_bytes() {
        use crate::crosswords::grid::row::SemanticPrompt;
        use crate::performer::handler::Processor;

        let size = CrosswordsSize::new(40, 5);
        let window_id = crate::event::WindowId::from(0);
        let mut cw = Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener {},
            window_id,
            0,
            10_000,
        );
        let mut processor = Processor::default();

        // A prompt mark, a full A/B/C/D cycle with options, and a
        // user var ("hello" in base64), bell-terminated like shells
        // emit them.
        let bytes = b"]133;A;aid=1prompt]133;Bcmd
]133;Cout
]133;D;0]1337;SetUserVar=foo=aGVsbG8=";
        processor.advance(&mut cw, bytes);

        assert_eq!(cw.grid[Line(0)].semantic_prompt, SemanticPrompt::Prompt);
        assert_eq!(cw.grid[Line(1)].semantic_prompt, SemanticPrompt::None);
        assert_eq!(cw.user_vars.get("foo").map(String::as_str), Some("hello"));
    }

    #[test]
    fn grid_iterator_survives_narrow_rows() {
        let mut cw = make_crosswords();
        for _ in 0..6 {
            cw.linefeed();
        }
        // History rows keep their old length across column growth;
        // simulate one narrower than the grid (#1713).
        cw.grid[Line(-2)].inner.truncate(2);
        let start = Pos::new(Line(-3), Column(0));
        // Must not panic; may end early at the narrow row.
        let _ = cw.grid.iter_from(start).count();

        // A stale position beyond the grid's history must end the
        // iteration instead of panicking.
        let stale = Pos::new(Line(-40), Column(0));
        assert_eq!(cw.grid.iter_from(stale).count(), 0);
    }

    #[test]
    fn user_vars_are_stored() {
        let mut cw = make_crosswords();
        assert!(cw.user_vars.is_empty());
        cw.set_user_var("k".to_string(), "v".to_string());
        cw.set_user_var("k".to_string(), "v2".to_string());
        assert_eq!(cw.user_vars.get("k").map(String::as_str), Some("v2"));
    }

    #[cfg(feature = "graphics")]
    #[cfg(feature = "graphics")]
    #[test]
    fn glyph_registry_is_none_until_first_register() {
        let cw = make_crosswords();
        assert!(cw.glyph_registry.is_none());
    }

    #[cfg(feature = "graphics")]
    #[cfg(feature = "graphics")]
    #[test]
    fn glyph_protocol_register_populates_registry() {
        let mut cw = make_crosswords();

        let glyf = minimal_glyf_bytes();
        // E0A0 is the Powerline branch codepoint — in basic PUA.
        let res = Handler::glyph_register(&mut cw, 0xE0A0, glyf_payload(glyf, 1000), 1);
        assert!(res.is_ok());
        assert!(cw.glyph_registry.is_some());
        assert!(registry_contains(&cw, 0xE0A0));
    }

    #[cfg(feature = "graphics")]
    #[cfg(feature = "graphics")]
    #[test]
    fn glyph_protocol_register_rejects_non_pua() {
        use crate::ansi::glyph_protocol::RegisterError;
        let mut cw = make_crosswords();

        // 0x61 is 'a' — not in PUA. Registry must refuse.
        let res = Handler::glyph_register(
            &mut cw,
            0x61,
            glyf_payload(minimal_glyf_bytes(), 1000),
            1,
        );
        assert_eq!(res, Err(RegisterError::OutOfNamespace));
        assert!(cw.glyph_registry.is_none());
    }

    #[cfg(feature = "graphics")]
    #[cfg(feature = "graphics")]
    #[test]
    fn glyph_protocol_register_rejects_hinted_payload() {
        use crate::ansi::glyph_protocol::RegisterError;
        let mut cw = make_crosswords();

        let mut v = Vec::new();
        v.extend_from_slice(&1i16.to_be_bytes());
        v.extend_from_slice(&[0u8; 8]);
        v.extend_from_slice(&0u16.to_be_bytes()); // endPts[0]
        v.extend_from_slice(&1u16.to_be_bytes()); // instructionLength = 1
        v.push(0x00); // the instruction
        v.push(0x01); // on-curve flag
        v.extend_from_slice(&0i16.to_be_bytes());
        v.extend_from_slice(&0i16.to_be_bytes());

        let res = Handler::glyph_register(&mut cw, 0xE0A0, glyf_payload(v, 1000), 1);
        assert_eq!(res, Err(RegisterError::HintingUnsupported));
        // Decode failed before the registry was touched, so it stays
        // uninitialised.
        assert!(cw.glyph_registry.is_none());
    }

    // A simple glyph declaring `n` points in almost no wire bytes:
    // the §8.2 decode-amplification payload.
    #[cfg(feature = "graphics")]
    fn dense_glyf_bytes(n: usize) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&1i16.to_be_bytes()); // numberOfContours
        v.extend_from_slice(&[0u8; 8]); // bounding box
        v.extend_from_slice(&((n - 1) as u16).to_be_bytes()); // endPts
        v.extend_from_slice(&0u16.to_be_bytes()); // instructionLength
        let flag = 0x01 | 0x08 | 0x10 | 0x20; // on-curve|repeat|x-same|y-same
        let mut left = n;
        while left > 0 {
            let run = left.min(255);
            v.push(flag);
            v.push((run - 1) as u8);
            left -= run;
        }
        v
    }

    #[cfg(feature = "graphics")]
    #[test]
    fn glyph_protocol_register_rejects_oversized_outline() {
        use crate::ansi::glyph_protocol::RegisterError;
        use rio_graphics::glyph::glyf_decode::MAX_DECODED_POINTS;
        let mut cw = make_crosswords();

        let v = dense_glyf_bytes(MAX_DECODED_POINTS + 1);
        let res = Handler::glyph_register(&mut cw, 0xE0A0, glyf_payload(v, 1000), 1);
        assert_eq!(res, Err(RegisterError::OutlineTooLarge));
        assert!(cw.glyph_registry.is_none());
    }

    #[cfg(feature = "graphics")]
    #[test]
    fn glyph_protocol_register_rejects_container_over_budget() {
        use crate::ansi::glyph_protocol::{ColrContainer, GlyphPayload, RegisterError};
        use rio_graphics::glyph::glyf_decode::MAX_DECODED_POINTS;
        let mut cw = make_crosswords();

        // Each outline is under the per-outline budget, but together
        // they blow the per-registration budget. The budget check runs
        // before COLR validation, so junk table bytes are fine here.
        let half = MAX_DECODED_POINTS / 2 + 1;
        let container = ColrContainer {
            glyphs: vec![dense_glyf_bytes(half), dense_glyf_bytes(half)],
            colr: vec![0u8; 4],
            cpal: Vec::new(),
        };
        let res = Handler::glyph_register(
            &mut cw,
            0xE0A0,
            GlyphPayload::ColrV0 {
                container,
                upm: 1000,
            },
            1,
        );
        assert_eq!(res, Err(RegisterError::OutlineTooLarge));
        assert!(cw.glyph_registry.is_none());

        // The same total spread across outlines that stay within the
        // budget registers fine.
        let container = ColrContainer {
            glyphs: vec![
                dense_glyf_bytes(MAX_DECODED_POINTS / 2),
                dense_glyf_bytes(MAX_DECODED_POINTS / 2),
            ],
            colr: vec![0u8; 4],
            cpal: Vec::new(),
        };
        let res = Handler::glyph_register(
            &mut cw,
            0xE0A0,
            GlyphPayload::ColrV0 {
                container,
                upm: 1000,
            },
            1,
        );
        assert!(res.is_ok());
        assert!(registry_contains(&cw, 0xE0A0));
    }

    #[cfg(feature = "graphics")]
    #[cfg(feature = "graphics")]
    #[test]
    fn glyph_protocol_clear_before_any_register_is_noop() {
        let mut cw = make_crosswords();
        Handler::glyph_clear(&mut cw, None);
        // No panic, registry still absent.
        assert!(cw.glyph_registry.is_none());
    }

    #[cfg(feature = "graphics")]
    #[cfg(feature = "graphics")]
    #[test]
    fn glyph_protocol_clear_all_wipes_registry() {
        let mut cw = make_crosswords();
        Handler::glyph_register(
            &mut cw,
            0xE0A0,
            glyf_payload(minimal_glyf_bytes(), 1000),
            1,
        )
        .unwrap();
        Handler::glyph_register(
            &mut cw,
            0xE0A1,
            glyf_payload(minimal_glyf_bytes(), 1000),
            1,
        )
        .unwrap();
        assert_eq!(registry_len(&cw), 2);

        Handler::glyph_clear(&mut cw, None);
        // Clear-all empties the registry but leaves the Arc in place,
        // since a program that cleared once will likely register again.
        assert_eq!(registry_len(&cw), 0);
        assert!(cw.glyph_registry.is_some());
    }

    #[cfg(feature = "graphics")]
    #[cfg(feature = "graphics")]
    #[test]
    fn glyph_protocol_width_does_not_change_layout() {
        use crate::crosswords::square::Wide;
        let mut cw = make_crosswords();

        // A width=2 registration must NOT reserve two cells: the
        // declared width is a render-time hint only, so the logical
        // layout stays consistent with system wcwidth (PUA = 1). This
        // keeps the cursor / selection / copy in sync with width-unaware
        // consumers; the glyph overflows into the next cell in pixels at
        // render time.
        Handler::glyph_register(
            &mut cw,
            0xE0A0,
            glyf_payload(minimal_glyf_bytes(), 1000),
            2,
        )
        .unwrap();
        Handler::input(&mut cw, '\u{E0A0}');

        let row = cw.grid.cursor.pos.row;
        assert!(matches!(cw.grid[row][Column(0)].wide(), Wide::Narrow));
        assert_eq!(
            cw.grid.cursor.pos.col,
            Column(1),
            "registered glyph advances the cursor by one cell"
        );
    }

    #[cfg(feature = "graphics")]
    #[test]
    fn glyph_protocol_register_and_clear_mark_full_damage() {
        let mut cw = make_crosswords();
        cw.reset_damage();
        assert!(!cw.is_fully_damaged());

        Handler::glyph_register(
            &mut cw,
            0xE0A0,
            glyf_payload(minimal_glyf_bytes(), 1000),
            1,
        )
        .unwrap();
        assert!(cw.is_fully_damaged(), "register must repaint (§7.3)");

        cw.reset_damage();
        Handler::glyph_clear(&mut cw, Some(0xE0A0));
        assert!(cw.is_fully_damaged(), "clear must repaint (§7.3)");
    }

    #[cfg(feature = "graphics")]
    #[test]
    fn glyph_protocol_clear_one_leaves_others_intact() {
        let mut cw = make_crosswords();
        Handler::glyph_register(
            &mut cw,
            0xE0A0,
            glyf_payload(minimal_glyf_bytes(), 1000),
            1,
        )
        .unwrap();
        Handler::glyph_register(
            &mut cw,
            0xE0A1,
            glyf_payload(minimal_glyf_bytes(), 1000),
            1,
        )
        .unwrap();

        Handler::glyph_clear(&mut cw, Some(0xE0A0));
        assert!(!registry_contains(&cw, 0xE0A0));
        assert!(registry_contains(&cw, 0xE0A1));
    }

    #[test]
    fn scroll_up() {
        let size = CrosswordsSize::new(1, 10);
        let window_id = crate::event::WindowId::from(0);
        let mut cw = Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener {},
            window_id,
            0,
            10_000,
        );
        for i in 0..10 {
            cw.grid[Line(i)][Column(0)].set_c(i as u8 as char);
        }

        cw.grid.scroll_up(&(Line(0)..Line(10)), 2);

        assert_eq!(cw.grid[Line(0)][Column(0)].c(), '\u{2}');
        assert_eq!(cw.grid[Line(0)].occ, 1);
        assert_eq!(cw.grid[Line(1)][Column(0)].c(), '\u{3}');
        assert_eq!(cw.grid[Line(1)].occ, 1);
        assert_eq!(cw.grid[Line(2)][Column(0)].c(), '\u{4}');
        assert_eq!(cw.grid[Line(2)].occ, 1);
        assert_eq!(cw.grid[Line(3)][Column(0)].c(), '\u{5}');
        assert_eq!(cw.grid[Line(3)].occ, 1);
        assert_eq!(cw.grid[Line(4)][Column(0)].c(), '\u{6}');
        assert_eq!(cw.grid[Line(4)].occ, 1);
        assert_eq!(cw.grid[Line(5)][Column(0)].c(), '\u{7}');
        assert_eq!(cw.grid[Line(5)].occ, 1);
        assert_eq!(cw.grid[Line(6)][Column(0)].c(), '\u{8}');
        assert_eq!(cw.grid[Line(6)].occ, 1);
        assert_eq!(cw.grid[Line(7)][Column(0)].c(), '\u{9}');
        assert_eq!(cw.grid[Line(7)].occ, 1);
        assert_eq!(cw.grid[Line(8)][Column(0)].c(), '\0'); // was 0.
        assert_eq!(cw.grid[Line(8)].occ, 0);
        assert_eq!(cw.grid[Line(9)][Column(0)].c(), '\0'); // was 1.
        assert_eq!(cw.grid[Line(9)].occ, 0);
    }

    #[test]
    fn test_linefeed() {
        let size = CrosswordsSize::new(1, 1);
        let window_id = crate::event::WindowId::from(0);

        let mut cw = Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener {},
            window_id,
            0,
            10_000,
        );
        assert_eq!(cw.grid.total_lines(), 1);

        cw.linefeed();
        assert_eq!(cw.grid.total_lines(), 2);
    }

    #[test]
    fn test_linefeed_moving_cursor() {
        let size = CrosswordsSize::new(1, 3);

        let window_id = crate::event::WindowId::from(0);

        let mut cw = Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener {},
            window_id,
            0,
            10_000,
        );
        let cursor = cw.cursor();
        assert_eq!(cursor.pos.col, 0);
        assert_eq!(cursor.pos.row, 0);

        cw.linefeed();
        let cursor = cw.cursor();
        assert_eq!(cursor.pos.col, 0);
        assert_eq!(cursor.pos.row, 1);

        // Keep adding lines but keep cursor at max row
        for _ in 0..20 {
            cw.linefeed();
        }
        let cursor = cw.cursor();
        assert_eq!(cursor.pos.col, 0);
        assert_eq!(cursor.pos.row, 2);
        assert_eq!(cw.grid.total_lines(), 22);
    }

    #[test]
    fn test_input() {
        let size = CrosswordsSize::new(5, 10);
        let window_id = crate::event::WindowId::from(0);

        let mut cw = Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener {},
            window_id,
            0,
            10_000,
        );
        for i in 0..4 {
            cw.grid[Line(0)][Column(i)].set_c(i as u8 as char);
        }
        cw.grid[Line(1)][Column(3)].set_c('b');

        assert_eq!(cw.grid[Line(0)][Column(0)].c(), '\u{0}');
        assert_eq!(cw.grid[Line(0)][Column(1)].c(), '\u{1}');
        assert_eq!(cw.grid[Line(0)][Column(2)].c(), '\u{2}');
        assert_eq!(cw.grid[Line(0)][Column(3)].c(), '\u{3}');
        assert_eq!(cw.grid[Line(0)][Column(4)].c(), '\0');
        assert_eq!(cw.grid[Line(1)][Column(2)].c(), '\0');
        assert_eq!(cw.grid[Line(1)][Column(3)].c(), 'b');
        assert_eq!(cw.grid[Line(0)][Column(4)].c(), '\0');
    }

    /// Drive the parser with an OSC 8 hyperlink and assert that every
    /// cell in the link span carries the same `extras_id`, that the
    /// extras table holds the URI, and that cells outside the span
    /// have no hyperlink. Locks in the per-instance ExtrasTable wiring
    /// added in 2026-04-12 so future regressions in the cell repack
    /// path get caught at test time.
    #[test]
    fn osc8_hyperlink_basic() {
        use crate::performer::handler::Processor;

        let size = CrosswordsSize::new(40, 5);
        let window_id = crate::event::WindowId::from(0);
        let mut cw = Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener {},
            window_id,
            0,
            10_000,
        );
        let mut processor = Processor::default();

        // Plain text, then "click" wrapped in an OSC 8, then plain "."
        // The bell-terminated form is what most shells emit.
        let bytes = b"go \x1b]8;;https://example.com\x07click\x1b]8;;\x07.";
        processor.advance(&mut cw, bytes);

        // "go " — three cells, no hyperlink.
        for col in 0..3 {
            assert!(
                cw.cell_hyperlink(Line(0), Column(col)).is_none(),
                "col {} should have no hyperlink",
                col,
            );
            assert!(cw.cell_hyperlink_id(Line(0), Column(col)).is_none());
        }

        // "click" — five cells, all sharing the same extras_id and URI.
        let id = cw
            .cell_hyperlink_id(Line(0), Column(3))
            .expect("expected hyperlink id at col 3");
        for col in 3..8 {
            assert_eq!(
                cw.cell_hyperlink_id(Line(0), Column(col)),
                Some(id),
                "col {} should share the link's extras_id",
                col,
            );
            let hl = cw.cell_hyperlink(Line(0), Column(col)).expect("hyperlink");
            assert_eq!(hl.uri(), "https://example.com");
        }

        // "." — one cell after the OSC 8 reset, no hyperlink.
        assert!(cw.cell_hyperlink(Line(0), Column(8)).is_none());
        assert!(cw.cell_hyperlink_id(Line(0), Column(8)).is_none());
    }

    /// Two distinct hyperlinks back-to-back must use *different*
    /// `extras_id`s so that consumers walking cell-by-cell can detect
    /// the boundary by id comparison.
    #[test]
    fn osc8_hyperlink_two_distinct_links_use_distinct_ids() {
        use crate::performer::handler::Processor;

        let size = CrosswordsSize::new(40, 5);
        let window_id = crate::event::WindowId::from(0);
        let mut cw = Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener {},
            window_id,
            0,
            10_000,
        );
        let mut processor = Processor::default();

        let bytes = b"\x1b]8;;https://a.example\x07A\x1b]8;;\x07\
                      \x1b]8;;https://b.example\x07B\x1b]8;;\x07";
        processor.advance(&mut cw, bytes);

        let id_a = cw.cell_hyperlink_id(Line(0), Column(0));
        let id_b = cw.cell_hyperlink_id(Line(0), Column(1));
        assert!(id_a.is_some());
        assert!(id_b.is_some());
        assert_ne!(
            id_a, id_b,
            "two distinct hyperlinks must allocate distinct extras_ids",
        );

        let hl_a = cw.cell_hyperlink(Line(0), Column(0)).unwrap();
        let hl_b = cw.cell_hyperlink(Line(0), Column(1)).unwrap();
        assert_eq!(hl_a.uri(), "https://a.example");
        assert_eq!(hl_b.uri(), "https://b.example");
    }

    /// OSC 8 with no params clears the active hyperlink. Cells written
    /// after the reset must not inherit the previous link.
    #[test]
    fn osc8_hyperlink_reset_clears_template() {
        use crate::performer::handler::Processor;

        let size = CrosswordsSize::new(40, 5);
        let window_id = crate::event::WindowId::from(0);
        let mut cw = Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener {},
            window_id,
            0,
            10_000,
        );
        let mut processor = Processor::default();

        // Open a hyperlink, write 'X', close it, then write 'Y'.
        processor.advance(&mut cw, b"\x1b]8;;https://example.com\x07X\x1b]8;;\x07Y");

        // X has the link.
        assert!(cw.cell_hyperlink(Line(0), Column(0)).is_some());
        // Y does not.
        assert!(
            cw.cell_hyperlink(Line(0), Column(1)).is_none(),
            "cells after the OSC 8 reset must not inherit the previous link",
        );
    }

    #[test]
    fn osc8_hyperlink_uri_keeps_semicolons() {
        use crate::performer::handler::Processor;

        let size = CrosswordsSize::new(40, 5);
        let window_id = crate::event::WindowId::from(0);
        let mut cw = Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener {},
            window_id,
            0,
            10_000,
        );
        let mut processor = Processor::default();

        processor.advance(&mut cw, b"\x1b]8;;https://x.dev/a;b;\x07X\x1b]8;;\x07");

        let hl = cw.cell_hyperlink(Line(0), Column(0)).expect("hyperlink");
        assert_eq!(hl.uri(), "https://x.dev/a;b;");
    }

    #[test]
    fn osc8_hyperlink_invalid_utf8_and_id_close() {
        use crate::performer::handler::Processor;

        let size = CrosswordsSize::new(40, 5);
        let window_id = crate::event::WindowId::from(0);
        let mut cw = Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener {},
            window_id,
            0,
            10_000,
        );
        let mut processor = Processor::default();

        processor.advance(
            &mut cw,
            b"\x1b]8;;https://a.example\x07A\
              \x1b]8;;file:///caf\xe9\x07B\
              \x1b]8;id=foo;\x07C",
        );

        let a = cw.cell_hyperlink(Line(0), Column(0)).expect("hyperlink");
        assert_eq!(a.uri(), "https://a.example");
        let b = cw.cell_hyperlink(Line(0), Column(1)).expect("hyperlink");
        assert_eq!(b.uri(), "file:///caf\u{FFFD}");
        assert!(cw.cell_hyperlink(Line(0), Column(2)).is_none());
    }

    /// OSC 8 hyperlinks span multiple lines when the parser writes a
    /// linefeed in the middle of the link. The cells on both lines
    /// should carry the same `extras_id` (modulo the cells before /
    /// after the link on those lines).
    #[test]
    fn osc8_hyperlink_spans_linefeed() {
        use crate::performer::handler::Processor;

        let size = CrosswordsSize::new(40, 5);
        let window_id = crate::event::WindowId::from(0);
        let mut cw = Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener {},
            window_id,
            0,
            10_000,
        );
        let mut processor = Processor::default();

        // "AB\nCD" with the whole thing inside one OSC 8 link.
        processor.advance(
            &mut cw,
            b"\x1b]8;;https://example.com\x07AB\r\nCD\x1b]8;;\x07",
        );

        let id_a = cw.cell_hyperlink_id(Line(0), Column(0)).unwrap();
        let id_b = cw.cell_hyperlink_id(Line(0), Column(1)).unwrap();
        let id_c = cw.cell_hyperlink_id(Line(1), Column(0)).unwrap();
        let id_d = cw.cell_hyperlink_id(Line(1), Column(1)).unwrap();

        assert_eq!(id_a, id_b);
        assert_eq!(id_b, id_c);
        assert_eq!(id_c, id_d);

        let hl = cw.cell_hyperlink(Line(1), Column(1)).unwrap();
        assert_eq!(hl.uri(), "https://example.com");
    }

    #[test]
    fn test_damage_tracking_after_control_c() {
        let size = CrosswordsSize::new(80, 24);
        let window_id = crate::event::WindowId::from(0);
        let mut cw = Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener {},
            window_id,
            0,
            10_000,
        );

        // Simulate fzf-like scenario: write some text
        let test_text = "fzf> search term";
        for ch in test_text.chars() {
            cw.input(ch);
        }

        // Check that input caused damage
        assert!(
            cw.peek_damage_event().is_some(),
            "Input should cause damage"
        );

        // Reset damage to simulate a render cycle completing
        cw.reset_damage();

        // Update last cursor position to match current (simulating that cursor position was rendered)
        cw.damage.last_cursor = cw.grid.cursor.pos;

        // Verify damage was cleared
        assert!(
            cw.peek_damage_event().is_none(),
            "Should have no damage after reset with cursor sync"
        );

        // Simulate Control+C (ETX) - this should clear the line and damage it
        // In fzf, Control+C typically clears the current line and returns to prompt
        cw.carriage_return();
        cw.clear_line(LineClearMode::Right);

        // Check that damage was registered from the clear operation
        let damage = cw.peek_damage_event();
        assert!(
            damage.is_some(),
            "Clear line operation should register damage"
        );

        // Specifically check that it's not just cursor-only damage
        match damage {
            Some(TerminalDamage::Partial) | Some(TerminalDamage::Full) => {
                // Good - line damage was registered
            }
            Some(TerminalDamage::CursorOnly) | Some(TerminalDamage::Noop) => {
                panic!(
                    "Clear line should register line damage, not just cursor movement"
                );
            }
            None => {
                panic!("Clear line should register damage");
            }
        }

        // Verify the line was actually cleared
        let cursor_line = cw.grid.cursor.pos.row;
        for col in 0..test_text.len() {
            assert_eq!(
                cw.grid[cursor_line][Column(col)].c(),
                '\0',
                "Line should be cleared after Control+C"
            );
        }
    }

    #[test]
    fn test_damage_tracking_cursor_movement() {
        let size = CrosswordsSize::new(80, 24);
        let window_id = crate::event::WindowId::from(0);
        let mut cw = Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener {},
            window_id,
            0,
            10_000,
        );

        // Write text on multiple lines
        cw.input('A');
        cw.linefeed();
        cw.input('B');
        cw.linefeed();
        cw.input('C');

        // Reset damage
        cw.reset_damage();

        // Move cursor up - should damage both old and new cursor lines
        let old_line = cw.grid.cursor.pos.row;
        cw.move_up(1);
        let new_line = cw.grid.cursor.pos.row;

        // Check that damage was registered
        let damage = cw.peek_damage_event();
        assert!(damage.is_some(), "Cursor movement should register damage");

        // Verify both lines are marked as damaged
        assert_ne!(old_line, new_line, "Cursor should have moved");
    }

    #[test]
    fn test_damage_tracking_clear_operations() {
        let size = CrosswordsSize::new(80, 24);
        let window_id = crate::event::WindowId::from(0);
        let mut cw = Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener {},
            window_id,
            0,
            10_000,
        );

        // Fill some lines with content
        for line in 0..5 {
            for col in 0..10 {
                cw.grid[Line(line)][Column(col)].set_c('X');
            }
        }

        // Reset damage
        cw.reset_damage();

        // Clear from cursor to end of line
        cw.grid.cursor.pos = Pos::new(Line(2), Column(5));
        cw.clear_line(LineClearMode::Right);

        // Check damage is registered
        let damage = cw.peek_damage_event();
        assert!(damage.is_some(), "Clear line should register damage");

        // Verify the clear operation
        for col in 5..10 {
            assert_eq!(
                cw.grid[Line(2)][Column(col)].c(),
                '\0',
                "Characters from cursor to end should be cleared"
            );
        }

        // Characters before cursor should remain
        for col in 0..5 {
            assert_eq!(
                cw.grid[Line(2)][Column(col)].c(),
                'X',
                "Characters before cursor should remain"
            );
        }
    }

    #[test]
    fn test_damage_tracking_prompt_redraw() {
        let size = CrosswordsSize::new(80, 24);
        let window_id = crate::event::WindowId::from(0);
        let mut cw = Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener {},
            window_id,
            0,
            10_000,
        );

        // Simulate a shell prompt scenario
        let prompt = "$ ";
        for ch in prompt.chars() {
            cw.input(ch);
        }

        // User types a command
        let command = "ls -la";
        for ch in command.chars() {
            cw.input(ch);
        }

        // Reset damage (simulating a render)
        cw.reset_damage();

        // Simulate Control+C: clear line and redraw prompt
        cw.carriage_return();
        cw.clear_line(LineClearMode::Right);

        // Damage should be registered for the cleared line
        assert!(cw.peek_damage_event().is_some(), "Line clear should damage");

        // Write new prompt
        for ch in prompt.chars() {
            cw.input(ch);
        }

        // Verify prompt is displayed correctly
        assert_eq!(cw.grid[cw.grid.cursor.pos.row][Column(0)].c(), '$');
        assert_eq!(cw.grid[cw.grid.cursor.pos.row][Column(1)].c(), ' ');

        // Verify old command is cleared
        for col in 2..8 {
            assert_eq!(
                cw.grid[cw.grid.cursor.pos.row][Column(col)].c(),
                '\0',
                "Old command should be cleared"
            );
        }
    }

    #[test]
    fn simple_selection_works() {
        let size = CrosswordsSize::new(5, 5);
        let window_id = crate::event::WindowId::from(0);

        let mut term = Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener {},
            window_id,
            0,
            10_000,
        );
        let grid = &mut term.grid;
        for i in 0..4 {
            if i == 1 {
                continue;
            }

            grid[Line(i)][Column(0)].set_c('"');

            for j in 1..4 {
                grid[Line(i)][Column(j)].set_c('a');
            }

            grid[Line(i)][Column(4)].set_c('"');
        }
        grid[Line(2)][Column(0)].set_c(' ');
        grid[Line(2)][Column(4)].set_c(' ');
        grid[Line(2)][Column(4)].set_wrapline(true);
        grid[Line(3)][Column(0)].set_c(' ');

        // Multiple lines contain an empty line.
        term.selection = Some(Selection::new(
            SelectionType::Simple,
            Pos {
                row: Line(0),
                col: Column(0),
            },
            Side::Left,
        ));
        if let Some(s) = term.selection.as_mut() {
            s.update(
                Pos {
                    row: Line(2),
                    col: Column(4),
                },
                Side::Right,
            );
        }
        // Trailing space on the wrapped row is preserved as a buffered blank
        // and only flushed if a non-blank cell follows on the continuation
        // row. Here the selection ends mid-wrap so the trailing space is dropped.
        assert_eq!(
            term.selection_to_string(),
            Some(String::from("\"aaa\"\n\n aaa"))
        );

        // A wrapline.
        term.selection = Some(Selection::new(
            SelectionType::Simple,
            Pos {
                row: Line(2),
                col: Column(0),
            },
            Side::Left,
        ));
        if let Some(s) = term.selection.as_mut() {
            s.update(
                Pos {
                    row: Line(3),
                    col: Column(4),
                },
                Side::Right,
            );
        }
        assert_eq!(
            term.selection_to_string(),
            Some(String::from(" aaa  aaa\""))
        );
    }

    #[test]
    fn line_selection_works() {
        let size = CrosswordsSize::new(5, 1);
        let window_id = crate::event::WindowId::from(0);

        let mut term = Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener {},
            window_id,
            0,
            10_000,
        );
        let mut grid: Grid<Square> = Grid::new(1, 5, 0);
        for i in 0..5 {
            grid[Line(0)][Column(i)].set_c('a');
        }
        grid[Line(0)][Column(0)].set_c('"');
        grid[Line(0)][Column(3)].set_c('"');

        mem::swap(&mut term.grid, &mut grid);

        term.selection = Some(Selection::new(
            SelectionType::Lines,
            Pos {
                row: Line(0),
                col: Column(3),
            },
            Side::Left,
        ));
        assert_eq!(term.selection_to_string(), Some(String::from("\"aa\"a\n")));
    }

    #[test]
    fn block_selection_works() {
        let size = CrosswordsSize::new(5, 5);
        let window_id = crate::event::WindowId::from(0);

        let mut term = Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener {},
            window_id,
            0,
            10_000,
        );
        let grid = &mut term.grid;
        for i in 1..4 {
            grid[Line(i)][Column(0)].set_c('"');

            for j in 1..4 {
                grid[Line(i)][Column(j)].set_c('a');
            }

            grid[Line(i)][Column(4)].set_c('"');
        }
        grid[Line(2)][Column(2)].set_c(' ');
        grid[Line(2)][Column(4)].set_wrapline(true);
        grid[Line(3)][Column(4)].set_c(' ');

        term.selection = Some(Selection::new(
            SelectionType::Block,
            Pos {
                row: Line(0),
                col: Column(3),
            },
            Side::Left,
        ));

        // The same column.
        if let Some(s) = term.selection.as_mut() {
            s.update(
                Pos {
                    row: Line(3),
                    col: Column(3),
                },
                Side::Right,
            );
        }
        assert_eq!(term.selection_to_string(), Some(String::from("\na\na\na")));

        // The first column.
        if let Some(s) = term.selection.as_mut() {
            s.update(
                Pos {
                    row: Line(3),
                    col: Column(0),
                },
                Side::Left,
            );
        }
        assert_eq!(
            term.selection_to_string(),
            Some(String::from("\n\"aa\n\"a\n\"aa"))
        );

        // The last column.
        if let Some(s) = term.selection.as_mut() {
            s.update(
                Pos {
                    row: Line(3),
                    col: Column(4),
                },
                Side::Right,
            );
        }
        assert_eq!(
            term.selection_to_string(),
            Some(String::from("\na\"\na\"\na"))
        );
    }

    fn make_term_for_selection(rows: usize, cols: usize) -> Crosswords<VoidListener> {
        let size = CrosswordsSize::new(cols, rows);
        let window_id = crate::event::WindowId::from(0);
        Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener {},
            window_id,
            0,
            10_000,
        )
    }

    fn select_simple(
        term: &mut Crosswords<VoidListener>,
        start: (i32, usize),
        end: (i32, usize),
    ) {
        term.selection = Some(Selection::new(
            SelectionType::Simple,
            Pos {
                row: Line(start.0),
                col: Column(start.1),
            },
            Side::Left,
        ));
        if let Some(s) = term.selection.as_mut() {
            s.update(
                Pos {
                    row: Line(end.0),
                    col: Column(end.1),
                },
                Side::Right,
            );
        }
    }

    /// `\0` cells in the middle of a run of content must be emitted as ASCII
    /// spaces, not raw NULs. This is the "TUI redrew its UI and left holes"
    /// case (e.g. fullscreen apps that paint with cursor positioning).
    #[test]
    fn null_cells_inside_run_become_spaces() {
        let mut term = make_term_for_selection(1, 7);
        let grid = &mut term.grid;
        // Row layout: a, a, \0, \0, \0, b, b
        grid[Line(0)][Column(0)].set_c('a');
        grid[Line(0)][Column(1)].set_c('a');
        grid[Line(0)][Column(5)].set_c('b');
        grid[Line(0)][Column(6)].set_c('b');

        select_simple(&mut term, (0, 0), (0, 6));
        let s = term.selection_to_string().unwrap();
        assert_eq!(s, "aa   bb");
        assert!(!s.contains('\0'), "selection must not contain raw NULs");
    }

    /// Trailing `\0` and trailing spaces on a non-wrapped row must be dropped
    /// rather than padding the copy out to column width.
    #[test]
    fn trailing_blanks_on_non_wrapped_row_are_dropped() {
        let mut term = make_term_for_selection(1, 8);
        let grid = &mut term.grid;
        grid[Line(0)][Column(0)].set_c('h');
        grid[Line(0)][Column(1)].set_c('i');
        grid[Line(0)][Column(2)].set_c(' ');
        grid[Line(0)][Column(3)].set_c(' ');
        // cols 4..=7 stay as \0

        select_simple(&mut term, (0, 0), (0, 7));
        assert_eq!(term.selection_to_string(), Some(String::from("hi")));
    }

    /// Trailing blank rows in a multi-row selection must be dropped, not
    /// emitted as a run of `\n`s.
    #[test]
    fn trailing_blank_rows_are_dropped() {
        let mut term = make_term_for_selection(5, 5);
        let grid = &mut term.grid;
        grid[Line(0)][Column(0)].set_c('x');
        grid[Line(0)][Column(1)].set_c('y');
        // Rows 1..=4 are entirely \0.

        select_simple(&mut term, (0, 0), (4, 4));
        assert_eq!(term.selection_to_string(), Some(String::from("xy")));
    }

    /// Blank rows between non-blank rows must still be emitted as `\n`s, so
    /// real visual gaps in the selection are preserved.
    #[test]
    fn blank_rows_between_content_are_preserved() {
        let mut term = make_term_for_selection(5, 5);
        let grid = &mut term.grid;
        grid[Line(0)][Column(0)].set_c('a');
        // Rows 1, 2 entirely \0.
        grid[Line(3)][Column(0)].set_c('b');
        // Row 4 entirely \0.

        select_simple(&mut term, (0, 0), (4, 4));
        assert_eq!(term.selection_to_string(), Some(String::from("a\n\n\nb")));
    }

    /// When a row wraps into the next, the trailing-space buffer must carry
    /// across so the visual gap survives the round-trip through the clipboard.
    #[test]
    fn trailing_space_carries_across_wrap_continuation() {
        let mut term = make_term_for_selection(2, 5);
        let grid = &mut term.grid;
        // Row 0: "ab " with a wrap into row 1.
        grid[Line(0)][Column(0)].set_c('a');
        grid[Line(0)][Column(1)].set_c('b');
        grid[Line(0)][Column(2)].set_c(' ');
        grid[Line(0)][Column(3)].set_c(' ');
        grid[Line(0)][Column(4)].set_c(' ');
        grid[Line(0)][Column(4)].set_wrapline(true);
        // Row 1: " cd "
        grid[Line(1)][Column(0)].set_c(' ');
        grid[Line(1)][Column(1)].set_c('c');
        grid[Line(1)][Column(2)].set_c('d');
        grid[Line(1)][Column(3)].set_c(' ');
        grid[Line(1)][Column(4)].set_c(' ');

        // Trailing spaces on row 1 dropped (no further continuation), but the
        // 4 spaces between `b` and `c` must survive across the wrap.
        select_simple(&mut term, (0, 0), (1, 4));
        assert_eq!(term.selection_to_string(), Some(String::from("ab    cd")));
    }

    #[test]
    fn parse_cargo_version() {
        assert_eq!(version_number("0.0.1-nightly"), 1);
        assert_eq!(version_number("0.1.2-nightly"), 1_02);
        assert_eq!(version_number("1.2.3-nightly"), 1_02_03);
        assert_eq!(version_number("999.99.99"), 9_99_99_99);
    }

    #[test]
    fn test_cursor_damage_after_clear() {
        use crate::ansi::CursorShape;
        use crate::crosswords::CrosswordsSize;
        use crate::event::{VoidListener, WindowId};
        use crate::performer::handler::Handler;

        let size = CrosswordsSize::new(10, 10);
        let window_id = WindowId::from(0);
        let mut term = Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener {},
            window_id,
            0,
            10_000,
        );

        // Move cursor to position (1, 5) and type some text
        term.goto(Line(1), Column(5));
        for c in "hello".chars() {
            term.input(c);
        }

        // Get initial damage and reset
        {
            let _initial_damage = term.damage();
        }
        term.reset_damage();

        // Simulate `clear` command: clear screen and move cursor to home
        term.clear_screen(ClearMode::All);
        term.goto(Line(0), Column(0));

        // Verify cursor is at origin
        assert_eq!(term.grid.cursor.pos.row, Line(0));
        assert_eq!(term.grid.cursor.pos.col, Column(0));

        // Reset damage after clear
        {
            let _clear_damage = term.damage();
        }
        term.reset_damage();

        // Now type "aa" - both characters should trigger line damage
        term.input('a');

        // Check damage after first 'a' - should damage entire line 0
        let has_damage_first = {
            let damage_after_first_a = term.damage();
            match damage_after_first_a {
                TermDamage::Partial(iter) => {
                    let damaged_lines: Vec<_> = iter.collect();
                    !damaged_lines.is_empty()
                        && damaged_lines.iter().any(|line| line.line == 0)
                }
                TermDamage::Full => true,
            }
        };
        assert!(has_damage_first, "First 'a' should cause line damage");
        term.reset_damage();

        term.input('a');

        // Check damage after second 'a' - should also damage entire line 0
        let has_damage_second = {
            let damage_after_second_a = term.damage();
            match damage_after_second_a {
                TermDamage::Partial(iter) => {
                    let damaged_lines: Vec<_> = iter.collect();
                    !damaged_lines.is_empty()
                        && damaged_lines.iter().any(|line| line.line == 0)
                }
                TermDamage::Full => true,
            }
        };
        assert!(has_damage_second, "Second 'a' should cause line damage");
        term.reset_damage();

        // Verify final cursor position
        assert_eq!(term.grid.cursor.pos.row, Line(0));
        assert_eq!(term.grid.cursor.pos.col, Column(2)); // After typing "aa"
    }

    #[test]
    fn test_line_damage_approach() {
        use crate::ansi::CursorShape;
        use crate::crosswords::CrosswordsSize;
        use crate::event::{VoidListener, WindowId};

        let size = CrosswordsSize::new(10, 10);
        let window_id = WindowId::from(0);
        let mut term = Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener {},
            window_id,
            0,
            10_000,
        );

        // Reset damage to start clean
        term.reset_damage();

        // Move cursor to line 2 and damage that line
        term.goto(Line(2), Column(3));
        term.damage_cursor_line();

        let damage_result = {
            let damage = term.damage();
            match damage {
                TermDamage::Partial(iter) => {
                    let damaged_lines: Vec<_> = iter.collect();
                    damaged_lines
                        .iter()
                        .find(|line| line.line == 2)
                        .map(|line| line.damaged)
                }
                TermDamage::Full => Some(true), // Full damage
            }
        };

        // Should damage line 2
        assert_eq!(damage_result, Some(true), "Should damage line 2");
        term.reset_damage();

        // Test the general damage_line method
        term.damage_line(5);

        let damage_result_2 = {
            let damage = term.damage();
            match damage {
                TermDamage::Partial(iter) => {
                    let damaged_lines: Vec<_> = iter.collect();
                    damaged_lines
                        .iter()
                        .find(|line| line.line == 5)
                        .map(|line| line.damaged)
                }
                TermDamage::Full => Some(true),
            }
        };

        // Should damage line 5
        assert_eq!(damage_result_2, Some(true), "Should damage line 5");
    }

    /// Unit tests for keyboard mode stack functionality
    /// These tests verify the push, pop, and set operations for the keyboard mode stack
    /// which was refactored in commit 7cfd5f73a1934f641174ed3fe335b6f37cb75316
    ///
    /// Test coverage:
    /// - test_keyboard_mode_push_pop: Basic push and pop operations
    /// - test_keyboard_mode_stack_wraparound: Stack overflow protection and wraparound
    /// - test_keyboard_mode_pop_excessive: Handling of excessive pop operations
    /// - test_keyboard_mode_set_replace: Replace behavior for keyboard modes
    /// - test_keyboard_mode_set_union: Union behavior for keyboard modes
    /// - test_keyboard_mode_set_difference: Difference behavior for keyboard modes
    /// - test_keyboard_mode_report: Current mode reporting functionality
    /// - test_keyboard_mode_reset: Terminal reset behavior on keyboard stack
    /// - test_keyboard_mode_stack_underflow_protection: Stack underflow protection

    #[test]
    fn test_keyboard_mode_push_pop() {
        let size = CrosswordsSize::new(10, 10);
        let window_id = WindowId::from(0);
        let mut term = Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener {},
            window_id,
            0,
            10_000,
        );

        // Initial state: stack should be empty with NO_MODE
        assert_eq!(
            term.keyboard_mode_stack[term.keyboard_mode_idx],
            KeyboardModes::NO_MODE.bits()
        );
        assert_eq!(term.keyboard_mode_idx, 0);

        // Push first mode using Handler trait
        Handler::push_keyboard_mode(&mut term, KeyboardModes::DISAMBIGUATE_ESC_CODES);
        assert_eq!(term.keyboard_mode_idx, 1);
        assert_eq!(
            term.keyboard_mode_stack[1],
            KeyboardModes::DISAMBIGUATE_ESC_CODES.bits()
        );

        // Push second mode
        Handler::push_keyboard_mode(&mut term, KeyboardModes::REPORT_EVENT_TYPES);
        assert_eq!(term.keyboard_mode_idx, 2);
        assert_eq!(
            term.keyboard_mode_stack[2],
            KeyboardModes::REPORT_EVENT_TYPES.bits()
        );

        // Pop one mode using Handler trait
        Handler::pop_keyboard_modes(&mut term, 1);
        assert_eq!(term.keyboard_mode_idx, 1);
        assert_eq!(term.keyboard_mode_stack[2], KeyboardModes::NO_MODE.bits()); // Should be cleared

        // Pop remaining mode
        Handler::pop_keyboard_modes(&mut term, 1);
        assert_eq!(term.keyboard_mode_idx, 0);
        assert_eq!(term.keyboard_mode_stack[1], KeyboardModes::NO_MODE.bits()); // Should be cleared
    }

    #[test]
    fn test_keyboard_mode_stack_wraparound() {
        let size = CrosswordsSize::new(10, 10);
        let window_id = WindowId::from(0);
        let mut term = Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener {},
            window_id,
            0,
            10_000,
        );

        // Fill the stack to maximum depth using Handler trait
        for i in 0..KEYBOARD_MODE_STACK_MAX_DEPTH {
            Handler::push_keyboard_mode(&mut term, KeyboardModes::DISAMBIGUATE_ESC_CODES);
            assert_eq!(
                term.keyboard_mode_idx,
                (i + 1) % KEYBOARD_MODE_STACK_MAX_DEPTH
            );
        }

        // Push one more - should wrap around
        Handler::push_keyboard_mode(&mut term, KeyboardModes::REPORT_EVENT_TYPES);
        assert_eq!(term.keyboard_mode_idx, 1); // Should wrap to 1
        assert_eq!(
            term.keyboard_mode_stack[1],
            KeyboardModes::REPORT_EVENT_TYPES.bits()
        );
    }

    #[test]
    fn test_keyboard_mode_pop_excessive() {
        let size = CrosswordsSize::new(10, 10);
        let window_id = WindowId::from(0);
        let mut term = Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener {},
            window_id,
            0,
            10_000,
        );

        // Push a few modes using Handler trait
        Handler::push_keyboard_mode(&mut term, KeyboardModes::DISAMBIGUATE_ESC_CODES);
        Handler::push_keyboard_mode(&mut term, KeyboardModes::REPORT_EVENT_TYPES);
        Handler::push_keyboard_mode(&mut term, KeyboardModes::REPORT_ALTERNATE_KEYS);

        // Pop more modes than exist - should clear everything
        Handler::pop_keyboard_modes(&mut term, KEYBOARD_MODE_STACK_MAX_DEPTH as u16);

        assert_eq!(term.keyboard_mode_idx, 0);
        // All modes should be cleared to NO_MODE
        for i in 0..KEYBOARD_MODE_STACK_MAX_DEPTH {
            assert_eq!(term.keyboard_mode_stack[i], KeyboardModes::NO_MODE.bits());
        }
    }

    #[test]
    fn test_keyboard_mode_set_replace() {
        let size = CrosswordsSize::new(10, 10);
        let window_id = WindowId::from(0);
        let mut term = Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener {},
            window_id,
            0,
            10_000,
        );

        // Set initial mode using Handler trait method
        Handler::set_keyboard_mode(
            &mut term,
            KeyboardModes::DISAMBIGUATE_ESC_CODES,
            KeyboardModesApplyBehavior::Replace,
        );
        assert_eq!(
            term.keyboard_mode_stack[term.keyboard_mode_idx],
            KeyboardModes::DISAMBIGUATE_ESC_CODES.bits()
        );

        // Replace with different mode
        Handler::set_keyboard_mode(
            &mut term,
            KeyboardModes::REPORT_EVENT_TYPES,
            KeyboardModesApplyBehavior::Replace,
        );
        assert_eq!(
            term.keyboard_mode_stack[term.keyboard_mode_idx],
            KeyboardModes::REPORT_EVENT_TYPES.bits()
        );
    }

    #[test]
    fn test_keyboard_mode_set_union() {
        let size = CrosswordsSize::new(10, 10);
        let window_id = WindowId::from(0);
        let mut term = Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener {},
            window_id,
            0,
            10_000,
        );

        // Set initial mode using Handler trait method
        Handler::set_keyboard_mode(
            &mut term,
            KeyboardModes::DISAMBIGUATE_ESC_CODES,
            KeyboardModesApplyBehavior::Replace,
        );

        // Add another mode using union
        Handler::set_keyboard_mode(
            &mut term,
            KeyboardModes::REPORT_EVENT_TYPES,
            KeyboardModesApplyBehavior::Union,
        );

        let expected = KeyboardModes::DISAMBIGUATE_ESC_CODES.bits()
            | KeyboardModes::REPORT_EVENT_TYPES.bits();
        assert_eq!(term.keyboard_mode_stack[term.keyboard_mode_idx], expected);
    }

    #[test]
    fn test_keyboard_mode_set_difference() {
        let size = CrosswordsSize::new(10, 10);
        let window_id = WindowId::from(0);
        let mut term = Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener {},
            window_id,
            0,
            10_000,
        );

        // Set combined mode using Handler trait method
        let combined_mode =
            KeyboardModes::DISAMBIGUATE_ESC_CODES | KeyboardModes::REPORT_EVENT_TYPES;
        Handler::set_keyboard_mode(
            &mut term,
            combined_mode,
            KeyboardModesApplyBehavior::Replace,
        );

        // Remove one mode using difference
        Handler::set_keyboard_mode(
            &mut term,
            KeyboardModes::REPORT_EVENT_TYPES,
            KeyboardModesApplyBehavior::Difference,
        );

        assert_eq!(
            term.keyboard_mode_stack[term.keyboard_mode_idx],
            KeyboardModes::DISAMBIGUATE_ESC_CODES.bits()
        );
    }

    #[test]
    fn test_keyboard_mode_report() {
        let size = CrosswordsSize::new(10, 10);
        let window_id = WindowId::from(0);
        let listener = VoidListener {};
        let mut term =
            Crosswords::new(size, CursorShape::Block, listener, window_id, 0, 10_000);

        // Push a mode and test reporting using Handler trait
        Handler::push_keyboard_mode(&mut term, KeyboardModes::DISAMBIGUATE_ESC_CODES);

        // The report_keyboard_mode function sends an event through event_proxy
        // We can't easily test the exact output without mocking the event system,
        // but we can verify the current mode is correctly retrieved
        let current_mode = term.keyboard_mode_stack[term.keyboard_mode_idx];
        assert_eq!(current_mode, KeyboardModes::DISAMBIGUATE_ESC_CODES.bits());
    }

    #[test]
    fn test_keyboard_mode_reset() {
        let size = CrosswordsSize::new(10, 10);
        let window_id = WindowId::from(0);
        let mut term = Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener {},
            window_id,
            0,
            10_000,
        );

        // Push several modes
        Handler::push_keyboard_mode(&mut term, KeyboardModes::DISAMBIGUATE_ESC_CODES);
        Handler::push_keyboard_mode(&mut term, KeyboardModes::REPORT_EVENT_TYPES);
        Handler::push_keyboard_mode(&mut term, KeyboardModes::REPORT_ALTERNATE_KEYS);

        // Reset terminal using Handler trait - use reset_state method
        term.reset_state();

        // Verify stack is reset
        assert_eq!(term.keyboard_mode_idx, 0);
        assert_eq!(term.inactive_keyboard_mode_idx, 0);
        for i in 0..KEYBOARD_MODE_STACK_MAX_DEPTH {
            assert_eq!(term.keyboard_mode_stack[i], 0);
            assert_eq!(term.inactive_keyboard_mode_stack[i], 0);
        }
    }

    #[test]
    fn test_keyboard_mode_stack_underflow_protection() {
        let size = CrosswordsSize::new(10, 10);
        let window_id = WindowId::from(0);
        let mut term = Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener {},
            window_id,
            0,
            10_000,
        );

        // Start at index 0, try to pop using Handler trait - should wrap correctly
        assert_eq!(term.keyboard_mode_idx, 0);

        Handler::pop_keyboard_modes(&mut term, 1);

        // With wraparound logic, index should wrap to max-1
        let expected_idx = (0_usize.wrapping_sub(1)) % KEYBOARD_MODE_STACK_MAX_DEPTH;
        assert_eq!(term.keyboard_mode_idx, expected_idx);
        assert_eq!(term.keyboard_mode_stack[0], KeyboardModes::NO_MODE.bits()); // Should be cleared
    }

    #[test]
    fn clipboard_packet_overflow_aborts_without_publishing_partial_data() {
        use crate::performer::handler::Processor;
        use std::{cell::RefCell, rc::Rc};
        for (command, aborts) in [("5522", true), ("2", false)] {
            let events = Rc::new(RefCell::new(Vec::new()));
            let mut term = Crosswords::new(
                CrosswordsSize::new(10, 10),
                CursorShape::Block,
                TestListener {
                    events: events.clone(),
                },
                WindowId::from(0),
                71,
                10,
            );
            let mut processor = Processor::default();
            processor.advance(&mut term, b"\x1b]5522;type=write:id=overflow\x07\x1b]5522;type=wdata:mime=YQ==;YQ==\x07");
            let mut packet = format!("\x1b]{command};type=wdata:mime=Yg==;").into_bytes();
            packet.extend(std::iter::repeat_n(b'A', 64 * 1024 + 1));
            packet.extend_from_slice(b"\x1b\\");
            for chunk in packet.chunks(4096) {
                processor.advance(&mut term, chunk);
            }
            processor.advance(&mut term, b"\x1b]5522;type=wdata\x07z");
            assert_eq!(events.borrow().iter().any(|event| matches!(event, RioEvent::ClipboardReply(71, response) if response.as_slice() == b"\x1b]5522;type=write:status=EFBIG:id=overflow\x1b\\")), aborts);
            assert_eq!(
                events
                    .borrow()
                    .iter()
                    .any(|event| matches!(event, RioEvent::KittyClipboardRequest(..))),
                !aborts
            );
            assert_eq!(term.grid[Line(0)][Column(0)].c(), 'z');
        }
    }

    #[test]
    fn osc52_uses_shared_policy_and_originating_session() {
        use crate::clipboard::{Access, ClipboardPolicy};
        use crate::performer::handler::Processor;
        use std::{cell::RefCell, rc::Rc};
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut term = Crosswords::new(
            CrosswordsSize::new(10, 10),
            CursorShape::Block,
            TestListener {
                events: events.clone(),
            },
            WindowId::from(0),
            71,
            10,
        );
        let mut processor = Processor::default();
        term.set_clipboard_policy(ClipboardPolicy {
            read: Access::Deny,
            write: Access::Deny,
            ..Default::default()
        });
        processor.advance(&mut term, b"\x1b]52;c;?\x07\x1b]52;c;aGVsbG8=\x07");
        assert!(!events.borrow().iter().any(|event| matches!(
            event,
            RioEvent::KittyClipboardRequest(..)
                | RioEvent::ClipboardLoad(..)
                | RioEvent::ClipboardStore(..)
        )));
        term.set_clipboard_policy(ClipboardPolicy {
            read: Access::Ask,
            write: Access::Ask,
            ..Default::default()
        });
        processor.advance(&mut term, b"\x1b]52;c;?\x07\x1b]52;c;aGVsbG8=\x1b\\");
        let captured = events.borrow();
        let requests: Vec<_> = captured
            .iter()
            .filter_map(|event| match event {
                RioEvent::KittyClipboardRequest(route, request) => {
                    Some((*route, request))
                }
                _ => None,
            })
            .collect();
        assert_eq!(requests.len(), 2);
        for (route, request) in requests {
            assert_eq!(route, 71);
            assert!(term.clipboard_request_current(request));
            assert!(!request.prompt_exempt());
            assert!(request
                .status(crate::clipboard::kitty::Status::Permission)
                .is_empty());
        }
    }

    #[test]
    fn kitty_paste_event_mode_set_query_reset() {
        use crate::performer::handler::Processor;
        use std::{cell::RefCell, rc::Rc};
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut term = Crosswords::new(
            CrosswordsSize::new(10, 10),
            CursorShape::Block,
            TestListener {
                events: events.clone(),
            },
            WindowId::from(0),
            71,
            10,
        );
        let mut processor = Processor::default();
        processor.advance(&mut term, b"\x1b[?5522$p\x1b[?5522h\x1b[?5522$p");
        assert!(term.mode.contains(Mode::KITTY_PASTE_EVENTS));
        assert!(events
            .borrow()
            .iter()
            .any(|e| matches!(e, RioEvent::PtyWrite(71,s) if s == "\x1b[?5522;2$y")));
        assert!(events
            .borrow()
            .iter()
            .any(|e| matches!(e, RioEvent::PtyWrite(71,s) if s == "\x1b[?5522;1$y")));
        processor.advance(&mut term, b"\x1bc");
        assert!(!term.mode.contains(Mode::KITTY_PASTE_EVENTS));
        assert!(term
            .clipboard_paste_event(ClipboardType::Clipboard, &[])
            .unwrap()
            .is_none());
    }

    #[test]
    fn kitty_clipboard_session_routes_commits_and_cancels_on_reset() {
        use crate::clipboard::kitty::Request;
        use crate::performer::handler::Processor;
        use std::{cell::RefCell, rc::Rc};
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut term = Crosswords::new(
            CrosswordsSize::new(10, 10),
            CursorShape::Block,
            TestListener {
                events: events.clone(),
            },
            WindowId::from(0),
            71,
            10,
        );
        let mut processor = Processor::default();
        processor.advance(&mut term, b"\x1b]5522;type=write:id=old\x07\x1b]5522;type=wdata:mime=YQ==;YQ\x07\x1b]5522;type=write:id=new\x07\x1b]5522;type=wdata:mime=YQ==;Yg==\x07\x1b]5522;type=wdata\x07");
        let captured = events.borrow();
        let requests: Vec<_> = captured
            .iter()
            .filter_map(|e| match e {
                RioEvent::KittyClipboardRequest(route, request) => Some((route, request)),
                _ => None,
            })
            .collect();
        assert_eq!(requests.len(), 1);
        assert_eq!(*requests[0].0, 71);
        let Request::Write { committed, .. } = requests[0].1.as_ref() else {
            panic!("expected commit")
        };
        assert_eq!(committed.metadata.id, "new");
        assert_eq!(committed.contents[0].data.as_slice(), b"b");
        drop(captured);
        events.borrow_mut().clear();
        processor.advance(&mut term, b"\x1b]5522;type=write:id=reset\x07\x1b]5522;type=wdata:mime=YQ==;YQ==\x07\x1bc\x1b]5522;type=wdata\x07");
        assert!(!events
            .borrow()
            .iter()
            .any(|e| matches!(e, RioEvent::KittyClipboardRequest(..))));
        events.borrow_mut().clear();
        processor.advance(&mut term, b"\x1b]5522;type=write:id=bad\x07\x1b]5522;type=wdata:mime=***;YQ==\x07\x1b]5522;type=wdata\x07");
        assert!(events.borrow().iter().any(|e| matches!(e, RioEvent::ClipboardReply(71, response) if response.as_slice() == b"\x1b]5522;type=write:status=EINVAL:id=bad\x07")));
        assert!(!events
            .borrow()
            .iter()
            .any(|e| matches!(e, RioEvent::KittyClipboardRequest(..))));
    }

    #[test]
    fn test_xtversion_report() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let size = CrosswordsSize::new(10, 10);
        let window_id = WindowId::from(0);
        let events = Rc::new(RefCell::new(Vec::new()));
        let listener = TestListener {
            events: events.clone(),
        };
        let mut term =
            Crosswords::new(size, CursorShape::Block, listener, window_id, 0, 10_000);

        // Call report_version using Handler trait
        Handler::report_version(&mut term);

        // Verify that a PtyWrite event was sent
        let captured_events = events.borrow();
        assert_eq!(captured_events.len(), 1, "Should have sent one event");

        // Verify the event is PtyWrite with the correct format
        match &captured_events[0] {
            RioEvent::PtyWrite(_route_id, text) => {
                // Expected format: DCS > | Rio {version} ST
                // DCS = \x1bP, ST = \x1b\\
                assert!(
                    text.starts_with("\x1bP>|Rio "),
                    "Should start with DCS>|Rio"
                );
                assert!(text.ends_with("\x1b\\"), "Should end with ST");

                // Extract version from the response
                let version = env!("CARGO_PKG_VERSION");
                let expected = format!("\x1bP>|Rio {}\x1b\\", version);
                assert_eq!(
                    text, &expected,
                    "XTVERSION response should match expected format"
                );
            }
            other => panic!("Expected PtyWrite event, got {:?}", other),
        }
    }

    #[test]
    fn chrome_metadata_follows_protocol_updates_and_reset() {
        let mut term = Crosswords::new(
            CrosswordsSize::new(80, 24),
            CursorShape::Block,
            VoidListener,
            WindowId::from(0),
            0,
            0,
        );
        let metadata = term.metadata.clone();
        let mut parser = crate::performer::handler::Processor::default();
        parser.advance(
            &mut term,
            b"\x1b]2;original\x07\x1b[22;0t\x1b]2;changed\x07",
        );
        assert_eq!(metadata.lock().title, "changed");
        parser.advance(&mut term, b"\x1b[23;0t");
        assert_eq!(metadata.lock().title, "original");
        parser.advance(&mut term, b"\x1b]1337;SetUserVar=RIO_APP=Y29kZXg=\x07\x1b]1337;SetUserVar=RIO_SSH=MQ==\x07");
        assert_eq!(metadata.lock().app.as_deref(), Some("codex"));
        assert!(metadata.lock().remote);
        parser.advance(
            &mut term,
            b"\x1b]7501;state=working:app=codex:progress=25\x1b\\",
        );
        assert_eq!(metadata.lock().status, term.program_status.display_status());
        assert_eq!(
            metadata.lock().status_revision,
            term.program_status.revision()
        );
        parser.advance(&mut term, b"\x1b]9;4;2;0\x07");
        assert_eq!(metadata.lock().status, term.program_status.display_status());
        parser.advance(&mut term, b"\x1bc");
        assert!(metadata.lock().title.is_empty());
        assert!(metadata.lock().status.is_none());
        assert_eq!(
            metadata.lock().status_revision,
            term.program_status.revision()
        );
        parser.advance(
            &mut term,
            b"\x1b]1337;SetUserVar=RIO_APP=\x07\x1b]1337;SetUserVar=RIO_SSH=MA==\x07",
        );
        assert!(metadata.lock().app.is_none());
        assert!(!metadata.lock().remote);
    }

    #[cfg(feature = "graphics")]
    #[test]
    fn expired_kitty_anchors_retain_bitmaps_until_placement_reaping() {
        use crate::performer::handler::Processor;
        let mut term = new_term(8, 3);
        term.graphics.cell_width = 10.0;
        term.graphics.cell_height = 20.0;
        let mut parser = Processor::default();
        term.set_scrollback_limits(1, None);
        parser.advance(
            &mut term,
            b"\x1b_Ga=T,i=1,p=1,f=32,s=1,v=1,c=1,r=10000,C=1;AQIDBA==\x1b\\",
        );
        parser.advance(
            &mut term,
            b"\x1b_Ga=T,i=2,p=1,f=32,s=1,v=1,c=1,r=1,C=1,P=1,Q=1;AQIDBA==\x1b\\",
        );
        parser.advance(&mut term, "line\r\n".repeat(20).as_bytes());
        assert!(term.graphics.resolved_kitty_placements().is_empty());
        assert!(
            term.graphics.kitty_placements.contains_key(&(1, 1)),
            "{:?}",
            term.graphics.kitty_placements
        );
        parser.advance(&mut term, b"\x1b[2J");
        assert!(term.graphics.kitty_images.contains_key(&1));
        assert!(term.graphics.kitty_images.contains_key(&2));
        let expired = term.graphics.kitty_placements[&(1, 1)].clone();
        for id in 2..4096 {
            let mut copy = expired.clone();
            copy.placement_id = id;
            term.graphics.kitty_placements.insert((1, id), copy);
        }
        assert_eq!(term.graphics.kitty_placements.len(), 4096);
        assert!(term.graphics.can_place_kitty(1, 4097, false));
        parser.advance(&mut term, b"\x1b_Ga=p,i=1,p=4097,c=1,r=1,C=1\x1b\\\x1b[2J");
        assert!(term.graphics.kitty_images.is_empty());
    }

    #[test]
    fn title_state_preserves_whitespace_across_chunks() {
        use crate::performer::handler::Processor;
        for command in [0, 2] {
            for chunk in [1, 7, 65536] {
                for title in ["  spaced  ", "   ", "\u{2003}界\u{2003}"] {
                    let mut term = new_term(8, 3);
                    let mut parser = Processor::default();
                    let text = format!("\x1b]{command};{title}\x1b\\");
                    for bytes in text.as_bytes().chunks(chunk) {
                        parser.advance(&mut term, bytes);
                    }
                    assert_eq!(term.title, title);
                    assert_eq!(term.metadata.lock().title, title);
                }
            }
        }
    }

    #[test]
    fn title_state_stack_ignores_unsupported_selectors() {
        use crate::performer::handler::Processor;
        for (suffix, supported) in [
            ("", false),
            (";0", true),
            (";1", false),
            (";2", true),
            (";3", false),
            (";0;0", true),
            (";2;2", true),
            (";2;1", true),
            (";2;65535", true),
            (";2;0;0", false),
            (":0;2", false),
            (";2:0", false),
        ] {
            let mut term = new_term(8, 3);
            let mut parser = Processor::default();
            parser.advance(&mut term, b"\x1b]2;before\x07");
            parser.advance(&mut term, format!("\x1b[22{suffix}t").as_bytes());
            parser.advance(&mut term, b"\x1b]2;after\x07\x1b[23;0t");
            assert_eq!(
                term.title,
                if supported { "before" } else { "after" },
                "push {suffix}"
            );

            parser.advance(&mut term, b"\x1b]2;before\x07\x1b[22;0t\x1b]2;after\x07");
            parser.advance(&mut term, format!("\x1b[23{suffix}t").as_bytes());
            assert_eq!(
                term.title,
                if supported { "before" } else { "after" },
                "pop {suffix}"
            );
            parser.advance(&mut term, b"\x1b]2;final\x07\x1b[23;0t");
            assert_eq!(
                term.title,
                if supported { "final" } else { "before" },
                "remaining {suffix}"
            );
        }
    }

    #[test]
    fn set_title_emits_title_event() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let events = Rc::new(RefCell::new(Vec::new()));
        let mut term = Crosswords::new(
            CrosswordsSize::new(10, 10),
            CursorShape::Block,
            TestListener {
                events: events.clone(),
            },
            WindowId::from(0),
            7,
            10,
        );

        Handler::set_title(&mut term, Some("my title".to_string()));

        assert_eq!(term.title, "my title");
        assert!(
            events
                .borrow()
                .iter()
                .any(|e| matches!(e, RioEvent::Title(route, t) if *route == 7 && t == "my title")),
            "set_title should emit RioEvent::Title carrying its route id"
        );
    }

    #[test]
    fn reset_emits_empty_title_event_once() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let events = Rc::new(RefCell::new(Vec::new()));
        let mut term = Crosswords::new(
            CrosswordsSize::new(10, 10),
            CursorShape::Block,
            TestListener {
                events: events.clone(),
            },
            WindowId::from(0),
            7,
            10,
        );

        Handler::set_title(&mut term, Some("vim - foo.rs".to_string()));
        events.borrow_mut().clear();

        // RIS must announce the cleared title like any other change,
        // or the strip and titlebar keep the pre-reset text forever.
        Handler::reset_state(&mut term);
        let count = events
            .borrow()
            .iter()
            .filter(
                |e| matches!(e, RioEvent::Title(route, t) if *route == 7 && t.is_empty()),
            )
            .count();
        assert_eq!(count, 1);

        // A reset with no title set stays silent.
        events.borrow_mut().clear();
        Handler::reset_state(&mut term);
        assert!(events
            .borrow()
            .iter()
            .all(|e| !matches!(e, RioEvent::Title(..))));
    }

    #[test]
    fn set_current_directory_emits_event_once_per_change() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let events = Rc::new(RefCell::new(Vec::new()));
        let mut term = Crosswords::new(
            CrosswordsSize::new(10, 10),
            CursorShape::Block,
            TestListener {
                events: events.clone(),
            },
            WindowId::from(0),
            7,
            10,
        );

        Handler::set_current_directory(&mut term, "/tmp/a".into());
        Handler::set_current_directory(&mut term, "/tmp/a".into());

        let count = events
            .borrow()
            .iter()
            .filter(
                |e| matches!(e, RioEvent::CurrentDirectoryChanged(route) if *route == 7),
            )
            .count();
        assert_eq!(
            count, 1,
            "a repeated OSC 7 for the same directory must not re-emit"
        );
        assert_eq!(
            term.current_directory.as_deref(),
            Some(std::path::Path::new("/tmp/a"))
        );
    }

    #[test]
    fn bell_emits_bell_event_for_its_route() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let events = Rc::new(RefCell::new(Vec::new()));
        let mut term = Crosswords::new(
            CrosswordsSize::new(10, 10),
            CursorShape::Block,
            TestListener {
                events: events.clone(),
            },
            WindowId::from(0),
            7,
            10,
        );

        // Drive a real BEL byte through the parser rather than calling the
        // handler directly, so the C0 dispatch stays covered too.
        let mut parser = crate::performer::handler::Processor::default();
        parser.advance(&mut term, b"\x07");

        assert!(
            events
                .borrow()
                .iter()
                .any(|e| matches!(e, RioEvent::Bell(route) if *route == 7)),
            "BEL should emit RioEvent::Bell carrying its route id"
        );
    }

    #[test]
    fn test_keyboard_mode_syncs_with_mode() {
        let size = CrosswordsSize::new(10, 10);
        let window_id = WindowId::from(0);
        let mut term = Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener {},
            window_id,
            0,
            10_000,
        );

        // Initially, no keyboard mode should be set
        assert!(!term.mode().contains(Mode::DISAMBIGUATE_ESC_CODES));
        assert!(!term.mode().contains(Mode::REPORT_ALL_KEYS_AS_ESC));

        // Push DISAMBIGUATE_ESC_CODES
        Handler::push_keyboard_mode(&mut term, KeyboardModes::DISAMBIGUATE_ESC_CODES);
        assert!(
            term.mode().contains(Mode::DISAMBIGUATE_ESC_CODES),
            "mode() should contain DISAMBIGUATE_ESC_CODES after push"
        );
        assert!(!term.mode().contains(Mode::REPORT_ALL_KEYS_AS_ESC));

        // Push REPORT_ALL_KEYS_AS_ESC (replaces previous mode at this stack level)
        Handler::push_keyboard_mode(&mut term, KeyboardModes::REPORT_ALL_KEYS_AS_ESC);
        assert!(
            term.mode().contains(Mode::REPORT_ALL_KEYS_AS_ESC),
            "mode() should contain REPORT_ALL_KEYS_AS_ESC after push"
        );
        assert!(!term.mode().contains(Mode::DISAMBIGUATE_ESC_CODES),
            "mode() should not contain DISAMBIGUATE_ESC_CODES after pushing different mode"
        );

        // Pop back to previous level
        Handler::pop_keyboard_modes(&mut term, 1);
        assert!(
            term.mode().contains(Mode::DISAMBIGUATE_ESC_CODES),
            "mode() should contain DISAMBIGUATE_ESC_CODES after pop"
        );
        assert!(
            !term.mode().contains(Mode::REPORT_ALL_KEYS_AS_ESC),
            "mode() should not contain REPORT_ALL_KEYS_AS_ESC after pop"
        );

        // Test set_keyboard_mode with Union
        Handler::set_keyboard_mode(
            &mut term,
            KeyboardModes::REPORT_EVENT_TYPES,
            KeyboardModesApplyBehavior::Union,
        );
        assert!(
            term.mode().contains(Mode::DISAMBIGUATE_ESC_CODES),
            "mode() should still contain DISAMBIGUATE_ESC_CODES after union"
        );
        assert!(
            term.mode().contains(Mode::REPORT_EVENT_TYPES),
            "mode() should contain REPORT_EVENT_TYPES after union"
        );

        // Test set_keyboard_mode with Replace
        Handler::set_keyboard_mode(
            &mut term,
            KeyboardModes::REPORT_ALTERNATE_KEYS,
            KeyboardModesApplyBehavior::Replace,
        );
        assert!(
            term.mode().contains(Mode::REPORT_ALTERNATE_KEYS),
            "mode() should contain REPORT_ALTERNATE_KEYS after replace"
        );
        assert!(
            !term.mode().contains(Mode::DISAMBIGUATE_ESC_CODES),
            "mode() should not contain DISAMBIGUATE_ESC_CODES after replace"
        );
        assert!(
            !term.mode().contains(Mode::REPORT_EVENT_TYPES),
            "mode() should not contain REPORT_EVENT_TYPES after replace"
        );
    }

    /// Insert a small sixel graphic and verify that every cell in the
    /// image span carries the GRAPHICS flag with a valid extras_id
    /// pointing to a GraphicCell in the extras table.
    #[test]
    fn sixel_stores_placement_spanning_cells() {
        let size = CrosswordsSize::new(20, 10);
        let window_id = crate::event::WindowId::from(0);
        let mut cw = Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener {},
            window_id,
            0,
            10_000,
        );
        cw.graphics.cell_width = 10.0;
        cw.graphics.cell_height = 10.0;

        let graphic = GraphicData {
            id: rio_graphics::GraphicId::new(0),
            width: 20,
            height: 20,
            pixels: vec![0u8; 20 * 20 * 4],
            color_type: rio_graphics::ColorType::Rgba,
            is_opaque: true,
            display_width: None,
            display_height: None,
            resize: None,
            transmit_time: crate::time::Instant::now(),
        };

        cw.insert_graphic(graphic, None, None);

        // One placement spanning rows 0..2 x cols 0..2; cells stay
        // untouched (placements are the single source of truth).
        assert_eq!(cw.graphics.atlas_placements.len(), 1);
        let p = &cw.graphics.atlas_placements[0];
        assert_eq!((p.abs_row, p.col, p.columns, p.rows), (0, 0, 2, 2));
        assert!(cw.grid[Line(0)][Column(0)].extras_id().is_none());
        // The placement holds the image key alive.
        assert_eq!(cw.graphics.atlas_key_refs.len(), 1);
    }

    /// Verify that `cell_graphic()` reads the first GraphicCell back.
    #[test]
    fn insert_graphic_creates_and_replaces_placements() {
        let size = CrosswordsSize::new(20, 10);
        let window_id = crate::event::WindowId::from(0);
        let mut cw = Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener {},
            window_id,
            0,
            10_000,
        );
        cw.graphics.cell_width = 10.0;
        cw.graphics.cell_height = 10.0;

        let graphic = GraphicData {
            id: rio_graphics::GraphicId::new(0),
            width: 10,
            height: 10,
            pixels: vec![0u8; 10 * 10 * 4],
            color_type: rio_graphics::ColorType::Rgba,
            is_opaque: true,
            display_width: None,
            display_height: None,
            resize: None,
            transmit_time: crate::time::Instant::now(),
        };
        cw.insert_graphic(graphic.clone(), None, Some(1));

        assert_eq!(cw.graphics.atlas_placements.len(), 1);
        let p = &cw.graphics.atlas_placements[0];
        assert_eq!((p.abs_row, p.col, p.columns, p.rows), (0, 0, 1, 1));

        // A second image fully covering the first replaces it.
        cw.grid.cursor.pos = Pos::new(Line(0), Column(0));
        cw.insert_graphic(graphic, None, Some(1));
        assert_eq!(cw.graphics.atlas_placements.len(), 1);
        assert_eq!(cw.graphics.atlas_key_refs.len(), 1);
    }

    /// Headless embedders never set `graphics.cell_width`/`cell_height`;
    /// inserting a graphic then must be a no-op instead of panicking in the
    /// row-fill loop (`step_by(0)`).
    #[test]
    fn insert_graphic_without_cell_dimensions_is_noop() {
        let size = CrosswordsSize::new(20, 10);
        let window_id = crate::event::WindowId::from(0);
        let mut cw = Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener {},
            window_id,
            0,
            10_000,
        );

        let graphic = GraphicData {
            id: rio_graphics::GraphicId::new(0),
            width: 10,
            height: 10,
            pixels: vec![0u8; 10 * 10 * 4],
            color_type: rio_graphics::ColorType::Rgba,
            is_opaque: true,
            display_width: None,
            display_height: None,
            resize: None,
            transmit_time: crate::time::Instant::now(),
        };
        cw.insert_graphic(graphic, None, Some(1));
        assert!(cw.graphics.atlas_placements.is_empty());
    }

    // ------------------------------------------------------------------
    // Emoji presentation variation selectors (VS15 / VS16).
    // See `input()` + `apply_emoji_vs16` / `apply_emoji_vs15`.
    // ------------------------------------------------------------------

    fn new_term(cols: usize, rows: usize) -> Crosswords<VoidListener> {
        let size = CrosswordsSize::new(cols, rows);
        let window_id = crate::event::WindowId::from(0);
        Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener {},
            window_id,
            0,
            10_000,
        )
    }

    #[test]
    fn erased_prompt_rows_do_not_reflow_into_history() {
        use crate::performer::handler::Processor;
        let mut term = new_term(150, 36);
        let mut parser = Processor::default();
        parser.advance(&mut term, b"cwd\r\n> \x1b[38;2;205;214;244m\x1b[J");
        let cursor = term.grid.cursor.pos;

        term.resize(CrosswordsSize::new(74, 35));
        assert_eq!(term.history_size(), 0);
        term.resize(CrosswordsSize::new(150, 36));
        assert_eq!(term.history_size(), 0);
        assert_eq!(term.grid.cursor.pos, cursor);
    }

    #[test]
    fn row_erasure_preserves_only_the_background() {
        use crate::performer::handler::Processor;

        // ED, IL, DL and scrolling all recycle whole rows through the grid.
        for (erase, rows) in [
            ("\x1b[J", 0..4),
            ("\x1b[2J", 0..4),
            ("\x1b[L", 0..1),
            ("\x1b[M", 3..4),
            ("\x1b[S", 3..4),
            ("\x1b[T", 0..1),
        ] {
            for background in ["", "\x1b[48;5;42m", "\x1b[48;2;10;20;30m"] {
                let mut term = new_term(12, 4);
                let mut parser = Processor::default();
                let input = format!("\x1b[1;4;7;38;2;205;214;244m{background}{erase}");
                parser.advance(&mut term, input.as_bytes());
                let expected = crate::crosswords::style::Style {
                    bg: term.grid.template_style().bg,
                    ..Default::default()
                };
                for row in rows.clone() {
                    for column in 0..12 {
                        let cell = term.grid[Line(row)][Column(column)];
                        assert_eq!(term.grid.style_of(&cell), expected, "{input:?}");
                        assert_eq!(cell.c(), '\0');
                        assert!(!cell.has_extras());
                    }
                }
            }
        }
    }

    #[test]
    fn vs16_widens_text_presentation_emoji() {
        use crate::performer::handler::Handler;
        let mut cw = new_term(10, 3);
        // 🎟 (U+1F39F, EAW=N, default text presentation) then VS16.
        cw.input('\u{1F39F}');
        cw.input('\u{FE0F}');

        let row = Line(0);
        assert_eq!(cw.grid[row][Column(0)].c(), '\u{1F39F}');
        assert_eq!(cw.grid[row][Column(0)].wide(), Wide::Wide);
        assert_eq!(cw.grid[row][Column(1)].wide(), Wide::Spacer);
        assert_eq!(cw.grid.cursor.pos.col, Column(2));
        assert!(!cw.grid.cursor.should_wrap);
        // VS16 still attached as combining mark to the base cell.
        let extras_id = cw.grid[row][Column(0)].extras_id();
        assert!(extras_id.is_some());
    }

    #[test]
    fn vs16_on_non_emoji_base_leaves_cell_narrow() {
        use crate::performer::handler::Handler;
        let mut cw = new_term(10, 3);
        cw.input('a');
        cw.input('\u{FE0F}');

        let row = Line(0);
        assert_eq!(cw.grid[row][Column(0)].c(), 'a');
        assert_eq!(cw.grid[row][Column(0)].wide(), Wide::Narrow);
        assert_eq!(cw.grid[row][Column(1)].wide(), Wide::Narrow);
        assert_eq!(cw.grid.cursor.pos.col, Column(1));
    }

    #[test]
    fn vs16_on_already_wide_emoji_is_noop() {
        use crate::performer::handler::Handler;
        let mut cw = new_term(10, 3);
        // 🤝 (U+1F91D, EAW=W, already wide).
        cw.input('\u{1F91D}');
        cw.input('\u{FE0F}');

        let row = Line(0);
        assert_eq!(cw.grid[row][Column(0)].wide(), Wide::Wide);
        assert_eq!(cw.grid[row][Column(1)].wide(), Wide::Spacer);
        assert_eq!(cw.grid.cursor.pos.col, Column(2));
    }

    #[test]
    fn vs15_narrows_default_emoji() {
        use crate::performer::handler::Handler;
        let mut cw = new_term(10, 3);
        // 👍 (U+1F44D THUMBS UP) defaults to emoji presentation. It is
        // listed in emoji-variation-sequences.txt with a VS15 narrowing
        // sequence (unlike e.g. 🤝 U+1F91D, which has no VS entry at all).
        cw.input('\u{1F44D}');
        cw.input('\u{FE0E}');

        let row = Line(0);
        assert_eq!(cw.grid[row][Column(0)].c(), '\u{1F44D}');
        assert_eq!(cw.grid[row][Column(0)].wide(), Wide::Narrow);
        assert_eq!(cw.grid[row][Column(1)].wide(), Wide::Narrow);
        assert_eq!(cw.grid.cursor.pos.col, Column(1));
        assert!(!cw.grid.cursor.should_wrap);
    }

    #[test]
    fn vs15_on_non_listed_emoji_is_noop() {
        use crate::performer::handler::Handler;
        let mut cw = new_term(10, 3);
        // 🤝 (U+1F91D) is default-emoji but has no VS entry, so VS15
        // cannot narrow it — the grid must keep the Wide+Spacer pair.
        cw.input('\u{1F91D}');
        cw.input('\u{FE0E}');

        let row = Line(0);
        assert_eq!(cw.grid[row][Column(0)].wide(), Wide::Wide);
        assert_eq!(cw.grid[row][Column(1)].wide(), Wide::Spacer);
        assert_eq!(cw.grid.cursor.pos.col, Column(2));
    }

    #[test]
    fn vs16_at_column_zero_noop() {
        use crate::performer::handler::Handler;
        let mut cw = new_term(10, 3);
        // VS16 with no preceding base must be ignored.
        cw.input('\u{FE0F}');
        let row = Line(0);
        assert_eq!(cw.grid[row][Column(0)].wide(), Wide::Narrow);
        assert_eq!(cw.grid.cursor.pos.col, Column(0));
    }

    /// ECH repairs its wide boundary and resets the row's soft wrap.
    #[test]
    fn erase_chars_repairs_pair_and_resets_wrapline() {
        use crate::performer::handler::Handler;
        let mut cw = new_term(4, 3);
        cw.input('a');
        cw.input('a');
        cw.input('\u{5BBD}'); // Wide at 2, Spacer at 3
        cw.input('b'); // wraps; WRAPLINE lands on the spacer at (0,3)
        assert!(cw.grid[Line(0)][Column(3)].wrapline());
        cw.goto(Line(0), Column(2));
        cw.erase_chars(Column(1)); // blank the lead; sweep repairs the spacer
        assert_eq!(cw.grid[Line(0)][Column(3)].wide(), Wide::Narrow);
        assert!(!cw.grid[Line(0)][Column(3)].wrapline());
        assert!(!cw.grid[Line(1)].wrap_continuation);
    }

    /// ED partial-row clears split pairs at the cursor boundary too.
    #[test]
    fn clear_screen_partial_rows_repair_seams() {
        use crate::performer::handler::Handler;
        // Above: cursor on the lead → clears [..=0], strands the spacer.
        let mut cw = new_term(6, 3);
        cw.input('\u{5BBD}'); // Wide at 0, Spacer at 1
        cw.goto(Line(0), Column(0));
        cw.clear_screen(ClearMode::Above);
        assert_eq!(cw.grid[Line(0)][Column(1)].wide(), Wide::Narrow);

        // Below: cursor on the spacer → clears [2..], strands the lead.
        let mut cw = new_term(6, 3);
        cw.input('a');
        cw.input('\u{5BBD}'); // Wide at 1, Spacer at 2
        cw.goto(Line(0), Column(2));
        cw.clear_screen(ClearMode::Below);
        assert_eq!(cw.grid[Line(0)][Column(1)].wide(), Wide::Narrow);
        assert_eq!(cw.grid[Line(0)][Column(1)].c(), '\0');
    }

    /// DCH dragging a row-final LeadingSpacer into the interior leaves
    /// meaningless padding; the sweep reverts it to narrow.
    #[test]
    fn delete_chars_normalizes_interior_leading_spacer() {
        use crate::performer::handler::Handler;
        let mut cw = new_term(4, 3);
        cw.input('a');
        cw.input('a');
        cw.input('a');
        cw.input('\u{5BBD}'); // no room: LeadingSpacer at (0,3), pair wraps
        assert_eq!(cw.grid[Line(0)][Column(3)].wide(), Wide::LeadingSpacer);
        cw.goto(Line(0), Column(0));
        cw.delete_chars(1); // LeadingSpacer shifts to col 2
        assert_eq!(cw.grid[Line(0)][Column(2)].wide(), Wide::Narrow);
    }

    /// ECH erasing only the spacer half of a wide pair must clear the
    /// stranded lead too, mirroring `write_cell`'s overwrite cleanup.
    #[test]
    fn erase_chars_repairs_split_wide_pair() {
        use crate::performer::handler::Handler;
        let mut cw = new_term(6, 2);
        cw.input('\u{5BBD}'); // 宽: Wide at 0, Spacer at 1
        cw.goto(Line(0), Column(1));
        cw.erase_chars(Column(1)); // erase just the spacer
        assert_eq!(cw.grid[Line(0)][Column(0)].wide(), Wide::Narrow);
        assert_eq!(cw.grid[Line(0)][Column(0)].c(), '\0');
        assert_eq!(cw.grid[Line(0)][Column(1)].wide(), Wide::Narrow);

        // And the mirror case: erase through the lead, strand the spacer.
        let mut cw = new_term(6, 2);
        cw.input('a');
        cw.input('\u{5BBD}'); // Wide at 1, Spacer at 2
        cw.goto(Line(0), Column(0));
        cw.erase_chars(Column(2)); // erases 'a' + the lead
        assert_eq!(cw.grid[Line(0)][Column(2)].wide(), Wide::Narrow);
        assert_eq!(cw.grid[Line(0)][Column(2)].c(), '\0');
    }

    /// DCH shifting a spacer into a position whose lead was deleted
    /// must not leave the orphan behind.
    #[test]
    fn delete_chars_repairs_orphaned_spacer() {
        use crate::performer::handler::Handler;
        let mut cw = new_term(6, 2);
        cw.input('a');
        cw.input('\u{5BBD}'); // a at 0, Wide at 1, Spacer at 2
        cw.goto(Line(0), Column(1));
        cw.delete_chars(1); // deletes the lead; spacer shifts to col 1
        assert_eq!(cw.grid[Line(0)][Column(1)].wide(), Wide::Narrow);
        assert_eq!(cw.grid[Line(0)][Column(1)].c(), '\0');
    }

    /// ICH pushing a wide pair so its lead lands on the last column
    /// (spacer pushed off the row) must clear the stranded lead.
    #[test]
    fn insert_blank_repairs_pair_pushed_off_the_edge() {
        use crate::performer::handler::Handler;
        let mut cw = new_term(4, 2);
        cw.goto(Line(0), Column(2));
        cw.input('\u{5BBD}'); // Wide at 2, Spacer at 3
        cw.goto(Line(0), Column(0));
        cw.insert_blank(1); // lead lands at 3, spacer falls off
        assert_eq!(cw.grid[Line(0)][Column(3)].wide(), Wide::Narrow);
        assert_eq!(cw.grid[Line(0)][Column(3)].c(), '\0');
    }

    /// EL to the left of the cursor erases the lead of a pair the
    /// cursor sits on; the spacer past the cleared range must go too.
    #[test]
    fn clear_line_left_repairs_stranded_spacer() {
        use crate::performer::handler::Handler;
        let mut cw = new_term(6, 2);
        cw.input('\u{5BBD}'); // Wide at 0, Spacer at 1
        cw.goto(Line(0), Column(0));
        cw.clear_line(LineClearMode::Left); // clears only col 0
        assert_eq!(cw.grid[Line(0)][Column(1)].wide(), Wide::Narrow);
        assert_eq!(cw.grid[Line(0)][Column(1)].c(), '\0');
    }

    /// EL retains the preceding row's spacer head, matching Ghostty.
    #[test]
    fn clear_line_preserves_preceding_spacer_head() {
        use crate::performer::handler::Handler;
        let mut cw = new_term(3, 3);
        cw.input('a');
        cw.input('a');
        cw.input('\u{5BBD}'); // no room: LeadingSpacer at (0,2), pair on row 1
        assert_eq!(cw.grid[Line(0)][Column(2)].wide(), Wide::LeadingSpacer);
        cw.goto(Line(1), Column(0));
        cw.clear_line(LineClearMode::All);
        assert_eq!(cw.grid[Line(0)][Column(2)].wide(), Wide::LeadingSpacer);
        assert_eq!(cw.grid[Line(1)][Column(0)].wide(), Wide::Narrow);
    }

    #[test]
    fn vs16_at_last_column_wraps_base_to_next_row() {
        use crate::performer::handler::Handler;
        // Width 3 so that a 1-cell base at col 2 has no room for a spacer.
        let mut cw = new_term(3, 3);
        cw.input('a');
        cw.input('a');
        cw.input('\u{1F39F}');
        // Cursor at col 2 with should_wrap=true after writing base in last col.
        assert!(cw.grid.cursor.should_wrap);
        cw.input('\u{FE0F}');

        // Old row's last cell is now a LeadingSpacer signalling that a wide
        // glyph continues on the wrapped line. The base char itself moves to
        // (1, 0) marked Wide, with a Spacer at (1, 1).
        assert_eq!(cw.grid[Line(0)][Column(0)].c(), 'a');
        assert_eq!(cw.grid[Line(0)][Column(1)].c(), 'a');
        assert_eq!(cw.grid[Line(0)][Column(2)].wide(), Wide::LeadingSpacer);

        assert_eq!(cw.grid[Line(1)][Column(0)].c(), '\u{1F39F}');
        assert_eq!(cw.grid[Line(1)][Column(0)].wide(), Wide::Wide);
        assert_eq!(cw.grid[Line(1)][Column(1)].wide(), Wide::Spacer);

        assert_eq!(cw.grid.cursor.pos.row, Line(1));
        assert_eq!(cw.grid.cursor.pos.col, Column(2));
        assert!(!cw.grid.cursor.should_wrap);
    }

    /// A combining mark aimed at a bg-only cell must be dropped: those
    /// cells store color where the extras id lives, so reading an id
    /// there would clone an unrelated slot (someone's hyperlink) onto
    /// the cell, and writing one back would corrupt the color.
    #[test]
    fn combining_mark_on_bg_only_cell_is_dropped() {
        use crate::performer::handler::Handler;
        let mut cw = new_term(6, 3);
        // Keep an early slot id live so a garbage id would hit it.
        let link = Hyperlink::new(None::<&str>, "https://x.example");
        cw.set_hyperlink(Some(link));
        cw.input('a');
        cw.set_hyperlink(None);
        // Colored bg-only blanks.
        cw.terminal_attribute(Attr::Background(crate::config::colors::AnsiColor::Spec(
            ColorRgb { r: 0, g: 0, b: 200 },
        )));
        cw.goto(Line(1), Column(0));
        cw.erase_chars(Column(3));
        let before = cw.grid[Line(1)][Column(0)];
        assert!(!matches!(
            before.content_tag(),
            crate::crosswords::square::ContentTag::Codepoint
        ));
        // Mark lands after the blank at column 0.
        cw.goto(Line(1), Column(1));
        cw.input('\u{301}');
        let after = cw.grid[Line(1)][Column(0)];
        assert_eq!(after, before, "bg-only cell must be untouched");
        assert!(!after.has_grapheme());
    }

    /// An OSC 8 hyperlink left open across an alt-screen switch must be
    /// re-interned into the alt grid's table: extras ids are
    /// table-local, and the seeded cursor template would otherwise
    /// stamp a foreign id onto every alt-screen cell.
    fn term_2027(cols: usize, rows: usize) -> Crosswords<VoidListener> {
        use crate::ansi::mode::PrivateMode;
        use crate::performer::handler::Handler;
        let mut cw = new_term(cols, rows);
        cw.set_private_mode(PrivateMode::new(2027));
        cw
    }

    fn extras_of(cw: &Crosswords<VoidListener>, line: i32, col: usize) -> Vec<char> {
        cw.grid[Line(line)][Column(col)]
            .extras_id()
            .and_then(|id| cw.grid.extras_table.get(id))
            .map(|e| e.zerowidth.clone())
            .unwrap_or_default()
    }

    /// ZWJ emoji occupy one two-cell slot under mode 2027 (GB11),
    /// and shatter into per-emoji cells without it.
    #[test]
    fn mode_2027_zwj_emoji_is_one_cluster() {
        use crate::performer::handler::Handler;
        let farmer = ['\u{1F9D1}', '\u{200D}', '\u{1F33E}'];

        let mut cw = term_2027(10, 2);
        for c in farmer {
            cw.input(c);
        }
        assert_eq!(cw.grid[Line(0)][Column(0)].c(), '\u{1F9D1}');
        assert_eq!(cw.grid[Line(0)][Column(0)].wide(), Wide::Wide);
        assert_eq!(cw.grid[Line(0)][Column(1)].wide(), Wide::Spacer);
        assert_eq!(extras_of(&cw, 0, 0), ['\u{200D}', '\u{1F33E}']);
        assert_eq!(cw.grid.cursor.pos.col, Column(2));

        // Legacy (consumer opt-out): the trailing emoji starts its
        // own pair.
        let mut cw = new_term(10, 2);
        cw.set_grapheme_clustering(false);
        for c in farmer {
            cw.input(c);
        }
        assert_eq!(cw.grid[Line(0)][Column(2)].c(), '\u{1F33E}');
        assert_eq!(cw.grid.cursor.pos.col, Column(4));
    }

    /// Regional indicators pair up (GB12/13): two RIs form one wide
    /// cluster, the third starts a new cell.
    #[test]
    fn mode_2027_regional_indicators_pair() {
        use crate::performer::handler::Handler;
        let mut cw = term_2027(10, 2);
        let ri = ['\u{1F1E7}', '\u{1F1F7}', '\u{1F1E6}'];
        for c in ri {
            cw.input(c);
        }
        // First pair: one wide cluster.
        assert_eq!(cw.grid[Line(0)][Column(0)].wide(), Wide::Wide);
        assert_eq!(extras_of(&cw, 0, 0), ['\u{1F1F7}']);
        // Third RI: fresh narrow cell (parity even at the boundary).
        assert_eq!(cw.grid[Line(0)][Column(2)].c(), '\u{1F1E6}');
        assert_eq!(cw.grid[Line(0)][Column(2)].wide(), Wide::Narrow);
    }

    /// Indic conjuncts join across the linker (GB9c) and the cluster
    /// goes wide the moment a second width-bearing codepoint joins.
    #[test]
    fn mode_2027_indic_conjunct_joins() {
        use crate::performer::handler::Handler;
        let mut cw = term_2027(10, 2);
        for c in ['\u{915}', '\u{94D}', '\u{937}'] {
            cw.input(c);
        }
        assert_eq!(cw.grid[Line(0)][Column(0)].c(), '\u{915}');
        assert_eq!(cw.grid[Line(0)][Column(0)].wide(), Wide::Wide);
        assert_eq!(extras_of(&cw, 0, 0), ['\u{94D}', '\u{937}']);
        assert_eq!(cw.grid.cursor.pos.col, Column(2));
    }

    /// A valid emoji variation sequence widens within the cluster; an
    /// invalid selector is ignored outright rather
    /// than attached like the legacy path does.
    #[test]
    fn mode_2027_variation_selectors() {
        use crate::performer::handler::Handler;
        let mut cw = term_2027(10, 2);
        cw.input('\u{1F39F}'); // text-presentation default, narrow
        assert_eq!(cw.grid[Line(0)][Column(0)].wide(), Wide::Narrow);
        cw.input('\u{FE0F}');
        assert_eq!(cw.grid[Line(0)][Column(0)].wide(), Wide::Wide);
        assert_eq!(extras_of(&cw, 0, 0), ['\u{FE0F}']);

        let mut cw = term_2027(10, 2);
        cw.input('a');
        cw.input('\u{FE0F}'); // 'a' is no emoji base: dropped entirely
        assert_eq!(extras_of(&cw, 0, 0), Vec::<char>::new());
        assert!(!cw.grid[Line(0)][Column(0)].has_grapheme());
        assert_eq!(cw.grid.cursor.pos.col, Column(1));
    }

    /// A skin-tone modifier (width-bearing Extend) joins its emoji
    /// without growing the already-wide cluster.
    #[test]
    fn mode_2027_skin_tone_joins() {
        use crate::performer::handler::Handler;
        let mut cw = term_2027(10, 2);
        cw.input('\u{1F44B}');
        cw.input('\u{1F3FB}');
        assert_eq!(cw.grid[Line(0)][Column(0)].wide(), Wide::Wide);
        assert_eq!(extras_of(&cw, 0, 0), ['\u{1F3FB}']);
        assert_eq!(cw.grid.cursor.pos.col, Column(2));
    }

    /// Plain combining marks behave as before: joined, still narrow.
    /// Plain text never joins anything.
    #[test]
    fn mode_2027_narrow_cases() {
        use crate::performer::handler::Handler;
        let mut cw = term_2027(10, 2);
        cw.input('e');
        cw.input('\u{301}');
        cw.input('x');
        assert_eq!(cw.grid[Line(0)][Column(0)].wide(), Wide::Narrow);
        assert_eq!(extras_of(&cw, 0, 0), ['\u{301}']);
        assert_eq!(cw.grid[Line(0)][Column(1)].c(), 'x');
    }

    /// A width-bearing continuation arriving when the narrow cluster
    /// sits at the last column widens it through the wrap dance:
    /// LeadingSpacer stays behind, the cluster moves to the next row.
    #[test]
    fn mode_2027_cluster_widens_across_row_edge() {
        use crate::performer::handler::Handler;
        let mut cw = term_2027(3, 3);
        cw.input('a');
        cw.input('a');
        cw.input('\u{1F1E7}'); // narrow RI at the last column
        cw.input('\u{1F1F7}'); // pairs: cluster must go wide → wraps
        assert_eq!(cw.grid[Line(0)][Column(2)].wide(), Wide::LeadingSpacer);
        assert_eq!(cw.grid[Line(1)][Column(0)].c(), '\u{1F1E7}');
        assert_eq!(cw.grid[Line(1)][Column(0)].wide(), Wide::Wide);
        assert_eq!(extras_of(&cw, 1, 0), ['\u{1F1F7}']);
    }

    /// The bulk decode path must route through the cluster machine:
    /// a ZWJ sequence arriving as one codepoint batch clusters the
    /// same as scalar input.
    #[test]
    fn mode_2027_bulk_codepoints_cluster() {
        use crate::performer::handler::Handler;
        let mut cw = term_2027(10, 2);
        cw.input_codepoints(&[0x1F9D1, 0x200D, 0x1F33E, 0x61]);
        assert_eq!(cw.grid[Line(0)][Column(0)].wide(), Wide::Wide);
        assert_eq!(extras_of(&cw, 0, 0), ['\u{200D}', '\u{1F33E}']);
        assert_eq!(cw.grid[Line(0)][Column(2)].c(), 'a');
        assert_eq!(cw.grid.cursor.pos.col, Column(3));
    }

    #[test]
    fn clustered_bulk_runs_match_scalar_input_across_wraps_and_chunks() {
        use crate::performer::handler::Handler;
        let alphabet = [
            0x61, 0xa9, 0xe9, 0x301, 0x600, 0xd4e, 0x110bd, 0x4e00, 0x20000, 0x10ffff,
            0x200d, 0xfe0f, 0xfe0e, 0x1f469, 0x1f33e, 0x1f1e7, 0x1f1f7, 0xd800, 0x110000,
        ];
        let mut seed = 42u64;
        let mut codepoints = vec![0xd4e, 0x20000, 0x20001, 0x61, 0x301, 0xe9, 0x62];
        for _ in 0..400 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            codepoints.push(alphabet[(seed >> 32) as usize % alphabet.len()]);
        }
        for cols in [1, 2, 3, 20] {
            for chunk in [1, 2, 7, 64] {
                let mut scalar = term_2027(cols, 3);
                let mut bulk = term_2027(cols, 3);
                for &cp in &codepoints {
                    scalar.input(char::from_u32(cp).unwrap_or('\u{fffd}'));
                }
                for run in codepoints.chunks(chunk) {
                    bulk.input_codepoints(run);
                }
                assert_eq!(bulk.grid.cursor.pos, scalar.grid.cursor.pos);
                assert_eq!(bulk.grid.cursor.should_wrap, scalar.grid.cursor.should_wrap);
                assert_eq!(bulk.grid.topmost_line(), scalar.grid.topmost_line());
                for line in scalar.grid.topmost_line().0..3 {
                    for col in 0..cols {
                        let a = bulk.grid[Line(line)][Column(col)];
                        let b = scalar.grid[Line(line)][Column(col)];
                        assert_eq!(
                            (a.c(), a.wide()),
                            (b.c(), b.wide()),
                            "cols={cols} chunk={chunk} cell={line},{col}"
                        );
                        assert_eq!(a.cell_flags(), b.cell_flags());
                        assert_eq!(
                            extras_of(&bulk, line, col),
                            extras_of(&scalar, line, col)
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn mixed_bulk_writes_match_scalar_over_existing_wide_cells() {
        use crate::performer::handler::Handler;

        let runs: Vec<Vec<u32>> = [
            "a中é日b界c",
            "中a日é界b",
            "é日a中\u{301}b界",
            "\u{600}中a日é界",
            "中a\u{1f469}\u{200d}\u{1f33e}é日",
            "a中é日\u{10eeee}b界",
            "é日a中\u{20000}b",
        ]
        .iter()
        .map(|s| s.chars().map(u32::from).collect())
        .collect();
        for cols in [1, 2, 3, 5, 8, 20, 120] {
            for clustering in [false, true] {
                for wrapping in [false, true] {
                    for chunk in [1, 2, 7, 64] {
                        let make = || {
                            let mut cw = new_term(cols, 3);
                            cw.mode.set(Mode::GRAPHEME_CLUSTER, clustering);
                            for _ in 0..cols * 2 {
                                cw.input('中');
                                cw.input('a');
                            }
                            cw.mode.set(Mode::LINE_WRAP, wrapping);
                            cw.terminal_attribute(Attr::Bold);
                            cw.set_hyperlink(Some(Hyperlink::new(
                                Some("mixed"),
                                "https://example.com/mixed",
                            )));
                            cw
                        };
                        let mut bulk = make();
                        let mut scalar = make();
                        for (edit, run) in runs.iter().cycle().take(21).enumerate() {
                            let line = Line((edit % 3) as i32);
                            let col = Column(edit % cols);
                            bulk.goto(line, col);
                            scalar.goto(line, col);
                            for &cp in run {
                                scalar.input(char::from_u32(cp).unwrap());
                            }
                            for part in run.chunks(chunk) {
                                bulk.input_codepoints(part);
                            }
                            assert_eq!(bulk.grid.cursor.pos, scalar.grid.cursor.pos);
                            assert_eq!(
                                bulk.grid.cursor.should_wrap,
                                scalar.grid.cursor.should_wrap
                            );
                            assert_eq!(
                                bulk.grid.topmost_line(),
                                scalar.grid.topmost_line()
                            );
                            for line in scalar.grid.topmost_line().0..3 {
                                let row = &bulk.grid[Line(line)];
                                for col in 0..cols {
                                    let a = row[Column(col)];
                                    let b = scalar.grid[Line(line)][Column(col)];
                                    assert_eq!(
                                        (a.c(), a.wide(), a.cell_flags(), a.style_id()),
                                        (b.c(), b.wide(), b.cell_flags(), b.style_id()),
                                        "cols={cols} clustering={clustering} wrapping={wrapping} chunk={chunk} edit={edit} cell={line},{col}"
                                    );
                                    assert_eq!(
                                        extras_of(&bulk, line, col),
                                        extras_of(&scalar, line, col)
                                    );
                                    assert_eq!(
                                        bulk.cell_hyperlink(Line(line), Column(col)),
                                        scalar.cell_hyperlink(Line(line), Column(col))
                                    );
                                    assert!(a.extras_id().is_none() || row.has_extras);
                                    assert!(a.style_id() == 0 || row.has_styles);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Reflow moves a cluster atomically: the wide pair and its
    /// attached codepoints survive shrink-rewrap and grow-unwrap.
    #[test]
    fn mode_2027_cluster_survives_reflow() {
        use crate::performer::handler::Handler;
        let mut cw = term_2027(6, 3);
        cw.input('a');
        cw.input('b');
        for c in ['\u{1F9D1}', '\u{200D}', '\u{1F33E}'] {
            cw.input(c);
        }

        // Shrink: the cluster no longer fits after "ab" and rewraps.
        cw.resize(CrosswordsSize::new(3, 3));
        let mut found = None;
        for line in cw.grid.topmost_line().0..3 {
            for col in 0..3 {
                let cell = cw.grid[Line(line)][Column(col)];
                if cell.c() == '\u{1F9D1}' {
                    found = Some((line, col));
                }
            }
        }
        let (line, col) = found.expect("cluster base survives shrink");
        assert_eq!(cw.grid[Line(line)][Column(col)].wide(), Wide::Wide);
        assert_eq!(extras_of(&cw, line, col), ['\u{200D}', '\u{1F33E}']);

        // Grow back: still one intact cluster.
        cw.resize(CrosswordsSize::new(6, 3));
        let mut found = None;
        for line in cw.grid.topmost_line().0..3 {
            for col in 0..6 {
                let cell = cw.grid[Line(line)][Column(col)];
                if cell.c() == '\u{1F9D1}' {
                    found = Some((line, col));
                }
            }
        }
        let (line, col) = found.expect("cluster base survives grow");
        assert_eq!(cw.grid[Line(line)][Column(col)].wide(), Wide::Wide);
        assert_eq!(extras_of(&cw, line, col), ['\u{200D}', '\u{1F33E}']);

        // The moved cells' rows must keep the extras hint, or reclaim
        // would sweep the live slot out from under them.
        cw.grid.reclaim_extras();
        assert_eq!(extras_of(&cw, line, col), ['\u{200D}', '\u{1F33E}']);
    }

    /// Copy/serialize emits the full cluster text, and replaying that
    /// text into a fresh mode-2027 terminal reproduces the same cells.
    #[test]
    fn mode_2027_cluster_round_trips_through_text() {
        use crate::performer::handler::Handler;
        let mut cw = term_2027(10, 2);
        cw.input('a');
        for c in ['\u{1F9D1}', '\u{200D}', '\u{1F33E}'] {
            cw.input(c);
        }
        cw.input('b');

        let text = cw
            .bounds_to_string(Pos::new(Line(0), Column(0)), Pos::new(Line(0), Column(4)));
        assert_eq!(text, "a\u{1F9D1}\u{200D}\u{1F33E}b");

        let mut replay = term_2027(10, 2);
        for c in text.chars() {
            replay.input(c);
        }
        assert_eq!(replay.grid[Line(0)][Column(1)].wide(), Wide::Wide);
        assert_eq!(extras_of(&replay, 0, 1), ['\u{200D}', '\u{1F33E}']);
        assert_eq!(replay.grid[Line(0)][Column(3)].c(), 'b');
    }

    #[test]
    fn hyperlink_ends_before_copying_the_alt_cursor() {
        use crate::performer::handler::Handler;
        let mut cw = new_term(6, 3);
        let link = Hyperlink::new(None::<&str>, "https://alt.example");
        cw.set_hyperlink(Some(link.clone()));
        cw.swap_alt();
        cw.input('x');
        assert!(cw.cell_hyperlink(Line(0), Column(0)).is_none());
        cw.swap_alt();
    }

    fn partial_damage_lines(cw: &mut Crosswords<VoidListener>) -> Vec<usize> {
        match cw.damage() {
            TermDamage::Full => panic!("expected partial damage"),
            TermDamage::Partial(iter) => {
                let mut lines: Vec<usize> = iter.map(|d| d.line).collect();
                lines.sort_unstable();
                lines
            }
        }
    }

    /// A combining mark arriving in its own damage window (its base was
    /// painted and the damage consumed) must damage the base's row: the
    /// attach mutates a painted cell. This is the mechanism behind the
    /// 0.5.20 "stale glyphs until you scroll" report.
    #[test]
    fn cluster_attach_records_damage() {
        use crate::performer::handler::Handler;
        let mut cw = term_2027(8, 3);
        cw.input('e');
        cw.reset_damage(); // renderer consumed the frame with the base

        cw.input('\u{0301}');
        assert_eq!(extras_of(&cw, 0, 0), ['\u{0301}']);
        assert_eq!(partial_damage_lines(&mut cw), [0]);
    }

    /// Same contract on the legacy wcwidth path (clustering opted out):
    /// the zero-width attach predates mode 2027 and had the same gap.
    #[test]
    fn legacy_zero_width_attach_records_damage() {
        use crate::performer::handler::Handler;
        let mut cw = new_term(8, 3);
        cw.set_grapheme_clustering(false);
        cw.input('e');
        cw.reset_damage();

        cw.input('\u{0301}');
        assert_eq!(extras_of(&cw, 0, 0), ['\u{0301}']);
        assert_eq!(partial_damage_lines(&mut cw), [0]);
    }

    /// A VS16 that widens the cluster damages the row through both the
    /// widen and the attach; the selector must also land in the extras.
    #[test]
    fn vs16_widen_records_damage() {
        use crate::performer::handler::Handler;
        let mut cw = term_2027(8, 3);
        cw.input('\u{2764}'); // text-default heart, narrow
        cw.reset_damage();

        cw.input('\u{FE0F}');
        assert_eq!(cw.grid[Line(0)][Column(0)].wide(), Wide::Wide);
        assert_eq!(extras_of(&cw, 0, 0), ['\u{FE0F}']);
        assert!(partial_damage_lines(&mut cw).contains(&0));
    }

    /// Damage soundness: every row whose visible content changed since
    /// the last reset must be covered by the damage the terminal
    /// reports. Feeds randomized VT streams (clusters, wide chars,
    /// erases, scrolls, alt screen) and diffs full row snapshots. Bare
    /// continuation codepoints are separate chunks deliberately: an
    /// attach that lands in its own damage window is exactly the
    /// "stale glyphs until you scroll" class of bug.
    #[test]
    fn damage_covers_every_content_change() {
        use crate::performer::handler::Processor;

        const COLS: usize = 40;
        const ROWS: usize = 12;

        fn row_snapshot(
            cw: &Crosswords<VoidListener>,
        ) -> Vec<Vec<(char, u8, Vec<char>)>> {
            (0..ROWS)
                .map(|r| {
                    (0..COLS)
                        .map(|c| {
                            let sq = cw.grid[Line(r as i32)][Column(c)];
                            let zw = sq
                                .extras_id()
                                .and_then(|id| cw.grid.extras_table.get(id))
                                .map(|e| e.zerowidth.clone())
                                .unwrap_or_default();
                            (sq.c(), sq.wide() as u8, zw)
                        })
                        .collect()
                })
                .collect()
        }

        fn damaged_rows(cw: &mut Crosswords<VoidListener>) -> Vec<bool> {
            match cw.damage() {
                TermDamage::Full => vec![true; ROWS],
                TermDamage::Partial(iter) => {
                    let mut out = vec![false; ROWS];
                    for d in iter {
                        if d.line < ROWS {
                            out[d.line] = true;
                        }
                    }
                    out
                }
            }
        }

        // Deterministic LCG so failures reproduce.
        let mut state: u64 = 0x2027_5EED;
        let mut rng = move |n: usize| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((state >> 33) as usize) % n
        };

        let chunks: &[&[u8]] = &[
            b"hello world 12345",
            b"\x1b[H",
            b"\x1b[3;7H5",
            b"\r\n",
            "你好世界".as_bytes(),
            "🧑\u{200D}🌾".as_bytes(),
            "e\u{301}".as_bytes(),
            "❤\u{FE0F}".as_bytes(),
            "👍🏻x".as_bytes(),
            // Bare continuations: attach to whatever the previous step
            // left at the cursor, inside a fresh damage window.
            "\u{301}".as_bytes(),
            "\u{200D}🌾".as_bytes(),
            "\u{FE0F}".as_bytes(),
            "🏻".as_bytes(),
            b"\x1b[K",
            b"\x1b[2K",
            b"\x1b[J",
            b"\x1b[2J",
            b"\x1b[4X",
            b"\x1b[3@",
            b"\x1b[2P",
            b"\x1b[2L",
            b"\x1b[2M",
            b"\x1b[3;9r\x1b[2S",
            b"\x1b[2T\x1b[r",
            b"\x1b[?1049h",
            b"\x1b[?1049l",
            b"\x1b[3b",
            b"\x1b[10;20Hmid",
        ];

        let mut cw: Crosswords<VoidListener> = Crosswords::new(
            CrosswordsSize::new(COLS, ROWS),
            CursorShape::Block,
            VoidListener,
            WindowId::from(0),
            0,
            50,
        );
        let mut parser = Processor::default();
        cw.reset_damage();
        let mut prev = row_snapshot(&cw);

        for step in 0..800 {
            for _ in 0..(1 + rng(3)) {
                let chunk = chunks[rng(chunks.len())];
                parser.advance(&mut cw, chunk);
            }
            let damaged = damaged_rows(&mut cw);
            cw.reset_damage();
            let now = row_snapshot(&cw);
            for r in 0..ROWS {
                if now[r] != prev[r] {
                    assert!(
                        damaged[r],
                        "step {step}: row {r} content changed without damage"
                    );
                }
            }
            prev = now;
        }
    }

    fn scrolled_back_term() -> Crosswords<VoidListener> {
        use crate::crosswords::grid::Scroll;
        use crate::performer::handler::Handler;
        // Small scrollback cap so the tests can sit at the boundary.
        let mut cw = Crosswords::new(
            CrosswordsSize::new(10, 6),
            CursorShape::Block,
            VoidListener {},
            crate::event::WindowId::from(0),
            0,
            40,
        );
        // Overfill scrollback so history sits at its cap.
        for i in 0..300 {
            cw.input((b'a' + (i % 26) as u8) as char);
            cw.linefeed();
            cw.carriage_return();
        }
        cw.scroll_display(Scroll::Top);
        assert!(cw.display_offset() > 0);
        cw.reset_damage();
        cw
    }

    /// A sub-region scroll (IL/DL, scroll region not starting at the
    /// top) adds nothing to history, so it must not move a viewport
    /// pinned into scrollback. The drifted offset made
    /// `compute_index` resolve visible lines to wrong storage slots:
    /// arbitrary rows painted at arbitrary positions.
    #[test]
    fn sub_region_scroll_keeps_display_offset() {
        use crate::performer::handler::Handler;
        let mut cw = scrolled_back_term();
        let offset = cw.display_offset();

        // DL and IL at a mid-screen cursor line.
        cw.grid.cursor.pos = Pos::new(Line(2), Column(0));
        cw.delete_lines(2);
        assert_eq!(cw.display_offset(), offset, "DL moved the offset");
        cw.insert_blank_lines(2);
        assert_eq!(cw.display_offset(), offset, "IL moved the offset");
        assert!(
            cw.display_offset() <= cw.history_size(),
            "offset past available history"
        );
    }

    /// A top-anchored region scroll grows history; a viewport pinned
    /// in scrollback absorbs it (same absolute rows stay visible), so
    /// only the region lines are damaged, not the whole screen.
    #[test]
    fn pinned_viewport_absorbs_history_scroll() {
        use crate::performer::handler::Handler;
        let mut cw = new_term(10, 6);
        for _ in 0..20 {
            cw.linefeed();
        }
        cw.scroll_display(crate::crosswords::grid::Scroll::Delta(5));
        assert_eq!(cw.display_offset(), 5);
        cw.reset_damage();

        // Region 0..5 with a fixed bottom line: rows rotate into
        // history and offset grows in lockstep while under the cap.
        cw.set_scrolling_region(1, Some(5));
        cw.grid.cursor.pos = Pos::new(Line(4), Column(0));
        cw.linefeed();
        assert_eq!(cw.display_offset(), 6, "pin did not absorb the scroll");
        assert!(cw.display_offset() <= cw.history_size());
        assert!(
            !matches!(cw.peek_damage_event(), Some(TerminalDamage::Full)),
            "absorbed scroll needs no full repaint"
        );
    }

    /// At the history cap the pinned offset clamps and the whole
    /// scrolled-back view slides one row: every visible row changes
    /// while region damage only covers active-area lines. That frame
    /// must be full damage or the view keeps stale rows.
    #[test]
    fn pinned_viewport_slide_at_cap_is_full_damage() {
        use crate::performer::handler::Handler;
        let mut cw = scrolled_back_term();
        let cap = cw.history_size();
        assert_eq!(cw.display_offset(), cap, "expected offset pinned at cap");

        cw.set_scrolling_region(1, Some(5));
        cw.grid.cursor.pos = Pos::new(Line(4), Column(0));
        cw.linefeed();
        assert_eq!(cw.history_size(), cap, "history can no longer grow");
        assert_eq!(cw.display_offset(), cap, "offset must clamp at the cap");
        assert!(
            matches!(cw.peek_damage_event(), Some(TerminalDamage::Full)),
            "slid view must be fully damaged"
        );
    }

    /// Growing the viewport pulls rows from history; the offset must
    /// track the rows actually pulled and never exceed what remains.
    #[test]
    fn resize_clamps_pinned_display_offset() {
        let mut cw = scrolled_back_term();
        cw.resize(CrosswordsSize::new(10, 9));
        assert!(
            cw.display_offset() <= cw.history_size(),
            "grow_lines left offset {} past history {}",
            cw.display_offset(),
            cw.history_size()
        );
        cw.resize(CrosswordsSize::new(10, 4));
        assert!(cw.display_offset() <= cw.history_size());
        cw.resize(CrosswordsSize::new(7, 8));
        assert!(cw.display_offset() <= cw.history_size());
    }

    /// Consuming a frame records the cursor position it carried:
    /// without that, every subsequent peek reports CursorOnly and
    /// each PTY drain fires a redraw event forever.
    #[test]
    fn reset_damage_quiesces_cursor_events() {
        use crate::performer::handler::Handler;
        let mut cw = new_term(10, 4);
        cw.reset_damage();
        assert!(cw.peek_damage_event().is_none());

        cw.input('x');
        assert!(cw.peek_damage_event().is_some());
        cw.reset_damage();
        assert!(
            cw.peek_damage_event().is_none(),
            "consumed frame keeps re-reporting the cursor"
        );
    }

    /// Mode 2027: a zero-width codepoint with no base to join (orphan
    /// at column 0) is dropped, never legacy-attached: the mode just
    /// computed a boundary and wcwidth-attaching would contradict it.
    #[test]
    fn mode_2027_orphan_zero_width_dropped() {
        use crate::ansi::mode::PrivateMode;
        use crate::performer::handler::Handler;
        let mut cw = new_term(6, 3);
        cw.set_private_mode(PrivateMode::new(2027));
        cw.input('\u{0301}');
        let cell = cw.grid[Line(0)][Column(0)];
        assert_eq!(cell.c(), '\0');
        assert!(cell.extras_id().is_none());
        assert_eq!(cw.grid.cursor.pos.col, Column(0));
    }

    /// Mode 2027: variation-selector validity is judged against the
    /// cluster's *last* codepoint, not its base: a selector modifies
    /// the character immediately before it. U+261D is a valid VS16
    /// base but the
    /// skin tone that joined after it is not, so the trailing VS16 is
    /// ignored outright.
    #[test]
    fn mode_2027_vs_validated_against_last_codepoint() {
        use crate::ansi::mode::PrivateMode;
        use crate::performer::handler::Handler;
        let mut cw = new_term(8, 3);
        cw.set_private_mode(PrivateMode::new(2027));
        cw.input('\u{261D}'); // ☝ narrow, valid VS16 base
        cw.input('\u{1F3FB}'); // skin tone: Extend, width 2 → cluster goes wide
        let col_after_modifier = cw.grid.cursor.pos.col;
        cw.input('\u{FE0F}'); // last cp is the skin tone → invalid → ignored

        let cell = cw.grid[Line(0)][Column(0)];
        assert_eq!(cell.c(), '\u{261D}');
        assert_eq!(cell.wide(), Wide::Wide);
        let extras = cw.grid.extras_table.get(cell.extras_id().unwrap()).unwrap();
        assert_eq!(extras.zerowidth, vec!['\u{1F3FB}']);
        assert_eq!(cw.grid.cursor.pos.col, col_after_modifier);
    }

    /// DEC private mode 2027 (grapheme cluster processing): set,
    /// reset, DECRQM visibility, RIS, and cross-screen sharing. The
    /// mode is plumbing-complete here; segmentation behavior arrives
    /// with the cluster input path.
    #[test]
    fn mode_2027_plumbing() {
        use crate::ansi::mode::{NamedPrivateMode, PrivateMode};
        use crate::performer::handler::Handler;
        let mut cw = new_term(6, 3);

        // Default: set (the consumer knob below configures it).
        assert!(cw.mode().contains(Mode::GRAPHEME_CLUSTER));

        // DECRST / DECSET round-trip: the program wins at runtime.
        cw.unset_private_mode(PrivateMode::new(2027));
        assert!(!cw.mode().contains(Mode::GRAPHEME_CLUSTER));
        cw.set_private_mode(PrivateMode::new(2027));
        assert!(cw.mode().contains(Mode::GRAPHEME_CLUSTER));

        // 2027 resolves to the named mode, not Unknown.
        assert!(matches!(
            PrivateMode::new(2027),
            PrivateMode::Named(NamedPrivateMode::GraphemeCluster)
        ));

        // Shared across screens: the mode lives on the terminal.
        cw.set_private_mode(PrivateMode::new(2027));
        cw.swap_alt();
        assert!(cw.mode().contains(Mode::GRAPHEME_CLUSTER));
        cw.swap_alt();

        // RIS restores the configured default: on out of the box,
        // whatever a program had DECRST'd...
        cw.unset_private_mode(PrivateMode::new(2027));
        cw.reset_state();
        assert!(cw.mode().contains(Mode::GRAPHEME_CLUSTER));

        // ...and off for a consumer that opted out, surviving both a
        // program's DECSET and the RIS after it.
        cw.set_grapheme_clustering(false);
        assert!(!cw.mode().contains(Mode::GRAPHEME_CLUSTER));
        cw.set_private_mode(PrivateMode::new(2027));
        assert!(cw.mode().contains(Mode::GRAPHEME_CLUSTER));
        cw.reset_state();
        assert!(!cw.mode().contains(Mode::GRAPHEME_CLUSTER));
    }

    /// Identical extras content interns into one slot: the u16 id
    /// space stops being a per-occurrence budget.
    #[test]
    fn extras_intern_by_content() {
        use crate::performer::handler::Handler;
        let mut cw = new_term(10, 2);
        cw.input('e');
        cw.input('\u{301}');
        cw.input('e');
        cw.input('\u{301}');
        let a = cw.grid[Line(0)][Column(0)].extras_id().unwrap();
        let b = cw.grid[Line(0)][Column(1)].extras_id().unwrap();
        assert_eq!(a, b);
        // Different content gets its own slot.
        cw.input('e');
        cw.input('\u{302}');
        let c = cw.grid[Line(0)][Column(2)].extras_id().unwrap();
        assert_ne!(a, c);
    }

    /// A combining mark typed inside an OSC 8 hyperlink must attach to
    /// its own cell only. The old in-place mutation edited the shared
    /// template slot, leaking the mark into every cell of the span —
    /// and the marked cell must keep the hyperlink.
    #[test]
    fn combining_mark_inside_hyperlink_stays_on_one_cell() {
        use crate::performer::handler::Handler;
        let mut cw = new_term(10, 2);
        let link = Hyperlink::new(None::<&str>, "https://example.com");
        cw.set_hyperlink(Some(link.clone()));
        cw.input('a');
        cw.input('\u{301}');
        cw.input('b');
        cw.set_hyperlink(None);

        // The marked cell carries mark + hyperlink.
        let a_id = cw.grid[Line(0)][Column(0)].extras_id().unwrap();
        let a = cw.grid.extras_table.get(a_id).unwrap();
        assert_eq!(a.zerowidth, vec!['\u{301}']);
        assert_eq!(a.hyperlink.as_ref(), Some(&link));

        // Its neighbor keeps the pristine hyperlink-only slot.
        let b_id = cw.grid[Line(0)][Column(1)].extras_id().unwrap();
        let b = cw.grid.extras_table.get(b_id).unwrap();
        assert!(b.zerowidth.is_empty());
        assert_eq!(b.hyperlink.as_ref(), Some(&link));
        assert_ne!(a_id, b_id);
    }

    /// The interning lookup must be purged when slots are swept, or a
    /// re-alloc of old content would return a dead slot id.
    #[test]
    fn reclaim_purges_interning_lookup() {
        use crate::performer::handler::Handler;
        let mut cw = new_term(10, 2);
        cw.input('e');
        cw.input('\u{301}');
        let content = cw
            .grid
            .extras_table
            .get(cw.grid[Line(0)][Column(0)].extras_id().unwrap())
            .unwrap()
            .clone();
        // Drop the only reference and sweep.
        cw.clear_screen(ClearMode::All);
        cw.grid.reclaim_extras();
        // Re-alloc of the same content must yield a live slot.
        let id = cw.grid.alloc_extras(content.clone());
        assert_ne!(id, 0);
        assert_eq!(cw.grid.extras_table.get(id), Some(&content));
    }

    /// A width-bearing continuation joining a cluster whose base sits
    /// at the last column widens through the row edge: the base moves
    /// to the next row as a wide pair and its earlier extras move with
    /// it.
    #[test]
    fn cluster_widen_at_last_column_preserves_base_extras() {
        use crate::performer::handler::Handler;
        let mut cw = term_2027(3, 3);
        cw.input('a');
        cw.input('a');
        cw.input('\u{261D}'); // narrow index-up at the last column
        cw.input('\u{0301}'); // combining mark joins the cluster
        assert_eq!(extras_of(&cw, 0, 2), ['\u{0301}']);

        cw.input('\u{1F3FB}'); // skin tone: width-bearing joiner, widens

        assert_eq!(cw.grid[Line(0)][Column(2)].wide(), Wide::LeadingSpacer);
        assert_eq!(cw.grid[Line(1)][Column(0)].c(), '\u{261D}');
        assert_eq!(cw.grid[Line(1)][Column(0)].wide(), Wide::Wide);
        assert_eq!(
            extras_of(&cw, 1, 0),
            ['\u{0301}', '\u{1F3FB}'],
            "extras must ride the wrap with their base"
        );
        assert_eq!(cw.grid[Line(1)][Column(1)].wide(), Wide::Spacer);
    }

    /// The measurement API must agree with what printing lays out:
    /// for a cluster printed into a fresh cell under mode 2027,
    /// `cluster_width` predicts both the codepoint count the cell
    /// absorbs (base plus extras) and its final cell width. Embedders
    /// rely on this to size cells without replaying the input path.
    #[test]
    fn cluster_width_matches_printed_layout() {
        use crate::grapheme_lut::cluster_width;
        use crate::performer::handler::Handler;
        let corpus: &[&[u32]] = &[
            &[0x61],                     // plain narrow
            &[0x4E00],                   // plain wide
            &[0x65, 0x301],              // combining mark
            &[0x2764, 0xFE0F],           // VS16 widens
            &[0x231A, 0xFE0E],           // VS15 narrows
            &[0x31, 0xFE0F, 0x20E3],     // keycap
            &[0x1F468, 0x200D, 0x1F33E], // ZWJ farmer
            &[0x1F44D, 0x1F3FB],         // skin tone
            &[0x1F1E7, 0x1F1F7],         // flag pair
            &[0x1F39F, 0xFE0F],          // text-default emoji + VS16
        ];
        for cps in corpus {
            let mut cw = term_2027(10, 3);
            for &cp in *cps {
                cw.input(char::from_u32(cp).unwrap());
            }
            let (len, width) = cluster_width(cps);
            assert_eq!(len, cps.len(), "one whole cluster: {cps:04X?}");
            assert_eq!(
                1 + extras_of(&cw, 0, 0).len(),
                len,
                "printed codepoint count: {cps:04X?}"
            );
            let printed = match cw.grid[Line(0)][Column(0)].wide() {
                Wide::Wide => 2,
                _ => 1,
            };
            assert_eq!(width, printed, "printed cell width: {cps:04X?}");
        }
    }

    /// Randomized version of the above: for arbitrary sequences over
    /// an alphabet of joiners, selectors, modifiers and bases, print
    /// exactly the codepoints `cluster_width` claims form the first
    /// cluster and demand the grid agrees on both the cell width and
    /// the cursor advance. Catches width-rule divergence the curated
    /// corpus misses (selectors after joiners, stacked modifiers).
    #[test]
    fn cluster_width_matches_printed_layout_randomized() {
        use crate::grapheme_lut::cluster_width;
        use crate::performer::handler::Handler;
        let alphabet: &[u32] = &[
            0x61, 0x31, 0x4E00, 0x301, 0x200D, 0xFE0F, 0xFE0E, 0x20E3, 0x1F468, 0x1F469,
            0x1F33E, 0x1F3FB, 0x1F1E7, 0x1F1F7, 0x2764, 0x231A, 0x1F39F, 0x2620, 0x1F3F4,
        ];
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = |bound: usize| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((seed >> 33) as usize) % bound
        };
        for _ in 0..2000 {
            let cps: Vec<u32> = (0..1 + next(7))
                .map(|_| alphabet[next(alphabet.len())])
                .collect();
            // A cluster only owns a cell when its base takes one.
            if crate::codepoint_width::codepoint_width(cps[0]).unwrap_or(0) == 0 {
                continue;
            }
            let (len, width) = cluster_width(&cps);
            let mut cw = term_2027(20, 4);
            for &cp in &cps[..len] {
                cw.input(char::from_u32(cp).unwrap());
            }
            let printed = match cw.grid[Line(0)][Column(0)].wide() {
                Wide::Wide => 2,
                _ => 1,
            };
            assert_eq!(width, printed, "cell width for {cps:04X?}");
            assert_eq!(
                cw.grid.cursor.pos.col.0, width as usize,
                "cursor advance for {cps:04X?}"
            );
        }
    }

    /// Legacy widths: a selector after a non-emoji base is presentation
    /// noise and is dropped rather than attached, so copy/serialize and
    /// the extras table never carry it.
    #[test]
    fn legacy_variation_selector_dropped_off_emoji_base() {
        use crate::performer::handler::Handler;
        let mut cw = new_term(10, 3);
        cw.set_grapheme_clustering(false);

        cw.input('a');
        cw.input('\u{FE0F}');
        assert!(cw.grid[Line(0)][Column(0)].extras_id().is_none());
        assert_eq!(cw.grid[Line(0)][Column(0)].wide(), Wide::Narrow);

        // Ordinary combining marks still attach to anything.
        cw.input('\u{0301}');
        assert_eq!(extras_of(&cw, 0, 0), ['\u{0301}']);
    }

    /// Legacy widths (clustering opted out): variation selectors attach
    /// as combining data and never change the base cell's width, pure
    /// wcwidth semantics. A width the application cannot predict
    /// desyncs every wcwidth-based redraw (tmux most of all).
    #[test]
    fn legacy_variation_selectors_never_change_width() {
        use crate::performer::handler::Handler;
        let mut cw = new_term(10, 3);
        cw.set_grapheme_clustering(false);

        // Text-default emoji + VS16: stays narrow, selector attached.
        cw.input('\u{1F39F}');
        cw.input('\u{FE0F}');
        assert_eq!(cw.grid[Line(0)][Column(0)].wide(), Wide::Narrow);
        assert_eq!(extras_of(&cw, 0, 0), ['\u{FE0F}']);
        assert_eq!(cw.grid.cursor.pos.col, Column(1));

        // Emoji-default wide char + VS15: stays wide, selector attached.
        cw.input('\u{231A}');
        cw.input('\u{FE0E}');
        assert_eq!(cw.grid[Line(0)][Column(1)].wide(), Wide::Wide);
        assert_eq!(extras_of(&cw, 0, 1), ['\u{FE0E}']);
        assert_eq!(cw.grid.cursor.pos.col, Column(3));
    }

    #[test]
    fn vs16_then_following_char_does_not_overlap() {
        use crate::performer::handler::Handler;
        // Reproduces the original vim-split-misalignment scenario: after
        // widening the text-presentation emoji, the next character must
        // land *past* the spacer, not on top of it.
        let mut cw = new_term(10, 3);
        cw.input('"');
        cw.input('\u{1F39F}');
        cw.input('\u{FE0F}');
        cw.input('"');

        let row = Line(0);
        assert_eq!(cw.grid[row][Column(0)].c(), '"');
        assert_eq!(cw.grid[row][Column(1)].c(), '\u{1F39F}');
        assert_eq!(cw.grid[row][Column(1)].wide(), Wide::Wide);
        assert_eq!(cw.grid[row][Column(2)].wide(), Wide::Spacer);
        assert_eq!(cw.grid[row][Column(3)].c(), '"');
        assert_eq!(cw.grid.cursor.pos.col, Column(4));
    }

    /// End-to-end: feed rio the exact byte sequence `kitten icat
    /// --unicode-placeholder` emits and verify the grid ends up with the
    /// expected U+10EEEE cells, fg colors, and diacritics. Reproduces
    /// the wire protocol from kitty's `kittens/icat/transmit.go:221`
    /// (`write_unicode_placeholder`) — `\e[38:2:R:G:Bm` foreground +
    /// per-cell `<U+10EEEE><row diac><col diac><high diac>` payload.
    #[test]
    fn icat_unicode_placeholder_wire_sequence_lands_in_grid() {
        use crate::ansi::kitty_virtual::{DIACRITICS, PLACEHOLDER};

        let size = CrosswordsSize::new(40, 20);
        let columns = size.columns;
        let window_id = crate::event::WindowId::from(0);
        let mut cw = Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener {},
            window_id,
            0,
            10_000,
        );
        let mut processor = crate::performer::handler::Processor::default();

        // 32-bit image_id with high byte non-zero (matches icat's
        // rejection-sample loop in `transmit.go:280-288`).
        let image_id: u32 = 0x0201_0203;
        let high = (image_id >> 24) & 0xFF;
        let r = (image_id >> 16) & 0xFF;
        let g = (image_id >> 8) & 0xFF;
        let b = image_id & 0xFF;

        // 1) Transmit a 1×1 RGBA pixel under the chosen image_id (we
        // don't care about the pixel data — we just need an entry in
        // `kitty_images` so the renderer's existence check passes).
        // base64("\xFF\x00\x00\xFF") = "/wAA/w==".
        let xmit = format!("\x1b_Gf=32,a=t,i={image_id},s=1,v=1;/wAA/w==\x1b\\");
        processor.advance(&mut cw, xmit.as_bytes());

        // 2) Register the virtual placement: 4 cols × 2 rows.
        let cols = 4u32;
        let rows = 2u32;
        let place = format!("\x1b_Ga=p,U=1,i={image_id},c={cols},r={rows},q=2\x1b\\");
        processor.advance(&mut cw, place.as_bytes());

        // 3) Emit the placeholder cells themselves (what icat writes
        // after the placement APC). Set fg via colon-separated SGR,
        // write `<U+10EEEE><row><col><high>` per cell.
        let id_high_diac = DIACRITICS[high as usize];
        let mut cells = format!("\x1b[38:2:{r}:{g}:{b}m");
        for (row, &row_diac) in DIACRITICS.iter().enumerate().take(rows as usize) {
            for &col_diac in DIACRITICS.iter().take(cols as usize) {
                cells.push(PLACEHOLDER);
                cells.push(row_diac);
                cells.push(col_diac);
                cells.push(id_high_diac);
            }
            if row < rows as usize - 1 {
                cells.push_str("\n\r");
            }
        }
        cells.push_str("\x1b[39m");
        processor.advance(&mut cw, cells.as_bytes());

        // Metadata stored.
        let vp = crate::ansi::kitty_virtual::resolve_virtual_placement(
            &cw.graphics.kitty_virtual_placements,
            image_id,
            0,
        )
        .expect("virtual placement registered");
        assert_eq!(vp.columns, cols);
        assert_eq!(vp.rows, rows);

        // Image transmitted.
        assert!(
            cw.graphics.get_kitty_image(image_id).is_some(),
            "image was not stored under id {image_id:#X}"
        );

        // Each cell of the placement carries U+10EEEE + the right fg
        // RGB + the right two diacritics (zerowidth). The third
        // diacritic encodes image_id_high.
        let extras = cw.grid.extras_table.clone();
        let _ = columns;
        for (row, &row_diac) in DIACRITICS.iter().enumerate().take(rows as usize) {
            for (col, &col_diac) in DIACRITICS.iter().enumerate().take(cols as usize) {
                let sq = cw.grid[Line(row as i32)][Column(col)];
                assert_eq!(
                    sq.c(),
                    PLACEHOLDER,
                    "expected U+10EEEE at ({row},{col}), got {:#X}",
                    sq.c() as u32
                );
                let style = cw.grid.style_of(&sq);
                match style.fg {
                    crate::config::colors::AnsiColor::Spec(rgb) => {
                        assert_eq!(rgb.r as u32, r);
                        assert_eq!(rgb.g as u32, g);
                        assert_eq!(rgb.b as u32, b);
                    }
                    other => panic!("expected RGB fg at ({row},{col}), got {other:?}"),
                }
                let zw = sq
                    .extras_id()
                    .and_then(|id| extras.get(id))
                    .map(|e| e.zerowidth.as_slice())
                    .unwrap_or(&[]);
                assert_eq!(
                    zw.len(),
                    3,
                    "expected 3 diacritics at ({row},{col}), got {}",
                    zw.len()
                );
                assert_eq!(zw[0], row_diac, "row diacritic mismatch at ({row},{col})");
                assert_eq!(zw[1], col_diac, "col diacritic mismatch at ({row},{col})");
                assert_eq!(
                    zw[2], id_high_diac,
                    "high diacritic mismatch at ({row},{col})"
                );
            }
        }

        // Per-row dirty flag: rows that received placeholder cells must
        // have it set; other rows must not.
        for row in 0..(rows as i32) {
            assert!(
                cw.grid[Line(row)].kitty_virtual_placeholder,
                "row {row} should have kitty_virtual_placeholder = true",
            );
        }
        // A row past the placement (no placeholder cells written there).
        let past = rows as i32;
        if past < cw.grid.screen_lines() as i32 {
            assert!(
                !cw.grid[Line(past)].kitty_virtual_placeholder,
                "row {past} (no placeholders) must not have the flag set",
            );
        }
    }

    /// End-to-end: yazi's kgp driver (yazi-adapter/src/drivers/kgp.rs)
    /// transmits with `a=T,C=1,U=1` and NO `c=`/`r=` grid params, sets
    /// the fg via semicolon-form RGB, and writes 2 diacritics per
    /// placeholder cell. The placement must register and the implied
    /// grid must come from the image's pixel size, not collapse to a
    /// 1×1 cell box (the 0.4.12 yazi-preview regression from #1314).
    #[test]
    #[allow(clippy::needless_range_loop)]
    fn yazi_kgp_wire_sequence_implies_grid_from_image_size() {
        use crate::ansi::kitty_virtual::{DIACRITICS, PLACEHOLDER};

        let size = CrosswordsSize::new(40, 20);
        let window_id = crate::event::WindowId::from(0);
        let mut cw = Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener {},
            window_id,
            0,
            10_000,
        );
        let mut processor = crate::performer::handler::Processor::default();

        // yazi: id = pid % 0xffffff → 24 bits, no high-byte diacritic.
        let image_id: u32 = 0x0A12BF;
        let r = (image_id >> 16) & 0xFF;
        let g = (image_id >> 8) & 0xFF;
        let b = image_id & 0xFF;

        // Combined transmit+display, virtual, no c=/r=. 2×2 RGBA image
        // (base64 of 16 bytes of 0xFF).
        let xmit = format!(
            "\x1b_Gq=2,a=T,C=1,U=1,f=32,s=2,v=2,i={image_id},m=0;/////////////////////w==\x1b\\"
        );
        processor.advance(&mut cw, xmit.as_bytes());

        // Placeholder grid 4 cols × 2 rows, absolute cursor moves per
        // row, semicolon-form RGB fg carrying the image id.
        let cols = 4usize;
        let rows = 2usize;
        let mut cells = format!("\x1b[38;2;{r};{g};{b}m");
        for row in 0..rows {
            cells.push_str(&format!("\x1b[{};1H", row + 1));
            for col in 0..cols {
                cells.push(PLACEHOLDER);
                cells.push(DIACRITICS[row]);
                cells.push(DIACRITICS[col]);
            }
        }
        cells.push_str("\x1b[0m");
        processor.advance(&mut cw, cells.as_bytes());

        let vp = crate::ansi::kitty_virtual::resolve_virtual_placement(
            &cw.graphics.kitty_virtual_placements,
            image_id,
            0,
        )
        .expect("a=T,U=1 must register a virtual placement");
        assert_eq!((vp.columns, vp.rows), (0, 0), "c=/r= omitted on the wire");
        assert!(
            cw.graphics.get_kitty_image(image_id).is_some(),
            "image stored under id {image_id:#X}"
        );

        // Placeholder cells landed with both diacritics.
        let extras = cw.grid.extras_table.clone();
        for row in 0..rows {
            let sq = cw.grid[Line(row as i32)][Column(0)];
            assert_eq!(sq.c(), PLACEHOLDER);
            let zw = sq
                .extras_id()
                .and_then(|id| extras.get(id))
                .map(|e| e.zerowidth.as_slice())
                .unwrap_or(&[]);
            assert_eq!(zw, &[DIACRITICS[row], DIACRITICS[0]]);
        }

        // A 2x2 px image on 1x2 px cells occupies two columns and one row, so
        // the first placeholder run uses the image's full vertical extent.
        let run = crate::ansi::kitty_virtual::PlaceholderRun {
            image_id,
            placement_id: 0,
            row: 0,
            col: 0,
            width: cols as u32,
        };
        let geom = crate::ansi::kitty_virtual::compute_run_geometry(
            &run,
            vp.columns,
            vp.rows,
            2,
            2,
            (vp.x, vp.y, vp.width, vp.height),
            1.0,
            2.0,
            0.0,
            0.0,
            0,
            0,
        )
        .expect("implied-grid run must be drawable");
        assert_eq!(geom.width, 2.0);
        assert_eq!(geom.height, 2.0);
        assert_eq!(geom.source_rect, [0.0, 0.0, 1.0, 1.0]);
    }

    /// `place_virtual_graphic` is the handler for `_Ga=p,U=1,…\e\` — the
    /// virtual-placement registration used by `kitten icat
    /// --unicode-placeholder`. Per the kitty protocol spec the command
    /// only stores metadata; the application emits the U+10EEEE
    /// placeholder cells itself as ordinary text afterwards. This test
    /// pins that contract: previously rio also auto-wrote the cells,
    /// which raced kitty's own writes and broke the rendering.
    /// `a=d,d=i` must remove virtual (U=1) placements, while `d=a`
    /// ("all visible placements") leaves them alone as kitty does.
    #[test]
    fn delete_removes_virtual_placements() {
        use crate::ansi::kitty_graphics_protocol::{DeleteRequest, PlacementRequest};

        let size = CrosswordsSize::new(40, 20);
        let window_id = crate::event::WindowId::from(0);
        let mut cw = Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener {},
            window_id,
            0,
            10_000,
        );

        let store = |cw: &mut Crosswords<VoidListener>, id: u32| {
            cw.graphics.store_kitty_image(
                id,
                None,
                GraphicData {
                    id: rio_graphics::GraphicId::new(id as u64),
                    width: 1,
                    height: 1,
                    pixels: vec![0u8; 4],
                    color_type: rio_graphics::ColorType::Rgba,
                    is_opaque: true,
                    display_width: None,
                    display_height: None,
                    resize: None,
                    transmit_time: crate::time::Instant::now(),
                },
            );
        };
        let place = |cw: &mut Crosswords<VoidListener>, id: u32, pid: u32| {
            assert!(cw.place_graphic(PlacementRequest {
                relative: None,
                image_id: id,
                placement_id: pid,
                x: 0,
                y: 0,
                width: 0,
                height: 0,
                columns: 2,
                rows: 1,
                z_index: 0,
                virtual_placement: true,
                unicode_placeholder: 0,
                cursor_movement: 0,
                cell_x_offset: 0,
                cell_y_offset: 0,
            }));
        };
        let delete = |action: u8, image_id: u32, placement_id: u32| DeleteRequest {
            frame_number: 0,
            action,
            image_id,
            image_number: 0,
            placement_id,
            x: 0,
            y: 0,
            z_index: 0,
            delete_data: false,
        };

        store(&mut cw, 1);
        store(&mut cw, 2);
        place(&mut cw, 1, 5);
        place(&mut cw, 1, 6);
        place(&mut cw, 2, 5);
        assert_eq!(cw.graphics.kitty_virtual_placements.len(), 3);

        // By id and placement: only (1, 5) goes.
        cw.delete_graphics(delete(b'i', 1, 5));
        assert_eq!(cw.graphics.kitty_virtual_placements.len(), 2);
        assert!(!cw.graphics.kitty_virtual_placements.contains_key(&(1, 5)));

        // By id: the rest of image 1 goes, image 2 stays.
        cw.delete_graphics(delete(b'i', 1, 0));
        assert_eq!(cw.graphics.kitty_virtual_placements.len(), 1);
        assert!(cw.graphics.kitty_virtual_placements.contains_key(&(2, 5)));

        // Delete all visible placements: virtual ones are not visible
        // by themselves and stay.
        cw.delete_graphics(delete(b'a', 0, 0));
        assert!(cw.graphics.kitty_virtual_placements.contains_key(&(2, 5)));
    }

    #[test]
    fn place_virtual_graphic_stores_metadata_without_writing_cells() {
        use crate::ansi::kitty_graphics_protocol::PlacementRequest;

        let size = CrosswordsSize::new(40, 20);
        let columns = size.columns;
        let screen_lines = size.screen_lines as i32;
        let window_id = crate::event::WindowId::from(0);
        let mut cw = Crosswords::new(
            size,
            CursorShape::Block,
            VoidListener {},
            window_id,
            0,
            10_000,
        );

        // Virtual placements reference a transmitted image like any
        // other `a=p`; placing an unknown id is ENOENT.
        cw.graphics.store_kitty_image(
            1234,
            None,
            GraphicData {
                id: rio_graphics::GraphicId::new(1234),
                width: 1,
                height: 1,
                pixels: vec![0u8; 4],
                color_type: rio_graphics::ColorType::Rgba,
                is_opaque: true,
                display_width: None,
                display_height: None,
                resize: None,
                transmit_time: crate::time::Instant::now(),
            },
        );

        let cursor_before = cw.grid.cursor.pos;
        let placement = PlacementRequest {
            relative: None,
            image_id: 1234,
            placement_id: 0,
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            columns: 8,
            rows: 4,
            z_index: 0,
            virtual_placement: true,
            unicode_placeholder: 0,
            cursor_movement: 0,
            cell_x_offset: 0,
            cell_y_offset: 0,
        };

        assert!(cw.place_graphic(placement), "image exists, must place");

        // Metadata stored …
        let vp = crate::ansi::kitty_virtual::resolve_virtual_placement(
            &cw.graphics.kitty_virtual_placements,
            1234,
            0,
        )
        .expect("virtual placement registered");
        assert_eq!(vp.image_id, 1234);
        assert_eq!(vp.columns, 8);
        assert_eq!(vp.rows, 4);

        // … but no placeholder cells written. The application is
        // responsible for emitting U+10EEEE itself; the terminal must not
        // double-write.
        for line in 0..screen_lines {
            for col in 0..columns {
                let cp = cw.grid[Line(line)][Column(col)].c();
                assert_ne!(
                    cp,
                    crate::ansi::kitty_virtual::PLACEHOLDER,
                    "unexpected U+10EEEE cell at ({line},{col})"
                );
            }
        }

        // Cursor must be untouched.
        assert_eq!(cw.grid.cursor.pos, cursor_before);
    }

    fn kitty_test_image(id: u32, width: usize, height: usize) -> GraphicData {
        GraphicData {
            id: rio_graphics::GraphicId::new(id as u64),
            width,
            height,
            pixels: vec![0u8; width * height * 4],
            color_type: rio_graphics::ColorType::Rgba,
            is_opaque: true,
            display_width: None,
            display_height: None,
            resize: None,
            transmit_time: crate::time::Instant::now(),
        }
    }

    fn virtual_request(
        image_id: u32,
        placement_id: u32,
    ) -> crate::ansi::kitty_graphics_protocol::PlacementRequest {
        crate::ansi::kitty_graphics_protocol::PlacementRequest {
            relative: None,
            image_id,
            placement_id,
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            columns: 8,
            rows: 4,
            z_index: 0,
            virtual_placement: true,
            unicode_placeholder: 0,
            cursor_movement: 0,
            cell_x_offset: 0,
            cell_y_offset: 0,
        }
    }

    fn uploaded(cw: &Crosswords<VoidListener>, image_id: u32) -> bool {
        cw.graphics.is_kitty_uploaded(image_id)
    }

    #[test]
    fn virtual_placement_uploads_pixels_once_per_image() {
        let mut cw = make_crosswords();
        // Unknown image: nothing to upload.
        assert!(!cw.ensure_kitty_upload(1234));

        // `a=t` stores without uploading; the first `a=p,U=1` queues
        // the pixels, a re-place (relayout) must not resend them.
        cw.store_graphic(kitty_test_image(1234, 1, 1));
        assert!(!uploaded(&cw, 1234));
        assert!(cw.place_graphic(virtual_request(1234, 7)));
        assert!(uploaded(&cw, 1234));
        assert!(!cw.ensure_kitty_upload(1234));
        assert!(cw.place_graphic(virtual_request(1234, 7)));
        assert!(cw.place_graphic(virtual_request(1234, 8)));
        assert!(!cw.ensure_kitty_upload(1234));

        // Deleting the placements but keeping the data leaves the
        // texture valid, so placing again does not resend either.
        cw.graphics.kitty_virtual_placements.clear();
        assert!(cw.place_graphic(virtual_request(1234, 7)));
        assert!(!cw.ensure_kitty_upload(1234));

        // Retransmission removes the old placements. The next placement
        // uploads the replacement pixels once.
        cw.store_graphic(kitty_test_image(1234, 2, 2));
        assert!(!uploaded(&cw, 1234));
        assert!(cw.graphics.kitty_virtual_placements.is_empty());
        assert!(cw.place_graphic(virtual_request(1234, 7)));
        assert!(uploaded(&cw, 1234));
        assert!(!cw.ensure_kitty_upload(1234));

        // A retransmission of an unplaced id waits for the placement.
        cw.graphics.kitty_virtual_placements.clear();
        cw.store_graphic(kitty_test_image(1234, 3, 3));
        assert!(!uploaded(&cw, 1234));
        assert!(cw.ensure_kitty_upload(1234));
    }

    #[test]
    fn transmit_and_display_virtual_uploads_once() {
        let mut cw = make_crosswords();
        cw.kitty_transmit_and_display(kitty_test_image(5, 2, 2), virtual_request(5, 1));
        assert!(uploaded(&cw, 5));
        assert!(cw.graphics.kitty_virtual_placements.contains_key(&(5, 1)));
        assert!(!cw.ensure_kitty_upload(5));
    }

    #[test]
    fn direct_placement_shares_texture_with_virtual_placement() {
        use crate::ansi::kitty_graphics_protocol::PlacementRequest;
        let mut cw = make_crosswords();
        cw.graphics.cell_width = 10.0;
        cw.graphics.cell_height = 20.0;
        cw.store_graphic(kitty_test_image(3, 10, 20));
        cw.place_graphic(PlacementRequest {
            relative: None,
            virtual_placement: false,
            columns: 0,
            rows: 0,
            ..virtual_request(3, 1)
        });
        assert!(uploaded(&cw, 3));
        assert!(!cw.ensure_kitty_upload(3));
        cw.place_graphic(virtual_request(3, 2));
        assert!(uploaded(&cw, 3));
    }

    #[test]
    fn alt_screen_swap_reuploads_images_whose_texture_was_overwritten() {
        let mut cw = make_crosswords();
        cw.store_graphic(kitty_test_image(1, 1, 1));
        cw.place_graphic(virtual_request(1, 1));
        cw.store_graphic(kitty_test_image(2, 1, 1));
        cw.place_graphic(virtual_request(2, 1));
        assert!(uploaded(&cw, 1) && uploaded(&cw, 2));

        // The alt screen transmits its own image 1, overwriting the
        // window-wide texture, and a never-uploaded image 2.
        cw.swap_alt();
        assert!(cw.graphics.get_kitty_image(1).is_none());
        cw.store_graphic(kitty_test_image(1, 2, 2));
        cw.place_graphic(virtual_request(1, 1));
        cw.store_graphic(kitty_test_image(2, 2, 2));
        assert!(uploaded(&cw, 1) && !uploaded(&cw, 2));

        // Back on the main screen: image 1 must be resent, image 2's
        // texture was never touched so it is still current.
        assert!(cw.graphics.swap_kitty_screen_state());
        let queued: Vec<u32> = cw
            .graphics
            .pending_images
            .iter()
            .map(|(id, _)| *id)
            .collect();
        assert_eq!(queued, vec![1]);
        assert_eq!(cw.graphics.pending_images[0].1.width, 1);
        assert!(uploaded(&cw, 1) && uploaded(&cw, 2));
        cw.graphics.pending_images.clear();

        // And the alt screen's image 1 is stale again when it returns;
        // its unplaced image 2 is not resent.
        assert!(cw.graphics.swap_kitty_screen_state());
        let queued: Vec<u32> = cw
            .graphics
            .pending_images
            .iter()
            .map(|(id, _)| *id)
            .collect();
        assert_eq!(queued, vec![1]);
        assert_eq!(cw.graphics.pending_images[0].1.width, 2);
        assert!(!uploaded(&cw, 2));
        cw.graphics.pending_images.clear();
        cw.graphics.kitty_virtual_placements.clear();
        cw.graphics
            .kitty_inactive_screen
            .kitty_virtual_placements
            .clear();

        // Without a placement on the incoming screen nothing is resent;
        // the record still mismatches, so a later placement uploads.
        assert!(!cw.graphics.swap_kitty_screen_state());
        assert!(cw.graphics.pending_images.is_empty());
        assert!(!uploaded(&cw, 1));
        assert!(cw.ensure_kitty_upload(1));
    }

    #[test]
    fn alt_screen_delete_still_invalidates_main_texture() {
        use crate::ansi::kitty_graphics_protocol::DeleteRequest;
        let mut cw = make_crosswords();
        cw.store_graphic(kitty_test_image(1, 1, 1));
        cw.place_graphic(virtual_request(1, 1));
        assert!(uploaded(&cw, 1));

        // The alt screen uploads its own image 1, then deletes it with
        // its data before switching back; the texture still holds the
        // alt pixels.
        cw.swap_alt();
        cw.store_graphic(kitty_test_image(1, 2, 2));
        cw.place_graphic(virtual_request(1, 1));
        cw.delete_graphics(DeleteRequest {
            frame_number: 0,
            action: b'I',
            delete_data: true,
            image_id: 1,
            image_number: 0,
            placement_id: 0,
            x: 0,
            y: 0,
            z_index: 0,
        });
        assert!(cw.graphics.get_kitty_image(1).is_none());

        assert!(cw.graphics.swap_kitty_screen_state());
        assert_eq!(cw.graphics.pending_images.len(), 1);
        assert_eq!(cw.graphics.pending_images[0].1.width, 1);
        assert!(uploaded(&cw, 1));
    }

    #[test]
    fn alt_screen_retransmit_without_placement_invalidates_main_texture() {
        let mut cw = make_crosswords();
        cw.store_graphic(kitty_test_image(1, 1, 1));
        cw.place_graphic(virtual_request(1, 1));

        // Alt uploads image 1, drops its placements, then retransmits
        // without placing: the texture holds the first alt pixels.
        cw.swap_alt();
        cw.store_graphic(kitty_test_image(1, 2, 2));
        cw.place_graphic(virtual_request(1, 1));
        cw.graphics.kitty_virtual_placements.clear();
        cw.store_graphic(kitty_test_image(1, 3, 3));
        assert!(!uploaded(&cw, 1));

        assert!(cw.graphics.swap_kitty_screen_state());
        assert_eq!(cw.graphics.pending_images.len(), 1);
        assert_eq!(cw.graphics.pending_images[0].1.width, 1);
        assert!(uploaded(&cw, 1));
    }

    #[test]
    fn cursor_delete_with_data_keeps_virtual_and_unplaced_images() {
        use crate::ansi::kitty_graphics_protocol::DeleteRequest;
        let mut cw = make_crosswords();
        cw.store_graphic(kitty_test_image(1, 1, 1));
        cw.place_graphic(virtual_request(1, 1));
        cw.store_graphic(kitty_test_image(2, 1, 1));
        cw.delete_graphics(DeleteRequest {
            frame_number: 0,
            action: b'C',
            delete_data: true,
            image_id: 0,
            image_number: 0,
            placement_id: 0,
            x: 0,
            y: 0,
            z_index: 0,
        });
        assert!(cw.graphics.get_kitty_image(1).is_some());
        assert!(cw.graphics.get_kitty_image(2).is_some());
    }

    #[test]
    fn evicting_inactive_same_id_image_keeps_active_texture() {
        let mut cw = make_crosswords();
        cw.graphics.total_limit = 100_000;
        cw.store_graphic(kitty_test_image(1, 100, 100));
        cw.place_graphic(virtual_request(1, 1));
        assert!(uploaded(&cw, 1));

        cw.swap_alt();
        cw.store_graphic(kitty_test_image(1, 100, 100));
        cw.swap_alt();
        assert!(uploaded(&cw, 1));

        // Storing image 2 has to evict; the inactive image 1 goes first.
        // The texture holds the active copy, so it is neither freed nor
        // resent.
        cw.store_graphic(kitty_test_image(2, 100, 100));
        assert!(cw.graphics.kitty_inactive_screen.kitty_images.is_empty());
        assert!(cw.graphics.get_kitty_image(1).is_some());
        assert!(uploaded(&cw, 1));
        assert!(cw
            .graphics
            .texture_operations
            .lock()
            .iter()
            .all(|key| *key != rio_graphics::kitty_image_key(1)));
    }

    #[test]
    fn delete_all_keeps_virtual_placements_and_their_images() {
        use crate::ansi::kitty_graphics_protocol::{DeleteRequest, PlacementRequest};
        let mut cw = make_crosswords();
        cw.graphics.cell_width = 10.0;
        cw.graphics.cell_height = 20.0;
        cw.store_graphic(kitty_test_image(1, 10, 20));
        cw.place_graphic(virtual_request(1, 1));
        cw.store_graphic(kitty_test_image(2, 10, 20));
        cw.place_graphic(PlacementRequest {
            relative: None,
            virtual_placement: false,
            columns: 0,
            rows: 0,
            ..virtual_request(2, 1)
        });
        assert_eq!(cw.graphics.kitty_placements.len(), 1);

        cw.delete_graphics(DeleteRequest {
            frame_number: 0,
            action: b'A',
            delete_data: true,
            image_id: 0,
            image_number: 0,
            placement_id: 0,
            x: 0,
            y: 0,
            z_index: 0,
        });
        assert!(cw.graphics.kitty_placements.is_empty());
        assert!(cw.graphics.kitty_virtual_placements.contains_key(&(1, 1)));
        assert!(cw.graphics.get_kitty_image(1).is_some());
        assert!(cw.graphics.get_kitty_image(2).is_none());
    }

    #[test]
    fn delete_range_covers_virtual_placements() {
        use crate::ansi::kitty_graphics_protocol::DeleteRequest;
        let mut cw = make_crosswords();
        cw.store_graphic(kitty_test_image(5, 1, 1));
        cw.place_graphic(virtual_request(5, 1));
        cw.store_graphic(kitty_test_image(9, 1, 1));
        cw.place_graphic(virtual_request(9, 1));

        cw.delete_graphics(DeleteRequest {
            frame_number: 0,
            action: b'R',
            delete_data: true,
            image_id: 0,
            image_number: 0,
            placement_id: 0,
            x: 4,
            y: 6,
            z_index: 0,
        });
        assert!(!cw.graphics.kitty_virtual_placements.contains_key(&(5, 1)));
        assert!(cw.graphics.get_kitty_image(5).is_none());
        assert!(cw.graphics.kitty_virtual_placements.contains_key(&(9, 1)));
        assert!(cw.graphics.get_kitty_image(9).is_some());
    }

    #[test]
    fn deleting_image_data_sweeps_its_placements() {
        let mut cw = make_crosswords();
        cw.store_graphic(kitty_test_image(1, 1, 1));
        cw.place_graphic(virtual_request(1, 1));
        cw.graphics.delete_kitty_images(|id, _| *id == 1);
        assert!(cw.graphics.kitty_virtual_placements.is_empty());
    }

    #[test]
    fn retransmission_removes_all_virtual_placements() {
        let mut cw = make_crosswords();
        cw.store_graphic(kitty_test_image(9, 100, 100));
        let mut req = virtual_request(9, 1);
        req.x = 50;
        req.y = 10;
        req.width = 20;
        req.height = 20;
        cw.place_graphic(req);
        let mut req = virtual_request(9, 2);
        req.x = 5;
        req.width = 10;
        cw.place_graphic(req);

        cw.store_graphic(kitty_test_image(9, 20, 20));
        assert!(cw.graphics.kitty_virtual_placements.is_empty());
        assert!(cw.graphics.get_kitty_image(9).is_some());
    }

    /// DECSTBM bounds where scrolling happens, not what is on screen. Both
    /// shapes matter: `1;N` only shrinks `end`, so a fix that just zeroed
    /// `start` would pass that case while still dropping the bottom rows.
    #[test]
    fn visible_rows_ignore_the_scroll_region() {
        use crate::performer::handler::Processor;
        fn term_with_lines(rows: usize) -> (Crosswords<VoidListener>, Processor) {
            let mut term = Crosswords::new(
                CrosswordsSize::new(20, rows),
                CursorShape::Block,
                VoidListener,
                crate::event::WindowId::from(0),
                0,
                100,
            );
            let mut parser = Processor::default();
            for i in 0..rows {
                parser
                    .advance(&mut term, format!("\x1b[{};1Hline{}", i + 1, i).as_bytes());
            }
            (term, parser)
        }

        let first_row_text = |term: &Crosswords<VoidListener>| -> String {
            let rows = term.visible_rows();
            (0..5).map(|c| rows[0][Column(c)].c()).collect()
        };

        // Region narrowed at both ends: full height, no vertical shift.
        let (mut term, mut parser) = term_with_lines(10);
        parser.advance(&mut term, b"\x1b[2;8r");
        assert_eq!(term.screen_lines(), 10);
        assert_eq!(term.visible_rows().len(), 10);
        assert_eq!(first_row_text(&term), "line0");

        // Region narrowed at the bottom only, as vim does around its status
        // line: the last row must still be reported.
        let (mut term, mut parser) = term_with_lines(10);
        parser.advance(&mut term, b"\x1b[1;9r");
        assert_eq!(term.visible_rows().len(), 10);
        assert_eq!(first_row_text(&term), "line0");

        // Same on the alternate screen.
        let (mut term, mut parser) = term_with_lines(6);
        parser.advance(&mut term, b"\x1b[?1049h\x1b[1;3r");
        assert_eq!(term.visible_rows().len(), 6);

        // Scrolled into history, the viewport still spans the screen height
        // and starts one line up per scrolled line.
        let (mut term, mut parser) = term_with_lines(4);
        for i in 0..4 {
            parser.advance(&mut term, format!("\r\nscroll{i}").as_bytes());
        }
        parser.advance(&mut term, b"\x1b[2;3r");
        term.scroll_display(crate::crosswords::grid::Scroll::Delta(2));
        assert_eq!(term.display_offset(), 2);
        assert_eq!(term.visible_rows().len(), 4);
    }

    /// The flag byte an embedder needs to encode keys is the one the terminal
    /// would report; it must survive set, push and pop.
    #[test]
    fn keyboard_mode_reports_the_active_flags() {
        use crate::performer::handler::Processor;
        let mut term = Crosswords::new(
            CrosswordsSize::new(10, 3),
            CursorShape::Block,
            VoidListener,
            crate::event::WindowId::from(0),
            0,
            10,
        );
        let mut parser = Processor::default();
        assert_eq!(term.keyboard_mode(), KeyboardModes::NO_MODE);

        // CSI = 5 ; 1 u -> disambiguate + report alternate keys.
        parser.advance(&mut term, b"\x1b[=5;1u");
        assert_eq!(
            term.keyboard_mode(),
            KeyboardModes::DISAMBIGUATE_ESC_CODES | KeyboardModes::REPORT_ALTERNATE_KEYS
        );

        // Pushing a new mode shadows it, popping restores it.
        parser.advance(&mut term, b"\x1b[>1u");
        assert_eq!(term.keyboard_mode(), KeyboardModes::DISAMBIGUATE_ESC_CODES);
        parser.advance(&mut term, b"\x1b[<1u");
        assert_eq!(
            term.keyboard_mode(),
            KeyboardModes::DISAMBIGUATE_ESC_CODES | KeyboardModes::REPORT_ALTERNATE_KEYS
        );
    }

    /// `c()` alone drops combining marks, which is the trap `cell_text`
    /// exists to close. Also covers the bg-only guard: those cells reuse the
    /// extras-id bits for colour, so an unguarded lookup reads a channel as
    /// an id.
    #[test]
    fn cell_text_includes_zero_width_marks() {
        use crate::performer::handler::Processor;
        let mut term = Crosswords::new(
            CrosswordsSize::new(10, 2),
            CursorShape::Block,
            VoidListener,
            crate::event::WindowId::from(0),
            0,
            10,
        );
        let mut parser = Processor::default();
        // "e" + U+0301 COMBINING ACUTE, then "X".
        parser.advance(&mut term, "e\u{0301}X".as_bytes());

        let cell = Pos::new(Line(0), Column(0));
        assert_eq!(term.grid[cell].c(), 'e', "base char only");
        assert_eq!(
            term.grid.cell_text(cell).collect::<String>(),
            "e\u{0301}",
            "cell_text keeps the mark"
        );
        assert_eq!(
            term.grid
                .cell_text(Pos::new(Line(0), Column(1)))
                .collect::<String>(),
            "X"
        );

        // A background-only cell has no text and must not read its colour
        // bits as an extras id.
        let bg = Pos::new(Line(1), Column(0));
        term.grid[bg].set_bg_rgb(0xAA, 0xBB, 0xCC);
        assert!(term.grid[bg].is_bg_only());
        assert_eq!(
            term.grid.cell_text(bg).collect::<String>(),
            "\0",
            "bg-only cell is blank, with no marks"
        );
    }

    /// modifyOtherKeys has no Mode bit, so without this an embedder has to
    /// sniff raw PTY bytes outside the parser to find it.
    #[test]
    fn modify_other_keys_tracks_the_level() {
        use crate::performer::handler::Processor;
        let mut term = Crosswords::new(
            CrosswordsSize::new(10, 2),
            CursorShape::Block,
            VoidListener,
            crate::event::WindowId::from(0),
            0,
            10,
        );
        let mut parser = Processor::default();
        assert_eq!(term.modify_other_keys(), None);

        parser.advance(&mut term, b"\x1b[>4;2m");
        assert_eq!(term.modify_other_keys(), Some(2));

        parser.advance(&mut term, b"\x1b[>4;1m");
        assert_eq!(term.modify_other_keys(), Some(1));

        // No level, and an explicit 0, both disable it.
        parser.advance(&mut term, b"\x1b[>4m");
        assert_eq!(term.modify_other_keys(), None);
        parser.advance(&mut term, b"\x1b[>4;2m\x1b[>4;0m");
        assert_eq!(term.modify_other_keys(), None);

        // Another resource must not be mistaken for modifyOtherKeys, and an
        // out-of-range level must not reach an embedder as a level it cannot
        // interpret.
        parser.advance(&mut term, b"\x1b[>2;2m");
        assert_eq!(term.modify_other_keys(), None);
        parser.advance(&mut term, b"\x1b[>4;7m");
        assert_eq!(term.modify_other_keys(), None);

        // RIS clears it, like the other keyboard state.
        parser.advance(&mut term, b"\x1b[>4;2m");
        assert_eq!(term.modify_other_keys(), Some(2));
        parser.advance(&mut term, b"\x1bc");
        assert_eq!(term.modify_other_keys(), None);
    }
    /// A grid one column wide cannot hold a wide pair. Every path that can
    /// place or preserve one has to cope: the per-cell write, the bulk wide
    /// run, the VS16 promotion that widens an already-written cell, and the
    /// reflow that shrinks a grid under existing wide content. Before this
    /// was handled the first two panicked out of bounds and the last looped
    /// forever, growing the new row buffer without bound.
    #[test]
    fn one_column_grid_handles_wide_glyphs() {
        use crate::performer::handler::Processor;

        let feed = |cols: usize, bytes: &[u8]| {
            let mut term = Crosswords::new(
                CrosswordsSize::new(cols, 3),
                CursorShape::Block,
                VoidListener,
                crate::event::WindowId::from(0),
                0,
                100,
            );
            Processor::default().advance(&mut term, bytes);
            term
        };

        // The reported repro: one wide char, one column. The glyph cannot be
        // represented, so it is dropped for a blank narrow cell.
        let term = feed(1, "你".as_bytes());
        let cell = term.grid[Pos::new(Line(0), Column(0))];
        assert!(!cell.is_wide(), "nothing to pair a wide cell with");
        assert!(!cell.is_spacer(), "and no orphaned Spacer either");
        assert_eq!(cell.c(), ' ');

        for cols in [1, 2] {
            feed(cols, "你好世界".as_bytes());
            feed(cols, "🦀".as_bytes());
            feed(cols, "\u{2764}\u{FE0F}".as_bytes());
            feed(cols, "你\u{0301}".as_bytes());
            feed(cols, "你\r\n好\r\n世".as_bytes());
            feed(cols, "\x1b[?1049h你好".as_bytes());
            feed(cols, "\x1b[1;2r你好".as_bytes());
        }

        // Shrinking under existing wide content, then writing, then growing.
        // Reflowing to one column destroys the wide chars, so no row should
        // come out of it still claiming to be wide or a Spacer.
        let mut term = feed(4, "你好".as_bytes());
        term.resize(CrosswordsSize::new(1, 3));
        for row in term.visible_rows() {
            let cell = row[Column(0)];
            assert!(!cell.is_wide() && !cell.is_spacer());
        }
        Processor::default().advance(&mut term, "世".as_bytes());
        term.resize(CrosswordsSize::new(4, 3));
    }

    /// The mouse protocols are one setting with four states, not four
    /// independent flags, so the last mode set wins and clears the rest.
    #[test]
    fn x10_mouse_mode_displaces_the_other_protocols() {
        use crate::performer::handler::Processor;

        let mut term = Crosswords::new(
            CrosswordsSize::new(10, 10),
            CursorShape::Block,
            VoidListener,
            WindowId::from(0),
            0,
            10,
        );
        let mut parser = Processor::default();

        parser.advance(&mut term, b"\x1b[?9h");
        assert!(term.mode().contains(Mode::MOUSE_REPORT_X10));
        assert!(term.mode().intersects(Mode::MOUSE_MODE));

        // 1000 supersedes X10.
        parser.advance(&mut term, b"\x1b[?1000h");
        assert!(!term.mode().contains(Mode::MOUSE_REPORT_X10));
        assert!(term.mode().contains(Mode::MOUSE_REPORT_CLICK));

        // And X10 displaces 1000 in turn.
        parser.advance(&mut term, b"\x1b[?9h");
        assert!(term.mode().contains(Mode::MOUSE_REPORT_X10));
        assert!(!term.mode().contains(Mode::MOUSE_REPORT_CLICK));

        parser.advance(&mut term, b"\x1b[?9l");
        assert!(!term.mode().intersects(Mode::MOUSE_MODE));
    }

    /// Resetting one protocol while another is active turns reporting off
    /// rather than leaving the active one running, because the four are one
    /// setting and a reset cannot name a protocol to fall back to.
    #[test]
    fn resetting_any_mouse_protocol_clears_reporting() {
        use crate::performer::handler::Processor;

        let sequences: [&[u8]; 4] =
            [b"\x1b[?9l", b"\x1b[?1000l", b"\x1b[?1002l", b"\x1b[?1003l"];
        for reset in sequences {
            let sets: [&[u8]; 4] =
                [b"\x1b[?9h", b"\x1b[?1000h", b"\x1b[?1002h", b"\x1b[?1003h"];
            for set in sets {
                let mut term = Crosswords::new(
                    CrosswordsSize::new(10, 10),
                    CursorShape::Block,
                    VoidListener,
                    WindowId::from(0),
                    0,
                    10,
                );
                let mut parser = Processor::default();

                parser.advance(&mut term, set);
                assert!(term.mode().intersects(Mode::MOUSE_MODE));

                parser.advance(&mut term, reset);
                assert!(
                    !term.mode().intersects(Mode::MOUSE_MODE),
                    "{:?} left reporting on after {:?}",
                    String::from_utf8_lossy(reset),
                    String::from_utf8_lossy(set),
                );
            }
        }
    }

    /// DECRQM has to answer for mode 9 now that it is tracked, otherwise
    /// programs cannot tell that their request took effect.
    #[test]
    fn x10_mouse_mode_answers_decrqm() {
        use crate::performer::handler::Processor;
        use std::cell::RefCell;
        use std::rc::Rc;

        let events = Rc::new(RefCell::new(Vec::new()));
        let mut term = Crosswords::new(
            CrosswordsSize::new(10, 10),
            CursorShape::Block,
            TestListener {
                events: events.clone(),
            },
            WindowId::from(0),
            0,
            10,
        );
        let mut parser = Processor::default();

        let replies = |events: &Rc<RefCell<Vec<RioEvent>>>| -> Vec<String> {
            events
                .borrow()
                .iter()
                .filter_map(|event| match event {
                    RioEvent::PtyWrite(_, text) => Some(text.clone()),
                    _ => None,
                })
                .collect()
        };

        // Reset (2) before it is set, set (1) after.
        parser.advance(&mut term, b"\x1b[?9$p");
        assert_eq!(replies(&events), vec!["\x1b[?9;2$y".to_string()]);

        events.borrow_mut().clear();
        parser.advance(&mut term, b"\x1b[?9h\x1b[?9$p");
        assert_eq!(replies(&events), vec!["\x1b[?9;1$y".to_string()]);
    }
}
