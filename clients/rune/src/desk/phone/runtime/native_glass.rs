//! The native host's Liquid Glass controls: Rune lays them out and runs
//! their presses; UIKit draws them only where glass may draw and nothing
//! covers them. Desktop and opaque phones keep Rune's own controls.

use super::*;
use crate::desk::phone::navigation::Page;
use serde_json::Value;

const TOP: [&str; 4] = ["phone-back", "phone-place", "phone-news", "phone-more"];

fn native() -> (std::sync::Arc<Client>, Runtime<Desk>) {
    let (client, mut rt) = fixture();
    rt.update(|d, tx| {
        d.native = Some(crate::mobile::Platform::test());
        tx.instant();
    });
    settle(&mut rt);
    (client, rt)
}

fn shown(rt: &Runtime<Desk>) -> Vec<Value> {
    crate::mobile::controls(rt).as_array().unwrap().clone()
}

fn ids(rt: &Runtime<Desk>) -> Vec<String> {
    shown(rt)
        .iter()
        .map(|c| c["id"].as_str().unwrap().to_owned())
        .collect()
}

fn control(rt: &Runtime<Desk>, id: &str) -> Value {
    shown(rt)
        .into_iter()
        .find(|c| c["id"] == id)
        .unwrap_or_else(|| panic!("no native {id}"))
}

fn element(rt: &Runtime<Desk>, key: &str) -> rune::ElementInfo {
    rt.elements()
        .into_iter()
        .find(|e| e.key.as_deref() == Some(key))
        .unwrap_or_else(|| panic!("no element {key}"))
}

#[test]
fn the_top_bar_is_native_glass_at_rune_layout_with_no_rune_paint() {
    let (_client, rt) = native();
    assert_eq!(ids(&rt), TOP);
    for id in TOP {
        let c = control(&rt, id);
        let (rect, _) = rt.presented(id).unwrap();
        let exported: Vec<f32> = c["rect"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_f64().unwrap() as f32)
            .collect();
        assert_eq!(exported, rect, "{id} sits exactly where Rune laid it out");
        assert!(
            rect[2] >= 44.0 && rect[3] >= 44.0,
            "{id} keeps its touch size"
        );
        assert!(c["symbol"].as_str().is_some_and(|s| !s.is_empty()));
        assert!(c["label"].as_str().is_some_and(|s| !s.is_empty()));
        assert_eq!(c["enabled"], true);
        // The placeholder is empty: Rune paints neither surface nor glyph
        // under the glass, which would otherwise blur through it.
        let info = element(&rt, id);
        assert_eq!(info.fill.a, 0.0, "{id} has no Rune surface");
        assert!(info.label.is_some(), "{id} keeps its accessibility name");
    }
    assert_eq!(control(&rt, "phone-back")["symbol"], "chevron.left");
    let place = control(&rt, "phone-place");
    assert_eq!(place["leading"], true);
    assert!(place["title"].is_string());
    assert_eq!(place["subtitle_ink"].as_array().map(Vec::len), Some(3));
}

#[test]
fn desktop_phones_and_opaque_accessibility_settings_keep_rune_controls() {
    // The desktop-hosted phone has no native host.
    let (_client, mut rt) = fixture();
    settle(&mut rt);
    assert!(ids(&rt).is_empty());
    assert!(
        element(&rt, "phone-back").fill.a > 0.0,
        "Rune paints the opaque control"
    );

    let (_client, mut rt) = native();
    rt.update(|d, tx| {
        d.set_accessibility(true, "UICTContentSizeCategoryL");
        tx.instant();
    });
    settle(&mut rt);
    assert!(ids(&rt).is_empty(), "Reduce Transparency draws opaque");
    assert!(element(&rt, "phone-back").fill.a > 0.0);

    let (_client, mut rt) = native();
    rt.set_system_increased_contrast(true);
    settle(&mut rt);
    assert!(ids(&rt).is_empty(), "Increased Contrast draws opaque");
}

#[test]
fn a_native_tap_runs_the_controls_press_and_a_covering_page_hides_it() {
    let (_client, mut rt) = native();
    assert!(
        !crate::mobile::press(&mut rt, "phone-session-new"),
        "not shown this frame"
    );
    assert!(crate::mobile::press(&mut rt, "phone-back"));
    settle(&mut rt);
    assert_eq!(rt.state.phone_page(), Some(Page::Sessions));
    // The drawer slides in over the top bar: let it land.
    for _ in 0..90 {
        rt.frame(1.0 / 60.0);
    }
    // The page covers the top bar: its glass would float over the page.
    let now = ids(&rt);
    assert!(!now.iter().any(|id| TOP.contains(&id.as_str())), "{now:?}");
    let dock = control(&rt, "phone-session-new");
    assert_eq!(dock["title"], "New session");
    assert_eq!(dock["label"], "New session");
    assert_eq!(dock["symbol"], "plus");
    assert_eq!(dock["enabled"], true);
    assert_eq!(element(&rt, "phone-session-new").fill.a, 0.0);
}

#[test]
fn the_bell_carries_its_unread_dot_natively() {
    let (_client, mut rt) = native();
    rt.update(|d, tx| {
        d.news.unread = false;
        tx.instant();
    });
    settle(&mut rt);
    assert_eq!(control(&rt, "phone-news")["label"], "What's new");
    assert_eq!(control(&rt, "phone-news")["badge"], Value::Null);
    rt.update(|d, tx| {
        d.news.unread = true;
        tx.instant();
    });
    settle(&mut rt);
    let bell = control(&rt, "phone-news");
    assert_eq!(bell["label"], "What's new, unread");
    assert_eq!(bell["badge"].as_array().map(Vec::len), Some(3));
}
