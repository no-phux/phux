//! The forced-state page (§5.2): every primitive in each state, kit and
//! phux's own, and the status ladder's marks and words.

use rune::{Cx, El, Env, Runtime, col, el, row, text};
use rune_kit::{
    Button, ButtonVariant, Checkbox, IconButton, IconButtonVariant, Progress, Spinner, Switch,
};

use super::SCALE;
use crate::desk::Desk;
use crate::theme::Theme;
use crate::ui::rows::{Row, State};
use crate::ui::tokens::{Paint, Tokens};
use crate::ui::{badge, icons};

pub(super) const SIZE: (f32, f32) = (1280.0, 900.0);

const KIT_STATES: [(rune_kit::State, &str); 5] = [
    (rune_kit::State::Rest, "rest"),
    (rune_kit::State::Hover, "hover"),
    (rune_kit::State::Pressed, "press"),
    (rune_kit::State::Focus, "focus"),
    (rune_kit::State::Disabled, "disabled"),
];

fn heading(t: &Tokens, words: &str) -> El {
    text(words.to_owned())
        .font(t.ty.caption)
        .color(t.roles.text.dim.paint())
}

/// Each OWN row state, plain, current and selected.
fn rows(t: &Tokens) -> El {
    let column = |tag: &str, selected: bool, current: bool| {
        col()
            .w(t.space.sidebar_w)
            .gap(t.space.s1)
            .child(heading(t, tag))
            .children(State::ALL.map(|s| {
                Row::new(
                    format!("state-row-{tag}-{}", s.key()),
                    format!("{} row", s.key()),
                )
                .detail("2m · ~/phux")
                .leading(badge::mark(t, phux_design::State::Working, t.icon.size.sm))
                .selected(selected)
                .current(current)
                .state(s)
                .on_press(|_| {})
                .build(t)
            }))
    };
    row()
        .gap(t.space.s6)
        .child(column("plain", false, false))
        .child(column("current", false, true))
        .child(column("selected", true, false))
}

/// The status ladder: each state's mark, word and badge.
fn ladder(t: &Tokens) -> El {
    col().gap(t.space.s2).child(heading(t, "status")).child(
        row().gap(t.space.s3).items_center().children(
            phux_design::State::ALL
                .map(|s| badge::tone_badge(t, s, None).key(format!("state-badge-{}", s.key()))),
        ),
    )
}

/// Kit controls in each forced state.
fn kit(t: &Tokens) -> El {
    let k = &t.kit;
    col()
        .gap(t.space.s2)
        .child(heading(t, "kit"))
        .children(KIT_STATES.map(|(s, tag)| {
            row()
                .gap(t.space.s3)
                .items_center()
                .child(el().w(t.space.s16).child(heading(t, tag)))
                .children(
                    [
                        ButtonVariant::Neutral,
                        ButtonVariant::Submit,
                        ButtonVariant::Outline,
                        ButtonVariant::Ghost,
                        ButtonVariant::Danger,
                    ]
                    .map(|v| {
                        Button::new(format!("{v:?}"))
                            .variant(v)
                            .state(s)
                            .on_press(|_: &mut Desk| {})
                            .build(k)
                    }),
                )
                .child(
                    IconButton::new(&icons::CLOSE)
                        .variant(IconButtonVariant::Ghost)
                        .label("Close")
                        .state(s)
                        .on_press(|_: &mut Desk| {})
                        .build(k),
                )
                .child(
                    Switch::new(true)
                        .label("On")
                        .state(s)
                        .on_toggle(|_: &mut Desk| {})
                        .build(t.controls(s == rune_kit::State::Disabled)),
                )
                .child(
                    Switch::new(false)
                        .label("Off")
                        .state(s)
                        .on_toggle(|_: &mut Desk| {})
                        .build(t.controls(s == rune_kit::State::Disabled)),
                )
                .child(
                    Checkbox::new(true)
                        .label("Checked")
                        .state(s)
                        .on_toggle(|_: &mut Desk| {})
                        .build(t.controls(s == rune_kit::State::Disabled)),
                )
                .child(
                    Checkbox::new(false)
                        .label("Unchecked")
                        .state(s)
                        .on_toggle(|_: &mut Desk| {})
                        .build(t.controls(s == rune_kit::State::Disabled)),
                )
        }))
        .child(
            row()
                .gap(t.space.s3)
                .items_center()
                .child(
                    el().w(t.space.sidebar_w)
                        .child(Progress::new(None).build(t.controls(false))),
                )
                .child(
                    el().w(t.space.sidebar_w)
                        .child(Progress::new(Some(0.6)).build(t.controls(false))),
                )
                .child(Spinner::new().build(k)),
        )
}

thread_local! {
    /// Paint the page with every focusable control's real ring forced on
    /// (`Cx::force_focus_rings`), for the focus state's frame.
    static FORCE_RINGS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn view(desk: &Desk, cx: &mut Cx<Desk>) -> El {
    let tokens = Tokens::for_window(desk, cx);
    let t = &*tokens;
    cx.foreground(t.roles.text.text.paint());
    let ring = t.space.focus_ring;
    cx.focus_ring(ring.width, ring.offset, t.roles.border.focus.paint());
    if FORCE_RINGS.get() {
        cx.force_focus_rings();
    }
    col()
        .key("states")
        .w_full()
        .h_full()
        .padding(t.space.s6)
        .gap(t.space.s6)
        .fill(t.roles.layer.ground.paint())
        .child(
            col()
                .padding(t.space.card_inset)
                .gap(t.space.s4)
                .radius(t.radius.lg)
                .fill(t.roles.layer.raised.paint())
                .child(rows(t)),
        )
        .child(ladder(t))
        .child(kit(t))
}

pub(super) fn page(dark: bool, theme: Option<&'static Theme>) -> Runtime<Desk> {
    FORCE_RINGS.set(false);
    let mut desk = crate::test_support::window();
    desk.settings.theme = theme;
    Runtime::new(desk, view, SIZE, Env::new(dark, SCALE))
}

/// The page with every focusable control drawing its real focus ring.
pub(super) fn focus_page(dark: bool, theme: Option<&'static Theme>) -> Runtime<Desk> {
    let rt = page(dark, theme);
    FORCE_RINGS.set(true);
    rt
}

/// A selection never masks an invalid row: the selected invalid row's
/// edge is nearer `status.failed` than the selected resting row's is.
pub(super) fn assert_invalid_shows(rt: &Runtime<Desk>, pixels: &[u8], t: &Tokens) {
    let failed = t.roles.status.failed.rgb();
    let edge = |key: &str| {
        let r = crate::ui::element(rt, key).rect;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "on screen"
        )]
        let (x, y) = (
            ((r[0] + r[2] / 2.0) * SCALE) as usize,
            (r[1] * SCALE) as usize,
        );
        let at = (y * (SIZE.0 * SCALE) as usize + x) * 4;
        let p = &pixels[at..at + 3];
        [failed.r, failed.g, failed.b]
            .iter()
            .zip(p)
            .map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs())
            .sum::<i32>()
    };
    let invalid = edge("state-row-selected-invalid");
    let rest = edge("state-row-selected-rest");
    assert!(
        invalid < rest,
        "the selected invalid row lost its failed edge ({invalid} vs {rest})"
    );
}

/// Every mark is named by its word, and the quiet pair by different words.
pub(super) fn assert_marks(rt: &Runtime<Desk>) {
    let json = rt.accessibility_json().to_string();
    for s in phux_design::State::ALL {
        if let Some(word) = s.word() {
            assert!(json.contains(word), "{}: {word}", s.key());
        }
    }
    assert_ne!(
        phux_design::State::DoneSeen.word(),
        phux_design::State::Idle.word(),
        "done (seen) and idle share a mark, never a name"
    );
}
