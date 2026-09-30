use std::process::ExitCode;

use phux_client::resource;
use phux_client::state::{Degradation, StateView};
use phux_core::session_list::SessionListJson;

use phux_protocol::wire::info::{HostInventory, HostSessionInfo, SessionInfo, SessionSnapshot};

use crate::commands::json_err::{self, CliError, codes};
use crate::commands::partial;
use crate::commands::server_target::{ServerSpec, ServerTarget};
use phux_core::host_list::{HostJson, HostKind, HostListJson};

/// `phux ls` — list sessions via `GET_STATE`; never auto-starts a server.
/// `--json` emits the stable [`SessionListJson`] (ADR-0022). A partial listing
/// from a degraded hub still exits 0, reporting the gap on stderr or in the
/// `unreachable` list (see [`partial`]).
pub(crate) fn run_ls(json: bool, server: ServerSpec) -> ExitCode {
    let (rt, target) = match server.prepare("ls", json) {
        Ok(prepared) => prepared,
        Err(code) => return code,
    };
    let listing = rt.block_on(async {
        let mut conn = target.connect().await?;
        phux_client::session_list::listing_view(&mut conn).await
    });
    match listing {
        Ok(view) => render_listing(json, view),
        Err(err) => target.report_unreachable(json, &err, "ls"),
    }
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
                .map(ServerTarget::Remote)
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
                eprintln!("phux: failed to serialize host list as JSON: {err}");
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
                let (snapshot, _) = view.into_parts();
                let mut sessions: Vec<_> = snapshot
                    .sessions
                    .iter()
                    .map(phux_client::session_list::session_json)
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
fn local_hostname() -> String {
    let uname = rustix::system::uname();
    let node = uname.nodename().to_string_lossy();
    let short = node.split('.').next().unwrap_or_default();
    if short.is_empty() {
        "local".to_owned()
    } else {
        short.to_owned()
    }
}

/// The human `--all` listing: one header per machine, its sessions indented
/// beneath, and an unreachable machine's reason in place of its sessions.
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
        for session in &host.sessions {
            let windows = if session.windows == 1 {
                "window"
            } else {
                "windows"
            };
            lines.push(format!(
                "  {}: {} {windows}{}",
                session.name,
                session.windows,
                attached_note(session.attached_clients)
            ));
        }
    }
    lines
}

/// Print one fetched listing in the requested shape.
fn render_listing(json: bool, view: StateView) -> ExitCode {
    let hosts_complete = view.host_sessions_complete();
    let (snapshot, degradation) = view.into_parts();
    if json {
        // Not stderr: a `--json` consumer's channel is the document.
        return print_sessions_json(&snapshot, &degradation, hosts_complete);
    }
    print_sessions(&snapshot);
    if !hosts_complete && has_satellite_terminals(&snapshot) {
        eprintln!("{HOST_SESSIONS_UNAVAILABLE}");
    }
    partial::warn_partial_view("ls", &degradation);
    ExitCode::SUCCESS
}

/// The text-mode note for a hub that does not report its satellites'
/// sessions (no `HOST_SESSIONS`): the satellite Terminals it does list are
/// real, but which sessions they belong to cannot be known from here.
const HOST_SESSIONS_UNAVAILABLE: &str = "phux ls: satellite sessions are unavailable from this \
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
pub(crate) fn print_sessions(snapshot: &SessionSnapshot) {
    let lines = session_lines(snapshot);
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

/// The human `phux ls` body: name-sorted session lines, each followed by its
/// panes' agent sessions, then satellite Terminals. Empty exactly when there is
/// nothing to list ([`EMPTY_STATE`]). A hub reporting a host inventory renders
/// grouped by host ([`host_grouped_lines`]).
fn session_lines(snapshot: &SessionSnapshot) -> Vec<String> {
    if snapshot.hosts().is_empty() {
        return flat_session_lines(snapshot);
    }
    host_grouped_lines(snapshot)
}

/// Header for this host's group in the host-grouped listing.
pub(crate) const LOCAL_HOST_LABEL: &str = "This host";

/// This host's name-sorted session lines with their nested agent-session
/// lines, unindented.
fn local_session_lines(snapshot: &SessionSnapshot) -> Vec<String> {
    let mut sessions: Vec<_> = snapshot.sessions.iter().collect();
    sessions.sort_by(|a, b| a.name.cmp(&b.name));
    let mut lines: Vec<String> = Vec::new();
    for session in sessions {
        lines.push(format_session_line(session));
        lines.extend(agent_session_lines(snapshot, session));
    }
    lines
}

/// The host-grouped listing: a [`LOCAL_HOST_LABEL`] header over this host's
/// sessions, then one header per satellite with its sessions beneath. An
/// unreachable satellite keeps its header, marked, instead of vanishing.
fn host_grouped_lines(snapshot: &SessionSnapshot) -> Vec<String> {
    let mut lines = vec![LOCAL_HOST_LABEL.to_owned()];
    let local = local_session_lines(snapshot);
    if local.is_empty() {
        lines.push("  (no sessions)".to_owned());
    }
    lines.extend(local.into_iter().map(|line| format!("  {line}")));
    for host in snapshot.hosts() {
        lines.extend(satellite_host_lines(snapshot, host));
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
fn flat_session_lines(snapshot: &SessionSnapshot) -> Vec<String> {
    let mut lines = local_session_lines(snapshot);
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
    for window in snapshot
        .windows
        .iter()
        .filter(|window| window.session_id == session.id)
    {
        for pane in resource::terminals(snapshot).filter(|pane| pane.window_id == window.id) {
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
    degradation: &Degradation,
    hosts_complete: bool,
) -> ExitCode {
    let list: SessionListJson =
        phux_client::session_list::document(snapshot, degradation, hosts_complete);
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
        use phux_core::host_list::{HostJson, HostKind, HostListJson};
        use phux_core::session_list::SessionJson;
        let session = |name: &str, clients| SessionJson {
            name: name.to_owned(),
            windows: 2,
            attached: clients > 0,
            attached_clients: clients,
            keep_empty: false,
            empty: false,
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
                "  work: 2 windows (1 client attached)",
                "mini",
                "  (no sessions)",
                "xps  unreachable: did not answer within 3s",
            ]
        );
    }

    use phux_protocol::wire::info::{SessionInfo, SessionSnapshot};
    use phux_protocol::{ResourceId, SessionId, WindowId};

    use super::{EMPTY_STATE, format_session_line, session_lines};

    fn session(name: &str, windows: u16, clients: u16) -> SessionInfo {
        SessionInfo::new(SessionId::new(1), name)
            .with_window_count(windows)
            .with_attached_client_count(clients)
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
        assert!(session_lines(&snapshot).is_empty());

        // The two-line empty state: what is missing, then the exact commands
        // that fix it (the `phux host ls` precedent).
        assert_eq!(EMPTY_STATE.len(), 2);
        assert_eq!(EMPTY_STATE[0], "No sessions.");
        assert!(EMPTY_STATE[1].contains("`phux`"));
        assert!(EMPTY_STATE[1].contains("`phux new NAME -- CMD`"));
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
            session_lines(&snapshot),
            vec![
                "work: 1 window",
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
            session_lines(&federated_snapshot()),
            vec![
                "This host",
                "  work: 1 window",
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
        assert_eq!(session_lines(&snapshot), vec!["work: 1 window"]);
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
            session_lines(&snapshot),
            vec![
                "This host",
                "  work: 1 window",
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
        let lines = session_lines(&snapshot);
        assert_eq!(lines, vec!["alpha: 1 window", "beta: 1 window"]);
    }
}
