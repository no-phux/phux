//! Helpers shared by the terminal suites.

#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` demands pub(crate) in a test-binary module"
)]

use std::time::Duration;

use phux_protocol::ids::ResourceId;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, StateScope};
use tokio::net::UnixStream;

use phux_server_testkit::command;
use portable_pty::CommandBuilder;

/// `/bin/sh -c script`.
pub(crate) fn sh(script: &str) -> CommandBuilder {
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.args(["-c", script]);
    cmd
}

/// An unmodified press of a text-less key (Enter, Escape, arrows, ...).
pub(crate) const fn named_key(key: PhysicalKey) -> KeyEvent {
    KeyEvent {
        action: KeyAction::Press,
        key,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: None,
    }
}

/// Count non-overlapping occurrences of `needle` in `hay`.
pub(crate) fn count(hay: &[u8], needle: &[u8]) -> usize {
    let mut n = 0;
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        if &hay[i..i + needle.len()] == needle {
            n += 1;
            i += needle.len();
        } else {
            i += 1;
        }
    }
    n
}

/// `GET_STATE { Server }` -> the focused pane of the seeded session.
pub(crate) async fn focused_resource(stream: &mut UnixStream, request_id: u32) -> ResourceId {
    let scope = StateScope::Server;
    match command(stream, request_id, Command::GetState { scope }).await {
        CommandResult::OkWith(CommandValue::State(snap)) => snap.focused_resource,
        other => panic!("expected Ok_With(State(..)), got {other:?}"),
    }
}

/// `GET_SCREEN` for `pane`, parsed.
pub(crate) async fn screen(
    stream: &mut UnixStream,
    request_id: u32,
    pane: &ResourceId,
) -> phux_core::screen::ScreenState {
    let get = Command::GetScreen {
        terminal_id: pane.clone(),
        request_scrollback: None,
        cells: false,
        format: 0,
    };
    match command(stream, request_id, get).await {
        CommandResult::OkWith(CommandValue::Json(json)) => serde_json::from_str(&json).unwrap(),
        other => panic!("expected Ok_With(Json(..)), got {other:?}"),
    }
}

/// Poll `GET_SCREEN` (up to ~5s) until `done` holds; returns the last screen
/// either way so the caller's assertion carries diagnostics.
pub(crate) async fn poll_screen(
    stream: &mut UnixStream,
    pane: &ResourceId,
    done: impl Fn(&phux_core::screen::ScreenState) -> bool,
) -> phux_core::screen::ScreenState {
    let mut last = None;
    for i in 0..200 {
        let s = screen(stream, 2000 + i, pane).await;
        if done(&s) {
            return s;
        }
        last = Some(s);
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    last.unwrap()
}
