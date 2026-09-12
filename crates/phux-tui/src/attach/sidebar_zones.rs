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
//! already receives — the session graph from `ATTACHED`, peer layouts and
//! agent records from the L3 subscriptions phux-k0cw.5 opened, and peer asked
//! flags from the server-wide event stream the client has always held.

use std::collections::{HashMap, HashSet};

use phux_protocol::ids::{ResourceId, SessionId};
use phux_protocol::wire::info::{HostInventory, SessionInfo};

use crate::layout::Workspace;
use crate::render::chrome::sidebar::{AgentEntry, SessionRosterEntry, attention_rank};
use phux_client::agent_meta::{AgentAttention, AgentMetaState, AgentRecord};

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
    /// Each peer session's persisted pane tree.
    pub foreign_layouts: &'a HashMap<SessionId, Workspace>,
    /// Each peer pane's `phux.agent/v1` record.
    pub foreign_agents: &'a HashMap<ResourceId, AgentRecord>,
    /// Peer panes that raised an ADR-0035 `Asked`.
    pub foreign_attention: &'a HashSet<ResourceId>,
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
        let layout = peers.foreign_layouts.get(&session.id);
        let Some(layout) = layout else { continue };
        for (w, p, id) in leaves_with_position(layout) {
            let asked = peers.foreign_attention.contains(&id);
            let record = peers.foreign_agents.get(&id);
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
            rows.push(AgentEntry {
                session: Some(session.name.clone()),
                window: w,
                window_name: session.name.clone(),
                pane: Some(p),
                name,
                state,
                attention: asked
                    || record.is_some_and(|r| r.effective_attention() == AgentAttention::High),
                seen: false,
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
        } else if let Some(layout) = peers.foreign_layouts.get(&session.id) {
            count_peer_panes(&mut entry, layout, peers);
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
fn count_peer_panes(entry: &mut SessionRosterEntry, layout: &Workspace, peers: &PeerInputs<'_>) {
    for (_, _, id) in leaves_with_position(layout) {
        if !id.is_local() {
            entry.unknown += 1;
            continue;
        }
        let asked = peers.foreign_attention.contains(&id);
        let (state, attention) =
            peers
                .foreign_agents
                .get(&id)
                .map_or((AgentMetaState::Unknown, asked), |r| {
                    (
                        r.state,
                        asked || r.effective_attention() == AgentAttention::High,
                    )
                });
        count_rank(entry, attention_rank(state, attention, false));
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
            window: 0,
            window_name: "here".to_owned(),
            pane: Some(0),
            name: name.to_owned(),
            state,
            attention: false,
            seen: false,
        }
    }

    struct Fixture {
        sessions: Vec<SessionInfo>,
        layouts: HashMap<SessionId, Workspace>,
        agents: HashMap<ResourceId, AgentRecord>,
        attention: HashSet<ResourceId>,
    }

    impl Fixture {
        fn inputs(&self) -> PeerInputs<'_> {
            PeerInputs {
                serving_host: Some("mini"),
                hosts: &[],
                sessions: &self.sessions,
                focused_session: Some(SessionId::new(1)),
                foreign_layouts: &self.layouts,
                foreign_agents: &self.agents,
                foreign_attention: &self.attention,
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
            layouts,
            agents: HashMap::new(),
            attention: HashSet::new(),
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
        assert_eq!(rows[1].pane, Some(0), "and the pane that wants the human");
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
        assert_eq!(peer.blocked, 1);
        assert_eq!(peer.working, 1);
        assert_eq!(peer.total(), 2);
        assert_eq!(
            peer.top_rank(),
            attention_rank(AgentMetaState::Blocked, false, false),
            "the session takes its worst pane's rung"
        );
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
    /// is the roster's job, and a session we cannot describe yet is exactly
    /// the one a user has forgotten about.
    #[test]
    fn a_peer_with_no_cached_layout_still_gets_a_row() {
        let mut f = fixture();
        f.layouts.clear();

        let roster = session_roster(&f.inputs(), &[]);
        assert_eq!(roster.len(), 2);
        assert_eq!(roster[1].name, "peer");
        assert_eq!(roster[1].total(), 0);
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
