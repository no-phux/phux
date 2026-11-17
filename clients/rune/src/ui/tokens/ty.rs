//! The type roles at a text scale and their weight and voice variants
//! (§1.4).

use phux_design::tokens as tok;
use rune::{Family, TextStyle};

use super::MONO;

/// The seven type roles (§1.4) at this window's text scale, plus the
/// machine voice: `mono` (`label` in the chrome's mono face, for machine
/// nouns), `mono_caption` (the same at caption size) and `readout`
/// (`mono_caption` with tabular figures, for numbers that change in place).
#[derive(Debug, Clone, Copy, PartialEq, Hash)]
pub(crate) struct Type {
    pub(crate) micro: TextStyle,
    pub(crate) caption: TextStyle,
    pub(crate) label: TextStyle,
    pub(crate) body: TextStyle,
    pub(crate) lead: TextStyle,
    pub(crate) title: TextStyle,
    pub(crate) display: TextStyle,
    pub(crate) mono: TextStyle,
    pub(crate) mono_caption: TextStyle,
    pub(crate) readout: TextStyle,
}

/// A type role's weight and voice variants, derived here once (§1.4:
/// emphasis by weight, never size; machine nouns in the mono face). Views
/// call these instead of rebuilding a `TextStyle`.
pub(crate) trait Voice {
    /// The role at `type.weights.semibold`.
    fn semibold(self) -> Self;
    /// The role at `type.weights.medium`, the label weight.
    fn medium(self) -> Self;
    /// The role in the chrome's mono face (`Paper Mono`), for machine
    /// nouns: hosts, pane ids, paths.
    fn machine(self) -> Self;
    /// The role with tabular figures, for numbers that change in place.
    fn figures(self) -> Self;
}

#[allow(clippy::cast_precision_loss, reason = "font weights are 100..=900")]
const fn weight(w: u32) -> f32 {
    w as f32
}

impl Voice for TextStyle {
    fn semibold(self) -> Self {
        Self {
            weight: weight(tok::TYPE.weights.semibold),
            ..self
        }
    }

    fn medium(self) -> Self {
        Self {
            weight: weight(tok::TYPE.weights.medium),
            ..self
        }
    }

    fn machine(self) -> Self {
        self.family(MONO)
    }

    fn figures(self) -> Self {
        self.features("'tnum' 1")
    }
}

impl Type {
    /// The roles from the seven styles, the machine voice derived.
    pub(crate) fn from_roles(
        [micro, caption, label, body, lead, title, display]: [TextStyle; 7],
    ) -> Self {
        Self {
            micro,
            caption,
            label,
            body,
            lead,
            title,
            display,
            mono: label.machine(),
            mono_caption: caption.machine(),
            readout: caption.machine().figures(),
        }
    }
}

impl Type {
    pub(super) fn new(scale: f32) -> Self {
        let r = &tok::TYPE.role;
        Self::from_roles(
            [
                r.micro, r.caption, r.label, r.body, r.lead, r.title, r.display,
            ]
            .map(|role| style(&role, scale)),
        )
    }
}

#[allow(clippy::cast_precision_loss, reason = "font weights are 100..=900")]
pub(super) fn style(role: &tok::TypeRole, scale: f32) -> TextStyle {
    let size = role.size * scale;
    let features = if role.features.contains(&"tnum") {
        "'tnum' 1"
    } else {
        ""
    };
    TextStyle::new(size, role.weight as f32)
        .lh(role.line_height * scale)
        .tracking(role.tracking_em * size)
        .features(features)
        .family(if role.family == "mono" {
            MONO
        } else {
            Family::Sans
        })
}
