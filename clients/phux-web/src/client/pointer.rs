//! The input surface and pointer: links, mouse reports, and selection.

use super::*;

/// Keyboard, IME, and paste: a hidden `<textarea>` beside the canvas owns
/// terminal input. Focusing or clicking the canvas focuses it, so only keys
/// typed at the terminal become `INPUT_KEY`; the embedding page's other
/// controls keep theirs. Keydowns the terminal encodes are sent and
/// cancelled; composed text arrives as `compositionend` or `input`, and a
/// clipboard paste as one `INPUT_PASTE`.
pub(super) fn install_input(app: &Rc<RefCell<App>>) -> Result<(), JsValue> {
    let canvas = app.borrow().canvas.clone();
    let document = canvas
        .owner_document()
        .ok_or_else(|| JsValue::from_str("no document"))?;
    let surface = create_input_surface(&document, &canvas)?;
    let style = canvas.style();
    for (property, value) in CANVAS_TOUCH_STYLE {
        style.set_property(property, value)?;
    }
    let mut binding = InputBinding {
        surface: surface.clone(),
        listeners: Listeners::default(),
    };
    let handlers: [(&str, InputHandler); 8] = [
        ("keydown", on_keydown),
        ("focus", on_focus_change),
        ("blur", on_focus_change),
        ("compositionstart", |app, _, _| {
            app.borrow().place_input_surface()
        }),
        ("compositionend", on_composition_end),
        ("input", on_text_input),
        ("paste", on_paste),
        ("copy", on_copy),
    ];
    let canvas_handlers: [(&str, InputHandler); 8] = [
        ("focus", focus_surface),
        ("mousedown", focus_surface),
        ("wheel", on_wheel),
        ("pointerdown", on_pointer),
        ("pointermove", on_pointer),
        ("pointerup", on_pointer),
        ("pointercancel", on_pointer),
        ("contextmenu", on_context_menu),
    ];
    let targets: [(&web_sys::EventTarget, &[(&str, InputHandler)]); 2] = [
        (surface.as_ref(), &handlers),
        (canvas.as_ref(), &canvas_handlers),
    ];
    for (target, handlers) in targets {
        for &(kind, handler) in handlers {
            let weak = Rc::downgrade(app);
            let surface = surface.clone();
            binding.listeners.listen(target, kind, move |event| {
                if let Some(app) = weak.upgrade() {
                    handler(&app, &event, &surface);
                }
            })?;
        }
    }

    let old = app.borrow().bindings.borrow_mut().input.replace(binding);
    if let Some(old) = old {
        old.dispose();
    }
    find::install(app, &document)?;
    if focus_is_idle(&document) {
        app.borrow().place_input_surface();
        let _ = surface.focus();
    }
    Ok(())
}

/// The hidden, focusable text control, mounted beside the canvas.
fn create_input_surface(
    document: &web_sys::Document,
    canvas: &HtmlCanvasElement,
) -> Result<HtmlTextAreaElement, JsValue> {
    let surface: HtmlTextAreaElement = document.create_element("textarea")?.dyn_into()?;
    surface.set_class_name(INPUT_SURFACE_CLASS);
    for (name, value) in [
        ("aria-label", "Terminal input"),
        ("autocomplete", "off"),
        ("autocorrect", "off"),
        ("autocapitalize", "off"),
        ("spellcheck", "false"),
        ("inputmode", "text"),
        ("enterkeyhint", "enter"),
        ("tabindex", "-1"),
        ("style", INPUT_SURFACE_STYLE),
    ] {
        surface.set_attribute(name, value)?;
    }
    let parent = canvas
        .parent_element()
        .or_else(|| document.body().map(Into::into))
        .ok_or_else(|| JsValue::from_str("no element to host the input surface"))?;
    parent.append_child(&surface)?;
    Ok(surface)
}

/// Whether nothing holds focus: a fresh page, or a reconnect whose previous
/// surface held it. The terminal takes focus then, so typing works without
/// a click, but never from another control the user is in.
fn focus_is_idle(document: &web_sys::Document) -> bool {
    document
        .active_element()
        .is_none_or(|active| document.body().is_some_and(|body| active == *body.as_ref()))
}

/// Focusing or pressing on the canvas focuses the input surface.
fn focus_surface(_: &Rc<RefCell<App>>, event: &web_sys::Event, surface: &HtmlTextAreaElement) {
    if event.type_() == "mousedown" {
        // Keep the canvas from taking focus back from the surface.
        event.prevent_default();
    }
    let _ = surface.focus();
}

/// The pointer over the canvas, in precedence order: Command/Ctrl+click
/// opens a link; while the program tracks the mouse, presses, releases, and
/// the motion it asked for reach it as `INPUT_MOUSE`; otherwise (and always
/// with Shift held) a primary-button drag selects text locally. A finger
/// goes to [`on_touch`] instead.
fn on_pointer(app: &Rc<RefCell<App>>, event: &web_sys::Event, _: &HtmlTextAreaElement) {
    let Some(event) = event.dyn_ref::<web_sys::PointerEvent>() else {
        return;
    };
    if event.type_() == "pointerdown" {
        focus_pointer_pane(app, event);
    }
    if event.pointer_type() == "touch" {
        on_touch(app, event);
        return;
    }
    if event.type_() == "pointermove" {
        hover_link(&app.borrow(), event);
    }
    if open_link(app, event) || forward_pointer(app, event) {
        return;
    }
    select_with_pointer(app, event);
}

/// Whether a pointer event carries the link modifier: Command on macOS,
/// Ctrl elsewhere (either is accepted), without Shift.
fn link_modifier(event: &web_sys::MouseEvent) -> bool {
    (event.meta_key() || event.ctrl_key()) && !event.shift_key()
}

/// The link at a viewport cell that a click may open: the program's OSC 8
/// hyperlink when it set one (even one this client refuses to open), else a
/// plain `http`, `https`, or `mailto` URL in the row's text.
fn link_at(app: &App, (col, row): (u16, u16)) -> Option<String> {
    let terminal = app.session.terminal()?;
    if let Some(uri) = terminal.hyperlink_at(col, row) {
        return crate::links::allowed_link(&uri).map(str::to_owned);
    }
    let painted = app.painted.borrow();
    let grid = painted.as_ref()?;
    let cols = usize::from(grid.cols);
    let start = usize::from(row) * cols;
    let cells = grid.cells.get(start..start + cols)?;
    let text: Vec<char> = cells.iter().map(|cell| cell.ch).collect();
    crate::links::url_at(&text, usize::from(col))
}

/// Command/Ctrl+click on a link opens it in a new tab, with no opener or
/// referrer. Returns whether the click was a link's.
fn open_link(app: &Rc<RefCell<App>>, event: &web_sys::PointerEvent) -> bool {
    if event.type_() != "pointerdown" || event.button() != 0 || !link_modifier(event) {
        return false;
    }
    if !open_link_under(app, event) {
        return false;
    }
    event.prevent_default();
    true
}

/// Open the link under the pointer, if there is one, in a new tab with no
/// opener or referrer. Returns whether there was a link.
pub(super) fn open_link_under(app: &Rc<RefCell<App>>, event: &web_sys::MouseEvent) -> bool {
    let url = {
        let app = app.borrow();
        link_at(&app, app.cell_under(event))
    };
    let Some(url) = url else {
        return false;
    };
    if let Some(window) = web_sys::window() {
        let _ = window.open_with_url_and_target_and_features(&url, "_blank", "noopener,noreferrer");
    }
    true
}

/// Show a pointer over a link while the link modifier is held.
fn hover_link(app: &App, event: &web_sys::PointerEvent) {
    let over_link = link_modifier(event) && link_at(app, app.cell_under(event)).is_some();
    if app.link_hover.replace(over_link) == over_link {
        return;
    }
    let style = app.canvas.style();
    let _ = if over_link {
        style.set_property("cursor", "pointer")
    } else {
        style.remove_property("cursor").map(|_| ())
    };
}

/// Whether pointer input goes to the program: it tracks the mouse, the
/// viewport shows the live screen its coordinates name, and Shift (the
/// local-selection override) is not held.
pub(super) fn program_takes_mouse(app: &App, shift: bool) -> bool {
    !shift
        && !app.session.viewport_scrolled()
        && app
            .session
            .terminal()
            .is_some_and(phux_vt_web::Terminal::mouse_tracking)
}

/// Forward a press, release, or reported motion to a mouse-tracking
/// program. Returns whether the event was the program's.
fn forward_pointer(app: &Rc<RefCell<App>>, event: &web_sys::PointerEvent) -> bool {
    let route = route_pointer(&app.borrow(), event);
    match route {
        PointerRoute::Local => false,
        PointerRoute::Swallow => true,
        PointerRoute::Report(action, button) => {
            send_mouse(app, event, action, button);
            true
        }
    }
}

fn route_pointer(app: &App, event: &web_sys::PointerEvent) -> PointerRoute {
    if app.selecting.get() {
        return PointerRoute::Local;
    }
    match event.type_().as_str() {
        "pointerdown" => route_press(app, event),
        "pointerup" | "pointercancel" => route_release(app, event),
        "pointermove" => route_motion(app, event),
        _ => PointerRoute::Local,
    }
}

/// A press goes to a tracking program, which then owns the drag.
fn route_press(app: &App, event: &web_sys::PointerEvent) -> PointerRoute {
    let Some(button) = crate::input::mouse_button(event.button()) else {
        return PointerRoute::Local;
    };
    if !program_takes_mouse(app, event.shift_key()) {
        return PointerRoute::Local;
    }
    app.forwarded_button.set(Some(button));
    // Keep receiving moves and the release outside the canvas.
    let _ = app.canvas.set_pointer_capture(event.pointer_id());
    app.clear_selection();
    PointerRoute::Report(MouseAction::Press, button)
}

/// A release (or cancel) ends a forwarded press, even if tracking stopped.
fn route_release(app: &App, event: &web_sys::PointerEvent) -> PointerRoute {
    let Some(pressed) = app.forwarded_button.take() else {
        return PointerRoute::Local;
    };
    let released = crate::input::mouse_button(event.button()).unwrap_or(pressed);
    PointerRoute::Report(MouseAction::Release, released)
}

/// Motion reaches the program once per cell, when its mode reports it.
fn route_motion(app: &App, event: &web_sys::PointerEvent) -> PointerRoute {
    let forwarding = app.forwarded_button.get().is_some();
    if !forwarding && !program_takes_mouse(app, event.shift_key()) {
        return PointerRoute::Local;
    }
    let Some(terminal) = app.session.terminal() else {
        return PointerRoute::Local;
    };
    let dragging = crate::input::held_button(event.buttons());
    let wanted = crate::input::reports_motion(
        terminal.dec_mode(1003),
        terminal.dec_mode(1002),
        dragging.is_some(),
    );
    if !wanted || app.mouse_cell.get() == Some(app.cell_under(event)) {
        return PointerRoute::Swallow;
    }
    PointerRoute::Report(
        MouseAction::Motion,
        dragging.unwrap_or(MouseButton::Unknown),
    )
}

/// Send one mouse report at the pointer's cell-grid pixel position.
pub(super) fn send_mouse(
    app: &Rc<RefCell<App>>,
    event: &web_sys::MouseEvent,
    action: MouseAction,
    button: MouseButton,
) {
    let (x, y) = {
        let app = app.borrow();
        app.mouse_cell.set(Some(app.cell_under(event)));
        app.surface_pixels(event)
    };
    let mut mods = ModSet::empty();
    if event.ctrl_key() {
        mods |= ModSet::CTRL;
    }
    if event.alt_key() {
        mods |= ModSet::ALT;
    }
    if event.shift_key() {
        mods |= ModSet::SHIFT;
    }
    send_input(
        app,
        [InputEvent::Mouse(MouseEvent {
            action,
            button,
            mods,
            x,
            y,
        })],
    );
}

/// The context menu is the program's while it tracks the mouse, so a
/// right-click reaches it; Shift+right-click still opens the browser's.
fn on_context_menu(app: &Rc<RefCell<App>>, event: &web_sys::Event, _: &HtmlTextAreaElement) {
    let shift = event
        .dyn_ref::<web_sys::MouseEvent>()
        .is_some_and(web_sys::MouseEvent::shift_key);
    let app = app.borrow();
    // A long press is a selection, never the browser's menu.
    if app.touch.get().is_some() || program_takes_mouse(&app, shift) {
        event.prevent_default();
    }
}

/// A primary-button drag over the canvas selects cells; a click clears.
fn select_with_pointer(app: &Rc<RefCell<App>>, event: &web_sys::PointerEvent) {
    let app = app.borrow();
    let cell = app.cell_under(event);
    match event.type_().as_str() {
        "pointerdown" if event.button() == 0 => begin_selection(&app, event, cell),
        "pointermove" if app.selecting.get() => extend_selection(&app, cell),
        // A cancelled pointer (a touch taken for panning) ends the drag too.
        "pointerup" | "pointercancel" => {
            let dragging = app.selecting.replace(false);
            if dragging && app.selection.get().is_some_and(|s| s.is_click()) {
                app.clear_selection();
            }
        }
        _ => {}
    }
}

/// Start a selection at `cell`, following the pointer that pressed there.
pub(super) fn begin_selection(app: &App, event: &web_sys::PointerEvent, cell: (u16, u16)) {
    app.selection.set(Some(Selection::at(cell)));
    app.selecting.set(true);
    // Keep receiving moves when the drag leaves the canvas.
    let _ = app.canvas.set_pointer_capture(event.pointer_id());
    app.request_paint();
}

/// Move the live selection's head to `cell`.
pub(super) fn extend_selection(app: &App, cell: (u16, u16)) {
    if let Some(mut selection) = app.selection.get()
        && selection.head != cell
    {
        selection.head = cell;
        app.selection.set(Some(selection));
        app.request_paint();
    }
}
