use super::grid::{row::Row, ExtrasTable, ID_BITSET_WORDS};
use super::square::{Extras, Square};
use rustc_hash::FxHashMap;
use std::collections::hash_map::Entry;
use std::sync::Arc;

#[derive(Clone, Debug)]
struct SnapshotExtra {
    value: Option<Arc<Extras>>,
    rows: usize,
    refreshed: u64,
}

/// Immutable extras referenced by the snapshot, with dependencies per row.
/// Terminal IDs can be recycled, so a dirty row refreshes its IDs against the
/// live table once per update, even when the snapshot already contains them.
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
                    if entry.refreshed != self.generation {
                        entry.value = table.shared(id).cloned();
                        entry.refreshed = self.generation;
                    }
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
