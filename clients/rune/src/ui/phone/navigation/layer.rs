//! How a navigation page comes and goes: Sessions drifts in over the
//! frost from the left edge it is pulled from and swiped back to; a sheet
//! rises from below and is swiped down. A finger swiping one moves it
//! through the phone's live dismissal values.
use rune::{El, el};

use super::{Desk, Look, Page};
use crate::desk::phone::dismiss::Away;
use crate::ui::Cover;
use crate::ui::phone::motion;
use crate::ui::tokens::GlassSurface;

/// The page in the layer that comes and goes: it enters and leaves the way
/// it is swiped away and follows a finger swiping it. Sessions drifts in
/// from the left edge it came from, ahead of its frost; a sheet rises
/// whole from below under its grabber. Each page has its own layer, so one
/// giving way to another (the switcher's All sessions) plays both: the
/// sheet lowers as the drawer comes in. Under Reduce Motion it lands at
/// once.
pub(super) fn layer(desk: &Desk, page_el: El, page: Page, top: f32, p: &Look) -> El {
    let (away, layer) = match page {
        Page::Sessions => {
            let drift = p.space.s16;
            // The list goes ahead of its frost, which clears after it.
            let springs = list_springs(p);
            let drawer = rune::Transition::offset(-drift, 0.0)
                .combined(rune::Transition::opacity())
                .springs(springs);
            // Over a sheet it replaces (the switcher's All sessions), which
            // lowers under the frost rather than across the list.
            let layer = el().transition(drawer).paint_order(OVER_LEAVING);
            (Away::Sessions, layer.child(page_el))
        }
        _ => {
            let sheet = el()
                .transition(motion::sheet(p))
                .child(page_el)
                .child(crate::ui::phone::sheet::grabber(top, p));
            (Away::Sheet(page), sheet)
        }
    };
    let layer = layer.key(layer_key(page)).cover();
    crate::ui::phone::swipeable(desk, layer, away, p)
}

/// Where Sessions and its frost paint among the phone's surfaces: over a
/// page leaving as they come in, which Rune otherwise paints over them
/// where it stood. Nothing else is open beside them.
const OVER_LEAVING: i16 = 1;

/// The key of `page`'s layer: what moves as it comes, goes and is swiped.
const fn layer_key(page: Page) -> &'static str {
    match page {
        Page::Sessions => "phone-navigation-layer",
        Page::Actions => "phone-actions-layer",
        Page::Switcher => "phone-switcher-layer",
    }
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
    // A swipe toward the edge leaves it whole while the list goes, and it
    // clears on a longer exit than the list's, so the terminal never shows
    // crisp through a fading list (the dismiss motion sidecars' frost and
    // list opacities).
    Some(
        frost
            .transition(rune::Transition::opacity().springs(frost_springs(p)))
            .paint_order(OVER_LEAVING)
            .inert()
            .a11y_hidden(),
    )
}

/// Sessions' list leaves on a control's quick exit, well inside the
/// drawer's, so it is gone before the frost under it clears.
fn list_springs(p: &Look) -> rune::SpringPair {
    rune::SpringPair::new(p.motion.drawer.enter, p.motion.control.exit)
}

/// The frost clears on the drawer's longer entrance spring, so the terminal
/// never shows crisp through a list still on screen.
fn frost_springs(p: &Look) -> rune::SpringPair {
    rune::SpringPair::new(p.motion.drawer.enter, p.motion.drawer.enter)
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
