//! Line staging: cut one file's diff down to the lines a person selected.
//!
//! [`super::Op::Apply`] hands the cut to `git apply --cached --recount`, so
//! Git counts each hunk's lines itself and only bodies are rewritten here.
//! The selection mirrors the diff kit's `DiffLineRange`.

/// One side of a diff, mirroring the kit's `DiffSide`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    /// Before the change: the index for unstaged changes, HEAD for staged ones.
    Old,
    New,
}

/// Selected source lines with the kit's `DiffLineRange` semantics.
///
/// From `start` on `side` to `end` on `end_side` (1-based). On one side it covers
/// that side's lines between the two numbers, in either order; across sides
/// it covers the patch body between them, as a unified view shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub side: Side,
    pub start: usize,
    pub end_side: Side,
    pub end: usize,
}

impl Selection {
    /// Lines `start` to `end`, in either order, on one side.
    #[must_use]
    pub const fn lines(side: Side, start: usize, end: usize) -> Self {
        Self {
            side,
            start,
            end_side: side,
            end,
        }
    }

    #[must_use]
    pub const fn with_end_side(mut self, side: Side) -> Self {
        self.end_side = side;
        self
    }
}

/// `file`'s diff (one `diff --git` section) holding only the selected added
/// and removed lines, or `None` when the selection holds none.
///
/// Forward, for staging: an unselected `+` is dropped and an unselected `-`
/// becomes context. Reverse, for `-R` unstaging: an unselected `+` becomes
/// context and an unselected `-` is dropped. A new or deleted file whose cut
/// keeps lines on both sides becomes a plain modification.
/// Renames and copies are refused; line selection never moves index paths.
#[must_use]
pub fn partial_patch(file: &str, selection: Selection, reverse: bool) -> Option<String> {
    if line_staging_refusal(file).is_some() {
        return None;
    }
    let (header, hunks) = parse(file)?;
    let lines: Vec<&Line<'_>> = hunks.iter().flat_map(|hunk| &hunk.lines).collect();
    let span = Span::of(&lines, selection);
    let mut chosen = lines
        .iter()
        .enumerate()
        .map(|(ix, line)| line.kind != Kind::Context && span.covers(ix, line));
    let (mut body, mut old, mut new) = (String::new(), false, false);
    for hunk in &hunks {
        let out: Vec<Out<'_>> = hunk
            .lines
            .iter()
            .filter_map(|line| keep(line, chosen.next().unwrap_or(false), reverse))
            .collect();
        if out.iter().all(|line| line.0 == ' ') {
            continue;
        }
        old |= out.iter().any(|line| line.0 != '+');
        new |= out.iter().any(|line| line.0 != '-');
        body.push_str(hunk.header);
        body.push('\n');
        for (prefix, text, no_eol) in end_lines_last(out) {
            body.push(prefix);
            body.push_str(text);
            body.push('\n');
            if no_eol {
                body.push_str("\\ No newline at end of file\n");
            }
        }
    }
    (!body.is_empty()).then(|| file_header(&header, old && new) + &body)
}

/// Why this file cannot be staged by line. Whole-file actions still work.
#[must_use]
pub fn line_staging_refusal(file: &str) -> Option<&'static str> {
    let relocated = file
        .lines()
        .take_while(|line| !line.starts_with("@@ "))
        .any(|line| {
            ["rename from ", "rename to ", "copy from ", "copy to "]
                .iter()
                .any(|prefix| line.starts_with(prefix))
        });
    relocated.then_some("Renames and copies cannot be staged or unstaged by line. Use the whole-file action or the terminal instead.")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Context,
    Removed,
    Added,
}

/// One body line, numbered on each side it is on.
struct Line<'a> {
    kind: Kind,
    text: &'a str,
    old: Option<usize>,
    new: Option<usize>,
    /// Followed by `\ No newline at end of file`.
    no_eol: bool,
}

impl Line<'_> {
    const fn number(&self, side: Side) -> Option<usize> {
        match side {
            Side::Old => self.old,
            Side::New => self.new,
        }
    }
}

struct Hunk<'a> {
    header: &'a str,
    lines: Vec<Line<'a>>,
}

/// A rewritten body line: prefix, text, and whether it lacks a newline.
type Out<'a> = (char, &'a str, bool);

/// The lines before the first hunk, then the hunks. Lines keep a `\r`.
fn parse(file: &str) -> Option<(Vec<&str>, Vec<Hunk<'_>>)> {
    let (mut header, mut hunks) = (Vec::new(), Vec::<Hunk<'_>>::new());
    let (mut old, mut new) = (0, 0);
    for raw in file.split_inclusive('\n') {
        let raw = raw.strip_suffix('\n').unwrap_or(raw);
        if raw.starts_with("@@ ") {
            (old, new) = starts(raw)?;
            hunks.push(Hunk {
                header: raw,
                lines: Vec::new(),
            });
            continue;
        }
        let Some(hunk) = hunks.last_mut() else {
            header.push(raw);
            continue;
        };
        if raw.starts_with('\\') {
            if let Some(last) = hunk.lines.last_mut() {
                last.no_eol = true;
            }
            continue;
        }
        let (kind, text) = body_line(raw)?;
        hunk.lines.push(Line {
            kind,
            text,
            old: (kind != Kind::Added).then(|| next(&mut old)),
            new: (kind != Kind::Removed).then(|| next(&mut new)),
            no_eol: false,
        });
    }
    (!hunks.is_empty()).then_some((header, hunks))
}

const fn next(line: &mut usize) -> usize {
    *line += 1;
    *line - 1
}

/// The first old and new line numbers of `@@ -a[,b] +c[,d] @@`.
fn starts(header: &str) -> Option<(usize, usize)> {
    let mut fields = header.split(' ').skip(1);
    let mut start = |sign: char| -> Option<usize> {
        fields
            .next()?
            .strip_prefix(sign)?
            .split(',')
            .next()?
            .parse()
            .ok()
    };
    Some((start('-')?, start('+')?))
}

/// A body line's kind and text. An empty line is context whose trailing
/// space was stripped in transit.
fn body_line(raw: &str) -> Option<(Kind, &str)> {
    let kind = match raw.bytes().next() {
        None => return Some((Kind::Context, "")),
        Some(b' ') => Kind::Context,
        Some(b'-') => Kind::Removed,
        Some(b'+') => Kind::Added,
        Some(_) => return None,
    };
    Some((kind, &raw[1..]))
}

/// The forward or reverse rule for one line.
const fn keep<'a>(line: &Line<'a>, chosen: bool, reverse: bool) -> Option<Out<'a>> {
    let prefix = match (line.kind, chosen, reverse) {
        (Kind::Removed, true, _) => '-',
        (Kind::Added, true, _) => '+',
        (Kind::Added, false, false) | (Kind::Removed, false, true) => return None,
        (Kind::Context, ..) | (Kind::Added, false, true) | (Kind::Removed, false, false) => ' ',
    };
    Some((prefix, line.text, line.no_eol))
}

/// A context line without a newline must end its hunk. When lines follow one
/// (the old last line, kept while lines are added after it), it is removed
/// and re-added with its newline.
fn end_lines_last(out: Vec<Out<'_>>) -> Vec<Out<'_>> {
    let last = out.len().saturating_sub(1);
    out.into_iter()
        .enumerate()
        .flat_map(|(ix, (prefix, text, no_eol))| {
            if prefix == ' ' && no_eol && ix != last {
                vec![('-', text, true), ('+', text, false)]
            } else {
                vec![(prefix, text, no_eol)]
            }
        })
        .collect()
}

/// A selection resolved against one file's lines, as the kit resolves it.
enum Span {
    /// Lines numbered `low..=high` on one side.
    Side {
        side: Side,
        low: usize,
        high: usize,
    },
    /// Body lines by patch index, across both sides.
    Patch(std::ops::RangeInclusive<usize>),
    Empty,
}

impl Span {
    fn of(lines: &[&Line<'_>], selection: Selection) -> Self {
        let Selection {
            side,
            start,
            end_side,
            end,
        } = selection;
        if side == end_side {
            return Self::Side {
                side,
                low: start.min(end),
                high: start.max(end),
            };
        }
        match endpoint(lines, side, start, true).zip(endpoint(lines, end_side, end, false)) {
            Some((a, b)) => Self::Patch(a.min(b)..=a.max(b)),
            None => Self::Empty,
        }
    }

    fn covers(&self, ix: usize, line: &Line<'_>) -> bool {
        match self {
            Self::Side { side, low, high } => line
                .number(*side)
                .is_some_and(|n| (*low..=*high).contains(&n)),
            Self::Patch(span) => span.contains(&ix),
            Self::Empty => false,
        }
    }
}

/// The patch index of `line` on `side`, or of the nearest line there: the
/// next for a start, the previous for an end, as the kit clips a range.
fn endpoint(lines: &[&Line<'_>], side: Side, line: usize, start: bool) -> Option<usize> {
    let numbered = || {
        lines
            .iter()
            .enumerate()
            .filter_map(move |(ix, source)| Some((source.number(side)?, ix)))
    };
    let after = numbered().find(|&(n, _)| n >= line);
    let before = numbered().rev().find(|&(n, _)| n <= line);
    let pick = if start {
        after.or(before)
    } else {
        before.or(after)
    };
    pick.map(|(_, ix)| ix)
}

/// Header lines a modification does not carry.
const EXISTENCE: [&str; 3] = ["new file mode ", "deleted file mode ", "index "];

/// The file header, as a modification's when a new or deleted file keeps
/// lines on both sides.
fn file_header(header: &[&str], both: bool) -> String {
    // These describe file metadata, not selected content. Git preserves the
    // current index mode when a content patch carries no mode transition.
    let header: Vec<&str> = header
        .iter()
        .copied()
        .filter(|line| !line.starts_with("old mode ") && !line.starts_with("new mode "))
        .collect();
    let creates_or_deletes = header
        .iter()
        .any(|line| EXISTENCE[..2].iter().any(|mode| line.starts_with(mode)));
    let lines = if both && creates_or_deletes {
        as_modification(&header)
    } else {
        header.iter().map(|line| (*line).to_owned()).collect()
    };
    lines
        .iter()
        .flat_map(|line| [line.as_str(), "\n"])
        .collect()
}

/// `header` without mode or index lines, and with the real path, its `a/`
/// or `b/` swapped (inside quotes too), in place of `/dev/null`.
fn as_modification(header: &[&str]) -> Vec<String> {
    let path = |prefix: &str| {
        header
            .iter()
            .find_map(|line| line.strip_prefix(prefix))
            .unwrap_or_default()
    };
    header
        .iter()
        .filter(|line| !EXISTENCE.iter().any(|kind| line.starts_with(kind)))
        .map(|line| match *line {
            "--- /dev/null" => format!("--- {}", path("+++ ").replacen("b/", "a/", 1)),
            "+++ /dev/null" => format!("+++ {}", path("--- ").replacen("a/", "b/", 1)),
            line => line.to_owned(),
        })
        .collect()
}
