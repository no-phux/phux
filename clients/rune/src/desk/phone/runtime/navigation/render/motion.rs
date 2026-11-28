//! Every phone page coming and going by its own control: idle, the commit,
//! 50, 100 and 200 ms in, and settled, on both phone widths, light and
//! dark, and under Reduce Motion. Each frame's `.motion.json` records where
//! the page was drawn and how opaque, so a reviewer reads its direction and
//! settle time off the evidence. The settled frames double as the sheets'
//! keyboard-open and large-text compositions.
use super::*;

/// One way onto or off a page: what is on screen first, the press that
/// changes it, and the page that moves and which way: where it comes from
/// as it enters, or goes as it leaves, a unit step on each axis.
struct Change {
    name: &'static str,
    before: fn(&mut Desk),
    change: fn(&mut Desk),
    page: &'static str,
    way: [f32; 2],
    enters: bool,
}

const DOWN: [f32; 2] = [0.0, 1.0];
const LEFT: [f32; 2] = [-1.0, 0.0];
const RIGHT: [f32; 2] = [1.0, 0.0];

fn nothing(_: &mut Desk) {}

fn appearance(d: &mut Desk) {
    d.toggle_settings();
    d.settings_category(crate::desk::settings::Category::Appearance);
}

fn rename(d: &mut Desk) {
    d.rename_session(
        phux_protocol::ids::SessionKey::new([1; 16]).unwrap(),
        "phux",
    );
}

fn machines(d: &mut Desk) {
    // Never spawn an installed CLI in a unit test.
    d.machines.phux = None;
    d.toggle_machines();
}

const CHANGES: [Change; 17] = [
    Change {
        name: "sessions-open",
        before: nothing,
        change: Desk::open_phone_sessions,
        page: "phone-navigation-layer",
        way: LEFT,
        enters: true,
    },
    Change {
        name: "sessions-close",
        before: Desk::open_phone_sessions,
        change: Desk::close_phone_navigation,
        page: "phone-navigation-layer",
        way: LEFT,
        enters: false,
    },
    Change {
        name: "actions-open",
        before: nothing,
        change: Desk::open_phone_actions,
        page: "phone-actions-layer",
        way: DOWN,
        enters: true,
    },
    Change {
        name: "actions-close",
        before: Desk::open_phone_actions,
        change: Desk::close_phone_navigation,
        page: "phone-actions-layer",
        way: DOWN,
        enters: false,
    },
    Change {
        name: "switcher-open",
        before: nothing,
        change: Desk::open_phone_switcher,
        page: "phone-switcher-layer",
        way: DOWN,
        enters: true,
    },
    Change {
        name: "switcher-to-sessions",
        before: Desk::open_phone_switcher,
        change: |d| {
            d.close_phone_navigation();
            d.open_phone_sessions();
        },
        page: "phone-switcher-layer",
        way: DOWN,
        enters: false,
    },
    Change {
        name: "inbox-open",
        before: nothing,
        change: Desk::toggle_inbox,
        page: "inbox",
        way: DOWN,
        enters: true,
    },
    Change {
        name: "inbox-close",
        before: Desk::toggle_inbox,
        change: Desk::close_inbox,
        page: "inbox",
        way: DOWN,
        enters: false,
    },
    Change {
        name: "palette-open",
        before: nothing,
        change: Desk::toggle_palette,
        page: "palette",
        way: DOWN,
        enters: true,
    },
    Change {
        name: "settings-open",
        before: nothing,
        change: Desk::toggle_settings,
        page: "settings",
        way: RIGHT,
        enters: true,
    },
    Change {
        name: "settings-push",
        before: Desk::toggle_settings,
        change: |d| d.settings_category(crate::desk::settings::Category::Appearance),
        page: "settings-content-appearance",
        way: RIGHT,
        enters: true,
    },
    Change {
        name: "settings-pop",
        before: appearance,
        change: Desk::settings_back,
        page: "settings-content",
        way: LEFT,
        enters: true,
    },
    Change {
        name: "settings-close",
        before: Desk::toggle_settings,
        change: Desk::close_settings,
        page: "settings",
        way: RIGHT,
        enters: false,
    },
    Change {
        name: "rename-open",
        before: nothing,
        change: rename,
        page: "rename",
        way: DOWN,
        enters: true,
    },
    Change {
        name: "find-open",
        before: nothing,
        change: Desk::open_find,
        page: "phone-find",
        way: DOWN,
        enters: true,
    },
    Change {
        name: "machines-open",
        before: nothing,
        change: machines,
        page: "machines",
        way: DOWN,
        enters: true,
    },
    Change {
        name: "news-open",
        before: nothing,
        change: Desk::toggle_news,
        page: "news",
        way: DOWN,
        enters: true,
    },
];

/// Everything a page is drawn as, whichever is on screen.
const PAGES: [&str; 18] = [
    TERMINAL,
    "phone-navigation-layer",
    "phone-actions-layer",
    "phone-switcher-layer",
    "phone-drawer-frost",
    "inbox",
    "inbox-scrim-dim",
    "palette",
    "settings",
    "settings-content",
    "settings-content-appearance",
    "rename",
    "machines",
    "news",
    "find-bar",
    "phone-find",
    "phone-switcher-card",
    "phone-sheet-grabber",
];

/// Frames after the press, at 60 Hz: the commit, 50, 100, 200 ms, settled.
const STAGES: [(&str, u32); 5] = [
    ("commit", 1),
    ("plus-50ms", 3),
    ("plus-100ms", 6),
    ("plus-200ms", 12),
    ("settled", 60),
];

#[test]
fn page_motion_visual_review_frames() {
    let gpu = rune::render::Gpu::headless();
    let light = crate::theme::find("phux-light");
    let cases = [
        ((393, 852), "dark", None, false, false),
        ((320, 568), "light", light, false, false),
        ((393, 852), "dark", None, true, false),
        ((320, 568), "dark", None, true, true),
    ];
    for change in &CHANGES {
        for (size, tag, theme, reduced, large) in cases {
            frames(&gpu, change, size, (tag, theme), reduced, large);
        }
    }
}

fn frames(
    gpu: &rune::render::Gpu,
    change: &Change,
    size: (u32, u32),
    (theme_name, theme): (&str, Option<&'static crate::theme::Theme>),
    reduced: bool,
    large: bool,
) {
    let (client, mut rt) = fixture();
    detailed_inventory(&mut rt);
    rt.set_viewport((size.0 as f32, size.1 as f32), 2.0);
    rt.set_system_reduced_motion(reduced);
    rt.set_srgb_blend(true);
    rt.update(|d, tx| {
        d.settings.theme = theme;
        if large {
            d.set_accessibility(false, "UICTContentSizeCategoryAccessibilityXXXL");
        }
        // The large-text case also has the drawn keyboard up, so a sheet with
        // a field shows where it rests above it.
        d.phone.as_mut().unwrap().keyboard = large;
        tx.instant();
    });
    // The terminal is laid out first, as a page is always opened from it.
    for _ in 0..10 {
        rt.frame(1.0 / 60.0);
    }
    rt.update(|d, tx| {
        (change.before)(d);
        tx.instant();
    });
    for _ in 0..90 {
        rt.frame(1.0 / 60.0);
    }
    let _ = client.take_outbound();
    let mut renderer = app_renderer(gpu);
    let pixels = (size.0 * 2, size.1 * 2);
    let text = if large { "ax5" } else { "standard" };
    let name = format!(
        "motion-{}-{theme_name}-{}x{}-2x-{text}-reduced-{reduced}",
        change.name, size.0, size.1
    );
    let mut capture = |rt: &mut Runtime<Desk>, stage: &str, frame: u32| {
        let image = as_app(gpu, &mut renderer, rt, pixels);
        touch_targets(&rt.accessibility_json(), &name);
        let glass_ok = glass_whole(rt);
        if let Some(directory) = std::env::var_os("RUNE_NAVIGATION_SNAP") {
            let path = std::path::PathBuf::from(directory).join(format!("{name}-{stage}.png"));
            crate::test_support::visual::write(&path, rt, pixels, &image, &name);
            let motion = serde_json::json!({
                "schema": 1, "scenario": name, "stage": stage, "frame": frame,
                "elapsed_ms": f64::from(frame) * 1000.0 / 60.0,
                "runtime_clock": rt.clock,
                "reduced_motion": reduced,
                "change": change.name,
                "drawn": drawn_pages(rt),
                "glass_ok": glass_ok,
            });
            let motion = serde_json::to_vec_pretty(&motion).unwrap();
            std::fs::write(path.with_extension("motion.json"), motion).unwrap();
        }
        // Failed once the frame showing it is written.
        assert!(
            glass_ok,
            "{name} {stage}: glass drawn inside a fading layer"
        );
    };
    capture(&mut rt, "idle", 0);
    let idle = rt.presented(change.page).map(|(rect, _)| rect);
    let idle_terminal = rt.presented(TERMINAL).map(|(rect, _)| rect);
    // The press: a commit an input caused, so its motion plays.
    rt.update(|d, tx| {
        (change.change)(d);
        tx.animated();
    });
    let mut drawn = Vec::new();
    let mut terminal = Vec::new();
    let mut previous = 0;
    for (stage, frame) in STAGES {
        for _ in previous..frame {
            rt.frame(1.0 / 60.0);
        }
        capture(&mut rt, stage, frame);
        drawn.push(rt.presented(change.page));
        terminal.push(rt.presented(TERMINAL));
        previous = frame;
    }
    goes_its_way(change, idle, &drawn, reduced, &name);
    terminal_stands_still(change, idle_terminal, &terminal, &name);
    field_in_view(change, &rt, &name);
    no_terminal_input(&client);
}

/// A bottom sheet that opens typing shows its whole field in its body,
/// clear of the fade over its hidden edge, however little room the
/// keyboard and the text size leave it.
fn field_in_view(change: &Change, rt: &Runtime<Desk>, name: &str) {
    let (body, field) = match change.name {
        "machines-open" => ("machines-body", "machine-field"),
        "rename-open" => ("rename-body", "rename-field"),
        _ => return,
    };
    let body = crate::ui::element(rt, body).rect;
    let field = crate::ui::element(rt, field);
    assert!(field.focused, "{name}: the sheet did not open typing");
    let fade = phux_design::tokens::SPACE.s6;
    let [_, y, _, h] = field.rect;
    assert!(
        y >= body[1] - 0.5 && y + h <= body[1] + body[3] - fade + 0.5,
        "{name}: field {:?} out of view in its body {body:?}",
        field.rect
    );
}

/// The terminal Settings covers and uncovers.
const TERMINAL: &str = "pane-@1";

/// Settings slides over the terminal and off it again; the terminal never
/// moves under it. Opening, it fades where it stood; closing, it is there
/// whole, at its settled place and size, from the frame Settings starts
/// to leave.
fn terminal_stands_still(
    change: &Change,
    idle: Option<[f32; 4]>,
    drawn: &[Option<([f32; 4], f32)>],
    name: &str,
) {
    match change.name {
        "settings-open" => {
            let idle = idle.unwrap_or_else(|| panic!("{name}: no terminal at rest"));
            for (rect, _) in drawn.iter().flatten() {
                assert_eq!(*rect, idle, "{name}: the terminal moved under Settings");
            }
        }
        "settings-close" => {
            let settled = drawn[4].unwrap_or_else(|| panic!("{name}: no terminal settled"));
            let commit = drawn[0].unwrap_or_else(|| panic!("{name}: no terminal at once"));
            assert_eq!(
                commit.0, settled.0,
                "{name}: the terminal grew in under Settings"
            );
            assert!(
                commit.1 > 0.99,
                "{name}: the terminal faded in ({})",
                commit.1
            );
        }
        _ => {}
    }
}

/// Whether every glass panel draws its own blur: none sits in a layer that
/// fades or scales as one, which would flatten its backdrop.
fn glass_whole(rt: &Runtime<Desk>) -> bool {
    let panels =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| crate::ui::glass_panels(rt)));
    panels.is_ok_and(|panels| panels <= 1)
}

/// Where each page on screen was drawn, and how opaque.
fn drawn_pages(rt: &Runtime<Desk>) -> serde_json::Map<String, serde_json::Value> {
    PAGES
        .iter()
        .filter_map(|key| {
            let (rect, opacity) = rt.presented(key)?;
            let drawn = serde_json::json!({ "rect": rect, "opacity": opacity });
            Some(((*key).to_owned(), drawn))
        })
        .collect()
}

/// The page travels its own way: entering, 50 ms in it is still out along
/// its way from where it settles; leaving, it is out along its way from
/// where it rested. Under Reduce Motion an entering page is home in the
/// frame it appears. A sheet comes in whole, never faded.
fn goes_its_way(
    change: &Change,
    idle: Option<[f32; 4]>,
    drawn: &[Option<([f32; 4], f32)>],
    reduced: bool,
    name: &str,
) {
    let along = |from: [f32; 4], to: [f32; 4]| {
        (to[0] - from[0]) * change.way[0] + (to[1] - from[1]) * change.way[1]
    };
    let early = drawn[1].unwrap_or_else(|| panic!("{name}: no {} at 50 ms", change.page));
    if !change.enters {
        let idle = idle.unwrap_or_else(|| panic!("{name}: no {} at rest", change.page));
        if !reduced {
            assert!(along(idle, early.0) > 1.0, "{name}: did not leave its way");
        }
        return;
    }
    let settled = drawn[4].unwrap_or_else(|| panic!("{name}: {} not settled", change.page));
    if reduced {
        let first = drawn[0].unwrap_or_else(|| panic!("{name}: no first frame"));
        assert!(along(settled.0, first.0).abs() < 0.5, "{name}: travelled");
        return;
    }
    assert!(
        along(settled.0, early.0) > 1.0,
        "{name}: did not come its way"
    );
    if change.way == DOWN {
        assert!(early.1 > 0.99, "{name}: a sheet faded in ({})", early.1);
    }
}

/// The switcher's card swiped to the next session as the terminal is
/// paged: at rest, one to one mid-drag, past its point, at the lift, 100 ms
/// on and settled, on both widths, light and dark, with and without Reduce
/// Motion. Sidecars record the card's live shift, where it was drawn, the
/// session previewed and the haptic the frame played.
#[test]
fn switcher_swipe_visual_review_frames() {
    let gpu = rune::render::Gpu::headless();
    let light = crate::theme::find("phux-light");
    for (size, tag, theme) in [((393, 852), "dark", None), ((320, 568), "light", light)] {
        for reduced in [false, true] {
            switcher_frames(&gpu, size, (tag, theme), reduced);
        }
    }
}

fn switcher_frames(
    gpu: &rune::render::Gpu,
    size: (u32, u32),
    (theme_name, theme): (&str, Option<&'static crate::theme::Theme>),
    reduced: bool,
) {
    use rune::TouchPhase::{Ended, Moved, Started};
    let (client, mut rt) = fixture();
    detailed_inventory(&mut rt);
    // The app's poll, where a paged card settles.
    rt.poll = Some(Desk::poll);
    rt.set_viewport((size.0 as f32, size.1 as f32), 2.0);
    rt.set_system_reduced_motion(reduced);
    rt.set_srgb_blend(true);
    rt.update(|d, tx| {
        d.settings.theme = theme;
        d.phone.as_mut().unwrap().keyboard = false;
        d.open_phone_switcher();
        tx.instant();
    });
    for _ in 0..90 {
        rt.frame(1.0 / 60.0);
    }
    let _ = client.take_outbound();
    let mut renderer = app_renderer(gpu);
    let pixels = (size.0 * 2, size.1 * 2);
    let name = format!(
        "switcher-swipe-{theme_name}-{}x{}-2x-reduced-{reduced}",
        size.0, size.1
    );
    let mut capture = |rt: &mut Runtime<Desk>, stage: &str, frame: u32| {
        let feedback = rt.state.phone.as_mut().unwrap().feedback.take();
        let image = as_app(gpu, &mut renderer, rt, pixels);
        touch_targets(&rt.accessibility_json(), &name);
        let Some(directory) = std::env::var_os("RUNE_NAVIGATION_SNAP") else {
            return;
        };
        let path = std::path::PathBuf::from(directory).join(format!("{name}-{stage}.png"));
        crate::test_support::visual::write(&path, rt, pixels, &image, &name);
        let page = rt.state.phone.as_ref().unwrap().switcher.as_ref().unwrap();
        std::fs::write(
            path.with_extension("motion.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema": 1, "scenario": name, "stage": stage, "frame": frame,
                "elapsed_ms": f64::from(frame) * 1000.0 / 60.0,
                "runtime_clock": rt.clock,
                "reduced_motion": reduced,
                "live_shift": page.slide.shift.get(),
                "drawn_rect": rt.presented("phone-switcher-card").map(|(rect, _)| rect),
                "previewed": rt.state.switcher_selection().map(|(_, index, _)| index),
                "feedback": feedback.map(|f| format!("{f:?}")),
            }))
            .unwrap(),
        )
        .unwrap();
    };
    capture(&mut rt, "idle", 0);
    let rest = rt.presented("phone-switcher-card").expect("the card").0;
    let (x, y) = rt.find("phone-switcher-card").unwrap();
    rt.touch(1, Started, x, y, 1.0);
    rt.touch(1, Moved, x - 20.0, y, 1.01);
    rt.frame(1.0 / 60.0);
    rt.touch(1, Moved, x - 80.0, y, 1.02);
    rt.frame(1.0 / 60.0);
    // Drawn one to one with the finger in the frame it moved.
    let drawn = rt.presented("phone-switcher-card").unwrap().0;
    assert!((drawn[0] - rest[0] + 80.0).abs() < 0.5, "{name}: {drawn:?}");
    capture(&mut rt, "mid-drag", 1);
    rt.touch(1, Moved, x - 160.0, y, 1.03);
    rt.frame(1.0 / 60.0);
    capture(&mut rt, "past-commit", 2);
    rt.touch(1, Ended, x - 160.0, y, 1.04);
    rt.frame(1.0 / 60.0);
    capture(&mut rt, "release", 3);
    for _ in 0..6 {
        rt.frame(1.0 / 60.0);
    }
    capture(&mut rt, "release-plus-100ms", 9);
    for _ in 0..54 {
        rt.frame(1.0 / 60.0);
    }
    capture(&mut rt, "settled", 63);
    assert_eq!(
        rt.state.switcher_selection().map(|s| s.1),
        Some(1),
        "{name}"
    );
    let home = rt.presented("phone-switcher-card").unwrap().0;
    assert!(
        (home[0] - rest[0]).abs() < 0.5,
        "{name}: settled at {home:?}"
    );
    no_terminal_input(&client);
}
