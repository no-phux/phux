//! `phux archive` — the session archive (`export`, `import`), shared with
//! `phux project open` and the `phux server --autosave` export schedule.
//!
//! An archive is a JSON document of sessions, windows, panes, split trees,
//! and resumable agent sessions. Importing one never starts a process
//! ([`import_archive`]): it recreates each missing session persistent, with
//! its host, project, project link, and directory, and each archived local
//! pane as a dormant resource record placed in the session's arrangement. `phux
//! restore` starts them, as does [`relaunch_imported`], the separate step
//! `phux project open` runs right after the import.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_client::attach::connection::Connection;
use phux_client::layout::{ArrangementDoc, Group, LayoutNode, LayoutState, SplitDir};
use phux_client::layout_ops::LayoutOps;
use phux_protocol::ids::{ResourceId, ResourceKey, ResourceKind, SessionKey};
use phux_protocol::wire::frame::{
    ArrangementDoc as WireDoc, ArrangementGroup, ArrangementNode, Command, EntryRef, EntryTarget,
    LaunchMode, LaunchSpec, OrgFilter, SpawnResult, SplitDirection,
};
use phux_server::runtime::default_socket_path;

use phux_client::agent_session_record::{AgentSessionRecord, fetch_record_index};

use crate::commands::agent::{PreparedAgentSession, prepare};
use crate::commands::{ArchiveAction, cli_runtime, partial, report_no_server};
use phux_client::agent_session_record::spawn_with_agent_session_on;

mod bridge;
pub(crate) mod model;
mod snapshot;

use model::{RestoreSummary, WorkspaceWindow, parse_archive, restore_plan};
use snapshot::archive_from_snapshot;

/// `phux archive <action>`.
pub(crate) fn run_archive(action: &ArchiveAction, socket: Option<PathBuf>) -> ExitCode {
    match action {
        ArchiveAction::Export { output, projection } => {
            run_export(socket, output.as_ref(), projection.as_deref())
        }
        ArchiveAction::Import {
            archive,
            plan: true,
        } => print_plan(archive),
        ArchiveAction::Import {
            archive,
            plan: false,
        } => run_import(archive, socket),
    }
}

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
    // The organization records (`None` from a server that predates them):
    // the sessions' arrangements and typed attributes.
    let records = phux_client::org::fetch_from_socket(socket_path)
        .await
        .map_err(|err| {
            CaptureError::Failed(format!("could not capture session attributes: {err}"))
        })?;
    // `GET_STATE` never carries split trees, so each session's is read from
    // its arrangement (or, from an older server or under `--projection`, its
    // L3 layout key), best-effort: missing or undecodable falls back to the
    // pane-list projection.
    let (layouts, layout_warnings) =
        fetch_session_layouts(socket_path, &snapshot, projection, records.as_ref()).await;
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
    // Typed attributes are authoritative and per-session. Singleton metadata
    // values remain a fallback for older sessions and archives.
    let mut projects = phux_client::workspace_place::fetch_project_by_session(socket_path).await;
    let mut hosts = phux_client::workspace_place::fetch_host_by_session(socket_path).await;
    let mut directories =
        phux_client::workspace_place::fetch_directory_by_session(socket_path).await;
    let mut links = HashMap::new();
    if let Some(records) = records {
        for session in records.sessions {
            for (key, value) in session.attrs {
                match key.as_str() {
                    "project" => {
                        projects.insert(session.name.clone(), value);
                    }
                    phux_core::project_identity::attr::LINK => {
                        links.insert(session.name.clone(), value);
                    }
                    "directory" => {
                        directories.insert(session.name.clone(), value);
                    }
                    "host" => {
                        hosts.insert(session.name.clone(), value);
                    }
                    _ => {}
                }
            }
        }
    }
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
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
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
///
/// Against an `ORGANIZATION` server (`records`, its organization) with no
/// `--projection`, each session's layout is its typed arrangement
/// ([`fetch_session_arrangements`]). The `phux.tui.layout/v1/<session>` key
/// is read only from an older server; a named projection is always its own
/// metadata key (ADR-0161 §8).
async fn fetch_session_layouts(
    socket_path: &Path,
    snapshot: &phux_protocol::wire::info::SessionSnapshot,
    projection: Option<&str>,
    records: Option<&phux_client::org::OrgInventory>,
) -> (SessionLayouts, Vec<String>) {
    let mut layouts = HashMap::new();
    let mut warnings = Vec::new();
    let Ok(mut conn) = Connection::connect(socket_path).await else {
        return (layouts, warnings);
    };
    if projection.is_none()
        && let Some(records) = records
    {
        return (
            fetch_session_arrangements(&mut conn, snapshot, records).await,
            warnings,
        );
    }
    let prefix = projection.unwrap_or(phux_client::layout_ops::LAYOUT_KEY);
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
        if !found && projection.is_some() {
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

/// Print the archive's session, host, directory, and project organization.
/// Does not dial a server. The directory is the focused pane's cwd when the
/// session itself has none.
pub(crate) fn print_plan(archive_path: &Path) -> ExitCode {
    let text = match std::fs::read_to_string(archive_path) {
        Ok(text) => text,
        Err(err) => return fail(&format!("could not read {}: {err}", archive_path.display())),
    };
    let archive = match model::parse_archive(&text) {
        Ok(archive) => archive,
        Err(err) => return fail(&err),
    };
    let rows = match model::organization_rows(&archive) {
        Ok(rows) => rows,
        Err(err) => return fail(&err),
    };
    for line in phux_client_core::organization::organization_lines(&rows) {
        outln!("{line}");
    }
    ExitCode::SUCCESS
}

/// `phux archive import`: recreate the archive's missing sessions, empty and
/// persistent, without starting any process ([`import_archive`]), and print
/// the summary document. `restored` names the sessions imported.
pub(crate) fn run_import(archive_path: &Path, socket: Option<PathBuf>) -> ExitCode {
    let archive = match load_archive(archive_path) {
        Ok(archive) => archive,
        Err(code) => return code,
    };
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    let imported = match rt.block_on(import_archive(
        &socket_path,
        &archive,
        "archive import",
        ImportKind::Archive,
        &[],
    )) {
        Ok(imported) => imported,
        Err(RestoreError::Reported(code)) => return code,
        Err(RestoreError::Failed(err)) => return fail(&err),
    };
    let summary = imported.summary();
    for failed in &summary.failed {
        error!(
            "archive import: session {:?} failed: {}",
            failed.name, failed.reason
        );
    }
    let any_failed = !summary.failed.is_empty();
    let print_code = crate::output::json(&summary);
    if any_failed {
        ExitCode::FAILURE
    } else {
        print_code
    }
}

/// Why [`import_archive`] produced no summary.
pub(crate) enum RestoreError {
    /// The diagnostic is already on stderr; exit with this code.
    Reported(ExitCode),
    /// Nothing printed yet: report this message.
    Failed(String),
}

/// Whether import keeps writing the legacy metadata tags after creating the
/// authoritative organization record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImportKind<'a> {
    /// `archive import` also keeps writing its legacy metadata tags.
    Archive,
    /// `project open` binds the checkout root independently of pane cwd.
    Project { directory: &'a Path },
}

/// The sessions [`import_archive`] recreated, ready for
/// [`relaunch_imported`].
#[derive(Debug)]
pub(crate) struct Imported {
    /// Each recreated session's archived plan.
    sessions: Vec<model::CreateRequest>,
    /// Sessions whose archived panes exist as dormant records, by name.
    /// [`relaunch_imported`] relaunches those instead of spawning panes.
    dormant: HashSet<String>,
    /// Panes imported as a plain shell instead of their archived agent.
    warnings: Vec<model::RestoreWarning>,
    /// Archived sessions a session of the same name already covers.
    skipped_existing: Vec<String>,
    /// Sessions the import could not create.
    failed: Vec<model::FailedRestore>,
    /// Names raced by another creator after restore planning.
    name_taken: HashSet<String>,
}

impl Imported {
    /// The import's own summary document: `restored` names the sessions it
    /// recreated, none of them started.
    fn summary(&self) -> RestoreSummary {
        RestoreSummary {
            schema_version: model::RESTORE_SUMMARY_SCHEMA_VERSION,
            restored: self
                .sessions
                .iter()
                .map(|create| create.name.clone())
                .collect(),
            skipped_existing: self.skipped_existing.clone(),
            failed: self.failed.clone(),
            warnings: self.warnings.clone(),
        }
    }
    /// Whether import found the requested name already occupied.
    pub(crate) fn skipped_existing(&self, name: &str) -> bool {
        self.skipped_existing.iter().any(|skipped| skipped == name)
    }
    pub(crate) fn name_taken(&self, name: &str) -> bool {
        self.name_taken.contains(name)
    }
}

/// Recreate the sessions of an already-validated `archive` that are missing
/// on the server at `socket_path`, without starting any process (R16): each
/// becomes a persistent session carrying the archived host, project, and
/// directory, with its local panes as dormant records in its arrangement
/// ([`import_dormant_panes`]). Each also carries `attrs`, such as a project
/// identity (ADR-0168). A session that cannot be created is reported in
/// [`Imported`] rather than aborting the rest.
///
/// # Errors
///
/// [`RestoreError::Failed`] for an invalid archive or a server without
/// `ORGANIZATION`.
pub(crate) async fn import_archive(
    socket_path: &Path,
    archive: &model::WorkspaceArchive,
    verb: &str,
    kind: ImportKind<'_>,
    extra: &[(String, String)],
) -> Result<Imported, RestoreError> {
    let existing = fetch_existing_sessions(socket_path, verb)
        .await
        .map_err(RestoreError::Reported)?;
    let plan = restore_plan(archive, &existing).map_err(RestoreError::Failed)?;
    let organization = organization_supported(socket_path)
        .await
        .map_err(|err| RestoreError::Reported(report_no_server(&err, socket_path, verb)))?;
    if !organization {
        let reason = match kind {
            ImportKind::Archive => "cannot recreate a session without starting its panes",
            ImportKind::Project { .. } => "cannot create a session with per-session attributes",
        };
        return Err(RestoreError::Failed(format!(
            "{verb}: this server predates organization records, so it {reason}; upgrade it (`phux upgrade`)"
        )));
    }
    let mut sessions = Vec::with_capacity(plan.creates.len());
    let mut failed = Vec::new();
    let mut dormant = HashSet::new();
    let mut name_taken = HashSet::new();
    let mut warnings = Vec::new();
    for create in plan.creates {
        let imported = match check_agent_ownership(&create) {
            Ok(()) => create_imported_session(socket_path, &create, kind, extra).await,
            Err(reason) => Err(ImportFailure::Other(reason)),
        };
        match imported {
            Ok(outcome) => {
                warnings.extend(outcome.warnings.into_iter().map(|message| {
                    model::RestoreWarning {
                        session: create.name.clone(),
                        message,
                    }
                }));
                if outcome.dormant {
                    dormant.insert(create.name.clone());
                }
                sessions.push(create);
            }
            Err(reason) => {
                if matches!(reason, ImportFailure::NameTaken) {
                    name_taken.insert(create.name.clone());
                }
                failed.push(model::FailedRestore {
                    name: create.name,
                    reason: reason.message(),
                });
            }
        }
    }
    Ok(Imported {
        sessions,
        dormant,
        warnings,
        skipped_existing: plan.skipped_existing,
        failed,
        name_taken,
    })
}

/// Start the archived panes of the sessions [`import_archive`] recreated:
/// a session imported with dormant records relaunches them in place, as
/// `phux restore SESSION` does; any other session starts its seed pane
/// (resuming its native agent session when it can), then the rest of its
/// split tree. A session whose spawned relaunch fails has every pane started
/// for it closed again and stays imported, empty; it is reported in the
/// summary rather than aborting the rest.
pub(crate) async fn relaunch_imported(socket_path: &Path, imported: Imported) -> RestoreSummary {
    let mut restored = Vec::with_capacity(imported.sessions.len());
    let mut failed = imported.failed;
    let mut warnings = imported.warnings;
    for create in imported.sessions {
        let name = create.name.clone();
        let outcome = if imported.dormant.contains(&name) {
            relaunch_dormant_session(socket_path, &name).await
        } else {
            relaunch_one_session(socket_path, create).await
        };
        match outcome {
            Ok(outcome) => {
                warnings.extend(outcome.warnings.into_iter().map(|message| {
                    model::RestoreWarning {
                        session: name.clone(),
                        message,
                    }
                }));
                restored.push(name);
            }
            Err(reason) => failed.push(model::FailedRestore { name, reason }),
        }
    }
    RestoreSummary {
        schema_version: model::RESTORE_SUMMARY_SCHEMA_VERSION,
        restored,
        skipped_existing: imported.skipped_existing,
        failed,
        warnings,
    }
}

/// Whether the server at `socket_path` advertises `ORGANIZATION`.
async fn organization_supported(
    socket_path: &Path,
) -> Result<bool, phux_client::attach::AttachError> {
    let conn = Connection::connect(socket_path).await?;
    Ok(phux_client::org::supported(&conn))
}

/// Refuse a session whose archived agent session would now resume through a
/// plugin other than the one that owned it: a security boundary, checked
/// before anything is created. Any other resolution gap is the relaunch's
/// concern (it falls back to a plain shell with a warning).
fn check_agent_ownership(create: &model::CreateRequest) -> Result<(), String> {
    if create.host.as_deref().is_some_and(|host| !host.is_empty())
        && (create.native_launch.is_some()
            || create
                .windows
                .iter()
                .flat_map(|window| &window.panes)
                .any(|pane| pane.native_launch.is_some()))
    {
        return Err("restore native SDK recipes on their executing host; no local terminal or SDK fallback was attempted".to_owned());
    }
    let seed = create
        .agent_session
        .as_ref()
        .map(|agent| (agent, create.cwd.as_deref()));
    let panes = create.windows.iter().flat_map(|window| {
        window.panes.iter().filter_map(|pane| {
            pane.agent_session
                .as_ref()
                .map(|agent| (agent, pane.cwd.as_deref()))
        })
    });
    for (agent, cwd) in seed.into_iter().chain(panes) {
        if let Err(PrepareAgentError::OwnershipMismatch(err)) = prepare_archived_agent(agent, cwd) {
            return Err(err);
        }
    }
    Ok(())
}

/// What [`create_imported_session`] made besides the session.
struct ImportOutcome {
    /// Whether its panes exist as dormant records.
    dormant: bool,
    /// Panes imported as a plain shell instead of their archived agent.
    warnings: Vec<String>,
}

enum ImportFailure {
    NameTaken,
    Other(String),
}

impl From<String> for ImportFailure {
    fn from(reason: String) -> Self {
        Self::Other(reason)
    }
}

impl ImportFailure {
    fn message(self) -> String {
        match self {
            Self::NameTaken => format!(
                "the server refused it: {}",
                phux_client::org::refusal_sentence(&phux_protocol::wire::frame::Refusal::NameTaken,)
            ),
            Self::Other(reason) => reason,
        }
    }
}

/// Import one archived session as records: the session, persistent, tagged
/// with its archived host, project, and directory, then its local panes as
/// dormant records ([`import_dormant_panes`]). Nothing starts. A session
/// saved on another host gets no dormant panes: [`relaunch_imported`].
async fn create_imported_session(
    socket_path: &Path,
    create: &model::CreateRequest,
    kind: ImportKind<'_>,
    extra: &[(String, String)],
) -> Result<ImportOutcome, ImportFailure> {
    let mut conn = Connection::connect(socket_path)
        .await
        .map_err(|err| format!("could not reach the server: {err}"))?;
    let mut notices = Vec::new();
    let mut attrs = session_attrs(create, kind);
    attrs.extend(extra.iter().cloned());
    let outcome = phux_client::session::create_empty_session_with_attrs(
        &mut conn,
        &create.name,
        &attrs,
        &mut notices,
    )
    .await
    .map_err(|err| format!("could not create the session: {err}"))?;
    for notice in notices {
        warn!("partial results — {notice}");
    }
    match outcome {
        phux_client::session::CreateEmptyOutcome::Created => {}
        phux_client::session::CreateEmptyOutcome::Refused(
            phux_protocol::wire::frame::Refusal::NameTaken,
        ) => return Err(ImportFailure::NameTaken),
        phux_client::session::CreateEmptyOutcome::Refused(refusal) => {
            return Err(ImportFailure::Other(format!(
                "the server refused it: {}",
                phux_client::org::refusal_sentence(&refusal)
            )));
        }
        other => {
            return Err(ImportFailure::Other(format!(
                "the server did not create it: {other:?}"
            )));
        }
    }
    if matches!(kind, ImportKind::Archive) {
        tag_session(socket_path, create).await?;
    }
    if create.host.as_deref().is_some_and(|host| !host.is_empty()) {
        return Ok(ImportOutcome {
            dormant: false,
            warnings: Vec::new(),
        });
    }
    let warnings = import_dormant_panes(&mut conn, create).await;
    drop(conn);
    let warnings = warnings?;
    Ok(ImportOutcome {
        dormant: true,
        warnings,
    })
}

/// What one archived pane runs: its command, directory, environment, and
/// native agent session. The seed pane's come from the session's plan.
struct PaneSource<'a> {
    command: Option<&'a Vec<String>>,
    cwd: Option<&'a str>,
    env: &'a std::collections::BTreeMap<String, String>,
    agent_session: Option<&'a model::WorkspaceAgentSession>,
    native_launch: Option<&'a phux_protocol::native_agent::SpawnNativeAgent>,
    role: &'static str,
}

impl<'a> PaneSource<'a> {
    fn seed(create: &'a model::CreateRequest) -> Self {
        Self {
            command: create.command.as_ref(),
            cwd: create.cwd.as_deref(),
            env: &create.env,
            agent_session: create.agent_session.as_ref(),
            native_launch: create.native_launch.as_ref(),
            role: "seed pane",
        }
    }

    fn pane(pane: &'a model::WorkspacePane) -> Self {
        Self {
            command: pane.command.as_ref(),
            cwd: pane.cwd.as_deref(),
            env: &pane.env,
            agent_session: pane.agent_session.as_ref(),
            native_launch: pane.native_launch.as_ref(),
            role: "restored pane",
        }
    }
}

/// The launch spec one archived pane is stored with: its resolved native
/// agent session (argv, directory, environment, and the provenance the
/// server installs at each launch) when it has one, else its own command.
fn pane_launch(
    source: &PaneSource<'_>,
    session: &str,
    warnings: &mut Vec<String>,
) -> Result<LaunchSpec, String> {
    if let Some(native) = source.native_launch {
        let mut spec = native.clone();
        session.clone_into(&mut spec.session);
        spec.guardian = None;
        spec.guardian_key = None;
        spec.guardian_expected = None;
        let marker = serde_json::to_string(&spec)
            .map_err(|error| format!("could not encode native launch: {error}"))?;
        return Ok(LaunchSpec::new(Vec::new())
            .with_cwd(source.cwd.map(str::to_owned))
            .with_env(vec![("PHUX_NATIVE_HOST_SPEC".to_owned(), marker)]));
    }
    let prepared = prepare_pane_agent(source.agent_session, source.cwd, source.role, warnings)?;
    let Some(session) = prepared else {
        return Ok(LaunchSpec::new(source.command.cloned().unwrap_or_default())
            .with_cwd(source.cwd.map(str::to_owned))
            .with_env(source.env.clone().into_iter().collect()));
    };
    let restore = session
        .record
        .encode()
        .map_err(|err| format!("could not encode agent session provenance: {err}"))?;
    Ok(LaunchSpec::new(session.argv.clone())
        .with_cwd(Some(session.cwd.display().to_string()))
        .with_env(session.env.clone().into_iter().collect())
        .with_restore(Some(restore)))
}

/// Create every archived pane of the imported session `create` as a dormant
/// resource record and place them in its arrangement as the archive laid
/// them out. A failure removes the records made and deletes the session, so
/// a failed import leaves nothing behind. Returns the agent fallback
/// warnings.
async fn import_dormant_panes(
    conn: &mut Connection,
    create: &model::CreateRequest,
) -> Result<Vec<String>, String> {
    let session = phux_client::org::fetch(conn, OrgFilter::All, 1)
        .await
        .map_err(|err| format!("could not read the session records: {err}"))?
        .session_named(&create.name)
        .map(|record| record.key)
        .ok_or_else(|| format!("session {:?} is gone", create.name))?;
    let mut warnings = Vec::new();
    let mut made = Vec::new();
    let result = match create_dormant_records(conn, session, create, &mut made, &mut warnings).await
    {
        Ok(keys) if keys.iter().all(Vec::is_empty) => Ok(()),
        Ok(keys) => {
            let document = imported_arrangement(&create.windows, &keys);
            phux_client::org::replace_session_arrangement(conn, session, &document)
                .await
                .map(drop)
                .map_err(|err| format!("could not arrange the imported panes: {err}"))
        }
        Err(err) => Err(err),
    };
    if let Err(err) = result {
        for key in made {
            let _ = phux_client::org::remove_resource(conn, key).await;
        }
        let _ = phux_client::org::delete_session(conn, session).await;
        return Err(err);
    }
    Ok(warnings)
}

/// Create the dormant records, one row of keys per archived window. A
/// session with no windows but a seed command or agent gets one seed pane.
async fn create_dormant_records(
    conn: &mut Connection,
    session: SessionKey,
    create: &model::CreateRequest,
    made: &mut Vec<ResourceKey>,
    warnings: &mut Vec<String>,
) -> Result<Vec<Vec<ResourceKey>>, String> {
    let mut rows: Vec<Vec<PaneSource<'_>>> = create
        .windows
        .iter()
        .enumerate()
        .map(|(window_index, window)| {
            window
                .panes
                .iter()
                .enumerate()
                .map(|(pane_index, pane)| {
                    if create.seed_position == Some((window_index, pane_index)) {
                        PaneSource::seed(create)
                    } else {
                        PaneSource::pane(pane)
                    }
                })
                .collect()
        })
        .collect();
    if create.windows.is_empty()
        && (create.command.is_some()
            || create.agent_session.is_some()
            || create.native_launch.is_some())
    {
        rows.push(vec![PaneSource::seed(create)]);
    }
    let mut keys = Vec::with_capacity(rows.len());
    for row in rows {
        let mut row_keys = Vec::with_capacity(row.len());
        for source in row {
            let launch = pane_launch(&source, &create.name, warnings)?;
            let committed = phux_client::org::create_resource(
                conn,
                session,
                ResourceKind::Terminal,
                Some(launch),
                LaunchMode::Deferred,
            )
            .await
            .map_err(|err| format!("could not import an archived pane: {err}"))?;
            let key = committed
                .resource
                .ok_or_else(|| "the server created no pane record".to_owned())?;
            made.push(key);
            row_keys.push(key);
        }
        keys.push(row_keys);
    }
    Ok(keys)
}

/// The imported session's arrangement: one group per archived window with
/// panes, its archived split tree mapped onto that window's `keys` (a linear
/// chain without a captured layout, or for a corrupt one).
fn imported_arrangement(windows: &[WorkspaceWindow], keys: &[Vec<ResourceKey>]) -> WireDoc {
    let groups = keys
        .iter()
        .enumerate()
        .filter_map(|(index, row)| {
            let window = windows.get(index);
            let tree = window
                .and_then(|window| window.layout.as_ref())
                .and_then(|node| wire_node_from_archive(node, row))
                .or_else(|| wire_chain(row))?;
            Some(ArrangementGroup {
                id: phux_client::layout::fresh_id(),
                name: window
                    .map(|window| window.name.clone())
                    .filter(|name| !name.is_empty()),
                tree,
            })
        })
        .collect();
    WireDoc { groups }
}

fn wire_leaf(key: ResourceKey) -> ArrangementNode {
    ArrangementNode::Leaf(EntryRef {
        entry: phux_client::layout::fresh_id(),
        target: EntryTarget::Member(key),
    })
}

/// [`layout_node_from_archive`] in key space.
fn wire_node_from_archive(
    node: &model::WorkspaceLayoutNode,
    keys: &[ResourceKey],
) -> Option<ArrangementNode> {
    match node {
        model::WorkspaceLayoutNode::Pane { pane } => keys.get(*pane).copied().map(wire_leaf),
        model::WorkspaceLayoutNode::Split {
            dir,
            ratio,
            left,
            right,
        } => Some(ArrangementNode::Split {
            dir: match dir {
                model::WorkspaceSplitDir::Horizontal => SplitDirection::Horizontal,
                model::WorkspaceSplitDir::Vertical => SplitDirection::Vertical,
            },
            ratio: ratio.clamp(0.05, 0.95),
            first: Box::new(wire_node_from_archive(left, keys)?),
            second: Box::new(wire_node_from_archive(right, keys)?),
        }),
    }
}

/// [`linear_chain`] in key space; `None` for no keys.
fn wire_chain(keys: &[ResourceKey]) -> Option<ArrangementNode> {
    let mut rest = keys.iter().rev();
    let mut tree = wire_leaf(*rest.next()?);
    for key in rest {
        tree = ArrangementNode::Split {
            dir: SplitDirection::Horizontal,
            ratio: 0.5,
            first: Box::new(wire_leaf(*key)),
            second: Box::new(tree),
        };
    }
    Some(tree)
}

/// Relaunch the dormant panes of the imported session `name` in place.
async fn relaunch_dormant_session(
    socket_path: &Path,
    name: &str,
) -> Result<SessionOutcome, String> {
    let results = phux_client::org::restore_session_on(socket_path, name)
        .await
        .map_err(|err| format!("could not relaunch its panes: {err}"))?;
    let refused: Vec<String> = results
        .into_iter()
        .filter_map(|(key, result)| result.err().map(|err| format!("{key}: {err}")))
        .collect();
    if refused.is_empty() {
        Ok(SessionOutcome {
            warnings: Vec::new(),
        })
    } else {
        Err(format!(
            "could not relaunch its panes: {}",
            refused.join("; ")
        ))
    }
}

/// Attributes belong to the session record, not the legacy singleton tags.
fn session_attrs(create: &model::CreateRequest, kind: ImportKind<'_>) -> Vec<(String, String)> {
    let project_root = match kind {
        ImportKind::Archive => None,
        ImportKind::Project { directory } => Some(directory.to_string_lossy().into_owned()),
    };
    [
        ("host", create.host.as_deref()),
        ("project", create.project.as_deref()),
        (
            phux_core::project_identity::attr::LINK,
            create.project_link.as_deref(),
        ),
        (
            "directory",
            project_root.as_deref().or(create.directory.as_deref()),
        ),
    ]
    .into_iter()
    .filter_map(|(key, value)| {
        value
            .filter(|value| !value.is_empty())
            .map(|value| (key.to_owned(), value.to_owned()))
    })
    .collect()
}

/// Write a recreated session's archived host, project, and directory.
async fn tag_session(socket_path: &Path, create: &model::CreateRequest) -> Result<(), String> {
    if let Some(host) = create.host.as_deref().filter(|host| !host.is_empty()) {
        phux_client::workspace_place::set_session_host(socket_path, &create.name, host)
            .await
            .map_err(|err| err.to_string())?;
    }
    if let Some(project) = create
        .project
        .as_deref()
        .filter(|project| !project.is_empty())
    {
        phux_client::workspace_place::set_session_project(socket_path, &create.name, project)
            .await
            .map_err(|err| err.to_string())?;
    }
    if let Some(directory) = create.directory.as_deref().filter(|cwd| !cwd.is_empty()) {
        phux_client::workspace_place::set_session_directory(socket_path, &create.name, directory)
            .await
            .map_err(|err| err.to_string())?;
    }
    Ok(())
}

/// Start `launch` as a new Terminal member of the existing session `name`
/// (`RESOURCE_CREATE`), returning its live id.
async fn launch_into_session(
    socket_path: &Path,
    name: &str,
    launch: LaunchSpec,
) -> Result<ResourceId, String> {
    let mut conn = Connection::connect(socket_path)
        .await
        .map_err(|err| format!("could not reach the server: {err}"))?;
    let inventory = phux_client::org::fetch(&mut conn, OrgFilter::All, 1)
        .await
        .map_err(|err| format!("could not read the session records: {err}"))?;
    let session = inventory
        .session_named(name)
        .map(|record| record.key)
        .ok_or_else(|| format!("session {name:?} is gone"))?;
    let committed = phux_client::org::create_resource(
        &mut conn,
        session,
        ResourceKind::Terminal,
        Some(launch),
        LaunchMode::Immediate,
    )
    .await
    .map_err(|err| format!("could not start the seed pane: {err}"))?;
    drop(conn);
    committed
        .target
        .map(|target| target.resource)
        .ok_or_else(|| "the server started no seed pane".to_owned())
}

/// Read the archive document (a path or `-` for stdin) and parse it.
fn load_archive(archive_path: &Path) -> Result<model::WorkspaceArchive, ExitCode> {
    let input = read_archive_text(archive_path).map_err(|err| fail(&err))?;
    parse_archive(&input).map_err(|err| fail(&err))
}

/// One archived session's restore, past the point where partial failure
/// stops mattering to any other session.
struct SessionOutcome {
    /// Non-fatal notices for this session (e.g. an agent session that
    /// could not be resumed, downgraded to a plain shell).
    warnings: Vec<String>,
}

/// Start one imported session's seed pane (resuming its native agent
/// session when possible, else a plain shell with a warning), then replay the
/// rest of its split tree with fresh window identities (ADR-0129). Windows with
/// no captured layout (schema 1) are placed as a linear chain. Any failure past
/// the seed rolls back every pane created for the session.
async fn relaunch_one_session(
    socket_path: &Path,
    create: model::CreateRequest,
) -> Result<SessionOutcome, String> {
    let mut warnings = Vec::new();
    let windows = create.windows.clone();
    let seed_position = create.seed_position;

    let seed_prepared = prepare_pane_agent(
        create.agent_session.as_ref(),
        create.cwd.as_deref(),
        "seed pane",
        &mut warnings,
    )?;
    let command = seed_prepared.as_ref().map_or_else(
        || create.command.clone(),
        |session| Some(session.argv.clone()),
    );
    let env = seed_prepared
        .as_ref()
        .map_or_else(|| create.env.clone(), |session| session.env.clone());
    let cwd = seed_prepared.as_ref().map_or_else(
        || create.cwd.clone(),
        |session| Some(session.cwd.display().to_string()),
    );
    let agent_session_bytes = seed_prepared
        .as_ref()
        .map(|session| session.record.encode())
        .transpose()
        .map_err(|err| format!("could not encode agent session provenance: {err}"))?;

    let launch = LaunchSpec::new(command.unwrap_or_default())
        .with_cwd(cwd)
        .with_env(env.into_iter().collect())
        .with_restore(agent_session_bytes);
    let seed = launch_into_session(socket_path, &create.name, launch).await?;
    let mut created_panes = vec![seed.clone()];
    let created = &mut created_panes;

    if let Some(prepared) = &seed_prepared
        && let Err(err) = confirm_restored_agent(socket_path, &seed, prepared).await
    {
        // `confirm_restored_agent` already tried to remove the seed pane on
        // its own failure, and nothing else has been created yet.
        return Err(err);
    }

    if let Err(err) = replay_or_keep_local(
        socket_path,
        &seed,
        seed_position,
        &windows,
        create.host.as_deref(),
        created,
        &mut warnings,
    )
    .await
    {
        rollback_session(socket_path, created).await;
        return Err(err);
    }
    Ok(SessionOutcome { warnings })
}

/// Resolve one pane's archived native agent session. A prep failure is a
/// warning and the pane falls back to a plain shell.
fn prepare_pane_agent(
    agent_session: Option<&model::WorkspaceAgentSession>,
    cwd: Option<&str>,
    role: &str,
    warnings: &mut Vec<String>,
) -> Result<Option<PreparedAgentSession>, String> {
    let Some(record) = agent_session else {
        return Ok(None);
    };
    match prepare_archived_agent(record, cwd) {
        Ok(prepared) => Ok(Some(prepared)),
        // A different plugin now owning the integration is a security boundary:
        // fail the restore rather than hand the resume to it.
        Err(PrepareAgentError::OwnershipMismatch(err)) => Err(err),
        Err(PrepareAgentError::Other(err)) => {
            warnings.push(format!(
                "{role}: could not resume native agent session ({}): {err}; spawned a plain \
                 shell instead",
                record.native_id
            ));
            Ok(None)
        }
    }
}

/// Kill every pane created so far for a failed session; an already-dead id
/// is a no-op.
async fn rollback_session(socket_path: &Path, created: &[ResourceId]) {
    if created.is_empty() {
        return;
    }
    if let Err(err) = crate::commands::request_command(
        socket_path,
        Command::KillResources {
            ids: created.to_vec(),
            operation_id: None,
        },
    )
    .await
    {
        warn!("archive: could not roll back a failed session's panes: {err}");
    }
}

/// Place archived panes on `host` when this server can reach it.
///
/// A missing satellite route, or a hosted archive with no pane to send
/// there, keeps the session on this server. The seed pane already has the
/// archived directory, and the caller still writes the project tag. Any
/// other failure still rolls the session back.
async fn replay_or_keep_local(
    socket_path: &Path,
    seed: &ResourceId,
    seed_position: Option<(usize, usize)>,
    windows: &[WorkspaceWindow],
    host: Option<&str>,
    created: &mut Vec<ResourceId>,
    warnings: &mut Vec<String>,
) -> Result<(), String> {
    match replay_split_tree(
        socket_path,
        seed,
        seed_position,
        windows,
        host,
        created,
        warnings,
    )
    .await
    {
        Ok(()) => Ok(()),
        Err(err) if keep_local_when_host_unroutable(host, &err) => {
            let host_name = host.unwrap_or("");
            warnings.push(format!(
                "could not place this session on host {host_name}: {err}; kept the local session and its host tag"
            ));
            // The seed pane already exists. Spawning the tree again with no
            // satellite would drop the archived host.
            Ok(())
        }
        Err(err) => Err(err),
    }
}

/// A hosted restore that this server cannot route still has a local seed.
fn keep_local_when_host_unroutable(host: Option<&str>, err: &str) -> bool {
    if host.is_none_or(str::is_empty) {
        return false;
    }
    err.contains("no route to that satellite") || err.contains("no pane to restore there")
}

/// Recreate the archived panes and write the session's arrangement.
///
/// A local session keeps the seed pane the session-create call already
/// made. A session saved on a satellite spawns every archived pane there,
/// including that seed: a local owner cannot cross the link. Each spawned
/// pane is pushed onto `created` for rollback.
async fn replay_split_tree(
    socket_path: &Path,
    seed: &ResourceId,
    seed_position: Option<(usize, usize)>,
    windows: &[WorkspaceWindow],
    host: Option<&str>,
    created: &mut Vec<ResourceId>,
    warnings: &mut Vec<String>,
) -> Result<(), String> {
    if windows.is_empty() {
        return hosted_without_panes(host);
    }

    let hosted = host.is_some_and(|host| !host.is_empty());
    let mut pane_ids: Vec<Vec<Option<ResourceId>>> = windows
        .iter()
        .map(|window| vec![None; window.panes.len()])
        .collect();
    if !hosted
        && let Some((window_index, pane_index)) = seed_position
        && let Some(slot) = pane_ids
            .get_mut(window_index)
            .and_then(|row| row.get_mut(pane_index))
    {
        *slot = Some(seed.clone());
    }
    let mut remote_owner: Option<ResourceId> = None;
    for (window_index, window) in windows.iter().enumerate() {
        for (pane_index, pane) in window.panes.iter().enumerate() {
            if pane_ids[window_index][pane_index].is_some() {
                continue;
            }
            let owner = spawn_owner(hosted, seed, remote_owner.as_ref());
            let spawned =
                spawn_owned_pane(socket_path, owner, pane, host, created, warnings).await?;
            if hosted {
                remote_owner.get_or_insert_with(|| spawned.clone());
            }
            pane_ids[window_index][pane_index] = Some(spawned);
        }
    }

    let workspace = build_restored_workspace(windows, &pane_ids)?;
    write_restored_layout(socket_path, seed, workspace).await
}

/// A hosted archive with no panes has nowhere to put the host.
fn hosted_without_panes(host: Option<&str>) -> Result<(), String> {
    let Some(host) = host.filter(|host| !host.is_empty()) else {
        return Ok(());
    };
    Err(format!(
        "session was saved on host {host:?} but the archive has no pane to restore there"
    ))
}

/// Local panes join the seed. The first satellite pane has no owner on that
/// host yet; later panes join the first one spawned there.
const fn spawn_owner<'a>(
    hosted: bool,
    seed: &'a ResourceId,
    remote_owner: Option<&'a ResourceId>,
) -> Option<&'a ResourceId> {
    if hosted { remote_owner } else { Some(seed) }
}

/// Spawn one archived pane, resuming its agent session when possible.
///
/// `owner` `None` asks the satellite to place the pane. A local owner keeps
/// the pane in that pane's session.
async fn spawn_owned_pane(
    socket_path: &Path,
    owner: Option<&ResourceId>,
    pane: &model::WorkspacePane,
    host: Option<&str>,
    created: &mut Vec<ResourceId>,
    warnings: &mut Vec<String>,
) -> Result<ResourceId, String> {
    let prepared = prepare_pane_agent(
        pane.agent_session.as_ref(),
        pane.cwd.as_deref(),
        "restored pane",
        warnings,
    )?;
    let (command, cwd, env) = prepared.as_ref().map_or_else(
        || {
            let env =
                (!pane.env.is_empty()).then(|| pane.env.clone().into_iter().collect::<Vec<_>>());
            (pane.command.clone(), pane.cwd.clone(), env)
        },
        |session| {
            (
                Some(session.argv.clone()),
                Some(session.cwd.display().to_string()),
                Some(session.env.clone().into_iter().collect::<Vec<_>>()),
            )
        },
    );
    let frame = phux_client::workspace_place::restored_spawn_frame(owner, command, cwd, env, host);
    let spawned = match spawn_with_agent_session_on(socket_path, &frame, None).await {
        Ok(SpawnResult::Ok(id)) => id,
        Ok(SpawnResult::Err(err)) => {
            return Err(format!(
                "archive relaunch could not recreate an archived pane: {}",
                phux_client::spawn::spawn_error_message(&err)
            ));
        }
        Ok(_) => {
            return Err(
                "archive relaunch: unrecognized spawn result for an archived pane".to_owned(),
            );
        }
        Err(err) => {
            return Err(format!(
                "archive relaunch: could not recreate an archived pane: {err}"
            ));
        }
    };
    // Record the pane as soon as it exists, so a later failure still rolls it
    // back.
    created.push(spawned.clone());
    if let Some(prepared) = &prepared
        && let Err(err) = confirm_restored_agent(socket_path, &spawned, prepared).await
    {
        return Err(format!(
            "could not confirm a restored pane's resumed agent session: {err}"
        ));
    }
    Ok(spawned)
}

/// Build the restored session's `ArrangementDoc` envelope, mapping each archived
/// layout onto the fresh pane ids (linear chain without a captured layout).
fn build_restored_workspace(
    windows: &[WorkspaceWindow],
    pane_ids: &[Vec<Option<ResourceId>>],
) -> Result<ArrangementDoc, String> {
    let mut built = Vec::with_capacity(windows.len());
    let mut active_index = 0usize;
    for (window_index, window) in windows.iter().enumerate() {
        let ids: Vec<ResourceId> = pane_ids[window_index]
            .iter()
            .cloned()
            .collect::<Option<_>>()
            .ok_or_else(|| "archive relaunch: an archived pane was not recreated".to_owned())?;
        let Some(first) = ids.first().cloned() else {
            continue;
        };
        let tree = window
            .layout
            .as_ref()
            .and_then(|node| layout_node_from_archive(node, &ids))
            .unwrap_or_else(|| linear_chain(&ids));
        let focus = window
            .panes
            .iter()
            .position(|pane| pane.active)
            .and_then(|pane_index| ids.get(pane_index).cloned())
            .or(Some(first));
        if window.active {
            active_index = built.len();
        }
        built.push(Group::new(
            window.name.clone(),
            LayoutState {
                tree: Some(tree),
                focus,
            },
        ));
    }
    if built.is_empty() {
        return Err("archive relaunch: the archive named no panes to place".to_owned());
    }
    Ok(ArrangementDoc {
        groups: built,
        active: active_index,
    })
}

/// Map one archived `WorkspaceLayoutNode` onto `ids` (indexed exactly as the
/// archive's own pane list was), or `None` when a pane index is out of
/// range for a corrupt or hand-edited archive.
fn layout_node_from_archive(
    node: &model::WorkspaceLayoutNode,
    ids: &[ResourceId],
) -> Option<LayoutNode> {
    match node {
        model::WorkspaceLayoutNode::Pane { pane } => ids.get(*pane).cloned().map(LayoutNode::Leaf),
        model::WorkspaceLayoutNode::Split {
            dir,
            ratio,
            left,
            right,
        } => Some(LayoutNode::Split {
            dir: match dir {
                model::WorkspaceSplitDir::Horizontal => SplitDir::Horizontal,
                model::WorkspaceSplitDir::Vertical => SplitDir::Vertical,
            },
            ratio: *ratio,
            left: Box::new(layout_node_from_archive(left, ids)?),
            right: Box::new(layout_node_from_archive(right, ids)?),
        }),
    }
}

/// Place a window's panes in a right-leaning chain when no layout was
/// captured; also used by `snapshot::unplaced_window`.
pub(super) fn linear_chain(ids: &[ResourceId]) -> LayoutNode {
    let mut rest = ids.iter().rev();
    let Some(last) = rest.next() else {
        // `build_restored_workspace` never calls this with an empty `ids`
        // (it skips a window with no panes before reaching here), but a
        // single fallback leaf keeps this total instead of panicking.
        return LayoutNode::Leaf(ResourceId::local(0));
    };
    let mut tree = LayoutNode::Leaf(last.clone());
    for id in rest {
        tree = LayoutNode::Split {
            dir: SplitDir::Horizontal,
            ratio: 0.5,
            left: Box::new(LayoutNode::Leaf(id.clone())),
            right: Box::new(tree),
        };
    }
    tree
}

/// Write the restored `ArrangementDoc` as the seed's session arrangement
/// (`ArrangementReplace`, compare-and-swap), so a refused write is reported
/// instead of leaving panes with no layout. Only reached after
/// [`import_archive`] confirmed the server supports `ORGANIZATION`.
async fn write_restored_layout(
    socket_path: &Path,
    seed: &ResourceId,
    workspace: ArrangementDoc,
) -> Result<(), String> {
    phux_client::org::replace_arrangement_of_member(socket_path, seed, &workspace)
        .await
        .map(drop)
        .map_err(|err| format!("archive: could not write layout: {err}"))
}

/// [`prepare_archived_agent`]'s failure, split by whether it is a security
/// boundary (never silently downgrade) or an ordinary "could not resolve"
/// gap (a warning and a plain shell are the right recovery, review item 6).
enum PrepareAgentError {
    /// The integration now resolves to a plugin other than the one that
    /// originally owned this native agent session.
    OwnershipMismatch(String),
    /// Any other reason (malformed identity, the integration/plugin no
    /// longer resolves at all, a working-directory failure, ...).
    Other(String),
}

impl From<String> for PrepareAgentError {
    fn from(message: String) -> Self {
        Self::Other(message)
    }
}

fn prepare_archived_agent(
    archived: &model::WorkspaceAgentSession,
    cwd: Option<&str>,
) -> Result<PreparedAgentSession, PrepareAgentError> {
    let record = AgentSessionRecord::new(
        &archived.plugin_id,
        &archived.integration_id,
        &archived.native_id,
    )?;
    let workspace_cwd = match cwd {
        Some(cwd) => PathBuf::from(cwd),
        None => std::env::current_dir()
            .map_err(|err| format!("could not resolve restore working directory: {err}"))?,
    };
    let resolved = phux_plugin::resolve_launch(
        &phux_config::loader::config_path(),
        &record.integration_id,
        &[],
        &workspace_cwd,
        phux_plugin::LaunchMode::Terminal,
    )
    .map_err(|err| {
        format!(
            "cannot restore native agent session for integration '{}': {err}",
            record.integration_id
        )
    })?;
    if resolved.plugin_id != record.plugin_id {
        return Err(PrepareAgentError::OwnershipMismatch(format!(
            "cannot restore native agent session '{}': integration '{}' now resolves to plugin '{}', not owning plugin '{}'",
            record.native_id, record.integration_id, resolved.plugin_id, record.plugin_id
        )));
    }
    prepare(&resolved, &record.native_id).map_err(PrepareAgentError::Other)
}

async fn confirm_restored_agent(
    socket_path: &Path,
    terminal: &ResourceId,
    prepared: &PreparedAgentSession,
) -> Result<(), String> {
    phux_client::agent_session_record::confirm_agent_session_record_on(
        socket_path,
        terminal,
        &prepared.record,
        10,
    )
    .await
}

fn read_archive_text(path: &Path) -> Result<String, String> {
    if path == Path::new("-") {
        let mut input = String::new();
        io::stdin()
            .read_to_string(&mut input)
            .map_err(|err| format!("could not read workspace archive from stdin: {err}"))?;
        return Ok(input);
    }
    fs::read_to_string(path)
        .map_err(|err| format!("could not read workspace archive {}: {err}", path.display()))
}

/// The session names already on the server, for restore's collision check.
/// Session lists never aggregate across a federation, so degradation cannot
/// hide a name.
async fn fetch_existing_sessions(socket_path: &Path, verb: &str) -> Result<Vec<String>, ExitCode> {
    phux_client::state::get_state(socket_path)
        .await
        .map(|view| {
            view.into_snapshot_ignoring_degradation()
                .sessions
                .into_iter()
                .map(|session| session.name)
                .collect()
        })
        .map_err(|err| report_no_server(&err, socket_path, verb))
}

fn fail(message: &str) -> ExitCode {
    error!("{message}");
    ExitCode::FAILURE
}

fn same_local_terminals(
    before: &phux_protocol::wire::info::SessionSnapshot,
    after: &phux_protocol::wire::info::SessionSnapshot,
) -> bool {
    let local_ids = |snapshot: &phux_protocol::wire::info::SessionSnapshot| {
        snapshot
            .resources
            .iter()
            .filter(|pane| matches!(pane.id, ResourceId::Local { .. }))
            .map(|pane| pane.id.clone())
            .collect::<HashSet<_>>()
    };
    local_ids(before) == local_ids(after)
}

#[cfg(test)]
mod tests {
    use phux_protocol::ids::{SessionId, WindowId};
    use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot};

    use super::*;

    fn snapshot(ids: &[u32]) -> SessionSnapshot {
        SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
            .with_resources(
                ids.iter()
                    .map(|id| ResourceInfo::new(ResourceId::local(*id), WindowId::new(1), 80, 24))
                    .collect(),
            )
    }

    #[test]
    fn a_restored_session_carries_its_project_link_beside_its_label() {
        let archive = model::parse_archive(
            r#"{"schema_version": 2, "sessions": [
                {"name": "api", "project": "api", "project_link": "phux", "windows": []}
            ]}"#,
        )
        .expect("archive parses");
        let plan = model::restore_plan(&archive, &[]).expect("restore plan");
        let attrs = session_attrs(&plan.creates[0], ImportKind::Archive);
        assert!(attrs.contains(&("project".to_owned(), "api".to_owned())));
        assert!(attrs.contains(&(
            phux_core::project_identity::attr::LINK.to_owned(),
            "phux".to_owned()
        )));
    }

    #[test]
    fn save_guard_rejects_reaped_or_new_local_terminals() {
        assert!(same_local_terminals(&snapshot(&[1, 2]), &snapshot(&[2, 1])));
        assert!(!same_local_terminals(&snapshot(&[1, 2]), &snapshot(&[1])));
        assert!(!same_local_terminals(
            &snapshot(&[1, 2]),
            &snapshot(&[1, 2, 3])
        ));
    }

    fn pane_grid(rows: &[&[u32]]) -> Vec<Vec<Option<ResourceId>>> {
        rows.iter()
            .map(|row| row.iter().map(|id| Some(ResourceId::local(*id))).collect())
            .collect()
    }

    fn window(
        name: &str,
        active: bool,
        panes: usize,
        layout: Option<model::WorkspaceLayoutNode>,
    ) -> WorkspaceWindow {
        WorkspaceWindow {
            name: name.to_owned(),
            active,
            layout,
            panes: (0..panes)
                .map(|i| model::WorkspacePane {
                    active: i == 0,
                    title: None,
                    cwd: None,
                    command: None,
                    agent_session: None,
                    native_launch: None,
                    env: std::collections::BTreeMap::new(),
                    cols: 0,
                    rows: 0,
                })
                .collect(),
        }
    }

    /// ADR-0129: replaying the archived split tree produces one window per
    /// archived window, each with a fresh identity, and a leaf per archived
    /// pane at the position the archive named — not just one preferred pane.
    #[test]
    fn build_restored_workspace_replays_the_split_tree_with_fresh_window_ids() {
        let split = model::WorkspaceLayoutNode::Split {
            dir: model::WorkspaceSplitDir::Vertical,
            ratio: 0.6,
            left: Box::new(model::WorkspaceLayoutNode::Pane { pane: 0 }),
            right: Box::new(model::WorkspaceLayoutNode::Pane { pane: 1 }),
        };
        let windows = vec![
            window("main", true, 2, Some(split)),
            window("logs", false, 1, None),
        ];
        let pane_ids = pane_grid(&[&[10, 11], &[12]]);

        let workspace = build_restored_workspace(&windows, &pane_ids).expect("builds");
        assert_eq!(workspace.groups.len(), 2);
        assert_eq!(
            workspace.active, 0,
            "the archived active window stays active"
        );

        let main = &workspace.groups[0];
        assert_eq!(main.name, "main");
        assert!(matches!(
            main.state.tree,
            Some(LayoutNode::Split {
                dir: SplitDir::Vertical,
                ..
            })
        ));
        assert_eq!(
            phux_client::layout::leaves(main.state.tree.as_ref().unwrap()),
            vec![ResourceId::local(10), ResourceId::local(11)]
        );

        let logs = &workspace.groups[1];
        assert_eq!(logs.name, "logs");
        assert_eq!(
            phux_client::layout::leaves(logs.state.tree.as_ref().unwrap()),
            vec![ResourceId::local(12)],
            "a window with no captured layout still places every archived pane"
        );

        assert_ne!(
            main.id, logs.id,
            "each restored window gets its own fresh identity"
        );
        assert_ne!(
            main.id, [0; 16],
            "a fresh window identity is never the zero sentinel"
        );
    }

    #[test]
    fn linear_chain_places_every_pane_when_no_layout_was_captured() {
        let ids = vec![
            ResourceId::local(1),
            ResourceId::local(2),
            ResourceId::local(3),
        ];
        let tree = linear_chain(&ids);
        assert_eq!(phux_client::layout::leaves(&tree), ids);
    }

    #[test]
    fn an_unroutable_host_keeps_the_local_session() {
        let err = "archive relaunch could not recreate an archived pane: spawn failed: no route to that satellite";
        assert!(keep_local_when_host_unroutable(Some("edge"), err));
        assert!(keep_local_when_host_unroutable(
            Some("edge"),
            "session was saved on host \"edge\" but the archive has no pane to restore there"
        ));
        assert!(!keep_local_when_host_unroutable(None, err));
        assert!(!keep_local_when_host_unroutable(Some(""), err));
        assert!(!keep_local_when_host_unroutable(
            Some("edge"),
            "archive relaunch could not recreate an archived pane: spawn failed: permission denied"
        ));
    }

    #[test]
    fn layout_node_from_archive_rejects_an_out_of_range_pane_index() {
        let node = model::WorkspaceLayoutNode::Pane { pane: 5 };
        let ids = [ResourceId::local(1)];
        assert!(layout_node_from_archive(&node, &ids).is_none());
    }

    /// Against an `ORGANIZATION` server the archive reads and writes the
    /// typed arrangement, never the `phux.tui.layout/v1` key (ADR-0161 §9).
    mod organization {
        use phux_client::layout::wire::wire_leaves;
        use phux_client::testkit::{OrgFixture, ScriptSpec, serve_one};
        use phux_protocol::caps::{ServerFeatureExt, ServerFeatureExtSet};
        use phux_protocol::wire::frame::{FrameKind, SessionLifetime};

        use super::*;

        fn member(key: ResourceKey, n: u8) -> ArrangementNode {
            ArrangementNode::Leaf(EntryRef {
                entry: [n; 16],
                target: EntryTarget::Member(key),
            })
        }

        /// A session with two shells, `a` arranged alone in group "main".
        fn arranged() -> (OrgFixture, SessionKey, ResourceKey, ResourceKey) {
            arranged_as(Some("main"), true)
        }

        /// A session with two shells and a replaced arrangement: `a` alone
        /// in one group named `name`, or no group at all.
        fn arranged_as(
            name: Option<&str>,
            placed: bool,
        ) -> (OrgFixture, SessionKey, ResourceKey, ResourceKey) {
            let mut fixture = OrgFixture::new();
            let work = fixture.session("work", SessionLifetime::Persistent);
            let a = fixture.shell(work);
            let b = fixture.shell(work);
            let groups = placed
                .then(|| ArrangementGroup {
                    id: [7; 16],
                    name: name.map(str::to_owned),
                    tree: member(a, 1),
                })
                .into_iter()
                .collect();
            fixture.arrangement(work, WireDoc { groups });
            (fixture, work, a, b)
        }

        /// The organization records `fixture` serves, read as an export does.
        async fn records_of(fixture: OrgFixture) -> phux_client::org::OrgInventory {
            let (socket, server, _dir) = serve(fixture);
            let records = phux_client::org::fetch_from_socket(&socket)
                .await
                .expect("records")
                .expect("an ORGANIZATION server");
            server.await.expect("server");
            records
        }

        /// Export's layout read against `fixture`, with every frame sent.
        async fn export_layouts(
            fixture: OrgFixture,
            projection: Option<&str>,
        ) -> (SessionLayouts, Vec<String>, Vec<FrameKind>) {
            let snapshot = fixture.snapshot();
            let records = records_of(fixture.clone()).await;
            let (socket, server, _dir) = serve(fixture);
            let (layouts, warnings) =
                fetch_session_layouts(&socket, &snapshot, projection, Some(&records)).await;
            (layouts, warnings, server.await.expect("server"))
        }

        fn serve(
            fixture: OrgFixture,
        ) -> (
            PathBuf,
            tokio::task::JoinHandle<Vec<FrameKind>>,
            tempfile::TempDir,
        ) {
            let dir = tempfile::tempdir().expect("tempdir");
            let spec = ScriptSpec::new()
                .server_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::Organization]))
                .organization(fixture);
            let (socket, server) = serve_one(dir.path(), spec);
            (socket, server, dir)
        }

        fn commands(seen: &[FrameKind]) -> Vec<&Command> {
            seen.iter()
                .filter_map(|frame| match frame {
                    FrameKind::Command { command, .. } => Some(command),
                    _ => None,
                })
                .collect()
        }

        fn touches_layout_metadata(seen: &[FrameKind]) -> bool {
            seen.iter().any(|frame| {
                matches!(
                    frame,
                    FrameKind::GetMetadata { .. } | FrameKind::SetMetadata { .. }
                )
            })
        }

        #[tokio::test]
        async fn export_reads_each_sessions_arrangement() {
            let (fixture, work, a, _b) = arranged();
            let session = fixture.live_session(&work).expect("live session");
            let pane_a = fixture.live(&a).expect("live pane");

            let (layouts, warnings, seen) = export_layouts(fixture, None).await;

            assert!(warnings.is_empty(), "{warnings:?}");
            let layout = layouts.get(&session).expect("the session's arrangement");
            assert_eq!(layout.groups.len(), 1);
            assert_eq!(layout.groups[0].name, "main");
            assert_eq!(
                phux_client::layout::leaves(layout.groups[0].state.tree.as_ref().expect("tree")),
                vec![pane_a]
            );
            assert!(
                commands(&seen)
                    .iter()
                    .any(|command| matches!(command, Command::ArrangementGet { .. })),
                "{seen:?}"
            );
            assert!(!touches_layout_metadata(&seen), "{seen:?}");
        }

        /// A named projection is its own metadata key even on an
        /// `ORGANIZATION` server; it never reads the arrangement.
        #[tokio::test]
        async fn export_under_a_projection_reads_its_key() {
            let (fixture, ..) = arranged();

            let (layouts, warnings, seen) = export_layouts(fixture, Some("myapp.layout/v1")).await;

            assert!(layouts.is_empty());
            assert_eq!(warnings.len(), 1, "{warnings:?}");
            assert!(
                seen.iter()
                    .any(|frame| matches!(frame, FrameKind::GetMetadata { key, .. } if key.starts_with("myapp.layout/v1/"))),
                "{seen:?}"
            );
            assert!(commands(&seen).is_empty(), "{seen:?}");
        }

        /// An unnamed group stays unnamed in the archive rather than taking
        /// its position as a name an import would then keep.
        #[tokio::test]
        async fn export_keeps_an_unnamed_group_unnamed() {
            let (fixture, work, ..) = arranged_as(None, true);
            let session = fixture.live_session(&work).expect("live session");

            let (layouts, ..) = export_layouts(fixture, None).await;

            let layout = layouts.get(&session).expect("the session's arrangement");
            assert_eq!(layout.groups[0].name, "");
        }

        /// Every session has an arrangement from creation; one nobody has
        /// arranged exports as the one-window projection, with no warning,
        /// as it did before it had a layout key.
        #[tokio::test]
        async fn an_empty_arrangement_exports_as_one_window() {
            let (fixture, ..) = arranged_as(None, false);
            let snapshot = fixture.snapshot().with_windows(Vec::new());

            let (layouts, layout_warnings, _seen) = export_layouts(fixture, None).await;
            let (archive, warnings) = archive_from_snapshot(
                &snapshot,
                snapshot::AgentRecords {
                    attached: &HashMap::new(),
                    native: &HashMap::new(),
                },
                &layouts,
                &HashMap::new(),
                &HashMap::new(),
                &HashMap::new(),
            );

            assert!(layouts.is_empty());
            assert!(layout_warnings.is_empty(), "{layout_warnings:?}");
            assert!(warnings.is_empty(), "{warnings:?}");
            let windows = &archive.sessions[0].windows;
            assert_eq!(windows.len(), 1);
            assert_eq!(windows[0].name, "window-0");
            assert_eq!(windows[0].panes.len(), 2);
        }

        #[tokio::test]
        async fn restore_replaces_the_seed_sessions_arrangement() {
            let (fixture, _work, a, b) = arranged();
            let (pane_a, pane_b) = (
                fixture.live(&a).expect("live pane"),
                fixture.live(&b).expect("live pane"),
            );
            let (socket, server, _dir) = serve(fixture);
            let workspace = ArrangementDoc {
                groups: vec![Group::new(
                    "restored".to_owned(),
                    LayoutState {
                        tree: Some(LayoutNode::Split {
                            dir: SplitDir::Vertical,
                            ratio: 0.5,
                            left: Box::new(LayoutNode::Leaf(pane_a.clone())),
                            right: Box::new(LayoutNode::Leaf(pane_b)),
                        }),
                        focus: Some(pane_a.clone()),
                    },
                )],
                active: 0,
            };

            write_restored_layout(&socket, &pane_a, workspace)
                .await
                .expect("the arrangement is replaced");

            let seen = server.await.expect("server");
            let replaced: Vec<&WireDoc> = commands(&seen)
                .into_iter()
                .filter_map(|command| match command {
                    Command::ArrangementReplace { document, .. } => Some(document),
                    _ => None,
                })
                .collect();
            assert_eq!(replaced.len(), 1, "{seen:?}");
            assert_eq!(replaced[0].groups[0].name.as_deref(), Some("restored"));
            let targets: Vec<EntryTarget> = wire_leaves(&replaced[0].groups[0].tree)
                .into_iter()
                .map(|entry| entry.target)
                .collect();
            assert_eq!(
                targets,
                vec![EntryTarget::Member(a), EntryTarget::Member(b)]
            );
            assert!(!touches_layout_metadata(&seen), "{seen:?}");
        }
    }
}
