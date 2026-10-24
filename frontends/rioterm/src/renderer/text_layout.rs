/// Wrap at word boundaries using the same font metrics as rendering. Long paths
/// and other unbroken tokens split only when they exceed the available width.
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
                    result.push(std::mem::take(&mut line));
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
    fn long_unicode_paths_and_empty_paragraphs() {
        assert_eq!(
            wrap_text("ééééé\n\nnext", 3.0, measure),
            ["ééé", "éé", "", "nex", "t"]
        );
    }
}
