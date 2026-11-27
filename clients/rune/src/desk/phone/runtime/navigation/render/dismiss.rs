//! Every page swiped away the way it came, painted through the host's touch
//! path: at rest, in the finger's hand, at the lift and once settled, and a
//! let-go short of leaving springing home. Each frame's `.motion.json`
//! binds the scenario, frame, elapsed time and the live values the page was
//! drawn with, so a reviewer reads one-to-one following and settle timing
//! off the evidence rather than trusting it.
use super::*;
use crate::desk::phone::dismiss::Away;
use rune::TouchPhase::{Ended, Moved, Started};

/// One gesture: the page it starts on and how a thumb takes it away.
struct Gesture {
    name: &'static str,
    open: fn(&mut Desk),
    away: Away,
    /// The element that moves with the finger.
    layer: &'static str,
    /// Where the finger lands, as a share of the phone's width and height.
    from: [f32; 2],
    /// Its travel at the mid-drag frame and at the lift, in points.
    mid: [f32; 2],
    lift: [f32; 2],
}

const GESTURES: [Gesture; 6] = [
    Gesture {
        name: "sessions-left",
        open: Desk::open_phone_sessions,
        away: Away::Sessions,
        layer: "phone-navigation-layer",
        from: [0.75, 0.55],
        mid: [-80.0, 4.0],
        lift: [-190.0, 6.0],
    },
    Gesture {
        name: "actions-down",
        open: Desk::open_phone_actions,
        away: Away::Sheet(Page::Actions),
        layer: "phone-navigation-layer",
        from: [0.5, 0.3],
        mid: [4.0, 140.0],
        lift: [6.0, 300.0],
    },
    Gesture {
        name: "switcher-down",
        open: Desk::open_phone_switcher,
        away: Away::Sheet(Page::Switcher),
        layer: "phone-navigation-layer",
        from: [0.5, 0.4],
        mid: [4.0, 140.0],
        lift: [6.0, 300.0],
    },
    Gesture {
        name: "inbox-down",
        open: Desk::toggle_inbox,
        away: Away::Inbox,
        layer: "inbox",
        from: [0.5, 0.12],
        mid: [4.0, 140.0],
        lift: [6.0, 300.0],
    },
    Gesture {
        name: "palette-down",
        open: Desk::toggle_palette,
        away: Away::Palette,
        layer: "palette",
        from: [0.5, 0.45],
        mid: [4.0, 140.0],
        lift: [6.0, 300.0],
    },
    Gesture {
        name: "settings-back",
        open: |d| {
            d.toggle_settings();
            d.settings_category(crate::desk::settings::Category::Appearance);
        },
        away: Away::Settings,
        layer: "settings",
        from: [0.02, 0.5],
        mid: [80.0, 4.0],
        lift: [200.0, 6.0],
    },
];

#[test]
fn dismiss_motion_visual_review_frames() {
    let gpu = rune::render::Gpu::headless();
    for gesture in &GESTURES {
        for size in [(393, 852), (320, 568)] {
            for (tag, theme) in [("dark", None), ("light", crate::theme::find("phux-light"))] {
                for reduced in [false, true] {
                    frames(&gpu, gesture, size, tag, theme, reduced);
                }
            }
        }
    }
}

fn frames(
    gpu: &rune::render::Gpu,
    gesture: &Gesture,
    size: (u32, u32),
    theme_name: &str,
    theme: Option<&'static crate::theme::Theme>,
    reduced: bool,
) {
    let (client, mut rt) = fixture();
    detailed_inventory(&mut rt);
    rt.set_viewport((size.0 as f32, size.1 as f32), 2.0);
    rt.set_system_reduced_motion(reduced);
    rt.set_srgb_blend(true);
    rt.update(|d, tx| {
        d.settings.theme = theme;
        d.phone.as_mut().unwrap().keyboard = false;
        (gesture.open)(d);
        // The keyboard a field raises would hide what the swipe moves.
        d.palette_query.focused = false;
        d.inbox.reply.focused = false;
        tx.blur();
        tx.instant();
    });
    for _ in 0..90 {
        rt.frame(1.0 / 60.0);
    }
    assert_eq!(
        rt.state.swipe_away(),
        Some(gesture.away),
        "{}",
        gesture.name
    );
    let _ = client.take_outbound();
    let mut renderer = app_renderer(gpu);
    let pixels = (size.0 * 2, size.1 * 2);
    let name = format!(
        "dismiss-{}-{theme_name}-{}x{}-2x-reduced-{reduced}",
        gesture.name, size.0, size.1
    );
    let from = [
        size.0 as f32 * gesture.from[0],
        size.1 as f32 * gesture.from[1],
    ];
    let at = |travel: [f32; 2]| [from[0] + travel[0], from[1] + travel[1]];
    let mut capture = |rt: &mut Runtime<Desk>, stage: &str, frame: u32| {
        let image = as_app(gpu, &mut renderer, rt, pixels);
        touch_targets(&rt.accessibility_json(), &name);
        assert!(crate::ui::glass_panels(rt) <= 1, "{name}: one blur");
        let Some(directory) = std::env::var_os("RUNE_NAVIGATION_SNAP") else {
            return;
        };
        let path = std::path::PathBuf::from(directory).join(format!("{name}-{stage}.png"));
        crate::test_support::visual::write(&path, rt, pixels, &image, &name);
        let dismiss = &rt.state.phone.as_ref().unwrap().dismiss;
        std::fs::write(
            path.with_extension("motion.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema": 1, "scenario": name, "stage": stage, "frame": frame,
                "elapsed_ms": f64::from(frame) * 1000.0 / 60.0,
                "runtime_clock": rt.clock,
                "reduced_motion": reduced,
                "gesture": gesture.name,
                "travel_mid_points": gesture.mid, "travel_lift_points": gesture.lift,
                "live_shift": dismiss.shift.get(), "live_shown": dismiss.shown.get(),
                "swiping": dismiss.swiping(),
                // Where the page was drawn: one to one with the finger
                // mid-drag, home again once settled.
                "drawn_rect": rt.presented(gesture.layer).map(|(rect, _)| rect),
                "drawn_opacity": rt.presented(gesture.layer).map(|(_, opacity)| opacity),
                // Sessions' frost under it: never clearer than the list.
                "frost_opacity": rt.presented("phone-drawer-frost").map(|(_, opacity)| opacity),
                "page_on_top": format!("{:?}", rt.state.swipe_away()),
            }))
            .unwrap(),
        )
        .unwrap();
    };
    capture(&mut rt, "idle", 0);
    let rest = rt.presented(gesture.layer).expect("the page").0;
    // In the finger's hand: it follows one to one, mid-drag.
    crate::mobile::touch(&mut rt, 7, Started, from[0], from[1], 1.0);
    let half = [gesture.mid[0] * 0.5, gesture.mid[1] * 0.5];
    let [x, y] = at(half);
    crate::mobile::touch(&mut rt, 7, Moved, x, y, 1.05);
    let [x, y] = at(gesture.mid);
    crate::mobile::touch(&mut rt, 7, Moved, x, y, 1.1);
    rt.frame(1.0 / 60.0);
    assert!(rt.state.phone.as_ref().unwrap().dismiss.swiping(), "{name}");
    // Drawn one to one with the finger along the page's way out, in the
    // very frame it moved: no spring between them.
    let drawn = rt.presented(gesture.layer).expect("the page").0;
    let moved = [drawn[0] - rest[0], drawn[1] - rest[1]];
    let along = match gesture.away.way() {
        crate::desk::phone::dismiss::Way::Down => [0.0, gesture.mid[1]],
        _ => [gesture.mid[0], 0.0],
    };
    assert!(
        (moved[0] - along[0]).abs() < 0.5 && (moved[1] - along[1]).abs() < 0.5,
        "{name}: drawn {moved:?} for a finger's {along:?}"
    );
    capture(&mut rt, "mid-drag", 1);
    // Further moves paint without a commit: no view runs, nothing lays out.
    let commits = rt.stats.commits;
    for step in 1..=4u8 {
        let k = 1.0 + f32::from(step) * 0.05;
        crate::mobile::touch(
            &mut rt,
            7,
            Moved,
            from[0] + gesture.mid[0] * k,
            from[1] + gesture.mid[1] * k,
            1.1 + f64::from(step) * 0.016,
        );
        rt.frame(1.0 / 60.0);
    }
    assert_eq!(rt.stats.commits, commits, "{name}: a move committed");
    let [x, y] = at(gesture.mid);
    crate::mobile::touch(&mut rt, 7, Moved, x, y, 1.2);
    // Let go short: home on the drawer spring, or at once.
    crate::mobile::touch(&mut rt, 7, Ended, x, y, 1.6);
    rt.frame(1.0 / 60.0);
    capture(&mut rt, "short-release", 2);
    for _ in 0..6 {
        rt.frame(1.0 / 60.0);
    }
    capture(&mut rt, "short-release-plus-100ms", 8);
    for _ in 0..60 {
        rt.frame(1.0 / 60.0);
    }
    assert_eq!(rt.state.swipe_away(), Some(gesture.away), "{name} stayed");
    let home = rt.presented(gesture.layer).expect("the page").0;
    assert!(
        (home[0] - rest[0]).abs() < 0.5 && (home[1] - rest[1]).abs() < 0.5,
        "{name} home"
    );
    capture(&mut rt, "short-settled", 68);
    // Past its point: it leaves the way it was going.
    crate::mobile::touch(&mut rt, 7, Started, from[0], from[1], 3.0);
    let [x, y] = at(half);
    crate::mobile::touch(&mut rt, 7, Moved, x, y, 3.05);
    let [x, y] = at(gesture.lift);
    crate::mobile::touch(&mut rt, 7, Moved, x, y, 3.1);
    rt.frame(1.0 / 60.0);
    capture(&mut rt, "past-commit", 69);
    crate::mobile::touch(&mut rt, 7, Ended, x, y, 3.5);
    rt.frame(1.0 / 60.0);
    capture(&mut rt, "release", 70);
    for _ in 0..6 {
        rt.frame(1.0 / 60.0);
    }
    capture(&mut rt, "release-plus-100ms", 76);
    frost_outlasts_the_list(&rt, gesture, &name);
    for _ in 0..6 {
        rt.frame(1.0 / 60.0);
    }
    capture(&mut rt, "release-plus-200ms", 82);
    frost_outlasts_the_list(&rt, gesture, &name);
    for _ in 0..54 {
        rt.frame(1.0 / 60.0);
    }
    capture(&mut rt, "settled", 136);
    if gesture.away == Away::Settings {
        assert_eq!(rt.state.settings_ui.category, None, "{name} went back");
    } else {
        assert_ne!(rt.state.swipe_away(), Some(gesture.away), "{name} left");
    }
    no_terminal_input(&client);
}

/// Sessions leaving: its frost stays thicker than the list on it, so the
/// list never shows over a terminal the frost has already cleared from.
fn frost_outlasts_the_list(rt: &Runtime<Desk>, gesture: &Gesture, name: &str) {
    if gesture.away != Away::Sessions {
        return;
    }
    let list = rt.presented(gesture.layer).map_or(0.0, |(_, o)| o);
    let frost = rt.presented("phone-drawer-frost").map_or(0.0, |(_, o)| o);
    assert!(
        list <= 0.02 || list <= frost * frost,
        "{name}: list {list} over frost {frost}"
    );
}
