//! Agent-fleet dashboard model (phux-foz.7, the herdr payoff).
//!
//! One overlay over everything the attach stream already carries: every
//! window/pane of the attached session with its agent identity (the
//! ADR-0040 `phux.agent/v1` record, kept live by the driver's per-pane
//! metadata subscriptions), asked/attention state (ADR-0035), and the
//! pane's branch/cwd (phux-p4vp) — grouped under session headers, fuzzy
//! filterable, and committing pane focus through the single `run_action`
//! dispatch path. Zero new wire surface (ADR-0030): this module is a pure
//! client-side projection of state the client already receives.
//!
//! ## Foreign sessions: lazy per-pane query, no new wire (phux-jpqd)
//!
//! The `ATTACHED` snapshot's session graph
//! ([`phux_protocol::wire::info::SessionInfo`]) describes *other* sessions
//! only as name + window/client counts, and the ADR-0040 agent
//! subscriptions are per-attached-pane — so a foreign session's pane
//! topology and agent records are not in the attach stream. Rather than
//! grow the stream (ADR-0030 forbids new structured wire surface), the
//! driver reuses two **existing** L3 reads, the same lazy-query shape
//! phux-foz.8 established for the window picker (ADR-0018): the peer's
//! persisted `phux.tui.layout/v1` workspace (its pane tree) and, for each
//! `TerminalId` in that tree, a one-shot `GET_METADATA` on the pane's
//! `phux.agent/v1` record. Both land in [`fleet_items`] as
//! `foreign_layouts` + `foreign_agents`, so a peer session renders one
//! selectable row per pane committing a one-step
//! `switch-session { name, window, pane }` — the re-attach lands directly
//! on that pane with its agent glyph/state already shown. A peer with no
//! cached layout yet (nothing persisted, reply not landed, or created
//! after attach) still falls back to the single "switch to this session"
//! row. Foreign rows carry no asked flag or branch/cwd — those need a live
//! per-pane subscription, so the record's declared state is the honest
//! maximum until the client attaches there. The `phux agent list` CLI
//! remains the exhaustive projection (it queries the server per terminal).
//!
//! ## Row anatomy
//!
//! ```text
//! work (current)                       <- session header
//!   ! 0:main.0 reviewer [claude]  blocked - main   <- pane row
//!   * 0:main.1 builder            working - main
//!   ? 1:logs.0 tail -f                       logs
//! scratch                              <- foreign session header
//!   * 0:main.0 packer [codex]     working         <- foreign pane row
//!   ? 1:logs.0 no agent
//! ```
//!
//! The state glyph is `!` blocked, `*` working, `-` idle, `.` done,
//! `?` unknown. A pane with no `phux.agent/v1` record also renders `?`
//! (its state is unknown) and falls back to its OSC title for the display
//! name (the record outranks the title when both exist, ADR-0040 decision
//! 3). Attention rows (a pending ADR-0035 question, or a declared/derived
//! high attention) paint in the theme's `attention` slot.

use std::collections::{BTreeMap, HashMap};

use phux_protocol::TerminalId;
use phux_protocol::ids::SessionId;
use phux_protocol::wire::info::SessionInfo;

use super::driver::{PaneSlot, VcsIndex};
use crate::agent_meta::{AgentAttention, AgentMetaState, AgentRecord};
use crate::layout::Workspace;
use crate::render::overlay::SelectItem;

/// The live-refresh tag the fleet overlay is constructed with
/// ([`crate::render::overlay::SelectList::with_live_key`]) and the driver
/// hands to [`crate::render::overlay::OverlayState::refresh_items`] when a
/// server frame changes fleet-projected state.
pub(super) const FLEET_LIVE_KEY: &str = "agent-fleet";

/// Per-pane display metadata for one fleet row, extracted from the
/// driver's live state ([`collect_pane_meta`]) or built synthetically in
/// tests. Plain data so the row builder ([`fleet_items`]) is pure.
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
}

/// Snapshot the fleet-relevant metadata of every live pane.
///
/// Reads each pane's asked flag, cached OSC title, and cwd; the branch resolves
/// through the driver's memoized [`VcsIndex`] so repeated snapshots stay cheap.
/// dashboard opens and on each live refresh — both human-paced.
pub(super) fn collect_pane_meta(
    panes: &HashMap<TerminalId, PaneSlot>,
    vcs: &mut VcsIndex,
) -> HashMap<TerminalId, FleetPaneMeta> {
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
                },
            )
        })
        .collect()
}

/// Build the fleet dashboard's [`SelectItem`] rows.
///
/// Sessions are section headers ordered current-first then by name (the
/// window-picker convention). Under the current session, one selectable
/// row per pane in every window (windows in display order, panes in DFS
/// leaf order), committing `focus-pane { window, pane }`. A **foreign**
/// session with a cached persisted layout (`foreign_layouts`, phux-jpqd)
/// lists one row per pane committing a one-step
/// `switch-session { name, window, pane }`, its agent glyph/state drawn
/// from `foreign_agents` — the per-pane `phux.agent/v1` records the driver
/// fetched for the peer's panes when its layout landed. A foreign session
/// with no cached layout falls back to a single `switch-session { name }`
/// row (see the module docs). With no cached session graph yet
/// (pre-snapshot) the local panes list flat, so the dashboard is still
/// useful.
///
/// Pure: everything comes in as plain data, so tests drive it with fully
/// synthetic state.
pub(super) fn fleet_items(
    workspace: &Workspace,
    sessions: &[SessionInfo],
    focused_session: Option<SessionId>,
    agent_meta: &HashMap<TerminalId, AgentRecord>,
    pane_meta: &HashMap<TerminalId, FleetPaneMeta>,
    foreign_layouts: &HashMap<SessionId, Workspace>,
    foreign_agents: &HashMap<TerminalId, AgentRecord>,
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
                Some(foreign) => items.extend(foreign_session_pane_rows(
                    &session.name,
                    foreign,
                    foreign_agents,
                )),
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

/// The selectable pane rows for the attached session: every window's DFS
/// leaves, labelled `{glyph} {w}:{name}.{p} {agent-or-title}` and
/// committing `focus-pane { window, pane }`.
fn current_session_pane_rows(
    workspace: &Workspace,
    agent_meta: &HashMap<TerminalId, AgentRecord>,
    pane_meta: &HashMap<TerminalId, FleetPaneMeta>,
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
            rows.push(pane_row(w, &window.name, p, agent_meta.get(id), &meta));
        }
    }
    rows
}

/// One pane's fleet row.
///
/// The `phux.agent/v1` record, when present, supplies the display name
/// (`name [kind]`) and the state glyph/word; absent, the OSC title is the
/// compatibility fallback (ADR-0040 decision 3) with the `?` unknown
/// glyph and no state word. The secondary column is `state - place` where
/// place is the branch (preferred) or the cwd's last path component.
/// Attention = the ADR-0035 asked flag OR the record's effective high
/// attention; it drives the theme's `attention` label color.
fn pane_row(
    w: usize,
    window_name: &str,
    p: usize,
    record: Option<&AgentRecord>,
    meta: &FleetPaneMeta,
) -> SelectItem {
    let (glyph, who, state_word) = record.map_or_else(
        || {
            (
                '?',
                meta.title.clone().unwrap_or_else(|| "no agent".to_owned()),
                None,
            )
        },
        |r| {
            let who = r
                .kind
                .as_ref()
                .map_or_else(|| r.name.clone(), |kind| format!("{} [{kind}]", r.name));
            (state_glyph(r.state), who, Some(r.state.as_str()))
        },
    );
    let attention =
        meta.attention || record.is_some_and(|r| r.effective_attention() == AgentAttention::High);
    let label = format!("{glyph} {w}:{window_name}.{p} {who}");
    let place = meta
        .branch
        .clone()
        .or_else(|| meta.cwd.as_deref().map(short_cwd));
    let secondary = match (state_word, place) {
        (Some(state), Some(place)) => Some(format!("{state} - {place}")),
        (Some(state), None) => Some(state.to_owned()),
        (None, place) => place,
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

/// The single row under a foreign session's header: a `switch-session`
/// hop (the window-picker path), annotated with the session's window
/// count — the only per-session detail the attach stream carries.
fn foreign_session_row(session: &SessionInfo) -> SelectItem {
    let windows = if session.window_count == 1 {
        "1 window".to_owned()
    } else {
        format!("{} windows", session.window_count)
    };
    let mut args = BTreeMap::new();
    args.insert("name".to_owned(), toml::Value::String(session.name.clone()));
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

/// phux-jpqd: the selectable pane rows for a **foreign** session, drawn
/// from its cached persisted [`Workspace`] (`foreign_layouts`). Same DFS
/// leaf enumeration as [`current_session_pane_rows`], but each row commits
/// a one-step `switch-session { name, window, pane }` — the re-attach lands
/// directly on that pane — and its agent identity/state comes from
/// `foreign_agents`, the per-pane `phux.agent/v1` records the driver
/// fetched for the peer's leaves (no live subscription, so no asked flag or
/// branch/cwd).
fn foreign_session_pane_rows(
    session_name: &str,
    workspace: &Workspace,
    foreign_agents: &HashMap<TerminalId, AgentRecord>,
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
                session_name,
                w,
                &window.name,
                p,
                foreign_agents.get(id),
            ));
        }
    }
    rows
}

/// One foreign pane's fleet row (phux-jpqd): `{glyph} {w}:{name}.{p} {who}`
/// with the declared state word as its dimmed secondary, committing
/// `switch-session { name, window = w, pane = p }`. The `phux.agent/v1`
/// record supplies the name (`name [kind]`) and glyph/state; absent, the
/// row is `?` "no agent" (a foreign pane has no local mirror, so there is
/// no OSC-title fallback the way the attached session has). High effective
/// attention highlights the row.
fn foreign_pane_row(
    session_name: &str,
    w: usize,
    window_name: &str,
    p: usize,
    record: Option<&AgentRecord>,
) -> SelectItem {
    let (glyph, who, state_word) = record.map_or_else(
        || ('?', "no agent".to_owned(), None),
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
    args.insert(
        "name".to_owned(),
        toml::Value::String(session_name.to_owned()),
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

/// The one-character lifecycle glyph for a declared agent state:
/// `!` blocked, `*` working, `-` idle, `.` done, `?` unknown.
const fn state_glyph(state: AgentMetaState) -> char {
    match state {
        AgentMetaState::Blocked => '!',
        AgentMetaState::Working => '*',
        AgentMetaState::Idle => '-',
        AgentMetaState::Done => '.',
        AgentMetaState::Unknown => '?',
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
mod tests {
    use super::*;
    use crate::layout::{LayoutNode, LayoutState, SplitDir, WindowState, split_at};

    fn tid(n: u32) -> TerminalId {
        TerminalId::local(n)
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
                WindowState {
                    name: "main".to_owned(),
                    state: LayoutState {
                        tree: Some(tree),
                        focus: Some(tid(a)),
                    },
                },
                WindowState {
                    name: "logs".to_owned(),
                    state: LayoutState::single(tid(c)),
                },
            ],
            active: 0,
        }
    }

    /// Two windows: window 0 split into panes 1|2, window 1 a single pane 3.
    fn two_window_workspace() -> Workspace {
        two_window_workspace_ids(1, 2, 3)
    }

    #[test]
    fn groups_current_session_panes_under_header_and_foreign_as_switch_rows() {
        let workspace = two_window_workspace();
        let sessions = [sinfo(1, "work", 2), sinfo(2, "scratch", 3)];
        // No cached foreign layout for scratch, so it falls back to the
        // single switch row (the pane-row path is covered by
        // `foreign_session_with_cached_layout_lists_one_step_pane_rows`).
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
        assert_eq!(items[5].secondary.as_deref(), Some("3 windows"));
    }

    #[test]
    fn pane_rows_carry_window_and_leaf_ordinals() {
        let workspace = two_window_workspace();
        let items = fleet_items(
            &workspace,
            &[],
            None,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        );
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
        let items = fleet_items(
            &workspace,
            &[],
            None,
            &agents,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        );
        assert_eq!(items[0].label, "* 0:main.0 reviewer [claude]");
        assert_eq!(items[0].secondary.as_deref(), Some("working"));
        assert!(!items[0].attention, "working is not high attention");
        assert_eq!(items[1].label, "! 0:main.1 builder");
        assert_eq!(items[1].secondary.as_deref(), Some("blocked"));
        assert!(
            items[1].attention,
            "blocked derives high attention (ADR-0040) and must highlight"
        );
    }

    #[test]
    fn state_glyphs_cover_the_v1_vocabulary() {
        assert_eq!(state_glyph(AgentMetaState::Blocked), '!');
        assert_eq!(state_glyph(AgentMetaState::Working), '*');
        assert_eq!(state_glyph(AgentMetaState::Idle), '-');
        assert_eq!(state_glyph(AgentMetaState::Done), '.');
        assert_eq!(state_glyph(AgentMetaState::Unknown), '?');
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
        let items = fleet_items(
            &workspace,
            &[],
            None,
            &HashMap::new(),
            &meta,
            &HashMap::new(),
            &HashMap::new(),
        );
        assert_eq!(items[0].label, "? 0:1.0 vim src/main.rs");
        assert_eq!(items[0].secondary, None, "no record => no state word");
        // Without a title: the placeholder.
        let items = fleet_items(
            &workspace,
            &[],
            None,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        );
        assert_eq!(items[0].label, "? 0:1.0 no agent");
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
        let items = fleet_items(
            &workspace,
            &[],
            None,
            &HashMap::new(),
            &meta,
            &HashMap::new(),
            &HashMap::new(),
        );
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
        let items = fleet_items(
            &workspace,
            &[],
            None,
            &agents,
            &meta,
            &HashMap::new(),
            &HashMap::new(),
        );
        assert_eq!(
            items[0].label, "- 0:1.0 reviewer",
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
        let items = fleet_items(
            &workspace,
            &[],
            None,
            &agents,
            &meta,
            &HashMap::new(),
            &HashMap::new(),
        );
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

    /// phux-jpqd: a foreign session WITH a cached persisted layout lists one
    /// selectable row per pane committing a one-step
    /// `switch-session { name, window, pane }`, with agent glyph/state from
    /// the fetched foreign records — not the old single switch-session hop.
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
        assert_eq!(rows[0].label, "* 0:main.0 packer [codex]");
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
        assert_eq!(rows[1].label, "! 0:main.1 linter");
        assert!(rows[1].attention, "blocked foreign pane must highlight");
        // Window 1 pane 0 has no record: `?` + placeholder, no state word.
        assert_eq!(rows[2].label, "? 1:logs.0 no agent");
        assert_eq!(rows[2].secondary, None);
        assert_eq!(
            rows[2].action.args.get("window"),
            Some(&toml::Value::Integer(1))
        );
    }

    /// phux-jpqd: a foreign session with an EMPTY cached layout still falls
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
    fn short_cwd_takes_last_component() {
        assert_eq!(short_cwd("/a/b/c"), "c");
        assert_eq!(short_cwd("/a/b/c/"), "c");
        assert_eq!(short_cwd("rel"), "rel");
        assert_eq!(short_cwd("/"), "/");
    }
}
