//! The window overview: every open native window, its session, host and tabs,
//! with the current one marked, and activation that never opens a window.

use gpui::{AnyWindowHandle, Entity, TestAppContext, VisualTestContext};
use gpui_kit::component::Root;
use phux_client_core::layout::wire::HandleMap;
use phux_client_runtime::control::{PaneDescriptor, SessionDescriptor, Status, Topology};
use phux_protocol::ResourceId;
use phux_protocol::ids::SessionKey;

use super::{Section, Target, WINDOWS, goto_sections, prepare_goto, window_sections};
use crate::app::desk::Link;
use crate::app::{AppOptions, Desk, Desktop, Modal};
use crate::connection::{AgentAttention, AgentInfo, AgentState, Connection, DeterministicDial};
use crate::workspace::layout::{Axis, DeskTab, LayoutNode, Placement};
use crate::workspace::{Workspace, WorkspaceOptions, persist};

struct Fixture {
    desktop: Entity<Desktop>,
    first: (AnyWindowHandle, Entity<Desk>),
    second: (AnyWindowHandle, Entity<Desk>),
}

fn tab(id: &str, title: &str) -> DeskTab {
    DeskTab {
        id: id.into(),
        title: Some(title.into()),
        root: LayoutNode::Leaf {
            placement: Placement {
                id: id.into(),
                terminal_id: None,
                terminal_label: String::new(),
                view_id: None,
            },
        },
        focused_id: id.into(),
        zoomed_id: None,
    }
}

/// Give `desk` a session with `tabs`, the first one shown, without a runtime driver.
fn install(desk: &Entity<Desk>, session: &str, tabs: Vec<DeskTab>, cx: &mut TestAppContext) {
    let socket = desk.read_with(cx, |desk, _| desk.socket.clone());
    let connection = Connection::connect(socket, session.into()).unwrap();
    desk.update(cx, |desk, cx| {
        let mut workspace = Workspace::new(session.into(), WorkspaceOptions::default());
        workspace.active_id = tabs[0].id.clone();
        workspace.tabs = tabs;
        desk.session = session.into();
        desk.link = Some(Link {
            connection,
            workspace,
        });
        cx.notify();
    });
}

/// Two desk windows: `main` with two tabs whose titles match the other
/// window's, and `other` with two tabs.
fn fixture(cx: &mut TestAppContext) -> Fixture {
    let path = std::env::temp_dir().join(format!(
        "phux-window-overview-{}-{}",
        std::process::id(),
        cx.test_function_name().unwrap_or("isolated")
    ));
    let mut prefs = crate::settings::default_display();
    prefs.ghostty_keys = false;
    std::fs::create_dir_all(&path).unwrap();
    let layout = persist::save_layout("", &[], &prefs, None);
    std::fs::write(
        path.join("layout.json"),
        serde_json::to_vec(&persist::layout_value(&layout)).unwrap(),
    )
    .unwrap();
    let handle = cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
        crate::app::open(
            AppOptions {
                socket: path.join("never-connected.sock"),
                session: String::new(),
                phux: path.join("not-installed-phux"),
                layout_path: path.join("layout.json"),
                fresh: false,
                initial_terminal: None,
                quick: false,
                dev_telemetry: false,
                startup_error: None,
                cli_install: None,
            },
            cx,
        )
        .unwrap()
    });
    let desk = (*handle)
        .update(cx, |root, _, cx| {
            root.downcast::<Root>()
                .unwrap()
                .read(cx)
                .view()
                .clone()
                .downcast::<Desk>()
                .unwrap()
        })
        .unwrap();
    let desktop = desk.read_with(cx, |desk, _| desk.desktop.clone());
    desktop.update(cx, |desktop, cx| {
        desktop.open_session("other".into(), None, cx)
    });
    cx.run_until_parked();
    let windows: Vec<(AnyWindowHandle, Entity<Desk>)> = desktop.read_with(cx, |desktop, _| {
        desktop
            .desk_windows()
            .map(|(handle, desk, _)| (handle, desk.clone()))
            .collect()
    });
    assert_eq!(windows.len(), 2, "two desk windows are open");
    install(
        &windows[0].1,
        "main",
        vec![tab("main-a", "zsh"), tab("main-b", "build")],
        cx,
    );
    install(
        &windows[1].1,
        "other",
        vec![tab("other-a", "zsh"), tab("other-b", "logs")],
        cx,
    );
    cx.run_until_parked();
    Fixture {
        desktop,
        first: windows[0].clone(),
        second: windows[1].clone(),
    }
}

fn overview(desk: &Entity<Desk>, cx: &mut TestAppContext) -> Vec<Section> {
    desk.update(cx, |desk, cx| window_sections(desk, cx.entity_id(), cx))
}

fn details(sections: &[Section]) -> Vec<(String, String)> {
    sections
        .iter()
        .flat_map(|section| &section.rows)
        .map(|row| {
            (
                row.title.to_string(),
                row.detail.as_deref().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

fn render(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

fn type_text(cx: &mut VisualTestContext, text: &str) {
    for character in text.chars() {
        cx.update(|window, cx| {
            let key = character.to_string();
            window.dispatch_keystroke(
                gpui::Keystroke {
                    modifiers: gpui::Modifiers::default(),
                    key: key.clone(),
                    key_char: Some(key),
                },
                cx,
            );
        });
        render(cx);
    }
}

fn active_tab(desk: &Entity<Desk>, cx: &VisualTestContext) -> String {
    desk.read_with(cx, |desk, _| {
        desk.link.as_ref().unwrap().workspace.active_id.clone()
    })
}

#[gpui::test]
fn overview_lists_every_window_with_its_tabs_and_marks_one_current(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let fixture = fixture(cx);
    let sections = overview(&fixture.first.1, cx);
    assert_eq!(sections.len(), 1);
    assert_eq!(sections[0].label, Some(WINDOWS));
    assert_eq!(
        details(&sections),
        vec![
            ("Window 1".into(), "Current · main · Local · 2 tabs".into()),
            (
                "zsh".into(),
                "Current · Tab 1 of 2 · Window 1 · main".into()
            ),
            ("build".into(), "Tab 2 of 2 · Window 1 · main".into()),
            ("Window 2".into(), "other · Local · 2 tabs".into()),
            ("zsh".into(), "Shown · Tab 1 of 2 · Window 2 · other".into()),
            ("logs".into(), "Tab 2 of 2 · Window 2 · other".into()),
        ],
        "same-titled tabs stay distinguishable by window and session"
    );
    let rows = &sections[0].rows;
    let current_windows = rows
        .iter()
        .filter(|row| matches!(row.target, Target::Window { tab: None, .. }))
        .filter(|row| row.detail.as_deref().unwrap().starts_with("Current"))
        .count();
    let current_tabs = rows
        .iter()
        .filter(|row| matches!(row.target, Target::Window { tab: Some(_), .. }))
        .filter(|row| row.detail.as_deref().unwrap().starts_with("Current"))
        .count();
    assert_eq!((current_windows, current_tabs), (1, 1));

    // The other window sees itself as current, and its own shown tab.
    let from_second = details(&overview(&fixture.second.1, cx));
    assert_eq!(from_second[0].1, "main · Local · 2 tabs");
    assert_eq!(from_second[1].1, "Shown · Tab 1 of 2 · Window 1 · main");
    assert_eq!(from_second[3].1, "Current · other · Local · 2 tabs");
    assert_eq!(from_second[4].1, "Current · Tab 1 of 2 · Window 2 · other");
}

#[gpui::test]
fn choosing_another_windows_tab_raises_it_without_opening_a_window(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let fixture = fixture(cx);
    let windows = cx.update(|cx| cx.windows().len());
    let (first_handle, first) = fixture.first.clone();
    let (second_handle, second) = fixture.second.clone();
    let cx = &mut VisualTestContext::from_window(first_handle, cx);
    cx.update(|window, cx| {
        window.activate_window();
        first.update(cx, |desk, cx| desk.execute("windows", window, cx));
    });
    render(cx);
    assert_eq!(
        first.read_with(cx, |desk, _| desk.modal.clone()),
        Modal::Windows
    );
    type_text(cx, "logs");
    cx.simulate_keystrokes("enter");
    render(cx);
    assert_eq!(
        first.read_with(cx, |desk, _| desk.modal.clone()),
        Modal::None
    );
    assert_eq!(active_tab(&second, cx), "other-b");
    assert_eq!(
        active_tab(&first, cx),
        "main-a",
        "the first window keeps its tab"
    );
    assert_eq!(cx.update(|_, cx| cx.active_window()), Some(second_handle));
    assert_eq!(cx.update(|_, cx| cx.windows().len()), windows);
    assert_eq!(
        fixture
            .desktop
            .read_with(cx, |desktop, _| desktop.desk_windows().count()),
        2
    );
}

#[gpui::test]
fn choosing_a_tab_in_this_window_selects_it_and_reraising_is_idempotent(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let fixture = fixture(cx);
    let windows = cx.update(|cx| cx.windows().len());
    let (first_handle, first) = fixture.first.clone();
    let (second_handle, _) = fixture.second.clone();
    let cx = &mut VisualTestContext::from_window(first_handle, cx);
    for _ in 0..2 {
        cx.update(|window, cx| first.update(cx, |desk, cx| desk.execute("windows", window, cx)));
        render(cx);
        type_text(cx, "build");
        cx.simulate_keystrokes("enter");
        render(cx);
        assert_eq!(active_tab(&first, cx), "main-b");
        assert_eq!(cx.update(|_, cx| cx.windows().len()), windows);
    }
    // A window row raises that window and leaves its shown tab alone.
    cx.update(|window, cx| first.update(cx, |desk, cx| desk.execute("windows", window, cx)));
    render(cx);
    type_text(cx, "Window 2");
    cx.simulate_keystrokes("enter");
    render(cx);
    assert_eq!(cx.update(|_, cx| cx.active_window()), Some(second_handle));
    assert_eq!(cx.update(|_, cx| cx.windows().len()), windows);
}

#[gpui::test]
fn closing_a_window_renumbers_the_rest_and_a_stale_row_opens_nothing(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let fixture = fixture(cx);
    let (_, first) = fixture.first.clone();
    let (second_handle, _) = fixture.second.clone();
    let stale = overview(&first, cx)[0]
        .rows
        .iter()
        .find(|row| row.title.as_ref() == "logs")
        .unwrap()
        .target
        .clone();
    second_handle
        .update(cx, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    let windows = cx.update(|cx| cx.windows().len());
    assert_eq!(
        details(&overview(&first, cx)),
        vec![
            ("Window 1".into(), "Current · main · Local · 2 tabs".into()),
            (
                "zsh".into(),
                "Current · Tab 1 of 2 · Window 1 · main".into()
            ),
            ("build".into(), "Tab 2 of 2 · Window 1 · main".into()),
        ]
    );
    let Target::Window { handle, desk, tab } = stale else {
        panic!("a window row");
    };
    cx.update(|cx| super::raise_window(handle, &desk, tab.as_deref(), cx));
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| cx.windows().len()), windows);
    assert_eq!(
        fixture
            .desktop
            .read_with(cx, |desktop, _| desktop.desk_windows().count()),
        1
    );
}

mod menu;

fn placement(id: &str, terminal: u32) -> LayoutNode {
    LayoutNode::Leaf {
        placement: Placement {
            id: id.into(),
            terminal_id: Some(ResourceId::local(terminal)),
            terminal_label: String::new(),
            view_id: None,
        },
    }
}

fn split_tab(id: &str, title: &str, panes: [(&str, u32); 2]) -> DeskTab {
    DeskTab {
        id: id.into(),
        title: Some(title.into()),
        root: LayoutNode::Split {
            id: format!("{id}-split"),
            axis: Axis::Row,
            ratio: 0.5,
            first: Box::new(placement(panes[0].0, panes[0].1)),
            second: Box::new(placement(panes[1].0, panes[1].1)),
        },
        focused_id: panes[1].0.into(),
        zoomed_id: None,
    }
}

fn pane(terminal: u32) -> PaneDescriptor {
    PaneDescriptor {
        terminal_id: ResourceId::local(terminal),
        session_id: 1,
        session_name: "api".into(),
        cols: 80,
        rows: 24,
        title: Some(format!("zsh {terminal}")),
        cwd: Some("/repo".into()),
        is_focused: false,
        key: None,
        revision: None,
        name: None,
        activation: None,
        exec: None,
    }
}

fn session(byte: u8, name: &str) -> SessionDescriptor {
    SessionDescriptor {
        id: u32::from(byte),
        name: name.into(),
        attached_client_count: 0,
        keep_empty: true,
        created_at_unix_secs: 0,
        key: SessionKey::new([byte; 16]),
        lifetime: None,
        revision: None,
        members: 0,
        arrangement: None,
        awaiting_organization: false,
    }
}

/// A desk showing session "api" beside "api-old", with two tabs titled
/// "build". Tab "b" is active and shows terminal 4; terminal 2, focused in tab
/// "a", runs a working agent.
fn look_alike_desk(cx: &mut TestAppContext) -> Entity<Desk> {
    let fixture = fixture(cx);
    let desk = fixture.first.1.clone();
    let socket = desk.read_with(cx, |desk, _| desk.socket.clone());
    let sessions = vec![session(1, "api"), session(2, "api-old")];
    let mut connection =
        Connection::connect_with_key(socket, "api".into(), sessions[0].key).unwrap();
    connection.status = Status::Attached;
    connection.topology = Some(Topology {
        focused_session: 1,
        focused_pane: ResourceId::local(4),
        sessions,
        panes: (1..=4).map(pane).collect(),
        agent_sessions: Vec::new(),
        authority: None,
        inbox: None,
        handles: HandleMap::default(),
    });
    connection.agents.insert(
        ResourceId::local(2),
        AgentInfo {
            name: "claude".into(),
            kind: Some("claude".into()),
            state: AgentState::Working,
            attention: AgentAttention::Normal,
            changed_at_ms: 0,
        },
    );
    desk.update(cx, |desk, _| {
        let mut workspace = Workspace::new("api".into(), WorkspaceOptions::default());
        workspace.tabs = vec![
            split_tab("a", "build", [("a1", 1), ("a2", 2)]),
            split_tab("b", "build", [("b1", 3), ("b2", 4)]),
        ];
        workspace.active_id = "b".into();
        desk.session = "api".into();
        desk.link = Some(Link {
            connection,
            workspace,
        });
    });
    desk
}

type Marked = Vec<(&'static str, String, String)>;

/// (category, title, detail) for every row whose detail carries the Current
/// mark. A typed search ranks rows into one unlabeled section, so the row's
/// category names its kind rather than the section label.
fn current_rows(sections: &[Section]) -> Marked {
    sections
        .iter()
        .flat_map(|section| &section.rows)
        .map(|row| {
            (
                row.category,
                row.title.to_string(),
                row.detail.as_deref().unwrap_or_default().to_owned(),
            )
        })
        .filter(|(_, _, detail)| detail.starts_with("Current · "))
        .collect()
}

/// Go To names the work, terminal and tab this window shows among look-alikes:
/// two sessions whose names share a prefix and two tabs with the same title.
#[gpui::test]
fn goto_marks_the_work_terminal_and_tab_this_window_shows(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let desk = look_alike_desk(cx);
    let sections = desk.read_with(cx, |desk, _| goto_sections(desk));
    assert_eq!(
        current_rows(&sections),
        vec![
            ("Work", "api".into(), "Current · Local · 4 terminals".into()),
            ("Terminals", "zsh 4".into(), "Current · api · /repo".into()),
            ("Tabs", "build".into(), "Current · Tab 2 · 2 panes".into()),
        ],
        "exactly one work, terminal and tab is marked"
    );
}

/// Types "current" into the palette's cached search: the marked rows it ranks,
/// sorted by category, and how many rows it ranks in all.
fn search_current(desk: &Entity<Desk>, cx: &mut TestAppContext) -> (Marked, usize) {
    desk.update(cx, |desk, _| {
        prepare_goto(desk, "current");
        let ranked = desk.goto_search.ranked();
        let rows = ranked.iter().map(|section| section.rows.len()).sum();
        let mut marked = current_rows(ranked);
        marked.sort();
        (marked, rows)
    })
}

/// The palette renders from the cached search, and a tab switch runs no drain:
/// the Current marks still follow the active tab, and typing "current" finds
/// exactly the marked rows, an agent row included.
#[gpui::test]
fn goto_current_follows_a_tab_switch_through_the_search_cache(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let desk = look_alike_desk(cx);
    let (before, rows) = search_current(&desk, cx);
    assert_eq!(
        before,
        vec![
            ("Tabs", "build".into(), "Current · Tab 2 · 2 panes".into()),
            ("Terminals", "zsh 4".into(), "Current · api · /repo".into()),
            ("Work", "api".into(), "Current · Local · 4 terminals".into()),
        ]
    );
    assert_eq!(rows, before.len(), "\"current\" finds only the marked rows");

    desk.update(cx, |desk, _| desk.select_tab("a"));
    let (after, rows) = search_current(&desk, cx);
    assert_eq!(
        after,
        vec![
            (
                "Agents",
                "claude — zsh 2".into(),
                "Current · working · api · /repo".into()
            ),
            ("Tabs", "build".into(), "Current · Tab 1 · 2 panes".into()),
            ("Work", "api".into(), "Current · Local · 4 terminals".into()),
        ],
        "the marks move to tab a and its focused agent terminal"
    );
    assert_eq!(rows, after.len(), "\"current\" finds only the marked rows");
}
