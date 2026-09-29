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
//!   ● 0:main.0 reviewer [claude]  blocked - main   <- pane row
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
use crate::layout::Workspace;
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

/// Build the dashboard rows. Sessions are headers, current first then by
/// name. Current-session panes (windows in order, DFS leaves) commit
/// `focus-pane { window, pane }`; a foreign session with a cached layout
/// lists its panes committing `switch-session { name, window, pane }`, else
/// one `switch-session { name }` row. Before any session graph, local panes
/// list flat.
pub(super) fn fleet_items(
    workspace: &Workspace,
    sessions: &[SessionInfo],
    focused_session: Option<SessionId>,
    agent_meta: &HashMap<ResourceId, AgentRecord>,
    pane_meta: &HashMap<ResourceId, FleetPaneMeta>,
    foreign_layouts: &HashMap<SessionId, Workspace>,
    foreign_agents: &HashMap<ResourceId, AgentRecord>,
) -> Vec<SelectItem> {
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
                .filter(|ws| !ws.windows.is_empty())
            {
                Some(foreign) => {
                    items.extend(foreign_session_pane_rows(session, foreign, foreign_agents));
                }
                None => items.push(foreign_session_row(session)),
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
    workspace: &Workspace,
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
        peers.foreign_layouts,
        peers.foreign_agents,
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
    workspace: &Workspace,
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

fn open_leaves(workspace: &Workspace) -> HashSet<ResourceId> {
    let mut open = HashSet::new();
    for window in &workspace.windows {
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
        format!("{} {host}", state_glyph(record.state)),
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

/// The attached session's pane rows, committing `focus-pane`.
fn current_session_pane_rows(
    workspace: &Workspace,
    agent_meta: &HashMap<ResourceId, AgentRecord>,
    pane_meta: &HashMap<ResourceId, FleetPaneMeta>,
) -> Vec<SelectItem> {
    let mut rows = Vec::new();
    for (w, window) in workspace.windows.iter().enumerate() {
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
/// OSC title and `○`. The secondary is `state - place` (branch, else cwd
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
            let who = match (record, session.provider.as_deref()) {
                (Some(r), Some(provider)) => format!("{} [{provider}]", r.name),
                (Some(r), None) => r.name.clone(),
                (None, _) => session.name().to_owned(),
            };
            (
                state_glyph(session.state),
                who,
                Some(session.state.as_str()),
            )
        }
        (None, Some(r)) => {
            let who = r
                .kind
                .as_ref()
                .map_or_else(|| r.name.clone(), |kind| format!("{} [{kind}]", r.name));
            (state_glyph(r.state), who, Some(r.state.as_str()))
        }
        (None, None) => (
            AGENT_IDLE_GLYPH,
            meta.title.clone().unwrap_or_else(|| "no agent".to_owned()),
            None,
        ),
    };
    let attention = meta.attention
        || session.is_some_and(|s| s.state == AgentMetaState::Blocked)
        || record.is_some_and(|r| r.effective_attention() == AgentAttention::High);
    let label = format!("{glyph} {w}:{window_name}.{p} {who}");
    let place = meta
        .branch
        .clone()
        .or_else(|| meta.cwd.as_deref().map(short_cwd));
    let secondary = match (state_word, place, host) {
        (Some(state), Some(place), Some(host)) => Some(format!("{host} {state} - {place}")),
        (Some(state), None, Some(host)) => Some(format!("{host} {state}")),
        (None, Some(place), Some(host)) => Some(format!("{host} {place}")),
        (None, None, Some(host)) => Some(host.to_owned()),
        (Some(state), Some(place), None) => Some(format!("{state} - {place}")),
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

/// A foreign session with no cached layout: one `switch-session` row.
fn foreign_session_row(session: &SessionInfo) -> SelectItem {
    let windows = if session.window_count == 1 {
        "1 window".to_owned()
    } else {
        format!("{} windows", session.window_count)
    };
    let mut args = BTreeMap::new();
    args.insert("name".to_owned(), toml::Value::String(session.name.clone()));
    args.insert(
        "id".to_owned(),
        toml::Value::Integer(i64::from(session.id.get())),
    );
    SelectItem::new(
        "switch to this session",
        phux_config::keybind::ResolvedAction {
            action: "switch-session".to_owned(),
            args,
        },
    )
    .secondary(windows)
    .indented()
}

/// A foreign session's pane rows from its cached layout, each committing a
/// one-step `switch-session { name, window, pane }`.
fn foreign_session_pane_rows(
    session: &SessionInfo,
    workspace: &Workspace,
    foreign_agents: &HashMap<ResourceId, AgentRecord>,
) -> Vec<SelectItem> {
    let mut rows = Vec::new();
    for (w, window) in workspace.windows.iter().enumerate() {
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
            let who = r
                .kind
                .as_ref()
                .map_or_else(|| r.name.clone(), |kind| format!("{} [{kind}]", r.name));
            (state_glyph(r.state), who, Some(r.state.as_str()))
        },
    );
    let attention = record.is_some_and(|r| r.effective_attention() == AgentAttention::High);
    let label = format!("{glyph} {w}:{window_name}.{p} {who}");
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

/// The badge glyph for a declared agent state (shared chrome vocabulary).
const fn state_glyph(state: AgentMetaState) -> &'static str {
    use crate::render::chrome::{AGENT_BLOCKED_GLYPH, AGENT_DONE_GLYPH, AGENT_WORKING_GLYPH};
    match state {
        AgentMetaState::Blocked => AGENT_BLOCKED_GLYPH,
        AgentMetaState::Working => AGENT_WORKING_GLYPH,
        AgentMetaState::Done => AGENT_DONE_GLYPH,
        AgentMetaState::Idle | AgentMetaState::Unknown => AGENT_IDLE_GLYPH,
    }
}

/// The hollow ring: idle, reviewed, or unknown.
const AGENT_IDLE_GLYPH: &str = "\u{25cb}";

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
mod tests {
    use super::*;
    use crate::layout::{LayoutNode, LayoutState, SplitDir, WindowState, split_at};

    fn tid(n: u32) -> ResourceId {
        ResourceId::local(n)
    }

    fn sinfo(id: u32, name: &str, windows: u16) -> SessionInfo {
        SessionInfo::new(SessionId::new(id), name).with_window_count(windows)
    }

    fn record(name: &str, kind: Option<&str>, state: AgentMetaState) -> AgentRecord {
        AgentRecord {
            name: name.to_owned(),
            kind: kind.map(ToOwned::to_owned),
            state,
            ..AgentRecord::default()
        }
    }

    /// Two windows: window 0 (`main`) split into panes `a`|`b`, window 1
    /// (`logs`) a single pane `c`.
    fn two_window_workspace_ids(a: u32, b: u32, c: u32) -> Workspace {
        let tree = split_at(
            &LayoutNode::Leaf(tid(a)),
            &tid(a),
            &tid(b),
            SplitDir::Horizontal,
            0.5,
        )
        .expect("split");
        Workspace {
            windows: vec![
                WindowState::new(
                    "main".to_owned(),
                    LayoutState {
                        tree: Some(tree),
                        focus: Some(tid(a)),
                    },
                ),
                WindowState::new("logs".to_owned(), LayoutState::single(tid(c))),
            ],
            active: 0,
        }
    }

    /// Two windows: window 0 split into panes 1|2, window 1 a single pane 3.
    /// Rows for `workspace` alone: no session graph, no foreign state.
    fn local_items(
        workspace: &Workspace,
        agents: &HashMap<ResourceId, AgentRecord>,
        meta: &HashMap<ResourceId, FleetPaneMeta>,
    ) -> Vec<SelectItem> {
        fleet_items(
            workspace,
            &[],
            None,
            agents,
            meta,
            &HashMap::new(),
            &HashMap::new(),
        )
    }

    fn two_window_workspace() -> Workspace {
        two_window_workspace_ids(1, 2, 3)
    }

    #[test]
    fn groups_current_session_panes_under_header_and_foreign_as_switch_rows() {
        let workspace = two_window_workspace();
        let sessions = [sinfo(1, "work", 2), sinfo(2, "scratch", 3)];
        // No cached foreign layout: the single switch row.
        let items = fleet_items(
            &workspace,
            &sessions,
            Some(SessionId::new(1)),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        );
        // Current session leads as a header, then one row per pane (3),
        // then the foreign session header + its switch row.
        assert!(items[0].is_header());
        assert_eq!(items[0].label, "work (current)");
        let pane_rows: Vec<&SelectItem> = items[1..4].iter().collect();
        assert!(pane_rows.iter().all(|i| !i.is_header() && i.indented));
        assert!(
            pane_rows.iter().all(|i| i.action.action == "focus-pane"),
            "current-session rows commit focus-pane"
        );
        assert!(items[4].is_header());
        assert_eq!(items[4].label, "scratch");
        assert_eq!(items[5].action.action, "switch-session");
        assert_eq!(
            items[5].action.args.get("name"),
            Some(&toml::Value::String("scratch".to_owned()))
        );
        assert_eq!(
            items[5].action.args.get("id"),
            Some(&toml::Value::Integer(2))
        );
        assert_eq!(items[5].secondary.as_deref(), Some("3 windows"));
    }

    #[test]
    fn pane_rows_carry_window_and_leaf_ordinals() {
        let workspace = two_window_workspace();
        let items = local_items(&workspace, &HashMap::new(), &HashMap::new());
        // Pre-snapshot fallback: flat pane rows, no headers.
        assert_eq!(items.len(), 3);
        assert_eq!(
            items[0].action.args.get("window"),
            Some(&toml::Value::Integer(0))
        );
        assert_eq!(
            items[0].action.args.get("pane"),
            Some(&toml::Value::Integer(0))
        );
        assert_eq!(
            items[1].action.args.get("pane"),
            Some(&toml::Value::Integer(1))
        );
        assert_eq!(
            items[2].action.args.get("window"),
            Some(&toml::Value::Integer(1))
        );
        assert_eq!(
            items[2].action.args.get("pane"),
            Some(&toml::Value::Integer(0))
        );
        // Labels carry the window:name.pane coordinates.
        assert!(items[0].label.contains("0:main.0"), "{}", items[0].label);
        assert!(items[2].label.contains("1:logs.0"), "{}", items[2].label);
    }

    #[test]
    fn agent_record_supplies_name_kind_glyph_and_state_word() {
        let workspace = two_window_workspace();
        let mut agents = HashMap::new();
        agents.insert(
            tid(1),
            record("reviewer", Some("claude"), AgentMetaState::Working),
        );
        agents.insert(tid(2), record("builder", None, AgentMetaState::Blocked));
        let items = local_items(&workspace, &agents, &HashMap::new());
        assert_eq!(items[0].label, "◐ 0:main.0 reviewer [claude]");
        assert_eq!(items[0].secondary.as_deref(), Some("working"));
        assert!(!items[0].attention, "working is not high attention");
        assert_eq!(items[1].label, "● 0:main.1 builder");
        assert_eq!(items[1].secondary.as_deref(), Some("blocked"));
        assert!(
            items[1].attention,
            "blocked derives high attention (ADR-0040) and must highlight"
        );
    }

    /// An `AgentSession` stream bound to a pane supplies the row's state; the
    /// `phux.agent/v1` record still names it. Two sessions under one pane are
    /// two rows that both commit the same `focus-pane`.
    #[test]
    fn stream_sessions_outrank_the_record_and_list_one_row_each() {
        let workspace = Workspace::single(tid(1));
        let mut agents = HashMap::new();
        agents.insert(tid(1), record("reviewer", None, AgentMetaState::Idle));
        let mut meta = HashMap::new();
        meta.insert(
            tid(1),
            FleetPaneMeta {
                sessions: vec![
                    AgentSessionRow {
                        id: tid(8),
                        provider: Some("claude".to_owned()),
                        native_id: None,
                        state: AgentMetaState::Blocked,
                    },
                    AgentSessionRow {
                        id: tid(9),
                        provider: None,
                        native_id: None,
                        state: AgentMetaState::Working,
                    },
                ],
                ..FleetPaneMeta::default()
            },
        );
        let items = local_items(&workspace, &agents, &meta);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].label, "● 0:1.0 reviewer [claude]");
        assert_eq!(items[0].secondary.as_deref(), Some("blocked"));
        assert!(items[0].attention, "a blocked stream highlights");
        assert_eq!(items[1].label, "◐ 0:1.0 reviewer");
        assert_eq!(items[1].secondary.as_deref(), Some("working"));
        assert!(!items[1].attention);
        assert!(
            items.iter().all(|i| i.action.action == "focus-pane"),
            "the parent pane is every session row's target"
        );
    }

    /// With no record the provider names the stream row.
    #[test]
    fn a_stream_without_a_record_is_named_by_its_provider() {
        let workspace = Workspace::single(tid(1));
        let mut meta = HashMap::new();
        meta.insert(
            tid(1),
            FleetPaneMeta {
                title: Some("some title".to_owned()),
                sessions: vec![AgentSessionRow {
                    id: tid(8),
                    provider: Some("codex".to_owned()),
                    native_id: None,
                    state: AgentMetaState::Done,
                }],
                ..FleetPaneMeta::default()
            },
        );
        let items = local_items(&workspace, &HashMap::new(), &meta);
        assert_eq!(items[0].label, "◆ 0:1.0 codex");
        assert_eq!(items[0].secondary.as_deref(), Some("done"));
    }

    #[test]
    fn state_glyphs_cover_the_v1_vocabulary() {
        assert_eq!(state_glyph(AgentMetaState::Blocked), "●");
        assert_eq!(state_glyph(AgentMetaState::Working), "◐");
        assert_eq!(state_glyph(AgentMetaState::Idle), "○");
        assert_eq!(state_glyph(AgentMetaState::Done), "◆");
        assert_eq!(state_glyph(AgentMetaState::Unknown), "○");
    }

    #[test]
    fn no_record_falls_back_to_title_then_placeholder() {
        let workspace = Workspace::single(tid(1));
        // With a title: the OSC fallback (record outranks it when present).
        let mut meta = HashMap::new();
        meta.insert(
            tid(1),
            FleetPaneMeta {
                title: Some("vim src/main.rs".to_owned()),
                ..FleetPaneMeta::default()
            },
        );
        let items = local_items(&workspace, &HashMap::new(), &meta);
        assert_eq!(items[0].label, "○ 0:1.0 vim src/main.rs");
        assert_eq!(items[0].secondary, None, "no record => no state word");
        // Without a title: the placeholder.
        let items = local_items(&workspace, &HashMap::new(), &HashMap::new());
        assert_eq!(items[0].label, "○ 0:1.0 no agent");
    }

    #[test]
    fn asked_flag_highlights_even_without_a_record() {
        let workspace = Workspace::single(tid(1));
        let mut meta = HashMap::new();
        meta.insert(
            tid(1),
            FleetPaneMeta {
                attention: true,
                ..FleetPaneMeta::default()
            },
        );
        let items = local_items(&workspace, &HashMap::new(), &meta);
        assert!(
            items[0].attention,
            "the ADR-0035 asked flag must highlight the row"
        );
    }

    #[test]
    fn record_outranks_title_when_both_present() {
        let workspace = Workspace::single(tid(1));
        let mut agents = HashMap::new();
        agents.insert(tid(1), record("reviewer", None, AgentMetaState::Idle));
        let mut meta = HashMap::new();
        meta.insert(
            tid(1),
            FleetPaneMeta {
                title: Some("phux-ask: something".to_owned()),
                ..FleetPaneMeta::default()
            },
        );
        let items = local_items(&workspace, &agents, &meta);
        assert_eq!(
            items[0].label, "○ 0:1.0 reviewer",
            "ADR-0040 decision 3: the record must outrank the OSC title"
        );
    }

    #[test]
    fn secondary_prefers_branch_over_cwd_and_shortens_cwd() {
        let workspace = two_window_workspace();
        let mut agents = HashMap::new();
        agents.insert(tid(1), record("a", None, AgentMetaState::Working));
        agents.insert(tid(2), record("b", None, AgentMetaState::Idle));
        let mut meta = HashMap::new();
        meta.insert(
            tid(1),
            FleetPaneMeta {
                branch: Some("main".to_owned()),
                cwd: Some("/home/u/repo".to_owned()),
                ..FleetPaneMeta::default()
            },
        );
        meta.insert(
            tid(2),
            FleetPaneMeta {
                cwd: Some("/home/u/deep/dir/".to_owned()),
                ..FleetPaneMeta::default()
            },
        );
        let items = local_items(&workspace, &agents, &meta);
        assert_eq!(items[0].secondary.as_deref(), Some("working - main"));
        assert_eq!(items[1].secondary.as_deref(), Some("idle - dir"));
    }

    #[test]
    fn sessions_order_current_first_then_by_name() {
        let workspace = Workspace::single(tid(1));
        let sessions = [
            sinfo(3, "zeta", 1),
            sinfo(1, "alpha", 1),
            sinfo(2, "work", 1),
        ];
        let items = fleet_items(
            &workspace,
            &sessions,
            Some(SessionId::new(2)),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        );
        let headers: Vec<&str> = items
            .iter()
            .filter(|i| i.is_header())
            .map(|i| i.label.as_str())
            .collect();
        assert_eq!(headers, vec!["work (current)", "alpha", "zeta"]);
    }

    /// A foreign session with a cached layout lists one-step pane rows.
    #[test]
    fn foreign_session_with_cached_layout_lists_one_step_pane_rows() {
        let workspace = Workspace::single(tid(10));
        let sessions = [sinfo(1, "work", 1), sinfo(2, "scratch", 2)];
        // scratch's persisted layout: window 0 splits panes 20|21, window 1
        // is a single pane 22.
        let scratch = two_window_workspace_ids(20, 21, 22);
        let mut foreign_layouts = HashMap::new();
        foreign_layouts.insert(SessionId::new(2), scratch);
        // Agent records for two of scratch's panes.
        let mut foreign_agents = HashMap::new();
        foreign_agents.insert(
            tid(20),
            record("packer", Some("codex"), AgentMetaState::Working),
        );
        foreign_agents.insert(tid(21), record("linter", None, AgentMetaState::Blocked));

        let items = fleet_items(
            &workspace,
            &sessions,
            Some(SessionId::new(1)),
            &HashMap::new(),
            &HashMap::new(),
            &foreign_layouts,
            &foreign_agents,
        );
        // work (current) header + its 1 pane, then scratch header + 3 pane
        // rows (no switch-session-only fallback).
        let scratch_hdr = items
            .iter()
            .position(|i| i.is_header() && i.label == "scratch")
            .expect("scratch header present");
        let rows = &items[scratch_hdr + 1..scratch_hdr + 4];
        assert!(
            rows.iter().all(|i| !i.is_header() && i.indented),
            "foreign session lists indented pane rows"
        );
        // Every foreign row commits a one-step switch-session carrying the
        // target session name, window, and pane.
        for r in rows {
            assert_eq!(r.action.action, "switch-session");
            assert_eq!(
                r.action.args.get("name"),
                Some(&toml::Value::String("scratch".to_owned()))
            );
            assert!(r.action.args.contains_key("window"));
            assert!(r.action.args.contains_key("pane"));
        }
        // First row addresses window 0 pane 0 and shows the codex agent.
        assert_eq!(rows[0].label, "◐ 0:main.0 packer [codex]");
        assert_eq!(rows[0].secondary.as_deref(), Some("working"));
        assert_eq!(
            rows[0].action.args.get("window"),
            Some(&toml::Value::Integer(0))
        );
        assert_eq!(
            rows[0].action.args.get("pane"),
            Some(&toml::Value::Integer(0))
        );
        // Blocked pane highlights (effective high attention).
        assert_eq!(rows[1].label, "● 0:main.1 linter");
        assert!(rows[1].attention, "blocked foreign pane must highlight");
        // Window 1 pane 0 has no record: `○` + placeholder, no state word.
        assert_eq!(rows[2].label, "○ 1:logs.0 no agent");
        assert_eq!(rows[2].secondary, None);
        assert_eq!(
            rows[2].action.args.get("window"),
            Some(&toml::Value::Integer(1))
        );
    }

    /// A foreign session with an EMPTY cached layout still falls
    /// back to the single switch-session hop.
    #[test]
    fn foreign_session_with_empty_cached_layout_falls_back_to_switch_row() {
        let workspace = Workspace::single(tid(1));
        let sessions = [sinfo(1, "work", 1), sinfo(2, "scratch", 4)];
        let mut foreign_layouts = HashMap::new();
        foreign_layouts.insert(SessionId::new(2), Workspace::default());
        let items = fleet_items(
            &workspace,
            &sessions,
            Some(SessionId::new(1)),
            &HashMap::new(),
            &HashMap::new(),
            &foreign_layouts,
            &HashMap::new(),
        );
        let scratch_hdr = items
            .iter()
            .position(|i| i.is_header() && i.label == "scratch")
            .expect("scratch header present");
        assert_eq!(items[scratch_hdr + 1].action.action, "switch-session");
        assert!(!items[scratch_hdr + 1].action.args.contains_key("window"));
        assert_eq!(
            items[scratch_hdr + 1].secondary.as_deref(),
            Some("4 windows")
        );
    }

    #[test]
    fn satellite_agents_group_by_name_with_a_host_badge() {
        let gpu = ResourceId::satellite("gpubox", 4);
        let edge = ResourceId::satellite("edge", 9);
        let local = ResourceId::local(1);
        let mut agents = HashMap::new();
        agents.insert(
            gpu,
            AgentRecord {
                name: "reviewer".to_owned(),
                state: AgentMetaState::Working,
                ..AgentRecord::default()
            },
        );
        agents.insert(
            edge.clone(),
            AgentRecord {
                name: "reviewer".to_owned(),
                state: AgentMetaState::Blocked,
                ..AgentRecord::default()
            },
        );
        agents.insert(
            local,
            AgentRecord {
                name: "local-only".to_owned(),
                ..AgentRecord::default()
            },
        );
        let attention = HashSet::from([edge]);
        let items = satellite_agent_items(&agents, &attention, &Workspace::default());
        assert_eq!(items.len(), 3, "one header, two hosts: {items:?}");
        assert_eq!(items[0].label, "reviewer");
        assert!(items[0].is_header());
        assert_eq!(items[1].label, "● edge");
        assert!(items[1].attention, "the asked flag highlights the row");
        assert_eq!(items[1].action.action, "split-pane");
        assert_eq!(
            items[1]
                .action
                .args
                .get("resource")
                .and_then(|v| v.as_str()),
            Some("edge/@9")
        );
        assert_eq!(items[2].label, "◐ gpubox");
        assert!(!items[2].attention);
        assert_eq!(items[2].secondary.as_deref(), Some("working"));
    }
}
