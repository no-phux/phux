//! Clipboard pastes: refusals, uploads and paste protection.

use super::*;

fn copy_file(cx: &mut gpui::App) {
    cx.write_to_clipboard(gpui::ClipboardItem {
        entries: vec![gpui::ClipboardEntry::ExternalPaths(gpui::ExternalPaths(
            vec![std::path::PathBuf::from("/tmp/source.txt")].into(),
        ))],
    });
}

#[gpui::test]
fn a_file_paste_refuses_unpainted_and_reconnected_inputs(cx: &mut TestAppContext) {
    let client = session(None);
    let view = client.create_view(&terminal()).expect("view");
    cx.add_empty_window().update(|window, cx| {
        let input = bind(&client, view, window, cx);
        copy_file(cx);
        input.update(cx, |state, cx| {
            state.set_window_remote(Some("mini".into()));
            let notice = state.paste_command(cx).expect("unpainted input refuses");
            assert_eq!(notice.title, crate::attach::PASTE_REFUSED);
            assert_eq!(
                state.last_error(),
                Some(&crate::input::InputError::StalePresentation)
            );
        });
        arm(&input, false, 160., 68., window, cx);
        open_session(&client, 2);
        input.update(cx, |state, cx| {
            let notice = state.paste_command(cx).expect("reconnected input refuses");
            assert_eq!(notice.title, crate::attach::PASTE_REFUSED);
            assert!(state.last_error().is_some());
        });
        assert!(client.take_outbound().is_empty(), "no raw paste or upload");
    });
}

/// Revoked scope: once the server says the connection's authorization
/// was revoked, a paste is refused and nothing reaches the wire, even
/// before the transport closes.
#[gpui::test]
fn a_revoked_authorization_refuses_input_and_sends_nothing(cx: &mut TestAppContext) {
    let client = session(None);
    let view = client.create_view(&terminal()).expect("view");
    cx.add_empty_window().update(|window, cx| {
        let input = bind(&client, view, window, cx);
        arm(&input, false, 160., 68., window, cx);
        let _ = client.take_outbound();
        client
            .feed(FrameKind::Detached {
                reason: Some(DetachReason::AuthorizationRevoked),
                message: String::new(),
            })
            .expect("detached");
        let _ = client.take_outbound();
        input.update(cx, |state, _| {
            let receipt = state.paste("rm -rf ~\n", true);
            assert!(
                matches!(receipt, Ok(Some(_))),
                "the refusal is a typed receipt: {receipt:?}"
            );
            assert_eq!(
                state.with_current(true, |_| Ok(())),
                Err(crate::input::InputError::NotReady),
                "the ended attach admits no input"
            );
        });
        let sent = client.take_outbound();
        assert!(sent.is_empty(), "revoked input reached the wire: {sent:?}");
    });
}

/// Teardown race: the window's input outlives its connection (the link
/// closed while a paste was on its way). The paste is refused as a stale
/// handle; nothing panics and nothing is queued anywhere.
#[gpui::test]
fn input_that_outlives_its_connection_refuses_without_panicking(cx: &mut TestAppContext) {
    let client = session(None);
    let view = client.create_view(&terminal()).expect("view");
    cx.add_empty_window().update(move |window, cx| {
        let input = bind(&client, view, window, cx);
        arm(&input, false, 160., 68., window, cx);
        let weak = Arc::downgrade(&client);
        drop(client);
        assert!(weak.upgrade().is_none(), "the connection is gone");
        cx.write_to_clipboard(gpui::ClipboardItem {
            entries: vec![gpui::ClipboardEntry::ExternalPaths(gpui::ExternalPaths(
                vec![std::path::PathBuf::from("/tmp/source.txt")].into(),
            ))],
        });
        input.update(cx, |state, cx| {
            assert_eq!(
                state.paste("late", true),
                Err(crate::input::InputError::StaleHandle)
            );
            // Local: the dropped path pastes as text, refused as stale.
            assert_eq!(state.paste_command(cx), None);
            assert_eq!(
                state.last_error(),
                Some(&crate::input::InputError::StaleHandle)
            );
            // Remote: the upload is refused before any bytes are read.
            state.set_window_remote(Some("mini".into()));
            let notice = state.paste_command(cx).expect("stale input refuses");
            assert_eq!(notice.title, crate::attach::PASTE_REFUSED);
            assert_eq!(
                state.last_error(),
                Some(&crate::input::InputError::StaleHandle)
            );
        });
    });
}

#[gpui::test]
fn a_viewer_cannot_start_a_clipboard_file_upload(cx: &mut TestAppContext) {
    let client = session(Some(RolePolicy::VIEWER));
    let view = client.create_view(&terminal()).expect("view");
    cx.add_empty_window().update(|window, cx| {
        let input = bind(&client, view, window, cx);
        arm(&input, false, 160., 68., window, cx);
        copy_file(cx);
        input.update(cx, |state, cx| {
            state.set_window_remote(Some("mini".into()));
            let notice = state.paste_command(cx).expect("viewer refuses");
            assert_eq!(notice.title, crate::attach::PASTE_REFUSED);
            assert_eq!(
                state.last_error(),
                Some(&crate::input::InputError::NotReady)
            );
        });
        assert!(client.take_outbound().is_empty(), "no raw paste or upload");
    });
}

fn shell(client: &Client, event: phux_protocol::wire::frame::AgentEvent) {
    client
        .feed(FrameKind::Event {
            terminal: Some(terminal()),
            event,
            stamp: None,
        })
        .expect("shell status");
    let _ = client.take_events();
}

/// The paste bytes each acknowledged input carried, in order.
fn pasted(client: &Client) -> Vec<String> {
    client
        .take_outbound()
        .iter()
        .filter_map(|bytes| match FrameKind::decode(bytes).expect("decode").0 {
            FrameKind::Command {
                command: Command::ApplyInput { events, .. },
                ..
            } => events.into_iter().find_map(|event| match event {
                phux_protocol::input::InputEvent::Paste(paste) => {
                    Some(String::from_utf8(paste.data).expect("utf-8"))
                }
                _ => None,
            }),
            _ => None,
        })
        .collect()
}

fn paste_held(
    client: &Arc<Client>,
    view: ViewId,
    text: &str,
    cx: &mut TestAppContext,
) -> (Entity<TerminalInput>, Rc<std::cell::RefCell<Vec<String>>>) {
    let held = Rc::new(std::cell::RefCell::new(Vec::new()));
    let cx = cx.add_empty_window();
    let input = cx.update(|window, cx| {
        let input = bind(client, view, window, cx);
        arm(&input, false, 160., 68., window, cx);
        let seen = held.clone();
        cx.subscribe(&input, move |_, event: &crate::input::InputEvent, _| {
            if let crate::input::InputEvent::ConfirmPaste(paste) = event {
                seen.borrow_mut().push(paste.text.clone());
            }
        })
        .detach();
        input
    });
    let _ = client.take_outbound();
    cx.update(|_, cx| {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.to_owned()));
        input.update(cx, |state, cx| {
            assert_eq!(state.paste_command(cx), None);
        });
    });
    cx.run_until_parked();
    (input, held)
}

/// Bracketed paste on, as zsh, fish, vim and coding agents set it.
const BRACKETED: &[u8] = b"\x1b[?2004h$ ";

/// A line-breaking paste into a program that would read it as typed keys
/// is held for the shell to confirm, and goes only once confirmed.
#[gpui::test]
fn a_multi_line_paste_typed_as_keys_waits_for_confirmation(cx: &mut TestAppContext) {
    let client = session_with(None, b"$ ");
    let view = client.create_view(&terminal()).expect("view");
    let text = "make\nmake install";
    let (input, held) = paste_held(&client, view, text, cx);
    assert_eq!(*held.borrow(), vec![text.to_owned()], "the shell is asked");
    assert!(
        pasted(&client).is_empty(),
        "nothing reaches the program yet"
    );
    cx.update(|cx| input.update(cx, |state, _| state.paste_confirmed(text)));
    assert_eq!(pasted(&client), vec![text.to_owned()]);
}

/// A program with bracketed paste on sees a paste as a paste, so it goes
/// at once, even while the shell reports a running command (a coding
/// agent, an editor).
#[gpui::test]
fn a_bracketed_program_takes_a_multi_line_paste_directly(cx: &mut TestAppContext) {
    let client = session_with(None, BRACKETED);
    let view = client.create_view(&terminal()).expect("view");
    shell(
        &client,
        phux_protocol::wire::frame::AgentEvent::CommandStarted,
    );
    let text = "make\nmake install";
    let (_input, held) = paste_held(&client, view, text, cx);
    assert!(held.borrow().is_empty(), "nothing to confirm");
    assert_eq!(pasted(&client), vec![text.to_owned()]);
}

/// One line never asks, and the bracket terminator always does.
#[gpui::test]
fn one_line_goes_and_a_bracket_terminator_asks(cx: &mut TestAppContext) {
    let client = session_with(None, b"$ ");
    let view = client.create_view(&terminal()).expect("view");
    let (_input, held) = paste_held(&client, view, "echo one line", cx);
    assert!(held.borrow().is_empty());
    assert_eq!(pasted(&client), vec!["echo one line".to_owned()]);

    let client = session_with(None, BRACKETED);
    let view = client.create_view(&terminal()).expect("view");
    let text = "x\x1b[201~rm -rf ~";
    let (_input, held) = paste_held(&client, view, text, cx);
    assert_eq!(*held.borrow(), vec![text.to_owned()]);
    assert!(pasted(&client).is_empty());
}
