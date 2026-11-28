//! The phone's palette: a page that rises whole as a sheet, its search bar
//! under the grabber every sheet wears and its results below.

use rune::{El, col};

use super::header::phone_header;
use super::{Grid, rule};
use crate::desk::Desk;
use crate::desk::phone::dismiss::Away;
use crate::ui::phone;
use crate::ui::tokens::{Paint, Tokens};

/// The page over `results`, `top` under the phone's top inset.
pub(super) fn view(desk: &Desk, t: &Tokens, g: &Grid, results: El, top: f32) -> El {
    let page = col()
        .key("palette")
        .w_full()
        .h_full()
        // Room over the search bar for the grabber every sheet wears.
        .pt(top + t.space.s2)
        .fill(t.roles.layer.ground.paint())
        .transition(phone::motion::sheet(t))
        .child(phone::sheet::grabber(top, t))
        .child(phone_header(desk, t, g))
        .child(rule("palette-header-rule", t))
        .child(phone::swipe_list(desk, results, Away::Palette));
    // A swipe down, from its top, lowers it as Cancel does, clipped above
    // the key bar that ends the holder.
    let page = phone::swipeable(desk, page, Away::Palette, t);
    // The page covers the terminal whole, so it needs no dim; it rises
    // opaque in a holder that does not fade it.
    let holder = phone::sheet::holder(
        "palette-scrim",
        "Command palette",
        Desk::close_palette,
        false,
        t,
    );
    holder
        .on_layout(|d: &mut Desk, rect| d.pointer.window = [rect[2], rect[3]])
        .child(page)
}
