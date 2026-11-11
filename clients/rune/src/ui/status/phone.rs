//! The phone's side of the window's words: what the phone app says where a
//! desktop would name a command, the way back to pairing, and where a banner
//! goes under a phone's top bar.

use rune::El;

use super::super::phone::TOP_BAR;
use super::{Ink, TITLEBAR_H, action, server_behind};
use crate::desk::Desk;
use crate::desk::link::Standing;

/// A held confirmation on a phone, with the gap under it.
const CONFIRMATION: f32 = 68.0;

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
pub(super) fn pair_again(key: &'static str, p: &Ink) -> El {
    action("Pair again", key, p, Desk::toggle_machines)
}

/// Where the banner floats: under a desktop's title bar or a phone's top
/// bar, and below a held confirmation, which keeps the top of the stage.
pub(super) fn banner_top(desk: &Desk) -> f32 {
    let held = desk.pending.is_some();
    if desk.phone.is_none() {
        return if held { 96.0 } else { 44.0 };
    }
    // UIKit keeps the native surface inside the safe area; a desktop's
    // preview of the phone starts under its own title bar.
    let inset = if desk.native.is_some() {
        0.0
    } else {
        TITLEBAR_H
    };
    inset + TOP_BAR + 4.0 + if held { CONFIRMATION } else { 0.0 }
}
