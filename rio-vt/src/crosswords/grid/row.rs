// grid/row.rs was originally taken from Alacritty
// https://github.com/alacritty/alacritty/blob/e35e5ad14fce8456afdd89f2b392b9924bb27471/alacritty_terminal/src/grid/row.rs
// which is licensed under Apache 2.0 license.

use crate::crosswords::grid::GridSquare;
use crate::crosswords::Column;
use core::cmp::min;
use std::cmp::max;
use std::ops::{Index, IndexMut, Range, RangeFrom, RangeFull, RangeTo, RangeToInclusive};
use std::{ptr, slice};

/// Row-level semantic prompt mark from OSC 133 shell integration.
/// An index for prompt navigation, following the same model other
/// terminals use: a run of `Prompt`/`PromptContinuation` rows is one
/// prompt; everything else is `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SemanticPrompt {
    #[default]
    None,
    Prompt,
    PromptContinuation,
}

/// A row in the grid.
#[derive(Clone, Debug)]
pub struct Row<T> {
    pub inner: Vec<T>,

    /// Conservative end of the modified prefix. Plain tails contain the last
    /// reset's fill, optionally followed by defaults appended during growth.
    /// Arbitrary appended data carries `has_extras` and uses full snapshots.
    pub(crate) occ: usize,

    /// Set when at least one cell in the row contains a kitty Unicode
    /// graphics-protocol placeholder (U+10EEEE). The renderer skips the
    /// placeholder scan on rows where this is `false`.
    pub kitty_virtual_placeholder: bool,

    /// Conservative "some cell in this row may carry an extras id"
    /// hint. Load-bearing for correctness, not just a fast path: the
    /// extras mark-and-sweep skips rows where this is `false`, so a
    /// write path that stamps an extras id without setting it lets the
    /// sweep free a live slot.
    pub has_extras: bool,

    /// Conservative "some cell in this row may carry a non-default
    /// style id" hint, maintained the same way as `has_extras` and
    /// load-bearing for the style mark-and-sweep's row skip.
    pub has_styles: bool,

    /// OSC 133 semantic prompt mark. Travels with the row into
    /// scrollback; cleared when the row is recycled.
    pub semantic_prompt: SemanticPrompt,

    /// Per-row dirty bit set on every write through `IndexMut` /
    /// `last_mut` / `iter_mut` / `reset` / `append*` / `front_split_off`.
    /// Read + cleared by the renderer's snapshot path so it can copy
    /// only the rows that changed since the last snapshot. Defaults
    /// to `true` on `new()` and `default()` so a freshly-constructed
    /// row triggers its first snapshot.
    pub dirty: bool,
}

impl<T> Default for Row<T> {
    fn default() -> Self {
        Self {
            inner: Vec::new(),
            occ: 0,
            kitty_virtual_placeholder: false,
            has_extras: false,
            has_styles: false,
            semantic_prompt: SemanticPrompt::None,
            dirty: true,
        }
    }
}

impl<T: PartialEq> PartialEq for Row<T> {
    fn eq(&self, other: &Self) -> bool {
        self.inner == other.inner
    }
}

impl<T: Clone + Default> Row<T> {
    /// Create a new terminal row.
    ///
    /// Ideally the `template` should be `Copy` in all performance sensitive scenarios.
    pub fn new(columns: usize) -> Row<T> {
        debug_assert!(columns >= 1);

        let mut inner: Vec<T> = Vec::with_capacity(columns);

        // This is a slightly optimized version of `std::vec::Vec::resize`.
        unsafe {
            let mut ptr = inner.as_mut_ptr();

            for _ in 1..columns {
                ptr::write(ptr, T::default());
                ptr = ptr.offset(1);
            }
            ptr::write(ptr, T::default());

            inner.set_len(columns);
        }

        Row {
            inner,
            occ: 0,
            kitty_virtual_placeholder: false,
            has_extras: false,
            has_styles: false,
            semantic_prompt: SemanticPrompt::None,
            dirty: true,
        }
    }

    /// Copy `src` into `self` in place, reusing the existing `inner` Vec
    /// allocation. Equivalent to `*self = src.clone()` but skips the
    /// per-call `Vec` allocation when `self.inner.capacity() >=
    /// src.inner.len()` (the common case for renderer frame buffers).
    /// Does not touch `self.dirty` — the snapshot path manages that
    /// flag on the source row, not on the destination buffer.
    #[inline]
    pub fn copy_from(&mut self, src: &Self) {
        self.inner.clone_from(&src.inner);
        self.copy_metadata_from(src);
    }

    /// Copy a renderer snapshot, retaining an equal plain fill tail. Managed
    /// rows and changed dimensions or fill values keep the full-copy path.
    #[inline]
    pub(crate) fn copy_snapshot_from(&mut self, src: &Self)
    where
        T: PartialEq,
    {
        let prefix = self.occ.max(src.occ);
        let len = src.inner.len();
        if self.inner.len() == len
            && prefix < len
            && !self.has_styles
            && !self.has_extras
            && !src.has_styles
            && !src.has_extras
            && self.inner[len - 1] == src.inner[len - 1]
            && self.inner[prefix] == self.inner[len - 1]
            && src.inner[prefix] == src.inner[len - 1]
        {
            // Plain rows retain equal fill tails across output frames. Include
            // the old occupied prefix so shorter lines erase previous text.
            // Matching both ends excludes a reset-fill/default transition
            // inside either tail after column growth.
            // Managed/from_vec rows keep the complete-copy path.
            self.inner[..prefix].clone_from_slice(&src.inner[..prefix]);
        } else {
            self.inner.clone_from(&src.inner);
        }
        self.copy_metadata_from(src);
    }

    #[inline]
    fn copy_metadata_from(&mut self, src: &Self) {
        self.occ = src.occ;
        self.kitty_virtual_placeholder = src.kitty_virtual_placeholder;
        self.has_extras = src.has_extras;
        self.has_styles = src.has_styles;
        self.semantic_prompt = src.semantic_prompt;
    }

    /// Reset a recycled row back to a blank `columns`-wide row, reusing the
    /// existing `inner` allocation. Used by the storage recycle pool so a row
    /// freed on shrink can be handed back out on a later grow without
    /// reallocating its cell buffer. Produces the same state as
    /// [`Row::new`].
    #[inline]
    pub fn recycle(&mut self, columns: usize) {
        debug_assert!(columns >= 1);
        self.inner.clear();
        self.inner.resize_with(columns, T::default);
        self.occ = 0;
        self.kitty_virtual_placeholder = false;
        self.has_extras = false;
        self.has_styles = false;
        self.semantic_prompt = SemanticPrompt::None;
        self.dirty = true;
    }

    /// Truncate the row to `columns` in place, dropping trailing cells without
    /// the `split_off` allocation `shrink` does. The caller guarantees no
    /// occupied content lies beyond `columns` (so only blank tail cells are
    /// removed); used by the no-reflow column shrink fast path.
    #[inline]
    pub fn truncate_columns(&mut self, columns: usize) {
        if self.inner.len() > columns {
            self.inner.truncate(columns);
            self.occ = min(self.occ, columns);
            self.dirty = true;
        }
    }

    /// Increase the number of columns in the row.
    #[inline]
    pub fn grow(&mut self, columns: usize) {
        if self.inner.len() >= columns {
            return;
        }

        self.inner.resize_with(columns, T::default);
    }

    pub fn shrink(&mut self, columns: usize) -> Option<Vec<T>>
    where
        T: GridSquare,
    {
        if self.inner.len() <= columns {
            return None;
        }

        // Inspect the tail before allocating it. A mixed grid can need
        // reflow overall while many individual rows only lose blank cells.
        let index = self.inner[columns..]
            .iter()
            .rposition(|c| !c.is_empty())
            .map_or(0, |i| i + 1);

        let new_row = if index == 0 {
            self.inner.truncate(columns);
            None
        } else {
            let mut tail = self.inner.split_off(columns);
            tail.truncate(index);
            Some(tail)
        };

        self.occ = min(self.occ, columns);
        self.dirty = true;

        new_row
    }

    /// Reset the row using the template's background and default text attributes.
    #[inline]
    pub fn reset(&mut self, template: &T, styles: &mut crate::crosswords::style::StyleSet)
    where
        T: GridSquare + PartialEq,
    {
        let mut proto = T::default();
        proto.reset(template, styles);
        self.reset_to(proto);
    }

    /// Reset to a cell whose text attributes and extras are already erased.
    /// Region clears share this value instead of resolving the same template
    /// background through the style table separately for every row.
    pub(super) fn reset_to(&mut self, proto: T)
    where
        T: GridSquare + PartialEq,
    {
        debug_assert!(!self.inner.is_empty());

        // The tail beyond `occ` holds at most two fill values: the last
        // reset's fill, then defaults appended by `grow`. If both tail
        // ends match what this reset writes, only the prefix needs work.
        let len = self.inner.len();
        let mut occ = self.occ;
        if occ < len && (proto != self.inner[occ] || proto != self.inner[len - 1]) {
            occ = len;
        }
        self.has_styles = proto.carries_style();
        self.inner[..occ].fill(proto);
        self.occ = 0;
        self.kitty_virtual_placeholder = false;
        self.has_extras = false;
        self.semantic_prompt = SemanticPrompt::None;
        self.dirty = true;
    }
}

#[allow(clippy::len_without_is_empty)]
impl<T> Row<T> {
    #[inline]
    pub fn from_vec(vec: Vec<T>, occ: usize) -> Row<T>
    where
        T: GridSquare,
    {
        let has_styles = vec.iter().any(GridSquare::carries_style);
        Row {
            inner: vec,
            occ,
            kitty_virtual_placeholder: false,
            has_extras: true,
            has_styles,
            semantic_prompt: SemanticPrompt::None,
            dirty: true,
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    #[inline]
    pub fn last(&self) -> Option<&T> {
        self.inner.last()
    }

    #[inline]
    pub fn last_mut(&mut self) -> Option<&mut T> {
        self.occ = self.inner.len();
        self.dirty = true;
        self.inner.last_mut()
    }

    #[inline]
    pub fn append(&mut self, vec: &mut Vec<T>)
    where
        T: GridSquare,
    {
        self.occ += vec.len();
        self.dirty = true;
        self.has_extras = true;
        // Exact recompute over the moved span, so splices don't leave
        // rows permanently pinned into the style sweep's walk.
        self.has_styles |= vec.iter().any(GridSquare::carries_style);
        self.inner.append(vec);
    }

    /// Move the first `count` cells of `src` onto the end of `self`, in place:
    /// a memcpy of the span followed by a memmove that drops it from `src`, with
    /// no temporary allocation. Replaces `front_split_off` + `append` in the
    /// grow reflow, where allocating a `Vec` per wrapped row dominated.
    #[inline]
    pub fn append_front_of(&mut self, src: &mut Row<T>, count: usize)
    where
        T: Copy + GridSquare,
    {
        self.inner.extend_from_slice(&src.inner[..count]);
        self.occ = self.inner.len();
        self.dirty = true;
        // Reflow moves cells between rows, so their conservative hints must
        // follow them too. A false source hint also avoids scanning plain rows.
        self.has_extras |= src.has_extras;
        self.kitty_virtual_placeholder |= src.kitty_virtual_placeholder;
        if !self.has_styles && src.has_styles {
            self.has_styles = src.inner[..count].iter().any(GridSquare::carries_style);
        }

        src.inner.drain(..count);
        src.occ = src.occ.saturating_sub(count);
        src.dirty = true;
    }

    #[inline]
    pub fn append_front(&mut self, mut vec: Vec<T>)
    where
        T: GridSquare,
    {
        self.occ += vec.len();
        self.dirty = true;
        self.has_extras = true;
        self.has_styles |= vec.iter().any(GridSquare::carries_style);

        vec.append(&mut self.inner);
        self.inner = vec;
    }

    #[inline]
    pub fn front_split_off(&mut self, at: usize) -> Vec<T> {
        self.occ = self.occ.saturating_sub(at);
        self.dirty = true;

        let mut split = self.inner.split_off(at);
        std::mem::swap(&mut split, &mut self.inner);
        split
    }

    #[inline]
    pub fn is_clear(&self) -> bool
    where
        T: GridSquare,
    {
        self.inner.iter().all(GridSquare::is_empty)
    }
}

impl<'a, T> IntoIterator for &'a Row<T> {
    type IntoIter = slice::Iter<'a, T>;
    type Item = &'a T;

    #[inline]
    fn into_iter(self) -> slice::Iter<'a, T> {
        self.inner.iter()
    }
}

impl<'a, T> IntoIterator for &'a mut Row<T> {
    type IntoIter = slice::IterMut<'a, T>;
    type Item = &'a mut T;

    #[inline]
    fn into_iter(self) -> slice::IterMut<'a, T> {
        self.occ = self.len();
        self.dirty = true;
        self.inner.iter_mut()
    }
}

impl<T> Index<Column> for Row<T> {
    type Output = T;

    #[inline]
    fn index(&self, index: Column) -> &T {
        &self.inner[index.0]
    }
}

impl<T> IndexMut<Column> for Row<T> {
    #[inline]
    fn index_mut(&mut self, index: Column) -> &mut T {
        self.occ = max(self.occ, *index + 1);
        self.dirty = true;
        &mut self.inner[index.0]
    }
}

impl<T> Index<Range<Column>> for Row<T> {
    type Output = [T];

    #[inline]
    fn index(&self, index: Range<Column>) -> &[T] {
        &self.inner[(index.start.0)..(index.end.0)]
    }
}

impl<T> IndexMut<Range<Column>> for Row<T> {
    #[inline]
    fn index_mut(&mut self, index: Range<Column>) -> &mut [T] {
        self.occ = max(self.occ, *index.end);
        self.dirty = true;
        &mut self.inner[(index.start.0)..(index.end.0)]
    }
}

impl<T> Index<RangeTo<Column>> for Row<T> {
    type Output = [T];

    #[inline]
    fn index(&self, index: RangeTo<Column>) -> &[T] {
        &self.inner[..(index.end.0)]
    }
}

impl<T> IndexMut<RangeTo<Column>> for Row<T> {
    #[inline]
    fn index_mut(&mut self, index: RangeTo<Column>) -> &mut [T] {
        self.occ = max(self.occ, *index.end);
        self.dirty = true;
        &mut self.inner[..(index.end.0)]
    }
}

impl<T> Index<RangeFrom<Column>> for Row<T> {
    type Output = [T];

    #[inline]
    fn index(&self, index: RangeFrom<Column>) -> &[T] {
        &self.inner[(index.start.0)..]
    }
}

impl<T> IndexMut<RangeFrom<Column>> for Row<T> {
    #[inline]
    fn index_mut(&mut self, index: RangeFrom<Column>) -> &mut [T] {
        self.occ = self.len();
        self.dirty = true;
        &mut self.inner[(index.start.0)..]
    }
}

impl<T> Index<RangeFull> for Row<T> {
    type Output = [T];

    #[inline]
    fn index(&self, _: RangeFull) -> &[T] {
        &self.inner[..]
    }
}

impl<T> IndexMut<RangeFull> for Row<T> {
    #[inline]
    fn index_mut(&mut self, _: RangeFull) -> &mut [T] {
        self.occ = self.len();
        self.dirty = true;
        &mut self.inner[..]
    }
}

impl<T> Index<RangeToInclusive<Column>> for Row<T> {
    type Output = [T];

    #[inline]
    fn index(&self, index: RangeToInclusive<Column>) -> &[T] {
        &self.inner[..=(index.end.0)]
    }
}

impl<T> IndexMut<RangeToInclusive<Column>> for Row<T> {
    #[inline]
    fn index_mut(&mut self, index: RangeToInclusive<Column>) -> &mut [T] {
        self.occ = max(self.occ, *index.end + 1);
        self.dirty = true;
        &mut self.inner[..=(index.end.0)]
    }
}
