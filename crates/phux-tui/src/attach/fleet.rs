//! Agent-fleet dashboard model: a pure client-side projection (ADR-0030) of
//! every pane with its agent identity (ADR-0040), asked state (ADR-0035),
//! and branch/cwd, grouped under session headers and committing focus
//! through `run_action`.
//!
//! Foreign sessions come from two existing, subscribed L3 keys: the peer's
//! persisted `phux.tui.layout/v1/<session>` pane tree and each pane's
//! `phux.agent/v1` record; server-wide spawn/close events keep the set
//! current. A peer row commits a one-step `switch-session { name, window,
//! pane }`; a peer with no cached layout gets one "switch to session" row.
//! Satellite agents are listed by agent name with a host badge (ADR-0136).
//! Foreign rows carry no branch/cwd (no local slot).
//!
//! ```text
//! work (current)                                   <- session header
//!   ● 0:main.0 reviewer [claude]  blocked · main   <- pane row
//!   ○ 1:logs.0 tail -f                       logs
//! scratch                                          <- foreign session
//!   ◐ 0:main.0 packer [codex]     working
//! ```
//!
//! Glyphs are the chrome's badge vocabulary (`●` blocked, `◐` working, `◆`
//! done, `○` idle/unknown); a pane with no record falls back to its OSC
//! title. Attention rows use the theme's `attention` slot.

use std::collections::{BTreeMap, HashMap, HashSet};

use phux_protocol::ResourceId;
use phux_protocol::ids::SessionId;
use phux_protocol::wire::info::SessionInfo;

use super::agent_rows::{AgentSessionRow, AgentSessionRows};
use super::pane_state::{PaneSlot, VcsIndex};
use crate::layout::ArrangementDoc;
use crate::render::chrome::{AGENT_BLOCKED_GLYPH, AGENT_IDLE_GLYPH, agent_state_glyph};
use crate::render::overlay::SelectItem;
use phux_client::agent_meta::{AgentAttention, AgentMetaState, AgentRecord};

/// The fleet overlay's live-refresh tag.
pub(super) const FLEET_LIVE_KEY: &str = "agent-fleet";

/// Per-pane display metadata for one fleet row (plain data, so
/// [`fleet_items`] is pure).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct FleetPaneMeta {
    /// ADR-0035 asked flag: an agent in this pane is waiting on a human
    /// answer ([`PaneSlot::attention`]).
    pub attention: bool,
    /// The pane's OSC 0/2 title, trimmed and non-empty — the ADR-0040
    /// compatibility fallback when no agent record is declared.
    pub title: Option<String>,
    /// The pane's working directory as last announced (snapshot cwd
    /// refined by `cwd_changed` events).
    pub cwd: Option<String>,
    /// The VCS branch of `cwd`, when it resolves inside a repository
    /// (phux-p4vp cached `.git/HEAD` read — never a `git` subprocess).
    pub branch: Option<String>,
    /// `AgentSession` resources bound to this pane, from the kernel's record
    /// streams. Each one is its own row; the pane is their click target.
    pub sessions: Vec<AgentSessionRow>,
}

/// Snapshot the fleet-relevant metadata of every live pane (asked flag,
/// title, cwd, memoized branch).
pub(super) fn collect_pane_meta(
    panes: &HashMap<ResourceId, PaneSlot>,
    vcs: &mut VcsIndex,
    agent_sessions: &AgentSessionRows,
) -> HashMap<ResourceId, FleetPaneMeta> {
    panes
        .iter()
        .map(|(id, slot)| {
            let title =
                (!slot.last_title.trim().is_empty()).then(|| slot.last_title.trim().to_owned());
            // Prefer the live cwd (refined by cwd_changed events); fall
            // back to the snapshot-seeded index the sidebar branch uses.
            let branch = slot
                .cwd
                .as_deref()
                .and_then(|cwd| vcs.branch_for_cwd(cwd))
                .or_else(|| vcs.branch_for_pane(id));
            (
                id.clone(),
                FleetPaneMeta {
                    attention: slot.attention,
                    title,
                    cwd: slot.cwd.clone(),
                    branch,
                    sessions: agent_sessions.get(id).cloned().unwrap_or_default(),
                },
            )
        })
        .collect()
}

/// What the fleet knows about sessions other than the attached one.
#[derive(Clone, Copy)]
pub(super) struct ForeignSessions<'a> {
    /// Each peer session's cached arrangement.
    pub layouts: &'a HashMap<SessionId, ArrangementDoc>,
    /// Each peer pane's `phux.agent/v1` record.
    pub agents: &'a HashMap<ResourceId, AgentRecord>,
    /// How many terminals each session holds in the cached graph.
    pub terminals: &'a HashMap<SessionId, usize>,
}

/// Build the dashboard rows. Sessions are headers, current first then by
/// name. Current-session panes (windows in order, DFS leaves) commit
/// `focus-pane { window, pane }`; a foreign session with a cached layout
/// lists its panes committing `switch-session { name, window, pane }`, else
/// one `switch-session { name }` row. Before any session graph, local panes
/// list flat.
pub(super) fn fleet_items(
    workspace: &ArrangementDoc,
    sessions: &[SessionInfo],
    focused_session: Option<SessionId>,
    agent_meta: &HashMap<ResourceId, AgentRecord>,
    pane_meta: &HashMap<ResourceId, FleetPaneMeta>,
    foreign: ForeignSessions<'_>,
) -> Vec<SelectItem> {
    let ForeignSessions {
        layouts: foreign_layouts,
        agents: foreign_agents,
        terminals,
    } = foreign;
    let mut ordered: Vec<&SessionInfo> = sessions.iter().collect();
    ordered.sort_by(|a, b| {
        let a_cur = Some(a.id) == focused_session;
        let b_cur = Some(b.id) == focused_session;
        b_cur.cmp(&a_cur).then_with(|| a.name.cmp(&b.name))
    });

    let mut items = Vec::new();
    let mut pushed_current = false;
    for session in ordered {
        let is_current = Some(session.id) == focused_session;
        if is_current {
            items.push(SelectItem::header(format!("{} (current)", session.name)));
            items.extend(current_session_pane_rows(workspace, agent_meta, pane_meta));
            pushed_current = true;
        } else {
            items.push(SelectItem::header(session.name.clone()));
            match foreign_layouts
                .get(&session.id)
                .filter(|ws| !ws.groups.is_empty())
            {
                Some(foreign) => {
                    items.extend(foreign_session_pane_rows(session, foreign, foreign_agents));
                }
                None => items.push(foreign_session_row(
                    session,
                    terminals.get(&session.id).copied(),
                )),
            }
        }
    }
    // Pre-snapshot fallback: no session graph cached yet — list the local
    // workspace flat (mirrors the window picker's fallback).
    if !pushed_current {
        items.extend(current_session_pane_rows(workspace, agent_meta, pane_meta));
    }
    items
}

/// The live fleet dashboard's rows: every session's panes from
/// [`fleet_items`], then the satellite agents not already open here.
pub(super) fn live_fleet_items(
    workspace: &ArrangementDoc,
    panes: &HashMap<ResourceId, PaneSlot>,
    agent_sessions: &AgentSessionRows,
    agent_meta: &HashMap<ResourceId, AgentRecord>,
    vcs: &mut VcsIndex,
    peers: &super::sidebar_zones::PeerInputs<'_>,
) -> Vec<SelectItem> {
    let meta = collect_pane_meta(panes, vcs, agent_sessions);
    let mut items = fleet_items(
        workspace,
        peers.sessions,
        peers.focused_session,
        agent_meta,
        &meta,
        ForeignSessions {
            layouts: peers.foreign_layouts,
            agents: peers.foreign_agents,
            terminals: &peers.terminal_counts(),
        },
    );
    items.extend(satellite_agent_items(
        peers.foreign_agents,
        peers.foreign_attention,
        workspace,
    ));
    items
}

/// Satellite terminals grouped by agent name (ADR-0136): a header per agent,
/// a row per host, committing `split-pane { resource }` (which focuses the
/// pane when already open). Panes already in `workspace` are skipped.
pub(super) fn satellite_agent_items(
    agents: &HashMap<ResourceId, AgentRecord>,
    attention: &HashSet<ResourceId>,
    workspace: &ArrangementDoc,
) -> Vec<SelectItem> {
    let open = open_leaves(workspace);
    let mut rows: Vec<(&ResourceId, &AgentRecord)> = agents
        .iter()
        .filter(|(id, record)| {
            id.host().is_some() && !open.contains(*id) && !record.name.is_empty()
        })
        .collect();
    rows.sort_by(|a, b| {
        a.1.name
            .cmp(&b.1.name)
            .then_with(|| satellite_sort_key(a.0).cmp(&satellite_sort_key(b.0)))
    });
    let mut items = Vec::new();
    let mut header = String::new();
    for (id, record) in rows {
        if header != record.name {
            items.push(SelectItem::header(record.name.clone()));
            header.clone_from(&record.name);
        }
        items.push(satellite_agent_row(id, record, attention.contains(id)));
    }
    items
}

fn open_leaves(workspace: &ArrangementDoc) -> HashSet<ResourceId> {
    let mut open = HashSet::new();
    for window in &workspace.groups {
        if let Some(tree) = window.state.tree.as_ref() {
            open.extend(crate::layout::leaves(tree));
        }
    }
    open
}

fn satellite_sort_key(id: &ResourceId) -> (String, u32) {
    match id {
        ResourceId::Satellite { host, id } => (host.as_str().to_owned(), *id),
        ResourceId::Local { id } => (String::new(), *id),
    }
}

fn satellite_agent_row(id: &ResourceId, record: &AgentRecord, asked: bool) -> SelectItem {
    let host = id
        .host()
        .map_or("", phux_protocol::ids::SatelliteHost::as_str);
    let attention = asked || record.effective_attention() == AgentAttention::High;
    let mut args = BTreeMap::new();
    args.insert(
        "direction".to_owned(),
        toml::Value::String("horizontal".to_owned()),
    );
    args.insert(
        "resource".to_owned(),
        toml::Value::String(phux_client::selector::format_terminal_id(id)),
    );
    let mut item = SelectItem::new(
        format!("{} {host}", agent_state_glyph(record.state)),
        phux_config::keybind::ResolvedAction {
            action: "split-pane".to_owned(),
            args,
        },
    )
    .indented()
    .secondary(record.state.as_str().to_owned());
    if attention {
        item = item.attention();
    }
    item
}

/// A pane's address in a row: `w:name.p` for a user-named tab, `w.p` for
/// an auto-numbered one (whose stored name would read as a second,
/// disagreeing index: tab 0 is never called "1" anywhere else).
fn pane_address(w: usize, window_name: &str, p: usize) -> String {
    if crate::attach::pane_state::auto_named(window_name) {
        format!("{w}.{p}")
    } else {
        format!("{w}:{window_name}.{p}")
    }
}

/// The attached session's pane rows, committing `focus-pane`.
fn current_session_pane_rows(
    workspace: &ArrangementDoc,
    agent_meta: &HashMap<ResourceId, AgentRecord>,
    pane_meta: &HashMap<ResourceId, FleetPaneMeta>,
) -> Vec<SelectItem> {
    let mut rows = Vec::new();
    for (w, window) in workspace.groups.iter().enumerate() {
        let leaves = window
            .state
            .tree
            .as_ref()
            .map(crate::layout::leaves)
            .unwrap_or_default();
        for (p, id) in leaves.iter().enumerate() {
            let meta = pane_meta.get(id).cloned().unwrap_or_default();
            if meta.sessions.is_empty() {
                rows.push(pane_row(
                    w,
                    &window.name,
                    p,
                    agent_meta.get(id),
                    &meta,
                    None,
                    id.host().map(phux_protocol::ids::SatelliteHost::as_str),
                ));
                continue;
            }
            for session in &meta.sessions {
                rows.push(pane_row(
                    w,
                    &window.name,
                    p,
                    agent_meta.get(id),
                    &meta,
                    Some(session),
                    id.host().map(phux_protocol::ids::SatelliteHost::as_str),
                ));
            }
        }
    }
    rows
}

/// One pane's row, or one of its `AgentSession` rows. The stream (or else
/// the `phux.agent/v1` record) supplies state and name; with neither, the
/// OSC title and `○`. The secondary is `state · place` (branch, else cwd
/// leaf). Asked, blocked, or high declared attention highlight the row.
fn pane_row(
    w: usize,
    window_name: &str,
    p: usize,
    record: Option<&AgentRecord>,
    meta: &FleetPaneMeta,
    session: Option<&AgentSessionRow>,
    host: Option<&str>,
) -> SelectItem {
    let (glyph, who, state_word) = match (session, record) {
        (Some(session), record) => {
            let who = record.map_or_else(
                || session.name().to_owned(),
                |r| who_label(&r.name, session.provider.as_deref()),
            );
            (
                agent_state_glyph(session.state),
                who,
                Some(session.state.as_str()),
            )
        }
        (None, Some(r)) => {
            let who = who_label(&r.name, r.kind.as_deref());
            (agent_state_glyph(r.state), who, Some(r.state.as_str()))
        }
        // A plain pane that asked still needs you: it wears the blocked
        // ring its divider label does, not the idle one.
        (None, None) => (
            if meta.attention {
                AGENT_BLOCKED_GLYPH
            } else {
                AGENT_IDLE_GLYPH
            },
            meta.title.clone().unwrap_or_else(|| "no agent".to_owned()),
            None,
        ),
    };
    let attention = meta.attention
        || session.is_some_and(|s| s.state == AgentMetaState::Blocked)
        || record.is_some_and(|r| r.effective_attention() == AgentAttention::High);
    let label = format!("{glyph} {} {who}", pane_address(w, window_name, p));
    let place = meta
        .branch
        .clone()
        .or_else(|| meta.cwd.as_deref().map(short_cwd));
    let secondary = match (state_word, place, host) {
        (Some(state), Some(place), Some(host)) => Some(format!("{host} {state} \u{b7} {place}")),
        (Some(state), None, Some(host)) => Some(format!("{host} {state}")),
        (None, Some(place), Some(host)) => Some(format!("{host} {place}")),
        (None, None, Some(host)) => Some(host.to_owned()),
        (Some(state), Some(place), None) => Some(format!("{state} \u{b7} {place}")),
        (Some(state), None, None) => Some(state.to_owned()),
        (None, place, None) => place,
    };

    let mut args = BTreeMap::new();
    // Window/pane ordinals never approach i64::MAX; the lossless path is
    // the only one that can fire in practice.
    args.insert(
        "window".to_owned(),
        toml::Value::Integer(i64::try_from(w).unwrap_or(i64::MAX)),
    );
    args.insert(
        "pane".to_owned(),
        toml::Value::Integer(i64::try_from(p).unwrap_or(i64::MAX)),
    );
    let mut item = SelectItem::new(
        label,
        phux_config::keybind::ResolvedAction {
            action: "focus-pane".to_owned(),
            args,
        },
    )
    .indented();
    if let Some(sec) = secondary {
        item = item.secondary(sec);
    }
    if attention {
        item = item.attention();
    }
    item
}

/// A foreign session with no cached layout: one `switch-session` row,
/// naming how many terminals the session holds when the graph lists them.
fn foreign_session_row(session: &SessionInfo, terminals: Option<usize>) -> SelectItem {
    let count = terminals.map(|n| {
        if n == 1 {
            "1 terminal".to_owned()
        } else {
            format!("{n} terminals")
        }
    });
    let mut args = BTreeMap::new();
    args.insert("name".to_owned(), toml::Value::String(session.name.clone()));
    args.insert(
        "id".to_owned(),
        toml::Value::Integer(i64::from(session.id.get())),
    );
    let row = SelectItem::new(
        "switch to this session",
        phux_config::keybind::ResolvedAction {
            action: "switch-session".to_owned(),
            args,
        },
    );
    match count {
        Some(count) => row.secondary(count),
        None => row,
    }
    .indented()
}

/// A foreign session's pane rows from its cached layout, each committing a
/// one-step `switch-session { name, window, pane }`.
fn foreign_session_pane_rows(
    session: &SessionInfo,
    workspace: &ArrangementDoc,
    foreign_agents: &HashMap<ResourceId, AgentRecord>,
) -> Vec<SelectItem> {
    let mut rows = Vec::new();
    for (w, window) in workspace.groups.iter().enumerate() {
        let leaves = window
            .state
            .tree
            .as_ref()
            .map(crate::layout::leaves)
            .unwrap_or_default();
        for (p, id) in leaves.iter().enumerate() {
            rows.push(foreign_pane_row(
                session,
                w,
                &window.name,
                p,
                foreign_agents.get(id),
            ));
        }
    }
    rows
}

/// One foreign pane's row: the record's name and state, else `○` "no agent"
/// (no local mirror, so no title fallback).
fn foreign_pane_row(
    session: &SessionInfo,
    w: usize,
    window_name: &str,
    p: usize,
    record: Option<&AgentRecord>,
) -> SelectItem {
    let (glyph, who, state_word) = record.map_or_else(
        || (AGENT_IDLE_GLYPH, "no agent".to_owned(), None),
        |r| {
            let who = who_label(&r.name, r.kind.as_deref());
            (agent_state_glyph(r.state), who, Some(r.state.as_str()))
        },
    );
    let attention = record.is_some_and(|r| r.effective_attention() == AgentAttention::High);
    let label = format!("{glyph} {} {who}", pane_address(w, window_name, p));
    let mut args = BTreeMap::new();
    args.insert("name".to_owned(), toml::Value::String(session.name.clone()));
    args.insert(
        "id".to_owned(),
        toml::Value::Integer(i64::from(session.id.get())),
    );
    // Window/pane ordinals never approach i64::MAX; the lossless path is
    // the only one that can fire in practice.
    args.insert(
        "window".to_owned(),
        toml::Value::Integer(i64::try_from(w).unwrap_or(i64::MAX)),
    );
    args.insert(
        "pane".to_owned(),
        toml::Value::Integer(i64::try_from(p).unwrap_or(i64::MAX)),
    );
    let mut item = SelectItem::new(
        label,
        phux_config::keybind::ResolvedAction {
            action: "switch-session".to_owned(),
            args,
        },
    )
    .indented();
    if let Some(state) = state_word {
        item = item.secondary(state.to_owned());
    }
    if attention {
        item = item.attention();
    }
    item
}

/// The agent as the row names it: `name [kind]`, or the name alone when
/// the kind would only repeat it (`claude [claude]` says nothing twice).
fn who_label(name: &str, kind: Option<&str>) -> String {
    match kind {
        Some(kind) if !kind.eq_ignore_ascii_case(name) => format!("{name} [{kind}]"),
        _ => name.to_owned(),
    }
}

/// Shorten a cwd to its last path component for the secondary column
/// (a full path would push the state word off a narrow modal).
fn short_cwd(cwd: &str) -> String {
    cwd.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(cwd)
        .to_owned()
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests;
