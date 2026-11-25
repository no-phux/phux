//! Browser design-token, badge, bell, and house-palette regressions.

use super::*;

// --- Design-token fixtures --------------------------

const PAGE: &str = include_str!("../../index.html");
const TOKENS: &str = include_str!("../../assets/phux-tokens.css");

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
