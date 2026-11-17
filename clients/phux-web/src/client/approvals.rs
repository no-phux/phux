//! Approval Inbox, beside the terminal like the existing path picker. Text only, no secrets.

use super::*;
use crate::session::approvals::{Approval, Requester};
use web_sys::{Element, Event};

pub(super) struct ApprovalBinding {
    container: Element,
    callback: Closure<dyn FnMut(Event)>,
}

impl Drop for ApprovalBinding {
    fn drop(&mut self) {
        let _ = self
            .container
            .remove_event_listener_with_callback("click", self.callback.as_ref().unchecked_ref());
        self.container.remove();
    }
}

fn mount_container(app: &Rc<RefCell<App>>) -> Result<Element, JsValue> {
    let document = app
        .borrow()
        .canvas
        .owner_document()
        .ok_or_else(|| JsValue::from_str("no document"))?;
    let container = document.create_element("details")?;
    container.set_id("phux-approvals");
    container.set_attribute("hidden", "")?;
    container.set_attribute("open", "")?;
    container.set_attribute("style", &panel_style())?;
    // Static markup only. Every server value is assigned through textContent below.
    container.set_inner_html("<summary>Approvals</summary><button type='button' data-action='refresh-approvals'>Refresh approvals</button><div class='phux-approval-status' role='status' aria-live='polite'></div><div class='phux-approval-records'></div>");
    mount_beside_canvas(&document, &app.borrow().canvas, &container)?;
    Ok(container)
}

/// The inbox is a raised card: `layer.raised` with a `border.base` edge and
/// `radius.md`, from the page's tokens or the house fallbacks.
fn panel_style() -> String {
    use phux_design::tokens::{RADIUS, SPACE};
    let house = crate::ink::house();
    format!(
        "max-height:50vh;overflow:auto;padding:{};border:{} solid {};border-radius:{};\
         background:{};color:{}",
        crate::ink::px("space-s2", SPACE.s2),
        crate::ink::px("space-hairline", SPACE.hairline),
        crate::ink::color("border-base", house.border.base),
        crate::ink::px("radius-md", RADIUS.md),
        crate::ink::color("layer-raised", house.layer.raised),
        crate::ink::color("text-text", house.text.text),
    )
}

fn mount_beside_canvas(
    document: &web_sys::Document,
    canvas: &HtmlCanvasElement,
    container: &Element,
) -> Result<(), JsValue> {
    if let Some(parent) = canvas.parent_element() {
        parent.insert_before(container, Some(canvas))?;
    } else {
        // Embedders may install input before attaching their canvas, just like
        // the existing input surface. Disposal still owns/removes this element.
        document
            .body()
            .ok_or_else(|| JsValue::from_str("no approval host element"))?
            .append_child(container)?;
    }
    Ok(())
}
fn click(app: &Rc<RefCell<App>>, event: Event) {
    let Some(button) = event
        .target()
        .and_then(|target| target.dyn_into::<Element>().ok())
        .and_then(|target| {
            target
                .closest("button[data-approval-id],button[data-action='refresh-approvals']")
                .ok()
                .flatten()
        })
    else {
        return;
    };
    if button.has_attribute("disabled") {
        return;
    }
    let frames = if button.get_attribute("data-action").as_deref() == Some("refresh-approvals") {
        app.borrow_mut().session.refresh_approvals()
    } else {
        let Some(id) = button.get_attribute("data-approval-id") else {
            return;
        };
        let approve = button.get_attribute("data-decision").as_deref() == Some("approve");
        app.borrow_mut().session.decide_approval(&id, approve)
    };
    if frames.is_empty() {
        return;
    }
    let sent = app.borrow().send(frames);
    if let Err(message) = sent {
        super::close_with_transport_error(app, &message);
        return;
    }
    paint(&app.borrow());
}

pub(super) fn install(app: &Rc<RefCell<App>>) -> Result<(), JsValue> {
    let container = mount_container(app)?;
    let weak = Rc::downgrade(app);
    let callback = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
        let Some(app) = weak.upgrade() else { return };
        click(&app, event);
    });
    container.add_event_listener_with_callback("click", callback.as_ref().unchecked_ref())?;
    let old = app
        .borrow()
        .bindings
        .borrow_mut()
        .approvals
        .replace(ApprovalBinding {
            container,
            callback,
        });
    if let Some(old) = old {
        drop(old);
    }
    paint(&app.borrow());
    Ok(())
}

fn identity(requester: &Requester) -> String {
    format!(
        "client {} · {}",
        requester.client,
        requester
            .credential_id
            .as_deref()
            .unwrap_or("local connection (no workload credential)")
    )
}

fn details(record: &Approval) -> String {
    let mut lines = vec![format!("Requester: {}", identity(&record.requester))];
    if let Some(credential) = &record.credential {
        lines.extend([
            format!(
                "Purpose (untrusted requester text): {}",
                credential.request.purpose.escape_debug()
            ),
            format!(
                "Provider label (not identity): {}",
                credential.request.provider.escape_debug()
            ),
            format!("Bound provider: {}", identity(&credential.provider)),
            format!(
                "Announced name (not identity): {}",
                credential
                    .provider
                    .client_name
                    .as_deref()
                    .unwrap_or("none")
                    .escape_debug()
            ),
            "Exact 1Password references:".to_owned(),
        ]);
        lines.extend(credential.request.references.iter().cloned());
        lines.push("The provider desktop must unlock 1Password locally. No secret values are displayed here.".to_owned());
    } else {
        lines.push(format!(
            "Method: {}{}",
            record.method,
            record
                .signal
                .as_ref()
                .map_or_else(String::new, |signal| format!(" ({signal})"))
        ));
        lines.extend(record.subjects.iter().cloned());
    }
    lines.push(format!(
        "Approval: {} · {}",
        record.id,
        expires_in(record.expires_at_ms as f64 - js_sys::Date::now())
    ));
    lines.join("\n")
}

/// How long until an approval lapses, in relative words, never an epoch.
fn expires_in(ms: f64) -> String {
    if ms <= 0.0 {
        return "expired".to_owned();
    }
    let minutes = (ms / 60_000.0).ceil();
    if minutes < 2.0 {
        return "expires in under a minute".to_owned();
    }
    if minutes < 120.0 {
        return format!("expires in {minutes} minutes");
    }
    format!("expires in {} hours", (minutes / 60.0).round())
}

fn card(app: &App, record: &Approval, document: &web_sys::Document) -> Result<Element, JsValue> {
    let card = document.create_element("article")?;
    card.set_attribute("data-approval-card", &record.id)?;
    let title = if record.credential.is_some() {
        "1Password credential request"
    } else {
        "Held action"
    };
    append_text(&card, "h3", title, document)?;
    let body = append_text(&card, "pre", &details(record), document)?;
    body.set_attribute("style", "white-space:pre-wrap;overflow-wrap:anywhere")?;
    let disabled = app.session.approval_deciding(&record.id)
        || js_sys::Date::now() >= record.expires_at_ms as f64;
    append_buttons(&card, &record.id, disabled, document)?;
    Ok(card)
}

fn append_text(
    parent: &Element,
    tag: &str,
    text: &str,
    document: &web_sys::Document,
) -> Result<Element, JsValue> {
    let element = document.create_element(tag)?;
    element.set_text_content(Some(text));
    parent.append_child(&element)?;
    Ok(element)
}

fn append_buttons(
    card: &Element,
    id: &str,
    disabled: bool,
    document: &web_sys::Document,
) -> Result<(), JsValue> {
    for (decision, label) in [("deny", "Deny"), ("approve", "Approve")] {
        let button = document.create_element("button")?;
        button.set_attribute("type", "button")?;
        button.set_attribute("data-approval-id", id)?;
        button.set_attribute("data-decision", decision)?;
        // Deny is destructive (the danger variant); Approve is the view's one
        // primary action (§1.10). index.html styles both.
        button.set_attribute(
            "data-variant",
            if decision == "deny" {
                "danger"
            } else {
                "primary"
            },
        )?;
        button.set_text_content(Some(label));
        if disabled {
            button.set_attribute("disabled", "")?;
        }
        card.append_child(&button)?;
    }
    Ok(())
}

pub(super) fn paint(app: &App) {
    let container = app
        .bindings
        .borrow()
        .approvals
        .as_ref()
        .map(|binding| binding.container.clone());
    let Some(container) = container else { return };
    if !app.session.approvals_supported() {
        let _ = container.set_attribute("hidden", "");
        return;
    }
    let _ = container.remove_attribute("hidden");
    let records = app.session.approvals();
    if let Ok(Some(summary)) = container.query_selector("summary") {
        summary.set_text_content(Some(&format!("Approvals ({})", records.len())));
    }
    if let Ok(Some(status)) = container.query_selector(".phux-approval-status") {
        status.set_text_content(Some(app.session.approval_status()));
    }
    let Some(document) = container.owner_document() else {
        return;
    };
    let Ok(Some(list)) = container.query_selector(".phux-approval-records") else {
        return;
    };
    update_cards(app, &list, &document, &records);
}

fn update_cards(app: &App, list: &Element, document: &web_sys::Document, records: &[&Approval]) {
    let mut existing = list.first_element_child();
    while let Some(element) = existing {
        existing = element.next_element_sibling();
        if !records
            .iter()
            .any(|record| Some(&record.id) == element.get_attribute("data-approval-card").as_ref())
        {
            element.remove();
        }
    }
    for record in records {
        // Parsed approval ids are hex, never arbitrary selector text.
        if phux_protocol::ids::ApprovalId::parse(&record.id).is_none() {
            continue;
        }
        let selector = format!("[data-approval-card='{}']", record.id);
        match list.query_selector(&selector) {
            Ok(Some(existing)) => update_buttons(app, &existing, record),
            _ => {
                if let Ok(created) = card(app, record, document) {
                    let _ = list.append_child(&created);
                }
            }
        }
    }
}

fn update_buttons(app: &App, element: &Element, record: &Approval) {
    let disabled = app.session.approval_deciding(&record.id)
        || js_sys::Date::now() >= record.expires_at_ms as f64;
    for decision in ["approve", "deny"] {
        if let Ok(Some(button)) =
            element.query_selector(&format!("button[data-decision='{decision}']"))
        {
            if disabled {
                let _ = button.set_attribute("disabled", "");
            } else {
                let _ = button.remove_attribute("disabled");
            }
        }
    }
}

/// Keep uncertain outcomes in this page's DOM across connection replacement,
/// without persisting them to storage or leaving old decision buttons enabled.
pub(super) fn retain_uncertainty(canvas: &HtmlCanvasElement, status: &str) {
    if !status.to_ascii_lowercase().contains("unconfirmed") {
        return;
    }
    let Some(document) = canvas.owner_document() else {
        return;
    };
    let notice = document
        .get_element_by_id("phux-approval-uncertainty")
        .or_else(|| {
            let notice = document.create_element("div").ok()?;
            notice.set_id("phux-approval-uncertainty");
            notice.set_attribute("role", "alert").ok()?;
            mount_beside_canvas(&document, canvas, &notice).ok()?;
            Some(notice)
        });
    if let Some(notice) = notice {
        notice.set_text_content(Some(status));
    }
}

#[cfg(test)]
mod tests;
