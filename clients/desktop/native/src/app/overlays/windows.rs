//! Open-window overview rows and activation without opening another window.

use super::*;

/// What one open window shows, read for the window overview.
struct Shown {
    session: String,
    host: Option<String>,
    tabs: Vec<(String, String)>,
    active: Option<String>,
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
pub(in crate::app) const WINDOWS: &str = "Windows";

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
