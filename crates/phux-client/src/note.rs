//! The wire round trips behind `phux notify` and `phux notify-mode`: write a
//! pane's `phux.note/v1` record (its type lives in `phux_client_core::note`)
//! or its `phux.notify/v1` mode.

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{
    FrameKind, PaneNotify, RESOURCE_NOTE_KEY, RESOURCE_NOTIFY_KEY, Scope,
};

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::state::Degradation;

/// Write `value` as `pane`'s note, then read it back with `request_id + 1`:
/// `SET_METADATA` has no reply, so the read proves the write landed. True
/// when the server holds exactly `value`.
///
/// # Errors
///
/// Transport and decode failures from [`Connection::send`] /
/// [`Connection::request_metadata`].
pub async fn post(
    conn: &mut Connection,
    request_id: u32,
    pane: &ResourceId,
    value: Vec<u8>,
) -> Result<(bool, Degradation), AttachError> {
    write(conn, request_id, pane, RESOURCE_NOTE_KEY, value).await
}

/// Set `pane`'s notify mode, confirmed the same way as [`post`].
///
/// # Errors
///
/// As [`post`].
pub async fn set_mode(
    conn: &mut Connection,
    request_id: u32,
    pane: &ResourceId,
    mode: PaneNotify,
) -> Result<(bool, Degradation), AttachError> {
    let value = mode.as_str().as_bytes().to_vec();
    write(conn, request_id, pane, RESOURCE_NOTIFY_KEY, value).await
}

async fn write(
    conn: &mut Connection,
    request_id: u32,
    pane: &ResourceId,
    key: &str,
    value: Vec<u8>,
) -> Result<(bool, Degradation), AttachError> {
    let scope = Scope::Resource(pane.clone());
    conn.send(&FrameKind::SetMetadata {
        request_id,
        scope: scope.clone(),
        key: key.to_owned(),
        value: value.clone(),
    })
    .await?;
    let (answer, interleaved) = conn
        .request_metadata(request_id.wrapping_add(1), scope, key.to_owned())
        .await?
        .into_parts();
    let held = answer.ok().flatten().is_some_and(|held| held == value);
    Ok((held, Degradation::from_interleaved(&interleaved)))
}
