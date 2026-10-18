//! The window overview, the session list, and where Move sends a selection.

use gpui::{AnyWindowHandle, App, EntityId, WeakEntity};
use phux_protocol::ids::SessionKey;

use super::{CURRENT, Row, Section, Target, current_mark, simple_row};
use crate::app::desk::{self, Desk};

/// What one open window shows, read for the window overview.
pub(super) struct Shown {
    pub(super) session: String,
    pub(super) host: Option<String>,
    pub(super) tabs: Vec<(String, String)>,
    pub(super) active: Option<String>,
}

impl Shown {
    fn of(desk: &Desk) -> Self {
        let (tabs, active) = desk.link.as_ref().map_or((Vec::new(), None), |link| {
            let tabs = link
                .workspace
                .tabs
                .iter()
                .map(|tab| (tab.id.clone(), desk::tab_title(desk, tab)))
                .collect();
            (tabs, Some(link.workspace.active_id.clone()))
        });
        Self {
            session: if desk.session.is_empty() {
                "No session".to_owned()
            } else {
                desk.session.clone()
            },
            host: desk.remote.clone(),
            tabs,
            active,
        }
    }

    fn host_label(&self) -> &str {
        self.host.as_deref().unwrap_or("Local")
    }
}

/// Section label of the window overview, whose tab rows sit under their window.
pub(super) const WINDOWS: &str = "Windows";

/// One row per open native window, each followed by its tabs. Windows are
/// numbered in the order they opened; the session and host are phux's. The
/// window this list is in is Current, and so is the tab it shows. Another
/// window's tab on screen is Shown.
pub(super) fn window_sections(desk: &Desk, current: EntityId, cx: &App) -> Vec<Section> {
    let mut rows = Vec::new();
    for (index, (handle, entity, quick)) in desk.desktop.read(cx).desk_windows().enumerate() {
        let here = entity.entity_id() == current;
        let shown = if here {
            Shown::of(desk)
        } else {
            Shown::of(entity.read(cx))
        };
        let name = if quick {
            "Quick Terminal".to_owned()
        } else {
            format!("Window {}", index + 1)
        };
        let count = match shown.tabs.len() {
            0 => "no tabs".to_owned(),
            1 => "1 tab".to_owned(),
            count => format!("{count} tabs"),
        };
        let mark = current_mark(here);
        let target = |tab: Option<String>| Target::Window {
            handle,
            desk: entity.downgrade(),
            tab,
        };
        rows.push(simple_row(
            &name,
            format!("{mark}{} · {} · {count}", shown.session, shown.host_label()),
            "Windows",
            target(None),
        ));
        let total = shown.tabs.len();
        for (position, (id, title)) in shown.tabs.iter().enumerate() {
            let mark = match (shown.active.as_deref() == Some(id.as_str()), here) {
                (true, true) => CURRENT,
                (true, false) => "Shown · ",
                (false, _) => "",
            };
            let host = shown
                .host
                .as_deref()
                .map(|host| format!(" · {host}"))
                .unwrap_or_default();
            rows.push(simple_row(
                title,
                format!(
                    "{mark}Tab {} of {total} · {name} · {}{host}",
                    position + 1,
                    shown.session
                ),
                "Tabs",
                target(Some(id.clone())),
            ));
        }
    }
    if rows.is_empty() {
        return Vec::new();
    }
    vec![Section {
        label: Some(WINDOWS),
        rows,
    }]
}

/// Bring an open window forward, out of the Dock when minimized, and show
/// `tab` there. A window closed since it was listed is left closed.
pub(super) fn raise_window(
    handle: AnyWindowHandle,
    desk: &WeakEntity<Desk>,
    tab: Option<&str>,
    cx: &mut App,
) {
    let activate = crate::diag::may_activate();
    if activate {
        cx.activate(true);
    }
    let _ = handle.update(cx, |_, window, cx| {
        if activate {
            window.activate_window();
        }
        let _ = desk.update(cx, |desk, cx| {
            if let Some(tab) = tab {
                desk.select_tab(tab);
            }
            desk.after_command(window, cx);
        });
    });
}

pub(super) fn session_sections(desk: &Desk) -> Vec<Section> {
    let Some(link) = desk.link.as_ref() else {
        return Vec::new();
    };
    let Some(topology) = link.connection.topology.as_ref() else {
        return Vec::new();
    };
    let rows = topology
        .sessions
        .iter()
        .map(|session| {
            let panes = link
                .connection
                .panes()
                .iter()
                .filter(|pane| pane.session_id == session.id)
                .count();
            let detail = format!(
                "{panes} {}",
                if panes == 1 { "terminal" } else { "terminals" }
            );
            let detail = if session.key.is_some() && session.key == topology.inbox {
                format!("Intake · unorganized work · {detail}")
            } else if session.awaiting_organization {
                format!("Imported · awaiting organization · {detail}")
            } else {
                detail
            };
            Row {
                title: session.name.clone().into(),
                keywords: detail.clone(),
                detail: Some(detail.into()),
                target: Target::Session(session.name.clone()),
                command: None,
                category: "Sessions",
                chord: None,
                agent: None,
                preview: None,
            }
        })
        .collect();
    vec![Section {
        label: Some("Sessions"),
        rows,
    }]
}

pub(super) fn move_sections(desk: &Desk) -> Vec<Section> {
    let Some(client) = desk.client() else {
        return Vec::new();
    };
    let replica = client.organization();
    let selected = &desk.organization.selected;
    let destinations: Vec<_> = replica
        .sessions()
        .iter()
        .filter(|session| {
            selected.iter().any(|key| {
                replica
                    .resource(key)
                    .is_some_and(|resource| resource.session != session.key)
            })
        })
        .collect();
    move_destination_sections(&destinations, replica.intake(), selected.len())
}

pub(super) fn move_destination_sections(
    sessions: &[&phux_protocol::wire::frame::SessionRecord],
    intake: Option<SessionKey>,
    count: usize,
) -> Vec<Section> {
    let mut unorganized = Vec::new();
    let mut named = Vec::new();
    for session in sessions {
        let designated = Some(session.key) == intake;
        let row = move_destination_row(session, designated, count);
        if designated {
            unorganized.push(row);
        } else {
            named.push(row);
        }
    }
    [("Unorganized work", unorganized), ("Named sessions", named)]
        .into_iter()
        .filter(|(_, rows)| !rows.is_empty())
        .map(|(label, rows)| Section {
            label: Some(label),
            rows,
        })
        .collect()
}

pub(super) fn move_destination_row(
    session: &phux_protocol::wire::frame::SessionRecord,
    designated: bool,
    count: usize,
) -> Row {
    let (title, name) = if designated {
        ("Leave unorganized (Intake)".to_owned(), "Intake".to_owned())
    } else {
        (session.name.clone(), session.name.clone())
    };
    let imported = if !designated && session.awaiting_organization {
        " · imported"
    } else {
        ""
    };
    Row {
        title: title.into(),
        detail: Some(format!("{count} selected{imported}").into()),
        keywords: session.name.clone(),
        target: Target::Move(session.key, name),
        command: None,
        category: "Organization",
        chord: None,
        agent: None,
        preview: None,
    }
}
