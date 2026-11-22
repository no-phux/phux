//! Which links a click may open: OSC 8 hyperlinks the program set, and plain
//! URLs found in the row under the pointer. Only `http`, `https`, and
//! `mailto` open; a program cannot make the page run `javascript:` or read
//! `file:` by printing a link.

/// Schemes a link may open, with what must follow them.
const SCHEMES: [&str; 3] = ["https://", "http://", "mailto:"];

/// Characters that end a plain URL.
const URL_STOPS: [char; 7] = ['"', '\'', '<', '>', '`', '\0', '\u{a0}'];

/// Trailing characters that read as sentence punctuation, not URL.
const TRAILING: [char; 7] = ['.', ',', ';', ':', '!', '?', '\''];

/// The link to open for `uri`, trimmed, when its scheme is allowed and it
/// has no whitespace or control characters; `None` otherwise.
#[must_use]
pub fn allowed_link(uri: &str) -> Option<&str> {
    let uri = uri.trim();
    let scheme = SCHEMES
        .iter()
        .find(|scheme| starts_with_ignore_case(uri, scheme))?;
    let rest = &uri[scheme.len()..];
    let clean = !uri.chars().any(|ch| ch.is_whitespace() || ch.is_control());
    (clean && !rest.is_empty()).then_some(uri)
}

/// The plain URL covering cell `col` of a row, if the row has one there and
/// it may open. `row` is one character per cell.
#[must_use]
pub fn url_at(row: &[char], col: usize) -> Option<String> {
    let mut start = 0;
    while start < row.len() {
        let Some(scheme) = scheme_at(row, start) else {
            start += 1;
            continue;
        };
        let mut end = start + scheme.len();
        while end < row.len() && !ends_url(row[end]) {
            end += 1;
        }
        let url = trim_trailing(&row[start..end]);
        if (start..start + url.chars().count()).contains(&col) {
            return allowed_link(&url).map(str::to_owned);
        }
        start = end;
    }
    None
}

/// The scheme starting a URL at `start`: at a word boundary, not glued to
/// the text before it.
fn scheme_at(row: &[char], start: usize) -> Option<&'static str> {
    if start > 0 && row[start - 1].is_alphanumeric() {
        return None;
    }
    SCHEMES.iter().copied().find(|scheme| {
        scheme.len() <= row.len() - start
            && row[start..start + scheme.len()]
                .iter()
                .zip(scheme.chars())
                .all(|(ch, expected)| ch.eq_ignore_ascii_case(&expected))
    })
}

fn ends_url(ch: char) -> bool {
    ch.is_whitespace() || ch.is_control() || URL_STOPS.contains(&ch)
}

/// Drop sentence punctuation and closing brackets the URL did not open.
fn trim_trailing(url: &[char]) -> String {
    let mut end = url.len();
    while end > 0 {
        let last = url[end - 1];
        let unopened = |open: char| {
            let body = &url[..end];
            body.iter().filter(|ch| **ch == open).count()
                < body.iter().filter(|ch| **ch == last).count()
        };
        let strip = TRAILING.contains(&last)
            || (last == ')' && unopened('('))
            || (last == ']' && unopened('['))
            || (last == '}' && unopened('{'));
        if !strip {
            break;
        }
        end -= 1;
    }
    url[..end].iter().collect()
}

fn starts_with_ignore_case(text: &str, prefix: &str) -> bool {
    text.get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}
