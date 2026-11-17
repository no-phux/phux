//! Headless Chrome against the live `ws_demo_server`: keyboard, IME commit,
//! and clipboard paste reach the terminal through the client's input surface,
//! and nothing else on the page is captured; the wheel pages scrollback and a
//! drag selects text to copy; find, mouse and focus reporting, the
//! alternate screen's wheel, links, and the bell. The runner starts the
//! server with `PHUX_DEMO_PANE=cat`, so the seeded pane runs `cat` on a
//! cooked TTY: the line discipline echoes whatever bytes arrive (control
//! bytes as `^X`), and each finished line comes back raw as program output,
//! which is how these tests make the "program" enable mouse modes, print
//! OSC 8 links, and ring the bell.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gloo_timers::future::sleep;
use phux_web::client::Client;
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

#[path = "browser_support/metrics.rs"]
mod metrics;
#[path = "browser_support/page.rs"]
mod page;
use page::{MARKER, POLLS, poll};

const WS_URL: &str = match option_env!("PHUX_TEST_WS_URL") {
    Some(url) => url,
    None => "ws://127.0.0.1:47654/",
};
/// How long a finger rests before the drag selects. The gesture treats 400ms
/// as a hold.
const HOLD: Duration = Duration::from_millis(450);

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

async fn connect(canvas: &HtmlCanvasElement) -> Client {
    phux_web::client::run(WS_URL, canvas.clone(), 80, 24)
        .await
        .expect("connect to live phux server")
}

/// A mounted, connected 80x24 canvas and its input surface.
async fn connected(id: &str) -> (HtmlCanvasElement, Client, Element) {
    let canvas = mounted_canvas(id);
    let client = connect(&canvas).await;
    let surface = input_surface(&canvas);
    (canvas, client, surface)
}

async fn wait_for(client: &Client, needle: &str) -> bool {
    // Wide glyphs leave a spacer cell; compare with blanks removed.
    poll(|| {
        let screen: String = client.rows_text().concat().replace([' ', '\0'], "");
        screen.contains(needle)
    })
    .await
}

fn screen(client: &Client) -> String {
    client.rows_text().join("\n")
}

fn shows(client: &Client, needle: &str) -> bool {
    client.rows_text().iter().any(|row| row.contains(needle))
}

async fn wait_until(client: &Client, visible: bool, needle: &str) -> bool {
    poll(|| shows(client, needle) == visible).await
}

/// Wait until at least `count` rows contain `needle`.
async fn wait_rows(client: &Client, needle: &str, count: usize) -> bool {
    poll(|| {
        let rows = client.rows_text();
        rows.iter().filter(|row| row.contains(needle)).count() >= count
    })
    .await
}

/// How many times `needle` shows on screen.
fn occurrences(client: &Client, needle: &str) -> usize {
    screen(client).matches(needle).count()
}

/// Wait until `needle` shows more than `before` times.
async fn wait_more(client: &Client, needle: &str, before: usize) -> bool {
    poll(|| occurrences(client, needle) > before).await
}

/// Dispatch a keydown; returns whether the client took it.
fn key_with(
    target: &Element,
    key: &str,
    code: &str,
    mods: impl FnOnce(&KeyboardEventInit),
) -> bool {
    let init = KeyboardEventInit::new();
    init.set_key(key);
    init.set_code(code);
    mods(&init);
    init.set_bubbles(true);
    init.set_cancelable(true);
    let event = KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &init).unwrap();
    target.dispatch_event(&event).unwrap();
    event.default_prevented()
}

fn keydown(target: &Element, key: &str, code: &str, meta: bool) -> bool {
    key_with(target, key, code, |init| init.set_meta_key(meta))
}

fn enter(surface: &Element) {
    assert!(keydown(surface, "Enter", "Enter", false), "Enter is input");
}

/// Press Ctrl+`key` at the terminal.
fn ctrl_key(surface: &Element, key: &str, code: &str) {
    let taken = key_with(surface, key, code, |init| init.set_ctrl_key(true));
    assert!(taken, "Ctrl+{key} is terminal input");
}

/// The hidden input surface the client places beside its canvas.
fn input_surface(canvas: &HtmlCanvasElement) -> Element {
    beside(canvas, "textarea.phux-web-input")
        .expect("client mounts a textarea input surface beside the canvas")
}

/// The element matching `selector` in the canvas's host.
fn beside(canvas: &HtmlCanvasElement, selector: &str) -> Option<Element> {
    canvas.parent_element()?.query_selector(selector).ok()?
}

/// Type text at the terminal as one committed composition.
fn type_text(surface: &Element, text: &str) {
    let init = CompositionEventInit::new();
    init.set_data(text);
    init.set_bubbles(true);
    let commit = CompositionEvent::new_with_event_init_dict("compositionend", &init).unwrap();
    surface.dispatch_event(&commit).unwrap();
}

/// Start a fresh line: Ctrl+U discards whatever earlier tests left typed
/// but unsent in the TTY's line buffer.
fn fresh_line(surface: &Element) {
    ctrl_key(surface, "u", "KeyU");
}

/// Type `text` on a fresh line and press Enter.
fn type_line(surface: &Element, text: &str) {
    fresh_line(surface);
    type_text(surface, text);
    enter(surface);
}

/// Make the pane's `cat` print a line: each of `before` preceded by `ESC`,
/// then `marker`, then each of `after` preceded by `ESC`. The marker, alone
/// on its row, shows the program output (not just the TTY's `^[` echo) has
/// been applied.
async fn program_prints(
    client: &Client,
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
    enter(surface);
    let shown = poll(|| client.rows_text().iter().any(|row| row.trim() == marker)).await;
    assert!(shown, "the program printed {marker}: {}", screen(client));
}

/// Wait until the screen stops changing for a quarter second.
async fn settle(client: &Client) {
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

/// The 80×24 grid `run` requested, once an attach-time reflow has landed.
///
/// A previous test's geometry can still be in flight. Its snapshot replaces
/// the replica, so a marker is printed only after the grid is that size and
/// the screen has stopped changing.
async fn settled_80x24(client: &Client) {
    let sized = poll(|| {
        let rows = client.rows_text();
        rows.len() == 24 && rows.iter().all(|row| row.chars().count() == 80)
    })
    .await;
    assert!(
        sized,
        "the pane is 80x24 before the marker is printed: {}",
        screen(client)
    );
    settle(client).await;
}

/// Finish the shared `cat` pane's echo of a burst of Enters before scrolling.
///
/// A live chunk older than the server's stale-output budget (250ms) makes the
/// pump skip that client to a fresh snapshot. A debug CI build falls that far
/// behind while `cat` is still echoing, and the snapshot replaces a scrolled
/// replica with the live screen: a hold copies blank cells, and until the
/// snapshot lands the replica is frozen so a keydown is not handled. The
/// sentinel is committed once the replica accepts it (a frozen one drops the
/// key), and it is on screen twice (the tty echo and `cat`) before the view
/// is allowed to go still.
async fn drain_echo(client: &Client, surface: &Element) {
    let sentinel = format!("PHUX_DRAIN_{}", js_sys::Date::now() as u64);
    let submitted = poll(|| {
        type_text(surface, &sentinel);
        keydown(surface, "Enter", "Enter", false)
    })
    .await;
    assert!(
        submitted,
        "drain sentinel is terminal input: {}",
        screen(client)
    );
    assert!(
        wait_rows(client, &sentinel, 2).await,
        "echo drained ({sentinel}): {}",
        screen(client)
    );
    settle(client).await;
}

/// Push `marker` off the live screen with a burst of Enters, and wait for
/// the echo to finish. The tty echoes each Enter as a newline.
async fn scroll_away(client: &Client, surface: &Element, marker: &str) {
    for _ in 0..40 {
        enter(surface);
    }
    assert!(
        wait_until(client, false, marker).await,
        "marker scrolled away: {}",
        screen(client)
    );
    // Scroll only after `cat` has printed a sentinel, or a stale snapshot
    // lands mid-gesture and freezes input.
    drain_echo(client, surface).await;
}

/// The row of `marker` and its first and last columns.
fn marker_cells(client: &Client, marker: &str) -> (u16, u16, u16) {
    let rows = client.rows_text();
    let row = rows.iter().position(|r| r.contains(marker)).unwrap();
    let col = rows[row].find(marker).unwrap();
    (row as u16, col as u16, (col + marker.len() - 1) as u16)
}

/// Row index of the first row equal (trimmed) to `text`.
fn row_of(client: &Client, text: &str) -> u16 {
    client
        .rows_text()
        .iter()
        .position(|row| row.trim() == text)
        .unwrap_or_else(|| panic!("no row {text}: {}", screen(client))) as u16
}

/// The newest row containing `needle`, and the column it starts at.
fn newest(client: &Client, needle: &str) -> (u16, u16) {
    let rows = client.rows_text();
    rows.iter()
        .enumerate()
        .rev()
        .find_map(|(row, text)| text.find(needle).map(|col| (row as u16, col as u16)))
        .unwrap_or_else(|| panic!("no row with {needle}: {}", screen(client)))
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
    let cell = metrics::measured(canvas);
    let dpr = web_sys::window().unwrap().device_pixel_ratio();
    let data = ctx
        .get_image_data(
            (((f64::from(col) + 1.0) * cell.cell_w - 2.0) * dpr) as i32,
            (((f64::from(row) + 1.0) * cell.cell_h - 1.0) * dpr) as i32,
            1,
            1,
        )
        .unwrap()
        .data();
    [data[0], data[1], data[2]]
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

const NOTHING: (bool, String) = (false, String::new());

fn copied(text: &str) -> (bool, String) {
    (true, text.to_owned())
}

/// `window.open`, replaced by a recorder of `(url, target, features)` until
/// [`Self::restore`].
struct OpenStub {
    opened: Rc<RefCell<Vec<(String, String, String)>>>,
    real: JsValue,
    _stub: Closure<dyn FnMut(String, String, String) -> JsValue>,
}

impl OpenStub {
    fn install() -> Self {
        let opened: Rc<RefCell<Vec<_>>> = Rc::default();
        let record = Rc::clone(&opened);
        let stub = Closure::<dyn FnMut(String, String, String) -> JsValue>::new(
            move |url, target, features| {
                record.borrow_mut().push((url, target, features));
                JsValue::NULL
            },
        );
        let window = web_sys::window().unwrap();
        let real = js_sys::Reflect::get(&window, &"open".into()).unwrap();
        js_sys::Reflect::set(&window, &"open".into(), stub.as_ref()).unwrap();
        Self {
            opened,
            real,
            _stub: stub,
        }
    }

    fn restore(&self) -> Vec<(String, String, String)> {
        let window = web_sys::window().unwrap();
        js_sys::Reflect::set(&window, &"open".into(), &self.real).unwrap();
        self.opened.borrow().clone()
    }
}

#[wasm_bindgen_test]
async fn keys_ime_commits_and_paste_reach_the_terminal_and_nothing_else_is_captured() {
    let (canvas, client, surface) = connected("input-canvas").await;

    // Focusing the canvas (what an embedder does) lands on the input surface.
    canvas.focus().unwrap();
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
    type_text(&surface, "\u{65e5}\u{672c}");
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
        beside(&canvas, "textarea.phux-web-input").is_none(),
        "closing the client removes its input surface"
    );
}

#[wasm_bindgen_test]
async fn scrollback_pages_by_wheel_and_shift_page_up_and_a_drag_copies_from_it() {
    let (canvas, client, surface) = connected("scrollback-canvas").await;
    let marker = MARKER;
    assert!(
        wait_until(&client, true, marker).await,
        "{}",
        screen(&client)
    );
    scroll_away(&client, &surface, marker).await;

    wheel(&canvas, -4_000.0);
    assert!(
        wait_until(&client, true, marker).await,
        "wheel reached the marker in scrollback: {}",
        screen(&client)
    );

    // Typing returns to the live screen.
    enter(&surface);
    assert!(
        wait_until(&client, false, marker).await,
        "typing returned to live: {}",
        screen(&client)
    );

    // Shift+PageUp pages the local scrollback and never reaches the app.
    for _ in 0..4 {
        let taken = key_with(&surface, "PageUp", "PageUp", |init| {
            init.set_shift_key(true)
        });
        assert!(taken, "Shift+PageUp is consumed");
    }
    assert!(
        wait_until(&client, true, marker).await,
        "Shift+PageUp reached the marker: {}",
        screen(&client)
    );

    // Nothing selected: copy is left to the browser.
    assert_eq!(copy_event(&surface), NOTHING);
    // Drag across the marker in the scrolled-back view and copy it.
    let (row, first, last) = marker_cells(&client, marker);
    // A cancelled pointer (a touch the browser took for panning) ends the
    // drag: later moves select nothing.
    pointer(&canvas, "pointerdown", first, row);
    pointer(&canvas, "pointercancel", first, row);
    pointer(&canvas, "pointermove", last, row);
    assert_eq!(copy_event(&surface), NOTHING);
    pointer(&canvas, "pointerdown", first, row);
    pointer(&canvas, "pointermove", first + 3, row);
    pointer(&canvas, "pointermove", last, row);
    pointer(&canvas, "pointerup", last, row);
    assert_eq!(
        copy_event(&surface),
        copied(marker),
        "the drag copied the marker"
    );
    // A click without a drag clears the selection.
    pointer(&canvas, "pointerdown", 0, 0);
    pointer(&canvas, "pointerup", 0, 0);
    assert_eq!(copy_event(&surface), NOTHING);

    restore_seeded_marker(&client, &surface).await;
    client.close();
}

/// The Enters pushed the seeded marker off the live screen of the shared
/// pane; echo it back so suites that run later against the same server
/// still find it.
async fn restore_seeded_marker(client: &Client, surface: &Element) {
    type_line(surface, MARKER);
    assert!(wait_until(client, true, MARKER).await, "{}", screen(client));
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

const CTRL: Pointer = Pointer {
    button: None,
    shift: false,
    ctrl: true,
};

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
    let cell = metrics::measured(canvas);
    init.set_client_x((rect.left() + (f64::from(col) + 0.5) * cell.cell_w) as i32);
    init.set_client_y((rect.top() + (f64::from(row) + 0.5) * cell.cell_h) as i32);
    init.set_bubbles(true);
    init.set_cancelable(true);
    let event = PointerEvent::new_with_event_init_dict(kind, &init).unwrap();
    canvas.dispatch_event(&event).unwrap();
    event.default_prevented()
}

/// Press, drag to, and release at the cells, with `with`'s modifiers.
fn drag(canvas: &HtmlCanvasElement, from: (u16, u16), to: (u16, u16), with: Pointer) {
    pointer_with(canvas, "pointerdown", from, with);
    let held = Pointer {
        button: Some(0),
        ..with
    };
    pointer_with(canvas, "pointermove", to, held);
    pointer_with(canvas, "pointerup", to, with);
}

/// Press and release at the cell; returns whether the press was taken.
fn click(canvas: &HtmlCanvasElement, cell: (u16, u16), with: Pointer) -> bool {
    let taken = pointer_with(canvas, "pointerdown", cell, with);
    pointer_with(canvas, "pointerup", cell, with);
    taken
}

/// Whether a Ctrl hover at `cell` is a link the client would open.
///
/// `hover_link` runs the click's hit test (`cell_under` + `link_at`) and
/// sets the canvas cursor only when that laid-out cell is an allowed link.
/// A miss clears it, so the cursor after this move is the result of this
/// probe, not a previous one.
fn hover_sees_link(canvas: &HtmlCanvasElement, cell: (u16, u16)) -> bool {
    pointer_with(canvas, "pointermove", cell, CTRL);
    canvas.style().get_property_value("cursor").ok().as_deref() == Some("pointer")
}

/// Newest viewport row whose trimmed text is `marker`, once a Ctrl hover at
/// `col` hit-tests it as a link. An older copy of the marker can stay on
/// screen after it stops hit-testing as a link; the line just printed is
/// the last one, and it is not clickable until that hover succeeds.
async fn wait_newest_marker_link(
    canvas: &HtmlCanvasElement,
    client: &Client,
    marker: &str,
    col: u16,
) -> Option<u16> {
    let mut found = None;
    poll(|| {
        let rows = client.rows_text();
        found = rows
            .iter()
            .rposition(|text| text.trim() == marker)
            .map(|row| row as u16)
            .filter(|row| hover_sees_link(canvas, (col, *row)));
        found.is_some()
    })
    .await;
    found
}

#[wasm_bindgen_test]
async fn find_highlights_matches_in_history_and_steps_between_them() {
    let (canvas, client, surface) = connected("find-canvas").await;
    canvas.focus().unwrap();
    type_line(&surface, "FINDME_ONE");
    // Keep the echo and cat's copy together before flooding scrollback;
    // otherwise the blank-line echoes can put them more than a viewport apart.
    assert!(
        wait_rows(&client, "FINDME_ONE", 2).await,
        "the echo and cat's copy before scrolling: {}",
        screen(&client)
    );
    for _ in 0..30 {
        enter(&surface);
    }
    // cat writes its copies of those lines apart from the TTY's echo; let
    // them land so none splits the next line's echo.
    settle(&client).await;
    type_text(&surface, "FINDME_TWO");
    enter(&surface);
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
    let bar_selector = format!(".{}", phux_web::client::FIND_BAR_CLASS);
    let bar = beside(&canvas, &bar_selector).expect("client mounts a find bar beside the canvas");
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
    let expected = phux_web::ink::house_match_pixels();
    let mut marks = Vec::new();
    poll(|| {
        let rows = client.rows_text();
        marks = rows
            .iter()
            .enumerate()
            .filter_map(|(r, text)| {
                text.find("FINDME_ONE")
                    .map(|c| cell_pixel(&canvas, c as u16, r as u16))
            })
            .collect();
        marks == expected
    })
    .await;
    assert_eq!(
        marks,
        expected,
        "the echo's match, then the current (cat's copy), highlighted; viewport:\n{}",
        screen(&client)
    );

    // Escape closes find and gives the terminal its keys back.
    assert!(keydown(&field, "Escape", "Escape", false));
    assert!(bar.has_attribute("hidden"));
    assert_eq!(document().active_element().as_ref(), Some(&surface));
    assert!(keydown(&surface, "Enter", "Enter", false), "typing resumes");
    client.close();
    assert!(
        beside(&canvas, &bar_selector).is_none(),
        "closing the client removes its find bar"
    );
}

#[wasm_bindgen_test]
async fn mouse_reports_reach_a_tracking_program_and_shift_drag_still_selects() {
    let (canvas, client, surface) = connected("mouse-canvas").await;
    canvas.focus().unwrap();
    program_prints(&client, &surface, &["[?1002;1006h"], "MOUSE_ON", &[]).await;

    // A click is a press and a release at the cell, 1-based, in SGR form;
    // the TTY echoes the reports the server wrote to the pane.
    click(&canvas, (4, 2), Pointer::default());
    for (report, what) in [("[<0;5;3M", "press"), ("[<0;5;3m", "release")] {
        assert!(
            wait_for(&client, report).await,
            "{what}: {}",
            screen(&client)
        );
    }
    // A drag reports motion with the button held (1002).
    drag(&canvas, (6, 2), (7, 2), Pointer::default());
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
    drag(&canvas, (0, row), (7, row), shift);
    assert_eq!(copy_event(&surface), copied("MOUSE_ON"));

    program_prints(&client, &surface, &["[?1002;1006l"], "MOUSE_OFF", &[]).await;
    click(&canvas, (9, 9), Pointer::default());
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
    let open = OpenStub::install();
    let (canvas, client, surface) = connected("link-canvas").await;
    canvas.focus().unwrap();
    // OSC 8 open and close, each terminated by ESC \.
    let close = ["]8;;", "\\"];
    let link = ["]8;;https://example.com/osc8", "\\"];
    program_prints(&client, &surface, &link, "OSCLINK", &close).await;
    let bad = ["]8;;javascript:alert(1)", "\\"];
    program_prints(&client, &surface, &bad, "BADLINK", &close).await;
    type_line(&surface, "see https://example.com/plain.");
    // Settled (echo and cat's copy both in), so rows stop moving.
    assert!(wait_rows(&client, "https://example.com/plain.", 2).await);
    // The newest copy is the line this test just printed. Its text can be
    // visible before the canvas frame maps the click onto that hyperlink.
    let osc_row = wait_newest_marker_link(&canvas, &client, "OSCLINK", 2)
        .await
        .unwrap_or_else(|| {
            panic!(
                "OSC 8 link is hit-testable at the click: {}",
                screen(&client)
            )
        });
    let bad_row = client
        .rows_text()
        .iter()
        .rposition(|text| text.trim() == "BADLINK")
        .unwrap_or_else(|| panic!("no row BADLINK: {}", screen(&client))) as u16;
    let (plain_row, plain_col) = newest(&client, "https://example.com/plain");

    // A plain click on a link selects and opens nothing.
    click(&canvas, (2, osc_row), Pointer::default());
    assert!(
        open.opened.borrow().is_empty(),
        "no modifier, no navigation"
    );
    assert!(
        click(&canvas, (2, osc_row), CTRL),
        "the link took the click"
    );
    assert!(
        !click(&canvas, (2, bad_row), CTRL),
        "javascript: never opens"
    );
    // A plain URL is the painted grid, which the same frame updates. Hover
    // that cell before clicking it so the click cannot land on the pre-paint
    // snapshot.
    let plain = (plain_col + 10, plain_row);
    assert!(
        poll(|| hover_sees_link(&canvas, plain)).await,
        "plain URL is hit-testable at the click: {}",
        screen(&client)
    );
    click(&canvas, plain, CTRL);

    let expect = |url: &str| {
        (
            url.to_owned(),
            "_blank".to_owned(),
            "noopener,noreferrer".to_owned(),
        )
    };
    assert_eq!(
        open.restore(),
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
    let client = connect(&canvas).await;
    canvas.focus().unwrap();
    let surface = input_surface(&canvas);
    sleep(Duration::from_millis(300)).await;
    let background = cell_pixel(&canvas, 79, 23);
    assert_eq!(rung.get(), 0, "attaching does not ring");

    // Ctrl+G is BEL: the TTY echoes `^G`, then cat rings it.
    fresh_line(&surface);
    ctrl_key(&surface, "g", "KeyG");
    enter(&surface);
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
    let (canvas, client, surface) = connected("wide-canvas").await;
    canvas.focus().unwrap();
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
    let select = |last: u16| {
        pointer(&canvas, "pointerdown", first, row);
        pointer(&canvas, "pointermove", last, row);
        pointer(&canvas, "pointerup", last, row);
        copy_event(&surface)
    };
    assert_eq!(select(first + 4), copied("\u{4e2d}\u{6587}Z"));
    assert_eq!(
        select(first + 1),
        copied("\u{4e2d}"),
        "a wide character alone copies without its spacer"
    );
    enter(&surface);
    client.close();
}

#[wasm_bindgen_test]
async fn focus_changes_reach_a_program_that_asks_for_them() {
    let (canvas, client, surface) = connected("focus-canvas").await;
    canvas.focus().unwrap();
    let surface: web_sys::HtmlElement = surface.dyn_into().unwrap();
    program_prints(&client, &surface, &["[?1004h"], "FOCUS_ON", &[]).await;

    // The server writes CSI O and CSI I to the pane; the TTY echoes them.
    let reports = || (occurrences(&client, "^[[O"), occurrences(&client, "^[[I"));
    let (lost, gained) = reports();
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
    let before = reports();
    surface.blur().unwrap();
    surface.focus().unwrap();
    sleep(Duration::from_millis(300)).await;
    assert_eq!(
        reports(),
        before,
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
    let (canvas, client, surface) = connected("alt-wheel-canvas").await;
    canvas.focus().unwrap();
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

    // A program that clears alternate scroll (DECSET 1007) gets nothing,
    // and back on the primary screen the wheel is scrollback again.
    for (sequences, marker) in [
        (&["[?1007l"][..], "ALT_SCROLL_OFF"),
        (&["[?1007h", "[?1049l"][..], "ALT_OFF"),
    ] {
        program_prints(&client, &surface, sequences, marker, &[]).await;
        let arrows = occurrences(&client, "^[[A");
        wheel(&canvas, -48.0);
        sleep(Duration::from_millis(300)).await;
        assert_eq!(occurrences(&client, "^[[A"), arrows, "{}", screen(&client));
    }
    client.close();
}

/// A second connection that attaches reporting `cell_px` cells, as another
/// client (a desktop or TUI at another font size) does, then stays.
async fn attach_with_cells(cell_px: (u16, u16)) -> web_sys::WebSocket {
    use phux_protocol::wire::frame::FrameKind;

    let vt = phux_vt_web::Vt::load().await.expect("load engine");
    let session = Rc::new(RefCell::new(phux_web::Connection::new(&vt, 80, 24)));
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
                // HELLO_OK opens discovery and its reply carries the ATTACH,
                // so both go through the connection.
                Ok((frame @ (FrameKind::HelloOk { .. } | FrameKind::CommandResult { .. }), _)) => {
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
    assert!(
        poll(|| attached.get()).await,
        "the second connection never attached"
    );
    socket
}

/// Close `socket` and resolve when the browser's close event fires.
///
/// The server tears the connection down before it completes that handshake,
/// which is what queues the pixel-only resize. Waiting on the event, rather
/// than a delay, is what puts the detach at a known point.
async fn await_socket_close(socket: web_sys::WebSocket) {
    let closed = Rc::new(Cell::new(false));
    let flag = Rc::clone(&closed);
    let on_close = Closure::<dyn FnMut(web_sys::CloseEvent)>::new(move |_: web_sys::CloseEvent| {
        flag.set(true);
    });
    socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));
    on_close.forget();
    socket.close().unwrap();
    assert!(
        poll(|| closed.get()).await,
        "the second connection never closed (ready_state {})",
        socket.ready_state()
    );
}

/// A cell size other than the one this canvas reports, so the second client
/// is a real pixel change at the same 80×24 grid. 10×20 unless the measured
/// face is already that.
fn foreign_cell_px(canvas: &HtmlCanvasElement) -> (u16, u16) {
    let measured = metrics::measured(canvas).cell_px();
    if measured == (10, 20) {
        (12, 24)
    } else {
        (10, 20)
    }
}

/// The server divides mouse positions by the cell size of the most recent
/// viewport that reported pixels. The browser reports its own, so another
/// client's cell size does not move its clicks off their cells.
#[wasm_bindgen_test]
async fn mouse_positions_land_on_their_cells_after_another_client_reports_other_cells() {
    let other = attach_with_cells((10, 20)).await;
    let (canvas, client, surface) = connected("cells-canvas").await;
    canvas.focus().unwrap();
    program_prints(&client, &surface, &["[?1000;1006h"], "CELLS_ON", &[]).await;
    click(&canvas, (4, 2), Pointer::default());
    assert!(
        wait_for(&client, "[<0;5;3m").await,
        "the click lands on column 5, row 3: {}",
        screen(&client)
    );
    program_prints(&client, &surface, &["[?1000;1006l"], "CELLS_OFF", &[]).await;
    // `close()` only starts the handshake. The close event is the server
    // tearing the connection down, which is what queues this client's detach.
    await_socket_close(other).await;
    client.close();
}

/// A synthetic finger at the centre of cell `(col, row)`, or `offset_y` CSS
/// pixels below it.
fn finger(canvas: &HtmlCanvasElement, kind: &str, (col, row): (u16, u16), offset_y: f64) -> bool {
    let rect = canvas.get_bounding_client_rect();
    let cell = metrics::measured(canvas);
    let init = PointerEventInit::new();
    init.set_pointer_id(9);
    init.set_pointer_type("touch");
    init.set_is_primary(true);
    init.set_button(if kind == "pointermove" { -1 } else { 0 });
    init.set_buttons(u16::from(kind != "pointerup"));
    init.set_client_x((rect.left() + (f64::from(col) + 0.5) * cell.cell_w) as i32);
    init.set_client_y((rect.top() + (f64::from(row) + 0.5) * cell.cell_h + offset_y) as i32);
    init.set_bubbles(true);
    init.set_cancelable(true);
    let event = PointerEvent::new_with_event_init_dict(kind, &init).unwrap();
    canvas.dispatch_event(&event).unwrap();
    event.default_prevented()
}

/// Drag a finger down until `marker` is pulled back into view.
fn finger_scroll_to(canvas: &HtmlCanvasElement, client: &Client, marker: &str) {
    let row_px = metrics::measured(canvas).cell_h;
    let reached = (0..30).any(|_| {
        finger(canvas, "pointerdown", (10, 2), 0.0);
        for step in 1..=4 {
            finger(canvas, "pointermove", (10, 2), f64::from(step) * row_px);
        }
        finger(canvas, "pointerup", (10, 6), 0.0);
        shows(client, marker)
    });
    assert!(
        reached,
        "the drag reached the marker in scrollback: {}",
        screen(client)
    );
}

/// After a press at `first`, drag the finger across the marker and lift.
fn finger_select(canvas: &HtmlCanvasElement, (row, first, last): (u16, u16, u16)) {
    finger(canvas, "pointermove", (first + 3, row), 0.0);
    finger(canvas, "pointermove", (last, row), 0.0);
    finger(canvas, "pointerup", (last, row), 0.0);
}

/// A marker for this run alone: the pane is shared, so a previous
/// copy of a fixed marker can be the row a later scroll finds.
fn run_marker(prefix: &str) -> String {
    format!("{prefix}_{}", js_sys::Date::now() as u64)
}

#[wasm_bindgen_test]
async fn a_finger_scrolls_scrollback_a_hold_selects_and_a_tap_opens_a_link() {
    let open = OpenStub::install();
    let (canvas, client, surface) = connected("touch-canvas").await;
    assert_eq!(
        canvas.style().get_property_value("touch-action").unwrap(),
        "none",
        "the browser neither pans nor zooms over the terminal"
    );
    settled_80x24(&client).await;
    let marker = run_marker("PHUX_TOUCH");
    type_line(&surface, &marker);
    assert!(wait_rows(&client, &marker, 2).await, "{}", screen(&client));
    scroll_away(&client, &surface, &marker).await;

    // A finger dragged down pulls history into view, as on any phone.
    finger_scroll_to(&canvas, &client, &marker);
    assert_eq!(copy_event(&surface), NOTHING, "a scroll selects nothing");

    // Hold, then drag across the marker: the cells are selected.
    let cells = marker_cells(&client, &marker);
    finger(&canvas, "pointerdown", (cells.1, cells.0), 0.0);
    sleep(HOLD).await;
    finger_select(&canvas, cells);
    assert_eq!(
        copy_event(&surface),
        copied(&marker),
        "the hold-drag selected the marker"
    );
    // A tap clears it, as a click does.
    finger(&canvas, "pointerdown", (0, 0), 0.0);
    finger(&canvas, "pointerup", (0, 0), 0.0);
    assert_eq!(copy_event(&surface), NOTHING);

    // A tap on a URL opens it, with no modifier to hold.
    type_line(&surface, "see https://example.com/touch.");
    assert!(wait_rows(&client, "https://example.com/touch.", 2).await);
    let rows = client.rows_text();
    let link = rows
        .iter()
        .enumerate()
        .find_map(|(row, text)| {
            text.find("https://example.com/touch")
                .map(|col| (col as u16 + 5, row as u16))
        })
        .unwrap();
    // A tap opens what is painted: wait for the frame that draws the URL.
    assert!(poll(|| hover_sees_link(&canvas, link)).await);
    finger(&canvas, "pointerdown", link, 0.0);
    finger(&canvas, "pointerup", link, 0.0);
    // A hold on it is not a tap.
    finger(&canvas, "pointerdown", link, 0.0);
    sleep(HOLD).await;
    finger(&canvas, "pointerup", link, 0.0);

    let opened: Vec<_> = open.restore().into_iter().map(|(url, ..)| url).collect();
    assert_eq!(opened, ["https://example.com/touch"]);
    client.close();
}

/// A pixel-only detach during a scrollback hold must not wipe the selection.
///
/// PHUX-387: another client's cell size, at the same cols and rows, used to
/// be handled as a reflow. Detach queues that resize with a client resync, and
/// the debounced snapshot replaced the scrolled view with the live screen, so
/// the hold copied nothing. The second client attaches and, on its `ATTACHED`
/// frame, closes. That close event is the point in the hold that detaches it.
/// The drag runs only after the event, once the hold window has elapsed, so
/// the snapshot cannot arrive after the copy. The marker is this run's own, so
/// the test does not read another test's screen.
#[wasm_bindgen_test]
async fn a_hold_still_selects_scrollback_when_another_cell_size_detaches() {
    let (canvas, client, surface) = connected("hold-detach-canvas").await;
    // The other client reports this same grid; a reflow has to land first.
    settled_80x24(&client).await;

    let marker = run_marker("PHUX_HOLD");
    // Primary screen, with mouse and focus reporting off, whatever an earlier
    // test left on. The marker is this run's own line.
    let reset = [
        "[?1049l", "[?1000l", "[?1002l", "[?1003l", "[?1006l", "[?1004l",
    ];
    program_prints(&client, &surface, &reset, &marker, &[]).await;
    scroll_away(&client, &surface, &marker).await;
    finger_scroll_to(&canvas, &client, &marker);

    let cells = marker_cells(&client, &marker);
    let foreign = foreign_cell_px(&canvas);
    // Press, then attach. ATTACHED is the known point: detach there, while
    // the finger is still down and has not yet dragged.
    finger(&canvas, "pointerdown", (cells.1, cells.0), 0.0);
    let other = attach_with_cells(foreign).await;
    await_socket_close(other).await;
    // The close has been observed. Rest the hold from that event so the
    // selecting drag cannot start before the detach, and a debounced resync
    // owed by the old handler has the whole window to land first.
    sleep(HOLD).await;
    finger_select(&canvas, cells);
    assert_eq!(
        copy_event(&surface),
        copied(&marker),
        "the hold-drag selected the marker"
    );
    restore_seeded_marker(&client, &surface).await;
    client.close();
}

#[wasm_bindgen_test]
async fn on_screen_keys_and_the_ctrl_latch_reach_the_terminal() {
    let canvas = mounted_canvas("keybar-canvas");
    let latched: Rc<RefCell<Vec<bool>>> = Rc::default();
    let record = Rc::clone(&latched);
    let listener =
        Closure::<dyn FnMut(web_sys::CustomEvent)>::new(move |event: web_sys::CustomEvent| {
            let ctrl = js_sys::Reflect::get(&event.detail(), &"ctrl".into()).unwrap();
            record.borrow_mut().push(ctrl.as_bool().unwrap());
        });
    canvas
        .add_event_listener_with_callback("phux-modifiers", listener.as_ref().unchecked_ref())
        .unwrap();
    let client = connect(&canvas).await;
    let surface = input_surface(&canvas);
    fresh_line(&surface);
    // The latch chords the next key the phone keyboard types, then lets go.
    client.set_ctrl_latch(true);
    type_text(&surface, "b");
    for key in ["K", "E", "Y", "B", "A", "R"] {
        client.send_key(key).unwrap();
    }
    assert!(
        client.send_key("F13").is_err(),
        "only the documented keys are sent"
    );
    client.send_key("Enter").unwrap();
    // The TTY echoes Ctrl+B as `^B`; cat's copy prints the letters.
    assert!(
        wait_rows(&client, "^BKEYBAR", 1).await,
        "{}",
        screen(&client)
    );
    assert!(
        client
            .rows_text()
            .iter()
            .any(|row| row.trim_end().ends_with("KEYBAR") && !row.contains('^')),
        "{}",
        screen(&client)
    );
    assert_eq!(*latched.borrow(), [true, false], "held, then spent");
    client.close();
}
