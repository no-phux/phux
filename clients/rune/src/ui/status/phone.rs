//! The phone's side of the window's words: what the phone app says where a
//! desktop would name a command, and the way back to pairing.

use rune::El;

use super::{Ink, action, server_behind};
use crate::desk::Desk;
use crate::desk::link::Standing;

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
