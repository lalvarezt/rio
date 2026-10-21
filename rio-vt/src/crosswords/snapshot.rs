use super::grid::{row::Row, ExtrasTable, ID_BITSET_WORDS};
use super::square::{Extras, Square};
use super::style::{Style, StyleFlags};
use rustc_hash::{FxHashMap, FxHasher};
use std::collections::hash_map::Entry;
use std::hash::Hasher;
use std::ops::Range;
use std::sync::Arc;

#[derive(Clone, Debug)]
enum ClusterData {
    Empty,
    Shared(Arc<Extras>),
    Copied(Range<usize>),
}

impl ClusterData {
    fn capture(value: Option<&Arc<Extras>>, copied: &mut Vec<char>) -> Self {
        let Some(value) = value.filter(|value| !value.zerowidth.is_empty()) else {
            return Self::Empty;
        };
        if value.hyperlink.is_none() {
            return Self::Shared(value.clone());
        }
        let start = copied.len();
        copied.extend_from_slice(&value.zerowidth);
        Self::Copied(start..copied.len())
    }
}

/// Retained cell words and combining characters for glyph/search validation.
/// Hyperlink targets do not affect these results. Share cluster-only records;
/// copy just the characters when a record also owns a hyperlink target.
#[derive(Clone, Debug, Default)]
pub struct SnapshotCells {
    cells: Vec<Square>,
    extras: FxHashMap<u16, ClusterData>,
    copied: Vec<char>,
}

impl SnapshotCells {
    pub fn clear(&mut self) {
        self.cells.clear();
        self.extras.clear();
        self.copied.clear();
    }

    pub fn hash(cells: &[Square]) -> u64 {
        let mut hash = FxHasher::default();
        hash.write_usize(cells.len());
        for cell in cells {
            hash.write_u64(cell.raw());
        }
        hash.finish()
    }

    pub fn matches<'a>(
        &self,
        cells: &[Square],
        extra: impl Fn(u16) -> Option<&'a Arc<Extras>>,
    ) -> bool {
        self.cells == cells
            && self.extras.iter().all(|(id, saved)| {
                let current = extra(*id);
                match saved {
                    ClusterData::Empty => {
                        current.is_none_or(|value| value.zerowidth.is_empty())
                    }
                    ClusterData::Shared(saved) => current.is_some_and(|current| {
                        Arc::ptr_eq(saved, current)
                            || saved.zerowidth == current.zerowidth
                    }),
                    ClusterData::Copied(range) => current.is_some_and(|current| {
                        self.copied[range.clone()] == current.zerowidth
                    }),
                }
            })
    }

    pub fn update<'a>(
        &mut self,
        cells: &[Square],
        extra: impl Fn(u16) -> Option<&'a Arc<Extras>>,
    ) {
        // A recycled long wrapped group must not inflate every later short
        // group. Keep at most the usual vector growth allowance.
        if self.cells.capacity() > cells.len().saturating_mul(2) {
            self.cells = Vec::with_capacity(cells.len());
        }
        self.cells.clear();
        self.cells.extend_from_slice(cells);
        self.extras.clear();
        self.copied.clear();
        if self.extras.capacity() > cells.len().saturating_mul(2) {
            self.extras.shrink_to(cells.len());
        }
        for cell in cells {
            if let Some(id) = cell.extras_id_checked() {
                self.extras
                    .entry(id)
                    .or_insert_with(|| ClusterData::capture(extra(id), &mut self.copied));
            }
        }
        let retained = cells.len().max(self.copied.len());
        if self.copied.capacity() > retained.saturating_mul(2) {
            self.copied.shrink_to(retained);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StyleRun {
    end: usize,
    style: Style,
}

// Most rows have a few text/padding runs. Keep caching bounded when an
// application deliberately alternates styles at every cell.
const MAX_STYLE_RUNS: usize = 16;

/// Resolved styles owned by a row snapshot. Uniform rows store one value;
/// mixed rows keep an index-parallel vector. Values remain valid when the
/// terminal reuses style IDs after the snapshot lock is released.
#[derive(Debug, Default)]
pub struct RowStyles {
    uniform: Style,
    mixed: Vec<Style>,
    applied: Vec<StyleRun>,
    pending: Vec<StyleRun>,
}

impl PartialEq for RowStyles {
    fn eq(&self, other: &Self) -> bool {
        self.uniform == other.uniform && self.mixed == other.mixed
    }
}

impl Eq for RowStyles {}

impl Clone for RowStyles {
    fn clone(&self) -> Self {
        Self {
            uniform: self.uniform,
            mixed: self.mixed.clone(),
            // Copies may be paired with different cells by a caller. Only
            // resolution against that row may establish reusable metadata.
            applied: Vec::new(),
            pending: Vec::new(),
        }
    }

    fn clone_from(&mut self, source: &Self) {
        self.uniform = source.uniform;
        self.mixed.clone_from(&source.mixed);
        self.applied.clear();
        self.pending.clear();
    }
}

impl RowStyles {
    pub const DEFAULT: Self = Self {
        uniform: Style::DEFAULT,
        mixed: Vec::new(),
        applied: Vec::new(),
        pending: Vec::new(),
    };

    #[inline]
    pub fn at(&self, column: usize) -> Style {
        self.mixed.get(column).copied().unwrap_or(self.uniform)
    }

    /// One style for the whole row, or `None` for mixed styles.
    #[inline]
    pub fn uniform(&self) -> Option<Style> {
        self.mixed.is_empty().then_some(self.uniform)
    }

    #[inline]
    pub fn flags(&self) -> StyleFlags {
        self.mixed
            .iter()
            .fold(self.uniform.flags, |flags, style| flags | style.flags)
    }

    /// Expand only when an overlay needs to edit individual cell styles.
    pub fn make_mut(&mut self, columns: usize) -> &mut [Style] {
        // Overlays change the resolved values without changing source cells.
        self.applied.clear();
        self.mixed.resize(columns, self.uniform);
        &mut self.mixed
    }

    pub(super) fn set_uniform(&mut self, style: Style) {
        self.uniform = style;
        self.mixed.clear();
        self.applied.clear();
        self.pending.clear();
    }

    pub(super) fn set_mixed(
        &mut self,
        prefix: usize,
        style: Style,
        remainder: impl ExactSizeIterator<Item = Style>,
    ) {
        self.applied.clear();
        self.pending.clear();
        self.uniform = Style::DEFAULT;
        self.mixed.clear();
        self.mixed.reserve(prefix + remainder.len());
        self.mixed.resize(prefix, style);
        self.mixed.extend(remainder);
    }

    pub(super) fn resolve(&mut self, src: &Row<Square>, styles: &[Style]) {
        if !src.has_styles {
            self.set_uniform(Style::DEFAULT);
            return;
        }
        let first = src
            .inner
            .first()
            .and_then(|sq| sq.style_id_checked())
            .unwrap_or(0);
        let style = styles.get(first as usize).copied().unwrap_or_default();
        match src
            .inner
            .iter()
            .position(|sq| sq.style_id_checked().unwrap_or(0) != first)
        {
            None => {
                self.set_uniform(style);
                self.applied.push(StyleRun {
                    end: src.inner.len(),
                    style,
                });
            }
            Some(prefix) => {
                // A moving viewport often changes the leading style. It
                // already proves the old run key cannot match, so rebuild
                // directly instead of constructing another failed key.
                if self.at(0) != style {
                    self.resolve_mixed(src, prefix, style, styles);
                    return;
                }
                self.pending.clear();
                self.pending.push(StyleRun { end: prefix, style });
                let mut start = prefix;
                while start < src.inner.len() {
                    if self.pending.len() == MAX_STYLE_RUNS {
                        self.resolve_mixed(src, prefix, style, styles);
                        return;
                    }
                    let id = src.inner[start].style_id_checked().unwrap_or(0);
                    let end = src.inner[start..]
                        .iter()
                        .position(|sq| sq.style_id_checked().unwrap_or(0) != id)
                        .map_or(src.inner.len(), |offset| start + offset);
                    self.pending.push(StyleRun {
                        end,
                        style: styles.get(id as usize).copied().unwrap_or_default(),
                    });
                    start = end;
                }
                // Like Ghostty's applied style runs, compare compact resolved
                // values before rewriting a whole row. Store values, not IDs:
                // sweeps/grid replacement may recycle IDs between snapshots.
                if self.pending == self.applied {
                    return;
                }
                self.uniform = Style::DEFAULT;
                self.mixed.clear();
                self.mixed.reserve(src.inner.len());
                for run in &self.pending {
                    self.mixed.resize(run.end, run.style);
                }
                std::mem::swap(&mut self.applied, &mut self.pending);
                self.pending.clear();
            }
        }
    }

    fn resolve_mixed(
        &mut self,
        src: &Row<Square>,
        prefix: usize,
        style: Style,
        styles: &[Style],
    ) {
        self.set_mixed(
            prefix,
            style,
            src.inner[prefix..].iter().map(|sq| {
                sq.style_id_checked()
                    .and_then(|id| styles.get(id as usize).copied())
                    .unwrap_or_default()
            }),
        );
    }

    // The caller compared source metadata with the previously captured cells.
    // Validate palette values at the retained run boundaries: IDs alone are
    // insufficient because terminal sweeps may reuse them.
    pub(super) fn resolve_unchanged(&mut self, src: &Row<Square>, styles: &[Style]) {
        if !src.has_styles {
            self.set_uniform(Style::DEFAULT);
            return;
        }
        if src.inner.is_empty()
            || self.applied.last().map(|run| run.end) != Some(src.inner.len())
        {
            self.resolve(src, styles);
            return;
        }
        let mut start = 0;
        for run in &self.applied {
            let current = src.inner[start]
                .style_id_checked()
                .and_then(|id| styles.get(id as usize))
                .copied()
                .unwrap_or_default();
            if current != run.style {
                self.resolve(src, styles);
                return;
            }
            start = run.end;
        }
    }
}

/// Scratch storage for moving style expansion outside the terminal lock.
/// Owns the captured palette: terminal sweeps may reuse IDs before resolution.
#[derive(Debug, Default)]
pub struct DeferredStyles {
    palette: Vec<Style>,
    rows: Vec<(usize, bool)>,
}

/// Captured rows remain borrowed until their styles have been resolved. This
/// token does not borrow the terminal, so it can outlive the terminal lock.
#[must_use = "resolve the captured snapshot after releasing the terminal lock"]
pub struct CapturedStyles<'a> {
    pub(super) rows: &'a [Row<Square>],
    pub(super) styles: &'a mut [RowStyles],
    pub(super) deferred: &'a mut DeferredStyles,
    pub(super) all_rows_modified: bool,
}

impl CapturedStyles<'_> {
    /// Finish style expansion and report whether every source row was modified.
    pub fn resolve(self) -> bool {
        // Drop completes expansion before the caller can read the rows/styles.
        self.all_rows_modified
    }
}

impl Drop for CapturedStyles<'_> {
    fn drop(&mut self) {
        self.deferred.resolve(self.rows, self.styles);
    }
}

impl DeferredStyles {
    pub(super) fn begin(&mut self, styles: &[Style], cells: usize) {
        self.rows.clear();
        self.palette.clear();
        // Bound palette copying to half the potential per-cell style output.
        // A long-lived session can retain far more styles than its dirty rows
        // need; resolve eagerly instead of copying that whole table.
        if styles.len() <= cells / 2 {
            self.palette.extend_from_slice(styles);
        }
    }

    pub(super) fn capture(&mut self, row: usize, unchanged: bool) -> bool {
        if self.palette.is_empty() {
            return false;
        }
        self.rows.push((row, unchanged));
        true
    }

    fn resolve(&mut self, rows: &[Row<Square>], styles: &mut [RowStyles]) {
        for (row, unchanged) in self.rows.drain(..) {
            if unchanged {
                styles[row].resolve_unchanged(&rows[row], &self.palette);
            } else {
                styles[row].resolve(&rows[row], &self.palette);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_cells_verify_values_and_reused_extra_ids() {
        let mut cell = Square::from_char('e');
        cell.set_extras_id(Some(7));
        let cells = [cell];
        let first = Arc::new(Extras {
            zerowidth: vec!['\u{301}'],
            ..Extras::default()
        });
        let second = Arc::new(Extras {
            zerowidth: vec!['\u{308}'],
            ..Extras::default()
        });
        let mut saved = SnapshotCells::default();
        saved.update(&cells, |_| Some(&first));
        assert!(saved.matches(&cells, |_| Some(&first)));
        assert!(!saved.matches(&cells, |_| Some(&second)));
        assert!(!saved.matches(&cells, |_| None));
        assert!(!saved.matches(&[Square::from_char('x')], |_| Some(&first)));
        saved.update(&cells, |_| None);
        assert!(saved.matches(&cells, |_| None));
        assert!(!saved.matches(&cells, |_| Some(&second)));
    }

    #[test]
    fn glyph_keys_release_hyperlink_payloads_and_keep_only_cluster_text() {
        use crate::crosswords::square::Hyperlink;
        let mut cell = Square::from_char('e');
        cell.set_extras_id(Some(7));
        for zerowidth in [Vec::new(), vec!['\u{301}']] {
            let value = Arc::new(Extras {
                zerowidth: zerowidth.clone(),
                hyperlink: Some(Hyperlink::new(
                    None::<&str>,
                    "https://example.invalid/cache",
                )),
            });
            let weak = Arc::downgrade(&value);
            let mut saved = SnapshotCells::default();
            saved.update(&[cell], |_| Some(&value));
            assert_eq!(Arc::strong_count(&value), 1);
            drop(value);
            assert!(weak.upgrade().is_none());
            let replacement = Arc::new(Extras {
                zerowidth,
                hyperlink: None,
            });
            assert!(saved.matches(&[cell], |_| Some(&replacement)));
        }
    }

    #[test]
    fn repeated_style_runs_refresh_recycled_values_and_remove_overlays() {
        let bold = Style {
            flags: StyleFlags::BOLD,
            ..Style::DEFAULT
        };
        let italic = Style {
            flags: StyleFlags::ITALIC,
            ..Style::DEFAULT
        };
        let mut row = Row::<Square>::new(8);
        row.has_styles = true;
        for cell in &mut row.inner[..4] {
            cell.set_style_id(1);
        }
        let mut styles = RowStyles::default();
        let mut palette = vec![Style::DEFAULT, bold];
        styles.resolve(&row, &palette);
        let frozen = styles.clone();
        row.inner[0].set_c('x');
        styles.resolve_unchanged(&row, &palette);
        assert_eq!(styles.at(0), bold);
        assert_eq!(styles.at(7), Style::DEFAULT);
        styles.make_mut(8)[0] = italic;
        styles.resolve_unchanged(&row, &palette);
        assert_eq!(styles.at(0), bold);
        palette[1] = italic;
        styles.resolve_unchanged(&row, &palette);
        assert_eq!(styles.at(0), italic);
        assert_eq!(styles.at(3), italic);
        assert_eq!(styles.at(7), Style::DEFAULT);
        assert_eq!(frozen.at(0), bold);
    }

    #[test]
    fn fragmented_style_rows_follow_palette_and_geometry_changes() {
        let mut row = Row::<Square>::new(64);
        row.has_styles = true;
        for (i, cell) in row.inner.iter_mut().enumerate() {
            cell.set_style_id((i % 2) as u16);
        }
        let mut palette = vec![
            Style::DEFAULT,
            Style {
                flags: StyleFlags::BOLD,
                ..Style::DEFAULT
            },
        ];
        let mut styles = RowStyles::default();
        for columns in [64, 32, 8] {
            row.inner.truncate(columns);
            palette[1].flags = StyleFlags::ITALIC;
            styles.resolve(&row, &palette);
            for (i, cell) in row.inner.iter().enumerate() {
                assert_eq!(
                    styles.at(i),
                    palette[cell.style_id_checked().unwrap_or(0) as usize]
                );
            }
        }
    }

    #[test]
    fn extras_refresh_preserves_shared_rows_and_releases_removed_memberships() {
        use crate::crosswords::square::Hyperlink;
        let mut table = ExtrasTable::new();
        let a = table.alloc(Extras {
            hyperlink: Some(Hyperlink::new(None::<&str>, "https://a.invalid")),
            ..Extras::default()
        });
        let b = table.alloc(Extras {
            hyperlink: Some(Hyperlink::new(None::<&str>, "https://b.invalid")),
            ..Extras::default()
        });
        let mut row = Row::<Square>::new(8);
        row.has_extras = true;
        for cell in &mut row.inner {
            cell.set_extras_id(Some(a));
        }
        let mut extras = SnapshotExtras::default();
        let table_references = Arc::strong_count(table.shared(a).unwrap());
        extras.begin(2, true);
        extras.refresh(0, &row, &table);
        extras.refresh(1, &row, &table);
        for _ in 0..3 {
            extras.begin(2, false);
            extras.refresh(0, &row, &table);
            assert_eq!(
                Arc::strong_count(table.shared(a).unwrap()),
                table_references + 1
            );
        }
        row.inner[0].set_extras_id(Some(b));
        extras.begin(2, false);
        extras.refresh(1, &row, &table);
        assert!(extras.get(&a).is_some());
        assert!(extras.get(&b).is_some());
        row.inner.fill(Square::default());
        row.has_extras = false;
        extras.begin(2, false);
        extras.refresh(0, &row, &table);
        assert!(extras.get(&a).is_some());
        extras.refresh(1, &row, &table);
        assert!(extras.is_empty());
        assert_eq!(
            Arc::strong_count(table.shared(a).unwrap()),
            table_references
        );
        assert_eq!(
            Arc::strong_count(table.shared(b).unwrap()),
            table_references
        );
    }

    #[test]
    fn overlays_expand_uniform_styles_and_reuse_mixed_storage() {
        let bold = Style {
            flags: StyleFlags::BOLD,
            ..Style::DEFAULT
        };
        let italic = Style {
            flags: StyleFlags::ITALIC,
            ..Style::DEFAULT
        };
        let mut styles = RowStyles::default();
        styles.set_uniform(bold);
        let frozen = styles.clone();
        styles.make_mut(160)[79] = italic;
        assert_eq!(styles.at(0), bold);
        assert_eq!(styles.at(79), italic);
        assert_eq!(styles.at(159), bold);
        assert_eq!(styles.flags(), StyleFlags::BOLD | StyleFlags::ITALIC);
        assert_eq!(frozen.uniform(), Some(bold));
        assert_eq!(frozen.at(79), bold);

        let allocation = styles.mixed.as_ptr();
        styles.set_uniform(italic);
        assert_eq!(styles.uniform(), Some(italic));
        assert_eq!(styles.flags(), StyleFlags::ITALIC);
        styles.make_mut(160)[0] = bold;
        assert_eq!(styles.mixed.as_ptr(), allocation);
        assert_eq!(styles.at(79), italic);
    }
}

#[derive(Clone, Debug)]
struct SnapshotExtra {
    value: Option<Arc<Extras>>,
    rows: usize,
    refreshed: u64,
}

impl SnapshotExtra {
    #[inline]
    fn refresh(&mut self, id: u16, table: &ExtrasTable, generation: u64) {
        if self.refreshed == generation {
            return;
        }
        let current = table.shared(id);
        let same = match (self.value.as_ref(), current) {
            (Some(saved), Some(current)) => Arc::ptr_eq(saved, current),
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.value = current.cloned();
        }
        self.refreshed = generation;
    }
}

/// Immutable extras referenced by the snapshot, with dependencies per row.
/// Terminal IDs can be recycled, so a dirty row refreshes its IDs against the
/// live table once per update, even when the snapshot already contains them.
/// Row references release offscreen payloads without cloning shared extras per cell.
#[derive(Clone, Debug, Default)]
pub struct SnapshotExtras {
    entries: FxHashMap<u16, SnapshotExtra>,
    rows: Vec<Vec<u16>>,
    seen: Vec<u64>,
    generation: u64,
}

impl SnapshotExtras {
    #[inline]
    pub fn get(&self, id: &u16) -> Option<&Extras> {
        self.entries.get(id)?.value.as_deref()
    }

    pub fn shared(&self, id: &u16) -> Option<&Arc<Extras>> {
        self.entries.get(id)?.value.as_ref()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(super) fn row_count(&self) -> usize {
        self.rows.len()
    }

    pub(super) fn begin(&mut self, count: usize, full: bool) {
        if full {
            if !self.entries.is_empty() {
                self.entries.clear();
                for row in &mut self.rows {
                    row.clear();
                }
            }
            self.rows.resize_with(count, Vec::new);
        }
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            for entry in self.entries.values_mut() {
                entry.refreshed = u64::MAX;
            }
        }
    }

    #[inline]
    pub(super) fn refresh_unchanged(&mut self, y: usize, table: &ExtrasTable) {
        for id in &self.rows[y] {
            if let Some(entry) = self.entries.get_mut(id) {
                entry.refresh(*id, table, self.generation);
            }
        }
    }

    #[inline]
    pub(super) fn refresh(&mut self, y: usize, row: &Row<Square>, table: &ExtrasTable) {
        let ids = &mut self.rows[y];
        if !row.has_extras && ids.is_empty() {
            return;
        }
        for id in ids.drain(..) {
            if let Entry::Occupied(mut entry) = self.entries.entry(id) {
                entry.get_mut().rows -= 1;
                if entry.get().rows == 0 {
                    entry.remove();
                }
            }
        }
        if !row.has_extras {
            return;
        }
        if self.seen.is_empty() {
            if !row.inner.iter().any(|cell| cell.has_extras()) {
                return;
            }
            self.seen.resize(ID_BITSET_WORDS, 0);
        }
        for cell in &row.inner {
            let Some(id) = cell.extras_id_checked() else {
                continue;
            };
            let word = id as usize / 64;
            let bit = 1 << (id % 64);
            if self.seen[word] & bit != 0 {
                continue;
            }
            self.seen[word] |= bit;
            ids.push(id);
            match self.entries.entry(id) {
                Entry::Occupied(mut entry) => {
                    let entry = entry.get_mut();
                    entry.rows += 1;
                    entry.refresh(id, table, self.generation);
                }
                Entry::Vacant(entry) => {
                    entry.insert(SnapshotExtra {
                        value: table.shared(id).cloned(),
                        rows: 1,
                        refreshed: self.generation,
                    });
                }
            }
        }
        for &id in ids.iter() {
            self.seen[id as usize / 64] &= !(1 << (id % 64));
        }
    }
}
