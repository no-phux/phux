//! `git status --porcelain=v2` parsing.

use crate::Area;

/// One changed path in one [`Area`]. A file staged and then edited again is
/// two entries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) area: Area,
    /// Repository-relative path.
    pub(crate) path: String,
    /// The path a rename or copy came from.
    pub(crate) from: Option<String>,
    /// Porcelain status letter: `M`, `A`, `D`, `R`, `C`, `T`, `U` or `?`.
    pub(crate) code: char,
}

/// The checked-out branch and how it stands against its upstream.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Branch {
    /// `None` when HEAD is detached.
    pub(crate) name: Option<String>,
    pub(crate) upstream: Option<String>,
    pub(crate) ahead: u32,
    pub(crate) behind: u32,
}

/// One checkout's state as of one `git status`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Status {
    pub(crate) branch: Branch,
    /// Sorted by area, then path.
    pub(crate) entries: Vec<Entry>,
}

/// Git's message, trimmed to what fits in a notice.
pub(crate) fn failure(stderr: &str, stdout: &str) -> String {
    let text = if stderr.trim().is_empty() {
        stdout
    } else {
        stderr
    };
    let lines: Vec<&str> = text.trim().lines().collect();
    let tail = &lines[lines.len().saturating_sub(12)..];
    if tail.is_empty() {
        "git failed without a message.".into()
    } else {
        tail.join("\n")
    }
}

/// Parse `git status --porcelain=v2 --branch -z`.
pub(crate) fn parse_status(out: &str) -> Status {
    let mut branch = Branch::default();
    let mut entries = Vec::new();
    let mut records = out.split('\0');
    while let Some(record) = records.next() {
        if let Some(header) = record.strip_prefix("# ") {
            parse_header(header, &mut branch);
            continue;
        }
        let Some((kind, rest)) = record.split_once(' ') else {
            continue;
        };
        match kind {
            "1" => tracked(rest, 7, None, &mut entries),
            // A rename's source is the next record; output cut before it
            // leaves a rename that cannot be shown truthfully, so drop it.
            "2" => {
                let Some(from) = records.next().filter(|from| !from.is_empty()) else {
                    continue;
                };
                tracked(rest, 8, Some(from.to_owned()), &mut entries);
            }
            "u" => {
                if let Some(path) = rest.splitn(10, ' ').nth(9) {
                    entries.push(Entry {
                        area: Area::Conflicted,
                        path: path.to_owned(),
                        from: None,
                        code: 'U',
                    });
                }
            }
            "?" => entries.push(Entry {
                area: Area::Untracked,
                path: rest.to_owned(),
                from: None,
                code: '?',
            }),
            _ => {}
        }
    }
    entries.sort_by(|a, b| (a.area, &a.path).cmp(&(b.area, &b.path)));
    Status { branch, entries }
}

fn parse_header(header: &str, branch: &mut Branch) {
    let Some((key, value)) = header.split_once(' ') else {
        return;
    };
    match key {
        "branch.head" if value != "(detached)" => branch.name = Some(value.to_owned()),
        "branch.upstream" => branch.upstream = Some(value.to_owned()),
        "branch.ab" => {
            for part in value.split(' ') {
                if let Some(ahead) = part.strip_prefix('+') {
                    branch.ahead = ahead.parse().unwrap_or(0);
                } else if let Some(behind) = part.strip_prefix('-') {
                    branch.behind = behind.parse().unwrap_or(0);
                }
            }
        }
        _ => {}
    }
}

/// An ordinary (`1`) or renamed (`2`) record: `XY` then `skip` fields, then
/// the path, which may itself contain spaces.
fn tracked(rest: &str, skip: usize, from: Option<String>, entries: &mut Vec<Entry>) {
    let mut fields = rest.splitn(skip + 1, ' ');
    let Some(xy) = fields.next() else {
        return;
    };
    let Some(path) = fields.nth(skip - 1) else {
        return;
    };
    let mut codes = xy.chars();
    let (index, tree) = (codes.next().unwrap_or('.'), codes.next().unwrap_or('.'));
    if index != '.' {
        entries.push(Entry {
            area: Area::Staged,
            path: path.to_owned(),
            from,
            code: index,
        });
    }
    if tree != '.' {
        entries.push(Entry {
            area: Area::Unstaged,
            path: path.to_owned(),
            from: None,
            code: tree,
        });
    }
}
