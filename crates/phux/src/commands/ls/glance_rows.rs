//! The default glance: one line per session (or per collapsed project),
//! naming only counts and exceptions.

use std::collections::HashMap;

use phux_client::glance::{self, SessionGlance, Sources};
use phux_client::resource;
use phux_core::activation::SessionLifetime;
use phux_core::project_identity::SessionProject;
use phux_core::session_list::SessionJson;

use phux_protocol::ids::{SatelliteHost, SessionId};
use phux_protocol::wire::info::{HostInventory, ResourceInfo, SessionSnapshot};

use super::lines::{
    attached_note, flat_session_lines, host_grouped_lines, plural32, satellite_host_lines,
};

/// The default `phux ls` body: one glance line per session, sessions that
/// need you first, collapsed past [`GLANCE_ROWS`] ([`collapse`]), grouped
/// by host like [`session_lines`]. Intake is left out while it holds
/// nothing.
pub(super) fn glance_lines(sources: Sources<'_>) -> Vec<String> {
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
pub(super) fn with_hosts(snapshot: &SessionSnapshot, local: Vec<String>) -> Vec<String> {
    if snapshot.hosts().is_empty() {
        return flat_session_lines(snapshot, local);
    }
    host_grouped_lines(snapshot, local, |host| satellite_host_lines(snapshot, host))
}

/// One satellite's glance group: its header, then its sessions under the
/// same rules as this host's ([`collapse`]). Its terminals are left to
/// `phux ls -l`.
pub(super) fn satellite_glance_lines(sources: Sources<'_>, host: &HostInventory) -> Vec<String> {
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
pub(super) fn satellite_terminal_glance_lines(sources: Sources<'_>) -> Vec<String> {
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
pub(super) fn satellite_terminals<'a>(
    snapshot: &'a SessionSnapshot,
    host: &'a SatelliteHost,
) -> impl Iterator<Item = &'a ResourceInfo> {
    resource::terminals(snapshot).filter(move |pane| pane.id.host() == Some(host))
}

/// How many session lines the glance prints before it collapses the rest
/// into counts.
pub(super) const GLANCE_ROWS: usize = 20;

/// `rows` as glance lines: sessions that need you first, then the rest,
/// each group name-sorted. Past [`GLANCE_ROWS`], one line counts what was
/// left out; a quiet session never displaces one that needs you.
pub(super) fn collapse(rows: Vec<SessionGlance>) -> Vec<String> {
    collapse_by_project(
        rows.into_iter()
            .map(|row| (row, RowProject::default()))
            .collect(),
    )
}

/// What groups one glance row (ADR-0168): its repository identity and its
/// explicit project link, which outranks the identity.
#[derive(Debug, Clone, Default)]
pub(super) struct RowProject {
    pub(super) identity: Option<SessionProject>,
    pub(super) link: Option<String>,
}

/// [`collapse`], then the shown rows of one project (ADR-0168) grouped
/// under a `NAME (project)` header: the primary checkout two spaces in,
/// its linked worktrees four. A project with one shown session stays a
/// plain line.
pub(super) fn collapse_by_project(mut rows: Vec<(SessionGlance, RowProject)>) -> Vec<String> {
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
pub(super) fn project_grouped_glance(rows: &[(SessionGlance, RowProject)]) -> Vec<String> {
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
pub(super) fn glance_text(row: &SessionGlance) -> String {
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

pub(super) fn attention_parts(row: &SessionGlance) -> Vec<String> {
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

pub(super) fn execution_parts(row: &SessionGlance) -> Vec<String> {
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
pub(super) fn glance_of_row(row: &SessionJson) -> SessionGlance {
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
