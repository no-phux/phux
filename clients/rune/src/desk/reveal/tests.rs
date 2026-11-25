use super::super::tabs::Tab;
use super::*;
use crate::launch::Resolved;
use phux_client_runtime::control::{PaneDescriptor, SessionDescriptor};

fn pane(n: u32, session: &str) -> PaneDescriptor {
    PaneDescriptor {
        terminal_id: ResourceId::local(n),
        session_id: 1,
        session_name: session.to_owned(),
        cols: 80,
        rows: 24,
        title: None,
        cwd: None,
        is_focused: false,
        key: None,
        revision: None,
        name: None,
        activation: None,
        exec: None,
        foreground: None,
        icon: None,
        parent: None,
        unseen: None,
    }
}

fn session(id: u32, name: &str, arranged: bool) -> SessionDescriptor {
    SessionDescriptor {
        id,
        name: name.to_owned(),
        attached_client_count: 0,
        keep_empty: false,
        created_at_unix_secs: 0,
        key: None,
        touched: None,
        lifetime: None,
        revision: None,
        members: 0,
        arrangement: arranged.then(|| phux_protocol::ids::ArrangementKey::new([7; 16]).unwrap()),
        awaiting_organization: false,
        project: None,
        project_link: None,
    }
}

/// A window with a client, so a link has a view scope to be opened in.
fn desk() -> Desk {
    let mut desk = Desk::with_preferences(
        &Resolved::offline("reveal"),
        None,
        rune::prefs::Prefs::memory(),
    );
    desk.connection.client = Some(std::sync::Arc::new(phux_client_runtime::Runtime::embedded(
        phux_client_runtime::control::ControlOptions::default(),
    )));
    desk
}

/// The link a window still holds.
fn held(desk: &Desk) -> Option<&Link> {
    desk.connection.reveal.as_ref().map(|reveal| &reveal.link)
}

fn notice(desk: &Desk) -> Option<&str> {
    desk.notice.as_ref().map(|n| n.text.as_str())
}

#[test]
fn links_parse_strictly() {
    assert_eq!(
        target("phux://pane/@3"),
        Some(Ok(Link::Pane(ResourceId::local(3))))
    );
    assert_eq!(
        target("PHUX://Pane/@3/"),
        Some(Ok(Link::Pane(ResourceId::local(3))))
    );
    let satellite = ResourceId::satellite("box:2222", 7);
    assert_eq!(
        target("phux://pane/box:2222/@7"),
        Some(Ok(Link::Pane(satellite)))
    );
    assert_eq!(
        target("phux://session/my%20work"),
        Some(Ok(Link::Session("my work".to_owned())))
    );
    let tab = Link::Tab {
        session: "a/b".to_owned(),
        index: 2,
        since: None,
    };
    assert_eq!(target("phux://tab/a%2Fb/2"), Some(Ok(tab)));
    for bad in [
        "phux://pane/",
        "phux://pane/3",
        "phux://pane/@x",
        "phux://pane//@3",
        "phux://session/",
        "phux://session/%zz",
        "phux://session/a%0Ab",
        "phux://tab/work",
        "phux://tab/work/0",
        "phux://tab//1",
        "phux://tab/work/x",
    ] {
        assert!(matches!(target(bad), Some(Err(_))), "{bad}");
    }
    assert_eq!(target("phux://connect?quic=quic://mini:8788"), None);
    assert_eq!(target("phux://elsewhere/x"), None, "pairing refuses it");
}

#[test]
fn a_folder_link_must_name_a_directory_here() {
    let dir = std::env::temp_dir().join(format!("phux reveal {}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("f.txt"), "").unwrap();
    let encoded = dir.display().to_string().replace(' ', "%20");
    let link = format!("phux://folder{encoded}");
    assert_eq!(target(&link), Some(Ok(Link::Folder(dir.clone()))));
    for bad in [
        format!("{link}/f.txt"),
        format!("{link}/missing"),
        format!("{link}%00"),
        "phux://folder/%zz".to_owned(),
    ] {
        assert!(matches!(target(&bad), Some(Err(_))), "{bad}");
    }
    let mut desk = desk();
    desk.connection.topology = Some(crate::desk::topology(Vec::new(), Vec::new()));
    desk.open_system_link(&link);
    assert_eq!(held(&desk), None);
    assert_eq!(notice(&desk), None);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_pane_is_found_exactly_or_refused() {
    let graph = crate::desk::topology(Vec::new(), vec![pane(1, "work"), pane(2, "api")]);
    assert_eq!(locate(&graph, &ResourceId::local(2)), Ok("api".to_owned()));
    let gone = locate(&graph, &ResourceId::local(9)).unwrap_err();
    assert!(gone.contains("@9"), "{gone}");
}

#[test]
fn a_link_waits_for_the_panes_then_fails_closed() {
    let mut desk = desk();
    desk.open_system_link("phux://pane/@9");
    let link = Link::Pane(ResourceId::local(9));
    assert_eq!(held(&desk), Some(&link), "held with its original owner");
    assert!(desk.notice.is_none());
    desk.connection.topology = Some(crate::desk::topology(Vec::new(), vec![pane(1, "w")]));
    assert!(!desk.follow_reveal());
    assert_eq!(held(&desk), None);
    assert_eq!(
        notice(&desk),
        Some("No pane @9 on this server, so nothing was revealed.")
    );
    assert!(desk.connection.shown.is_none(), "no fallback pane");

    desk.machines.current = Some("mini".to_owned());
    desk.open_system_link("phux://pane/@1");
    assert_eq!(held(&desk), None);
    assert!(notice(&desk).is_some_and(|n| n.contains("mini")));
}

#[test]
fn a_session_or_tab_is_found_exactly_or_refused() {
    let mut desk = desk();
    let sessions = vec![session(1, "work", true), session(2, "bare", false)];
    desk.connection.topology = Some(crate::desk::topology(sessions, vec![pane(1, "work")]));

    desk.open_system_link("phux://session/work");
    assert_eq!((held(&desk), notice(&desk)), (None, None));
    desk.open_system_link("phux://session/gone");
    assert!(notice(&desk).is_some_and(|n| n.contains("\"gone\"")));

    desk.open_system_link("phux://tab/bare/1");
    assert!(
        notice(&desk).is_some_and(|n| n.contains("didn't load")),
        "no arrangement"
    );

    desk.notice = None;
    desk.open_system_link("phux://tab/work/2");
    assert!(
        matches!(held(&desk), Some(Link::Tab { since: Some(_), .. })),
        "waits for the session's tabs"
    );
    assert!(desk.follow_reveal(), "still waiting");
    let tab = Tab {
        id: [1; 16],
        name: "one".to_owned(),
        resources: vec![ResourceId::local(1)],
        active: true,
        locked: false,
    };
    desk.connection.tabs.by_session.insert(1, vec![tab]);
    assert!(!desk.follow_reveal());
    assert!(
        notice(&desk).is_some_and(|n| n.contains("no tab 2")),
        "{:?}",
        notice(&desk)
    );

    desk.notice = None;
    desk.open_system_link("phux://tab/work/1");
    assert_eq!((held(&desk), notice(&desk)), (None, None));

    desk.connection.tabs.by_session.clear();
    desk.open_system_link("phux://tab/work/1");
    let reveal = desk.connection.reveal.as_mut().expect("waits for the tabs");
    reveal.link = Link::Tab {
        session: "work".to_owned(),
        index: 1,
        since: Some(Instant::now() - TAB_WAIT),
    };
    assert!(!desk.follow_reveal(), "gives up");
    assert!(notice(&desk).is_some_and(|n| n.contains("didn't load")));
}

mod source_admission {
    use super::*;
    use crate::test_support::terminal::Replica;

    #[test]
    fn delayed_links_keep_their_original_source_and_first_source_epoch() {
        for pending in [false, true] {
            let replica = if pending {
                Replica::pending(80, 24, 2)
            } else {
                Replica::new(80, 24, 2)
            };
            let client = &replica.client;
            let mut desk = replica.window();
            let topology = desk.connection.topology.take();
            desk.open_system_link("phux://pane/@2");
            assert!(desk.connection.reveal.is_some());
            replica.replace_server(2);
            desk.connection.topology = topology;
            let _ = client.take_outbound();
            desk.follow_reveal();
            assert!(desk.connection.reveal.is_none());
            assert!(desk.connection.wanted.is_none());
            assert!(desk.connection.shown.is_none());
            assert!(client.take_outbound().is_empty());
        }
    }

    #[test]
    fn a_delayed_link_cannot_supersede_a_newer_focus_gesture() {
        let replica = Replica::pending(80, 24, 2);
        let mut desk = replica.window();
        let topology = desk.connection.topology.take();
        desk.open_system_link("phux://pane/@2");
        desk.want(ResourceId::local(1));
        desk.connection.topology = topology;
        desk.follow_reveal();
        assert!(desk.open_wanted(&replica.client));
        assert_eq!(
            desk.connection.shown.as_ref().map(|shown| &shown.resource),
            Some(&ResourceId::local(1))
        );
    }

    #[test]
    fn a_link_with_an_existing_source_can_open_the_retained_offline_replica() {
        let replica = Replica::new(80, 24, 1);
        let mut desk = replica.window();
        let topology = desk.connection.topology.take();
        desk.open_system_link("phux://pane/@1");
        replica
            .client
            .with_control(|control| control.connection_lost(None));
        desk.connection.topology = topology;
        desk.follow_reveal();
        let view = desk.connection.shown.as_ref().unwrap().view.unwrap();
        assert!(replica.client.acquire_view(view).is_some());
    }
}
