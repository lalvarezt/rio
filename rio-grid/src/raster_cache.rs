use super::RawGlyph;
use rustc_hash::FxHashMap;
use std::collections::VecDeque;

const BYTE_LIMIT: usize = 4 * 1024 * 1024;
const ENTRY_LIMIT: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct RasterKey {
    pub font_id: u32,
    pub glyph_id: u16,
    pub pixels: u16,
    pub emoji: bool,
    pub bold: bool,
    pub italic: bool,
}

/// Raw pixels can be shared between panes; atlas slots and bearings cannot.
/// FIFO eviction bounds bitmap storage and the number of metadata entries.
pub(super) struct RasterCache {
    entries: FxHashMap<RasterKey, RawGlyph>,
    order: VecDeque<RasterKey>,
    bytes: usize,
    limit: usize,
}

impl Default for RasterCache {
    fn default() -> Self {
        Self::new(BYTE_LIMIT)
    }
}

impl RasterCache {
    pub(super) fn new(limit: usize) -> Self {
        Self {
            entries: FxHashMap::default(),
            order: VecDeque::new(),
            bytes: 0,
            limit,
        }
    }

    pub(super) fn get(&self, key: &RasterKey) -> Option<&RawGlyph> {
        self.entries.get(key)
    }

    pub(super) fn insert(&mut self, key: RasterKey, glyph: &RawGlyph) {
        let bytes = glyph.bytes.len();
        if self.limit == 0 || bytes > self.limit || self.entries.contains_key(&key) {
            return;
        }
        while self.bytes + bytes > self.limit || self.entries.len() >= ENTRY_LIMIT {
            let Some(oldest) = self.order.pop_front() else {
                return;
            };
            if let Some(old) = self.entries.remove(&oldest) {
                self.bytes -= old.bytes.len();
            }
        }
        self.entries.insert(key, glyph.clone());
        self.order.push_back(key);
        self.bytes += bytes;
    }

    pub(super) fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.bytes = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(glyph_id: u16) -> RasterKey {
        RasterKey {
            font_id: 0,
            glyph_id,
            pixels: 16,
            emoji: false,
            bold: false,
            italic: false,
        }
    }

    fn glyph(bytes: usize) -> RawGlyph {
        RawGlyph {
            width: bytes as u32,
            height: 1,
            left: 2,
            top: 3,
            is_color: false,
            bytes: vec![127; bytes],
        }
    }

    #[test]
    fn evicts_by_byte_budget_and_skips_oversized_glyphs() {
        let mut cache = RasterCache::new(8);
        cache.insert(key(1), &glyph(4));
        cache.insert(key(2), &glyph(4));
        cache.insert(key(3), &glyph(4));
        assert!(cache.get(&key(1)).is_none());
        assert!(cache.get(&key(2)).is_some());
        assert_eq!(cache.get(&key(3)).unwrap().bytes, [127; 4]);
        assert_eq!(cache.bytes, 8);
        cache.insert(key(4), &glyph(9));
        assert!(cache.get(&key(4)).is_none());
        assert!(cache.get(&key(2)).is_some());
        cache.clear();
        assert_eq!(cache.bytes, 0);
        assert!(cache.entries.is_empty());
    }

    #[test]
    fn bounds_metadata_even_for_empty_bitmaps() {
        let mut cache = RasterCache::default();
        for id in 0..=ENTRY_LIMIT as u16 {
            cache.insert(key(id), &glyph(0));
        }
        assert_eq!(cache.entries.len(), ENTRY_LIMIT);
        assert_eq!(cache.order.len(), ENTRY_LIMIT);
        assert!(cache.get(&key(0)).is_none());
    }
}
