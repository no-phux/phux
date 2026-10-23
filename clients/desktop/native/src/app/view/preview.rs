//! Server-free captures of the real desktop composition, for visual review.

use std::path::PathBuf;
use std::time::Duration;

use gpui::{App, AppContext as _, WindowHandle, px, size};
use gpui_kit::component::Root;
use phux_client_runtime::Runtime;
use phux_client_runtime::control::{ControlOptions, ControlPlane};
use phux_protocol::{
    PROTOCOL_VERSION, ResourceId,
    caps::{BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, ServerCapabilities},
    ids::{BootstrapId, ClientId, SessionId, StreamId, WindowId},
    wire::{
        frame::{AttachTarget, FrameKind},
        info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo},
    },
};

use crate::app::{AppOptions, Desk, desk::Link};
use crate::connection::Connection;
use crate::workspace::{Workspace, WorkspaceOptions};

const SESSION: &str = "phux";
const TITLES: [&str; 3] = [
    "Refine desktop composition",
    "Review keyboard navigation",
    "shell",
];

fn connection() -> Connection {
    let client = Runtime::embedded(ControlOptions {
        attach: Some(AttachTarget::ByName(SESSION.into())),
        viewport: (120, 40),
        ..ControlOptions::default()
    });
    client.with_control(ControlPlane::connection_opened);
    let _ = client.take_outbound();
    client
        .feed(FrameKind::HelloOk {
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            server_caps: ServerCapabilities::new(),
            server_id: vec![1; 16],
            selected_profile: BootstrapProfile::SynthesizedVtRaw,
            bootstrap_limits: BootstrapLimits::default(),
        })
        .expect("hello");
    let attach_id = client
        .take_outbound()
        .iter()
        .find_map(|bytes| match FrameKind::decode(bytes).expect("decode").0 {
            FrameKind::Attach { attach_id, .. } => Some(attach_id),
            _ => None,
        })
        .expect("attach");
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
        .with_sessions(vec![SessionInfo::new(SessionId::new(1), SESSION)])
        .with_windows(vec![WindowInfo::new(
            WindowId::new(1),
            SessionId::new(1),
            "work",
        )])
        .with_resources(
            (1..=3)
                .map(|id| ResourceInfo::new(ResourceId::local(id), WindowId::new(1), 120, 40))
                .collect(),
        );
    client
        .feed(FrameKind::Attached {
            attach_id,
            snapshot,
            initial_client_id: ClientId::new(1),
        })
        .expect("attached");
    for id in 1..=3 {
        let terminal_id = ResourceId::local(id);
        let stream_id = StreamId::new(u64::from(id)).unwrap();
        let bootstrap_id = BootstrapId::new(u64::from(id)).unwrap();
        let title = TITLES[id as usize - 1];
        let payload = format!(
            "\x1b]2;{title}\x07\x1b[?25l\x1b[2J\x1b[H\r\n  \x1b[1m{title}\x1b[0m\r\n\r\n  One workspace. Less chrome.\r\n\r\n  \x1b[38;2;122;162;247m❯\x1b[0m cargo test --workspace\r\n\r\n  \x1b[32m✓\x1b[0m  Layout, focus and keyboard navigation\r\n  \x1b[32m✓\x1b[0m  Dark and light appearances\r\n  \x1b[32m✓\x1b[0m  Every action stays within reach\r\n\r\n  ~/workspace/phux  \x1b[2mfeat/desktop-design\x1b[0m\r\n  ❯ "
        );
        for frame in [
            FrameKind::BootstrapBegin {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                profile: BootstrapStreamProfile::SynthesizedVtRaw,
                cols: 120,
                rows: 40,
                base_seq: 0,
            },
            FrameKind::BootstrapChunk {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                chunk_seq: 0,
                payload: payload.into_bytes().into(),
            },
            FrameKind::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
                history_cursor: None,
            },
        ] {
            client.feed(frame).expect("bootstrap");
        }
    }
    client
        .feed(FrameKind::AttachReady { attach_id })
        .expect("ready");
    let mut connection = Connection::from_client(client, SESSION.into());
    connection.drain();
    connection
}

fn arrange(desk: &mut Desk, cx: &mut gpui::Context<Desk>) {
    let mut connection = connection();
    let mut workspace = Workspace::new(SESSION.into(), WorkspaceOptions::default());
    workspace.restore(None);
    workspace.settle(&connection);
    for (tab, title) in workspace.tabs.iter_mut().zip(TITLES) {
        tab.title = Some(title.into());
    }
    connection.projects.insert(1, "phux".into());
    desk.link = Some(Link {
        connection,
        workspace,
    });
    desk.session = SESSION.into();
    desk.connect_error = None;
    desk.feed.clear();
    desk.prefs.animations = false;
    desk.prefs.ghostty_keys = false;
    cx.notify();
}

fn capture(root: WindowHandle<Root>, directory: PathBuf, cx: &mut App) {
    cx.spawn(async move |cx| {
        for theme in ["midnight", "latte"] {
            for (name, command, width, sidebar) in [
                ("top", "tabs-top", 1280., true),
                ("side", "tabs-side", 1280., true),
                ("side-collapsed", "tabs-side", 1280., false),
                ("bottom", "tabs-bottom", 1280., true),
                ("side-narrow", "tabs-side", 760., true),
                ("classic-side", "tabs-side", 1280., true),
                ("glassy-side", "tabs-side", 1280., true),
                ("solid-panes", "tabs-side", 1280., true),
                ("main-menu", "main-menu", 1280., true),
                ("actions", "palette", 1280., true),
                ("work", "goto", 1280., true),
                ("notifications", "notifications", 1280., true),
                ("changes", "changes", 1280., true),
                ("settings-appearance", "settings", 1280., true),
                ("settings-interface", "settings-interface", 1280., true),
            ] {
                root.update(cx, |root, window, cx| {
                    let desk = root.view().clone().downcast::<Desk>().unwrap();
                    desk.update(cx, |desk, cx| {
                        desk.close_menu(cx);
                        desk.modal = crate::app::Modal::None;
                        desk.changes.open = false;
                        desk.update_prefs(
                            |prefs| scene_preferences(prefs, name, theme, sidebar),
                            cx,
                        );
                        if command == "settings-interface" {
                            desk.show_settings_page(
                                crate::app::settings_panel::Page::Layout,
                                window,
                                cx,
                            );
                        } else if command != "main-menu" {
                            desk.execute(command, window, cx);
                        }
                    });
                    window.resize(size(px(width), px(800.)));
                    window.refresh();
                })
                .expect("arrange scene");
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                if command == "main-menu" {
                    root.update(cx, |root, window, cx| {
                        root.view()
                            .clone()
                            .downcast::<Desk>()
                            .unwrap()
                            .update(cx, |desk, cx| desk.open_main_menu(window, cx));
                    })
                    .expect("open menu after resize");
                    cx.background_executor()
                        .timer(Duration::from_millis(300))
                        .await;
                }
                let menu = root
                    .update(cx, |root, _, cx| {
                        root.view()
                            .clone()
                            .downcast::<Desk>()
                            .unwrap()
                            .read(cx)
                            .menu_window()
                    })
                    .expect("menu handle");
                let settings = root
                    .update(cx, |root, _, cx| {
                        let desk = root.view().clone().downcast::<Desk>().unwrap();
                        desk.read(cx)
                            .desktop
                            .read(cx)
                            .settings
                            .as_ref()
                            .map(|(handle, _)| *handle)
                    })
                    .expect("settings handle");
                let target = if command == "main-menu" {
                    menu.expect("menu must remain open for its capture")
                } else if command.starts_with("settings") {
                    settings.expect("separate Settings window")
                } else {
                    root.into()
                };
                cx.update_window(target, |_, window, cx| window.draw(cx).clear(cx))
                    .expect("draw scene");
                cx.update_window(target, |_, window, _| {
                    window
                        .render_to_image()
                        .expect("native scene capture")
                        .save(directory.join(format!("{theme}-{name}.png")))
                        .expect("write PNG");
                })
                .expect("capture scene");
                if command.starts_with("settings") {
                    cx.update_window(target, |_, window, _| window.remove_window())
                        .expect("close settings capture");
                }
            }
        }
        cx.update(|cx| cx.quit());
    })
    .detach();
}

fn scene_preferences(
    prefs: &mut crate::settings::DisplayPrefs,
    name: &str,
    theme: &str,
    sidebar: bool,
) {
    prefs.theme_id = theme.into();
    prefs.sidebar_visible = sidebar;
    prefs.skin_id = if name.starts_with("classic") {
        "classic"
    } else {
        "glass"
    }
    .into();
    prefs.window_background = crate::settings::WindowBackground::Blur;
    prefs.pane_glass = name != "solid-panes";
    prefs.terminal_opacity = if name == "glassy-side" { 0.65 } else { 0.92 };
}

pub(crate) fn open(options: AppOptions, directory: PathBuf, cx: &mut App) -> anyhow::Result<()> {
    std::fs::create_dir_all(&directory)?;
    cx.set_reduce_motion(true);
    let root = crate::app::open(options, cx)?;
    root.update(cx, |root, _, cx| {
        root.view()
            .clone()
            .downcast::<Desk>()
            .unwrap()
            .update(cx, arrange);
    })?;
    capture(root, directory, cx);
    Ok(())
}
