//! Visual details must expose information, not just fit the outer card.

use super::*;
use crate::app::{Desk, Kind, Modal};
use crate::connection::DeterministicDial;
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, px, size};

fn draw(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

fn open_notifications(desk: &Entity<Desk>, cx: &mut VisualTestContext) {
    cx.update(|window, cx| desk.update(cx, |desk, cx| desk.execute("notifications", window, cx)));
    draw(cx);
}

#[gpui::test]
fn general_alerts_open_full_details_instead_of_dismissing_their_only_explanation(
    cx: &mut TestAppContext,
) {
    let _dial = DeterministicDial::arm();
    let (handle, desk) = palette_tests::fixture(cx);
    let id = desk.update(cx, |desk, _| {
        desk.feed = Default::default();
        desk.feed.post(
            Kind::Error,
            "Could not connect",
            Some("First line\nSecond line\nThe complete recovery instruction is here.".into()),
        )
    });
    let cx = &mut VisualTestContext::from_window(handle, cx);
    cx.simulate_resize(size(px(640.), px(420.)));
    open_notifications(&desk, cx);
    let button = cx
        .debug_bounds("notification-details-1")
        .expect("full details are discoverable");
    cx.simulate_click(button.center(), Modifiers::none());
    draw(cx);
    desk.read_with(cx, |desk, _| {
        assert!(desk.feed.details_open(id));
        assert_eq!(desk.modal, Modal::Notifications);
    });
    let sheet = cx.debug_bounds("modal-sheet").unwrap();
    assert!(sheet.origin.y >= px(0.) && sheet.bottom() <= px(420.));
    cx.update(|window, cx| desk.update(cx, |desk, cx| desk.open_notification(id, window, cx)));
    draw(cx);
    assert_eq!(
        desk.read_with(cx, |desk, _| desk.modal.clone()),
        Modal::Notifications
    );
    desk.update(cx, |desk, _| {
        desk.feed.clear();
        assert!(
            !desk.feed.details_open(id),
            "disclosures retire with feed entries"
        );
    });
}
