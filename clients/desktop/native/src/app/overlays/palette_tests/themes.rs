use gpui::{TestAppContext, VisualTestContext};

use super::{fixture, keys, modal, render};
use crate::app::Modal;

#[gpui::test]
fn the_highlighted_theme_previews_escape_restores_and_enter_keeps(cx: &mut TestAppContext) {
    let (handle, desk) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    let theme = |cx: &VisualTestContext| desk.read_with(cx, |desk, _| desk.prefs.theme_id.clone());
    let original = theme(cx);
    let order: Vec<String> = desk.read_with(cx, |desk, _| {
        super::super::theme_panel::listed_themes(desk)
            .into_iter()
            .map(|theme| theme.id)
            .collect()
    });
    let at = order.iter().position(|id| *id == original).unwrap();
    let next = order[(at + 1) % order.len()].clone();
    let open = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| desk.update(cx, |desk, cx| desk.execute("theme", window, cx)));
        render(cx);
    };

    open(cx);
    assert_eq!(modal(&desk, cx), Modal::Themes);
    assert_eq!(theme(cx), original, "the list opens on the theme in use");
    keys(cx, "down");
    assert_eq!(theme(cx), next, "moving the highlight previews the theme");
    keys(cx, "escape");
    assert_eq!(modal(&desk, cx), Modal::None);
    assert_eq!(
        theme(cx),
        original,
        "escape restores the theme it opened on"
    );

    open(cx);
    keys(cx, "down enter");
    assert_eq!(modal(&desk, cx), Modal::None);
    assert_eq!(theme(cx), next, "enter keeps the highlighted theme");
    assert!(desk.read_with(cx, |desk, _| desk.theme_pick.is_none()));
}
