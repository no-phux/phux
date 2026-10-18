//! The View page: tab placement, sidebar grouping and the panels beside the panes.

use gpui::{Context, WeakEntity};
use gpui_kit::assets::IconName;

use crate::settings::{SidebarGrouping, TabPosition};

use super::super::desk::Desk;
use super::super::ui::menu::{MenuPage, MenuSpec};
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
        ],
    )
}

pub(super) fn view_menu(desk: &Desk, cx: &mut Context<Desk>) -> MenuSpec {
    let mut page = view_page(desk, &cx.weak_entity());
    page.heading = None;
    spec("Sidebar view", page, 260., desk)
}
