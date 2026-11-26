use super::*;
use crate::activity::Mark;
use crate::theme::contrast;
use phux_config_core::host_color::PALETTE;
use phux_design::tokens::IDENTITY;
use phux_protocol::ids::ResourceId;

fn tokens(dark: bool, theme: Option<&'static crate::theme::Theme>) -> std::rc::Rc<Tokens> {
    super::super::tokens_for(dark, theme)
}

#[test]
fn host_marks_read_in_every_theme() {
    let mut low = Vec::new();
    for theme in std::iter::once(None).chain(crate::theme::all().map(Some)) {
        for dark in [true, false] {
            let ink = tokens(dark, theme);
            let t: &Tokens = ink.as_ref();
            let sidebar = t.roles.layer.sidebar.paint();
            let ground = t.roles.layer.ground.paint();
            for (i, (id, _)) in PALETTE.iter().enumerate() {
                let host = host_color::swatch(i, sidebar);
                let name = theme.map_or("system", |t| t.id);
                // Eased for the sidebar; the focused card's rim sits on
                // the window ground, a shade away.
                let mark = f64::from(contrast(host, sidebar));
                if mark < IDENTITY.host_bar
                    || f64::from(contrast(host, ground)) < IDENTITY.host_bar - 0.1
                {
                    low.push(format!("{id} on {name}/{dark}: {mark:.2}"));
                }
            }
        }
    }
    assert!(low.is_empty(), "{low:#?}");
}

#[test]
fn a_mark_never_borrows_the_accent() {
    let ink = tokens(true, None);
    let t: &Tokens = ink.as_ref();
    for mark in [
        Mark::Blocked,
        Mark::Done,
        Mark::Failed,
        Mark::Bell,
        Mark::Working,
        Mark::Running,
        Mark::Background,
        Mark::Succeeded,
        Mark::Idle,
        Mark::None,
    ] {
        // Colour means state: a terminal's mark never borrows the accent.
        assert_ne!(face(mark, t).ink, t.roles.accent.accent.paint(), "{mark:?}");
    }
}

#[test]
fn the_histogram_counts_only_what_asks_for_you() {
    let mut counts = Counts::default();
    for mark in [
        Mark::Blocked,
        Mark::Bell,
        Mark::Failed,
        Mark::Done,
        Mark::Done,
        Mark::Working,
        Mark::Idle,
    ] {
        counts.add(mark);
    }
    assert_eq!(
        counts,
        Counts {
            needs_you: 2,
            failed: 1,
            done: 2
        }
    );
    assert_eq!(counts.waiting(), 5);
}

/// A window with one session of four terminals in different states.
fn listed(dark: bool) -> rune::Runtime<Desk> {
    listed_with(dark, &[])
}

/// [`listed`], each `(child, parent)` pane started from its parent.
fn listed_with(dark: bool, parents: &[(u32, u32)]) -> rune::Runtime<Desk> {
    let mut desk = crate::test_support::settings_window();
    desk.settings_open = false;
    let mut rt = crate::test_support::runtime(
        desk,
        crate::ui::view,
        (1100.0, 720.0),
        rune::Env::new(dark, 1.0),
    );
    rt.frame(0.0);
    let session = phux_client_runtime::control::SessionDescriptor {
        id: 1,
        name: "work".to_owned(),
        attached_client_count: 1,
        keep_empty: true,
        created_at_unix_secs: 0,
        key: SessionKey::new([1; 16]),
        touched: None,
        lifetime: None,
        revision: None,
        members: 0,
        arrangement: None,
        awaiting_organization: false,
        project: None,
        project_link: None,
    };
    let panes = (1..=4)
        .map(|id| phux_client_runtime::control::PaneDescriptor {
            terminal_id: ResourceId::local(id),
            session_id: 1,
            session_name: "work".to_owned(),
            cols: 80,
            rows: 24,
            title: Some(format!("pane {id}")),
            cwd: None,
            is_focused: id == 1,
            key: None,
            revision: None,
            name: Some(format!("pane {id}")),
            activation: None,
            exec: None,
            foreground: None,
            icon: None,
            unseen: None,
            parent: (parents.iter())
                .find(|(child, _)| *child == id)
                .map(|(_, parent)| ResourceId::local(*parent)),
        })
        .collect();
    let mut topology = crate::desk::topology(vec![session], panes);
    crate::test_support::server::terminal_resources(&mut topology);
    rt.update(move |d, _| {
        d.connection.topology = Some(topology);
        for (id, state) in [(1, "working"), (2, "blocked"), (3, "done"), (4, "idle")] {
            let event = crate::test_support::server::agent(id, "claude", state);
            d.activities.apply(&event, &[]);
        }
    });
    for t in [0.0, 1.0] {
        rt.frame(t);
    }
    rt
}

/// Whether an element keyed `key` takes focus. A terminal's sidebar row
/// and its card share the key; the row is the one that does.
pub(in crate::ui) fn focusable(rt: &rune::Runtime<Desk>, key: &str) -> bool {
    (rt.elements().iter()).any(|e| e.key.as_deref() == Some(key) && e.focusable)
}

#[test]
fn rows_are_one_focusable_source_list_with_their_words() {
    let rt = listed(true);
    let list = crate::ui::element(&rt, "sidebar-list");
    assert_eq!(list.role, "list");
    for id in 1..=4 {
        assert!(
            focusable(&rt, &format!("pane-{}", ResourceId::local(id))),
            "pane {id}"
        );
    }
    let header = crate::ui::element(&rt, "select-session-1");
    assert!(header.focusable, "the session header is focusable");
    // What asks for you shows its word, from the vocabulary; every badge
    // is named by its word where a screen reader meets it, and the agent
    // and word ride in the row's description.
    let said = |words: &str| {
        rt.elements()
            .iter()
            .any(|e| e.text.as_deref() == Some(words) || e.label.as_deref() == Some(words))
    };
    let shows = |words: &str| (rt.elements().iter()).any(|e| e.text.as_deref() == Some(words));
    assert!(shows("needs you"), "what needs you says so");
    assert!(
        !shows("claude · working"),
        "one line: no second line of words"
    );
    assert!(
        said("needs you") && said("done") && said("working"),
        "badges are named"
    );
}

#[test]
fn twins_with_an_agent_still_name_their_pane() {
    let ink = tokens(true, None);
    let id = phux_protocol::ids::ResourceId::local(90);
    let detail = |place: Option<&str>| {
        super::Row {
            id: &id,
            target: &id,
            session: "rune-3",
            name: "Shopify inventory sync to Meta".to_owned(),
            place: place.map(str::to_owned),
            key: None,
            agent: Some("claude"),
            identity: crate::addressable::Identity {
                kind: phux_protocol::ids::ResourceKind::Terminal,
                tool: Some("claude".to_owned()),
                agent: true,
                glyph: None,
            },
            mark: crate::activity::Mark::Done,
            panes: 1,
            selected: false,
            members: Vec::new(),
            host: ("mini", 0),
            color: None,
            state: Forced::Rest,
        }
        .detail(&ink)
        .map(|(words, _)| words)
    };
    assert_eq!(detail(Some("@90")).as_deref(), Some("claude · done · @90"));
    assert_eq!(detail(None).as_deref(), Some("claude · done"));
}

/// The key the window's focus sits on, if any.
fn focused(rt: &mut rune::Runtime<Desk>) -> Option<String> {
    let mut key = None;
    rt.update(|_, tx| key = tx.focused().map(str::to_owned));
    key
}

#[test]
fn a_clicked_row_hands_the_keyboard_back_to_the_terminal() {
    let mut rt = listed(true);
    for row in [
        format!("pane-{}", ResourceId::local(2)),
        "select-session-1".to_owned(),
    ] {
        let (x, y) = rt.find(&row).expect("sidebar row");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.0);
        // Arrows, Space and Return go to the shown terminal, not the list.
        assert_eq!(focused(&mut rt), None, "{row} kept focus after a click");
    }
    // Keys still walk the list: Tab lands on it, Down moves along it.
    let in_list = |key: &Option<String>| {
        key.as_deref()
            .is_some_and(|k| k.starts_with("pane-") || k.starts_with("select-session"))
    };
    for _ in 0..20 {
        if in_list(&focused(&mut rt)) {
            break;
        }
        rt.key(rune::KeyEvent {
            key: rune::Key::Tab,
            mods: rune::Mods::default(),
        });
        rt.frame(0.0);
    }
    let landed = focused(&mut rt);
    assert!(in_list(&landed), "Tab reaches the list: {landed:?}");
    rt.key(rune::KeyEvent {
        key: rune::Key::Down,
        mods: rune::Mods::default(),
    });
    rt.frame(0.0);
    let next = focused(&mut rt);
    assert!(
        in_list(&next) && next != landed,
        "Down walks the list: {landed:?} -> {next:?}"
    );
}

#[test]
fn a_clicked_new_button_hands_the_keyboard_back_to_the_terminal() {
    let mut rt = listed(true);
    for key in ["new-terminal", "new-session"] {
        let (x, y) = rt.find(key).expect("footer button");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.0);
        // Space would press it again: a second session or terminal.
        assert_eq!(focused(&mut rt), None, "{key} kept focus after a click");
    }
}

#[test]
fn a_tab_is_one_row_however_many_panes_it_splits_into() {
    let mut rt = listed(true);
    let tab = |n: u8, name: &str, ids: &[u32]| crate::desk::Tab {
        id: [n; 16],
        name: name.to_owned(),
        resources: ids.iter().copied().map(ResourceId::local).collect(),
        active: false,
        locked: false,
    };
    let tabs = vec![tab(1, "1", &[4, 1]), tab(2, "Deploy", &[2])];
    rt.update(move |d, _| {
        d.connection.tabs.by_session.insert(1, tabs);
    });
    rt.frame(2.0);
    let at = |id: u32| rt.find(&format!("pane-{}", ResourceId::local(id)));
    // The split is named after its first agent's pane, and pane 1 rides
    // in it; pane 3, in no tab, keeps a row of its own.
    assert!(at(4).is_some() && at(1).is_none(), "a split tab is one row");
    let said = |words: &str| (rt.elements().iter()).any(|e| e.text.as_deref() == Some(words));
    assert!(said("Deploy"), "a tab a person named keeps its name");
    assert!(said("2"), "a split says how many panes it holds");
    let order = [4, 2, 3].map(|id| at(id).expect("a row").1);
    assert!(
        order.windows(2).all(|w| w[0] < w[1]),
        "tabs in order, then the rest: {order:?}"
    );
}

#[test]
fn ancestry_and_cycles_never_invent_tab_membership() {
    let pane = |id: u32, parent: Option<u32>| crate::addressable::Addressable {
        id: ResourceId::local(id),
        label: String::new(),
        session_id: 1,
        session: "work".to_owned(),
        kind: phux_protocol::ids::ResourceKind::Terminal,
        mirror: "",
        parent: parent.map(ResourceId::local),
        program: None,
    };
    // 3 is 2's child, so it sits with 2 under 1; 4's parent closed; 5 and
    // 6 name each other.
    let panes = [
        pane(1, None),
        pane(2, Some(1)),
        pane(3, Some(2)),
        pane(4, Some(9)),
        pane(5, Some(6)),
        pane(6, Some(5)),
    ];
    let rows: Vec<_> = panes.iter().collect();
    let items = items::items(&[], &rows);
    assert_eq!(items.len(), 6);
    assert!(items.iter().all(|item| item.members.len() == 1));
}

#[test]
fn child_rows_are_naturally_visible_without_a_gutter_or_fake_stack() {
    for dark in [true, false] {
        let rt = listed_with(dark, &[(3, 2), (4, 2)]);
        for id in 1..=4 {
            assert!(focusable(&rt, &format!("pane-{}", ResourceId::local(id))));
        }
        assert!(rt.find("fold-@2").is_none());
        assert!(
            rt.find("identity-layer-1").is_none(),
            "ancestry is not a split"
        );
        assert!(
            rt.elements()
                .iter()
                .any(|e| e.path.contains("pane-@2/") && e.label.as_deref() == Some("needs you"))
        );
    }
}

#[test]
fn satellite_singles_share_useful_detail_without_stacking_or_inventing_unknown_targets() {
    let mut rt = listed(true);
    let remote = ResourceId::satellite("box", 7);
    let unknown = ResourceId::satellite("box", 9);
    let (resource, unsupported) = (remote.clone(), unknown.clone());
    rt.update(move |d, _| {
        let topology = d.connection.topology.as_mut().unwrap();
        let mut pane = topology.panes[0].clone();
        pane.terminal_id = resource.clone();
        pane.session_id = 0;
        pane.session_name = "box".to_owned();
        pane.name = Some("Remote build".to_owned());
        pane.cwd = Some("/repo/component".to_owned());
        topology.panes.push(pane);
        let mut record = topology.resources[0].clone();
        record.id = resource.clone();
        record.session_id = 0;
        topology.resources.push(record.clone());
        record.id = unsupported.clone();
        record.kind = phux_protocol::ids::ResourceKind::Unknown { tag: 90 };
        topology.resources.push(record);
        topology
            .satellite_sessions
            .push(phux_client_runtime::control::SatelliteSession {
                host: "box".to_owned(),
                name: "work".to_owned(),
                members: vec![resource, unsupported],
            });
    });
    rt.frame(3.0);
    let key = format!("pane-{remote}");
    assert!(focusable(&rt, &key));
    assert!(
        crate::ui::element(&rt, &key)
            .help
            .as_deref()
            .unwrap()
            .contains("component")
    );
    assert!(!focusable(&rt, &format!("pane-{unknown}")));
    assert!(
        rt.find("identity-layer-1").is_none(),
        "satellite membership is not a split"
    );
}

#[test]
fn a_split_count_cannot_be_truncated_by_long_resource_detail() {
    let mut rt = listed(true);
    rt.update(|d, _| {
        d.connection.topology.as_mut().unwrap().panes[0].cwd =
            Some("/repo/a-long-declared-folder-that-needs-to-be-elided".to_owned());
        d.connection.tabs.by_session.insert(
            1,
            vec![crate::desk::Tab {
                id: [1; 16],
                name: "Build".to_owned(),
                resources: [1, 4].map(ResourceId::local).to_vec(),
                active: true,
                locked: false,
            }],
        );
    });
    rt.frame(3.0);
    let count = rt
        .elements()
        .into_iter()
        .find(|e| e.path.contains("pane-@1/") && e.text.as_deref() == Some("2"))
        .expect("the exact split count is separate from elided detail");
    assert!(count.visible[2] > 0.0);
    assert!((count.visible[2] - count.rect[2]).abs() < 0.01);
}
