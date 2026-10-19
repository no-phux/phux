//! The View page: tab placement, sidebar grouping and the panels beside the panes.

use gpui::{Context, WeakEntity};
use gpui_kit::assets::IconName;

use crate::settings::{SidebarGrouping, TabPosition};

use super::super::desk::Desk;
use super::super::ui::menu::{MenuItem, MenuPage, MenuSpec};
use super::{command, spec};

/// Where tabs go and how the sidebar groups sessions: different views of the
/// same work, never different work.
pub(super) fn view_page(desk: &Desk, handle: &WeakEntity<Desk>) -> MenuPage {
    let checked =
        |id: &'static str, icon: IconName, on: bool| command(desk, handle, id, icon).checked(on);
    let tabs = desk.prefs.tab_position;
    let grouping = desk.prefs.sidebar_grouping;
    MenuPage::titled(
        "View",
        vec![
            MenuItem::page("Visible machines", host_filter_page(desk, handle))
                .icon(IconName::Server)
                .selector("visible-machines"),
            checked(
                "tabs-top",
                IconName::PanelsTopLeft,
                tabs == TabPosition::Top,
            ),
            checked("tabs-side", IconName::PanelLeft, tabs == TabPosition::Side),
            checked(
                "tabs-bottom",
                IconName::PanelsTopLeft,
                tabs == TabPosition::Bottom,
            ),
            checked(
                "group-projects",
                IconName::Folder,
                grouping == SidebarGrouping::Project,
            )
            .gap(),
            checked(
                "group-machines",
                IconName::Server,
                grouping == SidebarGrouping::Machine,
            ),
            checked(
                "group-none",
                IconName::ListTree,
                grouping == SidebarGrouping::None,
            ),
            command(desk, handle, "sidebar", IconName::PanelLeft)
                .checked(desk.prefs.sidebar_visible)
                .gap(),
            command(desk, handle, "changes", IconName::GitBranch).checked(desk.changes.open),
            command(desk, handle, "plain-terminal", IconName::SquareTerminal)
                .checked(desk.prefs.plain),
            command(desk, handle, "native-agents", IconName::MessagesSquare)
                .checked(desk.prefs.native_agents),
        ],
    )
}

fn host_filter_page(desk: &Desk, handle: &WeakEntity<Desk>) -> MenuPage {
    let mut hosts = vec![None];
    hosts.extend(desk.hosts.iter().map(|host| Some(host.name.clone())));
    if !hosts.contains(&desk.remote) {
        hosts.push(desk.remote.clone());
    }
    let mut items = vec![{
        let handle = handle.clone();
        MenuItem::action("Show all machines", move |_, cx| {
            let _ = handle.update(cx, |desk, cx| {
                desk.update_prefs(|prefs| prefs.sidebar_hidden_hosts.clear(), cx)
            });
        })
        .selector("show-all-machines")
    }];
    items.extend(hosts.into_iter().map(|host| {
        let shown = desk.prefs.sidebar_shows_host(host.as_deref());
        let label = host.clone().unwrap_or_else(|| "This Mac".into());
        let handle = handle.clone();
        MenuItem::action(label.clone(), move |_, cx| {
            let _ = handle.update(cx, |desk, cx| {
                desk.update_prefs(
                    |prefs| prefs.set_sidebar_host_visible(host.clone(), !shown),
                    cx,
                )
            });
        })
        .checked(shown)
        .selector(format!("show-host-{label}"))
    }));
    MenuPage::titled("Visible machines", items)
}

pub(super) fn view_menu(desk: &Desk, cx: &mut Context<Desk>) -> MenuSpec {
    let mut page = view_page(desk, &cx.weak_entity());
    page.heading = None;
    spec("Sidebar view", page, 260., desk)
}
