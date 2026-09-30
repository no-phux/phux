//! Keyboard, IME, and paste routing (`phux_web::input`), under node.

use phux_protocol::input::key::{KeyAction, ModSet, PhysicalKey};
use phux_protocol::input::paste::PasteTrust;
use phux_web::input::{
    BrowserKey, MAX_PASTE_BYTES, code_to_physical_key, is_copy_chord, key_events_for_text,
    paste_event, route_key, scrollback_page, wheel_rows,
};
use wasm_bindgen_test::wasm_bindgen_test;

fn key<'a>(key: &'a str, code: &'a str) -> BrowserKey<'a> {
    BrowserKey {
        key,
        code,
        ..BrowserKey::default()
    }
}

#[wasm_bindgen_test]
fn function_numpad_and_intl_codes_reach_their_physical_keys() {
    for (code, expected) in [
        ("F1", PhysicalKey::F1),
        ("F5", PhysicalKey::F5),
        ("F12", PhysicalKey::F12),
        ("F25", PhysicalKey::F25),
        ("Numpad0", PhysicalKey::Numpad0),
        ("Numpad7", PhysicalKey::Numpad7),
        ("NumpadAdd", PhysicalKey::NumpadAdd),
        ("NumpadDecimal", PhysicalKey::NumpadDecimal),
        ("NumpadEnter", PhysicalKey::Enter),
        ("IntlBackslash", PhysicalKey::IntlBackslash),
        ("KeyQ", PhysicalKey::Q),
        ("Digit3", PhysicalKey::Digit3),
        ("Backquote", PhysicalKey::Backquote),
    ] {
        assert_eq!(code_to_physical_key(code), expected, "{code}");
    }
    for code in [
        "F0", "F01", "F26", "Fn", "Key", "Keya", "KeyAB", "Digit", "Numpad10", "",
    ] {
        assert_eq!(
            code_to_physical_key(code),
            PhysicalKey::Unidentified,
            "{code}"
        );
    }
}

#[wasm_bindgen_test]
fn ime_dead_and_unidentified_keystrokes_stay_with_the_browser() {
    let composing = BrowserKey {
        composing: true,
        ..key("a", "KeyA")
    };
    assert!(
        route_key(&composing).is_none(),
        "IME owns a composing keystroke"
    );
    assert!(route_key(&key("Process", "KeyA")).is_none());
    assert!(route_key(&key("Dead", "Quote")).is_none());
    assert!(route_key(&key("Unidentified", "")).is_none());
    for modifier in ["Shift", "Control", "Alt", "AltGraph", "Meta", "CapsLock"] {
        assert!(
            route_key(&key(modifier, "ShiftLeft")).is_none(),
            "{modifier}"
        );
    }
}

#[wasm_bindgen_test]
fn platform_shortcuts_and_paste_chords_stay_with_the_browser() {
    let command_f = BrowserKey {
        meta: true,
        ..key("f", "KeyF")
    };
    assert!(
        route_key(&command_f).is_none(),
        "Command+F is the browser's"
    );
    let command_v = BrowserKey {
        meta: true,
        ..key("v", "KeyV")
    };
    assert!(route_key(&command_v).is_none(), "Command+V raises paste");
    let ctrl_shift_v = BrowserKey {
        ctrl: true,
        shift: true,
        ..key("V", "KeyV")
    };
    assert!(
        route_key(&ctrl_shift_v).is_none(),
        "Ctrl+Shift+V raises paste"
    );
    let shift_insert = BrowserKey {
        shift: true,
        ..key("Insert", "Insert")
    };
    assert!(
        route_key(&shift_insert).is_none(),
        "Shift+Insert raises paste"
    );

    // Plain Ctrl+V stays the terminal's literal-next.
    let ctrl_v = BrowserKey {
        ctrl: true,
        ..key("v", "KeyV")
    };
    let event = route_key(&ctrl_v).expect("Ctrl+V is terminal input");
    assert_eq!(event.key, PhysicalKey::V);
    assert_eq!(event.mods, ModSet::CTRL);
    assert_eq!(event.text, None);
}

#[wasm_bindgen_test]
fn printable_keys_carry_text_and_the_modifiers_spent_producing_it() {
    let shifted = BrowserKey {
        shift: true,
        ..key("A", "KeyA")
    };
    let event = route_key(&shifted).unwrap();
    assert_eq!(event.text.as_deref(), Some("A"));
    assert_eq!(event.mods, ModSet::SHIFT);
    assert_eq!(event.consumed_mods, ModSet::SHIFT);
    assert_eq!(event.action, KeyAction::Press);

    // AltGr reports Ctrl+Alt on Windows while producing "@".
    let alt_gr = BrowserKey {
        ctrl: true,
        alt: true,
        alt_graph: true,
        ..key("@", "KeyQ")
    };
    let event = route_key(&alt_gr).unwrap();
    assert_eq!(event.text.as_deref(), Some("@"));
    assert_eq!(event.mods, ModSet::empty());

    // macOS Option composes "å": Alt was spent, so no ESC prefix.
    let option_a = BrowserKey {
        alt: true,
        ..key("\u{e5}", "KeyA")
    };
    let event = route_key(&option_a).unwrap();
    assert_eq!(event.text.as_deref(), Some("\u{e5}"));
    assert_eq!(event.consumed_mods, ModSet::ALT);

    // Alt with an ASCII letter stays a Meta chord.
    let alt_b = BrowserKey {
        alt: true,
        ..key("b", "KeyB")
    };
    let event = route_key(&alt_b).unwrap();
    assert_eq!(event.mods, ModSet::ALT);
    assert_eq!(event.consumed_mods, ModSet::empty());

    let named = route_key(&key("F3", "F3")).unwrap();
    assert_eq!(named.key, PhysicalKey::F3);
    assert_eq!(named.text, None, "a named key is not text");

    let held = BrowserKey {
        repeat: true,
        ..key("j", "KeyJ")
    };
    assert_eq!(route_key(&held).unwrap().action, KeyAction::Repeat);
}

#[wasm_bindgen_test]
fn committed_text_becomes_one_key_per_scalar() {
    let events = key_events_for_text("\u{65e5}\u{672c}");
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].text.as_deref(), Some("\u{65e5}"));
    assert_eq!(events[1].text.as_deref(), Some("\u{672c}"));
}

#[wasm_bindgen_test]
fn shift_page_keys_page_the_local_scrollback() {
    let shifted = |code| BrowserKey {
        shift: true,
        ..key(code, code)
    };
    assert_eq!(scrollback_page(&shifted("PageUp")), Some(-1));
    assert_eq!(scrollback_page(&shifted("PageDown")), Some(1));
    assert_eq!(
        scrollback_page(&key("PageUp", "PageUp")),
        None,
        "bare PageUp is the app's"
    );
    let ctrl_shift = BrowserKey {
        ctrl: true,
        ..shifted("PageUp")
    };
    assert_eq!(scrollback_page(&ctrl_shift), None);
    assert_eq!(scrollback_page(&shifted("ArrowUp")), None);
}

#[wasm_bindgen_test]
fn wheel_deltas_become_whole_rows_with_a_carry() {
    let mut carry = 0.0;
    // Trackpad pixels: 16px rows, three 6px nudges make one row.
    assert_eq!(wheel_rows(-6.0, 0, 16.0, 24, &mut carry), 0);
    assert_eq!(wheel_rows(-6.0, 0, 16.0, 24, &mut carry), 0);
    assert_eq!(wheel_rows(-6.0, 0, 16.0, 24, &mut carry), -1);
    let mut carry = 0.0;
    assert_eq!(wheel_rows(100.0, 0, 16.0, 24, &mut carry), 6);
    let mut carry = 0.0;
    assert_eq!(wheel_rows(-3.0, 1, 16.0, 24, &mut carry), -3, "line mode");
    assert_eq!(wheel_rows(1.0, 2, 16.0, 24, &mut carry), 24, "page mode");
}

#[wasm_bindgen_test]
fn clipboard_paste_is_trusted_and_bounded() {
    let paste = paste_event("echo one\necho two\n").expect("paste");
    assert_eq!(paste.trust, PasteTrust::Trusted);
    assert_eq!(paste.data, b"echo one\necho two\n");
    assert!(paste_event("").is_none());
    assert!(paste_event(&"x".repeat(MAX_PASTE_BYTES)).is_some());
    assert!(paste_event(&"x".repeat(MAX_PASTE_BYTES + 1)).is_none());
}

#[wasm_bindgen_test]
fn command_c_and_ctrl_shift_c_are_copy_chords() {
    let command_c = BrowserKey {
        meta: true,
        ..key("c", "KeyC")
    };
    assert!(is_copy_chord(&command_c));
    let ctrl_shift_c = BrowserKey {
        ctrl: true,
        shift: true,
        ..key("C", "KeyC")
    };
    assert!(is_copy_chord(&ctrl_shift_c));
    let ctrl_c = BrowserKey {
        ctrl: true,
        ..key("c", "KeyC")
    };
    assert!(!is_copy_chord(&ctrl_c), "Ctrl+C stays the interrupt");
}
