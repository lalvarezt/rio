//! Windows font fallback through DirectWrite's character mapping.

use std::borrow::Cow;
use std::path::PathBuf;

use dwrote::{
    FontCollection, FontFallback, FontStretch, FontStyle, FontWeight, TextAnalysisSource,
    TextAnalysisSourceMethods,
};

// DirectWrite objects stay on the thread that uses them. Mapping a character
// avoids enumerating and opening every installed font on the rendering thread.
thread_local! {
    static FALLBACK: (FontCollection, Option<FontFallback>) = (
        FontCollection::system(),
        FontFallback::get_system_fallback(),
    );
}

struct CharacterSource {
    length: u32,
}

impl TextAnalysisSourceMethods for CharacterSource {
    fn get_locale_name(&self, position: u32) -> (Cow<'_, str>, u32) {
        (Cow::Borrowed("en-us"), self.length.saturating_sub(position))
    }

    fn get_paragraph_reading_direction(&self) -> u32 {
        // DWRITE_READING_DIRECTION_LEFT_TO_RIGHT.
        0
    }
}

/// Find an installed font for one Unicode scalar, including supplementary
/// characters represented by a UTF-16 surrogate pair.
pub fn discover_fallback(
    _primary_family: &str,
    ch: char,
    _want_mono: bool,
    want_bold: bool,
    want_italic: bool,
) -> Option<(PathBuf, u32)> {
    let mut buffer = [0; 2];
    let text = ch.encode_utf16(&mut buffer);
    let length = text.len() as u32;
    let source = TextAnalysisSource::from_text(
        Box::new(CharacterSource { length }),
        Cow::Borrowed(text),
    );

    FALLBACK.with(|(collection, fallback)| {
        let mapped = fallback.as_ref()?.map_characters(
            &source,
            0,
            length,
            collection,
            // The configured faces have already been checked by the caller.
            None,
            if want_bold {
                FontWeight::Bold
            } else {
                FontWeight::Regular
            },
            if want_italic {
                FontStyle::Italic
            } else {
                FontStyle::Normal
            },
            FontStretch::Normal,
        );
        if mapped.mapped_length < text.len() {
            return None;
        }
        let face = mapped.mapped_font?.create_font_face();
        let path = face
            .files()
            .ok()?
            .into_iter()
            .find_map(|file| file.font_file_path().ok())?;
        Some((path, face.get_index()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapped_faces_cover_bmp_and_supplementary_characters() {
        for (ch, bold, italic) in
            [('─', false, false), ('Ω', true, true), ('😀', false, false)]
        {
            let (path, index) =
                discover_fallback("Cascadia Mono", ch, true, bold, italic)
                    .expect("Windows provides a font for this character");
            let bytes = std::fs::read(path).unwrap();
            let face = ttf_parser::Face::parse(&bytes, index).unwrap();
            assert!(
                face.glyph_index(ch).is_some(),
                "mapped face must cover {ch}"
            );
        }
    }

    #[test]
    fn unsupported_character_has_no_fallback() {
        assert!(
            discover_fallback("Cascadia Mono", '\u{10ffff}', true, false, false)
                .is_none()
        );
    }
}
