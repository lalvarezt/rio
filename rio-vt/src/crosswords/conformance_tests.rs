// Behavioral cases adapted from Ghostty's src/terminal/Terminal.zig at
// f54340ad77814fe319d136e5e915d6a48de8dd23 (MIT). See docs/test-audit.
// Feed real escape sequences so parser dispatch and terminal state are checked
// together. The table-driven parser cases run at every two-chunk boundary and
// byte by byte.

use super::grid::Dimensions;
use super::pos::{Column, Line, Pos};
use super::square::{Extras, Wide};
use super::style::{Style, StyleFlags};
use super::{Crosswords, CrosswordsSize};
use crate::ansi::CursorShape;
use crate::config::colors::{AnsiColor, ColorRgb, NamedColor};
use crate::event::{VoidListener, WindowId};
use crate::performer::handler::Processor;

fn terminal(columns: usize, rows: usize) -> Crosswords<VoidListener> {
    Crosswords::new(
        CrosswordsSize::new(columns, rows),
        CursorShape::Block,
        VoidListener,
        WindowId::from(0),
        0,
        100,
    )
}

fn check_state(term: &Crosswords<VoidListener>, rows: &[&str], cursor: (i32, usize)) {
    let actual: Vec<String> = (0..term.grid.screen_lines())
        .map(|row| {
            (0..term.grid.columns())
                .map(|col| match term.grid[Line(row as i32)][Column(col)].c() {
                    '\0' | '\t' => ' ',
                    c => c,
                })
                .collect::<String>()
                .trim_end_matches(' ')
                .to_owned()
        })
        .collect();
    assert_eq!(actual, rows, "visible rows");
    assert_eq!(term.grid.cursor.pos.row.0, cursor.0, "cursor row");
    assert_eq!(term.grid.cursor.pos.col.0, cursor.1, "cursor column");
}

fn check_case(columns: usize, input: &[u8], rows: &[&str], cursor: (i32, usize)) {
    for split in 0..=input.len() {
        let mut term = terminal(columns, rows.len());
        let mut parser = Processor::default();
        parser.advance(&mut term, &input[..split]);
        parser.advance(&mut term, &input[split..]);
        check_state(&term, rows, cursor);
    }
    let mut term = terminal(columns, rows.len());
    let mut parser = Processor::default();
    for byte in input {
        parser.advance(&mut term, std::slice::from_ref(byte));
    }
    check_state(&term, rows, cursor);
}

#[test]
fn optimized_printing_matches_fragmented_input_after_overwrites() {
    // PTY read boundaries must not change cell cleanup or wrap state. Exercise
    // the ASCII, homogeneous Unicode and short mixed-width bulk writers.
    for columns in [1, 2, 3, 5, 8] {
        for setup in ["", "界界界界界", "abc界de界", "\x1b[31;44m界界界界\x1b[0m"]
        {
            for mode in ["", "\x1b[?7l", "\x1b[?2027h", "\x1b[?2027l", "\x1b[4h"] {
                for row in 1..=3 {
                    for column in 1..=columns {
                        for text in [
                            "abcdefghi",
                            "éééééé",
                            "界界界界",
                            "a界b界c",
                            "界a界bc",
                            "a\u{301}界b",
                            "👩‍💻xyz",
                        ] {
                            let input =
                                format!("{setup}{mode}\x1b[{row};{column}H{text}");
                            let mut bulk = terminal(columns, 3);
                            let mut fragmented = terminal(columns, 3);
                            Processor::default().advance(&mut bulk, input.as_bytes());
                            let mut parser = Processor::default();
                            for byte in input.as_bytes() {
                                parser
                                    .advance(&mut fragmented, std::slice::from_ref(byte));
                            }
                            assert_eq!(
                                bulk.grid.cursor.pos, fragmented.grid.cursor.pos,
                                "cursor: {columns} {input:?}"
                            );
                            assert_eq!(
                                bulk.grid.cursor.should_wrap,
                                fragmented.grid.cursor.should_wrap,
                                "wrap: {columns} {input:?}"
                            );
                            assert_eq!(
                                bulk.history_size(),
                                fragmented.history_size(),
                                "history: {columns} {input:?}"
                            );
                            for line in -(bulk.history_size() as i32)..3 {
                                for col in 0..columns {
                                    let a = bulk.grid[Line(line)][Column(col)];
                                    let b = fragmented.grid[Line(line)][Column(col)];
                                    assert_eq!(
                                        (a.c(), a.wide(), a.wrapline()),
                                        (b.c(), b.wide(), b.wrapline()),
                                        "cell {line},{col}: {columns} {input:?}"
                                    );
                                    assert_eq!(
                                        bulk.grid.style_of(&a),
                                        fragmented.grid.style_of(&b),
                                        "style: {columns} {input:?}"
                                    );
                                    let extras = |term: &Crosswords<VoidListener>, cell: super::square::Square| cell.extras_id().and_then(|id| term.grid.extras_table.get(id)).cloned();
                                    assert_eq!(
                                        extras(&bulk, a),
                                        extras(&fragmented, b),
                                        "extras: {columns} {input:?}"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn overwriting_wrapped_wide_character_damages_previous_row() {
    for text in ["x", "éé", "é界"] {
        for column in [1, 2] {
            for fragmented in [false, true] {
                let mut term = terminal(5, 3);
                let mut parser = Processor::default();
                parser.advance(&mut term, "abcd界".as_bytes());
                assert_eq!(term.grid[Line(0)][Column(4)].wide(), Wide::LeadingSpacer);
                parser.advance(&mut term, format!("\x1b[2;{column}H").as_bytes());
                // Consume cursor movement separately so it cannot hide missing
                // damage for the previous row during the subsequent overwrite.
                term.reset_damage();
                if fragmented {
                    for byte in text.as_bytes() {
                        parser.advance(&mut term, std::slice::from_ref(byte));
                    }
                } else {
                    parser.advance(&mut term, text.as_bytes());
                }
                assert_eq!(term.grid[Line(0)][Column(4)].wide(), Wide::Narrow);
                let previous_damaged = match term.damage() {
                    super::TermDamage::Full => true,
                    super::TermDamage::Partial(mut lines) => lines.any(|d| d.line == 0),
                };
                assert!(
                    previous_damaged,
                    "previous row: {text:?}, column {column}, fragmented {fragmented}"
                );
            }
        }
    }
}

#[test]
fn overwriting_wide_character_clears_old_tail_style_and_link() {
    // Ghostty Terminal.zig: "print over wide char with bg color" and
    // "print over wide char with bold" clear the old spacer's attributes.
    for (setup, text, column, cleared_column) in [
        ("界", "x", 1, 1),
        ("界", "é", 1, 1),
        ("界界", "é界", 1, 3),
        ("界", "x", 2, 0),
        ("界", "é界", 2, 0),
    ] {
        for (sgr, bg) in [
            ("0", AnsiColor::Named(NamedColor::Background)),
            ("0;1;42", AnsiColor::Indexed(2)),
        ] {
            let mut term = terminal(8, 3);
            let mut parser = Processor::default();
            let input = format!(
                "\x1b[1;41m\x1b]8;;https://example.test\x1b\\{setup}\x1b[{sgr}m\x1b]8;;\x1b\\\x1b[{column}G"
            );
            parser.advance(&mut term, input.as_bytes());
            parser.advance(&mut term, text.as_bytes());
            let cell = term.grid[Line(0)][Column(cleared_column)];
            assert_eq!(cell.wide(), Wide::Narrow);
            assert_eq!(
                term.grid.style_of(&cell),
                Style {
                    bg,
                    ..Style::default()
                },
                "{text:?}, column {column}, SGR {sgr}"
            );
            assert!(cell.extras_id().is_none(), "old hyperlink: {text:?}");
        }
    }
}

macro_rules! terminal_case {
    ($name:ident, $cols:expr, $input:expr, $rows:expr, $cursor:expr) => {
        #[test]
        fn $name() {
            check_case($cols, $input, &$rows, $cursor);
        }
    };
}

#[test]
fn widening_at_row_edge_preserves_base_over_existing_wide_pair() {
    for destination in ["界", "a界", "界界", "", "abcd", "e\u{301}"] {
        for columns in [3, 5, 8] {
            let input =
                format!("\x1b[?2027h\x1b[2;1H{destination}\x1b[1;{columns}H❤\u{fe0f}");
            for split in 0..=input.len() {
                let mut term = terminal(columns, 3);
                let mut parser = Processor::default();
                parser.advance(&mut term, &input.as_bytes()[..split]);
                parser.advance(&mut term, &input.as_bytes()[split..]);
                let base = term.grid[Line(1)][Column(0)];
                assert_eq!(base.c(), '❤', "split {split}");
                assert_eq!(base.wide(), Wide::Wide);
                assert_eq!(
                    term.grid[Line(0)][Column(columns - 1)].wide(),
                    Wide::LeadingSpacer
                );
                assert_eq!(term.grid[Line(1)][Column(1)].wide(), Wide::Spacer);
                let extras = term
                    .grid
                    .extras_table
                    .get(base.extras_id().unwrap())
                    .unwrap();
                assert_eq!(extras.zerowidth, vec!['\u{fe0f}']);
                for col in 2..columns {
                    if term.grid[Line(1)][Column(col)].wide() == Wide::Spacer {
                        assert_eq!(
                            term.grid[Line(1)][Column(col - 1)].wide(),
                            Wide::Wide,
                            "orphan spacer: {destination:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn legacy_combining_mark_at_column_zero_does_not_modify_cell_under_cursor() {
    let input = b"\x1b[?2027la\r\xcc\x81";
    for split in 0..=input.len() {
        let mut term = terminal(5, 3);
        let mut parser = Processor::default();
        parser.advance(&mut term, &input[..split]);
        parser.advance(&mut term, &input[split..]);
        let cell = term.grid[Line(0)][Column(0)];
        assert_eq!(cell.c(), 'a');
        assert!(
            cell.extras_id().is_none(),
            "orphan mark attached at split {split}"
        );
    }
}

#[test]
fn wide_replacement_preserves_previous_row_leading_spacer() {
    for text in ["字", "字字", "字é字"] {
        for fragmented in [false, true] {
            let mut term = terminal(5, 3);
            let mut parser = Processor::default();
            parser.advance(&mut term, "abcd界\x1b[2;1H".as_bytes());
            term.reset_damage();
            if fragmented {
                for byte in text.as_bytes() {
                    parser.advance(&mut term, std::slice::from_ref(byte));
                }
            } else {
                parser.advance(&mut term, text.as_bytes());
            }
            assert_eq!(
                term.grid[Line(0)][Column(4)].wide(),
                Wide::LeadingSpacer,
                "{text:?}, fragmented {fragmented}"
            );
            assert_eq!(term.grid[Line(1)][Column(0)].c(), '字');
            assert_eq!(term.grid[Line(1)][Column(0)].wide(), Wide::Wide);
        }
    }
}

#[test]
fn vs15_narrowing_preserves_tail_background_and_wrap_marker() {
    // Ghostty's grapheme .narrow path changes only the old tail's width.
    for columns in [2, 5] {
        for wrap in [true, false] {
            let mut term = terminal(columns, 3);
            let mut parser = Processor::default();
            parser.advance(&mut term, "\x1b[?2027h\x1b[41m☔".as_bytes());
            let tail = term.grid[Line(0)][Column(1)];
            let original_style = term.grid.style_of(&tail);
            // For two columns, a later print has already wrapped the old tail.
            if columns == 2 && wrap {
                parser.advance(&mut term, b"X\x1b[1;2H");
            }
            if !wrap {
                parser.advance(&mut term, b"\x1b[?7l");
            }
            let wrapped = term.grid[Line(0)][Column(1)].wrapline();
            parser.advance(&mut term, "\x1b[0m\u{fe0e}".as_bytes());
            let tail = term.grid[Line(0)][Column(1)];
            assert_eq!(term.grid[Line(0)][Column(0)].wide(), Wide::Narrow);
            assert_eq!(tail.wide(), Wide::Narrow);
            assert_eq!(term.grid.style_of(&tail), original_style);
            assert_eq!(tail.wrapline(), wrapped);
            let base = term.grid[Line(0)][Column(0)];
            let extras = term
                .grid
                .extras_table
                .get(base.extras_id().unwrap())
                .unwrap();
            assert_eq!(extras.zerowidth, vec!['\u{fe0e}']);
        }
    }
}

#[test]
fn retained_snapshots_match_fresh_captures_after_wide_cluster_edits() {
    use crate::event::TerminalDamage;
    for deferred in [false, true] {
        let mut term = terminal(5, 3);
        let mut parser = Processor::default();
        let (mut rows, mut styles, mut extras) = Default::default();
        let mut scratch = super::snapshot::DeferredStyles::default();
        for input in [
            "\x1b[?2027h\x1b[41mabcd界",
            "\x1b[0m\x1b[2;1H字é字",
            "\x1b[2;1H\x1b]8;;https://example.test\x1b\\界\x1b]8;;\x1b\\",
            "\x1b[1;5H❤\u{fe0f}",
            "\x1b[1;1H\x1b[44m☔",
            "\x1b[0m\u{fe0e}",
            "\x1b[3;1Habc☔\u{fe0e}",
            "\x1b[2;1Hx",
            "\x1b[?2027la\r\u{301}",
        ] {
            parser.advance(&mut term, input.as_bytes());
            if deferred {
                term.snapshot_visible_deferred(
                    &TerminalDamage::Partial,
                    &mut rows,
                    &mut styles,
                    &mut extras,
                    &mut scratch,
                )
                .resolve();
            } else {
                term.snapshot_visible(
                    &TerminalDamage::Partial,
                    &mut rows,
                    &mut styles,
                    &mut extras,
                );
            }
            let (mut fresh_rows, mut fresh_styles, mut fresh_extras) = Default::default();
            term.snapshot_visible(
                &TerminalDamage::Full,
                &mut fresh_rows,
                &mut fresh_styles,
                &mut fresh_extras,
            );
            assert_eq!(
                rows, fresh_rows,
                "cells after {input:?}, deferred={deferred}"
            );
            assert_eq!(
                styles, fresh_styles,
                "styles after {input:?}, deferred={deferred}"
            );
            assert_eq!(extras.len(), fresh_extras.len());
            for row in &rows {
                for cell in &row.inner {
                    if let Some(id) = cell.extras_id_checked() {
                        assert_eq!(
                            extras.get(&id),
                            fresh_extras.get(&id),
                            "extras after {input:?}, deferred={deferred}"
                        );
                    }
                }
            }
            for row in &mut rows {
                row.dirty = false;
            }
            term.reset_damage();
        }
    }
}

#[test]
fn combining_at_right_edge_without_wrap_uses_mode_specific_base() {
    // Ghostty selects the occupied right-edge cell for clustering, but the
    // preceding cell for legacy zero-width attachment without wraparound.
    for (mode, suffix, marked_column) in [("h", "\x1b[5G\u{301}", 4), ("l", "\u{301}", 3)]
    {
        let input = format!("\x1b[?2027{mode}\x1b[?7lABCDE{suffix}");
        for split in 0..=input.len() {
            let mut term = terminal(5, 3);
            let mut parser = Processor::default();
            parser.advance(&mut term, &input.as_bytes()[..split]);
            parser.advance(&mut term, &input.as_bytes()[split..]);
            for col in [3, 4] {
                let cell = term.grid[Line(0)][Column(col)];
                let marks = cell
                    .extras_id()
                    .and_then(|id| term.grid.extras_table.get(id))
                    .map(|e| e.zerowidth.as_slice())
                    .unwrap_or_default();
                assert_eq!(
                    marks,
                    if col == marked_column {
                        &['\u{301}'][..]
                    } else {
                        &[][..]
                    },
                    "mode {mode}, col {col}, split {split}"
                );
            }
        }
    }
}

#[test]
fn vs16_without_room_or_wrap_does_not_attach_to_narrow_base() {
    let mut term = terminal(5, 3);
    let mut parser = Processor::default();
    parser.advance(&mut term, "\x1b[?2027h\x1b[?7l\x1b[5G❤".as_bytes());
    term.reset_damage();
    parser.advance(&mut term, "\u{fe0f}".as_bytes());
    let base = term.grid[Line(0)][Column(4)];
    assert_eq!(base.c(), '❤');
    assert_eq!(base.wide(), Wide::Narrow);
    assert!(base.extras_id().is_none());
    assert!(term.peek_damage_event().is_none());
}

#[test]
fn column_shrink_reflows_background_only_cells() {
    // Ghostty PageList.reflowRow trims using Cell.isEmpty, which treats
    // background-only cells as meaningful even when the row has no text.
    for tracked in [true, false] {
        let mut term = terminal(5, 3);
        Processor::default().advance(&mut term, b"\x1b[41m\x1b[2J\x1b[H");
        term.grid.track_reflow_remap = tracked;
        term.grid.resize(true, 3, 3);
        let colored = (-(term.history_size() as i32)..3)
            .flat_map(|line| (0..3).map(move |col| (line, col)))
            .filter(|&(line, col)| {
                term.grid.style_of(&term.grid[Line(line)][Column(col)]).bg
                    == AnsiColor::Indexed(1)
            })
            .count();
        assert_eq!(
            colored, 15,
            "background cells lost by column shrink, tracked={tracked}"
        );
    }
}

#[test]
fn resize_fast_paths_match_full_reflow_with_row_tracking() {
    for original_columns in [2, 3, 5, 8] {
        for input in [
            "",
            "abcde\x1b7",
            "a界b界c",
            "\x1b[41m\x1b[2J",
            "\x1b[44m\x1b[2J\x1b[0mhi",
            "\x1b[41m\x1b[2K",
            "\x1b[2;3H\x1b7\x1b[H",
            "\x1b]133;A\x07",
            "\x1b]8;id=resize;https://example.test\x07e\u{301}界\x1b]8;;\x07",
        ] {
            for reflow in [false, true] {
                for columns in [1, 2, 4, 7, 10] {
                    let mut fast = terminal(original_columns, 3);
                    let mut full = terminal(original_columns, 3);
                    Processor::default().advance(&mut fast, input.as_bytes());
                    Processor::default().advance(&mut full, input.as_bytes());
                    full.grid.track_reflow_remap = true;
                    for (width, height) in
                        [(columns, 3), (original_columns, 5), (columns, 2)]
                    {
                        fast.grid.resize(reflow, height, width);
                        full.grid.resize(reflow, height, width);
                        let context = format!("{original_columns} -> {width}x{height}, reflow={reflow}, {input:?}");
                        assert_eq!(
                            fast.grid.cursor.pos, full.grid.cursor.pos,
                            "cursor: {context}"
                        );
                        assert_eq!(
                            fast.grid.cursor.should_wrap, full.grid.cursor.should_wrap,
                            "pending: {context}"
                        );
                        assert_eq!(
                            fast.grid.saved_cursor.pos, full.grid.saved_cursor.pos,
                            "saved cursor: {context}"
                        );
                        assert_eq!(
                            fast.grid.saved_cursor.should_wrap,
                            full.grid.saved_cursor.should_wrap,
                            "saved pending: {context}"
                        );
                        assert_eq!(
                            fast.history_size(),
                            full.history_size(),
                            "history: {context}"
                        );
                        for line in -(fast.history_size() as i32)..height as i32 {
                            assert_eq!(
                                fast.grid[Line(line)].semantic_prompt,
                                full.grid[Line(line)].semantic_prompt,
                                "prompt: {context}"
                            );
                            for col in 0..width {
                                let a = fast.grid[Line(line)][Column(col)];
                                let b = full.grid[Line(line)][Column(col)];
                                assert_eq!(
                                    (a.c(), a.wide(), a.wrapline()),
                                    (b.c(), b.wide(), b.wrapline()),
                                    "cell {line},{col}: {context}"
                                );
                                assert_eq!(
                                    fast.grid.style_of(&a),
                                    full.grid.style_of(&b),
                                    "style {line},{col}: {context}"
                                );
                                let extras = |term: &Crosswords<VoidListener>, cell: super::square::Square| cell.extras_id().and_then(|id| term.grid.extras_table.get(id)).cloned();
                                assert_eq!(
                                    extras(&fast, a),
                                    extras(&full, b),
                                    "extras: {context}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn saved_cursor_tracks_its_row_when_height_shrink_scrolls() {
    // Ghostty Screen.resize tracks the saved cursor with a page pin, so rows
    // scrolled into history shift a still-visible saved cell upward too.
    let mut term = terminal(5, 5);
    let mut parser = Processor::default();
    parser.advance(&mut term, b"A\r\nB\r\nC\r\nD\r\nE\x1b[3;1H\x1b7\x1b[5;1H");
    term.resize(CrosswordsSize::new(5, 3));
    parser.advance(&mut term, b"\x1b8");
    assert_eq!(term.grid.cursor.pos, Pos::new(Line(0), Column(0)));
    assert_eq!(
        term.grid[term.grid.cursor.pos.row][term.grid.cursor.pos.col].c(),
        'C'
    );
}

#[test]
fn saved_cursor_follows_retained_content_on_height_shrink() {
    for (saved_row, live_row) in [(1, 5), (5, 1)] {
        let mut term = terminal(5, 5);
        let mut parser = Processor::default();
        parser.advance(
            &mut term,
            format!("\x1b[{saved_row};1HABCDE\x1b7\x1b[{live_row};1H").as_bytes(),
        );
        term.resize(CrosswordsSize::new(5, 3));
        parser.advance(&mut term, b"\x1b8");
        if saved_row == 1 {
            assert_eq!(term.grid.cursor.pos, Pos::new(Line(0), Column(0)));
            assert!(!term.grid.cursor.should_wrap);
        } else {
            assert_eq!(term.grid.cursor.pos, Pos::new(Line(2), Column(4)));
            assert!(term.grid.cursor.should_wrap);
        }
    }
}

#[test]
fn character_shifts_keep_wrap_metadata_at_the_row_boundary() {
    for count in [1, 2, 5] {
        for action in ['@', 'P'] {
            let mut term = terminal(5, 3);
            Processor::default().advance(
                &mut term,
                format!("ABCDEZ\x1b[H\x1b[{count}{action}").as_bytes(),
            );
            for col in 0..5 {
                assert_eq!(
                    term.grid[Line(0)][Column(col)].wrapline(),
                    action == '@' && col == 4,
                    "{action}, count {count}, column {col}"
                );
            }
        }
    }
}

#[test]
fn column_reflow_consumes_trailing_blanks_before_creating_history() {
    let mut term = terminal(8, 5);
    Processor::default().advance(&mut term, "a界b界c界d界e".as_bytes());
    term.resize(CrosswordsSize::new(3, 5));
    check_state(&term, &["a界", "b界", "c界", "d界", "e"], (4, 1));
    assert_eq!(term.history_size(), 0);
}

#[cfg(feature = "graphics")]
#[test]
fn column_reflow_keeps_blank_rows_with_image_anchors() {
    let mut term = Crosswords::new(
        CrosswordsSize::new_with_dimensions(50, 10, 500, 200, 10, 20),
        CursorShape::Block,
        VoidListener,
        WindowId::from(0),
        0,
        100,
    );
    let mut parser = Processor::default();
    parser.advance(&mut term, b"$ kitten icat /path/to/image.png\r\n\x1b[10;1H\x1b_Ga=T,i=1,p=1,f=32,s=1,v=1,C=1;AQIDBA==\x1b\\\x1b[2;1H");
    assert_eq!(term.graphics.kitty_placements.len(), 1);
    term.resize(CrosswordsSize::new_with_dimensions(
        20, 10, 200, 200, 10, 20,
    ));
    assert_eq!(term.graphics.kitty_placements.len(), 1);
    assert_eq!(term.history_size(), 1);
    assert_eq!(term.grid.cursor.pos, Pos::new(Line(1), Column(0)));
}

#[test]
fn clearing_an_empty_viewport_does_not_create_history() {
    for input in [b"\x1b[2J".as_slice(), b"\x1b[41m\x1b[2J", b"\x1b[2J\x1b[2J"] {
        let mut term = terminal(5, 3);
        Processor::default().advance(&mut term, input);
        assert_eq!(term.history_size(), 0);
    }
}

#[test]
fn display_erase_scrolls_only_at_a_prompt_or_when_requested() {
    for (input, history) in [
        ("abc\r\ndef\x1b[2J", 0),
        ("abc\r\ndef\x1b[22J", 2),
        ("abc\r\n\x1b]133;A\x1b\\$ \x1b[H\x1b[2J", 2),
        ("\x1b[41m\x1b[2J\x1b[2J\x1b[22J", 3),
    ] {
        let mut term = terminal(5, 3);
        Processor::default().advance(&mut term, input.as_bytes());
        assert_eq!(term.history_size(), history, "{input:?}");
        for row in 0..3 {
            for col in 0..5 {
                assert_eq!(term.grid[Line(row)][Column(col)].c(), '\0');
            }
        }
    }
}

#[test]
fn height_shrink_keeps_nonempty_rows_below_the_cursor() {
    let mut term = terminal(5, 4);
    Processor::default().advance(&mut term, b"AAA\r\nBBB\r\nCCC\r\nDDD\x1b[H");
    term.resize(CrosswordsSize::new(5, 2));
    check_state(&term, &["CCC", "DDD"], (0, 0));
    assert_eq!(term.history_size(), 2);
    assert_eq!(term.grid[Line(-2)][Column(0)].c(), 'A');
    assert_eq!(term.grid[Line(-1)][Column(0)].c(), 'B');
}

#[test]
fn height_growth_above_the_bottom_keeps_history_out_of_view() {
    let mut term = terminal(5, 3);
    Processor::default().advance(&mut term, b"0\r\n1\r\n2\r\n3\r\n4\r\n5\x1b[H");
    term.resize(CrosswordsSize::new(5, 5));
    check_state(&term, &["3", "4", "5", "", ""], (0, 0));
    assert_eq!(term.history_size(), 3);
}

#[test]
fn reflow_keeps_a_cursor_on_the_blank_after_an_exactly_full_line() {
    let mut term = terminal(8, 3);
    Processor::default().advance(&mut term, "abc界".as_bytes());
    term.resize(CrosswordsSize::new(3, 3));
    term.resize(CrosswordsSize::new(5, 3));
    assert_eq!(term.grid.cursor.pos, Pos::new(Line(1), Column(0)));
    assert!(!term.grid.cursor.should_wrap);
    assert!(term.grid[Line(0)][Column(4)].wrapline());
    assert!(term.grid[Line(1)].wrap_continuation);
    assert_eq!(term.history_size(), 0);
}

#[test]
fn delete_lines_at_the_top_does_not_add_scrollback() {
    for (operation, history) in [('M', 0), ('S', 1)] {
        let mut term = terminal(5, 3);
        Processor::default().advance(
            &mut term,
            format!("ABCDEZ\x1b[H\x1b[{operation}").as_bytes(),
        );
        assert_eq!(term.history_size(), history);
        assert_eq!(term.grid[Line(0)][Column(0)].c(), 'Z');
    }
}

#[test]
fn printing_over_a_wrapped_row_keeps_its_wrap_metadata() {
    for text in ["12345", "éabcd", "界abc", "ab界c", "界界a"] {
        let input = format!("ABCDEZ\x1b[H{text}");
        for chunk in [1, input.len()] {
            let mut term = terminal(5, 3);
            let mut parser = Processor::default();
            for bytes in input.as_bytes().chunks(chunk) {
                parser.advance(&mut term, bytes);
            }
            assert!(
                term.grid[Line(0)][Column(4)].wrapline(),
                "{text}, chunk {chunk}"
            );
            assert!(term.grid[Line(1)].wrap_continuation);
            assert!(term.grid.cursor.should_wrap);
        }
    }
}

#[test]
fn scrolling_down_breaks_wrap_links_in_the_region() {
    let mut term = terminal(5, 3);
    Processor::default().advance(&mut term, b"ABCDEFGHIJZ\x1b[H\x1b[T");
    for row in 0..3 {
        assert!(!term.grid[Line(row)][Column(4)].wrapline());
        assert!(!term.grid[Line(row)].wrap_continuation);
    }
}

#[test]
fn alternate_screen_exit_restores_the_resized_saved_cursor() {
    let mut term = terminal(8, 4);
    let mut parser = Processor::default();
    parser.advance(&mut term, "primary\x1b[?1049habc界DEFG".as_bytes());
    term.resize(CrosswordsSize::new(3, 4));
    term.resize(CrosswordsSize::new(12, 6));
    for row in 0..6 {
        assert!(!term.grid[Line(row)][Column(11)].wrapline());
        assert!(!term.grid[Line(row)].wrap_continuation);
    }
    parser.advance(&mut term, b"\x1b[?1049l");
    assert_eq!(term.grid.cursor.pos, Pos::new(Line(0), Column(2)));
    assert!(!term.grid.cursor.should_wrap);
}

#[test]
fn resizing_without_reflow_cuts_columns_before_trimming_rows() {
    for alternate in [false, true] {
        let mut term = terminal(5, 4);
        let prefix = if alternate { "\x1b[?1049h" } else { "\x1b[?7l" };
        Processor::default().advance(
            &mut term,
            format!("{prefix}ABC\r\nDEF\r\nGHI\x1b[4;5HZ\x1b[H").as_bytes(),
        );
        term.resize(CrosswordsSize::new(3, 2));
        check_state(&term, &["DEF", "GHI"], (0, 0));
    }
}

#[cfg(feature = "graphics")]
#[test]
fn display_erase_removes_visible_images_and_keeps_history_images() {
    let mut term = Crosswords::new(
        CrosswordsSize::new_with_dimensions(5, 3, 50, 60, 10, 20),
        CursorShape::Block,
        VoidListener,
        WindowId::from(0),
        0,
        100,
    );
    let mut parser = Processor::default();
    parser.advance(&mut term, b"\x1b_Ga=T,i=1,p=1,f=32,s=1,v=1,C=1;AQIDBA==\x1b\\\r\nx\r\nx\r\nx\x1b_Ga=T,i=2,p=1,f=32,s=1,v=1,C=1;AQIDBA==\x1b\\");
    assert_eq!(term.history_size(), 1);
    assert_eq!(term.graphics.kitty_placements.len(), 2);
    parser.advance(&mut term, b"\x1b[2J");
    assert_eq!(term.history_size(), 1);
    assert!(term.graphics.kitty_placements.contains_key(&(1, 1)));
    assert!(!term.graphics.kitty_placements.contains_key(&(2, 1)));
    assert!(term.graphics.get_kitty_image(1).is_some());
    assert!(term.graphics.get_kitty_image(2).is_none());
}

#[cfg(feature = "graphics")]
#[test]
fn kitty_resize_tracks_anchor_columns_and_clamps_blank_tails() {
    let image = "\x1b_Ga=T,i=1,p=1,f=32,s=1,v=1,C=1;AQIDBA==\x1b\\";
    for (columns, before, after, row) in [
        (8, "abcdef", "", 0),
        (8, "\x1b[2;7H", "\x1b[H", 1),
        (4, "1234界\x1b[2;1H", "\x1b[H", 0),
    ] {
        let mut term = Crosswords::new(
            CrosswordsSize::new_with_dimensions(
                columns,
                4,
                columns as u32 * 10,
                80,
                10,
                20,
            ),
            CursorShape::Block,
            VoidListener,
            WindowId::from(0),
            0,
            100,
        );
        Processor::default()
            .advance(&mut term, format!("{before}{image}{after}").as_bytes());
        term.resize(CrosswordsSize::new_with_dimensions(5, 4, 50, 80, 10, 20));
        let p = &term.graphics.kitty_placements[&(1, 1)];
        assert_eq!((p.dest_row, p.dest_col), (row, 4), "{before:?}");
    }
}

#[cfg(feature = "graphics")]
#[test]
fn kitty_anchor_on_a_wide_tail_survives_one_column_reflow() {
    let mut term = Crosswords::new(
        CrosswordsSize::new_with_dimensions(5, 3, 50, 60, 10, 20),
        CursorShape::Block,
        VoidListener,
        WindowId::from(0),
        0,
        100,
    );
    Processor::default().advance(
        &mut term,
        "界\x1b[1;2H\x1b_Ga=T,i=1,p=1,f=32,s=1,v=1,C=1;AQIDBA==\x1b\\\x1b[H".as_bytes(),
    );
    for columns in [1, 8] {
        term.resize(CrosswordsSize::new_with_dimensions(
            columns,
            3,
            columns as u32 * 10,
            60,
            10,
            20,
        ));
        let p = &term.graphics.kitty_placements[&(1, 1)];
        assert_eq!((p.dest_row, p.dest_col), (0, 0));
    }
}

#[cfg(feature = "graphics")]
#[test]
fn kitty_margin_scrolling_moves_images_but_line_edits_keep_them_fixed() {
    for (operation, row) in [
        ("\x1b[S", 1),
        ("\x1b[T", 3),
        ("\x1b[2;1H\x1b[M", 2),
        ("\x1b[2;1H\x1b[L", 2),
    ] {
        let mut term = Crosswords::new(
            CrosswordsSize::new_with_dimensions(8, 5, 80, 100, 10, 20),
            CursorShape::Block,
            VoidListener,
            WindowId::from(0),
            0,
            100,
        );
        Processor::default().advance(&mut term, format!("\x1b[3;3H\x1b_Ga=T,i=1,p=1,f=32,s=1,v=1,C=1;AQIDBA==\x1b\\\x1b[2;4r{operation}").as_bytes());
        let p = &term.graphics.kitty_placements[&(1, 1)];
        assert_eq!((p.dest_row, p.dest_col), (row, 2), "{operation:?}");
    }
}

#[cfg(feature = "graphics")]
#[test]
fn kitty_scaled_spans_subtract_the_subcell_offsets() {
    use crate::ansi::graphics::{kitty_overlay_geometry, OverlayViewport};
    for (span, width, height) in [("c=1,r=1", 2, 10), ("c=1", 2, 2), ("r=1", 10, 10)] {
        let mut term = Crosswords::new(
            CrosswordsSize::new_with_dimensions(8, 5, 80, 100, 10, 20),
            CursorShape::Block,
            VoidListener,
            WindowId::from(0),
            0,
            100,
        );
        Processor::default().advance(
            &mut term,
            format!("\x1b_Ga=T,i=1,p=1,f=32,s=1,v=1,C=1,{span},X=8,Y=10;AQIDBA==\x1b\\")
                .as_bytes(),
        );
        let p = &term.graphics.kitty_placements[&(1, 1)];
        assert_eq!((p.pixel_width, p.pixel_height), (width, height));
        let viewport = OverlayViewport {
            origin_x: 0.0,
            origin_y: 0.0,
            cell_width: 10.0,
            cell_height: 20.0,
            history_size: 0,
            display_offset: 0,
            screen_lines: 5,
        };
        let geometry = kitty_overlay_geometry(p, 1, 1, &viewport).unwrap();
        assert_eq!(
            (geometry.x, geometry.y, geometry.width, geometry.height),
            (8.0, 10.0, width as f64, height as f64)
        );
    }
}

#[cfg(feature = "graphics")]
#[test]
fn kitty_cursor_wraps_once_and_bounds_scrolling_for_large_placements() {
    // Reference: graphics_exec.zig at b2c78c9bd9bc88a16d9c3769b0fb17ddde1b78b1.
    for (position, span, cursor, history) in [
        ("\x1b[H", "c=8,r=1", (1, 0), 0),
        ("\x1b[5;1H", "c=2,r=1", (4, 2), 0),
        ("\x1b[H", "c=1,r=40", (4, 1), 5),
        ("\x1b[H", "c=4294967295,r=4294967295", (4, 0), 5),
    ] {
        let input =
            format!("{position}\x1b_Ga=T,i=1,p=1,f=32,s=1,v=1,{span};AQIDBA==\x1b\\");
        for chunk in [1, 17, input.len()] {
            let mut term = Crosswords::new(
                CrosswordsSize::new_with_dimensions(8, 5, 80, 100, 10, 20),
                CursorShape::Block,
                VoidListener,
                WindowId::from(0),
                0,
                100,
            );
            let mut parser = Processor::default();
            for bytes in input.as_bytes().chunks(chunk) {
                parser.advance(&mut term, bytes);
            }
            assert_eq!(
                (term.grid.cursor.pos.row.0, term.grid.cursor.pos.col.0),
                cursor,
                "{span}"
            );
            assert_eq!(term.history_size(), history, "{span}");
            assert!(!term.grid.cursor.should_wrap);
            assert_eq!(term.graphics.kitty_images[&1].data.pixels.len(), 4);
            if span.starts_with("c=4294967295") {
                let p = &term.graphics.kitty_placements[&(1, 1)];
                assert_eq!((p.pixel_width, p.pixel_height), (u32::MAX, u32::MAX));
            }
        }
    }
}

#[cfg(feature = "graphics")]
#[test]
fn kitty_margin_cropping_retains_source_pixels_and_image_data() {
    let mut term = Crosswords::new(
        CrosswordsSize::new_with_dimensions(8, 6, 80, 120, 10, 20),
        CursorShape::Block,
        VoidListener,
        WindowId::from(0),
        0,
        100,
    );
    let mut parser = Processor::default();
    parser.advance(&mut term, b"\x1b[2;3H\x1b_Ga=T,i=1,p=1,f=32,s=1,v=3,c=1,r=3,C=1;AQIDBAECAwQBAgME\x1b\\\x1b[2;4r");
    for (source_y, rows) in [(1, 2), (2, 1)] {
        parser.advance(&mut term, b"\x1b[S");
        let p = &term.graphics.kitty_placements[&(1, 1)];
        assert_eq!(
            (p.dest_row, p.source_y, p.source_height, p.rows),
            (1, source_y, rows, rows)
        );
        assert_eq!(p.pixel_height, rows * 20);
    }
    parser.advance(&mut term, b"\x1b[S");
    assert!(term.graphics.kitty_placements.is_empty());
    assert!(term.graphics.get_kitty_image(1).is_some());
}

#[cfg(feature = "graphics")]
#[test]
fn rejected_kitty_retransmission_removes_the_old_image_and_placement() {
    let mut term = Crosswords::new(
        CrosswordsSize::new_with_dimensions(8, 5, 80, 100, 10, 20),
        CursorShape::Block,
        VoidListener,
        WindowId::from(0),
        0,
        100,
    );
    let mut parser = Processor::default();
    parser.advance(
        &mut term,
        b"\x1b_Ga=T,i=1,p=1,f=32,s=1,v=1,C=1;AQIDBA==\x1b\\",
    );
    term.graphics.total_limit = 4;
    parser.advance(&mut term, b"\x1b_Ga=t,i=1,f=32,s=2,v=1;AQIDBAECAwQ=\x1b\\");
    assert!(term.graphics.kitty_placements.is_empty());
    assert!(term.graphics.get_kitty_image(1).is_none());
    assert_eq!(term.graphics.total_bytes, 0);
}

#[cfg(feature = "graphics")]
#[test]
fn kitty_retransmission_deletes_on_first_chunk_and_retains_accepted_data() {
    for chunk_size in [1, 17, 4096] {
        let mut term = Crosswords::new(
            CrosswordsSize::new_with_dimensions(8, 5, 80, 100, 10, 20),
            CursorShape::Block,
            VoidListener,
            WindowId::from(0),
            0,
            100,
        );
        let mut parser = Processor::default();
        for bytes in
            b"\x1b_Ga=T,i=1,p=1,f=32,s=1,v=1,C=1;AQIDBA==\x1b\\".chunks(chunk_size)
        {
            parser.advance(&mut term, bytes);
        }
        for bytes in b"\x1b_Ga=t,i=1,f=32,s=2,v=1,m=1;AQIDBA==\x1b\\".chunks(chunk_size) {
            parser.advance(&mut term, bytes);
        }
        assert!(term.graphics.kitty_placements.is_empty());
        assert!(term.graphics.get_kitty_image(1).is_none());
        for bytes in
            b"\x1b_Gi=1,m=0;AQIDBA==\x1b\\\x1b_Ga=p,i=1,p=2,C=1\x1b\\".chunks(chunk_size)
        {
            parser.advance(&mut term, bytes);
        }
        assert_eq!(term.graphics.get_kitty_image(1).unwrap().data.width, 2);
        assert!(term.graphics.kitty_placements.contains_key(&(1, 2)));
    }
}

#[cfg(feature = "graphics")]
#[test]
fn kitty_invalid_parent_keeps_new_image_but_removes_orphan_descendants() {
    let mut term = Crosswords::new(
        CrosswordsSize::new_with_dimensions(8, 5, 80, 100, 10, 20),
        CursorShape::Block,
        VoidListener,
        WindowId::from(0),
        0,
        100,
    );
    let mut parser = Processor::default();
    parser.advance(&mut term, b"\x1b_Ga=T,i=1,p=1,f=32,s=1,v=1,C=1;AQIDBA==\x1b\\\x1b_Ga=T,i=2,p=1,f=32,s=1,v=1,C=1,P=1,Q=1;AQIDBA==\x1b\\");
    parser.advance(
        &mut term,
        b"\x1b_Ga=T,i=1,p=2,f=32,s=2,v=1,C=1,P=2,Q=1;AQIDBAECAwQ=\x1b\\",
    );
    assert_eq!(term.graphics.get_kitty_image(1).unwrap().data.width, 2);
    assert!(term.graphics.get_kitty_image(2).is_none());
    assert!(term.graphics.kitty_placements.is_empty());
    parser.advance(&mut term, b"\x1b_Ga=p,i=1,p=3,C=1\x1b\\");
    assert!(term.graphics.kitty_placements.contains_key(&(1, 3)));
}

#[cfg(feature = "graphics")]
#[test]
fn kitty_numbered_and_implicit_image_ids_skip_existing_images() {
    let mut term = Crosswords::new(
        CrosswordsSize::new_with_dimensions(8, 5, 80, 100, 10, 20),
        CursorShape::Block,
        VoidListener,
        WindowId::from(0),
        0,
        100,
    );
    let mut parser = Processor::default();
    parser.advance(&mut term, b"\x1b_Ga=T,i=1,p=1,f=32,s=1,v=1,C=1;AQIDBA==\x1b\\\x1b_Ga=T,I=7,p=1,f=32,s=1,v=1,C=1;AQIDBA==\x1b\\");
    assert_eq!(term.graphics.kitty_image_numbers[&7], 2);
    parser.advance(
        &mut term,
        b"\x1b_Ga=d,d=I,i=1\x1b\\\x1b_Ga=T,I=8,p=1,f=32,s=1,v=1,C=1;AQIDBA==\x1b\\",
    );
    assert_eq!(term.graphics.kitty_image_numbers[&8], 1);
    parser.advance(&mut term, b"\x1b_Ga=T,i=2147483647,p=1,f=32,s=1,v=1,C=1;AQIDBA==\x1b\\\x1b_Ga=T,p=1,f=32,s=1,v=1,C=1;AQIDBA==\x1b\\");
    for id in [1, 2, 2147483647, 2147483648] {
        assert!(term.graphics.kitty_placements.contains_key(&(id, 1)));
    }
}

#[test]
fn scrolled_viewport_tracks_its_cell_through_repeated_reflow() {
    use crate::crosswords::grid::Scroll;
    let mut term = Crosswords::new(
        CrosswordsSize::new(8, 6),
        CursorShape::Block,
        VoidListener,
        WindowId::from(0),
        0,
        100,
    );
    let text = (0..12)
        .map(|i| format!("{i:02}abcdefghij"))
        .collect::<Vec<_>>()
        .join("\r\n");
    Processor::default().advance(&mut term, text.as_bytes());
    term.scroll_display(Scroll::Delta(1));
    term.resize(CrosswordsSize::new(5, 6));
    assert_eq!(term.history_size() - term.display_offset(), 25);
    term.scroll_display(Scroll::Delta(1));
    term.resize(CrosswordsSize::new(11, 6));
    assert_eq!(term.history_size() - term.display_offset(), 16);
    for (cols, rows, top) in [(3, 8, 41), (8, 4, 20), (12, 6, 7)] {
        term.resize(CrosswordsSize::new(cols, rows));
        assert_eq!(term.history_size() - term.display_offset(), top);
    }
    term.scroll_display(Scroll::Top);
    for cols in [5, 12, 3, 8] {
        term.resize(CrosswordsSize::new(cols, 6));
        assert_eq!(term.history_size() - term.display_offset(), 0);
    }
}

#[test]
fn cursor_reports_honor_origin_and_private_requests() {
    use crate::event::{EventListener, RioEvent};
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Clone, Default)]
    struct Replies(Rc<RefCell<String>>);
    impl EventListener for Replies {
        fn send_event(&self, event: RioEvent, _: WindowId) {
            if let RioEvent::PtyWrite(_, text) = event {
                self.0.borrow_mut().push_str(&text);
            }
        }
    }

    for (input, expected) in [
        ("\x1b[2;4H\x1b[6n", "\x1b[2;4R"),
        ("\x1b[2;3r\x1b[?6h\x1b[6n", "\x1b[1;1R"),
        ("\x1b[2;4H\x1b[?6n", "\x1b[?2;4;1R"),
        ("\x1b[2;3r\x1b[?6h\x1b[2;4H\x1b[?6n", "\x1b[?2;4;1R"),
        ("ABCDE\x1b[6n\x1b[?6n", "\x1b[1;5R\x1b[?1;5;1R"),
    ] {
        for chunk in [1, input.len()] {
            let replies = Replies::default();
            let mut term = Crosswords::new(
                CrosswordsSize::new(5, 3),
                CursorShape::Block,
                replies.clone(),
                WindowId::from(0),
                0,
                100,
            );
            let mut parser = Processor::default();
            for part in input.as_bytes().chunks(chunk) {
                parser.advance(&mut term, part);
            }
            assert_eq!(&*replies.0.borrow(), expected, "input {input:?}");
        }
    }
}

#[test]
fn screen_switches_end_active_hyperlinks() {
    let mut term = terminal(8, 3);
    let mut parser = Processor::default();
    parser.advance(&mut term, b"\x1b]8;;https://example.com\x1b\\a\x1b[?1049hb");
    assert!(term.cell_hyperlink(Line(0), Column(1)).is_none());
    parser.advance(
        &mut term,
        b"\x1b]8;;https://alt.example.com\x1b\\c\x1b[?1049ld",
    );
    assert_eq!(
        term.cell_hyperlink(Line(0), Column(0)).unwrap().uri(),
        "https://example.com"
    );
    assert!(term.cell_hyperlink(Line(0), Column(1)).is_none());
    parser.advance(&mut term, b"\x1b[?1049he");
    assert!(term.cell_hyperlink(Line(0), Column(2)).is_none());
}

#[test]
fn restoring_the_cursor_keeps_the_current_hyperlink() {
    for (before_save, after_save, expected) in [
        ("https://one.example", "", None),
        ("", "https://two.example", Some("https://two.example")),
        (
            "https://one.example",
            "https://two.example",
            Some("https://two.example"),
        ),
    ] {
        let mut term = terminal(8, 3);
        let input =
            format!("\x1b]8;;{before_save}\x1b\\a\x1b7\x1b]8;;{after_save}\x1b\\b\x1b8c");
        Processor::default().advance(&mut term, input.as_bytes());
        assert_eq!(
            term.cell_hyperlink(Line(0), Column(1))
                .as_ref()
                .map(|link| link.uri()),
            expected
        );
    }
}

// Printing, control characters, wrap cancellation, and default tab stops.
terminal_case!(plain_print, 5, b"abc", ["abc", ""], (0, 3));
terminal_case!(
    c1_ind_interrupts_csi_and_keeps_column,
    5,
    b"A\x1b[123\x84B",
    ["A", " B"],
    (1, 2)
);
terminal_case!(
    c1_nel_interrupts_csi_and_returns_column,
    5,
    b"A\x1b[123\x85B",
    ["A", "B"],
    (1, 1)
);
terminal_case!(
    c1_csi_restarts_parameter_collection,
    5,
    b"ABCDE\x1b[123\x9b3GX",
    ["ABXDE", ""],
    (0, 3)
);
terminal_case!(
    ignored_dcs_high_bytes_cannot_start_csi,
    5,
    b"A\x1bP:payload\x9b31mhidden\x1b\\B",
    ["AB", ""],
    (0, 2)
);
terminal_case!(
    apc_bell_is_payload,
    10,
    b"\x1b_opaque\x07hidden\x1b\\X",
    ["X", ""],
    (0, 1)
);
terminal_case!(
    sos_bell_is_payload,
    10,
    b"\x1bXopaque\x07hidden\x1b\\X",
    ["X", ""],
    (0, 1)
);
terminal_case!(
    pm_bell_is_payload,
    10,
    b"\x1b^opaque\x07hidden\x1b\\X",
    ["X", ""],
    (0, 1)
);
terminal_case!(
    raw_c1_in_ground_prints_replacement,
    10,
    b"A\x9b31mB",
    ["A�31mB", ""],
    (0, 6)
);
terminal_case!(
    encoded_c1_in_ground_is_ignored,
    10,
    b"A\xc2\x9b31mB",
    ["A31mB", ""],
    (0, 5)
);
terminal_case!(
    print_wraps_on_next_character,
    5,
    b"ABCDEZ",
    ["ABCDE", "Z"],
    (1, 1)
);
terminal_case!(
    print_scrolls_at_bottom,
    5,
    b"A\r\nB\r\nC",
    ["B", "C"],
    (1, 1)
);
terminal_case!(
    disabled_wrap_overwrites_last_column,
    5,
    b"\x1b[?7lABCDEFG",
    ["ABCDG", ""],
    (0, 4)
);
terminal_case!(linefeed_keeps_column, 5, b"A\nB", ["A", " B"], (1, 2));
terminal_case!(
    linefeed_cancels_pending_wrap,
    5,
    b"ABCDE\nX",
    ["ABCDE", "    X", ""],
    (1, 4)
);
terminal_case!(
    newline_mode_returns_to_column_zero,
    5,
    b"A\x1b[20h\nB",
    ["A", "B"],
    (1, 1)
);
terminal_case!(
    carriage_return_cancels_pending_wrap,
    5,
    b"ABCDE\rX",
    ["XBCDE", ""],
    (0, 1)
);
terminal_case!(
    backspace_overwrites_previous_cell,
    5,
    b"ABC\x08X",
    ["ABX", ""],
    (0, 3)
);
terminal_case!(
    backspace_stops_at_column_zero,
    5,
    b"\x08X",
    ["X", ""],
    (0, 1)
);
terminal_case!(
    tab_uses_default_eight_cell_stops,
    20,
    b"a\tb\tc\tD",
    ["a       b       c  D"],
    (0, 19)
);
terminal_case!(
    tab_from_stop_advances_to_next_stop,
    20,
    b"\x1b[9G\tX",
    ["                X"],
    (0, 17)
);
terminal_case!(
    back_tab_from_stop_uses_previous_stop,
    20,
    b"\x1b[17G\x1b[ZX",
    ["        X"],
    (0, 9)
);
terminal_case!(
    back_tab_without_stops_reaches_zero,
    20,
    b"\x1b[3g\x1b[17G\x1b[ZX",
    ["X"],
    (0, 1)
);
terminal_case!(
    back_tab_cancels_pending_wrap,
    5,
    b"ABCDE\x1b[ZX",
    ["XBCDE", ""],
    (0, 1)
);
terminal_case!(
    tab_clear_current_preserves_other_stops,
    20,
    b"\x1b[9G\x1b[g\r\tX",
    ["                X"],
    (0, 17)
);
terminal_case!(
    tab_clear_all_reaches_right_edge,
    20,
    b"\x1b[3g\tX",
    ["                   X"],
    (0, 19)
);
terminal_case!(
    tab_at_pending_wrap_does_not_linefeed,
    5,
    b"ABCDE\t\rX",
    ["XBCDE", ""],
    (0, 1)
);
terminal_case!(custom_tab_stop, 20, b"\x1b[4G\x1bH\r\tX", ["   X"], (0, 4));
terminal_case!(
    forward_tab_count,
    20,
    b"\x1b[2IX",
    ["                X"],
    (0, 17)
);
terminal_case!(
    backward_tab_count,
    20,
    b"\x1b[20G\x1b[2ZX",
    ["        X"],
    (0, 9)
);

// CUP is origin-relative. Relative motions and CHA must not add that origin
// again, and vertical movement must stop at a margin approached from inside.
terminal_case!(
    cursor_position_clamps_to_screen,
    5,
    b"\x1b[999;999HX",
    ["", "", "    X"],
    (2, 4)
);
terminal_case!(
    cursor_position_default_parameters,
    5,
    b"\x1b[3;4H\x1b[;HX",
    ["X", "", ""],
    (0, 1)
);
terminal_case!(
    cursor_up_stops_at_top_margin,
    5,
    b"\x1b[2;4r\x1b[3;2H\x1b[99AX",
    ["", " X", "", "", ""],
    (1, 2)
);
terminal_case!(
    cursor_up_above_margin_stops_at_screen_edge,
    5,
    b"\x1b[2;4r\x1b[1;2H\x1b[99AX",
    [" X", "", "", "", ""],
    (0, 2)
);
terminal_case!(
    cursor_down_stops_at_bottom_margin,
    5,
    b"\x1b[2;4r\x1b[3;2H\x1b[99BX",
    ["", "", "", " X", ""],
    (3, 2)
);
terminal_case!(
    cursor_down_below_margin_stops_at_screen_edge,
    5,
    b"\x1b[2;4r\x1b[5;2H\x1b[99BX",
    ["", "", "", "", " X"],
    (4, 2)
);
terminal_case!(
    origin_cursor_position,
    5,
    b"\x1b[2;4r\x1b[?6h\x1b[2;2HX",
    ["", "", " X", "", ""],
    (2, 2)
);
terminal_case!(
    origin_mode_enable_homes_cursor_to_margin,
    5,
    b"\x1b[2;4r\x1b[5;4H\x1b[?6hX",
    ["", "X", "", "", ""],
    (1, 1)
);
terminal_case!(
    origin_mode_disable_homes_cursor_to_screen,
    5,
    b"\x1b[2;4r\x1b[?6h\x1b[2;3H\x1b[?6lX",
    ["X", "", "", "", ""],
    (0, 1)
);
terminal_case!(
    origin_column_address_preserves_row,
    5,
    b"\x1b[2;4r\x1b[?6h\x1b[2;1H\x1b[3GX",
    ["", "", "  X", "", ""],
    (2, 3)
);
terminal_case!(
    origin_relative_up_does_not_add_offset,
    5,
    b"\x1b[2;4r\x1b[?6h\x1b[3;1H\x1b[AX",
    ["", "", "X", "", ""],
    (2, 1)
);
terminal_case!(
    origin_relative_down_does_not_add_offset,
    5,
    b"\x1b[2;4r\x1b[?6h\x1b[BX",
    ["", "", "X", "", ""],
    (2, 1)
);
terminal_case!(
    origin_next_line_does_not_add_offset,
    5,
    b"\x1b[2;4r\x1b[?6h\x1b[1;3H\x1b[EX",
    ["", "", "X", "", ""],
    (2, 1)
);
terminal_case!(
    origin_previous_line_does_not_add_offset,
    5,
    b"\x1b[2;4r\x1b[?6h\x1b[3;3H\x1b[FX",
    ["", "", "X", "", ""],
    (2, 1)
);
terminal_case!(
    cursor_forward_cancels_pending_wrap,
    5,
    b"ABCDE\x1b[CX",
    ["ABCDX", ""],
    (0, 4)
);
terminal_case!(
    cursor_backward_cancels_pending_wrap,
    5,
    b"ABCDE\x1b[DX",
    ["ABCXE", ""],
    (0, 4)
);
terminal_case!(
    cursor_up_cancels_pending_wrap,
    5,
    b"ABCDE\x1b[AX",
    ["ABCDX", ""],
    (0, 4)
);
terminal_case!(
    cursor_down_cancels_pending_wrap,
    5,
    b"ABCDE\x1b[BX",
    ["ABCDE", "    X"],
    (1, 4)
);
terminal_case!(
    next_line_returns_to_column_zero,
    5,
    b"\x1b[1;4H\x1b[EX",
    ["", "X", ""],
    (1, 1)
);
terminal_case!(
    previous_line_returns_to_column_zero,
    5,
    b"\x1b[3;4H\x1b[FX",
    ["", "X", ""],
    (1, 1)
);

// Character editing: counts are clipped to the remaining columns.
terminal_case!(
    erase_chars_default_count,
    5,
    b"ABCDE\x1b[3G\x1b[X",
    ["AB DE", ""],
    (0, 2)
);
terminal_case!(
    erase_chars_zero_means_one,
    5,
    b"ABCDE\x1b[3G\x1b[0X",
    ["AB DE", ""],
    (0, 2)
);
terminal_case!(
    erase_chars_clips_at_edge,
    5,
    b"ABCDE\x1b[4G\x1b[999X",
    ["ABC", ""],
    (0, 3)
);
terminal_case!(
    erase_chars_cancels_pending_wrap,
    5,
    b"ABCDE\x1b[XX",
    ["ABCDX", ""],
    (0, 4)
);
terminal_case!(
    delete_chars_shifts_tail,
    5,
    b"ABCDE\x1b[2G\x1b[2P",
    ["ADE", ""],
    (0, 1)
);
terminal_case!(
    delete_chars_zero_means_one,
    5,
    b"ABCDE\x1b[2G\x1b[0P",
    ["ACDE", ""],
    (0, 1)
);
terminal_case!(
    delete_chars_large_count_keeps_prefix,
    5,
    b"ABCDE\x1b[4G\x1b[999P",
    ["ABC", ""],
    (0, 3)
);
terminal_case!(
    delete_chars_half_width_keeps_prefix,
    5,
    b"ABCDE\x1b[4G\x1b[3P",
    ["ABC", ""],
    (0, 3)
);
terminal_case!(
    delete_chars_cancels_pending_wrap,
    5,
    b"ABCDE\x1b[PX",
    ["ABCDX", ""],
    (0, 4)
);
terminal_case!(
    insert_blanks_shifts_tail,
    5,
    b"ABCDE\x1b[2G\x1b[2@",
    ["A  BC", ""],
    (0, 1)
);
terminal_case!(
    insert_blanks_zero_means_one,
    5,
    b"ABCDE\x1b[2G\x1b[0@",
    ["A BCD", ""],
    (0, 1)
);
terminal_case!(
    insert_blanks_clips_at_edge,
    5,
    b"ABCDE\x1b[4G\x1b[999@",
    ["ABC", ""],
    (0, 3)
);
terminal_case!(
    insert_blanks_cancels_pending_wrap,
    5,
    b"ABCDE\x1b[@X",
    ["ABCDX", ""],
    (0, 4)
);
terminal_case!(
    insert_mode_drops_pushed_characters,
    5,
    b"ABCDE\x1b[2G\x1b[4hX",
    ["AXBCD", ""],
    (0, 2)
);
terminal_case!(
    repeat_previous_character,
    5,
    b"A\x1b[3b",
    ["AAAA", ""],
    (0, 4)
);
terminal_case!(
    repeat_without_previous_character_is_noop,
    5,
    b"\x1b[3bX",
    ["X", ""],
    (0, 1)
);

// Row editing and scrolling. Explicit row contents make movement observable.
terminal_case!(
    insert_lines_moves_rows_down,
    5,
    b"A\r\nB\r\nC\r\nD\r\nE\x1b[2;3H\x1b[L",
    ["A", "", "B", "C", "D"],
    (1, 0)
);
terminal_case!(
    insert_lines_large_count_stops_at_region,
    5,
    b"A\r\nB\r\nC\r\nD\r\nE\x1b[2;4r\x1b[3;2H\x1b[999L",
    ["A", "B", "", "", "E"],
    (2, 0)
);
terminal_case!(
    insert_lines_outside_region_does_nothing,
    5,
    b"A\x1b[2;4r\x1b[1;3H\x1b[L",
    ["A", "", "", "", ""],
    (0, 2)
);
terminal_case!(
    insert_lines_cancels_pending_wrap,
    5,
    b"ABCDE\x1b[LX",
    ["X", "ABCDE", ""],
    (0, 1)
);
terminal_case!(
    delete_lines_moves_rows_up,
    5,
    b"A\r\nB\r\nC\r\nD\r\nE\x1b[2;3H\x1b[M",
    ["A", "C", "D", "E", ""],
    (1, 0)
);
terminal_case!(
    delete_lines_large_count_stops_at_region,
    5,
    b"A\r\nB\r\nC\r\nD\r\nE\x1b[2;4r\x1b[3;2H\x1b[999M",
    ["A", "B", "", "", "E"],
    (2, 0)
);
terminal_case!(
    delete_lines_outside_region_does_nothing,
    5,
    b"A\x1b[2;4r\x1b[1;3H\x1b[M",
    ["A", "", "", "", ""],
    (0, 2)
);
terminal_case!(
    delete_lines_cancels_pending_wrap,
    5,
    b"ABCDE\x1b[MX",
    ["X", "", ""],
    (0, 1)
);
terminal_case!(
    scroll_up_preserves_cursor,
    5,
    b"A\r\nB\r\nC\x1b[1;3H\x1b[S",
    ["B", "C", ""],
    (0, 2)
);
terminal_case!(
    scroll_down_preserves_cursor,
    5,
    b"A\r\nB\r\nC\x1b[2;3H\x1b[T",
    ["", "A", "B"],
    (1, 2)
);
terminal_case!(
    index_scrolls_only_region,
    5,
    b"A\r\nB\r\nC\r\nD\r\nE\x1b[2;4r\x1b[4;1H\x1bD",
    ["A", "C", "D", "", "E"],
    (3, 0)
);
terminal_case!(
    reverse_index_scrolls_only_region,
    5,
    b"A\r\nB\r\nC\r\nD\r\nE\x1b[2;4r\x1b[2;1H\x1bM",
    ["A", "", "B", "C", "E"],
    (1, 0)
);

// Erasure must leave the cursor in place, including when the display is empty.
terminal_case!(
    erase_line_right,
    5,
    b"ABCDE\x1b[3G\x1b[K",
    ["AB", ""],
    (0, 2)
);
terminal_case!(
    erase_line_left,
    5,
    b"ABCDE\x1b[3G\x1b[1K",
    ["   DE", ""],
    (0, 2)
);
terminal_case!(
    erase_line_complete,
    5,
    b"ABCDE\x1b[3G\x1b[2K",
    ["", ""],
    (0, 2)
);
terminal_case!(
    erase_line_cancels_pending_wrap,
    5,
    b"ABCDE\x1b[KX",
    ["ABCDX", ""],
    (0, 4)
);
terminal_case!(
    erase_display_below,
    5,
    b"AAAA\r\nBBBB\r\nCCCC\x1b[2;3H\x1b[J",
    ["AAAA", "BB", ""],
    (1, 2)
);
terminal_case!(
    erase_display_above,
    5,
    b"AAAA\r\nBBBB\r\nCCCC\x1b[2;3H\x1b[1J",
    ["", "   B", "CCCC"],
    (1, 2)
);
terminal_case!(
    erase_display_complete_preserves_cursor,
    5,
    b"AAAA\r\nBBBB\r\nCCCC\x1b[2;3H\x1b[2J",
    ["", "", ""],
    (1, 2)
);
terminal_case!(
    restore_cursor_position,
    5,
    b"\x1b[2;3H\x1b7\x1b[1;1H\x1b8X",
    ["", "  X", ""],
    (1, 3)
);
terminal_case!(
    restore_cursor_pending_wrap,
    5,
    b"ABCDE\x1b7\r\x1b8X",
    ["ABCDE", "X"],
    (1, 1)
);
terminal_case!(
    full_reset_clears_saved_cursor,
    5,
    b"\x1b[2;3H\x1b7\x1bc\x1b8X",
    ["X", "", ""],
    (0, 1)
);

fn rgb(r: u8, g: u8, b: u8) -> AnsiColor {
    AnsiColor::Spec(ColorRgb { r, g, b })
}

macro_rules! erasure_case {
    ($name:ident, $position:expr, $command:expr, $regions:expr) => {
        #[test]
        fn $name() {
            let input = format!(
                "\x1b]8;;https://example.test\x1b\\123456\r\nabcdef\r\nUVWXYZ{}\x1b[1;3;4;7;38;5;196;48;2;10;20;30m{}",
                $position, $command
            );
            for split in 0..=input.len() {
                let mut term = terminal(6, 3);
                let mut parser = Processor::default();
                parser.advance(&mut term, &input.as_bytes()[..split]);
                parser.advance(&mut term, &input.as_bytes()[split..]);
                let expected = Style { bg: rgb(10, 20, 30), ..Style::default() };
                for (row, columns) in $regions {
                    for column in columns {
                        let cell = term.grid[Line(row)][Column(column)];
                        assert_eq!(cell.c(), '\0', "blank at {row}:{column}");
                        assert_eq!(term.grid.style_of(&cell), expected, "style at {row}:{column}");
                        assert!(!cell.has_extras(), "erased hyperlink at {row}:{column}");
                    }
                }
            }
        }
    };
}

// Ghostty's background-SGR cases check the blank cell's actual color. These
// also ensure erasure drops foreground attributes and hyperlink ownership.
erasure_case!(
    erase_right_preserves_only_background,
    "\x1b[2;3H",
    "\x1b[K",
    [(1, 2..6)]
);
erasure_case!(
    erase_left_preserves_only_background,
    "\x1b[2;3H",
    "\x1b[1K",
    [(1, 0..3)]
);
erasure_case!(
    erase_whole_line_preserves_only_background,
    "\x1b[2;3H",
    "\x1b[2K",
    [(1, 0..6)]
);
erasure_case!(
    erase_characters_preserves_only_background,
    "\x1b[2;3H",
    "\x1b[2X",
    [(1, 2..4)]
);
erasure_case!(
    insert_blanks_preserves_only_background,
    "\x1b[2;3H",
    "\x1b[2@",
    [(1, 2..4)]
);
erasure_case!(
    delete_characters_preserves_only_background,
    "\x1b[2;3H",
    "\x1b[2P",
    [(1, 4..6)]
);
erasure_case!(
    insert_rows_preserves_only_background,
    "\x1b[2;3H",
    "\x1b[L",
    [(1, 0..6)]
);
erasure_case!(
    delete_rows_preserves_only_background,
    "\x1b[2;3H",
    "\x1b[M",
    [(2, 0..6)]
);
erasure_case!(
    scroll_up_preserves_only_background,
    "\x1b[2;3H",
    "\x1b[S",
    [(2, 0..6)]
);
erasure_case!(
    scroll_down_preserves_only_background,
    "\x1b[2;3H",
    "\x1b[T",
    [(0, 0..6)]
);
erasure_case!(
    index_preserves_only_background,
    "\x1b[3;3H",
    "\x1bD",
    [(2, 0..6)]
);
erasure_case!(
    reverse_index_preserves_only_background,
    "\x1b[1;3H",
    "\x1bM",
    [(0, 0..6)]
);
erasure_case!(
    erase_below_preserves_only_background,
    "\x1b[2;3H",
    "\x1b[J",
    [(1, 2..6), (2, 0..6)]
);
erasure_case!(
    erase_above_preserves_only_background,
    "\x1b[2;3H",
    "\x1b[1J",
    [(0, 0..6), (1, 0..3)]
);
erasure_case!(
    erase_display_preserves_only_background,
    "\x1b[2;3H",
    "\x1b[2J",
    [(0, 0..6), (1, 0..6), (2, 0..6)]
);

fn check_sgr(parameters: &str, expected: Style) {
    let input = format!("\x1b[{parameters}mX");
    for split in 0..=input.len() {
        let mut term = terminal(5, 2);
        let mut parser = Processor::default();
        parser.advance(&mut term, &input.as_bytes()[..split]);
        parser.advance(&mut term, &input.as_bytes()[split..]);
        let cell = term.grid[Line(0)][Column(0)];
        assert_eq!(cell.c(), 'X');
        assert_eq!(term.grid.style_of(&cell), expected, "SGR {parameters}");
    }
}

macro_rules! sgr_case {
    ($name:ident, $parameters:expr, $expected:expr) => {
        #[test]
        fn $name() {
            check_sgr($parameters, $expected);
        }
    };
}

sgr_case!(sgr_empty_is_reset, "", Style::default());
sgr_case!(
    sgr_bold,
    "1",
    Style {
        flags: StyleFlags::BOLD,
        ..Style::default()
    }
);
sgr_case!(
    sgr_dim,
    "2",
    Style {
        flags: StyleFlags::DIM,
        ..Style::default()
    }
);
sgr_case!(
    sgr_italic,
    "3",
    Style {
        flags: StyleFlags::ITALIC,
        ..Style::default()
    }
);
sgr_case!(
    sgr_underline,
    "4",
    Style {
        flags: StyleFlags::UNDERLINE,
        ..Style::default()
    }
);
sgr_case!(
    sgr_double_underline,
    "4:2",
    Style {
        flags: StyleFlags::DOUBLE_UNDERLINE,
        ..Style::default()
    }
);
sgr_case!(
    sgr_curly_underline,
    "4:3",
    Style {
        flags: StyleFlags::UNDERCURL,
        ..Style::default()
    }
);
sgr_case!(
    sgr_dotted_underline,
    "4:4",
    Style {
        flags: StyleFlags::DOTTED_UNDERLINE,
        ..Style::default()
    }
);
sgr_case!(
    sgr_dashed_underline,
    "4:5",
    Style {
        flags: StyleFlags::DASHED_UNDERLINE,
        ..Style::default()
    }
);
sgr_case!(
    sgr_underline_kind_replaces_previous,
    "4:2;4:3",
    Style {
        flags: StyleFlags::UNDERCURL,
        ..Style::default()
    }
);
sgr_case!(sgr_underline_zero_clears_kind, "4:3;4:0", Style::default());
sgr_case!(sgr_underline_off_clears_kind, "4:5;24", Style::default());
sgr_case!(
    sgr_slow_blink,
    "5",
    Style {
        flags: StyleFlags::SLOW_BLINK,
        ..Style::default()
    }
);
sgr_case!(
    sgr_rapid_blink,
    "6",
    Style {
        flags: StyleFlags::RAPID_BLINK,
        ..Style::default()
    }
);
sgr_case!(
    sgr_inverse,
    "7",
    Style {
        flags: StyleFlags::INVERSE,
        ..Style::default()
    }
);
sgr_case!(
    sgr_hidden,
    "8",
    Style {
        flags: StyleFlags::HIDDEN,
        ..Style::default()
    }
);
sgr_case!(
    sgr_strikethrough,
    "9",
    Style {
        flags: StyleFlags::STRIKEOUT,
        ..Style::default()
    }
);
sgr_case!(
    sgr_intensity_reset_clears_bold_and_dim,
    "1;2;22",
    Style::default()
);
sgr_case!(
    sgr_individual_resets,
    "1;3;4:3;5;7;8;9;22;23;24;25;27;28;29",
    Style::default()
);
sgr_case!(
    sgr_standard_foreground,
    "31",
    Style {
        fg: AnsiColor::Named(NamedColor::Red),
        ..Style::default()
    }
);
sgr_case!(
    sgr_bright_background,
    "104",
    Style {
        bg: AnsiColor::Named(NamedColor::LightBlue),
        ..Style::default()
    }
);
sgr_case!(
    sgr_indexed_foreground,
    "38;5;201",
    Style {
        fg: AnsiColor::Indexed(201),
        ..Style::default()
    }
);
sgr_case!(
    sgr_indexed_background_colon,
    "48:5:42",
    Style {
        bg: AnsiColor::Indexed(42),
        ..Style::default()
    }
);
sgr_case!(
    sgr_rgb_foreground_semicolons,
    "38;2;10;20;30",
    Style {
        fg: rgb(10, 20, 30),
        ..Style::default()
    }
);
sgr_case!(
    sgr_rgb_background_colons,
    "48:2:10:20:30",
    Style {
        bg: rgb(10, 20, 30),
        ..Style::default()
    }
);
sgr_case!(
    sgr_rgb_empty_color_space,
    "38:2::10:20:30",
    Style {
        fg: rgb(10, 20, 30),
        ..Style::default()
    }
);
sgr_case!(
    sgr_rgb_explicit_color_space,
    "38:2:0:10:20:30",
    Style {
        fg: rgb(10, 20, 30),
        ..Style::default()
    }
);
sgr_case!(
    sgr_underline_color,
    "58:2::10:20:30",
    Style {
        underline_color: Some(rgb(10, 20, 30)),
        ..Style::default()
    }
);
sgr_case!(sgr_underline_color_reset, "58;5;42;59", Style::default());
sgr_case!(sgr_color_resets, "31;44;39;49", Style::default());
sgr_case!(
    sgr_reset_drops_all_attributes,
    "1;3;4:3;7;38;5;42;48;2;10;20;30;58;5;5;0",
    Style::default()
);
sgr_case!(
    sgr_mixed_separators,
    "4:3;38:2::10:20:30;48;5;42",
    Style {
        fg: rgb(10, 20, 30),
        bg: AnsiColor::Indexed(42),
        flags: StyleFlags::UNDERCURL,
        ..Style::default()
    }
);
sgr_case!(
    sgr_unknown_attribute_does_not_hide_following_bold,
    "999;1",
    Style {
        flags: StyleFlags::BOLD,
        ..Style::default()
    }
);

fn text_and_cells(
    term: &Crosswords<VoidListener>,
) -> (String, Vec<(char, Style, Option<Extras>)>) {
    let top = -(term.history_size() as i32);
    let text = term.bounds_to_string(
        Pos::new(Line(top), Column(0)),
        Pos::new(term.grid.bottommost_line(), term.grid.last_column()),
    );
    let mut cells = Vec::new();
    for row in top..=term.grid.bottommost_line().0 {
        for column in 0..term.grid.columns() {
            let cell = term.grid[Line(row)][Column(column)];
            if cell.c() == '\0'
                || matches!(cell.wide(), Wide::Spacer | Wide::LeadingSpacer)
            {
                continue;
            }
            let extras = cell
                .extras_id()
                .and_then(|id| term.grid.extras_table.get(id))
                .cloned();
            cells.push((cell.c(), term.grid.style_of(&cell), extras));
        }
    }
    (text, cells)
}

fn check_reflow(input: &str, expected_text: &str) {
    for original_columns in [4, 7, 12] {
        for original_rows in [2, 5] {
            let mut term = terminal(original_columns, original_rows);
            let mut parser = Processor::default();
            parser.advance(&mut term, input.as_bytes());
            let expected = text_and_cells(&term);
            assert_eq!(expected.0, expected_text);
            for (columns, rows) in [
                (3, 3),
                (11, 1),
                (4, 6),
                (15, 2),
                (7, 5),
                (original_columns, original_rows),
            ] {
                term.resize(CrosswordsSize::new(columns, rows));
                assert_eq!(
                    text_and_cells(&term),
                    expected,
                    "resize to {columns}x{rows} from {original_columns}x{original_rows}"
                );
                assert!(term.grid.cursor.pos.row.0 >= 0);
                assert!(term.grid.cursor.pos.row.0 < rows as i32);
                assert!(term.grid.cursor.pos.col.0 < columns);
            }
        }
    }
}

#[test]
fn repeated_reflow_preserves_ascii_and_hard_line_breaks() {
    check_reflow(
        "abcdefghijklmnop\r\nqrstuvwxyz",
        "abcdefghijklmnop\nqrstuvwxyz",
    );
}

#[test]
fn repeated_reflow_preserves_blank_lines_and_explicit_spaces() {
    check_reflow("alpha beta\r\n\r\ngamma delta", "alpha beta\n\ngamma delta");
}

#[test]
fn repeated_reflow_preserves_wide_characters_at_odd_widths() {
    check_reflow("a界b界c界d界e界f界g界", "a界b界c界d界e界f界g界");
}

#[test]
fn repeated_reflow_preserves_graphemes() {
    check_reflow("\x1b[?2027he\u{301}👩‍💻a\u{308}👨‍👩‍👧‍👦z", "e\u{301}👩‍💻a\u{308}👨‍👩‍👧‍👦z");
}

#[test]
fn repeated_reflow_preserves_resolved_styles_and_hyperlinks() {
    check_reflow(
        "\x1b[31;1mredtext\x1b]8;;https://example.test/one\x1b\\linkedtext\x1b[0m\x1b]8;;https://example.test/two\x1b\\secondlink\x1b]8;;\x1b\\plain",
        "redtextlinkedtextsecondlinkplain",
    );
}

#[test]
fn saved_cursor_follows_its_cell_when_widening() {
    let mut term = terminal(2, 3);
    let mut parser = Processor::default();
    parser.advance(&mut term, b"1A2B\x1b[2;2H\x1b7");
    term.resize(CrosswordsSize::new(5, 3));
    parser.advance(&mut term, b"\x1b8X");
    check_state(&term, &["1A2X", "", ""], (0, 4));
}

#[test]
fn saved_pending_wrap_becomes_a_normal_position_when_widening() {
    let mut term = terminal(2, 3);
    let mut parser = Processor::default();
    parser.advance(&mut term, b"1A2B\x1b7");
    term.resize(CrosswordsSize::new(5, 3));
    parser.advance(&mut term, b"\x1b8X");
    check_state(&term, &["1A2BX", "", ""], (0, 4));
}

#[test]
fn saved_cursor_follows_its_cell_when_narrowing() {
    let mut term = terminal(6, 4);
    let mut parser = Processor::default();
    parser.advance(&mut term, b"ABCDEF\x1b[1;5H\x1b7");
    term.resize(CrosswordsSize::new(3, 4));
    parser.advance(&mut term, b"\x1b8");
    assert_eq!(
        term.grid[term.grid.cursor.pos.row][term.grid.cursor.pos.col].c(),
        'E'
    );
    parser.advance(&mut term, b"X");
    assert_eq!(text_and_cells(&term).0, "ABCDXF");
    assert_eq!(term.grid.cursor.pos.col, Column(2));
}

#[test]
fn saved_cursor_survives_repeated_widening() {
    let mut term = terminal(2, 4);
    let mut parser = Processor::default();
    parser.advance(&mut term, b"1A2B3C\x1b[3;2H\x1b7");
    for columns in [3, 4, 5, 8] {
        term.resize(CrosswordsSize::new(columns, 4));
    }
    parser.advance(&mut term, b"\x1b8X");
    assert_eq!(text_and_cells(&term).0, "1A2B3X");
}

#[test]
fn saved_cursor_tracks_wide_char_displacement_independently_of_live_cursor() {
    for text in ["abcdefghijklmnoQ", "a界b界c界d界Q", "a界界界Q", "abc界Q"] {
        for original_columns in [4, 7, 12] {
            let mut term = terminal(original_columns, 6);
            let mut parser = Processor::default();
            parser.advance(&mut term, text.as_bytes());
            let target = (0..6)
                .find_map(|row| {
                    (0..original_columns).find_map(|column| {
                        (term.grid[Line(row)][Column(column)].c() == 'Q')
                            .then_some(Pos::new(Line(row), Column(column)))
                    })
                })
                .unwrap();
            parser.advance(
                &mut term,
                format!(
                    "\x1b[{};{}H\x1b7\x1b[1;1H",
                    target.row.0 + 1,
                    target.col.0 + 1
                )
                .as_bytes(),
            );
            for columns in [3, 11, 5, 15, 4] {
                term.resize(CrosswordsSize::new(columns, 6));
                assert_eq!(
                    text_and_cells(&term).0,
                    text,
                    "content after resizing to {columns} from {original_columns}"
                );
                parser.advance(&mut term, b"\x1b8");
                assert_eq!(term.grid[term.grid.cursor.pos.row][term.grid.cursor.pos.col].c(), 'Q', "saved Q after resizing {text:?} to {columns} from {original_columns}");
                parser.advance(&mut term, b"\x1b[1;1H");
            }
            parser.advance(&mut term, b"\x1b8#");
            assert_eq!(text_and_cells(&term).0, text.replace('Q', "#"));
        }
    }
}
