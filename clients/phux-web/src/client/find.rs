//! Find in the terminal: a small bar beside the canvas. Command+F or
//! Ctrl+Shift+F typed at the terminal opens it (the browser keeps its own
//! find everywhere else); Enter steps to the next older match, Shift+Enter
//! to the next newer one, and Escape closes it and returns to the terminal.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{Document, Element, Event, EventTarget, HtmlInputElement, KeyboardEvent};

use super::{App, FIND_BAR_CLASS, Listeners};

pub(super) struct FindBinding {
    container: Element,
    field: HtmlInputElement,
    label: Element,
    listeners: Listeners,
}

impl FindBinding {
    pub(super) fn dispose(self) {
        self.listeners.dispose();
        self.container.remove();
    }
}

type FindHandler = fn(&Rc<RefCell<App>>, &Event);

/// Mount the (hidden) find bar right after the canvas.
pub(super) fn install(app: &Rc<RefCell<App>>, document: &Document) -> Result<(), JsValue> {
    let container = document.create_element("div")?;
    container.set_class_name(FIND_BAR_CLASS);
    container.set_attribute("role", "search")?;
    container.set_attribute("hidden", "")?;
    container.set_inner_html(
        "<input type='search' aria-label='Find in terminal' placeholder='Find' \
         autocomplete='off' spellcheck='false'>\
         <span class='phux-find-count' role='status' aria-live='polite'></span>\
         <button type='button' data-action='older' title='Older match (Enter)'>Older</button>\
         <button type='button' data-action='newer' title='Newer match (Shift+Enter)'>Newer</button>\
         <button type='button' data-action='close' title='Close (Escape)'>Close</button>",
    );
    let field: HtmlInputElement = container
        .query_selector("input")?
        .ok_or_else(|| JsValue::from_str("find bar has no field"))?
        .dyn_into()?;
    let label = container
        .query_selector(".phux-find-count")?
        .ok_or_else(|| JsValue::from_str("find bar has no count"))?;
    let canvas = app.borrow().canvas.clone();
    let parent = canvas
        .parent_element()
        .or_else(|| document.body().map(Into::into))
        .ok_or_else(|| JsValue::from_str("no element to host the find bar"))?;
    parent.insert_before(&container, canvas.next_sibling().as_ref())?;

    let mut listeners = Listeners::default();
    let handlers: [(&EventTarget, &'static str, FindHandler); 3] = [
        (field.as_ref(), "input", on_input),
        (field.as_ref(), "keydown", on_keydown),
        (container.as_ref(), "click", on_click),
    ];
    for (target, kind, handler) in handlers {
        let weak = Rc::downgrade(app);
        listeners.listen(target, kind, move |event| {
            if let Some(app) = weak.upgrade() {
                handler(&app, &event);
            }
        })?;
    }
    let old = app
        .borrow()
        .bindings
        .borrow_mut()
        .find
        .replace(FindBinding {
            container,
            field,
            label,
            listeners,
        });
    if let Some(old) = old {
        old.dispose();
    }
    Ok(())
}

fn parts(app: &App) -> Option<(Element, HtmlInputElement)> {
    app.bindings
        .borrow()
        .find
        .as_ref()
        .map(|find| (find.container.clone(), find.field.clone()))
}

/// Show the bar and focus its field, searching again for what it holds.
pub(super) fn open(app: &Rc<RefCell<App>>) {
    let Some((container, field)) = parts(&app.borrow()) else {
        return;
    };
    let _ = container.remove_attribute("hidden");
    let _ = field.focus();
    field.select();
    search(app, &field.value());
}

/// Hide the bar, drop the highlights, and give the terminal its keys back.
fn close(app: &Rc<RefCell<App>>) {
    let app = app.borrow();
    if let Some((container, _)) = parts(&app) {
        let _ = container.set_attribute("hidden", "");
    }
    app.search.borrow_mut().clear();
    set_label(&app, "");
    app.request_paint();
    let surface = app
        .bindings
        .borrow()
        .input
        .as_ref()
        .map(|input| input.surface.clone());
    if let Some(surface) = surface {
        let _ = surface.focus();
    }
}

/// Show `text` as the match count.
pub(super) fn set_label(app: &App, text: &str) {
    if let Some(find) = app.bindings.borrow().find.as_ref() {
        find.label.set_text_content(Some(text));
    }
}

fn search(app: &Rc<RefCell<App>>, query: &str) {
    let app = app.borrow();
    app.run_search(query);
    app.reveal_current_match();
}

fn step(app: &Rc<RefCell<App>>, older: bool) {
    let app = app.borrow();
    if app.search_stale.get() {
        let query = app.search.borrow().query().to_owned();
        app.run_search(&query);
    }
    let moved = app.search.borrow_mut().step(older);
    if moved.is_some() {
        let label = app.search.borrow().label();
        set_label(&app, &label);
        app.reveal_current_match();
    }
}

fn on_input(app: &Rc<RefCell<App>>, _: &Event) {
    let Some((_, field)) = parts(&app.borrow()) else {
        return;
    };
    search(app, &field.value());
}

fn on_keydown(app: &Rc<RefCell<App>>, event: &Event) {
    let Some(event) = event.dyn_ref::<KeyboardEvent>() else {
        return;
    };
    if event.is_composing() {
        return;
    }
    match event.key().as_str() {
        "Enter" => {
            event.prevent_default();
            step(app, !event.shift_key());
        }
        "Escape" => {
            event.prevent_default();
            close(app);
        }
        // The find chord again selects the query rather than opening the
        // browser's find over the page.
        _ if (event.meta_key() || (event.ctrl_key() && event.shift_key()))
            && event.code() == "KeyF" =>
        {
            event.prevent_default();
            if let Some((_, field)) = parts(&app.borrow()) {
                field.select();
            }
        }
        _ => {}
    }
}

fn on_click(app: &Rc<RefCell<App>>, event: &Event) {
    let Some(button) = event
        .target()
        .and_then(|target| target.dyn_into::<Element>().ok())
        .and_then(|target| target.closest("button[data-action]").ok().flatten())
    else {
        return;
    };
    match button.get_attribute("data-action").as_deref() {
        Some("older") => step(app, true),
        Some("newer") => step(app, false),
        Some("close") => close(app),
        _ => {}
    }
}
