use super::pane::PANE_MENU;
use super::*;

#[test]
fn pane_and_session_menus_use_palette_titles() {
    let titles: Vec<_> = PANE_MENU.into_iter().map(title).collect();
    assert_eq!(
        titles,
        ["Split Right", "Split Down", "Mirror Pane", "Close Pane"]
    );
    for id in [
        "select-session-work",
        "rename-session",
        "kill-session",
        "rename",
        "close-tab",
        "group-projects",
        "group-machines",
        "group-none",
        "expand-sessions",
        "collapse-sessions",
        "main-menu",
    ] {
        assert!(!title(id).is_empty());
    }
}
