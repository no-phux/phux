//! Drives terminal input, [`TerminalInput::fit`] and [`TerminalInput::flush_fit`]
//! against an embedded runtime. Press, tab switch, and zoom are one handoff: the
//! newly focused writable view claims ownership while the previous timer is
//! still running, and hiding does not call `fit(false)`.

use super::{KeyDisposition, TerminalInput, fit::FIT_SETTLE, tests::metrics};
use gpui::{
    AppContext, Entity, EntityInputHandler, KeyDownEvent, KeyUpEvent, Keystroke, TestAppContext,
    Window, point, px,
};
use phux_client_runtime::control::{ControlOptions, ControlPlane};
use phux_client_runtime::{Client, Runtime, ViewId};
use phux_protocol::{
    PROTOCOL_VERSION, ResourceId,
    caps::{
        BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, ServerCapabilities,
        ServerFeature, ServerFeatureSet,
    },
    ids::{BootstrapId, ClientId, SessionId, StreamId, WindowId},
    input::key::{KeyAction, KeyEvent},
    wire::{
        frame::{
            AttachTarget, Command, CommandResult, DetachReason, ErrorCode, FrameKind, RolePolicy,
        },
        info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo},
    },
};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

mod fit;
mod keys;
mod paste;
mod scroll;
mod selection;

fn terminal() -> ResourceId {
    ResourceId::local(7)
}

fn session(role: Option<RolePolicy>) -> Arc<Client> {
    session_with(role, b"ready")
}

fn session_with(role: Option<RolePolicy>, output: &[u8]) -> Arc<Client> {
    let client = Arc::new(Runtime::embedded(ControlOptions {
        attach: Some(AttachTarget::ByName("fit".into())),
        attach_role: role,
        ..ControlOptions::default()
    }));
    open_session_with(&client, 1, output);
    client
}

fn open_session(client: &Client, generation: u64) {
    open_session_with(client, generation, b"ready");
}

fn open_session_with(client: &Client, generation: u64, output: &[u8]) {
    let attach_id = negotiate(client);
    let _ = bootstrap_output(client, attach_id, generation, 20, 4, output);
}

/// `HELLO_OK` and the engine, before any terminal snapshot.
fn negotiate(client: &Client) -> u32 {
    client.with_control(ControlPlane::connection_opened);
    let _ = client.take_outbound();
    client
        .feed(FrameKind::HelloOk {
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            server_caps: ServerCapabilities::new()
                .with_features(ServerFeatureSet::with(&[ServerFeature::AcknowledgedInput])),
            server_id: vec![1; 16],
            selected_profile: BootstrapProfile::SynthesizedVtRaw,
            bootstrap_limits: BootstrapLimits::default(),
        })
        .expect("hello");
    client
        .take_outbound()
        .iter()
        .find_map(|bytes| match FrameKind::decode(bytes).expect("decode").0 {
            FrameKind::Attach { attach_id, .. } => Some(attach_id),
            _ => None,
        })
        .expect("attach request")
}

fn bootstrap_output(
    client: &Client,
    attach_id: u32,
    generation: u64,
    cols: u16,
    rows: u16,
    output: &[u8],
) -> Vec<(u16, u16)> {
    let stream_id = StreamId::new(generation).unwrap();
    let bootstrap_id = BootstrapId::new(generation).unwrap();
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), terminal())
        .with_sessions(vec![SessionInfo::new(SessionId::new(1), "fit")])
        .with_windows(vec![WindowInfo::new(
            WindowId::new(1),
            SessionId::new(1),
            "shell",
        )])
        .with_resources(vec![ResourceInfo::new(
            terminal(),
            WindowId::new(1),
            cols,
            rows,
        )]);
    for frame in [
        FrameKind::Attached {
            attach_id,
            snapshot,
            initial_client_id: ClientId::new(1),
        },
        FrameKind::BootstrapBegin {
            terminal_id: terminal(),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols,
            rows,
            base_seq: 0,
        },
        FrameKind::BootstrapChunk {
            terminal_id: terminal(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: output.to_vec().into(),
        },
        FrameKind::BootstrapReady {
            terminal_id: terminal(),
            stream_id,
            bootstrap_id,
            history_cursor: None,
        },
        FrameKind::AttachReady { attach_id },
    ] {
        client.feed(frame).expect("bootstrap");
    }
    let sent = resizes(client);
    let _ = client.take_events();
    sent
}

fn fence(client: &Client) -> Option<phux_client_runtime::control::ProjectionFence> {
    client.with_control(|control| {
        control.apply_line(&terminal(), "command");
        let request_id = control
            .take_outbound()
            .iter()
            .find_map(|bytes| match FrameKind::decode(bytes).expect("decode").0 {
                FrameKind::Command {
                    request_id,
                    command: Command::ApplyInput { .. },
                } => Some(request_id),
                _ => None,
            })
            .expect("apply input");
        control
            .feed(FrameKind::CommandResult {
                request_id,
                result: CommandResult::Error {
                    code: ErrorCode::InputDeliveryUnknown,
                    message: "ambiguous fixture delivery".into(),
                },
            })
            .expect("unknown");
        let _ = control.take_outbound();
        control.projection_fence(&terminal())
    })
}

fn resizes(client: &Client) -> Vec<(u16, u16)> {
    client
        .take_outbound()
        .iter()
        .filter_map(|bytes| match FrameKind::decode(bytes).expect("decode").0 {
            FrameKind::ResizeTerminal { cols, rows, .. } => Some((cols, rows)),
            _ => None,
        })
        .collect()
}

fn bind(
    client: &Arc<Client>,
    view: ViewId,
    window: &mut Window,
    cx: &mut gpui::App,
) -> Entity<TerminalInput> {
    let focus = cx.focus_handle();
    let weak = Arc::downgrade(client);
    let terminal = terminal();
    cx.new(move |_cx| TerminalInput::new(weak, view, terminal, focus, window))
}

fn arm(
    input: &Entity<TerminalInput>,
    owner: bool,
    width: f32,
    height: f32,
    window: &mut Window,
    cx: &mut gpui::App,
) {
    input.update(cx, |state, cx| {
        let client = state.client().expect("client");
        let frame = client.acquire_view(state.view).expect("frame");
        let epoch = client.connection_epoch();
        state
            .presented_at(epoch, &frame, metrics(width, height))
            .expect("present");
        state.fit(owner, window, cx);
    });
}

fn settle(input: &Entity<TerminalInput>, cx: &mut gpui::App) {
    input.update(cx, |state, _| {
        if let Some(pending) = &mut state.settling {
            pending.since = Instant::now() - FIT_SETTLE;
        }
        state.flush_fit();
    });
}

fn keys(client: &Client) -> Vec<KeyEvent> {
    client
        .take_outbound()
        .iter()
        .map(|bytes| match FrameKind::decode(bytes).expect("decode").0 {
            FrameKind::InputKey { terminal_id, event } => {
                assert_eq!(terminal_id, terminal());
                event
            }
            frame => panic!("unexpected input frame: {frame:?}"),
        })
        .collect()
}

/// A painted, focused input in an active window.
fn with_focused(
    cx: &mut TestAppContext,
    test: impl FnOnce(&Client, &Entity<TerminalInput>, &mut Window, &mut gpui::App),
) {
    let client = session(None);
    let view = client.create_view(&terminal()).expect("view");
    let cx = cx.add_empty_window();
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    cx.update(|window, cx| {
        let input = bind(&client, view, window, cx);
        arm(&input, false, 160., 68., window, cx);
        input.update(cx, |state, cx| state.focus.focus(window, cx));
        test(&client, &input, window, cx);
    });
}

fn down(keystroke: &Keystroke, is_held: bool) -> KeyDownEvent {
    KeyDownEvent {
        keystroke: keystroke.clone(),
        is_held,
        prefer_character_input: false,
    }
}

fn press_at(
    state: &mut TerminalInput,
    position: gpui::Point<gpui::Pixels>,
    window: &mut Window,
    cx: &mut gpui::Context<TerminalInput>,
) {
    let down = gpui::MouseDownEvent {
        button: gpui::MouseButton::Left,
        position,
        click_count: 1,
        ..Default::default()
    };
    assert_eq!(state.mouse_down(&down, window, cx), Ok(true));
}

fn drag_to(
    state: &mut TerminalInput,
    position: gpui::Point<gpui::Pixels>,
    window: &mut Window,
    cx: &mut gpui::Context<TerminalInput>,
) {
    let drag = gpui::MouseMoveEvent {
        position,
        pressed_button: Some(gpui::MouseButton::Left),
        ..Default::default()
    };
    assert_eq!(state.mouse_move(&drag, window, cx), Ok(true));
}

fn release_at(
    state: &mut TerminalInput,
    position: gpui::Point<gpui::Pixels>,
    window: &mut Window,
    cx: &mut gpui::Context<TerminalInput>,
) {
    let up = gpui::MouseUpEvent {
        button: gpui::MouseButton::Left,
        position,
        click_count: 1,
        ..Default::default()
    };
    assert_eq!(state.mouse_up(&up, window, cx), Ok(true));
}

fn selection(client: &Client, view: ViewId) -> String {
    let text = client
        .engine()
        .expect("engine")
        .view_selection_text(view)
        .unwrap_or_default();
    String::from_utf8(text).expect("utf-8")
}
