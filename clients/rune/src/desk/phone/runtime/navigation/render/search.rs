//! Matched search recovery evidence; application controls are not mocked.
use super::*;

#[test]
fn phone_session_search_visual_review_frames() {
    let gpu = rune::render::Gpu::headless();
    for (size, native) in [
        ((320, 568), false),
        ((393, 852), false),
        ((320, 478), true),
        ((393, 759), true),
        ((320, 340), true),
        ((393, 480), true),
    ] {
        for (text, category) in [
            ("standard", "UICTContentSizeCategoryL"),
            ("ax5", "UICTContentSizeCategoryAccessibilityXXXL"),
        ] {
            for (appearance, theme) in [("dark", None), ("light", crate::theme::find("phux-light"))]
            {
                search_frames(&gpu, size, native, text, category, appearance, theme);
            }
        }
    }
}

fn search_frames(
    gpu: &rune::render::Gpu,
    size: (u32, u32),
    native: bool,
    text: &str,
    category: &str,
    appearance: &str,
    theme: Option<&'static crate::theme::Theme>,
) {
    let (client, mut rt) = fixture();
    detailed_inventory(&mut rt);
    rt.set_viewport((size.0 as f32, size.1 as f32), 2.0);
    rt.set_system_reduced_motion(true);
    rt.update(|d, _| {
        if native {
            d.native = Some(crate::mobile::Platform::test());
        }
        d.settings.theme = theme;
        d.set_accessibility(false, category);
        d.open_phone_sessions();
    });
    settle_for_snapshot(&mut rt);
    tap(&mut rt, "phone-session-query", 0.1);
    let mut renderer = app_renderer(gpu);
    for (state, query) in [
        ("matching", "review"),
        ("no-match", "nothing matches"),
        (
            "long",
            "workspace_build_integration_review_authentication_long_query",
        ),
    ] {
        rt.key(rune::KeyEvent {
            key: Key::Command('a'),
            mods: rune::Mods {
                cmd: true,
                ..rune::Mods::default()
            },
        });
        rt.key(crate::keys::plain(Key::Text(query.into())));
        settle_for_snapshot(&mut rt);
        assert_eq!(rt.state.phone.as_ref().unwrap().query.text, query);
        paint(
            gpu,
            &mut renderer,
            &mut rt,
            (size.0 * 2, size.1 * 2),
            &format!(
                "session-search-{state}-{appearance}-{text}-native-{native}-{}x{}",
                size.0, size.1
            ),
        );
    }
    rt.key(crate::keys::plain(Key::Enter));
    settle_for_snapshot(&mut rt);
    assert!(!rt.state.phone.as_ref().unwrap().query.focused);
    paint(
        gpu,
        &mut renderer,
        &mut rt,
        (size.0 * 2, size.1 * 2),
        &format!(
            "session-search-blurred-{appearance}-{text}-native-{native}-{}x{}",
            size.0, size.1
        ),
    );
    rt.update(|_, tx| tx.scroll_to_instant("phone-session-list", 0.0, 100_000.0));
    settle_for_snapshot(&mut rt);
    paint(
        gpu,
        &mut renderer,
        &mut rt,
        (size.0 * 2, size.1 * 2),
        &format!(
            "session-search-bottom-{appearance}-{text}-native-{native}-{}x{}",
            size.0, size.1
        ),
    );
    no_terminal_input(&client);
}
