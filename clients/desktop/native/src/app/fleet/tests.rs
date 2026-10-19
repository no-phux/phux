use super::*;
use crate::app::desk::Link;
use crate::app::{AppOptions, Desktop};
use crate::connection::DeterministicDial;
use crate::workspace::{Workspace, WorkspaceOptions};
use gpui::{Entity, TestAppContext};
use phux_client_core::layout::wire::HandleMap;
use phux_client_runtime::control::{SessionDescriptor, Topology};
use phux_protocol::ResourceId;

fn key(byte: u8) -> SessionKey {
    SessionKey::new([byte; 16]).unwrap()
}
fn authority(byte: u8) -> AuthorityId {
    AuthorityId::new([byte; 16]).unwrap()
}
fn session(byte: u8, name: &str) -> SessionDescriptor {
    SessionDescriptor {
        id: u32::from(byte),
        name: name.into(),
        attached_client_count: 0,
        keep_empty: true,
        created_at_unix_secs: 0,
        key: Some(key(byte)),
        lifetime: None,
        revision: None,
        members: 0,
        arrangement: None,
        awaiting_organization: false,
    }
}
fn connection(owner: u8, sessions: Vec<SessionDescriptor>) -> Connection {
    let first = &sessions[0];
    let mut connection =
        Connection::connect_with_key("/isolated-fleet.sock".into(), first.name.clone(), first.key)
            .unwrap();
    connection.status = Status::Attached;
    connection.topology = Some(Topology {
        focused_session: first.id,
        focused_pane: ResourceId::local(0),
        sessions,
        panes: Vec::new(),
        agent_sessions: Vec::new(),
        authority: Some(authority(owner)),
        inbox: None,
        handles: HandleMap::default(),
    });
    connection
}
fn fixture(cx: &mut TestAppContext) -> (Entity<Desk>, Entity<Desktop>) {
    let (_, desk, desktop) = launch(cx);
    (desk, desktop)
}
fn launch(
    cx: &mut TestAppContext,
) -> (
    gpui::WindowHandle<gpui_kit::component::Root>,
    Entity<Desk>,
    Entity<Desktop>,
) {
    let desktop = cx.update(|cx| {
        gpui_kit::init(cx);
        Desktop::install(cx)
    });
    let path = std::env::temp_dir().join(format!(
        "phux-fleet-{}-{}",
        std::process::id(),
        cx.test_function_name().unwrap()
    ));
    let mut prefs = crate::settings::default_display();
    prefs.ghostty_keys = false;
    std::fs::create_dir_all(&path).unwrap();
    let layout = crate::workspace::persist::save_layout("", &[], &prefs, None);
    std::fs::write(
        path.join("layout.json"),
        serde_json::to_vec(&crate::workspace::persist::layout_value(&layout)).unwrap(),
    )
    .unwrap();
    let handle = desktop.update(cx, |desktop, cx| {
        desktop
            .launch(
                AppOptions {
                    socket: path.join("isolated.sock"),
                    session: String::new(),
                    phux: path.join("not-installed"),
                    layout_path: path.join("layout.json"),
                    fresh: true,
                    initial_terminal: None,
                    quick: false,
                    dev_telemetry: false,
                    startup_error: None,
                },
                cx,
            )
            .unwrap()
    });
    let desk = handle
        .update(cx, |root, _, _| {
            root.view().clone().downcast::<Desk>().unwrap()
        })
        .unwrap();
    (handle, desk, desktop)
}
fn install(desk: &mut Desk, connection: Connection) {
    desk.session = connection.session_name().to_owned();
    desk.link = Some(Link {
        connection,
        workspace: Workspace::new(
            desk.session.clone(),
            WorkspaceOptions {
                fresh: false,
                initial_terminal: None,
            },
        ),
    });
}
fn observe(fleet: &mut Fleet, host: Option<&str>, connection: Connection) {
    let host = host.map(str::to_owned);
    let last = work_on(&connection, host.clone());
    fleet.observers.insert(
        host,
        Observer {
            connection: Some(connection),
            last,
            error: None,
            _wake: None,
        },
    );
}

fn draw(cx: &mut gpui::VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

fn shown(cx: &mut gpui::VisualTestContext, selector: &str) -> bool {
    cx.debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
        .is_some()
}

fn click_on(cx: &mut gpui::VisualTestContext, selector: &str) {
    let bounds = cx
        .debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
        .unwrap_or_else(|| panic!("no {selector} on screen"));
    cx.simulate_click(bounds.center(), gpui::Modifiers::none());
    draw(cx);
}

mod catalog;
mod machines;
mod sidebar;
