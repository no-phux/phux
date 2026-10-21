//! Keys, text, paste, and pane actions; the blink, frame, and expiry timers.

use super::*;

pub(super) fn pane_action(app: &Rc<RefCell<App>>, action: PaneAction<'_>) -> Result<(), JsValue> {
    let mut app = app.borrow_mut();
    let old_focus = app.session.focused_pane();
    let result = match action {
        PaneAction::Split(axis) => app.session.split_pane_frame(axis).map(|frame| vec![frame]),
        PaneAction::CloseView => app.session.close_view(),
        PaneAction::Kill => app.session.kill_pane_frame().map(|frame| vec![frame]),
        PaneAction::Next => app.session.focus_next_pane().map(|()| Vec::new()),
        PaneAction::Focus(id) => app.session.focus_pane(id).map(|()| Vec::new()),
    };
    let result = result.and_then(|frames| {
        if let Err(message) = app.send(frames) {
            app.fail(&message);
            return Err(message);
        }
        Ok(())
    });
    if let Err(message) = &result {
        app.session.set_pane_error(message.clone());
    }
    if old_focus != app.session.focused_pane() {
        app.focus_changed(old_focus);
    }
    app.publish_panes();
    app.request_paint();
    result.map_err(|message| JsValue::from_str(&message))
}

fn pane_keydown(app: &Rc<RefCell<App>>, event: &KeyboardEvent) -> bool {
    if event.is_composing() || event.key_code() == 229 {
        return false;
    }
    let key = event.key();
    let prefix =
        event.ctrl_key() && !event.alt_key() && !event.meta_key() && key.eq_ignore_ascii_case("a");
    let waiting = app.borrow().prefix.replace(false);
    if !waiting {
        if prefix {
            app.borrow().prefix.set(true);
        }
        return prefix;
    }
    if prefix {
        // Let the ordinary key encoder send the second Ctrl+a literally.
        return false;
    }
    let action = match key.as_str() {
        "%" => PaneAction::Split("vertical"),
        "\"" => PaneAction::Split("horizontal"),
        "o" => PaneAction::Next,
        "x" => PaneAction::Kill,
        "Shift" | "Control" | "Alt" | "Meta" => {
            app.borrow().prefix.set(true);
            return false;
        }
        _ => {
            let mut app = app.borrow_mut();
            app.session.set_pane_error("Unknown pane shortcut. After Ctrl+a use %, \", o, or x; Ctrl+a again sends the prefix.".to_owned());
            app.publish_panes();
            return true;
        }
    };
    let _ = pane_action(app, action);
    true
}

pub(super) fn focus_pointer_pane(app: &Rc<RefCell<App>>, event: &web_sys::MouseEvent) {
    let mut app = app.borrow_mut();
    let previous = app.session.focused_pane();
    let rect = app.canvas.get_bounding_client_rect();
    let (width, height) = app.css_cell(&rect);
    let col = (event.client_x() - rect.left()) / width;
    let row = (event.client_y() - rect.top()) / height;
    let target = app.session.pane_rects().into_iter().find(|(_, pane)| {
        col >= f64::from(pane.x)
            && col < f64::from(pane.x) + f64::from(pane.cols)
            && row >= f64::from(pane.y)
            && row < f64::from(pane.y) + f64::from(pane.rows)
    });
    if let Some((id, _)) = target
        && app.session.focused_pane().as_ref() != Some(&id)
        && app.session.focus_pane(&id).is_ok()
    {
        app.focus_changed(previous);
        app.publish_panes();
    }
}

/// Send one routed keydown; cancel the browser default only when sent.
pub(super) fn on_keydown(app: &Rc<RefCell<App>>, event: &web_sys::Event, _: &HtmlTextAreaElement) {
    let Some(event) = event.dyn_ref::<KeyboardEvent>() else {
        return;
    };
    if pane_keydown(app, event) {
        event.prevent_default();
        return;
    }
    let key = event.key();
    let code = event.code();
    let browser_key = crate::input::BrowserKey {
        key: &key,
        code: &code,
        ctrl: event.ctrl_key(),
        shift: event.shift_key(),
        alt: event.alt_key(),
        meta: event.meta_key(),
        alt_graph: event.get_modifier_state("AltGraph"),
        repeat: event.repeat(),
        // Safari reports the keydown that starts a composition as 229
        // before `isComposing` turns true.
        composing: event.is_composing() || event.key_code() == 229,
    };
    if crate::input::is_find_chord(&browser_key) {
        event.prevent_default();
        find::open(app);
        return;
    }
    if crate::input::is_copy_chord(&browser_key) && copy_selection(app) {
        event.prevent_default();
        return;
    }
    if let Some(direction) = crate::input::scrollback_page(&browser_key) {
        event.prevent_default();
        let scrolled = {
            let app = app.borrow();
            let (_, rows) = app.session.dims();
            let page = i32::from(rows.saturating_sub(1).max(1));
            let scrolled = app.session.scroll_viewport(direction * page);
            if scrolled {
                app.clear_selection();
                app.request_paint();
            }
            scrolled
        };
        if scrolled {
            demand_history(app);
        }
        return;
    }
    let routed = crate::input::route_key(&browser_key);
    if let Some(key) = routed
        && send_input(app, typed_keys(app, vec![key]))
    {
        event.prevent_default();
    }
}

/// An IME or dead-key composition committed its text.
pub(super) fn on_composition_end(
    app: &Rc<RefCell<App>>,
    event: &web_sys::Event,
    surface: &HtmlTextAreaElement,
) {
    if let Some(text) = event
        .dyn_ref::<CompositionEvent>()
        .and_then(CompositionEvent::data)
    {
        send_input(app, typed_text(app, &text));
    }
    surface.set_value("");
}

/// Text inserted without a composition (mobile keyboards, dictation, an
/// emoji picker). Composition commits arrive as `compositionend` instead,
/// whichever order the browser fires the two in.
pub(super) fn on_text_input(
    app: &Rc<RefCell<App>>,
    event: &web_sys::Event,
    surface: &HtmlTextAreaElement,
) {
    let Some(event) = event.dyn_ref::<web_sys::InputEvent>() else {
        return;
    };
    if event.is_composing() {
        return;
    }
    let plain = matches!(
        event.input_type().as_str(),
        "insertText" | "insertReplacementText"
    );
    if plain && let Some(text) = event.data() {
        send_input(app, typed_text(app, &text));
    }
    surface.set_value("");
}

/// A clipboard paste becomes one `INPUT_PASTE`; nothing lands in the surface.
pub(super) fn on_paste(app: &Rc<RefCell<App>>, event: &web_sys::Event, _: &HtmlTextAreaElement) {
    event.prevent_default();
    let text = event
        .dyn_ref::<ClipboardEvent>()
        .and_then(ClipboardEvent::clipboard_data)
        .and_then(|data| data.get_data("text/plain").ok())
        .unwrap_or_default();
    match crate::input::paste_event(&text) {
        Some(paste) => {
            send_input(app, [InputEvent::Paste(paste)]);
        }
        None if !text.is_empty() => web_sys::console::warn_1(&JsValue::from_str(
            "phux-web: paste larger than the 512 KiB limit was not sent",
        )),
        None => {}
    }
}

/// Copy the selection through the browser's copy command, which raises the
/// `copy` event [`on_copy`] fills. Returns whether there was one to copy.
fn copy_selection(app: &Rc<RefCell<App>>) -> bool {
    let document = {
        let app = app.borrow();
        if app.selected_text().is_none() {
            return false;
        }
        app.canvas.owner_document()
    };
    document
        .and_then(|document| document.dyn_into::<web_sys::HtmlDocument>().ok())
        .is_some_and(|document| document.exec_command("copy").unwrap_or(false))
}

/// Text typed at the terminal, as key presses.
fn typed_text(app: &Rc<RefCell<App>>, text: &str) -> Vec<InputEvent> {
    typed_keys(app, crate::input::key_events_for_text(text))
}

/// Keys typed at the terminal, the first chorded with Ctrl when the
/// on-screen Ctrl latch is held, which that spends.
pub(super) fn typed_keys(app: &Rc<RefCell<App>>, mut keys: Vec<KeyEvent>) -> Vec<InputEvent> {
    let app = app.borrow();
    if let Some(first) = keys.first_mut()
        && app.ctrl_latch.replace(false)
    {
        *first = crate::input::with_ctrl(first.clone());
        app.publish_modifiers();
    }
    keys.into_iter().map(InputEvent::Key).collect()
}

/// Request older history when a scroll nears the oldest loaded row; a
/// transport failure closes the connection.
pub(super) fn demand_history(app: &Rc<RefCell<App>>) {
    let mut a = app.borrow_mut();
    let frames = a.session.history_demand();
    if let Err(message) = a.send(frames) {
        drop(a);
        close_with_transport_error(app, &message);
    }
}

/// Send input the user typed or pointed: as [`send_events`], and when any
/// was sent, return a scrolled-back view to the live screen and end the
/// selection, as in a local terminal.
pub(super) fn send_input(
    app: &Rc<RefCell<App>>,
    events: impl IntoIterator<Item = InputEvent>,
) -> bool {
    let sent = send_events(app, events);
    let app = app.borrow();
    if sent {
        app.clear_selection();
        if app.session.scroll_to_bottom() {
            app.request_paint();
        }
    }
    sent
}

/// Encode and send input atoms for the focused terminal. Returns whether any
/// was sent; a transport failure closes the connection.
fn send_events(app: &Rc<RefCell<App>>, events: impl IntoIterator<Item = InputEvent>) -> bool {
    let mut sent = false;
    for event in events {
        let mut a = app.borrow_mut();
        let Some(frame) = a.session.input_frame(event) else {
            return sent;
        };
        if let Err(message) = a.tx.send(&frame) {
            drop(a);
            close_with_transport_error(app, &message);
            return false;
        }
        sent = true;
    }
    sent
}

/// Focus reporting (DECSET 1004): while the program asks for it, the input
/// surface gaining or losing focus (a click on the terminal, the find bar
/// or another control taking the keys, the window going to the background)
/// reaches it as `INPUT_FOCUS`, which the server writes as `CSI I` or
/// `CSI O`. It is not typing: the view and the selection stay.
pub(super) fn on_focus_change(
    app: &Rc<RefCell<App>>,
    event: &web_sys::Event,
    _: &HtmlTextAreaElement,
) {
    let reporting = app
        .borrow()
        .session
        .terminal()
        .is_some_and(|terminal| terminal.dec_mode(1004));
    if !reporting {
        return;
    }
    let focus = if event.type_() == "focus" {
        FocusEvent::Gained
    } else {
        FocusEvent::Lost
    };
    send_events(app, [InputEvent::Focus(focus)]);
}

/// Cursor blink: toggle the phase and repaint on a fixed cadence.
pub(super) fn install_cursor_blink(app: &Rc<RefCell<App>>) -> Result<(), JsValue> {
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
    let weak = Rc::downgrade(app);
    let blink = Closure::<dyn FnMut()>::new(move || {
        if let Some(app) = weak.upgrade() {
            app.borrow().blink();
        }
    });
    let interval = window.set_interval_with_callback_and_timeout_and_arguments_0(
        blink.as_ref().unchecked_ref(),
        530,
    )?;
    let old = app
        .borrow()
        .bindings
        .borrow_mut()
        .blink
        .replace(BlinkBinding {
            window,
            interval,
            callback: blink,
        });
    if let Some(old) = old {
        old.dispose();
    }
    Ok(())
}

/// Release the App borrow before dispatch: a presentation listener may focus
/// a pane or resize this same client. No event is emitted without real pixels.
pub(super) fn paint_and_publish(app: &Rc<RefCell<App>>) {
    let (canvas, detail) = {
        let app = app.borrow();
        if !app.paint() {
            return;
        }
        let detail = app.presentation_detail();
        let _ = js_sys::Reflect::set(
            &detail,
            &"pixelWidth".into(),
            &JsValue::from_f64(f64::from(app.canvas.width())),
        );
        let _ = js_sys::Reflect::set(
            &detail,
            &"pixelHeight".into(),
            &JsValue::from_f64(f64::from(app.canvas.height())),
        );
        (app.canvas.clone(), detail)
    };
    let init = web_sys::CustomEventInit::new();
    init.set_bubbles(true);
    init.set_detail(&detail);
    if let Ok(event) = web_sys::CustomEvent::new_with_event_init_dict(RENDER_EVENT, &init) {
        let _ = canvas.dispatch_event(&event);
    }
}

/// The animation-frame callback every paint request shares.
pub(super) fn install_frame(app: &Rc<RefCell<App>>) -> Result<(), JsValue> {
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
    let weak = Rc::downgrade(app);
    let callback = Closure::<dyn FnMut()>::new(move || {
        let Some(app) = weak.upgrade() else {
            return;
        };
        if let Some(frame) = app.borrow().bindings.borrow().frame.as_ref() {
            frame.pending.set(None);
        }
        paint_and_publish(&app);
    });
    app.borrow()
        .bindings
        .borrow_mut()
        .frame
        .replace(FrameBinding {
            window,
            callback,
            pending: Cell::new(None),
        });
    Ok(())
}

pub(super) fn install_bootstrap_expiry(app: &Rc<RefCell<App>>) -> Result<(), JsValue> {
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
    let weak = Rc::downgrade(app);
    let callback = Closure::<dyn FnMut()>::new(move || {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let mut app = app.borrow_mut();
        let outcome = app.session.expire_bootstrap_staging();
        if let Some(message) = outcome.fatal {
            app.fail(&message);
            return;
        }
        if let Err(message) = app.send(outcome.send) {
            app.fail(&message);
            return;
        }
        if outcome.panes {
            app.publish_panes();
        }
    });
    let interval = window.set_interval_with_callback_and_timeout_and_arguments_0(
        callback.as_ref().unchecked_ref(),
        BOOTSTRAP_EXPIRY_POLL_MS,
    )?;
    app.borrow()
        .bindings
        .borrow_mut()
        .bootstrap_expiry
        .replace(BlinkBinding {
            window,
            interval,
            callback,
        });
    Ok(())
}
