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

/// Schemes a click may hand to the system, besides a `file://` URL that
/// [`admit_url`] still has to accept.
const OPENABLE: &[&str] = &["https://", "http://", "mailto:", "ssh://", "ftp://"];

/// How many symlinks one click will follow. Linux's `MAXSYMLINKS`.
const MAX_SYMLINKS: usize = 40;

/// Title when a click does not open.
pub(crate) const LINK_REFUSED: &str = "Link not opened";

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
    if OPENABLE.iter().any(|scheme| url.starts_with(scheme)) {
        return LinkAdmit::Open;
    }
    let Some((authority, path)) = file_url(url) else {
        return refuse("This link was not opened.");
    };
    if let crate::attach::TerminalHost::Remote { label } = host {
        return refuse(&remote_file_body(label.as_deref()));
    }
    if !local_authority(&authority) {
        return refuse("This file link names another machine, so it was not opened.");
    }
    let Some(path) = decoded_path(&path) else {
        return refuse("This file link was not opened.");
    };
    admit_launch(Path::new(&path))
}

/// Whether a path this app is about to hand to the default application may
/// be opened. Bundles, executables, and nonlocal paths are refused.
/// [`HostAction::Open`](crate::terminal::HostAction::Open)'s only caller writes
/// a mode `0o600` `.txt` export under the temp directory; this check is what
/// keeps a different path from launching.
#[must_use]
pub(crate) fn admit_path(path: &Path) -> LinkAdmit {
    admit_launch(path)
}

fn admit_launch(path: &Path) -> LinkAdmit {
    match launch_block(path) {
        LaunchBlock::Open => LinkAdmit::Open,
        LaunchBlock::Remote => {
            refuse("This file link names another machine, so it was not opened.")
        }
        LaunchBlock::Program => refuse("This file is an app or a program, so it was not opened."),
    }
}

fn refuse(body: &str) -> LinkAdmit {
    LinkAdmit::Refuse {
        title: LINK_REFUSED,
        body: body.to_owned(),
    }
}

fn remote_file_body(label: Option<&str>) -> String {
    match label {
        Some(name) => format!("This file link came from {name}. It was not opened on this Mac."),
        None => "This file link came from another machine. It was not opened on this Mac.".into(),
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

fn local_authority(authority: &str) -> bool {
    authority.is_empty() || authority.eq_ignore_ascii_case("localhost")
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

enum LaunchBlock {
    Open,
    /// The path names another machine (`//server`, `/net`, a symlink there).
    Remote,
    /// An app bundle, a known launcher extension, an executable, or a loop.
    Program,
}

fn launch_block(path: &Path) -> LaunchBlock {
    // `//server/share` is a UNC path. Lexical normalization collapses the
    // extra slash, so this has to be read from the path as given.
    if path_starts_with_unc(path) {
        return LaunchBlock::Remote;
    }
    let mut current = lexical_normal(path);
    for _ in 0..MAX_SYMLINKS {
        if is_automount(&current) {
            return LaunchBlock::Remote;
        }
        if is_bundle(&current) || launches_by_extension(&current) {
            return LaunchBlock::Program;
        }
        let Ok(meta) = std::fs::symlink_metadata(&current) else {
            return LaunchBlock::Open;
        };
        if meta.file_type().is_symlink() {
            let Ok(target) = std::fs::read_link(&current) else {
                return LaunchBlock::Program;
            };
            if path_starts_with_unc(&target) {
                return LaunchBlock::Remote;
            }
            current = if target.is_absolute() {
                lexical_normal(&target)
            } else {
                match current.parent() {
                    Some(parent) => lexical_normal(&parent.join(target)),
                    None => return LaunchBlock::Program,
                }
            };
            continue;
        }
        return if meta.is_file() && is_executable(&meta) {
            LaunchBlock::Program
        } else {
            LaunchBlock::Open
        };
    }
    LaunchBlock::Program
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

/// Connection and replica the painted frame was admitted under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FrameStamp {
    epoch: u64,
    stream: u64,
    bootstrap: u64,
}

impl FrameStamp {
    pub(super) fn new(epoch: u64, frame: &GridFrame) -> Self {
        Self {
            epoch,
            stream: frame.stream_id,
            bootstrap: frame.bootstrap_id,
        }
    }
}

/// The link a Command-press read, and the viewport it was painted in.
#[derive(Clone, Debug, PartialEq, Eq)]
struct CapturedLink {
    url: String,
    stamp: FrameStamp,
    cols: u16,
    rows: u16,
    offset: u64,
}

impl CapturedLink {
    fn same_view(&self, stamp: FrameStamp, frame: &GridFrame) -> bool {
        self.stamp == stamp
            && self.cols == frame.cols
            && self.rows == frame.rows
            && self.offset == frame.scrollbar.offset
    }
}

/// Link identity for one view, read only from frames the painter admitted.
#[derive(Default)]
pub(super) struct PaintedLinks {
    frame: Option<Arc<GridFrame>>,
    stamp: Option<FrameStamp>,
    press: Option<CapturedLink>,
}

impl PaintedLinks {
    /// Remember the frame just painted. A held press stays held until release
    /// so pointer motion cannot fall through into application mouse reporting;
    /// the release compares this frame with the one the press captured.
    pub(super) fn present(&mut self, stamp: FrameStamp, frame: Arc<GridFrame>) {
        self.stamp = Some(stamp);
        self.frame = Some(frame);
    }

    /// Drop the painted frame and any press. A replaced connection calls this
    /// so a click cannot open the previous target.
    pub(super) fn forget(&mut self) {
        self.frame = None;
        self.stamp = None;
        self.press = None;
    }

    pub(super) fn cancel(&mut self) {
        self.press = None;
    }

    pub(super) fn is_held(&self) -> bool {
        self.press.is_some()
    }

    /// The link on the painted frame. A newer, unpainted publication is not
    /// consulted.
    pub(super) fn link_at(&self, row: u16, col: u16) -> Option<String> {
        link_in(self.frame.as_ref()?, row, col)
    }

    /// Capture the painted link under `(row, col)`.
    pub(super) fn press(&mut self, row: u16, col: u16) -> bool {
        self.press = None;
        let (Some(frame), Some(stamp)) = (self.frame.as_ref(), self.stamp) else {
            return false;
        };
        let Some(url) = link_in(frame, row, col) else {
            return false;
        };
        self.press = Some(CapturedLink {
            url,
            stamp,
            cols: frame.cols,
            rows: frame.rows,
            offset: frame.scrollbar.offset,
        });
        true
    }

    /// The URL to open when `(row, col)` is still the pressed link on the
    /// current painted frame. Any other release ends the gesture.
    pub(super) fn release(&mut self, row: u16, col: u16) -> Option<String> {
        let pressed = self.press.take()?;
        self.matches(&pressed, row, col).then_some(pressed.url)
    }

    fn matches(&self, pressed: &CapturedLink, row: u16, col: u16) -> bool {
        let (Some(frame), Some(stamp)) = (self.frame.as_ref(), self.stamp) else {
            return false;
        };
        pressed.same_view(stamp, frame) && link_in(frame, row, col).as_ref() == Some(&pressed.url)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FrameStamp, LINK_REFUSED, LinkAdmit, PaintedLinks, admit_path, admit_url, allowed, link_in,
        url_around,
    };
    use phux_client_core::grid::{Cell, CellMetadata, GridBuffer, GridDamage};
    use phux_client_runtime::publication::{Cursor, FrameColors, GridFrame, Scrollbar};
    use std::sync::Arc;

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

    #[test]
    fn an_unpainted_url_never_opens_instead_of_the_pressed_link() {
        let painted = text_frame(&["https://a.example"], 1, 0);
        let unseen = text_frame(&["https://b.example"], 9, 0);
        let mut links = hold(&painted);
        assert!(links.press(0, 4));
        assert_eq!(link_in(&unseen, 0, 4).as_deref(), Some("https://b.example"));
        assert_eq!(
            links.release(0, 4).as_deref(),
            Some("https://a.example"),
            "the gesture has no path to a publication that was not painted"
        );
    }

    #[test]
    fn an_osc8_replacement_under_the_cursor_does_not_open() {
        let pressed = osc8_frame("https://a.example", 1);
        let replaced = osc8_frame("https://b.example", 2);
        let mut links = hold(&pressed);
        assert!(links.press(0, 0));
        links.present(FrameStamp::new(1, &replaced), replaced.clone());
        assert_eq!(
            link_in(&replaced, 0, 0).as_deref(),
            Some("https://b.example")
        );
        assert_eq!(links.release(0, 0), None);
    }

    #[test]
    fn unrelated_output_keeps_the_pressed_link() {
        let painted = text_frame(&["https://a.example | ", "                    "], 1, 0);
        let spun = text_frame(&["https://a.example / ", "https://b.example   "], 2, 0);
        let mut links = hold(&painted);
        assert!(links.press(0, 4));
        links.present(FrameStamp::new(1, &spun), spun.clone());
        assert_eq!(link_in(&spun, 1, 0).as_deref(), Some("https://b.example"));
        assert_eq!(links.release(0, 10).as_deref(), Some("https://a.example"));
    }

    #[test]
    fn dragging_off_the_link_cancels_it() {
        let painted = text_frame(&["https://a.example       "], 1, 0);
        let mut links = hold(&painted);
        assert!(links.press(0, 2));
        assert_eq!(
            links.release(0, 8).as_deref(),
            Some("https://a.example"),
            "a release still on the link completes the click"
        );
        let mut links = hold(&painted);
        assert!(links.press(0, 2));
        assert_eq!(links.release(0, 20), None);
    }

    #[test]
    fn a_viewport_change_cancels_the_pressed_link() {
        let top = text_frame(&["https://a.example"], 1, 0);
        let scrolled = text_frame(&["https://a.example"], 2, 4);
        let mut links = hold(&top);
        assert!(links.press(0, 4));
        links.present(FrameStamp::new(1, &scrolled), scrolled);
        assert_eq!(links.release(0, 4), None);

        let resized = text_frame(&["https://a.example!!"], 3, 0);
        let mut links = hold(&top);
        assert!(links.press(0, 4));
        links.present(FrameStamp::new(1, &resized), resized);
        assert_eq!(links.release(0, 4), None);
    }

    #[test]
    fn reconnect_or_forget_cancels_the_pressed_link() {
        let painted = text_frame(&["https://a.example"], 1, 0);
        let mut links = hold(&painted);
        assert!(links.press(0, 4));
        let replaced = text_frame_at(&["https://a.example"], 9, 0, 4);
        links.present(FrameStamp::new(1, &replaced), replaced);
        assert_eq!(links.release(0, 4), None);

        let mut links = hold(&painted);
        assert!(links.press(0, 4));
        links.present(FrameStamp::new(2, &painted), Arc::clone(&painted));
        assert_eq!(links.release(0, 4), None);

        let mut links = hold(&painted);
        assert!(links.press(0, 4));
        links.cancel();
        assert_eq!(links.release(0, 4), None);

        let mut links = hold(&painted);
        assert!(links.press(0, 4));
        links.forget();
        assert_eq!(links.release(0, 4), None);
        assert_eq!(links.link_at(0, 4), None);
    }

    #[test]
    fn rejected_schemes_are_not_a_press_and_file_links_still_open() {
        let script = osc8_frame("javascript:alert(1)", 1);
        let mut links = hold(&script);
        assert!(!links.press(0, 0));
        assert_eq!(links.release(0, 0), None);

        let file = text_frame(&["file:///tmp/a"], 1, 0);
        let mut links = hold(&file);
        assert!(links.press(0, 1));
        assert_eq!(links.release(0, 6).as_deref(), Some("file:///tmp/a"));
    }

    #[test]
    fn web_links_open_and_file_links_follow_the_host() {
        let remote = crate::attach::TerminalHost::Remote {
            label: Some("mini".to_owned()),
        };
        assert!(matches!(
            admit_url("https://phux.dev/docs", &remote),
            LinkAdmit::Open
        ));
        assert!(matches!(
            admit_url("file:///tmp/notes.txt", &remote),
            LinkAdmit::Refuse {
                title: LINK_REFUSED,
                ..
            }
        ));
        match admit_url(
            "file://mini.local/tmp/notes.txt",
            &crate::attach::TerminalHost::Local,
        ) {
            LinkAdmit::Refuse { body, .. } => assert!(body.contains("another machine"), "{body}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert!(matches!(
            admit_url(
                "file://localhost/tmp/phux-link-does-not-exist.txt",
                &crate::attach::TerminalHost::Local
            ),
            LinkAdmit::Open
        ));
        assert!(matches!(
            admit_url(
                "file:///Applications/Safari.app",
                &crate::attach::TerminalHost::Local
            ),
            LinkAdmit::Refuse { .. }
        ));
        assert!(matches!(
            admit_url(
                "file:///Applications/Safari.app/../notes.txt",
                &crate::attach::TerminalHost::Local
            ),
            LinkAdmit::Open
        ));
        assert!(matches!(
            admit_url("file:///tmp/Foo%2Eapp", &crate::attach::TerminalHost::Local),
            LinkAdmit::Refuse { .. }
        ));
        assert!(matches!(
            admit_url(
                "file:////server/share/a.txt",
                &crate::attach::TerminalHost::Local
            ),
            LinkAdmit::Refuse { .. }
        ));
        assert!(matches!(
            admit_url("javascript:alert(1)", &crate::attach::TerminalHost::Local),
            LinkAdmit::Refuse { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn programs_bundles_and_automounts_are_not_opened() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let dir = std::env::temp_dir().join(format!("phux-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let doc = dir.join("notes.txt");
        std::fs::write(&doc, b"hi").unwrap();
        let program = dir.join("run");
        std::fs::write(&program, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let bundle = dir.join("Tool.app");
        std::fs::create_dir_all(bundle.join("Contents")).unwrap();
        let alias = dir.join("alias.txt");
        symlink(&bundle, &alias).unwrap();
        let share = dir.join("share");
        symlink("/net/elsewhere/share/notes.txt", &share).unwrap();
        let looped = dir.join("loop");
        symlink("loop", &looped).unwrap();
        let command = dir.join("build.command");
        std::fs::write(&command, b"echo hi\n").unwrap();

        let local = crate::attach::TerminalHost::Local;
        assert!(matches!(
            admit_url(&file_url_of(&doc), &local),
            LinkAdmit::Open
        ));
        assert!(matches!(admit_path(&doc), LinkAdmit::Open));
        assert!(matches!(
            admit_url(&file_url_of(&program), &local),
            LinkAdmit::Refuse { .. }
        ));
        assert!(matches!(admit_path(&program), LinkAdmit::Refuse { .. }));
        assert!(matches!(
            admit_url(&file_url_of(&bundle), &local),
            LinkAdmit::Refuse { .. }
        ));
        assert!(matches!(
            admit_url(&file_url_of(&alias), &local),
            LinkAdmit::Refuse { .. }
        ));
        assert!(matches!(
            admit_url(&file_url_of(&share), &local),
            LinkAdmit::Refuse { .. }
        ));
        assert!(matches!(
            admit_url(&file_url_of(&looped), &local),
            LinkAdmit::Refuse { .. }
        ));
        assert!(matches!(
            admit_url(&file_url_of(&command), &local),
            LinkAdmit::Refuse { .. }
        ));
        assert!(matches!(
            admit_url("file:///net/other/share/notes.txt", &local),
            LinkAdmit::Refuse { .. }
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    fn file_url_of(path: &std::path::Path) -> String {
        format!("file://{}", path.display())
    }

    fn hold(frame: &Arc<GridFrame>) -> PaintedLinks {
        let mut links = PaintedLinks::default();
        links.present(FrameStamp::new(1, frame), Arc::clone(frame));
        links
    }

    fn text_frame(lines: &[&str], seq: u64, offset: u64) -> Arc<GridFrame> {
        text_frame_at(lines, seq, offset, 3)
    }

    fn text_frame_at(lines: &[&str], seq: u64, offset: u64, bootstrap: u64) -> Arc<GridFrame> {
        let cols = u16::try_from(lines.iter().map(|line| line.len()).max().unwrap_or(0)).unwrap();
        let rows = u16::try_from(lines.len()).unwrap();
        let mut utf8 = Vec::new();
        let mut cells = Vec::new();
        for line in lines {
            let bytes = line.as_bytes();
            for col in 0..cols {
                let byte = bytes.get(usize::from(col)).copied().unwrap_or(b' ');
                let start = u32::try_from(utf8.len()).unwrap();
                utf8.push(byte);
                cells.push(Cell {
                    utf8_offset: start,
                    utf8_len: 1,
                    ..Cell::default()
                });
            }
        }
        Arc::new(grid(cols, rows, seq, bootstrap, cells, utf8, offset))
    }

    fn osc8_frame(uri: &str, seq: u64) -> Arc<GridFrame> {
        let mut utf8 = b"docs".to_vec();
        let link_at = u32::try_from(utf8.len()).unwrap();
        utf8.extend_from_slice(uri.as_bytes());
        let linked = Cell {
            utf8_offset: 0,
            utf8_len: 4,
            hyperlink_offset: link_at,
            hyperlink_len: u32::try_from(uri.len()).unwrap(),
            ..Cell::default()
        };
        Arc::new(grid(2, 1, seq, 3, vec![linked, Cell::default()], utf8, 0))
    }

    fn grid(
        cols: u16,
        rows: u16,
        seq: u64,
        bootstrap: u64,
        cells: Vec<Cell>,
        utf8: Vec<u8>,
        offset: u64,
    ) -> GridFrame {
        GridFrame {
            terminal_id: phux_protocol::ResourceId::local(1),
            generation: seq,
            stream_id: 7,
            bootstrap_id: bootstrap,
            last_seq: seq,
            cols,
            rows,
            cursor: Cursor::default(),
            scrollbar: Scrollbar {
                total: 40,
                offset,
                len: u64::from(rows),
            },
            colors: FrameColors::default(),
            damage: GridDamage::Full,
            buffer: GridBuffer {
                cells,
                utf8,
                metadata: vec![CellMetadata::default(); usize::from(cols) * usize::from(rows)],
                row_dirty: vec![true; usize::from(rows)],
                ..GridBuffer::default()
            },
        }
    }
}
