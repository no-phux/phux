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

fn desk() -> Desk {
    Desk::with_preferences(
        &Resolved::offline("reveal"),
        None,
        rune::prefs::Prefs::memory(),
    )
}

fn notice(desk: &Desk) -> Option<&str> {
    desk.notice.as_ref().map(|n| n.text.as_str())
}

#[test]
fn links_parse_strictly() {
    assert_eq!(
        target("phux://pane/@3"),
        Some(Ok(Reveal::Pane(ResourceId::local(3))))
    );
    assert_eq!(
        target("PHUX://Pane/@3/"),
        Some(Ok(Reveal::Pane(ResourceId::local(3))))
    );
    let satellite = ResourceId::satellite("box:2222", 7);
    assert_eq!(
        target("phux://pane/box:2222/@7"),
        Some(Ok(Reveal::Pane(satellite)))
    );
    assert_eq!(
        target("phux://session/my%20work"),
        Some(Ok(Reveal::Session("my work".to_owned())))
    );
    let tab = Reveal::Tab {
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
    assert_eq!(target(&link), Some(Ok(Reveal::Folder(dir.clone()))));
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
    assert_eq!(desk.connection.reveal, None);
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
    let held = Some(Reveal::Pane(ResourceId::local(9)));
    assert_eq!(desk.connection.reveal, held, "held");
    assert!(desk.notice.is_none());
    desk.connection.topology = Some(crate::desk::topology(Vec::new(), vec![pane(1, "w")]));
    assert!(!desk.follow_reveal());
    assert_eq!(desk.connection.reveal, None);
    assert_eq!(
        notice(&desk),
        Some("No pane @9 on this server, so nothing was revealed.")
    );
    assert!(desk.connection.shown.is_none(), "no fallback pane");

    desk.machines.current = Some("mini".to_owned());
    desk.open_system_link("phux://pane/@1");
    assert_eq!(desk.connection.reveal, None);
    assert!(notice(&desk).is_some_and(|n| n.contains("mini")));
}

#[test]
fn a_session_or_tab_is_found_exactly_or_refused() {
    let mut desk = desk();
    let sessions = vec![session(1, "work", true), session(2, "bare", false)];
    desk.connection.topology = Some(crate::desk::topology(sessions, vec![pane(1, "work")]));

    desk.open_system_link("phux://session/work");
    assert_eq!(
        (desk.connection.reveal.as_ref(), notice(&desk)),
        (None, None)
    );
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
        matches!(
            &desk.connection.reveal,
            Some(Reveal::Tab { since: Some(_), .. })
        ),
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
    assert_eq!(
        (desk.connection.reveal.as_ref(), notice(&desk)),
        (None, None)
    );

    desk.connection.tabs.by_session.clear();
    desk.connection.reveal = Some(Reveal::Tab {
        session: "work".to_owned(),
        index: 1,
        since: Some(Instant::now() - TAB_WAIT),
    });
    assert!(!desk.follow_reveal(), "gives up");
    assert!(notice(&desk).is_some_and(|n| n.contains("didn't load")));
}
