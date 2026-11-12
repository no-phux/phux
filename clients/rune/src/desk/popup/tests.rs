//! The popup's rules alone, then its life on the real Rune runtime over an
//! embedded client: asked for, laid out, spawned, ended and dismissed.

use std::sync::Arc;

use phux_client_runtime::Client;
use phux_client_runtime::control::Status;
use phux_protocol::ids::{ResourceId, SatelliteHost};
use phux_protocol::wire::frame::FrameKind;

use super::*;
use crate::desk::input::Modal;
use crate::keys::Shortcut;

fn id(n: u32) -> ResourceId {
    ResourceId::local(n)
}

/// An attached window showing one terminal, on settings no test can save.
fn window() -> (Arc<Client>, rune::Runtime<Desk>) {
    let (client, mut rt) = crate::grid::budget::input_fixture();
    rt.state.prefs = rune::prefs::Prefs::memory();
    rt.state.status = Status::Attached;
    rt.frame(0.0);
    let _ = client.take_outbound();
    (client, rt)
}

/// The spawns sent since the last look: each one's command and size.
type Sent = Vec<(Option<Vec<String>>, Option<(u16, u16)>)>;

fn spawns(client: &Client) -> Sent {
    let frames = client.take_outbound();
    let frames = frames
        .iter()
        .map(|bytes| FrameKind::decode(bytes).expect("a frame").0);
    frames
        .filter_map(|frame| match frame {
            FrameKind::SpawnResource {
                command,
                initial_size,
                ..
            } => Some((command, initial_size)),
            _ => None,
        })
        .collect()
}

/// Ask for `line` and let the box lay out; the spawn it sent.
fn open(rt: &mut rune::Runtime<Desk>, line: &str) -> u32 {
    rt.update(|d, _| d.open_popup(line));
    rt.frame(0.0);
    rt.frame(0.0);
    rt.state.popup.request.expect("a spawn is out")
}

#[test]
fn a_typed_line_is_a_command_only_when_it_names_one() {
    assert_eq!(command_line("  lazygit  "), Some("lazygit"));
    assert_eq!(command_line(" \t "), None);
    let row = item(" git log | head ").expect("a row");
    assert_eq!(row.target, Target::Popup("git log | head".to_owned()));
    assert!(item("").is_none());
}

#[test]
fn a_popup_is_refused_in_words_wherever_it_cannot_start() {
    let here = id(1);
    let away = ResourceId::satellite(SatelliteHost::new("box".to_owned()), 1);
    assert_eq!(refusal(false, false, true, Some(&here)), None);
    let said = |why: Option<&str>, word: &str| why.is_some_and(|why| why.contains(word));
    assert!(said(
        refusal(true, false, true, Some(&here)),
        "already open"
    ));
    assert!(said(refusal(false, true, true, Some(&here)), "desktop"));
    assert!(said(
        refusal(false, false, false, Some(&here)),
        "Not connected"
    ));
    assert!(said(refusal(false, false, true, None), "focused terminal"));
    assert!(said(
        refusal(false, false, true, Some(&away)),
        "that machine"
    ));
}

#[test]
fn the_palette_asks_and_the_box_spawns_at_its_own_size() {
    let (client, mut rt) = window();
    rt.update(|d, tx| d.shortcut(Shortcut::Popup, tx));
    assert!(rt.state.palette_open && rt.state.palette_mode == Mode::Popup);
    assert!(rt.state.palette_hits().is_empty(), "nothing typed yet");
    rt.update(|d, tx| {
        d.palette_query.text = "lazygit".to_owned();
        d.palette_run(None, tx);
    });
    assert_eq!(rt.state.popup.command(), Some("lazygit"));
    assert!(!rt.state.palette_open);
    rt.frame(0.0);
    rt.frame(0.0);

    let sh = |line: &str| vec!["/bin/sh".to_owned(), "-c".to_owned(), line.to_owned()];
    let sent = spawns(&client);
    assert_eq!(sent.len(), 1, "one spawn, however often it is laid out");
    assert_eq!(sent[0].0, Some(sh("lazygit")));
    let (cols, rows) = sent[0].1.expect("the box's cells");
    assert!(cols > 20 && rows > 5, "{cols}x{rows}");

    // Centred, and never the whole window.
    let (window, _) = rt.presented("popup-scrim").expect("the scrim");
    let (panel, _) = rt.presented("popup").expect("the box");
    let centre = |rect: [f32; 4], axis: usize| rect[axis] + rect[axis + 2] / 2.0;
    for axis in [0, 1] {
        assert!((centre(panel, axis) - centre(window, axis)).abs() < 1.0);
        assert!(panel[axis + 2] < window[axis + 2]);
    }
}

#[test]
fn the_box_takes_the_keyboard_and_goes_when_its_command_ends() {
    let (client, mut rt) = window();
    let request = open(&mut rt, "lazygit");
    // While it starts, typing has nowhere to go: never the pane underneath.
    assert_eq!(rt.state.modal(), Some(Modal::Popup));
    assert!(rt.state.keyboard().is_none());

    // One at a time: a second is refused and said.
    rt.update(|d, _| d.open_popup("htop"));
    assert_eq!(rt.state.popup.command(), Some("lazygit"));
    let said = rt.state.notice.as_ref().expect("a notice");
    assert!(said.text.contains("already open"));

    // Someone else's spawn is not the popup's.
    rt.update(|d, _| assert!(!d.popup_spawned(&client, request + 1, Some(&id(8)), None)));
    rt.update(|d, _| assert!(d.popup_spawned(&client, request, Some(&id(7)), None)));
    assert_eq!(rt.state.keyboard().map(|(has, _)| has.clone()), Some(id(7)));
    assert!(rt.state.addressable().iter().all(|r| r.id != id(7)));

    // Another terminal ending leaves it; its own takes the box down and
    // gives the keyboard back.
    rt.update(|d, _| assert!(!d.popup_ended(&client, &id(1), true)));
    rt.update(|d, _| assert!(d.popup_ended(&client, &id(7), true)));
    rt.frame(0.0);
    assert_eq!(rt.state.modal(), None);
    assert!(rt.find("popup").is_none());
    assert_eq!(rt.state.keyboard().map(|(has, _)| has.clone()), Some(id(1)));
    assert!(spawns(&client).len() <= 1, "nothing spawned again");
}

#[test]
fn a_spawn_that_fails_takes_the_box_down_and_says_why() {
    let (client, mut rt) = window();
    let request = open(&mut rt, "lazygit");
    rt.update(|d, _| assert!(d.popup_spawned(&client, request, None, Some("no pty"))));
    assert_eq!(rt.state.popup.command(), None);
    let said = rt.state.notice.as_ref().expect("a notice");
    assert_eq!(said.text, "Popup not started: no pty");
}

#[test]
fn dismissing_asks_as_closing_a_terminal_does_and_spares_the_pane_under_it() {
    let (client, mut rt) = window();
    let request = open(&mut rt, "lazygit");
    // Dismissed while it starts: the box goes now, and the terminal that
    // lands later is still the popup's to close.
    rt.update(|d, tx| d.shortcut(Shortcut::CloseTerminal, tx));
    assert_eq!(rt.state.popup.command(), None);
    assert_eq!(rt.state.popup.request, Some(request));
    rt.update(|d, _| assert!(d.popup_spawned(&client, request, Some(&id(7)), None)));
    assert!(rt.state.popup.terminal().is_none() && rt.state.popup.request.is_none());
    rt.frame(0.0);

    // With "Confirm before closing" on, a running command is asked about.
    let request = open(&mut rt, "lazygit");
    rt.update(|d, _| {
        d.settings.confirm_close = true;
        assert!(d.popup_spawned(&client, request, Some(&id(7)), None));
        d.close_terminal();
    });
    assert_eq!(rt.state.pending, Some(Pending::Close(id(7))));
    assert_eq!(rt.state.popup.terminal(), Some(&id(7)), "still up");
    rt.frame(0.0);
    assert!(rt.find("confirm-bar").is_some(), "asked in the box");

    // Its terminal closing answers the question; the pane under it stays.
    rt.update(|d, _| assert!(d.popup_ended(&client, &id(7), false)));
    assert_eq!(rt.state.pending, None);
    assert_eq!(
        rt.state.shown.as_ref().map(|s| s.resource.clone()),
        Some(id(1))
    );
}
