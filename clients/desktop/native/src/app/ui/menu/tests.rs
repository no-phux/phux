use super::*;
use crate::settings::skin::GLASS;
use std::cell::Cell;

#[test]
fn interrupted_surface_motion_settles_and_stops() {
    for fps in [30, 60, 120] {
        let mut motion = SurfaceMotion::new(400.);
        let start = motion.last;
        for frame in 1..=fps {
            if frame == fps / 6 {
                motion.target = HEADER + ROW * 3. + INSET;
            }
            motion.advance(
                start + Duration::from_secs_f32(frame as f32 / fps as f32),
                false,
                spring(&GLASS),
            );
        }
        assert_eq!(motion.height.position, motion.target);
        assert_eq!(motion.height.velocity, 0.);
        assert!(
            !motion.advance(start + Duration::from_secs(2), false, spring(&GLASS)),
            "settled surfaces request no frames"
        );
    }
}

#[test]
fn glass_washes_keep_the_shipped_menu_ink() {
    let ink = gpui::white();
    let washes = Washes::of(ink, &GLASS);
    for (wash, alpha) in [
        (washes.quiet, 0.035),
        (washes.disc, 0.07),
        (washes.selection, 0.085),
        (washes.hairline, 0.035),
        (washes.press, 0.13),
    ] {
        assert!((wash.a - alpha).abs() < 1e-6, "{} != {alpha}", wash.a);
    }
    assert_eq!(GLASS.shape.popover_radius, 26.);
    assert_eq!(GLASS.motion.menu_enter(), Duration::from_millis(150));
    assert_eq!(GLASS.motion.menu_exit(), Duration::from_millis(100));
    assert_eq!(spring(&GLASS), SpringConfig::new(950., 48., 1.));
    let terminal = crate::settings::skin_by_id("terminal");
    assert!(wants_glass(&GLASS) && !wants_glass(terminal));
    assert_eq!(span_progress(Duration::from_millis(5), Duration::ZERO), 1.);
}

#[test]
fn reduced_motion_resolves_surface_immediately() {
    let mut motion = SurfaceMotion::new(400.);
    motion.target = 228.;
    assert!(!motion.advance(Instant::now(), true, spring(&GLASS)));
    assert_eq!(motion.height.position, 228.);
    assert_eq!(motion.height.velocity, 0.);
}

#[test]
fn a_headless_first_page_reserves_no_header() {
    let page = MenuPage::new(vec![MenuItem::new("One", Choice::Run(Rc::new(|_, _| {})))]);
    assert_eq!(page_height(&page, true), INSET * 2. + ROW);
    assert_eq!(page_height(&page, false), HEADER + INSET * 2. + ROW);
    let trigger = Bounds::new(point(px(80.), px(600.)), size(px(120.), px(30.)));
    assert_eq!(
        Anchor::Below(trigger).corner(100.),
        point(px(80.), px(630. + DROP))
    );
    assert_eq!(
        Anchor::Above(trigger).corner(100.),
        point(px(80.), px(500. - DROP))
    );
    assert_eq!(
        Anchor::At(point(px(5.), px(9.))).corner(100.),
        point(px(5.), px(9.))
    );
    let screen = Bounds::new(point(px(0.), px(0.)), size(px(1000.), px(800.)));
    let parent = Bounds::new(point(px(100.), px(50.)), size(px(800.), px(700.)));
    let (origin, room) = place(Anchor::Above(trigger), parent, screen, 260., 900.);
    assert_eq!(origin.y, px(MARGIN), "a tall menu stops at the screen top");
    assert_eq!(room, 650. - DROP - MARGIN);
    let (origin, _) = place(Anchor::Below(trigger), parent, screen, 260., 100.);
    assert_eq!(origin.x, px(180.));
}

#[gpui::test]
fn dismissal_finishes_without_display_frames(cx: &mut gpui::TestAppContext) {
    let closed = Rc::new(Cell::new(0));
    let handle = cx.update(|cx| {
        gpui_kit::init(cx);
        let counted = closed.clone();
        cx.open_window(WindowOptions::default(), |window, cx| {
            let mut spec = sample(GLASS, 0);
            spec.on_close = Some(Box::new(move |closed, _| {
                assert_eq!(closed, Closed::Away);
                counted.set(counted.get() + 1)
            }));
            fixture(spec, 700., window, cx)
        })
        .unwrap()
    });
    handle
        .update(cx, |menu, window, cx| menu.dismiss(false, window, cx))
        .unwrap();
    cx.run_until_parked();
    // Simulate a covered window: deliver no further render/display frames.
    cx.executor().advance_clock(GLASS.motion.menu_exit());
    cx.run_until_parked();
    assert!(handle.read_with(cx, |_, _| ()).is_err());
    assert_eq!(closed.get(), 1, "the close callback runs exactly once");
}

#[gpui::test]
fn keyboard_reaches_header_and_scrolls_long_pages(cx: &mut gpui::TestAppContext) {
    let handle = cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(10.), px(10.)),
                    size(px(280.), px(220.)),
                ))),
                ..Default::default()
            },
            |window, cx| fixture(sample(GLASS, 70), 220., window, cx),
        )
        .unwrap()
    });
    let cx = &mut gpui::VisualTestContext::from_window(handle.into(), cx);
    cx.simulate_keystrokes("down");
    handle
        .update(cx, |menu, _, cx| {
            menu.hover_select(None, menu.pointer, cx);
            assert_eq!(
                menu.selected,
                Some(0),
                "stationary pointer preserves keyboard selection"
            );
            menu.hover_select(None, menu.pointer + point(px(1.), px(0.)), cx);
            assert_eq!(
                menu.selected, None,
                "pointer movement resumes hover selection"
            );
        })
        .unwrap();
    // Back to the header, then into its page.
    cx.simulate_keystrokes("down up enter");
    assert_eq!(handle.read_with(cx, |menu, _| menu.stack.len()).unwrap(), 2);
    cx.simulate_keystrokes("up");
    let bounds = cx
        .debug_bounds("glass-menu-row-69")
        .expect("last host-page row");
    assert!(bounds.origin.y >= px(HEADER));
    assert!(
        bounds.bottom() <= px(220. - INSET),
        "bounds={bounds:?}, scroll={:?}",
        handle
            .read_with(cx, |menu, _| menu.scroll.offset())
            .unwrap()
    );
    cx.simulate_keystrokes("left");
    assert_eq!(handle.read_with(cx, |menu, _| menu.stack.len()).unwrap(), 1);
    cx.update(|window, cx| {
        assert_eq!(
            window.simulate_next_frame(cx),
            0,
            "reduced motion has no queued animation frames"
        )
    });
    // An item's chord chooses it from any page.
    cx.simulate_keystrokes("cmd-,");
    assert!(handle.read_with(cx, |_, _| ()).is_err());
}
