// Copyright (c) 2023-present, Raphael Amorim.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Screen serialization: one walk of the active screen with several emit
//! targets. Today `Plain` (text) and `Vt` (ANSI escape sequences) are
//! implemented; the enum leaves room for `Html`.
//!
//! `Vt` reproduces the visible screen as a byte blob that, written to a fresh
//! terminal, renders the same thing: the classic "reconnect / restore
//! snapshot" (vt100's `contents_formatted`). Colors are emitted in their
//! ORIGINAL form (named / indexed / rgb), NOT resolved to RGB, so the client
//! applies its own palette.

use super::Crosswords;
use crate::config::colors::AnsiColor;
use crate::crosswords::pos::{Column, Line};
use crate::crosswords::style::{Style, StyleFlags};
use crate::event::EventListener;
use std::fmt::Write as _;

/// Serialization target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Emit {
    /// Plain UTF-8 text, no attributes.
    Plain,
    /// VT/ANSI escape sequences (SGR + positioning): a restore snapshot.
    Vt,
}

/// Options for [`Crosswords::format`].
#[derive(Debug, Clone, Copy)]
pub struct FormatOptions {
    pub emit: Emit,
    /// Trim trailing whitespace on each row and drop trailing blank rows.
    pub trim: bool,
}

impl FormatOptions {
    pub fn plain() -> Self {
        Self {
            emit: Emit::Plain,
            trim: true,
        }
    }
    pub fn vt() -> Self {
        Self {
            emit: Emit::Vt,
            trim: true,
        }
    }
}

impl<U: EventListener> Crosswords<U> {
    /// Serialize the visible screen using `opts`.
    pub fn format(&self, opts: FormatOptions) -> String {
        let mut out = String::new();
        self.format_into(opts, &mut out);
        out
    }

    /// Serialize into a reusable buffer, replacing its previous contents.
    /// Repeated snapshots keep their output allocation between calls.
    pub fn format_into(&self, opts: FormatOptions, out: &mut String) {
        out.clear();
        match opts.emit {
            Emit::Plain => self.format_plain(out),
            Emit::Vt => self.format_vt(opts, out),
        }
    }

    /// The visible screen as a VT/ANSI byte blob (vt100 `contents_formatted`
    /// equivalent): clear + home, cells with SGR, then the cursor restored.
    pub fn contents_formatted(&self) -> Vec<u8> {
        self.format(FormatOptions::vt()).into_bytes()
    }

    fn format_plain(&self, out: &mut String) {
        let rows = self.screen_lines();
        let cols = self.columns();
        if rows == 0 || cols == 0 {
            return;
        }
        let start = crate::crosswords::pos::Pos::new(Line(0), Column(0));
        let end =
            crate::crosswords::pos::Pos::new(Line(rows as i32 - 1), Column(cols - 1));
        self.bounds_to_string_into(start, end, out)
    }

    fn format_vt(&self, opts: FormatOptions, out: &mut String) {
        let rows = self.screen_lines();
        let cols = self.columns();
        if rows == 0 || cols == 0 {
            return;
        }

        out.reserve(rows.saturating_mul(cols));
        // Reset, clear, home so the snapshot renders on a fresh client.
        out.push_str("\x1b[0m\x1b[2J\x1b[H");
        // The style currently in effect on the client. Starts as the default
        // (matching the `\x1b[0m` above); we emit an SGR only when it changes.
        let mut current = Style::default();
        // The grid is immutable for this call, so a repeated ID has the
        // same resolved value. Inline background cells have no style ID
        // and must invalidate this key before another ordinary cell.
        let mut current_id = Some(super::style::DEFAULT_STYLE_ID);

        // Find the last non-blank row so we can drop trailing blank rows.
        let last_row = if opts.trim {
            (0..rows)
                .rev()
                .find(|&line| self.row_has_content(line, cols))
                .map(|l| l as i32)
        } else {
            Some(rows as i32 - 1)
        };
        let Some(last_row) = last_row else {
            // Blank screen; still restore the cursor.
            self.push_cursor(out);
            return;
        };

        // A small palette repeats the same escape sequences across many
        // rows. Encode it once per snapshot, without retaining recyclable
        // style ids across calls. Large palettes keep the direct emitter.
        let palette = self.grid.styles();
        let mut sgr = String::new();
        let mut offsets = [0; 33];
        let mut cached_styles = 0;
        if palette.len() > 1 && palette.len() <= 32 && rows.saturating_mul(cols) >= 256 {
            sgr.reserve(palette.len() * 48);
            for (i, style) in palette.iter().enumerate() {
                write_sgr(&mut sgr, style);
                offsets[i + 1] = sgr.len();
            }
            cached_styles = palette.len();
        }

        for line in 0..=last_row {
            let last_col = self.row_last_content_col(line, cols, opts.trim);
            let mut col = 0usize;
            while let Some(limit) = last_col {
                if col > limit {
                    break;
                }
                let square = &self.grid[Line(line)][Column(col)];
                if square.is_spacer() {
                    col += 1;
                    continue;
                }
                let id = square.style_id_checked();
                if id.is_none() || id != current_id {
                    let style = self.grid.style_of(square);
                    if style != current {
                        let cached =
                            id.map(|id| id as usize).filter(|&id| id < cached_styles);
                        if let Some(id) = cached {
                            out.push_str(&sgr[offsets[id]..offsets[id + 1]]);
                        } else {
                            write_sgr(out, &style);
                        }
                        current = style;
                    }
                    current_id = id;
                }
                let c = square.c();
                out.push(if c == '\0' { ' ' } else { c });
                col += if square.is_wide() { 2 } else { 1 };
            }
            if line < last_row {
                out.push_str("\r\n");
            }
        }

        out.push_str("\x1b[0m");
        self.push_cursor(out);
    }

    fn push_cursor(&self, out: &mut String) {
        let pos = self.grid.cursor.pos;
        let row = pos.row.0.max(0) + 1;
        let col = pos.col.0 + 1;
        let _ = write!(out, "\x1b[{row};{col}H");
        // Restore cursor visibility (DECTCEM). A screen with a hidden cursor
        // is common while a TUI redraws; without this a restored snapshot
        // would leave a stray cursor on the client until the next output.
        if self.mode.contains(super::Mode::SHOW_CURSOR) {
            out.push_str("\x1b[?25h");
        } else {
            out.push_str("\x1b[?25l");
        }
    }

    fn row_has_content(&self, line: usize, cols: usize) -> bool {
        self.row_last_content_col(line as i32, cols, true).is_some()
    }

    /// Rightmost column with a non-blank glyph or a style that renders
    /// on a blank cell (`Style::renders_on_blank`), or `None` for a row
    /// with neither. Fg-only attributes must NOT count as content:
    /// `ED`/`EL` reset cells with the full cursor template style, so
    /// after `\x1b[31m` + `clear` every blank carries a red fg, and
    /// trimming on any non-default style would then emit the whole
    /// screen as styled spaces.
    fn row_last_content_col(&self, line: i32, cols: usize, trim: bool) -> Option<usize> {
        if !trim {
            return Some(cols.saturating_sub(1));
        }
        let row = &self.grid[Line(line)];
        (0..cols).rev().find(|&col| {
            let square = &row[Column(col)];
            let c = square.c();
            (c != ' ' && c != '\0') || self.grid.style_of(square).renders_on_blank()
        })
    }
}

/// Write the SGR sequence for `style` directly into `out`, starting with `0`
/// (reset) then re-applying: styles are fully self-contained. Written inline
/// (no intermediate allocation); call it only when the style actually
/// changes. Colors keep their original named / indexed / rgb form. Also the
/// style emitter for librio's `Surface::serialize`.
pub fn write_sgr(out: &mut String, style: &Style) {
    use crate::crosswords::style::UnderlineKind;

    out.push_str("\x1b[0");
    let flags = style.flags;
    if flags.contains(StyleFlags::BOLD) {
        out.push_str(";1");
    }
    if flags.contains(StyleFlags::DIM) {
        out.push_str(";2");
    }
    if flags.contains(StyleFlags::ITALIC) {
        out.push_str(";3");
    }
    match flags.underline_kind() {
        None => {}
        Some(UnderlineKind::Single) => out.push_str(";4"),
        Some(UnderlineKind::Double) => out.push_str(";4:2"),
        Some(UnderlineKind::Curly) => out.push_str(";4:3"),
        Some(UnderlineKind::Dotted) => out.push_str(";4:4"),
        Some(UnderlineKind::Dashed) => out.push_str(";4:5"),
    }
    if flags.contains(StyleFlags::SLOW_BLINK) {
        out.push_str(";5");
    } else if flags.contains(StyleFlags::RAPID_BLINK) {
        out.push_str(";6");
    }
    if flags.contains(StyleFlags::INVERSE) {
        out.push_str(";7");
    }
    if flags.contains(StyleFlags::HIDDEN) {
        out.push_str(";8");
    }
    if flags.contains(StyleFlags::STRIKEOUT) {
        out.push_str(";9");
    }
    push_color(out, style.fg, true);
    push_color(out, style.bg, false);
    out.push('m');

    // Underline color travels as its own sequence; the leading reset
    // already cleared any previous one.
    if let Some(underline) = style.underline_color {
        match underline {
            AnsiColor::Indexed(i) => {
                out.push_str("\x1b[58;5;");
                push_byte(out, i);
                out.push('m');
            }
            AnsiColor::Spec(rgb) => {
                out.push_str("\x1b[58;2;");
                push_rgb(out, rgb.r, rgb.g, rgb.b);
                out.push('m');
            }
            AnsiColor::Named(n) => {
                // Specials (Foreground = 256 and up) have no palette
                // index; SGR 58 cannot express them, so leave the reset's
                // unset underline color rather than emit an invalid index.
                if let Ok(idx) = u8::try_from(n as u16) {
                    out.push_str("\x1b[58;5;");
                    push_byte(out, idx);
                    out.push('m');
                }
            }
        }
    }
}

// SGR colors are bounded bytes. Avoid general-purpose formatting machinery
// for each RGB component and palette index in a styled screen snapshot.
#[inline]
fn push_byte(out: &mut String, n: u8) {
    if n >= 100 {
        out.push((b'0' + n / 100) as char);
        out.push((b'0' + (n / 10) % 10) as char);
    } else if n >= 10 {
        out.push((b'0' + n / 10) as char);
    }
    out.push((b'0' + n % 10) as char);
}

#[inline]
fn push_rgb(out: &mut String, r: u8, g: u8, b: u8) {
    push_byte(out, r);
    out.push(';');
    push_byte(out, g);
    out.push(';');
    push_byte(out, b);
}

fn push_color(params: &mut String, color: AnsiColor, fg: bool) {
    match color {
        AnsiColor::Named(named) => {
            let n = named as u16;
            if n <= 7 {
                let base = if fg { 30 } else { 40 };
                params.push(';');
                push_byte(params, (base + n) as u8);
            } else if n <= 15 {
                let base = if fg { 90 } else { 100 };
                params.push(';');
                push_byte(params, (base + (n - 8)) as u8);
            }
            // Named default fg/bg (256/257) and specials: covered by the
            // leading `0` reset, so emit nothing.
        }
        AnsiColor::Indexed(i) => {
            params.push_str(if fg { ";38;5;" } else { ";48;5;" });
            push_byte(params, i);
        }
        AnsiColor::Spec(rgb) => {
            params.push_str(if fg { ";38;2;" } else { ";48;2;" });
            push_rgb(params, rgb.r, rgb.g, rgb.b);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ansi::CursorShape;
    use crate::crosswords::square::Square;
    use crate::crosswords::{Crosswords, CrosswordsSize};
    use crate::event::{VoidListener, WindowId};
    use crate::performer::handler::Processor;

    fn term(cols: usize, rows: usize) -> Crosswords<VoidListener> {
        Crosswords::new(
            CrosswordsSize::new(cols, rows),
            CursorShape::Block,
            VoidListener,
            WindowId::from(0),
            0,
            0,
        )
    }

    fn feed(t: &mut Crosswords<VoidListener>, bytes: &[u8]) {
        let mut p = Processor::default();
        p.advance(t, bytes);
    }

    #[test]
    fn reusable_plain_output_keeps_wraps_and_blank_rows_and_replaces_old_contents() {
        let mut t = term(6, 4);
        let mut output = String::from("stale snapshot");
        feed(&mut t, b"abcdefghi\r\n\r\nz");
        t.format_into(FormatOptions::plain(), &mut output);
        assert_eq!(output, "abcdefghi\n\nz");
        feed(&mut t, b"\x1b[2J\x1b[Hx");
        t.format_into(FormatOptions::plain(), &mut output);
        assert_eq!(output, "x");
        feed(&mut t, b"\x1b[2J\x1b[H");
        t.format_into(FormatOptions::plain(), &mut output);
        assert!(output.is_empty());
    }

    #[test]
    fn ascii_selection_matches_general_path_across_wraps_and_partial_rows() {
        for cols in [1, 3, 120, 256, 257] {
            for text in ["ab  cd ef\r\n\r\nxyz ", "\tword\r\n漢e\u{301} x"] {
                let mut fast = term(cols, 4);
                let mut general = term(cols, 4);
                feed(&mut fast, text.as_bytes());
                feed(&mut general, text.as_bytes());
                for line in 0..4 {
                    // A conservative extras hint selects the general path
                    // without changing any selected cells or their contents.
                    general.grid[Line(line)].has_extras = true;
                }
                for start in [0, cols / 2, cols - 1] {
                    for end in [start, cols - 1] {
                        let a = crate::crosswords::pos::Pos::new(Line(0), Column(start));
                        let b = crate::crosswords::pos::Pos::new(Line(3), Column(end));
                        assert_eq!(
                            fast.bounds_to_string(a, b),
                            general.bounds_to_string(a, b)
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn sgr_byte_boundaries_roundtrip() {
        use crate::config::colors::ColorRgb;
        for n in [0, 9, 10, 99, 100, 199, 255] {
            let style = Style {
                fg: AnsiColor::Spec(ColorRgb { r: n, g: n, b: n }),
                bg: AnsiColor::Indexed(n),
                underline_color: Some(AnsiColor::Indexed(n)),
                ..Style::default()
            };
            let mut out = String::new();
            write_sgr(&mut out, &style);
            out.push('x');
            let mut t = term(6, 2);
            feed(&mut t, out.as_bytes());
            assert_eq!(t.grid.style_of(&t.grid[Line(0)][Column(0)]), style);
        }
    }

    #[test]
    fn vt_roundtrip_preserves_text_and_style() {
        // Large enough to exercise the bounded per-snapshot SGR cache.
        let mut a = term(80, 4);
        // rapid-blink red "hello", default space, bold-blink-blue "world",
        // then a red-undercurled "curl" on the next line.
        feed(
            &mut a,
            b"\x1b[6;31mhello\x1b[0m \x1b[1;5;34mworld\x1b[0m\r\n\
                      \x1b[4:3;58;2;255;0;0mcurl\x1b[0m",
        );
        let snapshot = a.contents_formatted();

        // The snapshot must carry the colors (index form, not resolved rgb).
        let text = String::from_utf8(snapshot.clone()).unwrap();
        assert!(text.contains("31"), "missing red fg: {text:?}");
        assert!(text.contains("34"), "missing blue fg: {text:?}");

        // Re-parse into a fresh screen; the plain content must match.
        let mut b = term(80, 4);
        feed(&mut b, &snapshot);
        assert_eq!(
            a.format(FormatOptions::plain()),
            b.format(FormatOptions::plain()),
        );

        // And a styled cell round-trips its color. 'w' of "world" is at col 6.
        let sa = *(&a.grid[Line(0)][Column(6)] as &Square);
        let sb = *(&b.grid[Line(0)][Column(6)] as &Square);
        assert_eq!(sa.c(), 'w');
        assert_eq!(sb.c(), 'w');
        assert_eq!(a.grid.style_of(&sa).fg, b.grid.style_of(&sb).fg);
        assert!(b.grid.style_of(&sb).flags.contains(StyleFlags::BOLD));
        assert!(b.grid.style_of(&sb).flags.contains(StyleFlags::SLOW_BLINK));
        let hb = *(&b.grid[Line(0)][Column(0)] as &Square);
        assert!(b.grid.style_of(&hb).flags.contains(StyleFlags::RAPID_BLINK));

        // Underline kind and color survive too. 'c' of "curl" is at (1, 0).
        let cb = *(&b.grid[Line(1)][Column(0)] as &Square);
        assert_eq!(cb.c(), 'c');
        let curl = b.grid.style_of(&cb);
        assert!(curl.flags.contains(StyleFlags::UNDERCURL));
        assert_eq!(
            curl.underline_color,
            Some(AnsiColor::Spec(crate::config::colors::ColorRgb {
                r: 255,
                g: 0,
                b: 0
            }))
        );
    }

    #[test]
    fn vt_style_runs_reset_after_inline_background_cells() {
        let mut a = term(12, 2);
        feed(&mut a, b"\x1b[31mAA\x1b[0m\x1b[3CBB\x1b[31mCC");
        a.grid[Line(0)][Column(2)].set_bg_rgb(12, 34, 56);
        a.grid[Line(0)][Column(3)].set_bg_palette(42);
        let snapshot = a.contents_formatted();
        let mut b = term(12, 2);
        feed(&mut b, &snapshot);
        for col in 0..9 {
            let original = &a.grid[Line(0)][Column(col)];
            let restored = &b.grid[Line(0)][Column(col)];
            assert_eq!(a.grid.style_of(original), b.grid.style_of(restored));
        }
    }

    #[test]
    fn vt_roundtrip_preserves_styled_blanks() {
        use crate::config::colors::NamedColor;

        // Red-background trailing spaces after "AB" render, so the trim
        // must keep them.
        let mut a = term(20, 4);
        feed(&mut a, b"AB\x1b[41m   \x1b[0m");
        let snapshot = a.contents_formatted();
        let text = String::from_utf8(snapshot.clone()).unwrap();
        assert!(text.contains(";41"), "missing red bg: {text:?}");

        let mut b = term(20, 4);
        feed(&mut b, &snapshot);
        let sb = *(&b.grid[Line(0)][Column(3)] as &Square);
        assert_eq!(b.grid.style_of(&sb).bg, AnsiColor::Named(NamedColor::Red));

        // A row made only of styled blanks must not be dropped as blank.
        let mut c = term(20, 4);
        feed(&mut c, b"top\r\n\x1b[44m    \x1b[0m");
        let text = String::from_utf8(c.contents_formatted()).unwrap();
        assert!(text.contains(";44"), "styled-blank row dropped: {text:?}");
    }

    #[test]
    fn vt_trims_fg_only_erased_cells() {
        // ED resets cells with the full cursor template, so after
        // `\x1b[31m` + clear every blank carries a red fg. Fg-only
        // attributes render nothing on a blank: the snapshot must trim
        // them, not emit the whole screen as styled spaces.
        let mut a = term(20, 4);
        feed(&mut a, b"\x1b[31mhi\x1b[2J\x1b[H");
        let text = String::from_utf8(a.contents_formatted()).unwrap();
        assert!(
            !text.contains(' '),
            "erased fg-styled blanks not trimmed: {text:?}"
        );
    }

    #[test]
    fn vt_restores_cursor_visibility() {
        // Hidden cursor (as a TUI leaves it mid-draw) must be restored hidden.
        let mut hidden = term(10, 3);
        feed(&mut hidden, b"\x1b[?25l");
        let text = String::from_utf8(hidden.contents_formatted()).unwrap();
        assert!(
            text.contains("\x1b[?25l"),
            "snapshot should hide cursor: {text:?}"
        );
        assert!(!text.contains("\x1b[?25h"));

        // Default (shown) cursor is restored shown.
        let shown = term(10, 3);
        let text = String::from_utf8(shown.contents_formatted()).unwrap();
        assert!(
            text.contains("\x1b[?25h"),
            "snapshot should show cursor: {text:?}"
        );
    }

    #[test]
    fn plain_and_vt_agree_on_text() {
        let mut a = term(10, 3);
        feed(&mut a, b"abc\r\ndef");
        let plain = a.format(FormatOptions::plain());
        assert!(plain.contains("abc"));
        assert!(plain.contains("def"));
    }
}
