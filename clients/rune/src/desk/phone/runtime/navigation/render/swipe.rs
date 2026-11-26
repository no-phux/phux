//! The sideways thumb, painted: a page mid-swipe naming where it goes, the
//! drawer in the finger's hand, and the drawer open over the blur.
use super::*;
use rune::TouchPhase::{Cancelled, Moved, Started};

fn frames(gpu: &rune::render::Gpu, theme: Option<&'static crate::theme::Theme>, tag: &str) {
    let (_, mut rt) = fixture();
    detailed_inventory(&mut rt);
    rt.set_viewport((393.0, 852.0), 2.0);
    rt.update(|d, tx| {
        d.native = Some(crate::mobile::Platform::test());
        d.settings.theme = theme;
        d.phone.as_mut().unwrap().keyboard = false;
        tx.instant();
    });
    for _ in 0..90 {
        rt.frame(1.0 / 60.0);
    }
    let mut renderer = app_renderer(gpu);
    let stage = crate::ui::element(&rt, "phone-spatial-stage").rect;
    let y = stage[1] + stage[3] * 0.5;
    let held = |rt: &mut Runtime<Desk>, from: f32, to: f32, at: f64| {
        rt.touch(9, Started, from, y, at);
        rt.update(|d, tx| {
            d.poll(at, tx);
        });
        rt.frame(1.0 / 60.0);
        rt.touch(9, Moved, to, y, at + 0.016);
        rt.update(|d, tx| {
            d.poll(at + 0.016, tx);
        });
        settle_for_snapshot(rt);
    };
    held(&mut rt, 300.0, 120.0, 1.0);
    paint(
        gpu,
        &mut renderer,
        &mut rt,
        (786, 1704),
        &format!("swipe-{tag}-paging"),
    );
    rt.touch(9, Cancelled, 120.0, y, 2.0);
    for _ in 0..90 {
        rt.frame(1.0 / 60.0);
    }
    held(&mut rt, 4.0, 230.0, 3.0);
    paint(
        gpu,
        &mut renderer,
        &mut rt,
        (786, 1704),
        &format!("swipe-{tag}-pulling"),
    );
    rt.touch(9, Cancelled, 230.0, y, 4.0);
    rt.update(|d, _| d.open_phone_sessions());
    for _ in 0..90 {
        rt.frame(1.0 / 60.0);
    }
    settle_for_snapshot(&mut rt);
    paint(
        gpu,
        &mut renderer,
        &mut rt,
        (786, 1704),
        &format!("swipe-{tag}-drawer"),
    );
}

#[test]
fn swipe_visual_review_frames() {
    let gpu = rune::render::Gpu::headless();
    frames(&gpu, None, "dark");
    frames(&gpu, crate::theme::find("phux-light"), "light");
}
