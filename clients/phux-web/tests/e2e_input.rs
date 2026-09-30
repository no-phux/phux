//! Headless Chrome against the live `ws_demo_server`: keyboard, IME commit,
//! and clipboard paste reach the terminal through the client's input surface,
//! and nothing else on the page is captured; the wheel pages scrollback and a
//! drag selects text to copy; find, mouse reporting, links, and the bell.
//! The seeded pane runs `cat` on a cooked TTY: the line discipline echoes
//! whatever bytes arrive (control bytes as `^X`), and each finished line comes
//! back raw as program output, which is how these tests make the "program"
//! enable mouse modes, print OSC 8 links, and ring the bell.

use std::time::Duration;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gloo_timers::future::sleep;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{
    CanvasRenderingContext2d, ClipboardEvent, ClipboardEventInit, CompositionEvent,
    CompositionEventInit, DataTransfer, Document, Element, Event, EventInit, HtmlCanvasElement,
    HtmlInputElement, KeyboardEvent, KeyboardEventInit, PointerEvent, PointerEventInit, WheelEvent,
    WheelEventInit,
};

wasm_bindgen_test_configure!(run_in_browser);

const WS_URL: &str = match option_env!("PHUX_TEST_WS_URL") {
    Some(url) => url,
    None => "ws://127.0.0.1:47654/",
};
const POLL: Duration = Duration::from_millis(50);
const POLLS: usize = 120;

fn document() -> Document {
    web_sys::window().unwrap().document().unwrap()
}

fn mounted_canvas(id: &str) -> HtmlCanvasElement {
    let document = document();
    let host = document.create_element("div").unwrap();
    document.body().unwrap().append_child(&host).unwrap();
    let canvas: HtmlCanvasElement = document
        .create_element("canvas")
        .unwrap()
        .dyn_into()
        .unwrap();
    canvas.set_id(id);
    canvas.set_tab_index(0);
    host.append_child(&canvas).unwrap();
    canvas
}

async fn wait_for(client: &phux_web::client::Client, needle: &str) -> bool {
    for _ in 0..POLLS {
        // Wide glyphs leave a spacer cell; compare with blanks removed.
        let screen: String = client.rows_text().concat().replace([' ', '\0'], "");
        if screen.contains(needle) {
            return true;
        }
        sleep(POLL).await;
    }
    false
}

fn screen(client: &phux_web::client::Client) -> String {
    client.rows_text().join("\n")
}

fn keydown(target: &Element, key: &str, code: &str, meta: bool) -> bool {
    let init = KeyboardEventInit::new();
    init.set_key(key);
    init.set_code(code);
    init.set_meta_key(meta);
    init.set_bubbles(true);
    init.set_cancelable(true);
    let event = KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &init).unwrap();
    target.dispatch_event(&event).unwrap();
    event.default_prevented()
}

/// The hidden input surface the client places beside its canvas.
fn input_surface(canvas: &HtmlCanvasElement) -> Element {
    canvas
        .parent_element()
        .and_then(|host| {
            host.query_selector("textarea.phux-web-input")
                .ok()
                .flatten()
        })
        .expect("client mounts a textarea input surface beside the canvas")
}

#[wasm_bindgen_test]
async fn keys_ime_commits_and_paste_reach_the_terminal_and_nothing_else_is_captured() {
    let canvas = mounted_canvas("input-canvas");
    let client = phux_web::client::run(WS_URL, canvas.clone(), 80, 24)
        .await
        .expect("connect to live phux server");

    // Focusing the canvas (what an embedder does) lands on the input surface.
    canvas.focus().unwrap();
    let surface = input_surface(&canvas);
    assert_eq!(
        document().active_element().as_ref(),
        Some(&surface),
        "canvas focus is redirected to the input surface"
    );

    for (key, code) in [("q", "KeyQ"), ("7", "Digit7"), ("k", "KeyK")] {
        assert!(
            keydown(&surface, key, code, false),
            "{key} is terminal input"
        );
    }
    assert!(
        wait_for(&client, "q7k").await,
        "typed keys: {}",
        screen(&client)
    );

    // Command/Super chords are the browser's (reload, location, and the
    // rest); Command+F is the terminal's find, covered below.
    assert!(
        !keydown(&surface, "l", "KeyL", true),
        "Command+L left to the browser"
    );

    // A keydown aimed at another control on the page is not terminal input.
    let other = document().create_element("input").unwrap();
    document().body().unwrap().append_child(&other).unwrap();
    assert!(
        !keydown(&other, "w", "KeyW", false),
        "page input keeps its keys"
    );

    // An IME (or dead-key) commit arrives as committed text.
    let init = CompositionEventInit::new();
    init.set_data("\u{65e5}\u{672c}");
    init.set_bubbles(true);
    let commit = CompositionEvent::new_with_event_init_dict("compositionend", &init).unwrap();
    surface.dispatch_event(&commit).unwrap();
    assert!(
        wait_for(&client, "\u{65e5}\u{672c}").await,
        "IME commit: {}",
        screen(&client)
    );

    // A clipboard paste is one INPUT_PASTE.
    let clipboard = DataTransfer::new().unwrap();
    clipboard.set_data("text/plain", "pasted-xyz").unwrap();
    let init = ClipboardEventInit::new();
    init.set_clipboard_data(Some(&clipboard));
    init.set_bubbles(true);
    init.set_cancelable(true);
    let paste = ClipboardEvent::new_with_event_init_dict("paste", &init).unwrap();
    surface.dispatch_event(&paste).unwrap();
    assert!(paste.default_prevented(), "the client consumed the paste");
    assert!(
        wait_for(&client, "pasted-xyz").await,
        "paste: {}",
        screen(&client)
    );
    assert!(
        !screen(&client).contains('w'),
        "the page input's keystroke never reached the terminal"
    );

    client.close();
    assert!(
        canvas
            .parent_element()
            .unwrap()
            .query_selector("textarea.phux-web-input")
            .unwrap()
            .is_none(),
        "closing the client removes its input surface"
    );
}

fn shift_keydown(target: &Element, key: &str) -> bool {
    let init = KeyboardEventInit::new();
    init.set_key(key);
    init.set_code(key);
    init.set_shift_key(true);
    init.set_bubbles(true);
    init.set_cancelable(true);
    let event = KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &init).unwrap();
    target.dispatch_event(&event).unwrap();
    event.default_prevented()
}

async fn wait_until(client: &phux_web::client::Client, visible: bool, needle: &str) -> bool {
    for _ in 0..POLLS {
        if client.rows_text().iter().any(|row| row.contains(needle)) == visible {
            return true;
        }
        sleep(POLL).await;
    }
    false
}

#[wasm_bindgen_test]
async fn scrollback_pages_by_wheel_and_shift_page_up_and_a_drag_copies_from_it() {
    let canvas = mounted_canvas("scrollback-canvas");
    let client = phux_web::client::run(WS_URL, canvas.clone(), 80, 24)
        .await
        .expect("connect to live phux server");
    let marker = "PHUX_WEB_OK";
    assert!(
        wait_until(&client, true, marker).await,
        "{}",
        screen(&client)
    );
    let surface = input_surface(&canvas);

    // The tty echoes each Enter as a newline: push the marker off screen.
    for _ in 0..40 {
        assert!(keydown(&surface, "Enter", "Enter", false));
    }
    assert!(
        wait_until(&client, false, marker).await,
        "marker scrolled away: {}",
        screen(&client)
    );

    let init = WheelEventInit::new();
    init.set_delta_y(-4_000.0);
    init.set_bubbles(true);
    init.set_cancelable(true);
    let wheel = WheelEvent::new_with_event_init_dict("wheel", &init).unwrap();
    canvas.dispatch_event(&wheel).unwrap();
    assert!(wheel.default_prevented(), "the page does not scroll too");
    assert!(
        wait_until(&client, true, marker).await,
        "wheel reached the marker in scrollback: {}",
        screen(&client)
    );

    // Typing returns to the live screen.
    assert!(keydown(&surface, "Enter", "Enter", false));
    assert!(
        wait_until(&client, false, marker).await,
        "typing returned to live: {}",
        screen(&client)
    );

    // Shift+PageUp pages the local scrollback and never reaches the app.
    for _ in 0..4 {
        assert!(
            shift_keydown(&surface, "PageUp"),
            "Shift+PageUp is consumed"
        );
    }
    assert!(
        wait_until(&client, true, marker).await,
        "Shift+PageUp reached the marker: {}",
        screen(&client)
    );

    // Nothing selected: copy is left to the browser.
    assert_eq!(copy_event(&surface), (false, String::new()));
    // Drag across the marker in the scrolled-back view and copy it.
    let rows = client.rows_text();
    let row = rows.iter().position(|r| r.contains(marker)).unwrap();
    let col = rows[row].find(marker).unwrap();
    let (row, first, last) = (row as u16, col as u16, (col + marker.len() - 1) as u16);
    // A cancelled pointer (a touch the browser took for panning) ends the
    // drag: later moves select nothing.
    pointer(&canvas, "pointerdown", first, row);
    pointer(&canvas, "pointercancel", first, row);
    pointer(&canvas, "pointermove", last, row);
    assert_eq!(copy_event(&surface), (false, String::new()));
    pointer(&canvas, "pointerdown", first, row);
    pointer(&canvas, "pointermove", first + 3, row);
    pointer(&canvas, "pointermove", last, row);
    pointer(&canvas, "pointerup", last, row);
    assert_eq!(
        copy_event(&surface),
        (true, marker.to_owned()),
        "the drag copied the marker"
    );
    // A click without a drag clears the selection.
    pointer(&canvas, "pointerdown", 0, 0);
    pointer(&canvas, "pointerup", 0, 0);
    assert_eq!(copy_event(&surface), (false, String::new()));

    // The Enters pushed the seeded marker off the live screen of the shared
    // pane; echo it back so suites that run later against the same server
    // still find it.
    let init = CompositionEventInit::new();
    init.set_data(marker);
    let retype = CompositionEvent::new_with_event_init_dict("compositionend", &init).unwrap();
    surface.dispatch_event(&retype).unwrap();
    assert!(keydown(&surface, "Enter", "Enter", false));
    assert!(
        wait_until(&client, true, marker).await,
        "{}",
        screen(&client)
    );
    client.close();
}

fn pointer(canvas: &HtmlCanvasElement, kind: &str, col: u16, row: u16) {
    pointer_with(canvas, kind, (col, row), Pointer::default());
}

/// Modifiers and buttons of a synthetic pointer event.
#[derive(Clone, Copy, Default)]
struct Pointer {
    /// The button that changed (`MouseEvent.button`); `None` is primary.
    button: Option<i16>,
    shift: bool,
    ctrl: bool,
}

fn pointer_with(
    canvas: &HtmlCanvasElement,
    kind: &str,
    (col, row): (u16, u16),
    with: Pointer,
) -> bool {
    let rect = canvas.get_bounding_client_rect();
    let init = PointerEventInit::new();
    init.set_pointer_id(1);
    let button = with.button.unwrap_or(0);
    init.set_button(if kind == "pointermove" { -1 } else { button });
    // A press or a drag holds its button; a release and a hover hold none.
    let held = matches!(kind, "pointerdown") || (kind == "pointermove" && with.button.is_some());
    init.set_buttons(if held { 1 << button.max(0) } else { 0 });
    init.set_shift_key(with.shift);
    init.set_ctrl_key(with.ctrl);
    init.set_client_x((rect.left() + f64::from(col) * 8.0 + 4.0) as i32);
    init.set_client_y((rect.top() + f64::from(row) * 16.0 + 8.0) as i32);
    init.set_bubbles(true);
    init.set_cancelable(true);
    let event = PointerEvent::new_with_event_init_dict(kind, &init).unwrap();
    canvas.dispatch_event(&event).unwrap();
    event.default_prevented()
}

/// Type text at the terminal as one committed composition.
fn type_text(surface: &Element, text: &str) {
    let init = CompositionEventInit::new();
    init.set_data(text);
    init.set_bubbles(true);
    let commit = CompositionEvent::new_with_event_init_dict("compositionend", &init).unwrap();
    surface.dispatch_event(&commit).unwrap();
}

/// Press Ctrl+`key` at the terminal.
fn ctrl_key(surface: &Element, key: &str, code: &str) {
    let init = KeyboardEventInit::new();
    init.set_key(key);
    init.set_code(code);
    init.set_ctrl_key(true);
    init.set_bubbles(true);
    init.set_cancelable(true);
    let event = KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &init).unwrap();
    surface.dispatch_event(&event).unwrap();
    assert!(event.default_prevented(), "Ctrl+{key} is terminal input");
}

/// Start a fresh line: Ctrl+U discards whatever earlier tests left typed
/// but unsent in the TTY's line buffer.
fn fresh_line(surface: &Element) {
    ctrl_key(surface, "u", "KeyU");
}

/// Make the pane's `cat` print a line: each of `before` preceded by `ESC`,
/// then `marker`, then each of `after` preceded by `ESC`. The marker, alone
/// on its row, shows the program output (not just the TTY's `^[` echo) has
/// been applied.
async fn program_prints(
    client: &phux_web::client::Client,
    surface: &Element,
    before: &[&str],
    marker: &str,
    after: &[&str],
) {
    fresh_line(surface);
    let escaped = |sequences: &[&str]| {
        for sequence in sequences {
            assert!(keydown(surface, "Escape", "Escape", false));
            type_text(surface, sequence);
        }
    };
    escaped(before);
    type_text(surface, marker);
    escaped(after);
    assert!(keydown(surface, "Enter", "Enter", false));
    let mut shown = false;
    for _ in 0..POLLS {
        if client.rows_text().iter().any(|row| row.trim() == marker) {
            shown = true;
            break;
        }
        sleep(POLL).await;
    }
    assert!(shown, "the program printed {marker}: {}", screen(client));
}

/// Wait until the screen stops changing for a quarter second.
async fn settle(client: &phux_web::client::Client) {
    let mut last = screen(client);
    for _ in 0..POLLS {
        sleep(Duration::from_millis(250)).await;
        let now = screen(client);
        if now == last {
            return;
        }
        last = now;
    }
    panic!("the screen never settled: {last}");
}

/// Wait until at least `count` rows contain `needle`.
async fn wait_rows(client: &phux_web::client::Client, needle: &str, count: usize) -> bool {
    for _ in 0..POLLS {
        if client
            .rows_text()
            .iter()
            .filter(|row| row.contains(needle))
            .count()
            >= count
        {
            return true;
        }
        sleep(POLL).await;
    }
    false
}

/// Row index of the first row equal (trimmed) to `text`.
fn row_of(client: &phux_web::client::Client, text: &str) -> u16 {
    client
        .rows_text()
        .iter()
        .position(|row| row.trim() == text)
        .unwrap_or_else(|| panic!("no row {text}: {}", screen(client))) as u16
}

fn find_bar(canvas: &HtmlCanvasElement) -> Element {
    canvas
        .parent_element()
        .and_then(|host| {
            host.query_selector(&format!(".{}", phux_web::client::FIND_BAR_CLASS))
                .ok()
                .flatten()
        })
        .expect("client mounts a find bar beside the canvas")
}

fn find_count(bar: &Element) -> String {
    bar.query_selector(".phux-find-count")
        .unwrap()
        .unwrap()
        .text_content()
        .unwrap_or_default()
}

/// A background pixel of cell `(col, row)`: its bottom edge, below the glyph.
fn cell_pixel(canvas: &HtmlCanvasElement, col: u16, row: u16) -> [u8; 3] {
    let ctx: CanvasRenderingContext2d = canvas
        .get_context("2d")
        .unwrap()
        .unwrap()
        .dyn_into()
        .unwrap();
    let data = ctx
        .get_image_data(i32::from(col) * 8 + 6, i32::from(row) * 16 + 15, 1, 1)
        .unwrap()
        .data();
    [data[0], data[1], data[2]]
}

#[wasm_bindgen_test]
async fn find_highlights_matches_in_history_and_steps_between_them() {
    let canvas = mounted_canvas("find-canvas");
    let client = phux_web::client::run(WS_URL, canvas.clone(), 80, 24)
        .await
        .expect("connect to live phux server");
    canvas.focus().unwrap();
    let surface = input_surface(&canvas);
    fresh_line(&surface);
    type_text(&surface, "FINDME_ONE");
    assert!(keydown(&surface, "Enter", "Enter", false));
    for _ in 0..30 {
        assert!(keydown(&surface, "Enter", "Enter", false));
    }
    // cat writes its copies of those lines apart from the TTY's echo; let
    // them land so none splits the next line's echo.
    settle(&client).await;
    type_text(&surface, "FINDME_TWO");
    assert!(keydown(&surface, "Enter", "Enter", false));
    // Settled: the TTY echo and cat's copy of the last line are both in.
    assert!(
        wait_rows(&client, "FINDME_TWO", 2).await,
        "{}",
        screen(&client)
    );
    assert!(
        !screen(&client).contains("FINDME_ONE"),
        "scrolled into history: {}",
        screen(&client)
    );

    // Command+F aimed at the page is the browser's; at the terminal, ours.
    let bar = find_bar(&canvas);
    assert!(bar.has_attribute("hidden"), "closed until asked for");
    assert!(keydown(&surface, "f", "KeyF", true), "Command+F opens find");
    assert!(!bar.has_attribute("hidden"));
    let field: HtmlInputElement = bar
        .query_selector("input")
        .unwrap()
        .unwrap()
        .dyn_into()
        .unwrap();
    assert_eq!(
        document().active_element().as_ref(),
        Some(field.as_ref()),
        "the find field has focus"
    );

    field.set_value("findme_");
    let init = EventInit::new();
    init.set_bubbles(true);
    field
        .dispatch_event(&Event::new_with_event_init_dict("input", &init).unwrap())
        .unwrap();
    // Each line shows twice (the TTY echo, then cat): four matches, the
    // newest current and on screen.
    assert_eq!(
        find_count(&bar),
        "4 of 4",
        "case-insensitive, whole history"
    );
    assert!(wait_until(&client, true, "FINDME_TWO").await);

    // Enter steps to older matches, scrolling history into view.
    assert!(keydown(&field, "Enter", "Enter", false));
    assert!(keydown(&field, "Enter", "Enter", false));
    assert_eq!(find_count(&bar), "2 of 4");
    assert!(
        wait_until(&client, true, "FINDME_ONE").await,
        "{}",
        screen(&client)
    );
    // The current match paints in the current-match color, the other
    // match on screen in the match color.
    let rows = client.rows_text();
    let mut marks = Vec::new();
    for _ in 0..POLLS {
        marks = rows
            .iter()
            .enumerate()
            .filter_map(|(r, text)| {
                text.find("FINDME_ONE")
                    .map(|c| cell_pixel(&canvas, c as u16, r as u16))
            })
            .collect();
        if marks == [[0x8a, 0x72, 0x1c], [0xf2, 0xb1, 0x3a]] {
            break;
        }
        sleep(POLL).await;
    }
    assert_eq!(
        marks,
        [[0x8a, 0x72, 0x1c], [0xf2, 0xb1, 0x3a]],
        "the echo's match, then the current (cat's copy), highlighted"
    );

    // Escape closes find and gives the terminal its keys back.
    assert!(keydown(&field, "Escape", "Escape", false));
    assert!(bar.has_attribute("hidden"));
    assert_eq!(document().active_element().as_ref(), Some(&surface));
    assert!(keydown(&surface, "Enter", "Enter", false), "typing resumes");
    client.close();
    assert!(
        canvas
            .parent_element()
            .unwrap()
            .query_selector(&format!(".{}", phux_web::client::FIND_BAR_CLASS))
            .unwrap()
            .is_none(),
        "closing the client removes its find bar"
    );
}

#[wasm_bindgen_test]
async fn mouse_reports_reach_a_tracking_program_and_shift_drag_still_selects() {
    let canvas = mounted_canvas("mouse-canvas");
    let client = phux_web::client::run(WS_URL, canvas.clone(), 80, 24)
        .await
        .expect("connect to live phux server");
    canvas.focus().unwrap();
    let surface = input_surface(&canvas);
    program_prints(&client, &surface, &["[?1002;1006h"], "MOUSE_ON", &[]).await;

    // A click is a press and a release at the cell, 1-based, in SGR form;
    // the TTY echoes the reports the server wrote to the pane.
    pointer(&canvas, "pointerdown", 4, 2);
    pointer(&canvas, "pointerup", 4, 2);
    assert!(
        wait_for(&client, "[<0;5;3M").await,
        "press: {}",
        screen(&client)
    );
    assert!(
        wait_for(&client, "[<0;5;3m").await,
        "release: {}",
        screen(&client)
    );
    // A drag reports motion with the button held (1002).
    pointer_with(&canvas, "pointerdown", (6, 2), Pointer::default());
    pointer_with(
        &canvas,
        "pointermove",
        (7, 2),
        Pointer {
            button: Some(0),
            ..Pointer::default()
        },
    );
    pointer_with(&canvas, "pointerup", (7, 2), Pointer::default());
    assert!(
        wait_for(&client, "[<32;8;3M").await,
        "drag: {}",
        screen(&client)
    );
    // The wheel is buttons 4 and 5; a right-click is the program's too.
    let init = WheelEventInit::new();
    init.set_delta_y(-100.0);
    init.set_bubbles(true);
    init.set_cancelable(true);
    let rect = canvas.get_bounding_client_rect();
    init.set_client_x((rect.left() + 12.0) as i32);
    init.set_client_y((rect.top() + 40.0) as i32);
    canvas
        .dispatch_event(&WheelEvent::new_with_event_init_dict("wheel", &init).unwrap())
        .unwrap();
    assert!(
        wait_for(&client, "[<64;2;3M").await,
        "wheel up: {}",
        screen(&client)
    );
    let init = web_sys::MouseEventInit::new();
    init.set_bubbles(true);
    init.set_cancelable(true);
    let menu = web_sys::MouseEvent::new_with_mouse_event_init_dict("contextmenu", &init).unwrap();
    canvas.dispatch_event(&menu).unwrap();
    assert!(
        menu.default_prevented(),
        "the program owns the right button"
    );

    // Shift+drag selects locally, reporting nothing.
    let row = row_of(&client, "MOUSE_ON");
    let shift = Pointer {
        shift: true,
        ..Pointer::default()
    };
    pointer_with(&canvas, "pointerdown", (0, row), shift);
    pointer_with(
        &canvas,
        "pointermove",
        (7, row),
        Pointer {
            button: Some(0),
            ..shift
        },
    );
    pointer_with(&canvas, "pointerup", (7, row), shift);
    assert_eq!(copy_event(&surface), (true, "MOUSE_ON".to_owned()));

    program_prints(&client, &surface, &["[?1002;1006l"], "MOUSE_OFF", &[]).await;
    pointer(&canvas, "pointerdown", 9, 9);
    pointer(&canvas, "pointerup", 9, 9);
    sleep(Duration::from_millis(300)).await;
    assert!(
        !screen(&client).contains(";10;10"),
        "no reports once the program stops tracking: {}",
        screen(&client)
    );
    client.close();
}

#[wasm_bindgen_test]
async fn modifier_click_opens_http_links_in_a_new_tab_and_nothing_else() {
    let opened: Rc<RefCell<Vec<(String, String, String)>>> = Rc::default();
    let record = Rc::clone(&opened);
    let stub = Closure::<dyn FnMut(String, String, String) -> JsValue>::new(
        move |url: String, target: String, features: String| {
            record.borrow_mut().push((url, target, features));
            JsValue::NULL
        },
    );
    let window = web_sys::window().unwrap();
    let real_open = js_sys::Reflect::get(&window, &"open".into()).unwrap();
    js_sys::Reflect::set(&window, &"open".into(), stub.as_ref()).unwrap();

    let canvas = mounted_canvas("link-canvas");
    let client = phux_web::client::run(WS_URL, canvas.clone(), 80, 24)
        .await
        .expect("connect to live phux server");
    canvas.focus().unwrap();
    let surface = input_surface(&canvas);
    // OSC 8 open and close, each terminated by ESC \.
    let close = ["]8;;", "\\"];
    program_prints(
        &client,
        &surface,
        &["]8;;https://example.com/osc8", "\\"],
        "OSCLINK",
        &close,
    )
    .await;
    program_prints(
        &client,
        &surface,
        &["]8;;javascript:alert(1)", "\\"],
        "BADLINK",
        &close,
    )
    .await;
    fresh_line(&surface);
    type_text(&surface, "see https://example.com/plain.");
    assert!(keydown(&surface, "Enter", "Enter", false));
    // Settled (echo and cat's copy both in), so rows stop moving.
    assert!(wait_rows(&client, "https://example.com/plain.", 2).await);
    let osc_row = row_of(&client, "OSCLINK");
    let bad_row = row_of(&client, "BADLINK");
    let rows = client.rows_text();
    let (plain_row, plain_col) = rows
        .iter()
        .enumerate()
        .find_map(|(row, text)| {
            text.find("https://example.com/plain")
                .map(|col| (row as u16, col as u16))
        })
        .unwrap();

    let ctrl = Pointer {
        ctrl: true,
        ..Pointer::default()
    };
    // A plain click on a link selects and opens nothing.
    pointer(&canvas, "pointerdown", 2, osc_row);
    pointer(&canvas, "pointerup", 2, osc_row);
    assert!(opened.borrow().is_empty(), "no modifier, no navigation");
    assert!(
        pointer_with(&canvas, "pointerdown", (2, osc_row), ctrl),
        "the link took the click"
    );
    pointer_with(&canvas, "pointerup", (2, osc_row), ctrl);
    assert!(
        !pointer_with(&canvas, "pointerdown", (2, bad_row), ctrl),
        "javascript: never opens"
    );
    pointer_with(&canvas, "pointerup", (2, bad_row), ctrl);
    pointer_with(&canvas, "pointerdown", (plain_col + 10, plain_row), ctrl);
    pointer_with(&canvas, "pointerup", (plain_col + 10, plain_row), ctrl);

    js_sys::Reflect::set(&window, &"open".into(), &real_open).unwrap();
    let opened = opened.borrow();
    let expect = |url: &str| {
        (
            url.to_owned(),
            "_blank".to_owned(),
            "noopener,noreferrer".to_owned(),
        )
    };
    assert_eq!(
        *opened,
        [
            expect("https://example.com/osc8"),
            expect("https://example.com/plain")
        ],
        "the OSC 8 target and the plain URL, without its trailing period"
    );
    client.close();
}

#[wasm_bindgen_test]
async fn the_bell_announces_itself_and_flashes_the_canvas() {
    let canvas = mounted_canvas("bell-canvas");
    let rung = Rc::new(Cell::new(0));
    let count = Rc::clone(&rung);
    let listener = Closure::<dyn FnMut()>::new(move || count.set(count.get() + 1));
    canvas
        .add_event_listener_with_callback(
            phux_web::client::BELL_EVENT,
            listener.as_ref().unchecked_ref(),
        )
        .unwrap();
    let client = phux_web::client::run(WS_URL, canvas.clone(), 80, 24)
        .await
        .expect("connect to live phux server");
    canvas.focus().unwrap();
    let surface = input_surface(&canvas);
    sleep(Duration::from_millis(300)).await;
    let background = cell_pixel(&canvas, 79, 23);
    assert_eq!(rung.get(), 0, "attaching does not ring");

    // Ctrl+G is BEL: the TTY echoes `^G`, then cat rings it.
    fresh_line(&surface);
    ctrl_key(&surface, "g", "KeyG");
    assert!(keydown(&surface, "Enter", "Enter", false));
    let mut flashed = false;
    for _ in 0..POLLS * 4 {
        flashed |= rung.get() > 0 && cell_pixel(&canvas, 79, 23) != background;
        if flashed {
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(rung.get(), 1, "one phux-bell event");
    assert!(flashed, "the canvas flashed");
    sleep(Duration::from_millis(400)).await;
    assert_eq!(cell_pixel(&canvas, 79, 23), background, "the flash ends");
    client.close();
}

#[wasm_bindgen_test]
async fn a_double_width_character_copies_without_a_trailing_space() {
    let canvas = mounted_canvas("wide-canvas");
    let client = phux_web::client::run(WS_URL, canvas.clone(), 80, 24)
        .await
        .expect("connect to live phux server");
    canvas.focus().unwrap();
    let surface = input_surface(&canvas);
    fresh_line(&surface);
    type_text(&surface, "\u{4e2d}\u{6587}Z");
    assert!(wait_for(&client, "\u{4e2d}\u{6587}Z").await);
    let rows = client.rows_text();
    let (row, first) = rows
        .iter()
        .enumerate()
        .find_map(|(row, text)| {
            text.chars()
                .position(|ch| ch == '\u{4e2d}')
                .map(|col| (row as u16, col as u16))
        })
        .unwrap();
    // Cells: the two wide characters and their spacers, then "Z".
    pointer(&canvas, "pointerdown", first, row);
    pointer(&canvas, "pointermove", first + 4, row);
    pointer(&canvas, "pointerup", first + 4, row);
    assert_eq!(copy_event(&surface), (true, "\u{4e2d}\u{6587}Z".to_owned()));
    pointer(&canvas, "pointerdown", first, row);
    pointer(&canvas, "pointermove", first + 1, row);
    pointer(&canvas, "pointerup", first + 1, row);
    assert_eq!(
        copy_event(&surface),
        (true, "\u{4e2d}".to_owned()),
        "a wide character alone copies without its spacer"
    );
    assert!(keydown(&surface, "Enter", "Enter", false));
    client.close();
}

/// How many times `needle` shows on screen.
fn occurrences(client: &phux_web::client::Client, needle: &str) -> usize {
    screen(client).matches(needle).count()
}

/// Wait until `needle` shows more than `before` times.
async fn wait_more(client: &phux_web::client::Client, needle: &str, before: usize) -> bool {
    for _ in 0..POLLS {
        if occurrences(client, needle) > before {
            return true;
        }
        sleep(POLL).await;
    }
    false
}

#[wasm_bindgen_test]
async fn focus_changes_reach_a_program_that_asks_for_them() {
    let canvas = mounted_canvas("focus-canvas");
    let client = phux_web::client::run(WS_URL, canvas.clone(), 80, 24)
        .await
        .expect("connect to live phux server");
    canvas.focus().unwrap();
    let surface: web_sys::HtmlElement = input_surface(&canvas).dyn_into().unwrap();
    program_prints(&client, &surface, &["[?1004h"], "FOCUS_ON", &[]).await;

    // The server writes CSI O and CSI I to the pane; the TTY echoes them.
    let (lost, gained) = (occurrences(&client, "^[[O"), occurrences(&client, "^[[I"));
    surface.blur().unwrap();
    assert!(
        wait_more(&client, "^[[O", lost).await,
        "focus out: {}",
        screen(&client)
    );
    surface.focus().unwrap();
    assert!(
        wait_more(&client, "^[[I", gained).await,
        "focus in: {}",
        screen(&client)
    );

    program_prints(&client, &surface, &["[?1004l"], "FOCUS_OFF", &[]).await;
    let (lost, gained) = (occurrences(&client, "^[[O"), occurrences(&client, "^[[I"));
    surface.blur().unwrap();
    surface.focus().unwrap();
    sleep(Duration::from_millis(300)).await;
    assert_eq!(
        (occurrences(&client, "^[[O"), occurrences(&client, "^[[I")),
        (lost, gained),
        "no reports once the program stops asking: {}",
        screen(&client)
    );
    client.close();
}

fn wheel(canvas: &HtmlCanvasElement, delta_y: f64) {
    let init = WheelEventInit::new();
    init.set_delta_y(delta_y);
    init.set_bubbles(true);
    init.set_cancelable(true);
    let event = WheelEvent::new_with_event_init_dict("wheel", &init).unwrap();
    canvas.dispatch_event(&event).unwrap();
    assert!(event.default_prevented(), "the page does not scroll");
}

#[wasm_bindgen_test]
async fn the_wheel_on_the_alternate_screen_is_arrow_keys() {
    let canvas = mounted_canvas("alt-wheel-canvas");
    let client = phux_web::client::run(WS_URL, canvas.clone(), 80, 24)
        .await
        .expect("connect to live phux server");
    canvas.focus().unwrap();
    let surface = input_surface(&canvas);
    program_prints(&client, &surface, &["[?1049h"], "ALT_ON", &[]).await;

    // Three rows of travel up, two down: one arrow key each, which the
    // TTY echoes.
    wheel(&canvas, -48.0);
    assert!(
        wait_for(&client, "^[[A^[[A^[[A").await,
        "wheel up: {}",
        screen(&client)
    );
    wheel(&canvas, 32.0);
    assert!(
        wait_for(&client, "^[[A^[[A^[[A^[[B^[[B").await,
        "wheel down: {}",
        screen(&client)
    );

    // A program that clears alternate scroll (DECSET 1007) gets nothing.
    program_prints(&client, &surface, &["[?1007l"], "ALT_SCROLL_OFF", &[]).await;
    let arrows = occurrences(&client, "^[[A");
    wheel(&canvas, -48.0);
    sleep(Duration::from_millis(300)).await;
    assert_eq!(occurrences(&client, "^[[A"), arrows, "{}", screen(&client));

    // Back to the primary screen, where the wheel is scrollback again.
    program_prints(&client, &surface, &["[?1007h", "[?1049l"], "ALT_OFF", &[]).await;
    let arrows = occurrences(&client, "^[[A");
    wheel(&canvas, -48.0);
    sleep(Duration::from_millis(300)).await;
    assert_eq!(occurrences(&client, "^[[A"), arrows, "{}", screen(&client));
    client.close();
}

/// A second connection that attaches reporting `cell_px` cells, as another
/// client (a desktop or TUI at another font size) does, then stays.
async fn attach_with_cells(cell_px: (u16, u16)) -> web_sys::WebSocket {
    use phux_protocol::wire::frame::FrameKind;

    let vt = phux_vt_web::Vt::load().await.expect("load engine");
    let session = Rc::new(RefCell::new(phux_web::Session::new(&vt, 80, 24)));
    session.borrow_mut().set_cell_size(cell_px.0, cell_px.1);
    let socket = web_sys::WebSocket::new(WS_URL).unwrap();
    socket.set_binary_type(web_sys::BinaryType::Arraybuffer);
    let attached = Rc::new(Cell::new(false));
    let send = {
        let socket = socket.clone();
        move |frames: Vec<Vec<u8>>| {
            for frame in frames {
                socket.send_with_u8_array(&frame).unwrap();
            }
        }
    };
    let open = {
        let (session, send) = (Rc::clone(&session), send.clone());
        Closure::<dyn FnMut()>::new(move || send(session.borrow().handshake()))
    };
    let message = {
        let (session, attached) = (Rc::clone(&session), Rc::clone(&attached));
        Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
            let bytes = js_sys::Uint8Array::new(&event.data()).to_vec();
            match FrameKind::decode(&bytes) {
                Ok((frame @ FrameKind::HelloOk { .. }, _)) => {
                    send(session.borrow_mut().on_frame(frame).send);
                }
                Ok((FrameKind::Attached { .. }, _)) => attached.set(true),
                _ => {}
            }
        })
    };
    socket.set_onopen(Some(open.as_ref().unchecked_ref()));
    socket.set_onmessage(Some(message.as_ref().unchecked_ref()));
    open.forget();
    message.forget();
    for _ in 0..POLLS {
        if attached.get() {
            return socket;
        }
        sleep(POLL).await;
    }
    panic!("the second connection never attached");
}

/// The server divides mouse positions by the cell size of the most recent
/// viewport that reported pixels. The browser reports its own, so another
/// client's cell size does not move its clicks off their cells.
#[wasm_bindgen_test]
async fn mouse_positions_land_on_their_cells_after_another_client_reports_other_cells() {
    let other = attach_with_cells((10, 20)).await;
    let canvas = mounted_canvas("cells-canvas");
    let client = phux_web::client::run(WS_URL, canvas.clone(), 80, 24)
        .await
        .expect("connect to live phux server");
    canvas.focus().unwrap();
    let surface = input_surface(&canvas);
    program_prints(&client, &surface, &["[?1000;1006h"], "CELLS_ON", &[]).await;
    pointer(&canvas, "pointerdown", 4, 2);
    pointer(&canvas, "pointerup", 4, 2);
    assert!(
        wait_for(&client, "[<0;5;3m").await,
        "the click lands on column 5, row 3: {}",
        screen(&client)
    );
    program_prints(&client, &surface, &["[?1000;1006l"], "CELLS_OFF", &[]).await;
    other.close().unwrap();
    client.close();
}

fn copy_event(surface: &Element) -> (bool, String) {
    let clipboard = DataTransfer::new().unwrap();
    let init = ClipboardEventInit::new();
    init.set_clipboard_data(Some(&clipboard));
    init.set_bubbles(true);
    init.set_cancelable(true);
    let copy = ClipboardEvent::new_with_event_init_dict("copy", &init).unwrap();
    surface.dispatch_event(&copy).unwrap();
    (
        copy.default_prevented(),
        clipboard.get_data("text/plain").unwrap_or_default(),
    )
}
