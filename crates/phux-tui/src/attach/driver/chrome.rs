//! Window chrome projection: the status-bar badge/hint composers, the
//! window/agent row builders, and the single chrome-refresh chokepoint.

use std::collections::HashMap;

#[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
use phux_protocol::caps::BootstrapCapabilities;
use phux_protocol::ids::{ClientId, ResourceId};
use phux_protocol::wire::frame::ResourceLifecycle;

use crate::attach::agent_rows::AgentSessionRows;
use crate::attach::pane_state::{PaneSlot, VcsIndex};
use crate::attach::server_frame::AgentMetaIndex;
use crate::layout::Workspace;
use crate::render::chrome::sidebar::{AgentEntry, SidebarPainter, attention_rank};
use crate::render::chrome::status_bar::StatusBarPainter;
use phux_client::agent_meta::{AgentAttention, AgentMetaState, AgentRecord, agent_name_from_title};

/// ADR-0033: compose the status-bar supervisory badge for the focused pane,
/// or `None` when it is running and un-leased (so no badge paints). Reads the
/// per-pane lifecycle + input-lease holder tracked from `TerminalControl`
/// events; the holder renders as "you" when it matches this client's own id,
/// else as the other client's numeric id. No emojis (plain ASCII chrome).
fn supervisory_badge(
    panes: &HashMap<ResourceId, PaneSlot>,
    focused_resource: Option<&ResourceId>,
    own_client_id: Option<ClientId>,
) -> Option<String> {
    let slot = panes.get(focused_resource?)?;
    let frozen = matches!(slot.lifecycle, ResourceLifecycle::Frozen);
    format_supervisory_badge(frozen, slot.input_holder, own_client_id)
}

/// Pure badge formatter (split out from [`supervisory_badge`] so the
/// state→string mapping is testable without a libghostty-backed `PaneSlot`).
/// `None` ⇒ no badge (running and un-leased).
fn format_supervisory_badge(
    frozen: bool,
    input_holder: Option<ClientId>,
    own_client_id: Option<ClientId>,
) -> Option<String> {
    let wheel = input_holder.map(|holder| {
        if Some(holder) == own_client_id {
            "WHEEL:you".to_owned()
        } else {
            format!("WHEEL:c{}", holder.get())
        }
    });
    match (frozen, wheel) {
        (false, None) => None,
        (true, None) => Some("[ FROZEN ]".to_owned()),
        (false, Some(w)) => Some(format!("[ {w} ]")),
        (true, Some(w)) => Some(format!("[ FROZEN {w} ]")),
    }
}

/// phux-foz.1: compose the status-bar attention hint, or `None` when no pane
/// is waiting on a human answer. Counts every pane with the ADR-0035 asked
/// flag set (across ALL windows, not just the active one — the hint's job is
/// to surface a question the user cannot currently see).
fn attention_hint(panes: &HashMap<ResourceId, PaneSlot>) -> Option<String> {
    format_attention_hint(panes.values().filter(|slot| slot.attention).count())
}

/// Pure hint formatter (split out from [`attention_hint`] so the count→string
/// mapping is testable without a libghostty-backed `PaneSlot`). `None` ⇒ no
/// hint (nothing is asking). Plain ASCII chrome, matching the ADR-0033
/// supervisory badge convention.
fn format_attention_hint(asking: usize) -> Option<String> {
    match asking {
        0 => None,
        1 => Some("[ ASK ]".to_owned()),
        n => Some(format!("[ ASK x{n} ]")),
    }
}

/// Refresh the window strip AND the supervisory badge together (ADR-0033),
/// plus the phux-foz.1 attention hint.
///
/// All three feed one status-bar paint, so they must stay in lockstep: a site
/// that refreshed the window list on a focus/layout change but forgot the
/// badge would silently desync them. This single chokepoint makes that
/// impossible — every focus/layout-change site calls it instead of
/// hand-rolling the trio.
///
/// Returns `true` when any painter input actually changed, so a caller that
/// paints nothing else (the `chrome_dirty` event path) can gate its repaint
/// on it instead of repainting the full frame for state the user already
/// sees.
#[allow(
    clippy::too_many_arguments,
    reason = "arg list mirrors the driver's chrome state; the ADR-0040 agent index made it 8 and the phux-p4vp vcs index 9"
)]
/// An empty peer bundle for unit tests that exercise the chrome refresh
/// without any cross-session state.
#[cfg(test)]
fn no_peers() -> crate::attach::sidebar_zones::PeerInputs<'static> {
    use std::sync::LazyLock;
    static SESSIONS: &[phux_protocol::wire::info::SessionInfo] = &[];
    static LAYOUTS: LazyLock<HashMap<phux_protocol::ids::SessionId, Workspace>> =
        LazyLock::new(HashMap::new);
    static AGENTS: LazyLock<HashMap<ResourceId, AgentRecord>> = LazyLock::new(HashMap::new);
    static ATTENTION: LazyLock<std::collections::HashSet<ResourceId>> =
        LazyLock::new(std::collections::HashSet::new);
    crate::attach::sidebar_zones::PeerInputs {
        sessions: SESSIONS,
        focused_session: None,
        foreign_layouts: &LAYOUTS,
        foreign_agents: &AGENTS,
        foreign_attention: &ATTENTION,
    }
}

/// Bundle the driver's peer-wide caches for the sidebar's cross-session
/// zones (phux-k0cw).
///
/// A free function rather than a method so the call sites read the same at
/// all eleven of them, and so a test can build one from synthetic state
/// without standing up a driver.
pub(super) const fn peer_inputs<'a>(
    sessions: &'a [phux_protocol::wire::info::SessionInfo],
    focused_session: Option<phux_protocol::ids::SessionId>,
    foreign_layouts: &'a HashMap<phux_protocol::ids::SessionId, Workspace>,
    foreign_agents: &'a HashMap<ResourceId, AgentRecord>,
    foreign_attention: &'a std::collections::HashSet<ResourceId>,
) -> crate::attach::sidebar_zones::PeerInputs<'a> {
    crate::attach::sidebar_zones::PeerInputs {
        sessions,
        focused_session,
        foreign_layouts,
        foreign_agents,
        foreign_attention,
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the chrome refresh is the single chokepoint every painter feeds \
              through; collapsing the list means a context struct, which is \
              phux-jx39's job and not this stage's"
)]
pub(super) fn refresh_window_chrome(
    status_bar: Option<&mut StatusBarPainter>,
    sidebar_painter: &mut SidebarPainter,
    workspace: &Workspace,
    panes: &HashMap<ResourceId, PaneSlot>,
    focused_resource: Option<&ResourceId>,
    zoomed: Option<&ResourceId>,
    own_client_id: Option<ClientId>,
    // ADR-0040: structured `phux.agent/v1` records; a window whose focused
    // leaf carries one is labelled from it instead of the OSC title. The whole
    // index (not just `records`) because the sidebar's agent rows are ORDERED
    // by the attention ladder, whose tiebreak is the index's per-pane
    // last-change clock.
    agent_meta: &AgentMetaIndex,
    // phux-p4vp: pane cwd + branch memo; each window's branch line derives
    // from its focused leaf's working directory.
    vcs: &mut VcsIndex,
    // AgentSession resources under each pane, projected from the kernel's
    // record streams. A pane with one outranks its metadata record for state.
    agent_sessions: &AgentSessionRows,
    // phux-k0cw: the peer-wide state zones 1 and 3 are projected from. One
    // struct rather than five more positional parameters — this function
    // already carried a `too_many_arguments` allow, and growing that list is
    // how a 22-argument function happens (phux-jx39).
    peers: crate::attach::sidebar_zones::PeerInputs<'_>,
) -> bool {
    let windows = window_infos(workspace, panes, zoomed, &agent_meta.records, vcs);
    let mut changed = false;
    if let Some(sb) = status_bar {
        changed |= sb.set_windows(windows.clone());
        changed |= sb.set_supervisory(supervisory_badge(panes, focused_resource, own_client_id));
        changed |= sb.set_attention(attention_hint(panes));
        // phux-foz.4: project the focused pane's data feeds into the bar so
        // the `cwd` / `exit` widgets track focus changes and inbound
        // `cwd_changed` / `command_finished` events through this same
        // chokepoint. Unfocused (or unknown) folds to None => the widgets
        // render nothing.
        let focused = focused_resource.and_then(|id| panes.get(id));
        changed |= sb.set_focused_cwd(focused.and_then(|slot| slot.cwd.clone()));
        changed |= sb.set_last_exit(focused.and_then(|slot| slot.last_exit));
    }
    changed |= sidebar_painter.set_windows(windows);
    // phux-foz.9 / phux-k0cw: zone 1 is the attention queue — the local rows
    // the ADR-0040 records produce, merged with every peer session's and
    // ranked as one list, because urgency ignores locality.
    changed |= sidebar_painter.set_needs_you(crate::attach::sidebar_zones::needs_you_queue(
        agent_entries(workspace, panes, agent_meta, agent_sessions),
        &peers,
    ));
    // phux-k0cw: zone 3 is the roster — one rolled-up line per other session.
    changed |= sidebar_painter.set_roster(crate::attach::sidebar_zones::session_roster(&peers));
    changed
}

/// Snapshot the current workspace as the window widget's input.
///
/// Labels prefer structured agent metadata, then the focused pane's cached
/// OSC 0/2 title, then the stored window name.
pub(super) fn window_infos(
    workspace: &Workspace,
    panes: &HashMap<ResourceId, PaneSlot>,
    // phux-x2hm: the driver's pane-zoom state. The active window's tab gets a
    // `Z` marker (`WindowInfo.zoomed`) when a pane is zoomed; non-active tabs
    // never show it (zoom is per the active window).
    zoomed: Option<&ResourceId>,
    // ADR-0040: Terminal → decoded `phux.agent/v1` record, kept live by the
    // driver's per-pane metadata subscriptions.
    agent_meta: &HashMap<ResourceId, AgentRecord>,
    // phux-p4vp: pane-cwd index + branch memo. The window's branch line is
    // its focused leaf's VCS branch (mut only for the memo).
    vcs: &mut VcsIndex,
) -> Vec<phux_config::widget::WindowInfo> {
    workspace
        .windows
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let focus = w.state.focus.as_ref();
            let agent_label = focus
                .and_then(|fid| agent_meta.get(fid))
                .map(AgentRecord::label);
            let title = focus
                .and_then(|fid| panes.get(fid))
                .map(|slot| slot.last_title.trim())
                .filter(|title| !title.is_empty())
                .map(ToOwned::to_owned);
            let active = i == workspace.active;
            // phux-foz.1: a window carries attention when ANY of its leaves
            // has the ADR-0035 asked flag set — not just the focused leaf —
            // so a question in a background split still marks the tab.
            let attention = w
                .state
                .tree
                .as_ref()
                .map(crate::layout::leaves)
                .unwrap_or_default()
                .iter()
                .any(|id| panes.get(id).is_some_and(|slot| slot.attention));
            // phux-p4vp: the branch line under the label — the focused
            // leaf's cwd resolved to its VCS branch (cached file read).
            let branch = focus.and_then(|fid| vcs.branch_for_pane(fid));
            phux_config::widget::WindowInfo {
                name: agent_label.or(title).unwrap_or_else(|| w.name.clone()),
                active,
                zoomed: active && zoomed.is_some(),
                attention,
                branch,
            }
        })
        .collect()
}

/// phux-foz.9: build the sidebar's agents-section entries — one per
/// agent-running pane, every window's leaves in display order.
///
/// Identity + state per pane, in preference order:
///
/// 1. **The structured `phux.agent/v1` record** (ADR-0040), when the pane
///    declares one: name and state come straight from the record, and the
///    row carries attention when the record's effective attention is high
///    or the pane's ADR-0035 asked flag is up.
/// 2. **The OSC-title identity heuristic**
///    ([`agent_name_from_title`]) — the compatibility path for plain
///    `claude` / `codex` CLI panes, which never call `phux agent set` and
///    so never write a record. State is inferred from the only structured
///    signal the client tracks per pane: the ADR-0035 asked flag maps to
///    `blocked` (the agent is waiting on a human), otherwise `idle` — the
///    same "no blocking cue found" default `phux agent`'s detector uses
///    for a quiet screen, without scanning screen text on the render path.
///
/// Before either, **an `AgentSession` resource bound to the pane** (the
/// record stream the server serves for it): one row per session, its state
/// folded from the stream, its name from the `phux.agent/v1` record when one
/// is declared and the stream's provider otherwise. The stream is the
/// server-enforced source, so it outranks the advisory record for state.
///
/// A pane matching none of these produces no row: the agents section lists
/// agents, not shells.
///
/// # Ordering — the attention ladder
///
/// Rows are NOT in layout order. They are sorted by
/// [`attention_rank`](crate::render::chrome::sidebar::attention_rank)
/// descending, then by most-recent state change descending (a pane that has
/// never changed sorts last), with a STABLE sort so equal-rank, equal-clock
/// rows keep window/leaf order.
///
/// This is the whole "which of my nine agents needs me?" feature. Nine panes
/// tiling a screen is nine rows the user has to read; one row pinned to the
/// top that they must act on is a glance. The rung that carries it is
/// "finished but unreviewed" outranking "still working" — a `done` agent is
/// holding a result hostage until a human reads it; a `working` agent wants
/// nothing.
pub(super) fn agent_entries(
    workspace: &Workspace,
    panes: &HashMap<ResourceId, PaneSlot>,
    agent_meta: &AgentMetaIndex,
    agent_sessions: &AgentSessionRows,
) -> Vec<AgentEntry> {
    // (entry, rank, last-change) — rank and clock drive the sort but never
    // enter `AgentEntry`, which is the sidebar painter's content-cache key.
    let mut rows: Vec<(AgentEntry, u8, Option<std::time::Instant>)> = Vec::new();
    for (i, w) in workspace.windows.iter().enumerate() {
        let leaves = w
            .state
            .tree
            .as_ref()
            .map(crate::layout::leaves)
            .unwrap_or_default();
        for (leaf, id) in leaves.iter().enumerate() {
            let asked = panes.get(id).is_some_and(|slot| slot.attention);
            let seen = panes.get(id).is_some_and(|slot| slot.seen);
            let change_at = agent_meta.change_at.get(id).copied();
            let mut push = |entry: AgentEntry| {
                let rank = attention_rank(entry.state, entry.attention, entry.seen);
                rows.push((entry, rank, change_at));
            };
            if let Some(sessions) = agent_sessions.get(id).filter(|rows| !rows.is_empty()) {
                let record = agent_meta.records.get(id);
                for session in sessions {
                    push(AgentEntry {
                        session: None,
                        window: i,
                        window_name: w.name.clone(),
                        pane: Some(leaf),
                        name: record.map_or_else(|| session.name().to_owned(), |r| r.name.clone()),
                        state: session.state,
                        attention: asked
                            || session.state == AgentMetaState::Blocked
                            || record
                                .is_some_and(|r| r.effective_attention() == AgentAttention::High),
                        seen,
                    });
                }
                continue;
            }
            if let Some(record) = agent_meta.records.get(id) {
                push(AgentEntry {
                    // Local rows: `None` commits the cheap client-local
                    // `select-window` rather than a re-attach (phux-k0cw).
                    session: None,
                    window: i,
                    window_name: w.name.clone(),
                    pane: Some(leaf),
                    name: record.name.clone(),
                    state: record.state,
                    attention: asked || record.effective_attention() == AgentAttention::High,
                    seen,
                });
                continue;
            }
            let title_name = panes
                .get(id)
                .map(|slot| slot.last_title.as_str())
                .and_then(agent_name_from_title);
            if let Some(name) = title_name {
                push(AgentEntry {
                    session: None,
                    window: i,
                    window_name: w.name.clone(),
                    pane: Some(leaf),
                    name: name.to_owned(),
                    state: if asked {
                        AgentMetaState::Blocked
                    } else {
                        AgentMetaState::Idle
                    },
                    attention: asked,
                    seen,
                });
            }
        }
    }
    // Stable: rank desc, then last-change desc (`None` — never changed — sorts
    // last, since `None < Some(_)`), then declaration (window/leaf) order.
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.2.cmp(&a.2)));
    rows.into_iter().map(|(entry, _, _)| entry).collect()
}

/// Mark the focused pane as reviewed — the `seen` half of the attention ladder.
///
/// Returns `true` only on the FLIP (`false` -> `true`), so the caller can
/// schedule a chrome repaint on a real transition and nothing at all in the
/// steady state (the same shape as [`clear_attention_on_input`]).
///
/// The flip MUST be a repaint trigger. `seen` feeds both the sidebar's glyph
/// (the filled `◆` of "finished, unread" vs the quiet `○` of a reviewed row)
/// and its
/// [`attention_rank`](crate::render::chrome::sidebar::attention_rank), and the
/// focus action that made this pane focused recomputed the chrome one iteration
/// EARLIER — while the bit was still `false`. Left as a silent side effect, the
/// strip goes on claiming "finished, unreviewed", pinned above every working
/// agent, about the very pane the user is looking at, until some unrelated
/// chrome event happens to recompute [`agent_entries`].
pub(super) fn mark_focused_seen(
    panes: &mut HashMap<ResourceId, PaneSlot>,
    focused_resource: Option<&ResourceId>,
) -> bool {
    focused_resource
        .and_then(|fid| panes.get_mut(fid))
        .is_some_and(|slot| !std::mem::replace(&mut slot.seen, true))
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use crate::attach::pane_state::{clear_attention_on_input, published_test_state};

    #[test]
    fn supervisory_badge_formats_every_state() {
        // ADR-0033: the focused-pane supervisory badge. Running + un-leased
        // shows nothing; frozen and lease-holder render distinct chips, and the
        // holder is "you" only when it matches this client's own id.
        let me = ClientId::new(7);
        let other = ClientId::new(9);
        assert_eq!(format_supervisory_badge(false, None, Some(me)), None);
        assert_eq!(
            format_supervisory_badge(true, None, Some(me)).as_deref(),
            Some("[ FROZEN ]")
        );
        assert_eq!(
            format_supervisory_badge(false, Some(me), Some(me)).as_deref(),
            Some("[ WHEEL:you ]")
        );
        assert_eq!(
            format_supervisory_badge(false, Some(other), Some(me)).as_deref(),
            Some("[ WHEEL:c9 ]")
        );
        assert_eq!(
            format_supervisory_badge(true, Some(other), Some(me)).as_deref(),
            Some("[ FROZEN WHEEL:c9 ]")
        );
        // No own id yet (pre-ATTACHED): a holder still renders by id, never "you".
        assert_eq!(
            format_supervisory_badge(false, Some(me), None).as_deref(),
            Some("[ WHEEL:c7 ]")
        );
    }

    /// phux-foz.1: the status-bar attention hint. Nothing asking shows
    /// nothing; one asking pane shows the plain chip; several asking panes
    /// carry the count.
    #[test]
    fn attention_hint_formats_every_count() {
        assert_eq!(format_attention_hint(0), None);
        assert_eq!(format_attention_hint(1).as_deref(), Some("[ ASK ]"));
        assert_eq!(format_attention_hint(3).as_deref(), Some("[ ASK x3 ]"));
    }

    /// phux-foz.1: `window_infos` marks a window when ANY of its leaves has
    /// the asked flag — including a non-focused leaf — and only that window.
    #[test]
    fn window_infos_flags_attention_on_the_asking_window() {
        let front = ResourceId::local(1);
        let back = ResourceId::local(2);
        let mut workspace = Workspace::single(front.clone());
        workspace.add_window("2".to_owned(), back.clone());
        workspace.select(0);
        let mut panes: HashMap<ResourceId, PaneSlot> = HashMap::new();
        panes.insert(front, PaneSlot::new_with_size(80, 24).expect("slot"));
        let mut asking = PaneSlot::new_with_size(80, 24).expect("slot");
        asking.attention = true;
        panes.insert(back.clone(), asking);

        let infos = window_infos(
            &workspace,
            &panes,
            None,
            &HashMap::new(),
            &mut VcsIndex::default(),
        );
        assert!(
            !infos[0].attention,
            "quiet window must not carry the marker"
        );
        assert!(
            infos[1].attention,
            "the asking (background) window carries the marker"
        );

        // Clearing the flag clears the marker.
        assert!(clear_attention_on_input(&mut panes, &back));
        let infos = window_infos(
            &workspace,
            &panes,
            None,
            &HashMap::new(),
            &mut VcsIndex::default(),
        );
        assert!(!infos[1].attention);
    }

    #[test]
    fn window_infos_prefers_osc_title_over_stored_name() {
        // A program in the focused leaf sets an OSC 2 window title; the tab
        // strip must show it (tmux automatic-rename / Warp tab titling).
        let id = ResourceId::local(1);
        let workspace = Workspace::single(id.clone());
        let (_, _, panes) = published_test_state(&[(&id, 80, 24, b"\x1b]2;~/src/phux\x07")]);

        let infos = window_infos(
            &workspace,
            &panes,
            None,
            &HashMap::new(),
            &mut VcsIndex::default(),
        );
        assert_eq!(infos.len(), 1);
        assert_eq!(
            infos[0].name, "~/src/phux",
            "the OSC title should label the tab, overriding the stored name"
        );
        assert!(infos[0].active);
    }

    #[test]
    fn window_infos_falls_back_to_stored_name_without_title() {
        // No OSC title set ⇒ the window's stored name ("1" for the first).
        let id = ResourceId::local(1);
        let workspace = Workspace::single(id.clone());
        let mut panes: HashMap<ResourceId, PaneSlot> = HashMap::new();
        panes.insert(id, PaneSlot::new_with_size(80, 24).expect("slot"));

        let infos = window_infos(
            &workspace,
            &panes,
            None,
            &HashMap::new(),
            &mut VcsIndex::default(),
        );
        assert_eq!(infos[0].name, "1");
    }

    #[test]
    fn window_infos_ignores_a_whitespace_only_title() {
        // A title of only spaces is not a useful label; fall back to the name.
        let id = ResourceId::local(1);
        let workspace = Workspace::single(id.clone());
        let mut panes: HashMap<ResourceId, PaneSlot> = HashMap::new();
        let mut slot = PaneSlot::new_with_size(80, 24).expect("slot");
        slot.terminal.vt_write(b"\x1b]2;   \x07");
        panes.insert(id, slot);

        let infos = window_infos(
            &workspace,
            &panes,
            None,
            &HashMap::new(),
            &mut VcsIndex::default(),
        );
        assert_eq!(infos[0].name, "1");
    }

    #[test]
    fn window_infos_prefers_agent_record_over_osc_title() {
        // ADR-0040: a declared `phux.agent/v1` record labels the window from
        // structured data — the OSC title (set here to an unrelated string)
        // must NOT leak through, and no substring parsing is involved.
        let id = ResourceId::local(1);
        let workspace = Workspace::single(id.clone());
        let mut panes: HashMap<ResourceId, PaneSlot> = HashMap::new();
        let mut slot = PaneSlot::new_with_size(80, 24).expect("slot");
        slot.terminal.vt_write(b"\x1b]2;~/src/phux\x07");
        panes.insert(id.clone(), slot);
        let mut records: HashMap<ResourceId, AgentRecord> = HashMap::new();
        records.insert(
            id,
            AgentRecord {
                name: "reviewer".to_owned(),
                state: phux_client::agent_meta::AgentMetaState::Blocked,
                ..AgentRecord::default()
            },
        );

        let infos = window_infos(&workspace, &panes, None, &records, &mut VcsIndex::default());
        assert_eq!(
            infos[0].name, "!reviewer (blocked)",
            "structured record must beat the OSC title"
        );
    }

    #[test]
    fn window_infos_falls_back_to_title_when_record_cleared() {
        // ADR-0040 compatibility path: no record ⇒ the OSC title labels the
        // tab exactly as before.
        let id = ResourceId::local(1);
        let workspace = Workspace::single(id.clone());
        let (_, _, panes) = published_test_state(&[(&id, 80, 24, b"\x1b]2;claude task\x07")]);

        let infos = window_infos(
            &workspace,
            &panes,
            None,
            &HashMap::new(),
            &mut VcsIndex::default(),
        );
        assert_eq!(infos[0].name, "claude task");
    }

    /// An `AgentMetaIndex` holding `records` and nothing else — the shape
    /// `agent_entries` reads.
    fn meta_index(records: HashMap<ResourceId, AgentRecord>) -> AgentMetaIndex {
        AgentMetaIndex {
            records,
            ..AgentMetaIndex::default()
        }
    }

    /// The attention ladder, end to end through `agent_entries`: an UNSEEN
    /// `done` agent must sort ABOVE a `working` one, and a `blocked` one above
    /// both. This is the "which of my agents needs me?" contract — a finished
    /// agent is holding a result hostage until a human reads it, so it must
    /// outrank one that is merely still busy.
    #[test]
    fn agent_entries_rank_unreviewed_done_above_working() {
        let working = ResourceId::local(1);
        let done = ResourceId::local(2);
        let blocked = ResourceId::local(3);
        let mut workspace = Workspace::single(working.clone());
        workspace.add_window("w2".to_owned(), done.clone());
        workspace.add_window("w3".to_owned(), blocked.clone());

        let mut panes: HashMap<ResourceId, PaneSlot> = HashMap::new();
        for id in [&working, &done, &blocked] {
            panes.insert(id.clone(), PaneSlot::new_with_size(80, 24).expect("slot"));
        }
        let mut records: HashMap<ResourceId, AgentRecord> = HashMap::new();
        for (id, name, state) in [
            (&working, "w", AgentMetaState::Working),
            (&done, "d", AgentMetaState::Done),
            (&blocked, "b", AgentMetaState::Blocked),
        ] {
            records.insert(
                id.clone(),
                AgentRecord {
                    name: name.to_owned(),
                    state,
                    ..AgentRecord::default()
                },
            );
        }

        // Layout order is working, done, blocked. The ladder must reorder.
        let entries = agent_entries(
            &workspace,
            &panes,
            &meta_index(records.clone()),
            &HashMap::new(),
        );
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["b", "d", "w"],
            "blocked > unreviewed done > working"
        );

        // Visiting the finished pane demotes it below the working one: it has
        // been reviewed, so it is no longer asking for anything.
        panes.get_mut(&done).expect("slot").seen = true;
        let entries = agent_entries(&workspace, &panes, &meta_index(records), &HashMap::new());
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["b", "w", "d"],
            "a reviewed done drops below working"
        );
    }

    /// The TRIGGER half of the ladder's central promise: focusing a pane must
    /// not just flip `seen`, it must be OBSERVABLE, so the driver can recompute
    /// the chrome and repaint. The flip used to be a silent side effect at the
    /// top of the loop, one iteration AFTER the focus action already recomputed
    /// (and painted) the chrome with the stale bit — so the strip went on
    /// showing `◆ done` bold, pinned above every working agent, about the pane
    /// the user was staring at, until an unrelated chrome event fired.
    ///
    /// The contract: the flip reports `true` exactly once, that flip makes
    /// `refresh_window_chrome` report a real change (a demoted row + a new
    /// glyph), and the steady state — re-marking an already-seen pane — reports
    /// `false`, so an idle loop pass costs one hash lookup and nothing else.
    #[test]
    fn focusing_an_unreviewed_done_pane_flips_seen_and_dirties_the_chrome() {
        let working = ResourceId::local(1);
        let done = ResourceId::local(2);
        let mut workspace = Workspace::single(working.clone());
        workspace.add_window("w2".to_owned(), done.clone());

        let mut panes: HashMap<ResourceId, PaneSlot> = HashMap::new();
        for id in [&working, &done] {
            panes.insert(id.clone(), PaneSlot::new_with_size(80, 24).expect("slot"));
        }
        let mut records: HashMap<ResourceId, AgentRecord> = HashMap::new();
        for (id, name, state) in [
            (&working, "w", AgentMetaState::Working),
            (&done, "d", AgentMetaState::Done),
        ] {
            records.insert(
                id.clone(),
                AgentRecord {
                    name: name.to_owned(),
                    state,
                    ..AgentRecord::default()
                },
            );
        }
        let meta = meta_index(records);

        // The background agent finished while another pane was focused, so its
        // row is unreviewed: pinned to the top.
        let names: Vec<String> = agent_entries(&workspace, &panes, &meta, &HashMap::new())
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, vec!["d", "w"], "unreviewed done pins to the top");

        // Prime the painters against that (stale) view — this is the paint the
        // focus action itself produced, one iteration before the flip.
        let mut sidebar_painter = SidebarPainter::new(crate::render::Theme::default());
        let mut vcs = VcsIndex::default();
        refresh_window_chrome(
            None,
            &mut sidebar_painter,
            &workspace,
            &panes,
            Some(&done),
            None,
            None,
            &meta,
            &mut vcs,
            &HashMap::new(),
            no_peers(),
        );

        // The user is now looking at the finished pane.
        assert!(
            mark_focused_seen(&mut panes, Some(&done)),
            "the first mark after a focus change must report the flip"
        );

        // The flip must move the chrome: the row demotes below the working
        // agent, and its glyph stops shouting.
        let chrome_changed = refresh_window_chrome(
            None,
            &mut sidebar_painter,
            &workspace,
            &panes,
            Some(&done),
            None,
            None,
            &meta,
            &mut vcs,
            &HashMap::new(),
            no_peers(),
        );
        assert!(
            chrome_changed,
            "the seen flip must dirty the chrome, or nothing repaints the strip"
        );
        let entries = agent_entries(&workspace, &panes, &meta, &HashMap::new());
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["w", "d"],
            "the reviewed row drops below working"
        );
        // Only the FOCUSED pane's row is reviewed — the background `working`
        // one is still unvisited, and the glyph derives from this bit.
        let reviewed: Vec<(&str, bool)> =
            entries.iter().map(|e| (e.name.as_str(), e.seen)).collect();
        assert_eq!(
            reviewed,
            vec![("w", false), ("d", true)],
            "the focused pane's row — and only it — must carry the reviewed bit"
        );

        // Steady state: no flip, no chrome change, no paint.
        assert!(
            !mark_focused_seen(&mut panes, Some(&done)),
            "re-marking an already-seen pane must not report a flip"
        );
        assert!(
            !refresh_window_chrome(
                None,
                &mut sidebar_painter,
                &workspace,
                &panes,
                Some(&done),
                None,
                None,
                &meta,
                &mut vcs,
                &HashMap::new(),
                no_peers(),
            ),
            "an unchanged chrome must stay zero-cost"
        );
    }

    /// Equal-rank rows break the tie on the last-change clock: the agent that
    /// JUST blocked sits above one that has been blocked for an hour. Rows with
    /// no recorded change sort last, and the sort is stable, so a tie in both
    /// keys preserves window/leaf order.
    #[test]
    fn agent_entries_break_rank_ties_by_most_recent_change() {
        let old = ResourceId::local(1);
        let fresh = ResourceId::local(2);
        let never = ResourceId::local(3);
        let mut workspace = Workspace::single(old.clone());
        workspace.add_window("w2".to_owned(), fresh.clone());
        workspace.add_window("w3".to_owned(), never.clone());

        let mut panes: HashMap<ResourceId, PaneSlot> = HashMap::new();
        let mut records: HashMap<ResourceId, AgentRecord> = HashMap::new();
        for (id, name) in [(&old, "old"), (&fresh, "fresh"), (&never, "never")] {
            panes.insert(id.clone(), PaneSlot::new_with_size(80, 24).expect("slot"));
            records.insert(
                id.clone(),
                AgentRecord {
                    name: name.to_owned(),
                    state: AgentMetaState::Blocked,
                    ..AgentRecord::default()
                },
            );
        }

        let now = std::time::Instant::now();
        let mut index = meta_index(records);
        index.change_at.insert(
            old,
            now.checked_sub(std::time::Duration::from_secs(60))
                .expect("clock has an hour of headroom"),
        );
        index.change_at.insert(fresh, now);
        // `never` has no clock entry at all.

        let entries = agent_entries(&workspace, &panes, &index, &HashMap::new());
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["fresh", "old", "never"]);
    }

    /// phux-foz.9: a declared `phux.agent/v1` record produces an agents-row
    /// entry with the record's name + state; the pane's OSC title (set to a
    /// conflicting agent name here) is never consulted when a record exists.
    #[test]
    fn agent_entries_prefer_the_declared_record() {
        let id = ResourceId::local(1);
        let workspace = Workspace::single(id.clone());
        let mut panes: HashMap<ResourceId, PaneSlot> = HashMap::new();
        let mut slot = PaneSlot::new_with_size(80, 24).expect("slot");
        slot.terminal.vt_write(b"\x1b]2;codex resume\x07");
        panes.insert(id.clone(), slot);
        let mut records: HashMap<ResourceId, AgentRecord> = HashMap::new();
        records.insert(
            id,
            AgentRecord {
                name: "merge-queue-w5".to_owned(),
                state: AgentMetaState::Working,
                ..AgentRecord::default()
            },
        );

        let entries = agent_entries(&workspace, &panes, &meta_index(records), &HashMap::new());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].window, 0);
        assert_eq!(
            entries[0].window_name, "1",
            "stored window name, herdr's workspace column"
        );
        assert_eq!(entries[0].name, "merge-queue-w5");
        assert_eq!(entries[0].state, AgentMetaState::Working);
        assert!(!entries[0].attention);
    }

    /// phux-foz.9: no record => the OSC-title heuristic identifies plain
    /// `claude` / `codex` CLI panes; state is `idle` until the pane's
    /// ADR-0035 asked flag flips it to `blocked`. A pane matching neither
    /// source produces no row.
    #[test]
    fn agent_entries_fall_back_to_the_title_heuristic() {
        let claude = ResourceId::local(1);
        let shell = ResourceId::local(2);
        let mut workspace = Workspace::single(claude.clone());
        workspace.add_window("scratch".to_owned(), shell.clone());
        let (_, _, mut panes) = published_test_state(&[
            (&claude, 80, 24, b"\x1b]2;Claude Code - ~/src/phux\x07"),
            (&shell, 80, 24, b"\x1b]2;~/src/phux\x07"),
        ]);

        let entries = agent_entries(
            &workspace,
            &panes,
            &AgentMetaIndex::default(),
            &HashMap::new(),
        );
        assert_eq!(entries.len(), 1, "the plain shell pane must not list");
        assert_eq!(entries[0].name, "claude");
        assert_eq!(entries[0].state, AgentMetaState::Idle);
        assert!(!entries[0].attention);

        // The asked flag (ADR-0035) is the one structured state signal the
        // fallback trusts: it flips the row to blocked + attention.
        panes.get_mut(&claude).expect("slot").attention = true;
        let entries = agent_entries(
            &workspace,
            &panes,
            &AgentMetaIndex::default(),
            &HashMap::new(),
        );
        assert_eq!(entries[0].state, AgentMetaState::Blocked);
        assert!(entries[0].attention);
    }

    /// phux-foz.9: a record declaring (or deriving) high attention marks
    /// the entry even without the asked flag.
    #[test]
    fn agent_entries_carry_record_attention() {
        let id = ResourceId::local(1);
        let workspace = Workspace::single(id.clone());
        let mut panes: HashMap<ResourceId, PaneSlot> = HashMap::new();
        panes.insert(id.clone(), PaneSlot::new_with_size(80, 24).expect("slot"));
        let mut records: HashMap<ResourceId, AgentRecord> = HashMap::new();
        records.insert(
            id,
            AgentRecord {
                name: "reviewer".to_owned(),
                // Blocked derives high attention when none is declared.
                state: AgentMetaState::Blocked,
                ..AgentRecord::default()
            },
        );

        let entries = agent_entries(&workspace, &panes, &meta_index(records), &HashMap::new());
        assert!(entries[0].attention);
    }

    /// An `AgentSession` stream bound to a pane is the server-enforced state
    /// source: its state outranks the advisory `phux.agent/v1` record, while
    /// the record still names the row when it exists.
    #[test]
    fn agent_entries_take_state_from_the_stream_and_name_from_the_record() {
        use crate::attach::agent_rows::AgentSessionRow;
        let id = ResourceId::local(1);
        let workspace = Workspace::single(id.clone());
        let mut panes: HashMap<ResourceId, PaneSlot> = HashMap::new();
        panes.insert(id.clone(), PaneSlot::new_with_size(80, 24).expect("slot"));
        let mut records: HashMap<ResourceId, AgentRecord> = HashMap::new();
        records.insert(
            id.clone(),
            AgentRecord {
                name: "reviewer".to_owned(),
                state: AgentMetaState::Idle,
                ..AgentRecord::default()
            },
        );
        let mut sessions: AgentSessionRows = HashMap::new();
        sessions.insert(
            id,
            vec![AgentSessionRow {
                id: ResourceId::local(9),
                provider: Some("claude".to_owned()),
                native_id: Some("s-1".to_owned()),
                state: AgentMetaState::Blocked,
            }],
        );

        let entries = agent_entries(&workspace, &panes, &meta_index(records), &sessions);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "reviewer", "the record names the row");
        assert_eq!(
            entries[0].state,
            AgentMetaState::Blocked,
            "the stream's state outranks the record's"
        );
        assert!(entries[0].attention, "a blocked stream wants a human");
        assert_eq!(entries[0].pane, Some(0), "the parent pane is the target");
    }

    /// Without a record the stream still yields a row: the provider is the
    /// name, and a pane whose OSC title says nothing agent-like still lists.
    #[test]
    fn agent_entries_list_a_stream_without_any_record() {
        use crate::attach::agent_rows::AgentSessionRow;
        let id = ResourceId::local(1);
        let workspace = Workspace::single(id.clone());
        let mut panes: HashMap<ResourceId, PaneSlot> = HashMap::new();
        panes.insert(id.clone(), PaneSlot::new_with_size(80, 24).expect("slot"));
        let mut sessions: AgentSessionRows = HashMap::new();
        sessions.insert(
            id,
            vec![
                AgentSessionRow {
                    id: ResourceId::local(8),
                    provider: Some("codex".to_owned()),
                    native_id: None,
                    state: AgentMetaState::Working,
                },
                AgentSessionRow {
                    id: ResourceId::local(9),
                    provider: None,
                    native_id: None,
                    state: AgentMetaState::Done,
                },
            ],
        );

        let entries = agent_entries(&workspace, &panes, &AgentMetaIndex::default(), &sessions);
        let names: Vec<(&str, AgentMetaState)> =
            entries.iter().map(|e| (e.name.as_str(), e.state)).collect();
        // Unreviewed `done` outranks `working`; the nameless session reads
        // as a plain agent.
        assert_eq!(
            names,
            vec![
                ("agent", AgentMetaState::Done),
                ("codex", AgentMetaState::Working)
            ]
        );
        assert!(!entries[1].attention);
    }

    #[test]
    fn window_infos_flags_zoom_only_on_the_active_window() {
        // phux-x2hm: the active window's `zoomed` reflects the zoom state;
        // a non-active window is never marked zoomed.
        let active = ResourceId::local(1);
        let mut workspace = Workspace::single(active.clone());
        workspace.add_window("2".to_owned(), ResourceId::local(2));
        workspace.select(0); // active window is index 0
        let panes: HashMap<ResourceId, PaneSlot> = HashMap::new();

        let infos = window_infos(
            &workspace,
            &panes,
            Some(&active),
            &HashMap::new(),
            &mut VcsIndex::default(),
        );
        assert!(infos[0].zoomed, "active window reflects the zoom state");
        assert!(!infos[1].zoomed, "a non-active window is never zoomed");

        // No zoom ⇒ no window is marked.
        let infos = window_infos(
            &workspace,
            &panes,
            None,
            &HashMap::new(),
            &mut VcsIndex::default(),
        );
        assert!(!infos[0].zoomed && !infos[1].zoomed);
    }
}
