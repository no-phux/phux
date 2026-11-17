//! The phone's side of the window's words: what the phone app says where a
//! desktop would name a command, the way back to pairing, and where a banner
//! goes under a phone's top bar.

use rune::El;

use phux_design::tokens::SPACE;

use super::super::phone::top_bar_height;
use super::{Tokens, action, server_behind};
use crate::desk::Desk;
use crate::desk::link::Standing;

/// A held confirmation on a phone, with the gap under it.
fn confirmation() -> f32 {
    SPACE.s16 + SPACE.s1
}

/// A desktop banner's top: under the title bar, and under a held
/// confirmation's bar when there is one.
fn desktop_top(held: bool) -> f32 {
    let top = SPACE.titlebar_h + SPACE.s1 + SPACE.s0_5;
    if held {
        top + SPACE.s12 + SPACE.s1
    } else {
        top
    }
}

/// The phone app's title and detail for `standing`, where they differ from
/// a desktop's: its computer by name, nothing to run in a shell.
pub(super) fn words(
    standing: &Standing,
    machine: &str,
    unpaired: bool,
) -> Option<(String, String)> {
    Some(match standing {
        Standing::Unreachable(why) if unpaired => ("No longer paired".to_owned(), why.clone()),
        Standing::Unreachable(why) => (format!("Can't reach {machine}"), why.clone()),
        Standing::Connecting(why) => (format!("Connecting to {machine}…"), why.clone()),
        Standing::Incompatible(words) if server_behind(words) => (
            format!("Update phux on {machine}"),
            format!(
                "This app is newer than phux on {machine}. Update phux there, then try again. \
                 {words}"
            ),
        ),
        _ => return None,
    })
}

/// Whether pairing again is what `standing` needs: the phone app's computer
/// refused its credential, which no retry changes.
pub(super) fn repairs(desk: &Desk, standing: &Standing) -> bool {
    desk.native.is_some() && desk.unpaired() && matches!(standing, Standing::Unreachable(_))
}

/// Whether the phone app's dial has outlasted a healthy one: `standing`
/// then carries what to check, and trying again is offered beside it.
pub(super) fn slow_dial(desk: &Desk, standing: &Standing) -> bool {
    desk.native.is_some() && matches!(standing, Standing::Connecting(why) if !why.is_empty())
}

/// Back to the host's pairing, which is where the phone's computers are.
pub(super) fn pair_again(key: &'static str, p: &Tokens) -> El {
    action("Pair again", key, p, Desk::toggle_machines)
}

/// Where the banner floats: under a desktop's title bar or a phone's top
/// bar, and below a held confirmation, which keeps the top of the stage.
pub(super) fn banner_top(desk: &Desk) -> f32 {
    let held = desk.pending.is_some();
    if desk.phone.is_none() {
        return desktop_top(held);
    }
    // UIKit keeps the native surface inside the safe area; a desktop's
    // preview of the phone starts under its own title bar.
    let inset = if desk.native.is_some() {
        0.0
    } else {
        SPACE.titlebar_h
    };
    inset + top_bar_height() + SPACE.s1 + if held { confirmation() } else { 0.0 }
}
