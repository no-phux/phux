//! Command-click links: the cell's OSC 8 hyperlink, else a URL written in the
//! row's text. Only schemes a person expects a click to open are returned, so
//! program output cannot turn a click into an arbitrary URL-scheme launch.

use phux_client_runtime::publication::GridFrame;

const SCHEMES: &[&str] = &[
    "https://", "http://", "file://", "mailto:", "ssh://", "ftp://",
];
const STOP: &[char] = &[
    ' ', '\t', '"', '\'', '`', '<', '>', '{', '}', '|', '\\', '^',
];
const TRAILING: &[char] = &['.', ',', ';', ':', '!', '?', ')', ']', '\'', '"'];

/// The link under `row`/`col`, if any.
pub fn link_in(frame: &GridFrame, row: u16, col: u16) -> Option<String> {
    if let Some(uri) = hyperlink(frame, row, col) {
        return allowed(&uri).then_some(uri);
    }
    let (text, starts) = row_text(frame, row);
    url_around(&text, *starts.get(usize::from(col))?)
}

fn hyperlink(frame: &GridFrame, row: u16, col: u16) -> Option<String> {
    let cell = frame.cell(row, col)?;
    if cell.hyperlink_len == 0 {
        return None;
    }
    let start = cell.hyperlink_offset as usize;
    let end = start.checked_add(cell.hyperlink_len as usize)?;
    Some(String::from_utf8_lossy(frame.buffer.utf8.get(start..end)?).into_owned())
}

/// The row as text plus each column's byte offset into it. Empty cells (and
/// wide-character spacers) read as spaces so offsets stay monotonic.
fn row_text(frame: &GridFrame, row: u16) -> (String, Vec<usize>) {
    let mut text = String::new();
    let mut starts = Vec::with_capacity(usize::from(frame.cols));
    for col in 0..frame.cols {
        starts.push(text.len());
        let cell = frame.cell_text(row, col);
        if cell.is_empty() {
            text.push(' ');
        } else {
            text.push_str(&String::from_utf8_lossy(cell));
        }
    }
    (text, starts)
}

/// The URL whose span contains byte offset `at`.
pub fn url_around(text: &str, at: usize) -> Option<String> {
    for scheme in SCHEMES {
        for (start, _) in text.match_indices(scheme) {
            if start > at {
                break;
            }
            let rest = &text[start..];
            let len = rest.find(STOP).unwrap_or(rest.len());
            let url = tidy(&rest[..len]);
            if at < start + url.len() && url.len() > scheme.len() {
                return Some(url.to_owned());
            }
        }
    }
    None
}

/// Drop trailing prose punctuation, keeping a `)` that closes one inside the
/// URL (Wikipedia style).
fn tidy(mut url: &str) -> &str {
    loop {
        url = url.trim_end_matches(|c: char| c != ')' && TRAILING.contains(&c));
        let unbalanced = url.matches(')').count() > url.matches('(').count();
        match url.strip_suffix(')') {
            Some(shorter) if unbalanced => url = shorter,
            _ => return url,
        }
    }
}

fn allowed(uri: &str) -> bool {
    SCHEMES.iter().any(|scheme| uri.starts_with(scheme))
}

#[cfg(test)]
mod tests {
    use super::{allowed, url_around};

    #[test]
    fn finds_the_url_under_the_offset_and_drops_trailing_punctuation() {
        let text = "see https://phux.dev/docs, then (http://a.b/c) done";
        assert_eq!(
            url_around(text, 10).as_deref(),
            Some("https://phux.dev/docs")
        );
        assert_eq!(url_around(text, 36).as_deref(), Some("http://a.b/c"));
        assert_eq!(url_around(text, 2), None);
        assert_eq!(url_around(text, text.len() - 1), None);
    }

    #[test]
    fn keeps_balanced_parentheses_and_rejects_bare_schemes() {
        let text = "https://en.wikipedia.org/wiki/Rust_(language) x";
        assert_eq!(
            url_around(text, 5).as_deref(),
            Some("https://en.wikipedia.org/wiki/Rust_(language)")
        );
        assert_eq!(url_around("https:// nothing", 3), None);
    }

    #[test]
    fn osc8_targets_are_limited_to_expected_schemes() {
        assert!(allowed("https://example.com"));
        assert!(allowed("file:///tmp/a"));
        assert!(!allowed("x-apple-systempreferences:com.apple"));
        assert!(!allowed("javascript:alert(1)"));
    }
}
