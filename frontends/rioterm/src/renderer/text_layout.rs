/// Wrap at word boundaries using the same font metrics as rendering. Long paths
/// prefer separator boundaries; other unbroken tokens split when necessary.
pub(super) fn wrap_text(
    text: &str,
    width: f32,
    mut measure: impl FnMut(&str) -> f32,
) -> Vec<String> {
    let mut result = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            let candidate = if line.is_empty() {
                word.to_owned()
            } else {
                format!("{line} {word}")
            };
            if measure(&candidate) <= width {
                line = candidate;
                continue;
            }
            if !line.is_empty() {
                result.push(std::mem::take(&mut line));
            }
            for c in word.chars().filter(|c| !c.is_control()) {
                let mut candidate = line.clone();
                candidate.push(c);
                if !line.is_empty() && measure(&candidate) > width {
                    if let Some(split) = line.rfind(['/', '\\']).map(|i| i + 1) {
                        let tail = line.split_off(split);
                        result.push(std::mem::replace(&mut line, tail));
                    }
                    let mut candidate = line.clone();
                    candidate.push(c);
                    if !line.is_empty() && measure(&candidate) > width {
                        result.push(std::mem::take(&mut line));
                    }
                }
                line.push(c);
            }
        }
        result.push(line);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn measure(text: &str) -> f32 {
        text.chars().map(|c| if c == 'W' { 3.0 } else { 1.0 }).sum()
    }
    #[test]
    fn wraps_using_glyph_width_and_preserves_words() {
        assert_eq!(
            wrap_text("iiii iii WWW", 8.0, measure),
            ["iiii iii", "WW", "W"]
        );
        assert_eq!(
            wrap_text("one two three", 7.0, measure),
            ["one two", "three"]
        );
    }
    #[test]
    fn wraps_paths_at_separators_before_splitting_filenames() {
        for (path, expected) in [
            (
                r"C:\Users\alice\themes\missing-theme.toml",
                vec![r"C:\Users\alice\themes\", "missing-theme.toml"],
            ),
            (
                "/home/alice/themes/missing.toml",
                vec!["/home/alice/themes/", "missing.toml"],
            ),
        ] {
            let width = if path.starts_with('C') { 28.0 } else { 22.0 };
            assert_eq!(wrap_text(path, width, measure), expected);
        }
    }

    #[test]
    fn long_unicode_paths_and_empty_paragraphs() {
        assert_eq!(
            wrap_text("ééééé\n\nnext", 3.0, measure),
            ["ééé", "éé", "", "nex", "t"]
        );
    }
}
