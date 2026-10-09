//! Project tag and satellite spawn for `phux workspace` save and restore.
//!
//! The server stores `phux.session.project/v1` and does not interpret it.
//! A satellite on a restored pane is the session host recorded at save time.
//! `None` and `""` stay on the attached server.

use std::collections::HashMap;
use std::path::Path;

use phux_protocol::ids::{GroupId, ResourceId, SatelliteHost};
use phux_protocol::wire::frame::{
    FrameKind, SESSION_PROJECT_KEY, Scope, decode_session_project, encode_session_project,
};

use crate::attach::AttachError;
use crate::attach::connection::Connection;

/// Why [`set_session_project`] did not land the tag.
#[derive(Debug, thiserror::Error)]
pub enum SessionProjectError {
    /// The socket could not be reached, or a frame failed in transit.
    #[error("could not set the project tag: {0}")]
    Transport(#[from] AttachError),
    /// The confirming read was refused.
    #[error("server refused the project tag: {0}")]
    Refused(String),
    /// The read-back bytes were not the value just written.
    #[error("project tag for session {name:?} did not read back as {project:?}")]
    Mismatch {
        /// Session the write named.
        name: String,
        /// Project the write named.
        project: String,
    },
}

/// The one stored `phux.session.project/v1` value, keyed by session name.
///
/// The spec keeps a single global value, so the map has at most one entry.
/// A missing server or an empty value yields an empty map: save still writes
/// the rest of the archive.
pub async fn fetch_project_by_session(socket_path: &Path) -> HashMap<String, String> {
    let mut projects = HashMap::new();
    let Ok(mut conn) = Connection::connect(socket_path).await else {
        return projects;
    };
    let stored = read_project_value(&mut conn).await;
    drop(conn);
    if let Some((name, project)) = stored.as_deref().and_then(decode_session_project) {
        projects.insert(name.to_owned(), project.to_owned());
    }
    projects
}

/// Write `phux.session.project/v1` for `name` and read it back.
///
/// `SET_METADATA` has no reply, so the read is what proves the tag landed.
///
/// # Errors
///
/// [`SessionProjectError`] when the server cannot be reached, refuses the
/// read, or returns a different value.
pub async fn set_session_project(
    socket_path: &Path,
    name: &str,
    project: &str,
) -> Result<(), SessionProjectError> {
    let expected = encode_session_project(name, project);
    let stored = write_project_tag(socket_path, name, project).await?;
    if stored.as_deref() != Some(expected.as_slice()) {
        return Err(SessionProjectError::Mismatch {
            name: name.to_owned(),
            project: project.to_owned(),
        });
    }
    Ok(())
}

/// Spawn frame for one restored pane.
///
/// `host` is the session's satellite. `None` and `""` leave the pane on the
/// attached server. `owner` joins that pane's window. A local owner cannot
/// cross a satellite link, so it is omitted and the satellite places the pane.
#[must_use]
pub fn restored_spawn_frame(
    owner: Option<&ResourceId>,
    command: Option<Vec<String>>,
    cwd: Option<String>,
    env: Option<Vec<(String, String)>>,
    host: Option<&str>,
) -> FrameKind {
    FrameKind::SpawnResource {
        request_id: 1,
        group: GroupId::new(1),
        command,
        cwd,
        env,
        term: None,
        satellite: host.filter(|host| !host.is_empty()).map(SatelliteHost::new),
        owner_terminal: owner_on_spawn_host(owner, host),
        agent_session: None,
        initial_size: None,
        resource: None,
    }
}

/// An owner may ride a spawn only when it lives on the same host as the pane.
fn owner_on_spawn_host(owner: Option<&ResourceId>, host: Option<&str>) -> Option<ResourceId> {
    let owner = owner?;
    let Some(host) = host.filter(|host| !host.is_empty()) else {
        return Some(owner.clone());
    };
    match owner {
        ResourceId::Satellite {
            host: owner_host, ..
        } if owner_host.as_str() == host => Some(owner.clone()),
        _ => None,
    }
}

/// `SET_METADATA` has no reply. The caller reads the key back on this connection.
async fn write_project_tag(
    socket_path: &Path,
    name: &str,
    project: &str,
) -> Result<Option<Vec<u8>>, SessionProjectError> {
    let mut conn = Connection::connect(socket_path).await?;
    conn.send(&project_set_frame(name, project)).await?;
    let reply = conn
        .request_metadata(2, Scope::Global, SESSION_PROJECT_KEY.to_owned())
        .await?;
    let (answer, _) = reply.into_parts();
    let stored = answer.map_err(|err| SessionProjectError::Refused(err.to_string()))?;
    drop(conn);
    Ok(stored)
}

async fn read_project_value(conn: &mut Connection) -> Option<Vec<u8>> {
    let reply = conn
        .request_metadata(1, Scope::Global, SESSION_PROJECT_KEY.to_owned())
        .await
        .ok()?;
    let (answer, _) = reply.into_parts();
    answer.ok().flatten()
}

fn project_set_frame(name: &str, project: &str) -> FrameKind {
    FrameKind::SetMetadata {
        request_id: 1,
        scope: Scope::Global,
        key: SESSION_PROJECT_KEY.to_owned(),
        value: encode_session_project(name, project),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restored_spawn_uses_the_session_host() {
        let frame = restored_spawn_frame(
            Some(&ResourceId::local(1)),
            None,
            Some("/src/api".to_owned()),
            None,
            Some("edge"),
        );
        match frame {
            FrameKind::SpawnResource {
                satellite: Some(host),
                cwd: Some(dir),
                owner_terminal: None,
                ..
            } => {
                assert_eq!(host.as_str(), "edge");
                assert_eq!(dir, "/src/api");
            }
            other => panic!("spawn dropped the session host: {other:?}"),
        }
    }

    #[test]
    fn restored_spawn_keeps_an_owner_on_the_same_satellite() {
        let owner = ResourceId::satellite("edge", 4);
        let frame = restored_spawn_frame(Some(&owner), None, None, None, Some("edge"));
        match frame {
            FrameKind::SpawnResource {
                satellite: Some(host),
                owner_terminal: Some(kept),
                ..
            } => {
                assert_eq!(host.as_str(), "edge");
                assert_eq!(kept, owner);
            }
            other => panic!("same-host owner was dropped: {other:?}"),
        }
    }

    #[test]
    fn restored_spawn_stays_local_without_a_host() {
        let frame = restored_spawn_frame(Some(&ResourceId::local(1)), None, None, None, Some(""));
        match frame {
            FrameKind::SpawnResource {
                satellite: None,
                owner_terminal: Some(owner),
                ..
            } => assert_eq!(owner, ResourceId::local(1)),
            other => panic!("empty host became a satellite: {other:?}"),
        }
    }

    #[tokio::test]
    async fn set_session_project_round_trips_through_the_server_store() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (socket, server) =
            crate::testkit::serve_one(dir.path(), crate::testkit::ScriptSpec::new());
        set_session_project(&socket, "api", "phux")
            .await
            .expect("tag lands");
        let seen = server.await.expect("scripted server task");
        let value = encode_session_project("api", "phux");
        assert!(
            seen.iter().any(|frame| matches!(
                frame,
                FrameKind::SetMetadata {
                    scope: Scope::Global,
                    key,
                    value: stored,
                    ..
                } if key == SESSION_PROJECT_KEY && stored == &value
            )),
            "expected the project write; sent {seen:?}"
        );
    }

    #[tokio::test]
    async fn fetch_project_by_session_reads_the_stored_tag() {
        let dir = tempfile::tempdir().expect("temp dir");
        let spec = crate::testkit::ScriptSpec::new().stored_metadata(
            Scope::Global,
            SESSION_PROJECT_KEY,
            encode_session_project("api", "phux"),
        );
        let (socket, server) = crate::testkit::serve_one(dir.path(), spec);
        let projects = fetch_project_by_session(&socket).await;
        let _ = server.await.expect("scripted server task");
        assert_eq!(projects.get("api").map(String::as_str), Some("phux"));
    }

    #[tokio::test]
    async fn set_session_project_refuses_a_read_back_that_does_not_match() {
        let dir = tempfile::tempdir().expect("temp dir");
        let spec = crate::testkit::ScriptSpec::new()
            .drop_metadata_writes(Scope::Global, SESSION_PROJECT_KEY)
            .stored_metadata(
                Scope::Global,
                SESSION_PROJECT_KEY,
                encode_session_project("api", "other"),
            );
        let (socket, server) = crate::testkit::serve_one(dir.path(), spec);
        let err = set_session_project(&socket, "api", "phux")
            .await
            .expect_err("mismatched read-back");
        let _ = server.await.expect("scripted server task");
        assert!(matches!(err, SessionProjectError::Mismatch { .. }));
    }

    #[test]
    fn project_write_names_the_session() {
        let frame = project_set_frame("api", "phux");
        match frame {
            FrameKind::SetMetadata {
                scope: Scope::Global,
                key,
                value,
                ..
            } => {
                assert_eq!(key, SESSION_PROJECT_KEY);
                assert_eq!(decode_session_project(&value), Some(("api", "phux")));
            }
            other => panic!("project tag was not a global metadata write: {other:?}"),
        }
    }
}
