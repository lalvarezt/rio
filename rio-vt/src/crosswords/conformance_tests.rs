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

macro_rules! terminal_case {
    ($name:ident, $cols:expr, $input:expr, $rows:expr, $cursor:expr) => {
        #[test]
        fn $name() {
            check_case($cols, $input, &$rows, $cursor);
        }
    };
}

// Printing, control characters, wrap cancellation, and default tab stops.
terminal_case!(plain_print, 5, b"abc", ["abc", ""], (0, 3));
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
