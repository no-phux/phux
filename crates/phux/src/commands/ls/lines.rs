//! The full session listing: per-host groups, satellite terminals, agent
//! sessions nested under their pane, and the JSON document.

use std::process::ExitCode;

use phux_client::org::OrgInventory;
use phux_client::resource;
use phux_client::state::Degradation;
use phux_core::activation::SessionLifetime;
use phux_core::session_list::{ExecCounts, SessionJson, SessionListJson};

use phux_protocol::wire::info::{HostInventory, HostSessionInfo, SessionInfo, SessionSnapshot};

/// Header for this host's group in the host-grouped listing.
pub(crate) const LOCAL_HOST_LABEL: &str = "This host";

/// This host's name-sorted session lines with their nested agent-session
/// lines, unindented. With organization records, each session line ends in
/// its key (`work: ...  ^ab12cd`), the shortest prefix no other record
/// shares, so `phux rm ^ab12cd` can address it; the `name:` prefix scripts
/// match stays as it was.
pub(super) fn local_session_lines(
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
pub(super) fn with_key(line: String, key: Option<&str>) -> String {
    match key {
        Some(key) => format!("{line}  {key}"),
        None => line,
    }
}

/// The host-grouped listing: a [`LOCAL_HOST_LABEL`] header over this host's
/// sessions, then one header per satellite with its sessions beneath. An
/// unreachable satellite keeps its header, marked, instead of vanishing.
pub(super) fn host_grouped_lines(
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
pub(super) fn satellite_host_lines(
    snapshot: &SessionSnapshot,
    host: &HostInventory,
) -> Vec<String> {
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
pub(super) fn satellite_terminal_lines(
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
pub(super) fn format_host_session_line(s: &HostSessionInfo) -> String {
    let windows = plural(s.window_count, "window", "windows");
    let panes = plural(s.pane_count, "pane", "panes");
    format!(
        "{}: {windows}, {panes}{}",
        s.name,
        attached_note(s.attached_client_count)
    )
}

pub(super) fn plural(n: u16, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// The pre-host-inventory listing: this host's sessions, then satellite
/// Terminals that cannot be joined to hub-local sessions.
pub(super) fn flat_session_lines(snapshot: &SessionSnapshot, local: Vec<String>) -> Vec<String> {
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
pub(super) fn agent_session_lines(
    snapshot: &SessionSnapshot,
    session: &SessionInfo,
) -> Vec<String> {
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
pub(super) fn exec_summary(exec: &ExecCounts) -> String {
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

pub(super) fn plural32(n: u32, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// The ` (N clients attached)` suffix a session line carries, or nothing at
/// zero. Shared so a satellite session line reads like a local one.
pub(super) fn attached_note(clients: u16) -> String {
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
