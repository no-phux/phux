//! Whole-window headless benchmark: the real `Desk` root (titlebar, sidebar
//! with sessions and agent rows, tab strip, status bar, and one live terminal
//! pane) drawn through GPUI's headless renderer.
//!
//! No daemon. [`DeterministicDial`] routes the window's own dial to an
//! embedded runtime, and this file plays the server by feeding that client
//! the handshake, a multi-session snapshot, and one terminal bootstrap. Each
//! tick is a full `Window::draw`, so the timing covers layout, prepaint and
//! paint of every element the window shows, not only the terminal painter.

use std::sync::Arc;

use gpui::{Entity, HeadlessAppContext, Modifiers, MouseMoveEvent, point, px};
use phux_client_runtime::Client;
use phux_desktop::app::{AppOptions, Desk};
use phux_desktop::connection::{AgentAttention, AgentInfo, AgentState, DeterministicDial};
use phux_protocol::{
    PROTOCOL_VERSION, ResourceId,
    caps::{
        BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, ServerCapabilities,
        ServerFeature, ServerFeatureSet,
    },
    ids::{BootstrapId, ClientId, SessionId, StreamId, WindowId},
    wire::{
        frame::FrameKind,
        info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo},
    },
};

const FRAMES: usize = 128;
const COLS: u16 = 120;
const ROWS: u16 = 40;
/// Sessions in the sidebar, each with this many terminals.
const SESSIONS: &[&str] = &["bench", "api", "web", "infra", "notes"];
const TERMINALS_PER_SESSION: u32 = 3;
const FILL: &str = "window workload window workload window workload window workload window ";

fn resource(session: usize, index: u32) -> ResourceId {
    ResourceId::local(session as u32 * 16 + index + 1)
}

/// The streamed terminal: the first terminal of the home session.
fn live() -> ResourceId {
    resource(0, 0)
}

fn snapshot() -> SessionSnapshot {
    let mut sessions = Vec::new();
    let mut windows = Vec::new();
    let mut resources = Vec::new();
    for (index, name) in SESSIONS.iter().enumerate() {
        let session = SessionId::new(index as u32 + 1);
        let window = WindowId::new(index as u32 + 1);
        sessions.push(SessionInfo::new(session, *name));
        windows.push(
            WindowInfo::new(window, session, "shell")
                .with_active_resource(Some(resource(index, 0))),
        );
        for terminal in 0..TERMINALS_PER_SESSION {
            resources.push(
                ResourceInfo::new(resource(index, terminal), window, COLS, ROWS)
                    .with_title(Some(format!("{name} {terminal}: cargo build")))
                    .with_cwd(Some(format!("/work/{name}"))),
            );
        }
    }
    SessionSnapshot::new(SessionId::new(1), WindowId::new(1), live())
        .with_sessions(sessions)
        .with_windows(windows)
        .with_resources(resources)
}

/// Play the server: handshake, attach the home session, bootstrap the live
/// terminal. Mirrors `publish_at`, but on the window's own client.
fn serve(client: &Client) {
    let _ = client.take_outbound();
    client
        .feed(FrameKind::HelloOk {
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            server_caps: ServerCapabilities::new()
                .with_features(ServerFeatureSet::with(&[ServerFeature::AcknowledgedInput])),
            server_id: vec![2; 16],
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
        .expect("attach request");
    let payload = (0..ROWS)
        .map(|row| format!("{row:02} {FILL}"))
        .collect::<Vec<_>>()
        .join("\r\n");
    client
        .feed(FrameKind::Attached {
            attach_id,
            snapshot: snapshot(),
            initial_client_id: ClientId::new(1),
        })
        .expect("attached");
    // Attach readiness waits on every terminal of the home session. Each gets
    // its own stream; the live one is stream 1.
    for terminal in 0..TERMINALS_PER_SESSION {
        let terminal_id = resource(0, terminal);
        let stream_id = StreamId::new(u64::from(terminal) + 1).unwrap();
        let bootstrap_id = BootstrapId::new(u64::from(terminal) + 1).unwrap();
        for frame in [
            FrameKind::BootstrapBegin {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                profile: BootstrapStreamProfile::SynthesizedVtRaw,
                cols: COLS,
                rows: ROWS,
                base_seq: 0,
            },
            FrameKind::BootstrapChunk {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                chunk_seq: 0,
                payload: payload.clone().into_bytes().into(),
            },
            FrameKind::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
                history_cursor: None,
            },
        ] {
            client.feed(frame).expect("scripted bootstrap");
        }
    }
    client
        .feed(FrameKind::AttachReady { attach_id })
        .expect("attach ready");
    // An embedded feed queues events without waking; a transport driver
    // wakes the listener after each read, and so does this scripted server.
    client.wake();
    let _ = client.take_outbound();
}

fn agent(index: usize, tick: usize) -> AgentInfo {
    const STATES: [AgentState; 4] = [
        AgentState::Blocked,
        AgentState::Working,
        AgentState::Done,
        AgentState::Idle,
    ];
    AgentInfo {
        name: format!("agent-{index}"),
        kind: Some("claude".to_owned()),
        state: STATES[(index + tick) % STATES.len()],
        attention: if (index + tick).is_multiple_of(3) {
            AgentAttention::High
        } else {
            AgentAttention::Normal
        },
        changed_at_ms: 1_700_000_000_000 + tick as u64,
    }
}

/// Every terminal outside the live one carries an agent badge; the blocked
/// and finished ones also carry attention marks.
fn set_agents(desk: &mut Desk, tick: usize) {
    let Some(link) = desk.link.as_mut() else {
        return;
    };
    for session in 0..SESSIONS.len() {
        for terminal in 0..TERMINALS_PER_SESSION {
            let id = resource(session, terminal);
            if id != live() {
                let index = session * TERMINALS_PER_SESSION as usize + terminal as usize;
                link.connection.agents.insert(id, agent(index, tick));
            }
        }
    }
}

fn retained_matches_scratch(
    cx: &mut HeadlessAppContext,
    window: gpui::AnyWindowHandle,
    name: &str,
) {
    let retained = cx
        .capture_screenshot(window)
        .expect("retained window pixels");
    super::hub::assert_retained_matches_scratch(cx, window, &retained, name);
}

fn settle(cx: &mut HeadlessAppContext, window: gpui::AnyWindowHandle) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| window.draw(cx).clear(cx))
            .expect("settle draw");
    }
}

pub(super) fn headless_perf_window(bench: fn(&str, usize, bool, &mut dyn FnMut(usize))) {
    let _dial = DeterministicDial::arm();
    let platform = gpui_platform::current_platform(true);
    let mut cx = HeadlessAppContext::with_platform(
        platform.text_system(),
        Arc::new(phux_desktop::assets::Assets),
        gpui_platform::current_headless_renderer,
    );
    let root = std::env::temp_dir().join(format!("phux-window-perf-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let mut prefs = phux_desktop::settings::default_display();
    prefs.ghostty_keys = false;
    prefs.sidebar_visible = true;
    let layout = phux_desktop::workspace::persist::save_layout("", &[], &prefs, None);
    std::fs::write(
        root.join("layout.json"),
        serde_json::to_vec(&phux_desktop::workspace::persist::layout_value(&layout)).unwrap(),
    )
    .unwrap();
    let handle = cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
        phux_desktop::app::open(
            AppOptions {
                socket: root.join("no-server.sock"),
                session: SESSIONS[0].to_owned(),
                phux: root.join("not-installed"),
                layout_path: root.join("layout.json"),
                fresh: false,
                initial_terminal: None,
                quick: false,
                dev_telemetry: false,
                startup_error: None,
            },
            cx,
        )
        .expect("desk window")
    });
    let window: gpui::AnyWindowHandle = handle.into();
    let desk: Entity<Desk> = handle
        .update(&mut cx, |root, _, _| {
            root.view().clone().downcast::<Desk>().unwrap()
        })
        .unwrap();
    let client = desk.read_with(&cx, |desk, _| {
        Arc::clone(&desk.link.as_ref().expect("embedded link").connection.client)
    });
    serve(&client);
    cx.update(|cx| {
        desk.update(cx, |desk, cx| {
            set_agents(desk, 0);
            cx.notify();
        })
    });
    settle(&mut cx, window);
    let before = cx
        .capture_screenshot(window)
        .expect("initial window pixels");

    let draw = |cx: &mut HeadlessAppContext| {
        cx.update_window(window, |_, window, cx| window.draw(cx).clear(cx))
            .expect("draw");
    };

    bench("window_idle", FRAMES, false, &mut |_| {
        cx.update_window(window, |_, window, _| window.refresh())
            .expect("refresh");
        draw(&mut cx);
    });

    bench("window_agents", FRAMES, false, &mut |index| {
        cx.update(|cx| {
            desk.update(cx, |desk, cx| {
                set_agents(desk, index + 1);
                cx.notify();
            })
        });
        draw(&mut cx);
    });

    // The terminal was drawn from its last frame through every agent tick.
    retained_matches_scratch(&mut cx, window, "window_agents");

    let size = cx
        .update_window(window, |_, window, _| window.viewport_size())
        .expect("viewport");
    bench("window_hover", FRAMES, false, &mut |index| {
        // Sweep the sidebar rows, then the tab strip, a row-height step at a time.
        let rows = 24;
        let step = index % (rows * 2);
        let position = if step < rows {
            point(px(120.), px(56.) + px(24.) * step as f32)
        } else {
            point(
                px(320.) + (size.width - px(360.)) * ((step - rows) as f32 / rows as f32),
                px(20.),
            )
        };
        cx.update_window(window, |_, window, cx| {
            window.dispatch_event(
                gpui::PlatformInput::MouseMove(MouseMoveEvent {
                    position,
                    pressed_button: None,
                    modifiers: Modifiers::default(),
                }),
                cx,
            );
        })
        .expect("hover");
        draw(&mut cx);
    });

    let mut seq = 0_u64;
    bench("window_stream", FRAMES, true, &mut |index| {
        seq += 1;
        client
            .feed(FrameKind::ResourceOutput {
                terminal_id: live(),
                stream_id: StreamId::new(1).unwrap(),
                bootstrap_id: BootstrapId::new(1).unwrap(),
                seq,
                bytes: format!("\x1b[{ROWS};1H{index:04} {FILL}\r\n")
                    .into_bytes()
                    .into(),
            })
            .expect("stream output");
        client.wake();
        // The window's wake task drains the batch and notifies the pane.
        cx.run_until_parked();
        draw(&mut cx);
    });

    // Output repainted the terminal alone; the desk chrome was retained.
    retained_matches_scratch(&mut cx, window, "window_stream");

    tab_peek_pixels(&mut cx, window, &desk, &client);

    let after = cx.capture_screenshot(window).expect("final window pixels");
    if let Some(dir) = std::env::var_os("PHUX_DESKTOP_UI_EVIDENCE") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        before.save(dir.join("window-perf-before.png")).unwrap();
        after.save(dir.join("window-perf-after.png")).unwrap();
        // The same work under each tab placement and sidebar grouping.
        for command in [
            "tabs-side",
            "group-machines",
            "group-none",
            "tabs-bottom",
            "tabs-top",
            "main-menu",
        ] {
            cx.update_window(window, |_, window, cx| {
                desk.update(cx, |desk, cx| desk.execute(command, window, cx))
            })
            .expect("view command");
            settle(&mut cx, window);
            cx.capture_screenshot(window)
                .expect("view pixels")
                .save(dir.join(format!("window-{command}.png")))
                .unwrap();
        }
        let menu = desk.read_with(&cx, |desk, _| desk.menu_window());
        if let Some(menu) = menu {
            settle(&mut cx, menu);
            cx.capture_screenshot(menu)
                .expect("menu pixels")
                .save(dir.join("glass-main-menu.png"))
                .unwrap();
        }
    }
    assert_ne!(
        before.as_raw(),
        after.as_raw(),
        "agents, hover and output must change visible pixels"
    );
    cx.update(|cx| desk.update(cx, |desk, cx| desk.prepare_close(cx)));
    drop(cx);
    drop(client);
    let _ = std::fs::remove_dir_all(root);
}

/// Real Metal shelf pixels must change for output on an inactive tab, while
/// retained and scratch draws agree. This exercises the production wake task.
fn tab_peek_pixels(
    cx: &mut HeadlessAppContext,
    window: gpui::AnyWindowHandle,
    desk: &Entity<Desk>,
    client: &Client,
) {
    cx.update(|cx| {
        desk.update(cx, |desk, cx| {
            let link = desk.link.as_mut().unwrap();
            for index in 1..TERMINALS_PER_SESSION {
                let pane = link.connection.pane(&resource(0, index)).unwrap().clone();
                link.workspace.open(&link.connection, &pane);
            }
            cx.notify();
        })
    });
    settle(cx, window);
    let execute = |cx: &mut HeadlessAppContext, command| {
        cx.update_window(window, |_, window, cx| {
            desk.update(cx, |desk, cx| desk.execute(command, window, cx))
        })
        .unwrap();
        settle(cx, window);
    };
    execute(cx, "tab-peek");
    let before = cx.capture_screenshot(window).expect("peek before output");
    client
        .feed(FrameKind::ResourceOutput {
            terminal_id: resource(0, 1),
            stream_id: StreamId::new(2).unwrap(),
            bootstrap_id: BootstrapId::new(2).unwrap(),
            seq: 1,
            bytes: b"\x1b[2J\x1b[H\x1b[42mBACKGROUND TAB IS LIVE\x1b[0m"
                .to_vec()
                .into(),
        })
        .unwrap();
    client.wake();
    settle(cx, window);
    let after = cx.capture_screenshot(window).expect("peek after output");
    // The full terminal starts below 216 points. Only the miniatures occupy
    // this band; a main-terminal repaint cannot make this assertion pass.
    let scale = before.width() as f32
        / cx.update_window(window, |_, window, _| window.viewport_size().width.as_f32())
            .unwrap();
    let band = |image: &image::RgbaImage| {
        image::imageops::crop_imm(
            image,
            0,
            (90. * scale) as u32,
            image.width(),
            (100. * scale) as u32,
        )
        .to_image()
    };
    assert_ne!(
        band(&before),
        band(&after),
        "inactive output must repaint its live miniature"
    );
    retained_matches_scratch(cx, window, "tab_peek_live");
    if let Some(dir) = std::env::var_os("PHUX_DESKTOP_UI_EVIDENCE") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        before.save(dir.join("tab-peek-before.png")).unwrap();
        after.save(dir.join("tab-peek-live.png")).unwrap();
    }
    execute(cx, "tab-peek");
}
