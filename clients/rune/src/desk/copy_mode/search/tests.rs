use rune::Key;

use super::*;
use crate::desk::input::selection::Selected;

fn press(rt: &mut rune::Runtime<Desk>, keys: &str) {
    for c in keys.chars() {
        rt.key(crate::keys::plain(Key::Text(c.to_string())));
        rt.frame(0.0);
    }
}

fn selected(rt: &rune::Runtime<Desk>) -> Option<String> {
    match rt.state.selected() {
        Selected::Text(text) => Some(text),
        _ => None,
    }
}

/// Type `query` in the field `/` or `?` opened, and press Return.
fn search(rt: &mut rune::Runtime<Desk>, query: &str) {
    assert!(rt.state.connection.find_query.focused, "the field reads it");
    rt.update(|desk, tx| {
        desk.connection.find_query = rune::TextEdit::new(query.to_owned());
        desk.find_key(&crate::keys::plain(Key::Enter), tx);
    });
    rt.frame(0.0);
}

fn fixture() -> (
    std::sync::Arc<phux_client_runtime::Client>,
    rune::Runtime<Desk>,
) {
    let (client, mut rt) =
        crate::test_support::input_fixture_showing("\r\nalpha beta\r\nbeta.delta\r\nlast beta");
    rt.frame(0.0);
    (client, rt)
}

#[test]
fn search_lands_each_match_selected_and_y_copies_just_it() {
    let (client, mut rt) = fixture();
    let board = rune::clipboard::memory();
    rt.use_clipboard(&board);
    rt.update(|desk, _| desk.toggle_copy_mode());
    rt.frame(0.0);
    let _ = client.take_outbound();
    // From the end of the last row, back to its own `beta`, then older.
    press(&mut rt, "?");
    search(&mut rt, "beta");
    assert!(rt.state.connection.find.is_none(), "the field closes");
    assert_eq!(selected(&rt).as_deref(), Some("beta"));
    press(&mut rt, "n");
    assert_eq!(selected(&rt).as_deref(), Some("beta"));
    let row = |rt: &rune::Runtime<Desk>| rt.state.connection.copy_mode.as_ref().map(|m| m.row);
    let middle = row(&rt);
    press(&mut rt, "N");
    assert_ne!(row(&rt), middle, "N goes the other way");
    press(&mut rt, "y");
    rt.frame(0.0);
    assert_eq!(board.text().as_ref().map(|t| t.as_str()), Some("beta"));
    assert!(client.take_outbound().is_empty(), "no key was typed");
}

#[test]
fn a_miss_says_so_and_find_hands_its_match_to_copy_mode() {
    let (_client, mut rt) = fixture();
    rt.update(|desk, _| desk.toggle_copy_mode());
    press(&mut rt, "/");
    search(&mut rt, "zebra");
    assert!(rt.state.connection.copy_mode.is_some());
    let said = |n: &crate::desk::notice::Notice| n.text.starts_with("No match for");
    assert!(rt.state.dismiss_notices(said));
    rt.update(|desk, _| desk.toggle_copy_mode());

    rt.update(|desk, _| {
        desk.open_find();
        desk.connection.find_query = rune::TextEdit::new("delta".to_owned());
        desk.run_search();
        desk.toggle_copy_mode();
    });
    rt.frame(0.0);
    assert!(rt.state.connection.find.is_none());
    assert_eq!(selected(&rt).as_deref(), Some("delta"));
}

#[test]
fn picks_wrap_and_skip_the_match_under_the_cursor() {
    let at = |row, col| Some((row, col));
    // A pruned match sorts first, where pruning happens.
    let matches = [(None, None), (at(1, 0), at(1, 3)), (at(2, 4), at(2, 7))];
    let pick = |cursor, back| pick(matches.len(), |i| matches[i], cursor, back);
    assert_eq!(pick((1, 3), false), Some(2));
    assert_eq!(pick((2, 7), false), Some(0), "wraps");
    assert_eq!(pick((2, 7), true), Some(1));
    assert_eq!(pick((1, 3), true), Some(0));
    assert_eq!(super::pick(0, |_| (None, None), (0, 0), true), None);
}
