/// `phux archive export`: write the running server's sessions as an archive.
pub(crate) fn run_export(
    socket: Option<PathBuf>,
    output: Option<&PathBuf>,
    projection: Option<&str>,
) -> ExitCode {
    if let Some(prefix) = projection
        && let Err(err) = validate_save_projection_prefix(prefix)
    {
        return usage_error(&err);
    }
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    let rendered = match rt.block_on(capture_archive(&socket_path, projection)) {
        Ok(rendered) => rendered,
        Err(CaptureError::NoServer(err)) => {
            return report_no_server(&err, &socket_path, "archive export");
        }
        Err(CaptureError::Failed(err)) => return fail(&err),
    };
    let Some(path) = output else {
        outln!("{rendered}");
        return ExitCode::SUCCESS;
    };
    // Atomic, so an interrupted save never replaces a good archive with a
    // torn one (ADR-0150).
    match phux_server::autosave::write_atomic(path, rendered.as_bytes()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => fail(&format!("could not write {}: {err}", path.display())),
    }
}

/// Why [`capture_archive`] produced no archive.
enum CaptureError {
    /// The server could not be reached.
    NoServer(phux_client::attach::AttachError),
    /// Anything else, as a one-line reason.
    Failed(String),
}

/// Render the archive `archive export` writes: the state snapshot,
/// native agent-session records, and each session's L3 layout, confirmed
/// unchanged across the capture. Warnings go to stderr.
async fn capture_archive(
    socket_path: &Path,
    projection: Option<&str>,
) -> Result<String, CaptureError> {
    let (snapshot, degradation) = phux_client::state::get_state(socket_path)
        .await
        .map_err(CaptureError::NoServer)?
        .into_parts();
    // An archive is restored later, so an incomplete capture outlives the
    // command: warn before the file lands, but do not refuse.
    partial::warn_partial_view("archive export", &degradation);
    let recorded = fetch_record_index(socket_path, &snapshot)
        .await
        .map_err(|err| {
            CaptureError::Failed(format!("could not capture native agent sessions: {err}"))
        })?;
    let native_launches = capture_native_launches(socket_path, &snapshot).await?;
    // Agents started inside an existing shell carry a live `AgentSession`
    // instead of a launch record (ADR-0151).
    let (agent_sessions, bridge_warnings) = bridge::bridge_live_agent_sessions(
        &snapshot,
        recorded,
        &native_launches,
        bridge::resolve_resume_owner,
    );
    // The organization records: the sessions' arrangements and typed
    // attributes.
    let records = phux_client::org::fetch_from_socket(socket_path)
        .await
        .map_err(|err| {
            CaptureError::Failed(format!("could not capture session attributes: {err}"))
        })?;
    // `GET_STATE` never carries split trees, so each session's is read from
    // its arrangement (or, under `--projection`, its L3 layout key),
    // best-effort: missing or undecodable falls back to the
    // pane-list projection.
    let (layouts, layout_warnings) =
        fetch_session_layouts(socket_path, &snapshot, projection, &records).await;
    let confirmation = phux_client::state::get_state(socket_path)
        .await
        .map_err(CaptureError::NoServer)?
        .into_snapshot_ignoring_degradation();
    if !same_local_terminals(&snapshot, &confirmation) {
        return Err(CaptureError::Failed(
            "sessions changed while native agent sessions were captured; retry archive export"
                .to_owned(),
        ));
    }
    let places::Places {
        projects,
        hosts,
        directories,
        mut links,
    } = places::session_places(socket_path, records.sessions).await;
    let (mut archive, reconcile_warnings) = archive_from_snapshot(
        &snapshot,
        snapshot::AgentRecords {
            attached: &agent_sessions,
            native: &native_launches,
        },
        &layouts,
        &projects,
        &hosts,
        &directories,
    );
    for session in &mut archive.sessions {
        session.project_link = links.remove(&session.name);
    }
    for warning in bridge_warnings
        .iter()
        .chain(&layout_warnings)
        .chain(&reconcile_warnings)
    {
        warn!("{warning}");
    }
    serde_json::to_string_pretty(&archive)
        .map_err(|err| CaptureError::Failed(format!("could not render the archive: {err}")))
}

async fn capture_native_launches(
    socket: &Path,
    snapshot: &phux_protocol::wire::info::SessionSnapshot,
) -> Result<HashMap<ResourceId, phux_protocol::native_agent::SpawnNativeAgent>, CaptureError> {
    use phux_client::native_agent::{ClientError, fetch_launch_recipes};
    let mut records = fetch_launch_recipes(socket, snapshot, |degradation| {
        partial::warn_partial_view("archive export", degradation);
    })
    .await
    .map_err(|error| match error {
        ClientError::Transport(error) => CaptureError::NoServer(error),
        ClientError::Invalid(message) | ClientError::Protocol(message) => {
            CaptureError::Failed(message)
        }
        error => CaptureError::Failed(error.to_string()),
    })?;
    for spec in records.values_mut() {
        // Export intent, never a source binding, host-local package tree or
        // process environment. The destination mints all of those afresh.
        spec.session.clear();
        spec.guardian = None;
        spec.guardian_key = None;
        spec.guardian_expected = None;
        spec.env.clear();
    }
    Ok(records)
}

/// The `phux server --autosave` archiver (ADR-0150), now an export schedule:
/// the same capture as `phux archive export`, run on the autosave thread
/// against the server's own socket. The organization store keeps sessions
/// across a restart, so a cold start only imports what the store lacks, and
/// like `phux archive import` it starts nothing; `phux restore` relaunches.
#[derive(Debug, Default)]
pub(crate) struct AutosaveArchiver {
    runtime: Option<tokio::runtime::Runtime>,
}

impl phux_server::autosave::WorkspaceArchiver for AutosaveArchiver {
    fn restore(&mut self, socket: &Path, archive: &Path) -> Result<(), String> {
        if run_import(archive, Some(socket.to_path_buf())) == ExitCode::SUCCESS {
            Ok(())
        } else {
            Err("archive import failed (see the diagnostics above)".to_owned())
        }
    }

    fn capture(&mut self, socket: &Path) -> Result<Vec<u8>, String> {
        if self.runtime.is_none() {
            let runtime = crate::commands::build_cli_runtime()
                .map_err(|err| format!("could not build the autosave runtime: {err}"))?;
            self.runtime = Some(runtime);
        }
        let Some(runtime) = &self.runtime else {
            return Err("the autosave runtime is missing".to_owned());
        };
        match runtime.block_on(capture_archive(socket, None)) {
            Ok(rendered) => Ok(rendered.into_bytes()),
            Err(CaptureError::NoServer(err)) => Err(format!("could not reach the server: {err}")),
            Err(CaptureError::Failed(err)) => Err(err),
        }
    }
}

/// Validate an `archive export --projection` value. Unlike the other
/// `--projection` flags it takes the bare `<prefix>.layout/v1` prefix and
/// appends each session id itself; a full key would silently match nothing.
fn validate_save_projection_prefix(value: &str) -> Result<(), String> {
    const SUFFIX: &str = ".layout/v1";
    let Some(prefix) = value.strip_suffix(SUFFIX) else {
        return Err(format!(
            "--projection {value:?} does not end in \"{SUFFIX}\": `archive export` takes the \
             bare prefix, not a full key, and appends \"/<session-id>\" itself for each \
             session — pass \"<prefix>{SUFFIX}\", e.g. \"myapp{SUFFIX}\""
        ));
    };
    if prefix.is_empty() {
        return Err(format!(
            "--projection {value:?} has nothing before \"{SUFFIX}\"; the expected form is \
             \"<prefix>{SUFFIX}\""
        ));
    }
    Ok(())
}

fn usage_error(message: &str) -> ExitCode {
    error!("{message}");
    ExitCode::from(2)
}

/// Each session's stored layout, by live session id.
type SessionLayouts = HashMap<phux_protocol::ids::SessionId, ArrangementDoc>;

/// Best-effort per-session L3 layout read over one connection. A session
/// with nothing decodable is absent from the map (and falls back to the
/// registry projection); under an explicit `--projection` the absence is also
/// a warning, since the key was probably mistyped.
async fn fetch_session_layouts(
    socket_path: &Path,
    snapshot: &phux_protocol::wire::info::SessionSnapshot,
    projection: Option<&str>,
    records: &phux_client::org::OrgInventory,
) -> (SessionLayouts, Vec<String>) {
    let mut layouts = HashMap::new();
    let mut warnings = Vec::new();
    let Ok(mut conn) = Connection::connect(socket_path).await else {
        return (layouts, warnings);
    };
    let Some(prefix) = projection else {
        return (
            fetch_session_arrangements(&mut conn, snapshot, records).await,
            warnings,
        );
    };
    let mut request_id = 1u32;
    for session in &snapshot.sessions {
        let key = format!("{prefix}/{}", session.id.get());
        let found = match LayoutOps::with_key(&mut conn, session.id, key.clone(), request_id) {
            Ok(mut ops) => {
                request_id = request_id.wrapping_add(2);
                ops.read().await.is_ok_and(|workspace| {
                    layouts.insert(session.id, workspace);
                    true
                })
            }
            Err(_) => false,
        };
        if !found {
            warnings.push(format!(
                "archive export: session {:?} has no layout stored at projection key {key:?}",
                session.name
            ));
        }
    }
    (layouts, warnings)
}

/// Each listed session's arrangement (`ArrangementGet`) that someone has
/// arranged, read as a client document over its members' live handles in
/// `inventory`. A session whose arrangement is empty or was never replaced
/// (still at its first revision: every session gets one at creation, and
/// adding a pane never touches it) is absent from the map, so it exports as
/// the registry projection, as it did when only an arranged session had a
/// layout key. An unreadable arrangement is absent too.
async fn fetch_session_arrangements(
    conn: &mut Connection,
    snapshot: &phux_protocol::wire::info::SessionSnapshot,
    inventory: &phux_client::org::OrgInventory,
) -> SessionLayouts {
    use phux_client::layout::wire::{HandleMap, from_protocol};
    let mut layouts = HashMap::new();
    let mut request_id = 0u32;
    for session in &snapshot.sessions {
        let Some(record) = inventory.session_by_live(session.id) else {
            continue;
        };
        let Some(key) = record.arrangement else {
            continue;
        };
        request_id = request_id.wrapping_add(1);
        let Ok(value) = phux_client::org::arrangement(conn, request_id, key).await else {
            continue;
        };
        if value.document.groups.is_empty() || value.revision.get() <= 1 {
            continue;
        }
        let handles = HandleMap::from_pairs(
            inventory
                .members(&record.key)
                .map(|member| (member.key, member.live.clone())),
        );
        let (mut document, _binding) = from_protocol(&value.document, &handles);
        // `from_protocol` names an unnamed group by its position; the
        // archive keeps it unnamed, so an import does not make the name up.
        for group in &mut document.groups {
            let unnamed = value
                .document
                .groups
                .iter()
                .any(|wire| wire.id == group.id && wire.name.is_none());
            if unnamed {
                group.name.clear();
            }
        }
        layouts.insert(session.id, document);
    }
    layouts
}