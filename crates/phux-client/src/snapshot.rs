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

use std::path::Path;

use phux_protocol::ids::TerminalId;
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue};

pub use phux_core::screen::{
    CursorState, RENDERED_SCHEMA_VERSION, RenderedCell, RenderedFrame, SCHEMA_VERSION, ScreenState,
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
    terminal_id: TerminalId,
) -> Result<ScreenState, AttachError> {
    get_screen_scrollback(socket, terminal_id, None, false).await
}

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
    terminal_id: TerminalId,
    request_scrollback: Option<u32>,
    cells: bool,
) -> Result<ScreenState, AttachError> {
    let mut conn = Connection::connect(socket).await?;
    // Safe to ignore the interleave: this connection is freshly opened and
    // never subscribes (no ATTACH, no ATTACH_TERMINAL, no SUBSCRIBE_EVENTS),
    // so nothing fans out onto its mailbox, and the server's
    // `handle_get_screen` is a pure projection that emits no frame of its own
    // before the ack — it does not even take the client's `out_tx`.
    let result = conn
        .request(
            1,
            Command::GetScreen {
                terminal_id,
                request_scrollback,
                cells,
            },
        )
        .await?
        .into_result_ignoring_interleaved();
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
