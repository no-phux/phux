//! Line staging: cut one file's diff down to the lines a person selected, as
//! a patch `git apply --cached` (or `--cached -R`) accepts.
//!
//! The selection mirrors the diff kit's `DiffLineRange`, so a range the panel
//! reports can be passed through unchanged. Everything here is pure text
//! work; [`super::Op::Apply`] hands the result to Git.

/// One side of a diff, mirroring the kit's `DiffSide`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    /// The file before the change: the index for unstaged changes, HEAD for
    /// staged ones.
    Old,
    /// The file after the change.
    New,
}

/// A selection of source lines in one file, with the kit's `DiffLineRange`
/// semantics: it starts at `start` on `side` and ends at `end` on `end_side`
/// (1-based lines). When both sides are the same it covers that side's lines
/// between the two numbers, in either order. A range ending on the other
/// side covers the patch body between the two lines in patch order, as a
/// unified view shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    /// Repository-relative path of the file, as the diff names it.
    pub path: String,
    /// The side `start` is on.
    pub side: Side,
    /// First selected line on `side`.
    pub start: usize,
    /// The side `end` is on.
    pub end_side: Side,
    /// Last selected line on `end_side`.
    pub end: usize,
}

impl Selection {
    /// Lines `start` to `end` (in either order) on one side of `path`.
    #[must_use]
    pub fn lines(path: impl Into<String>, side: Side, start: usize, end: usize) -> Self {
        Self {
            path: path.into(),
            side,
            start,
            end_side: side,
            end,
        }
    }

    /// End the selection on `side`, keeping `end` as its line there.
    #[must_use]
    pub fn with_end_side(mut self, side: Side) -> Self {
        self.end_side = side;
        self
    }
}

/// A patch that applies only the selected added or removed lines of `path`'s
/// diff in `patch`.
///
/// Forward (`reverse == false`) is for `git apply --cached`, staging out of
/// [`super::Source::Changes`]: a selected `+` or `-` stays, an unselected `+`
/// is dropped and an unselected `-` becomes context. Reverse is for
/// `git apply --cached -R`, unstaging out of [`super::Source::Staged`]: an
/// unselected `+` becomes context and an unselected `-` is dropped.
///
/// Hunk headers are recomputed, hunks left without a change are dropped and
/// the file header is kept. A new or deleted file whose cut leaves content on
/// both sides becomes a plain modification. Returns `None` when the selection
/// holds no changed line, or `path` has no textual diff in `patch`.
#[must_use]
pub fn partial_patch(patch: &str, selection: &Selection, reverse: bool) -> Option<String> {
    let lines: Vec<&str> = patch
        .split_inclusive('\n')
        .map(|line| line.strip_suffix('\n').unwrap_or(line))
        .collect();
    let file = file_sections(&lines)
        .into_iter()
        .filter_map(parse_file)
        .find(|file| file.names(&selection.path))?;
    let chosen = chosen(&file.lines, selection);
    if !chosen.iter().any(|&chosen| chosen) {
        return None;
    }
    file.cut(&chosen, reverse)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Context,
    Removed,
    Added,
}

/// One body line of a hunk, with its number on each side it is on.
#[derive(Debug)]
struct Line<'a> {
    kind: Kind,
    text: &'a str,
    old: Option<usize>,
    new: Option<usize>,
    /// Followed by `\ No newline at end of file`.
    no_eol: bool,
}

impl Line<'_> {
    fn number(&self, side: Side) -> Option<usize> {
        match side {
            Side::Old => self.old,
            Side::New => self.new,
        }
    }
}

#[derive(Debug)]
struct Hunk<'a> {
    old_start: usize,
    old_count: usize,
    new_start: usize,
    new_count: usize,
    /// Whatever follows the closing `@@`, such as a function name.
    tail: &'a str,
    /// This hunk's lines in [`FileDiff::lines`].
    lines: std::ops::Range<usize>,
}

/// One file's section of a Git diff.
#[derive(Debug)]
struct FileDiff<'a> {
    /// Every line before the first hunk.
    header: Vec<&'a str>,
    hunks: Vec<Hunk<'a>>,
    /// Every hunk's body, in patch order.
    lines: Vec<Line<'a>>,
}

/// A rewritten body line: its prefix, text, and end-of-file marker.
type Out<'a> = (char, &'a str, bool);

/// Split a patch into per-file sections at each `diff --git` line.
fn file_sections<'a, 'b>(lines: &'b [&'a str]) -> Vec<&'b [&'a str]> {
    let starts: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.starts_with("diff --git "))
        .map(|(ix, _)| ix)
        .collect();
    starts
        .iter()
        .enumerate()
        .map(|(n, &start)| &lines[start..starts.get(n + 1).copied().unwrap_or(lines.len())])
        .collect()
}

fn parse_file<'a>(section: &[&'a str]) -> Option<FileDiff<'a>> {
    let first_hunk = section.iter().position(|line| line.starts_with("@@ "))?;
    let mut file = FileDiff {
        header: section[..first_hunk].to_vec(),
        hunks: Vec::new(),
        lines: Vec::new(),
    };
    let mut ix = first_hunk;
    while ix < section.len() {
        if !section[ix].starts_with("@@ ") {
            ix += 1;
            continue;
        }
        let hunk = parse_hunk(section, &mut ix, &mut file.lines)?;
        file.hunks.push(hunk);
    }
    Some(file)
}

/// Parse the hunk whose header is `section[*ix]`, leaving `ix` past it.
fn parse_hunk<'a>(
    section: &[&'a str],
    ix: &mut usize,
    lines: &mut Vec<Line<'a>>,
) -> Option<Hunk<'a>> {
    let (old_start, old_count, new_start, new_count, tail) = hunk_header(section[*ix])?;
    *ix += 1;
    let begin = lines.len();
    let (mut old_left, mut new_left) = (old_count, new_count);
    let (mut old, mut new) = (old_start, new_start);
    while old_left + new_left > 0 || section.get(*ix).is_some_and(|line| line.starts_with('\\')) {
        let raw = *section.get(*ix)?;
        *ix += 1;
        if raw.starts_with('\\') {
            if lines.len() > begin {
                lines.last_mut()?.no_eol = true;
            }
            continue;
        }
        let (kind, text) = body_line(raw)?;
        let mut line = Line {
            kind,
            text,
            old: None,
            new: None,
            no_eol: false,
        };
        if kind != Kind::Added {
            old_left = old_left.checked_sub(1)?;
            line.old = Some(old);
            old += 1;
        }
        if kind != Kind::Removed {
            new_left = new_left.checked_sub(1)?;
            line.new = Some(new);
            new += 1;
        }
        lines.push(line);
    }
    Some(Hunk {
        old_start,
        old_count,
        new_start,
        new_count,
        tail,
        lines: begin..lines.len(),
    })
}

/// A body line's kind and its text without the prefix. An empty line is
/// context whose trailing space was stripped in transit.
fn body_line(raw: &str) -> Option<(Kind, &str)> {
    let kind = match raw.as_bytes().first() {
        None => return Some((Kind::Context, "")),
        Some(b' ') => Kind::Context,
        Some(b'-') => Kind::Removed,
        Some(b'+') => Kind::Added,
        Some(_) => return None,
    };
    Some((kind, &raw[1..]))
}

/// `@@ -a[,b] +c[,d] @@tail` as `(a, b, c, d, tail)`; a missing count is 1.
fn hunk_header(line: &str) -> Option<(usize, usize, usize, usize, &str)> {
    let rest = line.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let (new, tail) = rest.split_once(" @@")?;
    let range = |text: &str| -> Option<(usize, usize)> {
        match text.split_once(',') {
            Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
            None => Some((text.parse().ok()?, 1)),
        }
    };
    let (old_start, old_count) = range(old)?;
    let (new_start, new_count) = range(new)?;
    Some((old_start, old_count, new_start, new_count, tail))
}

/// Which changed lines the selection covers, by index in `lines`.
fn chosen(lines: &[Line], selection: &Selection) -> Vec<bool> {
    let span = Span::of(lines, selection);
    lines
        .iter()
        .enumerate()
        .map(|(ix, line)| line.kind != Kind::Context && span.covers(ix, line))
        .collect()
}

/// A selection resolved against one file's lines, as the kit resolves it.
enum Span {
    /// Lines numbered `low..=high` on one side.
    Side { side: Side, low: usize, high: usize },
    /// Body lines by patch index, across both sides.
    Patch(std::ops::RangeInclusive<usize>),
    /// Neither end lands anywhere in the file.
    Empty,
}

impl Span {
    fn of(lines: &[Line], selection: &Selection) -> Self {
        let (start, end) = (selection.start, selection.end);
        if selection.side == selection.end_side {
            return Self::Side {
                side: selection.side,
                low: start.min(end),
                high: start.max(end),
            };
        }
        let first = endpoint(lines, selection.side, start, true);
        let last = endpoint(lines, selection.end_side, end, false);
        match first.zip(last) {
            Some((a, b)) => Self::Patch(a.min(b)..=a.max(b)),
            None => Self::Empty,
        }
    }

    fn covers(&self, ix: usize, line: &Line) -> bool {
        match self {
            Self::Side { side, low, high } => line
                .number(*side)
                .is_some_and(|n| (*low..=*high).contains(&n)),
            Self::Patch(span) => span.contains(&ix),
            Self::Empty => false,
        }
    }
}

/// The patch index of `line` on `side`, or the nearest line there: the next
/// one for a start, the previous one for an end, as the kit clips a range.
fn endpoint(lines: &[Line], side: Side, line: usize, start: bool) -> Option<usize> {
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

impl<'a> FileDiff<'a> {
    /// Whether this section is `path`'s, on either side.
    fn names(&self, path: &str) -> bool {
        let side = |prefix: &str, strip: &str| {
            self.header
                .iter()
                .find_map(|line| line.strip_prefix(prefix))
                .map(|value| decode_path(value.split('\t').next().unwrap_or(value)))
                .is_some_and(|value| value.strip_prefix(strip) == Some(path))
        };
        side("+++ ", "b/") || side("--- ", "a/")
    }

    fn cut(&self, chosen: &[bool], reverse: bool) -> Option<String> {
        let mut body = Vec::new();
        let (mut old_total, mut new_total) = (0, 0);
        // How far the moving side has drifted from the anchored one.
        let mut drift: isize = 0;
        for hunk in &self.hunks {
            let out = rewrite(
                &self.lines[hunk.lines.clone()],
                &chosen[hunk.lines.clone()],
                reverse,
            );
            if !out.iter().any(|&(prefix, _, _)| prefix != ' ') {
                continue;
            }
            let old_count = out.iter().filter(|line| line.0 != '+').count();
            let new_count = out.iter().filter(|line| line.0 != '-').count();
            let (old_start, new_start) = starts(hunk, reverse, old_count, new_count, drift);
            drift += if reverse {
                old_count as isize - new_count as isize
            } else {
                new_count as isize - old_count as isize
            };
            old_total += old_count;
            new_total += new_count;
            body.push(format!(
                "@@ -{old_start},{old_count} +{new_start},{new_count} @@{}",
                hunk.tail
            ));
            for (prefix, text, no_eol) in out {
                body.push(format!("{prefix}{text}"));
                if no_eol {
                    body.push("\\ No newline at end of file".to_owned());
                }
            }
        }
        if body.is_empty() {
            return None;
        }
        let mut lines = self.header(old_total, new_total);
        lines.extend(body);
        Some(lines.join("\n") + "\n")
    }

    /// The file header, turned into a modification's when a new or deleted
    /// file keeps lines on both sides.
    fn header(&self, old_total: usize, new_total: usize) -> Vec<String> {
        let has = |prefix: &str| self.header.iter().any(|line| line.starts_with(prefix));
        let both = old_total > 0 && new_total > 0;
        if !(both && (has("new file mode ") || has("deleted file mode "))) {
            return self.header.iter().map(|&line| line.to_owned()).collect();
        }
        let value = |prefix: &str| {
            self.header
                .iter()
                .find_map(|line| line.strip_prefix(prefix))
                .unwrap_or_default()
        };
        let (old, new) = (value("--- "), value("+++ "));
        self.header
            .iter()
            .filter(|line| {
                !["new file mode ", "deleted file mode ", "index "]
                    .iter()
                    .any(|prefix| line.starts_with(prefix))
            })
            .map(|&line| match line {
                "--- /dev/null" => format!("--- {}", swap_prefix(new, "b/", "a/")),
                "+++ /dev/null" => format!("+++ {}", swap_prefix(old, "a/", "b/")),
                _ => line.to_owned(),
            })
            .collect()
    }
}

/// The new hunk's starts. The anchored side (old going forward, new in
/// reverse) keeps its lines and so its start; the other side's first line
/// moves by `drift`. A side with no lines names the line before, as Git does.
fn starts(
    hunk: &Hunk,
    reverse: bool,
    old_count: usize,
    new_count: usize,
    drift: isize,
) -> (usize, usize) {
    let first = |start: usize, count: usize| if count == 0 { start + 1 } else { start };
    let place = |first: usize, count: usize| {
        let first = first.saturating_add_signed(drift);
        if count == 0 {
            first.saturating_sub(1)
        } else {
            first
        }
    };
    if reverse {
        let anchor = first(hunk.new_start, hunk.new_count);
        (place(anchor, old_count), hunk.new_start)
    } else {
        let anchor = first(hunk.old_start, hunk.old_count);
        (hunk.old_start, place(anchor, new_count))
    }
}

/// Apply the forward or reverse rules to one hunk's body.
fn rewrite<'a>(lines: &[Line<'a>], chosen: &[bool], reverse: bool) -> Vec<Out<'a>> {
    let mut out: Vec<Out<'a>> = Vec::with_capacity(lines.len());
    for (line, &chosen) in lines.iter().zip(chosen) {
        let prefix = match (line.kind, chosen, reverse) {
            (Kind::Context, _, _) => ' ',
            (Kind::Removed, true, _) => '-',
            (Kind::Added, true, _) => '+',
            (Kind::Added, false, false) | (Kind::Removed, false, true) => continue,
            (Kind::Added, false, true) | (Kind::Removed, false, false) => ' ',
        };
        out.push((prefix, line.text, line.no_eol));
    }
    end_lines_last(out)
}

/// A context line without a newline must end the hunk. When lines were added
/// after one (the old file's last line, kept while new lines follow it), it
/// is removed and re-added with its newline.
fn end_lines_last(out: Vec<Out<'_>>) -> Vec<Out<'_>> {
    let last = out.len().saturating_sub(1);
    let mut fixed = Vec::with_capacity(out.len() + 1);
    for (ix, (prefix, text, no_eol)) in out.into_iter().enumerate() {
        if prefix == ' ' && no_eol && ix != last {
            fixed.push(('-', text, true));
            fixed.push(('+', text, false));
        } else {
            fixed.push((prefix, text, no_eol));
        }
    }
    fixed
}

/// Replace `from` with `to` at the start of a header path, inside quotes too.
fn swap_prefix(path: &str, from: &str, to: &str) -> String {
    if let Some(rest) = path
        .strip_prefix('"')
        .and_then(|inner| inner.strip_prefix(from))
    {
        return format!("\"{to}{rest}");
    }
    match path.strip_prefix(from) {
        Some(rest) => format!("{to}{rest}"),
        None => path.to_owned(),
    }
}

/// Decode a path Git wrote C-quoted (`"a/caf\303\251"`); others pass through.
fn decode_path(value: &str) -> String {
    let Some(inner) = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    else {
        return value.to_owned();
    };
    let bytes = inner.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut ix = 0;
    while ix < bytes.len() {
        let byte = bytes[ix];
        ix += 1;
        if byte != b'\\' || ix == bytes.len() {
            out.push(byte);
            continue;
        }
        let escaped = bytes[ix];
        ix += 1;
        let octal = bytes
            .get(ix - 1..ix + 2)
            .filter(|digits| digits.iter().all(|d| (b'0'..=b'7').contains(d)));
        if let Some(digits) = octal {
            out.push(
                digits
                    .iter()
                    .fold(0u8, |value, d| value.wrapping_mul(8) + (d - b'0')),
            );
            ix += 2;
            continue;
        }
        out.push(match escaped {
            b'a' => 0x07,
            b'b' => 0x08,
            b't' => b'\t',
            b'n' => b'\n',
            b'v' => 0x0b,
            b'f' => 0x0c,
            b'r' => b'\r',
            other => other,
        });
    }
    String::from_utf8_lossy(&out).into_owned()
}
