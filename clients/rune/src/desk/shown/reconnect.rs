//! A reconnect to the same server, through the window's own poll and the
//! connection it really makes: an inventory first, then the session's
//! terminals one by one. The terminal must stay on screen throughout.

use std::sync::Arc;

use phux_client_runtime::control::{ControlOptions, ControlPlane, SessionRecovery, Status};
use phux_client_runtime::{Client, Runtime};
use phux_protocol::ids::{ResourceId, SessionId, WindowId};
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, FrameKind};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};

use crate::desk::{Desk, input};
use crate::launch::Resolved;

fn terminal() -> ResourceId {
    ResourceId::local(1)
}

use crate::wire_fixture::{answer, bootstrap, outbound as sent};

/// The server answers a new connection: the same incarnation, and an
/// inventory of one session holding one terminal.
fn greet(client: &Client) {
    let _ = client.take_outbound();
    client
        .feed(crate::wire_fixture::hello(1, false))
        .expect("hello");
    let read = |frame: &FrameKind| match frame {
        FrameKind::Command {
            request_id,
            command: Command::GetState { .. },
        } => Some(*request_id),
        _ => None,
    };
    let request_id = sent(client).iter().find_map(read).expect("an inventory");
    let (session, window) = (SessionId::new(1), WindowId::new(1));
    let shell = WindowInfo::new(window, session, "shell").with_active_resource(Some(terminal()));
    let snapshot = SessionSnapshot::new(session, window, terminal())
        .with_sessions(vec![SessionInfo::new(session, "work")])
        .with_windows(vec![shell])
        .with_resources(vec![ResourceInfo::new(terminal(), window, 80, 24)]);
    answer(client, request_id, CommandValue::State(snapshot));
}

/// The server streams the terminal the window subscribed to, showing `says`.
fn stream(client: &Client, round: u64, says: &str) {
    let subscribe = |frame: &FrameKind| match frame {
        FrameKind::Command {
            request_id,
            command: Command::AttachResource { terminal_id, .. },
        } if *terminal_id == terminal() => Some(*request_id),
        _ => None,
    };
    let request_id = sent(client)
        .iter()
        .find_map(subscribe)
        .expect("the window subscribes to its terminal");
    client
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::Ok,
        })
        .expect("the subscription");
    bootstrap(client, &terminal(), [round; 2], [80, 24], says.as_bytes());
}

/// One poll and one frame, as the window runs them.
fn step(rt: &mut rune::Runtime<Desk>) {
    rt.update(|desk, tx| {
        desk.poll(1.0, tx);
    });
    rt.frame(1.0 / 60.0);
}

/// The first row of the terminal on screen, or `None` with nothing painted.
fn on_screen(rt: &rune::Runtime<Desk>, client: &Client) -> Option<String> {
    let view = rt.state.connection.shown.as_ref()?.view?;
    let frame = client.acquire_view(view)?;
    rt.state.connection.layout.as_ref()?;
    if rt.find("placeholder-machines").is_some() {
        return None;
    }
    let cells = 0..usize::from(frame.cols);
    let row: String = cells
        .map(|cell| String::from_utf8_lossy(frame.buffer.cell_text(cell)).into_owned())
        .collect();
    Some(row.trim_end().to_owned())
}

fn type_x(rt: &mut rune::Runtime<Desk>) {
    rt.update(|desk, _| desk.send_input(|control, id| control.send_text(id, "x")));
}

#[test]
fn a_reconnect_to_the_same_server_never_takes_the_terminal_off_screen() {
    reconnects_in_place(false, (1100.0, 720.0));
    reconnects_in_place(true, (393.0, 852.0));
}

fn reconnects_in_place(phone: bool, size: (f32, f32)) {
    // The options `sessions::connect` dials with, as far as they matter here.
    let client = Arc::new(Runtime::embedded(ControlOptions {
        session_recovery: SessionRecovery::CallerValidated,
        ..ControlOptions::default()
    }));
    let mut desk = Desk::with_preferences(
        &Resolved::offline("reconnect"),
        None,
        rune::prefs::Prefs::memory(),
    )
    .with_phone(phone);
    desk.link.heard = true; // no first-run tour over the terminal
    desk.connection.client = Some(Arc::clone(&client));
    let env = rune::Env::new(true, 2.0);
    let mut rt = rune::Runtime::new(desk, crate::ui::view, size, env);

    client.with_control(ControlPlane::connection_opened);
    greet(&client);
    step(&mut rt);
    assert_eq!(on_screen(&rt, &client), None, "nothing is streamed yet");
    stream(&client, 1, "one");
    step(&mut rt);
    assert_eq!(on_screen(&rt, &client).as_deref(), Some("one"));

    // The socket drops. The screen stays, and input says it can't land.
    client.with_control(|control| control.connection_lost(None));
    step(&mut rt);
    assert_eq!(rt.state.connection.status, Status::Connecting);
    assert_eq!(on_screen(&rt, &client).as_deref(), Some("one"));
    type_x(&mut rt);
    let notice = rt.state.notice.as_ref().map(|notice| notice.text.as_str());
    assert_eq!(notice, Some(input::INPUT_PAUSED));

    // A new connection: its epoch reopens the view in the same poll, on the
    // replica the runtime kept, so no frame is drawn without the terminal.
    client.with_control(ControlPlane::connection_opened);
    step(&mut rt);
    assert_eq!(on_screen(&rt, &client).as_deref(), Some("one"));
    greet(&client);
    step(&mut rt);
    assert_eq!(rt.state.connection.status, Status::Attached);
    assert_eq!(on_screen(&rt, &client).as_deref(), Some("one"));

    // The stream resumes into what is on screen, and typing reaches it.
    stream(&client, 2, "\x1b[2J\x1b[Htwo");
    step(&mut rt);
    assert_eq!(on_screen(&rt, &client).as_deref(), Some("two"));
    let _ = client.take_outbound();
    type_x(&mut rt);
    let typed = |frame: &FrameKind| match frame {
        FrameKind::InputKey { terminal_id, .. } | FrameKind::InputPaste { terminal_id, .. } => {
            *terminal_id == terminal()
        }
        _ => false,
    };
    assert!(
        sent(&client).iter().any(typed),
        "the key went to the terminal"
    );
}
