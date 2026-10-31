//! Links in terminal output: OSC 8 hyperlinks a program attached to cells,
//! and URLs written as plain text, including ones soft-wrapped across rows.
//!
//! The schemes are GPUI's, so a program can't turn a ⌘-click into launching
//! an arbitrary handler; `file://` links face `open`'s further checks.

use phux_client_core::grid::CELL_HYPERLINK;
use phux_client_runtime::publication::GridFrame;

mod open;

/// libghostty's `CellWide::SpacerTail`: the right half of a wide glyph.
const WIDE_SPACER_TAIL: u8 = 2;
/// Rows a wrapped URL may span on either side of the pointer.
const WRAP_ROWS: u16 = 8;
/// Characters of `https://`, the longest of `SCHEMES`.
const LONGEST_SCHEME: usize = 8;
/// What a ⌘-click may open.
const SCHEMES: [&str; 6] = [
    "https://", "http://", "file://", "mailto:", "ssh://", "ftp://",
];

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

/// Whether `row`'s line soft-wraps into the next. Only the terminal knows:
/// a hard line that happens to fill the row ends there.
fn runs_on(frame: &GridFrame, row: u16) -> bool {
    frame
        .buffer
        .row_wrapped
        .get(usize::from(row))
        .copied()
        .unwrap_or(false)
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
        let rest: String = chars[i..].iter().take(LONGEST_SCHEME).collect();
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

/// Why a link can't be opened from this window, for `file://` links.
pub fn refusal(url: &str, remote: bool) -> Option<String> {
    let scheme = url.get(.."file://".len())?;
    if !scheme.eq_ignore_ascii_case("file://") {
        return None;
    }
    if remote {
        return Some("That file is on the other machine, so it wasn't opened here.".to_owned());
    }
    open::check_file(url)
}

/// Hand `url` to the system. Rune's `Tx::open_url` takes only web and mail
/// links, so the rest go to the platform's opener directly. `failed` hears
/// why if the opener starts but gives up.
pub fn open_other(url: &str, failed: impl FnOnce(String) + Send + 'static) -> std::io::Result<()> {
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        let reason = "a link can't hold spaces";
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            reason,
        ));
    }
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    open_with(opener, url, failed)
}

fn open_with(
    opener: &str,
    url: &str,
    failed: impl FnOnce(String) + Send + 'static,
) -> std::io::Result<()> {
    let mut child = std::process::Command::new(opener)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let url = url.to_owned();
    // The wait is also what reaps it; an opener can outlive the click.
    std::thread::spawn(move || match child.wait() {
        Ok(status) if status.success() => {}
        Ok(status) => failed(format!(
            "Couldn't open {url}: the opener ended with {status}."
        )),
        Err(error) => failed(format!("Couldn't open {url}: {error}")),
    });
    Ok(())
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
            let last = text.chars().count() >= usize::from(cols);
            buffer.row_wrapped.push(last && r + 1 < rows.len());
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
    fn a_hard_line_at_the_margin_does_not_join_the_next_row() {
        let mut f = frame(&["see https://phux.sh/do", "cs is unrelated"], 22, None);
        f.buffer.row_wrapped = vec![false, false];
        assert_eq!(at(&f, 6, 0).expect("a link").url, "https://phux.sh/do");
        assert_eq!(at(&f, 1, 1), None);
        f.buffer.row_wrapped = vec![true, false];
        assert_eq!(at(&f, 1, 1).expect("a link").url, "https://phux.sh/docs");
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
        assert!(openable("ssh://box") && openable("ftp://box/x"));
        assert!(openable("file:///tmp/notes.txt"));
        assert!(!openable("https://"));
        assert!(!openable("javascript:alert(1)"));
        assert!(!openable("x-man-page://ls"));
        let cells = line(&["notahttps://x.y"]);
        assert_eq!(find(&cells, 0, 8), None);
    }

    #[cfg(unix)]
    fn outcome(opener: &str) -> Result<String, std::sync::mpsc::RecvTimeoutError> {
        let (sender, receiver) = std::sync::mpsc::channel();
        open_with(opener, "ssh://box", move |why| {
            let _ = sender.send(why);
        })
        .expect("the opener starts");
        receiver.recv_timeout(std::time::Duration::from_secs(5))
    }

    #[cfg(unix)]
    #[test]
    fn an_opener_that_fails_is_reported_and_one_that_works_is_not() {
        use std::sync::mpsc::RecvTimeoutError::Disconnected;
        let why = outcome("false").expect("a failure");
        assert!(why.starts_with("Couldn't open ssh://box: "), "{why}");
        // Success drops the sender unused, once the child has been waited on.
        assert_eq!(outcome("true"), Err(Disconnected));
    }
}
