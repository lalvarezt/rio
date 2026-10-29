//! Grapheme cluster segmentation (UAX #29, extended grapheme clusters).
//!
//! The break decision is a pairwise state machine, the shape a terminal
//! needs: `is_break(prev, next, &mut state)` answers whether a cluster
//! boundary falls between two adjacent codepoints while the state
//! carries the three pieces of history single-pair rules can't see —
//! emoji ZWJ sequences (GB11), regional-indicator pair parity
//! (GB12/13), and Indic conjunct linkage (GB9c). Grid code appending
//! codepoints cell by cell can run it without ever materializing a
//! string; [`Graphemes`]/[`GraphemeIndices`] wrap the same machine for
//! string callers.
//!
//! Property data is generated from the same UCD release as the width
//! tables (`scripts/grapheme.py`), so segmentation and width can never
//! disagree about what Unicode says.

use crate::grapheme_tables::GRAPHEME_CLASS_RANGES;
pub use crate::grapheme_tables::GRAPHEME_UNICODE_VERSION;

/// Grapheme_Cluster_Break class, refined with the Extended_Pictographic
/// and InCB properties so every rule can run on classes alone. The
/// undocumented variants are the plain UAX #29 GCB classes.
#[allow(missing_docs)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum GraphemeClass {
    Other,
    CR,
    LF,
    Control,
    /// GCB=Extend without InCB=Extend.
    Extend,
    /// GCB=Extend with InCB=Extend: continues an Indic conjunct chain.
    ExtendIncb,
    /// GCB=Extend with InCB=Linker (viramas).
    Linker,
    Zwj,
    RegionalIndicator,
    Prepend,
    SpacingMark,
    L,
    V,
    T,
    LV,
    LVT,
    /// Extended_Pictographic (GCB=Other).
    ExtPic,
    /// InCB=Consonant (GCB=Other).
    Consonant,
}

impl GraphemeClass {
    /// Number of classes; `as u8` values are contiguous in
    /// `0..COUNT`, so classes can index precomputed tables.
    pub const COUNT: usize = 18;

    /// Inverse of `as u8`, for table builders enumerating classes.
    pub const fn from_u8(value: u8) -> Option<Self> {
        use GraphemeClass as G;
        Some(match value {
            0 => G::Other,
            1 => G::CR,
            2 => G::LF,
            3 => G::Control,
            4 => G::Extend,
            5 => G::ExtendIncb,
            6 => G::Linker,
            7 => G::Zwj,
            8 => G::RegionalIndicator,
            9 => G::Prepend,
            10 => G::SpacingMark,
            11 => G::L,
            12 => G::V,
            13 => G::T,
            14 => G::LV,
            15 => G::LVT,
            16 => G::ExtPic,
            17 => G::Consonant,
            _ => return None,
        })
    }
}

// Most Unicode pages contain only Other. Share one empty block for those
// pages and materialize only pages intersecting a generated property range.
// Everything is computed at compile time from the conformance-tested data.
static CLASS_PAGES: [(u16, u16); 0x1100] = {
    assert!(GRAPHEME_CLASS_RANGES.len() <= u16::MAX as usize);
    let mut pages = [(0, 0); 0x1100];
    let mut page = 0;
    let mut start = 0;
    while page < pages.len() {
        let lo = (page as u32) << 8;
        while start < GRAPHEME_CLASS_RANGES.len() && GRAPHEME_CLASS_RANGES[start].1 < lo {
            start += 1;
        }
        let mut end = start;
        while end < GRAPHEME_CLASS_RANGES.len()
            && GRAPHEME_CLASS_RANGES[end].0 <= lo + 255
        {
            end += 1;
        }
        pages[page] = (start as u16, end as u16);
        page += 1;
    }
    pages
};

const CLASS_BLOCK_COUNT: usize = {
    let mut count = 1; // Block zero is the shared Other page.
    let mut page = 0;
    while page < CLASS_PAGES.len() {
        if CLASS_PAGES[page].0 != CLASS_PAGES[page].1 {
            count += 1;
        }
        page += 1;
    }
    count
};

struct ClassTable {
    pages: [u16; 0x1100],
    blocks: [[GraphemeClass; 256]; CLASS_BLOCK_COUNT],
}

static CLASS_TABLE: ClassTable = {
    let mut table = ClassTable {
        pages: [0; 0x1100],
        blocks: [[GraphemeClass::Other; 256]; CLASS_BLOCK_COUNT],
    };
    let mut block = 0;
    let mut page = 0;
    while page < CLASS_PAGES.len() {
        let (start, end) = CLASS_PAGES[page];
        if start != end {
            block += 1;
            table.pages[page] = block as u16;
            let base = (page as u32) << 8;
            let mut range = start as usize;
            while range < end as usize {
                let (lo, hi, class) = GRAPHEME_CLASS_RANGES[range];
                let mut cp = if lo > base { lo } else { base };
                let limit = if hi < base + 255 { hi } else { base + 255 };
                while cp <= limit {
                    table.blocks[block][(cp - base) as usize] = class;
                    cp += 1;
                }
                range += 1;
            }
        }
        page += 1;
    }
    table
};

// One load classifies ASCII controls and Latin-1 alike. Keep this derived
// from the same ranges as the paged table, rather than branching per class
// or making a dependent page lookup for these common codepoints.
static LATIN1_CLASSES: [GraphemeClass; 256] = {
    let mut classes = [GraphemeClass::Other; 256];
    let mut range = 0;
    while range < GRAPHEME_CLASS_RANGES.len() {
        let (lo, hi, class) = GRAPHEME_CLASS_RANGES[range];
        if lo > 255 {
            break;
        }
        let mut cp = lo;
        while cp <= hi && cp <= 255 {
            classes[cp as usize] = class;
            cp += 1;
        }
        range += 1;
    }
    classes
};

/// Class lookup through two compact tables covering every Unicode plane.
#[inline]
pub fn grapheme_class(c: char) -> GraphemeClass {
    let cp = c as u32;
    if cp < 256 {
        return LATIN1_CLASSES[cp as usize];
    }
    let block = CLASS_TABLE.pages[(cp >> 8) as usize] as usize;
    CLASS_TABLE.blocks[block][(cp & 255) as usize]
}

/// Emoji ZWJ sequence progress (GB11): `\p{ExtPic} Extend* ZWJ × \p{ExtPic}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum EmojiSeq {
    #[default]
    None,
    /// Saw Extended_Pictographic, possibly followed by Extends.
    Emoji,
    /// ...then a ZWJ: the very next ExtPic joins.
    EmojiZwj,
}

/// Indic conjunct progress (GB9c):
/// `Consonant [Extend Linker]* Linker [Extend Linker]* × Consonant`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Incb {
    #[default]
    None,
    /// Saw a consonant; no linker in the chain yet.
    Consonant,
    /// Consonant then a chain containing at least one linker: the next
    /// consonant joins.
    LinkerSeen,
}

/// Cross-pair segmentation state. Describes the codepoint run ending at
/// the `prev` side of the next [`is_break`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BreakState {
    emoji: EmojiSeq,
    incb: Incb,
    /// The regional-indicator run ending at `prev` has odd length.
    ri_odd: bool,
}

impl BreakState {
    /// Number of distinct states; `pack` values are contiguous in
    /// `0..COUNT`, so states can index precomputed transition tables.
    pub const COUNT: usize = 18;

    /// Pack into `0..COUNT` for table indexing.
    pub const fn pack(self) -> u8 {
        let emoji = match self.emoji {
            EmojiSeq::None => 0u8,
            EmojiSeq::Emoji => 1,
            EmojiSeq::EmojiZwj => 2,
        };
        let incb = match self.incb {
            Incb::None => 0u8,
            Incb::Consonant => 1,
            Incb::LinkerSeen => 2,
        };
        emoji * 6 + incb * 2 + self.ri_odd as u8
    }

    /// Inverse of [`pack`](Self::pack).
    pub const fn unpack(value: u8) -> Option<Self> {
        if value as usize >= Self::COUNT {
            return None;
        }
        Some(Self {
            emoji: match value / 6 {
                0 => EmojiSeq::None,
                1 => EmojiSeq::Emoji,
                _ => EmojiSeq::EmojiZwj,
            },
            incb: match (value % 6) / 2 {
                0 => Incb::None,
                1 => Incb::Consonant,
                _ => Incb::LinkerSeen,
            },
            ri_odd: value % 2 == 1,
        })
    }

    /// State after the first codepoint of a sequence.
    #[inline]
    pub fn start(first: GraphemeClass) -> Self {
        let mut state = Self::default();
        state.advance(first);
        state
    }

    #[inline]
    const fn advance(&mut self, class: GraphemeClass) {
        use GraphemeClass as G;
        self.emoji = match (self.emoji, class) {
            (_, G::ExtPic) => EmojiSeq::Emoji,
            (EmojiSeq::Emoji, G::Extend | G::ExtendIncb | G::Linker) => EmojiSeq::Emoji,
            (EmojiSeq::Emoji, G::Zwj) => EmojiSeq::EmojiZwj,
            _ => EmojiSeq::None,
        };
        self.incb = match (self.incb, class) {
            (_, G::Consonant) => Incb::Consonant,
            (Incb::Consonant, G::Linker) => Incb::LinkerSeen,
            (Incb::Consonant, G::ExtendIncb | G::Zwj) => Incb::Consonant,
            (Incb::LinkerSeen, G::Linker | G::ExtendIncb | G::Zwj) => Incb::LinkerSeen,
            _ => Incb::None,
        };
        self.ri_odd = match class {
            G::RegionalIndicator => !self.ri_odd,
            _ => false,
        };
    }
}

/// Whether an extended-grapheme-cluster boundary falls between `prev`
/// and `next`. `state` must describe the sequence ending at `prev`
/// (see [`BreakState::start`]); it is advanced past `next` before
/// returning, ready for the following pair.
// One branch per numbered UAX #29 rule, in spec order — merging the
// identical-bodied arms would make the machine harder to audit against
// the standard for zero behavioral gain.
#[allow(clippy::if_same_then_else)]
#[inline]
pub const fn is_break(
    prev: GraphemeClass,
    next: GraphemeClass,
    state: &mut BreakState,
) -> bool {
    use GraphemeClass as G;
    let decision = {
        // GB3: CR × LF.
        if matches!(prev, G::CR) && matches!(next, G::LF) {
            false
        // GB4 / GB5: controls break on both sides.
        } else if matches!(prev, G::Control | G::CR | G::LF)
            || matches!(next, G::Control | G::CR | G::LF)
        {
            true
        // GB6-8: Hangul jamo.
        } else if matches!(prev, G::L) && matches!(next, G::L | G::V | G::LV | G::LVT) {
            false
        } else if matches!(prev, G::LV | G::V) && matches!(next, G::V | G::T) {
            false
        } else if matches!(prev, G::LVT | G::T) && matches!(next, G::T) {
            false
        // GB9 / GB9a / GB9b: extenders, spacing marks, prepends.
        } else if matches!(next, G::Extend | G::ExtendIncb | G::Linker | G::Zwj) {
            false
        } else if matches!(next, G::SpacingMark) {
            false
        } else if matches!(prev, G::Prepend) {
            false
        // GB9c: Indic conjuncts join across a linker chain.
        } else if matches!(next, G::Consonant) && matches!(state.incb, Incb::LinkerSeen) {
            false
        // GB11: emoji ZWJ sequences.
        } else if matches!(prev, G::Zwj)
            && matches!(next, G::ExtPic)
            && matches!(state.emoji, EmojiSeq::EmojiZwj)
        {
            false
        // GB12/GB13: regional indicators pair up.
        } else if matches!(prev, G::RegionalIndicator)
            && matches!(next, G::RegionalIndicator)
            && state.ri_odd
        {
            false
        // GB999.
        } else {
            true
        }
    };
    state.advance(next);
    decision
}

/// Iterator over the extended grapheme clusters of a string.
#[derive(Debug, Clone)]
pub struct Graphemes<'a> {
    text: &'a str,
    offset: usize,
}

impl<'a> Graphemes<'a> {
    /// Segment `text` from its start.
    pub fn new(text: &'a str) -> Self {
        Self { text, offset: 0 }
    }
}

impl<'a> Iterator for Graphemes<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        let rest = &self.text[self.offset..];
        let mut chars = rest.char_indices();
        let (_, first) = chars.next()?;
        let mut prev = grapheme_class(first);
        let mut state = BreakState::start(prev);
        let end = loop {
            match chars.next() {
                Some((i, c)) => {
                    let class = grapheme_class(c);
                    if is_break(prev, class, &mut state) {
                        break i;
                    }
                    prev = class;
                }
                None => break rest.len(),
            }
        };
        let start = self.offset;
        self.offset += end;
        Some(&self.text[start..start + end])
    }
}

/// Like [`Graphemes`], yielding `(byte_offset, cluster)` pairs.
#[derive(Debug, Clone)]
pub struct GraphemeIndices<'a> {
    inner: Graphemes<'a>,
}

impl<'a> GraphemeIndices<'a> {
    /// Segment `text` from its start.
    pub fn new(text: &'a str) -> Self {
        Self {
            inner: Graphemes::new(text),
        }
    }
}

impl<'a> Iterator for GraphemeIndices<'a> {
    type Item = (usize, &'a str);

    fn next(&mut self) -> Option<(usize, &'a str)> {
        let offset = self.inner.offset;
        self.inner.next().map(|g| (offset, g))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    fn clusters(s: &str) -> Vec<&str> {
        Graphemes::new(s).collect()
    }

    #[test]
    fn page_index_matches_property_ranges_for_every_scalar() {
        let mut range = 0;
        for cp in 0..=0x10ffff {
            let Some(c) = char::from_u32(cp) else {
                continue;
            };
            while range < GRAPHEME_CLASS_RANGES.len()
                && GRAPHEME_CLASS_RANGES[range].1 < cp
            {
                range += 1;
            }
            let expected = GRAPHEME_CLASS_RANGES
                .get(range)
                .filter(|&&(lo, _, _)| lo <= cp)
                .map_or(GraphemeClass::Other, |&(_, _, class)| class);
            assert_eq!(grapheme_class(c), expected, "U+{cp:04X}");
        }
    }

    #[test]
    fn versions_stay_in_lockstep() {
        // The whole point of this crate: one Unicode version.
        assert_eq!(GRAPHEME_UNICODE_VERSION, crate::UNICODE_VERSION);
    }

    #[test]
    fn ascii_and_combining() {
        assert_eq!(clusters("abc"), ["a", "b", "c"]);
        assert_eq!(clusters("e\u{301}x"), ["e\u{301}", "x"]);
    }

    #[test]
    fn zwj_emoji_is_one_cluster() {
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        assert_eq!(clusters(family), [family]);
    }

    #[test]
    fn regional_indicators_pair() {
        // Two flags: BR + AR = four RIs, two clusters.
        let flags = "\u{1F1E7}\u{1F1F7}\u{1F1E6}\u{1F1F7}";
        assert_eq!(
            clusters(flags),
            ["\u{1F1E7}\u{1F1F7}", "\u{1F1E6}\u{1F1F7}"]
        );
    }

    #[test]
    fn crlf_and_controls() {
        assert_eq!(clusters("a\r\nb"), ["a", "\r\n", "b"]);
    }

    #[test]
    fn devanagari_conjunct_joins() {
        // ka + virama + ssa: GB9c keeps the conjunct together.
        assert_eq!(clusters("\u{915}\u{94D}\u{937}"), ["\u{915}\u{94D}\u{937}"]);
    }

    #[test]
    fn indices_report_byte_offsets() {
        let pairs: Vec<(usize, &str)> = GraphemeIndices::new("aé日").collect();
        assert_eq!(pairs, [(0, "a"), (1, "é"), (3, "日")]);
    }
}
