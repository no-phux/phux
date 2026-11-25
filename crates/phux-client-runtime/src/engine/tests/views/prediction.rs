use super::*;
use std::time::Duration;

/// Terminal 1 at a `$ ` prompt with two views; the first is typed into.
fn prompt() -> (EngineHandle, Arc<Publication>, ResourceId, ViewId, ViewId) {
    let (owner, publication) = owner();
    let terminal = id(1);
    attach(&owner, &terminal, b"$ ");
    let typed = owner.create_view(&terminal).unwrap();
    let other = owner.create_view(&terminal).unwrap();
    (owner, publication, terminal, typed, other)
}

#[test]
fn a_guess_draws_in_its_own_view_until_the_echo_confirms_it() {
    let (owner, publication, terminal, typed, other) = prompt();
    owner.predict_view_text(typed, "ls".to_owned()).unwrap();
    let guessed = publication.acquire_view(typed).unwrap();
    assert_eq!(guessed.row_text(0), "$ ls");
    assert_eq!((guessed.cursor.col, guessed.cursor.row), (4, 0));
    assert_eq!(publication.acquire_view(other).unwrap().row_text(0), "$");
    assert_eq!(publication.acquire(&terminal).unwrap().row_text(0), "$");
    assert!(owner.presentation_deadline().is_some());

    output(&owner, &terminal, 1, b"ls");
    for view in [typed, other] {
        assert_eq!(publication.acquire_view(view).unwrap().row_text(0), "$ ls");
    }
    assert!(
        owner.presentation_deadline().is_none(),
        "a confirmed guess leaves nothing to erase"
    );
}

#[test]
fn a_guess_the_program_never_echoes_is_erased_at_its_deadline() {
    let (owner, publication, terminal, typed, _) = prompt();
    owner
        .predict_view_text(typed, "hunter2".to_owned())
        .unwrap();
    assert_eq!(
        publication.acquire_view(typed).unwrap().row_text(0),
        "$ hunter2"
    );
    let deadline = owner.presentation_deadline().unwrap();
    assert!(
        owner
            .expire_presentation(deadline.checked_sub(Duration::from_millis(5)).unwrap())
            .is_empty(),
        "a guess inside its lifetime stays"
    );
    assert_eq!(owner.expire_presentation(deadline), vec![terminal.clone()]);
    let erased = publication.acquire_view(typed).unwrap();
    assert_eq!(erased.row_text(0), "$");
    assert_eq!((erased.cursor.col, erased.cursor.row), (2, 0));
    assert!(owner.presentation_deadline().is_none());

    // The dropped guesses no longer anchor the next one.
    output(&owner, &terminal, 1, b"\r\n> ");
    owner.predict_view_text(typed, "x".to_owned()).unwrap();
    assert_eq!(publication.acquire_view(typed).unwrap().row_text(1), "> x");
}

#[test]
fn a_view_scrolled_into_history_draws_no_guess() {
    let (owner, publication) = owner();
    let terminal = id(1);
    attach(&owner, &terminal, b"1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n$ ");
    let view = owner.create_view(&terminal).unwrap();
    owner.scroll_view(view, Scroll::Row(0)).unwrap();
    let before = publication.acquire_view(view).unwrap();
    owner.predict_view_text(view, "x".to_owned()).unwrap();
    let after = publication.acquire_view(view).unwrap();
    assert_eq!(held_text(&after), held_text(&before));
    assert!(owner.presentation_deadline().is_none());
}

#[test]
fn a_guess_leaves_a_hidden_cursor_hidden() {
    let (owner, publication) = owner();
    let terminal = id(1);
    attach(&owner, &terminal, b"\x1b[?25l> ");
    let view = owner.create_view(&terminal).unwrap();
    owner.predict_view_text(view, "x".to_owned()).unwrap();
    let frame = publication.acquire_view(view).unwrap();
    assert_eq!(frame.row_text(0), "> x");
    assert!(
        !frame.cursor.visible,
        "a program that draws its own cursor gets no second one"
    );
}

#[test]
fn a_backspace_over_echoed_text_erases_ahead_of_the_server() {
    let (owner, publication) = owner();
    let terminal = id(1);
    attach(&owner, &terminal, b"$ ab");
    let view = owner.create_view(&terminal).unwrap();
    owner.predict_view_text(view, "\u{7f}".to_owned()).unwrap();
    let guessed = publication.acquire_view(view).unwrap();
    assert_eq!(guessed.row_text(0), "$ a");
    assert_eq!(guessed.cursor.col, 3);

    output(&owner, &terminal, 1, b"\x08 \x08");
    assert_eq!(publication.acquire_view(view).unwrap().row_text(0), "$ a");
    assert!(owner.presentation_deadline().is_none());
}

#[test]
fn typing_over_a_shell_suggestion_draws_until_the_echo_replaces_it() {
    let (owner, publication) = owner();
    let terminal = id(1);
    // A grey suggestion after the cursor, as fish and zsh draw one.
    attach(&owner, &terminal, b"$ g\x1b[90mit log\x1b[0m\x1b[4G");
    let view = owner.create_view(&terminal).unwrap();
    owner.predict_view_text(view, "r".to_owned()).unwrap();
    assert_eq!(
        publication.acquire_view(view).unwrap().row_text(0),
        "$ grt log"
    );

    output(&owner, &terminal, 1, b"r\x1b[K");
    assert_eq!(publication.acquire_view(view).unwrap().row_text(0), "$ gr");
    assert!(owner.presentation_deadline().is_none());
}
