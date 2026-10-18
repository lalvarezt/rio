//! Prepared row geometry, bounded to the last viewport. Callers clear it when
//! the atlas or palette changes and bypass it for registered custom glyph rows and
//! composition. Ordinary partial updates keep their existing direct emit path.

use crate::{ExtrasMap, RowHint, RowSelection};
use rio_backend::config::colors::term::TermColors;
use rio_backend::crosswords::snapshot::{RowStyles, SnapshotCells};
use rio_backend::crosswords::square::Square;
use rio_backend::sugarloaf::grid::{CellBg, CellText};
use rustc_hash::FxHashMap;

pub struct RowInput<'a> {
    hash: u64,
    cells: &'a [Square],
    styles: &'a RowStyles,
    extras: &'a ExtrasMap,
    selection: Option<RowSelection>,
    hints: &'a [RowHint],
    cursor: Option<u16>,
}

impl<'a> RowInput<'a> {
    pub fn new(
        cells: &'a [Square],
        styles: &'a RowStyles,
        extras: &'a ExtrasMap,
        selection: Option<RowSelection>,
        hints: &'a [RowHint],
        cursor: Option<u16>,
    ) -> Self {
        Self {
            hash: SnapshotCells::hash(cells),
            cells,
            styles,
            extras,
            selection,
            hints,
            cursor,
        }
    }
}

#[derive(Default)]
pub struct PreparedRow {
    cells: SnapshotCells,
    styles: RowStyles,
    selection: Option<RowSelection>,
    hints: Vec<RowHint>,
    cursor: Option<u16>,
    background: Vec<CellBg>,
    foreground: Vec<CellText>,
    used: u64,
}

impl PreparedRow {
    fn matches(&self, input: &RowInput<'_>) -> bool {
        self.selection == input.selection
            && self.cursor == input.cursor
            && self.hints == input.hints
            && self.styles == *input.styles
            && self
                .cells
                .matches(input.cells, |id| input.extras.shared(&id))
    }

    pub fn restore(
        &self,
        row: u16,
        background: &mut Vec<CellBg>,
        foreground: &mut Vec<CellText>,
    ) {
        background.clone_from(&self.background);
        foreground.clone_from(&self.foreground);
        for cell in foreground {
            cell.grid_pos[1] = row;
        }
    }

    fn reset(&mut self) {
        self.cells.clear();
        self.styles.clone_from(&RowStyles::DEFAULT);
        self.hints.clear();
        self.background.clear();
        self.foreground.clear();
    }
}

struct Settings {
    columns: usize,
    size: [u32; 3],
    colors: TermColors,
}

#[derive(Default)]
pub struct PreparedRows {
    rows: FxHashMap<u64, PreparedRow>,
    spare: Vec<PreparedRow>,
    settings: Option<Box<Settings>>,
    frame: u64,
}

impl PreparedRows {
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn clear(&mut self) {
        self.rows.clear();
        self.spare.clear();
    }

    /// Start a full rebuild. Rows not reused or replaced are retired at `finish`.
    pub fn begin(&mut self, columns: usize, size: [f32; 3], colors: &TermColors) {
        let size = size.map(f32::to_bits);
        let changed = self.settings.as_deref().is_none_or(|old| {
            old.columns != columns || old.size != size || old.colors != *colors
        });
        if changed {
            self.clear();
            self.settings = Some(Box::new(Settings {
                columns,
                size,
                colors: *colors,
            }));
        }
        self.frame = self.frame.wrapping_add(1);
        if self.frame == 0 {
            self.clear();
        }
    }

    pub fn get(&mut self, input: &RowInput<'_>) -> Option<&PreparedRow> {
        let row = self.rows.get_mut(&input.hash)?;
        if !row.matches(input) {
            return None;
        }
        row.used = self.frame;
        Some(row)
    }

    pub fn put(
        &mut self,
        input: &RowInput<'_>,
        background: &[CellBg],
        foreground: &[CellText],
    ) {
        let spare = &mut self.spare;
        let row = self
            .rows
            .entry(input.hash)
            .or_insert_with(|| spare.pop().unwrap_or_default());
        row.cells.update(input.cells, |id| input.extras.shared(&id));
        row.styles.clone_from(input.styles);
        row.selection = input.selection;
        row.cursor = input.cursor;
        row.hints.clear();
        row.hints.extend_from_slice(input.hints);
        row.background.clear();
        row.background.extend_from_slice(background);
        row.foreground.clear();
        row.foreground.extend_from_slice(foreground);
        row.used = self.frame;
    }

    pub fn finish(&mut self, viewport_rows: usize) {
        let spare = &mut self.spare;
        self.rows.retain(|_, row| {
            if row.used == self.frame {
                true
            } else {
                let mut retired = std::mem::take(row);
                retired.reset();
                spare.push(retired);
                false
            }
        });
        spare.truncate(viewport_rows);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reuse_checks_content_paint_and_settings_then_relocates_geometry() {
        let cells = [Square::from_char('a'), Square::from_char('b')];
        let mut styles = RowStyles::default();
        let extras = ExtrasMap::default();
        let colors = TermColors::default();
        let mut rows = PreparedRows::default();
        rows.begin(2, [16.0, 9.0, 20.0], &colors);
        let input = RowInput::new(&cells, &styles, &extras, None, &[], None);
        let bg = [CellBg { rgba: [1, 2, 3, 4] }; 2];
        let fg = [CellText {
            grid_pos: [1, 0],
            ..CellText::default()
        }];
        rows.put(&input, &bg, &fg);
        rows.finish(1);
        rows.begin(2, [16.0, 9.0, 20.0], &colors);
        let (mut restored_bg, mut restored_fg) = (Vec::new(), Vec::new());
        rows.get(&input)
            .unwrap()
            .restore(7, &mut restored_bg, &mut restored_fg);
        assert_eq!(restored_bg, bg);
        assert_eq!(restored_fg[0].grid_pos, [1, 7]);

        let changed = [Square::from_char('x'), cells[1]];
        let mut collision = RowInput::new(&changed, &styles, &extras, None, &[], None);
        collision.hash = input.hash;
        assert!(rows.get(&collision).is_none());
        assert!(rows
            .get(&RowInput::new(
                &cells,
                &styles,
                &extras,
                Some(RowSelection { lo: 0, hi: 0 }),
                &[],
                None
            ))
            .is_none());
        assert!(rows
            .get(&RowInput::new(&cells, &styles, &extras, None, &[], Some(1)))
            .is_none());
        let hints = [RowHint {
            lo: 0,
            hi: 0,
            tag: crate::HintTag::Match,
        }];
        assert!(rows
            .get(&RowInput::new(&cells, &styles, &extras, None, &hints, None))
            .is_none());

        styles.make_mut(2)[0].flags = rio_backend::crosswords::style::StyleFlags::BOLD;
        let input = RowInput::new(&cells, &styles, &extras, None, &[], None);
        assert!(rows.get(&input).is_none());
        rows.put(&input, &bg, &fg);
        rows.finish(1);
        rows.begin(2, [17.0, 9.0, 20.0], &colors);
        assert!(rows.get(&input).is_none());
        rows.put(&input, &bg, &fg);
        let mut changed_colors = colors;
        changed_colors[0] = Some([1.0, 0.0, 0.0, 1.0]);
        rows.begin(2, [17.0, 9.0, 20.0], &changed_colors);
        assert!(rows.get(&input).is_none());
    }

    #[test]
    fn viewport_retirement_reuses_storage_without_retaining_old_content() {
        let styles = RowStyles::default();
        let extras = ExtrasMap::default();
        let colors = TermColors::default();
        let mut rows = PreparedRows::default();
        let a = [Square::from_char('a')];
        let b = [Square::from_char('b')];
        let first = RowInput::new(&a, &styles, &extras, None, &[], None);
        let second = RowInput::new(&b, &styles, &extras, None, &[], None);
        rows.begin(1, [16.0, 9.0, 20.0], &colors);
        rows.put(&first, &[], &[]);
        rows.put(&second, &[], &[]);
        rows.finish(2);
        rows.begin(1, [16.0, 9.0, 20.0], &colors);
        assert!(rows.get(&first).is_some());
        rows.finish(1);
        assert!(rows.get(&second).is_none());
        assert_eq!(rows.spare.len(), 1);
        rows.clear();
        assert!(rows.get(&first).is_none());
    }
}
