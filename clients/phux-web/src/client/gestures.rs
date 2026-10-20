//! Touch, clipboard copy, and the wheel.

use super::*;

/// One finger over the canvas ([`crate::touch`]): a drag scrolls as the
/// wheel does, a hold then drag selects (and copies when the finger lifts),
/// and a tap opens the link under it, or else clicks a mouse-tracking
/// program.
pub(super) fn on_touch(app: &Rc<RefCell<App>>, event: &web_sys::PointerEvent) {
    let position = (event.client_x(), event.client_y());
    let now = event.time_stamp();
    if event.type_() == "pointerdown" {
        // Only the first finger down starts a gesture; a second one (a pinch,
        // a resting thumb) is ignored. A primary finger means every earlier
        // one has lifted, so it also replaces a gesture whose end was lost.
        if event.is_primary() {
            let app = app.borrow();
            app.wheel_carry.set(0.0);
            let gesture = TouchGesture::begin(event.pointer_id(), position, now);
            app.touch.set(Some(gesture));
        }
        return;
    }
    let Some(mut gesture) = app
        .borrow()
        .touch
        .get()
        .filter(|gesture| gesture.pointer_id == event.pointer_id())
    else {
        return;
    };
    match event.type_().as_str() {
        "pointermove" => {
            let step = gesture.moved(position, now);
            app.borrow().touch.set(Some(gesture));
            touch_step(app, event, step);
        }
        "pointerup" | "pointercancel" => {
            app.borrow().touch.set(None);
            let lifted = event.type_() == "pointerup";
            touch_end(app, event, gesture, lifted);
        }
        _ => {}
    }
}

/// Carry out one finger move.
fn touch_step(app: &Rc<RefCell<App>>, event: &web_sys::PointerEvent, step: TouchStep) {
    match step {
        TouchStep::Wait => {}
        TouchStep::Scroll(delta) => scroll(app, event, delta, crate::input::WHEEL_PIXELS, false),
        TouchStep::StartSelect(anchor) => {
            let app = app.borrow();
            begin_selection(&app, event, app.cell_at_point(anchor));
            extend_selection(&app, app.cell_under(event));
        }
        TouchStep::ExtendSelect => {
            let app = app.borrow();
            extend_selection(&app, app.cell_under(event));
        }
    }
}

/// The finger lifted (or the browser cancelled it): a selection ends and is
/// copied; a tap opens a link or clicks a tracking program, else clears the
/// selection as a click does.
fn touch_end(
    app: &Rc<RefCell<App>>,
    event: &web_sys::PointerEvent,
    gesture: TouchGesture,
    lifted: bool,
) {
    if gesture.mode() == TouchMode::Select {
        app.borrow().selecting.set(false);
        if lifted {
            write_selection_to_clipboard(&app.borrow());
        }
        return;
    }
    if !lifted || !gesture.is_tap(event.time_stamp()) || open_link_under(app, event) {
        return;
    }
    if program_takes_mouse(&app.borrow(), false) {
        send_mouse(app, event, MouseAction::Press, MouseButton::Left);
        send_mouse(app, event, MouseAction::Release, MouseButton::Left);
    } else {
        app.borrow().clear_selection();
    }
}

/// Put the selected text on the clipboard: a phone has no copy chord, so a
/// touch selection copies itself when the finger lifts. The asynchronous
/// Clipboard API needs no focused element; a refusal (an insecure page, a
/// denied permission) leaves the selection showing and the clipboard as it
/// was.
fn write_selection_to_clipboard(app: &App) {
    let Some(text) = app.selected_text() else {
        return;
    };
    let Some(window) = web_sys::window() else {
        return;
    };
    let promise = window.navigator().clipboard().write_text(&text);
    wasm_bindgen_futures::spawn_local(async move {
        let _ = JsFuture::from(promise).await;
    });
}

/// Copying (the browser's own Command+C, or the chord handler) takes the
/// selected text when there is a selection.
pub(super) fn on_copy(app: &Rc<RefCell<App>>, event: &web_sys::Event, _: &HtmlTextAreaElement) {
    let Some(text) = app.borrow().selected_text() else {
        return;
    };
    if let Some(data) = event
        .dyn_ref::<ClipboardEvent>()
        .and_then(ClipboardEvent::clipboard_data)
        && data.set_data("text/plain", &text).is_ok()
    {
        event.prevent_default();
    }
}

/// The wheel pages the local scrollback, scrolls a mouse-tracking program,
/// or reaches a program on the alternate screen as arrow keys
/// ([`crate::input::route_wheel`]); the page itself does not scroll.
pub(super) fn on_wheel(app: &Rc<RefCell<App>>, event: &web_sys::Event, _: &HtmlTextAreaElement) {
    let Some(event) = event.dyn_ref::<web_sys::WheelEvent>() else {
        return;
    };
    event.prevent_default();
    scroll(
        app,
        event,
        event.delta_y(),
        event.delta_mode(),
        event.shift_key(),
    );
}

/// Vertical travel over the terminal, from the wheel or a finger drag:
/// `delta_y` in `delta_mode` units as `WheelEvent` reports them, routed by
/// [`crate::input::route_wheel`]. A report to the program lands at `at`.
pub(super) fn scroll(
    app: &Rc<RefCell<App>>,
    at: &web_sys::MouseEvent,
    delta_y: f64,
    delta_mode: u32,
    shift: bool,
) {
    let action = {
        let app = app.borrow();
        crate::input::route_wheel(wheel_modes(&app), shift, app.session.viewport_scrolled())
    };
    match action {
        WheelAction::Report => forward_wheel(app, at, delta_y, delta_mode),
        WheelAction::Arrows => {
            let rows = wheel_travel(&app.borrow(), delta_y, delta_mode, 1.0);
            let arrows =
                crate::input::wheel_arrows(rows.clamp(-MAX_WHEEL_ARROWS, MAX_WHEEL_ARROWS));
            send_input(app, arrows.into_iter().map(InputEvent::Key));
        }
        WheelAction::Scrollback => {
            let scrolled = {
                let app = app.borrow();
                let rows = wheel_travel(&app, delta_y, delta_mode, 1.0);
                let scrolled = rows != 0 && app.session.scroll_viewport(rows);
                if scrolled {
                    app.clear_selection();
                    app.request_paint();
                }
                scrolled
            };
            if scrolled {
                demand_history(app);
            }
        }
    }
}

/// The replica's modes a wheel event is routed by.
fn wheel_modes(app: &App) -> crate::input::WheelModes {
    app.session
        .terminal()
        .map_or_else(Default::default, |terminal| crate::input::WheelModes {
            tracking: terminal.mouse_tracking(),
            alt_screen: [1049, 1047, 47]
                .into_iter()
                .any(|mode| terminal.dec_mode(mode)),
            alt_scroll: terminal.dec_mode(1007),
        })
}

/// Whole units of wheel travel, `rows_per_unit` rows each; the remainder
/// carries to the next wheel event. A row is a cell's drawn height.
fn wheel_travel(app: &App, delta_y: f64, delta_mode: u32, rows_per_unit: f64) -> i32 {
    let rect = app.canvas.get_bounding_client_rect();
    let (_, css_h) = app.css_cell(&rect);
    let (_, page_rows) = app.session.dims();
    let mut carry = app.wheel_carry.get();
    let units = crate::input::wheel_rows(
        delta_y,
        delta_mode,
        css_h * rows_per_unit,
        page_rows,
        &mut carry,
    );
    app.wheel_carry.set(carry);
    units
}

/// Report wheel travel to the program as xterm wheel presses (buttons 4
/// and 5), one per [`crate::input::WHEEL_ROWS_PER_CLICK`] rows.
fn forward_wheel(app: &Rc<RefCell<App>>, at: &web_sys::MouseEvent, delta_y: f64, delta_mode: u32) {
    let clicks = wheel_travel(
        &app.borrow(),
        delta_y,
        delta_mode,
        crate::input::WHEEL_ROWS_PER_CLICK,
    )
    .clamp(-MAX_WHEEL_CLICKS, MAX_WHEEL_CLICKS);
    let button = crate::input::wheel_button(clicks);
    for _ in 0..clicks.unsigned_abs() {
        send_mouse(app, at, MouseAction::Press, button);
    }
}
