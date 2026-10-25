//! Command-click links: the cell's OSC 8 hyperlink, else a URL written in the
//! row's text. Only schemes a person expects a click to open are returned, so
//! program output cannot turn a click into an arbitrary URL-scheme launch.
//!
//! A click is a press and a release against one painted frame. The press
//! captures the link on that frame. The release opens it only when the same
//! link is still under the pointer, in the same viewport and connection.
//! Output that does not change that link is not a new target. Dragging off,
//! scrolling, resizing, or replacing the connection cancels the gesture.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use phux_client_runtime::publication::GridFrame;

use super::Identity;

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

/// How many symlinks one click will follow. Linux's `MAXSYMLINKS`.
const MAX_SYMLINKS: usize = 40;

/// Title when a click does not open.
pub(crate) const LINK_REFUSED: &str = "Link not opened";

const ANOTHER_MACHINE: &str = "This file link names another machine, so it was not opened.";
const PROGRAM: &str = "This file is an app or a program, so it was not opened.";

/// Whether a clicked link may be handed to the system.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LinkAdmit {
    /// Open the original URL.
    Open,
    /// Do not open. Show this notice.
    Refuse {
        /// Notification title.
        title: &'static str,
        /// Notification body.
        body: String,
    },
}

/// `file://` links from a remote terminal name that host's files. A nonlocal
/// authority, an app bundle, or an executable is not opened on this Mac
/// either. Other schemes already allowed by [`link_in`] still open.
#[must_use]
pub(crate) fn admit_url(url: &str, host: &crate::attach::TerminalHost) -> LinkAdmit {
    if allowed(url) && !url.starts_with("file://") {
        return LinkAdmit::Open;
    }
    let Some((authority, path)) = file_url(url) else {
        return refuse("This link was not opened.");
    };
    if let crate::attach::TerminalHost::Remote { label } = host {
        let from = label.as_deref().unwrap_or("another machine");
        return refuse(&format!(
            "This file link came from {from}. It was not opened on this Mac."
        ));
    }
    if !(authority.is_empty() || authority.eq_ignore_ascii_case("localhost")) {
        return refuse(ANOTHER_MACHINE);
    }
    let Some(path) = decoded_path(&path) else {
        return refuse("This file link was not opened.");
    };
    admit_path(Path::new(&path))
}

/// Whether a path this app is about to hand to the default application may
/// be opened. Bundles, executables, and nonlocal paths are refused.
/// [`HostAction::Open`](crate::terminal::HostAction::Open)'s only caller writes
/// a mode `0o600` `.txt` export under the temp directory; this check is what
/// keeps a different path from launching.
#[must_use]
pub(crate) fn admit_path(path: &Path) -> LinkAdmit {
    match launch_block(path) {
        Ok(()) => LinkAdmit::Open,
        Err(body) => refuse(body),
    }
}

fn refuse(body: &str) -> LinkAdmit {
    LinkAdmit::Refuse {
        title: LINK_REFUSED,
        body: body.to_owned(),
    }
}

/// `(authority, path)` of a `file:` URL. The path still includes its query
/// and fragment, and is not percent-decoded.
fn file_url(url: &str) -> Option<(String, String)> {
    let (scheme, rest) = url.split_once(':')?;
    if !scheme.eq_ignore_ascii_case("file") {
        return None;
    }
    let rest = rest.strip_prefix("//")?;
    if let Some(path) = rest.strip_prefix('/') {
        Some((String::new(), format!("/{path}")))
    } else if let Some((authority, path)) = rest.split_once('/') {
        Some((percent_decode(authority)?, format!("/{path}")))
    } else if rest.is_empty() {
        Some((String::new(), "/".to_owned()))
    } else {
        Some((percent_decode(rest)?, "/".to_owned()))
    }
}

/// Absolute path with query and fragment removed. `None` when decoding fails
/// or the path is not absolute.
fn decoded_path(raw: &str) -> Option<String> {
    let bare = raw.split(['?', '#']).next().unwrap_or(raw);
    let decoded = percent_decode(bare)?;
    if decoded.contains('\0') || !decoded.starts_with('/') {
        return None;
    }
    Some(decoded)
}

fn percent_decode(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = bytes.get(index + 1..index + 3)?;
            let value = u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?;
            out.push(value);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The refusal body when `path` must not launch: it names another machine
/// (`//server`, `/net`, a symlink there), or it is an app bundle, a known
/// launcher extension, an executable, or a symlink loop.
fn launch_block(path: &Path) -> Result<(), &'static str> {
    // `//server/share` is a UNC path. Lexical normalization collapses the
    // extra slash, so this has to be read from the path as given.
    if path_starts_with_unc(path) {
        return Err(ANOTHER_MACHINE);
    }
    let mut current = lexical_normal(path);
    for _ in 0..MAX_SYMLINKS {
        if is_automount(&current) {
            return Err(ANOTHER_MACHINE);
        }
        if is_bundle(&current) || launches_by_extension(&current) {
            return Err(PROGRAM);
        }
        let Ok(meta) = std::fs::symlink_metadata(&current) else {
            return Ok(());
        };
        if !meta.file_type().is_symlink() {
            return if is_executable(&meta) {
                Err(PROGRAM)
            } else {
                Ok(())
            };
        }
        let target = std::fs::read_link(&current).map_err(|_| PROGRAM)?;
        if path_starts_with_unc(&target) {
            return Err(ANOTHER_MACHINE);
        }
        current = if target.is_absolute() {
            lexical_normal(&target)
        } else {
            lexical_normal(&current.parent().ok_or(PROGRAM)?.join(target))
        };
    }
    Err(PROGRAM)
}

fn lexical_normal(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The automounter's host map. Checked before any stat, which would reach
/// the other machine.
fn is_automount(path: &Path) -> bool {
    cfg!(unix)
        && path
            .components()
            .nth(1)
            .is_some_and(|component| component.as_os_str() == "net")
}

fn path_starts_with_unc(path: &Path) -> bool {
    let text = path.to_string_lossy();
    text.starts_with("//") || text.starts_with("\\\\")
}

fn is_bundle(path: &Path) -> bool {
    path.components().any(|component| {
        let name = component.as_os_str().to_string_lossy();
        let lower = name.to_ascii_lowercase();
        lower.ends_with(".app") && lower.len() > ".app".len()
    })
}

fn launches_by_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "command" | "exe" | "bat" | "cmd"
            )
        })
}

#[cfg(unix)]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.is_file() && meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_: &std::fs::Metadata) -> bool {
    false
}

/// Connection, replica and viewport a link was read in: `(identity, cols,
/// rows, scroll offset)`.
type Viewport = (Identity, u16, u16, u64);

/// Link identity for one view, read only from frames the painter admitted.
#[derive(Default)]
pub(super) struct PaintedLinks {
    painted: Option<(Identity, Arc<GridFrame>)>,
    /// The link a Command-press read, and the viewport it was painted in.
    press: Option<(String, Viewport)>,
}

impl PaintedLinks {
    /// Remember the frame just painted. A held press stays held until release
    /// so pointer motion cannot fall through into application mouse reporting;
    /// the release compares this frame with the one the press captured.
    pub(super) fn present(&mut self, stamp: Identity, frame: Arc<GridFrame>) {
        self.painted = Some((stamp, frame));
    }

    /// Drop the painted frame and any press. A replaced connection calls this
    /// so a click cannot open the previous target.
    pub(super) fn forget(&mut self) {
        self.painted = None;
        self.press = None;
    }

    pub(super) fn cancel(&mut self) {
        self.press = None;
    }

    pub(super) fn is_held(&self) -> bool {
        self.press.is_some()
    }

    /// The link under `(row, col)` on the painted frame, and its viewport. A
    /// newer, unpainted publication is not consulted.
    fn read(&self, row: u16, col: u16) -> Option<(String, Viewport)> {
        let (stamp, frame) = self.painted.as_ref()?;
        let viewport = (*stamp, frame.cols, frame.rows, frame.scrollbar.offset);
        Some((link_in(frame, row, col)?, viewport))
    }

    /// Capture the painted link under `(row, col)`.
    pub(super) fn press(&mut self, row: u16, col: u16) -> bool {
        self.press = self.read(row, col);
        self.press.is_some()
    }

    /// The URL to open when `(row, col)` is still the pressed link on the
    /// current painted frame, in the same viewport. Any other release ends
    /// the gesture.
    pub(super) fn release(&mut self, row: u16, col: u16) -> Option<String> {
        let pressed = self.press.take()?;
        (self.read(row, col).as_ref() == Some(&pressed)).then_some(pressed.0)
    }
}

#[cfg(test)]
mod tests;
