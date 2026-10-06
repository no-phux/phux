//! Window chrome projection: the status-bar badge/hint composers, the
//! window/agent row builders, and the single chrome-refresh chokepoint.

use std::collections::{HashMap, HashSet};

use phux_protocol::ids::{ClientId, ResourceId};
use phux_protocol::wire::frame::ResourceLifecycle;

use crate::attach::agent_rows::AgentSessionRows;
use crate::attach::chrome_ctx::{ChromeCtx, PaneScene};
use crate::attach::pane_state::{ExitMark, PaneSlot, VcsIndex};
use crate::attach::review::ReviewIndex;
use crate::attach::server_frame::AgentMetaIndex;
use crate::layout::Workspace;
use crate::render::chrome::sidebar::{AgentEntry, SidebarPainter};
use crate::render::chrome::status_bar::StatusBarPainter;
use phux_client::agent_meta::{AgentAttention, AgentMetaState, AgentRecord};

/// ADR-0033: the focused pane's supervisory badge (lifecycle + lease
/// holder, "you" for this client), or `None` when running and un-leased.
fn supervisory_badge(
    panes: &HashMap<ResourceId, PaneSlot>,
    focused_resource: Option<&ResourceId>,
    own_client_id: Option<ClientId>,
) -> Option<String> {
    let id = focused_resource?;
    let slot = panes.get(id)?;
    // ADR-0124: a retained pane's process is gone; how it ended is the whole
    // story, and no lease or brake applies to it any more.
    if let Some(mark) = slot.exited {
        return Some(format!("[ {} ]", mark.label()));
    }
    // A down satellite is the whole story until it returns.
    if slot.satellite_down {
        let name = id
            .host()
            .map_or("satellite", phux_protocol::SatelliteHost::as_str);
        return Some(format!(" {name} down "));
    }
    let frozen = matches!(slot.lifecycle, ResourceLifecycle::Frozen);
    format_supervisory_badge(
        frozen,
        slot.input_holder,
        own_client_id,
        id.host().map(phux_protocol::SatelliteHost::as_str),
        slot.history_degraded,
    )
}

/// Pure formatter behind [`supervisory_badge`]: the facts that hold, host
/// first, as space-separated tokens padded by one space each side.
fn format_supervisory_badge(
    frozen: bool,
    input_holder: Option<ClientId>,
    own_client_id: Option<ClientId>,
    host: Option<&str>,
    history_degraded: bool,
) -> Option<String> {
    let host = host.filter(|host| !host.is_empty()).map(str::to_owned);
    let frozen = frozen.then(|| "frozen".to_owned());
    let wheel = input_holder.map(|holder| {
        if Some(holder) == own_client_id {
            "wheel".to_owned()
        } else {
            format!("wheel:c{}", holder.get())
        }
    });
    let history = history_degraded.then(|| "no-scrollback".to_owned());
    let tokens: Vec<String> = [host, frozen, wheel, history]
        .into_iter()
        .flatten()
        .collect();
    (!tokens.is_empty()).then(|| format!(" {} ", tokens.join(" ")))
}

/// The attention hint, counting asking panes across ALL windows (the point is
/// a question the user cannot see), or `None` when nothing asks.
fn attention_hint(panes: &HashMap<ResourceId, PaneSlot>) -> Option<String> {
    format_attention_hint(panes.values().filter(|slot| slot.attention).count())
}

/// Pure formatter behind [`attention_hint`].
fn format_attention_hint(asking: usize) -> Option<String> {
    match asking {
        0 => None,
        1 => Some(" ask ".to_owned()),
        n => Some(format!(" ask·{n} ")),
    }
}

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
    static WINDOWS: &[phux_protocol::wire::info::WindowInfo] = &[];
    static RESOURCES: &[phux_protocol::wire::info::ResourceInfo] = &[];
    static REVIEW: LazyLock<ReviewIndex> = LazyLock::new(ReviewIndex::new);
    crate::attach::sidebar_zones::PeerInputs {
        serving_host: None,
        origin: None,
        remote_hosts: &[],
        hosts: &[],
        sessions: SESSIONS,
        focused_session: None,
        windows: WINDOWS,
        resources: RESOURCES,
        foreign_layouts: &LAYOUTS,
        foreign_agents: &AGENTS,
        foreign_attention: &ATTENTION,
        review: &REVIEW,
    }
}

/// Refresh all chrome inputs from one coherent view: window tabs, supervisory
/// badges, agent rows and host-qualified session navigation. Returns whether
/// any painter input changed, so unchanged metadata bursts need no paint.
pub(super) fn refresh_window_chrome(
    chrome: &mut ChromeCtx<'_>,
    scene: PaneScene<'_>,
    own_client_id: Option<ClientId>,
    // ADR-0040 records: a window whose focused leaf declares one is labelled
    // from it instead of the OSC title.
    agent_meta: &AgentMetaIndex,
    // Pane cwd + branch memo; each window's branch line derives
    // from its focused leaf's working directory.
    vcs: &mut VcsIndex,
    // AgentSession resources under each pane, projected from the kernel's
    // record streams. A pane with one outranks its metadata record for state.
    agent_sessions: &AgentSessionRows,
    // The peer-wide state the sidebar zones project from.
    peers: crate::attach::sidebar_zones::PeerInputs<'_>,
) -> bool {
    let PaneScene {
        workspace,
        panes,
        zoomed,
        ..
    } = scene;
    let mut windows = window_infos(workspace, panes, zoomed, &agent_meta.records, vcs);
    let local = agent_entries(workspace, panes, agent_meta, agent_sessions, peers.review);
    let badge_theme = chrome
        .sidebar_painter
        .as_deref()
        .map_or(chrome.theme, SidebarPainter::theme);
    badge_windows(&mut windows, workspace, &local, badge_theme);
    let mut changed = false;
    if let Some(sb) = chrome.status_bar.as_deref_mut() {
        changed |= feed_status_bar(sb, windows.clone(), scene, own_client_id);
    }
    if let Some(sidebar_painter) = chrome.sidebar_painter.as_deref_mut() {
        // ADR-0148: plugin sections read the same tab labels the strip shows.
        let sections = crate::attach::plugin_sidebar::project(
            sidebar_painter.plugin_specs(),
            workspace,
            &windows,
            panes,
            &agent_meta.records,
        );
        changed |= sidebar_painter.set_plugin_sections(sections);
        changed |= feed_sidebar(sidebar_painter, windows, local, workspace, &peers);
    }
    changed
}

/// Push the window tabs and the focused pane's badges into the bar.
fn feed_status_bar(
    sb: &mut StatusBarPainter,
    windows: Vec<phux_config::widget::WindowInfo>,
    scene: PaneScene<'_>,
    own_client_id: Option<ClientId>,
) -> bool {
    let panes = scene.panes;
    let mut changed = sb.set_windows(windows);
    changed |= sb.set_supervisory(supervisory_badge(panes, scene.focused, own_client_id));
    changed |= sb.set_attention(attention_hint(panes));
    // The focused pane's cwd / exit feed the bar widgets.
    let focused = scene.focused.and_then(|id| panes.get(id));
    changed |= sb.set_focused_cwd(focused.and_then(|slot| slot.cwd.clone()));
    changed |= sb.set_last_exit(focused.and_then(|slot| slot.last_exit));
    changed
}

/// Push the window rows, the session roster, and the needs-you queue into
/// the sidebar strip.
fn feed_sidebar(
    sidebar_painter: &mut SidebarPainter,
    windows: Vec<phux_config::widget::WindowInfo>,
    local: Vec<AgentEntry>,
    workspace: &Workspace,
    peers: &crate::attach::sidebar_zones::PeerInputs<'_>,
) -> bool {
    let mut changed = sidebar_painter.set_windows(windows);
    changed |=
        sidebar_painter.set_roster(crate::attach::sidebar_zones::session_roster(peers, &local));
    // Stable navigation order; lifecycle changes only restyle existing rows.
    // Satellite agents append after that order, grouped by name then host.
    let mut agents = crate::attach::sidebar_zones::needs_you_queue(local, peers);
    let mut open = HashSet::new();
    for window in &workspace.windows {
        if let Some(tree) = window.state.tree.as_ref() {
            open.extend(crate::layout::leaves(tree));
        }
    }
    agents.extend(crate::attach::sidebar_zones::satellite_agent_rows(
        peers, &open,
    ));
    changed |= sidebar_painter.set_needs_you(agents);
    changed
}

/// Give each window the badge of the agent in its focused pane, from the
/// same [`agent_entries`] rows the sidebar paints, so tab, row, and title
/// agree.
pub(super) fn badge_windows(
    windows: &mut [phux_config::widget::WindowInfo],
    workspace: &Workspace,
    agents: &[AgentEntry],
    theme: &crate::render::Theme,
) {
    for (i, (info, window)) in windows.iter_mut().zip(&workspace.windows).enumerate() {
        let focused_leaf = window.state.focus.as_ref().and_then(|focus| {
            window
                .state
                .tree
                .as_ref()
                .map(crate::layout::leaves)
                .and_then(|leaves| leaves.iter().position(|id| id == focus))
        });
        let Some(entry) = agents
            .iter()
            .find(|e| e.window == i && e.pane.is_some() && e.pane == focused_leaf)
        else {
            continue;
        };
        let badge =
            crate::render::chrome::agent_badge(theme, entry.state, entry.attention, entry.seen);
        info.badge = Some(phux_config::widget::WindowBadge {
            glyph: badge.glyph.to_owned(),
            style: phux_config::widget::CellStyle {
                fg: Some(crate::render::theme::color_to_string(badge.color)),
                bold: badge.emphatic,
                ..phux_config::widget::CellStyle::default()
            },
        });
    }
}

/// The window widget's input. Labels prefer a user-given name, then the
/// agent name, the OSC title, the cwd, and finally the auto-numbered name.
pub(super) fn window_infos(
    workspace: &Workspace,
    panes: &HashMap<ResourceId, PaneSlot>,
    // Only the active window's tab shows the zoom marker.
    zoomed: Option<&ResourceId>,
    // ADR-0040: Terminal → decoded `phux.agent/v1` record, kept live by the
    // driver's per-pane metadata subscriptions.
    agent_meta: &HashMap<ResourceId, AgentRecord>,
    // Pane-cwd index + branch memo. The window's branch line is
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
                .map(|record| record.name.clone());
            let title = focus
                .and_then(|fid| panes.get(fid))
                .map(|slot| slot.last_title.trim())
                .filter(|title| !title.is_empty())
                .map(ToOwned::to_owned);
            let active = i == workspace.active;
            let leaves = w
                .state
                .tree
                .as_ref()
                .map(crate::layout::leaves)
                .unwrap_or_default();
            // ANY asking leaf marks the tab, not just the focused one.
            let attention = leaves
                .iter()
                .any(|id| panes.get(id).is_some_and(|slot| slot.attention));
            // The branch line under the label — the focused
            // leaf's cwd resolved to its VCS branch (cached file read).
            let branch = focus.and_then(|fid| vcs.branch_for_pane(fid));
            let place = focus
                .and_then(|fid| panes.get(fid))
                .and_then(|slot| slot.cwd.as_deref())
                .and_then(cwd_basename);
            let explicit = (!auto_named(&w.name)).then(|| w.name.clone());
            phux_config::widget::WindowInfo {
                name: explicit
                    .or(agent_label)
                    .or(title)
                    .or(place)
                    .unwrap_or_else(|| w.name.clone()),
                active,
                zoomed: active && zoomed.is_some(),
                attention,
                branch,
                exited: window_exited_mark(&leaves, focus, panes),
                badge: None,
            }
        })
        .collect()
}

/// A name [`Workspace::default_window_name`] handed out (a bare integer):
/// it would read as a second, disagreeing index, so the tab prefers context.
fn auto_named(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit())
}

/// Where an agent row says the agent is: a user-given window name, else the
/// pane's cwd, else the auto-numbered name.
fn agent_locator(window_name: &str, slot: Option<&PaneSlot>) -> String {
    if !auto_named(window_name) {
        return window_name.to_owned();
    }
    slot.and_then(|slot| slot.cwd.as_deref())
        .and_then(cwd_basename)
        .unwrap_or_else(|| window_name.to_owned())
}

/// The last component of a working directory, `~` for the home directory
/// itself. `None` for the filesystem root or an empty path.
fn cwd_basename(cwd: &str) -> Option<String> {
    let trimmed = cwd.trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    if std::env::var_os("HOME").is_some_and(|home| home == trimmed) {
        return Some("~".to_owned());
    }
    trimmed
        .rsplit('/')
        .next()
        .filter(|base| !base.is_empty())
        .map(ToOwned::to_owned)
}

/// ADR-0124: compact exit mark for a window with a retained exited leaf,
/// preferring the focused pane's.
fn window_exited_mark(
    leaves: &[ResourceId],
    focus: Option<&ResourceId>,
    panes: &HashMap<ResourceId, PaneSlot>,
) -> Option<String> {
    let mark = |id: &ResourceId| panes.get(id).and_then(|slot| slot.exited);
    focus
        .and_then(mark)
        .or_else(|| leaves.iter().find_map(mark))
        .map(ExitMark::compact)
}

/// The sidebar's agent rows, one per agent-running pane in window/leaf order.
/// Identity and state come from an `AgentSession` stream bound to the pane
/// (server-enforced, so it wins on state) or the pane's ADR-0040 record;
/// a pane with neither is a shell and gets no row. Rows never reorder on
/// lifecycle changes, so navigation targets stay put.
pub(super) fn agent_entries(
    workspace: &Workspace,
    panes: &HashMap<ResourceId, PaneSlot>,
    agent_meta: &AgentMetaIndex,
    agent_sessions: &AgentSessionRows,
    review: &ReviewIndex,
) -> Vec<AgentEntry> {
    let mut rows = Vec::new();
    for (i, w) in workspace.windows.iter().enumerate() {
        let leaves = w
            .state
            .tree
            .as_ref()
            .map(crate::layout::leaves)
            .unwrap_or_default();
        for (leaf, id) in leaves.iter().enumerate() {
            let base = AgentEntry {
                session: None,
                session_id: None,
                resource: id.host().map(|_| id.clone()),
                window: i,
                window_name: agent_locator(&w.name, panes.get(id)),
                pane: Some(leaf),
                name: String::new(),
                state: AgentMetaState::Unknown,
                attention: panes.get(id).is_some_and(|slot| slot.attention),
                host: id.host().map(|host| host.as_str().to_owned()),
                seen: review.seen_or(id, panes.get(id).is_some_and(|slot| slot.seen)),
            };
            let record = agent_meta.records.get(id);
            if let Some(sessions) = agent_sessions.get(id).filter(|rows| !rows.is_empty()) {
                for session in sessions {
                    rows.push(AgentEntry {
                        name: record.map_or_else(|| session.name().to_owned(), |r| r.name.clone()),
                        state: session.state,
                        attention: base.attention
                            || session.state == AgentMetaState::Blocked
                            || record
                                .is_some_and(|r| r.effective_attention() == AgentAttention::High),
                        ..base.clone()
                    });
                }
                continue;
            }
            if let Some(entry) = advisory_agent_entry(base, record) {
                rows.push(entry);
            }
        }
    }
    rows
}

/// Fill identity from advisory metadata.
fn advisory_agent_entry(base: AgentEntry, record: Option<&AgentRecord>) -> Option<AgentEntry> {
    let record = record?;
    Some(AgentEntry {
        name: record.name.clone(),
        state: record.state,
        attention: base.attention || record.effective_attention() == AgentAttention::High,
        ..base
    })
}

/// Mark the focused pane seen. Returns `true` only on the flip, which the
/// caller must treat as a chrome repaint trigger (the chrome was computed
/// before the bit changed).
pub(super) fn mark_focused_seen(
    panes: &mut HashMap<ResourceId, PaneSlot>,
    review: &mut ReviewIndex,
    focused_resource: Option<&ResourceId>,
) -> bool {
    let Some(id) = focused_resource else {
        return false;
    };
    let flipped = review.mark_seen(id);
    if let Some(slot) = panes.get_mut(id) {
        slot.seen = true;
    }
    flipped
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use crate::attach::pane_state::{clear_attention_on_input, published_test_state};

    /// ADR-0124: a retained exited pane keeps its replica, gets an exit
    /// badge, and refuses input.
    #[test]
    fn a_retained_pane_renders_its_last_grid_with_an_exit_mark_and_refuses_input() {
        use crate::attach::pane_state::{ExitMark, pane_exited, published_terminal};
        let id = ResourceId::local(1);
        let (kernel, _effects, mut panes) =
            published_test_state(&[(&id, 20, 4, b"final screen\r\n")]);
        assert!(!pane_exited(&panes, &id), "a live pane takes input");
        panes.get_mut(&id).expect("slot").exited = Some(ExitMark {
            status: Some(3),
            signal: None,
        });
        assert!(
            published_terminal(&kernel, &id).is_some(),
            "the last grid is still published for the renderer"
        );
        assert_eq!(
            supervisory_badge(&panes, Some(&id), Some(ClientId::new(7))).as_deref(),
            Some("[ exited 3 ]")
        );
        assert!(pane_exited(&panes, &id), "input to it is refused");
        let signalled = ExitMark {
            status: None,
            signal: Some(9),
        };
        assert_eq!(signalled.label(), "exited signal 9");
        assert_eq!(signalled.compact(), "sig9");
        let unknown = ExitMark {
            status: None,
            signal: None,
        };
        assert_eq!(unknown.label(), "exited");
        assert_eq!(unknown.compact(), "");
    }

    #[test]
    fn supervisory_badge_formats_every_state() {
        let me = ClientId::new(7);
        let other = ClientId::new(9);
        assert_eq!(
            format_supervisory_badge(false, None, Some(me), None, false),
            None
        );
        assert_eq!(
            format_supervisory_badge(true, None, Some(me), None, false).as_deref(),
            Some(" frozen ")
        );
        assert_eq!(
            format_supervisory_badge(false, Some(me), Some(me), None, false).as_deref(),
            Some(" wheel ")
        );
        assert_eq!(
            format_supervisory_badge(false, Some(other), Some(me), None, false).as_deref(),
            Some(" wheel:c9 ")
        );
        assert_eq!(
            format_supervisory_badge(true, Some(other), Some(me), None, false).as_deref(),
            Some(" frozen wheel:c9 ")
        );
        // No own id yet (pre-ATTACHED): a holder still renders by id, never "you".
        assert_eq!(
            format_supervisory_badge(false, Some(me), None, None, false).as_deref(),
            Some(" wheel:c7 ")
        );
        // A satellite pane badges its host on the status bar,
        // beside any lease or brake already shown.
        assert_eq!(
            format_supervisory_badge(false, None, Some(me), Some("devbox"), false).as_deref(),
            Some(" devbox ")
        );
        assert_eq!(
            format_supervisory_badge(true, None, Some(me), Some("devbox"), false).as_deref(),
            Some(" devbox frozen ")
        );
        // Lost progressive history is a persistent token, last in the badge.
        assert_eq!(
            format_supervisory_badge(false, None, Some(me), None, true).as_deref(),
            Some(" no-scrollback ")
        );
        assert_eq!(
            format_supervisory_badge(true, Some(me), Some(me), Some("devbox"), true).as_deref(),
            Some(" devbox frozen wheel no-scrollback ")
        );
    }

    /// A pane whose progressive history is unavailable keeps a badge after
    /// the transient notice expires; an exited pane's mark still wins.
    #[test]
    fn a_history_degraded_pane_badges_no_scrollback_on_the_status_bar() {
        use crate::attach::pane_state::ExitMark;
        let id = ResourceId::local(4);
        let mut slot = crate::attach::pane_state::PaneSlot::new().expect("slot");
        slot.history_degraded = true;
        let mut panes = HashMap::from([(id.clone(), slot)]);
        assert_eq!(
            supervisory_badge(&panes, Some(&id), None).as_deref(),
            Some(" no-scrollback ")
        );
        panes.get_mut(&id).expect("slot").exited = Some(ExitMark {
            status: Some(1),
            signal: None,
        });
        assert_eq!(
            supervisory_badge(&panes, Some(&id), None).as_deref(),
            Some("[ exited 1 ]")
        );
    }

    /// The status bar names a down satellite, and the layout
    /// leaf that owns the slot is not this function's to remove.
    #[test]
    fn a_down_satellite_pane_badges_its_host_on_the_status_bar() {
        let id = ResourceId::satellite("devbox", 7);
        let mut slot = crate::attach::pane_state::PaneSlot::new().expect("slot");
        slot.satellite_down = true;
        let panes = HashMap::from([(id.clone(), slot)]);
        assert_eq!(
            supervisory_badge(&panes, Some(&id), None).as_deref(),
            Some(" devbox down ")
        );
    }

    #[test]
    fn attention_hint_formats_every_count() {
        assert_eq!(format_attention_hint(0), None);
        assert_eq!(format_attention_hint(1).as_deref(), Some(" ask "));
        assert_eq!(format_attention_hint(3).as_deref(), Some(" ask·3 "));
    }

    /// `window_infos` marks a window when ANY of its leaves has
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

    /// `window_infos` marks a window when ANY of its leaves is
    /// retained after exit — including a non-focused leaf — and only that window.
    #[test]
    fn window_infos_flags_exited_on_the_retained_window() {
        let front = ResourceId::local(1);
        let back = ResourceId::local(2);
        let mut workspace = Workspace::single(front.clone());
        workspace.add_window("2".to_owned(), back.clone());
        workspace.select(0);
        let mut panes: HashMap<ResourceId, PaneSlot> = HashMap::new();
        panes.insert(front, PaneSlot::new_with_size(80, 24).expect("slot"));
        let mut retained = PaneSlot::new_with_size(80, 24).expect("slot");
        retained.exited = Some(ExitMark {
            status: Some(3),
            signal: None,
        });
        panes.insert(back, retained);

        let infos = window_infos(
            &workspace,
            &panes,
            None,
            &HashMap::new(),
            &mut VcsIndex::default(),
        );
        assert_eq!(
            infos[0].exited.as_deref(),
            None,
            "live window stays unmarked"
        );
        assert_eq!(
            infos[1].exited.as_deref(),
            Some("3"),
            "the retained (background) window carries the compact status"
        );
    }

    /// A split whose unfocused leaf is retained still marks
    /// the window, so the tab/sidebar show it without focusing that pane.
    #[test]
    fn window_infos_marks_a_split_when_the_unfocused_leaf_exited() {
        use crate::layout::{LayoutNode, LayoutState, SplitDir};
        let live = ResourceId::local(1);
        let dead = ResourceId::local(2);
        let mut workspace = Workspace::single(live.clone());
        workspace.windows[0].state = LayoutState {
            tree: Some(LayoutNode::Split {
                dir: SplitDir::Horizontal,
                ratio: 0.5,
                left: Box::new(LayoutNode::Leaf(live.clone())),
                right: Box::new(LayoutNode::Leaf(dead.clone())),
            }),
            focus: Some(live.clone()),
        };
        let mut panes: HashMap<ResourceId, PaneSlot> = HashMap::new();
        panes.insert(live, PaneSlot::new_with_size(80, 24).expect("slot"));
        let mut retained = PaneSlot::new_with_size(80, 24).expect("slot");
        retained.exited = Some(ExitMark {
            status: None,
            signal: Some(9),
        });
        panes.insert(dead, retained);

        let infos = window_infos(
            &workspace,
            &panes,
            None,
            &HashMap::new(),
            &mut VcsIndex::default(),
        );
        assert_eq!(
            infos[0].exited.as_deref(),
            Some("sig9"),
            "unfocused retained leaf still marks the window"
        );
    }

    /// Tab labels: the focused leaf's OSC title beats the stored name, a
    /// declared agent record beats the title, and no (or a blank) title falls
    /// back to the stored name.
    #[test]
    fn window_infos_label_precedence() {
        let id = ResourceId::local(1);
        let workspace = Workspace::single(id.clone());
        let reviewer = AgentRecord {
            name: "reviewer".to_owned(),
            state: phux_client::agent_meta::AgentMetaState::Blocked,
            ..AgentRecord::default()
        };
        for (title, record, label) in [
            (&b"\x1b]2;~/src/phux\x07"[..], None, "~/src/phux"),
            (b"", None, "1"),
            (b"\x1b]2;   \x07", None, "1"),
            (b"\x1b]2;~/src/phux\x07", Some(reviewer), "reviewer"),
            (b"\x1b]2;claude task\x07", None, "claude task"),
        ] {
            let (_, _, panes) = published_test_state(&[(&id, 80, 24, title)]);
            let records: HashMap<ResourceId, AgentRecord> =
                record.into_iter().map(|r| (id.clone(), r)).collect();
            let infos = window_infos(&workspace, &panes, None, &records, &mut VcsIndex::default());
            assert_eq!(infos[0].name, label, "{title:?}");
            assert!(infos[0].active);
        }
    }

    /// An `AgentMetaIndex` holding `records` and nothing else — the shape
    /// `agent_entries` reads.
    fn meta_index(records: HashMap<ResourceId, AgentRecord>) -> AgentMetaIndex {
        AgentMetaIndex {
            records,
            ..AgentMetaIndex::default()
        }
    }

    /// Mixed lifecycle states and review changes never move navigation rows.
    #[test]
    fn agent_entries_keep_layout_order_across_review_changes() {
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

        // Status must not reorder the working, done, blocked layout.
        let entries = agent_entries(
            &workspace,
            &panes,
            &meta_index(records.clone()),
            &HashMap::new(),
            &ReviewIndex::new(),
        );
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["w", "d", "b"],
            "navigation follows layout, not urgency"
        );

        // Visiting changes the badge in place.
        panes.get_mut(&done).expect("slot").seen = true;
        let entries = agent_entries(
            &workspace,
            &panes,
            &meta_index(records),
            &HashMap::new(),
            &ReviewIndex::new(),
        );
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["w", "d", "b"], "review does not move the row");
    }

    /// Focusing a pane flips `seen` exactly once, and that flip makes
    /// `refresh_window_chrome` report a real change; re-marking reports
    /// `false`.
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

        // Completion stays in its original row while unreviewed.
        let names: Vec<String> = agent_entries(
            &workspace,
            &panes,
            &meta,
            &HashMap::new(),
            &ReviewIndex::new(),
        )
        .into_iter()
        .map(|e| e.name)
        .collect();
        assert_eq!(names, vec!["w", "d"], "completion preserves row order");

        // Prime the painters against that (stale) view — this is the paint the
        // focus action itself produced, one iteration before the flip.
        let mut sidebar_painter = SidebarPainter::new(crate::render::Theme::default());
        let mut vcs = VcsIndex::default();
        let theme = crate::render::Theme::default();
        let mut refresh = |painter: &mut SidebarPainter, panes: &HashMap<ResourceId, PaneSlot>| {
            let mut chrome = ChromeCtx {
                viewport: (80, 24),
                sidebar: None,
                status_bar: None,
                sidebar_painter: Some(painter),
                session_name: "",
                theme: &theme,
            };
            let scene = PaneScene {
                workspace: &workspace,
                panes,
                focused: Some(&done),
                zoomed: None,
            };
            refresh_window_chrome(
                &mut chrome,
                scene,
                None,
                &meta,
                &mut vcs,
                &HashMap::new(),
                no_peers(),
            )
        };
        refresh(&mut sidebar_painter, &panes);

        // The user is now looking at the finished pane.
        let mut review = ReviewIndex::new();
        assert!(
            mark_focused_seen(&mut panes, &mut review, Some(&done)),
            "the first mark after a focus change must report the flip"
        );

        assert!(
            refresh(&mut sidebar_painter, &panes),
            "the seen flip must dirty the chrome, or nothing repaints the strip"
        );
        let entries = agent_entries(&workspace, &panes, &meta, &HashMap::new(), &review);
        assert_eq!(
            entries
                .iter()
                .map(|e| (e.name.as_str(), e.seen))
                .collect::<Vec<_>>(),
            vec![("w", false), ("d", true)],
            "the focused pane's row stays put and is the only reviewed bit"
        );

        assert!(
            !mark_focused_seen(&mut panes, &mut review, Some(&done)),
            "re-marking an already-seen pane must not report a flip"
        );
        assert!(
            !refresh(&mut sidebar_painter, &panes),
            "an unchanged chrome must stay zero-cost"
        );
    }

    /// Last-change timestamps are not navigation-order inputs.
    #[test]
    fn agent_entries_ignore_change_timestamps_for_ordering() {
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

        let entries = agent_entries(
            &workspace,
            &panes,
            &index,
            &HashMap::new(),
            &ReviewIndex::new(),
        );
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["old", "fresh", "never"]);
    }

    /// A declared record names the agent row; the OSC title is not consulted.
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

        let entries = agent_entries(
            &workspace,
            &panes,
            &meta_index(records),
            &HashMap::new(),
            &ReviewIndex::new(),
        );
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

    /// Titles and attention flags alone never make a shell an agent row.
    #[test]
    fn agent_entries_require_structured_identity() {
        let claude = ResourceId::local(1);
        let shell = ResourceId::local(2);
        let mut workspace = Workspace::single(claude.clone());
        workspace.add_window("scratch".to_owned(), shell.clone());
        let (_, _, panes) = published_test_state(&[
            (&claude, 80, 24, b"\x1b]2;Claude Code - ~/src/phux\x07"),
            (&shell, 80, 24, b"\x1b]2;~/src/phux\x07"),
        ]);

        let entries = agent_entries(
            &workspace,
            &panes,
            &AgentMetaIndex::default(),
            &HashMap::new(),
            &ReviewIndex::new(),
        );
        assert!(
            entries.is_empty(),
            "title and attention state are not agent identity",
        );
    }

    /// A record declaring (or deriving) high attention marks
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

        let entries = agent_entries(
            &workspace,
            &panes,
            &meta_index(records),
            &HashMap::new(),
            &ReviewIndex::new(),
        );
        assert!(entries[0].attention);
    }

    /// A bound `AgentSession` stream's state outranks the record, which still
    /// names the row.
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

        let entries = agent_entries(
            &workspace,
            &panes,
            &meta_index(records),
            &sessions,
            &ReviewIndex::new(),
        );
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

        let entries = agent_entries(
            &workspace,
            &panes,
            &AgentMetaIndex::default(),
            &sessions,
            &ReviewIndex::new(),
        );
        let names: Vec<(&str, AgentMetaState)> =
            entries.iter().map(|e| (e.name.as_str(), e.state)).collect();
        // Stream declaration order survives differing states.
        assert_eq!(
            names,
            vec![
                ("codex", AgentMetaState::Working),
                ("agent", AgentMetaState::Done)
            ]
        );
        assert!(!entries[1].attention);
    }

    #[test]
    fn window_infos_flags_zoom_only_on_the_active_window() {
        // The active window's `zoomed` reflects the zoom state;
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

    /// An auto-numbered window has not been named, so its tab says where it
    /// is working instead of a second number beside the selector.
    #[test]
    fn an_auto_numbered_window_is_labelled_by_where_it_works() {
        let id = ResourceId::local(1);
        let workspace = Workspace::single(id.clone());
        let (_, _, mut panes) = published_test_state(&[(&id, 80, 24, b"")]);
        panes.get_mut(&id).expect("slot").cwd = Some("/src/phux/".to_owned());
        let infos = window_infos(
            &workspace,
            &panes,
            None,
            &HashMap::new(),
            &mut VcsIndex::default(),
        );
        assert_eq!(infos[0].name, "phux");
    }

    /// A name the user gave is the label they chose; it outranks the agent
    /// record and the program's title.
    #[test]
    fn a_name_the_user_gave_beats_the_agent_and_the_title() {
        let id = ResourceId::local(1);
        let mut workspace = Workspace::single(id.clone());
        workspace.windows[0].name = "deploy".to_owned();
        let (_, _, panes) = published_test_state(&[(&id, 80, 24, b"\x1b]2;~/src\x07")]);
        let records = HashMap::from([(
            id,
            AgentRecord {
                name: "claude".to_owned(),
                ..AgentRecord::default()
            },
        )]);
        let infos = window_infos(&workspace, &panes, None, &records, &mut VcsIndex::default());
        assert_eq!(infos[0].name, "deploy");
    }

    /// The tab badge is the focused pane's agent badge, from the same
    /// vocabulary the sidebar paints.
    #[test]
    fn badge_windows_marks_the_focused_agent() {
        let id = ResourceId::local(1);
        let workspace = Workspace::single(id.clone());
        let (_, _, panes) = published_test_state(&[(&id, 80, 24, b"")]);
        let mut infos = window_infos(
            &workspace,
            &panes,
            None,
            &HashMap::new(),
            &mut VcsIndex::default(),
        );
        let agents = vec![AgentEntry {
            session: None,
            session_id: None,
            resource: None,
            window: 0,
            window_name: "1".to_owned(),
            pane: Some(0),
            name: "claude".to_owned(),
            state: AgentMetaState::Working,
            attention: false,
            host: None,
            seen: true,
        }];
        let theme = crate::render::Theme::default();
        badge_windows(&mut infos, &workspace, &agents, &theme);
        let badge = infos[0].badge.as_ref().expect("badged");
        assert_eq!(badge.glyph, crate::render::chrome::AGENT_WORKING_GLYPH);
        assert_eq!(
            badge.style.fg.as_deref(),
            Some(crate::render::theme::color_to_string(theme.agent_working).as_str())
        );
    }
}
