//! Window chrome projection: the status-bar badge/hint composers, the
//! window/agent row builders, and the single chrome-refresh chokepoint.

use std::collections::{HashMap, HashSet};

use phux_protocol::ids::{ClientId, ResourceId};
use phux_protocol::wire::frame::ResourceLifecycle;

use crate::attach::agent_rows::AgentSessionRows;
use crate::attach::chrome_ctx::{ChromeCtx, PaneScene};
use crate::attach::notify::{NotifyRow, Phase};
use crate::attach::pane_state::{
    ExitMark, PaneSlot, VcsIndex, auto_named, cwd_basename, display_window_name,
};
use crate::attach::review::ReviewIndex;
use crate::attach::server_frame::AgentMetaIndex;
use crate::layout::ArrangementDoc;
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
            "wheel:other".to_owned()
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

/// Refresh all chrome inputs from one coherent view: window tabs, supervisory
/// badges, agent rows and host-qualified session navigation. Returns whether
/// any painter input changed, so unchanged metadata bursts need no paint, and
/// the full agent list (local, peer, satellite) the Agents zone paints, built
/// whether or not the sidebar shows, for the notifier.
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
) -> (bool, Vec<AgentEntry>) {
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
    let agents = agent_queue(local.clone(), workspace, &peers);
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
        // The Agents panel lists every agent and filters on its own; a tab
        // row's badge does not take its agent off the list.
        changed |= feed_sidebar(sidebar_painter, windows, &local, agents.clone(), &peers);
    }
    (changed, agents)
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

/// Push the tab rows, the session roster, and the Agents list into the
/// sidebar strip.
fn feed_sidebar(
    sidebar_painter: &mut SidebarPainter,
    windows: Vec<phux_config::widget::WindowInfo>,
    local: &[AgentEntry],
    agents: Vec<AgentEntry>,
    peers: &crate::attach::sidebar_zones::PeerInputs<'_>,
) -> bool {
    let mut changed = sidebar_painter.set_windows(windows);
    changed |=
        sidebar_painter.set_roster(crate::attach::sidebar_zones::session_roster(peers, local));
    changed |= sidebar_painter.set_agents(agents);
    changed
}

/// The full agent list in the Agents zone's order: stable navigation order
/// for local and peer rows (lifecycle changes only restyle existing rows),
/// then satellite agents not already open, grouped by name then host.
pub(super) fn agent_queue(
    local: Vec<AgentEntry>,
    workspace: &ArrangementDoc,
    peers: &crate::attach::sidebar_zones::PeerInputs<'_>,
) -> Vec<AgentEntry> {
    let mut agents = crate::attach::sidebar_zones::needs_you_queue(local, peers);
    let mut open = HashSet::new();
    for window in &workspace.groups {
        if let Some(tree) = window.state.tree.as_ref() {
            open.extend(crate::layout::leaves(tree));
        }
    }
    agents.extend(crate::attach::sidebar_zones::satellite_agent_rows(
        peers, &open,
    ));
    agents
}

/// Give each window the badge of the agent in its focused pane, from the
/// same [`agent_entries`] rows the sidebar paints, so tab, row, and title
/// agree.
pub(super) fn badge_windows(
    windows: &mut [phux_config::widget::WindowInfo],
    workspace: &ArrangementDoc,
    agents: &[AgentEntry],
    theme: &crate::render::Theme,
) {
    for (i, (info, window)) in windows.iter_mut().zip(&workspace.groups).enumerate() {
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
    workspace: &ArrangementDoc,
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
        .groups
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let focus = w.state.focus.as_ref();
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
            let name = display_window_name(&w.name, focus, panes, agent_meta);
            phux_config::widget::WindowInfo {
                // A locked tab (`lock-window`) wears a lock.
                name: if w.locked {
                    format!("\u{1f512}{name}")
                } else {
                    name
                },
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
    workspace: &ArrangementDoc,
    panes: &HashMap<ResourceId, PaneSlot>,
    agent_meta: &AgentMetaIndex,
    agent_sessions: &AgentSessionRows,
    review: &ReviewIndex,
) -> Vec<AgentEntry> {
    let mut rows = Vec::new();
    for (i, w) in workspace.groups.iter().enumerate() {
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
            match record {
                Some(record) => rows.push(advisory_agent_entry(base, record)),
                // An ask from a pane with no agent identity (a script's
                // `phux ask`) still needs you: list it by its title, as
                // the notifier does, not under "none running".
                None if base.attention => rows.push(AgentEntry {
                    name: panes
                        .get(id)
                        .map_or("", |slot| slot.last_title.trim())
                        .to_owned(),
                    ..base
                }),
                None => {}
            }
        }
    }
    rows
}

/// The notifier's view of the agent list: one row per agent-running pane,
/// keyed by the pane's identity. Local rows resolve their leaf from the
/// attached layout; a pane with several `AgentSession`s reads as its most
/// urgent one (blocked, then working, then settled). A local pane that
/// asked (ADR-0035) without any agent identity arrives as an `agents` row
/// from [`agent_entries`].
pub(super) fn notify_rows(
    agents: &[AgentEntry],
    workspace: &ArrangementDoc,
    focused: Option<&ResourceId>,
    session_name: &str,
) -> Vec<NotifyRow> {
    let local_leaf = |window: usize, pane: Option<usize>| -> Option<ResourceId> {
        let tree = workspace.groups.get(window)?.state.tree.as_ref()?;
        crate::layout::leaves(tree).get(pane?).cloned()
    };
    let mut rows: Vec<NotifyRow> = Vec::with_capacity(agents.len());
    for entry in agents {
        let key = match (&entry.resource, &entry.session) {
            (Some(id), _) => id.clone(),
            (None, None) => match local_leaf(entry.window, entry.pane) {
                Some(id) => id,
                None => continue,
            },
            (None, Some(_)) => continue,
        };
        let row = NotifyRow {
            viewing: focused == Some(&key),
            key,
            name: if entry.name.is_empty() {
                "agent".to_owned()
            } else {
                entry.name.clone()
            },
            phase: Phase::of(entry.state, entry.attention),
            asking: entry.attention && entry.state != AgentMetaState::Blocked,
            session: entry
                .session
                .clone()
                .unwrap_or_else(|| session_name.to_owned()),
            place: entry.window_name.clone(),
            host: entry.host.clone(),
        };
        match rows.iter_mut().find(|existing| existing.key == row.key) {
            Some(existing) if urgency(row.phase) > urgency(existing.phase) => *existing = row,
            Some(_) => {}
            None => rows.push(row),
        }
    }
    rows
}

/// Which of a pane's agents speaks for it.
const fn urgency(phase: Phase) -> u8 {
    match phase {
        Phase::Blocked => 3,
        Phase::Working => 2,
        Phase::Settled => 1,
        Phase::Unknown => 0,
    }
}

/// Fill identity from advisory metadata.
fn advisory_agent_entry(base: AgentEntry, record: &AgentRecord) -> AgentEntry {
    AgentEntry {
        name: record.name.clone(),
        state: record.state,
        attention: base.attention || record.effective_attention() == AgentAttention::High,
        ..base
    }
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
mod tests;
