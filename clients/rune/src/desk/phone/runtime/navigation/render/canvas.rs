//! Selected-flow gaps: full large Connections, long filters, and an empty server.
use super::*;

#[test]
fn phone_session_canvas_visual_review_frames() {
    let gpu = rune::render::Gpu::headless();
    for size in [(320, 478), (393, 759)] {
        for (appearance, theme) in [("dark", None), ("light", crate::theme::find("phux-light"))] {
            let (client, mut rt) = fixture();
            detailed_inventory(&mut rt);
            rt.set_viewport((size.0 as f32, size.1 as f32), 2.0);
            rt.set_system_reduced_motion(true);
            rt.update(|d, _| {
                d.native = Some(crate::mobile::Platform::test());
                d.settings.theme = theme;
                d.machines.current = Some("Build studio".into());
                d.set_accessibility(false, "UICTContentSizeCategoryAccessibilityXXXL");
                d.open_phone_sessions();
            });
            rt.update(|_, tx| {
                tx.focus("phone-session-connections", true);
                tx.scroll_into_view("phone-session-connections");
                tx.instant();
            });
            settle_for_snapshot(&mut rt);
            let mut renderer = app_renderer(&gpu);
            capture(
                &gpu,
                &mut renderer,
                &mut rt,
                size,
                appearance,
                "connections",
            );
            let destination = crate::ui::element(&rt, "phone-session-connections");
            assert!(
                destination.visible[3] >= destination.rect[3] - 0.5,
                "whole large destination: {destination:?}"
            );
            rt.update(|_, tx| tx.scroll_to_instant("phone-session-list", 0.0, 0.0));
            settle_for_snapshot(&mut rt);
            tap(&mut rt, "phone-session-query", 0.1);
            let keyboard_size = (size.0, if size.0 == 320 { 340 } else { 480 });
            rt.set_viewport((keyboard_size.0 as f32, keyboard_size.1 as f32), 2.0);
            let query = "workspace_build_integration_review_authentication_long_query";
            rt.key(crate::keys::plain(Key::Text(query.into())));
            settle_for_snapshot(&mut rt);
            assert_eq!(rt.state.phone.as_ref().unwrap().query.text, query);
            capture(
                &gpu,
                &mut renderer,
                &mut rt,
                keyboard_size,
                appearance,
                "query-top",
            );
            rt.update(|_, tx| tx.scroll_to_instant("phone-session-list", 0.0, 100_000.0));
            settle_for_snapshot(&mut rt);
            capture(
                &gpu,
                &mut renderer,
                &mut rt,
                keyboard_size,
                appearance,
                "query-bottom",
            );
            let action = crate::ui::element(&rt, "phone-session-new");
            assert!(
                action.visible[3] >= action.rect[3] - 0.5,
                "complete long-filter action: {action:?}"
            );
            no_terminal_input(&client);
            rt.set_viewport((size.0 as f32, size.1 as f32), 2.0);
            rt.update(|d, _| {
                d.close_phone_navigation();
                d.set_accessibility(false, "UICTContentSizeCategoryL");
            });
            set_inventory(
                &mut rt,
                SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
                    .with_sessions(vec![])
                    .with_resources(vec![]),
            );
            rt.update(|d, _| {
                d.phone.as_mut().unwrap().query.replace_text("");
                d.open_phone_sessions();
            });
            settle_for_snapshot(&mut rt);
            assert!(rt.find("phone-session-empty").is_some());
            capture(&gpu, &mut renderer, &mut rt, size, appearance, "empty");
        }
    }
}

fn capture(
    gpu: &rune::render::Gpu,
    renderer: &mut rune::render::Renderer,
    rt: &mut Runtime<Desk>,
    size: (u32, u32),
    appearance: &str,
    state: &str,
) {
    paint(
        gpu,
        renderer,
        rt,
        (size.0 * 2, size.1 * 2),
        &format!("canvas-{state}-{appearance}"),
    );
}
