//! Waiting for a key's echo inside the frame that sent it.
//!
//! Rune runs a key inside a frame and presents that frame. An echo landing
//! a moment later finds a surface that just presented, so it waits for the
//! display's next opportunity: a refresh or more (16.7 ms at 60 Hz). A local
//! server answers in about half a millisecond, so the key's frame holds for
//! the view's next publication, up to [`ECHO_WAIT`], and draws the echo
//! itself. The runtime's publish wakes the wait; nothing polls. `PHUX_ECHO_FAST=0` turns this off for a before/after.

use std::sync::Arc;
use std::time::Duration;

use phux_client_runtime::publication::GridFrame;
use phux_client_runtime::{Client, ViewId};
use phux_protocol::input::key::PhysicalKey;

use crate::keys::Typed;

/// What `typed` should put on screen, for drawing ahead of a remote echo:
/// the text itself, or a bare Backspace. Enter, arrows and chords wait for
/// the server, since only the program knows what they do.
pub(super) fn guess(typed: &Typed) -> Option<&str> {
    match typed {
        Typed::Text(text) => Some(text),
        Typed::Key(key) if key.key == PhysicalKey::Backspace && key.mods.is_empty() => {
            Some("\u{7f}")
        }
        Typed::Key(_) => None,
    }
}

/// The longest a key's frame waits. A local server's echo is back within it
/// 99 times in 100 (`scripts/bench/rune-echo.sh`), and it stays under the
/// 8 ms Rune calls a stall.
pub(super) const ECHO_WAIT: Duration = Duration::from_millis(4);

/// Return once `view` publishes a frame newer than `before` or goes away,
/// or after [`ECHO_WAIT`]. A key the program doesn't echo (a password
/// prompt) pays the whole wait.
pub(super) fn await_echo(client: &Client, view: ViewId, before: &Arc<GridFrame>) {
    if !phux_perf::echo_hop::fast_path_enabled() {
        return;
    }
    if let Some(slot) = client.view_slot(view) {
        let _ = slot.wait_past(before.generation, ECHO_WAIT);
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use phux_protocol::ids::{BootstrapId, StreamId};
    use phux_protocol::wire::frame::FrameKind;

    use super::{ECHO_WAIT, PhysicalKey, await_echo};

    #[test]
    fn an_echo_ends_the_wait_and_silence_is_bounded() {
        let (client, rt) = crate::test_support::input_fixture();
        let shown = rt.state.connection.shown.as_ref().unwrap();
        let (terminal_id, view) = (shown.resource.clone(), shown.view.unwrap());
        let before = client.acquire_view(view).unwrap();
        let start = Instant::now();
        await_echo(&client, view, &before);
        let silent = start.elapsed();
        assert!(silent >= ECHO_WAIT && silent < ECHO_WAIT + Duration::from_millis(50));
        // An echo already published ends it at once. The engine's owner
        // thread publishes it; the runtime's tests cover a publish waking a
        // wait in progress.
        client
            .feed(FrameKind::ResourceOutput {
                terminal_id,
                stream_id: StreamId::new(1).unwrap(),
                bootstrap_id: BootstrapId::new(1).unwrap(),
                seq: 1,
                bytes: b"x".to_vec().into(),
            })
            .unwrap();
        let slot = client.view_slot(view).unwrap();
        assert!(slot.wait_past(before.generation, Duration::from_secs(10)));
        let start = Instant::now();
        await_echo(&client, view, &before);
        assert!(start.elapsed() < ECHO_WAIT);
        assert!(client.acquire_view(view).unwrap().text().ends_with('x'));
    }

    fn press(rt: &mut rune::Runtime<crate::desk::Desk>, key: rune::Key) {
        rt.key(crate::keys::plain(key));
        rt.frame(1.0);
    }

    fn type_text(rt: &mut rune::Runtime<crate::desk::Desk>, text: &str) {
        press(rt, rune::Key::Text(text.into()));
    }

    #[test]
    fn typing_on_another_machine_draws_before_its_echo() {
        let (client, mut rt) = crate::test_support::input_fixture_showing("\r\n$ ");
        let view = rt.state.connection.shown.as_ref().unwrap().view.unwrap();
        rt.frame(1.0);
        let (x, y) = rt.find("pane-@1").expect("the terminal");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(1.0);
        let row = || {
            let frame = client.acquire_view(view).unwrap();
            frame.row_text(frame.cursor.row)
        };
        type_text(&mut rt, "l");
        assert_eq!(row(), "$", "this computer's echo is a frame away: no guess");

        rt.state.machines.current = Some("mini".to_owned());
        type_text(&mut rt, "l");
        type_text(&mut rt, "s");
        assert_eq!(row(), "$ ls");
        press(&mut rt, rune::Key::Backspace);
        assert_eq!(row(), "$ l");

        rt.state.predictive_echo = Some(false);
        type_text(&mut rt, "x");
        assert_eq!(row(), "$ l", "predictive-echo = false waits for the server");
    }

    #[test]
    fn only_text_and_a_bare_backspace_are_guessed() {
        use crate::keys::Typed;
        use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet};
        let key = |key, mods| {
            Typed::Key(KeyEvent {
                action: KeyAction::Press,
                key,
                mods,
                consumed_mods: ModSet::empty(),
                composing: false,
                text: None,
                unshifted_codepoint: None,
            })
        };
        assert_eq!(super::guess(&Typed::Text("é".into())), Some("é"));
        assert_eq!(
            super::guess(&key(PhysicalKey::Backspace, ModSet::empty())),
            Some("\u{7f}")
        );
        assert_eq!(
            super::guess(&key(PhysicalKey::Backspace, ModSet::ALT)),
            None
        );
        assert_eq!(
            super::guess(&key(PhysicalKey::Enter, ModSet::empty())),
            None
        );
    }
}
