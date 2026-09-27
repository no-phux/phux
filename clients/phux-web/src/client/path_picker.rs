//! Host-side path discovery. DOM text is always textContent, never server HTML.

use std::cell::RefCell;
use std::rc::Rc;

use phux_protocol::wire::frame::{PathKind, PathStatus};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{Element, Event, HtmlInputElement, KeyboardEvent};

use super::App;

pub(super) struct PickerBinding {
    container: Element,
    callback: Closure<dyn FnMut(Event)>,
}

impl PickerBinding {
    pub(super) fn dispose(self) {
        let _ = self
            .container
            .remove_event_listener_with_callback("click", self.callback.as_ref().unchecked_ref());
        if let Some(parent) = self.container.parent_node() {
            let _ = parent.remove_child(&self.container);
        }
    }
}

pub(super) fn install(app: &Rc<RefCell<App>>) -> Result<(), JsValue> {
    let document = app
        .borrow()
        .canvas
        .owner_document()
        .ok_or_else(|| JsValue::from_str("no document"))?;
    let container = document.create_element("section")?;
    container.set_id("phux-path-picker");
    container.set_inner_html(
        "<button type='button' data-action='toggle'>Host paths</button>\
         <span class='phux-path-fallback' hidden>Older server: type paths in the terminal instead.</span>\
         <div class='phux-path-panel' hidden>\
         <label>Host directory <input data-field='root' value='~' aria-label='Host directory'></label>\
         <label>Find files or directories <input data-field='query' aria-label='Path query'></label>\
         <button type='button' data-action='browse'>Browse one level</button>\
         <button type='button' data-action='search'>Search recursively</button>\
         <button type='button' data-action='cancel'>Close</button>\
         <div class='phux-path-status' role='status' aria-live='polite'></div>\
         <div class='phux-path-results'></div></div>",
    );
    let parent = app
        .borrow()
        .canvas
        .parent_element()
        .ok_or_else(|| JsValue::from_str("canvas has no parent"))?;
    parent.insert_before(&container, Some(&app.borrow().canvas))?;
    let weak = Rc::downgrade(app);
    let on_click = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
        let Some(app) = weak.upgrade() else { return };
        let Some(button) = event
            .target()
            .and_then(|target| target.dyn_into::<Element>().ok())
            .and_then(|target| target.closest("button[data-action]").ok().flatten())
        else {
            return;
        };
        let action = button.get_attribute("data-action").unwrap_or_default();
        click(&app, &action, button.get_attribute("data-index"));
    });
    container.add_event_listener_with_callback("click", on_click.as_ref().unchecked_ref())?;
    app.borrow()
        .bindings
        .borrow_mut()
        .path_picker
        .replace(PickerBinding {
            container,
            callback: on_click,
        });
    paint(&app.borrow());
    Ok(())
}

pub(super) fn is_picker_event(event: &KeyboardEvent) -> bool {
    event
        .target()
        .and_then(|target| target.dyn_into::<Element>().ok())
        .is_some_and(|target| target.closest("#phux-path-picker").ok().flatten().is_some())
}

fn click(app: &Rc<RefCell<App>>, action: &str, index: Option<String>) {
    match action {
        "toggle" => toggle(app),
        "cancel" => {
            app.borrow_mut().session.cancel_path_query();
            paint(&app.borrow());
            hide(&app.borrow());
        }
        "browse" | "search" => query(app, action == "search"),
        "parent" => browse_parent(app),
        "open" => open_directory(app, index),
        "select" => select(app, index),
        _ => {}
    }
}

fn container(app: &App) -> Option<Element> {
    app.bindings
        .borrow()
        .path_picker
        .as_ref()
        .map(|binding| binding.container.clone())
}

fn field(app: &App, name: &str) -> Option<HtmlInputElement> {
    container(app)?
        .query_selector(&format!("input[data-field='{name}']"))
        .ok()??
        .dyn_into()
        .ok()
}

fn toggle(app: &Rc<RefCell<App>>) {
    let Some(panel) = container(&app.borrow())
        .and_then(|root| root.query_selector(".phux-path-panel").ok().flatten())
    else {
        return;
    };
    if panel.has_attribute("hidden") {
        let _ = panel.remove_attribute("hidden");
        if let Some(input) = field(&app.borrow(), "query") {
            let _ = input.focus();
        }
    } else {
        app.borrow_mut().session.cancel_path_query();
        paint(&app.borrow());
        hide(&app.borrow());
    }
}

fn hide(app: &App) {
    if let Some(panel) =
        container(app).and_then(|root| root.query_selector(".phux-path-panel").ok().flatten())
    {
        let _ = panel.set_attribute("hidden", "");
    }
    let _ = app.canvas.focus();
}

fn query(app: &Rc<RefCell<App>>, recursive: bool) {
    let (root, term) = {
        let app = app.borrow();
        let Some(root) = field(&app, "root") else {
            return;
        };
        let Some(term) = field(&app, "query") else {
            return;
        };
        (root.value(), term.value())
    };
    if recursive && term.trim().is_empty() {
        status(&app.borrow(), "Enter a name to search recursively.");
        return;
    }
    let frame = app
        .borrow_mut()
        .session
        .path_query_frame(&root, &term, recursive);
    let Some(frame) = frame else {
        status(
            &app.borrow(),
            "Host path search is unavailable on this connection.",
        );
        return;
    };
    let sent = app.borrow().tx.send(&frame);
    if let Err(message) = sent {
        super::close_with_transport_error(app, &message);
        return;
    }
    paint(&app.borrow());
}

fn browse_parent(app: &Rc<RefCell<App>>) {
    let parent = app
        .borrow()
        .session
        .path_results()
        .and_then(|results| results.parent.clone());
    if let Some(parent) = parent {
        if let Some(root) = field(&app.borrow(), "root") {
            root.set_value(&parent);
        }
        if let Some(term) = field(&app.borrow(), "query") {
            term.set_value("");
        }
        query(app, false);
    }
}

fn open_directory(app: &Rc<RefCell<App>>, index: Option<String>) {
    let directory = index
        .and_then(|index| index.parse::<usize>().ok())
        .and_then(|index| {
            app.borrow()
                .session
                .path_results()?
                .rows
                .get(index)
                .cloned()
        })
        .filter(|row| row.kind == PathKind::Directory);
    let Some(directory) = directory else { return };
    if let Some(root) = field(&app.borrow(), "root") {
        root.set_value(&directory.path);
    }
    if let Some(term) = field(&app.borrow(), "query") {
        term.set_value("");
    }
    query(app, false);
}

fn select(app: &Rc<RefCell<App>>, index: Option<String>) {
    let frame = index
        .and_then(|index| index.parse::<usize>().ok())
        .and_then(|index| app.borrow_mut().session.paste_path_row(index));
    let Some(frame) = frame else {
        status(
            &app.borrow(),
            "Selection expired or input lease unavailable. Search again.",
        );
        return;
    };
    let sent = app.borrow().tx.send(&frame);
    if let Err(message) = sent {
        super::close_with_transport_error(app, &message);
        return;
    }
    paint(&app.borrow());
    hide(&app.borrow());
}

fn status(app: &App, text: &str) {
    if let Some(element) =
        container(app).and_then(|root| root.query_selector(".phux-path-status").ok().flatten())
    {
        element.set_text_content(Some(text));
    }
}

pub(super) fn paint(app: &App) {
    let Some(root) = container(app) else { return };
    paint_support(&root, app.session.path_query_supported());
    let Some(list) = root.query_selector(".phux-path-results").ok().flatten() else {
        return;
    };
    list.set_text_content(None);
    status(app, "");
    if app.session.path_pending() {
        status(app, "Searching on the host...");
        return;
    }
    if let Some(error) = app.session.path_error() {
        status(app, error);
        return;
    }
    let Some(results) = app.session.path_results() else {
        return;
    };
    paint_results(app, &list, results);
}

fn paint_support(root: &Element, supported: bool) {
    if let Some(fallback) = root.query_selector(".phux-path-fallback").ok().flatten() {
        if supported {
            let _ = fallback.set_attribute("hidden", "");
        } else {
            let _ = fallback.remove_attribute("hidden");
        }
    }
    if let Some(button) = root
        .query_selector("button[data-action='toggle']")
        .ok()
        .flatten()
    {
        if supported {
            let _ = button.remove_attribute("disabled");
            let _ = button.remove_attribute("title");
        } else {
            let _ = button.set_attribute("disabled", "");
            let _ = button.set_attribute(
                "title",
                "Older server: type the path in the terminal instead",
            );
        }
    }
}

fn paint_results(app: &App, list: &Element, results: &phux_protocol::wire::frame::PathResults) {
    let state = match results.status {
        PathStatus::Complete => "Complete",
        PathStatus::Warming => "Still indexing; search again for more",
        PathStatus::Truncated => "Partial results; narrow your search",
    };
    status(app, &format!("{} entries - {state}", results.rows.len()));
    if let Some(parent) = &results.parent {
        add_button(list, "parent", "..", None, Some(parent));
    }
    for (index, row) in results.rows.iter().enumerate() {
        let kind = match row.kind {
            PathKind::File => "file",
            PathKind::Directory => "dir",
            PathKind::Symlink => "link",
        };
        let label = format!("[{kind}] {}", display_path(&row.path));
        add_button(list, "select", &label, Some(index), None);
        if row.kind == PathKind::Directory {
            add_button(list, "open", "Browse inside", Some(index), Some(&row.path));
        }
    }
}

fn add_button(
    list: &Element,
    action: &str,
    label: &str,
    index: Option<usize>,
    title: Option<&str>,
) {
    let Some(document) = list.owner_document() else {
        return;
    };
    let Ok(button) = document.create_element("button") else {
        return;
    };
    let _ = button.set_attribute("type", "button");
    let _ = button.set_attribute("data-action", action);
    if let Some(index) = index {
        let _ = button.set_attribute("data-index", &index.to_string());
    }
    if let Some(title) = title {
        let _ = button.set_attribute("title", title);
    }
    button.set_text_content(Some(label));
    let _ = list.append_child(&button);
}

fn display_path(path: &str) -> String {
    path.chars()
        .flat_map(|ch| {
            if ch.is_control() {
                ch.escape_default().collect::<Vec<_>>()
            } else {
                vec![ch]
            }
        })
        .collect()
}
