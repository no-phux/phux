//! The sidebar's cross-session projections (phux-k0cw).
//!
//! Agents and Sessions describe the whole server rather than only the
//! attached session, so their
//! inputs are the peer caches the driver keeps rather than the workspace it
//! renders. Pure functions over plain data: everything arrives as arguments,
//! nothing is fetched here, and the tests drive them with fully synthetic
//! state.
//!
//! Zero new wire surface (ADR-0030). Every input is something the client
//! already receives — the session graph from `ATTACHED`/`GET_STATE`, peer
//! layouts and agent records from the L3 subscriptions phux-k0cw.5 opened,
//! and peer asked flags from the server-wide event stream the client has
//! always held. A CLI-created session has no persisted TUI layout until a
//! TUI visits it; the server window/resource graph is the inventory until
//! then (phux-ah84).

use std::collections::{HashMap, HashSet};

use phux_protocol::ids::{ResourceId, SessionId};
use phux_protocol::wire::info::{HostInventory, ResourceInfo, SessionInfo, WindowInfo};

use crate::layout::Workspace;
use crate::render::chrome::sidebar::{AgentEntry, SessionRosterEntry, attention_rank};
use phux_client::agent_meta::{AgentAttention, AgentMetaState, AgentRecord};

use super::driver::review::ReviewIndex;

/// Label for a peer pane that asked for a human but declares no agent
/// record, so the strip can say WHAT happened without claiming to know who.
const UNNAMED_AGENT: &str = "unnamed agent";

/// The peer-wide state zones 1 and 3 are projected from.
///
/// Grouped rather than threaded as five more positional parameters: the
/// chrome refresh already carried a `too_many_arguments` allow before this
/// stage, and adding to that list is how a 22-argument function happens (see
/// phux-jx39).
#[derive(Clone, Copy)]
pub(super) struct PeerInputs<'a> {
    /// The serving server's own hostname, never the TUI process's hostname.
    pub serving_host: Option<&'a str>,
    /// Host-qualified satellite inventories, separate from local session ids.
    pub hosts: &'a [HostInventory],
    /// The server's session graph, from the `ATTACHED` snapshot.
    pub sessions: &'a [SessionInfo],
    /// Which of those sessions this client is attached to.
    pub focused_session: Option<SessionId>,
    /// Windows from the same snapshot graph, joined via `WindowInfo::session_id`.
    pub windows: &'a [WindowInfo],
    /// Resources from the same snapshot graph, joined via `ResourceInfo::window_id`.
    pub resources: &'a [ResourceInfo],
    /// Each peer session's persisted pane tree.
    pub foreign_layouts: &'a HashMap<SessionId, Workspace>,
    /// Each peer pane's `phux.agent/v1` record.
    pub foreign_agents: &'a HashMap<ResourceId, AgentRecord>,
    /// Peer panes that raised an ADR-0035 `Asked`.
    pub foreign_attention: &'a HashSet<ResourceId>,
    /// Connection-lifetime review index (phux-deya). Peer rows read `seen`
    /// from here instead of hardcoding unseen.
    pub review: &'a ReviewIndex,
}

impl PeerInputs<'_> {
    /// The peer sessions, in the graph's order, paired with the cached layout
    /// each one has (if any).
    fn ordered_sessions(&self) -> Vec<&SessionInfo> {
        let mut sessions: Vec<_> = self.sessions.iter().collect();
        sessions.sort_by_key(|s| s.id);
        sessions
    }
}

/// One peer terminal as the Agents list visits it.
struct PeerLeaf {
    window: usize,
    pane: Option<usize>,
    id: ResourceId,
    window_name: String,
}

/// A persisted TUI workspace with at least one window. An empty entry is
/// treated as missing so the server graph can still name the session's panes.
fn persisted_layout<'a>(peers: &'a PeerInputs<'_>, session: SessionId) -> Option<&'a Workspace> {
    peers
        .foreign_layouts
        .get(&session)
        .filter(|ws| !ws.windows.is_empty())
}

/// Every pane of `workspace` as `(window index, dfs ordinal, id)`.
fn leaves_with_position(workspace: &Workspace) -> Vec<(usize, usize, ResourceId)> {
    let mut out = Vec::new();
    for (w, window) in workspace.windows.iter().enumerate() {
        if let Some(tree) = window.state.tree.as_ref() {
            for (p, id) in crate::layout::leaves(tree).into_iter().enumerate() {
                out.push((w, p, id));
            }
        }
    }
    out
}

/// Peer terminals in stable session/window/leaf order. A persisted TUI layout
/// wins when it has windows; otherwise the ATTACHED/`GET_STATE` graph is the
/// inventory. Pane ordinals are `Some` only for layout-backed leaves so a
/// click never fabricates a TUI index from the server graph.
fn peer_leaves(peers: &PeerInputs<'_>, session: &SessionInfo) -> Vec<PeerLeaf> {
    if let Some(layout) = persisted_layout(peers, session.id) {
        return leaves_with_position(layout)
            .into_iter()
            .map(|(window, pane, id)| PeerLeaf {
                window,
                pane: Some(pane),
                id,
                window_name: session.name.clone(),
            })
            .collect();
    }
    graph_leaves(peers, session.id)
}

fn graph_leaves(peers: &PeerInputs<'_>, session: SessionId) -> Vec<PeerLeaf> {
    let mut windows: Vec<&WindowInfo> = peers
        .windows
        .iter()
        .filter(|window| window.session_id == session)
        .collect();
    windows.sort_by_key(|window| (window.index, window.id));
    let mut out = Vec::new();
    for window in windows {
        for id in window_terminal_ids(window, peers.resources) {
            out.push(PeerLeaf {
                window: usize::from(window.index),
                pane: None,
                id,
                window_name: window.name.clone(),
            });
        }
    }
    out
}

fn window_terminal_ids(window: &WindowInfo, resources: &[ResourceInfo]) -> Vec<ResourceId> {
    if let Some(layout) = &window.layout {
        let leaves = crate::layout::leaves(layout);
        if !leaves.is_empty() {
            return leaves;
        }
    }
    let mut ids: Vec<_> = resources
        .iter()
        .filter(|resource| resource.window_id == window.id && resource.kind.is_terminal())
        .map(|resource| resource.id.clone())
        .collect();
    ids.sort();
    ids
}

/// Whether `window` holds `id` according to its layout tree or resource list.
pub(super) fn window_contains_terminal(
    window: &WindowInfo,
    resources: &[ResourceInfo],
    id: &ResourceId,
) -> bool {
    window_terminal_ids(window, resources)
        .iter()
        .any(|leaf| leaf == id)
}

/// Resource identities the foreign agent cache should retain: layout leaves
/// when a TUI workspace exists, otherwise the server graph's terminals.
pub(super) fn foreign_terminal_ids(peers: &PeerInputs<'_>) -> HashSet<ResourceId> {
    peers
        .ordered_sessions()
        .into_iter()
        .filter(|session| Some(session.id) != peers.focused_session)
        .flat_map(|session| peer_leaves(peers, session))
        .map(|leaf| leaf.id)
        .collect()
}

/// Full agent list in stable session-id, window and leaf order. Status and
/// review changes affect the row's badge only. Truncation belongs to the
/// painter's fixed Agents panel, never to this projection.
pub(super) fn needs_you_queue(local: Vec<AgentEntry>, peers: &PeerInputs<'_>) -> Vec<AgentEntry> {
    let mut rows = Vec::new();
    let mut local = Some(local);
    for session in peers.ordered_sessions() {
        if Some(session.id) == peers.focused_session {
            rows.extend(local.take().unwrap_or_default());
            continue;
        }
        for leaf in peer_leaves(peers, session) {
            let asked = peers.foreign_attention.contains(&leaf.id);
            let record = peers.foreign_agents.get(&leaf.id);
            // A pane with neither a record nor an ask is a shell, not an
            // agent. The queue lists agents — otherwise every idle prompt on
            // the server competes with a blocked agent for the strip.
            let named = record.is_some_and(|r| !r.name.is_empty());
            if !named && !asked {
                continue;
            }
            // An ask with no record still earns a row: it is blocked on a
            // human by definition, which is the most important thing the
            // strip can say. It just cannot say who.
            let (name, state) = record.filter(|_| named).map_or_else(
                || (UNNAMED_AGENT.to_owned(), AgentMetaState::Blocked),
                |r| (r.name.clone(), r.state),
            );
            let seen = peers.review.is_seen(&leaf.id);
            rows.push(AgentEntry {
                session: Some(session.name.clone()),
                session_id: Some(session.id),
                resource: Some(leaf.id),
                window: leaf.window,
                window_name: leaf.window_name,
                pane: leaf.pane,
                name,
                state,
                attention: asked
                    || record.is_some_and(|r| r.effective_attention() == AgentAttention::High),
                seen,
            });
        }
    }
    // Synthetic or not-yet-snapshotted local state still remains navigable.
    rows.extend(local.unwrap_or_default());
    rows
}

/// Every session, including the current one, with its serving host.
///
/// A session with no cached layout still gets a row — "this space exists" is
/// the roster's whole job, and a session the client cannot describe yet is
/// exactly the one a user is most likely to have forgotten. Its counts are
/// zero and its dot is the quiet rung, which reads as "nothing known", not as
/// "nothing happening".
///
/// Satellite sessions come from the host inventory, not from scanning a
/// local session's leaves. Their agent counts remain explicitly unknown.
pub(super) fn session_roster(
    peers: &PeerInputs<'_>,
    local: &[AgentEntry],
) -> Vec<SessionRosterEntry> {
    let mut out = Vec::new();
    for session in peers.ordered_sessions() {
        let mut entry = SessionRosterEntry {
            name: session.name.clone(),
            id: Some(session.id),
            host: peers.serving_host.unwrap_or("this server").to_owned(),
            active: Some(session.id) == peers.focused_session,
            route_host: None,
            selectable: true,
            blocked: 0,
            working: 0,
            done_unvisited: 0,
            settled: 0,
            unknown: 0,
            satellite: false,
        };
        if entry.active {
            summarize_local_agents(&mut entry, local);
        } else {
            count_peer_leaves(&mut entry, &peer_leaves(peers, session), peers);
        }
        out.push(entry);
    }
    out.extend(satellite_roster(peers.hosts));
    out
}

/// Share the current-session summary between live and headless chrome.
pub(super) fn summarize_local_agents(entry: &mut SessionRosterEntry, agents: &[AgentEntry]) {
    for agent in agents {
        count_rank(
            entry,
            attention_rank(agent.state, agent.attention, agent.seen),
        );
    }
}

/// A remote pane does not change the host of the session containing it.
fn count_peer_leaves(entry: &mut SessionRosterEntry, leaves: &[PeerLeaf], peers: &PeerInputs<'_>) {
    for leaf in leaves {
        if !leaf.id.is_local() {
            entry.unknown += 1;
            continue;
        }
        let asked = peers.foreign_attention.contains(&leaf.id);
        let (state, attention) =
            peers
                .foreign_agents
                .get(&leaf.id)
                .map_or((AgentMetaState::Unknown, asked), |r| {
                    (
                        r.state,
                        asked || r.effective_attention() == AgentAttention::High,
                    )
                });
        count_rank(
            entry,
            attention_rank(state, attention, peers.review.is_seen(&leaf.id)),
        );
    }
}

const fn count_rank(entry: &mut SessionRosterEntry, rank: u8) {
    match rank {
        4 => entry.blocked += 1,
        3 => entry.done_unvisited += 1,
        2 => entry.working += 1,
        1 => entry.settled += 1,
        _ => entry.unknown += 1,
    }
}

/// Satellite session ids are host-local; never merge them with local ids.
fn satellite_roster(hosts: &[HostInventory]) -> Vec<SessionRosterEntry> {
    let mut hosts: Vec<_> = hosts.iter().collect();
    hosts.sort_by(|a, b| a.host.as_str().cmp(b.host.as_str()));
    let mut out = Vec::new();
    for host in hosts {
        let base = SessionRosterEntry {
            host: host.host.to_string(),
            route_host: Some(host.host.to_string()),
            satellite: true,
            selectable: host.is_reachable(),
            ..SessionRosterEntry::default()
        };
        if !host.is_reachable() {
            out.push(SessionRosterEntry {
                name: "unreachable".to_owned(),
                ..base
            });
            continue;
        }
        let mut sessions: Vec<_> = host.sessions.iter().collect();
        sessions.sort_by_key(|s| s.id);
        out.extend(sessions.into_iter().map(|session| SessionRosterEntry {
            name: session.name.clone(),
            unknown: usize::from(session.pane_count),
            ..base.clone()
        }));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Workspace;
    use phux_protocol::ids::WindowId;
    use phux_protocol::wire::info::LayoutNode;

    fn sinfo(id: u32, name: &str) -> SessionInfo {
        SessionInfo::new(SessionId::new(id), name).with_window_count(1)
    }

    fn record(name: &str, state: AgentMetaState) -> AgentRecord {
        AgentRecord {
            name: name.to_owned(),
            state,
            ..AgentRecord::default()
        }
    }

    fn local_row(name: &str, state: AgentMetaState) -> AgentEntry {
        AgentEntry {
            session: None,
            session_id: None,
            resource: None,
            window: 0,
            window_name: "here".to_owned(),
            pane: Some(0),
            name: name.to_owned(),
            state,
            attention: false,
            seen: false,
        }
    }

    fn graph_window(
        session: u32,
        window_id: u32,
        index: u16,
        name: &str,
        leaf: ResourceId,
    ) -> WindowInfo {
        WindowInfo::new(WindowId::new(window_id), SessionId::new(session), name)
            .with_index(index)
            .with_layout(Some(LayoutNode::Leaf(leaf.clone())))
            .with_active_resource(Some(leaf))
    }

    fn graph_resource(id: ResourceId, window: u32) -> ResourceInfo {
        ResourceInfo::new(id, WindowId::new(window), 80, 24)
    }

    struct Fixture {
        sessions: Vec<SessionInfo>,
        windows: Vec<WindowInfo>,
        resources: Vec<ResourceInfo>,
        layouts: HashMap<SessionId, Workspace>,
        agents: HashMap<ResourceId, AgentRecord>,
        attention: HashSet<ResourceId>,
        review: ReviewIndex,
    }

    impl Fixture {
        fn inputs(&self) -> PeerInputs<'_> {
            PeerInputs {
                serving_host: Some("mini"),
                hosts: &[],
                sessions: &self.sessions,
                focused_session: Some(SessionId::new(1)),
                windows: &self.windows,
                resources: &self.resources,
                foreign_layouts: &self.layouts,
                foreign_agents: &self.agents,
                foreign_attention: &self.attention,
                review: &self.review,
            }
        }
    }

    /// One peer session, `peer`, holding two panes.
    fn fixture() -> Fixture {
        let mut ws = Workspace::single(ResourceId::local(10));
        ws.add_window("two".to_owned(), ResourceId::local(11));
        let mut layouts = HashMap::new();
        layouts.insert(SessionId::new(2), ws);
        Fixture {
            sessions: vec![sinfo(1, "here"), sinfo(2, "peer")],
            windows: vec![
                graph_window(2, 10, 0, "main", ResourceId::local(10)),
                graph_window(2, 11, 1, "two", ResourceId::local(11)),
            ],
            resources: vec![
                graph_resource(ResourceId::local(10), 10),
                graph_resource(ResourceId::local(11), 11),
            ],
            layouts,
            agents: HashMap::new(),
            attention: HashSet::new(),
            review: ReviewIndex::default(),
        }
    }

    /// A peer changing state cannot jump ahead of an existing local row.
    #[test]
    fn a_peers_blocked_agent_keeps_its_session_order() {
        let mut f = fixture();
        f.agents.insert(
            ResourceId::local(10),
            record("claude", AgentMetaState::Blocked),
        );

        let rows = needs_you_queue(
            vec![local_row("codex", AgentMetaState::Working)],
            &f.inputs(),
        );

        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[1].name, "claude",
            "the blocked peer stays in its session position: {rows:?}"
        );
        assert_eq!(
            rows[1].session.as_deref(),
            Some("peer"),
            "and it carries the session a click must switch to"
        );
        assert_eq!(
            rows[1].session_id,
            Some(SessionId::new(2)),
            "the click carries the stable id, not only the display name"
        );
        assert_eq!(rows[1].pane, Some(0), "and the pane that wants the human");
        assert_eq!(
            rows[1].resource,
            Some(ResourceId::local(10)),
            "and the stable resource a click must attach to"
        );
        assert_eq!(rows[0].name, "codex");

        f.agents.get_mut(&ResourceId::local(10)).unwrap().state = AgentMetaState::Done;
        let done = needs_you_queue(vec![local_row("codex", AgentMetaState::Idle)], &f.inputs());
        assert_eq!(
            done.iter()
                .map(|e| (&e.name, e.window, e.pane))
                .collect::<Vec<_>>(),
            rows.iter()
                .map(|e| (&e.name, e.window, e.pane))
                .collect::<Vec<_>>()
        );
    }

    /// A pane with no record and no ask is a shell, not an agent. The queue
    /// lists agents — otherwise every idle prompt on the server competes with
    /// a blocked one for the strip.
    #[test]
    fn plain_shells_on_a_peer_do_not_enter_the_queue() {
        let f = fixture();
        let rows = needs_you_queue(Vec::new(), &f.inputs());
        assert!(rows.is_empty(), "{rows:?}");
    }

    /// An ADR-0035 ask alone is enough, even with no record: a peer agent
    /// that asked for a human IS blocked on one, and it is the single most
    /// important row the strip can show.
    #[test]
    fn a_peer_ask_alone_puts_a_row_on_the_queue() {
        let mut f = fixture();
        f.attention.insert(ResourceId::local(11));

        let rows = needs_you_queue(Vec::new(), &f.inputs());
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(rows[0].attention);
        assert_eq!(rows[0].session.as_deref(), Some("peer"));
        assert_eq!(rows[0].session_id, Some(SessionId::new(2)));
    }

    #[test]
    fn the_roster_rolls_a_peer_session_into_one_histogram() {
        let mut f = fixture();
        f.agents.insert(
            ResourceId::local(10),
            record("claude", AgentMetaState::Blocked),
        );
        f.agents.insert(
            ResourceId::local(11),
            record("codex", AgentMetaState::Working),
        );

        let roster = session_roster(&f.inputs(), &[]);
        assert_eq!(
            roster.len(),
            2,
            "current and peer sessions appear: {roster:?}"
        );
        assert!(roster[0].active);
        assert_eq!(roster[0].host, "mini");
        let peer = &roster[1];
        assert_eq!(peer.name, "peer");
        assert_eq!(peer.id, Some(SessionId::new(2)));
        assert_eq!(peer.blocked, 1);
        assert_eq!(peer.working, 1);
        assert_eq!(peer.total(), 2);
        assert_eq!(
            peer.top_rank(),
            attention_rank(AgentMetaState::Blocked, false, false),
            "the session takes its worst pane's rung"
        );
    }

    #[test]
    fn a_reviewed_peer_done_is_settled_not_unvisited() {
        let mut f = fixture();
        let done = ResourceId::local(10);
        f.agents
            .insert(done.clone(), record("claude", AgentMetaState::Done));
        f.review
            .observe_record(&done, f.agents.get(&done), Some(&done));

        let rows = needs_you_queue(Vec::new(), &f.inputs());
        assert_eq!(rows.len(), 1);
        assert!(rows[0].seen, "the peer row must keep the reviewed bit");
        let roster = session_roster(&f.inputs(), &[]);
        let peer = &roster[1];
        assert_eq!(peer.done_unvisited, 0);
        assert_eq!(peer.settled, 1);
    }

    /// A satellite's panes are structurally unknowable from here, so the row
    /// must say `unknown` and never `blocked: 0`. A calm-looking zero on a
    /// session we cannot inspect is the one bug that would discredit the
    /// whole attention surface.
    #[test]
    fn a_satellite_session_reports_unknown_not_zero() {
        let mut f = fixture();
        let sat = ResourceId::satellite("prod-3", 1);
        f.layouts
            .insert(SessionId::new(2), Workspace::single(sat.clone()));
        // Even a record cached from somewhere must not promote it.
        f.agents
            .insert(sat, record("claude", AgentMetaState::Blocked));

        let roster = session_roster(&f.inputs(), &[]);
        let peer = &roster[1];
        assert!(
            !peer.satellite,
            "a remote pane does not change its containing session's host"
        );
        assert_eq!(peer.host, "mini");
        assert_eq!(peer.unknown, 1);
        assert_eq!(peer.blocked, 0);
        assert_eq!(
            peer.top_rank(),
            attention_rank(AgentMetaState::Unknown, false, true),
            "an unknowable session sits on the bottom rung, not a calm one"
        );
    }

    /// A session with no cached layout still gets a row: "this space exists"
    /// is the roster's job. Counts come from the server graph until a TUI
    /// layout lands; an empty graph still reads as "nothing known".
    #[test]
    fn a_peer_with_no_cached_layout_still_gets_a_row() {
        let mut f = fixture();
        f.layouts.clear();

        let roster = session_roster(&f.inputs(), &[]);
        assert_eq!(roster.len(), 2);
        assert_eq!(roster[1].name, "peer");
        assert_eq!(roster[1].total(), 2);
        assert_eq!(roster[1].unknown, 2);

        f.windows.clear();
        f.resources.clear();
        let empty = session_roster(&f.inputs(), &[]);
        assert_eq!(empty[1].name, "peer");
        assert_eq!(empty[1].total(), 0);
    }

    /// CLI-created sessions have a server graph before any TUI layout is
    /// persisted. Agents there must still appear, keyed by `ResourceId`, with
    /// no fabricated pane ordinal.
    #[test]
    fn an_unvisited_peer_agent_appears_from_server_inventory() {
        let mut f = fixture();
        f.layouts.clear();
        f.agents.insert(
            ResourceId::local(10),
            record("reviewer", AgentMetaState::Idle),
        );

        let rows = needs_you_queue(Vec::new(), &f.inputs());
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].name, "reviewer");
        assert_eq!(rows[0].session.as_deref(), Some("peer"));
        assert_eq!(rows[0].session_id, Some(SessionId::new(2)));
        assert_eq!(rows[0].resource, Some(ResourceId::local(10)));
        assert_eq!(
            rows[0].pane, None,
            "graph fallback must not invent a TUI pane ordinal: {rows:?}"
        );

        let roster = session_roster(&f.inputs(), &[]);
        assert_eq!(roster[1].settled, 1);
        assert_eq!(roster[1].total(), 2);
    }

    /// Two terminals in one unvisited session appear once each, in window
    /// then `ResourceId` order, and stay the same rows after a TUI layout
    /// lands.
    #[test]
    fn inventory_rows_are_stable_across_layout_persist() {
        let mut f = fixture();
        f.layouts.clear();
        f.agents.insert(
            ResourceId::local(11),
            record("builder", AgentMetaState::Working),
        );
        f.agents.insert(
            ResourceId::local(10),
            record("reviewer", AgentMetaState::Idle),
        );

        let before = needs_you_queue(Vec::new(), &f.inputs());
        assert_eq!(
            before
                .iter()
                .map(|row| (row.name.as_str(), row.resource.clone(), row.pane))
                .collect::<Vec<_>>(),
            vec![
                ("reviewer", Some(ResourceId::local(10)), None),
                ("builder", Some(ResourceId::local(11)), None),
            ]
        );

        let mut ws = Workspace::single(ResourceId::local(10));
        ws.add_window("two".to_owned(), ResourceId::local(11));
        f.layouts.insert(SessionId::new(2), ws);
        let after = needs_you_queue(Vec::new(), &f.inputs());
        assert_eq!(after.len(), 2, "{after:?}");
        assert_eq!(after[0].resource, Some(ResourceId::local(10)));
        assert_eq!(after[1].resource, Some(ResourceId::local(11)));
        assert_eq!(after[0].name, "reviewer");
        assert_eq!(after[1].name, "builder");
        assert_eq!(after[0].pane, Some(0));
        assert_eq!(after[1].pane, Some(0));
        let ids = foreign_terminal_ids(&f.inputs());
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&ResourceId::local(10)));
        assert!(ids.contains(&ResourceId::local(11)));
    }

    /// Host-qualified resource identities from the server graph keep their identity
    /// on the row so a click cannot collide with a local id of the same
    /// number.
    #[test]
    fn host_qualified_inventory_rows_keep_resource_identity() {
        let mut f = fixture();
        f.layouts.clear();
        let sat = ResourceId::satellite("prod-3", 10);
        f.windows = vec![graph_window(2, 10, 0, "main", sat.clone())];
        f.resources = vec![graph_resource(sat.clone(), 10)];
        f.attention.insert(sat.clone());

        let rows = needs_you_queue(Vec::new(), &f.inputs());
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].resource.as_ref(), Some(&sat));
        assert_eq!(rows[0].pane, None);
        assert!(rows[0].attention);
    }

    #[test]
    fn same_named_sessions_keep_host_qualified_targets() {
        use phux_protocol::wire::info::HostSessionInfo;
        let f = fixture();
        let hosts = vec![
            HostInventory::reachable(
                "devbox".into(),
                vec![
                    HostSessionInfo::new(SessionId::new(1), "here")
                        .with_window_count(2)
                        .with_pane_count(3),
                ],
            ),
            HostInventory::unreachable("offline".into(), "link down"),
        ];
        let mut inputs = f.inputs();
        inputs.hosts = &hosts;
        let roster = session_roster(&inputs, &[]);
        assert_eq!(
            (&roster[0].name, &roster[0].host),
            (&"here".to_owned(), &"mini".to_owned())
        );
        assert!(roster[0].route_host.is_none());
        assert_eq!(roster[2].name, "here");
        assert_eq!(roster[2].host, "devbox");
        assert_eq!(roster[2].route_host.as_deref(), Some("devbox"));
        assert_eq!(roster[2].unknown, 3);
        assert!(roster[2].selectable);
        assert_eq!(roster[3].host, "offline");
        assert!(!roster[3].selectable);
        inputs.serving_host = None;
        assert_eq!(session_roster(&inputs, &[])[0].host, "this server");
    }

    /// The projections feed a change-gated painter, so identical inputs must
    /// produce identical output. If they did not, `refresh_window_chrome`
    /// would report a change every frame and the strip would repaint forever
    /// against the ADR-0029 accumulator.
    #[test]
    fn an_unchanged_projection_does_not_report_a_change() {
        let mut f = fixture();
        f.agents.insert(
            ResourceId::local(10),
            record("claude", AgentMetaState::Blocked),
        );
        let local = vec![local_row("codex", AgentMetaState::Working)];

        assert_eq!(
            needs_you_queue(local.clone(), &f.inputs()),
            needs_you_queue(local, &f.inputs())
        );
        assert_eq!(
            session_roster(&f.inputs(), &[]),
            session_roster(&f.inputs(), &[])
        );
    }
}
