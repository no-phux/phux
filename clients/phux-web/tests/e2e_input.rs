//! Headless Chrome against the live `ws_demo_server`: keyboard, IME commit,
//! and clipboard paste reach the terminal through the client's input surface,
//! and nothing else on the page is captured; the wheel pages scrollback and a
//! drag selects text to copy. The seeded pane runs `sleep` on
//! a cooked TTY, so the line discipline echoes whatever bytes arrive.

use std::time::Duration;

use gloo_timers::future::sleep;
use wasm_bindgen::JsCast;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{
    ClipboardEvent, ClipboardEventInit, CompositionEvent, CompositionEventInit, DataTransfer,
    Document, Element, HtmlCanvasElement, KeyboardEvent, KeyboardEventInit, PointerEvent,
    PointerEventInit, WheelEvent, WheelEventInit,
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

    // Command/Super chords are the browser's (find, reload, copy, paste).
    assert!(
        !keydown(&surface, "f", "KeyF", true),
        "Command+F left to the browser"
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
    let rect = canvas.get_bounding_client_rect();
    let init = PointerEventInit::new();
    init.set_pointer_id(1);
    init.set_button(if kind == "pointermove" { -1 } else { 0 });
    init.set_client_x((rect.left() + f64::from(col) * 8.0 + 4.0) as i32);
    init.set_client_y((rect.top() + f64::from(row) * 16.0 + 8.0) as i32);
    init.set_bubbles(true);
    let event = PointerEvent::new_with_event_init_dict(kind, &init).unwrap();
    canvas.dispatch_event(&event).unwrap();
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
