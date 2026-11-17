//! Today's `Ink` values, reproduced exactly while the lanes convert: the
//! contract commit changes no pixel. The values commit replaces this with
//! the brand tokens.

use std::hash::{Hash, Hasher};
use std::rc::Rc;

use rune::{Color, tok};

use super::{Inputs, Tokens};
use crate::theme::{Marks, SELECTED_WASH, Selection, Theme, on_fill};
use crate::ui::Ink;

/// A card's shadow: deep on a dark ground, soft on a light one.
fn shade(ground: Color) -> Color {
    let dark = on_fill(ground) == Color::WHITE;
    Color::BLACK.alpha(if dark { 0.38 } else { 0.13 })
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Values {
    bg: Color,
    raised: Color,
    sidebar: Color,
    text: Color,
    dim: Color,
    separator: Color,
    selected: Color,
    current: Color,
    hover: Color,
    accent: Color,
    host: Color,
    on_accent: Color,
    danger: Color,
    edge: Color,
    marks: Marks,
    selection: Selection,
    shade: Color,
}

impl Values {
    pub(super) fn new(inputs: &Inputs) -> Self {
        let mut v = Self::of(inputs.dark, inputs.accent, inputs.theme.as_ref());
        if let Some(rgb) = inputs.host {
            v.host = crate::host_color::eased(rgb, v.sidebar);
        }
        v
    }

    fn of(dark: bool, accent: Color, theme: Option<&Theme>) -> Self {
        if let Some(theme) = theme {
            let roles = theme.roles();
            return Self {
                bg: crate::theme::color(theme.background),
                raised: theme.raised(),
                sidebar: theme.surface(0.045),
                text: roles.text,
                dim: roles.dim,
                separator: roles.text.alpha(0.13),
                selected: crate::theme::color(theme.accent).alpha(SELECTED_WASH),
                current: roles.text.alpha(0.1),
                hover: roles.text.alpha(0.07),
                accent: roles.accent,
                host: roles.accent,
                on_accent: roles.on_accent,
                danger: roles.danger,
                edge: roles.edge,
                marks: roles.marks,
                selection: roles.selection,
                shade: shade(crate::theme::color(theme.background)),
            };
        }
        let t = tok::theme(dark);
        // Neutral, so no chrome tints the terminal's own colours.
        let (sidebar, raised, edge) = if dark {
            (0x14_1517, 0x1e_1f22, 0x2b_2c30)
        } else {
            (0xf1_f1f2, 0xfb_fbfc, 0xdc_dcdf)
        };
        Self {
            bg: t.bg,
            raised: crate::theme::color(raised),
            sidebar: crate::theme::color(sidebar),
            text: t.text,
            dim: t.text_secondary,
            separator: t.separator,
            selected: accent.alpha(SELECTED_WASH),
            current: t.text.alpha(0.1),
            hover: t.hover,
            accent,
            host: accent,
            on_accent: on_fill(accent),
            danger: Theme::system_danger(dark),
            edge: crate::theme::color(edge),
            marks: Theme::system_marks(dark),
            selection: Selection::system(),
            shade: shade(t.bg),
        }
    }

    pub(super) fn ink(&self, tokens: Rc<Tokens>) -> Ink {
        Ink {
            bg: self.bg,
            raised: self.raised,
            sidebar: self.sidebar,
            text: self.text,
            dim: self.dim,
            separator: self.separator,
            selected: self.selected,
            current: self.current,
            hover: self.hover,
            accent: self.accent,
            host: self.host,
            on_accent: self.on_accent,
            danger: self.danger,
            edge: self.edge,
            marks: self.marks,
            selection: self.selection,
            shade: self.shade,
            tokens,
        }
    }

    pub(super) fn hash_into(&self, h: &mut impl Hasher) {
        for c in [
            self.bg,
            self.raised,
            self.sidebar,
            self.text,
            self.dim,
            self.separator,
            self.selected,
            self.current,
            self.hover,
            self.accent,
            self.host,
            self.on_accent,
            self.danger,
            self.edge,
            self.shade,
        ] {
            c.hash(h);
        }
        self.selection.key().hash(h);
    }
}
