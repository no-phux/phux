//! `phux tag` — read and write a Terminal's L3 tags (`phux-f8wi`, ADR-0027).
//!
//! Tags are freeform strings stored as L3 metadata under the conventional
//! key [`RESOURCE_TAGS_KEY`] (`phux.tags/v1`), scoped to a `ResourceId`. The
//! value is a UTF-8 JSON array of tag strings; the server stores the bytes
//! opaquely ([`docs/spec/L3.md`](../../../docs/spec/L3.md) §3.6).

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{FrameKind, RESOURCE_TAGS_KEY, Scope};

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::selector::{self, Selector, TagIndex};
use crate::state::Degradation;

/// Everything `phux tag` needs to act on `selector`: the resolved Terminals,
/// their current tags (for `ls` and as the mutation base for `add`/`rm`),
/// and the connection to write any edits back on.
#[derive(Debug)]
pub struct TagSession {
    /// The connection this session was resolved on; further writes go here.
    pub conn: Connection,
    /// The current server-wide snapshot the resolution used.
    pub snapshot: phux_protocol::wire::info::SessionSnapshot,
    /// What that snapshot and tag index could not see.
    pub degradation: Degradation,
    /// The full tag index fetched alongside the snapshot.
    pub index: TagIndex,
    /// `selector` resolved against `snapshot`/`index`.
    pub targets: Vec<ResourceId>,
}

/// Connect, fetch a `GET_STATE` snapshot and the L3 tag index, and resolve
/// `selector` against both.
///
/// # Errors
///
/// Transport failures connecting or fetching state.
pub async fn prepare(
    socket_path: &std::path::Path,
    selector: &Selector,
) -> Result<TagSession, AttachError> {
    let mut conn = Connection::connect(socket_path).await?;
    let (snapshot, degradation) = crate::state::get_state_on(&mut conn).await?.into_parts();
    let index = crate::state::fetch_tag_index(&mut conn, &snapshot).await;
    let targets = selector::resolve_with_tags(selector, &snapshot, &index);
    Ok(TagSession {
        conn,
        snapshot,
        degradation,
        index,
        targets,
    })
}

/// What writing then confirming one Terminal's tag set answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagWriteOutcome {
    /// The write was confirmed; these are the tags now on record.
    Confirmed(Vec<String>),
    /// The server refused the confirming read.
    Refused(String),
}

/// Replace `id`'s full tag set and confirm it landed.
///
/// `SET_METADATA` carries no reply frame, so the confirming
/// `GET_METADATA` round-trip (consuming `request_id + 1`) is load-bearing,
/// not cosmetic: frames are ordered on one connection, so its reply proves
/// the write was applied before this returns — the same reason `phux new`
/// GETs after its create SET.
///
/// # Errors
///
/// Transport failures from [`Connection::send`]/[`Connection::request_metadata`].
pub async fn write_tags(
    conn: &mut Connection,
    request_id: u32,
    id: &ResourceId,
    tags: &[String],
) -> Result<(TagWriteOutcome, Degradation), AttachError> {
    let value = serde_json::to_vec(tags).unwrap_or_else(|_| b"[]".to_vec());
    conn.send(&FrameKind::SetMetadata {
        request_id,
        scope: Scope::Resource(id.clone()),
        key: RESOURCE_TAGS_KEY.to_owned(),
        value,
    })
    .await?;
    let (answer, interleaved) = conn
        .request_metadata(
            request_id.wrapping_add(1),
            Scope::Resource(id.clone()),
            RESOURCE_TAGS_KEY.to_owned(),
        )
        .await?
        .into_parts();
    let degradation = Degradation::from_interleaved(&interleaved);
    let outcome = match answer {
        Ok(value) => TagWriteOutcome::Confirmed(
            value
                .and_then(|bytes| serde_json::from_slice::<Vec<String>>(&bytes).ok())
                .unwrap_or_default(),
        ),
        Err(refusal) => TagWriteOutcome::Refused(refusal.to_string()),
    };
    Ok((outcome, degradation))
}

/// Strip an optional leading `#` from each supplied tag and drop empties /
/// duplicates, so `phux tag add x #x` and `phux tag add x` are equivalent.
#[must_use]
pub fn normalize(tags: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in tags {
        let t = t.strip_prefix('#').unwrap_or(t).trim();
        if !t.is_empty() && !out.iter().any(|e| e == t) {
            out.push(t.to_owned());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_hash_drops_empties_and_dedupes() {
        assert_eq!(
            normalize(&[
                "#build".to_owned(),
                "build".to_owned(),
                "  ".to_owned(),
                "ci".to_owned()
            ]),
            vec!["build".to_owned(), "ci".to_owned()]
        );
    }

    #[tokio::test]
    async fn write_tags_sets_then_confirms_from_the_servers_own_store() {
        use crate::testkit::{ScriptSpec, ScriptedServer};

        let dir = tempfile::tempdir().expect("temp dir");
        let socket = dir.path().join("phux.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let listener = tokio::net::UnixListener::from_std(listener).expect("tokio listener");
        let server =
            tokio::spawn(async move { ScriptedServer::accept(&listener, ScriptSpec::new()).await });

        let mut conn = Connection::connect(&socket).await.expect("connect");
        let id = ResourceId::local(7);
        let tags = vec!["build".to_owned(), "ci".to_owned()];
        let (outcome, degradation) = write_tags(&mut conn, 101, &id, &tags)
            .await
            .expect("scripted server answers");
        drop(conn);
        let seen = server.await.expect("scripted server task");

        assert!(degradation.is_complete());
        assert_eq!(outcome, TagWriteOutcome::Confirmed(tags));
        assert!(
            seen.iter().any(|frame| matches!(
                frame,
                FrameKind::SetMetadata { request_id: 101, scope: Scope::Resource(scoped), key, .. }
                    if *scoped == id && key == RESOURCE_TAGS_KEY
            )),
            "expected the SET at request_id 101; sent {seen:?}"
        );
        assert!(
            seen.iter().any(|frame| matches!(
                frame,
                FrameKind::GetMetadata { request_id: 102, scope: Scope::Resource(scoped), key }
                    if *scoped == id && key == RESOURCE_TAGS_KEY
            )),
            "expected the confirming GET at request_id 102; sent {seen:?}"
        );
    }
}
