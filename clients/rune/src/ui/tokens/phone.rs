//! The phone form factor's tokens (§1.4, §1.6, §4.1.5): its type roles and
//! page margin at the brand's iOS values, the iOS text size the bridge
//! reports, and the tokens for a terminal ground.
//!
//! The generated Rust carries desktop values only; the phone's sizes are
//! phux-brand's iOS values (`type.role.*.ios`, `space.page_margin.ios`),
//! with every other field of a role taken from the generated desktop role.

use std::cell::Cell;
use std::rc::Rc;

use phux_design::tokens as tok;
use rune::{Color, Cx};

use super::{Inputs, Tokens, Type, forced, style};
use crate::desk::Desk;
use crate::desk::accessibility::ContentSize;
use crate::theme::Theme;

/// The phone's (size, line height) per role, `micro` to `display`.
const IOS_ROLES: [(f32, f32); 7] = [
    (11.0, 13.0),
    (12.0, 16.0),
    (14.0, 18.0),
    (14.0, 20.0),
    (16.0, 22.0),
    (22.0, 28.0),
    (28.0, 34.0),
];

/// A phone page's inset from the screen's edge (`space.page_margin.ios`).
const IOS_PAGE_MARGIN: f32 = 16.0;

/// The seven roles at the phone's sizes, unscaled.
fn roles() -> [tok::TypeRole; 7] {
    let r = &tok::TYPE.role;
    let mut roles = [
        r.micro, r.caption, r.label, r.body, r.lead, r.title, r.display,
    ];
    for (role, (size, line_height)) in roles.iter_mut().zip(IOS_ROLES) {
        role.size = size;
        role.line_height = line_height;
    }
    roles
}

/// The seven roles at the phone's sizes, times `scale`; `mono` is `label`
/// in the chrome's mono face.
pub(crate) fn ty(scale: f32) -> Type {
    Type::from_roles(roles().map(|r| style(&r, scale)))
}

/// A phone page's inset from the screen's edge.
pub(crate) const fn page_margin() -> f32 {
    IOS_PAGE_MARGIN
}

/// The system contrast the phone's view read on its latest run, for the
/// surfaces it hosts that have no `Cx` (and the host's edge colour).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct System {
    increased_contrast: bool,
    forced: Option<forced::Forced>,
}

thread_local! {
    static SYSTEM: Cell<System> = Cell::new(System::default());
}

/// Note the system contrast for this run of the phone's view; every
/// [`Tokens::for_phone`] after it takes it.
pub(crate) fn observe(cx: &Cx<Desk>) {
    SYSTEM.set(System {
        increased_contrast: cx.system_increased_contrast(),
        forced: cx.system_contrast().map(forced::Forced::from),
    });
}

/// Which text scale a surface takes: chrome beside the grid is capped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scale {
    Free,
    GridCapped,
}

impl Scale {
    /// The factor for the reported `size`.
    pub(crate) fn of(self, size: ContentSize) -> f32 {
        match self {
            Self::Free => size.factor(),
            Self::GridCapped => size.grid_capped(),
        }
    }
}

impl Tokens {
    /// The phone's tokens (§4.1.5): `theme` when its mode is `dark`'s, else
    /// the house look in that mode, at the text size the host reported,
    /// with the observed system contrast and the host's Reduce
    /// Transparency (both in `Desk`). Cached as every `Tokens` is.
    pub(crate) fn for_phone(
        desk: &Desk,
        dark: bool,
        theme: Option<&Theme>,
        scale: Scale,
    ) -> Rc<Self> {
        let system = SYSTEM.get();
        Self::resolve(&Inputs {
            theme: theme.filter(|t| t.dark == dark).copied(),
            increased_contrast: system.increased_contrast,
            forced: system.forced,
            reduce_transparency: desk.reduce_transparency(),
            type_scale: scale.of(desk.content_size()),
            ..Inputs::system(dark)
        })
    }

    /// Whether `ground` reads as dark: what mode its controls take.
    pub(crate) fn dark_ground(ground: Color) -> bool {
        crate::theme::on_fill(ground) == Color::WHITE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_phone_reads_at_its_own_sizes() {
        let ty = ty(1.0);
        // §1.4: the phone's body is 14, its label 14 and micro the HIG's 11.
        assert!((ty.body.size - 14.0).abs() < f32::EPSILON);
        assert!(ty.label.size > tok::TYPE.role.label.size);
        assert!(ty.micro.size >= 11.0);
        assert!(
            page_margin() >= 16.0,
            "§1.6: a phone page margin is at least 16"
        );
    }
}
