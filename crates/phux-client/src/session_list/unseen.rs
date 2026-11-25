//! Unseen marks (`phux.unseen/v1`): read for a listing, and cleared by
//! `phux seen`.

use phux_core::session_list::SessionListJson;
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{FrameKind, PaneUnseen, RESOURCE_UNSEEN_KEY, Scope};
use phux_protocol::wire::info::SessionSnapshot;

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::selector::format_terminal_id;
use crate::state::Degradation;

/// First request id [`read`] reads with, clear of the parent reads.
const FIRST_REQUEST_ID: u32 = 0x6000;

/// Each local Terminal's mark, less the ones already seen. A refused or
/// unknown one reads as seen.
pub(super) async fn read(
    conn: &mut Connection,
    snapshot: &SessionSnapshot,
) -> Result<Vec<(ResourceId, PaneUnseen)>, AttachError> {
    let panes: Vec<ResourceId> = (snapshot.resources.iter())
        .filter(|r| r.kind.is_terminal() && r.id.is_local())
        .map(|r| r.id.clone())
        .collect();
    if panes.is_empty() {
        return Ok(Vec::new());
    }
    let reads = (panes.iter())
        .map(|id| (Scope::Resource(id.clone()), RESOURCE_UNSEEN_KEY.to_owned()))
        .collect();
    let (answers, _) = conn
        .request_metadata_many(FIRST_REQUEST_ID, reads)
        .await?
        .into_parts();
    Ok(panes
        .into_iter()
        .zip(answers)
        .filter_map(|(id, answer)| Some((id, PaneUnseen::decode(&answer.ok()??)?)))
        .filter(|(_, mark)| *mark != PaneUnseen::Seen)
        .collect())
}

/// Write `marks` onto the resources they name.
pub(super) fn annotate(list: &mut SessionListJson, marks: &[(ResourceId, PaneUnseen)]) {
    for (id, mark) in marks {
        let id = format_terminal_id(id);
        if let Some(row) = list.resources.iter_mut().find(|row| row.id == id) {
            row.unseen = Some(mark.as_str().to_owned());
        }
    }
}

/// Mark `pane` seen, then read it back with `request_id + 1`:
/// `DELETE_METADATA` has no reply. True when the server holds no mark, as an
/// older server never does.
///
/// # Errors
///
/// Transport and decode failures from [`Connection::send`] /
/// [`Connection::request_metadata`].
pub async fn clear(
    conn: &mut Connection,
    request_id: u32,
    pane: &ResourceId,
) -> Result<(bool, Degradation), AttachError> {
    let scope = Scope::Resource(pane.clone());
    conn.send(&FrameKind::DeleteMetadata {
        request_id,
        scope: scope.clone(),
        key: RESOURCE_UNSEEN_KEY.to_owned(),
    })
    .await?;
    let (answer, interleaved) = conn
        .request_metadata(
            request_id.wrapping_add(1),
            scope,
            RESOURCE_UNSEEN_KEY.to_owned(),
        )
        .await?
        .into_parts();
    let seen = answer.is_ok_and(|held| {
        held.and_then(|value| PaneUnseen::decode(&value))
            .is_none_or(|mark| mark == PaneUnseen::Seen)
    });
    Ok((seen, Degradation::from_interleaved(&interleaved)))
}
