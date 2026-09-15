//! Structured, side-effect-free screen capture — the floor of the agent
//! surface (ADR-0022 §5, `phux-oki`).
//!
//! Sends the `GET_SCREEN` control command and parses the
//! `phux_core::ScreenState` the server returns. The server walks its *own*
//! `Terminal` grid, so — unlike the attach path — this neither resizes the
//! pane nor disturbs the live session. That is what makes it safe to poll
//! (the `phux wait`/`run` floor) against a pane a human or another agent
//! is actively using.
//!
//! The read shape ([`ScreenState`]) lives in `phux-core` so the server
//! (producer) and this client (consumer) share one definition; we
//! re-export it here for callers that only depend on `phux-client`.
//!
//! # Reading rows as written (ADR-0077 §2)
//!
//! The server reports libghostty's per-row soft-wrap bit in
//! [`ScreenState::soft_wrap`], and joining stays consumer-side.
//! **Every match path in this crate must match against
//! [`ScreenState::unwrapped_rows`], not against `ScreenState::lines`** — a
//! substring that straddles a soft wrap is absent from the rows as painted,
//! so matching raw rows fails silently and only for long lines, which is
//! the worst shape a bug can have. [`ScreenState::has_soft_wrap_info`]
//! reports whether the server said anything at all, so "nothing wraps" is
//! distinguishable from "older server, cannot know".
//!
//! [`row_window`] is the matching row-count clamp: the most recent `n`
//! rendered rows, `0` for all, capped at [`ROW_WINDOW_MAX`], returning
//! whether older rows were dropped (which is what
//! [`ScreenState::truncated`] carries).

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

/// Read `terminal_id`'s current screen as structured data, viewport only.
///
/// Convenience wrapper over [`get_screen_scrollback`] with no scrollback
/// requested — the poll floor used by `phux wait`/`run`.
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

/// `GET_SCREEN`'s wire byte for "no rendering requested" — today's default
/// (D9). See [`get_screen_scrollback_format`].
pub const SCREEN_FORMAT_NONE: u8 = 0;

/// `GET_SCREEN`'s wire byte (low bits) for a libghostty-vt HTML rendering
/// (D9).
pub const SCREEN_FORMAT_HTML: u8 = 1;

/// `GET_SCREEN`'s wire byte (low bits) for a libghostty-vt VT rendering
/// (D9).
pub const SCREEN_FORMAT_VT: u8 = 2;

/// `GET_SCREEN`'s wire byte high bit.
///
/// Join soft-wrapped capture rows via the engine Formatter's own unwrap
/// (`phux snapshot --format html|vt --unwrap`, D9). Combine with
/// [`SCREEN_FORMAT_HTML`]/[`SCREEN_FORMAT_VT`] (`format |
/// SCREEN_FORMAT_UNWRAP`); ignored when combined with
/// [`SCREEN_FORMAT_NONE`].
pub const SCREEN_FORMAT_UNWRAP: u8 = 0x80;

/// Read `terminal_id`'s current screen as structured data, optionally
/// including scrollback history.
///
/// Opens a fresh connection, negotiates generic L1, issues `GET_SCREEN`, and
/// deserializes the JSON reply. It never sends `ATTACH`, so the read remains
/// side-effect-free.
///
/// `request_scrollback` (`phux-o1v`): `None` for viewport only, `Some(0)`
/// for all retained history, `Some(n)` for the most-recent `n` history
/// rows. The history lands in [`ScreenState::scrollback`].
///
/// `cells` (`phux-8yl`): when `true`, the reply's [`ScreenState::cells`]
/// field carries per-cell OSC-133 semantic marks + styles; when `false`
/// it is `None`.
///
/// # Errors
///
/// Returns [`AttachError`] on connect/transport failure, when the server
/// refuses the command (e.g. unknown terminal), or when the reply is not
/// the expected `OK_WITH(JSON(..))` carrying a valid [`ScreenState`].
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

/// Like [`get_screen_scrollback`], additionally requesting a Formatter rendering.
///
/// `phux snapshot --format html|vt`, D9: `format` is `GET_SCREEN`'s wire
/// byte ([`SCREEN_FORMAT_NONE`], [`SCREEN_FORMAT_HTML`] /
/// [`SCREEN_FORMAT_VT`] optionally combined with [`SCREEN_FORMAT_UNWRAP`]);
/// when it requests a rendering, the reply's [`ScreenState::rendered`]
/// carries it.
///
/// No feature bit gates `format` (bits are scarce, and a peer that
/// predates this byte cannot advertise its absence). Instead this detects
/// the gap client-side: an `Ok` reply to a non-`SCREEN_FORMAT_NONE`
/// request with no `rendered` field is either a pre-D9 peer that silently
/// dropped the byte, or a D9-or-later peer whose render failed on its own
/// engine (non-fatal there — the plain projection still ships). Either
/// way this returns [`AttachError::FormatUnsupported`] rather than
/// success with a capture that never arrived; when the server named a
/// reason (`ScreenState::rendered_error`), the message carries it
/// verbatim.
///
/// # Errors
///
/// See [`get_screen_scrollback`]; additionally
/// [`AttachError::FormatUnsupported`] per the above.
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

/// The [`AttachError::FormatUnsupported`] message: the server's own
/// explanation when it gave one, otherwise the generic "either an old
/// peer or a failed render" statement — the two are indistinguishable
/// from the reply alone (see [`get_screen_scrollback_format`]).
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

/// One persistent control connection for a bounded screen-polling operation.
///
/// Kept crate-private so the public snapshot functions remain fresh one-shot
/// reads. A wait loop owns one of these, reuses its negotiated UDS connection,
/// and reconnects only after transport loss.
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
    // Safe to ignore the interleave: this control-only connection never
    // subscribes (no ATTACH, no ATTACH_RESOURCE, no SUBSCRIBE_EVENTS),
    // so nothing fans out onto its mailbox, and the server's
    // `handle_get_screen` is a pure projection that emits no frame of its own
    // before the ack — it does not even take the client's `out_tx`.
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

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use tokio::net::UnixListener;

    use phux_protocol::ResourceId;

    use super::{
        SCREEN_FORMAT_HTML, SCREEN_FORMAT_NONE, format_unsupported_message,
        get_screen_scrollback_format,
    };
    use crate::attach::AttachError;
    use crate::testkit::{self, ScriptSpec};

    /// A pre-D9 peer's `GET_SCREEN` decoder never reads the trailing
    /// `format` byte, so it answers `Ok` with no `rendered` key at all —
    /// exactly `ScreenState::default()`'s shape. `format != 0` against
    /// that reply must surface a typed refusal, not a silent "succeeded
    /// but rendered nothing" (D9, review item 1: no feature bit gates a
    /// byte an old peer cannot know exists).
    #[tokio::test]
    async fn old_peer_silently_dropping_format_is_detected_client_side() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("old-peer.sock");
        let listener = UnixListener::bind(&socket).expect("bind");
        let screen = phux_core::screen::ScreenState {
            lines: vec!["hi".to_owned()],
            ..phux_core::screen::ScreenState::default()
        };
        let peer = tokio::spawn(testkit::serve_every(listener, move || {
            ScriptSpec::new().screen(&screen)
        }));

        let err = get_screen_scrollback_format(
            &socket,
            ResourceId::local(1),
            None,
            false,
            SCREEN_FORMAT_HTML,
        )
        .await
        .expect_err("an old peer's silent rendered:None must be refused, not returned as success");
        assert!(
            matches!(err, AttachError::FormatUnsupported(_)),
            "expected FormatUnsupported, got {err:?}"
        );
        peer.abort();
    }

    /// `format: 0` never checks for a `rendered` reply at all, so the same
    /// old-peer-shaped reply is still an ordinary success.
    #[tokio::test]
    async fn format_none_does_not_require_a_rendered_reply() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("plain.sock");
        let listener = UnixListener::bind(&socket).expect("bind");
        let screen = phux_core::screen::ScreenState {
            lines: vec!["hi".to_owned()],
            ..phux_core::screen::ScreenState::default()
        };
        let peer = tokio::spawn(testkit::serve_every(listener, move || {
            ScriptSpec::new().screen(&screen)
        }));

        let screen = get_screen_scrollback_format(
            &socket,
            ResourceId::local(1),
            None,
            false,
            SCREEN_FORMAT_NONE,
        )
        .await
        .expect("format: 0 must succeed even against a reply with no rendered field");
        assert!(screen.rendered.is_none());
        peer.abort();
    }

    /// A reply that named why the render failed (`rendered_error`, review
    /// item 3) must reach the caller's message verbatim, distinguishing
    /// "the server tried and failed" from "the server never saw the
    /// request".
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
