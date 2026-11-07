use super::{Modal, Shortcut, paste_needs_confirmation};

#[test]
fn nothing_pastes_past_a_question() {
    assert!(!Modal::FindField.allows(Shortcut::Paste));
    assert!(!Modal::Confirm.allows(Shortcut::Paste));
    assert!(!Modal::Settings.allows(Shortcut::Paste));
}

#[test]
fn the_find_field_leaves_commands_running() {
    assert!(Modal::FindField.allows(Shortcut::FindNext));
    assert!(Modal::FindField.allows(Shortcut::FindPrevious));
    for action in [Shortcut::Copy, Shortcut::Paste, Shortcut::SelectAll] {
        assert!(!Modal::FindField.allows(action));
    }
}

#[test]
fn sheets_stop_what_they_hide() {
    assert!(!Modal::Inbox.allows(Shortcut::CloseTerminal));
    assert!(Modal::Settings.allows(Shortcut::ZoomIn));
    assert!(!Modal::Confirm.allows(Shortcut::ZoomIn));
    assert!(!Modal::Palette.allows(Shortcut::FindNext));
    assert!(Modal::Palette.allows(Shortcut::Palette));
}

#[test]
fn only_rune_modals_take_the_pointer() {
    assert!(Modal::ContextMenu.takes_pointer());
    assert!(Modal::Palette.takes_pointer());
    assert!(!Modal::FindField.takes_pointer());
    assert!(!Modal::Confirm.takes_pointer());
}

#[test]
fn line_breaks_ask_only_without_bracketed_paste() {
    assert!(paste_needs_confirmation(false, "ls\nrm -rf x\n"));
    assert!(paste_needs_confirmation(false, "a\rb"));
    assert!(!paste_needs_confirmation(true, "ls\nrm -rf x\n"));
    assert!(!paste_needs_confirmation(false, "one line"));
}

#[test]
fn the_bracket_terminator_always_asks() {
    assert!(paste_needs_confirmation(true, "x\x1b[201~y"));
}
