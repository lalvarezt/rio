//! Conservative reuse for the current Swash Latin/LTR shaping contract.
//!
//! Each plan belongs to one font face, size, feature configuration, and variation.
//! `clear_font_caches` clears plans with the rest of the font state. On a whole-run
//! miss, the callback samples each distinct ASCII byte through the same Swash
//! builder as full shaping. Rejected fonts and text keep the full-run path.
//! Run formation still controls cursor, selection, and font breaks.
//!
//! This is a font-specific proof, not an ASCII-to-cmap shortcut. Every retained
//! glyph was shaped with the actual enabled features and variations. All
//! lookups, including inactive ones, are checked so feature variations cannot
//! select an unexamined contextual lookup. Only one-to-one, context-independent
//! GSUB types 1/3 and GPOS type 1 are admitted, including extension wrappers.
//! Ligatures, pair positioning, contextual/reverse substitutions, unknown
//! lookups, and AAT/legacy kerning force full shaping. Fonts with such tables
//! can be optimized later only with a stronger proof, not sampled text tests.
//! In particular, bundled Cascadia/JetBrains/Maple/DejaVu text faces fail this
//! certificate. This implementation makes no fresh-output speedup claim for
//! those fonts. Fonts without those capabilities reuse unchanged suffixes,
//! even on their first occurrence, after each distinct byte has been sampled.
//!
//! Source contracts inspected: Swash 0.2.10 `shape/mod.rs` (`add_str`, `finish`,
//! `shape_with`) and `shape/engine.rs` (`EngineMetadata::from_font`). Printable
//! ASCII has independent parser clusters under the current Latin/LTR builder.
//! Non-ASCII, controls, combining marks and ZWJ always take the full path.
//! Do not use this plan for CoreText: `font/macos.rs::shape_text_utf16` computes
//! advances by subtracting absolute CTLine positions, which depends on the
//! whole line's floating-point rounding and may include cascade font runs.

#[derive(Clone, Copy, Debug)]
pub(super) struct SimpleGlyph {
    pub id: u16,
    pub advance: f32,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct MappedGlyph {
    pub id: u16,
    pub cell: u16,
}

#[derive(Debug)]
pub(super) struct SimpleRun {
    pub glyphs: Vec<MappedGlyph>,
    pub advance: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Rejection {
    MalformedFont,
    ContextualFont,
}

/// Bounded to 95 glyph records for one immutable shaping configuration.
pub(super) struct SimpleRunPlan {
    glyphs: [Option<SimpleGlyph>; 95],
}

impl SimpleRunPlan {
    /// `offset` is the SFNT face directory offset, including for TTC faces.
    /// Table offsets are absolute from the beginning of `data`, as in Swash.
    pub fn new(data: &[u8], offset: u32) -> Result<Self, Rejection> {
        certify(data, offset as usize)?;
        Ok(Self { glyphs: [None; 95] })
    }

    /// Returns `None` without emitting a partial run when input or sampling
    /// cannot meet the proof. Previously sampled independent glyphs may remain
    /// cached after a callback rejection. The callback must not change settings.
    pub fn shape(
        &mut self,
        text: &str,
        starts: &[u32],
        mut shape_one: impl FnMut(u8) -> Option<SimpleGlyph>,
    ) -> Option<SimpleRun> {
        let bytes = text.as_bytes();
        if bytes.is_empty()
            || !bytes.iter().all(|b| (0x20..=0x7e).contains(b))
            || starts.first() != Some(&0)
            || starts.len() > u16::MAX as usize + 1
            || starts.windows(2).any(|p| p[0] > p[1])
            || starts.last().copied()? as usize >= bytes.len()
        {
            return None;
        }
        let mut glyphs = Vec::with_capacity(bytes.len());
        let mut advance = 0.0f32;
        let mut cell = 0usize;
        for (offset, &byte) in bytes.iter().enumerate() {
            let slot = &mut self.glyphs[(byte - 0x20) as usize];
            let glyph = match *slot {
                Some(glyph) => glyph,
                None => {
                    let glyph = shape_one(byte)?;
                    if !glyph.advance.is_finite() {
                        return None;
                    }
                    *slot = Some(glyph);
                    glyph
                }
            };
            // Match source_cell, including duplicate starts and multiple
            // printable scalars assigned to one source cell.
            while cell + 1 < starts.len() && starts[cell + 1] as usize <= offset {
                cell += 1;
            }
            glyphs.push(MappedGlyph {
                id: glyph.id,
                cell: cell as u16,
            });
            // Preserve full-run left-to-right f32 addition order. Adding
            // separately accumulated prefix/suffix advances is not equivalent.
            advance += glyph.advance;
        }
        Some(SimpleRun { glyphs, advance })
    }
}

fn u16_at(data: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_be_bytes(
        data.get(offset..offset.checked_add(2)?)?.try_into().ok()?,
    ))
}

fn u32_at(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        data.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn certify(data: &[u8], offset: usize) -> Result<(), Rejection> {
    use Rejection::{ContextualFont, MalformedFont};
    let signature = u32_at(data, offset).ok_or(MalformedFont)?;
    if ![
        0x0001_0000,
        u32::from_be_bytes(*b"OTTO"),
        u32::from_be_bytes(*b"true"),
    ]
    .contains(&signature)
    {
        return Err(MalformedFont);
    }
    let count = u16_at(data, offset.checked_add(4).ok_or(MalformedFont)?)
        .ok_or(MalformedFont)? as usize;
    let directory = offset.checked_add(12).ok_or(MalformedFont)?;
    let end = directory.checked_add(count * 16).ok_or(MalformedFont)?;
    if end > data.len() {
        return Err(MalformedFont);
    }
    // Reject duplicate or unsorted tags: Swash binary-searches the directory.
    let mut previous = None;
    for entry in (directory..end).step_by(16) {
        let tag = u32_at(data, entry).ok_or(MalformedFont)?;
        if previous.is_some_and(|prev| prev >= tag) {
            return Err(MalformedFont);
        }
        previous = Some(tag);
        let start = u32_at(data, entry + 8).ok_or(MalformedFont)? as usize;
        let len = u32_at(data, entry + 12).ok_or(MalformedFont)? as usize;
        let table = data
            .get(start..start.checked_add(len).ok_or(MalformedFont)?)
            .ok_or(MalformedFont)?;
        match tag.to_be_bytes() {
            // Reject even when a preferred OpenType table would hide these.
            // This keeps the certificate independent of engine selection.
            [b'm', b'o', b'r', b'x']
            | [b'm', b'o', b'r', b't']
            | [b'k', b'e', b'r', b'x']
            | [b'k', b'e', b'r', b'n'] => {
                return Err(ContextualFont);
            }
            [b'G', b'S', b'U', b'B'] => check_layout(table, true)?,
            [b'G', b'P', b'O', b'S'] => check_layout(table, false)?,
            _ => {}
        }
    }
    Ok(())
}

fn check_layout(table: &[u8], substitution: bool) -> Result<(), Rejection> {
    use Rejection::{ContextualFont, MalformedFont};
    let version = u32_at(table, 0).ok_or(MalformedFont)?;
    if version != 0x0001_0000 && version != 0x0001_0001 {
        return Err(MalformedFont);
    }
    if version == 0x0001_0001 && table.len() < 14 {
        return Err(MalformedFont);
    }
    let list = u16_at(table, 8).filter(|&n| n >= 10).ok_or(MalformedFont)? as usize;
    let count = u16_at(table, list).ok_or(MalformedFont)? as usize;
    if list + 2 + count * 2 > table.len() {
        return Err(MalformedFont);
    }
    for i in 0..count {
        let relative = u16_at(table, list + 2 + i * 2).ok_or(MalformedFont)? as usize;
        if relative < 2 + count * 2 {
            return Err(MalformedFont);
        }
        let lookup = list + relative;
        let kind = u16_at(table, lookup).ok_or(MalformedFont)?;
        let flags = u16_at(table, lookup + 2).ok_or(MalformedFont)?;
        let subtables = u16_at(table, lookup + 4).ok_or(MalformedFont)? as usize;
        let header_len = 6 + subtables * 2 + if flags & 0x10 != 0 { 2 } else { 0 };
        if lookup + header_len > table.len() {
            return Err(MalformedFont);
        }
        let independent = |k| k == 1 || (substitution && k == 3);
        let extension = if substitution { 7 } else { 9 };
        if !independent(kind) && kind != extension {
            return Err(ContextualFont);
        }
        for j in 0..subtables {
            let relative =
                u16_at(table, lookup + 6 + j * 2).ok_or(MalformedFont)? as usize;
            if relative < header_len {
                return Err(MalformedFont);
            }
            let subtable = lookup + relative;
            let format = u16_at(table, subtable).ok_or(MalformedFont)?;
            if kind == extension {
                if format != 1 {
                    return Err(MalformedFont);
                }
                let inner = u16_at(table, subtable + 2).ok_or(MalformedFont)?;
                if !independent(inner) {
                    return Err(ContextualFont);
                }
                let relative = u32_at(table, subtable + 4).ok_or(MalformedFont)? as usize;
                let target = subtable.checked_add(relative).ok_or(MalformedFont)?;
                if relative < 8 || u16_at(table, target).is_none() {
                    return Err(MalformedFont);
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn font(tables: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut tables = tables.to_vec();
        tables.sort_by_key(|t| t.0);
        let mut data = vec![0u8; 12 + tables.len() * 16];
        data[..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
        data[4..6].copy_from_slice(&(tables.len() as u16).to_be_bytes());
        for (i, (tag, table)) in tables.iter().enumerate() {
            let entry = 12 + i * 16;
            data[entry..entry + 4].copy_from_slice(tag);
            let start = data.len() as u32;
            data[entry + 8..entry + 12].copy_from_slice(&start.to_be_bytes());
            data[entry + 12..entry + 16]
                .copy_from_slice(&(table.len() as u32).to_be_bytes());
            data.extend(table);
        }
        data
    }

    fn layout(kind: u16, inner: Option<u16>) -> Vec<u8> {
        let words = [1u16, 0, 0, 0, 10, 1, 4, kind, 0, 1, 8, 1];
        let mut data: Vec<u8> = words.iter().flat_map(|n| n.to_be_bytes()).collect();
        if let Some(inner) = inner {
            data.extend(inner.to_be_bytes());
            data.extend(8u32.to_be_bytes());
            data.extend(1u16.to_be_bytes());
        }
        data
    }

    #[test]
    fn certifies_only_independent_lookup_kinds() {
        for kind in 1..=8 {
            let data = font(&[(*b"GSUB", layout(kind, None))]);
            assert_eq!(SimpleRunPlan::new(&data, 0).is_ok(), kind == 1 || kind == 3);
        }
        for kind in 1..=9 {
            let data = font(&[(*b"GPOS", layout(kind, None))]);
            assert_eq!(SimpleRunPlan::new(&data, 0).is_ok(), kind == 1);
        }
        for tag in [*b"kern", *b"kerx", *b"morx", *b"mort"] {
            assert!(SimpleRunPlan::new(&font(&[(tag, vec![])]), 0).is_err());
        }
    }

    #[test]
    fn extensions_cannot_hide_context_or_ligatures() {
        for (tag, wrapper) in [(*b"GSUB", 7), (*b"GPOS", 9)] {
            for inner in 1..=9 {
                let data = font(&[(tag, layout(wrapper, Some(inner)))]);
                let expected = inner == 1 || (tag == *b"GSUB" && inner == 3);
                assert_eq!(SimpleRunPlan::new(&data, 0).is_ok(), expected);
            }
        }
    }

    #[test]
    fn rejects_truncation_and_invalid_face_offsets_without_panicking() {
        let data = font(&[(*b"GSUB", layout(7, Some(1)))]);
        for end in 0..data.len() {
            assert!(SimpleRunPlan::new(&data[..end], 0).is_err());
        }
        assert!(SimpleRunPlan::new(&data, u32::MAX).is_err());
        assert!(SimpleRunPlan::new(b"wOFF", 0).is_err());
    }

    #[test]
    fn fresh_prefix_reuses_suffix_and_preserves_cell_mapping() {
        let mut plan = SimpleRunPlan::new(&font(&[]), 0).unwrap();
        let mut calls = Vec::new();
        let mut sample = |byte| {
            calls.push(byte);
            Some(SimpleGlyph {
                id: byte as u16,
                advance: 0.1,
            })
        };
        let first = plan
            .shape("01 suffix", &[0, 1, 2, 3, 4, 5, 6, 7, 8], &mut sample)
            .unwrap();
        let second = plan
            .shape("02 suffix", &[0, 2, 3, 5, 8], &mut sample)
            .unwrap();
        assert_eq!(calls.iter().filter(|&&b| b == b's').count(), 1);
        assert_eq!(calls.last(), Some(&b'2'));
        assert_eq!(
            second.glyphs.iter().map(|g| g.cell).collect::<Vec<_>>(),
            vec![0, 0, 1, 2, 2, 3, 3, 3, 4]
        );
        assert_eq!(first.advance.to_bits(), second.advance.to_bits());
        let mut expected = 0.0f32;
        for _ in 0..9 {
            expected += 0.1;
        }
        assert_eq!(second.advance.to_bits(), expected.to_bits());
    }

    #[test]
    fn unicode_and_bad_boundaries_never_sample() {
        let mut plan = SimpleRunPlan::new(&font(&[]), 0).unwrap();
        for text in ["a\u{301}", "a\u{200d}b", "é", "a\tb", "a\nb", ""] {
            assert!(plan
                .shape(text, &[0], |_| panic!("must fall back"))
                .is_none());
        }
        for starts in [vec![], vec![1], vec![0, 3], vec![0, 2, 1]] {
            assert!(plan
                .shape("abc", &starts, |_| panic!("must fall back"))
                .is_none());
        }
        assert!(plan.shape("abc", &[0], |_| None).is_none());
    }

    #[test]
    fn failed_sampling_does_not_return_a_partial_run() {
        let mut plan = SimpleRunPlan::new(&font(&[]), 0).unwrap();
        assert!(plan
            .shape("ab", &[0, 1], |b| {
                (b == b'a').then_some(SimpleGlyph {
                    id: 1,
                    advance: 1.0,
                })
            })
            .is_none());
        assert!(plan
            .shape("b", &[0], |_| Some(SimpleGlyph {
                id: 2,
                advance: f32::NAN
            }))
            .is_none());
    }

    #[test]
    fn preserves_sequential_rounding_after_a_changed_prefix() {
        let mut plan = SimpleRunPlan::new(&font(&[]), 0).unwrap();
        let sample = |b| {
            Some(SimpleGlyph {
                id: b as u16,
                advance: if b == b'A' { 16_777_216.0 } else { 1.0 },
            })
        };
        let run = plan.shape("Abb", &[0, 1, 2], sample).unwrap();
        let mut expected = 0.0f32;
        for value in [16_777_216.0, 1.0, 1.0] {
            expected += value;
        }
        assert_eq!(run.advance.to_bits(), expected.to_bits());
        assert_ne!(run.advance.to_bits(), (16_777_216.0f32 + 2.0).to_bits());
        let run = plan
            .shape("bbb", &[0, 0, 2], |_| panic!("already sampled"))
            .unwrap();
        assert_eq!(run.advance, 3.0);
        assert_eq!(
            run.glyphs.iter().map(|g| g.cell).collect::<Vec<_>>(),
            vec![1, 1, 2]
        );
    }

    #[test]
    fn face_directory_offsets_keep_absolute_table_offsets() {
        let original = font(&[(*b"GSUB", layout(1, None))]);
        let mut collection = vec![0u8; 32];
        collection.extend(&original);
        let table_offset = u32_at(&collection, 32 + 12 + 8).unwrap() + 32;
        collection[32 + 12 + 8..32 + 12 + 12]
            .copy_from_slice(&table_offset.to_be_bytes());
        assert!(SimpleRunPlan::new(&collection, 32).is_ok());
        assert!(SimpleRunPlan::new(&collection, 0).is_err());
    }
}
