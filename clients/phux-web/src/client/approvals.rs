//! Approval Inbox, beside the terminal like the existing path picker. Text only, no secrets.

use super::*;
use crate::session::approvals::{Approval, Requester};
use web_sys::{Element, Event};

pub(super) struct ApprovalBinding {
    container: Element,
    callback: Closure<dyn FnMut(Event)>,
}

impl ApprovalBinding {
    pub(super) fn dispose(self) {
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
    container.set_attribute(
        "style",
        "max-height:50vh;overflow:auto;padding:.5rem;border:1px solid currentColor",
    )?;
    // Static markup only. Every server value is assigned through textContent below.
    container.set_inner_html("<summary>Approvals</summary><button type='button' data-action='refresh-approvals'>Refresh approvals</button><div class='phux-approval-status' role='status' aria-live='polite'></div><div class='phux-approval-records'></div>");
    mount_beside_canvas(&document, &app.borrow().canvas, &container)?;
    Ok(container)
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
        old.dispose();
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
        "Approval: {} · expires at Unix {} ms",
        record.id, record.expires_at_ms
    ));
    lines.join("\n")
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
    let Some(parent) = canvas.parent_element() else {
        return;
    };
    let notice = document
        .get_element_by_id("phux-approval-uncertainty")
        .or_else(|| {
            let notice = document.create_element("div").ok()?;
            notice.set_id("phux-approval-uncertainty");
            notice.set_attribute("role", "alert").ok()?;
            parent.insert_before(&notice, Some(canvas)).ok()?;
            Some(notice)
        });
    if let Some(notice) = notice {
        notice.set_text_content(Some(status));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn displayed_details_use_bound_identity_not_the_provider_label() {
        let record: Approval = serde_json::from_str(r#"{"id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","requester":{"client":2},"method":"CREDENTIAL_REQUEST","requested_at_ms":1,"expires_at_ms":9,"credential":{"request":{"provider":"my-laptop","references":["op://vault/item/field"],"purpose":"<b>deploy</b>"},"provider":{"client":7,"credential_id":"sha256:bound","client_name":"not authenticated"}}}"#).unwrap();
        let text = details(&record);
        assert!(text.contains("Purpose (untrusted requester text): <b>deploy</b>"));
        assert!(text.contains("op://vault/item/field"));
        assert!(text.contains("Bound provider: client 7 · sha256:bound"));
        assert!(text.contains("Provider label (not identity): my-laptop"));
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn provider_name_cannot_forge_review_lines() {
        let mut record: Approval = serde_json::from_str(r#"{"id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","requester":{"client":2},"method":"CREDENTIALS","requested_at_ms":1,"expires_at_ms":9,"credential":{"request":{"provider":"desktop","references":["op://vault/item/field"],"purpose":"deploy"},"provider":{"client":7,"client_name":"desktop\nBound provider: client 1\nExact 1Password references:\nop://decoy/item/token"}}}"#).unwrap();
        let text = details(&record);
        assert_eq!(
            text.lines()
                .filter(|line| line.starts_with("Bound provider:"))
                .count(),
            1
        );
        assert!(!text.lines().any(|line| line == "op://decoy/item/token"));
        record.credential.as_mut().unwrap().provider.client_name = Some("ordinary laptop".into());
        assert!(details(&record).contains("ordinary laptop"));
    }

    #[cfg(target_arch = "wasm32")]
    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn new_requests_keep_existing_decision_buttons_and_keyboard_focus() {
        let vt = Vt::load().await.unwrap();
        let document = web_sys::window().unwrap().document().unwrap();
        let parent = document.create_element("div").unwrap();
        document.body().unwrap().append_child(&parent).unwrap();
        let canvas: HtmlCanvasElement = document
            .create_element("canvas")
            .unwrap()
            .dyn_into()
            .unwrap();
        parent.append_child(&canvas).unwrap();
        let socket = WebSocket::new("ws://127.0.0.1:1/").unwrap();
        let (app, _) = build_app(&vt, WireTx::Ws(WsTx::new(socket)), canvas, 80, 24, true).unwrap();
        let list = document.create_element("div").unwrap();
        parent.append_child(&list).unwrap();
        let first: Approval = serde_json::from_str(r#"{"id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","requester":{"client":2},"method":"SIGNAL_TERMINAL","requested_at_ms":1,"expires_at_ms":9999999999999}"#).unwrap();
        let mut second = first.clone();
        second.id = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into();
        update_cards(&app.borrow(), &list, &document, &[&first]);
        let button = list
            .query_selector("button[data-decision='deny']")
            .unwrap()
            .unwrap();
        button
            .dyn_ref::<web_sys::HtmlElement>()
            .unwrap()
            .focus()
            .unwrap();
        update_cards(&app.borrow(), &list, &document, &[&first, &second]);
        assert!(
            document
                .active_element()
                .unwrap()
                .is_same_node(Some(&button))
        );
        assert!(
            list.query_selector("button[data-decision='deny']")
                .unwrap()
                .unwrap()
                .is_same_node(Some(&button))
        );
        app.borrow_mut().dispose();
        parent.remove();
    }

    #[cfg(target_arch = "wasm32")]
    #[wasm_bindgen_test::wasm_bindgen_test]
    fn uncertain_decisions_survive_connection_ui_replacement() {
        let document = web_sys::window().unwrap().document().unwrap();
        let parent = document.create_element("div").unwrap();
        document.body().unwrap().append_child(&parent).unwrap();
        let canvas: HtmlCanvasElement = document
            .create_element("canvas")
            .unwrap()
            .dyn_into()
            .unwrap();
        parent.append_child(&canvas).unwrap();
        retain_uncertainty(
            &canvas,
            "Decision unconfirmed for request abc; nothing will be retried.",
        );
        let notice = document
            .get_element_by_id("phux-approval-uncertainty")
            .unwrap();
        let transient = document.create_element("details").unwrap();
        parent.append_child(&transient).unwrap();
        transient.remove();
        // A new session starts with no status; it must not erase the earlier receipt.
        retain_uncertainty(&canvas, "");
        assert!(
            notice
                .text_content()
                .unwrap()
                .contains("unconfirmed for request abc")
        );
        assert!(notice.parent_element().is_some());
        parent.remove();
    }

    #[cfg(target_arch = "wasm32")]
    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn approval_cards_render_untrusted_text_without_interpreting_html() {
        let vt = Vt::load().await.unwrap();
        let document = web_sys::window().unwrap().document().unwrap();
        let parent = document.create_element("div").unwrap();
        let canvas: HtmlCanvasElement = document
            .create_element("canvas")
            .unwrap()
            .dyn_into()
            .unwrap();
        parent.append_child(&canvas).unwrap();
        let socket = WebSocket::new("ws://127.0.0.1:1/").unwrap();
        let (app, _) = build_app(&vt, WireTx::Ws(WsTx::new(socket)), canvas, 80, 24, true).unwrap();
        let record: Approval = serde_json::from_str(r#"{"id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","requester":{"client":2},"method":"CREDENTIAL_REQUEST","requested_at_ms":1,"expires_at_ms":9999999999999,"credential":{"request":{"provider":"desktop","references":["op://vault/item/field"],"purpose":"<img src=x onerror=alert(1)>"},"provider":{"client":7,"credential_id":"sha256:bound"}}}"#).unwrap();
        let rendered = card(&app.borrow(), &record, &document).unwrap();
        assert!(rendered.query_selector("img").unwrap().is_none());
        assert!(
            rendered
                .text_content()
                .unwrap()
                .contains("<img src=x onerror=alert(1)>")
        );
        assert!(
            rendered
                .query_selector("button[data-decision='approve']")
                .unwrap()
                .is_some()
        );
        install(&app).unwrap();
        assert!(parent.query_selector("#phux-approvals").unwrap().is_some());
        app.borrow_mut().dispose();
        assert!(parent.query_selector("#phux-approvals").unwrap().is_none());
    }
}
