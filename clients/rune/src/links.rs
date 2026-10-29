//! Links in terminal output: OSC 8 hyperlinks a program attached to cells,
//! and URLs written as plain text, including ones soft-wrapped across rows.
//!
//! Only schemes a browser or mail client opens are offered, so a program
//! can't turn a ⌘-click into launching an arbitrary handler.

use phux_client_core::grid::CELL_HYPERLINK;
use phux_client_runtime::publication::GridFrame;

/// libghostty's `CellWide::SpacerTail`: the right half of a wide glyph.
const WIDE_SPACER_TAIL: u8 = 2;
/// Rows a wrapped URL may span on either side of the pointer.
const WRAP_ROWS: u16 = 8;
/// What `Tx::open_url` hands to the system.
const SCHEMES: [&str; 3] = ["https://", "http://", "mailto:"];

/// A link under the pointer: its target and the cells it covers, as
/// `(row, first column, end column)` per row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub url: String,
    pub spans: Vec<(u16, u16, u16)>,
}

/// Whether `url` names a scheme this client will hand to the system.
pub fn openable(url: &str) -> bool {
    SCHEMES.iter().any(|scheme| {
        url.len() > scheme.len()
            && url
                .get(..scheme.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(scheme))
    })
}

/// The link at a cell of the frame, if any.
pub fn at(frame: &GridFrame, col: u16, row: u16) -> Option<Link> {
    hyperlink(frame, col, row).or_else(|| written(frame, col, row))
}

fn hyperlink_uri(frame: &GridFrame, row: u16, col: u16) -> Option<&[u8]> {
    let cell = frame.cell(row, col)?;
    if cell.flags & CELL_HYPERLINK == 0 {
        return None;
    }
    let start = cell.hyperlink_offset as usize;
    frame
        .buffer
        .utf8
        .get(start..start + cell.hyperlink_len as usize)
}

/// An OSC 8 link: the run of cells in this row carrying the same URI.
fn hyperlink(frame: &GridFrame, col: u16, row: u16) -> Option<Link> {
    let uri = hyperlink_uri(frame, row, col)?;
    let url = std::str::from_utf8(uri).ok().filter(|u| openable(u))?;
    let same = |c: u16| hyperlink_uri(frame, row, c) == Some(uri);
    let first = (0..col)
        .rev()
        .take_while(|c| same(*c))
        .last()
        .unwrap_or(col);
    let end = (col..frame.cols)
        .take_while(|c| same(*c))
        .last()
        .unwrap_or(col)
        + 1;
    Some(Link {
        url: url.to_owned(),
        spans: vec![(row, first, end)],
    })
}

/// A row whose last cell holds text likely runs on into the next row.
fn runs_on(frame: &GridFrame, row: u16) -> bool {
    let text = frame.cell_text(row, frame.cols.saturating_sub(1));
    !text.is_empty() && text != b" "
}

/// A URL typed into the output, found in the logical line around the cell.
fn written(frame: &GridFrame, col: u16, row: u16) -> Option<Link> {
    let mut top = row;
    while top > 0 && row - top < WRAP_ROWS && runs_on(frame, top - 1) {
        top -= 1;
    }
    let mut bottom = row;
    while bottom + 1 < frame.rows && bottom - row < WRAP_ROWS && runs_on(frame, bottom) {
        bottom += 1;
    }
    let mut cells = Vec::new();
    for r in top..=bottom {
        for c in 0..frame.cols {
            let text = frame.cell_text(r, c);
            if text.is_empty() {
                let spacer = frame
                    .cell(r, c)
                    .is_some_and(|cell| cell.wide == WIDE_SPACER_TAIL);
                if !spacer {
                    cells.push((' ', r, c));
                }
                continue;
            }
            for ch in String::from_utf8_lossy(text).chars() {
                cells.push((ch, r, c));
            }
        }
    }
    find(&cells, row, col)
}

fn url_char(c: char) -> bool {
    !c.is_whitespace()
        && !c.is_control()
        && !matches!(
            c,
            '<' | '>' | '"' | '\'' | '`' | '\\' | '^' | '{' | '}' | '|'
        )
}

/// Drop sentence punctuation and closing brackets the URL didn't open.
fn trim_end(chars: &[char]) -> usize {
    let mut end = chars.len();
    while end > 0 {
        let last = chars[end - 1];
        let open = match last {
            ')' => '(',
            ']' => '[',
            _ if matches!(last, '.' | ',' | ';' | ':' | '!' | '?' | '*') => {
                end -= 1;
                continue;
            }
            _ => break,
        };
        let opened = chars[..end].iter().filter(|c| **c == open).count();
        let closed = chars[..end].iter().filter(|c| **c == last).count();
        if closed <= opened {
            break;
        }
        end -= 1;
    }
    end
}

/// The URL in `cells` (character, row, column) covering `(row, col)`.
pub fn find(cells: &[(char, u16, u16)], row: u16, col: u16) -> Option<Link> {
    let chars: Vec<char> = cells.iter().map(|c| c.0).collect();
    let mut i = 0;
    while i < chars.len() {
        // A row's first cell starts a word even when the row above ends
        // in a letter.
        let boundary = i == 0 || !chars[i - 1].is_alphanumeric() || cells[i - 1].1 != cells[i].1;
        let rest: String = chars[i..].iter().take(8).collect();
        let Some(scheme) = SCHEMES
            .iter()
            .find(|s| boundary && openable_prefix(&rest, s))
        else {
            i += 1;
            continue;
        };
        let run = chars[i..].iter().take_while(|c| url_char(**c)).count();
        let len = trim_end(&chars[i..i + run]);
        let span = &cells[i..i + len];
        if len > scheme.len() && span.iter().any(|c| c.1 == row && c.2 == col) {
            return Some(Link {
                url: chars[i..i + len].iter().collect(),
                spans: spans(span),
            });
        }
        i += run.max(1);
    }
    None
}

fn openable_prefix(text: &str, scheme: &str) -> bool {
    text.get(..scheme.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(scheme))
}

fn spans(cells: &[(char, u16, u16)]) -> Vec<(u16, u16, u16)> {
    let mut out: Vec<(u16, u16, u16)> = Vec::new();
    for &(_, row, col) in cells {
        match out.last_mut() {
            Some(span) if span.0 == row => span.2 = span.2.max(col + 1),
            _ => out.push((row, col, col + 1)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(rows: &[&str]) -> Vec<(char, u16, u16)> {
        rows.iter()
            .enumerate()
            .flat_map(|(r, text)| {
                text.chars()
                    .enumerate()
                    .map(move |(c, ch)| (ch, r as u16, c as u16))
            })
            .collect()
    }

    #[test]
    fn finds_the_url_under_the_pointer() {
        let cells = line(&["see https://phux.sh/docs for more"]);
        let link = find(&cells, 0, 10).expect("a link");
        assert_eq!(link.url, "https://phux.sh/docs");
        assert_eq!(link.spans, vec![(0, 4, 24)]);
        assert_eq!(find(&cells, 0, 2), None);
        assert_eq!(find(&cells, 0, 26), None);
    }

    #[test]
    fn trims_punctuation_and_unbalanced_brackets() {
        let cells = line(&["(see https://en.wikipedia.org/wiki/Rust_(language))."]);
        let link = find(&cells, 0, 8).expect("a link");
        assert_eq!(link.url, "https://en.wikipedia.org/wiki/Rust_(language)");
        let cells = line(&["go to https://phux.sh, then"]);
        assert_eq!(find(&cells, 0, 8).expect("a link").url, "https://phux.sh");
    }

    #[test]
    fn follows_a_url_across_a_soft_wrap() {
        let cells = line(&["x https://github.com/no-ph", "ux/phux/pull/1 done"]);
        let link = find(&cells, 1, 3).expect("a link");
        assert_eq!(link.url, "https://github.com/no-phux/phux/pull/1");
        assert_eq!(link.spans, vec![(0, 2, 26), (1, 0, 14)]);
    }

    /// A frame of `cols`-wide rows, the cells of `link_cols` in row 0
    /// carrying an OSC 8 URI.
    fn frame(rows: &[&str], cols: u16, link: Option<(&str, std::ops::Range<u16>)>) -> GridFrame {
        use phux_client_core::grid::{Cell, GridBuffer};
        let mut buffer = GridBuffer::default();
        let uri = link.as_ref().map(|(uri, _)| {
            buffer.utf8.extend_from_slice(uri.as_bytes());
            (0u32, uri.len() as u32)
        });
        for (r, text) in rows.iter().enumerate() {
            let chars: Vec<char> = text.chars().collect();
            for c in 0..cols {
                let mut cell = Cell::default();
                if let Some(ch) = chars.get(usize::from(c)).filter(|ch| **ch != ' ') {
                    cell.utf8_offset = buffer.utf8.len() as u32;
                    cell.utf8_len = ch.len_utf8() as u16;
                    buffer.utf8.extend_from_slice(ch.to_string().as_bytes());
                }
                if let (Some((offset, len)), Some((_, range))) = (uri, &link)
                    && r == 0
                    && range.contains(&c)
                {
                    cell.flags |= CELL_HYPERLINK;
                    cell.hyperlink_offset = offset;
                    cell.hyperlink_len = len;
                }
                buffer.cells.push(cell);
            }
        }
        GridFrame {
            terminal_id: phux_protocol::ids::ResourceId::local(1),
            generation: 1,
            stream_id: 0,
            bootstrap_id: 0,
            last_seq: 0,
            cols,
            rows: rows.len() as u16,
            cursor: Default::default(),
            scrollbar: Default::default(),
            colors: Default::default(),
            images: Vec::new(),
            damage: Default::default(),
            buffer,
        }
    }

    #[test]
    fn reads_wrapped_urls_off_a_frame() {
        let f = frame(&["open https://phux.sh/do", "cs now", "next"], 23, None);
        let link = at(&f, 1, 1).expect("a link on the wrapped half");
        assert_eq!(link.url, "https://phux.sh/docs");
        assert_eq!(link.spans, vec![(0, 5, 23), (1, 0, 2)]);
        assert_eq!(at(&f, 1, 2), None);
    }

    #[test]
    fn osc8_hyperlinks_win_over_their_text() {
        let f = frame(
            &["click here please"],
            20,
            Some(("https://phux.sh/x", 6..10)),
        );
        let link = at(&f, 7, 0).expect("an OSC 8 link");
        assert_eq!(link.url, "https://phux.sh/x");
        assert_eq!(link.spans, vec![(0, 6, 10)]);
        assert_eq!(at(&f, 2, 0), None);
        let unsafe_link = frame(&["run"], 5, Some(("javascript:x", 0..3)));
        assert_eq!(at(&unsafe_link, 1, 0), None);
    }

    #[test]
    fn a_row_start_is_a_word_boundary() {
        let cells = line(&["full width text ending in letters", "https://phux.sh next"]);
        assert_eq!(find(&cells, 1, 3).expect("a link").url, "https://phux.sh");
    }

    #[test]
    fn only_known_schemes_open() {
        assert!(openable("https://phux.sh"));
        assert!(openable("MAILTO:a@b.c"));
        assert!(!openable("https://"));
        assert!(!openable("javascript:alert(1)"));
        assert!(!openable("x-man-page://ls"));
        let cells = line(&["notahttps://x.y"]);
        assert_eq!(find(&cells, 0, 8), None);
    }
}
