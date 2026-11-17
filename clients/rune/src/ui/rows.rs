//! phux's own rows (§4.1.3 OWN): what kit `List` and `SidebarRow` cannot
//! express (details, marks, disclosure, drag), built only from tokens.
//! Every row takes a [`State`], so the gallery can force each look.

use rune::{Cursor, El, el, row, text};

use super::tokens::{Paint, Tokens};
use crate::desk::Desk;

/// A forced interaction state (§1.10). `Rest` is the live row: it shows
/// hover and press itself. The others paint that state still, for the
/// gallery's `states` page.
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "the gallery's states page forces each look")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub(crate) enum State {
    #[default]
    Rest,
    Hover,
    Press,
    Focus,
    Disabled,
    Invalid,
}

#[cfg_attr(
    not(test),
    allow(dead_code, reason = "the gallery's states page forces each look")
)]
impl State {
    pub(crate) const ALL: [Self; 6] = [
        Self::Rest,
        Self::Hover,
        Self::Press,
        Self::Focus,
        Self::Disabled,
        Self::Invalid,
    ];

    pub(crate) const fn key(self) -> &'static str {
        match self {
            Self::Rest => "rest",
            Self::Hover => "hover",
            Self::Press => "press",
            Self::Focus => "focus",
            Self::Disabled => "disabled",
            Self::Invalid => "invalid",
        }
    }
}

type Press = Box<dyn Fn(&mut Desk)>;

/// One row: an optional leading mark, a title and a detail.
pub(crate) struct Row {
    key: String,
    title: String,
    detail: Option<String>,
    leading: Option<El>,
    selected: bool,
    current: bool,
    mono: bool,
    state: State,
    press: Option<Press>,
}

impl Row {
    pub(crate) fn new(key: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            title: title.into(),
            detail: None,
            leading: None,
            selected: false,
            current: false,
            mono: false,
            state: State::Rest,
            press: None,
        }
    }

    /// Secondary copy after the title: an age, a path, a count.
    pub(crate) fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub(crate) fn leading(mut self, mark: El) -> Self {
        self.leading = Some(mark);
        self
    }

    /// The selected row: the one §1.10 treatment.
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "the gallery's states page forces each look")
    )]
    pub(crate) const fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// The keyboard cursor inside a focus group.
    pub(crate) const fn current(mut self, current: bool) -> Self {
        self.current = current;
        self
    }

    /// A machine noun (a host, a path, a pane id): set in the mono face.
    pub(crate) const fn mono(mut self, mono: bool) -> Self {
        self.mono = mono;
        self
    }

    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "the gallery's states page forces each look")
    )]
    pub(crate) const fn state(mut self, state: State) -> Self {
        self.state = state;
        self
    }

    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "the gallery's states page forces each look")
    )]
    pub(crate) fn on_press(mut self, press: impl Fn(&mut Desk) + 'static) -> Self {
        self.press = Some(Box::new(press));
        self
    }

    pub(crate) fn build(self, t: &Tokens) -> El {
        let r = &t.roles;
        let disabled = self.state == State::Disabled;
        let washed =
            self.selected || self.current || matches!(self.state, State::Hover | State::Press);
        let title_ink = if disabled { r.text.faint } else { r.text.text };
        // Faint never sits on a wash: a washed row promotes it to dim.
        let detail_ink = if washed { r.text.dim } else { r.text.faint };
        let font = if self.mono { t.ty.mono } else { t.ty.label };
        let mut e = row()
            .key(self.key)
            .w_full()
            .h(t.space.row_h)
            .min_h(t.space.pointer_min)
            .min_w(0.0)
            .px(t.space.row_inset_x)
            .gap(t.space.icon_gap)
            .items_center()
            .radius(t.radius.sm)
            .label(self.title.clone())
            .children(self.leading)
            .child(
                text(self.title)
                    .font(font)
                    .color(title_ink.paint())
                    .truncate()
                    .min_w(0.0)
                    .shrink()
                    .a11y_hidden(),
            )
            .children(self.detail.map(|d| {
                text(d)
                    .font(t.ty.caption)
                    .color(detail_ink.paint())
                    .truncate()
                    .min_w(0.0)
                    .shrink()
                    .a11y_hidden()
            }))
            .child(el().grow(1.0));
        e = wash(e, t, self.selected, self.current, self.state);
        if disabled {
            return e.disabled(true);
        }
        e = e.focusable(true).focus_selected(self.selected);
        match self.press {
            Some(press) if self.state == State::Rest => {
                t.kit.interactive(e).cursor(Cursor::Pointer).on_press(press)
            }
            Some(press) => e.cursor(Cursor::Pointer).on_press(press),
            None => e,
        }
    }
}

/// The row's fill and edge for its selection and forced state.
fn wash(e: El, t: &Tokens, selected: bool, current: bool, state: State) -> El {
    let r = &t.roles;
    let e = match state {
        State::Hover => e.fill(r.wash.hover.paint()),
        State::Press => e.fill(r.wash.press.paint()),
        State::Focus => e.outline(
            t.space.focus_ring.width,
            t.space.focus_ring.offset,
            r.border.focus.paint(),
        ),
        State::Invalid => e.border(t.space.hairline, r.status.failed.paint()),
        State::Rest | State::Disabled => e,
    };
    if selected {
        return lifted(e, t);
    }
    if current {
        return e.fill(r.wash.current.paint());
    }
    e
}

/// §1.10's selected treatment: the selected wash, a lit edge from
/// `border.strong` to `border.muted`, and under increased contrast a
/// leading accent bar, so selection never rests on a wash alone.
pub(crate) fn lifted(e: El, t: &Tokens) -> El {
    let r = &t.roles;
    let e = e.fill(r.wash.selected.paint()).border_gradient(
        t.space.hairline,
        r.border.strong.paint(),
        r.border.muted.paint(),
    );
    if !t.increased_contrast {
        return e;
    }
    e.child(
        el().pin(Some(0.0), Some(t.space.s1), None, Some(t.space.s1))
            .w(t.space.s0_5)
            .radius(t.radius.full)
            .fill(r.accent.accent.paint())
            .inert()
            .a11y_hidden(),
    )
}
