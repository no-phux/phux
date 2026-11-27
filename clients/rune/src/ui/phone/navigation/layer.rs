//! How a navigation page comes and goes: Sessions drifts in over the
//! frost from the left edge it is pulled from and swiped back to; a sheet
//! rises from below and is swiped down. A finger swiping one moves it
//! through the phone's live dismissal values.
use rune::{El, el};

use super::{Desk, Look, Page};
use crate::desk::phone::dismiss::Away;
use crate::ui::Cover;
use crate::ui::tokens::GlassSurface;

/// The page in the layer that comes and goes: it enters and leaves the way
/// it is swiped away (Sessions toward the left edge it came from, a sheet
/// downward) and follows a finger swiping it. Under Reduce Motion it lands
/// at once.
pub(super) fn layer(desk: &Desk, page_el: El, page: Page, p: &Look) -> El {
    let drift = p.space.s16;
    let (away, from) = match page {
        Page::Sessions => (Away::Sessions, (-drift, 0.0)),
        _ => (Away::Sheet(page), (0.0, drift)),
    };
    let layer = el()
        .key("phone-navigation-layer")
        .cover()
        .transition(
            rune::Transition::offset(from.0, from.1)
                .combined(rune::Transition::opacity())
                .springs(p.motion.drawer),
        )
        .child(page_el);
    crate::ui::phone::swipeable(desk, layer, away, p)
}

/// Sessions comes in from the left edge, where the thumb pulls it from, as
/// frost rather than a sliding panel: the terminal blurs in place under
/// [`frost`] (the phone's one blur) while the list drifts a short way in
/// behind the finger and sharpens. A let-go pull or a close fades it back
/// off, and a swipe toward that edge takes it. Terminal actions is one
/// glass page. Opaque where glass may not draw.
pub(super) fn drawer(sheet: El, page: Page, pull: Option<f32>, width: f32, p: &Look) -> El {
    if page != Page::Sessions {
        return glass(sheet, p);
    }
    let shown = shown(pull, width);
    let drift = p.space.s16;
    // The list trails the frost, so it reads once the glass has come up
    // behind it rather than as text floating on the terminal.
    sheet
        .offset(-drift * (1.0 - shown), 0.0)
        .offset_motion(follow(pull, p))
        .opacity(shown * shown)
        .opacity_motion(follow(pull, p))
}

/// The frost Sessions comes up on: the whole terminal blurred in place.
/// Under a pulling finger it thickens, its blur and tint growing from
/// clear to whole by the point a lift opens, so the terminal never shows
/// crisp through the list. A sibling under the list, never inside a layer
/// that fades as one, which would flatten what it blurs.
pub(in crate::ui) fn frost(desk: &Desk) -> Option<El> {
    let phone = desk.phone.as_ref()?;
    let pull = phone.swipe.pull.filter(|_| desk.phone_page().is_none());
    let page = desk.phone_page().or(pull.map(|_| Page::Sessions))?;
    if page != Page::Sessions {
        return None;
    }
    let p = &crate::ui::phone::page_look(desk);
    let commit = crate::desk::phone::swipe::PULL_COMMIT;
    let thick = (shown(pull, phone.stage[0]) / commit).min(1.0);
    let frost = p.glass_panel_thick(
        el().key("phone-drawer-frost").cover(),
        GlassSurface::PhoneBar,
        p.glass_tint_layer(),
        p.roles.layer.ground,
        thick,
    );
    // A swipe toward the edge leaves it whole while the list goes, so
    // the terminal never shows crisp through the list; it clears as the
    // drawer closes.
    Some(
        frost
            .transition(rune::Transition::opacity().springs(p.motion.drawer))
            .inert()
            .a11y_hidden(),
    )
}

fn glass(panel: El, p: &Look) -> El {
    p.glass_panel(
        panel,
        GlassSurface::PhoneBar,
        p.glass_tint_layer(),
        p.roles.layer.ground,
    )
}

/// How far in Sessions has come: whole once open.
fn shown(pull: Option<f32>, width: f32) -> f32 {
    pull.map_or(1.0, |pulled| (pulled / width.max(1.0)).clamp(0.0, 1.0))
}

/// A pulling finger is followed exactly; a let-go one settles on the
/// drawer spring.
fn follow(pull: Option<f32>, p: &Look) -> rune::Spring {
    if pull.is_some() {
        rune::Spring::NONE
    } else {
        p.motion.drawer.enter
    }
}
