//! The Sessions page's organisation: what needs you first, sessions as
//! grouped cards that fold, quiet sessions folded under their names, and a
//! hold on a row for its menu.
use super::*;
use crate::desk::phone::navigation::Held;
use crate::desk::phone::touch::Feedback;

mod native;

/// A finger held still past the long press, then lifted, on a row scrolled
/// clear of the search field and the floating dock.
pub(super) fn hold(rt: &mut Runtime<Desk>, key: &str, at: f64) {
    let row = crate::ui::element(rt, key).rect;
    let list = crate::ui::element(rt, "phone-session-list").rect;
    if row[1] < list[1] || row[1] + row[3] > list[1] + list[3] - 80.0 {
        // Measure from the top, then bring the row up under the search field.
        rt.update(|_, tx| tx.scroll_to_instant("phone-session-list", 0.0, 0.0));
        settle(rt);
        let offset = crate::ui::element(rt, key).rect[1] - list[1] - 20.0;
        rt.update(|_, tx| tx.scroll_to_instant("phone-session-list", 0.0, offset));
        settle(rt);
    }
    let (x, y) = rt.find(key).unwrap_or_else(|| panic!("no {key}"));
    rt.touch(1, TouchPhase::Started, x, y, at);
    for _ in 0..40 {
        rt.frame(1.0 / 60.0);
    }
    rt.touch(1, TouchPhase::Ended, x, y, at + 0.7);
    settle(rt);
}

fn open(rt: &mut Runtime<Desk>) {
    rt.set_system_reduced_motion(true);
    rt.update(|d, tx| {
        d.open_phone_sessions();
        tx.instant();
    });
    settle(rt);
}

/// The server reports persistent keys; the fixture's sessions need them to
/// be renamed or closed.
fn keyed(rt: &mut Runtime<Desk>) {
    rt.update(|d, _| {
        for session in &mut d.connection.topology.as_mut().unwrap().sessions {
            let byte = u8::try_from(session.id).unwrap();
            session.key = Some(phux_protocol::ids::SessionKey::new([byte; 16]).unwrap());
        }
    });
}

fn menu(rt: &Runtime<Desk>) -> Option<Held> {
    rt.state
        .phone
        .as_ref()?
        .menu
        .as_ref()
        .map(|m| m.held.clone())
}

#[test]
fn needs_you_leads_and_quiet_sessions_fold_under_their_names() {
    let (_, mut rt) = fixture();
    detailed_inventory(&mut rt);
    open(&mut rt);
    let band = crate::ui::element(&rt, "phone-needs-you").rect;
    let first = crate::ui::element(&rt, "phone-session-section-1").rect;
    assert!(band[1] < first[1], "needs you leads: {band:?} {first:?}");
    // Claude is blocked; OpenCode finished unseen. Both ask, from two sessions.
    let waiting = crate::ui::element(&rt, "phone-needs-you-heading");
    assert!(
        waiting
            .label
            .as_deref()
            .unwrap_or_default()
            .contains("Needs you, 2"),
        "{waiting:?}"
    );
    // "notes" has no panes and nothing moving: it folds into Quiet.
    assert!(rt.find("phone-session-section-3").is_none());
    // The scroll tail lets the last band clear the floating dock.
    rt.update(|_, tx| tx.scroll_to_instant("phone-session-list", 0.0, 100_000.0));
    settle(&mut rt);
    assert!(
        crate::ui::element(&rt, "phone-quiet")
            .label
            .as_deref()
            .unwrap_or_default()
            .contains("folded")
    );
    tap(&mut rt, "phone-quiet", 0.1);
    assert!(rt.state.phone.as_ref().unwrap().quiet_shown);
    assert!(rt.find("phone-session-section-3").is_some());
    // A search lists every match in place; the triage band steps aside.
    rt.update(|d, tx| {
        d.phone.as_mut().unwrap().query.replace_text("notes");
        d.phone_search_edited(tx);
    });
    settle(&mut rt);
    assert!(rt.find("phone-needs-you").is_none());
    assert!(rt.find("phone-quiet").is_none());
    assert!(rt.find("phone-session-section-3").is_some());
}

#[test]
fn a_session_folds_to_its_header_and_its_chevron_unfolds_it() {
    let (client, mut rt) = fixture();
    detailed_inventory(&mut rt);
    open(&mut rt);
    let _ = client.take_outbound();
    assert!(rt.find("phone-session-pane-@3").is_some());
    tap(&mut rt, "phone-session-fold-1", 0.1);
    assert!(rt.find("phone-session-pane-@3").is_none(), "folded");
    assert!(rt.find("phone-session-1").is_some(), "header stays");
    assert_eq!(rt.state.phone_page(), Some(Page::Sessions));
    tap(&mut rt, "phone-session-fold-1", 0.3);
    assert!(rt.find("phone-session-pane-@3").is_some(), "unfolded");
    let sent: Vec<_> = client
        .take_outbound()
        .into_iter()
        .map(|bytes| FrameKind::decode(&bytes).unwrap().0)
        .filter(|frame| {
            matches!(
                frame,
                FrameKind::Command { .. }
                    | FrameKind::InputKey { .. }
                    | FrameKind::InputPaste { .. }
            )
        })
        .collect();
    assert!(
        sent.is_empty(),
        "folding is local: no input, no commands: {sent:?}"
    );
}

#[test]
fn a_hold_on_a_pane_opens_its_menu_with_a_tick_and_never_types() {
    let (client, mut rt) = fixture();
    detailed_inventory(&mut rt);
    open(&mut rt);
    let _ = client.take_outbound();
    hold(&mut rt, "phone-session-pane-@3", 0.1);
    assert_eq!(
        menu(&rt),
        Some(Held::Pane(ResourceId::local(3), "phux".into()))
    );
    assert_eq!(
        rt.state.phone.as_ref().unwrap().feedback,
        Some(Feedback::Hold)
    );
    assert_eq!(
        rt.state.phone_page(),
        Some(Page::Sessions),
        "a hold opens nothing"
    );
    for key in ["phone-menu-Open", "phone-menu-NewPane", "phone-menu-Close"] {
        let rect = crate::ui::element(&rt, key).rect;
        assert!(rect[3] >= 44.0, "{key} is a touch row: {rect:?}");
    }
    // The menu stays on the page.
    let menu_rect = crate::ui::element(&rt, "phone-menu").rect;
    assert!(
        menu_rect[1] >= 0.0 && menu_rect[1] + menu_rect[3] <= 852.0,
        "{menu_rect:?}"
    );
    // Escape and a press outside both dismiss it.
    rt.key(crate::keys::plain(Key::Escape));
    settle(&mut rt);
    assert_eq!(menu(&rt), None);
    assert_eq!(rt.state.phone_page(), Some(Page::Sessions));
    hold(&mut rt, "phone-session-pane-@3", 1.0);
    let (x, y) = (20.0, 840.0);
    rt.touch(1, TouchPhase::Started, x, y, 2.0);
    settle(&mut rt);
    rt.touch(1, TouchPhase::Ended, x, y, 2.05);
    settle(&mut rt);
    assert_eq!(menu(&rt), None);
    assert!(
        client.take_outbound().into_iter().all(|bytes| !matches!(
            FrameKind::decode(&bytes).unwrap().0,
            FrameKind::InputKey { .. } | FrameKind::InputPaste { .. }
        )),
        "holding never types"
    );
}

#[test]
fn a_pane_menu_opens_it_or_closes_it_by_the_close_rules() {
    let (client, mut rt) = fixture();
    detailed_inventory(&mut rt);
    open(&mut rt);
    hold(&mut rt, "phone-session-pane-@3", 0.1);
    tap(&mut rt, "phone-menu-Open", 1.0);
    assert!(rt.state.phone_page().is_none());
    assert_eq!(
        rt.state
            .connection
            .wanted
            .as_ref()
            .map(|(id, _)| id.clone()),
        Some(ResourceId::local(3))
    );
    // Nothing to ask about: the pane closes and the page stays.
    open(&mut rt);
    let _ = client.take_outbound();
    hold(&mut rt, "phone-session-pane-@5", 2.0);
    tap(&mut rt, "phone-menu-Close", 3.0);
    assert!(rt.state.pending.is_none());
    assert_eq!(rt.state.phone_page(), Some(Page::Sessions));
    // The close went to the server at once: this fixture's server has no
    // close feature, so the submission is refused and says so. Either way
    // nothing asked and nothing was typed.
    let frames: Vec<_> = client
        .take_outbound()
        .into_iter()
        .map(|bytes| FrameKind::decode(&bytes).unwrap().0)
        .collect();
    let killed = frames
        .iter()
        .any(|frame| format!("{frame:?}").contains("KillResource"));
    let refused = rt
        .state
        .notice
        .as_ref()
        .is_some_and(|n| n.text.starts_with("Close unavailable"));
    assert!(killed || refused, "the idle pane's close was submitted");
    assert!(!frames.iter().any(|frame| matches!(
        frame,
        FrameKind::InputKey { .. } | FrameKind::InputPaste { .. }
    )));
    // With "Confirm before closing" on, it asks, over the pane it is about.
    rt.update(|d, _| d.settings.confirm_close = true);
    hold(&mut rt, "phone-session-pane-@3", 4.0);
    tap(&mut rt, "phone-menu-Close", 5.0);
    assert_eq!(
        rt.state.pending,
        Some(crate::desk::Pending::Close(ResourceId::local(3)))
    );
    assert!(rt.state.phone_page().is_none());
    assert_eq!(
        rt.state
            .connection
            .wanted
            .as_ref()
            .map(|(id, _)| id.clone()),
        Some(ResourceId::local(3))
    );
}

#[test]
fn a_session_menu_folds_renames_and_asks_before_closing() {
    let (_, mut rt) = fixture();
    detailed_inventory(&mut rt);
    keyed(&mut rt);
    open(&mut rt);
    hold(&mut rt, "phone-session-2", 0.1);
    assert_eq!(menu(&rt), Some(Held::Session(2)));
    tap(&mut rt, "phone-menu-Fold", 1.0);
    assert!(rt.state.phone.as_ref().unwrap().folded.contains(&2));
    assert!(rt.find("phone-session-pane-@5").is_none());
    assert_eq!(menu(&rt), None);
    // Rename opens the rename sheet on that session, by its key, named.
    hold(&mut rt, "phone-session-2", 2.0);
    tap(&mut rt, "phone-menu-Rename", 3.0);
    assert!(rt.state.phone_page().is_none());
    let key = phux_protocol::ids::SessionKey::new([2; 16]).unwrap();
    assert_eq!(
        rt.state.connection.tabs.renaming,
        Some(crate::desk::Renaming::Session(key))
    );
    assert!(
        rt.state
            .connection
            .tabs
            .rename_field
            .text
            .starts_with("api")
    );
    rt.update(|d, _| d.cancel_rename());
    open(&mut rt);
    hold(&mut rt, "phone-session-2", 4.0);
    tap(&mut rt, "phone-menu-Close", 5.0);
    assert!(rt.state.phone_page().is_none());
    assert!(
        matches!(
            rt.state.pending,
            Some(crate::desk::Pending::CloseSession(asked, ref name))
                if asked == key && name.starts_with("api")
        ),
        "{:?}",
        rt.state.pending
    );
}

#[test]
fn a_search_offers_no_fold_and_shows_folded_sessions_whole() {
    let (_, mut rt) = fixture();
    detailed_inventory(&mut rt);
    open(&mut rt);
    rt.update(|d, _| d.toggle_phone_fold(1));
    rt.update(|d, tx| {
        d.phone.as_mut().unwrap().query.replace_text("phux");
        d.phone_search_edited(tx);
    });
    settle(&mut rt);
    assert!(rt.find("phone-session-pane-@3").is_some(), "matches show");
    assert!(
        rt.find("phone-session-fold-1").is_none(),
        "no fold to offer"
    );
    hold(&mut rt, "phone-session-1", 0.1);
    assert!(rt.find("phone-menu-Open").is_some());
    assert!(rt.find("phone-menu-Fold").is_none());
}

#[test]
fn a_question_that_arrives_under_the_menu_is_answered_first() {
    let (_, mut rt) = fixture();
    detailed_inventory(&mut rt);
    open(&mut rt);
    hold(&mut rt, "phone-session-pane-@5", 0.1);
    let asked = crate::desk::Pending::Close(ResourceId::local(4));
    rt.update(|d, _| d.pending = Some(asked.clone()));
    tap(&mut rt, "phone-menu-Close", 1.0);
    assert_eq!(rt.state.pending, Some(asked), "never replaced");
    assert!(
        rt.state.phone_page().is_none(),
        "its bar is over the terminal"
    );
}

#[test]
fn escape_closes_the_page_when_the_held_row_has_gone() {
    let (_, mut rt) = fixture();
    detailed_inventory(&mut rt);
    open(&mut rt);
    hold(&mut rt, "phone-session-pane-@3", 0.1);
    rt.update(|d, _| {
        d.phone.as_mut().unwrap().menu.as_mut().unwrap().held =
            Held::Pane(ResourceId::local(99), "phux".into());
    });
    settle(&mut rt);
    assert!(rt.find("phone-menu").is_none(), "nothing to draw");
    rt.key(crate::keys::plain(Key::Escape));
    settle(&mut rt);
    assert!(rt.state.phone_page().is_none());
}

#[test]
fn a_session_menu_starts_a_pane_in_that_session() {
    let (client, mut rt) = fixture();
    detailed_inventory(&mut rt);
    open(&mut rt);
    let _ = client.take_outbound();
    let before = rt.state.connection.pending_spawns.len();
    hold(&mut rt, "phone-session-2", 0.1);
    tap(&mut rt, "phone-menu-NewPane", 1.0);
    assert!(rt.state.phone_page().is_none());
    assert_eq!(rt.state.connection.pending_spawns.len(), before + 1);
    let spawn = client
        .take_outbound()
        .into_iter()
        .map(|bytes| format!("{:?}", FrameKind::decode(&bytes).unwrap().0))
        .find(|frame| frame.contains("Spawn"))
        .expect("a spawn went out");
    assert!(
        spawn.contains("/workspace/api"),
        "in the session's folder: {spawn}"
    );
}

#[test]
fn a_late_native_menu_pick_cannot_consume_a_newer_menu() {
    let (_, mut rt) = fixture();
    detailed_inventory(&mut rt);
    keyed(&mut rt);
    rt.update(|d, _| d.native = Some(crate::mobile::Platform::test()));
    open(&mut rt);
    hold(&mut rt, "phone-session-2", 0.1);
    let old = crate::mobile::take_requests(&mut rt)["menu"]["request"]
        .as_u64()
        .unwrap();
    rt.update(|d, _| d.close_phone_navigation());
    open(&mut rt);
    hold(&mut rt, "phone-session-1", 2.0);
    let new = crate::mobile::take_requests(&mut rt)["menu"]["request"]
        .as_u64()
        .unwrap();
    assert_ne!(old, new);
    assert!(!crate::mobile::menu_pick(&mut rt, old, "fold"));
    crate::mobile::menu_dismiss(&mut rt, old);
    assert_eq!(menu(&rt), Some(Held::Session(1)));
    assert!(!rt.state.phone.as_ref().unwrap().folded.contains(&1));
    assert!(crate::mobile::menu_pick(&mut rt, new, "fold"));
    assert!(rt.state.phone.as_ref().unwrap().folded.contains(&1));
}

#[test]
fn the_native_host_opens_the_systems_menu_once_and_runs_its_pick() {
    let (_, mut rt) = fixture();
    detailed_inventory(&mut rt);
    keyed(&mut rt);
    rt.update(|d, _| d.native = Some(crate::mobile::Platform::test()));
    open(&mut rt);
    let _ = crate::mobile::take_requests(&mut rt);
    hold(&mut rt, "phone-session-2", 0.1);
    assert_eq!(menu(&rt), Some(Held::Session(2)));
    assert!(rt.find("phone-menu").is_none(), "Rune draws no menu there");
    let asked = crate::mobile::take_requests(&mut rt);
    let ask = &asked["menu"];
    let request = ask["request"].as_u64().unwrap();
    assert!(ask["title"].as_str().unwrap().starts_with("api"), "{asked}");
    let (row, _) = rt.presented("phone-session-2").unwrap();
    let rect: Vec<f32> = ask["rect"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap() as f32)
        .collect();
    assert_eq!(rect, row, "it opens from the held row");
    let items = ask["items"].as_array().unwrap();
    let ids: Vec<&str> = items.iter().map(|i| i["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["open", "new-pane", "fold", "rename", "close"]);
    assert!(items.iter().all(|i| i["symbol"].as_str().is_some()));
    let destructive: Vec<bool> = items.iter().map(|i| i["destructive"] == true).collect();
    assert_eq!(destructive, [false, false, false, false, true]);
    assert_eq!(
        crate::mobile::take_requests(&mut rt)["menu"],
        serde_json::Value::Null,
        "asked once"
    );
    // UIKit may run the pick after its menu has gone; it still lands.
    assert!(!crate::mobile::menu_pick(&mut rt, request, "explode"));
    crate::mobile::menu_dismiss(&mut rt, request);
    assert_eq!(
        menu(&rt),
        None,
        "visual menu retired, not an unbound current target"
    );
    assert!(crate::mobile::menu_pick(&mut rt, request, "fold"));
    assert!(
        !crate::mobile::menu_pick(&mut rt, request, "fold"),
        "once only"
    );
    settle(&mut rt);
    assert!(rt.state.phone.as_ref().unwrap().folded.contains(&2));
    assert_eq!(menu(&rt), None);
    // A menu UIKit dismissed leaves Escape to close the page.
    hold(&mut rt, "phone-session-2", 2.0);
    rt.key(crate::keys::plain(Key::Escape));
    settle(&mut rt);
    assert_eq!(rt.state.phone_page(), None);
}
