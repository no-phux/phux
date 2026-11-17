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
fn a_detached_canvas_keeps_its_uncertain_decision_notice() {
    let document = web_sys::window().unwrap().document().unwrap();
    let canvas: HtmlCanvasElement = document
        .create_element("canvas")
        .unwrap()
        .dyn_into()
        .unwrap();
    assert!(canvas.parent_element().is_none());
    retain_uncertainty(
        &canvas,
        "Decision unconfirmed for detached request; never retried.",
    );
    let notice = document
        .get_element_by_id("phux-approval-uncertainty")
        .unwrap();
    assert!(document.body().unwrap().contains(Some(&notice)));
    retain_uncertainty(&canvas, "");
    assert!(
        notice
            .text_content()
            .unwrap()
            .contains("unconfirmed for detached request")
    );
    notice.remove();
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
