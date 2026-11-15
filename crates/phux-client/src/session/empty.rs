use std::path::Path;

use phux_protocol::wire::frame::{Command, LaunchSpec, OrgFilter, Refusal, SessionLifetime};

use crate::attach::connection::Connection;

use super::create::CreateSessionError;

/// What creating an empty, keep-empty session (ADR-0105) answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateEmptyOutcome {
    /// The empty session was created.
    Created,
    /// The server answered the create with an error.
    ReadRefused(String),
    /// The create committed nothing this request can name.
    NotRegistered,
    /// The server refused the `SESSION_CREATE`.
    Refused(Refusal),
}

/// Create an empty, persistent session named `name`.
///
/// # Errors
///
/// Transport/decode failures, or an organization request that failed
/// outright.
pub async fn create_empty_session(
    conn: &mut Connection,
    name: &str,
    notices: &mut Vec<String>,
) -> Result<CreateEmptyOutcome, CreateSessionError> {
    create_empty_session_with_attrs(conn, name, &[], notices).await
}

/// Create an empty persistent session with attributes in its organization
/// record, atomically with `SESSION_CREATE`.
///
/// # Errors
///
/// Transport/decode failures, or an organization request that failed
/// outright.
pub async fn create_empty_session_with_attrs(
    conn: &mut Connection,
    name: &str,
    attrs: &[(String, String)],
    notices: &mut Vec<String>,
) -> Result<CreateEmptyOutcome, CreateSessionError> {
    let command = Command::SessionCreate {
        operation: crate::org::mint_operation(),
        name: name.to_owned(),
        lifetime: SessionLifetime::Persistent,
        attrs: attrs.to_vec(),
        launch: None,
    };
    let (outcome, degradation) = crate::org::request(conn, 1, command)
        .await
        .map_err(CreateSessionError::Org)?;
    notices.extend(degradation.notices().iter().cloned());
    Ok(match outcome {
        crate::org::OrgOutcome::Committed(_) => CreateEmptyOutcome::Created,
        crate::org::OrgOutcome::Refused(refusal) => CreateEmptyOutcome::Refused(refusal),
        crate::org::OrgOutcome::Error { message, .. } => CreateEmptyOutcome::ReadRefused(message),
        _ => CreateEmptyOutcome::NotRegistered,
    })
}

/// Check a session's authoritative checkout directory and return its oldest
/// local Terminal.
///
/// If `seed_empty`, start and arrange one shell at that directory when no
/// local Terminal is live; never rerun a repository recipe.
///
/// # Errors
///
/// The record is absent, has no matching directory, or its organization
/// request, shell launch, or arrangement write fails.
pub async fn check_session_directory(
    socket: &Path,
    name: &str,
    root: &Path,
    seed_empty: bool,
) -> Result<Option<u32>, String> {
    let mut conn = Connection::connect(socket)
        .await
        .map_err(|err| format!("could not inspect session {name:?}: {err}"))?;
    let records = crate::org::fetch(&mut conn, OrgFilter::All, 1)
        .await
        .map_err(|err| format!("could not inspect session {name:?}: {err}"))?;
    let record = records.session_named(name).ok_or_else(|| {
        format!("session {name:?} disappeared before its directory could be checked")
    })?;
    let directory = checked_session_directory(record, name, root)?;
    let live = records
        .members(&record.key)
        .filter(|resource| resource.kind == phux_protocol::ids::ResourceKind::Terminal)
        .filter_map(|resource| resource.live.as_ref()?.local_id())
        .min();
    if live.is_some() || !seed_empty {
        return Ok(live);
    }
    seed_directory_shell(&mut conn, record, directory, name)
        .await
        .map(Some)
}

async fn seed_directory_shell(
    conn: &mut Connection,
    record: &phux_protocol::wire::frame::SessionRecord,
    directory: String,
    name: &str,
) -> Result<u32, String> {
    let placed = conn
        .server_features_ext()
        .contains(phux_protocol::caps::ServerFeatureExt::CreatePlacement);
    let launch = LaunchSpec::new(Vec::new()).with_cwd(Some(directory));
    let committed = create_directory_shell(conn, record.key, launch, placed)
        .await
        .map_err(|err| format!("could not start a shell in session {name:?}: {err}"))?;
    let resource = committed
        .resource
        .ok_or_else(|| format!("server created no shell record in session {name:?}"))?;
    let terminal_id = committed
        .target
        .and_then(|target| target.resource.local_id())
        .ok_or_else(|| format!("server started no local shell in session {name:?}"))?;
    if !placed {
        arrange_legacy_directory_shell(conn, record.arrangement, resource)
            .await
            .map_err(|err| format!("could not arrange the shell in session {name:?}: {err}"))?;
    }
    Ok(terminal_id)
}

fn checked_session_directory(
    record: &phux_protocol::wire::frame::SessionRecord,
    name: &str,
    root: &Path,
) -> Result<String, String> {
    let directory = record
        .attrs
        .iter()
        .find(|(key, _)| key == "directory")
        .map(|(_, value)| value.as_str());
    let expected = root.to_string_lossy();
    match directory {
        Some(directory) if directory == expected => Ok(expected.into_owned()),
        Some(directory) => Err(format!(
            "session {name:?} belongs to {directory:?}, not {}",
            root.display()
        )),
        None => Err(format!(
            "session {name:?} has no checkout directory in its organization record"
        )),
    }
}

async fn create_directory_shell(
    conn: &mut Connection,
    session: phux_protocol::ids::SessionKey,
    launch: LaunchSpec,
    placed: bool,
) -> Result<phux_protocol::wire::frame::Committed, crate::org::OrgError> {
    crate::org::commit(
        conn,
        2,
        Command::ResourceCreate {
            operation: crate::org::mint_operation(),
            session,
            kind: phux_protocol::ids::ResourceKind::Terminal,
            launch: Some(Box::new(launch)),
            name: None,
            on_exit: phux_protocol::wire::frame::ExitPolicy::Remove,
            launch_mode: phux_protocol::wire::frame::LaunchMode::Immediate,
            placement: placed
                .then(|| Box::new(phux_protocol::wire::frame::CreatePlacement::NewGroup)),
        },
    )
    .await
}

/// Compatibility only: old organization servers cannot commit placement with creation.
async fn arrange_legacy_directory_shell(
    conn: &mut Connection,
    arrangement: Option<phux_protocol::ids::ArrangementKey>,
    resource: phux_protocol::ids::ResourceKey,
) -> Result<(), String> {
    let arranged = async {
        let arrangement = arrangement.ok_or("session has no arrangement")?;
        let value = crate::org::arrangement(conn, 3, arrangement)
            .await
            .map_err(|err| err.to_string())?;
        let mut document = value.document;
        document
            .groups
            .push(phux_protocol::wire::frame::ArrangementGroup {
                locked: false,
                id: crate::layout::fresh_id(),
                name: None,
                tree: phux_protocol::wire::frame::ArrangementNode::Leaf(
                    phux_protocol::wire::frame::EntryRef {
                        entry: crate::layout::fresh_id(),
                        target: phux_protocol::wire::frame::EntryTarget::Member(resource),
                    },
                ),
            });
        crate::org::commit(
            conn,
            4,
            Command::ArrangementReplace {
                operation: crate::org::mint_operation(),
                arrangement,
                expected: value.revision,
                document,
            },
        )
        .await
        .map_err(|err| err.to_string())?;
        Ok::<_, String>(())
    }
    .await;
    if arranged.is_err() {
        // Never replay the create to repair an uncertain legacy layout write.
        let _ = crate::org::remove_resource(conn, resource).await;
    }
    arranged
}
