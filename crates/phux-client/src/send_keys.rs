//! Translate tmux-style key-specs into routed input events (ADR-0022).
//!
//! Each argument is a named key (`Enter`, `Tab`, `C-c`, `M-x`, ...) or a
//! literal string typed character by character. A literal run immediately
//! before `Enter` is instead one paste followed by the real key, so a
//! paste-aware TUI can tell text from submission. Bytes go through the same
//! [`StdinParser`] the interactive client uses, and events ride the
//! side-effect-free `ROUTE_INPUT` command (L1.md §5.1): nothing attaches,
//! subscribes, or resizes the pane.

use std::path::Path;

use phux_protocol::ResourceId;
use phux_protocol::input::InputEvent;
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::{AttachTarget, Command, CommandResult, CommandValue, StateScope};
use phux_protocol::wire::info::SessionSnapshot;

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::attach::input::StdinParser;
use crate::state::report_degradation;

/// The bytes of a named key (case-insensitive), if `arg` is one.
fn named_key_bytes(arg: &str) -> Option<&'static [u8]> {
    Some(match arg.to_ascii_lowercase().as_str() {
        "enter" | "return" => b"\r",
        "tab" => b"\t",
        "escape" | "esc" => b"\x1b",
        "space" => b" ",
        "bspace" | "backspace" => b"\x7f",
        "up" => b"\x1b[A",
        "down" => b"\x1b[B",
        "right" => b"\x1b[C",
        "left" => b"\x1b[D",
        "home" => b"\x1b[H",
        "end" => b"\x1b[F",
        _ => return None,
    })
}

/// Translate one key-spec argument to the bytes a terminal would receive:
/// named keys to their escape bytes, `C-<x>` to a control byte, `M-<x>`
/// ESC-prefixed, anything else literal UTF-8.
#[must_use]
pub fn spec_to_bytes(arg: &str) -> Vec<u8> {
    if let Some(bytes) = named_key_bytes(arg) {
        return bytes.to_vec();
    }
    if let Some(rest) = strip_prefix_ci(arg, "c-")
        && rest.chars().count() == 1
        && let Some(c) = rest.chars().next()
        && c.is_ascii()
    {
        return vec![(c.to_ascii_uppercase() as u8) & 0x1f];
    }
    if let Some(rest) = strip_prefix_ci(arg, "m-") {
        let mut v = vec![0x1b];
        v.extend_from_slice(rest.as_bytes());
        return v;
    }
    arg.as_bytes().to_vec()
}

/// Case-insensitive prefix strip (so `C-` and `c-` both work).
fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

/// Translate all key-spec args into [`InputEvent`]s. A literal run followed
/// by `Enter` becomes one trusted paste plus the key, so the server can
/// bracket it when the pane has DEC mode 2004 on.
#[must_use]
pub fn events_for(args: &[String]) -> Vec<InputEvent> {
    let mut parser = StdinParser::default();
    let mut events = Vec::new();
    let mut index = 0;

    while index < args.len() {
        if is_named_spec(&args[index]) {
            events.extend(parser.feed(&spec_to_bytes(&args[index])));
            index += 1;
            continue;
        }

        let literal_start = index;
        while index < args.len() && !is_named_spec(&args[index]) {
            index += 1;
        }
        if index < args.len() && is_enter_spec(&args[index]) {
            events.extend(parser.flush());
            let data = args[literal_start..index]
                .iter()
                .flat_map(|arg| arg.bytes())
                .collect();
            events.push(InputEvent::Paste(PasteEvent {
                trust: PasteTrust::Trusted,
                data,
            }));
        } else {
            for arg in &args[literal_start..index] {
                events.extend(parser.feed(arg.as_bytes()));
            }
        }
    }
    events.extend(parser.flush());
    events
}

fn is_enter_spec(arg: &str) -> bool {
    matches!(arg.to_ascii_lowercase().as_str(), "enter" | "return")
}

fn is_named_spec(arg: &str) -> bool {
    named_key_bytes(arg).is_some()
        || strip_prefix_ci(arg, "c-").is_some_and(|rest| rest.chars().count() == 1)
        || strip_prefix_ci(arg, "m-").is_some()
}

/// Resolve `target` to its focused pane: the session's `active_window`, then
/// that window's `active_resource`; [`AttachTarget::Last`] is the server-wide
/// `focused_resource`.
fn resolve_focused_pane(snapshot: &SessionSnapshot, target: &AttachTarget) -> Option<ResourceId> {
    let session = match target {
        AttachTarget::Last => return Some(snapshot.focused_resource.clone()),
        AttachTarget::ByName(name) | AttachTarget::CreateIfMissing { name, .. } => {
            snapshot.sessions.iter().find(|s| &s.name == name)?
        }
        AttachTarget::ById(id) => snapshot.sessions.iter().find(|s| s.id == *id)?,
        // `#[non_exhaustive]`: a newer target is not resolvable here.
        _ => return None,
    };
    let active_window = session.active_window?;
    snapshot
        .windows
        .iter()
        .find(|w| w.id == active_window)
        .and_then(|w| w.active_resource.clone())
}

/// Send `keys` to the focused pane of `target` without attaching, returning
/// the pane so callers (e.g. `phux run`) can read back the same one. Callers
/// with a resolved pane use [`send_to`].
pub async fn send(
    socket: &Path,
    target: AttachTarget,
    keys: &[String],
) -> Result<ResourceId, AttachError> {
    let mut conn = Connection::connect(socket).await?;
    let pane = focused_pane(&mut conn, &target).await?;
    route_keys(&mut conn, &pane, keys).await?;
    drop(conn);
    Ok(pane)
}

/// Resolve `target`'s focused pane over `conn` without attaching.
pub(crate) async fn focused_pane(
    conn: &mut Connection,
    target: &AttachTarget,
) -> Result<ResourceId, AttachError> {
    // A hub interleaves one ERROR per unreachable satellite; a degraded
    // snapshot is not fatal here, but it is reported.
    let (result, interleaved) = conn
        .request(
            1,
            Command::GetState {
                scope: StateScope::Server,
            },
        )
        .await?
        .into_parts();
    report_degradation(&interleaved);
    let snapshot = match result {
        CommandResult::OkWith(CommandValue::State(snap)) => snap,
        CommandResult::Error { message, .. } => return Err(AttachError::Refused(message)),
        other => {
            return Err(AttachError::Protocol(crate::explain::explain_unexpected(
                "GET_STATE",
                &other,
            )));
        }
    };
    resolve_focused_pane(&snapshot, target).ok_or_else(|| {
        AttachError::Refused("no such session, or it has no focused pane".to_owned())
    })
}

/// Send `keys` to a pre-resolved `pane` via `ROUTE_INPUT`.
///
/// # Errors
///
/// Connection or `ROUTE_INPUT` failures; an unknown pane is
/// [`AttachError::Refused`].
pub async fn send_to(socket: &Path, pane: ResourceId, keys: &[String]) -> Result<(), AttachError> {
    let mut conn = Connection::connect(socket).await?;
    route_keys(&mut conn, &pane, keys).await
}

/// Paste `data` into a pre-resolved `pane` as one [`InputEvent::Paste`] via
/// `ROUTE_INPUT`.
///
/// The server brackets it when the pane has DEC mode 2004 on;
/// an [`PasteTrust::Untrusted`] payload may be dropped by the pane's policy
/// while the command still acks `Ok`.
///
/// # Errors
///
/// Connection or `ROUTE_INPUT` failures; an unknown pane is
/// [`AttachError::Refused`].
pub async fn paste_to(
    socket: &Path,
    pane: ResourceId,
    data: Vec<u8>,
    trust: PasteTrust,
) -> Result<(), AttachError> {
    let mut conn = Connection::connect(socket).await?;
    route_input(
        &mut conn,
        1,
        pane,
        InputEvent::Paste(PasteEvent { trust, data }),
    )
    .await
}

/// Deliver each built [`InputEvent`] to `pane` over `conn`, in order.
pub(crate) async fn route_keys(
    conn: &mut Connection,
    pane: &ResourceId,
    keys: &[String],
) -> Result<(), AttachError> {
    for (i, event) in events_for(keys).into_iter().enumerate() {
        // Id 1 may have been a preceding GET_STATE on this connection.
        let request_id = u32::try_from(i).unwrap_or(u32::MAX - 1).saturating_add(2);
        route_input(conn, request_id, pane.clone(), event).await?;
    }
    Ok(())
}

/// One `ROUTE_INPUT`. The interleave is safely ignored: this connection
/// never attaches or subscribes, and the handler pushes nothing before the
/// ack.
async fn route_input(
    conn: &mut Connection,
    request_id: u32,
    terminal_id: ResourceId,
    event: InputEvent,
) -> Result<(), AttachError> {
    match conn
        .request(request_id, Command::RouteInput { terminal_id, event })
        .await?
        .into_result_ignoring_interleaved()
    {
        CommandResult::Ok => Ok(()),
        CommandResult::Error { message, .. } => Err(AttachError::Refused(message)),
        other => Err(AttachError::Protocol(crate::explain::explain_unexpected(
            "ROUTE_INPUT",
            &other,
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_specs_map_to_terminal_bytes() {
        for (spec, bytes) in [
            ("Enter", &b"\r"[..]),
            ("tab", b"\t"),
            ("Escape", b"\x1b"),
            ("Up", b"\x1b[A"),
            ("C-c", b"\x03"),
            ("c-a", b"\x01"),
            ("M-x", b"\x1bx"),
            ("echo hi", b"echo hi"),
        ] {
            assert_eq!(spec_to_bytes(spec), bytes, "{spec}");
        }
    }

    #[test]
    fn literal_text_before_enter_becomes_submission_safe_paste() {
        let events = events_for(&[
            "Reply with ".to_owned(),
            "OK".to_owned(),
            "Enter".to_owned(),
        ]);
        assert_eq!(events.len(), 2, "got {events:?}");
        assert_eq!(
            events[0],
            InputEvent::Paste(PasteEvent {
                trust: PasteTrust::Trusted,
                data: b"Reply with OK".to_vec(),
            })
        );
        assert!(matches!(events[1], InputEvent::Key(_)));
    }

    #[test]
    fn literal_text_without_enter_remains_character_keys() {
        let events = events_for(&["ab".to_owned()]);
        assert_eq!(events.len(), 2, "got {events:?}");
        assert!(
            events
                .iter()
                .all(|event| matches!(event, InputEvent::Key(_)))
        );
    }
}
