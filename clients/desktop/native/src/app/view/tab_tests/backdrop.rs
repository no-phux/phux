//! The backdrop shader layer under the desk chrome.

use super::*;

#[gpui::test]
fn a_backdrop_shader_follows_its_preference_and_steps_aside_for_plain(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    let showing = |desk: &Entity<Desk>, cx: &mut VisualTestContext| {
        desk.read_with(cx, |desk, _| desk.backdrop_shader.view().is_some())
    };
    draw(cx);
    assert!(!showing(&desk, cx), "off by default");
    desk.update(cx, |desk, cx| {
        desk.update_prefs(|prefs| prefs.backdrop_shader = "aurora".into(), cx);
    });
    draw(cx);
    assert!(showing(&desk, cx));
    desk.update(cx, |desk, cx| {
        desk.update_prefs(|prefs| prefs.plain = true, cx)
    });
    draw(cx);
    assert!(!showing(&desk, cx), "plain terminal draws no backdrop");
    desk.update(cx, |desk, cx| {
        desk.update_prefs(
            |prefs| {
                prefs.plain = false;
                prefs.backdrop_shader = "file:phux-test-absent.wgsl".into();
            },
            cx,
        );
    });
    draw(cx);
    desk.read_with(cx, |desk, cx| {
        assert!(desk.backdrop_shader.view().is_none());
        let problem = desk
            .backdrop_shader
            .problem(cx)
            .expect("a missing file is a problem");
        assert!(problem.contains("could not read"), "{problem}");
    });
    // Drawing again does not post the same error twice.
    let errors = |desk: &Entity<Desk>, cx: &mut VisualTestContext| {
        desk.read_with(cx, |desk, _| {
            desk.feed
                .items()
                .filter(|item| item.title == "Backdrop shader did not compile")
                .count()
        })
    };
    assert_eq!(errors(&desk, cx), 1);
    desk.update(cx, |_, cx| cx.notify());
    draw(cx);
    assert_eq!(errors(&desk, cx), 1);
    desk.update(cx, |desk, cx| {
        desk.update_prefs(|prefs| prefs.backdrop_shader = "off".into(), cx);
    });
    for expected in ["silk", "aurora"] {
        desk.update_in(cx, |desk, window, cx| {
            desk.execute("backdrop-shader-next", window, cx)
        });
        desk.read_with(cx, |desk, _| {
            assert_eq!(desk.prefs.backdrop_shader, expected)
        });
    }
}
