//! The command palette through the real renderer, over the gallery's
//! populated server (agents working, blocked and done): its empty list, a
//! typed query, no match, and the Themes and Sessions modes, on two
//! desktop windows and both phone widths, dark and light, and on the phone
//! with its keyboard up and at the largest text. `RUNE_PALETTE_SNAP=DIR`
//! writes each frame as `DIR/NAME.png` with its geometry and accessibility
//! tree beside it as `DIR/NAME.json`.

use rune::Runtime;

use super::{Locale, settle, window_on};
use crate::desk::Desk;
use crate::keys::Shortcut;

const SCALE: f32 = 2.0;

/// Each window: its size and whether it is the phone form.
const WINDOWS: [((f32, f32), bool); 4] = [
    ((1100.0, 720.0), false),
    ((640.0, 360.0), false),
    ((393.0, 852.0), true),
    ((320.0, 568.0), true),
];

/// Each state the palette is painted in.
const STATES: [&str; 5] = ["empty", "typed", "no-match", "themes", "sessions"];

fn arrange(d: &mut Desk, state: &str, tx: &mut rune::Tx) {
    match state {
        "themes" => d.shortcut(Shortcut::ChangeTheme, tx),
        "sessions" => d.shortcut(Shortcut::SwitchSession, tx),
        _ => d.toggle_palette(),
    }
    let query = match state {
        "typed" => "re",
        "no-match" => "deploy staging",
        _ => return,
    };
    d.palette_query.replace_text(query);
    d.palette_edited();
}

/// The palette in `state`, open and settled, the phone's keyboard down
/// unless `keyboard`.
pub(super) fn palette_window(
    size: (f32, f32),
    phone: bool,
    dark: bool,
    state: &str,
    keyboard: bool,
) -> Runtime<Desk> {
    let mut rt = window_on(size, dark, None, Locale::En, phone);
    rt.set_system_reduced_motion(true);
    settle(&mut rt);
    let state = state.to_owned();
    rt.update(move |d, tx| {
        if let Some(phone) = d.phone.as_mut() {
            phone.keyboard = keyboard;
        }
        arrange(d, &state, tx);
    });
    settle(&mut rt);
    rt
}

fn snap(gpu: &rune::render::Gpu, rt: &mut Runtime<Desk>, size: (f32, f32), name: &str) {
    rt.set_srgb_blend(true);
    settle(rt);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a fixed size"
    )]
    let px = ((size.0 * SCALE) as u32, (size.1 * SCALE) as u32);
    let pixels = crate::ui::offscreen_as_app(gpu, rt, px);
    assert!(
        pixels.as_chunks::<4>().0.iter().any(|p| p[0] != p[1]),
        "{name} painted nothing but grey"
    );
    if let Some(dir) = std::env::var_os("RUNE_PALETTE_SNAP") {
        let dir = std::path::Path::new(&dir);
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(format!("{name}.png"));
        crate::test_support::visual::write(&path, rt, px, &pixels, name);
    }
}

#[test]
fn palette_visual_review_frames() {
    let gpu = rune::render::Gpu::headless();
    for (size, phone) in WINDOWS {
        let form = if phone { "phone" } else { "desktop" };
        let tag = format!("{form}-{}x{}", size.0, size.1);
        for dark in [true, false] {
            let tone = if dark { "dark" } else { "light" };
            for state in STATES {
                let mut rt = palette_window(size, phone, dark, state, false);
                snap(&gpu, &mut rt, size, &format!("{tag}-{state}-{tone}"));
            }
            if !phone {
                continue;
            }
            for state in ["empty", "typed"] {
                let mut rt = palette_window(size, phone, dark, state, true);
                snap(
                    &gpu,
                    &mut rt,
                    size,
                    &format!("{tag}-{state}-keyboard-{tone}"),
                );
                let mut rt = palette_window(size, phone, dark, state, false);
                // The phone takes the system's largest Dynamic Type, as the
                // native host reports it.
                rt.update(|d, _| {
                    d.set_accessibility(false, "UICTContentSizeCategoryAccessibilityXXXL");
                });
                snap(
                    &gpu,
                    &mut rt,
                    size,
                    &format!("{tag}-{state}-largest-{tone}"),
                );
            }
        }
    }
}

/// Within half a point.
fn near(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.5
}

/// One column grid, measured in the real tree: the query starts where the
/// row titles do, the field's glyph centres on the rows' marks, every row
/// has a mark and is a full row (a touch target on the phone).
#[test]
fn the_palette_keeps_one_column_grid() {
    for (size, phone) in WINDOWS {
        for state in ["empty", "typed", "themes"] {
            let rt = palette_window(size, phone, true, state, false);
            let rect = |key: &str| rt.presented(key).unwrap_or_else(|| panic!("{key}")).0;
            let tag = format!("{size:?} {state}");
            let query = rect("palette-query");
            let glyph = rect("palette-glyph");
            let rows: Vec<usize> = (0..crate::palette::ROWS)
                .take_while(|i| rt.presented(&format!("palette-row-{i}")).is_some())
                .collect();
            assert!(rows.len() > 2, "{tag}: {} rows", rows.len());
            let tall = if phone { 44.0 } else { 32.0 };
            for &i in &rows {
                let row = rect(&format!("palette-row-{i}"));
                let mark = rect(&format!("palette-mark-{i}"));
                let title = rect(&format!("palette-title-{i}"));
                assert!(row[3] >= tall - 0.01, "{tag}: row {i} is {row:?}");
                assert!(mark[2] > 0.0 && title[0] > mark[0], "{tag}: row {i}");
                // A pane under its tab sits a column in; the rest share one.
                if rt.state.palette_hits()[i].item.kind == "pane" {
                    continue;
                }
                assert!(
                    near(title[0], query[0]) || state == "themes",
                    "{tag}: title {i} at {} under the query at {}",
                    title[0],
                    query[0]
                );
                let centre = |r: [f32; 4]| r[0] + r[2] / 2.0;
                assert!(
                    near(centre(mark), centre(glyph)),
                    "{tag}: mark {i} {mark:?} under the glyph {glyph:?}"
                );
            }
            if !phone {
                // A short window's panel drops a share of its height, a
                // tall one its usual drop.
                let extents = crate::ui::tokens::layout::OVERLAY;
                let top = (extents.palette_top)
                    .min(size.1 * extents.palette_top_share)
                    .max(extents.palette_top_min);
                let panel = rect("palette");
                assert!(near(panel[1], top), "{tag}: {panel:?} not at {top}");
            }
        }
    }
}

fn press(rt: &mut Runtime<Desk>, key: rune::Key) {
    rt.update(move |d, tx| {
        let event = rune::KeyEvent {
            key,
            mods: rune::Mods::default(),
        };
        d.palette_key(&event, tx);
    });
    settle(rt);
}

fn washed(rt: &Runtime<Desk>, index: usize) -> bool {
    crate::ui::element(rt, &format!("palette-row-{index}"))
        .fill
        .a
        > 0.001
}

/// A phone's untouched list shows no picked row, so the keyboard never
/// moves or runs one nobody can see: the first arrow shows the first row,
/// the next moves on, and Return on an untouched list only shows it.
#[test]
fn a_phone_shows_the_row_the_keyboard_picks() {
    for size in [(393.0, 852.0), (320.0, 568.0)] {
        let mut rt = palette_window(size, true, true, "empty", false);
        assert!(!washed(&rt, 0), "{size:?}: untouched");
        press(&mut rt, rune::Key::Down);
        assert_eq!(rt.state.palette_picked, 0, "{size:?}");
        assert!(washed(&rt, 0), "{size:?}: the first arrow shows row 0");
        press(&mut rt, rune::Key::Down);
        assert!(washed(&rt, 1) && !washed(&rt, 0), "{size:?}: then it moves");
        assert!(rt.state.palette_open);

        let mut rt = palette_window(size, true, true, "empty", false);
        rt.update(|d, tx| d.palette_run(None, tx));
        settle(&mut rt);
        assert!(rt.state.palette_open, "{size:?}: Return ran an unseen row");
        assert!(
            washed(&rt, 0),
            "{size:?}: Return shows the row it would run"
        );
    }
}

/// Every row of a list that can hold a check keeps its slot, so the
/// trailing column ends at one x whether a row is current or not.
#[test]
fn the_trailing_column_ends_at_one_x() {
    for (size, phone) in WINDOWS {
        for state in ["empty", "themes"] {
            let rt = palette_window(size, phone, true, state, false);
            let slots: Vec<f32> = (0..crate::palette::ROWS)
                .filter_map(|i| rt.presented(&format!("palette-check-{i}")))
                .map(|(r, ..)| r[0])
                .collect();
            assert!(slots.len() > 2, "{size:?} {state}: {slots:?}");
            assert!(
                slots.iter().all(|x| near(*x, slots[0])),
                "{size:?} {state}: {slots:?}"
            );
        }
    }
}

/// The phone's search line sits on the bar's centre line, level with the
/// magnifier, at the default text size and at the largest.
#[test]
fn the_phone_search_line_centres_on_its_magnifier() {
    for size in [(393.0, 852.0), (320.0, 568.0)] {
        for largest in [false, true] {
            for state in ["empty", "typed", "themes"] {
                let mut rt = palette_window(size, true, true, state, false);
                if largest {
                    rt.update(|d, _| {
                        d.set_accessibility(false, "UICTContentSizeCategoryAccessibilityXXXL");
                    });
                    settle(&mut rt);
                }
                let tag = format!("{size:?} {state} largest={largest}");
                let field = crate::ui::element(&rt, "palette-query");
                let caret = field.caret.unwrap_or_else(|| panic!("{tag}: no caret"));
                let glyph = rt.presented("palette-glyph").expect("glyph").0;
                let mid = |r: [f32; 4]| r[1] + r[3] / 2.0;
                assert!(
                    (mid(caret) - mid(glyph)).abs() <= 1.0,
                    "{tag}: line {caret:?} off the magnifier {glyph:?}"
                );
                assert!(
                    (mid(caret) - mid(field.rect)).abs() <= 1.0,
                    "{tag}: line {caret:?} off the field {:?}",
                    field.rect
                );
            }
        }
    }
}

/// However many headings the list holds, the desktop list ends through
/// the middle of a row, so half a row says more lies below.
#[test]
fn the_desktop_list_ends_through_half_a_row() {
    for state in ["empty", "typed", "themes"] {
        let rt = palette_window((1100.0, 720.0), false, true, state, false);
        let list = rt.presented("palette-scroll").expect("list").0;
        let bottom = list[1] + list[3];
        let cut = (0..crate::palette::ROWS)
            .filter_map(|i| rt.presented(&format!("palette-row-{i}")))
            .map(|(r, ..)| r)
            .find(|r| r[1] < bottom && r[1] + r[3] > bottom)
            .unwrap_or_else(|| panic!("{state}: no row under the edge at {bottom}"));
        let shown = (bottom - cut[1]) / cut[3];
        assert!(
            (0.4..=0.6).contains(&shown),
            "{state}: {shown} of {cut:?} shows above {bottom}"
        );
    }
}
