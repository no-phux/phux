//! Drawn controls retain field focus without sending input behind a modal.

use phux_client_runtime::Client;
use phux_protocol::wire::frame::FrameKind;
use rune::prefs::Prefs;
use rune::{Runtime, TouchPhase};

use super::*;

mod settings;

fn fixture() -> (std::sync::Arc<Client>, Runtime<Desk>) {
    let (client, mut rt) = crate::grid::budget::input_fixture();
    rt.state.phone = Some(Phone::default());
    rt.state.prefs = Prefs::memory();
    rt.set_viewport((393.0, 852.0), 2.0);
    (client, rt)
}

/// Flush the next frame too: no deferred input may outlive its field.
fn settle(rt: &mut Runtime<Desk>) {
    rt.frame(1.0 / 60.0);
    rt.frame(1.0 / 60.0);
}

fn tap(rt: &mut Runtime<Desk>, key: &str, at: f64) {
    let (x, y) = rt.find(key).unwrap_or_else(|| panic!("no {key}"));
    rt.touch(1, TouchPhase::Started, x, y, at);
    settle(rt);
    rt.touch(1, TouchPhase::Ended, x, y, at + 0.05);
    settle(rt);
}

/// A phone has no ⌘: a tap that lifts where it landed opens the URL under it,
/// while a drag across the same URL selects and opens nothing. Driven as the
/// pane's drag handler drives it, in grid points over a real frame.
#[test]
fn a_tap_on_a_url_opens_it_and_a_drag_across_it_does_not() {
    let (_client, mut rt) =
        crate::grid::budget::input_fixture_showing("\x1b[2J\x1b[Hsee https://phux.sh/docs now");
    rt.state.phone = Some(Phone::default());
    rt.state.prefs = Prefs::memory();
    rt.set_viewport((393.0, 852.0), 2.0);
    settle(&mut rt);
    let id = rt
        .state
        .shown
        .as_ref()
        .expect("a terminal")
        .terminal
        .clone();
    let m = rt.state.metrics();
    let cell = |col: f32| (m.cell_w * (col + 0.5), m.cell_h * 0.5);
    let press = |rt: &mut Runtime<Desk>, from: (f32, f32), to: (f32, f32)| {
        for (phase, (x, y)) in [(Phase::Down, from), (Phase::Move, to), (Phase::Up, to)] {
            rt.update(|d, tx| {
                d.press_link(&id, phase, x, y, false, tx);
            });
        }
        settle(rt);
        rt.take_url_requests()
    };
    let on_url = cell(8.0);
    assert_eq!(press(&mut rt, on_url, on_url), ["https://phux.sh/docs"]);
    assert!(
        press(&mut rt, on_url, cell(24.0)).is_empty(),
        "a drag selects"
    );
    assert!(
        press(&mut rt, cell(1.0), cell(1.0)).is_empty(),
        "plain text"
    );
    // Outside the phone form a plain press is a selection, as it always was.
    rt.state.phone = None;
    assert!(press(&mut rt, on_url, on_url).is_empty(), "desktop wants ⌘");
}

fn no_terminal_input(client: &Client) {
    for bytes in client.take_outbound() {
        assert!(
            !matches!(
                FrameKind::decode(&bytes).expect("decode outbound").0,
                FrameKind::InputKey { .. } | FrameKind::InputPaste { .. }
            ),
            "a soft key reached the terminal behind a field"
        );
    }
}

#[test]
fn the_drawn_keyboard_keeps_palette_focus_and_field_editing() {
    let (client, mut rt) = fixture();
    rt.state.palette_open = true;
    rt.frame(0.0);
    rt.state.palette_picked = 1;
    tap(&mut rt, "phone-latch-Shift", 0.1);
    tap(&mut rt, "phone-cap-116", 0.3);
    tap(&mut rt, "phone-cap-104", 0.5);
    assert!(rt.state.palette_open);
    assert!(rt.state.palette_query.focused);
    assert_eq!(rt.state.palette_query.text, "Th");
    assert_eq!(rt.state.palette_picked, 0, "query change callback ran");
    assert!(!rt.state.phone.as_ref().unwrap().shift);
    rt.update(|desk, tx| desk.soft(Soft::Key(Key::Left), tx));
    settle(&mut rt);
    tap(&mut rt, "phone-cap-backspace", 0.7);
    assert_eq!(rt.state.palette_query.text, "h");
    tap(&mut rt, "phone-key-Escape", 0.9);
    assert!(!rt.state.palette_open);
    no_terminal_input(&client);
}

#[test]
fn find_keeps_focus_when_the_drawn_keyboard_is_pressed() {
    let (client, mut rt) = fixture();
    rt.state.open_find();
    rt.frame(0.0);
    tap(&mut rt, "phone-cap-116", 0.1);
    tap(&mut rt, "phone-cap-104", 0.3);
    assert!(rt.state.find_query.focused);
    assert_eq!(rt.state.find_query.text, "th");
    let generation = rt.state.generation;
    tap(&mut rt, "phone-cap-return", 0.4);
    assert!(rt.state.generation > generation, "Return searches again");
    assert_eq!(rt.state.find_query.text, "th");
    tap(&mut rt, "phone-key-Escape", 0.5);
    assert!(rt.state.find.is_none());
    assert!(!rt.state.find_query.focused);
    no_terminal_input(&client);
}

#[test]
fn a_drawn_return_submits_the_palette_field() {
    let (client, mut rt) = fixture();
    rt.state.open_themes();
    rt.frame(0.0);
    for (index, letter) in "nord".chars().enumerate() {
        tap(
            &mut rt,
            &format!("phone-cap-{}", u32::from(letter)),
            0.1 + index as f64 * 0.2,
        );
    }
    assert_eq!(rt.state.settings.theme, crate::theme::find("nord"));
    tap(&mut rt, "phone-cap-return", 1.0);
    assert!(!rt.state.palette_open);
    assert_eq!(rt.state.settings.theme, crate::theme::find("nord"));
    no_terminal_input(&client);
}

#[test]
fn confirmation_over_find_neither_edits_nor_types_into_the_terminal() {
    let (client, mut rt) = fixture();
    rt.state.open_find();
    rt.state.pending = Some(crate::desk::Pending::Close(
        phux_protocol::ids::ResourceId::local(1),
    ));
    rt.frame(0.0);
    tap(&mut rt, "phone-cap-116", 0.1);
    assert!(rt.state.pending.is_some());
    assert!(rt.state.find_query.text.is_empty());
    tap(&mut rt, "phone-key-Escape", 0.3);
    assert!(rt.state.pending.is_none());
    assert!(rt.state.find.is_some());
    no_terminal_input(&client);
}

#[test]
fn unsupported_sheets_never_send_soft_keys_to_the_terminal() {
    let (client, mut rt) = fixture();
    rt.state.inbox.open = true;
    rt.frame(0.0);
    rt.update(|desk, tx| desk.soft(Soft::Char('x'), tx));
    settle(&mut rt);
    assert!(rt.state.inbox.open);
    no_terminal_input(&client);
}

#[test]
fn closing_a_field_cannot_deliver_its_soft_key_to_the_terminal() {
    for palette in [false, true] {
        let (client, mut rt) = fixture();
        if palette {
            rt.state.toggle_palette();
        } else {
            rt.state.open_find();
        }
        rt.frame(0.0);
        rt.update(|desk, tx| {
            desk.soft(Soft::Char('x'), tx);
            if palette {
                desk.close_palette();
            } else {
                desk.close_find();
            }
        });
        settle(&mut rt);
        no_terminal_input(&client);
    }
}

#[test]
fn field_typing_finishes_before_a_confirmation_replaces_it() {
    let (client, mut rt) = fixture();
    rt.state.open_find();
    rt.frame(0.0);
    rt.update(|desk, tx| {
        desk.soft(Soft::Char('x'), tx);
        desk.pending = Some(crate::desk::Pending::Close(
            phux_protocol::ids::ResourceId::local(1),
        ));
        desk.find_query.clear();
    });
    settle(&mut rt);
    assert!(rt.state.find_query.text.is_empty());
    assert!(rt.state.pending.is_some());
    no_terminal_input(&client);
}
