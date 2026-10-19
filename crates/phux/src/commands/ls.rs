use std::collections::HashMap;
use std::process::ExitCode;

use phux_client::agent_meta::AgentRecord;
use phux_client::glance::{self, MemberGlance, SessionGlance, Sources};
use phux_client::org::{KeyNames, OrgInventory};
use phux_client::resource;
use phux_client::state::{Degradation, StateView};
use phux_core::activation::SessionLifetime;
use phux_core::project_identity::SessionProject;
use phux_core::session_list::{ExecCounts, SessionJson, SessionListJson};

use phux_protocol::ids::{ResourceId, SatelliteHost, SessionId};
use phux_protocol::wire::info::{
    HostInventory, HostSessionInfo, ResourceInfo, SessionInfo, SessionSnapshot,
};

use crate::commands::json_err::{self, CliError, codes};
use crate::commands::partial;
use crate::commands::server_target::{ServerSpec, ServerTarget};
use phux_core::host_list::{HostJson, HostKind, HostListJson};

/// How deep `phux ls` reads (`docs/consumers/attention.md`): the glance by
/// default, one session's terminals with `SESSION`, and keys, revisions, and
/// runs with `-l`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Depth {
    /// List this session's terminals instead of every session.
    pub session: Option<String>,
    /// Add keys, lifetimes, windows, and runs; never collapse.
    pub detail: bool,
}

impl Depth {
    /// The depth `phux ls [SESSION] [-l]` asks for.
    pub(crate) const fn of(session: Option<String>, detail: bool) -> Self {
        Self { session, detail }
    }

    /// Whether the text needs each terminal's agent record. The detail
    /// listing of every session prints none, so it skips the reads.
    const fn reads_agents(&self) -> bool {
        self.session.is_some() || !self.detail
    }
}

/// First request id for the agent-record reads, clear of the listing's
/// `GET_STATE`, layout, and organization reads.
const AGENT_READ_FIRST_REQUEST_ID: u32 = 0x8000_0000;

/// `phux ls` — list sessions via `GET_STATE`; never auto-starts a server.
/// `--json` emits the stable [`SessionListJson`] (ADR-0022), complete at
/// every depth. A partial listing from a degraded hub still exits 0,
/// reporting the gap on stderr or in the `unreachable` list (see
/// [`partial`]).
pub(crate) fn run_ls(json: bool, depth: &Depth, server: ServerSpec) -> ExitCode {
    let (rt, target) = match server.prepare("ls", json) {
        Ok(prepared) => prepared,
        Err(code) => return code,
    };
    let listing = rt.block_on(async {
        let mut conn = target.connect().await?;
        // The glance prints no window counts, so it skips their reads.
        let view = if json || depth.detail {
            phux_client::session_list::listing_view(&mut conn).await?
        } else {
            phux_client::session_list::glance_view(&mut conn).await?
        };
        let agents = if json || !depth.reads_agents() {
            HashMap::new()
        } else {
            let panes = agent_panes(view.snapshot(), depth.session.as_deref());
            // The listing already warns about a partial view; a record a
            // degraded hub could not read only leaves its agent unnamed.
            phux_client::agent_record::fetch_index_on(
                &mut conn,
                &panes,
                AGENT_READ_FIRST_REQUEST_ID,
                &mut Vec::new(),
            )
            .await
        };
        drop(conn);
        Ok::<_, phux_client::attach::AttachError>((view, agents))
    });
    match listing {
        Ok((view, agents)) => render_listing(json, depth, view, &agents),
        Err(err) => target.report_unreachable(json, &err, "ls"),
    }
}

/// The terminals whose agent records the text needs: one session's, or
/// every local one.
fn agent_panes(snapshot: &SessionSnapshot, session: Option<&str>) -> Vec<ResourceId> {
    let session = session.and_then(|name| snapshot.sessions.iter().find(|s| s.name == name));
    session.map_or_else(
        || {
            resource::terminals(snapshot)
                .filter(|pane| pane.id.is_local())
                .map(|pane| pane.id.clone())
                .collect()
        },
        |session| {
            resource::terminals_in(snapshot, session.id)
                .map(|pane| pane.id.clone())
                .collect()
        },
    )
}

/// How long `phux ls --all` waits on any one host. A tailnet peer answers
/// in well under a second; one that has not answered by now is reported as
/// unreachable rather than holding up every other machine's rows.
const HOST_DEADLINE: std::time::Duration = std::time::Duration::from_secs(3);

/// `phux ls --all`: this machine and every registered host, grouped by
/// machine (`phux.hosts/v1` with `json`).
///
/// Every host is queried at once and each gets [`HOST_DEADLINE`], so the
/// listing costs one slow host's deadline at worst, not the sum. A host that
/// cannot be planned, dialed, or listed keeps its row with the reason. The
/// exit status is 0 whenever the listing itself could be produced: "mini is
/// down" is a true answer about mini, not a failure of `ls`.
pub(crate) fn run_ls_all(json: bool, socket: Option<std::path::PathBuf>) -> ExitCode {
    let rt = match super::cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    let registry = match super::remote::load_registry() {
        Ok(entries) => entries,
        Err(err) => {
            let err = CliError::new(
                codes::REMOTE_UNRESOLVED,
                format!("could not read the host registry: {err}"),
                "fix the config (`phux config check`), then retry",
            );
            return json_err::emit(json, &err, 1);
        }
    };
    let socket = socket.unwrap_or_else(phux_server::runtime::default_socket_path);
    let mut probes: Vec<HostProbe> = vec![HostProbe {
        row: host_row("local", &local_hostname(), HostKind::Local, None),
        target: Ok(ServerTarget::local(&socket)),
    }];
    for entry in &registry {
        probes.push(HostProbe {
            row: host_row(
                &entry.name,
                &entry.name,
                HostKind::Remote,
                Some(entry.endpoint.clone()),
            ),
            target: super::server_target::plan_entry(&rt, entry, "ls")
                .map(|remote| ServerTarget::Remote(Box::new(remote)))
                .map_err(|err| err.message),
        });
    }
    // Client connections are `!Send` (current-thread runtime, ADR-0003), so
    // the probes run concurrently as local tasks rather than on a pool.
    let local = tokio::task::LocalSet::new();
    let hosts = local.block_on(&rt, async {
        let handles: Vec<_> = probes
            .into_iter()
            .map(|probe| tokio::task::spawn_local(probe.run()))
            .collect();
        let mut rows = Vec::with_capacity(handles.len());
        for handle in handles {
            if let Ok(row) = handle.await {
                rows.push(row);
            }
        }
        rows
    });
    let doc = HostListJson::new(hosts);
    if json {
        return match serde_json::to_string_pretty(&doc) {
            Ok(text) => {
                outln!("{text}");
                ExitCode::SUCCESS
            }
            Err(err) => {
                error!("failed to serialize host list as JSON: {err}");
                ExitCode::FAILURE
            }
        };
    }
    for line in host_list_lines(&doc) {
        outln!("{line}");
    }
    ExitCode::SUCCESS
}

/// One machine to list: its row, pre-filled, and how to reach it (or why it
/// cannot be reached before a dial is even tried).
struct HostProbe {
    row: HostJson,
    target: Result<ServerTarget, String>,
}

impl HostProbe {
    async fn run(self) -> HostJson {
        let Self { mut row, target } = self;
        let target = match target {
            Ok(target) => target,
            Err(err) => {
                row.error = Some(err);
                return row;
            }
        };
        let listing = async {
            let mut conn = target.connect().await?;
            phux_client::session_list::listing_view(&mut conn).await
        };
        match tokio::time::timeout(HOST_DEADLINE, listing).await {
            Ok(Ok(view)) => {
                let organization = view.organization().cloned();
                let (snapshot, _) = view.into_parts();
                let mut sessions: Vec<_> = snapshot
                    .sessions
                    .iter()
                    .map(|s| {
                        phux_client::session_list::session_json(&snapshot, organization.as_ref(), s)
                    })
                    .collect();
                sessions.sort_by(|a, b| a.name.cmp(&b.name));
                row.reachable = true;
                row.sessions = sessions;
            }
            Ok(Err(err)) => row.error = Some(unreachable_reason(&err)),
            Err(_) => {
                row.error = Some(format!(
                    "did not answer within {}s",
                    HOST_DEADLINE.as_secs()
                ));
            }
        }
        row
    }
}

/// Why a host did not list, in the words `phux ls` uses: a socket with no
/// server behind it is "no server running", not an attach-loop I/O error.
fn unreachable_reason(err: &phux_client::attach::AttachError) -> String {
    match err {
        phux_client::attach::AttachError::Io(io)
            if matches!(
                io.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
            ) =>
        {
            "no server running".to_owned()
        }
        other => other.to_string(),
    }
}

fn host_row(name: &str, label: &str, kind: HostKind, endpoint: Option<String>) -> HostJson {
    HostJson {
        name: name.to_owned(),
        label: label.to_owned(),
        kind,
        endpoint,
        reachable: false,
        error: None,
        sessions: Vec::new(),
    }
}

/// This machine's short hostname (`mac.local` becomes `mac`), or `local`.
pub(crate) fn local_hostname() -> String {
    phux_config::instance::local_host_name()
}

/// The human `--all` listing: one header per machine, its sessions' glance
/// lines indented beneath, and an unreachable machine's reason in place of
/// its sessions.
fn host_list_lines(doc: &HostListJson) -> Vec<String> {
    let mut lines = Vec::new();
    for host in &doc.hosts {
        let header = match host.kind {
            HostKind::Local => format!("{} (this machine)", host.label),
            HostKind::Remote => host.label.clone(),
        };
        if !host.reachable {
            let why = host.error.as_deref().unwrap_or("unreachable");
            lines.push(format!("{header}  unreachable: {why}"));
            continue;
        }
        lines.push(header);
        if host.sessions.is_empty() {
            lines.push("  (no sessions)".to_owned());
        }
        let rows = host
            .sessions
            .iter()
            .map(|row| {
                let project = RowProject {
                    identity: row.project.clone(),
                    link: row.project_link.clone(),
                };
                (glance_of_row(row), project)
            })
            .collect();
        lines.extend(
            collapse_by_project(rows)
                .into_iter()
                .map(|line| format!("  {line}")),
        );
    }
    lines
}

/// Print one fetched listing in the requested shape.
fn render_listing(
    json: bool,
    depth: &Depth,
    view: StateView,
    agents: &HashMap<ResourceId, AgentRecord>,
) -> ExitCode {
    let hosts_complete = view.host_sessions_complete();
    let organization = view.organization().cloned();
    let (snapshot, degradation) = view.into_parts();
    if json {
        // Not stderr: a `--json` consumer's channel is the document.
        return print_sessions_json(
            &snapshot,
            organization.as_ref(),
            &degradation,
            hosts_complete,
        );
    }
    let sources = Sources {
        snapshot: &snapshot,
        organization: organization.as_ref(),
        agents,
    };
    if let Some(name) = depth.session.as_deref() {
        let code = print_one_session(sources, name, depth.detail);
        partial::warn_partial_view("ls", &degradation);
        return code;
    }
    if depth.detail {
        print_sessions(&snapshot, organization.as_ref());
    } else {
        print_lines(glance_lines(sources));
    }
    if !hosts_complete && has_satellite_terminals(&snapshot) {
        warn!("{HOST_SESSIONS_UNAVAILABLE}");
    }
    partial::warn_partial_view("ls", &degradation);
    ExitCode::SUCCESS
}

/// The text-mode note for a hub that does not report its satellites'
/// sessions (no `HOST_SESSIONS`): the satellite Terminals it does list are
/// real, but which sessions they belong to cannot be known from here.
const HOST_SESSIONS_UNAVAILABLE: &str = "ls: satellite sessions are unavailable from this \
     server (it does not report HOST_SESSIONS); its satellite terminals are listed above";

fn has_satellite_terminals(snapshot: &SessionSnapshot) -> bool {
    resource::terminals(snapshot).any(|pane| pane.id.host().is_some())
}

/// The empty listing, in the `phux host ls` mold: say what is missing,
/// then name the exact next commands. Stdout (it answers the question asked)
/// and exit 0 (an empty enumeration is a true answer, not a failure — the
/// non-zero case is *no server*, which never reaches this).
const EMPTY_STATE: [&str; 2] = [
    "No sessions.",
    "Start one with `phux` (attaches, auto-starting a server), or `phux new NAME -- CMD`.",
];

/// Render the session list, one line per session (tmux-`ls`-ish), followed
/// by satellite Terminals that cannot be joined to hub-local sessions. An
/// empty server prints [`EMPTY_STATE`] instead of nothing.
pub(crate) fn print_sessions(snapshot: &SessionSnapshot, organization: Option<&OrgInventory>) {
    print_lines(session_lines(snapshot, organization));
}

/// Print `lines`, or [`EMPTY_STATE`] when there are none.
fn print_lines(lines: Vec<String>) {
    if lines.is_empty() {
        for line in EMPTY_STATE {
            outln!("{line}");
        }
        return;
    }
    for line in lines {
        outln!("{line}");
    }
}

/// The `phux ls -l` body: name-sorted session lines with windows, lifetime,
/// and key, each followed by its panes' agent sessions, then satellite
/// Terminals. Empty exactly when there is nothing to list ([`EMPTY_STATE`]).
/// A hub reporting a host inventory renders grouped by host
/// ([`host_grouped_lines`]). `organization` supplies each session's
/// lifetime and execution counts when the server reports them.
fn session_lines(snapshot: &SessionSnapshot, organization: Option<&OrgInventory>) -> Vec<String> {
    with_hosts(snapshot, local_session_lines(snapshot, organization))
}

/// The default `phux ls` body: one glance line per session, sessions that
/// need you first, collapsed past [`GLANCE_ROWS`] ([`collapse`]), grouped
/// by host like [`session_lines`]. Intake is left out while it holds
/// nothing.
fn glance_lines(sources: Sources<'_>) -> Vec<String> {
    let rows = glance::sessions(sources)
        .into_iter()
        .map(|(row, _)| row)
        .filter(|row| row.to_organize != Some(0))
        .map(|row| {
            let session = sources
                .snapshot
                .sessions
                .iter()
                .find(|session| session.name == row.name);
            let project = RowProject {
                identity: session.and_then(phux_client::project_identity::of_session),
                link: session
                    .and_then(phux_client::project_identity::link_of_session)
                    .map(str::to_owned),
            };
            (row, project)
        })
        .collect();
    let local = collapse_by_project(rows);
    let snapshot = sources.snapshot;
    if snapshot.hosts().is_empty() {
        let mut lines = local;
        lines.extend(satellite_terminal_glance_lines(sources));
        return lines;
    }
    host_grouped_lines(snapshot, local, |host| {
        satellite_glance_lines(sources, host)
    })
}

/// `local` (this host's lines) placed in the listing's host shape: flat,
/// or under [`LOCAL_HOST_LABEL`] beside each satellite's group.
fn with_hosts(snapshot: &SessionSnapshot, local: Vec<String>) -> Vec<String> {
    if snapshot.hosts().is_empty() {
        return flat_session_lines(snapshot, local);
    }
    host_grouped_lines(snapshot, local, |host| satellite_host_lines(snapshot, host))
}

/// One satellite's glance group: its header, then its sessions under the
/// same rules as this host's ([`collapse`]). Its terminals are left to
/// `phux ls -l`.
fn satellite_glance_lines(sources: Sources<'_>, host: &HostInventory) -> Vec<String> {
    if !host.is_reachable() {
        return vec![format!("{} (unreachable)", host.host)];
    }
    let mut lines = vec![host.host.to_string()];
    if host.sessions.is_empty() {
        lines.push("  (no sessions)".to_owned());
    }
    let mut by_session: HashMap<SessionId, Vec<&ResourceInfo>> = HashMap::new();
    for pane in satellite_terminals(sources.snapshot, &host.host) {
        if let Some(session) = pane.session {
            by_session.entry(session).or_default().push(pane);
        }
    }
    let rows = host
        .sessions
        .iter()
        .map(|session| {
            let terminals = by_session.remove(&session.id).unwrap_or_default();
            let joined = u32::try_from(terminals.len()).unwrap_or(u32::MAX);
            let mut row = glance::terminals_glance(sources, &session.name, terminals);
            // A terminal the satellite did not tag with its session is
            // still in its pane count; it reads as running.
            row.running += u32::from(session.pane_count).saturating_sub(joined);
            row.attached_clients = session.attached_client_count;
            row
        })
        .collect();
    lines.extend(collapse(rows).into_iter().map(|line| format!("  {line}")));
    lines
}

/// The flat glance's satellite Terminals, from a hub that does not report
/// which sessions they belong to: one line per satellite, its terminals
/// counted (`edge terminals: needs you (1 failed), 11 running`).
fn satellite_terminal_glance_lines(sources: Sources<'_>) -> Vec<String> {
    let mut hosts: Vec<&SatelliteHost> = Vec::new();
    for pane in resource::terminals(sources.snapshot) {
        if let Some(host) = pane.id.host()
            && !hosts.contains(&host)
        {
            hosts.push(host);
        }
    }
    hosts
        .into_iter()
        .map(|host| {
            let name = format!("{host} terminals");
            let terminals = satellite_terminals(sources.snapshot, host);
            glance_text(&glance::terminals_glance(sources, &name, terminals))
        })
        .collect()
}

/// One satellite's Terminals in the merged snapshot, in snapshot order.
fn satellite_terminals<'a>(
    snapshot: &'a SessionSnapshot,
    host: &'a SatelliteHost,
) -> impl Iterator<Item = &'a ResourceInfo> {
    resource::terminals(snapshot).filter(move |pane| pane.id.host() == Some(host))
}

/// How many session lines the glance prints before it collapses the rest
/// into counts.
const GLANCE_ROWS: usize = 20;

/// `rows` as glance lines: sessions that need you first, then the rest,
/// each group name-sorted. Past [`GLANCE_ROWS`], one line counts what was
/// left out; a quiet session never displaces one that needs you.
fn collapse(rows: Vec<SessionGlance>) -> Vec<String> {
    collapse_by_project(
        rows.into_iter()
            .map(|row| (row, RowProject::default()))
            .collect(),
    )
}

/// What groups one glance row (ADR-0168): its repository identity and its
/// explicit project link, which outranks the identity.
#[derive(Debug, Clone, Default)]
struct RowProject {
    identity: Option<SessionProject>,
    link: Option<String>,
}

/// [`collapse`], then the shown rows of one project (ADR-0168) grouped
/// under a `NAME (project)` header: the primary checkout two spaces in,
/// its linked worktrees four. A project with one shown session stays a
/// plain line.
fn collapse_by_project(mut rows: Vec<(SessionGlance, RowProject)>) -> Vec<String> {
    rows.sort_by(|(a, _), (b, _)| {
        (a.needs_you() == 0)
            .cmp(&(b.needs_you() == 0))
            .then_with(|| a.name.cmp(&b.name))
    });
    let shown_len = rows.len().min(GLANCE_ROWS);
    let (shown, hidden) = rows.split_at(shown_len);
    let mut lines = project_grouped_glance(shown);
    if hidden.is_empty() {
        return lines;
    }
    let loud = hidden.iter().filter(|(row, _)| row.needs_you() > 0).count();
    let quiet = hidden.len() - loud;
    let sessions = |n: usize| if n == 1 { "session" } else { "sessions" };
    let mut counts = Vec::new();
    if loud > 0 {
        counts.push(format!("+{loud} more {} that need you", sessions(loud)));
    }
    if quiet > 0 {
        counts.push(format!("+{quiet} quiet {}", sessions(quiet)));
    }
    lines.push(format!(
        "{}; `phux ls -l` lists every session",
        counts.join(", ")
    ));
    lines
}

/// Glance lines for `rows` in order, each project of two or more rows under
/// one header.
fn project_grouped_glance(rows: &[(SessionGlance, RowProject)]) -> Vec<String> {
    use phux_client::project_groups::{Block, GroupRow, group_rows};
    let group: Vec<GroupRow<'_>> = rows
        .iter()
        .map(|(row, project)| GroupRow {
            project: project.identity.as_ref(),
            link: project.link.as_deref(),
            label: None,
            needs_you: usize::try_from(row.needs_you()).unwrap_or(usize::MAX),
        })
        .collect();
    let mut lines = Vec::new();
    for block in group_rows(&group, 2) {
        match block {
            Block::Single(index) => lines.push(glance_text(&rows[index].0)),
            Block::Project(project) => {
                lines.push(match project.needs_you {
                    0 => format!("{} (project)", project.name),
                    n => format!("{} (project, {n} need you)", project.name),
                });
                for member in project.members {
                    let indent = if member.linked { "    " } else { "  " };
                    lines.push(format!("{indent}{}", glance_text(&rows[member.index].0)));
                }
            }
        }
    }
    lines
}

/// One session's glance line: what needs you, what runs, and `ephemeral`
/// when the session vanishes with its last terminal.
///
/// `api: needs you (1 agent waiting, 1 failed), 5 running`
fn glance_text(row: &SessionGlance) -> String {
    let needs = attention_parts(row);
    let mut parts = Vec::new();
    if !needs.is_empty() {
        parts.push(format!("needs you ({})", needs.join(", ")));
    }
    match row.to_organize {
        Some(0) => parts.push("Intake".to_owned()),
        Some(n) => parts.push(format!("{n} in Intake")),
        None => {}
    }
    parts.extend(execution_parts(row));
    if row.ephemeral {
        parts.push("ephemeral".to_owned());
    }
    format!(
        "{}: {}{}",
        row.name,
        parts.join(", "),
        attached_note(row.attached_clients)
    )
}

fn attention_parts(row: &SessionGlance) -> Vec<String> {
    let mut needs = Vec::new();
    if row.waiting > 0 {
        let agents = plural32(row.waiting, "agent", "agents");
        needs.push(format!("{agents} waiting"));
    }
    if row.failed > 0 {
        needs.push(format!("{} failed", row.failed));
    }
    if row.lost > 0 {
        needs.push(format!("{} lost", row.lost));
    }
    needs
}

fn execution_parts(row: &SessionGlance) -> Vec<String> {
    let mut parts = Vec::new();
    if row.running > 0 {
        parts.push(format!("{} running", row.running));
    }
    if row.not_started > 0 {
        parts.push(format!("{} not started", row.not_started));
    }
    if parts.is_empty() && row.done > 0 && row.needs_you() == 0 {
        parts.push(format!("{} done", row.done));
    }
    if row.is_empty() {
        parts.push("empty".to_owned());
    }
    parts
}

/// A [`SessionJson`] row as a glance: what its execution counts tell
/// (`--all` reads other machines' JSON, which carries no agent records).
fn glance_of_row(row: &SessionJson) -> SessionGlance {
    let exec = &row.exec;
    SessionGlance {
        name: row.name.clone(),
        failed: exec.failed,
        lost: exec.lost,
        running: exec.starting + exec.running + exec.stopping,
        not_started: exec.dormant,
        done: exec.completed + exec.abandoned,
        ephemeral: row.lifetime == Some(SessionLifetime::Ephemeral),
        attached_clients: row.attached_clients,
        ..SessionGlance::default()
    }
}

/// `phux ls SESSION`: the session's glance line (its `-l` line with
/// `detail`), then one line per terminal, exceptions first.
fn print_one_session(sources: Sources<'_>, name: &str, detail: bool) -> ExitCode {
    let Some(session) = sources.snapshot.sessions.iter().find(|s| s.name == name) else {
        return json_err::emit(
            false,
            &CliError::new(
                codes::NO_SUCH_TARGET,
                format!("no such session: {name}"),
                "run `phux ls` to see sessions",
            ),
            crate::exit_codes::EXIT_FAILURE,
        );
    };
    for line in one_session_lines(sources, session, detail) {
        outln!("{line}");
    }
    ExitCode::SUCCESS
}

fn one_session_lines(sources: Sources<'_>, session: &SessionInfo, detail: bool) -> Vec<String> {
    let names = sources.organization.map(KeyNames::of_inventory);
    let header = if detail {
        let row = phux_client::session_list::session_json(
            sources.snapshot,
            sources.organization,
            session,
        );
        let key = sources
            .organization
            .and_then(|inventory| inventory.session_by_live(session.id))
            .zip(names.as_ref())
            .map(|(record, names)| names.display(*record.key.as_bytes()));
        with_key(format_row(&row), key.as_deref())
    } else {
        glance_text(&glance::session_glance(sources, session))
    };
    let members = glance::members(sources, session);
    if members.is_empty() {
        return vec![header, "  (no terminals)".to_owned()];
    }
    let rows: Vec<Vec<String>> = members
        .iter()
        .map(|member| member_cells(member, detail, names.as_ref()))
        .collect();
    let mut lines = vec![header];
    lines.extend(columns(&rows).into_iter().map(|line| format!("  {line}")));
    lines
}

/// One terminal's cells: selector, state, agent, title, directory; with
/// `detail` also its key and its revision and run.
fn member_cells(member: &MemberGlance, detail: bool, names: Option<&KeyNames>) -> Vec<String> {
    let mut cells = vec![member.selector()];
    if detail {
        let key = member.key.zip(names);
        cells.push(key.map_or_else(String::new, |(key, names)| names.display(*key.as_bytes())));
    }
    cells.push(member.status.clone());
    cells.push(member.agent.as_deref().map(printable).unwrap_or_default());
    cells.push(member.title.as_deref().map(printable).unwrap_or_default());
    cells.push(member.cwd.as_deref().map(home_relative).unwrap_or_default());
    if detail {
        cells.push(member.record.clone().unwrap_or_default());
    }
    cells
}

/// `rows` as left-aligned columns two spaces apart, dropping columns empty
/// in every row and trailing blanks.
fn columns(rows: &[Vec<String>]) -> Vec<String> {
    let count = rows.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..count)
        .map(|col| {
            rows.iter()
                .map(|row| row.get(col).map_or(0, |cell| cell.chars().count()))
                .max()
                .unwrap_or(0)
        })
        .collect();
    rows.iter()
        .map(|row| {
            let mut line = String::new();
            for (cell, width) in row.iter().zip(&widths) {
                if *width == 0 {
                    continue;
                }
                if !line.is_empty() {
                    line.push_str("  ");
                }
                line.push_str(cell);
                line.extend(std::iter::repeat_n(' ', width - cell.chars().count()));
            }
            line.trim_end().to_owned()
        })
        .collect()
}

/// Program-set text without control or bidi characters, so it cannot
/// rewrite the listing around it.
fn printable(text: &str) -> String {
    text.chars()
        .filter(|ch| !ch.is_control() && !phux_tui::render::is_bidi_control(*ch))
        .collect()
}

/// `dir` with the home directory spelled `~`.
fn home_relative(dir: &str) -> String {
    let dir = printable(dir);
    let home = std::env::var("HOME").unwrap_or_default();
    if home.is_empty() {
        return dir;
    }
    if dir == home {
        return "~".to_owned();
    }
    match dir.strip_prefix(&home) {
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => dir,
    }
}

/// Header for this host's group in the host-grouped listing.
pub(crate) const LOCAL_HOST_LABEL: &str = "This host";

/// This host's name-sorted session lines with their nested agent-session
/// lines, unindented. With organization records, each session line ends in
/// its key (`work: ...  ^ab12cd`), the shortest prefix no other record
/// shares, so `phux rm ^ab12cd` can address it; the `name:` prefix scripts
/// match stays as it was.
fn local_session_lines(
    snapshot: &SessionSnapshot,
    organization: Option<&OrgInventory>,
) -> Vec<String> {
    let mut sessions: Vec<_> = snapshot.sessions.iter().collect();
    sessions.sort_by(|a, b| a.name.cmp(&b.name));
    let names = organization.map(phux_client::org::KeyNames::of_inventory);
    let mut lines: Vec<String> = Vec::new();
    for session in sessions {
        let row = phux_client::session_list::session_json(snapshot, organization, session);
        let key = organization
            .and_then(|inventory| inventory.session_by_live(session.id))
            .zip(names.as_ref())
            .map(|(record, names)| names.display(*record.key.as_bytes()));
        lines.push(with_key(format_row(&row), key.as_deref()));
        lines.extend(agent_session_lines(snapshot, session));
    }
    lines
}

/// `line` with `key`, when there is one, after two spaces.
fn with_key(line: String, key: Option<&str>) -> String {
    match key {
        Some(key) => format!("{line}  {key}"),
        None => line,
    }
}

/// The host-grouped listing: a [`LOCAL_HOST_LABEL`] header over this host's
/// sessions, then one header per satellite with its sessions beneath. An
/// unreachable satellite keeps its header, marked, instead of vanishing.
fn host_grouped_lines(
    snapshot: &SessionSnapshot,
    local: Vec<String>,
    satellite: impl Fn(&HostInventory) -> Vec<String>,
) -> Vec<String> {
    let mut lines = vec![LOCAL_HOST_LABEL.to_owned()];
    if local.is_empty() {
        lines.push("  (no sessions)".to_owned());
    }
    lines.extend(local.into_iter().map(|line| format!("  {line}")));
    for host in snapshot.hosts() {
        lines.extend(satellite(host));
    }
    lines
}

/// One satellite's group: its header, its name-sorted sessions, then that
/// satellite's Terminals as the `host/@N` selectors every Terminal-facet
/// verb accepts through the hub — the addressable handles the flat listing
/// printed, kept under the host they live on.
fn satellite_host_lines(snapshot: &SessionSnapshot, host: &HostInventory) -> Vec<String> {
    if !host.is_reachable() {
        // The diagnostic itself goes to stderr with the partial-view
        // warning; the listing only marks the host.
        return vec![format!("{} (unreachable)", host.host)];
    }
    let mut lines = vec![host.host.to_string()];
    if host.sessions.is_empty() {
        lines.push("  (no sessions)".to_owned());
    }
    let mut sessions: Vec<_> = host.sessions.iter().collect();
    sessions.sort_by(|a, b| a.name.cmp(&b.name));
    lines.extend(
        sessions
            .into_iter()
            .map(|s| format!("  {}", format_host_session_line(s))),
    );
    lines.extend(satellite_terminal_lines(snapshot, &host.host));
    lines
}

/// `  host/@N: satellite terminal` for each of one satellite's Terminals in
/// the merged snapshot, in snapshot order.
fn satellite_terminal_lines(
    snapshot: &SessionSnapshot,
    host: &phux_protocol::ids::SatelliteHost,
) -> Vec<String> {
    resource::terminals(snapshot)
        .filter(|pane| pane.id.host().is_some_and(|h| h == host))
        .map(|pane| {
            format!(
                "  {}: satellite terminal",
                phux_client::selector::format_terminal_id(&pane.id)
            )
        })
        .collect()
}

/// One satellite session's line: the local line's shape plus its pane
/// count, which is all a hub knows about a session it does not own.
fn format_host_session_line(s: &HostSessionInfo) -> String {
    let windows = plural(s.window_count, "window", "windows");
    let panes = plural(s.pane_count, "pane", "panes");
    format!(
        "{}: {windows}, {panes}{}",
        s.name,
        attached_note(s.attached_client_count)
    )
}

fn plural(n: u16, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// The pre-host-inventory listing: this host's sessions, then satellite
/// Terminals that cannot be joined to hub-local sessions.
fn flat_session_lines(snapshot: &SessionSnapshot, local: Vec<String>) -> Vec<String> {
    let mut lines = local;
    for pane in resource::terminals(snapshot) {
        if pane.id.host().is_some() {
            lines.push(format!(
                "{}: satellite terminal",
                phux_client::selector::format_terminal_id(&pane.id)
            ));
        }
    }
    lines
}

/// The nested lines under one session: `  @7` for each of its panes that
/// hosts an agent session, then `    @9: agent session <provider> (<state>)`
/// per session child.
fn agent_session_lines(snapshot: &SessionSnapshot, session: &SessionInfo) -> Vec<String> {
    let mut lines = Vec::new();
    for pane in resource::terminals_in(snapshot, session.id) {
        let children: Vec<_> = resource::children_of(snapshot, &pane.id).collect();
        if children.is_empty() {
            continue;
        }
        lines.push(format!(
            "  {}",
            phux_client::selector::format_terminal_id(&pane.id)
        ));
        for child in children {
            let facet = child.agent.as_ref();
            let provider = facet.map_or("agent session", |facet| facet.provider.as_str());
            let state = facet.map_or("unknown", |facet| facet.state.as_str());
            lines.push(format!(
                "    {}: agent session {provider} ({state})",
                phux_client::selector::format_terminal_id(&child.id)
            ));
        }
    }
    lines
}

/// One session's `ls` line, with the real attached-client count (zero says
/// nothing) and `(empty)` for a windowless session (ADR-0105). Shared with
/// `phux status`.
pub(crate) fn format_session_line(s: &SessionInfo) -> String {
    let windows = if s.window_count == 1 {
        "window"
    } else {
        "windows"
    };
    let empty = if s.is_empty() { " (empty)" } else { "" };
    format!(
        "{}: {} {windows}{empty}{}",
        s.name,
        s.window_count,
        attached_note(s.attached_client_count)
    )
}

/// One `phux ls` session line from its [`SessionJson`] row: member count
/// with the members' execution states, window count, lifetime, and attached
/// clients. A session with no members reads `empty`, so a persistent session
/// kept after its last process explains itself; the lifetime is omitted when
/// the server does not report one.
///
/// `work: 3 members (2 running, 1 completed), 2 windows, persistent`
pub(crate) fn format_row(row: &SessionJson) -> String {
    let mut parts = Vec::new();
    if row.members == 0 && (row.lifetime.is_some() || row.empty) {
        parts.push("empty".to_owned());
    } else {
        let members = plural32(row.members, "member", "members");
        let states = exec_summary(&row.exec);
        parts.push(if states.is_empty() {
            members
        } else {
            format!("{members} ({states})")
        });
    }
    if row.windows > 0 {
        parts.push(plural(row.windows, "window", "windows"));
    }
    if let Some(lifetime) = row.lifetime {
        parts.push(
            match lifetime {
                SessionLifetime::Persistent => "persistent",
                SessionLifetime::Ephemeral => "ephemeral",
            }
            .to_owned(),
        );
    }
    format!(
        "{}: {}{}",
        row.name,
        parts.join(", "),
        attached_note(row.attached_clients)
    )
}

/// The non-zero execution counts, most active first:
/// `2 running, 1 completed, 1 lost`.
fn exec_summary(exec: &ExecCounts) -> String {
    [
        (exec.starting, "starting"),
        (exec.running, "running"),
        (exec.stopping, "stopping"),
        (exec.completed, "completed"),
        (exec.failed, "failed"),
        (exec.lost, "lost"),
        (exec.abandoned, "abandoned"),
        (exec.dormant, "not started"),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, label)| format!("{n} {label}"))
    .collect::<Vec<_>>()
    .join(", ")
}

fn plural32(n: u32, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// The ` (N clients attached)` suffix a session line carries, or nothing at
/// zero. Shared so a satellite session line reads like a local one.
fn attached_note(clients: u16) -> String {
    match clients {
        0 => String::new(),
        1 => " (1 client attached)".to_owned(),
        n => format!(" ({n} clients attached)"),
    }
}

/// Emit the session list as the stable [`SessionListJson`] contract, built
/// by [`phux_client::session_list::document`] — the one builder the MCP
/// `phux_ls` tool also returns, so the two surfaces cannot drift.
pub(crate) fn print_sessions_json(
    snapshot: &SessionSnapshot,
    organization: Option<&OrgInventory>,
    degradation: &Degradation,
    hosts_complete: bool,
) -> ExitCode {
    let list: SessionListJson =
        phux_client::session_list::document(snapshot, organization, degradation, hosts_complete);
    crate::output::json(&list)
}

#[cfg(test)]
mod tests {
    /// `phux ls --all` with no local server said "unreachable: attach loop
    /// io error: No such file or directory (os error 2)".
    #[test]
    fn a_host_with_no_server_is_named_so() {
        use phux_client::attach::AttachError;
        for kind in [
            std::io::ErrorKind::NotFound,
            std::io::ErrorKind::ConnectionRefused,
        ] {
            let err = AttachError::Io(std::io::Error::from(kind));
            assert_eq!(super::unreachable_reason(&err), "no server running");
        }
        assert_eq!(
            super::unreachable_reason(&AttachError::Refused("no".to_owned())),
            AttachError::Refused("no".to_owned()).to_string()
        );
    }

    #[test]
    fn all_hosts_listing_groups_by_machine_and_keeps_a_down_host() {
        use phux_core::activation::SessionLifetime;
        use phux_core::host_list::{HostJson, HostKind, HostListJson};
        use phux_core::session_list::{ExecCounts, SessionJson};
        let session = |name: &str, clients| SessionJson {
            name: name.to_owned(),
            windows: 2,
            attached: clients > 0,
            attached_clients: clients,
            keep_empty: true,
            empty: false,
            lifetime: Some(SessionLifetime::Persistent),
            members: 2,
            exec: ExecCounts {
                running: 2,
                ..ExecCounts::default()
            },
            project: None,
            project_link: None,
            key: None,
        };
        let doc = HostListJson::new(vec![
            HostJson {
                name: "local".to_owned(),
                label: "laptop".to_owned(),
                kind: HostKind::Local,
                endpoint: None,
                reachable: true,
                error: None,
                sessions: vec![session("work", 1)],
            },
            HostJson {
                name: "mini".to_owned(),
                label: "mini".to_owned(),
                kind: HostKind::Remote,
                endpoint: Some("quic://100.64.0.2:8788".to_owned()),
                reachable: true,
                error: None,
                sessions: Vec::new(),
            },
            HostJson {
                name: "xps".to_owned(),
                label: "xps".to_owned(),
                kind: HostKind::Remote,
                endpoint: Some("quic://100.64.0.3:8788".to_owned()),
                reachable: false,
                error: Some("did not answer within 3s".to_owned()),
                sessions: Vec::new(),
            },
        ]);
        assert_eq!(
            super::host_list_lines(&doc),
            [
                "laptop (this machine)",
                "  work: 2 running (1 client attached)",
                "mini",
                "  (no sessions)",
                "xps  unreachable: did not answer within 3s",
            ]
        );
    }

    use phux_protocol::wire::info::{SessionInfo, SessionSnapshot};
    use phux_protocol::{ResourceId, SessionId, WindowId};

    use super::{EMPTY_STATE, format_row, format_session_line, session_lines, with_key};

    fn session(name: &str, windows: u16, clients: u16) -> SessionInfo {
        SessionInfo::new(SessionId::new(1), name)
            .with_window_count(windows)
            .with_attached_client_count(clients)
    }

    /// A session line ends in its key, so the listed prefix is what
    /// `phux rm ^ab12cd` takes; a server without records prints none.
    #[test]
    fn a_session_line_ends_in_its_key() {
        let line = "work: 1 member (1 running), persistent".to_owned();
        assert_eq!(
            with_key(line.clone(), Some("^ab12cd")),
            "work: 1 member (1 running), persistent  ^ab12cd"
        );
        assert_eq!(with_key(line.clone(), None), line);
    }

    /// ADR-0105: a session with no windows says so, so a server that stays
    /// up with zero processes explains itself.
    #[test]
    fn an_empty_session_is_marked() {
        assert_eq!(
            format_session_line(&session("parked", 0, 0).with_keep_empty(true)),
            "parked: 0 windows (empty)"
        );
        assert_eq!(
            format_session_line(&session("parked", 0, 1).with_keep_empty(true)),
            "parked: 0 windows (empty) (1 client attached)"
        );
        assert_eq!(
            format_session_line(&session("work", 1, 0).with_keep_empty(true)),
            "work: 1 window",
            "a populated keep-empty session is not marked empty"
        );
    }

    #[test]
    fn empty_snapshot_yields_no_lines_and_the_empty_state_names_next_commands() {
        let snapshot =
            SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::new(1));
        assert!(session_lines(&snapshot, None).is_empty());

        // The two-line empty state: what is missing, then the exact commands
        // that fix it (the `phux host ls` precedent).
        assert_eq!(EMPTY_STATE.len(), 2);
        assert_eq!(EMPTY_STATE[0], "No sessions.");
        assert!(EMPTY_STATE[1].contains("`phux`"));
        assert!(EMPTY_STATE[1].contains("`phux new NAME -- CMD`"));
    }

    /// `phux ls` shows each session's lifetime, member count, and the
    /// members' execution states; a member-less session reads `empty`.
    #[test]
    fn rows_show_lifetime_members_and_execution_states() {
        use phux_core::activation::SessionLifetime;
        use phux_core::session_list::{ExecCounts, SessionJson};
        let row = |name: &str, lifetime, members, exec, windows| SessionJson {
            name: name.to_owned(),
            windows,
            attached: false,
            attached_clients: 0,
            keep_empty: lifetime == Some(SessionLifetime::Persistent),
            empty: members == 0,
            lifetime,
            members,
            exec,
            project: None,
            project_link: None,
            key: None,
        };
        assert_eq!(
            format_row(&row(
                "parked",
                Some(SessionLifetime::Persistent),
                0,
                ExecCounts::default(),
                0
            )),
            "parked: empty, persistent"
        );
        assert_eq!(
            format_row(&row(
                "work",
                Some(SessionLifetime::Ephemeral),
                4,
                ExecCounts {
                    running: 2,
                    completed: 1,
                    lost: 1,
                    ..ExecCounts::default()
                },
                2
            )),
            "work: 4 members (2 running, 1 completed, 1 lost), 2 windows, ephemeral"
        );
        assert_eq!(
            format_row(&row(
                "old",
                None,
                1,
                ExecCounts {
                    running: 1,
                    ..ExecCounts::default()
                },
                1
            )),
            "old: 1 member (1 running), 1 window",
            "a server that reports no lifetime gets none printed"
        );
    }

    #[test]
    fn zero_clients_render_no_attachment_note() {
        assert_eq!(
            format_session_line(&session("work", 3, 0)),
            "work: 3 windows"
        );
    }

    #[test]
    fn one_client_renders_a_singular_count() {
        assert_eq!(
            format_session_line(&session("work", 1, 1)),
            "work: 1 window (1 client attached)"
        );
    }

    #[test]
    fn two_clients_render_the_real_count() {
        assert_eq!(
            format_session_line(&session("work", 2, 2)),
            "work: 2 windows (2 clients attached)"
        );
    }

    /// An agent session nests under its pane, under its session; panes with
    /// no session child print nothing, so the plain listing is unchanged.
    #[test]
    fn agent_sessions_nest_under_their_pane() {
        use phux_protocol::ids::ResourceKind;
        use phux_protocol::wire::info::{AgentFacet, ResourceInfo, WindowInfo};

        let work = SessionId::new(1);
        let window = WindowId::new(10);
        let snapshot = SessionSnapshot::new(work, window, ResourceId::local(7))
            .with_sessions(vec![SessionInfo::new(work, "work").with_window_count(1)])
            .with_windows(vec![WindowInfo::new(window, work, "shell").with_index(0)])
            .with_resources(vec![
                ResourceInfo::new(ResourceId::local(7), window, 80, 24),
                ResourceInfo::new(ResourceId::local(8), window, 80, 24),
                ResourceInfo::new(ResourceId::local(9), window, 0, 0)
                    .with_kind(ResourceKind::AgentSession)
                    .with_parent(Some(ResourceId::local(7)))
                    .with_agent(Some(AgentFacet::new("claude", "working"))),
            ]);
        assert_eq!(
            session_lines(&snapshot, None),
            vec![
                "work: 3 members (3 running), 1 window",
                "  @7",
                "    @9: agent session claude (working)",
            ]
        );
    }

    /// A snapshot from a hub: one local session, a live satellite with one
    /// session, and one the hub could not reach.
    fn federated_snapshot() -> SessionSnapshot {
        use phux_protocol::ids::SatelliteHost;
        use phux_protocol::wire::info::{HostInventory, HostSessionInfo};

        let work = SessionId::new(1);
        SessionSnapshot::new(work, WindowId::new(10), ResourceId::local(7))
            .with_sessions(vec![
                SessionInfo::new(work, "work")
                    .with_window_count(1)
                    .with_active_window(Some(WindowId::new(10))),
            ])
            .with_windows(vec![
                phux_protocol::wire::info::WindowInfo::new(WindowId::new(10), work, "shell")
                    .with_active_resource(Some(ResourceId::local(7))),
            ])
            .with_resources(vec![phux_protocol::wire::info::ResourceInfo::new(
                ResourceId::local(7),
                WindowId::new(10),
                80,
                24,
            )])
            .with_hosts(vec![
                HostInventory::reachable(
                    SatelliteHost::new("edge"),
                    vec![
                        HostSessionInfo::new(SessionId::new(1), "build")
                            .with_window_count(2)
                            .with_pane_count(3)
                            .with_attached_client_count(1)
                            .with_active_resource(Some(ResourceId::satellite(
                                SatelliteHost::new("edge"),
                                9,
                            ))),
                    ],
                ),
                HostInventory::unreachable(SatelliteHost::new("down"), "link is down"),
            ])
    }

    /// A hub groups the listing by host; an unreachable satellite is marked,
    /// not dropped.
    #[test]
    fn host_inventory_groups_the_listing() {
        assert_eq!(
            session_lines(&federated_snapshot(), None),
            vec![
                "This host",
                "  work: 1 member (1 running), 1 window",
                "edge",
                "  build: 2 windows, 3 panes (1 client attached)",
                "down (unreachable)",
            ]
        );
    }

    /// Without a host inventory the listing is the flat, pre-grouping one.
    #[test]
    fn no_host_inventory_keeps_the_flat_listing() {
        let snapshot =
            SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::new(1))
                .with_sessions(vec![session("work", 1, 0)]);
        assert_eq!(
            session_lines(&snapshot, None),
            vec!["work: 0 members, 1 window"]
        );
    }

    /// A grouped listing keeps each satellite Terminal's `host/@N` selector
    /// under the host it lives on.
    #[test]
    fn grouped_listing_keeps_satellite_terminal_selectors() {
        use phux_protocol::ids::SatelliteHost;

        let mut snapshot = federated_snapshot();
        snapshot
            .resources
            .push(phux_protocol::wire::info::ResourceInfo::new(
                ResourceId::satellite(SatelliteHost::new("edge"), 9),
                WindowId::new(3),
                80,
                24,
            ));
        assert_eq!(
            session_lines(&snapshot, None),
            vec![
                "This host",
                "  work: 1 member (1 running), 1 window",
                "edge",
                "  build: 2 windows, 3 panes (1 client attached)",
                "  edge/@9: satellite terminal",
                "down (unreachable)",
            ]
        );
    }

    /// The text-mode "satellite sessions are unavailable" note fires only for
    /// a server that lists satellite Terminals.
    #[test]
    fn host_sessions_note_needs_satellite_terminals() {
        use phux_protocol::ids::SatelliteHost;

        let plain = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::new(1))
            .with_sessions(vec![session("work", 1, 0)]);
        assert!(!super::has_satellite_terminals(&plain));
        let hub = plain.with_resources(vec![phux_protocol::wire::info::ResourceInfo::new(
            ResourceId::satellite(SatelliteHost::new("edge"), 9),
            WindowId::new(3),
            80,
            24,
        )]);
        assert!(super::has_satellite_terminals(&hub));
    }

    #[test]
    fn lines_are_name_sorted() {
        let snapshot =
            SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::new(1))
                .with_sessions(vec![session("beta", 1, 0), session("alpha", 1, 0)]);
        let lines = session_lines(&snapshot, None);
        assert_eq!(
            lines,
            vec!["alpha: 0 members, 1 window", "beta: 0 members, 1 window"]
        );
    }

    /// The glance: what needs you leads, then what runs; a persistent
    /// lifetime, window counts, and keys are left to `-l`.
    #[test]
    fn a_glance_line_names_only_counts_and_exceptions() {
        use phux_client::glance::SessionGlance;
        let row = |name: &str| SessionGlance {
            name: name.to_owned(),
            ..SessionGlance::default()
        };
        assert_eq!(
            super::glance_text(&SessionGlance {
                waiting: 1,
                failed: 1,
                running: 5,
                ..row("api")
            }),
            "api: needs you (1 agent waiting, 1 failed), 5 running"
        );
        assert_eq!(
            super::glance_text(&SessionGlance {
                running: 12,
                attached_clients: 1,
                ..row("build")
            }),
            "build: 12 running (1 client attached)"
        );
        assert_eq!(
            super::glance_text(&SessionGlance {
                ephemeral: true,
                ..row("scratch")
            }),
            "scratch: empty, ephemeral"
        );
        assert_eq!(
            super::glance_text(&SessionGlance {
                done: 3,
                ..row("ci")
            }),
            "ci: 3 done"
        );
        assert_eq!(
            super::glance_text(&SessionGlance {
                not_started: 2,
                to_organize: Some(2),
                ..row("inbox")
            }),
            "inbox: 2 in Intake, 2 not started"
        );
        assert_eq!(
            super::glance_text(&SessionGlance {
                waiting: 1,
                running: 1,
                to_organize: Some(2),
                ..row("my queue")
            }),
            "my queue: needs you (1 agent waiting), 2 in Intake, 1 running"
        );
        assert_eq!(
            super::glance_text(&SessionGlance {
                to_organize: Some(0),
                ..row("inbox")
            }),
            "inbox: Intake, empty"
        );
    }

    /// A swarm: sessions that need you sort first and are never collapsed
    /// for a quiet one; the rest fold into one count line, so a thousand
    /// sessions print about twenty lines.
    #[test]
    fn a_swarm_collapses_its_quiet_sessions() {
        use phux_client::glance::SessionGlance;
        let mut rows: Vec<SessionGlance> = (0..1000)
            .map(|n| SessionGlance {
                name: format!("s{n:04}"),
                running: 1,
                ..SessionGlance::default()
            })
            .collect();
        rows[700].failed = 1;
        rows[900].waiting = 1;
        let lines = super::collapse(rows);
        assert_eq!(lines.len(), super::GLANCE_ROWS + 1, "{lines:?}");
        assert!(lines[0].starts_with("s0700: needs you"), "{lines:?}");
        assert!(lines[1].starts_with("s0900: needs you"), "{lines:?}");
        assert_eq!(lines[2], "s0000: 1 running");
        assert_eq!(
            lines[super::GLANCE_ROWS],
            "+980 quiet sessions; `phux ls -l` lists every session"
        );

        let few: Vec<SessionGlance> = (0..3)
            .map(|n| SessionGlance {
                name: format!("s{n}"),
                ..SessionGlance::default()
            })
            .collect();
        assert_eq!(super::collapse(few).len(), 3, "no count line when it fits");
    }

    /// A session linked into a project by name glances under that
    /// project's header beside its git sessions; a link naming no
    /// identity groups on its own.
    #[test]
    fn a_linked_session_glances_under_its_project() {
        use phux_client::glance::SessionGlance;
        use phux_core::project_identity::SessionProject;
        let phux = SessionProject {
            id: "github.com/no-phux/phux".to_owned(),
            name: "phux".to_owned(),
            checkout: "/w".to_owned(),
            branch: None,
            linked_worktree: false,
            host: Some("mini".to_owned()),
        };
        let glance = |name: &str| SessionGlance {
            name: name.to_owned(),
            running: 1,
            ..SessionGlance::default()
        };
        let row = |identity, link: Option<&str>| super::RowProject {
            identity,
            link: link.map(str::to_owned),
        };
        let lines = super::collapse_by_project(vec![
            (glance("main"), row(Some(phux), None)),
            (glance("notes"), row(None, Some("phux"))),
            (glance("ops-a"), row(None, Some("ops"))),
            (glance("ops-b"), row(None, Some("ops"))),
        ]);
        assert_eq!(
            lines,
            [
                "phux (project)",
                "  main: 1 running",
                "  notes: 1 running",
                "ops (project)",
                "  ops-a: 1 running",
                "  ops-b: 1 running",
            ]
        );
    }

    /// ADR-0168: sessions of one repository glance under one header, the
    /// primary checkout first and its linked worktrees indented beneath;
    /// a project with one session stays a plain line.
    #[test]
    fn the_glance_groups_sessions_by_project() {
        use phux_client::glance::SessionGlance;
        use phux_core::project_identity::SessionProject;
        let project = |linked| SessionProject {
            id: "github.com/no-phux/phux".to_owned(),
            name: "phux".to_owned(),
            checkout: "/w".to_owned(),
            branch: None,
            linked_worktree: linked,
            host: Some("mini".to_owned()),
        };
        let lone = SessionProject {
            id: "github.com/a/other".to_owned(),
            ..project(false)
        };
        let (main, feat) = (project(false), project(true));
        let glance = |name: &str, waiting| SessionGlance {
            name: name.to_owned(),
            waiting,
            running: 1,
            ..SessionGlance::default()
        };
        let of = |identity| super::RowProject {
            identity,
            link: None,
        };
        let lines = super::collapse_by_project(vec![
            (glance("feat-auth", 1), of(Some(feat))),
            (glance("scratch", 0), of(None)),
            (glance("phux", 0), of(Some(main))),
            (glance("other", 0), of(Some(lone))),
        ]);
        assert_eq!(
            lines,
            [
                "phux (project, 1 need you)",
                "  phux: 1 running",
                "    feat-auth: needs you (1 agent waiting), 1 running",
                "other: 1 running",
                "scratch: 1 running",
            ]
        );
    }

    /// A hub's satellite glances like this host: its sessions that need
    /// you first, the quiet rest folded into one count, its terminals left
    /// to `-l`, which still prints every session and terminal.
    #[test]
    fn a_satellite_swarm_collapses_like_this_host() {
        use std::collections::HashMap;

        use phux_client::glance::Sources;
        use phux_protocol::ids::SatelliteHost;
        use phux_protocol::wire::frame::ResourceLifecycle;
        use phux_protocol::wire::info::{ExitFacet, HostInventory, HostSessionInfo, ResourceInfo};

        let edge = SatelliteHost::new("edge");
        let sessions = (0..500)
            .map(|n| {
                HostSessionInfo::new(SessionId::new(n), format!("s{n:03}"))
                    .with_window_count(1)
                    .with_pane_count(1)
            })
            .collect();
        let mut snapshot = federated_snapshot().with_hosts(vec![
            HostInventory::reachable(edge.clone(), sessions),
            HostInventory::unreachable(SatelliteHost::new("down"), "link is down"),
        ]);
        snapshot.resources.extend((0..500).map(|n| {
            let pane = ResourceInfo::new(
                ResourceId::satellite(edge.clone(), n),
                WindowId::new(0),
                80,
                24,
            )
            .with_session(Some(SessionId::new(n)));
            if n == 321 {
                pane.with_lifecycle(ResourceLifecycle::Exited)
                    .with_exit(Some(ExitFacet::new(0, 0).with_exit_status(Some(2))))
            } else {
                pane
            }
        }));
        let agents = HashMap::new();
        let sources = Sources {
            snapshot: &snapshot,
            organization: None,
            agents: &agents,
        };
        let lines = super::glance_lines(sources);
        assert_eq!(
            lines[..4],
            [
                "This host",
                "  work: 1 running",
                "edge",
                "  s321: needs you (1 failed)",
            ],
            "{lines:?}"
        );
        assert_eq!(lines[4], "  s000: 1 running");
        assert_eq!(
            lines[3 + super::GLANCE_ROWS],
            "  +480 quiet sessions; `phux ls -l` lists every session"
        );
        assert_eq!(lines[4 + super::GLANCE_ROWS], "down (unreachable)");
        assert_eq!(lines.len(), 5 + super::GLANCE_ROWS);

        let detail = session_lines(&snapshot, None);
        assert_eq!(detail.len(), 2 + 1 + 500 + 500 + 1, "-l keeps every line");
    }

    /// A hub that does not say which satellite session a terminal is in
    /// glances each satellite's terminals as one counted line.
    #[test]
    fn unjoined_satellite_terminals_glance_as_one_line_per_host() {
        use std::collections::HashMap;

        use phux_client::glance::Sources;
        use phux_protocol::ids::SatelliteHost;
        use phux_protocol::wire::frame::ResourceLifecycle;
        use phux_protocol::wire::info::{ExitFacet, ResourceInfo};

        let edge = SatelliteHost::new("edge");
        let snapshot =
            SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::new(1))
                .with_sessions(vec![session("work", 1, 0)])
                .with_resources(
                    (0..300)
                        .map(|n| {
                            let pane = ResourceInfo::new(
                                ResourceId::satellite(edge.clone(), n),
                                WindowId::new(0),
                                80,
                                24,
                            );
                            if n == 7 {
                                pane.with_lifecycle(ResourceLifecycle::Exited)
                                    .with_exit(Some(ExitFacet::new(0, 0).with_exit_status(Some(1))))
                            } else {
                                pane
                            }
                        })
                        .collect(),
                );
        let agents = HashMap::new();
        let sources = Sources {
            snapshot: &snapshot,
            organization: None,
            agents: &agents,
        };
        assert_eq!(
            super::glance_lines(sources),
            [
                "work: empty",
                "edge terminals: needs you (1 failed), 299 running"
            ]
        );
        assert_eq!(session_lines(&snapshot, None).len(), 301);
    }

    /// The session view lists each terminal, exceptions first: an agent
    /// blocked on a person, then the failure, then quiet work.
    #[test]
    fn a_session_view_lists_terminals_exceptions_first() {
        use std::collections::HashMap;

        use phux_client::agent_meta::{AgentMetaState, AgentRecord};
        use phux_client::glance::Sources;
        use phux_protocol::wire::frame::ResourceLifecycle;
        use phux_protocol::wire::info::{ExitFacet, ResourceInfo, WindowInfo};

        let work = SessionId::new(1);
        let window = WindowId::new(10);
        let failed = ResourceInfo::new(ResourceId::local(9), window, 80, 24)
            .with_lifecycle(ResourceLifecycle::Exited)
            .with_exit(Some(ExitFacet::new(0, 0).with_exit_status(Some(1))));
        let snapshot = SessionSnapshot::new(work, window, ResourceId::local(3))
            .with_sessions(vec![SessionInfo::new(work, "api").with_window_count(1)])
            .with_windows(vec![WindowInfo::new(window, work, "shell").with_index(0)])
            .with_resources(vec![
                ResourceInfo::new(ResourceId::local(3), window, 80, 24)
                    .with_title(Some("zsh".to_owned())),
                ResourceInfo::new(ResourceId::local(7), window, 80, 24),
                failed,
            ]);
        let agents = HashMap::from([(
            ResourceId::local(7),
            AgentRecord {
                name: "claude".to_owned(),
                state: AgentMetaState::Blocked,
                ..AgentRecord::default()
            },
        )]);
        let sources = Sources {
            snapshot: &snapshot,
            organization: None,
            agents: &agents,
        };
        assert_eq!(
            super::glance_lines(sources),
            ["api: needs you (1 agent waiting, 1 failed), 1 running"]
        );
        assert_eq!(
            super::one_session_lines(sources, &snapshot.sessions[0], false),
            [
                "api: needs you (1 agent waiting, 1 failed), 1 running",
                "  @7  waiting          claude blocked",
                "  @9  failed (exit 1)",
                "  @3  running                          zsh",
            ]
        );
    }

    /// Program-set text cannot smuggle control or bidi characters into the
    /// listing, and the home directory reads `~`.
    #[test]
    fn titles_and_directories_print_safely() {
        assert_eq!(super::printable("a\u{1b}[2Jb\u{202e}c"), "a[2Jbc");
        let home = std::env::var("HOME").unwrap_or_default();
        if !home.is_empty() {
            assert_eq!(super::home_relative(&format!("{home}/src")), "~/src");
            assert_eq!(
                super::home_relative(&format!("{home}x")),
                format!("{home}x")
            );
        }
    }
}
