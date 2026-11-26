//! The organised Sessions page and its long-press menus, painted.
use super::*;
use crate::desk::phone::runtime::navigation::organize::hold;

#[test]
fn phone_session_menu_visual_review_frames() {
    let gpu = rune::render::Gpu::headless();
    let light = crate::theme::all().find(|theme| !theme.dark).unwrap();
    for size in [(393, 852), (320, 568)] {
        for (appearance, theme) in [("dark", None), ("light", Some(light))] {
            let (_, mut rt) = fixture();
            detailed_inventory(&mut rt);
            rt.set_viewport((size.0 as f32, size.1 as f32), 1.0);
            rt.set_system_reduced_motion(true);
            rt.update(|d, tx| {
                d.settings.theme = theme;
                d.open_phone_sessions();
                tx.instant();
            });
            settle_for_snapshot(&mut rt);
            let mut renderer = app_renderer(&gpu);
            hold(&mut rt, "phone-session-pane-@3", 10.0);
            settle_for_snapshot(&mut rt);
            let menu = crate::ui::element(&rt, "phone-menu").rect;
            assert!(
                menu[1] >= 0.0 && menu[1] + menu[3] <= size.1 as f32,
                "the menu stays on the page: {menu:?}"
            );
            paint(
                &gpu,
                &mut renderer,
                &mut rt,
                size,
                &format!("menu-pane-{appearance}"),
            );
            rt.update(|d, _| d.close_phone_menu());
            rt.update(|_, tx| tx.scroll_to_instant("phone-session-list", 0.0, 0.0));
            settle_for_snapshot(&mut rt);
            hold(&mut rt, "phone-session-1", 20.0);
            settle_for_snapshot(&mut rt);
            paint(
                &gpu,
                &mut renderer,
                &mut rt,
                size,
                &format!("menu-session-{appearance}"),
            );
            rt.update(|d, _| {
                d.close_phone_menu();
                d.toggle_phone_fold(2);
                d.phone.as_mut().unwrap().quiet_shown = true;
            });
            rt.update(|_, tx| tx.scroll_to_instant("phone-session-list", 0.0, 100_000.0));
            settle_for_snapshot(&mut rt);
            paint(
                &gpu,
                &mut renderer,
                &mut rt,
                size,
                &format!("folded-{appearance}"),
            );
        }
    }
}
