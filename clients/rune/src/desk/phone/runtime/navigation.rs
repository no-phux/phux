use super::*;
use crate::desk::phone::navigation::Page;
use phux_client_runtime::control::Topology;
use phux_protocol::ids::{ResourceId, SessionId, WindowId};
use phux_protocol::wire::frame::{CommandResult, CommandValue};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot};

mod layout;
mod render;

fn inventory(rt: &mut Runtime<Desk>) {
    let session = SessionId::new(1);
    let window = WindowId::new(1);
    let snapshot = SessionSnapshot::new(session, window, ResourceId::local(1))
        .with_sessions(vec![
            SessionInfo::new(session, "budget"),
            SessionInfo::new(SessionId::new(2), "empty"),
        ])
        .with_resources(vec![
            ResourceInfo::new(ResourceId::local(1), window, 80, 24)
                .with_session(Some(session))
                .with_title(Some("editor".into())),
        ]);
    set_inventory(rt, snapshot);
}

fn set_inventory(rt: &mut Runtime<Desk>, snapshot: SessionSnapshot) {
    let client = rt.state.client.clone().unwrap();
    let request_id = client.refresh_topology().unwrap();
    client
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(CommandValue::State(snapshot.clone())),
        })
        .unwrap();
    rt.update(|d, _| d.topology = Some(Topology::from_snapshot(&snapshot)));
}

fn detailed_inventory(rt: &mut Runtime<Desk>) {
    let session = SessionId::new(1);
    let window = WindowId::new(1);
    let pane = |n, session, title: &str, cwd: &str| {
        ResourceInfo::new(ResourceId::local(n), window, 80, 24)
            .with_session(Some(SessionId::new(session)))
            .with_title(Some(title.into()))
            .with_cwd(Some(cwd.into()))
    };
    set_inventory(
        rt,
        SessionSnapshot::new(session, window, ResourceId::local(1))
            .with_sessions(vec![
                SessionInfo::new(session, "phux"),
                SessionInfo::new(SessionId::new(2), "api — authentication and permissions"),
                SessionInfo::new(SessionId::new(3), "notes"),
            ])
            .with_resources(vec![
                pane(1, 1, "editor", "/workspace/phux"),
                pane(2, 1, "Review session navigation", "/workspace/phux"),
                pane(3, 1, "Build and test", "/workspace/phux"),
                pane(4, 2, "Investigate reconnect behavior", "/workspace/api"),
                pane(5, 2, "shell", "/workspace/api"),
            ]),
    );
    rt.update(|d, _| {
        for (id, name, state) in [
            (2, "Claude", "blocked"),
            (3, "Codex", "working"),
            (4, "OpenCode", "done"),
        ] {
            d.activities.apply(
                &phux_client_runtime::control::Event::AgentMetadata {
                    terminal_id: ResourceId::local(id),
                    value: Some(
                        serde_json::to_vec(&serde_json::json!({"name":name,"state":state}))
                            .unwrap(),
                    ),
                },
                &[],
            );
        }
    });
}

#[test]
fn top_buttons_have_distinct_destinations_and_never_type_into_the_terminal() {
    let (client, mut rt) = fixture();
    inventory(&mut rt);
    settle(&mut rt);
    tap(&mut rt, "phone-back", 0.1);
    assert_eq!(rt.state.phone_page(), Some(Page::Sessions));
    assert!(!rt.state.palette_open);
    assert!(
        !rt.state.phone_keyboard_visible(),
        "browsing does not summon the keyboard"
    );
    assert!(rt.find("phone-session-1").is_some());
    tap(&mut rt, "phone-navigation-close", 0.3);
    tap(&mut rt, "phone-place", 0.5);
    assert!(rt.state.phone_picker_visible());
    assert!(!rt.state.palette_open);
    tap(&mut rt, "phone-picker-close", 0.7);
    tap(&mut rt, "phone-more", 0.9);
    assert_eq!(rt.state.phone_page(), Some(Page::Actions));
    assert!(!rt.state.palette_open);
    assert!(rt.find("phone-action-NewTerminal").is_some());
    no_terminal_input(&client);
}

#[test]
fn session_search_filters_and_drawn_keys_stay_in_the_field() {
    let (client, mut rt) = fixture();
    inventory(&mut rt);
    rt.update(|d, _| d.open_phone_sessions());
    settle(&mut rt);
    tap(&mut rt, "phone-session-query", 0.1);
    for (index, c) in "empty".chars().enumerate() {
        rt.update(|d, tx| d.soft(Soft::Char(c), tx));
        rt.frame(index as f64 / 60.0);
    }
    settle(&mut rt);
    assert_eq!(rt.state.phone.as_ref().unwrap().query.text, "empty");
    assert!(rt.find("phone-session-1").is_none());
    assert!(rt.find("phone-session-2").is_some());
    rt.update(|d, tx| d.soft(Soft::Key(Key::Enter), tx));
    assert!(!rt.state.phone_keyboard_visible());
    assert_eq!(rt.state.phone.as_ref().unwrap().query.text, "empty");
    no_terminal_input(&client);
}

#[test]
fn selecting_an_empty_session_switches_the_runtime_and_clears_the_old_terminal() {
    let (client, mut rt) = fixture();
    inventory(&mut rt);
    rt.update(|d, _| d.open_phone_sessions());
    settle(&mut rt);
    let _ = client.take_outbound();
    tap(&mut rt, "phone-session-2", 0.1);
    assert!(rt.state.phone_page().is_none());
    assert_eq!(rt.state.pending_session.as_deref(), Some("empty"));
    assert_eq!(client.selected_session(), Some(2));
    rt.update(|d, _| {
        d.open_wanted(&client);
        d.refresh_layout(&client);
    });
    assert!(rt.state.pending_session.is_none());
    assert!(rt.state.shown.is_none());
    assert!(rt.state.tabs.list.is_empty());
    no_terminal_input(&client);
}

#[test]
fn settings_action_closes_navigation_and_opens_settings_not_command_search() {
    let (client, mut rt) = fixture();
    rt.update(|d, _| d.open_phone_actions());
    settle(&mut rt);
    assert!(rt.accessibility_action(
        "phone-action-Settings",
        rune::accesskit::Action::Click,
        None
    ));
    settle(&mut rt);
    assert!(rt.state.settings_open);
    assert!(rt.state.phone_page().is_none());
    assert!(!rt.state.palette_open);
    no_terminal_input(&client);
}

#[test]
fn sessions_remain_usable_in_a_small_phone_with_search_keyboard_open() {
    for size in [(320.0, 568.0), (393.0, 852.0)] {
        let (_client, mut rt) = fixture();
        inventory(&mut rt);
        rt.set_viewport(size, 2.0);
        rt.update(|d, _| d.open_phone_sessions());
        settle(&mut rt);
        tap(&mut rt, "phone-session-query", 0.1);
        for key in [
            "phone-navigation-close",
            "phone-session-query",
            "phone-session-new",
        ] {
            let element = rt
                .elements()
                .into_iter()
                .find(|e| e.key.as_deref() == Some(key))
                .unwrap();
            assert!(element.visible[3] >= 40.0, "{size:?} {key}: {element:?}");
            assert!(element.rect[0] >= 0.0 && element.rect[0] + element.rect[2] <= size.0 + 0.5);
        }
    }
}

#[test]
fn a_higher_modal_owns_soft_keys_even_with_session_search_retained_underneath() {
    let (client, mut rt) = fixture();
    rt.update(|d, _| d.open_phone_sessions());
    settle(&mut rt);
    tap(&mut rt, "phone-session-query", 0.1);
    rt.update(|d, _| d.inbox.open = true);
    rt.update(|d, tx| d.soft(Soft::Char('x'), tx));
    assert_eq!(rt.state.phone.as_ref().unwrap().query.text, "");
    assert_eq!(rt.state.modal(), Some(Modal::Inbox));
    no_terminal_input(&client);
}

#[test]
fn the_native_browser_never_offers_a_nonexistent_local_mac() {
    let (_, mut rt) = fixture();
    rt.update(|d, _| {
        d.native = Some(crate::mobile::Platform {
            target: phux_client_runtime::Target::uds("/tmp/not-used"),
            active: true,
            connection_requested: false,
            paste_requested: false,
            away_since: None,
        });
        d.machines.current = Some("mini".into());
        d.open_phone_sessions();
    });
    settle(&mut rt);
    assert!(rt.find("phone-session-connections").is_some());
    assert!(
        rt.find(&format!("phone-sessions-host-{}", crate::machines::LOCAL))
            .is_none()
    );
    tap(&mut rt, "phone-session-connections", 0.1);
    assert!(rt.state.native.as_ref().unwrap().connection_requested);
    assert!(rt.state.phone_page().is_none());
}

#[test]
fn navigation_reserves_desktop_traffic_lights_but_not_a_second_native_safe_area() {
    let (_, mut rt) = fixture();
    rt.set_titlebar_inset(80.0);
    rt.update(|d, _| d.open_phone_sessions());
    settle(&mut rt);
    let close = rt.presented("phone-navigation-close").unwrap().0;
    let inset = rt.presented("phone-top").unwrap().0[1];
    assert!(inset > 0.0);
    assert!(close[1] >= inset);
    rt.update(|d, tx| {
        d.native = Some(crate::mobile::Platform {
            target: phux_client_runtime::Target::uds("/tmp/not-used"),
            active: true,
            connection_requested: false,
            paste_requested: false,
            away_since: None,
        });
        tx.instant();
    });
    settle(&mut rt);
    let close = rt.presented("phone-navigation-close").unwrap().0;
    assert!(close[1] < inset, "UIKit already owns the safe area");
}
