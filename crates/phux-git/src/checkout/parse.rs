//! Parsers for Git's machine output and text bounding, free of I/O.

use std::fmt::Write as _;

use super::{BINARY_SNIFF, BranchRef, Commit, CommitFile, FileDiff, FileText};
use crate::bounded::Bounded;

/// Status output up to its last complete NUL-terminated record.
pub(super) fn complete_records(out: &Bounded) -> String {
    let bytes = if out.truncated {
        let end = out
            .stdout
            .iter()
            .rposition(|byte| *byte == 0)
            .map_or(0, |i| i + 1);
        &out.stdout[..end]
    } else {
        &out.stdout[..]
    };
    String::from_utf8_lossy(bytes).into_owned()
}

pub(super) fn head_oid(status: &str) -> Option<String> {
    status
        .split('\0')
        .find_map(|record| record.strip_prefix("# branch.oid "))
        .filter(|oid| *oid != "(initial)")
        .map(ToOwned::to_owned)
}

#[derive(Default)]
pub(super) struct Counts {
    by_path: std::collections::HashMap<String, (Option<u32>, Option<u32>, bool)>,
    pub(super) truncated: bool,
}

impl Counts {
    pub(super) fn parse(out: &[u8], truncated: bool) -> Self {
        let by_path = parse_numstat(out)
            .into_iter()
            .map(|file| (file.path, (file.added, file.deleted, file.binary)))
            .collect();
        Self { by_path, truncated }
    }

    pub(super) fn get(&self, path: &str) -> Option<&(Option<u32>, Option<u32>, bool)> {
        self.by_path.get(path)
    }
}

/// `git diff --numstat -z`: `A\tD\tpath\0`, or for a rename
/// `A\tD\t\0from\0to\0`; binary files count `-`.
pub(super) fn parse_numstat(out: &[u8]) -> Vec<CommitFile> {
    let text = String::from_utf8_lossy(out);
    let mut records = text.split('\0');
    let mut files = Vec::new();
    while let Some(record) = records.next() {
        let mut fields = record.splitn(3, '\t');
        let (Some(added), Some(deleted), Some(path)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let (from, path) = if path.is_empty() {
            match (records.next(), records.next()) {
                (Some(from), Some(to)) => (Some(from.to_owned()), to),
                _ => continue,
            }
        } else {
            (None, path)
        };
        files.push(CommitFile {
            path: path.to_owned(),
            from,
            added: added.parse().ok(),
            deleted: deleted.parse().ok(),
            binary: added == "-" && deleted == "-",
        });
    }
    files
}

pub(super) fn parse_commit(record: &str) -> Option<Commit> {
    let mut fields = record.splitn(7, '\x1f');
    let sha = fields.next().filter(|sha| !sha.is_empty())?.to_owned();
    let short_sha = fields.next()?.to_owned();
    let author_name = fields.next()?.to_owned();
    let author_email = fields.next()?.to_owned();
    let time = fields.next()?.parse().unwrap_or(0);
    let refs = fields
        .next()?
        .split(", ")
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    let subject = fields.next().unwrap_or_default().to_owned();
    Some(Commit {
        sha,
        short_sha,
        subject,
        author_name,
        author_email,
        time,
        refs,
    })
}

pub(super) fn parse_branch(line: &str, current: Option<&str>) -> Option<BranchRef> {
    let mut fields = line.splitn(7, '\x1f');
    let refname = fields.next()?;
    let name = fields.next()?.to_owned();
    // `refs/remotes/origin/HEAD` is a pointer, not a branch.
    if refname.ends_with("/HEAD") {
        return None;
    }
    let remote = refname.starts_with("refs/remotes/");
    let upstream = Some(fields.next()?.to_owned()).filter(|name| !name.is_empty());
    let track = fields.next()?;
    let (ahead, behind) = if upstream.is_some() && track != "gone" {
        tracking(track)
    } else {
        (None, None)
    };
    Some(BranchRef {
        current: !remote && current == Some(name.as_str()),
        name,
        remote,
        upstream,
        ahead,
        behind,
        sha: fields.next()?.to_owned(),
        time: fields.next()?.parse().unwrap_or(0),
        subject: fields.next().unwrap_or_default().to_owned(),
    })
}

/// `%(upstream:track,nobracket)`: `ahead 2, behind 1`, either half, or empty
/// when even.
pub(super) fn tracking(track: &str) -> (Option<u32>, Option<u32>) {
    let mut ahead = 0;
    let mut behind = 0;
    for part in track.split(", ") {
        if let Some(count) = part.strip_prefix("ahead ") {
            ahead = count.parse().unwrap_or(0);
        } else if let Some(count) = part.strip_prefix("behind ") {
            behind = count.parse().unwrap_or(0);
        }
    }
    (Some(ahead), Some(behind))
}

/// Lossy text cut back to its last newline when the output was capped.
pub(super) fn text_at_line(bytes: &[u8], truncated: bool) -> (String, bool) {
    let end = match bytes.iter().rposition(|byte| *byte == b'\n') {
        Some(newline) if truncated => newline + 1,
        _ => bytes.len(),
    };
    (
        String::from_utf8_lossy(&bytes[..end]).into_owned(),
        truncated,
    )
}

/// A diff's text, cut at a line when capped, and whether Git called it binary.
pub(super) fn file_diff(bytes: &[u8], truncated: bool) -> FileDiff {
    let (text, truncated) = text_at_line(bytes, truncated);
    let binary = text
        .lines()
        .any(|line| line.starts_with("Binary files ") || line == "GIT binary patch");
    FileDiff {
        text,
        truncated,
        binary,
    }
}

/// The patch `git diff --no-index /dev/null <path>` prints for a new file
/// with `mode` and contents `bytes`, minus its `index` line (Git omits it
/// too when the new side comes from stdin). A symlink's bytes are its
/// target.
pub(super) fn new_file_patch(path: &str, mode: &str, bytes: &[u8]) -> Vec<u8> {
    let (a, b) = (quoted("a/", path), quoted("b/", path));
    let mut out = format!("diff --git {a} {b}\nnew file mode {mode}\n");
    if bytes.is_empty() {
        return out.into_bytes();
    }
    if is_binary(bytes) {
        let _ = writeln!(out, "Binary files /dev/null and {b} differ");
        return out.into_bytes();
    }
    let tab = if path.contains(' ') { "\t" } else { "" };
    let lines = line_count(bytes);
    let range = if lines == 1 {
        "1".to_owned()
    } else {
        format!("1,{lines}")
    };
    let _ = write!(out, "--- /dev/null\n+++ {b}{tab}\n@@ -0,0 +{range} @@\n");
    let mut out = out.into_bytes();
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        out.push(b'+');
        out.extend_from_slice(line);
    }
    if bytes.last() != Some(&b'\n') {
        out.extend_from_slice(b"\n\\ No newline at end of file\n");
    }
    out
}

/// `prefix` and `path` as Git names them in a patch header: C-quoted when
/// the path holds a quote, backslash, or control character
/// (`quote_c_style` with `core.quotePath=false`).
fn quoted(prefix: &str, path: &str) -> String {
    let special = |c: char| c == '"' || c == '\\' || c.is_ascii_control();
    if !path.chars().any(special) {
        return format!("{prefix}{path}");
    }
    let mut out = format!("\"{prefix}");
    for c in path.chars() {
        let escape = match c {
            '\x07' => "\\a",
            '\x08' => "\\b",
            '\t' => "\\t",
            '\n' => "\\n",
            '\x0b' => "\\v",
            '\x0c' => "\\f",
            '\r' => "\\r",
            '"' => "\\\"",
            '\\' => "\\\\",
            c if special(c) => {
                let _ = write!(out, "\\{:03o}", u32::from(c));
                continue;
            }
            c => {
                out.push(c);
                continue;
            }
        };
        out.push_str(escape);
    }
    out.push('"');
    out
}

pub(super) fn is_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(BINARY_SNIFF)].contains(&0)
}

pub(super) fn line_count(bytes: &[u8]) -> u32 {
    let newlines = memchr::memchr_iter(b'\n', bytes).count();
    let partial = usize::from(bytes.last().is_some_and(|byte| *byte != b'\n'));
    u32::try_from(newlines + partial).unwrap_or(u32::MAX)
}

/// UTF-8 text, or `binary`. A cut that split the final character keeps the
/// text before it.
pub(super) fn file_text(size: u64, mut bytes: Vec<u8>, truncated: bool) -> FileText {
    if is_binary(&bytes) {
        return FileText {
            size,
            binary: true,
            truncated,
            text: String::new(),
        };
    }
    if let Err(error) = std::str::from_utf8(&bytes) {
        if truncated && error.error_len().is_none() {
            bytes.truncate(error.valid_up_to());
        } else {
            return FileText {
                size,
                binary: true,
                truncated,
                text: String::new(),
            };
        }
    }
    FileText {
        size,
        binary: false,
        truncated,
        text: String::from_utf8(bytes).unwrap_or_default(),
    }
}
