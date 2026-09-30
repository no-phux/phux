//! Headless Chrome against the live `ws_demo_server`: keyboard, IME commit,
//! and clipboard paste reach the terminal through the client's input surface,
//! and nothing else on the page is captured. The seeded pane runs `sleep` on
//! a cooked TTY, so the line discipline echoes whatever bytes arrive.

use std::time::Duration;

use gloo_timers::future::sleep;
use wasm_bindgen::JsCast;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{
    ClipboardEvent, ClipboardEventInit, CompositionEvent, CompositionEventInit, DataTransfer,
    Document, Element, HtmlCanvasElement, KeyboardEvent, KeyboardEventInit,
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
