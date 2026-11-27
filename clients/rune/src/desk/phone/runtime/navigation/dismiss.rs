//! Every page goes the way it came, through the host's own touch path
//! (`mobile::touch`), and closing a page a page opened returns to it.
use super::*;
use crate::desk::phone::dismiss::Away;
use crate::desk::phone::navigation::Held;
use rune::TouchPhase::{Cancelled, Ended, Moved, Started};

/// The host's touch: what the native bridge calls for every finger.
fn finger(rt: &mut Runtime<Desk>, phase: rune::TouchPhase, [x, y]: [f32; 2], at: f64) {
    crate::mobile::touch(rt, 7, phase, x, y, at);
}

/// A slow drag from `from` by `by`, held still before lifting so no flick
/// counts: its distance alone decides.
fn drag(rt: &mut Runtime<Desk>, from: [f32; 2], by: [f32; 2], at: f64) {
    finger(rt, Started, from, at);
    let step = |k: f32| [from[0] + by[0] * k, from[1] + by[1] * k];
    for (i, k) in [0.25, 0.5, 0.75, 1.0].into_iter().enumerate() {
        finger(rt, Moved, step(k), at + 0.05 * (i + 1) as f64);
    }
    finger(rt, Ended, step(1.0), at + 0.6);
    settle(rt);
}

/// The app's poll, which a frame runs before it draws: where a closed child
/// goes back to its page.
fn poll(rt: &mut Runtime<Desk>) {
    rt.update(|d, tx| {
        let clock = d.clock + 1.0 / 60.0;
        d.poll(clock, tx);
    });
    settle(rt);
}

fn page(rt: &mut Runtime<Desk>, page: Page) {
    rt.set_system_reduced_motion(true);
    rt.update(|d, tx| {
        match page {
            Page::Sessions => d.open_phone_sessions(),
            Page::Actions => d.open_phone_actions(),
            Page::Switcher => d.open_phone_switcher(),
        }
        tx.instant();
    });
    settle(rt);
}

fn centre(rt: &Runtime<Desk>, key: &str) -> [f32; 2] {
    let (x, y) = rt.find(key).unwrap_or_else(|| panic!("no {key}"));
    [x, y]
}

/// How far Terminal actions' list is scrolled, as it reported.
fn actions_top(rt: &Runtime<Desk>) -> f32 {
    let dismiss = &rt.state.phone.as_ref().unwrap().dismiss;
    dismiss.list_offset(Away::Sheet(Page::Actions))
}

fn shift(rt: &Runtime<Desk>) -> [f32; 2] {
    rt.state.phone.as_ref().unwrap().dismiss.shift.get()
}

#[test]
fn a_left_swipe_anywhere_on_sessions_closes_it_without_pressing_the_row_under_it() {
    let (client, mut rt) = fixture();
    detailed_inventory(&mut rt);
    page(&mut rt, Page::Sessions);
    let before = rt
        .state
        .connection
        .shown
        .as_ref()
        .map(|s| s.resource.clone());
    // From the middle of a pane row: Rune would have pressed it.
    let from = centre(&rt, "phone-session-pane-@3");
    finger(&mut rt, Started, from, 1.0);
    finger(&mut rt, Moved, [from[0] - 30.0, from[1] + 2.0], 1.05);
    finger(&mut rt, Moved, [from[0] - 90.0, from[1] + 3.0], 1.1);
    // The page is where the finger has it, one to one.
    assert_eq!(shift(&rt), [-90.0, 0.0]);
    assert_eq!(rt.state.swipe_away(), Some(Away::Sessions));
    let shown = rt.state.phone.as_ref().unwrap().dismiss.shown.get();
    assert!((shown - (1.0 - 90.0 / 393.0)).abs() < 1e-4, "{shown}");
    finger(&mut rt, Moved, [from[0] - 160.0, from[1] + 3.0], 1.2);
    finger(&mut rt, Ended, [from[0] - 160.0, from[1] + 3.0], 1.6);
    settle(&mut rt);
    assert_eq!(rt.state.phone_page(), None);
    assert_eq!(
        rt.state
            .connection
            .shown
            .as_ref()
            .map(|s| s.resource.clone()),
        before,
        "the row under the finger was not opened"
    );
    no_terminal_input(&client);
}

#[test]
fn a_short_sessions_swipe_springs_home_and_a_flick_closes_it() {
    let (_, mut rt) = fixture();
    detailed_inventory(&mut rt);
    page(&mut rt, Page::Sessions);
    drag(&mut rt, [300.0, 500.0], [-60.0, 0.0], 1.0);
    assert_eq!(rt.state.phone_page(), Some(Page::Sessions));
    assert_eq!(shift(&rt), [0.0, 0.0], "let go short, it comes home");
    // Under Reduce Motion it is home in the next frame, not on a spring.
    assert_eq!(
        crate::ui::element(&rt, "phone-navigation-layer").rect[0],
        0.0
    );
    // A quick flick of a few dozen points does what a long drag does.
    finger(&mut rt, Started, [300.0, 500.0], 3.0);
    finger(&mut rt, Moved, [280.0, 500.0], 3.016);
    finger(&mut rt, Moved, [250.0, 500.0], 3.032);
    finger(&mut rt, Ended, [250.0, 500.0], 3.04);
    settle(&mut rt);
    assert_eq!(rt.state.phone_page(), None);
}

#[test]
fn a_swipe_past_its_point_ticks_once_on_the_way_in() {
    let (_, mut rt) = fixture();
    detailed_inventory(&mut rt);
    page(&mut rt, Page::Sessions);
    finger(&mut rt, Started, [350.0, 500.0], 1.0);
    finger(&mut rt, Moved, [300.0, 500.0], 1.05);
    assert_eq!(rt.state.phone.as_mut().unwrap().feedback.take(), None);
    finger(&mut rt, Moved, [200.0, 500.0], 1.1);
    assert_eq!(
        rt.state.phone.as_mut().unwrap().feedback.take(),
        Some(crate::desk::phone::touch::Feedback::Detent)
    );
    finger(&mut rt, Moved, [180.0, 500.0], 1.15);
    assert_eq!(rt.state.phone.as_mut().unwrap().feedback.take(), None);
    finger(&mut rt, Cancelled, [180.0, 500.0], 1.2);
    settle(&mut rt);
    assert_eq!(rt.state.phone_page(), Some(Page::Sessions), "cancelled");
}

#[test]
fn vertical_drags_scroll_sessions_taps_still_open_rows_and_fields_keep_their_drags() {
    let (_, mut rt) = fixture();
    detailed_inventory(&mut rt);
    rt.set_viewport((320.0, 568.0), 2.0);
    page(&mut rt, Page::Sessions);
    // Up the list: Rune's pan, never the swipe.
    drag(&mut rt, [200.0, 450.0], [-4.0, -200.0], 1.0);
    assert_eq!(rt.state.phone_page(), Some(Page::Sessions));
    assert!(rt.state.phone.as_ref().unwrap().sessions_scroll.get() > 50.0);
    assert_eq!(shift(&rt), [0.0, 0.0]);
    // Sideways from the search field: its drag, not the page's.
    rt.update(|_, tx| {
        tx.scroll_to_instant("phone-session-list", 0.0, 0.0);
        tx.instant();
    });
    settle(&mut rt);
    let field = centre(&rt, "phone-session-query");
    drag(&mut rt, field, [-150.0, 0.0], 2.0);
    assert_eq!(rt.state.phone_page(), Some(Page::Sessions));
}

#[test]
fn a_tap_through_the_swipe_path_still_opens_a_row() {
    let (_, mut rt) = fixture();
    detailed_inventory(&mut rt);
    page(&mut rt, Page::Sessions);
    rt.update(|d, _| d.phone.as_mut().unwrap().query.replace_text("build"));
    settle(&mut rt);
    let row = centre(&rt, "phone-session-pane-@3");
    finger(&mut rt, Started, row, 4.0);
    finger(&mut rt, Ended, row, 4.05);
    settle(&mut rt);
    assert_eq!(rt.state.phone_page(), None);
    assert!(
        rt.state.phone.as_ref().unwrap().query.text.is_empty(),
        "picked"
    );
}

#[test]
fn a_held_row_opens_its_menu_and_a_later_move_never_swipes() {
    let (_, mut rt) = fixture();
    detailed_inventory(&mut rt);
    page(&mut rt, Page::Sessions);
    let row = centre(&rt, "phone-session-pane-@3");
    finger(&mut rt, Started, row, 1.0);
    finger(&mut rt, Moved, [row[0] - 120.0, row[1]], 1.7);
    finger(&mut rt, Ended, [row[0] - 120.0, row[1]], 1.8);
    settle(&mut rt);
    assert_eq!(rt.state.phone_page(), Some(Page::Sessions));
    assert_eq!(shift(&rt), [0.0, 0.0]);
}

#[test]
fn sheets_lower_on_a_downward_swipe_from_their_top_and_lists_keep_theirs() {
    let (_, mut rt) = fixture();
    detailed_inventory(&mut rt);
    rt.set_viewport((320.0, 568.0), 2.0);
    page(&mut rt, Page::Actions);
    // The actions list is scrolled down: a downward drag on it scrolls it.
    rt.update(|_, tx| {
        tx.scroll_to_instant("phone-actions-list", 0.0, 120.0);
        tx.instant();
    });
    settle(&mut rt);
    assert!(actions_top(&rt) > 1.0);
    let list = centre(&rt, "phone-actions-list");
    drag(&mut rt, list, [0.0, 220.0], 1.0);
    assert_eq!(rt.state.phone_page(), Some(Page::Actions));
    assert!(actions_top(&rt) < 1.0);
    // At its top the same drag lowers the sheet.
    drag(&mut rt, list, [3.0, 220.0], 3.0);
    assert_eq!(rt.state.phone_page(), None);
    // The switcher lowers too, while its card's sideways swipe still pages.
    page(&mut rt, Page::Switcher);
    let card = centre(&rt, "phone-switcher-card");
    drag(&mut rt, card, [0.0, 200.0], 5.0);
    assert_eq!(rt.state.phone_page(), None);
}

#[test]
fn the_inbox_lowers_unless_a_reply_is_typed_and_the_palette_lowers_as_cancel_does() {
    let (_, mut rt) = fixture();
    detailed_inventory(&mut rt);
    rt.set_system_reduced_motion(true);
    rt.update(|d, _| {
        d.toggle_inbox();
        d.inbox.reply.replace_text("half a thought");
    });
    settle(&mut rt);
    let title = crate::ui::element(&rt, "inbox").rect;
    let top = [title[0] + 120.0, title[1] + 30.0];
    // A typed reply holds the sheet: a swipe never discards it.
    assert_eq!(rt.state.swipe_away(), None);
    drag(&mut rt, top, [0.0, 300.0], 1.0);
    assert!(rt.state.inbox.open);
    assert_eq!(rt.state.inbox.reply.text, "half a thought");
    rt.update(|d, _| d.inbox.reply.replace_text(""));
    settle(&mut rt);
    assert_eq!(rt.state.swipe_away(), Some(Away::Inbox));
    drag(&mut rt, top, [0.0, 300.0], 2.0);
    assert!(!rt.state.inbox.open);
    rt.update(|d, _| d.toggle_palette());
    settle(&mut rt);
    assert_eq!(rt.state.swipe_away(), Some(Away::Palette));
    // Not from its field, whose drags edit.
    let field = centre(&rt, "palette-search");
    drag(&mut rt, field, [0.0, 300.0], 3.0);
    assert!(rt.state.palette_open);
    let list = centre(&rt, "palette-scroll");
    drag(&mut rt, list, [0.0, 300.0], 5.0);
    assert!(!rt.state.palette_open);
}

#[test]
fn a_swipe_in_from_the_left_edge_goes_back_a_level_in_settings() {
    let (_, mut rt) = fixture();
    rt.set_system_reduced_motion(true);
    rt.update(|d, _| {
        d.toggle_settings();
        d.settings_category(crate::desk::settings::Category::Appearance);
    });
    settle(&mut rt);
    assert_eq!(rt.state.swipe_away(), Some(Away::Settings));
    // Away from the edge it is the page's own drag.
    drag(&mut rt, [120.0, 400.0], [200.0, 0.0], 1.0);
    assert!(rt.state.settings_ui.category.is_some());
    drag(&mut rt, [10.0, 400.0], [200.0, 0.0], 3.0);
    assert!(rt.state.settings_open);
    assert_eq!(rt.state.settings_ui.category, None, "back a level");
    assert_eq!(shift(&rt), [0.0, 0.0], "the level it comes back to is home");
    drag(&mut rt, [10.0, 400.0], [200.0, 0.0], 5.0);
    assert!(!rt.state.settings_open, "the top level closes");
}

#[test]
fn children_a_page_opens_return_to_it_on_closing() {
    let (_, mut rt) = fixture();
    detailed_inventory(&mut rt);
    page(&mut rt, Page::Actions);
    tap(&mut rt, "phone-action-Settings", 0.1);
    assert!(rt.state.settings_open);
    assert_eq!(rt.state.phone_page(), None);
    tap(&mut rt, "settings-close", 0.3);
    poll(&mut rt);
    assert!(!rt.state.settings_open);
    assert_eq!(
        rt.state.phone_page(),
        Some(Page::Actions),
        "back to actions"
    );
    tap(&mut rt, "phone-action-Inbox", 0.5);
    assert!(rt.state.inbox.open);
    tap(&mut rt, "inbox-close", 0.7);
    poll(&mut rt);
    assert_eq!(rt.state.phone_page(), Some(Page::Actions));
    tap(&mut rt, "phone-action-news", 0.9);
    assert!(rt.state.news.open);
    rt.update(|d, _| d.close_news());
    poll(&mut rt);
    assert_eq!(rt.state.phone_page(), Some(Page::Actions));
    // The desktop-hosted phone's Machines sheet takes the page's place.
    tap(&mut rt, "phone-action-Machines", 1.1);
    assert!(rt.state.machines.open);
    rt.update(|d, _| d.toggle_machines());
    poll(&mut rt);
    assert_eq!(rt.state.phone_page(), Some(Page::Actions));
}

#[test]
fn needs_you_opens_the_inbox_in_one_tap_and_comes_back_to_sessions() {
    let (_, mut rt) = fixture();
    detailed_inventory(&mut rt);
    page(&mut rt, Page::Sessions);
    tap(&mut rt, "phone-needs-you-heading", 1.3);
    assert!(rt.state.inbox.open);
    rt.key(crate::keys::plain(Key::Escape));
    settle(&mut rt);
    assert!(!rt.state.inbox.open);
    poll(&mut rt);
    assert_eq!(rt.state.phone_page(), Some(Page::Sessions));
}

#[test]
fn a_child_that_left_for_another_pane_or_page_does_not_come_back() {
    let (_, mut rt) = fixture();
    detailed_inventory(&mut rt);
    page(&mut rt, Page::Actions);
    tap(&mut rt, "phone-action-Inbox", 0.1);
    rt.update(|d, _| {
        d.connection.shown.as_mut().unwrap().resource = ResourceId::local(4);
        d.close_inbox();
    });
    poll(&mut rt);
    assert_eq!(rt.state.phone_page(), None, "it went to another pane");
    page(&mut rt, Page::Actions);
    tap(&mut rt, "phone-action-Settings", 0.3);
    // Another surface took the screen: the return is forgotten.
    rt.update(|d, _| {
        d.close_settings();
        d.toggle_palette();
    });
    poll(&mut rt);
    rt.update(|d, _| d.close_palette());
    poll(&mut rt);
    assert_eq!(rt.state.phone_page(), None);
    assert!(rt.state.phone.as_ref().unwrap().return_to.is_none());
}

#[test]
fn ios_connections_comes_up_over_the_page_it_was_opened_from() {
    let (_, mut rt) = fixture();
    detailed_inventory(&mut rt);
    rt.update(|d, _| d.native = Some(crate::mobile::Platform::test()));
    page(&mut rt, Page::Switcher);
    tap(&mut rt, "phone-switcher-computer", 0.1);
    assert!(rt.state.native.as_ref().unwrap().connection_requested);
    assert_eq!(rt.state.phone_page(), Some(Page::Switcher));
}

#[test]
fn sessions_keeps_its_search_and_place_until_a_row_is_picked() {
    let (_, mut rt) = fixture();
    detailed_inventory(&mut rt);
    rt.set_viewport((320.0, 568.0), 2.0);
    page(&mut rt, Page::Sessions);
    rt.update(|d, tx| {
        d.phone.as_mut().unwrap().query.replace_text("workspace");
        d.phone_search_edited(tx);
    });
    settle(&mut rt);
    rt.update(|_, tx| {
        tx.scroll_to_instant("phone-session-list", 0.0, 140.0);
        tx.instant();
    });
    settle(&mut rt);
    let kept = rt.state.phone.as_ref().unwrap().sessions_scroll.get();
    assert!(kept > 100.0, "{kept}");
    tap(&mut rt, "phone-navigation-close", 0.1);
    assert_eq!(rt.state.phone_page(), None);
    page(&mut rt, Page::Sessions);
    let phone = rt.state.phone.as_ref().unwrap();
    assert_eq!(phone.query.text, "workspace", "the search survives Done");
    assert!(!phone.query.focused);
    let back = rt.state.phone.as_ref().unwrap().sessions_scroll.get();
    assert!((back - kept).abs() < 1.0, "{back} vs {kept}");
    // Picking a row is done with the search.
    let (x, y) = rt.find("phone-session-pane-@5").unwrap();
    rt.touch(1, Started, x, y, 2.0);
    rt.touch(1, Ended, x, y, 2.05);
    settle(&mut rt);
    assert_eq!(rt.state.phone_page(), None);
    assert!(rt.state.phone.as_ref().unwrap().query.text.is_empty());
}

#[test]
fn sessions_opens_on_the_pane_on_screen() {
    let (_, mut rt) = fixture();
    detailed_inventory(&mut rt);
    // Short enough that the current pane starts below the fold.
    rt.set_viewport((320.0, 440.0), 2.0);
    page(&mut rt, Page::Sessions);
    let list = crate::ui::element(&rt, "phone-session-list");
    let row = crate::ui::element(&rt, "phone-session-pane-@1");
    let scrolled = rt.state.phone.as_ref().unwrap().sessions_scroll.get();
    assert!(scrolled > 0.0, "it scrolled to the pane: {scrolled}");
    assert!(
        row.rect[1] >= list.visible[1] - 0.5
            && row.rect[1] + row.rect[3] <= list.visible[1] + list.visible[3] + 0.5,
        "{row:?} in {list:?}"
    );
}

#[test]
fn a_session_headers_more_opens_its_menu_in_one_tap() {
    let (_, mut rt) = fixture();
    detailed_inventory(&mut rt);
    page(&mut rt, Page::Sessions);
    let more = crate::ui::element(&rt, "phone-session-more-1");
    assert!(more.rect[2] >= 44.0 && more.rect[3] >= 44.0, "{more:?}");
    assert_eq!(more.label.as_deref(), Some("More for phux"));
    tap(&mut rt, "phone-session-more-1", 0.1);
    let phone = rt.state.phone.as_ref().unwrap();
    assert_eq!(
        phone.menu.as_ref().map(|m| m.held.clone()),
        Some(Held::Session(1))
    );
    assert_eq!(phone.feedback, None, "a tap is not a hold");
    assert!(rt.find("phone-menu").is_some());
    // The ⋯ sits on the column the rows' checks share.
    let check = crate::ui::element(&rt, "phone-session-current-@1").rect;
    let mid = |r: [f32; 4]| r[0] + r[2] / 2.0;
    assert!(
        (mid(more.rect) - mid(check)).abs() <= 0.5,
        "{more:?} {check:?}"
    );
}

#[test]
fn back_carries_a_dot_while_a_pane_elsewhere_needs_you() {
    let (_, mut rt) = fixture();
    settle(&mut rt);
    let label = |rt: &Runtime<Desk>| {
        crate::ui::element(rt, "phone-back")
            .label
            .unwrap_or_default()
    };
    assert_eq!(label(&rt), "Sessions");
    detailed_inventory(&mut rt);
    settle(&mut rt);
    assert_eq!(label(&rt), "Sessions, 2 need you");
}
