//! Tests for `surface_grim_input`.

use super::*;

/// A 4K panel at 2x left of a 1080p panel at 1x rotated upright, as
/// `hyprctl -j monitors` lists them.
const MONITORS: &str = r#"[
    {"name":"DP-1","x":0,"y":0,"width":3840,"height":2160,"scale":2.0,"transform":0},
    {"name":"HDMI-A-1","x":1920,"y":0,"width":1920,"height":1080,"scale":1.0,"transform":1}
]"#;

const fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
    Rect { x, y, w, h }
}

#[test]
fn outputs_are_logical_rectangles_and_display_is_their_union() {
    assert_eq!(
        monitors_rect(MONITORS, Some("DP-1")),
        Ok(rect(0.0, 0.0, 1920.0, 1080.0))
    );
    assert_eq!(
        monitors_rect(MONITORS, Some("HDMI-A-1")),
        Ok(rect(1920.0, 0.0, 1080.0, 1920.0))
    );
    assert_eq!(
        monitors_rect(MONITORS, None),
        Ok(rect(0.0, 0.0, 3000.0, 1920.0))
    );
    assert!(monitors_rect(MONITORS, Some("eDP-1")).is_err());
}

#[test]
fn image_pixels_map_to_layout_points_and_clamp_to_the_frame() {
    // grim renders `display` at the highest scale: 6000x3840 for 3000x1920.
    let all = rect(0.0, 0.0, 3000.0, 1920.0);
    assert_eq!(to_layout((4000, 200), (6000, 3840), all), Ok((2000, 100)));
    // The same layout under `--max-width 1500`: a quarter-size frame.
    assert_eq!(to_layout((1000, 50), (1500, 960), all), Ok((2000, 100)));
    // One output: its origin is added.
    let hdmi = rect(1920.0, 0.0, 1080.0, 1920.0);
    assert_eq!(to_layout((10, 20), (1080, 1920), hdmi), Ok((1930, 20)));
    // A window at (12,34), captured natively on a 2x output.
    let window = rect(12.0, 34.0, 800.0, 600.0);
    assert_eq!(to_layout((100, 50), (1600, 1200), window), Ok((62, 59)));
    // Outside the image lands on the far edge, still inside the window.
    assert_eq!(
        to_layout((9999, 9999), (1600, 1200), window),
        Ok((811, 633))
    );
    assert!(to_layout((0, 0), (0, 0), window).is_err());
}

const ALL: Tools = Tools {
    hyprctl: true,
    wlrctl: true,
    ydotool: true,
    wtype: true,
};
const YDOTOOL: Tools = Tools {
    hyprctl: true,
    wlrctl: false,
    ydotool: true,
    wtype: false,
};

fn cmds(input: &DocumentInput, place: Option<&Place>, tools: Tools) -> Result<Vec<String>, String> {
    plan(input, (1600, 1200), place, tools, false).map(|out| out.iter().map(|a| shown(a)).collect())
}

#[test]
fn a_window_click_focuses_moves_then_presses() {
    // Copy, cut and select-all: Ctrl+C/X/A, a terminal's Ctrl+Shift+C for
    // both copy and cut, and no select-all in a terminal.
    let try_chord = |input: &DocumentInput, terminal, tools| {
        plan(input, (1, 1), None, tools, terminal)
            .map(|out| out.iter().map(|a| shown(a)).collect::<Vec<_>>())
    };
    let chord = |input: &DocumentInput, terminal, tools| try_chord(input, terminal, tools).unwrap();
    let cut = DocumentInput::CopySelection {
        reply: 1,
        cut: true,
    };
    assert_eq!(chord(&cut, false, ALL), ["wtype -M ctrl x -m ctrl"]);
    assert_eq!(
        chord(&cut, true, ALL),
        ["wtype -M ctrl -M shift c -m shift -m ctrl"]
    );
    assert_eq!(
        chord(&cut, true, YDOTOOL),
        ["ydotool key 29:1 42:1 46:1 46:0 42:0 29:0"]
    );
    let all = DocumentInput::SelectAll;
    assert_eq!(
        chord(&all, false, YDOTOOL),
        ["ydotool key 29:1 30:1 30:0 29:0"]
    );
    assert!(
        try_chord(&all, true, ALL)
            .unwrap_err()
            .contains("select-all")
    );
    let place = Place {
        frame: rect(12.0, 34.0, 800.0, 600.0),
        window: Some("0xa1".into()),
        layout: rect(0.0, 0.0, 3000.0, 1920.0),
    };
    let click = DocumentInput::Click { x: 100, y: 50 };
    assert_eq!(
        cmds(&click, Some(&place), ALL).unwrap(),
        [
            "hyprctl dispatch focuswindow address:0xa1",
            "hyprctl dispatch movecursor 62 59",
            "wlrctl pointer click left",
        ]
    );
    assert_eq!(
        cmds(&click, Some(&place), YDOTOOL).unwrap()[2],
        "ydotool click 0xC0"
    );
    let none = Tools {
        hyprctl: true,
        ..Tools::default()
    };
    assert!(
        cmds(&click, Some(&place), none)
            .unwrap_err()
            .contains("install wlrctl")
    );
    assert!(cmds(&click, None, ALL).unwrap_err().contains("Hyprland"));
}

#[test]
fn keys_text_and_scroll_pick_the_first_tool_present() {
    let display = Place {
        frame: rect(0.0, 0.0, 800.0, 600.0),
        window: None,
        layout: rect(0.0, 0.0, 800.0, 600.0),
    };
    let enter = DocumentInput::Key {
        key: DocumentKey::Enter,
    };
    assert_eq!(cmds(&enter, None, ALL).unwrap(), ["wtype -k Return"]);
    assert_eq!(
        cmds(&enter, None, YDOTOOL).unwrap(),
        ["ydotool key 28:1 28:0"]
    );
    let wlrctl_only = Tools {
        wlrctl: true,
        ..Tools::default()
    };
    assert!(
        cmds(&enter, None, wlrctl_only)
            .unwrap_err()
            .contains("install wtype")
    );
    let text = DocumentInput::Type {
        text: "it's -x".into(),
    };
    assert_eq!(cmds(&text, None, ALL).unwrap(), [r"wtype -- 'it'\''s -x'"]);
    assert_eq!(
        cmds(&text, None, wlrctl_only).unwrap(),
        [r"wlrctl keyboard type 'it'\''s -x'"]
    );
    // A 2x frame halves the delta; a display scrolls where the pointer is.
    let scroll = DocumentInput::Scroll { dx: 0, dy: 200 };
    assert_eq!(
        cmds(&scroll, Some(&display), ALL).unwrap(),
        ["wlrctl pointer scroll 100 0"]
    );
    assert_eq!(
        cmds(&scroll, Some(&display), YDOTOOL).unwrap(),
        ["ydotool mousemove --wheel -x 0 -y -2"]
    );
    let window = Place {
        window: Some("0xb2".into()),
        ..display
    };
    assert_eq!(
        cmds(&scroll, Some(&window), ALL).unwrap()[1],
        "hyprctl dispatch movecursor 400 300"
    );
    let nav = DocumentInput::Navigate {
        url: "https://phux.sh/".into(),
    };
    assert_eq!(
        cmds(&nav, Some(&window), ALL).unwrap(),
        Vec::<String>::new()
    );
    let paste = DocumentInput::Paste {
        text: "x".into(),
        chord: true,
    };
    assert_eq!(
        cmds(&paste, None, ALL).unwrap(),
        ["wtype -M ctrl v -m ctrl"]
    );
    assert_eq!(
        cmds(&paste, None, YDOTOOL).unwrap(),
        ["ydotool key 29:1 47:1 47:0 29:0"]
    );
    assert!(cmds(&paste, None, wlrctl_only).is_err());
}

#[test]
fn a_terminal_pastes_on_ctrl_shift_v_and_a_drag_holds_the_button() {
    assert!(is_terminal(r#"{"class":"kitty","address":"0x1"}"#, None));
    assert!(is_terminal(r#"{"class":"com.mitchellh.ghostty"}"#, None));
    assert!(is_terminal(r#"{"class":"Alacritty"}"#, None));
    assert!(!is_terminal(r#"{"class":"chromium"}"#, None));
    assert!(!is_terminal("{}", None));
    assert!(!is_terminal("not json", None));
    let clients = r#"[{"address":"0x1","class":"chromium"},{"address":"0x2","class":"foot"}]"#;
    assert!(is_terminal(clients, Some("0x2")));
    assert!(!is_terminal(clients, Some("0x1")));
    assert!(!is_terminal(clients, Some("0x9")));
    let paste = DocumentInput::Paste {
        text: "x".into(),
        chord: true,
    };
    let shifted = |tools| {
        plan(&paste, (1, 1), None, tools, true)
            .map(|out| out.iter().map(|a| shown(a)).collect::<Vec<_>>())
    };
    assert_eq!(
        shifted(ALL).unwrap(),
        ["wtype -M ctrl -M shift v -m shift -m ctrl"]
    );
    assert_eq!(
        shifted(YDOTOOL).unwrap(),
        ["ydotool key 29:1 42:1 47:1 47:0 42:0 29:0"]
    );
    let place = Place {
        frame: rect(12.0, 34.0, 800.0, 600.0),
        window: Some("0xa1".into()),
        layout: rect(0.0, 0.0, 3000.0, 1920.0),
    };
    let drag = DocumentInput::Drag {
        from: (100, 50),
        to: (300, 50),
    };
    assert_eq!(
        cmds(&drag, Some(&place), YDOTOOL).unwrap(),
        [
            "hyprctl dispatch focuswindow address:0xa1",
            "hyprctl dispatch movecursor 62 59",
            "ydotool click 0x40",
            "hyprctl dispatch movecursor 162 59",
            "ydotool click 0x80",
        ]
    );
    let wlrctl = Tools {
        hyprctl: true,
        wlrctl: true,
        ..Tools::default()
    };
    assert!(
        cmds(&drag, Some(&place), wlrctl)
            .unwrap_err()
            .contains("ydotool")
    );
}
