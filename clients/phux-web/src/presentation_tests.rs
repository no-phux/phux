//! Browser regressions against the actual Ghostty replicas and canvas.
#![allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]

use super::*;
use phux_protocol::ids::{BootstrapId, ResourceId, StreamId};
use wasm_bindgen_test::wasm_bindgen_test;

async fn fixture(count: u32) -> Client {
    let vt = Vt::load().await.unwrap();
    let document = web_sys::window().unwrap().document().unwrap();
    let canvas = document
        .create_element("canvas")
        .unwrap()
        .dyn_into()
        .unwrap();
    let socket = WebSocket::new("ws://127.0.0.1:1/").unwrap();
    let (app, _ready) =
        build_app(&vt, WireTx::Ws(WsTx::new(socket)), canvas, 80, 24, false).unwrap();
    for frame in super::tests::attached_frames(count, b"\x1b[48;2;255;0;0m \x1b[0mreal") {
        assert!(app.borrow_mut().session.on_frame(frame).fatal.is_none());
    }
    Client { app }
}

fn field(value: &JsValue, key: &str) -> JsValue {
    js_sys::Reflect::get(value, &key.into()).unwrap()
}
fn number(value: &JsValue, key: &str) -> f64 {
    field(value, key).as_f64().unwrap()
}

#[wasm_bindgen_test]
async fn presentation_metadata_matches_real_pane_rectangles_and_painted_pixels() {
    let client = fixture(2).await;
    let canvas = client.app.borrow().canvas.clone();
    let renders = Rc::new(RefCell::new(Vec::new()));
    let recorded = Rc::clone(&renders);
    let app = Rc::clone(&client.app);
    let listener =
        Closure::<dyn FnMut(web_sys::CustomEvent)>::new(move |event: web_sys::CustomEvent| {
            // A listener is allowed to borrow mutably: dispatch must be outside App's borrow.
            let reentrant = Client {
                app: Rc::clone(&app),
            };
            reentrant.focus_pane("@1").unwrap();
            recorded.borrow_mut().push(event.detail());
        });
    canvas
        .add_event_listener_with_callback(RENDER_EVENT, listener.as_ref().unchecked_ref())
        .unwrap();
    assert!(renders.borrow().is_empty(), "no event before painting");
    paint_and_publish(&client.app);
    let detail = renders.borrow()[0].clone();
    assert_eq!(number(&detail, "cols"), 80.0);
    assert_eq!(number(&detail, "rows"), 24.0);
    assert_eq!(number(&detail, "pixelWidth"), f64::from(canvas.width()));
    assert_eq!(number(&detail, "pixelHeight"), f64::from(canvas.height()));
    let panes = js_sys::Array::from(&field(&detail, "panes"));
    assert_eq!(panes.length(), 2);
    assert_eq!(
        field(&panes.get(0), "id").as_string().as_deref(),
        Some("@1")
    );
    assert_eq!(number(&panes.get(0), "x"), 0.0);
    assert_eq!(number(&panes.get(0), "cols"), 39.0);
    assert_eq!(number(&panes.get(1), "x"), 40.0);
    assert_eq!(number(&panes.get(1), "cols"), 40.0);
    assert_eq!(number(&panes.get(0), "cursorRow"), 0.0);
    assert_eq!(number(&panes.get(1), "cursorRow"), 0.0);
    {
        let mut app = client.app.borrow_mut();
        app.session.on_frame(FrameKind::ResourceOutput {
            terminal_id: ResourceId::new(2),
            stream_id: StreamId::new(2).unwrap(),
            bootstrap_id: BootstrapId::new(2).unwrap(),
            seq: 1,
            bytes: bytes::Bytes::from_static(b"\r\nnext\r\nrow"),
        });
    }
    paint_and_publish(&client.app);
    let latest = renders.borrow().last().unwrap().clone();
    let latest_panes = js_sys::Array::from(&field(&latest, "panes"));
    assert_eq!(number(&latest_panes.get(0), "cursorRow"), 0.0);
    assert_eq!(number(&latest_panes.get(1), "cursorRow"), 2.0);
    let pixel = client
        .app
        .borrow()
        .ctx
        .get_image_data(3, 3, 1, 1)
        .unwrap()
        .data();
    assert!(
        pixel[0] > 200 && pixel[1] < 60 && pixel[2] < 60,
        "real red VT cell painted"
    );
    canvas
        .remove_event_listener_with_callback(RENDER_EVENT, listener.as_ref().unchecked_ref())
        .unwrap();
    client.close();
}

#[wasm_bindgen_test]
async fn presentation_state_preserves_old_fields_and_direct_focus_rejects_hidden_ids() {
    let client = fixture(2).await;
    let canvas = client.app.borrow().canvas.clone();
    let seen = Rc::new(RefCell::new(Vec::new()));
    let record = Rc::clone(&seen);
    let listener =
        Closure::<dyn FnMut(web_sys::CustomEvent)>::new(move |event: web_sys::CustomEvent| {
            record.borrow_mut().push(event.detail());
        });
    canvas
        .add_event_listener_with_callback("phux-panes", listener.as_ref().unchecked_ref())
        .unwrap();
    client.focus_pane("@2").unwrap();
    let detail = seen.borrow().last().unwrap().clone();
    assert_eq!(number(&detail, "count"), 2.0);
    assert_eq!(field(&detail, "focused").as_string().as_deref(), Some("@2"));
    assert_eq!(field(&detail, "pending").as_bool(), Some(false));
    assert_eq!(js_sys::Array::from(&field(&detail, "panes")).length(), 2);
    assert!(client.focus_pane("@999").is_err());
    assert!(
        client.focus_pane("2").is_err(),
        "canonical exact id required"
    );
    client.app.borrow_mut().session.close_view().unwrap();
    assert!(
        client.focus_pane("@2").is_err(),
        "closed view is not targetable"
    );
    assert!(
        client
            .app
            .borrow()
            .session
            .pane_terminal(&ResourceId::new(2))
            .is_some(),
        "closing view preserves resource"
    );
    client.focus_pane("@1").unwrap();
    client.app.borrow_mut().session.fail_protocol("test");
    assert!(client.focus_pane("@1").is_err());
    canvas
        .remove_event_listener_with_callback("phux-panes", listener.as_ref().unchecked_ref())
        .unwrap();
    client.close();
}

#[wasm_bindgen_test]
async fn presentation_single_pane_resize_uses_published_geometry_until_new_bootstrap() {
    let client = fixture(1).await;
    let _ = client.app.borrow_mut().session.resize_panes(100, 30);
    let detail = client.app.borrow().presentation_detail();
    assert_eq!(number(&detail, "cols"), 80.0);
    assert_eq!(number(&detail, "rows"), 24.0);
    let pane = js_sys::Array::from(&field(&detail, "panes")).get(0);
    assert_eq!(number(&pane, "cols"), 80.0);
    assert_eq!(number(&pane, "rows"), 24.0);
    client.close();
}

#[wasm_bindgen_test]
async fn presentation_send_text_bounds_utf8_and_refuses_unavailable_input() {
    let client = fixture(1).await;
    assert!(client.send_text("").is_ok());
    assert!(client.send_text(&"a".repeat(4097)).is_err());
    assert!(client.send_text(&"é".repeat(2049)).is_err());
    client.app.borrow_mut().session.fail_protocol("test");
    assert!(client.send_text("echo actual").is_err());
    client.close();
}

#[wasm_bindgen_test]
async fn glass_hosted_mode_clears_backdrop_borders_and_cursor_then_returns_opaque() {
    let client = fixture(2).await;
    paint_and_publish(&client.app);
    let sample = |x: i32, y: i32| {
        client
            .app
            .borrow()
            .ctx
            .get_image_data(x, y, 1, 1)
            .unwrap()
            .data()
            .to_vec()
    };
    let metrics = client.app.borrow().metrics.clone();
    let blank_y = (metrics.cell_h * 2.0 + 2.0) as i32;
    assert_eq!(sample(2, blank_y)[3], 255, "default client remains opaque");
    client.set_glass_mode(true);
    paint_and_publish(&client.app);
    assert_eq!(sample(2, blank_y), [0, 0, 0, 0], "default rows transparent");
    assert_eq!(sample(0, blank_y), [0, 0, 0, 0], "no native pane border");
    assert_eq!(
        sample((metrics.cell_w * 39.0 + 2.0) as i32, blank_y),
        [0, 0, 0, 0],
        "no opaque split separator"
    );
    assert_eq!(
        sample(3, 3),
        [255, 0, 0, 255],
        "explicit ANSI survives full paint"
    );
    let grid = client.app.borrow().session.grid();
    let cursor_x = (f64::from(grid.cursor_col) * metrics.cell_w + 1.0) as i32;
    let cursor_y = (f64::from(grid.cursor_row) * metrics.cell_h + 1.0) as i32;
    assert_eq!(sample(cursor_x, cursor_y)[3], 255);
    client.app.borrow().blink();
    assert_eq!(
        sample(cursor_x, cursor_y)[3],
        0,
        "cursor row clears rather than filling opaque default"
    );
    client.app.borrow().blink();
    assert_eq!(sample(cursor_x, cursor_y)[3], 255);
    client.app.borrow().blink();
    assert_eq!(sample(cursor_x, cursor_y)[3], 0);
    assert_eq!(sample(3, 3), [255, 0, 0, 255]);
    client.set_glass_mode(false);
    paint_and_publish(&client.app);
    assert_eq!(
        sample(2, blank_y)[3],
        255,
        "opt-out restores opaque presentation"
    );
    client.close();
}

// --- Design-token fixtures --------------------------

const PAGE: &str = include_str!("../index.html");
const TOKENS: &str = include_str!("../assets/phux-tokens.css");

/// The page's own stylesheet: the `<style>` of `index.html`.
fn page_style() -> &'static str {
    let start = PAGE.find("<style>").unwrap() + "<style>".len();
    let end = PAGE.find("</style>").unwrap();
    &PAGE[start..end]
}

/// A host element under the real token and page stylesheets, in `theme`
/// (`data-theme`) and direction `dir`.
fn styled_host(theme: &str, dir: &str) -> web_sys::Element {
    let document = web_sys::window().unwrap().document().unwrap();
    if document.get_element_by_id("phux-test-styles").is_none() {
        let style = document.create_element("style").unwrap();
        style.set_id("phux-test-styles");
        style.set_text_content(Some(&format!("{TOKENS}\n{}", page_style())));
        document.body().unwrap().append_child(&style).unwrap();
    }
    let host = document.create_element("div").unwrap();
    host.set_attribute("data-theme", theme).unwrap();
    host.set_attribute("dir", dir).unwrap();
    document.body().unwrap().append_child(&host).unwrap();
    host
}

fn computed(element: &web_sys::Element, property: &str) -> String {
    web_sys::window()
        .unwrap()
        .get_computed_style(element)
        .unwrap()
        .unwrap()
        .get_property_value(property)
        .unwrap()
}

/// The red, green and blue channels of a computed colour.
fn channels(computed: &str) -> Vec<u8> {
    computed
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|n| n.parse().ok())
        .collect()
}

fn badge(state: &str) -> crate::AgentBadge {
    crate::AgentBadge {
        id: ResourceId::new(7),
        parent: None,
        provider: "claude".to_owned(),
        state: state.to_owned(),
    }
}

#[wasm_bindgen_test]
fn badges_draw_every_tone_in_its_token_colour_with_mark_and_word() {
    use phux_design::{Mode, Roles, State};
    let document = web_sys::window().unwrap().document().unwrap();
    for (theme, mode) in [("dark", Mode::Dark), ("light", Mode::Light)] {
        let host = styled_host(theme, "ltr");
        let roles = Roles::house(mode);
        for state in State::ALL {
            let span = badges::badge_element(&document, &badge("x"), "claude", state).unwrap();
            host.append_child(&span).unwrap();
            assert_eq!(
                span.get_attribute("data-tone").as_deref(),
                Some(state.tone().key())
            );
            assert!(
                span.query_selector("svg.phux-mark").unwrap().is_some(),
                "mark"
            );
            let ink = roles.tone(state.tone()).ink;
            assert_eq!(
                channels(&computed(&span, "color")),
                [ink.r, ink.g, ink.b],
                "{theme} {state:?} draws in status.{}",
                state.tone().key()
            );
            let text = span.text_content().unwrap();
            match state.word() {
                Some(word) => assert_eq!(text, format!("claude {word}")),
                None => assert_eq!(text, "claude", "unknown shows the provider only"),
            }
        }
        host.remove();
    }
}

#[wasm_bindgen_test]
fn badges_lead_with_their_mark_in_right_to_left_layouts() {
    use phux_design::State;
    let document = web_sys::window().unwrap().document().unwrap();
    for dir in ["ltr", "rtl"] {
        let host = styled_host("dark", dir);
        let span =
            badges::badge_element(&document, &badge("blocked"), "claude", State::NeedsYou).unwrap();
        host.append_child(&span).unwrap();
        let mark = span
            .query_selector("svg")
            .unwrap()
            .unwrap()
            .get_bounding_client_rect();
        let word = span
            .query_selector(".phux-agent-word")
            .unwrap()
            .unwrap()
            .get_bounding_client_rect();
        if dir == "rtl" {
            assert!(
                mark.left() > word.left(),
                "the mark sits at the inline start (right)"
            );
        } else {
            assert!(mark.left() < word.left());
        }
        host.remove();
    }
}

#[wasm_bindgen_test]
fn forced_colours_keep_marks_words_and_an_outline_focus_ring() {
    let forced = TOKENS
        .split("@media (forced-colors: active)")
        .nth(1)
        .expect("the tokens map forced colours");
    for role in [
        "--phux-status-needs-you: CanvasText",
        "--phux-status-failed: CanvasText",
        "--phux-border-focus: CanvasText",
        "--phux-layer-ground: Canvas",
    ] {
        assert!(forced.contains(role), "{role}");
    }
    let page = page_style();
    assert!(page.contains("@media (forced-colors: active)"));
    let focus = page.split(":focus-visible {").nth(1).unwrap();
    let focus = &focus[..focus.find('}').unwrap()];
    assert!(focus.contains("outline:"), "the focus ring is an outline");
    assert!(
        !page.contains("box-shadow"),
        "no ring that forced colours would drop"
    );
}

#[wasm_bindgen_test]
async fn the_unread_bell_dot_is_named_and_cleared_by_input_and_rows_stay_quiet() {
    let client = fixture(1).await;
    let document = web_sys::window().unwrap().document().unwrap();
    let app = client.app.borrow();
    app.paint_badges();
    let container = document.get_element_by_id(BADGE_CONTAINER_ID).unwrap();
    assert!(container.has_attribute("hidden"), "no agents, no bell");
    assert!(
        !container.has_attribute("aria-live"),
        "the list is not a live region"
    );
    app.bell_unread.set(true);
    app.paint_badges();
    assert!(!container.has_attribute("hidden"));
    let dot = container
        .query_selector(&format!(".{BELL_DOT_CLASS}"))
        .unwrap()
        .expect("the unread dot");
    assert_eq!(dot.get_attribute("aria-label").as_deref(), Some("bell"));
    app.acknowledge_input();
    assert!(
        container
            .query_selector(&format!(".{BELL_DOT_CLASS}"))
            .unwrap()
            .is_none()
    );
    assert!(container.has_attribute("hidden"));
    drop(app);
    container.remove();
    client.close();
}

#[wasm_bindgen_test]
async fn the_backdrop_paints_layer_ground_of_the_house_palette() {
    let client = fixture(2).await;
    paint_and_publish(&client.app);
    let app = client.app.borrow();
    let metrics = app.metrics.clone();
    // The column between the two panes shows the backdrop.
    let gap_x = (metrics.cell_w * 39.5) as i32;
    let y = (metrics.cell_h * 3.5) as i32;
    let ground = crate::ink::house().layer.ground;
    assert_eq!(
        app.ctx
            .get_image_data(gap_x, y, 1, 1)
            .unwrap()
            .data()
            .to_vec(),
        vec![ground.r, ground.g, ground.b, 255]
    );
    drop(app);
    client.close();
}

#[wasm_bindgen_test]
fn a_finished_agent_stays_seen_until_it_works_again_and_moves_are_announced_once() {
    use phux_design::State;
    let mut memory = badges::BadgeMemory::default();
    let id = ResourceId::new(7);
    assert_eq!(
        memory.settle(&id, "working", None, false),
        (State::Working, None),
        "first sight is not a transition"
    );
    assert_eq!(
        memory.settle(&id, "blocked", None, false),
        (State::NeedsYou, Some(State::Working))
    );
    assert_eq!(
        memory.settle(&id, "blocked", None, false),
        (State::NeedsYou, None),
        "no repeat while it stays"
    );
    assert_eq!(memory.settle(&id, "done", None, false).0, State::Done);
    assert!(memory.see_done(), "typing sees it");
    for _ in 0..3 {
        assert_eq!(memory.settle(&id, "done", None, false).0, State::DoneSeen);
    }
    assert!(!memory.see_done(), "nothing new to see");
    assert_eq!(memory.settle(&id, "working", None, false).0, State::Working);
    assert_eq!(
        memory.settle(&id, "done", None, false).0,
        State::Done,
        "a new finish is unseen again"
    );
    memory.keep_only(&[]);
    assert_eq!(memory.settle(&id, "done", None, false), (State::Done, None));
}
