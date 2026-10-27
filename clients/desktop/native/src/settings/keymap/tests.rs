use super::super::ghostty::parse_ghostty;
use super::{ChordSource, chord_source, default_chords, effective_keymap, keymap_conflicts};

#[cfg(windows)]
#[test]
fn windows_shortcuts_do_not_intercept_shell_signals() {
    let chords = default_chords();
    for (key, action) in [
        ("ctrl+shift+c", "copy"),
        ("ctrl+shift+v", "paste"),
        ("ctrl+shift+r", "reconnect"),
        ("ctrl+shift+d", "split-right"),
    ] {
        assert!(chords.contains(&(key.into(), action.into())));
    }
    for key in ["ctrl+c", "ctrl+z", "ctrl+d"] {
        assert!(!chords.iter().any(|(chord, _)| chord == key));
    }
    assert_eq!(super::display_chord("ctrl+shift+c"), "Ctrl+Shift+C");
    assert_eq!(super::display_chord("cmd+t"), "Win+T");
}

#[cfg(target_os = "macos")]
#[test]
fn mac_command_arrows_walk_tabs_and_sessions() {
    let chords = default_chords();
    for (key, action) in [
        ("cmd+left", "tab-prev"),
        ("cmd+right", "tab-next"),
        ("cmd+up", "session-prev"),
        ("cmd+down", "session-next"),
    ] {
        let bound: Vec<_> = chords.iter().filter(|(chord, _)| chord == key).collect();
        assert_eq!(bound, [&(key.to_owned(), action.to_owned())]);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn linux_shortcuts_leave_shell_control_keys_alone() {
    let chords = default_chords();
    assert!(chords.contains(&("ctrl+shift+c".into(), "copy".into())));
    assert!(chords.contains(&("ctrl+shift+v".into(), "paste".into())));
    for key in ["ctrl+c", "ctrl+z", "ctrl+d"] {
        assert!(!chords.iter().any(|(chord, _)| chord == key));
    }
    assert_eq!(super::display_chord("ctrl+shift+c"), "Ctrl+Shift+C");
    assert_eq!(super::display_chord("cmd+t"), "Super+T");
}

#[cfg(target_os = "linux")]
#[test]
fn linux_clipboard_shortcuts_can_be_unbound() {
    let config = parse_ghostty("keybind = ctrl+shift+c=unbind\nkeybind = ctrl+shift+v=unbind\n");
    let keymap = effective_keymap(&config, true);
    for chord in ["ctrl+shift+c", "ctrl+shift+v"] {
        assert!(!keymap.iter().any(|(key, _)| key == chord));
        assert!(!super::app_chords(&keymap).iter().any(|key| key == chord));
    }
}

#[test]
fn built_in_chords_never_conflict() {
    let chords = default_chords();
    for (index, (chord, id)) in chords.iter().enumerate() {
        assert!(
            !chords[index + 1..]
                .iter()
                .any(|(other, other_id)| other == chord && other_id != id),
            "{chord} is bound to more than one built-in command"
        );
    }
}

#[test]
fn capture_has_a_local_chord_and_accepts_a_user_override() {
    let capture = super::builtin_commands()
        .iter()
        .find(|command| command.id == "capture")
        .unwrap();
    assert!(capture.is_listed());
    assert_eq!(capture.chord, Some("cmd+shift+space"));
    let config = parse_ghostty("keybind = super+shift+k=capture\n");
    assert!(effective_keymap(&config, true).contains(&("cmd+shift+k".into(), "capture".into())));
}

#[test]
fn a_chord_bound_to_different_actions_is_a_conflict() {
    let config = parse_ghostty(
        "keybind = super+d=new_split:right\n\
         keybind = super+k=clear_screen\n\
         keybind = super+d=new_split:down\n\
         keybind = super+k=clear_screen\n\
         keybind = super+j=new_tab\n",
    );
    let conflicts = keymap_conflicts(&config);
    assert_eq!(
        conflicts.len(),
        1,
        "a repeated identical bind is not a conflict"
    );
    let conflict = &conflicts[0];
    assert_eq!(conflict.chord, "cmd+d");
    let lines: Vec<(usize, &str)> = conflict
        .binds
        .iter()
        .map(|bind| (bind.origin.line, bind.command.as_str()))
        .collect();
    assert_eq!(lines, vec![(1, "split-right"), (3, "split-down")]);
    // The last line is the one in effect, and the keymap agrees.
    let keymap = effective_keymap(&config, true);
    assert!(keymap.contains(&("cmd+d".to_owned(), "split-down".to_owned())));
    assert_eq!(
        chord_source(&config, true, "cmd+d", "split-down"),
        ChordSource::Ghostty(conflict.binds[1].origin.clone())
    );
    assert_eq!(
        chord_source(&config, false, "cmd+d", "split-right"),
        ChordSource::BuiltIn
    );
    assert_eq!(
        chord_source(&config, true, "cmd+t", "new"),
        ChordSource::BuiltIn
    );
}

#[test]
fn an_unbind_is_not_a_conflict() {
    let config = parse_ghostty(
        "keybind = super+d=new_split:right\n\
         keybind = super+d=unbind\n\
         keybind = super+k=clear_screen\n\
         keybind = super+k=unbind\n\
         keybind = super+k=new_tab\n\
         keybind = super+j=new_tab\n\
         keybind = super+j=unbind\n\
         keybind = super+j=clear_screen\n\
         keybind = super+j=new_split:down\n",
    );
    let conflicts = keymap_conflicts(&config);
    let chords: Vec<&str> = conflicts.iter().map(|c| c.chord.as_str()).collect();
    assert_eq!(chords, vec!["cmd+j"], "an unbind resets the chord");
    let lines: Vec<usize> = conflicts[0]
        .binds
        .iter()
        .map(|bind| bind.origin.line)
        .collect();
    assert_eq!(lines, vec![8, 9]);
}

#[test]
fn clear_and_built_in_restores_drop_earlier_binds() {
    let config = parse_ghostty(
        "keybind = super+d=new_split:right\n\
         keybind = clear\n\
         keybind = super+d=new_split:down\n\
         keybind = super+c=new_tab\n\
         keybind = super+c=copy_to_clipboard\n",
    );
    assert!(keymap_conflicts(&config).is_empty());
    assert_eq!(config.binds.len(), 1);
    assert_eq!(config.binds[0].origin.line, 3);
}
