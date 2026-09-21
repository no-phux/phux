//! `phux tag` — read and write a Terminal's L3 tags (`phux-f8wi`, ADR-0027).
//!
//! Tags are freeform strings stored as L3 metadata under the conventional
//! key [`RESOURCE_TAGS_KEY`] (`phux.tags/v1`), scoped to a `ResourceId`. The
//! value is a UTF-8 JSON array of tag strings; the server stores the bytes
//! opaquely ([`docs/spec/L3.md`](../../../docs/spec/L3.md) §3.6).
//!
//! [`apply`] is the list/add/rm orchestration both `phux tag` and MCP
//! `phux_tag` call.

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

/// One `phux tag` action, with add/rm tags already parsed (not yet
/// normalized).
#[derive(Debug, Clone, Copy)]
pub enum TagOp<'a> {
    /// List each resolved Terminal's current tags.
    List,
    /// Append each missing tag.
    Add(&'a [String]),
    /// Drop each named tag.
    Remove(&'a [String]),
}

/// A successful [`apply`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagOutcome {
    /// One row per resolved Terminal: the id and its (confirmed) tags.
    pub rows: Vec<(ResourceId, Vec<String>)>,
    /// Snapshot/index degradation from [`prepare`].
    pub view: Degradation,
    /// Interleaved notices from confirming writes.
    pub interleaved: Vec<String>,
}

/// Why [`apply`] did not complete.
#[derive(Debug, thiserror::Error)]
pub enum TagError {
    /// Transport failure connecting or fetching state.
    #[error(transparent)]
    Attach(#[from] AttachError),
    /// The selector matched no Terminal.
    #[error("no such target")]
    Miss {
        /// What the snapshot could not see.
        degradation: Degradation,
    },
    /// A write's confirming read was refused.
    #[error("{message}")]
    WriteRefused {
        /// The full refusal sentence, matching `phux tag`.
        message: String,
    },
}

/// `add` appends each missing tag; `rm` drops each named one. The result is
/// sorted and de-duplicated, as `phux tag` writes it.
pub fn merge(current: &mut Vec<String>, add: bool, tags: &[String]) {
    if add {
        for tag in tags {
            if !current.iter().any(|existing| existing == tag) {
                current.push(tag.clone());
            }
        }
    } else {
        current.retain(|existing| !tags.iter().any(|tag| tag == existing));
    }
    current.sort();
    current.dedup();
}

/// Resolve `selector`, then list or edit tags.
///
/// Snapshot partial-view notices for a hit live on [`TagOutcome::view`]; a
/// miss is [`TagError::Miss`] without treating the view as a hit.
///
/// # Errors
///
/// [`TagError`] — see its variants.
pub async fn apply(
    socket_path: &std::path::Path,
    selector: &Selector,
    op: TagOp<'_>,
) -> Result<TagOutcome, TagError> {
    let mut session = prepare(socket_path, selector).await?;
    if session.targets.is_empty() {
        return Err(TagError::Miss {
            degradation: session.degradation,
        });
    }
    let view = session.degradation.clone();
    let (rows, interleaved) = match op {
        TagOp::List => (listed_rows(&session), Vec::new()),
        TagOp::Add(tags) => edit_rows(&mut session, true, tags).await?,
        TagOp::Remove(tags) => edit_rows(&mut session, false, tags).await?,
    };
    drop(session);
    Ok(TagOutcome {
        rows,
        view,
        interleaved,
    })
}

fn listed_rows(session: &TagSession) -> Vec<(ResourceId, Vec<String>)> {
    session
        .targets
        .iter()
        .map(|id| {
            (
                id.clone(),
                session.index.get(id).cloned().unwrap_or_default(),
            )
        })
        .collect()
}

async fn edit_rows(
    session: &mut TagSession,
    add: bool,
    tags: &[String],
) -> Result<(Vec<(ResourceId, Vec<String>)>, Vec<String>), TagError> {
    let wanted = normalize(tags);
    let mut rows = Vec::with_capacity(session.targets.len());
    let mut interleaved = Vec::new();
    let mut request_id: u32 = 100;
    for id in session.targets.clone() {
        let mut current = session.index.get(&id).cloned().unwrap_or_default();
        merge(&mut current, add, &wanted);
        request_id += 1;
        let (outcome, degradation) =
            write_tags(&mut session.conn, request_id, &id, &current).await?;
        request_id += 1;
        interleaved.extend(degradation.notices().iter().cloned());
        match outcome {
            TagWriteOutcome::Confirmed(confirmed) => rows.push((id, confirmed)),
            TagWriteOutcome::Refused(refusal) => {
                return Err(TagError::WriteRefused {
                    message: format!(
                        "tag write to {} could not be confirmed: server refused the read: {refusal}",
                        selector::format_terminal_id(&id),
                    ),
                });
            }
        }
    }
    Ok((rows, interleaved))
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

    #[test]
    fn merge_sorts_dedups_and_removes() {
        let mut tags = vec!["b".to_owned(), "a".to_owned()];
        merge(&mut tags, true, &["c".to_owned(), "a".to_owned()]);
        assert_eq!(tags, ["a", "b", "c"]);
        merge(&mut tags, false, &["b".to_owned()]);
        assert_eq!(tags, ["a", "c"]);
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

    fn pane_state() -> phux_protocol::wire::info::SessionSnapshot {
        use phux_protocol::ids::{SessionId, WindowId};
        use phux_protocol::wire::info::{ResourceInfo, SessionInfo, WindowInfo};
        let session = SessionId::new(1);
        let window = WindowId::new(10);
        phux_protocol::wire::info::SessionSnapshot::new(session, window, ResourceId::local(1))
            .with_sessions(vec![SessionInfo::new(session, "work").with_window_count(1)])
            .with_windows(vec![WindowInfo::new(window, session, "shell")])
            .with_resources(vec![
                ResourceInfo::new(ResourceId::local(1), window, 80, 24),
                ResourceInfo::new(ResourceId::local(2), window, 80, 24),
            ])
    }

    async fn apply_on(
        spec: crate::testkit::ScriptSpec,
        target: &str,
        op: TagOp<'_>,
    ) -> Result<TagOutcome, TagError> {
        use crate::testkit::ScriptedServer;
        let dir = tempfile::tempdir().expect("temp dir");
        let socket = dir.path().join("phux.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let listener = tokio::net::UnixListener::from_std(listener).expect("tokio listener");
        let server = tokio::spawn(async move { ScriptedServer::accept(&listener, spec).await });
        let selector = crate::selector::parse(target).expect("selector");
        let result = apply(&socket, &selector, op).await;
        drop(server);
        result
    }

    #[tokio::test]
    async fn apply_lists_a_hit_and_splits_a_partial_miss() {
        use crate::testkit::ScriptSpec;
        const NOTICE: &str = "satellite build-box is unreachable: link is down";

        let listed = apply_on(ScriptSpec::new().state(pane_state()), "@1", TagOp::List)
            .await
            .expect("list");
        assert_eq!(listed.rows.len(), 1);
        assert!(listed.view.is_complete());

        let miss = apply_on(ScriptSpec::new().state(pane_state()), "@9", TagOp::List).await;
        assert!(
            matches!(miss, Err(TagError::Miss { ref degradation }) if degradation.is_complete()),
            "{miss:?}"
        );

        let unresolved = apply_on(
            ScriptSpec::new()
                .state(pane_state())
                .degradation_notice(NOTICE),
            "@9",
            TagOp::List,
        )
        .await;
        assert!(
            matches!(
                unresolved,
                Err(TagError::Miss { ref degradation }) if degradation.notices() == [NOTICE]
            ),
            "{unresolved:?}"
        );

        let hit = apply_on(
            ScriptSpec::new()
                .state(pane_state())
                .degradation_notice(NOTICE),
            "@1",
            TagOp::List,
        )
        .await
        .expect("partial hit");
        assert_eq!(hit.view.notices(), [NOTICE]);
        assert_eq!(hit.rows.len(), 1);
    }
}
