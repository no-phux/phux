//! The sidebar's cross-session projections (phux-k0cw).
//!
//! Zone 1 (`needs you`) and zone 3 (`spaces`) are the two parts of the strip
//! that describe the whole server rather than the attached session, so their
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

use phux_protocol::ids::{SessionId, TerminalId};
use phux_protocol::wire::info::SessionInfo;

use crate::agent_meta::{AgentAttention, AgentMetaState, AgentRecord};
use crate::layout::Workspace;
use crate::render::chrome::sidebar::{AgentEntry, SessionRosterEntry, attention_rank};

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
    /// The server's session graph, from the `ATTACHED` snapshot.
    pub sessions: &'a [SessionInfo],
    /// Which of those sessions this client is attached to.
    pub focused_session: Option<SessionId>,
    /// Each peer session's persisted pane tree.
    pub foreign_layouts: &'a HashMap<SessionId, Workspace>,
    /// Each peer pane's `phux.agent/v1` record.
    pub foreign_agents: &'a HashMap<TerminalId, AgentRecord>,
    /// Peer panes that raised an ADR-0035 `Asked`.
    pub foreign_attention: &'a HashSet<TerminalId>,
}

impl PeerInputs<'_> {
    /// The peer sessions, in the graph's order, paired with the cached layout
    /// each one has (if any).
    fn peers(&self) -> impl Iterator<Item = (&SessionInfo, Option<&Workspace>)> {
        self.sessions
            .iter()
            .filter(move |s| Some(s.id) != self.focused_session)
            .map(move |s| (s, self.foreign_layouts.get(&s.id)))
    }
}

/// Every pane of `workspace` as `(window index, dfs ordinal, id)`.
fn leaves_with_position(workspace: &Workspace) -> Vec<(usize, usize, TerminalId)> {
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

/// The cross-session attention queue, most-wanting-a-human first.
///
/// `local` is the attached session's rows, already ranked by the driver;
/// peers are appended and the whole list re-sorted by [`attention_rank`]. The
/// sort is STABLE, which is what gives the tiebreak its shape: local rows
/// keep the driver's last-change ordering, and peer rows follow in the
/// session graph's order below any local row of equal rank.
///
/// Two honest degradations, both structural rather than oversights:
///
/// - **Peer rows have no clock.** The per-pane last-change map is keyed by
///   local `TerminalId`, so two peer agents that both went `blocked` cannot
///   be ordered by recency. They hold declaration order instead.
/// - **Peer rows are never `seen`.** A pane is marked seen by focusing it,
///   which for a peer means switching sessions — at which point it stops
///   being a peer. So a peer's finished-and-unread agent stays on the
///   done-unvisited rung until someone goes and looks, which is the correct
///   behaviour for an inbox but means the queue never self-clears from a
///   distance.
///
/// Returns the FULL ranked list. Truncation belongs to the row model, so the
/// `+N more` count can be honest about what it dropped.
pub(super) fn needs_you_queue(local: Vec<AgentEntry>, peers: &PeerInputs<'_>) -> Vec<AgentEntry> {
    let mut rows = local;
    for (session, layout) in peers.peers() {
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
    rows.sort_by(|a, b| {
        attention_rank(b.state, b.attention, b.seen).cmp(&attention_rank(
            a.state,
            a.attention,
            a.seen,
        ))
    });
    rows
}

/// One roster line per peer session, rolled up from its cached panes.
///
/// A session with no cached layout still gets a row — "this space exists" is
/// the roster's whole job, and a session the client cannot describe yet is
/// exactly the one a user is most likely to have forgotten. Its counts are
/// zero and its dot is the quiet rung, which reads as "nothing known", not as
/// "nothing happening".
///
/// A **satellite** session reports every pane as `unknown` and never as
/// `blocked: 0`. Its per-Terminal metadata is normatively unsubscribable
/// (`docs/spec/L3.md`), so a zero there would not be a measurement — it would
/// be an attention surface lying by omission, which is the one failure mode
/// that discredits the whole strip.
pub(super) fn session_roster(peers: &PeerInputs<'_>) -> Vec<SessionRosterEntry> {
    let mut out = Vec::new();
    for (session, layout) in peers.peers() {
        let mut entry = SessionRosterEntry {
            name: session.name.clone(),
            blocked: 0,
            working: 0,
            done_unvisited: 0,
            settled: 0,
            unknown: 0,
            satellite: false,
        };
        if let Some(layout) = layout {
            for (_, _, id) in leaves_with_position(layout) {
                if !id.is_local() {
                    entry.satellite = true;
                    entry.unknown += 1;
                    continue;
                }
                let asked = peers.foreign_attention.contains(&id);
                let record = peers.foreign_agents.get(&id);
                let (state, attention) = record.map_or((AgentMetaState::Unknown, asked), |r| {
                    (
                        r.state,
                        asked || r.effective_attention() == AgentAttention::High,
                    )
                });
                // A peer pane is never `seen` (see `needs_you_queue`), so the
                // roster and the queue classify the same pane identically.
                match attention_rank(state, attention, false) {
                    4 => entry.blocked += 1,
                    3 => entry.done_unvisited += 1,
                    2 => entry.working += 1,
                    1 => entry.settled += 1,
                    _ => entry.unknown += 1,
                }
            }
        }
        out.push(entry);
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
        agents: HashMap<TerminalId, AgentRecord>,
        attention: HashSet<TerminalId>,
    }

    impl Fixture {
        fn inputs(&self) -> PeerInputs<'_> {
            PeerInputs {
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
        let mut ws = Workspace::single(TerminalId::local(10));
        ws.add_window("two".to_owned(), TerminalId::local(11));
        let mut layouts = HashMap::new();
        layouts.insert(SessionId::new(2), ws);
        Fixture {
            sessions: vec![sinfo(1, "here"), sinfo(2, "peer")],
            layouts,
            agents: HashMap::new(),
            attention: HashSet::new(),
        }
    }

    /// THE point of a cross-session queue: urgency ignores locality. A peer's
    /// blocked agent must outrank a local working one, or the user still has
    /// to go looking — which is the keyhole the whole design replaces.
    #[test]
    fn a_peers_blocked_agent_outranks_a_local_working_one() {
        let mut f = fixture();
        f.agents.insert(
            TerminalId::local(10),
            record("claude", AgentMetaState::Blocked),
        );

        let rows = needs_you_queue(
            vec![local_row("codex", AgentMetaState::Working)],
            &f.inputs(),
        );

        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0].name, "claude",
            "the blocked peer is first: {rows:?}"
        );
        assert_eq!(
            rows[0].session.as_deref(),
            Some("peer"),
            "and it carries the session a click must switch to"
        );
        assert_eq!(rows[0].pane, Some(0), "and the pane that wants the human");
        assert_eq!(rows[1].name, "codex");
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
        f.attention.insert(TerminalId::local(11));

        let rows = needs_you_queue(Vec::new(), &f.inputs());
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(rows[0].attention);
        assert_eq!(rows[0].session.as_deref(), Some("peer"));
    }

    #[test]
    fn the_roster_rolls_a_peer_session_into_one_histogram() {
        let mut f = fixture();
        f.agents.insert(
            TerminalId::local(10),
            record("claude", AgentMetaState::Blocked),
        );
        f.agents.insert(
            TerminalId::local(11),
            record("codex", AgentMetaState::Working),
        );

        let roster = session_roster(&f.inputs());
        assert_eq!(roster.len(), 1, "only peers appear: {roster:?}");
        let peer = &roster[0];
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
        let sat = TerminalId::satellite("prod-3", 1);
        f.layouts
            .insert(SessionId::new(2), Workspace::single(sat.clone()));
        // Even a record cached from somewhere must not promote it.
        f.agents
            .insert(sat, record("claude", AgentMetaState::Blocked));

        let roster = session_roster(&f.inputs());
        let peer = &roster[0];
        assert!(peer.satellite, "the row is marked as a satellite");
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

        let roster = session_roster(&f.inputs());
        assert_eq!(roster.len(), 1);
        assert_eq!(roster[0].name, "peer");
        assert_eq!(roster[0].total(), 0);
    }

    /// The projections feed a change-gated painter, so identical inputs must
    /// produce identical output. If they did not, `refresh_window_chrome`
    /// would report a change every frame and the strip would repaint forever
    /// against the ADR-0029 accumulator.
    #[test]
    fn an_unchanged_projection_does_not_report_a_change() {
        let mut f = fixture();
        f.agents.insert(
            TerminalId::local(10),
            record("claude", AgentMetaState::Blocked),
        );
        let local = vec![local_row("codex", AgentMetaState::Working)];

        assert_eq!(
            needs_you_queue(local.clone(), &f.inputs()),
            needs_you_queue(local, &f.inputs())
        );
        assert_eq!(session_roster(&f.inputs()), session_roster(&f.inputs()));
    }
}
