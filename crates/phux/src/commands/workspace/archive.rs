use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_client::attach::connection::Connection;
use phux_client::layout::{LayoutNode, LayoutState, SplitDir, WindowState, Workspace};
use phux_client::layout_ops::LayoutOps;
use phux_protocol::ids::{GroupId, ResourceId};
use phux_protocol::wire::frame::{Command, FrameKind, SpawnResult};
use phux_server::runtime::default_socket_path;

use crate::commands::agent::{
    AgentSessionRecord, PreparedAgentSession, fetch_record_index, prepare,
};
use crate::commands::new::{create_session_via_metadata, preflight_atomic_agent_session_create};
use crate::commands::spawn::{dispatch_spawn_async, report_spawn_error};
use crate::commands::{cli_runtime, partial, report_no_server};

mod model;
mod snapshot;

use model::{ARCHIVE_SCHEMA_VERSION, RestoreSummary, WorkspaceWindow, parse_archive, restore_plan};
use snapshot::archive_from_snapshot;

pub(super) fn run_save(
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
    let (snapshot, degradation) = match rt.block_on(phux_client::state::get_state(&socket_path)) {
        Ok(view) => view.into_parts(),
        Err(err) => return report_no_server(&err, &socket_path, "workspace save"),
    };
    // A save writes a file the user will restore *later*, so an incomplete
    // capture is the one degradation that outlives the command: panes on an
    // unreachable satellite are simply not in the archive, and nothing at
    // restore time can tell they were meant to be. Still not a failure —
    // refusing to snapshot this laptop because a remote box is down would be
    // worse — but it has to be said out loud before the file lands.
    partial::warn_partial_view("workspace save", &degradation);
    let agent_sessions = match rt.block_on(fetch_record_index(&socket_path, &snapshot)) {
        Ok(index) => index,
        Err(err) => return fail(&format!("could not capture native agent sessions: {err}")),
    };
    // ADR-0129 / PHA-406 L18 review item 9: `GET_STATE`'s `WindowInfo.layout`
    // is never populated by the reference server, so the only place a
    // session's real split tree lives is its own L3 layout envelope. Read
    // it per session (best-effort — a session that never had one, or an
    // undecodable value, just falls back to the bare pane-list projection
    // `archive_from_snapshot` already produced for every session before
    // this lane).
    let (layouts, layout_warnings) =
        rt.block_on(fetch_session_layouts(&socket_path, &snapshot, projection));
    let confirmation = match rt.block_on(phux_client::state::get_state(&socket_path)) {
        Ok(view) => view.into_snapshot_ignoring_degradation(),
        Err(err) => return report_no_server(&err, &socket_path, "workspace save"),
    };
    if !same_local_terminals(&snapshot, &confirmation) {
        return fail(
            "workspace changed while native agent sessions were captured; retry workspace save",
        );
    }
    let (archive, reconcile_warnings) = archive_from_snapshot(&snapshot, &agent_sessions, &layouts);
    for warning in layout_warnings.iter().chain(&reconcile_warnings) {
        eprintln!("phux: warning: {warning}");
    }
    let rendered = match serde_json::to_string_pretty(&archive) {
        Ok(rendered) => rendered,
        Err(err) => return fail(&format!("could not render workspace archive: {err}")),
    };
    if let Some(path) = output {
        match fs::write(path, rendered) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => fail(&format!("could not write {}: {err}", path.display())),
        }
    } else {
        outln!("{rendered}");
        ExitCode::SUCCESS
    }
}

/// Validate a `workspace save --projection` value.
///
/// Unlike every other `--projection` flag (`insert-pane`, `move-pane`,
/// `swap-pane`, `spawn`/`launch`'s placement), which takes one exact
/// `<prefix>.layout/v1/<session-id>` key, `workspace save` addresses many
/// sessions at once and so takes the bare *prefix* — `<prefix>.layout/v1`
/// — appending `/<session-id>` itself per session. Passing a full key
/// here (with the id already appended) used to exit 0 having silently
/// found nothing at every session's `<key>/<id>` and fallen back to the
/// registry-only projection for all of them.
///
/// # Errors
///
/// A ready-to-print message when `value` is empty, has an empty prefix
/// before `.layout/v1`, or does not end in `.layout/v1` at all (most
/// commonly because it already carries a trailing session id).
fn validate_save_projection_prefix(value: &str) -> Result<(), String> {
    const SUFFIX: &str = ".layout/v1";
    let Some(prefix) = value.strip_suffix(SUFFIX) else {
        return Err(format!(
            "--projection {value:?} does not end in \"{SUFFIX}\": `workspace save` takes the \
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
    eprintln!("phux: {message}");
    ExitCode::from(2)
}

/// Best-effort per-session L3 layout read (ADR-0129 review item 9): for
/// every session in `snapshot`, read `<prefix>.layout/v1/<session-id>`
/// (`prefix` from `--projection`, else the shared default) over one
/// dedicated connection and decode it. A session with nothing stored under
/// the *default* key, or a value that doesn't decode as the current v3
/// envelope, is simply absent from the returned map —
/// `archive_from_snapshot` falls back to its registry-window projection
/// for exactly those sessions, the same projection every session used
/// before this lane. When `projection` names an explicit key, the same
/// absence is instead surfaced as a warning naming the session (review
/// item 4): a caller who asked for a specific projection is more likely
/// to have mistyped it than to be fine with a silent fallback.
async fn fetch_session_layouts(
    socket_path: &Path,
    snapshot: &phux_protocol::wire::info::SessionSnapshot,
    projection: Option<&str>,
) -> (
    HashMap<phux_protocol::ids::SessionId, phux_client::layout::Workspace>,
    Vec<String>,
) {
    let mut layouts = HashMap::new();
    let mut warnings = Vec::new();
    let Ok(mut conn) = Connection::connect(socket_path).await else {
        return (layouts, warnings);
    };
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
                "workspace save: session {:?} has no layout stored at projection key {key:?}",
                session.name
            ));
        }
    }
    (layouts, warnings)
}

pub(super) fn run_restore(archive_path: &Path, socket: Option<PathBuf>) -> ExitCode {
    let archive = match load_archive(archive_path) {
        Ok(archive) => archive,
        Err(code) => return code,
    };
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    let existing = match rt.block_on(fetch_existing_sessions(&socket_path)) {
        Ok(existing) => existing,
        Err(code) => return code,
    };
    let plan = match restore_plan(&archive, &existing) {
        Ok(plan) => plan,
        Err(err) => return fail(&err),
    };
    // Only `create_session_via_metadata`'s embedded-agent-session path needs
    // the atomic-create guarantee, and only a session's *seed* pane ever
    // takes that path (every other archived pane is an ordinary
    // `SPAWN_RESOURCE` + a follow-up record write, same as `phux launch`).
    if plan
        .creates
        .iter()
        .any(|create| create.agent_session.is_some())
        && let Err(code) = rt.block_on(preflight_atomic_agent_session_create(&socket_path))
    {
        return code;
    }

    // PHA-406 L18 review item 5: a session's restore failure never aborts
    // the rest of the archive. It rolls back that one session (every pane
    // created for it so far) and the loop continues; the summary names
    // both the restored and the failed sessions, and the process exits
    // non-zero iff anything failed.
    let mut restored = Vec::with_capacity(plan.creates.len());
    let mut failed = Vec::new();
    let mut warnings = Vec::new();
    for create in plan.creates {
        let name = create.name.clone();
        match rt.block_on(restore_one_session(&socket_path, create)) {
            Ok(outcome) => {
                warnings.extend(outcome.warnings.into_iter().map(|message| {
                    model::RestoreWarning {
                        session: name.clone(),
                        message,
                    }
                }));
                restored.push(name);
            }
            Err(reason) => {
                eprintln!("phux: workspace restore: session {name:?} failed: {reason}");
                failed.push(model::FailedRestore { name, reason });
            }
        }
    }

    let any_failed = !failed.is_empty();
    let print_code = render_restore_summary(&RestoreSummary {
        schema_version: ARCHIVE_SCHEMA_VERSION,
        restored,
        skipped_existing: plan.skipped_existing,
        failed,
        warnings,
    });
    if any_failed {
        ExitCode::FAILURE
    } else {
        print_code
    }
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

/// Create one archived session's seed pane (resuming its native agent
/// session when the archive carries one and it can still be prepared —
/// otherwise a plain shell, noted as a warning, PHA-406 L18 review item 6),
/// then replay the rest of its archived split tree (ADR-0129): every other
/// pane is spawned owned by the seed pane and placed to mirror the
/// captured `WorkspaceLayoutNode` tree, with fresh window identities. This
/// runs for every schema version — `restore_plan` copies a session's
/// `windows` unconditionally — so a schema-1 archive still replays every
/// archived pane, just placed in a simple linear chain rather than a real
/// captured tree (schema 1 predates captured layouts entirely, so its
/// windows carry panes but no `layout`, exactly like a schema-2 window
/// archived before this lane). Only a session with no captured windows at
/// all leaves the seed pane as the session's sole content.
///
/// On any failure past the seed's own creation, every pane created for this
/// session so far (the seed included) is rolled back (review item 5) before
/// the error is returned; the caller does not need to repeat that cleanup.
async fn restore_one_session(
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
        .map_or_else(BTreeMap::new, |session| session.env.clone());
    let cwd = seed_prepared.as_ref().map_or_else(
        || create.cwd.clone(),
        |session| Some(session.cwd.display().to_string()),
    );
    let agent_session_bytes = seed_prepared
        .as_ref()
        .map(|session| session.record.encode())
        .transpose()
        .map_err(|err| format!("could not encode agent session provenance: {err}"))?;
    let agent_session_preflighted = seed_prepared.is_some();

    let pane_id = create_session_via_metadata(
        &crate::commands::server_target::ServerTarget::local(socket_path),
        &create.name,
        command,
        cwd,
        env,
        agent_session_bytes,
        agent_session_preflighted,
        false,
        None,
    )
    .await
    .map_err(|_| "could not create the seed pane (see the diagnostic above)".to_owned())?;

    let Ok(seed_local) = u32::try_from(pane_id) else {
        return Err("restored terminal id exceeds the local wire-id range".to_owned());
    };
    let seed = ResourceId::local(seed_local);
    let mut created = vec![seed.clone()];

    if let Some(prepared) = &seed_prepared
        && let Err(err) = confirm_restored_agent(socket_path, &seed, prepared).await
    {
        // `confirm_restored_agent` already tried to remove the seed pane on
        // its own failure, and nothing else has been created yet.
        return Err(err);
    }

    match replay_split_tree(
        socket_path,
        &seed,
        seed_position,
        &windows,
        &mut created,
        &mut warnings,
    )
    .await
    {
        Ok(()) => Ok(SessionOutcome { warnings }),
        Err(err) => {
            rollback_session(socket_path, &created).await;
            Err(err)
        }
    }
}

/// Resolve one pane's archived native agent session, if any. A prep failure
/// is a warning, not a fatal error (review item 6): the caller falls back
/// to a plain shell (or the archive's own `command`/`cwd`) rather than
/// failing the whole session over one unresolvable integration.
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
        // A different plugin now owning the integration is a security
        // boundary, not a "could not resolve" gap — fail the restore
        // rather than silently handing the resume to a plugin that never
        // owned it (pinned by
        // `native_agent_session_is_replayed_after_pane_restart_and_rejects_stale_ownership`).
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

/// Best-effort cleanup for one session whose restore failed partway
/// through: kill every pane created for it so far (the seed and any panes
/// spawned while replaying its split tree), so a failed session leaves no
/// half-restored trace behind (PHA-406 L18 review item 5). Killing an id
/// that already died on its own (e.g. `confirm_restored_agent`'s own
/// cleanup) is a documented no-op, not an error.
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
        eprintln!("phux: workspace restore: could not roll back a failed session's panes: {err}");
    }
}

/// Recreate every archived pane beyond the seed pane and write the session's
/// default layout envelope to place them exactly as archived. A no-op when
/// the archive named no windows (schema 1, or a session archived with no
/// captured layout at all) — the seed pane already stands for the session.
/// Every pane this creates is pushed onto `created` as it is spawned, so a
/// caller that aborts partway through can roll back exactly what exists.
async fn replay_split_tree(
    socket_path: &Path,
    seed: &ResourceId,
    seed_position: Option<(usize, usize)>,
    windows: &[WorkspaceWindow],
    created: &mut Vec<ResourceId>,
    warnings: &mut Vec<String>,
) -> Result<(), String> {
    if windows.is_empty() {
        return Ok(());
    }

    let mut pane_ids: Vec<Vec<Option<ResourceId>>> = windows
        .iter()
        .map(|window| vec![None; window.panes.len()])
        .collect();
    if let Some((window_index, pane_index)) = seed_position
        && let Some(slot) = pane_ids
            .get_mut(window_index)
            .and_then(|row| row.get_mut(pane_index))
    {
        *slot = Some(seed.clone());
    }
    for (window_index, window) in windows.iter().enumerate() {
        for (pane_index, pane) in window.panes.iter().enumerate() {
            if pane_ids[window_index][pane_index].is_some() {
                continue;
            }
            let spawned = spawn_owned_pane(socket_path, seed, pane, created, warnings).await?;
            pane_ids[window_index][pane_index] = Some(spawned);
        }
    }

    let workspace = build_restored_workspace(windows, &pane_ids)?;
    write_restored_layout(socket_path, seed, workspace).await
}

/// Spawn one archived pane, owned by the session's seed pane, resuming its
/// archived native agent session when possible (review item 6). Owning it
/// through the seed pane keeps it in the seed's session
/// (`SPAWN_RESOURCE.owner_terminal`) without recreating the seed itself.
async fn spawn_owned_pane(
    socket_path: &Path,
    owner: &ResourceId,
    pane: &model::WorkspacePane,
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
        || (pane.command.clone(), pane.cwd.clone(), None),
        |session| {
            (
                Some(session.argv.clone()),
                Some(session.cwd.display().to_string()),
                Some(session.env.clone().into_iter().collect::<Vec<_>>()),
            )
        },
    );
    let frame = FrameKind::SpawnResource {
        request_id: 1,
        group: GroupId::new(1),
        command,
        cwd,
        env,
        term: None,
        satellite: None,
        owner_terminal: Some(owner.clone()),
        agent_session: None,
        initial_size: None,
        resource: None,
    };
    let spawned = match dispatch_spawn_async(socket_path, &frame, None).await {
        Ok(SpawnResult::Ok(id)) => id,
        Ok(SpawnResult::Err(err)) => {
            report_spawn_error(&err);
            return Err("workspace restore could not recreate an archived pane".to_owned());
        }
        Ok(_) => {
            return Err(
                "workspace restore: unrecognized spawn result for an archived pane".to_owned(),
            );
        }
        Err(err) => {
            return Err(format!(
                "workspace restore: could not recreate an archived pane: {err}"
            ));
        }
    };
    // Record the pane the instant the server confirms it exists, before the
    // follow-up agent-session confirmation below — a transport failure
    // confirming that write must still roll this pane back (review item
    // 5b), not leak it because the failure happened after the spawn but
    // before this function otherwise had a chance to report the id back to
    // its caller.
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

/// Build the `Workspace` envelope value to write for a restored session,
/// mapping each archived window's `WorkspaceLayoutNode` onto the freshly
/// spawned pane ids at the same positions (falling back to a linear chain
/// when a window carries no captured layout). Window identities are fresh,
/// derived from each window's own first leaf (`phux-client-core`'s
/// `layout::identity`, via `WindowState::new`).
fn build_restored_workspace(
    windows: &[WorkspaceWindow],
    pane_ids: &[Vec<Option<ResourceId>>],
) -> Result<Workspace, String> {
    let mut built = Vec::with_capacity(windows.len());
    let mut active_index = 0usize;
    for (window_index, window) in windows.iter().enumerate() {
        let ids: Vec<ResourceId> = pane_ids[window_index]
            .iter()
            .cloned()
            .collect::<Option<_>>()
            .ok_or_else(|| "workspace restore: an archived pane was not recreated".to_owned())?;
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
        built.push(WindowState::new(
            window.name.clone(),
            LayoutState {
                tree: Some(tree),
                focus,
            },
        ));
    }
    if built.is_empty() {
        return Err("workspace restore: the archive named no panes to place".to_owned());
    }
    Ok(Workspace {
        windows: built,
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

/// A window with no captured layout (schema 1, or a window archived before
/// this feature) still has every archived pane; place them in a simple
/// right-leaning chain rather than dropping any of them. Also shared by
/// `snapshot::unplaced_window` (ADR-0129 review items 1+2) to place a live
/// pane a reconciled layout had nowhere else to put.
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

/// Write the freshly built `Workspace` as the seed pane's session's default
/// layout envelope, through `LayoutOps`'s write-then-confirming-read (review
/// item 7) rather than a fire-and-forget `SET_METADATA` — the session was
/// just created and has never been attached, so this is always a fresh
/// write, never a read-modify-write, but the confirming read is what turns
/// a silently-dropped over-cap write into a reported restore failure
/// instead of a session with panes and no layout.
async fn write_restored_layout(
    socket_path: &Path,
    seed: &ResourceId,
    workspace: Workspace,
) -> Result<(), String> {
    let snapshot = phux_client::state::get_state(socket_path)
        .await
        .map_err(|err| format!("workspace restore: could not read state: {err}"))?
        .into_snapshot_ignoring_degradation();
    let window_id = snapshot
        .resources
        .iter()
        .find(|resource| &resource.id == seed)
        .map(|resource| resource.window_id)
        .ok_or_else(|| {
            "workspace restore: the seed pane vanished before its layout could be written"
                .to_owned()
        })?;
    let session_id = snapshot
        .windows
        .iter()
        .find(|window| window.id == window_id)
        .map(|window| window.session_id)
        .ok_or_else(|| {
            "workspace restore: the seed pane's session could not be resolved".to_owned()
        })?;
    phux_client::layout_ops::write_layout_on(socket_path, session_id, &workspace, 1)
        .await
        .map_err(|err| format!("workspace restore: could not write layout: {err}"))
}

/// Emit the restore summary document on stdout.
fn render_restore_summary(summary: &RestoreSummary) -> ExitCode {
    match serde_json::to_string_pretty(summary) {
        Ok(rendered) => {
            outln!("{rendered}");
            ExitCode::SUCCESS
        }
        Err(err) => fail(&format!("could not render restore summary: {err}")),
    }
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
///
/// `into_snapshot_ignoring_degradation`: this reads `sessions` only, and a
/// satellite's session list never enters the merge —
/// `handle_get_state_federated` discards it because its `u32` ids would
/// collide with the hub's. An unreachable satellite therefore cannot hide a
/// name this check would otherwise catch.
async fn fetch_existing_sessions(socket_path: &Path) -> Result<Vec<String>, ExitCode> {
    phux_client::state::get_state(socket_path)
        .await
        .map(|view| {
            view.into_snapshot_ignoring_degradation()
                .sessions
                .into_iter()
                .map(|session| session.name)
                .collect()
        })
        .map_err(|err| report_no_server(&err, socket_path, "workspace restore"))
}

fn fail(message: &str) -> ExitCode {
    eprintln!("phux: {message}");
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
        assert_eq!(workspace.windows.len(), 2);
        assert_eq!(
            workspace.active, 0,
            "the archived active window stays active"
        );

        let main = &workspace.windows[0];
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

        let logs = &workspace.windows[1];
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
    fn layout_node_from_archive_rejects_an_out_of_range_pane_index() {
        let node = model::WorkspaceLayoutNode::Pane { pane: 5 };
        let ids = [ResourceId::local(1)];
        assert!(layout_node_from_archive(&node, &ids).is_none());
    }
}
