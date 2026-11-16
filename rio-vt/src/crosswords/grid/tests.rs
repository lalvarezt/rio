// grid/tests.rs was originally taken from Alacritty
// https://github.com/alacritty/alacritty/blob/e35e5ad14fce8456afdd89f2b392b9924bb27471/alacritty_terminal/src/grid/tests.rs
// which is licensed under Apache 2.0 license.

use super::*;

use crate::crosswords::square::Square;

#[test]
fn history_byte_budget_tracks_width_and_can_be_disabled() {
    for columns in [1, 80, 4096] {
        let mut grid = Grid::<Square>::new(3, columns, 100);
        let row_bytes = Grid::<Square>::logical_row_bytes(columns);
        let budget = grid.style_set.logical_bytes() + 2 * row_bytes;
        grid.set_history_byte_limit(Some(budget));
        for _ in 0..20 {
            grid.scroll_up(&(Line(0)..Line(3)), 1);
        }
        assert_eq!(grid.history_size(), 2);
        assert_eq!(grid.history_budget_charge(), budget);
        grid.cursor.pos = Pos::new(Line(1), Column(0));
        grid[Line(1)][Column(0)].set_c('x');
        grid.resize(false, 3, columns * 2);
        assert!(grid.history_budget_charge() <= budget);
        assert_eq!(grid[Line(1)][Column(0)].c(), 'x');
        assert_eq!(grid.cursor.pos, Pos::new(Line(1), Column(0)));
        grid.set_history_byte_limit(Some(0));
        assert_eq!(grid.history_size(), 0);
        grid.set_history_byte_limit(None);
        for _ in 0..10 {
            grid.scroll_up(&(Line(0)..Line(3)), 1);
        }
        assert_eq!(grid.history_size(), 10);
        grid.update_history(1);
        assert_eq!(grid.history_size(), 1);
        grid.set_history_byte_limit(Some(usize::MAX));
        grid.scroll_up(&(Line(0)..Line(3)), 1);
        assert_eq!(grid.history_size(), 1);
    }
}

#[test]
fn history_budget_charges_shared_extras_once_and_keeps_live_ids() {
    use crate::crosswords::square::{Extras, Hyperlink};
    let mut grid = Grid::<Square>::new(3, 80, 100);
    let row_bytes = Grid::<Square>::logical_row_bytes(80);
    grid.set_history_byte_limit(Some(grid.style_set.logical_bytes() + row_bytes * 4));
    for _ in 0..10 {
        grid.scroll_up(&(Line(0)..Line(3)), 1);
    }
    let extras = Extras {
        hyperlink: Some(Hyperlink::new(Some("id"), "https://example.com")),
        zerowidth: vec!['\u{301}'],
    };
    let id = grid.alloc_extras(extras.clone());
    let charged = grid.extras_table.payload_bytes;
    assert_eq!(grid.alloc_extras(extras.clone()), id);
    assert_eq!(grid.extras_table.payload_bytes, charged);
    grid.enforce_history_byte_limit();
    assert_eq!(grid.history_size(), 3);
    grid[Line(0)][Column(0)].set_extras_id(Some(id));
    grid[Line(0)].has_extras = true;
    grid.set_history_byte_limit(Some(0));
    grid.reclaim_extras();
    assert_eq!(grid.extras_table.get(id), Some(&extras));
    assert!(Grid::<Square>::logical_row_bytes(usize::MAX) >= row_bytes);
}

#[test]
fn extras_intern_matches_fields_for_shared_and_distinct_links() {
    use crate::crosswords::square::{Extras, Hyperlink};
    let link = Hyperlink::new(Some("id"), "https://example.com");
    let links = [
        None,
        Some(link.clone()),
        Some(link),
        Some(Hyperlink::new(Some("id"), "https://example.com")),
        Some(Hyperlink::new(Some("other"), "https://example.com")),
        Some(Hyperlink::new(Some("id"), "https://example.org")),
    ];
    let clusters = [
        vec![],
        vec!['\u{301}'],
        vec!['\u{302}'],
        vec!['\u{301}', '\u{302}'],
    ];
    for left_link in &links {
        for right_link in &links {
            for left_cluster in &clusters {
                for right_cluster in &clusters {
                    let left = Extras {
                        zerowidth: left_cluster.clone(),
                        hyperlink: left_link.clone(),
                    };
                    let right = Extras {
                        zerowidth: right_cluster.clone(),
                        hyperlink: right_link.clone(),
                    };
                    let mut table = ExtrasTable::new();
                    let left_id = table.intern(&left);
                    let right_id = table.intern(&right);
                    assert_eq!(
                        left_id == right_id,
                        left_cluster == right_cluster && left_link == right_link
                    );
                    assert_eq!(table.intern(&left), left_id);
                    assert_eq!(table.get(left_id), Some(&left));
                    assert_eq!(table.get(right_id), Some(&right));
                }
            }
        }
    }
}

#[test]
fn extras_last_allocation_survives_sweep_and_slot_reuse() {
    use crate::crosswords::square::Extras;
    let mut table = ExtrasTable::new();
    let first = Extras {
        zerowidth: vec!['\u{301}'],
        ..Extras::default()
    };
    let second = Extras {
        zerowidth: vec!['\u{308}'],
        ..Extras::default()
    };
    let id = table.alloc(first.clone());
    assert_eq!(table.intern(&first), id);
    table.sweep_unmarked(&vec![0; ID_BITSET_WORDS]);
    assert_eq!(table.intern(&second), id);
    let other = table.intern(&first);
    assert_ne!(other, id);
    assert_eq!(table.get(id), Some(&second));
    assert_eq!(table.get(other), Some(&first));
}

impl GridSquare for usize {
    fn is_empty(&self) -> bool {
        *self == 0
    }

    fn reset(&mut self, template: &Self, _: &mut crate::crosswords::style::StyleSet) {
        *self = *template;
    }
}

#[test]
fn clear_history_preserves_viewport_and_cursor_with_small_limits() {
    for limit in [0, 1, 2000] {
        let mut grid = Grid::<usize>::new(3, 8, limit);
        for _ in 0..3 {
            for _ in 0..20 {
                grid.scroll_up(&(Line(0)..Line(3)), 1);
            }
            for line in 0..3 {
                grid[Line(line)][Column(0)] = line as usize + 10;
            }
            grid.cursor.pos = Pos::new(Line(1), Column(4));
            grid.scroll_display(Scroll::Top);
            let evicted = grid.lines_evicted();
            let history = grid.history_size();
            grid.clear_history();
            assert_eq!(grid.history_size(), 0);
            assert_eq!(grid.display_offset(), 0);
            assert_eq!(grid.lines_evicted(), evicted + history as u64);
            assert_eq!(grid.cursor.pos, Pos::new(Line(1), Column(4)));
            for line in 0..3 {
                assert_eq!(grid[Line(line)][Column(0)], line as usize + 10);
            }
        }
    }
}

// Scroll up moves lines upward.
#[test]
fn scroll_up() {
    let mut grid = Grid::<usize>::new(10, 1, 0);
    for i in 0..10 {
        grid[Line(i as i32)][Column(0)] = i;
    }

    grid.scroll_up(&(Line(0)..Line(10)), 2);

    assert_eq!(grid[Line(0)][Column(0)], 2);
    assert_eq!(grid[Line(0)].occ, 1);
    assert_eq!(grid[Line(1)][Column(0)], 3);
    assert_eq!(grid[Line(1)].occ, 1);
    assert_eq!(grid[Line(2)][Column(0)], 4);
    assert_eq!(grid[Line(2)].occ, 1);
    assert_eq!(grid[Line(3)][Column(0)], 5);
    assert_eq!(grid[Line(3)].occ, 1);
    assert_eq!(grid[Line(4)][Column(0)], 6);
    assert_eq!(grid[Line(4)].occ, 1);
    assert_eq!(grid[Line(5)][Column(0)], 7);
    assert_eq!(grid[Line(5)].occ, 1);
    assert_eq!(grid[Line(6)][Column(0)], 8);
    assert_eq!(grid[Line(6)].occ, 1);
    assert_eq!(grid[Line(7)][Column(0)], 9);
    assert_eq!(grid[Line(7)].occ, 1);
    assert_eq!(grid[Line(8)][Column(0)], 0); // was 0.
    assert_eq!(grid[Line(8)].occ, 0);
    assert_eq!(grid[Line(9)][Column(0)], 0); // was 1.
    assert_eq!(grid[Line(9)].occ, 0);
}

// Scroll down moves lines downward.
#[test]
fn scroll_down() {
    let mut grid = Grid::<usize>::new(10, 1, 0);
    for i in 0..10 {
        grid[Line(i as i32)][Column(0)] = i;
    }

    grid.scroll_down(&(Line(0)..Line(10)), 2);

    assert_eq!(grid[Line(0)][Column(0)], 0); // was 8.
    assert_eq!(grid[Line(0)].occ, 0);
    assert_eq!(grid[Line(1)][Column(0)], 0); // was 9.
    assert_eq!(grid[Line(1)].occ, 0);
    assert_eq!(grid[Line(2)][Column(0)], 0);
    assert_eq!(grid[Line(2)].occ, 1);
    assert_eq!(grid[Line(3)][Column(0)], 1);
    assert_eq!(grid[Line(3)].occ, 1);
    assert_eq!(grid[Line(4)][Column(0)], 2);
    assert_eq!(grid[Line(4)].occ, 1);
    assert_eq!(grid[Line(5)][Column(0)], 3);
    assert_eq!(grid[Line(5)].occ, 1);
    assert_eq!(grid[Line(6)][Column(0)], 4);
    assert_eq!(grid[Line(6)].occ, 1);
    assert_eq!(grid[Line(7)][Column(0)], 5);
    assert_eq!(grid[Line(7)].occ, 1);
    assert_eq!(grid[Line(8)][Column(0)], 6);
    assert_eq!(grid[Line(8)].occ, 1);
    assert_eq!(grid[Line(9)][Column(0)], 7);
    assert_eq!(grid[Line(9)].occ, 1);
}

#[test]
fn scroll_down_with_history() {
    let mut grid = Grid::<usize>::new(10, 1, 1);
    grid.increase_scroll_limit(1);
    for i in 0..10 {
        grid[Line(i as i32)][Column(0)] = i;
    }

    grid.scroll_down(&(Line(0)..Line(10)), 2);

    assert_eq!(grid[Line(0)][Column(0)], 0); // was 8.
    assert_eq!(grid[Line(0)].occ, 0);
    assert_eq!(grid[Line(1)][Column(0)], 0); // was 9.
    assert_eq!(grid[Line(1)].occ, 0);
    assert_eq!(grid[Line(2)][Column(0)], 0);
    assert_eq!(grid[Line(2)].occ, 1);
    assert_eq!(grid[Line(3)][Column(0)], 1);
    assert_eq!(grid[Line(3)].occ, 1);
    assert_eq!(grid[Line(4)][Column(0)], 2);
    assert_eq!(grid[Line(4)].occ, 1);
    assert_eq!(grid[Line(5)][Column(0)], 3);
    assert_eq!(grid[Line(5)].occ, 1);
    assert_eq!(grid[Line(6)][Column(0)], 4);
    assert_eq!(grid[Line(6)].occ, 1);
    assert_eq!(grid[Line(7)][Column(0)], 5);
    assert_eq!(grid[Line(7)].occ, 1);
    assert_eq!(grid[Line(8)][Column(0)], 6);
    assert_eq!(grid[Line(8)].occ, 1);
    assert_eq!(grid[Line(9)][Column(0)], 7);
    assert_eq!(grid[Line(9)].occ, 1);
}

// Test that GridIterator works.
#[test]
fn test_iter() {
    let assert_indexed = |value: usize, indexed: Option<Indexed<&usize>>| {
        assert_eq!(Some(&value), indexed.map(|indexed| indexed.square));
    };

    let mut grid = Grid::<usize>::new(5, 5, 0);
    for i in 0..5 {
        for j in 0..5 {
            grid[Line(i)][Column(j)] = i as usize * 5 + j;
        }
    }

    let mut iter = grid.iter_from(Pos::new(Line(0), Column(0)));

    assert_eq!(None, iter.prev());
    assert_indexed(1, iter.next());
    assert_eq!(Column(1), iter.pos().col);
    assert_eq!(0, iter.pos().row);

    assert_indexed(2, iter.next());
    assert_indexed(3, iter.next());
    assert_indexed(4, iter.next());

    // Test line-wrapping.
    assert_indexed(5, iter.next());
    assert_eq!(Column(0), iter.pos().col);
    assert_eq!(1, iter.pos().row);

    assert_indexed(4, iter.prev());
    assert_eq!(Column(4), iter.pos().col);
    assert_eq!(0, iter.pos().row);

    // Make sure iter.cell() returns the current iterator position.
    assert_eq!(&4, iter.square());

    // Test that iter ends at end of grid.
    let mut final_iter = grid.iter_from(Pos {
        row: Line(4),
        col: Column(4),
    });
    assert_eq!(None, final_iter.next());
    assert_indexed(23, final_iter.prev());
}

#[test]
fn shrink_reflow() {
    let mut grid = Grid::<Square>::new(1, 5, 2);
    grid[Line(0)][Column(0)] = cell('1');
    grid[Line(0)][Column(1)] = cell('2');
    grid[Line(0)][Column(2)] = cell('3');
    grid[Line(0)][Column(3)] = cell('4');
    grid[Line(0)][Column(4)] = cell('5');

    grid.resize(true, 1, 2);

    assert_eq!(grid.total_lines(), 3);

    assert_eq!(grid[Line(-2)].len(), 2);
    assert_eq!(grid[Line(-2)][Column(0)], cell('1'));
    assert_eq!(grid[Line(-2)][Column(1)], wrap_cell('2'));

    assert_eq!(grid[Line(-1)].len(), 2);
    assert_eq!(grid[Line(-1)][Column(0)], cell('3'));
    assert_eq!(grid[Line(-1)][Column(1)], wrap_cell('4'));

    assert_eq!(grid[Line(0)].len(), 2);
    assert_eq!(grid[Line(0)][Column(0)], cell('5'));
    assert_eq!(grid[Line(0)][Column(1)], Square::default());
}

#[test]
fn shrink_reflow_twice() {
    let mut grid = Grid::<Square>::new(1, 5, 2);
    grid[Line(0)][Column(0)] = cell('1');
    grid[Line(0)][Column(1)] = cell('2');
    grid[Line(0)][Column(2)] = cell('3');
    grid[Line(0)][Column(3)] = cell('4');
    grid[Line(0)][Column(4)] = cell('5');

    grid.resize(true, 1, 4);
    grid.resize(true, 1, 2);

    assert_eq!(grid.total_lines(), 3);

    assert_eq!(grid[Line(-2)].len(), 2);
    assert_eq!(grid[Line(-2)][Column(0)], cell('1'));
    assert_eq!(grid[Line(-2)][Column(1)], wrap_cell('2'));

    assert_eq!(grid[Line(-1)].len(), 2);
    assert_eq!(grid[Line(-1)][Column(0)], cell('3'));
    assert_eq!(grid[Line(-1)][Column(1)], wrap_cell('4'));

    assert_eq!(grid[Line(0)].len(), 2);
    assert_eq!(grid[Line(0)][Column(0)], cell('5'));
    assert_eq!(grid[Line(0)][Column(1)], Square::default());
}

#[test]
fn shrink_reflow_empty_cell_inside_line() {
    let mut grid = Grid::<Square>::new(1, 5, 3);
    grid[Line(0)][Column(0)] = cell('1');
    grid[Line(0)][Column(1)] = Square::default();
    grid[Line(0)][Column(2)] = cell('3');
    grid[Line(0)][Column(3)] = cell('4');
    grid[Line(0)][Column(4)] = Square::default();

    grid.resize(true, 1, 2);

    assert_eq!(grid.total_lines(), 2);

    assert_eq!(grid[Line(-1)].len(), 2);
    assert_eq!(grid[Line(-1)][Column(0)], cell('1'));
    assert_eq!(grid[Line(-1)][Column(1)], wrap_cell('\0'));

    assert_eq!(grid[Line(0)].len(), 2);
    assert_eq!(grid[Line(0)][Column(0)], cell('3'));
    assert_eq!(grid[Line(0)][Column(1)], cell('4'));

    grid.resize(true, 1, 1);

    assert_eq!(grid.total_lines(), 4);

    assert_eq!(grid[Line(-3)].len(), 1);
    assert_eq!(grid[Line(-3)][Column(0)], wrap_cell('1'));

    assert_eq!(grid[Line(-2)].len(), 1);
    assert_eq!(grid[Line(-2)][Column(0)], wrap_cell('\0'));

    assert_eq!(grid[Line(-1)].len(), 1);
    assert_eq!(grid[Line(-1)][Column(0)], wrap_cell('3'));

    assert_eq!(grid[Line(0)].len(), 1);
    assert_eq!(grid[Line(0)][Column(0)], cell('4'));
}

#[test]
fn grow_reflow() {
    let mut grid = Grid::<Square>::new(2, 2, 0);
    grid[Line(0)][Column(0)] = cell('1');
    grid[Line(0)][Column(1)] = wrap_cell('2');
    grid[Line(1)][Column(0)] = cell('3');
    grid[Line(1)][Column(1)] = Square::default();

    grid.resize(true, 2, 3);

    assert_eq!(grid.total_lines(), 2);

    assert_eq!(grid[Line(0)].len(), 3);
    assert_eq!(grid[Line(0)][Column(0)], cell('1'));
    assert_eq!(grid[Line(0)][Column(1)], cell('2'));
    assert_eq!(grid[Line(0)][Column(2)], cell('3'));

    // Make sure rest of grid is empty.
    assert_eq!(grid[Line(1)].len(), 3);
    assert_eq!(grid[Line(1)][Column(0)], Square::default());
    assert_eq!(grid[Line(1)][Column(1)], Square::default());
    assert_eq!(grid[Line(1)][Column(2)], Square::default());
}

#[test]
fn grow_reflow_multiline() {
    let mut grid = Grid::<Square>::new(3, 2, 0);
    grid[Line(0)][Column(0)] = cell('1');
    grid[Line(0)][Column(1)] = wrap_cell('2');
    grid[Line(1)][Column(0)] = cell('3');
    grid[Line(1)][Column(1)] = wrap_cell('4');
    grid[Line(2)][Column(0)] = cell('5');
    grid[Line(2)][Column(1)] = cell('6');

    grid.resize(true, 3, 6);

    assert_eq!(grid.total_lines(), 3);

    assert_eq!(grid[Line(0)].len(), 6);
    assert_eq!(grid[Line(0)][Column(0)], cell('1'));
    assert_eq!(grid[Line(0)][Column(1)], cell('2'));
    assert_eq!(grid[Line(0)][Column(2)], cell('3'));
    assert_eq!(grid[Line(0)][Column(3)], cell('4'));
    assert_eq!(grid[Line(0)][Column(4)], cell('5'));
    assert_eq!(grid[Line(0)][Column(5)], cell('6'));

    // Make sure rest of grid is empty.
    for r in (1..3).map(Line::from) {
        assert_eq!(grid[r].len(), 6);
        for c in 0..6 {
            assert_eq!(grid[r][Column(c)], Square::default());
        }
    }
}

#[test]
fn grow_reflow_disabled() {
    let mut grid = Grid::<Square>::new(2, 2, 0);
    grid[Line(0)][Column(0)] = cell('1');
    grid[Line(0)][Column(1)] = wrap_cell('2');
    grid[Line(1)][Column(0)] = cell('3');
    grid[Line(1)][Column(1)] = Square::default();

    grid.resize(false, 2, 3);

    assert_eq!(grid.total_lines(), 2);

    assert_eq!(grid[Line(0)].len(), 3);
    assert_eq!(grid[Line(0)][Column(0)], cell('1'));
    assert_eq!(grid[Line(0)][Column(1)], cell('2'));
    assert_eq!(grid[Line(0)][Column(2)], Square::default());

    assert_eq!(grid[Line(1)].len(), 3);
    assert_eq!(grid[Line(1)][Column(0)], cell('3'));
    assert_eq!(grid[Line(1)][Column(1)], Square::default());
    assert_eq!(grid[Line(1)][Column(2)], Square::default());
}

#[test]
fn shrink_reflow_disabled() {
    let mut grid = Grid::<Square>::new(1, 5, 2);
    grid[Line(0)][Column(0)] = cell('1');
    grid[Line(0)][Column(1)] = cell('2');
    grid[Line(0)][Column(2)] = cell('3');
    grid[Line(0)][Column(3)] = cell('4');
    grid[Line(0)][Column(4)] = cell('5');

    grid.resize(false, 1, 2);

    assert_eq!(grid.total_lines(), 1);

    assert_eq!(grid[Line(0)].len(), 2);
    assert_eq!(grid[Line(0)][Column(0)], cell('1'));
    assert_eq!(grid[Line(0)][Column(1)], cell('2'));
}

// https://github.com/rust-lang/rust-clippy/pull/6375
#[allow(clippy::all)]
fn cell(c: char) -> Square {
    let mut cell = Square::default();
    cell.set_c(c);
    cell
}

fn wrap_cell(c: char) -> Square {
    let mut cell = cell(c);
    cell.set_wrapline(true);
    cell
}

fn wide_cell(c: char) -> Square {
    let mut cell = cell(c);
    cell.set_wide(crate::crosswords::square::Wide::Wide);
    cell
}

fn spacer_cell() -> Square {
    let mut cell = Square::default();
    cell.set_wide(crate::crosswords::square::Wide::Spacer);
    cell
}

#[test]
fn shrink_reflow_remap_tracks_displaced_wide_char() {
    // A wrapped tail of exactly `columns - 1` cells is buffered into
    // the next row, whose first cell is a wide char. The spacer logic
    // displaces that wide char into the following push, so the remap
    // must record the row after the one receiving the buffered tail.
    let mut grid = Grid::<Square>::new(3, 8, 4);
    for (n, c) in "1234567".chars().enumerate() {
        grid[Line(0)][Column(n)] = cell(c);
    }
    grid[Line(0)][Column(6)] = wrap_cell('7');
    grid[Line(1)][Column(0)] = wide_cell('W');
    grid[Line(1)][Column(1)] = spacer_cell();
    grid[Line(1)][Column(2)] = cell('x');

    grid.track_reflow_remap = true;
    grid.resize(true, 3, 4);
    grid.track_reflow_remap = false;

    let remap = grid.reflow_remap.take().expect("remap must be recorded");
    assert_eq!(remap.base_abs, 0);
    // Old row 0 lands at 0; the wide-char row's first cell lands at 2
    // (position 1 holds the buffered "567" tail plus the spacer); the
    // trailing blank row paid for the row the wrap added and is gone.
    assert_eq!(remap.new_pos, vec![0, 2, -1]);
    assert_eq!(
        grid.history_size(),
        0,
        "three rows of content fit three lines"
    );

    // Cross-check against where the wide char actually sits.
    let total = grid.total_lines() as i32;
    let screen = grid.screen_lines() as i32;
    let wide_line = Line(2 - (total - screen));
    assert_eq!(grid[wide_line][Column(0)], wide_cell('W'));
}

#[test]
fn grow_reflow_remap_tracks_unmerged_wide_char() {
    // The merge target has exactly one free column, so only a leading
    // spacer is appended and the wide char stays on its own row. The
    // row pin is remapped before that character is displaced, like
    // Ghostty's tracked pins: it stays on the spacer in the merge target.
    let mut grid = Grid::<Square>::new(2, 4, 2);
    for (n, c) in "1234".chars().enumerate() {
        grid[Line(0)][Column(n)] = cell(c);
    }
    grid[Line(0)][Column(3)] = wrap_cell('4');
    grid[Line(1)][Column(0)] = wide_cell('W');
    grid[Line(1)][Column(1)] = spacer_cell();

    grid.track_reflow_remap = true;
    grid.resize(true, 2, 5);
    grid.track_reflow_remap = false;

    let remap = grid.reflow_remap.take().expect("remap must be recorded");
    assert_eq!(remap.base_abs, 0);
    assert_eq!(remap.new_pos, vec![0, 0]);
    assert_eq!(grid[Line(1)][Column(0)], wide_cell('W'));
}

#[test]
fn extras_sweep_resets_reclaim_cadence() {
    use crate::crosswords::square::{Extras, Hyperlink};

    let mut table = ExtrasTable::new();
    for i in 0..EXTRAS_RECLAIM_CADENCE {
        table.alloc(Extras {
            zerowidth: Vec::new(),
            hyperlink: Some(Hyperlink::new(Some(i.to_string()), i.to_string())),
        });
    }
    assert!(table.should_reclaim());

    // A sweep resets the cadence even when every slot stays live, so a
    // fully-live table can't re-walk the ring on every allocation.
    let live = vec![u64::MAX; ID_BITSET_WORDS];
    table.sweep_unmarked(&live);
    assert!(!table.should_reclaim());
}

#[test]
fn style_sweep_marks_cursor_template() {
    use crate::config::colors::{AnsiColor, NamedColor};
    use crate::crosswords::style::Style;

    let mut grid: Grid<Square> = Grid::new(4, 4, 10);

    // Intern a style that ends up referenced by nothing once the
    // template moves on: it must be swept.
    grid.set_template_style(Style {
        fg: AnsiColor::Named(NamedColor::Blue),
        ..Style::default()
    });
    grid.sync_template_style();
    let dead_id = grid.template_style_id();

    // The template's current style is a live root even when no cell
    // references it.
    grid.set_template_style(Style {
        fg: AnsiColor::Named(NamedColor::Red),
        ..Style::default()
    });
    grid.sync_template_style();
    let live_id = grid.template_style_id();

    grid.reclaim_styles();
    assert_eq!(
        grid.style_set.get(live_id).fg,
        AnsiColor::Named(NamedColor::Red)
    );
    assert_eq!(grid.style_set.get(dead_id), Style::default());
}

#[test]
fn extras_reclaim_keeps_ids_of_hidden_cached_rows() {
    use crate::crosswords::square::Extras;

    let mut grid: Grid<Square> = Grid::new(4, 2, 10);
    let id = grid.alloc_extras(Extras {
        zerowidth: vec!['\u{301}'],
        hyperlink: None,
    });
    grid[Line(3)][Column(0)].set_extras_id(Some(id));
    grid[Line(3)].has_extras = true;
    let dead = grid.alloc_extras(Extras {
        zerowidth: vec!['\u{302}'],
        hyperlink: None,
    });

    // Shrinking with the cursor at the top drops the bottom rows into
    // Storage's hidden cache; their extras must stay live while the
    // unreferenced slot is freed.
    grid.resize(false, 2, 2);
    grid.reclaim_extras();
    assert!(grid.extras_table.get(id).is_some());
    assert!(grid.extras_table.get(dead).is_none());
}

#[test]
fn reflow_append_preserves_placeholder_hint() {
    let mut destination: Row<Square> = Row::new(2);
    let mut source: Row<Square> = Row::new(2);
    source.inner[0] = Square::from_char('\u{10eeee}');
    source.kitty_virtual_placeholder = true;

    destination.append_front_of(&mut source, 1);

    assert_eq!(destination.inner[2].c(), '\u{10eeee}');
    assert!(destination.kitty_virtual_placeholder);
}

#[test]
fn row_hint_follows_erased_background() {
    use crate::config::colors::{AnsiColor, NamedColor};
    use crate::crosswords::style::Style;

    let mut grid: Grid<Square> = Grid::new(2, 4, 0);
    grid.set_template_style(Style {
        fg: AnsiColor::Named(NamedColor::Red),
        ..Style::default()
    });
    grid.sync_template_style();
    let styled_template = grid.cursor.template;
    assert!(styled_template.carries_style());

    let mut row: Row<Square> = Row::new(4);
    assert!(!row.has_styles);
    row.reset(&styled_template, &mut grid.style_set);
    assert!(!row.has_styles);

    grid.set_template_style(Style {
        bg: AnsiColor::Named(NamedColor::Foreground),
        ..Style::default()
    });
    grid.sync_template_style();
    row.reset(&grid.cursor.template, &mut grid.style_set);
    assert!(row.has_styles);

    row.reset(&Square::default(), &mut grid.style_set);
    assert!(!row.has_styles);
}

#[test]
fn row_snapshot_copy_skips_equal_uniform_tails() {
    use std::cell::Cell;
    use std::rc::Rc;

    #[derive(Default)]
    struct Counted {
        value: u8,
        copies: Rc<Cell<usize>>,
    }
    impl Clone for Counted {
        fn clone(&self) -> Self {
            self.copies.set(self.copies.get() + 1);
            Self {
                value: self.value,
                copies: self.copies.clone(),
            }
        }
    }
    impl PartialEq for Counted {
        fn eq(&self, other: &Self) -> bool {
            self.value == other.value
        }
    }
    let copies = Rc::new(Cell::new(0));
    let mut source: Row<Counted> = Row::new(120);
    let mut saved: Row<Counted> = Row::new(120);
    for row in [&mut source, &mut saved] {
        for cell in &mut row.inner {
            cell.copies = copies.clone();
        }
    }
    source[Column(2)].value = 1;
    saved[Column(6)].value = 2;
    copies.set(0);
    saved.copy_snapshot_from(&source);
    assert!(saved == source);
    assert_eq!(
        copies.get(),
        7,
        "copy the larger occupied prefix, including erased cells"
    );
}

#[test]
fn row_tail_bound_survives_growth_splices_and_inclusive_writes() {
    let filled = Square::from_char('x');
    let mut row: Row<Square> = Row::new(6);
    row.reset_to(filled);
    row.grow(10);
    assert_eq!(
        row.occ, 0,
        "growth must preserve the no-reflow resize bound"
    );
    assert_eq!(row.inner[0], filled);
    assert_eq!(row.inner[9], Square::default());
    row.append(&mut vec![Square::default(), filled, Square::default()]);
    assert!(
        row.has_extras,
        "arbitrary appends must use full snapshot copying"
    );

    let mut other: Row<Square> = Row::new(4);
    other[Column(1)] = filled;
    let mut dst: Row<Square> = Row::new(6);
    dst.append_front_of(&mut other, 3);
    assert!(dst.inner[dst.occ..]
        .iter()
        .all(|cell| *cell == dst.inner[8]));
    assert!(other.inner[other.occ..]
        .iter()
        .all(|cell| *cell == other.inner[0]));

    let mut inclusive: Row<Square> = Row::new(10);
    inclusive[..=Column(3)].fill(filled);
    assert!(inclusive.occ >= 4);
}

#[test]
fn snapshot_copy_checks_both_ends_of_grown_fill_tails() {
    let blue = Square::blank_with_bg(
        crate::config::colors::AnsiColor::Indexed(4),
        |_| unreachable!(),
    );
    let mut source: Row<Square> = Row::new(4);
    let mut saved: Row<Square> = Row::new(6);
    source.reset_to(blue);
    saved.reset_to(blue);
    source.grow(8);
    saved.grow(8);
    // Equal tail endpoints alone must not hide different fill transitions.
    assert_eq!(source.inner[7], saved.inner[7]);
    saved.copy_snapshot_from(&source);
    assert_eq!(saved.inner, source.inner);
}

#[test]
fn snapshot_prefix_copy_matches_full_copy_after_row_mutations() {
    let mut source: Row<Square> = Row::new(17);
    let mut saved: Row<Square> = Row::new(17);
    let mut seed = 17_u64;
    for step in 0..2048 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let col = (seed >> 32) as usize % source.len();
        match step % 10 {
            0 => source[Column(col)] = Square::from_char('界'),
            1 => source[..=Column(col)].fill(Square::from_char('x')),
            2 => source.reset_to(Square::blank_with_bg(
                crate::config::colors::AnsiColor::Indexed((seed >> 32) as u8),
                |_| unreachable!(),
            )),
            3 => source.grow(source.len() + 3),
            4 => source.truncate_columns((source.len() / 2).max(1)),
            5 => source.append(&mut vec![Square::from_char('a'), Square::default()]),
            6 => {
                let mut next: Row<Square> = Row::new(5);
                next[Column(1)] = Square::from_char('b');
                source.append_front_of(&mut next, 3);
            }
            7 => {
                source = Row::from_vec(
                    vec![Square::default(), Square::from_char('c'), Square::default()],
                    0,
                );
            }
            8 => source.reset_to(Square::default()),
            _ => {
                // Renderer-side edits must also be erased by the next copy.
                let last = saved.len() - 1;
                saved[Column(last)] = Square::from_char('z');
            }
        }
        source.semantic_prompt = if step % 2 == 0 {
            row::SemanticPrompt::Prompt
        } else {
            row::SemanticPrompt::None
        };
        source.kitty_virtual_placeholder = step % 3 == 0;
        let dirty = saved.dirty;
        let mut expected = saved.clone();
        expected.copy_from(&source);
        saved.copy_snapshot_from(&source);
        assert_eq!(saved.inner, expected.inner, "step={step}");
        assert_eq!(saved.occ, expected.occ);
        assert_eq!(saved.has_styles, expected.has_styles);
        assert_eq!(saved.has_extras, expected.has_extras);
        assert_eq!(saved.semantic_prompt, expected.semantic_prompt);
        assert_eq!(
            saved.kitty_virtual_placeholder,
            expected.kitty_virtual_placeholder
        );
        assert_eq!(saved.dirty, dirty);
    }
}

#[test]
fn row_hint_propagates_and_clears() {
    let mut styled: Row<Square> = Row::new(4);
    styled[Column(0)].set_style_id(9);
    styled.has_styles = true;

    let mut dst: Row<Square> = Row::new(4);
    dst.copy_from(&styled);
    assert!(dst.has_styles);

    dst.recycle(4);
    assert!(!dst.has_styles);
}

#[test]
fn row_splices_recompute_hint_exactly() {
    // Unstyled cells moving in must not pin the row into the sweep walk.
    let mut dst: Row<Square> = Row::new(2);
    let mut plain = vec![Square::default(), Square::default()];
    dst.append(&mut plain);
    assert!(!dst.has_styles);

    let mut styled_vec = vec![Square::default().with_style_id(3)];
    dst.append(&mut styled_vec);
    assert!(dst.has_styles);

    // append_front_of scans only the moved span.
    let mut src: Row<Square> = Row::new(4);
    src[Column(3)].set_style_id(5);
    src.has_styles = true;
    let mut dst2: Row<Square> = Row::new(1);
    dst2.append_front_of(&mut src, 2);
    assert!(!dst2.has_styles);
    let mut dst3: Row<Square> = Row::new(1);
    dst3.append_front_of(&mut src, 2);
    assert!(dst3.has_styles);

    // from_vec derives the hint from its contents.
    assert!(!Row::from_vec(vec![Square::default(); 3], 0).has_styles);
    assert!(Row::from_vec(vec![Square::default().with_style_id(1)], 1).has_styles);
}

#[test]
fn bg_only_cells_do_not_count_as_styled() {
    let mut bg = Square::default();
    bg.set_bg_rgb(10, 20, 30);
    assert!(!bg.carries_style());

    let mut row: Row<Square> = Row::new(2);
    let mut moved = vec![bg];
    row.append(&mut moved);
    assert!(!row.has_styles);
}

#[test]
fn resolver_fast_path_matches_slow_path() {
    use crate::config::colors::{AnsiColor, NamedColor};
    use crate::crosswords::style::Style;

    let mut grid: Grid<Square> = Grid::new(2, 3, 0);
    grid.set_template_style(Style {
        fg: AnsiColor::Named(NamedColor::Blue),
        ..Style::default()
    });
    grid.sync_template_style();
    let id = grid.template_style_id();

    // Styled row: full resolution.
    let mut styled: Row<Square> = Row::new(3);
    styled[Column(1)].set_style_id(id);
    styled.has_styles = true;
    let mut out = crate::crosswords::snapshot::RowStyles::default();
    grid.resolve_row_styles(&styled, &mut out);
    assert_eq!(out.uniform(), None);
    assert_eq!(out.at(0), Style::default());
    assert_eq!(out.at(1).fg, AnsiColor::Named(NamedColor::Blue));

    // Unstyled rows discard old values while retaining mixed-row capacity.
    let plain: Row<Square> = Row::new(3);
    grid.resolve_row_styles(&plain, &mut out);
    assert_eq!(out.uniform(), Some(Style::DEFAULT));
}

#[test]
fn uniform_snapshot_keeps_values_after_style_id_reuse() {
    use crate::crosswords::snapshot::RowStyles;
    use crate::crosswords::style::{Style, StyleFlags};

    let mut grid: Grid<Square> = Grid::new(2, 160, 0);
    let bold = Style {
        flags: StyleFlags::BOLD,
        ..Style::DEFAULT
    };
    let italic = Style {
        flags: StyleFlags::ITALIC,
        ..Style::DEFAULT
    };
    let id = grid.style_set.intern(bold);
    let row = Row::from_vec(vec![Square::from_char('x').with_style_id(id); 160], 160);
    let mut snapshot = RowStyles::default();
    grid.resolve_row_styles(&row, &mut snapshot);
    assert_eq!(snapshot.uniform(), Some(bold));

    grid.style_set
        .sweep_unmarked(&vec![0; super::ID_BITSET_WORDS], 0);
    assert_eq!(grid.style_set.intern(italic), id);
    assert_eq!(snapshot.at(159), bold);
    grid.resolve_row_styles(&row, &mut snapshot);
    assert_eq!(snapshot.uniform(), Some(italic));
}

// `lines_evicted` is the absolute index of the oldest row still in the
// ring. It grows when a scroll at the cap pushes a row off, when the
// cap is lowered, and when the history is cleared.
#[test]
fn evictions_are_counted_at_the_cap() {
    let mut grid = Grid::<usize>::new(3, 1, 2);
    assert_eq!(grid.lines_evicted(), 0);
    for _ in 0..5 {
        grid.scroll_up(&(Line(0)..Line(3)), 1);
    }
    assert_eq!(grid.history_size(), 2, "history stops at the cap");
    assert_eq!(
        grid.lines_evicted(),
        3,
        "the three scrolls past the cap evicted a row each"
    );

    grid.update_history(1);
    assert_eq!(grid.history_size(), 1);
    assert_eq!(
        grid.lines_evicted(),
        4,
        "lowering the cap evicts the oldest"
    );

    grid.clear_history();
    assert_eq!(grid.history_size(), 0);
    assert_eq!(grid.lines_evicted(), 5, "clearing evicts what was left");

    // A scroll inside a sub region adds nothing to history and evicts nothing.
    grid.scroll_up(&(Line(1)..Line(3)), 1);
    assert_eq!(grid.lines_evicted(), 5);
}

// A reflow can produce more rows than the ring holds. The oldest fall
// off and count as evicted, like a scroll at the cap.
#[test]
fn reflow_past_the_cap_evicts_the_oldest_rows() {
    let mut grid = Grid::<Square>::new(1, 5, 2);
    for (i, c) in ['1', '2', '3', '4', '5'].into_iter().enumerate() {
        grid[Line(0)][Column(i)] = cell(c);
    }

    grid.resize(true, 1, 1);

    // Five rows wanted, three fit (one screen line plus two of history).
    assert_eq!(grid.total_lines(), 3);
    assert_eq!(grid.history_size(), 2);
    assert_eq!(grid.lines_evicted(), 2);
    assert_eq!(grid[Line(-2)][Column(0)], wrap_cell('3'));
    assert_eq!(grid[Line(-1)][Column(0)], wrap_cell('4'));
    assert_eq!(grid[Line(0)][Column(0)], cell('5'));
}

fn reflow_fixture() -> Grid<Square> {
    let mut grid = Grid::<Square>::new(3, 6, 5);
    for (line, text) in ["ab", "cde", "f"].into_iter().enumerate() {
        for (col, c) in text.chars().enumerate() {
            grid[Line(line as i32)][Column(col)] = cell(c);
        }
    }
    grid.cursor.pos = Pos::new(Line(2), Column(1));
    grid.saved_cursor.pos = Pos::new(Line(1), Column(3));
    grid
}

fn assert_same_grid(fast: &Grid<Square>, full: &Grid<Square>) {
    assert_eq!(fast.total_lines(), full.total_lines());
    assert_eq!(fast.screen_lines(), full.screen_lines());
    assert_eq!(fast.columns(), full.columns());
    assert_eq!(fast.cursor.pos, full.cursor.pos);
    assert_eq!(fast.cursor.should_wrap, full.cursor.should_wrap);
    assert_eq!(fast.saved_cursor.pos, full.saved_cursor.pos);
    let history = fast.history_size() as i32;
    for line in -history..fast.screen_lines() as i32 {
        let (a, b) = (&fast[Line(line)], &full[Line(line)]);
        assert_eq!(a.len(), b.len(), "row {line} width");
        for col in 0..a.len() {
            assert_eq!(a[Column(col)], b[Column(col)], "row {line} col {col}");
        }
    }
}

// The grow and shrink fast paths skip the full reflow when no row
// wraps or overflows. They must land on the same grid the full reflow
// does; `track_reflow_remap` forces the full path. Both widths keep
// every row within its occupancy, so `fast` takes the fast path.
#[test]
fn reflow_fast_paths_match_the_full_reflow() {
    for columns in [8usize, 4] {
        let mut fast = reflow_fixture();
        let mut full = reflow_fixture();
        full.track_reflow_remap = true;
        fast.resize(true, 3, columns);
        full.resize(true, 3, columns);
        assert!(full.reflow_remap.is_some(), "the full path ran");
        assert_same_grid(&fast, &full);

        // And back again from the result.
        full.track_reflow_remap = true;
        fast.resize(true, 3, 6);
        full.resize(true, 3, 6);
        assert_same_grid(&fast, &full);
    }
}

// A position captured before a resize can point past the live grid.
// `next` already ends the iteration there; `prev` must too.
#[test]
fn prev_on_a_stale_position_ends_instead_of_panicking() {
    let grid = Grid::<Square>::new(2, 5, 0);
    let past_the_row = Pos::new(Line(0), Column(9));
    assert!(grid.iter_from(past_the_row).next().is_none());
    assert!(grid.iter_from(past_the_row).prev().is_none());

    let past_the_screen = Pos::new(Line(5), Column(0));
    assert!(grid.iter_from(past_the_screen).next().is_none());
    assert!(grid.iter_from(past_the_screen).prev().is_none());

    // A live position still walks backwards to the start and stops.
    let mut it = grid.iter_from(Pos::new(Line(0), Column(1)));
    assert_eq!(it.prev().map(|i| i.pos), Some(Pos::new(Line(0), Column(0))));
    assert!(it.prev().is_none());

    // Wrapping back onto a shorter row enters it at its real end. The
    // forward walk pays for no such row, since none exists today: past
    // its end it stops like any stale position.
    let mut grid = Grid::<Square>::new(2, 5, 0);
    grid.raw[Line(0)].shrink(3);
    let mut it = grid.iter_from(Pos::new(Line(1), Column(0)));
    assert_eq!(it.prev().map(|i| i.pos), Some(Pos::new(Line(0), Column(2))));
    assert!(it.next().is_none());
}

fn fill(grid: &mut Grid<Square>, line: i32, text: &str, wrap: bool) {
    let last = text.chars().count() - 1;
    for (col, c) in text.chars().enumerate() {
        grid[Line(line)][Column(col)] = if wrap && col == last {
            wrap_cell(c)
        } else {
            cell(c)
        };
    }
}

fn text_of(grid: &Grid<Square>, line: i32) -> String {
    let row = &grid[Line(line)];
    (0..row.len())
        .map(|c| row[Column(c)].c())
        .collect::<String>()
        .trim_end_matches('\0')
        .to_string()
}

// A blank row right under a line that wraps is that line's continuation:
// the wrapped tail lands in it, so it cannot pay for an added row. With
// it counted as room, the cursor was moved for a row that was never
// popped and ended one row under its text.
#[test]
fn a_blank_continuation_row_is_not_room() {
    let mut grid = Grid::<Square>::new(6, 10, 10);
    fill(&mut grid, 0, "xxxxxxxxxx", true);
    fill(&mut grid, 1, "xxxxxxxxxx", true);
    fill(&mut grid, 2, "xxxxxxxxxx", false);
    fill(&mut grid, 3, "abcdefghij", true);
    // Rows 4 (the continuation of row 3) and 5 are blank.
    grid.cursor.pos = Pos::new(Line(3), Column(3));

    grid.resize(true, 6, 7);

    assert_eq!(
        grid.history_size(),
        1,
        "one blank row paid, one row went up"
    );
    assert_eq!(text_of(&grid, -1), "xxxxxxx");
    assert_eq!(text_of(&grid, 3), "xx");
    assert_eq!(text_of(&grid, 4), "abcdefg");
    assert_eq!(text_of(&grid, 5), "hij");
    assert_eq!(grid.cursor.pos, Pos::new(Line(4), Column(3)));
    assert_eq!(grid[grid.cursor.pos].c(), 'd');
}

// A wide char at the seam stays behind its spacer when the row above
// takes the cells before it, so one cell fewer moves than fit. A cursor
// on a moved cell lands after the cells the row above already had.
#[test]
fn a_cursor_before_a_displaced_wide_char_rejoins_on_its_cell() {
    let mut grid = Grid::<Square>::new(2, 9, 0);
    fill(&mut grid, 0, "abcdefgh", false);
    let mut leading = spacer_cell();
    leading.set_wide(crate::crosswords::square::Wide::LeadingSpacer);
    leading.set_wrapline(true);
    grid[Line(0)][Column(8)] = leading;
    grid[Line(1)][Column(0)] = cell('x');
    grid[Line(1)][Column(1)] = wide_cell('\u{4e2d}');
    grid[Line(1)][Column(2)] = spacer_cell();
    grid.cursor.pos = Pos::new(Line(1), Column(0));

    grid.resize(true, 2, 10);

    assert_eq!(grid[Line(0)][Column(8)], cell('x'));
    assert!(matches!(
        grid[Line(0)][Column(9)].wide(),
        crate::crosswords::square::Wide::LeadingSpacer
    ));
    assert_eq!(grid[Line(1)][Column(0)], wide_cell('\u{4e2d}'));
    assert_eq!(grid.cursor.pos, Pos::new(Line(0), Column(8)));
    assert_eq!(grid[grid.cursor.pos].c(), 'x');
}

// Rows popped to pay for added rows are gone for the image remap too.
#[test]
fn popped_rows_leave_no_remap_entry() {
    let mut grid = Grid::<Square>::new(3, 10, 5);
    fill(&mut grid, 0, "abcdefghij", false);
    grid.cursor.pos = Pos::new(Line(0), Column(3));
    grid.track_reflow_remap = true;

    grid.resize(true, 3, 4);

    let remap = grid.reflow_remap.take().expect("the full path ran");
    assert_eq!(remap.new_pos, vec![0, -1, -1]);
    assert_eq!(grid.history_size(), 0);
    assert_eq!(text_of(&grid, 2), "ij");
}

// The fork pins a scrolled viewport to its first cell. Popping blank rows
// below it must keep that cell visible instead of following the bottom row.
#[test]
fn a_scrolled_viewport_keeps_its_top_cell_when_rows_under_it_are_popped() {
    let mut grid = Grid::<Square>::new(4, 10, 5);
    for word in ["gggg", "hhhh"] {
        fill(&mut grid, 0, word, false);
        grid.scroll_up(&(Line(0)..Line(4)), 1);
    }
    fill(&mut grid, 0, "abcdefghij", false);
    grid.cursor.pos = Pos::new(Line(0), Column(3));
    assert_eq!(grid.history_size(), 2);
    grid.scroll_display(Scroll::Delta(2));
    assert_eq!(grid.display_offset(), 2, "the viewport ends on a blank row");

    grid.resize(true, 4, 4);

    assert_eq!(
        grid.history_size(),
        2,
        "the two blank rows paid for the two added"
    );
    assert_eq!(
        grid.display_offset(),
        2,
        "the viewport still starts at gggg"
    );
    assert_eq!(text_of(&grid, -2), "gggg");
    assert_eq!(text_of(&grid, 0), "abcd");
    assert_eq!(text_of(&grid, 2), "ij");
    assert_eq!(text_of(&grid, 3), "", "the viewport still ends on it");
    assert_eq!(grid.cursor.pos, Pos::new(Line(0), Column(3)));
}

// A grow that gives rows back at the bottom while the viewport is
// scrolled into history: the viewport keeps showing the same rows, as
// it does when a shrink pops rows under it.
#[test]
fn a_scrolled_viewport_stays_put_when_rows_are_given_back() {
    let mut grid = Grid::<Square>::new(4, 4, 10);
    for word in ["gggg", "hhhh", "iiii", "jjjj", "kkkk"] {
        fill(&mut grid, 0, word, false);
        grid.scroll_up(&(Line(0)..Line(4)), 1);
    }
    fill(&mut grid, 0, "abcd", true);
    fill(&mut grid, 1, "efgh", true);
    fill(&mut grid, 2, "ij", false);
    grid.cursor.pos = Pos::new(Line(2), Column(2));
    assert_eq!(grid.history_size(), 5);
    grid.scroll_display(Scroll::Delta(5));
    assert_eq!(grid.display_offset(), 5);
    assert_eq!(text_of(&grid, -5), "gggg", "the viewport starts at gggg");

    grid.resize(true, 4, 10);

    assert_eq!(
        grid.history_size(),
        5,
        "no history row slid onto the screen"
    );
    assert_eq!(text_of(&grid, 0), "abcdefghij");
    assert_eq!(text_of(&grid, 1), "");
    // Preserve an explicit cursor on the blank after the full joined line.
    assert_eq!(grid.cursor.pos, Pos::new(Line(1), Column(0)));
    assert!(!grid.cursor.should_wrap);
    assert_eq!(grid.display_offset(), 5);
    assert_eq!(
        text_of(&grid, -5),
        "gggg",
        "the viewport still starts at gggg"
    );
}

// A line whose head is in history and whose tail is on the screen was
// visible, so a grow that joins it is not sliding history onto the
// screen: the joined row stays where its tail was.
#[test]
fn a_line_straddling_the_top_of_the_screen_stays_visible_after_a_grow() {
    let mut grid = Grid::<Square>::new(4, 2, 10);
    fill(&mut grid, 0, "ab", true);
    grid.scroll_up(&(Line(0)..Line(4)), 1);
    fill(&mut grid, 0, "cd", true);
    fill(&mut grid, 1, "e", false);
    fill(&mut grid, 2, "x", false);
    grid.cursor.pos = Pos::new(Line(2), Column(1));
    assert_eq!(grid.history_size(), 1);

    grid.resize(true, 4, 6);

    assert_eq!(grid.history_size(), 0);
    assert_eq!(text_of(&grid, 0), "abcde");
    assert_eq!(text_of(&grid, 1), "x");
    assert_eq!(text_of(&grid, 2), "");
    assert_eq!(grid.cursor.pos, Pos::new(Line(1), Column(1)));
}

// A cursor on the top row whose cells join a row in history: the
// joined row is pulled down through the blank rows at the bottom, one
// row, so the cursor stays on its character.
#[test]
fn a_top_row_cursor_pulls_its_joined_row_down_through_blank_rows() {
    let mut grid = Grid::<Square>::new(4, 5, 10);
    fill(&mut grid, 0, "abcde", true);
    grid.scroll_up(&(Line(0)..Line(4)), 1);
    fill(&mut grid, 0, "fghij", false);
    grid.cursor.pos = Pos::new(Line(0), Column(1));
    assert_eq!(grid[grid.cursor.pos].c(), 'g');

    grid.resize(true, 4, 7);

    assert_eq!(grid.history_size(), 0, "one blank row was dropped");
    assert_eq!(text_of(&grid, 0), "abcdefg");
    assert!(grid[Line(0)][Column(6)].wrapline());
    assert_eq!(text_of(&grid, 1), "hij");
    assert_eq!(text_of(&grid, 3), "");
    assert_eq!(grid.cursor.pos, Pos::new(Line(0), Column(6)));
    assert_eq!(grid[grid.cursor.pos].c(), 'g');
}

// `display_iter` starts one row above the viewport so that the first
// `next` lands on its first cell, also when the viewport is scrolled to
// the very top of history.
#[test]
fn display_iter_starts_at_the_top_of_the_viewport() {
    let mut grid = Grid::<Square>::new(2, 3, 5);
    fill(&mut grid, 0, "abc", false);
    fill(&mut grid, 1, "xyz", false);
    let visible = |grid: &Grid<Square>| -> String {
        grid.display_iter().map(|cell| cell.square.c()).collect()
    };
    assert_eq!(visible(&grid), "abcxyz", "no history at all");

    grid.scroll_up(&(Line(0)..Line(2)), 1);
    fill(&mut grid, 1, "def", false);
    assert_eq!(visible(&grid), "xyzdef");
    grid.scroll_display(Scroll::Delta(1));
    assert_eq!(visible(&grid), "abcxyz", "scrolled to the top of history");
}
