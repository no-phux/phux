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
use phux_client::layout::{ArrangementDoc, LayoutNode, SplitDir};
use phux_client::layout_ops::LayoutOps;
use phux_config::socket::default_socket_path;
use phux_protocol::ids::{ResourceId, ResourceKey, ResourceKind, SessionKey};
use phux_protocol::wire::frame::{
    ArrangementDoc as WireDoc, ArrangementGroup, ArrangementNode, Command, EntryRef, EntryTarget,
    LaunchMode, LaunchSpec, OrgFilter, SplitDirection,
};

use phux_client::agent_session_record::fetch_record_index;

use crate::commands::{ArchiveAction, cli_runtime, partial, report_no_server};

mod bridge;
pub(crate) mod model;
mod places;
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

include!("archive/export.rs");

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
    let mut sessions = Vec::with_capacity(plan.creates.len());
    let mut failed = Vec::new();
    let mut name_taken = HashSet::new();
    let mut warnings = Vec::new();
    for create in plan.creates {
        let imported = create_imported_session(socket_path, &create, kind, extra).await;
        match imported {
            Ok(outcome) => {
                warnings.extend(outcome.warnings.into_iter().map(|message| {
                    model::RestoreWarning {
                        session: create.name.clone(),
                        message,
                    }
                }));
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
        warnings,
        skipped_existing: plan.skipped_existing,
        failed,
        name_taken,
    })
}

/// Relaunch imported local dormant records through executing-host admission.
/// Remote archived intent stays inert: never disguise it as a fresh raw spawn.
pub(crate) async fn relaunch_imported(socket_path: &Path, imported: Imported) -> RestoreSummary {
    let mut restored = Vec::with_capacity(imported.sessions.len());
    let mut failed = imported.failed;
    let mut warnings = imported.warnings;
    for create in imported.sessions {
        let name = create.name.clone();
        let outcome = if create.host.as_deref().is_some_and(|host| !host.is_empty()) {
            Err("restore this archived intent on its executing host; no local or remote raw fallback was attempted".to_owned())
        } else {
            relaunch_dormant_session(socket_path, &name).await
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


include!("archive/records.rs");


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



/// Place a window's panes in a right-leaning chain when no layout was
/// captured; also used by `snapshot::unplaced_window`.
pub(super) fn linear_chain(ids: &[ResourceId]) -> LayoutNode {
    let mut rest = ids.iter().rev();
    let Some(last) = rest.next() else {
        // An empty layout has no real resource identity; callers supply panes.
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

include!("archive/tests.rs");
