use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

/// The only editable text is the pending IME composition. Terminal output and
/// the shell's edit buffer are not an editable document owned by this client.
#[derive(Debug, Default)]
pub(super) struct Composition {
    pub text: String,
    pub selected: Range<usize>,
}

impl Composition {
    pub fn clear(&mut self) {
        self.text.clear();
        self.selected = 0..0;
    }

    pub fn len(&self) -> usize {
        self.text.encode_utf16().count()
    }

    pub fn range(&self, range: Range<usize>) -> Option<(Range<usize>, Range<usize>)> {
        if range.start > range.end || range.end > self.len() {
            return None;
        }
        let start = boundary(&self.text, range.start, false);
        let end = boundary(&self.text, range.end, true);
        let actual =
            self.text[..start].encode_utf16().count()..self.text[..end].encode_utf16().count();
        Some((start..end, actual))
    }

    pub fn replace(&mut self, range: Option<Range<usize>>, text: &str) -> Option<usize> {
        let range = range.unwrap_or(0..self.len());
        let (bytes, actual) = self.range(range)?;
        if self.text.len() - bytes.len() + text.len() > 4096 {
            return None;
        }
        self.text.replace_range(bytes, text);
        Some(actual.start)
    }

    pub fn mark(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selection: Option<Range<usize>>,
    ) -> bool {
        let Some(start) = self.replace(range, text) else {
            return false;
        };
        let inserted_len = text.encode_utf16().count();
        let selection = selection.unwrap_or(inserted_len..inserted_len);
        let selection = selection.start.min(inserted_len)..selection.end.min(inserted_len);
        let Some((_, actual)) = self.range(start + selection.start..start + selection.end) else {
            self.selected = start..start;
            return false;
        };
        self.selected = actual;
        true
    }
}

// Expanding to complete graphemes keeps UTF-16 surrogate halves, combining
// sequences, and ZWJ families intact. Return the adjusted range to the platform.
fn boundary(text: &str, offset: usize, round_up: bool) -> usize {
    let mut utf16 = 0;
    for (byte, grapheme) in text.grapheme_indices(true) {
        if utf16 == offset {
            return byte;
        }
        utf16 += grapheme.encode_utf16().count();
        if utf16 > offset {
            return if round_up {
                byte + grapheme.len()
            } else {
                byte
            };
        }
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_queries_adjust_to_whole_graphemes() {
        let mut value = Composition::default();
        assert!(value.mark(None, "a👩‍👩‍👧‍👦e\u{301}漢", None));
        let (bytes, actual) = value.range(2..3).expect("range");
        assert_eq!(&value.text[bytes], "👩‍👩‍👧‍👦");
        assert_eq!(actual, 1..12);
        let (bytes, actual) = value.range(13..14).expect("range");
        assert_eq!(&value.text[bytes], "e\u{301}");
        assert_eq!(actual, 12..14);
        assert!(value.range(0..99).is_none());
        assert!(value.range(Range { start: 2, end: 1 }).is_none());
    }

    #[test]
    fn replacement_is_composition_local_and_never_splits_a_surrogate() {
        let mut value = Composition::default();
        assert!(value.mark(None, "a😀z", Some(2..2)));
        assert_eq!(value.selected, 1..3);
        assert_eq!(value.replace(Some(2..3), "漢"), Some(1));
        assert_eq!(value.text, "a漢z");
        value.clear();
        assert_eq!(value.selected, 0..0);
        assert!(value.replace(Some(1..2), "x").is_none());
    }
}
