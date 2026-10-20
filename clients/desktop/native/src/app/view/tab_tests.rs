use std::path::PathBuf;

use super::tabs::{RAIL_WIDTH, tab_height};
use super::titlebar::controls_fit;
use super::*;
use crate::app::desk::Modal;
use crate::app::ui::list::row_radius;
use crate::app::{AppOptions, desk::Link};
use crate::connection::{Connection, DeterministicDial};
use crate::workspace::layout::{Axis, DeskTab, LayoutNode, Placement};
use crate::workspace::{Workspace, WorkspaceOptions, persist};
use gpui::{AnyWindowHandle, Entity, Modifiers, TestAppContext, VisualTestContext};
use gpui::{MouseButton, point};
use gpui_kit::component::Root;

/// The default skin's titlebar, which these layouts paint.
const TITLEBAR: f32 = crate::settings::skin::GLASS.chrome.titlebar;

fn fixture(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<Desk>, PathBuf) {
    let path = std::env::temp_dir().join(format!(
        "phux-tab-layout-{}-{}",
        std::process::id(),
        cx.test_function_name().unwrap_or("isolated")
    ));
    let mut prefs = crate::settings::default_display();
    prefs.ghostty_keys = false;
    prefs.sidebar_visible = false;
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
    let connection =
        Connection::connect(path.join("never-connected.sock"), "fixture".into()).unwrap();
    desk.update(cx, |desk, cx| {
        desk.link = Some(Link {
            connection,
            workspace: Workspace::new("fixture".into(), WorkspaceOptions::default()),
        });
        cx.notify();
    });
    (*handle, desk, path)
}

fn tabs(count: usize) -> Vec<DeskTab> {
    (0..count)
        .map(|index| {
            let id = format!("fixture-{}", char::from(b'a' + index as u8));
            DeskTab {
                id: id.clone(),
                title: Some(format!("Workspace {}", index + 1)),
                root: LayoutNode::Leaf {
                    placement: Placement {
                        id: id.clone(),
                        terminal_id: None,
                        terminal_label: String::new(),
                        view_id: None,
                    },
                },
                focused_id: id,
                zoomed_id: None,
            }
        })
        .collect()
}

fn draw(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.bounds_changed(cx);
            window.draw(cx).clear(cx);
        });
    }
}

fn run(desk: &Entity<Desk>, command: &str, cx: &mut VisualTestContext) {
    cx.update(|window, cx| desk.update(cx, |desk, cx| desk.execute(command, window, cx)));
    draw(cx);
}

fn ids(desk: &Entity<Desk>, cx: &VisualTestContext) -> Vec<String> {
    desk.read_with(cx, |desk, _| {
        desk.link
            .as_ref()
            .unwrap()
            .workspace
            .tabs
            .iter()
            .map(|tab| tab.id.clone())
            .collect()
    })
}

fn resource_key(byte: u8) -> phux_protocol::ids::ResourceKey {
    phux_protocol::ids::ResourceKey::new([byte; 16]).unwrap()
}

/// Three tab-backed terminals and a fourth that is only in the navigator.
fn selectable_work(desk: &mut Desk, cx: &mut gpui::Context<Desk>) {
    use phux_client_runtime::control::{PaneDescriptor, SessionDescriptor, Status, Topology};
    use phux_protocol::ResourceId;

    desk.session = "fixture".into();
    let link = desk.link.as_mut().unwrap();
    link.workspace.tabs = tabs(3);
    link.workspace.active_id = "fixture-c".into();
    for (index, tab) in link.workspace.tabs.iter_mut().enumerate() {
        let LayoutNode::Leaf { placement } = &mut tab.root else {
            unreachable!()
        };
        placement.terminal_id = Some(ResourceId::local(index as u32 + 1));
    }
    link.connection.status = Status::Attached;
    link.connection.topology = Some(Topology {
        sessions: vec![SessionDescriptor {
            id: 1,
            name: "fixture".into(),
            attached_client_count: 1,
            keep_empty: true,
            created_at_unix_secs: 0,
            key: None,
            lifetime: None,
            revision: None,
            members: 4,
            arrangement: None,
            awaiting_organization: false,
        }],
        panes: (1..=4)
            .map(|index| PaneDescriptor {
                terminal_id: ResourceId::local(index),
                session_id: 1,
                session_name: "fixture".into(),
                cols: 80,
                rows: 24,
                title: None,
                cwd: None,
                is_focused: index == 3,
                key: Some(resource_key(index as u8)),
                revision: None,
                name: None,
                activation: None,
                exec: None,
            })
            .collect(),
        agent_sessions: Vec::new(),
        focused_session: 1,
        focused_pane: ResourceId::local(3),
        authority: None,
        inbox: None,
        handles: Default::default(),
    });
    desk.organization.selected.clear();
    cx.notify();
}

mod chrome;
mod closing;
mod geometry;
mod placement;
mod selection;
