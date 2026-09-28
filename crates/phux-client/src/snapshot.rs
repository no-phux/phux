//! Structured, side-effect-free screen capture (ADR-0022 §5).
//!
//! `GET_SCREEN` reads the server's own grid, so it never resizes or disturbs
//! the pane: safe to poll against a pane someone is using. Every match path
//! must match [`ScreenState::unwrapped_rows`], not raw `lines`, or a
//! substring straddling a soft wrap is silently missed (ADR-0077 §2).

use std::path::{Path, PathBuf};

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue};

pub use phux_core::screen::{
    CursorState, RENDERED_FORMAT_HTML, RENDERED_FORMAT_VT, RENDERED_SCHEMA_VERSION, ROW_WINDOW_ALL,
    ROW_WINDOW_DEFAULT, ROW_WINDOW_MAX, RenderedCell, RenderedFrame, RenderedScreen,
    SCHEMA_VERSION, ScreenState, SoftWrap, TRUNCATED_ROW_WINDOW, row_window,
};

use crate::attach::AttachError;
use crate::attach::connection::Connection;

/// Read `terminal_id`'s current screen, viewport only.
///
/// # Errors
///
/// See [`get_screen_scrollback`].
pub async fn get_screen(
    socket: &Path,
    terminal_id: ResourceId,
) -> Result<ScreenState, AttachError> {
    get_screen_scrollback(socket, terminal_id, None, false).await
}

/// `GET_SCREEN`'s wire byte for "no rendering requested" (D9).
pub const SCREEN_FORMAT_NONE: u8 = 0;

/// `GET_SCREEN`'s wire byte (low bits) for a libghostty-vt HTML rendering
/// (D9).
pub const SCREEN_FORMAT_HTML: u8 = 1;

/// `GET_SCREEN`'s wire byte (low bits) for a libghostty-vt VT rendering
/// (D9).
pub const SCREEN_FORMAT_VT: u8 = 2;

/// `GET_SCREEN`'s wire byte high bit: join soft-wrapped rows in the rendered
/// capture (combine with HTML/VT; ignored with NONE).
pub const SCREEN_FORMAT_UNWRAP: u8 = 0x80;

/// Read `terminal_id`'s current screen over a fresh connection, optionally
/// with scrollback.
///
/// `request_scrollback`: `None` viewport only, `Some(0)` all history,
/// `Some(n)` the last `n` rows. `cells` adds per-cell OSC-133 marks and
/// styles.
///
/// # Errors
///
/// Transport failure, a refusal (e.g. unknown terminal), or a malformed
/// reply.
pub async fn get_screen_scrollback(
    socket: &Path,
    terminal_id: ResourceId,
    request_scrollback: Option<u32>,
    cells: bool,
) -> Result<ScreenState, AttachError> {
    get_screen_scrollback_format(
        socket,
        terminal_id,
        request_scrollback,
        cells,
        SCREEN_FORMAT_NONE,
    )
    .await
}

/// Like [`get_screen_scrollback`], additionally requesting a Formatter
/// rendering (`format`, D9) into [`ScreenState::rendered`].
///
/// No feature bit gates `format`, so a reply with no `rendered` (an old peer
/// dropped the byte, or the render failed) is [`AttachError::FormatUnsupported`],
/// carrying the server's `rendered_error` when it gave one.
///
/// # Errors
///
/// See [`get_screen_scrollback`], plus [`AttachError::FormatUnsupported`].
pub async fn get_screen_scrollback_format(
    socket: &Path,
    terminal_id: ResourceId,
    request_scrollback: Option<u32>,
    cells: bool,
    format: u8,
) -> Result<ScreenState, AttachError> {
    let screen = ScreenPollConnection::new(socket)
        .read(terminal_id, request_scrollback, cells, format)
        .await?;
    if format != SCREEN_FORMAT_NONE && screen.rendered.is_none() {
        return Err(AttachError::FormatUnsupported(format_unsupported_message(
            screen.rendered_error.as_deref(),
        )));
    }
    Ok(screen)
}

/// The [`AttachError::FormatUnsupported`] message: the server's reason, or
/// the generic "old peer or failed render".
fn format_unsupported_message(rendered_error: Option<&str>) -> String {
    rendered_error.map_or_else(
        || {
            "the server returned no rendered capture for --format; it may predate --format \
             support (upgrade it), or the render may have failed silently"
                .to_owned()
        },
        |detail| format!("the server could not render the requested --format capture: {detail}"),
    )
}

/// One persistent control connection for a screen-polling loop, reconnecting
/// once after a transport loss.
pub(crate) struct ScreenPollConnection {
    socket: PathBuf,
    conn: Option<Connection>,
    next_request_id: u32,
}

impl ScreenPollConnection {
    pub(crate) fn new(socket: &Path) -> Self {
        Self {
            socket: socket.to_path_buf(),
            conn: None,
            next_request_id: 1,
        }
    }

    pub(crate) async fn read(
        &mut self,
        terminal_id: ResourceId,
        request_scrollback: Option<u32>,
        cells: bool,
        format: u8,
    ) -> Result<ScreenState, AttachError> {
        let mut recovered = false;
        loop {
            if self.conn.is_none() {
                self.conn = Some(Connection::connect(&self.socket).await?);
            }
            let request_id = self.take_request_id();
            let Some(conn) = self.conn.as_mut() else {
                return Err(AttachError::Protocol(
                    "screen poll connection was not established".to_owned(),
                ));
            };
            let result = conn
                .request(
                    request_id,
                    Command::GetScreen {
                        terminal_id: terminal_id.clone(),
                        request_scrollback,
                        cells,
                        format,
                    },
                )
                .await;
            match result {
                Ok(reply) => return decode_screen_reply(reply.into_result_ignoring_interleaved()),
                Err(error) if !recovered && is_transport_loss(&error) => {
                    self.conn = None;
                    recovered = true;
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn take_request_id(&mut self) -> u32 {
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1).max(1);
        request_id
    }
}

const fn is_transport_loss(error: &AttachError) -> bool {
    matches!(
        error,
        AttachError::Io(_)
            | AttachError::Connect(_)
            | AttachError::Unreachable(_)
            | AttachError::Disconnected
    )
}

fn decode_screen_reply(result: CommandResult) -> Result<ScreenState, AttachError> {
    // The interleave was safely ignored: this connection never subscribes.
    match result {
        CommandResult::OkWith(CommandValue::Json(json)) => serde_json::from_str(&json)
            .map_err(|err| AttachError::Protocol(format!("malformed GET_SCREEN JSON: {err}"))),
        CommandResult::Error { message, .. } => Err(AttachError::Refused(message)),
        other => Err(AttachError::Protocol(crate::explain::explain_unexpected(
            "GET_SCREEN",
            &other,
        ))),
    }
}

/// The history window a read asks the server for (ADR-0077): an explicit
/// `scrollback` wins; otherwise `tail N` asks for `N` rows (a superset of
/// the window) and [`project`] clamps.
#[must_use]
pub const fn history_request(scrollback: Option<u32>, tail: Option<u32>) -> Option<u32> {
    match scrollback {
        Some(rows) => Some(rows),
        None => tail,
    }
}

/// Apply the ADR-0077 read modifiers (`--unwrap`, then `--tail`) to a
/// `GET_SCREEN` reply, for both the CLI and MCP.
///
/// Unwrap first: it changes the row count the window counts.
#[must_use]
pub fn project(mut screen: ScreenState, unwrap: bool, tail: Option<u32>) -> ScreenState {
    if unwrap {
        let (history, viewport) = screen.unwrapped_split();
        screen.scrollback = history;
        screen.lines = viewport;
        // Present-and-empty is "reported, none"; `None` would be "unknown".
        screen.soft_wrap = Some(SoftWrap::default());
    }
    match tail {
        Some(want) => window_history(screen, want),
        None => screen,
    }
}

/// Clip `screen`'s history to a `want`-row window over history plus
/// viewport. The viewport (a grid) is never returned in part, so it is a
/// floor.
fn window_history(mut screen: ScreenState, want: u32) -> ScreenState {
    let ceiling = usize::try_from(ROW_WINDOW_MAX).unwrap_or(usize::MAX);
    let want = if want == ROW_WINDOW_ALL {
        ceiling
    } else {
        usize::try_from(want).unwrap_or(usize::MAX).min(ceiling)
    };
    let keep = want.saturating_sub(screen.lines.len());
    let before = screen.scrollback.len();
    let clipped = if keep == 0 {
        screen.scrollback.clear();
        before > 0
    } else {
        let (kept, clipped) = row_window(
            std::mem::take(&mut screen.scrollback),
            u32::try_from(keep).unwrap_or(u32::MAX),
        );
        screen.scrollback = kept;
        clipped
    };

    // History wrap indices are relative to the returned array: shift them.
    let dropped = u32::try_from(before - screen.scrollback.len()).unwrap_or(u32::MAX);
    if dropped > 0
        && let Some(wrap) = screen.soft_wrap.as_mut()
    {
        wrap.scrollback = wrap
            .scrollback
            .iter()
            .filter_map(|index| index.checked_sub(dropped))
            .collect();
    }

    if clipped {
        screen.truncated = true;
        screen.truncated_reason = Some(TRUNCATED_ROW_WINDOW.to_owned());
    }
    screen
}

/// The ceiling on a projected snapshot document: 1 MiB of pretty JSON.
pub const PROJECTED_DOCUMENT_MAX_BYTES: usize = 1024 * 1024;

/// Why [`bounded_document`] refused a screen.
#[derive(Debug, thiserror::Error)]
pub enum SnapshotDocumentError {
    /// The pretty-printed document is over the caller's ceiling.
    #[error(
        "the snapshot document is {size} bytes, over the {limit}-byte ceiling; \
         request fewer rows with `tail` or a smaller `scrollback`"
    )]
    TooLarge {
        /// The document's size, counting the trailing newline.
        size: usize,
        /// The ceiling it exceeded.
        limit: usize,
    },
    /// The screen did not serialize.
    #[error("failed to serialize screen: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// `screen` as a JSON document, refused when its pretty-printed form (plus
/// the trailing newline a CLI prints) would exceed `limit` bytes.
///
/// # Errors
///
/// [`SnapshotDocumentError::TooLarge`] over the ceiling, or
/// [`SnapshotDocumentError::Serialize`].
pub fn bounded_document(
    screen: &ScreenState,
    limit: usize,
) -> Result<serde_json::Value, SnapshotDocumentError> {
    let size = serde_json::to_string_pretty(screen)?
        .len()
        .saturating_add(1);
    if size > limit {
        return Err(SnapshotDocumentError::TooLarge { size, limit });
    }
    Ok(serde_json::to_value(screen)?)
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use tokio::net::UnixListener;

    use phux_protocol::ResourceId;

    use super::{
        PROJECTED_DOCUMENT_MAX_BYTES, ROW_WINDOW_ALL, SCREEN_FORMAT_HTML, SCREEN_FORMAT_NONE,
        ScreenState, SnapshotDocumentError, SoftWrap, TRUNCATED_ROW_WINDOW, bounded_document,
        format_unsupported_message, get_screen_scrollback_format, history_request, project,
    };

    fn screen(scrollback: &[&str], lines: &[&str], wrapped_lines: &[u32]) -> ScreenState {
        ScreenState {
            pane: 1,
            cols: 10,
            rows: u16::try_from(lines.len()).unwrap_or(0),
            lines: lines.iter().map(|s| (*s).to_owned()).collect(),
            scrollback: scrollback.iter().map(|s| (*s).to_owned()).collect(),
            soft_wrap: Some(SoftWrap {
                lines: wrapped_lines.to_vec(),
                scrollback: Vec::new(),
            }),
            ..ScreenState::default()
        }
    }

    /// `tail N` asks the server for `N` history rows; an explicit
    /// `scrollback` wins over it.
    #[test]
    fn history_request_prefers_an_explicit_scrollback() {
        assert_eq!(history_request(None, None), None);
        assert_eq!(history_request(None, Some(80)), Some(80));
        assert_eq!(history_request(None, Some(0)), Some(0));
        assert_eq!(history_request(Some(5), Some(80)), Some(5));
    }

    /// `unwrap` joins wrapped rows and reports that nothing in the returned
    /// projection continues — `Some(empty)`, never `None`.
    #[test]
    fn unwrap_joins_rows_and_keeps_reporting_wrap_info() {
        let out = project(screen(&[], &["the quick", "brown fox"], &[0]), true, None);
        assert_eq!(out.lines, vec!["the quickbrown fox".to_owned()]);
        assert_eq!(out.soft_wrap, Some(SoftWrap::default()));
        assert!(
            out.has_soft_wrap_info(),
            "an unwrapped projection still reported wrap info",
        );
        assert!(!out.truncated);
    }

    /// The row window counts the viewport and clips only history, and it
    /// says so.
    #[test]
    fn tail_clips_history_and_reports_truncation() {
        let base = screen(&["h1", "h2", "h3"], &["v1", "v2"], &[]);

        let out = project(base.clone(), false, Some(4));
        assert_eq!(
            out.scrollback,
            vec!["h2".to_owned(), "h3".to_owned()],
            "a window of 4 is 2 viewport rows + the 2 most-recent history rows",
        );
        assert_eq!(out.lines, vec!["v1".to_owned(), "v2".to_owned()]);
        assert!(out.truncated);
        assert_eq!(out.truncated_reason.as_deref(), Some(TRUNCATED_ROW_WINDOW));

        let out = project(base.clone(), false, Some(5));
        assert_eq!(out.scrollback.len(), 3, "a window that fits clips nothing");
        assert!(!out.truncated);
        assert!(out.truncated_reason.is_none());

        let out = project(base.clone(), false, Some(ROW_WINDOW_ALL));
        assert_eq!(out.scrollback.len(), 3);
        assert!(!out.truncated);

        // A window narrower than the viewport: the grid is a floor, so the
        // viewport survives whole and only history goes.
        let out = project(base, false, Some(1));
        assert!(out.scrollback.is_empty());
        assert_eq!(out.lines.len(), 2, "the viewport is never returned in part");
        assert!(out.truncated);
    }

    /// Clipping history shifts the history wrap indices, which are relative
    /// to the returned array.
    #[test]
    fn tail_shifts_history_wrap_indices() {
        let mut base = screen(&["h1", "h2", "h3"], &["v1"], &[]);
        base.soft_wrap = Some(SoftWrap {
            lines: Vec::new(),
            scrollback: vec![0, 2],
        });
        let out = project(base, false, Some(3));
        assert_eq!(out.scrollback, vec!["h2".to_owned(), "h3".to_owned()]);
        let wrap = out.soft_wrap.expect("wrap info survives the window");
        assert_eq!(
            wrap.scrollback,
            vec![1],
            "index 2 became 1; index 0 pointed at a dropped row and went away",
        );
    }

    /// The document bound measures the pretty-printed bytes a CLI would have
    /// printed, and refuses over the ceiling instead of truncating.
    #[test]
    fn bounded_document_refuses_over_the_ceiling() {
        let small = screen(&[], &["v1"], &[]);
        let value = bounded_document(&small, PROJECTED_DOCUMENT_MAX_BYTES).expect("fits");
        assert_eq!(value, serde_json::to_value(&small).expect("value"));

        let exact = serde_json::to_string_pretty(&small).expect("pretty").len() + 1;
        assert!(
            bounded_document(&small, exact).is_ok(),
            "the ceiling is inclusive"
        );
        let refused = bounded_document(&small, exact - 1).expect_err("one byte over");
        assert!(
            matches!(refused, SnapshotDocumentError::TooLarge { size, limit } if size == exact && limit == exact - 1),
            "{refused:?}"
        );
    }
    use crate::attach::AttachError;
    use crate::testkit::{self, ScriptSpec};

    /// An old peer answers `GET_SCREEN` with no `rendered` key: a format
    /// request against it is refused, a plain read still succeeds.
    #[tokio::test]
    async fn a_missing_rendered_capture_is_refused_only_when_one_was_asked_for() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("old-peer.sock");
        let listener = UnixListener::bind(&socket).expect("bind");
        let screen = ScreenState {
            lines: vec!["hi".to_owned()],
            ..ScreenState::default()
        };
        let peer = tokio::spawn(testkit::serve_every(listener, move || {
            ScriptSpec::new().screen(&screen)
        }));
        let read = |format| {
            get_screen_scrollback_format(&socket, ResourceId::local(1), None, false, format)
        };

        let err = read(SCREEN_FORMAT_HTML).await.expect_err("refused");
        assert!(
            matches!(err, AttachError::FormatUnsupported(_)),
            "expected FormatUnsupported, got {err:?}"
        );
        let plain = read(SCREEN_FORMAT_NONE)
            .await
            .expect("a plain read succeeds");
        assert!(plain.rendered.is_none());
        peer.abort();
    }

    /// The server's own render failure reason reaches the message verbatim.
    #[test]
    fn format_unsupported_message_prefers_the_servers_own_reason() {
        let generic = format_unsupported_message(None);
        assert!(generic.contains("upgrade"), "got: {generic}");

        let specific = format_unsupported_message(Some("libghostty: out of memory"));
        assert!(
            specific.contains("libghostty: out of memory"),
            "got: {specific}"
        );
        assert_ne!(generic, specific);
    }
}
