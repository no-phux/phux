//! The working thread stays quiet; actionable exceptions never disappear.

use super::*;
use gpui::{TestAppContext, VisualTestContext, size};

fn fixture(cx: &mut TestAppContext) -> (gpui::AnyWindowHandle, Entity<Desk>, String) {
    let (handle, desk) = crate::app::overlays::palette_tests::fixture(cx);
    let mut connection = crate::workspace::view_tests::attached();
    let workspace = crate::workspace::view_tests::opened(&mut connection);
    let placement = workspace.focused().unwrap();
    let id = placement.id.clone();
    preview::seed(&mut connection, placement.terminal_id.clone().unwrap());
    desk.update(cx, |desk, cx| {
        desk.link = Some(crate::app::desk::Link {
            connection,
            workspace,
        });
        desk.prefs.native_agents = true;
        desk.prefs.animations = false;
        cx.notify();
    });
    (handle, desk, id)
}

#[test]
fn normal_composing_has_no_instructional_footer_but_blockers_remain_visible() {
    use super::render::composer_hint;
    assert_eq!(composer_hint(true, false, true, false), None);
    assert_eq!(composer_hint(false, false, true, false), None);
    assert_eq!(composer_hint(true, false, false, true), None);
    assert_eq!(
        composer_hint(true, true, false, false),
        Some("Answer the request above to continue")
    );
    assert_eq!(
        composer_hint(false, true, false, false),
        Some("Open Terminal to answer the request")
    );
    assert_eq!(composer_hint(true, false, false, false), Some("Read-only"));
}

#[gpui::test]
fn reply_surface_keeps_send_anchored_and_questions_and_disconnects_visible(
    cx: &mut TestAppContext,
) {
    let _dial = crate::connection::DeterministicDial::arm();
    let (handle, desk, placement) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    for width in [480., 760., 1280.] {
        cx.simulate_resize(size(px(width), px(800.)));
        draw(cx);
        let composer = cx.debug_bounds("conversation-composer").expect("composer");
        let send_selector = "conversation-send";
        let send = cx.debug_bounds(send_selector).expect("send action");
        assert!(composer.contains(&send.center()) && send.right() <= composer.right());
        assert!(composer.size.width <= px(720.));
        let answer = cx
            .debug_bounds("conversation-entry-answer")
            .expect("latest answer");
        assert!(
            composer.top() - answer.bottom() <= px(32.),
            "the latest answer stays near the reply box: {answer:?} before {composer:?}"
        );
        assert!(cx.debug_bounds("conversation-composer-hint").is_none());
        assert!(cx.debug_bounds("conversation-stop").is_none());
        desk.update(cx, |desk, cx| {
            preview::scene(desk, "conversation-working");
            cx.notify();
        });
        draw(cx);
        assert!(cx.debug_bounds("conversation-stop").is_some());
        assert_eq!(
            cx.debug_bounds(send_selector).unwrap().right(),
            send.right()
        );
        desk.update(cx, |desk, cx| {
            preview::scene(desk, "conversation-needs-you");
            cx.notify();
        });
        draw(cx);
        assert!(cx.debug_bounds("conversation-composer-hint").is_some());
        // The provider request is still held while the composer is blocked.
        assert!(!desk.read_with(cx, |desk, _| {
            desk.link.as_ref().unwrap().connection.conversations[&ResourceId::local(10)]
                .native
                .as_ref()
                .unwrap()
                .questions
                .is_empty()
        }));
        desk.update(cx, |desk, cx| {
            preview::scene(desk, "conversation-disconnected");
            cx.notify();
        });
        draw(cx);
        assert!(cx.debug_bounds("conversation-composer-hint").is_some());
        assert!(!desk.read_with(cx, |desk, _| desk.conversation_writable(&placement, false)));
        desk.update(cx, |desk, cx| {
            preview::scene(desk, "conversation");
            cx.notify();
        });
    }
}

fn draw(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

#[gpui::test]
fn enter_submits_without_inserting_a_newline_and_shift_enter_keeps_composing(
    cx: &mut TestAppContext,
) {
    let _dial = crate::connection::DeterministicDial::arm();
    let (handle, desk, placement) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    draw(cx);
    cx.update(|window, cx| {
        desk.update(cx, |desk, cx| {
            assert!(desk.focus_conversation(&placement, window, cx));
        })
    });
    cx.simulate_input("Keep this");
    cx.simulate_keystrokes("shift-enter");
    cx.simulate_input("draft");
    draw(cx);
    let before = desk.read_with(cx, |desk, cx| {
        desk.conversation_faces[&placement]
            .input
            .read(cx)
            .value()
            .to_string()
    });
    assert_eq!(before, "Keep this\ndraft");
    cx.simulate_keystrokes("enter");
    draw(cx);
    desk.read_with(cx, |desk, cx| {
        let face = &desk.conversation_faces[&placement];
        // The fixture has no real ACP owner: the real write fence refuses it,
        // so the draft must remain exactly intact, not gain a newline.
        assert!(face.error.is_some());
        assert_eq!(face.input.read(cx).value().as_str(), before);
    });
}
