// grid/resize.rs was originally taken from Alacritty
// https://github.com/alacritty/alacritty/blob/e35e5ad14fce8456afdd89f2b392b9924bb27471/alacritty_terminal/src/grid/resize.rs
// which is licensed under Apache 2.0 license.

use crate::crosswords::grid::{
    Dimensions, Grid, GridSquare, ReflowRemap, ViewportAnchor,
};
use crate::crosswords::pos::{Column, Line};
use crate::crosswords::square::{Square, Wide};
use crate::crosswords::Row;
use std::cmp::{min, Ordering};
use std::mem;

impl Grid<Square> {
    /// Resize the grid's width and/or height.
    pub fn resize(&mut self, reflow: bool, lines: usize, columns: usize) {
        let was_pending = self.cursor.should_wrap;
        let mut cursor_lost = false;
        self.apply_history_byte_limit(columns);
        // Use empty template cell for resetting cells due to resize.
        let template = mem::take(&mut self.cursor.template);

        // Only the column passes below produce a row remap; a stale
        // one from an earlier resize must not leak through.
        self.reflow_remap = None;

        if !reflow && columns != self.columns {
            self.resize_columns_without_reflow(columns);
        }

        // Wider columns must unwrap before changing the viewport height.
        let grow_columns_first = reflow && columns > self.columns;
        if grow_columns_first {
            self.grow_columns(reflow, columns);
        }
        match self.lines.cmp(&lines) {
            Ordering::Less => self.grow_lines(lines, columns >= self.columns),
            Ordering::Greater => cursor_lost = self.shrink_lines(lines),
            Ordering::Equal => (),
        }

        match self.columns.cmp(&columns) {
            Ordering::Less if !grow_columns_first => self.grow_columns(reflow, columns),
            Ordering::Greater => self.shrink_columns(reflow, columns),
            _ => (),
        }

        // Restore template cell.
        self.cursor.template = template;
        if cursor_lost && was_pending {
            self.cursor.pos.col = Column(1.min(columns - 1));
            self.cursor.should_wrap = columns == 1;
        }
        self.apply_history_byte_limit(columns);
        self.display_anchor = match self.display_anchor {
            ViewportAnchor::Top => {
                self.display_offset = self.history_size();
                ViewportAnchor::Top
            }
            ViewportAnchor::Cell(col) if self.display_offset != 0 => {
                ViewportAnchor::Cell(col.min(columns - 1))
            }
            _ => {
                self.display_offset = 0;
                ViewportAnchor::Active
            }
        };
    }

    /// Add lines to the visible area.
    ///
    /// Pull history only when the cursor is at the old bottom. Otherwise
    /// append blanks so growth preserves its row, as Ghostty does.
    fn grow_lines(&mut self, target: usize, pull_scrollback: bool) {
        let lines_added = target - self.lines;
        // Ghostty's intermediate row resize can absorb a cell pin into
        // the active area before a subsequent column shrink reflows it.
        if matches!(self.display_anchor, ViewportAnchor::Cell(_))
            && !pull_scrollback
            && self.display_offset <= lines_added
        {
            self.display_offset = 0;
            self.display_anchor = ViewportAnchor::Active;
        }
        let pull_history =
            pull_scrollback && self.cursor.pos.row.0 as usize == self.lines - 1;

        // Need to resize before updating buffer.
        self.raw.grow_visible_lines(target);
        self.lines = target;

        let history_size = self.history_size();
        let from_history = if pull_history {
            min(history_size, lines_added)
        } else {
            0
        };

        // Move existing lines up for every line that couldn't be pulled from history.
        if from_history != lines_added {
            let delta = lines_added - from_history;
            // Append without scrolling: no history may be evicted merely
            // because the window grew, even when the history is full.
            self.raw.rotate(-(delta as isize));
            self.reset_rows(Line((target - delta) as i32)..Line(target as i32));
        }

        // Move cursor down for every line pulled from history.
        self.saved_cursor.pos.row += from_history;
        self.cursor.pos.row += from_history;

        // The viewport absorbed exactly the rows pulled out of history,
        // not the full growth (history may have had fewer). Clamp like
        // the reflow paths do: an offset past the remaining history makes
        // `visible_line_bounds` request rows the storage doesn't have.
        self.display_offset = self.display_offset.saturating_sub(from_history);
        self.decrease_scroll_limit(from_history);
        self.display_offset = min(self.display_offset, self.history_size());
    }

    /// Remove lines from the visible area.
    ///
    /// Trim trailing text-free rows where possible, preserving cursor and
    /// image pins. Move the remaining excess rows into history.
    fn shrink_lines(&mut self, target: usize) -> bool {
        let shrinkage = self.lines - target;
        let mut trimmed = 0;
        let active_top = self.lines_evicted() as i64 + self.history_size() as i64;
        for row in (target..self.lines).rev() {
            if self.cursor.pos.row.0 as usize == row
                || self.saved_cursor.pos.row.0 as usize == row
                || self
                    .reflow_protected_rows
                    .contains(&(active_top + row as i64))
                || self[Line(row as i32)].inner.iter().any(|cell| {
                    !cell.is_bg_only()
                        && !matches!(cell.c(), '\0' | '\t')
                        && !matches!(cell.wide(), Wide::Spacer | Wide::LeadingSpacer)
                })
            {
                break;
            }
            trimmed += 1;
        }
        let required_scrolling = shrinkage - trimmed;
        let cursor_lost = self.cursor.pos.row.0 < required_scrolling as i32;
        if required_scrolling > 0 {
            self.scroll_up(&(Line(0)..Line(self.lines as i32)), required_scrolling);
            if self.cursor.pos.row.0 < required_scrolling as i32 {
                self.cursor.pos.row = Line(0);
                self.cursor.pos.col = Column(0);
                self.cursor.should_wrap = false;
            } else {
                self.cursor.pos.row -= required_scrolling;
            }
        }

        // Keep the saved cursor attached to the same cell when rows move into
        // history. If that cell leaves the active viewport, reset its position
        // like Ghostty's saved-cursor pin fallback.
        let saved_row = self.saved_cursor.pos.row - required_scrolling;
        if saved_row.0 >= 0 && saved_row.0 < target as i32 {
            self.saved_cursor.pos.row = saved_row;
        } else {
            self.saved_cursor.pos.row = Line(0);
            self.saved_cursor.pos.col = Column(0);
            self.saved_cursor.should_wrap = false;
        }

        self.raw.rotate(shrinkage as isize);
        self.raw.shrink_visible_lines(target);
        self.lines = target;
        // Rows moved into history above; same defensive clamp as
        // `grow_lines` and the column reflows.
        self.display_offset = min(self.display_offset, self.history_size());
        cursor_lost
    }

    /// Resize columns without rebuilding rows when no cells need reflow.
    fn grow_columns(&mut self, reflow: bool, columns: usize) {
        if !reflow {
            self.resize_columns_without_reflow(columns);
        } else if !self.track_reflow_remap
            && self
                .raw
                .rows()
                .all(|row| !row[Column(row.len() - 1)].wrapline())
        {
            self.columns = columns;
            self.raw.grow_all_rows(columns);
            for cursor in [&mut self.cursor, &mut self.saved_cursor] {
                if cursor.should_wrap {
                    cursor.should_wrap = false;
                    cursor.pos.col += 1;
                }
            }
        } else {
            self.reflow_columns(columns);
        }
    }

    fn shrink_columns(&mut self, reflow: bool, columns: usize) {
        if !reflow {
            self.resize_columns_without_reflow(columns);
        } else if !self.track_reflow_remap
            && self.cursor.pos.col.0 < columns
            && self.saved_cursor.pos.col.0 < columns
            && self
                .raw
                .rows()
                .all(|row| row.inner[columns..].iter().all(GridSquare::is_empty))
        {
            self.columns = columns;
            self.raw.shrink_all_rows(columns);
        } else {
            self.reflow_columns(columns);
        }
    }

    fn resize_columns_without_reflow(&mut self, columns: usize) {
        // Spare storage rows must not retain an obsolete width and later
        // reappear when the viewport grows.
        self.raw.truncate();
        let old_columns = self.columns;
        if self.track_reflow_remap {
            self.reflow_remap = Some(ReflowRemap {
                base_abs: self.lines_evicted(),
                new_pos: (0..self.raw.len()).map(|row| row as i64).collect(),
                cells: self
                    .reflow_protected_cells
                    .iter()
                    .map(|&(row, col)| ((row, col), (row, col.min(columns - 1))))
                    .collect(),
            });
        }
        for line in self.topmost_line().0..self.lines as i32 {
            let row = &mut self.raw[Line(line)];
            let wrap = row[Column(old_columns - 1)].wrapline();
            row[Column(old_columns - 1)].set_wrapline(false);
            if columns > old_columns {
                row.grow(columns);
                row.wrap_continuation = false;
            } else {
                if row[Column(columns - 1)].wide() == Wide::Wide {
                    row[Column(columns - 1)] = Square::default();
                }
                row.truncate_columns(columns);
            }
            row[Column(columns - 1)].set_wrapline(wrap && columns < old_columns);
        }
        self.columns = columns;
        for cursor in [&mut self.cursor, &mut self.saved_cursor] {
            cursor.pos.col = min(cursor.pos.col, Column(columns - 1));
            if cursor.should_wrap && cursor.pos.col.0 != columns - 1 {
                cursor.should_wrap = false;
                cursor.pos.col += 1;
            }
        }
    }

    /// Reflow oldest-first, retaining the actual cell under each cursor.
    /// Empty hard lines are deferred so trailing blanks can be replaced by
    /// the new viewport padding. This follows Ghostty's ReflowCursor policy.
    fn reflow_columns(&mut self, columns: usize) {
        let old_columns = self.columns;
        let cursor_row = self.history_size() + self.cursor.pos.row.0 as usize;
        let saved_row = self.history_size() + self.saved_cursor.pos.row.0 as usize;
        let cursor_col = self.cursor.pos.col.0;
        let mut saved_col = self.saved_cursor.pos.col.0;
        let remaining = self.lines - self.cursor.pos.row.0 as usize - 1;
        let old_top = self.history_size();
        let was_scrolled = self.display_anchor != ViewportAnchor::Active;
        let (viewport_row, mut viewport_col) = match self.display_anchor {
            ViewportAnchor::Cell(col) => (Some(old_top - self.display_offset), col),
            _ => (None, 0),
        };
        let mut viewport_location = None;
        let base_abs = self.lines_evicted();
        let mut source = self.raw.take_all();
        source.reverse();
        let old_wrapped = (old_top..=cursor_row)
            .filter(|&row| source[row].wrap_continuation)
            .count();
        let mut remap = self.track_reflow_remap.then(|| ReflowRemap {
            base_abs,
            new_pos: vec![-1; source.len()],
            cells: std::collections::HashMap::new(),
        });
        let mut output = vec![Row::<Square>::new(columns)];
        let mut col = 0;
        let mut deferred = 0;
        let mut cursor_location = None;
        let mut saved_location = None;
        for (old_row, row) in source.iter().enumerate() {
            let wrapped = row[Column(old_columns - 1)].wrapline();
            let mut len = if wrapped {
                old_columns
            } else {
                row.inner
                    .iter()
                    .rposition(|cell| !cell.is_empty())
                    .map_or(0, |i| i + 1)
            };
            if row.semantic_prompt != super::row::SemanticPrompt::None {
                len = len.max(1);
            }
            let start = if deferred != 0 || col == columns {
                0
            } else {
                col
            };
            if old_row == saved_row {
                if saved_col >= len {
                    saved_col = saved_col.min(columns - 1 - start);
                }
                len = len.max(saved_col + 1);
            }
            let abs_row = base_abs as i64 + old_row as i64;
            let pins: Vec<_> = self
                .reflow_protected_cells
                .iter()
                .filter_map(|&(row, col)| {
                    if row != abs_row {
                        return None;
                    }
                    let x = if col >= len {
                        col.min(columns - 1 - start)
                    } else {
                        col
                    };
                    len = len.max(x + 1);
                    Some(((row, col), x))
                })
                .collect();
            if old_row == cursor_row {
                len = len.max(cursor_col + 1);
            }
            if Some(old_row) == viewport_row {
                if viewport_col >= len {
                    viewport_col = viewport_col.min(columns - 1 - start);
                }
                len = len.max(viewport_col + 1);
            }
            let protected = self.reflow_protected_rows.iter().any(|&pin| {
                u64::try_from(pin).ok() == base_abs.checked_add(old_row as u64)
            });
            if protected {
                len = len.max(1);
            }
            if len == 0 {
                if !row.wrap_continuation {
                    deferred += 1;
                }
                continue;
            }
            if deferred != 0 {
                output.resize_with(output.len() + deferred, || Row::new(columns));
                col = 0;
                deferred = 0;
            }
            output.last_mut().unwrap().semantic_prompt = row.semantic_prompt;
            let mut x = 0;
            while x < len {
                if col == columns {
                    output.last_mut().unwrap()[Column(columns - 1)].set_wrapline(true);
                    let mut next = Row::new(columns);
                    next.semantic_prompt = row.semantic_prompt;
                    next.wrap_continuation = true;
                    output.push(next);
                    col = 0;
                }
                let position = (output.len() - 1, col);
                if old_row == cursor_row && x == cursor_col && cursor_location.is_none() {
                    cursor_location = Some(position);
                }
                if old_row == saved_row && x == saved_col && saved_location.is_none() {
                    saved_location = Some(position);
                }
                if Some(old_row) == viewport_row
                    && x == viewport_col
                    && viewport_location.is_none()
                {
                    viewport_location = Some(position);
                }
                if let Some(remap) = remap.as_mut() {
                    for &(key, pin_x) in &pins {
                        if pin_x == x {
                            remap.cells.entry(key).or_insert((
                                base_abs as i64 + position.0 as i64,
                                position.1,
                            ));
                        }
                    }
                    if remap.new_pos[old_row] == -1 {
                        remap.new_pos[old_row] = position.0 as i64;
                    }
                }
                let mut cell = row.inner.get(x).copied().unwrap_or_default();
                cell.set_wrapline(false);
                if cell.wide() == Wide::LeadingSpacer {
                    x += 1;
                    continue;
                }
                if cell.wide() == Wide::Wide && columns > 1 && col == columns - 1 {
                    let mut spacer = Square::default();
                    spacer.set_wide(Wide::LeadingSpacer);
                    output.last_mut().unwrap()[Column(col)] = spacer;
                    col += 1;
                    continue;
                }
                let skip_tail = cell.wide() == Wide::Wide && columns == 1;
                if columns == 1 && matches!(cell.wide(), Wide::Wide | Wide::Spacer) {
                    cell = Square::default();
                }
                let target = output.last_mut().unwrap();
                target[Column(col)] = cell;
                target.has_extras |= cell.extras_id().is_some();
                target.has_styles |= cell.carries_style();
                target.kitty_virtual_placeholder |=
                    cell.c() == crate::ansi::kitty_virtual::PLACEHOLDER;
                col += 1;
                if skip_tail {
                    if let Some(remap) = remap.as_mut() {
                        for &(key, pin_x) in &pins {
                            if pin_x == x + 1 {
                                remap.cells.entry(key).or_insert((
                                    base_abs as i64 + output.len() as i64 - 1,
                                    col.min(columns - 1),
                                ));
                            }
                        }
                    }
                    if old_row == cursor_row && x + 1 == cursor_col {
                        cursor_location = Some((output.len() - 1, col.min(columns - 1)));
                    }
                    if old_row == saved_row && x + 1 == saved_col {
                        saved_location = Some((output.len() - 1, col.min(columns - 1)));
                    }
                    if Some(old_row) == viewport_row && x + 1 == viewport_col {
                        viewport_location =
                            Some((output.len() - 1, col.min(columns - 1)));
                    }
                    x += 2;
                } else {
                    x += 1;
                }
            }
            if !wrapped {
                deferred += 1;
            }
        }
        output.resize_with(output.len().max(self.lines), || Row::new(columns));
        if let Some((row, _)) = cursor_location {
            let top = output.len().saturating_sub(self.lines);
            if row >= top {
                let new_wrapped = (top..=row)
                    .filter(|&row| output[row].wrap_continuation)
                    .count();
                let wanted =
                    remaining.saturating_sub(new_wrapped.saturating_sub(old_wrapped));
                let current = output.len() - row - 1;
                if wanted > current {
                    output.resize_with(output.len() + wanted - current, || {
                        Row::new(columns)
                    });
                }
            }
        }
        let top = output.len().saturating_sub(self.lines);
        let map_cursor = |location: Option<(usize, usize)>, pending: bool| {
            let Some((row, mut col)) = location.filter(|&(row, _)| row >= top) else {
                return (crate::crosswords::pos::Pos::new(Line(0), Column(0)), false);
            };
            let pending = pending && col == columns - 1;
            col = col.min(columns - 1);
            (
                crate::crosswords::pos::Pos::new(Line((row - top) as i32), Column(col)),
                pending,
            )
        };
        let live_pending = self.cursor.should_wrap;
        let saved_pending = self.saved_cursor.should_wrap;
        let (mut live, live_wrap) = map_cursor(cursor_location, live_pending);
        let (mut saved, saved_wrap) = map_cursor(saved_location, saved_pending);
        if live_pending && !live_wrap {
            live.col = Column((live.col.0 + 1).min(columns - 1));
        }
        if saved_pending
            && !saved_wrap
            && saved_location.is_some_and(|(row, _)| row >= top)
        {
            saved.col = Column((saved.col.0 + 1).min(columns - 1));
        }
        self.columns = columns;
        self.cursor.pos = live;
        self.cursor.should_wrap = live_wrap;
        self.saved_cursor.pos = saved;
        self.saved_cursor.should_wrap = saved_wrap;
        let cap = self.max_scroll_limit + self.lines;
        let dropped = output.len().saturating_sub(cap);
        if output.len() > cap {
            self.total_lines_scrolled += (output.len() - cap) as u64;
        }
        output.reverse();
        output.truncate(cap);
        self.raw.replace_inner(output);
        if !was_scrolled {
            self.display_offset = 0;
            self.display_anchor = ViewportAnchor::Active;
        } else if let Some((row, col)) =
            viewport_location.filter(|&(row, _)| row >= dropped)
        {
            self.display_offset = top.saturating_sub(row).min(self.history_size());
            self.display_anchor = if self.display_offset == 0 {
                ViewportAnchor::Active
            } else {
                ViewportAnchor::Cell(col)
            };
        } else {
            // A top-pinned viewport follows the oldest retained row. The
            // same fallback applies when history pruning evicts a cell pin.
            self.display_offset = self.history_size();
            self.display_anchor = ViewportAnchor::Top;
        }
        self.reflow_remap = remap;
    }
}
